use intear_dex_common::{AssetId, TokenAmount, expect};
use intear_dex_types::AccountOrDexId;
use near_sdk::store::IterableMap;

use crate::{AssetBalances, DexEngine, StorageKey, call_events::CallEvents};

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
        events: &mut CallEvents,
        from: AccountOrDexId,
        to: AccountOrDexId,
        asset_id: AssetId,
        amount: TokenAmount,
    ) {
        if amount.is_zero() {
            return;
        }
        self.internal_decrease_assets(events, from, asset_id.clone(), amount);
        self.internal_increase_assets(events, to, asset_id, amount);
    }

    pub fn assert_has_enough(
        &self,
        account_or_dex_id: AccountOrDexId,
        asset_id: AssetId,
        amount: TokenAmount,
    ) {
        let balance = self
            .asset_balances(&account_or_dex_id)
            .and_then(|asset_balances| asset_balances.get(&asset_id).copied())
            .unwrap_or_else(|| {
                panic!("Balance not found for {account_or_dex_id} and asset {asset_id}")
            });
        expect!(
            balance >= amount,
            "Insufficient balance of {} for {}: {} < {}",
            asset_id,
            account_or_dex_id,
            balance,
            amount
        );
    }

    pub fn internal_increase_assets(
        &mut self,
        events: &mut CallEvents,
        account_or_dex_id: AccountOrDexId,
        asset_id: AssetId,
        amount: TokenAmount,
    ) {
        let balance = self.update_registered_balance(&account_or_dex_id, &asset_id, |balance| {
            balance.checked_add(amount).unwrap_or_else(|| {
                panic!(
                    "Balance overflow for {account_or_dex_id} and asset {asset_id}: {} + {} > {}",
                    balance,
                    amount,
                    u128::MAX
                )
            })
        });
        events.balance_updated(account_or_dex_id, asset_id, balance);
    }

    pub fn internal_decrease_assets(
        &mut self,
        events: &mut CallEvents,
        account_or_dex_id: AccountOrDexId,
        asset_id: AssetId,
        amount: TokenAmount,
    ) {
        let balance = self.update_registered_balance(&account_or_dex_id, &asset_id, |balance| {
            balance.checked_sub(amount).unwrap_or_else(|| {
                panic!(
                    "Insufficient balance for {account_or_dex_id} and asset {asset_id}: {} < {}",
                    balance, amount
                )
            })
        });
        events.balance_updated(account_or_dex_id, asset_id, balance);
    }

    fn update_registered_balance(
        &mut self,
        account_or_dex_id: &AccountOrDexId,
        asset_id: &AssetId,
        update: impl FnOnce(TokenAmount) -> TokenAmount,
    ) -> TokenAmount {
        let asset_balances = match account_or_dex_id {
            AccountOrDexId::Account(account) => self.user_balances.get_mut(account),
            AccountOrDexId::Dex(dex_id) => self.dex_balances.get_mut(dex_id),
        };
        let Some(balance) =
            asset_balances.and_then(|asset_balances| asset_balances.get_mut(asset_id))
        else {
            panic!("Asset {asset_id} is not registered for {account_or_dex_id}");
        };
        *balance = update(*balance);
        *balance
    }
}
