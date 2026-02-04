#![deny(clippy::arithmetic_side_effects)]
#![allow(clippy::too_many_arguments)]

pub mod asset_deposit;
pub mod host_functions;
pub mod internal_asset_operations;
pub mod internal_operations;
pub mod storage_management;

use std::collections::HashMap;

use crate::{
    internal_asset_operations::AccountOrDexId,
    internal_operations::{DirectWithdrawAmount, Operation, TradeAccount},
    storage_management::StorageBalances,
};
use intear_dex_types::{AssetId, DexId, SwapRequest, SwapRequestAmount, expect};
use near_sdk::{
    AccountId, BorshStorageKey, NearToken, PromiseOrValue,
    json_types::{Base58CryptoHash, Base64VecU8, U128},
    near,
    store::{IterableMap, LookupMap},
};

#[near(contract_state)]
pub struct DexEngine {
    /// Assets that are custodied by the dex engine contract
    /// for the dexes that run inside it. Other dexes or users
    /// can't access other dexes' balances.
    dex_balances: LookupMap<(DexId, AssetId), U128>,
    /// Persistent storage for each dex, similar to contract
    /// storage of traditional smart contract dexes. It's
    /// public, but currently there's no way to access other
    /// dexes' storage from dex runtime.
    dex_storage: DexStorage,
    /// Wasm code for each dex.
    dex_codes: LookupMap<DexId, Vec<u8>>,
    /// Storage balances for each dex, translated to storage
    /// of this smart contract. use dex_* methods to interact
    /// with it, such as dex_storage_deposit.
    pub dex_storage_balances: StorageBalances<DexId>,
    /// Balances for each user, custodied by the dex engine
    /// contract for faster access. This reduces the need for
    /// ft_transfer_call, which takes time.
    user_balances: LookupMap<(AccountId, AssetId), U128>,
    /// Storage balances for each user, translated to storage
    /// of this smart contract. use storage management methods
    /// to interact with it, such as storage_deposit.
    pub user_storage_balances: StorageBalances<AccountId>,
    /// Balances for all the funds custodied by the dex engine
    /// contract. This means if the dex engine contract has
    /// less than this amount, it's either bug of the asset
    /// implementation, or funds have been drained from the
    /// dex engine contract. And if the balance is greater
    /// than this stored amount, it can be freely taken out
    /// without causing any issues.
    total_in_custody: IterableMap<AssetId, U128>,
}

#[derive(BorshStorageKey)]
#[near(serializers=[borsh])]
enum StorageKey {
    DexBalances,
    DexStorage,
    DexCodes,
    DexStorageBalances,
    UserBalances,
    UserStorageBalances,
    ContractTrackedBalance,
}

impl Default for DexEngine {
    fn default() -> Self {
        Self {
            dex_balances: LookupMap::new(StorageKey::DexBalances),
            dex_storage: LookupMap::new(StorageKey::DexStorage),
            dex_codes: LookupMap::new(StorageKey::DexCodes),
            dex_storage_balances: StorageBalances::new(StorageKey::DexStorageBalances),
            user_balances: LookupMap::new(StorageKey::UserBalances),
            user_storage_balances: StorageBalances::new(StorageKey::UserStorageBalances),
            total_in_custody: IterableMap::new(StorageKey::ContractTrackedBalance),
        }
    }
}

#[near(event_json(standard = "inteardex"))]
pub enum IntearDexEvent {
    #[event_version("1.0.0")]
    DexDeployed {
        dex_id: DexId,
        code_hash: Base58CryptoHash,
    },
    #[event_version("1.0.0")]
    DexEvent {
        dex_id: DexId,
        event: near_sdk::serde_json::Value,
        referrer: Option<AccountId>,
        user: Option<AccountId>,
    },
    #[event_version("1.0.0")]
    UserDeposit {
        account_id: AccountId,
        asset_id: AssetId,
        amount: U128,
    },
    #[event_version("1.0.0")]
    Withdraw {
        from: AccountOrDexId,
        to: AccountId,
        asset_id: AssetId,
        amount: U128,
    },
    #[event_version("1.0.0")]
    UserBalanceUpdate {
        account_id: AccountId,
        asset_id: AssetId,
        balance: U128,
    },
    #[event_version("1.0.0")]
    DexBalanceUpdate {
        dex_id: DexId,
        asset_id: AssetId,
        balance: U128,
    },
    #[event_version("1.0.0")]
    Swap {
        dex_id: DexId,
        request: SwapRequest,
        amount_in: U128,
        amount_out: U128,
        trader: AccountId,
        referrer: Option<AccountId>,
    },
}

