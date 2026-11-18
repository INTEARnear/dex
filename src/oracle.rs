use std::collections::HashMap;

use intear_dex_common::{AssetId, Caller, DexId, TokenAmount, expect};
use intear_dex_types::{AccountOrDexId, IntearDexEvent, Operation, ReferralFees};
use near_sdk::{
    AccountId, Allowance, CryptoHash, Gas, GasWeight, IntoStorageKey, NearToken, Promise,
    PromiseError, PromiseOrValue, PublicKey, PublicKeyHandle, UserYieldId,
    json_types::{Base58CryptoHash, Base64VecU8},
    near,
    serde_json::{Value, json},
};

use crate::{
    DexEngine, DexEngineExt, OracleResponse, STORAGE_BYTES_PER_RECORD, StorageKey,
    call_events::CallEvents,
};

pub const GAS_FOR_AFTER_ORACLE_OPERATIONS: Gas = Gas::from_tgas(10);
const GAS_FOR_FAILING_AFTER_ORACLE_OPERATIONS: Gas = Gas::from_tgas(2);
pub const GAS_FOR_ABSORBING_DEPOSIT_IF_REFUND_FAILED: Gas = Gas::from_tgas(10);

/// More than the longest result of `after_oracle_operations_in_sandbox`, an
/// array of the largest amount
const MAX_REFUND_RESULT_BYTES: usize = 64;

// https://github.com/near/nearcore/blob/2.14.0/core/parameters/res/runtime_configs/parameters.yaml#L321
pub const MAX_ORACLE_RESPONSE_BYTES: usize = 1024;

pub const MAX_ORACLE_REQUEST_BYTES: usize = 1024;

const ORACLE_KEY_NONCES: u32 = 4;
const ORACLE_KEY_METHODS: [&str; 2] = ["oracle_respond", "remove_oracle_key"];
const GAS_FOR_AFTER_ORACLE_KEY_CREATION: Gas = Gas::from_tgas(5);
const GAS_FOR_AFTER_ORACLE_KEY_REMOVAL: Gas = Gas::from_tgas(10);

// https://github.com/near/nearcore/blob/2.14.0/core/primitives-core/src/types.rs#L19-L21
const GAS_KEY_NONCE_STORAGE_BYTES: u64 = 8 + 2;

#[near]
impl DexEngine {
    /// Add a gas key that can only call `oracle_respond` and `remove_oracle_key`,
    /// for an oracle to respond with. `fee_receiver_id` takes the oracle's fees.
    /// The attached NEAR pays for the storage of the key and its fee receiver, and
    /// the rest of it is the gas balance.
    #[payable]
    pub fn create_oracle_key(
        &mut self,
        oracle_key: PublicKey,
        fee_receiver_id: AccountId,
    ) -> Promise {
        self.assert_not_paused();
        let storage_cost = oracle_key_storage_cost(&oracle_key, Some(&fee_receiver_id));
        let gas_key_balance = near_sdk::env::attached_deposit()
            .checked_sub(storage_cost)
            .unwrap_or_else(|| panic!("Attach at least {storage_cost} for the storage of the key"));
        let current_account_id = near_sdk::env::current_account_id();
        Promise::new(current_account_id.clone())
            .add_gas_key_allowance_function_call(
                oracle_key.clone(),
                ORACLE_KEY_NONCES,
                Allowance::Unlimited,
                current_account_id.clone(),
                ORACLE_KEY_METHODS.join(","),
            )
            .transfer_to_gas_key(oracle_key.clone(), gas_key_balance)
            // Pays for the storage of the key and its fee receiver once the key is
            // added, and is refunded with the gas balance if it can't be
            .transfer(storage_cost)
            .refund_to(near_sdk::env::predecessor_account_id())
            .then(
                Self::ext(current_account_id)
                    .with_static_gas(GAS_FOR_AFTER_ORACLE_KEY_CREATION)
                    .after_oracle_key_creation(oracle_key, fee_receiver_id),
            )
    }

