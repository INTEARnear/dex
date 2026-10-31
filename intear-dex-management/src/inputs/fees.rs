use std::str::FromStr;

use intear_dex_types::AssetId;
use near_primitives::types::AccountId;
use xyk_dex_types::{
    ClaimMode, FeeAmount, FeeConfigurationV2, FeePayoutType, FeeReceiver, ScheduledFeeCurve,
};

use super::percent::PercentArg;

const FEES_FORMAT: &str = "<RECEIVER>=<FEE>, comma-separated, where RECEIVER is pool, an account or community:<account>, and FEE is a percentage like 0.25% or a linear schedule like 1%..0.25%@now..now+7d; or none";
const PAYOUTS_FORMAT: &str = "<RECEIVER>=<PAYOUT>, comma-separated, where RECEIVER is an account or community:<account> of the fees, and PAYOUT is keep or the pool asset to convert its fees to, like near";
const TIME_FORMAT: &str = "now, now+<N><s|m|h|d|w> or an RFC 3339 time like 2026-01-01T00:00:00Z";

/// A point in time for a fee schedule. `now` is the time of the block the
/// transaction is planned at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeArg {
    Now {
        offset_amount: u64,
        offset_unit: char,
    },
    At(chrono::DateTime<chrono::Utc>),
}

impl TimeArg {
    pub fn nanoseconds(&self, now_nanoseconds: u64) -> Result<u64, String> {
        match self {
            Self::Now {
                offset_amount,
                offset_unit,
            } => {
                let seconds_per_unit: u64 = match offset_unit {
                    's' => 1,
                    'm' => 60,
                    'h' => 60 * 60,
                    'd' => 24 * 60 * 60,
                    'w' => 7 * 24 * 60 * 60,
                    _ => return Err(format!("Unknown time unit {offset_unit}")),
                };
                offset_amount
                    .checked_mul(seconds_per_unit)
                    .and_then(|offset_seconds| offset_seconds.checked_mul(1_000_000_000))
                    .and_then(|offset_nanoseconds| now_nanoseconds.checked_add(offset_nanoseconds))
                    .ok_or_else(|| format!("{self} is too far in the future"))
            }
            Self::At(time) => time
                .timestamp_nanos_opt()
                .and_then(|nanoseconds| u64::try_from(nanoseconds).ok())
                .ok_or_else(|| format!("{self} is outside the range of block times")),
        }
    }
}

impl std::fmt::Display for TimeArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Now {
                offset_amount: 0, ..
            } => write!(f, "now"),
            Self::Now {
                offset_amount,
                offset_unit,
            } => write!(f, "now+{offset_amount}{offset_unit}"),
            Self::At(time) => write!(
                f,
                "{}",
                time.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
            ),
        }
    }
}

impl FromStr for TimeArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "now" {
            return Ok(Self::Now {
                offset_amount: 0,
                offset_unit: 's',
            });
        }
        if let Some(offset) = s.strip_prefix("now+") {
            let offset_unit = offset
                .chars()
                .last()
                .filter(|unit| ['s', 'm', 'h', 'd', 'w'].contains(unit))
                .ok_or_else(|| format!("Invalid time '{s}'. Expected {TIME_FORMAT}"))?;
            let offset_amount = offset
                .strip_suffix(offset_unit)
                .and_then(|amount| amount.parse().ok())
                .ok_or_else(|| format!("Invalid time '{s}'. Expected {TIME_FORMAT}"))?;
            return Ok(Self::Now {
                offset_amount,
                offset_unit,
            });
        }
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|time| Self::At(time.with_timezone(&chrono::Utc)))
            .map_err(|error| format!("Invalid time '{s}': {error}. Expected {TIME_FORMAT}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeeAmountArg {
    Fixed(PercentArg),
    /// Decreases linearly from the start fee at the start time to the end fee
    /// at the end time
    Scheduled {
        start_fee: PercentArg,
        end_fee: PercentArg,
        start_time: TimeArg,
        end_time: TimeArg,
    },
}

impl std::fmt::Display for FeeAmountArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fixed(fee) => write!(f, "{fee}"),
            Self::Scheduled {
                start_fee,
                end_fee,
                start_time,
                end_time,
            } => write!(f, "{start_fee}..{end_fee}@{start_time}..{end_time}"),
        }
    }
}

