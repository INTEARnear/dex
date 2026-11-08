#![deny(clippy::arithmetic_side_effects)]

use std::{fmt, num::ParseIntError, str::FromStr};

use intear_dex_common::AssetId;
pub use intear_dex_common::{
    AssetIndex, AssetWithBalance, FULL_FEE_DISCOUNT, FULL_FEE_FRACTION, FULL_ROUTE_FRACTION,
    FeeDiscountFraction, FeeFraction, GetPendingFeesArgs, PayoutGroupShares, PoolAssets,
    ProviderShares, RouteFraction, SetFeeDiscountSignerArgs, TokenAmount, TotalShares,
    WithdrawFeesArgs,
};
use near_sdk::{AccountId, AccountIdRef, PublicKeyHandle, json_types::U128, near};

mod liquidity_fees;
pub mod math;
mod pool;
mod prices;

pub use liquidity_fees::{
    FeeGrowth, FeeGrowthPerShare, LiquidityPosition, PayoutGroup, PayoutGroups,
};
pub use pool::Pool;
pub use prices::{
    ExchangePrices, MAX_PRICE_SPREAD_MILLIONTHS, MIN_EXCHANGES, ParsePriceError, Price,
    PricesError, PricesRequest, PricesResponse,
};

#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PoolId(u32);

impl PoolId {
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for PoolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for PoolId {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

/// The part of a pool's fee that the protocol takes
#[derive(Clone, Copy)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct ProtocolFeeShare {
    percent: u32,
}

pub const PROTOCOL_FEE_SHARE: ProtocolFeeShare = ProtocolFeeShare { percent: 20 };

impl ProtocolFeeShare {
    pub fn of(self, fee: FeeFraction) -> FeeFraction {
        FeeFraction::new(
            fee.get()
                .checked_mul(self.percent)
                .expect("Fee fraction overflow")
                / 100,
        )
    }
}

impl fmt::Display for ProtocolFeeShare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.percent)
    }
}

pub const PROTOCOL_FEE_RECEIVER_ID: &AccountIdRef = AccountIdRef::new_or_panic("qoq.intear.near");

/// Who takes a part of the fee of a swap
#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum FeeReceiver {
    /// The liquidity providers of the pool, and its liquidity
    Pool,
    /// [`PROTOCOL_FEE_RECEIVER_ID`], which keeps its fees in the asset that swaps pay them in
    Protocol,
}

/// The fee of a swap, split between its receivers
#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SwapFees {
    pub pool: FeeFraction,
    pub protocol: FeeFraction,
}

impl SwapFees {
    /// The fees of a pool whose fee is `fee`, of which the protocol takes its share
    pub fn of_pool_fee(fee: FeeFraction) -> Self {
        let protocol = PROTOCOL_FEE_SHARE.of(fee);
        Self {
            pool: fee
                .checked_sub(protocol)
                .expect("The protocol's part is less than the fee"),
            protocol,
        }
    }

    pub fn total(self) -> FeeFraction {
        self.pool
            .checked_add(self.protocol)
            .expect("Fee fraction overflow")
    }

    /// Each fee is rounded down after the discount
    pub fn discounted(self, fee_discount: FeeDiscountFraction) -> Self {
        Self {
            pool: fee_discount.apply(self.pool),
            protocol: fee_discount.apply(self.protocol),
        }
    }
}

pub const INITIAL_SHARES: ProviderShares = ProviderShares::new(10u128.pow(18));

/// How a liquidity provider takes its part of the pool's fee
#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum FeePayoutType {
    KeepBoth,
    ConvertToFirstAsset,
    ConvertToSecondAsset,
}

impl FeePayoutType {
    pub const ALL: [FeePayoutType; 3] = [
        FeePayoutType::KeepBoth,
        FeePayoutType::ConvertToFirstAsset,
        FeePayoutType::ConvertToSecondAsset,
    ];

    /// Which asset of the pool the payout takes a fee paid in `fee_asset_index` in
    pub fn asset_index(self, fee_asset_index: AssetIndex) -> AssetIndex {
        match self {
            FeePayoutType::KeepBoth => fee_asset_index,
            FeePayoutType::ConvertToFirstAsset => AssetIndex::First,
            FeePayoutType::ConvertToSecondAsset => AssetIndex::Second,
        }
    }
}

