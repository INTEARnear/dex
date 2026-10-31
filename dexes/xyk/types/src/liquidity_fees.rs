use ethnum::U256;
use near_sdk::near;

use crate::{
    AssetIndex, FeePayoutType, PayoutGroupShares, PoolAssets, ProviderShares, TokenAmount,
    TotalShares,
};

/// What one liquidity share earned of an asset, times [`FeeGrowthPerShare::SCALE`], as a 256-bit
/// number stored as its high and low halves
#[near(serializers=[borsh])]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct FeeGrowthPerShare([u128; 2]);

impl FeeGrowthPerShare {
    const SCALE: u128 = 10u128.pow(18);

    fn scaled(self) -> U256 {
        let [high, low] = self.0;
        U256::from_words(high, low)
    }

    fn from_scaled(growth: U256) -> Self {
        let (high, low) = growth.into_words();
        Self([high, low])
    }

    fn of_fee(fee: TokenAmount, shares: PayoutGroupShares) -> Option<Self> {
        let shares = U256::from(shares.non_zero()?.get());
        let growth = U256::from(fee.get())
            .checked_mul(U256::from(Self::SCALE))
            .and_then(|scaled_fee| scaled_fee.checked_div(shares))
            .expect("u128 times 10^18 fits in u256");
        Some(Self::from_scaled(growth))
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        self.scaled()
            .checked_add(other.scaled())
            .map(Self::from_scaled)
    }

    fn since(self, earlier: Self) -> Self {
        Self::from_scaled(
            self.scaled()
                .checked_sub(earlier.scaled())
                .expect("Fee growth only grows"),
        )
    }

    fn earned_by(self, shares: ProviderShares) -> TokenAmount {
        let earned = self
            .scaled()
            .checked_mul(U256::from(shares.get()))
            .and_then(|scaled_earned| scaled_earned.checked_div(U256::from(Self::SCALE)))
            .expect("Earned fees overflow");
        TokenAmount::new(u128::try_from(earned).expect("Earned fees overflow"))
    }

    fn taken_by_at_most(self, shares: PayoutGroupShares) -> TokenAmount {
        // Each position rounds its part down, so all of them take at most this
        let shares = U256::from(shares.non_zero().map_or(0, |shares| shares.get()));
        let scale = U256::from(Self::SCALE);
        let taken = self
            .scaled()
            .checked_mul(shares)
            .and_then(|scaled_taken| scaled_taken.checked_add(scale.checked_sub(U256::ONE)?))
            .and_then(|scaled_taken| scaled_taken.checked_div(scale))
            .expect("Fee growth overflow");
        TokenAmount::new(u128::try_from(taken).expect("The shares take at most the fee"))
    }
}

#[near(serializers=[borsh])]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct FeeGrowth(PoolAssets<FeeGrowthPerShare>);

impl FeeGrowth {
    /// Adds `fee` of the asset at `asset_index`, shared by `shares`. Returns the part of it that
    /// the shares take, which is less than `fee` by what they can't take after rounding.
    fn add(
        &mut self,
        asset_index: AssetIndex,
        fee: TokenAmount,
        shares: PayoutGroupShares,
    ) -> TokenAmount {
        let Some(added_growth) = FeeGrowthPerShare::of_fee(fee, shares) else {
            return TokenAmount::ZERO;
        };
        self.0[asset_index] = self.0[asset_index]
            .checked_add(added_growth)
            .expect("Fee growth overflow");
        added_growth.taken_by_at_most(shares)
    }

    /// What `shares` earned since the growth was `collected`, in each asset, rounded down
    fn earned_since(
        &self,
        collected: FeeGrowth,
        shares: ProviderShares,
    ) -> PoolAssets<TokenAmount> {
        PoolAssets(AssetIndex::BOTH.map(|asset_index| {
            self.0[asset_index]
                .since(collected.0[asset_index])
                .earned_by(shares)
        }))
    }
}

/// The payout group of each payout
#[near(serializers=[borsh])]
#[derive(Clone, Default)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct PayoutGroups {
    pub keep_both: PayoutGroup,
    pub convert_to_first_asset: PayoutGroup,
    pub convert_to_second_asset: PayoutGroup,
}

/// The liquidity providers that take their fees with one payout
#[near(serializers=[borsh])]
#[derive(Clone, Default)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct PayoutGroup {
    pub shares: PayoutGroupShares,
    pub fee_growth: FeeGrowth,
}