enum CallType<'a> {
    Trade {
        dex_storage_mut: &'a mut DexStorage,
        alleged_trader: AccountId,
    },
    TradeView {
        dex_storage: &'a DexStorage,
        trader: AccountId,
        ephemeral_storage_updates: HashMap<Vec<u8>, Option<Vec<u8>>>,
    },
    View {
        dex_storage: &'a DexStorage,
    },
    Call {
        dex_storage_mut: &'a mut DexStorage,
        predecessor_id: AccountId,
        is_authorized: bool,
    },
}

type DexStorage = LookupMap<(DexId, Vec<u8>), Vec<u8>>;

impl CallType<'_> {
    pub fn dex_storage_get(&self, dex_id: DexId, key: Vec<u8>) -> Option<&Vec<u8>> {
        match self {
            CallType::Trade {
                dex_storage_mut, ..
            } => dex_storage_mut.get(&(dex_id, key)),
            CallType::TradeView {
                dex_storage,
                trader: _,
                ephemeral_storage_updates,
            } => {
                if let Some(value) = ephemeral_storage_updates.get(&key) {
                    value.as_ref()
                } else if let Some(value) = dex_storage.get(&(dex_id, key)) {
                    Some(value)
                } else {
                    None
                }
            }
            CallType::View { dex_storage } => dex_storage.get(&(dex_id, key)),
            CallType::Call {
                dex_storage_mut, ..
            } => dex_storage_mut.get(&(dex_id, key)),
        }
    }

    pub fn dex_storage_contains_key(&self, dex_id: DexId, key: Vec<u8>) -> bool {
        match self {
            CallType::Trade {
                dex_storage_mut, ..
            } => dex_storage_mut.contains_key(&(dex_id, key)),
            CallType::TradeView {
                dex_storage,
                ephemeral_storage_updates,
                ..
            } => {
                if let Some(value) = ephemeral_storage_updates.get(&key) {
                    value.is_some()
                } else {
                    dex_storage.contains_key(&(dex_id, key))
                }
            }
            CallType::View { dex_storage } => dex_storage.contains_key(&(dex_id, key)),
            CallType::Call {
                dex_storage_mut, ..
            } => dex_storage_mut.contains_key(&(dex_id, key)),
        }
    }

    pub fn dex_storage_insert(
        &mut self,
        dex_id: DexId,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, wasmi::Error> {
        match self {
            CallType::Trade {
                dex_storage_mut, ..
            } => Ok(dex_storage_mut.insert((dex_id, key), value)),
            CallType::TradeView {
                dex_storage,
                ephemeral_storage_updates,
                ..
            } => {
                if let Some(prev_value) = ephemeral_storage_updates.get(&key).cloned() {
                    ephemeral_storage_updates.insert(key, Some(value));
                    Ok(prev_value)
                } else if let Some(prev_value) = dex_storage.get(&(dex_id, key.clone())) {
                    ephemeral_storage_updates.insert(key, Some(value));
                    Ok(Some(prev_value.clone()))
                } else {
                    ephemeral_storage_updates.insert(key, Some(value));
                    Ok(None)
                }
            }
            CallType::View { .. } => Err(wasmi::Error::new("Cannot modify data in view-only mode")),
            CallType::Call {
                dex_storage_mut, ..
            } => Ok(dex_storage_mut.insert((dex_id, key), value)),
        }
    }

    pub fn dex_storage_remove(
        &mut self,
        dex_id: DexId,
        key: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, wasmi::Error> {
        match self {
            CallType::Trade {
                dex_storage_mut, ..
            } => Ok(dex_storage_mut.remove(&(dex_id, key))),
            CallType::TradeView {
                dex_storage,
                ephemeral_storage_updates,
                ..
            } => {
                if let Some(prev_value) = ephemeral_storage_updates.insert(key.clone(), None) {
                    Ok(prev_value)
                } else {
                    Ok(dex_storage.get(&(dex_id, key.clone())).cloned())
                }
            }
            CallType::View { .. } => Err(wasmi::Error::new("Cannot modify data in view-only mode")),
            CallType::Call {
                dex_storage_mut, ..
            } => Ok(dex_storage_mut.remove(&(dex_id, key))),
        }
    }

    pub fn dex_storage_flush(&mut self) {
        match self {
            CallType::Trade {
                dex_storage_mut, ..
            } => dex_storage_mut.flush(),
            CallType::TradeView { .. } => (),
            CallType::View { .. } => (),
            CallType::Call {
                dex_storage_mut, ..
            } => dex_storage_mut.flush(),
        }
    }
}

pub struct RunnerData<'a> {
    request: Vec<u8>,
    response: Option<Vec<u8>>,
    registers: HashMap<u64, Vec<u8>>,
    call_type: CallType<'a>,
    dex_id: DexId,
    dex_storage_balances: &'a StorageBalances<DexId>,
    dex_storage_usage_before_transaction: u64,
    referrer: Option<AccountId>,
}

