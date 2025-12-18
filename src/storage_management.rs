use intear_dex_types::{DexId, expect};
use near_contract_standards::storage_management::{
    StorageBalance, StorageBalanceBounds, StorageManagement,
};
use near_sdk::{
    AccountId, IntoStorageKey, NearToken, Promise,
    borsh::{BorshDeserialize, BorshSerialize},
    near,
    store::LookupMap,
};

use crate::{DexEngine, DexEngineExt};

#[derive(Clone, Copy, Default)]
#[near(serializers=[borsh])]
pub struct StorageUsed {
    total: NearToken,
    used: NearToken,
}

impl From<StorageUsed> for StorageBalance {
    fn from(value: StorageUsed) -> Self {
        Self {
            total: value.total,
            available: value
                .total
                .checked_sub(value.used)
                .expect("Total balance less than used balance"),
        }
    }
}

const STORAGE_MIN_BOUND: NearToken = NearToken::from_millinear(10); // 0.01 NEAR = 1KB

#[near(serializers=[borsh])]
pub struct StorageBalances<K: Ord + BorshSerialize + BorshDeserialize> {
    storage_balances: LookupMap<K, StorageUsed>,
}

impl<K: Ord + BorshSerialize + BorshDeserialize + Clone> StorageBalances<K> {
    pub fn new(storage_key: impl IntoStorageKey) -> Self {
        Self {
            storage_balances: LookupMap::new(storage_key),
        }
    }

    pub fn deposit(&mut self, account_id: &K, amount: NearToken) {
        let b = self.storage_balances.entry(account_id.clone()).or_default();
        b.total = b.total.saturating_add(amount);
        self.storage_balances.flush();
    }

    pub fn charge(&mut self, account_id: &K, storage_usage_before: u64, storage_usage_after: u64) {
        match storage_usage_after.cmp(&storage_usage_before) {
            std::cmp::Ordering::Greater => {
                // charge the difference
                let storage_cost = near_sdk::env::storage_byte_cost()
                    .saturating_mul(storage_usage_after as u128 - storage_usage_before as u128);
                let b = self.storage_balances.entry(account_id.clone()).or_default();
                b.used = b
                    .used
                    .checked_add(storage_cost)
                    .expect("Storage cost overflow");
                if b.used > b.total {
                    panic!("Storage used ({}) exceeds total ({})", b.used, b.total);
                }
                self.storage_balances.flush();
            }
            std::cmp::Ordering::Less => {
                // refund the difference
                let storage_cost = near_sdk::env::storage_byte_cost()
                    .saturating_mul(storage_usage_before as u128 - storage_usage_after as u128);
                let b = self.storage_balances.entry(account_id.clone()).or_default();
                b.used = b
                    .used
                    .checked_sub(storage_cost)
                    .expect("Storage cost underflow");
                self.storage_balances.flush();
            }
            std::cmp::Ordering::Equal => {
                // nothing changed
            }
        }
    }

    pub fn get_bytes_used(&self, account_id: &K) -> u64 {
        self.storage_balances
            .get(account_id)
            .map(|b| {
                u64::try_from(
                    b.used
                        .as_yoctonear()
                        .saturating_div(near_sdk::env::storage_byte_cost().as_yoctonear()),
                )
                .expect("Storage usage overflow")
            })
            .unwrap_or_default()
    }

    fn storage_deposit(
        &mut self,
        account_id: K,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        let mut deposit = near_sdk::env::attached_deposit();
        if registration_only.is_some_and(|r| r) {
            if let Some(balance) = self.storage_balances.get(&account_id) {
                Promise::new(near_sdk::env::predecessor_account_id())
                    .transfer(deposit)
                    .detach();
                return (*balance).into();
            }
            if let Some(above_minimum) = deposit.checked_sub(STORAGE_MIN_BOUND) {
                Promise::new(near_sdk::env::predecessor_account_id())
                    .transfer(above_minimum)
                    .detach();
                deposit = STORAGE_MIN_BOUND;
            }
        }

        let storage_usage_before = near_sdk::env::storage_usage();
        self.storage_balances
            .entry(account_id.clone())
            .and_modify(|b| b.total = b.total.saturating_add(deposit))
            .or_insert(StorageUsed {
                total: deposit,
                used: NearToken::default(),
            });
        self.storage_balances.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        let storage_cost = near_sdk::env::storage_byte_cost()
            .saturating_mul(storage_usage_after as u128 - storage_usage_before as u128);
        let balance = *self
            .storage_balances
            .entry(account_id)
            .and_modify(|b| b.used = b.used.saturating_add(storage_cost))
            .or_insert_with(|| unreachable!("Just inserted"));
        balance.into()
    }

