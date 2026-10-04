#![deny(clippy::arithmetic_side_effects)]

use std::{fmt, num::NonZeroU128};

use intear_dex_types::{AssetId, SwapRequest};
use near_sdk::{AccountId, AccountIdRef, Timestamp, json_types::U128, near, serde::Deserialize};

pub type PoolId = u32;

pub const LAST_CREATED_POOL_ID_MARKER: PoolId = PoolId::MAX;

/// 100% = 1000000
pub type FeeFraction = u32;
pub const FULL_FEE_FRACTION: FeeFraction = 1000000;
pub const MAX_TOTAL_FEE_FRACTION: FeeFraction = FULL_FEE_FRACTION / 2; // 50%
pub const MAX_FEE_RECEIVERS: usize = 42;
pub const PROTOCOL_FEE: FeeFraction = FULL_FEE_FRACTION / 1000; // 0.1%
pub const PROTOCOL_FEE_RECEIVER_ID: &AccountIdRef = AccountIdRef::new_or_panic("plach.intear.near");

pub type SharesBalance = NonZeroU128;
pub const INITIAL_SHARES: SharesBalance = NonZeroU128::new(10u128.pow(18)).unwrap();

pub const NEAR_ACCOUNT_ID: &AccountIdRef = AccountIdRef::new_or_panic("near");
pub const PROTOCOL_FEE_REDUCE_ASSET_PARENT_ACCOUNTS: &[&AccountIdRef] = &[
    AccountIdRef::new_or_panic("omft.near"),
    AccountIdRef::new_or_panic("omni.hot.tg"),
];
pub const PROTOCOL_FEE_REDUCE_ASSET_ACCOUNTS: &[&AccountIdRef] = &[
    AccountIdRef::new_or_panic("17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1"),
    AccountIdRef::new_or_panic("usdt.tether-token.near"),
    NEAR_ACCOUNT_ID,
];
pub const PROTOCOL_FEE_REDUCED: FeeFraction = 1; // 0.0001%
pub const MAX_REFERRAL_FEE_FRACTION: FeeFraction = FULL_FEE_FRACTION / 20; // 5%

