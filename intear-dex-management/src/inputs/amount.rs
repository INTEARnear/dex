use std::str::FromStr;

use color_eyre::eyre::{bail, eyre};
use intear_dex_types::AssetId;

use crate::chain::asset_metadata::AssetMetadata;

const AMOUNT_FORMAT: &str = "<decimal> <SYMBOL|ASSET_ID> or <integer> raw [SYMBOL|ASSET_ID], e.g. '10 NEAR', '25.5 USDT', '1000000 raw nep141:usdt.tether-token.near'";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmountArg {
    pub quantity: Quantity,
    pub unit: Option<AmountUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quantity {
    Decimal {
        integer_digits: String,
        fraction_digits: String,
    },
    Raw(u128),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmountUnit {
    Symbol(String),
    Asset(AssetId),
}

impl std::fmt::Display for AmountUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Symbol(symbol) => write!(f, "{symbol}"),
            Self::Asset(asset_id) => write!(f, "{asset_id}"),
        }
    }
}

impl std::fmt::Display for AmountArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.quantity {
            Quantity::Decimal {
                integer_digits,
                fraction_digits,
            } if fraction_digits.is_empty() => write!(f, "{integer_digits}")?,
            Quantity::Decimal {
                integer_digits,
                fraction_digits,
            } => write!(f, "{integer_digits}.{fraction_digits}")?,
            Quantity::Raw(raw_amount) => write!(f, "{raw_amount} raw")?,
        }
        match &self.unit {
            Some(unit) => write!(f, " {unit}"),
            None => Ok(()),
        }
    }
}

impl FromStr for AmountArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let words = s.split_whitespace().collect::<Vec<_>>();
        let (quantity, unit) = match words.as_slice() {
            [quantity, "raw"] => (parse_raw_quantity(quantity)?, None),
            [quantity, "raw", unit] => (parse_raw_quantity(quantity)?, Some(parse_unit(unit)?)),
            [quantity, unit] => (parse_decimal_quantity(quantity)?, Some(parse_unit(unit)?)),
            [_] => return Err(format!("Missing unit in '{s}'. Expected {AMOUNT_FORMAT}")),
            _ => return Err(format!("Invalid amount '{s}'. Expected {AMOUNT_FORMAT}")),
        };
        Ok(Self { quantity, unit })
    }
}

impl interactive_clap::ToCli for AmountArg {
    type CliVariant = AmountArg;
}

/// An amount, or everything there is
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmountOrAllArg {
    All,
    Amount(AmountArg),
}

impl std::fmt::Display for AmountOrAllArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::Amount(amount) => write!(f, "{amount}"),
        }
    }
}

impl FromStr for AmountOrAllArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "all" {
            Ok(Self::All)
        } else {
            AmountArg::from_str(s)
                .map(Self::Amount)
                .map_err(|error| format!("{error}, or all"))
        }
    }
}

impl interactive_clap::ToCli for AmountOrAllArg {
    type CliVariant = AmountOrAllArg;
}

fn parse_raw_quantity(quantity: &str) -> Result<Quantity, String> {
    if !quantity.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "Invalid raw amount '{quantity}': raw amounts are whole numbers of the asset's smallest unit"
        ));
    }
    quantity
        .parse::<u128>()
        .map(Quantity::Raw)
        .map_err(|_| format!("Raw amount '{quantity}' is too large"))
}

fn parse_decimal_quantity(quantity: &str) -> Result<Quantity, String> {
    let (integer_digits, fraction_digits) = quantity.split_once('.').unwrap_or((quantity, ""));
    let is_digits =
        |digits: &str| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit());
    if !is_digits(integer_digits) || (quantity.contains('.') && !is_digits(fraction_digits)) {
        return Err(format!(
            "Invalid amount '{quantity}': expected a decimal number like 10 or 25.5"
        ));
    }
    Ok(Quantity::Decimal {
        integer_digits: integer_digits.to_string(),
        fraction_digits: fraction_digits.to_string(),
    })
}

fn parse_unit(unit: &str) -> Result<AmountUnit, String> {
    if unit.contains(':') {
        AssetId::from_str(unit)
            .map(AmountUnit::Asset)
            .map_err(|error| format!("{error} in amount unit '{unit}'"))
    } else {
        Ok(AmountUnit::Symbol(unit.to_string()))
    }
}