/// A fee receiver as `--fees` and `--payouts` name it
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeeReceiverArg {
    /// An account that collects its fees itself
    Account(AccountId),
    /// An account whose fees anyone can claim for it
    Community(AccountId),
    Pool,
}

/// Who gets which part of every swap's input
#[derive(Clone, PartialEq)]
pub struct FeesArg(pub Vec<(FeeReceiverArg, FeeAmountArg)>);

// As the command line writes it
impl std::fmt::Debug for FeesArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

fn parse_fee_receiver(receiver: &str) -> Result<FeeReceiverArg, String> {
    match receiver {
        "pool" => Ok(FeeReceiverArg::Pool),
        _ => match receiver.strip_prefix("community:") {
            Some(account_id) => Ok(FeeReceiverArg::Community(
                AccountId::from_str(account_id)
                    .map_err(|error| format!("Invalid account id {account_id}: {error}"))?,
            )),
            None => Ok(FeeReceiverArg::Account(AccountId::from_str(receiver).map_err(
                |error| {
                    format!(
                        "Invalid fee receiver {receiver}: {error}. Expected pool, an account or community:<account>"
                    )
                },
            )?)),
        },
    }
}

fn fee_receiver_text(receiver: &FeeReceiverArg) -> String {
    match receiver {
        FeeReceiverArg::Account(account_id) => account_id.to_string(),
        FeeReceiverArg::Community(account_id) => format!("community:{account_id}"),
        FeeReceiverArg::Pool => "pool".to_string(),
    }
}

impl std::fmt::Display for FeesArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            return write!(f, "none");
        }
        let receivers = self
            .0
            .iter()
            .map(|(receiver, amount)| format!("{}={amount}", fee_receiver_text(receiver)))
            .collect::<Vec<_>>();
        write!(f, "{}", receivers.join(","))
    }
}

impl FromStr for FeesArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim() == "none" {
            return Ok(Self(Vec::new()));
        }
        let mut receivers: Vec<(FeeReceiverArg, FeeAmountArg)> = Vec::new();
        for receiver_fee in s.split(',') {
            let (receiver, amount) = receiver_fee
                .trim()
                .split_once('=')
                .ok_or_else(|| format!("Invalid fee '{receiver_fee}'. Expected {FEES_FORMAT}"))?;
            let receiver = parse_fee_receiver(receiver)?;
            if receivers
                .iter()
                .any(|(listed_receiver, _)| *listed_receiver == receiver)
            {
                return Err(format!("{} is listed twice", fee_receiver_text(&receiver)));
            }
            let amount = match amount.split_once('@') {
                None => FeeAmountArg::Fixed(amount.parse()?),
                Some((fees, times)) => {
                    let invalid_schedule = || {
                        format!(
                            "Invalid fee schedule '{amount}'. Expected <START_FEE>..<END_FEE>@<START_TIME>..<END_TIME>, e.g. 1%..0.25%@now..now+7d"
                        )
                    };
                    let (start_fee, end_fee) =
                        fees.split_once("..").ok_or_else(invalid_schedule)?;
                    let (start_time, end_time) =
                        times.split_once("..").ok_or_else(invalid_schedule)?;
                    FeeAmountArg::Scheduled {
                        start_fee: start_fee.parse()?,
                        end_fee: end_fee.parse()?,
                        start_time: start_time.parse()?,
                        end_time: end_time.parse()?,
                    }
                }
            };
            receivers.push((receiver, amount));
        }
        Ok(Self(receivers))
    }
}

impl interactive_clap::ToCli for FeesArg {
    type CliVariant = FeesArg;
}

/// How the account and community receivers of fees take them
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayoutArg {
    Keep,
    ConvertTo(AssetId),
}

impl std::fmt::Display for PayoutArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep => write!(f, "keep"),
            Self::ConvertTo(asset_id) => write!(f, "{asset_id}"),
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct PayoutsArg(pub Vec<(FeeReceiverArg, PayoutArg)>);

// As the command line writes it
impl std::fmt::Debug for PayoutsArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

impl std::fmt::Display for PayoutsArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let payouts = self
            .0
            .iter()
            .map(|(receiver, payout)| format!("{}={payout}", fee_receiver_text(receiver)))
            .collect::<Vec<_>>();
        write!(f, "{}", payouts.join(","))
    }
}

