#![deny(clippy::arithmetic_side_effects)]
#![allow(clippy::too_many_arguments)]

pub mod asset_deposit;
pub mod call_events;
pub mod host_functions;
pub mod internal_asset_operations;
pub mod internal_operations;
pub mod rescue;
pub mod storage_management;

use std::collections::HashMap;

use crate::{
    call_events::CallEvents, internal_operations::TradeAccount, storage_management::StorageBalances,
};
use intear_dex_types::{
    AccountOrDexId, AssetId, CAN_PAUSE, Caller, DexEventData, DexId, DirectWithdrawAmount,
    IntearDexEvent, Operation, ReferralFees, SwapRequestAmount, expect,
};
use near_sdk::{
    AccountId, BorshStorageKey, IntoStorageKey, NearToken, PanicOnDefault, PromiseOrValue,
    json_types::{Base64VecU8, U128},
    near,
    store::{IterableMap, LookupMap},
};

pub type AssetBalances = IterableMap<AssetId, U128>;

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct DexEngine {
    /// Assets that are custodied by the dex engine contract
    /// for the dexes that run inside it. Other dexes or users
    /// can't access other dexes' balances.
    dex_balances: LookupMap<DexId, AssetBalances>,
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
    user_balances: LookupMap<AccountId, AssetBalances>,
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
    paused: bool,
    /// The only account that can deploy dex code. Dex code isn't
    /// validated when it's loaded, so it must come from a trusted source.
    trusted_code_deployer: AccountId,
}

#[derive(BorshStorageKey)]
#[near(serializers=[borsh])]
enum StorageKey {
    #[allow(dead_code)]
    UnusedFlatDexBalances,
    DexStorage,
    DexCodes,
    DexStorageBalances,
    #[allow(dead_code)]
    UnusedFlatUserBalances,
    UserStorageBalances,
    ContractTrackedBalance,
    UserBalances,
    UserBalancesOf {
        account_id: AccountId,
    },
    DexBalances,
    DexBalancesOf {
        dex_id: DexId,
    },
}

enum CallType<'a> {
    Trade {
        dex_storage: &'a DexStorage,
        trader: Caller,
        pending_storage_updates: PendingStorageUpdates,
    },
    View {
        dex_storage: &'a DexStorage,
    },
    Call {
        dex_storage_mut: &'a mut DexStorage,
        caller: Caller,
    },
}

type DexStorage = LookupMap<(DexId, Vec<u8>), Vec<u8>>;

type PendingStorageUpdates = HashMap<Vec<u8>, Option<Vec<u8>>>;

const STORAGE_BYTES_PER_RECORD: u64 = 40;