impl AmountArg {
    /// Picks the asset this amount is in among `candidates` and converts it
    /// to the asset's smallest unit.
    pub fn resolve(
        &self,
        candidates: &[(AssetId, Option<AssetMetadata>)],
    ) -> color_eyre::eyre::Result<(AssetId, u128)> {
        let candidate_names = candidates
            .iter()
            .map(|(asset_id, metadata)| match metadata {
                Some(metadata) => format!("{} ({asset_id})", metadata.symbol),
                None => asset_id.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let (asset_id, metadata) = match &self.unit {
            Some(AmountUnit::Asset(unit_asset_id)) => candidates
                .iter()
                .find(|(asset_id, _)| asset_id == unit_asset_id)
                .ok_or_else(|| eyre!("{unit_asset_id} isn't one of: {candidate_names}"))?,
            Some(AmountUnit::Symbol(symbol)) => {
                let matching_candidates = candidates
                    .iter()
                    .filter(|(_, metadata)| {
                        metadata.as_ref().is_some_and(|metadata| {
                            metadata.symbol.to_lowercase() == symbol.to_lowercase()
                        })
                    })
                    .collect::<Vec<_>>();
                match matching_candidates.as_slice() {
                    [matching_candidate] => *matching_candidate,
                    [] => bail!("{symbol} isn't one of: {candidate_names}"),
                    _ => bail!(
                        "Several assets have the symbol {symbol}, so write the asset id instead of the symbol: {candidate_names}"
                    ),
                }
            }
            None => match candidates {
                [single_candidate] => single_candidate,
                _ => bail!(
                    "Name the asset of the raw amount, e.g. '{self} <ASSET_ID>', one of: {candidate_names}"
                ),
            },
        };
        let amount = match &self.quantity {
            Quantity::Raw(raw_amount) => *raw_amount,
            Quantity::Decimal {
                integer_digits,
                fraction_digits,
            } => {
                let Some(metadata) = metadata else {
                    bail!(
                        "{asset_id} has no metadata, so its decimals are unknown. Write the amount in raw units, e.g. '1000000 raw {asset_id}'"
                    );
                };
                let decimals = usize::from(metadata.decimals);
                if fraction_digits.len() > decimals {
                    bail!(
                        "{} has {decimals} decimals, but '{self}' has {} digits after the dot",
                        metadata.symbol,
                        fraction_digits.len()
                    );
                }
                format!("{integer_digits}{fraction_digits:0<decimals$}")
                    .parse::<u128>()
                    .map_err(|_| eyre!("'{self}' is too large"))?
            }
        };
        Ok((asset_id.clone(), amount))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usdt_and_near() -> Vec<(AssetId, Option<AssetMetadata>)> {
        vec![
            (AssetId::Near, Some(AssetMetadata::near())),
            (
                AssetId::Nep141("usdt.tether-token.near".parse().unwrap()),
                Some(AssetMetadata {
                    symbol: "USDT".to_string(),
                    decimals: 6,
                }),
            ),
        ]
    }

    #[test]
    fn parses_and_displays_every_form() {
        for amount in [
            "10 NEAR",
            "25.5 USDT",
            "0.000001 nep141:usdt.tether-token.near",
            "1000000 raw",
            "1000000 raw USDT",
            "1000000 raw nep141:usdt.tether-token.near",
        ] {
            assert_eq!(AmountArg::from_str(amount).unwrap().to_string(), amount);
        }
    }

    #[test]
    fn rejects_ambiguous_or_malformed_amounts() {
        for amount in [
            "10",
            ".5 NEAR",
            "5. NEAR",
            "1e6 NEAR",
            "-1 NEAR",
            "1.5 raw",
            "1,000 USDT",
            "1 NEAR extra words",
        ] {
            assert!(
                AmountArg::from_str(amount).is_err(),
                "{amount} should be rejected"
            );
        }
    }

    #[test]
    fn resolves_symbol_case_insensitively_to_smallest_units() {
        let amount = AmountArg::from_str("25.5 usdt").unwrap();
        assert_eq!(
            amount.resolve(&usdt_and_near()).unwrap(),
            (
                AssetId::Nep141("usdt.tether-token.near".parse().unwrap()),
                25_500_000
            )
        );
        let amount = AmountArg::from_str("1.5 near").unwrap();
        assert_eq!(
            amount.resolve(&usdt_and_near()).unwrap(),
            (AssetId::Near, 1_500_000_000_000_000_000_000_000)
        );
    }

    #[test]
    fn rejects_more_decimals_than_the_token_has() {
        let amount = AmountArg::from_str("0.0000001 USDT").unwrap();
        assert!(amount.resolve(&usdt_and_near()).is_err());
    }

    #[test]
    fn raw_amount_needs_a_unit_when_there_are_two_candidates() {
        let amount = AmountArg::from_str("100 raw").unwrap();
        assert!(amount.resolve(&usdt_and_near()).is_err());
        let amount = AmountArg::from_str("100 raw nep141:usdt.tether-token.near").unwrap();
        assert_eq!(amount.resolve(&usdt_and_near()).unwrap().1, 100);
    }

    #[test]
    fn decimal_amount_needs_metadata() {
        let candidates = vec![(AssetId::Nep141("token.near".parse().unwrap()), None)];
        let amount = AmountArg::from_str("1 nep141:token.near").unwrap();
        assert!(amount.resolve(&candidates).is_err());
        let amount = AmountArg::from_str("1 raw").unwrap();
        assert_eq!(amount.resolve(&candidates).unwrap().1, 1);
    }

    #[test]
    fn shared_symbol_asks_for_the_asset_id() {
        let mut candidates = usdt_and_near();
        candidates[0].1 = Some(AssetMetadata {
            symbol: "USDT".to_string(),
            decimals: 24,
        });
        let amount = AmountArg::from_str("1 USDT").unwrap();
        assert!(amount.resolve(&candidates).is_err());
    }
}
