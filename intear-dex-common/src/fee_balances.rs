use near_sdk::{AccountId, near};

use crate::AssetId;

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct WithdrawFeesArgs {
    pub assets: Vec<AssetId>,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct GetPendingFeesArgs {
    pub account_id: AccountId,
    pub asset_ids: Vec<AssetId>,
}

#[cfg(feature = "dex")]
pub use balances::FeeBalances;

#[cfg(feature = "dex")]
mod balances {
    use std::collections::HashMap;

    use near_sdk::{
        AccountId, IntoStorageKey, near,
        store::{LookupMap, lookup_map::Entry},
    };

    use crate::{AssetId, AssetWithdrawRequest, AssetWithdrawalType, TokenAmount};

    /// Fees that accounts collected on a dex, by account and asset. A balance starts before fees
    /// are credited to it, so that whoever starts it pays for its storage.
    #[near(serializers=[borsh])]
    pub struct FeeBalances(LookupMap<(AccountId, AssetId), TokenAmount>);

    impl FeeBalances {
        pub fn new(prefix: impl IntoStorageKey) -> Self {
            Self(LookupMap::new(prefix))
        }

        /// Starts the balance of `account_id` in `asset_id`, returning whether it hadn't
        /// started yet
        pub fn start(&mut self, account_id: &AccountId, asset_id: &AssetId) -> bool {
            match self.0.entry((account_id.clone(), asset_id.clone())) {
                Entry::Occupied(_) => false,
                Entry::Vacant(balance) => {
                    balance.insert(TokenAmount::ZERO);
                    true
                }
            }
        }

        /// Adds `amount` to a balance that started
        pub fn credit(&mut self, account_id: &AccountId, asset_id: &AssetId, amount: TokenAmount) {
            self.0
                .entry((account_id.clone(), asset_id.clone()))
                .and_modify(|balance| *balance = balance.checked_add(amount).expect("Overflow"))
                .or_insert_with(|| panic!("Fee asset not registered; this is a bug"));
        }

        /// Empties a balance, returning what it had, `None` if it didn't start
        pub fn take(&mut self, account_id: &AccountId, asset_id: &AssetId) -> Option<TokenAmount> {
            self.0
                .get_mut(&(account_id.clone(), asset_id.clone()))
                .map(std::mem::take)
        }

        /// Empties the balances of `account_id` in `asset_ids` into requests that move them to
        /// its balance on the engine
        pub fn withdraw_to_engine(
            &mut self,
            account_id: &AccountId,
            asset_ids: Vec<AssetId>,
        ) -> Vec<AssetWithdrawRequest> {
            asset_ids
                .into_iter()
                .filter_map(|asset_id| {
                    let balance = self
                        .take(account_id, &asset_id)
                        .filter(|balance| !balance.is_zero())?;
                    Some(AssetWithdrawRequest {
                        asset_id,
                        amount: balance,
                        withdrawal_type: AssetWithdrawalType::ToInternalUserBalance(
                            account_id.clone(),
                        ),
                    })
                })
                .collect()
        }

        /// The balances of `account_id` in `asset_ids`, without the ones that didn't start
        pub fn balances(
            &self,
            account_id: &AccountId,
            asset_ids: Vec<AssetId>,
        ) -> HashMap<AssetId, TokenAmount> {
            asset_ids
                .into_iter()
                .filter_map(|asset_id| {
                    self.0
                        .get(&(account_id.clone(), asset_id.clone()))
                        .map(|balance| (asset_id, *balance))
                })
                .collect()
        }

        pub fn flush(&mut self) {
            self.0.flush();
        }
    }
}
