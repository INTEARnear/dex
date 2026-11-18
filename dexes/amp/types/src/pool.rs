use intear_dex_common::AssetId;
use near_sdk::{AccountId, near, store::LookupMap};

use crate::{
    Amplification, AssetWithBalance, FeeFraction, LiquidityPosition, PayoutGroups, PoolAssets,
    PoolRateSource, PoolView, Price, PricesRequest, ProviderShares, SwapFees, TokenAmount,
    TotalShares, math::RatedPool,
};

/// A pool of two assets that swaps on a StableSwap curve, with the assets rated at a fixed price
/// or the median of the prices that its oracle collects from exchanges
#[near(serializers=[borsh])]
pub struct Pool {
    pub assets: (AssetWithBalance, AssetWithBalance),
    pub decimals: (u8, u8),
    pub amplification: Amplification,
    pub rate_source: PoolRateSource,
    pub fee: FeeFraction,
    pub providers: LookupMap<AccountId, LiquidityPosition>,
    pub total_shares: TotalShares,
    pub payout_groups: PayoutGroups,
}

impl Pool {
    pub fn asset_ids(&self) -> (&AssetId, &AssetId) {
        (&self.assets.0.asset_id, &self.assets.1.asset_id)
    }

    pub fn balances(&self) -> PoolAssets<TokenAmount> {
        PoolAssets([self.assets.0.balance, self.assets.1.balance])
    }

    pub fn set_balances(&mut self, PoolAssets([balance_0, balance_1]): PoolAssets<TokenAmount>) {
        self.assets.0.balance = balance_0;
        self.assets.1.balance = balance_1;
    }

    /// What swaps on the pool ask its oracle for
    pub fn prices_request(&self) -> PricesRequest {
        PricesRequest::of_pools([self.asset_ids()])
    }

    /// The pool with `balances`, rated at `price` of its first asset in its second
    pub fn rated(&self, balances: PoolAssets<TokenAmount>, price: Price) -> RatedPool {
        RatedPool {
            balances,
            decimals: PoolAssets([self.decimals.0, self.decimals.1]),
            price,
            amplification: self.amplification,
        }
    }

    /// The shares of a liquidity provider, `None` if it isn't registered
    pub fn liquidity_shares(&self, account_id: &AccountId) -> Option<ProviderShares> {
        self.providers
            .get(account_id)
            .map(|position| position.shares)
    }

    /// The fees of a swap without a discount
    pub fn swap_fees(&self) -> SwapFees {
        SwapFees::of_pool_fee(self.fee)
    }

    /// The pool as views and events show it
    pub fn view(&self) -> PoolView {
        PoolView {
            assets: self.assets.clone(),
            decimals: self.decimals,
            amplification: self.amplification,
            rate_source: self.rate_source.clone(),
            fee: self.fee,
            total_shares: self.total_shares,
        }
    }
}
