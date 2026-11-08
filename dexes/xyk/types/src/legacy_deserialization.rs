use intear_dex_common::{AssetId, SwapRequest};
use near_sdk::{
    AccountId,
    borsh::{BorshDeserialize, io::Read},
    json_types::U128,
    near,
    serde::Deserialize,
};

use crate::{
    AssetWithBalance, ClaimMode, CurrentFees, FeeAmount, FeeConfigurationV2, FeeFraction,
    FeePayoutType, FeeReceiver, PROTOCOL_FEE_RECEIVER_ID, PoolId, PoolView, ProviderShares,
    SignedFeeDiscount, SwapArgs, SwapRoute, TokenAmount, TotalShares, XykDexEvent,
};

/// The message of a swap through a single pool without a fee discount is just its pool ID, which
/// is what dex versions from before routes read, and messages from before the engine took
/// referral fees end with them
impl BorshDeserialize for SwapArgs {
    fn deserialize_reader<R: Read>(reader: &mut R) -> near_sdk::borsh::io::Result<Self> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        if let Ok(pool_id) = <[u8; 4]>::try_from(bytes.as_slice()) {
            return Ok(Self::single_pool(PoolId::new(u32::from_le_bytes(pool_id))));
        }
        let mut rest = bytes.as_slice();
        let routes = Vec::<SwapRoute>::deserialize(&mut rest)?;
        let fee_discount = Option::<SignedFeeDiscount>::deserialize(&mut rest)?;
        if !rest.is_empty() {
            // Referral fees of messages from before the engine took them, which are ignored
            <Vec<(AccountId, FeeFraction)> as BorshDeserialize>::deserialize(&mut rest)?;
        }
        if !rest.is_empty() {
            return Err(near_sdk::borsh::io::Error::new(
                near_sdk::borsh::io::ErrorKind::InvalidData,
                "Unexpected bytes after swap args",
            ));
        }
        Ok(Self {
            routes,
            fee_discount,
        })
    }
}