impl FromStr for PayoutsArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut payouts: Vec<(FeeReceiverArg, PayoutArg)> = Vec::new();
        for receiver_payout in s.split(',') {
            let (receiver, payout_type) =
                receiver_payout.trim().split_once('=').ok_or_else(|| {
                    format!("Invalid payout '{receiver_payout}'. Expected {PAYOUTS_FORMAT}")
                })?;
            let receiver = parse_fee_receiver(receiver)?;
            if receiver == FeeReceiverArg::Pool {
                return Err(
                    "The pool's liquidity providers choose how they take its fees".to_string(),
                );
            }
            if payouts
                .iter()
                .any(|(listed_receiver, _)| *listed_receiver == receiver)
            {
                return Err(format!("{} is listed twice", fee_receiver_text(&receiver)));
            }
            let payout_type = match payout_type {
                "keep" => PayoutArg::Keep,
                _ => PayoutArg::ConvertTo(
                    AssetId::from_str(payout_type)
                        .map_err(|error| format!("{error}. Expected {PAYOUTS_FORMAT}"))?,
                ),
            };
            payouts.push((receiver, payout_type));
        }
        Ok(Self(payouts))
    }
}

impl interactive_clap::ToCli for PayoutsArg {
    type CliVariant = PayoutsArg;
}

