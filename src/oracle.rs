use std::collections::{BTreeMap, HashMap};

use intear_dex_types::{AssetId, Caller, DexId, IntearDexEvent, Operation, ReferralFees, expect};
use near_sdk::{
    AccountId, Allowance, CurveType, Gas, GasWeight, NearToken, Promise, PromiseError, PublicKey,
    PublicKeyHandle, UserYieldId,
    json_types::{Base58CryptoHash, Base64VecU8, U128},
    near,
    serde_json::{Value, json},
};

use crate::{DexEngine, DexEngineExt, STORAGE_BYTES_PER_RECORD, call_events::CallEvents};

const MAX_WAITING_FOR_SAME_ORACLE_REQUEST: usize = 500;
const ORACLE_KEY_NONCES: u32 = 4;
const ORACLE_KEY_METHOD: &str = "oracle_respond";
// https://github.com/near/nearcore/blob/2.14.0/core/crypto/src/signature.rs#L331-L337
const ED25519_KEY_STORAGE_BYTES: u64 = 33;
const SECP256K1_KEY_STORAGE_BYTES: u64 = 65;
const ML_DSA_65_KEY_STORAGE_BYTES: u64 = 33;
// https://github.com/near/nearcore/blob/2.14.0/core/primitives-core/src/types.rs#L19-L21
const GAS_KEY_NONCE_STORAGE_BYTES: u64 = 8 + 2;
// https://github.com/near/nearcore/blob/2.14.0/core/parameters/res/runtime_configs/parameters.yaml#L321
const MAX_YIELD_PAYLOAD_SIZE: usize = 1024;

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

    /// Respond to requests to the oracle whose key signed the transaction, by
    /// the sha256 of the request.
    #[private]
    pub fn oracle_respond(&mut self, requests: BTreeMap<Base58CryptoHash, Base64VecU8>) {
        let oracle_key = PublicKeyHandle::from(near_sdk::env::signer_account_pk());
        for (request_hash, response) in requests {
            let waiting = self
                .oracle_requests
                .remove(&(oracle_key.clone(), *request_hash.as_ref()))
                .unwrap_or_default();
            for (yield_id, request_index) in waiting {
                let responses = self
                    .oracle_responses
                    .get_mut(&yield_id)
                    .expect("Waiting operations have no responses");
                responses[request_index as usize] = Some(response.0.clone());
                if responses.iter().any(Option::is_none) {
                    continue;
                }
                let responses: Vec<Vec<u8>> = self
                    .oracle_responses
                    .remove(&yield_id)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .collect();
                let payload = near_sdk::serde_json::to_vec(&responses).unwrap();
                // Operations whose requests could be crafted to fail the whole response
                // are left to time out
                if payload.len() > MAX_YIELD_PAYLOAD_SIZE {
                    near_sdk::env::log_str(&format!(
                        "Responses are {} bytes, more than {MAX_YIELD_PAYLOAD_SIZE} bytes that operations can get",
                        payload.len()
                    ));
                    continue;
                }
                // Fails if the operations timed out, but haven't removed their requests yet
                let _ = yield_id.resume(payload);
            }
        }
    }

    /// Run the operations after the oracle requests, once oracles responded to
    /// all of them, or refund the held assets if they didn't in time. Returns
    /// what `*_on_transfer` returns for the deposit.
    #[private]
    #[payable]
    pub fn after_oracle_responses(
        &mut self,
        yield_id: UserYieldId,
        requests: Vec<(PublicKeyHandle, DexId, Base64VecU8)>,
        operations: Vec<Operation>,
        trader: Caller,
        held_assets: Option<HashMap<AssetId, U128>>,
        referrer: Option<AccountId>,
        referral_fees: ReferralFees,
        #[callback_result] responses: Result<Vec<Base64VecU8>, PromiseError>,
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

        let Ok(responses) = responses else {
            self.oracle_responses.remove(&yield_id);
            for (oracle_key, _, request) in requests {
                let request_key = (oracle_key, near_sdk::env::sha256_array(&request.0));
                if let Some(waiting) = self.oracle_requests.get_mut(&request_key) {
                    waiting.retain(|(waiting_yield_id, _)| *waiting_yield_id != yield_id);
                    if waiting.is_empty() {
                        self.oracle_requests.remove(&request_key);
                    }
                }
            }
            near_sdk::env::log_str("Oracles didn't respond in time");
            return refund_return_value(&trader, &held_assets.unwrap_or_default());
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
        let oracle_responses = requests
            .into_iter()
            .zip(responses)
            .map(|((oracle_key, dex_id, request), response)| {
                ((oracle_key, dex_id, request.0), response.into())
            })
            .collect();
        let mut events = CallEvents::default();
        let waiting_for_oracles = self.internal_execute_operations(
            &mut events,
            operations,
            trader.clone(),
            held_assets,
            referrer,
            referral_fees,
            &oracle_responses,
        );
        expect!(
            waiting_for_oracles.is_none(),
            "Operations after oracle requests can't wait for oracles again"
        );
        events.emit();

        // refund nothing = return success
        refund_return_value(&trader, &HashMap::new())
    }
}