#[derive(BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
enum BorshFeeReceiver {
    AccountV1(AccountId),
    Pool,
    CommunityV1(AccountId),
    AccountV2 {
        account_id: AccountId,
        payout_type: FeePayoutType,
        claim_mode: ClaimMode,
    },
    Protocol,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub(crate) enum JsonFeeReceiver {
    Account(JsonAccount),
    Pool,
    Community(AccountId),
    Protocol,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde", untagged)]
pub(crate) enum JsonAccount {
    V1(AccountId),
    V2 {
        account_id: AccountId,
        payout_type: FeePayoutType,
        claim_mode: ClaimMode,
    },
}

impl TryFrom<BorshFeeReceiver> for FeeReceiver {
    type Error = &'static str;

    fn try_from(receiver: BorshFeeReceiver) -> Result<Self, Self::Error> {
        Ok(match receiver {
            BorshFeeReceiver::AccountV1(account_id) if account_id == PROTOCOL_FEE_RECEIVER_ID => {
                FeeReceiver::Protocol
            }
            BorshFeeReceiver::AccountV1(account_id) => FeeReceiver::Account {
                account_id,
                payout_type: FeePayoutType::KeepBoth,
                claim_mode: ClaimMode::OnlyOwner,
            },
            BorshFeeReceiver::CommunityV1(account_id) => FeeReceiver::Account {
                account_id,
                payout_type: FeePayoutType::KeepBoth,
                claim_mode: ClaimMode::Anyone,
            },
            BorshFeeReceiver::AccountV2 { account_id, .. }
                if account_id == PROTOCOL_FEE_RECEIVER_ID =>
            {
                return Err("The protocol fee receiver is FeeReceiver::Protocol, not an account");
            }
            BorshFeeReceiver::AccountV2 {
                account_id,
                payout_type,
                claim_mode,
            } => FeeReceiver::Account {
                account_id,
                payout_type,
                claim_mode,
            },
            BorshFeeReceiver::Pool => FeeReceiver::Pool,
            BorshFeeReceiver::Protocol => FeeReceiver::Protocol,
        })
    }
}

impl From<JsonFeeReceiver> for FeeReceiver {
    fn from(receiver: JsonFeeReceiver) -> Self {
        match receiver {
            JsonFeeReceiver::Account(JsonAccount::V1(account_id)) => {
                if account_id == PROTOCOL_FEE_RECEIVER_ID {
                    FeeReceiver::Protocol
                } else {
                    FeeReceiver::Account {
                        account_id,
                        payout_type: FeePayoutType::KeepBoth,
                        claim_mode: ClaimMode::OnlyOwner,
                    }
                }
            }
            JsonFeeReceiver::Account(JsonAccount::V2 {
                account_id,
                payout_type,
                claim_mode,
            }) => FeeReceiver::Account {
                account_id,
                payout_type,
                claim_mode,
            },
            JsonFeeReceiver::Pool => FeeReceiver::Pool,
            JsonFeeReceiver::Community(account_id) => FeeReceiver::Account {
                account_id,
                payout_type: FeePayoutType::KeepBoth,
                claim_mode: ClaimMode::Anyone,
            },
            JsonFeeReceiver::Protocol => FeeReceiver::Protocol,
        }
    }
}

impl BorshDeserialize for FeeReceiver {
    fn deserialize_reader<R: Read>(reader: &mut R) -> near_sdk::borsh::io::Result<Self> {
        BorshFeeReceiver::deserialize_reader(reader)?
            .try_into()
            .map_err(|error| {
                near_sdk::borsh::io::Error::new(near_sdk::borsh::io::ErrorKind::InvalidData, error)
            })
    }
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub(crate) struct JsonFeeConfiguration {
    receivers: Vec<(FeeReceiver, JsonFeeAmount)>,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde", untagged)]
pub(crate) enum JsonFeeAmount {
    V1(FeeFraction),
    V2(FeeAmount),
}

impl From<JsonFeeConfiguration> for FeeConfigurationV2 {
    fn from(fees: JsonFeeConfiguration) -> Self {
        FeeConfigurationV2 {
            receivers: fees
                .receivers
                .into_iter()
                .map(|(receiver, fee_amount)| match fee_amount {
                    JsonFeeAmount::V1(fee_fraction) => (receiver, FeeAmount::Fixed(fee_fraction)),
                    JsonFeeAmount::V2(fee_amount) => (receiver, fee_amount),
                })
                .collect(),
        }
    }
}

#[derive(BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
#[borsh(use_discriminant = true)]
#[repr(u8)]
enum BorshPoolView {
    PrivateV1 {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV1,
        owner_id: AccountId,
        locked: bool,
    } = 0,
    PublicV1 {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV1,
        total_shares: TotalShares,
    } = 1,
    LaunchV1 {
        near_amount: TokenAmount,
        launched_asset: AssetWithBalance,
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV1,
        phantom_liquidity_near: TokenAmount,
    } = 2,
    LaunchV2 {
        quote_asset: AssetWithBalance,
        launched_asset: AssetWithBalance,
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV1,
        phantom_liquidity: TokenAmount,
    } = 3,
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
    } = 5,
    Launch {
        quote_asset: AssetWithBalance,
        launched_asset: AssetWithBalance,
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV2,
        phantom_liquidity: TokenAmount,
    } = 6,
}

#[derive(BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
enum FeeConfigurationV1 {
    V1(CurrentFees),
    V2(FeeConfigurationV2),
}

impl From<FeeConfigurationV1> for FeeConfigurationV2 {
    fn from(fee_configuration: FeeConfigurationV1) -> Self {
        match fee_configuration {
            FeeConfigurationV1::V1(fees) => FeeConfigurationV2 {
                receivers: fees
                    .receivers
                    .into_iter()
                    .map(|(receiver, fee_fraction)| (receiver, FeeAmount::Fixed(fee_fraction)))
                    .collect(),
            },
            FeeConfigurationV1::V2(fees) => fees,
        }
    }
}

impl From<BorshPoolView> for PoolView {
    fn from(pool: BorshPoolView) -> Self {
        match pool {
            BorshPoolView::PrivateV1 {
                assets,
                fees,
                fee_configuration,
                owner_id,
                locked,
            } => PoolView::Private {
                assets,
                fees,
                fee_configuration: fee_configuration.into(),
                owner_id,
                locked,
            },
            BorshPoolView::PublicV1 {
                assets,
                fees,
                fee_configuration,
                total_shares,
            } => PoolView::Public {
                assets,
                fees,
                fee_configuration: fee_configuration.into(),
                total_shares,
            },
            BorshPoolView::LaunchV1 {
                near_amount,
                launched_asset,
                fees,
                fee_configuration,
                phantom_liquidity_near,
            } => convert_launch_pool_view(
                AssetWithBalance {
                    asset_id: AssetId::Near,
                    balance: near_amount,
                },
                launched_asset,
                fees,
                fee_configuration.into(),
                phantom_liquidity_near,
            ),
            BorshPoolView::LaunchV2 {
                quote_asset,
                launched_asset,
                fees,
                fee_configuration,
                phantom_liquidity,
            } => convert_launch_pool_view(
                quote_asset,
                launched_asset,
                fees,
                fee_configuration.into(),
                phantom_liquidity,
            ),
            BorshPoolView::Private {
                assets,
                fees,
                fee_configuration,
                owner_id,
                locked,
            } => PoolView::Private {
                assets,
                fees,
                fee_configuration,
                owner_id,
                locked,
            },
            BorshPoolView::Public {
                assets,
                fees,
                fee_configuration,
                total_shares,
            } => PoolView::Public {
                assets,
                fees,
                fee_configuration,
                total_shares,
            },
            BorshPoolView::Launch {
                quote_asset,
                launched_asset,
                fees,
                fee_configuration,
                phantom_liquidity,
            } => PoolView::Launch {
                quote_asset,
                launched_asset,
                fees,
                fee_configuration,
                phantom_liquidity,
            },
        }
    }
}

impl BorshDeserialize for PoolView {
    fn deserialize_reader<R: Read>(reader: &mut R) -> near_sdk::borsh::io::Result<Self> {
        BorshPoolView::deserialize_reader(reader).map(PoolView::from)
    }
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub(crate) enum JsonPoolView {
    Private {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV2,
        owner_id: AccountId,
        locked: bool,
    },
    Public {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV2,
        total_shares: TotalShares,
    },
    Launch(JsonLaunchPoolView),
    LaunchV2 {
        quote_asset: AssetWithBalance,
        launched_asset: AssetWithBalance,
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV2,
        phantom_liquidity: TokenAmount,
    },
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde", untagged)]
pub(crate) enum JsonLaunchPoolView {
    LaunchV1 {
        near_amount: TokenAmount,
        launched_asset: AssetWithBalance,
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV2,
        phantom_liquidity_near: TokenAmount,
    },
    LaunchV2 {
        quote_asset: AssetWithBalance,
        launched_asset: AssetWithBalance,
        fees: CurrentFees,
        fee_configuration: FeeConfigurationV2,
        phantom_liquidity: TokenAmount,
    },
}

impl From<JsonPoolView> for PoolView {
    fn from(pool: JsonPoolView) -> Self {
        match pool {
            JsonPoolView::Private {
                assets,
                fees,
                fee_configuration,
                owner_id,
                locked,
            } => PoolView::Private {
                assets,
                fees,
                fee_configuration,
                owner_id,
                locked,
            },
            JsonPoolView::Public {
                assets,
                fees,
                fee_configuration,
                total_shares,
            } => PoolView::Public {
                assets,
                fees,
                fee_configuration,
                total_shares,
            },
            JsonPoolView::Launch(JsonLaunchPoolView::LaunchV2 {
                quote_asset,
                launched_asset,
                fees,
                fee_configuration,
                phantom_liquidity,
            }) => PoolView::Launch {
                quote_asset,
                launched_asset,
                fees,
                fee_configuration,
                phantom_liquidity,
            },
            JsonPoolView::Launch(JsonLaunchPoolView::LaunchV1 {
                near_amount,
                launched_asset,
                fees,
                fee_configuration,
                phantom_liquidity_near,
            }) => convert_launch_pool_view(
                AssetWithBalance {
                    asset_id: AssetId::Near,
                    balance: near_amount,
                },
                launched_asset,
                fees,
                fee_configuration,
                phantom_liquidity_near,
            ),
            JsonPoolView::LaunchV2 {
                quote_asset,
                launched_asset,
                fees,
                fee_configuration,
                phantom_liquidity,
            } => convert_launch_pool_view(
                quote_asset,
                launched_asset,
                fees,
                fee_configuration,
                phantom_liquidity,
            ),
        }
    }
}

fn convert_launch_pool_view(
    quote_asset: AssetWithBalance,
    launched_asset: AssetWithBalance,
    mut fees: CurrentFees,
    mut fee_configuration: FeeConfigurationV2,
    phantom_liquidity: TokenAmount,
) -> PoolView {
    let receivers = fees
        .receivers
        .iter_mut()
        .map(|(receiver, _)| receiver)
        .chain(
            fee_configuration
                .receivers
                .iter_mut()
                .map(|(receiver, _)| receiver),
        );
    for receiver in receivers {
        if let FeeReceiver::Account { payout_type, .. } = receiver {
            *payout_type = FeePayoutType::ConvertToFirstAsset;
        }
    }
    PoolView::Launch {
        quote_asset,
        launched_asset,
        fees,
        fee_configuration,
        phantom_liquidity,
    }
}

#[near(event_json(standard = "xyk"))]
#[derive(Deserialize)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum XykDexEventV1 {
    #[event_version("1.0.0")]
    PoolUpdated { pool_id: PoolId, pool: PoolView },
    #[event_version("1.0.0")]
    Swap {
        pool_id: PoolId,
        request: SwapRequest,
        amount_in: TokenAmount,
        fees_breakdown: Vec<(FeeReceiver, AssetId, TokenAmount)>,
        amount_out: TokenAmount,
    },
    #[event_version("1.0.0")]
    LiquidityAdded {
        pool_id: PoolId,
        asset_0: AssetId,
        asset_1: AssetId,

        added_amount_0: TokenAmount,
        added_amount_1: TokenAmount,
        minted_shares: U128,

        new_owned_asset_0: TokenAmount,
        new_owned_asset_1: TokenAmount,
        new_owned_shares: U128,

        new_total_asset_0: TokenAmount,
        new_total_asset_1: TokenAmount,
        new_total_shares: U128,
    },
    #[event_version("1.0.0")]
    LiquidityRemoved {
        pool_id: PoolId,
        asset_0: AssetId,
        asset_1: AssetId,

        removed_amount_0: TokenAmount,
        removed_amount_1: TokenAmount,
        burned_shares: U128,

        new_owned_asset_0: TokenAmount,
        new_owned_asset_1: TokenAmount,
        new_owned_shares: U128,

        new_total_asset_0: TokenAmount,
        new_total_asset_1: TokenAmount,
        new_total_shares: U128,
    },
}

impl From<XykDexEventV1> for XykDexEvent {
    fn from(event: XykDexEventV1) -> Self {
        match event {
            XykDexEventV1::PoolUpdated { pool_id, pool } => Self::PoolUpdated { pool_id, pool },
            XykDexEventV1::Swap {
                pool_id,
                request,
                amount_in,
                fees_breakdown,
                amount_out,
            } => Self::Swap {
                pool_id,
                asset_in: request.asset_in,
                asset_out: request.asset_out,
                amount_in,
                fees_breakdown,
                amount_out,
            },
            XykDexEventV1::LiquidityAdded {
                pool_id,
                asset_0,
                asset_1,
                added_amount_0,
                added_amount_1,
                minted_shares,
                new_owned_asset_0,
                new_owned_asset_1,
                new_owned_shares,
                new_total_asset_0,
                new_total_asset_1,
                new_total_shares,
            } => Self::LiquidityAdded {
                pool_id,
                asset_0,
                asset_1,
                added_amount_0,
                added_amount_1,
                minted_shares: ProviderShares::new(minted_shares.0),
                new_owned_asset_0,
                new_owned_asset_1,
                new_owned_shares: ProviderShares::new(new_owned_shares.0),
                new_total_asset_0,
                new_total_asset_1,
                new_total_shares,
            },
            XykDexEventV1::LiquidityRemoved {
                pool_id,
                asset_0,
                asset_1,
                removed_amount_0,
                removed_amount_1,
                burned_shares,
                new_owned_asset_0,
                new_owned_asset_1,
                new_owned_shares,
                new_total_asset_0,
                new_total_asset_1,
                new_total_shares,
            } => Self::LiquidityRemoved {
                pool_id,
                asset_0,
                asset_1,
                removed_amount_0,
                removed_amount_1,
                burned_shares: ProviderShares::new(burned_shares.0),
                new_owned_asset_0,
                new_owned_asset_1,
                new_owned_shares: ProviderShares::new(new_owned_shares.0),
                new_total_asset_0,
                new_total_asset_1,
                new_total_shares,
            },
        }
    }
}