    #[private]
    pub fn after_oracle_key_creation(
        &mut self,
        oracle_key: PublicKey,
        fee_receiver_id: AccountId,
        #[callback_result] created: Result<(), PromiseError>,
    ) {
        expect!(created.is_ok(), "Failed to add the oracle key");
        self.oracle_fee_receivers
            .insert(PublicKeyHandle::from(oracle_key), fee_receiver_id);
    }

    /// Set what an oracle takes for a request in each asset, replacing its
    /// fees in all assets.
    #[payable]
    pub fn set_oracle_fees(
        &mut self,
        oracle_key: PublicKeyHandle,
        fees: HashMap<AssetId, TokenAmount>,
    ) {
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
        let fee_receiver_id = near_sdk::env::predecessor_account_id();
        expect!(
            self.oracle_fee_receivers.get(&oracle_key) == Some(&fee_receiver_id),
            "Only the fee receiver of the oracle can set its fees"
        );
        for asset_id in fees.keys() {
            self.assert_asset_registered(
                AccountOrDexId::Account(fee_receiver_id.clone()),
                asset_id.clone(),
            );
        }
        let storage_usage_before = near_sdk::env::storage_usage();
        self.oracle_fees.insert(oracle_key, fees);
        self.oracle_fees.flush();
        self.user_storage_balances.charge(
            &fee_receiver_id,
            storage_usage_before,
            near_sdk::env::storage_usage(),
        );
    }

    pub fn oracle_fee_receiver(&self, oracle_key: PublicKeyHandle) -> Option<AccountId> {
        self.oracle_fee_receivers.get(&oracle_key).cloned()
    }

    /// What an oracle takes for a request in each asset that it takes fees in
    pub fn oracle_fees(&self, oracle_key: PublicKeyHandle) -> HashMap<AssetId, TokenAmount> {
        self.oracle_fees
            .get(&oracle_key)
            .cloned()
            .unwrap_or_default()
    }

