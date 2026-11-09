#![deny(clippy::arithmetic_side_effects)]

use std::{fmt, num::ParseIntError, str::FromStr};

use intear_dex_common::AssetId;
pub use intear_dex_common::{
    AssetIndex, AssetWithBalance, FULL_FEE_DISCOUNT, FULL_FEE_FRACTION, FULL_ROUTE_FRACTION,
    FeeDiscountFraction, FeeFraction, GetPendingFeesArgs, PayoutGroupShares, PoolAssets,
    ProviderShares, RouteFraction, SetFeeDiscountSignerArgs, TokenAmount, TotalShares,
    WithdrawFeesArgs,
};
use near_sdk::{
    AccountId, AccountIdRef, Timestamp,
    borsh::{BorshSerialize, io::Write},
    json_types::U128,
    near,
};

mod legacy_deserialization;
mod liquidity_fees;
pub mod math;
mod pool;

pub use legacy_deserialization::XykDexEventV1;
pub use liquidity_fees::{
    DEFAULT_FEE_PAYOUT_TYPE, FeeGrowth, FeeGrowthPerShare, LiquidityPosition, PayoutGroup,
    PayoutGroups, ShareGrowth,
};
pub use pool::Pool;

#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PoolId(u32);

pub const LAST_CREATED_POOL_ID_MARKER: PoolId = PoolId(u32::MAX);

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

#[derive(Clone, Copy)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SwapReserves {
    pub input: TokenAmount,
    pub output: TokenAmount,
}

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
                .checked_div(100)
                .expect("Dividing by 100"),
        )
    }
}

impl fmt::Display for ProtocolFeeShare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.percent)
    }
}

pub const INITIAL_SHARES: ProviderShares = ProviderShares::new(10u128.pow(18));

pub const PRIVATE_POOL_TOTAL_SHARES: TotalShares = TotalShares::new(INITIAL_SHARES.get());

pub const MAX_TOTAL_FEE_FRACTION: FeeFraction = FeeFraction::new(900_000); // 90%
pub const MAX_FEE_RECEIVERS: usize = 20;
pub const PROTOCOL_FEE_RECEIVER_ID: &AccountIdRef = AccountIdRef::new_or_panic("plach.intear.near");

pub const STABLE_ASSETS_NEP141: &[&AccountIdRef] = &[
    AccountIdRef::new_or_panic("17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1"),
    AccountIdRef::new_or_panic("usdt.tether-token.near"),
];
const _: () = assert!(
    MAX_TOTAL_FEE_FRACTION.get() < FULL_FEE_FRACTION.get(),
    "Fees must stay below 100%"
);

pub const CAN_MIGRATE: &AccountIdRef = AccountIdRef::new_or_panic("slimedragon.near");

#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SwapArgs {
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

/// A swap through a single pool without a fee discount is serialized as just
/// its pool ID, which is what dex versions from before routes read
impl BorshSerialize for SwapArgs {
    fn serialize<W: Write>(&self, writer: &mut W) -> near_sdk::borsh::io::Result<()> {
        if let ([SwapRoute { fraction, pool_ids }], None) =
            (self.routes.as_slice(), &self.fee_discount)
            && *fraction == FULL_ROUTE_FRACTION
            && let [pool_id] = pool_ids.as_slice()
        {
            return BorshSerialize::serialize(pool_id, writer);
        }
        self.routes.serialize(writer)?;
        self.fee_discount.serialize(writer)
    }
}

#[near(serializers=[borsh])]
#[derive(Clone, PartialEq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SwapRoute {
    pub fraction: RouteFraction,
    /// Pools that the route swaps through, in order. Each pool swaps the
    /// output of the previous one into its other asset, and the last one
    /// outputs [`SwapRequest::asset_out`].
    pub pool_ids: Vec<PoolId>,
}