#[near]
impl DexEngine {
    /// Deploy or upgrade the code for a dex.
    #[payable]
    pub fn deploy_dex_code(&mut self, last_part_of_id: String, code_base64: Base64VecU8) {
        near_sdk::assert_one_yocto();
        self.internal_deploy_dex_code(
            last_part_of_id,
            code_base64,
            near_sdk::env::predecessor_account_id(),
        )
    }

    /// Swap one asset for another on a specific dex.
    /// Multi-step aggregator method coming soon.
    #[payable]
    pub fn swap_simple(
        &mut self,
        dex_id: DexId,
        message: Base64VecU8,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
        referrer: Option<AccountId>,
        constraint: Option<U128>,
    ) -> (U128, U128) {
        near_sdk::assert_one_yocto();
        self.internal_swap_simple(
            dex_id,
            message,
            asset_in,
            asset_out,
            amount,
            TradeAccount::User(near_sdk::env::predecessor_account_id()),
            referrer,
            constraint,
        )
    }

    /// An arbitrary call to a dex method. Can be used for
    /// operations such as adding liquidity, removing liquidity,
    /// oracle updates, manual curve / strategy updates by the
    /// developer, etc.
    #[payable]
    pub fn dex_call(
        &mut self,
        dex_id: DexId,
        method: String,
        args: Base64VecU8,
        attached_assets: HashMap<AssetId, U128>,
        referrer: Option<AccountId>,
    ) -> Base64VecU8 {
        near_sdk::assert_one_yocto();
        self.internal_dex_call(
            dex_id,
            method,
            args,
            attached_assets,
            near_sdk::env::predecessor_account_id(),
            None,
            referrer,
        )
    }

    #[payable]
    pub fn transfer_asset(&mut self, to: AccountOrDexId, asset_id: AssetId, amount: U128) {
        near_sdk::assert_one_yocto();
        self.internal_transfer_asset(
            AccountOrDexId::Account(near_sdk::env::predecessor_account_id()),
            to,
            asset_id,
            amount,
        );
    }