impl DexEngine {
    /// Make the operations after the oracle requests wait for
    /// oracles to respond, holding the sandboxed assets until then
    pub(crate) fn internal_wait_for_oracles(
        &mut self,
        mut operations: Vec<Operation>,
        trader: Caller,
        sandboxed_assets: Option<HashMap<AssetId, U128>>,
        referrer: Option<AccountId>,
        referral_fees: ReferralFees,
    ) -> Promise {
        let yield_id = UserYieldId::new(near_sdk::env::random_seed_array());
        let request_count = operations
            .iter()
            .take_while(|operation| matches!(operation, Operation::OracleRequest { .. }))
            .count();
        let operations_after_requests = operations.split_off(request_count);
        let mut requests = Vec::new();
        for (request_index, operation) in operations.iter().enumerate() {
            let Operation::OracleRequest {
                oracle_key,
                dex_id,
                request,
            } = operation
            else {
                unreachable!("Only oracle requests are left");
            };
            let waiting = self
                .oracle_requests
                .entry((oracle_key.clone(), near_sdk::env::sha256_array(&request.0)))
                .or_default();
            expect!(
                waiting.len() < MAX_WAITING_FOR_SAME_ORACLE_REQUEST,
                "{MAX_WAITING_FOR_SAME_ORACLE_REQUEST} operations already wait for the same request to {oracle_key}"
            );
            waiting.push((yield_id, request_index.try_into().unwrap()));
            IntearDexEvent::OracleRequest {
                oracle_key: oracle_key.clone(),
                dex_id: dex_id.clone(),
                request: request.clone(),
            }
            .emit();
            requests.push((oracle_key, dex_id, request));
        }
        self.oracle_responses
            .insert(yield_id, vec![None; requests.len()]);

        // Out of custody while held, so that if the operations fail, the
        // asset's contract refunds it without the engine tracking it
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
        let held_near = sandboxed_assets
            .as_ref()
            .and_then(|assets| assets.get(&AssetId::Near))
            .map_or(NearToken::ZERO, |amount| {
                NearToken::from_yoctonear(amount.0)
            });
        Promise::new_yield_with_id(
            "after_oracle_responses",
            json!({
                "yield_id": yield_id,
                "requests": requests,
                "operations": operations_after_requests,
                "trader": trader,
                "held_assets": sandboxed_assets,
                "referrer": referrer,
                "referral_fees": referral_fees,
            })
            .to_string(),
            held_near,
            Gas::from_gas(0),
            GasWeight(1),
            yield_id,
        )
        .expect("Operations with the same yield id already wait for oracles")
        // NEAR is held in the attached deposit, which goes back if the operations fail
        .refund_to(trader.alleged_account_id().clone())
    }
}
