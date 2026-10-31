use std::num::NonZeroU128;

use ethnum::U256;
use near_sdk::near;

use crate::{AssetIndex, FeePayoutType, PoolAssets, Shares, SharesBalance, math::mul_div};

pub const FEE_GROWTH_SCALE: NonZeroU128 = NonZeroU128::new(10u128.pow(18)).unwrap();

/// What one share earned in each asset of a pool, times [`FEE_GROWTH_SCALE`], as a 256-bit number
/// stored as its high and low halves.
#[near(serializers=[borsh])]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct FeeGrowth(PoolAssets<[u128; 2]>);

impl FeeGrowth {
    fn get(&self, asset_index: AssetIndex) -> U256 {
        let [high, low] = self.0[asset_index];
        U256::from_words(high, low)
    }

    fn set(&mut self, asset_index: AssetIndex, growth: U256) {
        let (high, low) = growth.into_words();
        self.0[asset_index] = [high, low];
    }

    /// Adds `amount` of the asset at `asset_index`, shared by `shares`. Returns the part of it that
    /// the shares take, which is less than `amount` by what they can't take after rounding.
    fn add(&mut self, asset_index: AssetIndex, amount: u128, shares: SharesBalance) -> u128 {
        let scale = U256::from(FEE_GROWTH_SCALE.get());
        let shares = U256::from(shares.get());
        let added_growth = U256::from(amount)
            .checked_mul(scale)
            .and_then(|scaled_amount| scaled_amount.checked_div(shares))
            .unwrap();
        self.set(
            asset_index,
            self.get(asset_index)
                .checked_add(added_growth)
                .expect("Fee growth overflow"),
        );
        // Each position rounds its part down, so all of them take at most this
        let shares_part = added_growth
            .checked_mul(shares)
            .and_then(|scaled_part| scaled_part.checked_add(scale.checked_sub(U256::ONE)?))
            .and_then(|scaled_part| scaled_part.checked_div(scale))
            .unwrap();
        u128::try_from(shares_part).expect("The shares take at most the amount")
    }

    /// What `shares` earned since the growth was `collected`, in each asset, rounded down
    fn earned_since(&self, collected: FeeGrowth, shares: Shares) -> PoolAssets<u128> {
        PoolAssets(AssetIndex::BOTH.map(|asset_index| {
            let growth = self
                .get(asset_index)
                .checked_sub(collected.get(asset_index))
                .expect("Fee growth only grows");
            let earned = growth
                .checked_mul(U256::from(shares))
                .and_then(|scaled_earned| {
                    scaled_earned.checked_div(U256::from(FEE_GROWTH_SCALE.get()))
                })
                .expect("Earned fees overflow");
            u128::try_from(earned).expect("Earned fees overflow")
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
    pub shares: Shares,
    pub fee_growth: FeeGrowth,
}

/// A registered liquidity provider of a public pool
#[near(serializers=[borsh])]
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct LiquidityPosition {
    pub payout_type: FeePayoutType,
    pub shares: Shares,
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
        shares: Shares,
    ) -> PoolAssets<u128> {
        let group = self.group_mut(position.payout_type);
        let collected_fees = position.collect(group);
        position.shares = position.shares.checked_add(shares).expect("Overflow");
        group.shares = group.shares.checked_add(shares).expect("Overflow");
        collected_fees
    }

    /// Collects what `position` earned, which this returns, and takes `shares` from it
    pub fn remove_shares(
        &mut self,
        position: &mut LiquidityPosition,
        shares: Shares,
    ) -> PoolAssets<u128> {
        let group = self.group_mut(position.payout_type);
        let collected_fees = position.collect(group);
        position.shares = position
            .shares
            .checked_sub(shares)
            .expect("Not enough shares to remove");
        group.shares = group.shares.checked_sub(shares).expect("Underflow");
        collected_fees
    }

    /// Collects what `position` earned, which this returns, and moves its shares to the group of
    /// `payout_type`
    pub fn change_payout_type(
        &mut self,
        position: &mut LiquidityPosition,
        payout_type: FeePayoutType,
    ) -> PoolAssets<u128> {
        let shares = position.shares;
        let collected_fees = self.remove_shares(position, shares);
        *position = LiquidityPosition::new(payout_type, 0, self.group(payout_type));
        self.add_shares(position, shares);
        collected_fees
    }

    /// The part of the pool's fee of a swap that each payout group takes, in proportion to its
    /// shares, each rounded down. What rounding leaves stays in the pool.
    pub fn split_pool_fee(
        &self,
        pool_fee: u128,
        total_shares: SharesBalance,
    ) -> [(FeePayoutType, u128); 3] {
        FeePayoutType::ALL.map(|payout_type| {
            (
                payout_type,
                mul_div(pool_fee, self.group(payout_type).shares, total_shares),
            )
        })
    }
}

impl PayoutGroup {
    /// Shares `amount` of the asset at `asset_index` among the group's shares. Returns the part
    /// that they can't take after rounding, which stays in the pool.
    pub fn share_fee(&mut self, asset_index: AssetIndex, amount: u128) -> u128 {
        let Some(shares) = NonZeroU128::new(self.shares) else {
            return amount;
        };
        amount
            .checked_sub(self.fee_growth.add(asset_index, amount, shares))
            .expect("Paid out more than the fee")
    }

    /// What `position` earned since it last collected, in each asset of the pool
    pub fn uncollected_fees(&self, position: &LiquidityPosition) -> PoolAssets<u128> {
        self.fee_growth
            .earned_since(position.fee_growth_collected, position.shares)
    }
}

impl LiquidityPosition {
    pub fn new(payout_type: FeePayoutType, shares: Shares, group: &PayoutGroup) -> Self {
        Self {
            payout_type,
            shares,
            fee_growth_collected: group.fee_growth,
        }
    }

    /// Returns what the position earned since it last collected, which it collects now
    pub fn collect(&mut self, group: &PayoutGroup) -> PoolAssets<u128> {
        let uncollected_fees = group.uncollected_fees(self);
        self.fee_growth_collected = group.fee_growth;
        uncollected_fees
    }
}
