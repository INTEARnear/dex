use std::collections::HashMap;

use intear_dex_common::{AssetId, Caller, DexId, TokenAmount, expect};
use intear_dex_types::{IntearDexEvent, Operation, ReferralFees};
use near_sdk::{
    AccountId, Allowance, CryptoHash, Gas, GasWeight, NearToken, Promise, PromiseError, PublicKey,
    PublicKeyHandle, UserYieldId,
    json_types::{Base58CryptoHash, Base64VecU8},
    near,
    serde_json::{Value, json},
};

use crate::{
    DexEngine, DexEngineExt, OracleResponse, STORAGE_BYTES_PER_RECORD, call_events::CallEvents,
};

pub const GAS_FOR_AFTER_ORACLE_OPERATIONS_IN_SANDBOX: Gas = Gas::from_tgas(5);
pub const GAS_FOR_ABSORBING_DEPOSIT_IF_REFUND_FAILED: Gas = Gas::from_tgas(10);

/// More than the longest result of `after_oracle_operations_in_sandbox`, an
/// array of the largest amount
const MAX_REFUND_RESULT_BYTES: usize = 64;

// https://github.com/near/nearcore/blob/2.14.0/core/parameters/res/runtime_configs/parameters.yaml#L321
pub const MAX_ORACLE_RESPONSE_BYTES: usize = 1024;

pub const MAX_ORACLE_REQUEST_BYTES: usize = 1024;

const ORACLE_KEY_NONCES: u32 = 4;
const ORACLE_KEY_METHODS: [&str; 2] = ["oracle_respond", "remove_oracle_key"];
const GAS_FOR_AFTER_ORACLE_KEY_REMOVAL: Gas = Gas::from_tgas(5);

// https://github.com/near/nearcore/blob/2.14.0/core/primitives-core/src/types.rs#L19-L21
const GAS_KEY_NONCE_STORAGE_BYTES: u64 = 8 + 2;

#[near]
impl DexEngine {
    /// Add a gas key that can only call `oracle_respond` and `remove_oracle_key`,
    /// for an oracle to respond with. The attached NEAR pays for the key's
    /// storage, and the rest of it is the gas balance.
    #[payable]
    pub fn create_oracle_key(&mut self, oracle_key: PublicKey) -> Promise {
        self.assert_not_paused();
        let storage_cost = oracle_key_storage_cost(&oracle_key);
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
                ORACLE_KEY_METHODS.join(","),
            )
            .transfer_to_gas_key(oracle_key, gas_key_balance)
            // Pays for the key's storage once it's added, and is refunded with the
            // gas balance if it can't be
            .transfer(storage_cost)
            .refund_to(near_sdk::env::predecessor_account_id())
    }

    /// Delete the oracle key that signed the transaction, and send the NEAR that
    /// paid for its storage to `receiver_id`. Deleting a gas key burns its
    /// balance, and fails if the balance is more than 1 NEAR.
    ///
    /// https://github.com/near/nearcore/blob/2.14.0/core/primitives-core/src/account.rs#L888
    #[private]
    pub fn remove_oracle_key(&self, receiver_id: AccountId) -> Promise {
        let oracle_key = near_sdk::env::signer_account_pk();
        let current_account_id = near_sdk::env::current_account_id();
        Promise::new(current_account_id.clone())
            .delete_key(oracle_key.clone())
            .then(
                Self::ext(current_account_id)
                    .with_static_gas(GAS_FOR_AFTER_ORACLE_KEY_REMOVAL)
                    .after_oracle_key_removal(oracle_key, receiver_id),
            )
    }

    #[private]
    pub fn after_oracle_key_removal(
        &self,
        oracle_key: PublicKey,
        receiver_id: AccountId,
        #[callback_result] removed: Result<(), PromiseError>,
    ) -> Promise {
        expect!(removed.is_ok(), "Failed to remove the oracle key");
        Promise::new(receiver_id).transfer(oracle_key_storage_cost(&oracle_key))
    }

    /// Add the attached NEAR to the gas balance of an oracle key
    #[payable]
    pub fn top_up_oracle_key(&mut self, oracle_key: PublicKey) -> Promise {
        self.assert_not_paused();
        Promise::new(near_sdk::env::current_account_id())
            .transfer_to_gas_key(oracle_key, near_sdk::env::attached_deposit())
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
        held_assets: Option<HashMap<AssetId, TokenAmount>>,
        referrer: Option<AccountId>,
        referral_fees: ReferralFees,
    ) {
        let Ok(response) = near_sdk::env::promise_result_checked(0, MAX_ORACLE_RESPONSE_BYTES)
        else {
            panic!("Oracle didn't respond in time");
        };
        self.assert_not_paused();
        let oracle_response = OracleResponse {
            oracle_key,
            dex_id,
            request: request.0,
            response,
        };
        let mut events = CallEvents::default();
        if self
            .internal_execute_operations(
                &mut events,
                operations,
                trader,
                held_assets,
                referrer,
                referral_fees,
                Some(&oracle_response),
            )
            .is_some()
        {
            unreachable!("Operations after an oracle request don't have another one");
        };
        events.emit();
    }

    /// Refund the held deposit if the operations after the oracle request
    /// didn't run. Returns what `*_on_transfer` returns for the deposit, or
    /// for NEAR, the amount refunded.
    #[private]
    pub fn after_oracle_operations_in_sandbox(
        &mut self,
        alleged_sender: AccountId,
        transfer_of: AssetId,
        held_assets: HashMap<AssetId, TokenAmount>,
        #[callback_result] executed: Result<(), PromiseError>,
    ) -> Value {
        let refund_amount = if executed.is_ok() {
            TokenAmount::ZERO
        } else {
            near_sdk::env::log_str("Operations after the oracle request failed");
            expect!(
                held_assets.keys().all(|asset_id| *asset_id == transfer_of),
                "Only the deposited {transfer_of} can be held"
            );
            for (asset_id, amount) in &held_assets {
                let in_custody = self
                    .total_in_custody
                    .get_mut(asset_id)
                    .expect("Held asset is not in custody");
                *in_custody = in_custody
                    .checked_sub(*amount)
                    .expect("Held asset is not in custody");
            }
            held_assets.get(&transfer_of).copied().unwrap_or_default()
        };
        match transfer_of {
            AssetId::Near => {
                if !refund_amount.is_zero() {
                    Promise::new(alleged_sender)
                        .transfer(NearToken::from_yoctonear(refund_amount.get()))
                        .detach();
                }
                json!(refund_amount)
            }
            AssetId::Nep141(_) => json!(refund_amount),
            AssetId::Nep171(..) => json!(!refund_amount.is_zero()),
            AssetId::Nep245(..) => json!([refund_amount]),
        }
    }

    /// Passes on what `after_oracle_operations_in_sandbox` returned, or if it
    /// failed, says that the whole deposit was used, since the token contract
    /// would refund a deposit that the operations may have already used. What
    /// it didn't refund stays on the engine.
    ///
    /// NEVER EDIT THIS FUNCTION OR `DepositStandard`. It must never fail, so it
    /// doesn't read the state, and its arguments are small and of a fixed size.
    #[private]
    pub fn absorb_deposit_if_refund_failed(standard: DepositStandard) {
        let fully_used: &[u8] = match standard {
            DepositStandard::Near | DepositStandard::Nep141 => br#""0""#,
            DepositStandard::Nep171 => b"false",
            DepositStandard::Nep245 => br#"["0"]"#,
        };
        match near_sdk::env::promise_result_checked(0, MAX_REFUND_RESULT_BYTES) {
            Ok(refund) => near_sdk::env::value_return(&refund),
            Err(_) => near_sdk::env::value_return(fully_used),
        }
    }
}