    fn storage_withdraw(&mut self, account_id: K, amount: Option<NearToken>) -> StorageBalance {
        near_sdk::assert_one_yocto();
        let Some(storage_used) = self.storage_balances.get_mut(&account_id) else {
            panic!("Storage used not found");
        };
        let balance = StorageBalance::from(*storage_used);
        let amount = if let Some(requested_amount) = amount {
            if requested_amount > balance.available {
                panic!("Amount exceeds storage used");
            }
            requested_amount
        } else {
            balance.available
        };
        storage_used.total = storage_used
            .total
            .checked_sub(amount)
            .expect("Total balance less than used balance");
        Promise::new(near_sdk::env::predecessor_account_id())
            .transfer(amount)
            .detach();
        (*storage_used).into()
    }

    fn storage_unregister(&mut self, account_id: K, force: Option<bool>) -> bool {
        near_sdk::assert_one_yocto();

        if force.is_some_and(|f| f) {
            panic!("Force unregistration is not supported");
        }

        let storage_usage_before = near_sdk::env::storage_usage();
        let Some(storage_used) = self.storage_balances.remove(&account_id) else {
            return false;
        };
        self.storage_balances.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        let storage_freed = near_sdk::env::storage_byte_cost()
            .saturating_mul(storage_usage_before as u128 - storage_usage_after as u128);
        if let Some(leftover) = storage_used.used.checked_sub(storage_freed) {
            panic!("User is using {leftover} worth of storage")
        } else {
            true
        }
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        StorageBalanceBounds {
            min: STORAGE_MIN_BOUND,
            max: None,
        }
    }

    fn storage_balance_of(&self, account_id: K) -> Option<StorageBalance> {
        self.storage_balances.get(&account_id).map(|b| (*b).into())
    }
}

#[near]
impl StorageManagement for DexEngine {
    #[payable]
    fn storage_deposit(
        &mut self,
        account_id: Option<AccountId>,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        self.user_storage_balances.storage_deposit(
            account_id.unwrap_or_else(near_sdk::env::predecessor_account_id),
            registration_only,
        )
    }

    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.user_storage_balances
            .storage_withdraw(near_sdk::env::predecessor_account_id(), amount)
    }

    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        self.user_storage_balances
            .storage_unregister(near_sdk::env::predecessor_account_id(), force)
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.user_storage_balances.storage_balance_bounds()
    }

    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.user_storage_balances.storage_balance_of(account_id)
    }
}

#[near]
impl DexEngine {
    #[payable]
    pub fn dex_storage_deposit(
        &mut self,
        dex_id: DexId,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        self.dex_storage_balances
            .storage_deposit(dex_id, registration_only)
    }

    #[payable]
    pub fn dex_storage_withdraw(
        &mut self,
        dex_id: DexId,
        amount: Option<NearToken>,
    ) -> StorageBalance {
        expect!(
            dex_id.deployer == near_sdk::env::predecessor_account_id(),
            "Only the deployer can withdraw dex storage"
        );
        self.dex_storage_balances.storage_withdraw(dex_id, amount)
    }

    pub fn dex_storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.dex_storage_balances.storage_balance_bounds()
    }

    pub fn dex_storage_balance_of(&self, dex_id: DexId) -> Option<StorageBalance> {
        self.dex_storage_balances.storage_balance_of(dex_id)
    }
}
