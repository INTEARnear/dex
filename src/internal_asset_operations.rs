use intear_dex_types::{AccountOrDexId, AssetId, expect};
use near_sdk::{json_types::U128, store::IterableMap};

use crate::{AssetBalances, DexEngine, IntearDexEvent, StorageKey};

impl DexEngine {
    pub fn asset_balances(&self, account_or_dex_id: &AccountOrDexId) -> Option<&AssetBalances> {
        match account_or_dex_id {
            AccountOrDexId::Account(account) => self.user_balances.get(account),
            AccountOrDexId::Dex(dex_id) => self.dex_balances.get(dex_id),
        }
    }

    pub(crate) fn asset_balances_or_new(
        &mut self,
        account_or_dex_id: &AccountOrDexId,
    ) -> &mut AssetBalances {
        match account_or_dex_id {
            AccountOrDexId::Account(account) => self
                .user_balances
                .entry(account.clone())
                .or_insert_with(|| {
                    IterableMap::new(StorageKey::UserBalancesOf {
                        account_id: account.clone(),
                    })
                }),
            AccountOrDexId::Dex(dex_id) => {
                self.dex_balances.entry(dex_id.clone()).or_insert_with(|| {
                    IterableMap::new(StorageKey::DexBalancesOf {
                        dex_id: dex_id.clone(),
                    })
                })
            }
        }
    }

    pub fn assert_asset_registered(&self, account_or_dex_id: AccountOrDexId, asset_id: AssetId) {
        expect!(
            self.asset_is_registered(account_or_dex_id.clone(), asset_id.clone()),
            "Asset {asset_id} is not registered for {account_or_dex_id}"
        );
    }

    pub fn asset_is_registered(
        &self,
        account_or_dex_id: AccountOrDexId,
        asset_id: AssetId,
    ) -> bool {
        self.asset_balances(&account_or_dex_id)
            .is_some_and(|asset_balances| asset_balances.contains_key(&asset_id))
    }

    pub fn internal_transfer_asset(
        &mut self,
        from: AccountOrDexId,
        to: AccountOrDexId,
        asset_id: AssetId,
        amount: U128,
    ) {
        if amount.0 == 0 {
            return;
        }
        self.internal_decrease_assets(from, asset_id.clone(), amount);
        self.internal_increase_assets(to, asset_id, amount);
    }

    pub fn assert_has_enough(
        &self,
        account_or_dex_id: AccountOrDexId,
        asset_id: AssetId,
        amount: U128,
    ) {
        let balance = match &account_or_dex_id {
            AccountOrDexId::Account(account) => self
                .user_balances
                .get(account)
                .and_then(|asset_balances| asset_balances.get(&asset_id))
                .unwrap_or_else(|| {
                    panic!("User balance not found for account {account} and asset {asset_id}")
                }),
            AccountOrDexId::Dex(dex_id) => self
                .dex_balances
                .get(dex_id)
                .and_then(|asset_balances| asset_balances.get(&asset_id))
                .unwrap_or_else(|| {
                    panic!("Dex balance not found for dex {dex_id} and asset {asset_id}")
                }),
        };
        expect!(
            balance.0 >= amount.0,
            "Insufficient balance of {} for {}: {} < {}",
            asset_id,
            account_or_dex_id,
            balance.0,
            amount.0
        );
    }

    pub fn internal_increase_assets(
        &mut self,
        account_or_dex_id: AccountOrDexId,
        asset_id: AssetId,
        amount: U128,
    ) {
        self.assert_asset_registered(account_or_dex_id.clone(), asset_id.clone());
        match account_or_dex_id {
            AccountOrDexId::Account(account) => {
                let balance = self.user_balances
                    .get_mut(&account)
                    .and_then(|asset_balances| asset_balances.get_mut(&asset_id))
                    .unwrap_or_else(|| panic!("Failed to deposit assets to user balance: user {account} balance for asset {asset_id} was not found"));
                balance.0 = balance.0.checked_add(amount.0).unwrap_or_else(|| {
                    panic!(
                        "Balance overflow for account {account} and asset {asset_id}: {} + {} > {}",
                        balance.0,
                        amount.0,
                        u128::MAX
                    )
                });
                let balance = *balance;
                IntearDexEvent::UserBalanceUpdate {
                    account_id: account,
                    asset_id,
                    balance,
                }
                .emit();
            }
            AccountOrDexId::Dex(dex_id) => {
                let balance = self.dex_balances
                    .get_mut(&dex_id)
                    .and_then(|asset_balances| asset_balances.get_mut(&asset_id))
                    .unwrap_or_else(|| panic!("Failed to deposit assets to dex balance: dex {dex_id} balance for asset {asset_id} was not found"));
                balance.0 = balance.0.checked_add(amount.0).unwrap_or_else(|| {
                    panic!(
                        "Balance overflow for dex {dex_id} and asset {asset_id}: {} + {} > {}",
                        balance.0,
                        amount.0,
                        u128::MAX
                    )
                });
                let balance = *balance;
                IntearDexEvent::DexBalanceUpdate {
                    dex_id,
                    asset_id,
                    balance,
                }
                .emit();
            }
        }
    }

    pub fn internal_decrease_assets(
        &mut self,
        account_or_dex_id: AccountOrDexId,
        asset_id: AssetId,
        amount: U128,
    ) {
        match account_or_dex_id {
            AccountOrDexId::Account(account) => {
                let balance = self.user_balances
                    .get_mut(&account)
                    .and_then(|asset_balances| asset_balances.get_mut(&asset_id))
                    .unwrap_or_else(|| {
                        panic!("Failed to withdraw assets from user balance: user {account} balance for asset {asset_id} was not found")
                    });
                balance.0 = balance.0.checked_sub(amount.0).unwrap_or_else(|| {
                    panic!(
                        "Insufficient balance for account {account} and asset {asset_id}: {} < {}",
                        balance.0, amount.0
                    )
                });
                let balance = *balance;
                IntearDexEvent::UserBalanceUpdate {
                    account_id: account,
                    asset_id,
                    balance,
                }
                .emit();
            }
            AccountOrDexId::Dex(dex_id) => {
                let balance = self.dex_balances
                    .get_mut(&dex_id)
                    .and_then(|asset_balances| asset_balances.get_mut(&asset_id))
                    .unwrap_or_else(|| {
                        panic!("Failed to withdraw assets from dex balance: dex {dex_id} balance for asset {asset_id} was not found")
                    });
                balance.0 = balance.0.checked_sub(amount.0).unwrap_or_else(|| {
                    panic!(
                        "Insufficient balance for dex {dex_id} and asset {asset_id}: {} < {}",
                        balance.0, amount.0
                    )
                });
                let balance = *balance;
                IntearDexEvent::DexBalanceUpdate {
                    dex_id,
                    asset_id,
                    balance,
                }
                .emit();
            }
        }
    }
}