    /// Delete the oracle key that signed the transaction with its fees, and send
    /// the NEAR that paid for the storage of the key and its fee receiver to
    /// `receiver_id`. Deleting a gas key burns its balance, and fails if the
    /// balance is more than 1 NEAR.
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
        &mut self,
        oracle_key: PublicKey,
        receiver_id: AccountId,
        #[callback_result] removed: Result<(), PromiseError>,
    ) -> Promise {
        expect!(removed.is_ok(), "Failed to remove the oracle key");
        let oracle_key_handle = PublicKeyHandle::from(&oracle_key);
        // Keys from before oracles had fee receivers have none, and no fees
        let fee_receiver_id = self.oracle_fee_receivers.remove(&oracle_key_handle);
        self.oracle_fee_receivers.flush();
        if let Some(fee_receiver_id) = &fee_receiver_id {
            let storage_usage_before = near_sdk::env::storage_usage();
            self.oracle_fees.remove(&oracle_key_handle);
            self.oracle_fees.flush();
            self.user_storage_balances.charge(
                fee_receiver_id,
                storage_usage_before,
                near_sdk::env::storage_usage(),
            );
        }
        Promise::new(receiver_id).transfer(oracle_key_storage_cost(
            &oracle_key,
            fee_receiver_id.as_ref(),
        ))
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
            near_sdk::env::signer_account_id() == near_sdk::env::current_account_id(),
            "Only the dex's account itself can call oracle_respond",
        );
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
    /// oracle resumed them with, if it responded in time. Only fails if the
    /// oracle responded, which `after_oracle_operations` pays the oracle for.
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
    ) -> OracleRequestOutcome {
        let Ok(response) = near_sdk::env::promise_result_checked(0, MAX_ORACLE_RESPONSE_BYTES)
        else {
            near_sdk::env::log_str("Oracle didn't respond in time");
            return OracleRequestOutcome::OracleTimedOut;
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
        OracleRequestOutcome::OperationsRan
    }

    /// Pay the oracle its fee if it responded, or refund the fee if it didn't,
    /// and refund the held deposit if the operations after the oracle request
    /// didn't run. For a deposit, returns what `*_on_transfer` returns for it,
    /// or for NEAR, the amount refunded. For `execute_operations`, fails if the
    /// operations didn't run, in a call of its own that keeps what this one did.
    #[private]
    pub fn after_oracle_operations(
        &mut self,
        trader: Caller,
        held_assets: Option<HashMap<AssetId, TokenAmount>>,
        oracle_fee: OracleFee,
        #[callback_result] outcome: Result<OracleRequestOutcome, PromiseError>,
    ) -> PromiseOrValue<Value> {
        let mut events = CallEvents::default();
        if outcome.is_err() {
            near_sdk::env::log_str("Operations after the oracle request failed");
        }
        let oracle_timed_out = matches!(outcome, Ok(OracleRequestOutcome::OracleTimedOut));
        let operations_ran = matches!(outcome, Ok(OracleRequestOutcome::OperationsRan));
        if !oracle_timed_out {
            self.internal_increase_assets(
                &mut events,
                AccountOrDexId::Account(oracle_fee.receiver_id),
                oracle_fee.asset_id.clone(),
                oracle_fee.amount,
            );
        }
        let returned = match trader {
            Caller::Direct(account_id) => {
                if oracle_timed_out {
                    self.internal_increase_assets(
                        &mut events,
                        AccountOrDexId::Account(account_id),
                        oracle_fee.asset_id,
                        oracle_fee.amount,
                    );
                }
                if operations_ran {
                    PromiseOrValue::Value(Value::Null)
                } else {
                    PromiseOrValue::Promise(
                        Self::ext(near_sdk::env::current_account_id())
                            .with_static_gas(GAS_FOR_FAILING_AFTER_ORACLE_OPERATIONS)
                            .with_unused_gas_weight(0)
                            .fail_after_oracle_operations(oracle_timed_out),
                    )
                }
            }
            Caller::OnTransfer {
                alleged_sender,
                transfer_of,
            } => {
                let mut refund_amount = if operations_ran {
                    TokenAmount::ZERO
                } else {
                    let held_assets = held_assets.expect("A deposit is held");
                    expect!(
                        held_assets.keys().all(|asset_id| *asset_id == transfer_of),
                        "Only the deposited {transfer_of} can be held"
                    );
                    held_assets.get(&transfer_of).copied().unwrap_or_default()
                };
                if oracle_timed_out {
                    // The fee was taken from the deposit
                    refund_amount = refund_amount
                        .checked_add(oracle_fee.amount)
                        .expect("Refund overflow");
                }
                if !refund_amount.is_zero() {
                    let in_custody = self
                        .total_in_custody
                        .get_mut(&transfer_of)
                        .expect("Held asset is not in custody");
                    *in_custody = in_custody
                        .checked_sub(refund_amount)
                        .expect("Held asset is not in custody");
                }
                PromiseOrValue::Value(match transfer_of {
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
                })
            }
        };
        events.emit();
        returned
    }

    /// Fails the transaction of an `execute_operations` whose operations after
    /// the oracle request didn't run
    #[private]
    pub fn fail_after_oracle_operations(oracle_timed_out: bool) {
        if oracle_timed_out {
            panic!("Oracle didn't respond in time");
        }
        panic!("Operations after the oracle request failed");
    }

    /// Passes on what `after_oracle_operations` returned, or if it
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

#[near(serializers=[json])]
pub enum OracleRequestOutcome {
    OperationsRan,
    OracleTimedOut,
}

#[near(serializers=[json])]
pub struct OracleFee {
    pub receiver_id: AccountId,
    pub asset_id: AssetId,
    pub amount: TokenAmount,
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
    /// respond, holding the oracle's fee and the sandboxed assets until then
    pub(crate) fn internal_wait_for_oracle(
        &mut self,
        events: &mut CallEvents,
        mut operations: Vec<Operation>,
        trader: Caller,
        mut sandboxed_assets: Option<HashMap<AssetId, TokenAmount>>,
        referrer: Option<AccountId>,
        referral_fees: ReferralFees,
    ) -> Promise {
        let Operation::OracleRequest {
            oracle_key,
            dex_id,
            request,
            fee: (fee_asset, fee),
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
        let Some(fee_receiver_id) = self.oracle_fee_receivers.get(&oracle_key) else {
            panic!("Oracle {oracle_key} has no fee receiver");
        };
        let Some(fee_of_oracle) = self
            .oracle_fees
            .get(&oracle_key)
            .and_then(|fees| fees.get(&fee_asset))
            .copied()
        else {
            panic!("Oracle {oracle_key} doesn't take fees in {fee_asset}");
        };
        expect!(
            fee == fee_of_oracle,
            "The fee of oracle {oracle_key} is {fee_of_oracle} {fee_asset}, not {fee}"
        );
        let oracle_fee = OracleFee {
            receiver_id: fee_receiver_id.clone(),
            asset_id: fee_asset.clone(),
            amount: fee,
        };
        match &mut sandboxed_assets {
            Some(sandboxed_assets) => {
                let Some(deposit) = sandboxed_assets.get_mut(&fee_asset) else {
                    panic!("The oracle's fee must be in the deposited asset, not {fee_asset}");
                };
                *deposit = deposit.checked_sub(fee).unwrap_or_else(|| {
                    panic!(
                        "Deposit of {deposit} {fee_asset} is less than the oracle's fee of {fee}"
                    )
                });
            }
            None => self.internal_decrease_assets(
                events,
                AccountOrDexId::Account(trader.alleged_account_id().clone()),
                fee_asset,
                fee,
            ),
        }
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
        let settled = waiting.then(
            Self::ext(near_sdk::env::current_account_id())
                .with_static_gas(GAS_FOR_AFTER_ORACLE_OPERATIONS)
                .with_unused_gas_weight(0)
                .after_oracle_operations(trader.clone(), sandboxed_assets, oracle_fee),
        );
        match trader {
            Caller::Direct(_) => settled,
            Caller::OnTransfer { transfer_of, .. } => {
                let standard = match transfer_of {
                    AssetId::Near => DepositStandard::Near,
                    AssetId::Nep141(_) => DepositStandard::Nep141,
                    AssetId::Nep171(..) => DepositStandard::Nep171,
                    AssetId::Nep245(..) => DepositStandard::Nep245,
                };
                settled.then(
                    Self::ext(near_sdk::env::current_account_id())
                        .with_static_gas(GAS_FOR_ABSORBING_DEPOSIT_IF_REFUND_FAILED)
                        .with_unused_gas_weight(0)
                        .absorb_deposit_if_refund_failed(standard),
                )
            }
        }
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

/// The NEAR that the storage of an oracle key and its fee receiver takes
fn oracle_key_storage_cost(
    oracle_key: &PublicKey,
    fee_receiver_id: Option<&AccountId>,
) -> NearToken {
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
    let fee_receiver_bytes = fee_receiver_id.map_or(0, |fee_receiver_id| {
        (StorageKey::OracleFeeReceivers.into_storage_key().len() as u64)
            .checked_add(key_bytes)
            .unwrap()
            .checked_add(near_sdk::borsh::object_length(fee_receiver_id).unwrap() as u64)
            .unwrap()
            .checked_add(STORAGE_BYTES_PER_RECORD)
            .unwrap()
    });
    near_sdk::env::storage_byte_cost().saturating_mul(u128::from(
        key_bytes
            .checked_add(access_key_bytes)
            .unwrap()
            .checked_add(STORAGE_BYTES_PER_RECORD)
            .unwrap()
            .checked_add(fee_receiver_bytes)
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