pub type SignedFeeDiscount = intear_dex_common::SignedFeeDiscount<Vec<SwapRoute>>;
pub type FeeDiscount = intear_dex_common::FeeDiscount<Vec<SwapRoute>>;
pub type FeeDiscountSignaturePayload =
    intear_dex_common::FeeDiscountSignaturePayload<Vec<SwapRoute>>;

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct CreatePoolArgs {
    pub assets: (AssetId, AssetId),
    pub fees: FeeConfigurationV2,
    pub pool_type: PoolType,
    /// Amounts of `assets` to deposit as the first liquidity, taken from the
    /// attached assets. Required for public pools, so nobody else can set their
    /// ratio before the creator does. Optional for private pools, not accepted
    /// for launch pools.
    pub initial_liquidity: Option<(TokenAmount, TokenAmount)>,
    /// How the creator takes its part of the pool's fees, as the provider of the first
    /// liquidity. Required for public pools, not accepted for others, which have no providers.
    pub creator_fee_payout_type: Option<FeePayoutType>,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct CreatePoolResponse {
    pub pool_id: PoolId,
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
pub struct EditFeesArgs {
    pub pool_id: PoolId,
    pub fees: FeeConfigurationV2,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct LockPoolArgs {
    pub pool_id: PoolId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct RegisterFeeAssetsArgs {
    pub asset_ids: Vec<AssetId>,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct ClaimFeesArgs {
    pub account_id: AccountId,
    pub asset_ids: Vec<AssetId>,
}

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
pub struct GetClaimableFeesArgs {
    pub account_id: AccountId,
    pub asset_ids: Vec<AssetId>,
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

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum PoolType {
    PrivateLatest,
    PublicLatest,
    LaunchLatest { phantom_liquidity: TokenAmount },
    LaunchV1 { phantom_liquidity_near: TokenAmount },
    PrivateV1,
    PublicV1,
    PrivateV2,
    PublicV2,
    LaunchV2 { phantom_liquidity: TokenAmount },
    PublicV3,
}

impl PoolType {
    pub fn is_launch(&self) -> bool {
        match self {
            PoolType::LaunchLatest { .. }
            | PoolType::LaunchV1 { .. }
            | PoolType::LaunchV2 { .. } => true,
            PoolType::PrivateLatest
            | PoolType::PublicLatest
            | PoolType::PrivateV1
            | PoolType::PublicV1
            | PoolType::PrivateV2
            | PoolType::PublicV2
            | PoolType::PublicV3 => false,
        }
    }
}

#[near(serializers=[json])]
#[serde(from = "legacy_deserialization::JsonPoolView")]
#[derive(Clone, PartialEq, BorshSerialize)]
#[borsh(crate = "near_sdk::borsh")]
#[borsh(use_discriminant = true)]
#[cfg_attr(
    all(feature = "abi", not(target_arch = "wasm32")),
    derive(near_sdk::borsh::BorshSchema)
)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[repr(u8)]
pub enum PoolView {
    Private {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV2,
        owner_id: AccountId,
        locked: bool,
    } = 4,
    Public {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV2,
        total_shares: TotalShares,
        reinvest_share_growth: Option<ShareGrowth>,
    } = 7,
    Launch {
        quote_asset: AssetWithBalance,
        launched_asset: AssetWithBalance,
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV2,
        phantom_liquidity: TokenAmount,
    } = 6,
}

#[near(serializers=[borsh, json])]
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct CurrentFees {
    pub receivers: Vec<(FeeReceiver, FeeFraction)>,
}

#[near(serializers=[borsh, json])]
#[serde(from = "legacy_deserialization::JsonFeeConfiguration")]
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct FeeConfigurationV2 {
    pub receivers: Vec<(FeeReceiver, FeeAmount)>,
}

/// Who can claim an account's fees to the account
#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum ClaimMode {
    /// The account, which withdraws them to its balance on the engine with `withdraw_fees`
    OnlyOwner,
    /// Anyone, who sends them to the account's wallet with `claim_fees`. For accounts that
    /// don't call the dex themselves, like a community's treasury, or fees that a smart
    /// contract receives and distributes to holders. It doesn't check for storage
    /// registrations and other prerequisites to receiving a transfer. Not recomended for
    /// normal LP providers.
    Anyone,
}

#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum FeePayoutType {
    KeepBoth,
    ConvertToFirstAsset,
    ConvertToSecondAsset,
    Reinvest,
}

impl FeePayoutType {
    pub const ALL: [FeePayoutType; 4] = [
        FeePayoutType::KeepBoth,
        FeePayoutType::ConvertToFirstAsset,
        FeePayoutType::ConvertToSecondAsset,
        FeePayoutType::Reinvest,
    ];

    /// Which asset of the pool the payout takes a fee paid in `fee_asset_index` in
    pub fn asset_index(self, fee_asset_index: AssetIndex) -> AssetIndex {
        match self {
            FeePayoutType::KeepBoth | FeePayoutType::Reinvest => fee_asset_index,
            FeePayoutType::ConvertToFirstAsset => AssetIndex::First,
            FeePayoutType::ConvertToSecondAsset => AssetIndex::Second,
        }
    }
}

#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum FeeAmount {
    Fixed(FeeFraction),
    Scheduled {
        start: (Timestamp, FeeFraction),
        end: (Timestamp, FeeFraction),
        curve: ScheduledFeeCurve,
    },
    Dynamic {
        min: FeeFraction,
        max: FeeFraction,
    },
}

#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum ScheduledFeeCurve {
    Linear,
}

#[near(serializers=[json])]
#[serde(from = "legacy_deserialization::JsonFeeReceiver")]
#[derive(PartialEq, Eq, Clone, BorshSerialize)]
#[borsh(crate = "near_sdk::borsh")]
#[borsh(use_discriminant = true)]
#[cfg_attr(
    all(feature = "abi", not(target_arch = "wasm32")),
    derive(near_sdk::borsh::BorshSchema)
)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[repr(u8)]
pub enum FeeReceiver {
    Pool = 1,
    Account {
        account_id: AccountId,
        payout_type: FeePayoutType,
        claim_mode: ClaimMode,
    } = 3,
    /// Only in the fees of a swap, never in a fee configuration
    Protocol = 4,
}

