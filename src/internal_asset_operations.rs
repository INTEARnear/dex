use std::fmt::Display;

use intear_dex_types::{AssetId, DexId, expect};
use near_sdk::{AccountId, json_types::U128, near};

use crate::{DexEngine, IntearDexEvent};

#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[json])]
pub enum AccountOrDexId {
    Account(AccountId),
    Dex(DexId),
}

impl Display for AccountOrDexId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Account(account) => write!(f, "Account({account})"),
            Self::Dex(dex_id) => write!(f, "Dex({dex_id})"),
        }
    }
}

impl DexEngine {
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
        match account_or_dex_id {
            AccountOrDexId::Account(account) => {
                self.user_balances.contains_key(&(account, asset_id))
            }
            AccountOrDexId::Dex(dex_id) => self.dex_balances.contains_key(&(dex_id, asset_id)),
        }
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
                .get(&(account.clone(), asset_id.clone()))
                .unwrap_or_else(|| {
                    panic!("User balance not found for account {account} and asset {asset_id}")
                }),
            AccountOrDexId::Dex(dex_id) => self
                .dex_balances
                .get(&(dex_id.clone(), asset_id.clone()))
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
                let balance = *self.user_balances
                    .entry((account.clone(), asset_id.clone()))
                    .and_modify(|b| {
                        b.0 = b.0.checked_add(amount.0).unwrap_or_else(|| panic!("Balance overflow for account {account} and asset {asset_id}: {} + {} > {}", b.0, amount.0, u128::MAX));
                    })
                    .or_insert_with(|| panic!("Failed to deposit assets to user balance: user {account} balance for asset {asset_id} was not found"));
                IntearDexEvent::UserBalanceUpdate {
                    account_id: account,
                    asset_id,
                    balance,
                }
                .emit();
            }
            AccountOrDexId::Dex(dex_id) => {
                let balance = *self.dex_balances
                    .entry((dex_id.clone(), asset_id.clone()))
                    .and_modify(|b| {
                        b.0 = b.0.checked_add(amount.0).unwrap_or_else(|| panic!("Balance overflow for dex {dex_id} and asset {asset_id}: {} + {} > {}", b.0, amount.0, u128::MAX));
                    })
                    .or_insert_with(|| panic!("Failed to deposit assets to dex balance: dex {dex_id} balance for asset {asset_id} was not found"));
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
                let balance = *self.user_balances
                    .entry((account.clone(), asset_id.clone()))
                    .and_modify(|b| {
                        b.0 = b.0.checked_sub(amount.0).unwrap_or_else(|| panic!("Insufficient balance for account {account} and asset {asset_id}: {} < {}", b.0, amount.0));
                    })
                    .or_insert_with(|| {
                        panic!("Failed to withdraw assets from user balance: user {account} balance for asset {asset_id} was not found")
                    });
                IntearDexEvent::UserBalanceUpdate {
                    account_id: account,
                    asset_id,
                    balance,
                }
                .emit();
            }
            AccountOrDexId::Dex(dex_id) => {
                let balance = *self.dex_balances
                    .entry((dex_id.clone(), asset_id.clone()))
                    .and_modify(|b| {
                        b.0 = b.0.checked_sub(amount.0).unwrap_or_else(|| panic!("Insufficient balance for dex {dex_id} and asset {asset_id}: {} < {}", b.0, amount.0));
                    })
                    .or_insert_with(|| {
                        panic!("Failed to withdraw assets from dex balance: dex {dex_id} balance for asset {asset_id} was not found")
                    });
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
