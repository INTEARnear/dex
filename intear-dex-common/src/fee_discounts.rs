use near_sdk::{AccountId, BlockHeight, borsh::BorshDeserialize, near};

use crate::{AssetId, FeeFraction, SwapRequestAmount};

/// 100% = 1000000
#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct FeeDiscountFraction(u32);

pub const FULL_FEE_DISCOUNT: FeeDiscountFraction = FeeDiscountFraction::new(1_000_000);

impl FeeDiscountFraction {
    pub const fn new(millionths: u32) -> Self {
        Self(millionths)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// `fee` without the discount, rounded down
    pub fn apply(self, fee: FeeFraction) -> FeeFraction {
        let remaining_millionths = FULL_FEE_DISCOUNT
            .0
            .checked_sub(self.0)
            .expect("Fee discount can't exceed 100%");
        let discounted = u64::from(fee.get())
            .checked_mul(u64::from(remaining_millionths))
            .expect("Fee fraction overflow")
            .checked_div(u64::from(FULL_FEE_DISCOUNT.0))
            .expect("Dividing by the full fee discount");
        FeeFraction::new(
            u32::try_from(discounted)
                .expect("Discounted fee can't be higher than the original fee"),
        )
    }
}

/// The fee discount of a swap message from before the engine took fee discounts, which dexes
/// signed for themselves. Dexes read past it, and the swap pays full fees.
#[derive(BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh", use_discriminant = true)]
#[repr(u8)]
#[allow(dead_code)]
pub enum FeeDiscountInSwapMessage<Route> {
    None = 0,
    ForSwap {
        route: Route,
        asset_in: AssetId,
        asset_out: AssetId,
        trader_id: Option<AccountId>,
        amount: SwapRequestAmount,
        fee_discount: FeeDiscountFraction,
        expiration_block_height: BlockHeight,
        signature: Vec<u8>,
    } = 1,
    ForTrader {
        trader_id: AccountId,
        fee_discount: FeeDiscountFraction,
        expiration_block_height: BlockHeight,
        signature: Vec<u8>,
    } = 2,
}
