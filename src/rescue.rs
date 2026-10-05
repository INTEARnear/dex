use intear_dex_types::{AssetId, expect};
use near_contract_standards::{
    fungible_token::core::ext_ft_core,
    non_fungible_token::{Token, core::ext_nft_core},
};
use near_sdk::{AccountId, Gas, NearToken, Promise, PromiseError, json_types::U128, near};

use crate::{DexEngine, DexEngineExt};

const GAS_FOR_BALANCE_QUERY: Gas = Gas::from_tgas(10);
const GAS_FOR_RESCUE_CALLBACK: Gas = Gas::from_tgas(20);

#[near]
impl DexEngine {
    /// NEAR on the contract's account that isn't owed to anyone: not in
    /// `total_in_custody`, not in storage balances of users and dexes,
    /// and not locked for storage that storage balances don't pay for
    /// (contract code, contract state root, access keys).
    pub fn untracked_near(&self) -> NearToken {
        self.assert_not_migrating();
        let users = self.user_storage_balances.sum();
        let dexes = self.dex_storage_balances.sum();
        let storage_balances_total = users
            .total
            .checked_add(dexes.total)
            .expect("Storage balances overflow");
        let storage_paid_by_storage_balances = users
            .used
            .checked_add(dexes.used)
            .expect("Storage balances overflow");
        let storage_locked = near_sdk::env::storage_byte_cost()
            .checked_mul(u128::from(near_sdk::env::storage_usage()))
            .expect("Storage cost overflow");
        let storage_not_paid_by_storage_balances =
            storage_locked.saturating_sub(storage_paid_by_storage_balances);
        let near_in_custody = NearToken::from_yoctonear(
            self.total_in_custody
                .get(&AssetId::Near)
                .map_or(0, |amount| amount.0),
        );
        let owed = near_in_custody
            .checked_add(storage_balances_total)
            .and_then(|owed| owed.checked_add(storage_not_paid_by_storage_balances))
            .expect("Owed NEAR overflow");
        let balance = near_sdk::env::account_balance();
        balance.checked_sub(owed).unwrap_or_else(|| {
            panic!(
                "NEAR deficit: balance {balance} < custody {near_in_custody} + storage balances {storage_balances_total} + unpaid storage {storage_not_paid_by_storage_balances}"
            )
        })
    }

    /// Send out assets that the contract holds but doesn't track, such
    /// as tokens sent with `ft_transfer` instead of `ft_transfer_call`.
    /// If `amount` is not provided, everything untracked is sent.
    ///
    /// The contract must be paused, so that no withdrawals are in
    /// flight while the untracked balance is calculated.
    #[private] // callable by admin
    pub fn rescue(&mut self, asset_id: AssetId, amount: Option<U128>, to: AccountId) -> Promise {
        self.assert_paused_for_rescue();
        let current_account_id = near_sdk::env::current_account_id();
        match &asset_id {
            AssetId::Near => {
                let untracked = self.untracked_near();
                let amount = amount.map_or(untracked, |amount| NearToken::from_yoctonear(amount.0));
                expect!(!amount.is_zero(), "Nothing to rescue");
                expect!(
                    amount <= untracked,
                    "Can't rescue {amount}, only {untracked} is untracked"
                );
                Promise::new(to).transfer(amount)
            }
            AssetId::Nep141(contract_id) => ext_ft_core::ext(contract_id.clone())
                .with_static_gas(GAS_FOR_BALANCE_QUERY)
                .ft_balance_of(current_account_id.clone())
                .then(
                    Self::ext(current_account_id)
                        .with_static_gas(GAS_FOR_RESCUE_CALLBACK)
                        .after_rescue_balance_nep141_nep245_query(asset_id, amount, to),
                ),
            AssetId::Nep245(contract_id, token_id) => Promise::new(contract_id.clone())
                .function_call(
                    "mt_balance_of",
                    near_sdk::serde_json::json!({
                        "account_id": current_account_id,
                        "token_id": token_id,
                    })
                    .to_string()
                    .into_bytes(),
                    NearToken::ZERO,
                    GAS_FOR_BALANCE_QUERY,
                )
                .then(
                    Self::ext(current_account_id)
                        .with_static_gas(GAS_FOR_RESCUE_CALLBACK)
                        .after_rescue_balance_nep141_nep245_query(asset_id, amount, to),
                ),
            AssetId::Nep171(contract_id, token_id) => {
                expect!(
                    amount.is_none_or(|amount| amount.0 == 1),
                    "NFT amount must be 1"
                );
                ext_nft_core::ext(contract_id.clone())
                    .with_static_gas(GAS_FOR_BALANCE_QUERY)
                    .nft_token(token_id.clone())
                    .then(
                        Self::ext(current_account_id)
                            .with_static_gas(GAS_FOR_RESCUE_CALLBACK)
                            .after_rescue_nep171_query(asset_id, to),
                    )
            }
        }
    }

    #[private]
    pub fn after_rescue_balance_nep141_nep245_query(
        &mut self,
        asset_id: AssetId,
        amount: Option<U128>,
        to: AccountId,
        #[callback_result] balance: Result<U128, PromiseError>,
    ) -> Promise {
        self.assert_paused_for_rescue();
        let balance =
            balance.unwrap_or_else(|err| panic!("Failed to query balance of {asset_id}: {err:?}"));
        let in_custody = self.total_in_custody.get(&asset_id).map_or(0, |b| b.0);
        let untracked = balance.0.checked_sub(in_custody).unwrap_or_else(|| {
            panic!(
                "{asset_id} deficit: balance {} < custody {in_custody}",
                balance.0
            )
        });
        let amount = amount.map_or(untracked, |amount| amount.0);
        expect!(amount != 0, "Nothing to rescue");
        expect!(
            amount <= untracked,
            "Can't rescue {amount} {asset_id}, only {untracked} is untracked"
        );
        Self::transfer_asset_promise(&asset_id, U128(amount), to)
    }

    #[private]
    pub fn after_rescue_nep171_query(
        &mut self,
        asset_id: AssetId,
        to: AccountId,
        #[callback_result] token: Result<Option<Token>, PromiseError>,
    ) -> Promise {
        self.assert_paused_for_rescue();
        let token = token
            .unwrap_or_else(|err| panic!("Failed to query {asset_id}: {err:?}"))
            .unwrap_or_else(|| panic!("{asset_id} doesn't exist"));
        expect!(
            token.owner_id == near_sdk::env::current_account_id(),
            "{asset_id} is owned by {}",
            token.owner_id
        );
        let in_custody = self.total_in_custody.get(&asset_id).map_or(0, |b| b.0);
        expect!(in_custody == 0, "{asset_id} is in custody");
        Self::transfer_asset_promise(&asset_id, U128(1), to)
    }
}

impl DexEngine {
    fn assert_paused_for_rescue(&self) {
        expect!(self.paused, "Contract must be paused to rescue assets");
    }
}