impl FeesArg {
    /// Every account and community receiver becomes an account with a payout_type:
    /// the one in `payouts`, or `default_payout_type`. Anyone can claim the fees of
    /// a community, in any pool and asset.
    pub fn fee_configuration(
        &self,
        now_nanoseconds: u64,
        payouts: Option<&PayoutsArg>,
        pool_assets: [&AssetId; 2],
        default_payout_type: FeePayoutType,
    ) -> Result<FeeConfigurationV2, String> {
        let payouts = payouts.map_or(&[][..], |PayoutsArg(payouts)| payouts.as_slice());
        if let Some((receiver, _)) = payouts.iter().find(|(receiver, _)| {
            !self
                .0
                .iter()
                .any(|(fee_receiver, _)| fee_receiver == receiver)
        }) {
            return Err(format!(
                "{} has a payout but no fee",
                fee_receiver_text(receiver)
            ));
        }
        let mut receivers = Vec::new();
        for (receiver, amount) in &self.0 {
            let payout_type = match payouts
                .iter()
                .find(|(payout_receiver, _)| payout_receiver == receiver)
            {
                None => default_payout_type,
                Some((_, PayoutArg::Keep)) => FeePayoutType::KeepBoth,
                Some((_, PayoutArg::ConvertTo(asset_id))) if asset_id == pool_assets[0] => {
                    FeePayoutType::ConvertToFirstAsset
                }
                Some((_, PayoutArg::ConvertTo(asset_id))) if asset_id == pool_assets[1] => {
                    FeePayoutType::ConvertToSecondAsset
                }
                Some((_, PayoutArg::ConvertTo(asset_id))) => {
                    return Err(format!(
                        "{} can't convert its fees to {asset_id}, which the pool doesn't hold",
                        fee_receiver_text(receiver)
                    ));
                }
            };
            let receiver = match receiver {
                FeeReceiverArg::Account(account_id) => FeeReceiver::Account {
                    account_id: account_id.clone(),
                    payout_type,
                    claim_mode: ClaimMode::OnlyOwner,
                },
                FeeReceiverArg::Community(account_id) => FeeReceiver::Account {
                    account_id: account_id.clone(),
                    payout_type,
                    claim_mode: ClaimMode::Anyone,
                },
                FeeReceiverArg::Pool => FeeReceiver::Pool,
            };
            let fee_amount = match amount {
                FeeAmountArg::Fixed(PercentArg(fee)) => FeeAmount::Fixed(*fee),
                FeeAmountArg::Scheduled {
                    start_fee: PercentArg(start_fee),
                    end_fee: PercentArg(end_fee),
                    start_time,
                    end_time,
                } => FeeAmount::Scheduled {
                    start: (start_time.nanoseconds(now_nanoseconds)?, *start_fee),
                    end: (end_time.nanoseconds(now_nanoseconds)?, *end_fee),
                    curve: ScheduledFeeCurve::Linear,
                },
            };
            receivers.push((receiver, fee_amount));
        }
        Ok(FeeConfigurationV2 { receivers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receivers_without_payouts_take_the_default() {
        let fees: FeesArg = "alice.near=0.25%,pool=0.05%".parse().unwrap();
        assert_eq!(fees.to_string(), "alice.near=0.25%,pool=0.05%");
        let token = AssetId::Nep141("token.near".parse().unwrap());
        let FeeConfigurationV2 { receivers } = fees
            .fee_configuration(0, None, [&token, &AssetId::Near], FeePayoutType::KeepBoth)
            .unwrap();
        assert_eq!(
            receivers,
            vec![
                (
                    FeeReceiver::Account {
                        account_id: "alice.near".parse().unwrap(),
                        payout_type: FeePayoutType::KeepBoth,
                        claim_mode: ClaimMode::OnlyOwner,
                    },
                    FeeAmount::Fixed(2_500)
                ),
                (FeeReceiver::Pool, FeeAmount::Fixed(500))
            ]
        );
        assert_eq!("none".parse::<FeesArg>().unwrap().0, Vec::new());
    }

    #[test]
    fn schedules_resolve_times_against_now() {
        let fees: FeesArg = "community:dao.near=1%..0.3%@now..now+7d,pool=0.1%"
            .parse()
            .unwrap();
        assert_eq!(
            fees.to_string(),
            "community:dao.near=1%..0.3%@now..now+7d,pool=0.1%"
        );
        let token = AssetId::Nep141("token.near".parse().unwrap());
        let FeeConfigurationV2 { receivers } = fees
            .fee_configuration(
                1_000,
                None,
                [&token, &AssetId::Near],
                FeePayoutType::KeepBoth,
            )
            .unwrap();
        let FeeAmount::Scheduled { start, end, .. } = receivers[0].1 else {
            panic!("the first fee should be scheduled");
        };
        assert_eq!(start, (1_000, 10_000));
        assert_eq!(end, (1_000 + 7 * 24 * 60 * 60 * 1_000_000_000, 3_000));
        let absolute: FeesArg = "alice.near=2%..1%@2025-01-01T00:00:00Z..2025-07-01T00:00:00Z"
            .parse()
            .unwrap();
        assert_eq!(
            absolute.to_string(),
            "alice.near=2%..1%@2025-01-01T00:00:00Z..2025-07-01T00:00:00Z"
        );
    }

    #[test]
    fn payouts_choose_the_asset_of_receivers() {
        let fees: FeesArg = "alice.near=0.25%,community:dao.near=1%,pool=0.05%"
            .parse()
            .unwrap();
        let payouts: PayoutsArg = "community:dao.near=near".parse().unwrap();
        assert_eq!(payouts.to_string(), "community:dao.near=near");
        let token = AssetId::Nep141("token.near".parse().unwrap());
        let FeeConfigurationV2 { receivers } = fees
            .fee_configuration(
                0,
                Some(&payouts),
                [&token, &AssetId::Near],
                FeePayoutType::KeepBoth,
            )
            .unwrap();
        assert_eq!(
            receivers,
            vec![
                (
                    FeeReceiver::Account {
                        account_id: "alice.near".parse().unwrap(),
                        payout_type: FeePayoutType::KeepBoth,
                        claim_mode: ClaimMode::OnlyOwner,
                    },
                    FeeAmount::Fixed(2_500)
                ),
                (
                    FeeReceiver::Account {
                        account_id: "dao.near".parse().unwrap(),
                        payout_type: FeePayoutType::ConvertToSecondAsset,
                        claim_mode: ClaimMode::Anyone,
                    },
                    FeeAmount::Fixed(10_000)
                ),
                (FeeReceiver::Pool, FeeAmount::Fixed(500)),
            ]
        );
        let usdt = AssetId::Nep141("usdt.tether-token.near".parse().unwrap());
        let foreign_payout_type: PayoutsArg = format!("alice.near={usdt}").parse().unwrap();
        assert!(
            fees.fee_configuration(
                0,
                Some(&foreign_payout_type),
                [&token, &AssetId::Near],
                FeePayoutType::KeepBoth
            )
            .is_err()
        );
        assert!("pool=keep".parse::<PayoutsArg>().is_err());
    }

    #[test]
    fn invalid_fees_say_what_is_wrong() {
        assert!("alice.near=1%,alice.near=2%".parse::<FeesArg>().is_err());
        assert!("alice.near".parse::<FeesArg>().is_err());
        assert!("pool=1%..0.5%@now".parse::<FeesArg>().is_err());
        assert!("pool=1%..0.5%@now..tomorrow".parse::<FeesArg>().is_err());
    }
}
