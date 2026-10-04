use color_eyre::eyre::eyre;
use intear_dex_types::AssetId;
use prettytable::Table;
use serde_json::json;
use xyk_dex_types::FeeFraction;

use crate::chain::asset_metadata::AssetMetadata;

const FEE_FRACTION_PERCENT_DECIMALS: u8 = 4;

#[derive(Debug, Clone, Copy)]
pub enum OutputFormat {
    Table,
    Json,
}

/// `raw` in units of 10^-`decimals`, without rounding, e.g. 1204500 with 3
/// decimals is "1204.5".
pub fn decimal_amount(raw: u128, decimals: u8) -> String {
    let decimals = usize::from(decimals);
    let digits = format!("{raw:0>width$}", width = decimals.saturating_add(1));
    let (integer_digits, fraction_digits) = digits.split_at(digits.len().saturating_sub(decimals));
    let fraction_digits = fraction_digits.trim_end_matches('0');
    if fraction_digits.is_empty() {
        integer_digits.to_string()
    } else {
        format!("{integer_digits}.{fraction_digits}")
    }
}

pub fn asset_label(asset_id: &AssetId, metadata: Option<&AssetMetadata>) -> String {
    match metadata {
        Some(metadata) => metadata.symbol.clone(),
        None => asset_id.to_string(),
    }
}

pub fn format_amount(raw: u128, metadata: Option<&AssetMetadata>) -> String {
    let Some(metadata) = metadata else {
        return format!("{raw} raw");
    };
    let decimal = decimal_amount(raw, metadata.decimals);
    let (integer_digits, fraction_digits) = match decimal.split_once('.') {
        Some((integer_digits, fraction_digits)) => (integer_digits, Some(fraction_digits)),
        None => (decimal.as_str(), None),
    };
    let grouped_integer_digits = integer_digits
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>()
        .join(",");
    match fraction_digits {
        Some(fraction_digits) => format!(
            "{grouped_integer_digits}.{fraction_digits} {}",
            metadata.symbol
        ),
        None => format!("{grouped_integer_digits} {}", metadata.symbol),
    }
}

pub fn amount_json(raw: u128, metadata: Option<&AssetMetadata>) -> serde_json::Value {
    json!({
        "raw": raw.to_string(),
        "decimal": metadata.map(|metadata| decimal_amount(raw, metadata.decimals)),
        "symbol": metadata.map(|metadata| metadata.symbol.clone()),
    })
}

pub fn format_fee(fee_fraction: FeeFraction) -> String {
    format!(
        "{}%",
        decimal_amount(u128::from(fee_fraction), FEE_FRACTION_PERCENT_DECIMALS)
    )
}

/// `part` as a percentage of `whole`, rounded down to 0.0001%, like fees
pub fn format_share(part: u128, whole: u128) -> color_eyre::eyre::Result<String> {
    const MILLIONTHS_PER_WHOLE: u128 = 1_000_000;
    let millionths = part
        .checked_mul(MILLIONTHS_PER_WHOLE)
        .and_then(|scaled_part| scaled_part.checked_div(whole))
        .ok_or_else(|| eyre!("{part} of {whole} can't be shown as a percentage"))?;
    Ok(format!(
        "{}%",
        decimal_amount(millionths, FEE_FRACTION_PERCENT_DECIMALS)
    ))
}

pub fn fee_json(fee_fraction: FeeFraction) -> serde_json::Value {
    json!({
        "fraction": fee_fraction,
        "percent": decimal_amount(u128::from(fee_fraction), FEE_FRACTION_PERCENT_DECIMALS),
    })
}

pub fn format_timestamp(timestamp_nanoseconds: u64) -> String {
    i64::try_from(timestamp_nanoseconds)
        .map(chrono::DateTime::from_timestamp_nanos)
        .map(|date_time| date_time.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
        .unwrap_or_else(|_| format!("{timestamp_nanoseconds} ns"))
}

pub fn table(titles: &[&str]) -> Table {
    let mut table = Table::new();
    table.set_format(*prettytable::format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
    table.set_titles(titles.iter().copied().collect());
    table
}

pub fn key_value_table(rows: Vec<(&str, String)>) -> String {
    let key_width = rows
        .iter()
        .map(|(key, _)| key.len())
        .max()
        .unwrap_or_default();
    rows.iter()
        .map(|(key, value)| format!("{key:<key_width$}  {value}\n"))
        .collect()
}

pub fn print_json(value: &serde_json::Value) -> serde_json::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_amounts_are_exact() {
        assert_eq!(decimal_amount(1_204_500, 3), "1204.5");
        assert_eq!(decimal_amount(5, 6), "0.000005");
        assert_eq!(decimal_amount(0, 24), "0");
        assert_eq!(decimal_amount(42, 0), "42");
        assert_eq!(
            decimal_amount(u128::MAX, 24),
            "340282366920938.463463374607431768211455"
        );
    }

    #[test]
    fn amounts_are_grouped_and_labeled() {
        let usdt = AssetMetadata {
            symbol: "USDT".to_string(),
            decimals: 6,
        };
        assert_eq!(format_amount(3_911_200_000, Some(&usdt)), "3,911.2 USDT");
        assert_eq!(
            format_amount(1_000_000_000_000, Some(&usdt)),
            "1,000,000 USDT"
        );
        assert_eq!(format_amount(123, None), "123 raw");
    }

    #[test]
    fn fees_are_percentages_of_one_million() {
        assert_eq!(format_fee(2500), "0.25%");
        assert_eq!(format_fee(1), "0.0001%");
        assert_eq!(format_fee(3001), "0.3001%");
        assert_eq!(format_fee(0), "0%");
    }
}