impl CallType<'_> {
    pub fn dex_storage_get(&self, dex_id: DexId, key: Vec<u8>) -> Option<&Vec<u8>> {
        match self {
            CallType::Trade {
                dex_storage,
                trader: _,
                pending_storage_updates,
            } => {
                if let Some(value) = pending_storage_updates.get(&key) {
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
                dex_storage,
                pending_storage_updates,
                ..
            } => {
                if let Some(value) = pending_storage_updates.get(&key) {
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
                dex_storage,
                pending_storage_updates,
                ..
            } => {
                if let Some(prev_value) = pending_storage_updates.get(&key).cloned() {
                    pending_storage_updates.insert(key, Some(value));
                    Ok(prev_value)
                } else if let Some(prev_value) = dex_storage.get(&(dex_id, key.clone())) {
                    pending_storage_updates.insert(key, Some(value));
                    Ok(Some(prev_value.clone()))
                } else {
                    pending_storage_updates.insert(key, Some(value));
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
                dex_storage,
                pending_storage_updates,
                ..
            } => {
                if let Some(prev_value) = pending_storage_updates.insert(key.clone(), None) {
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

    /// How much the contract's storage usage would change if a simulated trade's
    /// storage updates were written, so that the dex sees the same storage usage
    /// as in a real trade
    pub fn pending_storage_usage(&self, dex_id: &DexId) -> Result<i64, wasmi::Error> {
        let CallType::Trade {
            dex_storage,
            pending_storage_updates,
            ..
        } = self
        else {
            return Ok(0);
        };
        let prefix_len = StorageKey::DexStorage.into_storage_key().len();
        let record_size = |key: &Vec<u8>, value: Option<&Vec<u8>>| -> Result<i64, wasmi::Error> {
            let Some(value) = value else {
                return Ok(0);
            };
            let key_len = near_sdk::borsh::object_length(&(dex_id, key))
                .map_err(|err| wasmi::Error::new(format!("Failed to measure key: {err}")))?;
            let value_len = near_sdk::borsh::object_length(value)
                .map_err(|err| wasmi::Error::new(format!("Failed to measure value: {err}")))?;
            prefix_len
                .checked_add(key_len)
                .and_then(|size| size.checked_add(value_len))
                .and_then(|size| i64::try_from(size).ok())
                .and_then(|size| size.checked_add(STORAGE_BYTES_PER_RECORD as i64))
                .ok_or_else(|| wasmi::Error::new("Storage record size overflow"))
        };
        pending_storage_updates
            .iter()
            .try_fold(0i64, |total, (key, new_value)| {
                let old_value = dex_storage.get(&(dex_id.clone(), key.clone()));
                record_size(key, new_value.as_ref())?
                    .checked_sub(record_size(key, old_value)?)
                    .and_then(|change| total.checked_add(change))
                    .ok_or_else(|| wasmi::Error::new("Storage usage overflow"))
            })
    }

    pub fn dex_storage_flush(&mut self) {
        match self {
            CallType::Trade { .. } => (),
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
    dex_events: Vec<DexEventData>,
    pending_logs: Vec<String>,
}

#[near]
impl DexEngine {
    #[init]
    pub fn new(trusted_code_deployer: AccountId) -> Self {
        Self {
            dex_balances: LookupMap::new(StorageKey::DexBalances),
            dex_storage: LookupMap::new(StorageKey::DexStorage),
            dex_codes: LookupMap::new(StorageKey::DexCodes),
            dex_storage_balances: StorageBalances::new(StorageKey::DexStorageBalances),
            user_balances: LookupMap::new(StorageKey::UserBalances),
            user_storage_balances: StorageBalances::new(StorageKey::UserStorageBalances),
            total_in_custody: IterableMap::new(StorageKey::ContractTrackedBalance),
            paused: false,
            trusted_code_deployer,
        }
    }

    /// Transfer the permission to deploy dex code to another account
    #[payable]
    pub fn set_trusted_code_deployer(&mut self, account_id: AccountId) {
        near_sdk::assert_one_yocto();
        expect!(
            near_sdk::env::predecessor_account_id() == self.trusted_code_deployer,
            "Only the trusted code deployer can transfer this permission"
        );
        self.trusted_code_deployer = account_id;
    }

    pub fn get_trusted_code_deployer(&self) -> AccountId {
        self.trusted_code_deployer.clone()
    }

    #[payable]
    pub fn pause(&mut self) {
        near_sdk::assert_one_yocto();
        expect!(
            CAN_PAUSE.contains(&near_sdk::env::predecessor_account_id().as_str()),
            "Only authorized accounts can pause the contract"
        );
        self.paused = true;
    }

    #[payable]
    pub fn unpause(&mut self) {
        near_sdk::assert_one_yocto();
        expect!(
            CAN_PAUSE.contains(&near_sdk::env::predecessor_account_id().as_str()),
            "Only authorized accounts can pause the contract"
        );
        self.paused = false;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Deploy or upgrade the code for a dex.
    #[payable]
    pub fn deploy_dex_code(&mut self, last_part_of_id: String, code_base64: Base64VecU8) {
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
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
        referral_fees: Option<ReferralFees>,
        constraint: Option<U128>,
    ) -> (U128, U128) {
        let mut events = CallEvents::default();
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
        let amounts = self.internal_swap_simple(
            &mut events,
            dex_id,
            message,
            asset_in,
            asset_out,
            amount,
            TradeAccount::User(near_sdk::env::predecessor_account_id()),
            referrer,
            referral_fees.unwrap_or_default(),
            constraint,
        );
        events.emit();
        amounts
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
        let mut events = CallEvents::default();
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
        let response = self.internal_dex_call(
            &mut events,
            dex_id,
            method,
            args,
            attached_assets,
            Caller::Direct(near_sdk::env::predecessor_account_id()),
            None,
            referrer,
        );
        events.emit();
        response
    }

    #[payable]
    pub fn transfer_asset(&mut self, to: AccountOrDexId, asset_id: AssetId, amount: U128) {
        let mut events = CallEvents::default();
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
        self.internal_transfer_asset(
            &mut events,
            AccountOrDexId::Account(near_sdk::env::predecessor_account_id()),
            to,
            asset_id,
            amount,
        );
        events.emit();
    }

    /// Register assets for an account or dex, reserving storage
    /// for the balance. Unregistering is not possible. No-op if
    /// already registered.
    #[payable]
    pub fn register_assets(&mut self, asset_ids: Vec<AssetId>, r#for: Option<AccountOrDexId>) {
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
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
        let mut events = CallEvents::default();
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
        let withdrawal = self.internal_withdraw(
            &mut events,
            asset_id,
            amount,
            withdraw_to,
            AccountOrDexId::Account(near_sdk::env::predecessor_account_id()),
        );
        events.emit();
        withdrawal
    }

    #[payable]
    pub fn execute_operations(
        &mut self,
        operations: Vec<Operation>,
        referrer: Option<AccountId>,
        referral_fees: Option<ReferralFees>,
    ) {
        let mut events = CallEvents::default();
        expect!(
            !near_sdk::env::attached_deposit().is_zero(),
            "Deposit of at least one yoctoNEAR is required."
        );
        self.assert_not_paused();
        if near_sdk::env::attached_deposit() > NearToken::from_yoctonear(1) {
            self.internal_increase_assets(
                &mut events,
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
            &mut events,
            operations,
            Caller::Direct(near_sdk::env::predecessor_account_id()),
            None,
            referrer,
            referral_fees.unwrap_or_default(),
        );
        events.emit();
    }

    pub fn asset_balance_of(&self, of: AccountOrDexId, asset_id: AssetId) -> Option<U128> {
        self.asset_balances(&of)?.get(&asset_id).copied()
    }

    pub fn registered_assets_of(
        &self,
        account_id: Option<AccountId>,
        dex_id: Option<DexId>,
        from_index: u32,
        limit: u32,
    ) -> Vec<(AssetId, U128)> {
        let of = match (account_id, dex_id) {
            (Some(account_id), None) => AccountOrDexId::Account(account_id),
            (None, Some(dex_id)) => AccountOrDexId::Dex(dex_id),
            _ => panic!("account_id and dex_id are mutually exclusive"),
        };
        let Some(asset_balances) = self.asset_balances(&of) else {
            return Vec::new();
        };
        asset_balances
            .iter()
            .skip(from_index as usize)
            .take(limit as usize)
            .map(|(asset_id, balance)| (asset_id.clone(), *balance))
            .collect()
    }

    pub fn total_in_custody(&self, asset_id: AssetId) -> Option<U128> {
        self.total_in_custody.get(&asset_id).copied()
    }

    pub fn dex_code(&self, dex_id: DexId) -> Base64VecU8 {
        Base64VecU8(
            self.dex_codes
                .get(&dex_id)
                .expect("Dex code not found")
                .clone(),
        )
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
        referral_fees: Option<ReferralFees>,
    ) -> (U128, U128) {
        self.assert_not_paused();
        self.internal_simulate_swap_simple(
            dex_id,
            message,
            Caller::Direct(trader),
            asset_in,
            asset_out,
            amount,
            referrer,
            referral_fees.unwrap_or_default(),
        )
        .map(|swap| (swap.total_amount_in(), swap.amount_out()))
        .unwrap_or_else(|err| panic!("{err}"))
    }

    pub fn are_assets_registered(&self, asset_ids: Vec<AssetId>, r#for: AccountOrDexId) -> bool {
        let Some(asset_balances) = self.asset_balances(&r#for) else {
            return asset_ids.is_empty();
        };
        asset_ids
            .iter()
            .all(|asset_id| asset_balances.contains_key(asset_id))
    }
}

impl DexEngine {
    pub fn assert_not_paused(&self) {
        expect!(!self.paused, "Contract is paused");
    }
}