/// How flat the curve of a pool is around the oracle's price, as Curve's `A`: the higher it is,
/// the less a pool's imbalance moves its price away from the oracle's
#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Amplification(u32);

pub const MIN_AMPLIFICATION: Amplification = Amplification::new(1);
pub const MAX_AMPLIFICATION: Amplification = Amplification::new(1_000_000);

impl Amplification {
    pub const fn new(amplification: u32) -> Self {
        Self(amplification)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Amplification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The most decimals an asset can have, so that a whole token fits in u128
pub const MAX_DECIMALS: u8 = 38;

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SwapArgs {
    /// The parts of the swap's amount and the pools they swap through. Every pool asks the same
    /// oracle, which one `OracleRequest` asks for the prices of all their assets with
    /// [`PricesRequest::of_pools`].
    pub routes: Vec<SwapRoute>,
    pub fee_discount: Option<SignedFeeDiscount>,
}

impl SwapArgs {
    pub fn single_pool(pool_id: PoolId) -> Self {
        Self {
            routes: vec![SwapRoute {
                fraction: FULL_ROUTE_FRACTION,
                pool_ids: vec![pool_id],
            }],
            fee_discount: None,
        }
    }
}

#[near(serializers=[borsh])]
#[derive(Clone, PartialEq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SwapRoute {
    pub fraction: RouteFraction,
    /// Pools that the route swaps through, in order. Each pool swaps the output of the previous
    /// one into its other asset, and the last one outputs the swap's asset out.
    pub pool_ids: Vec<PoolId>,
}

pub type SignedFeeDiscount = intear_dex_common::SignedFeeDiscount<Vec<SwapRoute>>;
pub type FeeDiscount = intear_dex_common::FeeDiscount<Vec<SwapRoute>>;
pub type FeeDiscountSignaturePayload =
    intear_dex_common::FeeDiscountSignaturePayload<Vec<SwapRoute>>;

/// Only the owner of the dex, the account that deployed it, can create pools
#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct CreatePoolArgs {
    pub assets: (AssetId, AssetId),
    /// Decimals of `assets`, which rate their amounts at the oracle's prices of whole tokens
    pub decimals: (u8, u8),
    pub amplification: Amplification,
    /// The oracle that swaps ask for the prices of `assets`
    pub oracle_key: PublicKeyHandle,
    /// The fee of swaps, which goes to the pool, of which the protocol takes
    /// [`PROTOCOL_FEE_SHARE`]
    pub fee: FeeFraction,
    /// Amounts of `assets` to deposit as the first liquidity, taken from the attached assets
    pub initial_liquidity: (TokenAmount, TokenAmount),
    /// How the creator takes its part of the pool's fees, as the provider of the first liquidity
    pub creator_fee_payout_type: FeePayoutType,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct CreatePoolResponse {
    pub pool_id: PoolId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SetAmplificationArgs {
    pub pool_id: PoolId,
    pub amplification: Amplification,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SetOracleKeyArgs {
    pub pool_id: PoolId,
    pub oracle_key: PublicKeyHandle,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct RegisterLiquidityArgs {
    pub pool_id: PoolId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct AddLiquidityArgs {
    pub pool_id: PoolId,
    pub min_shares_received: Option<ProviderShares>,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct AddLiquidityResponse;

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct RemoveLiquidityArgs {
    pub pool_id: PoolId,
    pub shares_to_remove: Option<ProviderShares>,
    pub min_assets_received: Option<(TokenAmount, TokenAmount)>,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct RemoveLiquidityResponse;

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SetLiquidityFeePayoutArgs {
    pub pool_id: PoolId,
    pub payout_type: FeePayoutType,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct CollectLiquidityFeesArgs {
    pub pool_id: PoolId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct GetPoolArgs {
    pub pool_id: PoolId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct GetPoolsArgs {
    pub start_index: PoolId,
    pub limit: u32,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct GetPoolSharesArgs {
    pub pool_ids: Vec<PoolId>,
    pub account_id: AccountId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct GetLiquidityPositionArgs {
    pub pool_id: PoolId,
    pub account_id: AccountId,
}

#[near(serializers=[borsh, json])]
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct LiquidityPositionView {
    pub payout_type: FeePayoutType,
    pub uncollected_fees: (TokenAmount, TokenAmount),
}

#[near(serializers=[borsh, json])]
#[derive(Clone, PartialEq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct PoolView {
    pub assets: (AssetWithBalance, AssetWithBalance),
    pub decimals: (u8, u8),
    pub amplification: Amplification,
    pub oracle_key: PublicKeyHandle,
    /// The median of the oracle's prices of a whole token of the first asset in the second that
    /// the last swap rated the pool at, `None` before the first swap
    pub last_oracle_price: Option<Price>,
    /// The fee of swaps, of which the protocol takes [`PROTOCOL_FEE_SHARE`]
    pub fee: FeeFraction,
    pub total_shares: TotalShares,
}

impl PoolView {
    /// What swaps on the pool ask its oracle for
    pub fn prices_request(&self) -> PricesRequest {
        PricesRequest::of_pools([(&self.assets.0.asset_id, &self.assets.1.asset_id)])
    }

    /// The fees of a swap on the pool without a discount
    pub fn swap_fees(&self) -> SwapFees {
        SwapFees::of_pool_fee(self.fee)
    }

    /// The pool rated at `price` of its first asset in its second, to quote swaps on it
    pub fn rated(&self, price: Price) -> math::RatedPool {
        math::RatedPool {
            balances: PoolAssets([self.assets.0.balance, self.assets.1.balance]),
            decimals: PoolAssets([self.decimals.0, self.decimals.1]),
            price,
            amplification: self.amplification,
        }
    }
}

/// The events of the dex, logged as borsh with the engine's `dex_event` host function and
/// carried in its `dex_events_borsh` log
#[near(serializers=[borsh])]
#[derive(Clone, PartialEq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum AmpDexEvent {
    PoolUpdated {
        pool_id: PoolId,
        pool: PoolView,
    },
    Swap {
        pool_id: PoolId,
        asset_in: AssetId,
        asset_out: AssetId,
        amount_in: TokenAmount,
        fees_breakdown: Vec<(FeeReceiver, AssetId, TokenAmount)>,
        amount_out: TokenAmount,
        /// The median of the oracle's prices of the first asset in the second, which the swap
        /// rated the pool at
        price: Price,
    },
    LiquidityAdded {
        pool_id: PoolId,
        asset_0: AssetId,
        asset_1: AssetId,

        added_amount_0: TokenAmount,
        added_amount_1: TokenAmount,
        minted_shares: ProviderShares,

        new_owned_asset_0: TokenAmount,
        new_owned_asset_1: TokenAmount,
        new_owned_shares: ProviderShares,

        new_total_asset_0: TokenAmount,
        new_total_asset_1: TokenAmount,
        new_total_shares: U128,
    },
    LiquidityRemoved {
        pool_id: PoolId,
        asset_0: AssetId,
        asset_1: AssetId,

        removed_amount_0: TokenAmount,
        removed_amount_1: TokenAmount,
        burned_shares: ProviderShares,

        new_owned_asset_0: TokenAmount,
        new_owned_asset_1: TokenAmount,
        new_owned_shares: ProviderShares,

        new_total_asset_0: TokenAmount,
        new_total_asset_1: TokenAmount,
        new_total_shares: U128,
    },
    LiquidityFeePayoutSet {
        pool_id: PoolId,
        account_id: AccountId,
        payout_type: FeePayoutType,
    },
    /// Pool fees that a liquidity provider earned, moved to the fees it collected
    LiquidityFeesCollected {
        pool_id: PoolId,
        account_id: AccountId,
        asset_0: AssetId,
        asset_1: AssetId,
        amount_0: TokenAmount,
        amount_1: TokenAmount,
    },
}

impl AmpDexEvent {
    #[cfg(feature = "dex")]
    pub fn emit(&self) {
        intear_dex_common::env::dex_event(self);
    }
}