/// The events of the dex, logged as borsh with the engine's `dex_event` host function and
/// carried in its `dex_events_borsh` log. They have no JSON form: the dex logged
/// [`XykDexEventV1`] as JSON before.
#[near(serializers=[borsh])]
#[derive(Clone, PartialEq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum XykDexEvent {
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

impl XykDexEvent {
    #[cfg(feature = "dex")]
    pub fn emit(&self) {
        intear_dex_common::env::dex_event(self);
    }
}

pub fn is_stable_asset(asset_id: &AssetId) -> bool {
    match asset_id {
        AssetId::Near => true,
        AssetId::Nep141(account_id) => STABLE_ASSETS_NEP141.contains(&&**account_id),
        AssetId::Nep171(_, _) => false,
        AssetId::Nep245(_, _) => false,
    }
}

pub fn protocol_fee_payout(asset_ids: (&AssetId, &AssetId)) -> FeePayoutType {
    if is_stable_asset(asset_ids.0) {
        FeePayoutType::ConvertToFirstAsset
    } else if is_stable_asset(asset_ids.1) {
        FeePayoutType::ConvertToSecondAsset
    } else {
        FeePayoutType::KeepBoth
    }
}

impl FeeAmount {
    pub fn validate(&self) -> Result<(), FeeAmountError> {
        match self {
            FeeAmount::Fixed(fee_fraction) => {
                if *fee_fraction >= FULL_FEE_FRACTION {
                    return Err(FeeAmountError::FixedFeeTooHigh);
                }
            }
            FeeAmount::Scheduled {
                start,
                end,
                curve: _,
            } => {
                if start.0 >= end.0 {
                    return Err(FeeAmountError::ScheduleStartNotBeforeEnd);
                }
                if start.1 <= end.1 {
                    return Err(FeeAmountError::ScheduledFeeNotDecreasing);
                }
                if start.1 >= FULL_FEE_FRACTION {
                    return Err(FeeAmountError::ScheduledStartFeeTooHigh);
                }
            }
            FeeAmount::Dynamic { min, max } => {
                if *min >= *max {
                    return Err(FeeAmountError::DynamicMinNotBelowMax);
                }
                if *max >= FULL_FEE_FRACTION {
                    return Err(FeeAmountError::DynamicMaxTooHigh);
                }
                return Err(FeeAmountError::DynamicFeeNotImplemented);
            }
        }
        Ok(())
    }

