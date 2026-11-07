use std::collections::HashMap;

use intear_dex_types::{AssetId, Caller, DexId, IntearDexEvent, Operation, ReferralFees, expect};
use near_sdk::{
    AccountId, Allowance, CryptoHash, CurveType, Gas, GasWeight, NearToken, Promise, PromiseError,
    PublicKey, PublicKeyHandle, UserYieldId,
    json_types::{Base58CryptoHash, Base64VecU8, U128},
    near,
    serde_json::{Value, json},
};

use crate::{
    DexEngine, DexEngineExt, OracleResponse, STORAGE_BYTES_PER_RECORD, call_events::CallEvents,
};

pub const GAS_FOR_AFTER_ORACLE_OPERATIONS: Gas = Gas::from_tgas(5);

// https://github.com/near/nearcore/blob/2.14.0/core/parameters/res/runtime_configs/parameters.yaml#L321
pub const MAX_ORACLE_RESPONSE_BYTES: usize = 1024;

const ORACLE_KEY_NONCES: u32 = 4;
const ORACLE_KEY_METHOD: &str = "oracle_respond";

// https://github.com/near/nearcore/blob/2.14.0/core/crypto/src/signature.rs#L331-L337
const ED25519_KEY_STORAGE_BYTES: u64 = 33;
const SECP256K1_KEY_STORAGE_BYTES: u64 = 65;
const ML_DSA_65_KEY_STORAGE_BYTES: u64 = 33;

// https://github.com/near/nearcore/blob/2.14.0/core/primitives-core/src/types.rs#L19-L21
const GAS_KEY_NONCE_STORAGE_BYTES: u64 = 8 + 2;

#[near]
impl DexEngine {
    /// Add a gas key that can only call `oracle_respond`, for an oracle to
    /// respond with. The attached NEAR pays for the key's storage, and the rest
    /// of it is the gas balance.
    #[payable]
    pub fn create_oracle_key(&mut self, oracle_key: PublicKey) -> Promise {
        self.assert_not_paused();
        let storage_cost = near_sdk::env::storage_byte_cost().saturating_mul(u128::from({
            let key_bytes = match oracle_key.curve_type() {
                CurveType::ED25519 => ED25519_KEY_STORAGE_BYTES,
                CurveType::SECP256K1 => SECP256K1_KEY_STORAGE_BYTES,
                CurveType::MLDSA65 => ML_DSA_65_KEY_STORAGE_BYTES,
            };
            // Borsh of the access key: its nonce, the tag of its permission, the gas key's
            // balance and number of nonces, unlimited allowance, receiver and methods
            let access_key_bytes = near_sdk::borsh::object_length(&(
                0u64,
                0u8,
                0u128,
                0u16,
                None::<u128>,
                near_sdk::env::current_account_id(),
                vec![ORACLE_KEY_METHOD],
            ))
            .unwrap() as u64;
            key_bytes
                .checked_add(access_key_bytes)
                .unwrap()
                .checked_add(STORAGE_BYTES_PER_RECORD)
                .unwrap()
                .checked_add(
                    u64::from(ORACLE_KEY_NONCES)
                        .checked_mul(
                            key_bytes
                                .checked_add(GAS_KEY_NONCE_STORAGE_BYTES)
                                .unwrap()
                                .checked_add(STORAGE_BYTES_PER_RECORD)
                                .unwrap(),
                        )
                        .unwrap(),
                )
                .unwrap()
        }));
        let gas_key_balance = near_sdk::env::attached_deposit()
            .checked_sub(storage_cost)
            .unwrap_or_else(|| panic!("Attach at least {storage_cost} for the storage of the key"));
        let current_account_id = near_sdk::env::current_account_id();
        Promise::new(current_account_id.clone())
            .add_gas_key_allowance_function_call(
                oracle_key.clone(),
                ORACLE_KEY_NONCES,
                Allowance::Unlimited,
                current_account_id,
                ORACLE_KEY_METHOD.to_string(),
            )
            .transfer_to_gas_key(oracle_key, gas_key_balance)
            .refund_to(near_sdk::env::predecessor_account_id())
    }

    /// Respond to a request to the oracle whose key signed the transaction, by
    /// the sha256 of the request.
    #[private]
    pub fn oracle_respond(
        &self,
        request_hash: Base58CryptoHash,
        response: Base64VecU8,
        nonces: Vec<Base64VecU8>,
    ) {
        expect!(
            response.0.len() <= MAX_ORACLE_RESPONSE_BYTES,
            "Response is more than {MAX_ORACLE_RESPONSE_BYTES} bytes"
        );
        let oracle_key = PublicKeyHandle::from(near_sdk::env::signer_account_pk());
        let request_hash = CryptoHash::from(request_hash);
        for nonce in nonces {
            let nonce: [u8; 32] = nonce.0.try_into().expect("Nonce must be 32 bytes");
            // Fails if no operations wait with this yield id
            let _ = oracle_yield_id(&oracle_key, request_hash, nonce).resume(&response.0);
        }
    }