/// A registered liquidity provider of a public pool
#[near(serializers=[borsh])]
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct LiquidityPosition {
    pub payout_type: FeePayoutType,
    pub shares: ProviderShares,
    /// The fee growth of the payout's group when the position last collected
    pub fee_growth_collected: FeeGrowth,
}

impl PayoutGroups {
    pub fn group(&self, payout_type: FeePayoutType) -> &PayoutGroup {
        match payout_type {
            FeePayoutType::KeepBoth => &self.keep_both,
            FeePayoutType::ConvertToFirstAsset => &self.convert_to_first_asset,
            FeePayoutType::ConvertToSecondAsset => &self.convert_to_second_asset,
        }
    }

    pub fn group_mut(&mut self, payout_type: FeePayoutType) -> &mut PayoutGroup {
        match payout_type {
            FeePayoutType::KeepBoth => &mut self.keep_both,
            FeePayoutType::ConvertToFirstAsset => &mut self.convert_to_first_asset,
            FeePayoutType::ConvertToSecondAsset => &mut self.convert_to_second_asset,
        }
    }

    /// Collects what `position` earned, which this returns, and gives it `shares` more
    pub fn add_shares(
        &mut self,
        position: &mut LiquidityPosition,
        shares: ProviderShares,
    ) -> PoolAssets<TokenAmount> {
        let group = self.group_mut(position.payout_type);
        let collected_fees = position.collect(group);
        position.shares = position.shares.checked_add(shares).expect("Overflow");
        group.shares = group.shares.joined_by(shares).expect("Overflow");
        collected_fees
    }

    /// Collects what `position` earned, which this returns, and takes `shares` from it
    pub fn remove_shares(
        &mut self,
        position: &mut LiquidityPosition,
        shares: ProviderShares,
    ) -> PoolAssets<TokenAmount> {
        let group = self.group_mut(position.payout_type);
        let collected_fees = position.collect(group);
        position.shares = position
            .shares
            .checked_sub(shares)
            .expect("Not enough shares to remove");
        group.shares = group.shares.left_by(shares).expect("Underflow");
        collected_fees
    }

    /// Collects what `position` earned, which this returns, and moves its shares to the group of
    /// `payout_type`
    pub fn change_payout_type(
        &mut self,
        position: &mut LiquidityPosition,
        payout_type: FeePayoutType,
    ) -> PoolAssets<TokenAmount> {
        let shares = position.shares;
        let collected_fees = self.remove_shares(position, shares);
        *position =
            LiquidityPosition::new(payout_type, ProviderShares::ZERO, self.group(payout_type));
        self.add_shares(position, shares);
        collected_fees
    }

    /// The part of the pool's fee of a swap that each payout group takes, in proportion to its
    /// shares, each rounded down. What rounding leaves stays in the pool.
    pub fn split_pool_fee(
        &self,
        pool_fee: TokenAmount,
        total_shares: TotalShares,
    ) -> [(FeePayoutType, TokenAmount); 3] {
        FeePayoutType::ALL.map(|payout_type| {
            (
                payout_type,
                self.group(payout_type)
                    .shares
                    .part_of(pool_fee, total_shares),
            )
        })
    }
}

impl PayoutGroup {
    /// Shares `fee` of the asset at `asset_index` among the group's shares. Returns the part
    /// that they can't take after rounding, which stays in the pool.
    pub fn share_fee(&mut self, asset_index: AssetIndex, fee: TokenAmount) -> TokenAmount {
        fee.checked_sub(self.fee_growth.add(asset_index, fee, self.shares))
            .expect("Paid out more than the fee")
    }

    /// What `position` earned since it last collected, in each asset of the pool
    pub fn uncollected_fees(&self, position: &LiquidityPosition) -> PoolAssets<TokenAmount> {
        self.fee_growth
            .earned_since(position.fee_growth_collected, position.shares)
    }
}

impl LiquidityPosition {
    pub fn new(payout_type: FeePayoutType, shares: ProviderShares, group: &PayoutGroup) -> Self {
        Self {
            payout_type,
            shares,
            fee_growth_collected: group.fee_growth,
        }
    }

    /// Returns what the position earned since it last collected, which it collects now
    pub fn collect(&mut self, group: &PayoutGroup) -> PoolAssets<TokenAmount> {
        let uncollected_fees = group.uncollected_fees(self);
        self.fee_growth_collected = group.fee_growth;
        uncollected_fees
    }
}