/// The standard of a deposit that waits for an oracle, which decides how its
/// `*_on_transfer` says that the whole deposit was used
#[near(serializers=[json])]
#[derive(Clone, Copy)]
pub enum DepositStandard {
    Near,
    Nep141,
    Nep171,
    Nep245,
}

impl DexEngine {
    /// Make the operations after the oracle request wait for the oracle to
    /// respond, holding the sandboxed assets until then
    pub(crate) fn internal_wait_for_oracle(
        &mut self,
        mut operations: Vec<Operation>,
        trader: Caller,
        sandboxed_assets: Option<HashMap<AssetId, TokenAmount>>,
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
        expect!(
            request.0.len() <= MAX_ORACLE_REQUEST_BYTES,
            "Request is more than {MAX_ORACLE_REQUEST_BYTES} bytes"
        );
        expect!(
            !operations
                .iter()
                .any(|operation| matches!(operation, Operation::DeployDexCode { .. })),
            "Dex code can't be deployed after an oracle request"
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

        let waiting = Promise::new_yield_with_id(
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
        .expect("Operations with the same yield id already wait for an oracle");
        // With nothing held, there's nothing to refund, and the result of the
        // operations is the result of the transaction
        let Some(held_assets) = sandboxed_assets else {
            return waiting;
        };
        let Caller::OnTransfer {
            alleged_sender,
            transfer_of,
        } = trader
        else {
            unreachable!("sandboxed_assets is Some but caller is not OnTransfer")
        };
        let standard = match &transfer_of {
            AssetId::Near => DepositStandard::Near,
            AssetId::Nep141(_) => DepositStandard::Nep141,
            AssetId::Nep171(..) => DepositStandard::Nep171,
            AssetId::Nep245(..) => DepositStandard::Nep245,
        };
        // Both run whether the operations ran, failed, or timed out, with gas of their own
        waiting
            .then(
                Self::ext(near_sdk::env::current_account_id())
                    .with_static_gas(GAS_FOR_AFTER_ORACLE_OPERATIONS_IN_SANDBOX)
                    .with_unused_gas_weight(0)
                    .after_oracle_operations_in_sandbox(alleged_sender, transfer_of, held_assets),
            )
            .then(
                Self::ext(near_sdk::env::current_account_id())
                    .with_static_gas(GAS_FOR_ABSORBING_DEPOSIT_IF_REFUND_FAILED)
                    .with_unused_gas_weight(0)
                    .absorb_deposit_if_refund_failed(standard),
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

/// The NEAR that the storage of an oracle key takes
fn oracle_key_storage_cost(oracle_key: &PublicKey) -> NearToken {
    let key_bytes =
        near_sdk::borsh::object_length(&PublicKeyHandle::from(oracle_key)).unwrap() as u64;
    // Borsh of the access key: its nonce, the tag of its permission, the gas key's
    // balance and number of nonces, unlimited allowance, receiver and methods
    let access_key_bytes = near_sdk::borsh::object_length(&(
        0u64,
        0u8,
        0u128,
        0u16,
        None::<u128>,
        near_sdk::env::current_account_id(),
        ORACLE_KEY_METHODS.to_vec(),
    ))
    .unwrap() as u64;
    near_sdk::env::storage_byte_cost().saturating_mul(u128::from(
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
            .unwrap(),
    ))
}