    /// Run the operations after the oracle request with the response that the
    /// oracle resumed them with, if it responded in time
    #[private]
    pub fn after_oracle_response(
        &mut self,
        oracle_key: PublicKeyHandle,
        dex_id: DexId,
        request: Base64VecU8,
        operations: Vec<Operation>,
        trader: Caller,
        held_assets: Option<HashMap<AssetId, U128>>,
        referrer: Option<AccountId>,
        referral_fees: ReferralFees,
    ) {
        let Ok(response) = near_sdk::env::promise_result_checked(0, MAX_ORACLE_RESPONSE_BYTES)
        else {
            panic!("Oracle didn't respond in time");
        };
        self.assert_not_paused();
        for (asset_id, amount) in held_assets.iter().flatten() {
            let in_custody = self
                .total_in_custody
                .get_mut(asset_id)
                .expect("Held asset is not registered");
            in_custody.0 = in_custody
                .0
                .checked_add(amount.0)
                .expect("Balance overflow");
        }
        let oracle_response = OracleResponse {
            oracle_key,
            dex_id,
            request: request.0,
            response,
        };
        let mut events = CallEvents::default();
        let waiting_for_oracles = self.internal_execute_operations(
            &mut events,
            operations,
            trader,
            held_assets,
            referrer,
            referral_fees,
            Some(&oracle_response),
        );
        expect!(
            waiting_for_oracles.is_none(),
            "Operations after an oracle request can't wait for an oracle again"
        );
        events.emit();
    }

    /// Refund the held assets if the operations after the oracle request didn't
    /// run. Returns what `*_on_transfer` returns for the deposit.
    #[private]
    pub fn after_oracle_operations(
        &self,
        trader: Caller,
        held_assets: Option<HashMap<AssetId, U128>>,
        #[callback_result] executed: Result<(), PromiseError>,
    ) -> Value {
        fn refund_return_value(trader: &Caller, refund: &HashMap<AssetId, U128>) -> Value {
            let Caller::OnTransfer {
                alleged_sender,
                transfer_of,
            } = trader
            else {
                return Value::Null;
            };
            expect!(
                refund.is_empty() || refund.len() == 1,
                "refund must contain exactly zero or one asset, the input asset"
            );
            let refund_amount = refund.get(transfer_of).copied().unwrap_or(U128(0));
            match transfer_of {
                AssetId::Near => {
                    if refund_amount.0 != 0 {
                        Promise::new(alleged_sender.clone())
                            .transfer(NearToken::from_yoctonear(refund_amount.0))
                            .detach();
                    }
                    Value::Null
                }
                AssetId::Nep141(_) => json!(refund_amount),
                AssetId::Nep171(..) => json!(refund_amount.0 != 0),
                AssetId::Nep245(..) => json!([refund_amount]),
            }
        }

        if executed.is_ok() {
            // refund nothing = return success
            return refund_return_value(&trader, &HashMap::new());
        }
        near_sdk::env::log_str("Operations after the oracle request failed");
        refund_return_value(&trader, &held_assets.unwrap_or_default())
    }
}

impl DexEngine {
    /// Make the operations after the oracle request wait for the oracle to
    /// respond, holding the sandboxed assets until then
    pub(crate) fn internal_wait_for_oracle(
        &mut self,
        mut operations: Vec<Operation>,
        trader: Caller,
        sandboxed_assets: Option<HashMap<AssetId, U128>>,
        referrer: Option<AccountId>,
        referral_fees: ReferralFees,
    ) -> Promise {
        let Operation::OracleRequest {
            oracle_key,
            dex_id,
            request,
        } = operations.remove(0)
        else {
            unreachable!("Operations that wait for an oracle start with an oracle request");
        };
        expect!(
            !operations
                .iter()
                .any(|operation| matches!(operation, Operation::OracleRequest { .. })),
            "Only the first operation can be an oracle request"
        );
        let nonce = near_sdk::env::random_seed_array();
        let yield_id = oracle_yield_id(&oracle_key, near_sdk::env::sha256_array(&request.0), nonce);
        IntearDexEvent::OracleRequest {
            oracle_key: oracle_key.clone(),
            dex_id: dex_id.clone(),
            request: request.clone(),
            nonce: Base64VecU8(nonce.to_vec()),
        }
        .emit();

        // Out of custody while held, so that if the operations don't run, they're
        // refunded without the engine tracking them
        for (asset_id, amount) in sandboxed_assets.iter().flatten() {
            let in_custody = self
                .total_in_custody
                .get_mut(asset_id)
                .expect("Sandboxed asset is not in custody");
            in_custody.0 = in_custody
                .0
                .checked_sub(amount.0)
                .expect("Sandboxed asset is not in custody");
        }
        Promise::new_yield_with_id(
            "after_oracle_response",
            json!({
                "oracle_key": oracle_key,
                "dex_id": dex_id,
                "request": request,
                "operations": operations,
                "trader": trader,
                "held_assets": sandboxed_assets,
                "referrer": referrer,
                "referral_fees": referral_fees,
            })
            .to_string(),
            NearToken::ZERO,
            Gas::from_gas(0),
            GasWeight(1),
            yield_id,
        )
        .expect("Operations with the same yield id already wait for an oracle")
        // Runs whether the operations ran, failed, or timed out, with gas of its own
        .then(
            Self::ext(near_sdk::env::current_account_id())
                .with_static_gas(GAS_FOR_AFTER_ORACLE_OPERATIONS)
                .with_unused_gas_weight(0)
                .after_oracle_operations(trader, sandboxed_assets),
        )
    }
}

/// The yield id of operations that wait for the oracle with `oracle_key` to
/// respond to the request with `request_hash`, so that only that oracle can
/// resume them, and only with a response to that request
fn oracle_yield_id(
    oracle_key: &PublicKeyHandle,
    request_hash: CryptoHash,
    nonce: [u8; 32],
) -> UserYieldId {
    UserYieldId::new(near_sdk::env::sha256_array(
        near_sdk::borsh::to_vec(&(oracle_key, request_hash, nonce)).unwrap(),
    ))
}