    pub fn fee_fraction_at(&self, timestamp: Timestamp) -> FeeFraction {
        match self {
            FeeAmount::Fixed(fee_fraction) => *fee_fraction,
            FeeAmount::Scheduled { start, end, curve } => {
                let (start_time, start_fee_fraction) = *start;
                let (end_time, end_fee_fraction) = *end;
                let Some(time_elapsed) = timestamp.checked_sub(start_time) else {
                    return start_fee_fraction;
                };
                if timestamp >= end_time {
                    return end_fee_fraction;
                }

                // Was checked in .validate() check
                let total_duration = end_time.checked_sub(start_time).unwrap();
                let fee_range = start_fee_fraction.checked_sub(end_fee_fraction).unwrap();

                let fee_decrease = match curve {
                    ScheduledFeeCurve::Linear => {
                        // total_duration is not 0 due to .validate() check
                        let fee_decrease = u128::from(fee_range.get())
                            .checked_mul(u128::from(time_elapsed))
                            .and_then(|product| product.checked_div(u128::from(total_duration)))
                            .expect("Fee decrease calculation overflow");
                        FeeFraction::new(
                            u32::try_from(fee_decrease).expect("Fee decrease overflows u32"),
                        )
                    }
                };

                assert!(
                    fee_decrease <= fee_range,
                    "Fee decrease must be less than end and start fee difference"
                );

                start_fee_fraction
                    .checked_sub(fee_decrease)
                    .expect("Fee calculation underflow")
            }
            FeeAmount::Dynamic { min: _, max: _ } => {
                unimplemented!("Dynamic fee configuration is not implemented yet");
            }
        }
    }
}

impl FeeConfigurationV2 {
    pub fn receivers_at(&self, timestamp: Timestamp) -> Vec<(FeeReceiver, FeeFraction)> {
        self.receivers
            .iter()
            .map(|(receiver, fee)| (receiver.clone(), fee.fee_fraction_at(timestamp)))
            .collect()
    }

    pub fn validate(&self, timestamp: Timestamp) -> Result<(), FeeConfigurationError> {
        for (_, fee_amount) in &self.receivers {
            fee_amount
                .validate()
                .map_err(FeeConfigurationError::InvalidFeeAmount)?;
        }
        let receivers = self.receivers_at(timestamp);
        if receivers.len() > MAX_FEE_RECEIVERS {
            return Err(FeeConfigurationError::TooManyReceivers);
        }
        if receivers.iter().any(|(_, fee)| *fee >= FULL_FEE_FRACTION) {
            return Err(FeeConfigurationError::ReceiverFeeTooHigh);
        }
        let total_fee = receivers
            .iter()
            .map(|(_, fee)| *fee)
            .try_fold(FeeFraction::ZERO, |acc, fee| acc.checked_add(fee))
            .unwrap();
        if total_fee > MAX_TOTAL_FEE_FRACTION {
            return Err(FeeConfigurationError::TotalFeeTooHigh);
        }
        if receivers.iter().any(|(receiver, _)| match receiver {
            FeeReceiver::Account { account_id, .. } => account_id == PROTOCOL_FEE_RECEIVER_ID,
            FeeReceiver::Protocol => true,
            FeeReceiver::Pool => false,
        }) {
            return Err(FeeConfigurationError::ProtocolFeeReceiverSet);
        }
        if receivers.iter().any(|(receiver, _)| {
            matches!(
                receiver,
                FeeReceiver::Account {
                    payout_type: FeePayoutType::Reinvest,
                    ..
                }
            )
        }) {
            return Err(FeeConfigurationError::AccountReinvests);
        }
        Ok(())
    }

