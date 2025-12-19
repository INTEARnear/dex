#![deny(clippy::arithmetic_side_effects)]

pub mod asset_deposit;
pub mod host_functions;
pub mod internal_asset_operations;
pub mod internal_operations;
pub mod storage_management;

use std::collections::HashMap;

use crate::{
    internal_asset_operations::AccountOrDexId, internal_operations::Operation,
    storage_management::StorageBalances,
};
use intear_dex_types::{AssetId, DexId, SwapRequest, SwapRequestAmount};
use near_sdk::{
    AccountId, BorshStorageKey, Promise,
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
    dex_storage: LookupMap<(DexId, Vec<u8>), Vec<u8>>,
    /// Wasm code for each dex.
    dex_codes: LookupMap<DexId, Vec<u8>>,
    /// Storage balances for each dex, translated to storage
    /// of this smart contract. use dex_* methods to interact
    /// with it, such as dex_storage_deposit.
    dex_storage_balances: StorageBalances<DexId>,
    /// Balances for each user, custodied by the dex engine
    /// contract for faster access. This reduces the need for
    /// ft_transfer_call, which takes time.
    user_balances: LookupMap<(AccountId, AssetId), U128>,
    /// Storage balances for each user, translated to storage
    /// of this smart contract. use storage management methods
    /// to interact with it, such as storage_deposit.
    user_storage_balances: StorageBalances<AccountId>,
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
    },
}

enum CallType<'a> {
    View {
        dex_storage: &'a LookupMap<(DexId, Vec<u8>), Vec<u8>>,
    },
    Call {
        dex_storage_mut: &'a mut LookupMap<(DexId, Vec<u8>), Vec<u8>>,
        predecessor_id: AccountId,
    },
}

impl<'a> CallType<'a> {
    pub fn dex_storage(&'a self) -> &'a LookupMap<(DexId, Vec<u8>), Vec<u8>> {
        match self {
            CallType::View { dex_storage } => dex_storage,
            CallType::Call {
                dex_storage_mut, ..
            } => dex_storage_mut,
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
    ) -> (U128, U128) {
        near_sdk::assert_one_yocto();
        self.internal_swap_simple(
            dex_id,
            message,
            asset_in,
            asset_out,
            amount,
            near_sdk::env::predecessor_account_id(),
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
    ) -> Base64VecU8 {
        near_sdk::assert_one_yocto();
        self.internal_dex_call(
            dex_id,
            method,
            args,
            attached_assets,
            near_sdk::env::predecessor_account_id(),
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
        amount: Option<U128>,
        withdraw_to: Option<AccountId>,
    ) -> Promise {
        near_sdk::assert_one_yocto();
        self.internal_withdraw(
            asset_id,
            amount,
            withdraw_to,
            AccountOrDexId::Account(near_sdk::env::predecessor_account_id()),
        )
    }

    #[payable]
    pub fn execute_operations(&mut self, operations: Vec<Operation>) {
        near_sdk::assert_one_yocto();
        self.internal_execute_operations(operations, near_sdk::env::predecessor_account_id());
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
}