    /// Register assets for an account or dex, reserving storage
    /// for the balance. Unregistering is not possible. No-op if
    /// already registered.
    #[payable]
    pub fn register_assets(&mut self, asset_ids: Vec<AssetId>, r#for: Option<AccountOrDexId>) {
        near_sdk::assert_one_yocto();
        self.internal_register_assets(asset_ids, r#for, near_sdk::env::predecessor_account_id());
    }

    /// Withdraw assets from the dex engine contract's inner
    /// balance for the user. If `withdraw_to` is not provided,
    /// the assets will be withdrawn to the user's account.
    ///
    /// Returns `true` if the withdrawal was successful, `false`
    /// otherwise. If a withdrawal fails, the assets will be
    /// refunded to the contract's custody balance of the user.
    #[payable]
    pub fn withdraw(
        &mut self,
        asset_id: AssetId,
        amount: DirectWithdrawAmount,
        withdraw_to: Option<AccountId>,
    ) -> PromiseOrValue<bool> {
        near_sdk::assert_one_yocto();
        self.internal_withdraw(
            asset_id,
            amount,
            withdraw_to,
            AccountOrDexId::Account(near_sdk::env::predecessor_account_id()),
        )
    }

    #[payable]
    pub fn execute_operations(&mut self, operations: Vec<Operation>, referrer: Option<AccountId>) {
        expect!(
            !near_sdk::env::attached_deposit().is_zero(),
            "Deposit of one yoctoNEAR is required."
        );
        if near_sdk::env::attached_deposit() > NearToken::from_yoctonear(1) {
            self.internal_increase_assets(
                AccountOrDexId::Account(near_sdk::env::predecessor_account_id()),
                AssetId::Near,
                U128(near_sdk::env::attached_deposit().as_yoctonear()),
            );
            self.total_in_custody
                .entry(AssetId::Near)
                .and_modify(|b| {
                    b.0 =
                        b.0.checked_add(near_sdk::env::attached_deposit().as_yoctonear())
                            .unwrap_or_else(|| {
                                panic!(
                                    "Balance overflow for contract and asset Near: {} + {} > {}",
                                    b.0,
                                    near_sdk::env::attached_deposit().as_yoctonear(),
                                    u128::MAX
                                )
                            });
                })
                .or_insert_with(|| {
                    panic!(
                        "Failed to deposit assets to contract tracked balance: asset not registered"
                    )
                });
        }
        self.internal_execute_operations(
            operations,
            near_sdk::env::predecessor_account_id(),
            None,
            referrer,
        );
    }

    pub fn asset_balance_of(&self, of: AccountOrDexId, asset_id: AssetId) -> Option<U128> {
        match of {
            AccountOrDexId::Account(account) => {
                self.user_balances.get(&(account, asset_id)).copied()
            }
            AccountOrDexId::Dex(dex_id) => self.dex_balances.get(&(dex_id, asset_id)).copied(),
        }
    }

    pub fn total_in_custody(&self, asset_id: AssetId) -> Option<U128> {
        self.total_in_custody.get(&asset_id).copied()
    }

    // View method, but needs &mut for compatibility ergonomics with RunnerData
    pub fn dex_view(&self, dex_id: DexId, method: String, args: Base64VecU8) -> Base64VecU8 {
        self.internal_dex_view(dex_id, method, args)
    }

    /// Simulate a swap without executing it. Returns the expected
    /// (amount_in, amount_out) that would result from the swap.
    /// This is a view method and does not modify any state.
    pub fn simulate_swap_simple(
        &self,
        dex_id: DexId,
        message: Base64VecU8,
        trader: AccountId,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
        referrer: Option<AccountId>,
    ) -> (U128, U128) {
        self.internal_simulate_swap_simple(
            dex_id, message, trader, asset_in, asset_out, amount, referrer,
        )
    }

    pub fn are_assets_registered(&self, asset_ids: Vec<AssetId>, r#for: AccountOrDexId) -> bool {
        match r#for {
            AccountOrDexId::Account(account) => asset_ids.into_iter().all(|asset_id| {
                self.user_balances
                    .contains_key(&(account.clone(), asset_id))
            }),
            AccountOrDexId::Dex(dex_id) => asset_ids
                .into_iter()
                .all(|asset_id| self.dex_balances.contains_key(&(dex_id.clone(), asset_id))),
        }
    }
}
