use std::collections::HashMap;

use intear_dex_common::{AssetId, Caller, DexId, TokenAmount, expect};
use intear_dex_types::{AccountOrDexId, IntearDexEvent, Operation, ReferralFees};
use near_sdk::{
    AccountId, Allowance, CryptoHash, Gas, GasWeight, NearToken, Promise, PromiseError,
    PromiseOrValue, PublicKey, PublicKeyHandle, UserYieldId,
    json_types::{Base58CryptoHash, Base64VecU8},
    near,
    serde_json::{Value, json},
};

use crate::{
    DexEngine, DexEngineExt, OracleResponse, STORAGE_BYTES_PER_RECORD, call_events::CallEvents,
};

/// Also includes gas of the withdrawal that refunds a deposit
pub const GAS_FOR_AFTER_ORACLE_OPERATIONS: Gas = Gas::from_tgas(30);
const GAS_FOR_FAILING_AFTER_ORACLE_OPERATIONS: Gas = Gas::from_tgas(2);

// https://github.com/near/nearcore/blob/2.14.0/core/parameters/res/runtime_configs/parameters.yaml#L321
pub const MAX_ORACLE_RESPONSE_BYTES: usize = 1024;

pub const MAX_ORACLE_REQUEST_BYTES: usize = 1024;

const ORACLE_KEY_NONCES: u32 = 4;
const ORACLE_KEY_METHODS: [&str; 2] = ["oracle_respond", "remove_oracle_key"];
const GAS_FOR_AFTER_ORACLE_KEY_CREATION: Gas = Gas::from_tgas(10);
const GAS_FOR_AFTER_ORACLE_KEY_REMOVAL: Gas = Gas::from_tgas(10);

// https://github.com/near/nearcore/blob/2.14.0/core/primitives-core/src/types.rs#L19-L21
const GAS_KEY_NONCE_STORAGE_BYTES: u64 = 8 + 2;

#[near]
impl DexEngine {
    /// Add a gas key that can only call `oracle_respond` and `remove_oracle_key`,
    /// for an oracle to respond with. The transaction is signed with the key, as
    /// a full access key of the account that creates it, so that only the oracle
    /// that has the key can name its fee receiver, which is the account that
    /// calls this. The attached NEAR pays for the storage of the key, and the
    /// rest of it is the gas balance. The storage balance of the fee receiver
    /// pays for the record of it, as it does for the oracle's fees.
    #[payable]
    pub fn create_oracle_key(&mut self, oracle_key: PublicKey) -> Promise {
        self.assert_not_paused();
        expect!(
            near_sdk::env::signer_account_pk() == oracle_key,
            "The transaction that creates an oracle key must be signed with it"
        );
        let fee_receiver_id = near_sdk::env::predecessor_account_id();
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
                current_account_id.clone(),
                ORACLE_KEY_METHODS.join(","),
            )
            .transfer_to_gas_key(oracle_key.clone(), gas_key_balance)
            // Pays for the key's storage once it's added, and is refunded with the
            // gas balance if it can't be
            .transfer(storage_cost)
            .refund_to(fee_receiver_id.clone())
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
        let storage_usage_before = near_sdk::env::storage_usage();
        self.oracle_fee_receivers
            .insert(PublicKeyHandle::from(oracle_key), fee_receiver_id.clone());
        self.oracle_fee_receivers.flush();
        self.user_storage_balances.charge(
            &fee_receiver_id,
            storage_usage_before,
            near_sdk::env::storage_usage(),
        );
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

    /// Delete the oracle key that signed the transaction with its fee receiver
    /// and fees, whose storage the storage balance of the fee receiver stops
    /// paying for, and send the NEAR that paid for the storage of the key to
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
        let storage_usage_before = near_sdk::env::storage_usage();
        let fee_receiver_id = self.oracle_fee_receivers.remove(&oracle_key_handle);
        self.oracle_fee_receivers.flush();
        self.oracle_fees.remove(&oracle_key_handle);
        self.oracle_fees.flush();
        expect!(let Some(fee_receiver_id) = fee_receiver_id, "No fee receiver for this oracle key");
        self.user_storage_balances.charge(
            &fee_receiver_id,
            storage_usage_before,
            near_sdk::env::storage_usage(),
        );
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
    /// didn't run. A deposit is refunded by withdrawing it to its sender, since
    /// its `*_on_transfer` said that it used the whole deposit, so that a
    /// failure of this can't make the token contract refund a deposit that the
    /// operations used.
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
                    self.internal_withdraw_unchecked(
                        transfer_of,
                        refund_amount,
                        alleged_sender.clone(),
                        AccountOrDexId::Account(alleged_sender),
                    )
                    .detach();
                }
                PromiseOrValue::Value(Value::Null)
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
        waiting.then(
            Self::ext(near_sdk::env::current_account_id())
                .with_static_gas(GAS_FOR_AFTER_ORACLE_OPERATIONS)
                .with_unused_gas_weight(0)
                .after_oracle_operations(trader, sandboxed_assets, oracle_fee),
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