    pub fn with_protocol_fee(&self, timestamp: Timestamp) -> CurrentFees {
        let mut protocol_fee = FeeFraction::ZERO;
        let mut receivers = Vec::new();
        for (receiver, fee_fraction) in self.receivers_at(timestamp) {
            let protocol_part = PROTOCOL_FEE_SHARE.of(fee_fraction);
            protocol_fee = protocol_fee
                .checked_add(protocol_part)
                .expect("Fee fraction overflow");
            receivers.push((
                receiver,
                fee_fraction
                    .checked_sub(protocol_part)
                    .expect("The protocol's part is less than the fee"),
            ));
        }
        receivers.push((FeeReceiver::Protocol, protocol_fee));
        CurrentFees { receivers }
    }
}

impl CurrentFees {
    pub fn total_fee_fraction(&self) -> FeeFraction {
        self.receivers
            .iter()
            .try_fold(FeeFraction::ZERO, |total, (_, fee_fraction)| {
                total.checked_add(*fee_fraction)
            })
            .expect("Fee fraction overflow")
    }

    /// The fee of each receiver on `amount_in`, each rounded down
    pub fn amounts(
        &self,
        amount_in: TokenAmount,
    ) -> impl Iterator<Item = (&FeeReceiver, TokenAmount)> {
        self.receivers
            .iter()
            .map(move |(receiver, fee_fraction)| (receiver, fee_fraction.of(amount_in)))
    }

    /// Each fee is rounded down after the discount
    pub fn discounted(mut self, fee_discount: FeeDiscountFraction) -> CurrentFees {
        for (_, fee_fraction) in self.receivers.iter_mut() {
            *fee_fraction = fee_discount.apply(*fee_fraction);
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeAmountError {
    FixedFeeTooHigh,
    ScheduleStartNotBeforeEnd,
    ScheduledFeeNotDecreasing,
    ScheduledStartFeeTooHigh,
    DynamicMinNotBelowMax,
    DynamicMaxTooHigh,
    DynamicFeeNotImplemented,
}

impl fmt::Display for FeeAmountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FixedFeeTooHigh => write!(f, "Fee must be less than {FULL_FEE_FRACTION}"),
            Self::ScheduleStartNotBeforeEnd => {
                write!(f, "Scheduled fee start time must be before end time")
            }
            Self::ScheduledFeeNotDecreasing => {
                write!(f, "Start fee fraction must be greater than end fee")
            }
            Self::ScheduledStartFeeTooHigh => {
                write!(
                    f,
                    "Start fee fraction must be less than {FULL_FEE_FRACTION}"
                )
            }
            Self::DynamicMinNotBelowMax => {
                write!(f, "Min fee fraction must be less than max fee fraction")
            }
            Self::DynamicMaxTooHigh => {
                write!(f, "Max fee fraction must be less than {FULL_FEE_FRACTION}")
            }
            Self::DynamicFeeNotImplemented => {
                write!(f, "Dynamic fee configuration is not implemented yet")
            }
        }
    }
}

impl std::error::Error for FeeAmountError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeConfigurationError {
    InvalidFeeAmount(FeeAmountError),
    TooManyReceivers,
    ReceiverFeeTooHigh,
    TotalFeeTooHigh,
    ProtocolFeeReceiverSet,
    AccountReinvests,
}

impl fmt::Display for FeeConfigurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFeeAmount(fee_amount_error) => write!(f, "{fee_amount_error}"),
            Self::TooManyReceivers => write!(f, "Too many fee receivers"),
            Self::ReceiverFeeTooHigh => {
                write!(f, "Fee must be less than {FULL_FEE_FRACTION} per receiver")
            }
            Self::TotalFeeTooHigh => {
                write!(f, "Fees must add up to at most {MAX_TOTAL_FEE_FRACTION}")
            }
            Self::ProtocolFeeReceiverSet => {
                write!(f, "Protocol fee receiver can't be set by users")
            }
            Self::AccountReinvests => {
                write!(f, "Only liquidity providers can reinvest their fees")
            }
        }
    }
}

impl std::error::Error for FeeConfigurationError {}
