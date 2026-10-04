use std::str::FromStr;

use near_primitives::types::AccountId;
use xyk_dex_types::{
    CurrentFees, FeeAmount, FeeConfiguration, FeeReceiver, ScheduledFeeCurve, V2FeeConfiguration,
};

use super::percent::PercentArg;

const FEES_FORMAT: &str = "<RECEIVER>=<FEE>, comma-separated, where RECEIVER is pool, an account or community:<account>, and FEE is a percentage like 0.25% or a linear schedule like 1%..0.25%@now..now+7d; or none";
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

/// Who gets which part of every swap's input
#[derive(Clone, PartialEq)]
pub struct FeesArg(pub Vec<(FeeReceiver, FeeAmountArg)>);

// FeeReceiver is Debug only in debug builds
impl std::fmt::Debug for FeesArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

fn fee_receiver_text(receiver: &FeeReceiver) -> String {
    match receiver {
        FeeReceiver::Account(account_id) => account_id.to_string(),
        FeeReceiver::Pool => "pool".to_string(),
        FeeReceiver::Community(account_id) => format!("community:{account_id}"),
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
        let mut receivers: Vec<(FeeReceiver, FeeAmountArg)> = Vec::new();
        for receiver_fee in s.split(',') {
            let (receiver, amount) = receiver_fee
                .trim()
                .split_once('=')
                .ok_or_else(|| format!("Invalid fee '{receiver_fee}'. Expected {FEES_FORMAT}"))?;
            let receiver = match receiver {
                "pool" => FeeReceiver::Pool,
                _ => match receiver.strip_prefix("community:") {
                    Some(account_id) => FeeReceiver::Community(
                        AccountId::from_str(account_id)
                            .map_err(|error| format!("Invalid account id {account_id}: {error}"))?,
                    ),
                    None => FeeReceiver::Account(
                        AccountId::from_str(receiver)
                            .map_err(|error| format!("Invalid fee receiver {receiver}: {error}. Expected pool, an account or community:<account>"))?,
                    ),
                },
            };
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

impl FeesArg {
    /// Fixed fees only make the first version of fee configurations, which
    /// pools of every version take; a schedule needs the second
    pub fn fee_configuration(&self, now_nanoseconds: u64) -> Result<FeeConfiguration, String> {
        let is_every_fee_fixed = self
            .0
            .iter()
            .all(|(_, amount)| matches!(amount, FeeAmountArg::Fixed(_)));
        if is_every_fee_fixed {
            return Ok(FeeConfiguration::V1(CurrentFees {
                receivers: self
                    .0
                    .iter()
                    .filter_map(|(receiver, amount)| match amount {
                        FeeAmountArg::Fixed(PercentArg(fee)) => Some((receiver.clone(), *fee)),
                        FeeAmountArg::Scheduled { .. } => None,
                    })
                    .collect(),
            }));
        }
        let mut receivers = Vec::new();
        for (receiver, amount) in &self.0 {
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
            receivers.push((receiver.clone(), fee_amount));
        }
        Ok(FeeConfiguration::V2(V2FeeConfiguration { receivers }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_fees_make_the_first_configuration_version() {
        let fees: FeesArg = "alice.near=0.25%,pool=0.05%".parse().unwrap();
        assert_eq!(fees.to_string(), "alice.near=0.25%,pool=0.05%");
        let FeeConfiguration::V1(CurrentFees { receivers }) = fees.fee_configuration(0).unwrap()
        else {
            panic!("fixed fees should be V1");
        };
        assert_eq!(
            receivers,
            vec![
                (FeeReceiver::Account("alice.near".parse().unwrap()), 2_500),
                (FeeReceiver::Pool, 500)
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
        let FeeConfiguration::V2(V2FeeConfiguration { receivers }) =
            fees.fee_configuration(1_000).unwrap()
        else {
            panic!("a schedule should be V2");
        };
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
    fn invalid_fees_say_what_is_wrong() {
        assert!("alice.near=1%,alice.near=2%".parse::<FeesArg>().is_err());
        assert!("alice.near".parse::<FeesArg>().is_err());
        assert!("pool=1%..0.5%@now".parse::<FeesArg>().is_err());
        assert!("pool=1%..0.5%@now..tomorrow".parse::<FeesArg>().is_err());
    }
}