pub const CAN_MIGRATE: &AccountIdRef = AccountIdRef::new_or_panic("slimedragon.near");

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SwapArgs {
    pub pool_id: PoolId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct CreatePoolArgs {
    pub assets: (AssetId, AssetId),
    pub fees: FeeConfiguration,
    pub pool_type: PoolType,
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
    pub min_shares_received: Option<SharesBalance>,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct AddLiquidityResponse;

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct RemoveLiquidityArgs {
    pub pool_id: PoolId,
    pub shares_to_remove: Option<SharesBalance>,
    pub min_assets_received: Option<(U128, U128)>,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct RemoveLiquidityResponse;

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct EditFeesArgs {
    pub pool_id: PoolId,
    pub fees: FeeConfiguration,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct WithdrawFeesArgs {
    pub assets: Vec<AssetId>,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct UpgradePoolArgs {
    pub pool_id: PoolId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct LockPoolArgs {
    pub pool_id: PoolId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SetReferrerSettingsArgs {
    pub new_settings: ReferralSettings,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct RegisterFeeAssetsArgs {
    pub asset_ids: Vec<AssetId>,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct WithdrawCommunityFeeArgs {
    pub account_id: AccountId,
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
    pub limit: PoolId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct GetPoolSharesArgs {
    pub pool_ids: Vec<PoolId>,
    pub account_id: AccountId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct GetPendingFeesArgs {
    pub account_id: AccountId,
    pub asset_ids: Vec<AssetId>,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct PoolNeedsUpgradeArgs {
    pub pool_id: PoolId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct GetCommunityOwnedFeesArgs {
    pub account_id: AccountId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct GetReferralSettingsArgs {
    pub account_id: AccountId,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum PoolType {
    PrivateLatest,
    PublicLatest,
    LaunchLatest { phantom_liquidity_near: U128 },
    LaunchV1 { phantom_liquidity_near: U128 },
    PrivateV1,
    PublicV1,
    PrivateV2,
    PublicV2,
}

#[near(serializers=[borsh, json])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum PoolView {
    Private {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        fee_configuration: FeeConfiguration,
        owner_id: AccountId,
        locked: bool,
    },
    Public {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        fee_configuration: FeeConfiguration,
        total_shares: Option<U128>,
    },
    Launch {
        near_amount: U128,
        launched_asset: AssetWithBalance,
        fees: CurrentFees,
        fee_configuration: FeeConfiguration,
        phantom_liquidity_near: U128,
    },
}

#[near(serializers=[borsh, json])]
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct AssetWithBalance {
    pub asset_id: AssetId,
    pub balance: U128,
}

#[near(serializers=[borsh, json])]
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[serde(untagged)]
pub enum FeeConfiguration {
    V1(CurrentFees),
    V2(V2FeeConfiguration),
}

#[near(serializers=[borsh, json])]
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct CurrentFees {
    pub receivers: Vec<(FeeReceiver, FeeFraction)>,
}

#[near(serializers=[borsh, json])]
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct V2FeeConfiguration {
    pub receivers: Vec<(FeeReceiver, FeeAmount)>,
}

#[near(serializers=[borsh, json])]
#[derive(Clone, Copy)]
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
#[derive(Clone, Copy)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum ScheduledFeeCurve {
    Linear,
}

#[near(serializers=[borsh, json])]
#[derive(PartialEq, Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum FeeReceiver {
    Account(AccountId),
    Pool,
    Community(AccountId),
}

#[near(serializers=[borsh])]
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum ReferralSettings {
    V1 {
        fee_fraction: FeeFraction,
        fee_fraction_reduced: FeeFraction,
    },
}

#[near(event_json(standard = "xyk"))]
#[derive(Deserialize)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum XykDexEvent {
    #[event_version("1.0.0")]
    PoolUpdated { pool_id: PoolId, pool: PoolView },
    #[event_version("1.0.0")]
    Swap {
        pool_id: PoolId,
        request: SwapRequest,
        amount_in: U128,
        fees_breakdown: Vec<(FeeReceiver, AssetId, U128)>,
        amount_out: U128,
    },
    #[event_version("1.0.0")]
    LiquidityAdded {
        pool_id: PoolId,
        asset_0: AssetId,
        asset_1: AssetId,

        added_amount_0: U128,
        added_amount_1: U128,
        minted_shares: U128,

        new_owned_asset_0: U128,
        new_owned_asset_1: U128,
        new_owned_shares: U128,

        new_total_asset_0: U128,
        new_total_asset_1: U128,
        new_total_shares: U128,
    },
    #[event_version("1.0.0")]
    LiquidityRemoved {
        pool_id: PoolId,
        asset_0: AssetId,
        asset_1: AssetId,

        removed_amount_0: U128,
        removed_amount_1: U128,
        burned_shares: U128,

        new_owned_asset_0: U128,
        new_owned_asset_1: U128,
        new_owned_shares: U128,

        new_total_asset_0: U128,
        new_total_asset_1: U128,
        new_total_shares: U128,
    },
}

pub fn asset_account_ids<const N: usize>(asset_ids: [&AssetId; N]) -> [&AccountIdRef; N] {
    asset_ids.map(|asset_id| match asset_id {
        AssetId::Near => NEAR_ACCOUNT_ID,
        AssetId::Nep141(account_id) => account_id,
        AssetId::Nep171(_, _) => panic!("Nep171 assets are not supported"),
        AssetId::Nep245(account_id, _) => account_id,
    })
}

pub fn should_reduce_fee(asset_account_ids: &[&AccountIdRef]) -> bool {
    'assets: for asset_account_id in asset_account_ids {
        for reduce_asset_account in PROTOCOL_FEE_REDUCE_ASSET_ACCOUNTS {
            if asset_account_id.as_str() == *reduce_asset_account {
                continue 'assets;
            }
        }
        for bypass_asset_parent_account in PROTOCOL_FEE_REDUCE_ASSET_PARENT_ACCOUNTS {
            if asset_account_id.is_sub_account_of(bypass_asset_parent_account) {
                continue 'assets;
            }
        }
        // If not matched by either of the above, it's a non-stable asset.
        return false;
    }
    true
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
                        let fee_decrease = u128::from(fee_range)
                            .checked_mul(u128::from(time_elapsed))
                            .and_then(|product| product.checked_div(u128::from(total_duration)))
                            .expect("Fee decrease calculation overflow");
                        FeeFraction::try_from(fee_decrease).expect("Fee decrease overflows u32")
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

impl FeeConfiguration {
    pub fn receivers_at(&self, timestamp: Timestamp) -> Vec<(FeeReceiver, FeeFraction)> {
        match self {
            FeeConfiguration::V1(fees) => fees.receivers.clone(),
            FeeConfiguration::V2(fees) => fees
                .receivers
                .iter()
                .map(|(receiver, fee)| (receiver.clone(), fee.fee_fraction_at(timestamp)))
                .collect(),
        }
    }

    pub fn validate(
        &self,
        pool_type: &PoolType,
        timestamp: Timestamp,
    ) -> Result<(), FeeConfigurationError> {
        match self {
            FeeConfiguration::V1(_) => {}
            FeeConfiguration::V2(fees) => {
                for (_, fee_amount) in fees.receivers.iter() {
                    fee_amount
                        .validate()
                        .map_err(FeeConfigurationError::InvalidFeeAmount)?;
                }
            }
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
            .try_fold(0u32, |acc, fee| acc.checked_add(fee))
            .unwrap();
        if total_fee >= MAX_TOTAL_FEE_FRACTION {
            return Err(FeeConfigurationError::TotalFeeTooHigh);
        }
        if receivers.iter().any(|(receiver, _)| {
            matches!(
                receiver,
                FeeReceiver::Account(account_id) if account_id == PROTOCOL_FEE_RECEIVER_ID
            )
        }) {
            return Err(FeeConfigurationError::ProtocolFeeReceiverSet);
        }
        let has_community_receiver = receivers
            .iter()
            .any(|(receiver, _)| matches!(receiver, FeeReceiver::Community(_)));
        let is_launch_pool = matches!(
            pool_type,
            PoolType::LaunchV1 { .. } | PoolType::LaunchLatest { .. }
        );
        if has_community_receiver && !is_launch_pool {
            return Err(FeeConfigurationError::CommunityReceiverOutsideLaunchPool);
        }
        Ok(())
    }

    pub fn with_protocol_fee(
        &self,
        asset_account_ids: &[&AccountIdRef],
        timestamp: Timestamp,
    ) -> CurrentFees {
        match self {
            FeeConfiguration::V1(fees) => {
                let mut receivers = fees.receivers.clone();
                let mut protocol_fee = PROTOCOL_FEE;
                if receivers.is_empty() {
                    protocol_fee = 0;
                } else if should_reduce_fee(asset_account_ids) {
                    protocol_fee = PROTOCOL_FEE_REDUCED;
                }
                receivers.push((
                    FeeReceiver::Account(PROTOCOL_FEE_RECEIVER_ID.to_owned()),
                    protocol_fee,
                ));
                CurrentFees { receivers }
            }
            FeeConfiguration::V2(fees) => {
                let mut receivers = Vec::new();
                let mut protocol_fee = if fees.receivers.is_empty() {
                    0
                } else if should_reduce_fee(asset_account_ids) {
                    PROTOCOL_FEE_REDUCED
                } else {
                    PROTOCOL_FEE
                };
                for (receiver, amount) in fees.receivers.iter() {
                    match amount {
                        FeeAmount::Fixed(fee_fraction) => {
                            receivers.push((receiver.clone(), *fee_fraction));
                        }
                        FeeAmount::Scheduled {
                            end: (end_time, _), ..
                        } => {
                            let mut fraction = amount.fee_fraction_at(timestamp);
                            if timestamp < *end_time {
                                const PROTOCOL_SHARE_PERCENT: u32 = 5;
                                let protocol_share = fraction
                                    .checked_mul(PROTOCOL_SHARE_PERCENT)
                                    .unwrap()
                                    .checked_div(100)
                                    .unwrap();
                                fraction = fraction.checked_sub(protocol_share).unwrap();
                                protocol_fee = protocol_fee.checked_add(protocol_share).unwrap();
                            }
                            receivers.push((receiver.clone(), fraction));
                        }
                        FeeAmount::Dynamic { .. } => {
                            unimplemented!("Dynamic fee configuration is not implemented yet");
                        }
                    }
                }
                receivers.push((
                    FeeReceiver::Account(PROTOCOL_FEE_RECEIVER_ID.to_owned()),
                    protocol_fee,
                ));
                CurrentFees { receivers }
            }
        }
    }
}

impl ReferralSettings {
    pub fn validate(&self) -> Result<(), ReferralSettingsError> {
        match self {
            ReferralSettings::V1 {
                fee_fraction,
                fee_fraction_reduced,
            } => {
                if *fee_fraction >= MAX_REFERRAL_FEE_FRACTION {
                    return Err(ReferralSettingsError::FeeFractionTooHigh);
                }
                if *fee_fraction_reduced >= MAX_REFERRAL_FEE_FRACTION {
                    return Err(ReferralSettingsError::FeeFractionReducedTooHigh);
                }
            }
        }
        Ok(())
    }

    pub fn fee_fraction(&self, asset_account_ids: &[&AccountIdRef]) -> FeeFraction {
        match self {
            ReferralSettings::V1 {
                fee_fraction,
                fee_fraction_reduced,
            } => {
                if should_reduce_fee(asset_account_ids) {
                    *fee_fraction_reduced
                } else {
                    *fee_fraction
                }
            }
        }
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
                write!(f, "Start fee fraction must be less than {FULL_FEE_FRACTION}")
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
    CommunityReceiverOutsideLaunchPool,
}

impl fmt::Display for FeeConfigurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFeeAmount(fee_amount_error) => write!(f, "{fee_amount_error}"),
            Self::TooManyReceivers => write!(f, "Too many fee receivers"),
            Self::ReceiverFeeTooHigh => {
                write!(f, "Fee must be less than {FULL_FEE_FRACTION} per receiver")
            }
            Self::TotalFeeTooHigh => write!(
                f,
                "Fees must add up to less than 50% ({MAX_TOTAL_FEE_FRACTION})"
            ),
            Self::ProtocolFeeReceiverSet => {
                write!(f, "Protocol fee receiver can't be set by users")
            }
            Self::CommunityReceiverOutsideLaunchPool => {
                write!(
                    f,
                    "Community fee receiver is only supported in Launch pools"
                )
            }
        }
    }
}

impl std::error::Error for FeeConfigurationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferralSettingsError {
    FeeFractionTooHigh,
    FeeFractionReducedTooHigh,
}

impl fmt::Display for ReferralSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FeeFractionTooHigh => write!(
                f,
                "Fee fraction must be less than {MAX_REFERRAL_FEE_FRACTION}"
            ),
            Self::FeeFractionReducedTooHigh => write!(
                f,
                "Fee fraction reduced must be less than {MAX_REFERRAL_FEE_FRACTION}"
            ),
        }
    }
}

impl std::error::Error for ReferralSettingsError {}
