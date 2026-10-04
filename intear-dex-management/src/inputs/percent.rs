use std::str::FromStr;

use xyk_dex_types::{FULL_FEE_FRACTION, FeeFraction};

use crate::display;

const MILLIONTHS_PER_PERCENT: FeeFraction = 10_000;
const PERCENT_DECIMALS: usize = 4;

/// A percentage with up to four decimals, in millionths like the xyk
/// contract's fee fractions: 0.3% is 3000
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PercentArg(pub FeeFraction);

impl std::fmt::Display for PercentArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", display::format_fee(self.0))
    }
}

impl FromStr for PercentArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let expected = || format!("Invalid percentage '{s}'. Expected a number and %, e.g. 0.3%");
        let number = s.strip_suffix('%').ok_or_else(expected)?;
        let (integer_digits, fraction_digits) = number.split_once('.').unwrap_or((number, ""));
        let is_digits = |digits: &str| digits.bytes().all(|byte| byte.is_ascii_digit());
        if integer_digits.is_empty()
            || !is_digits(integer_digits)
            || !is_digits(fraction_digits)
            || (number.contains('.') && fraction_digits.is_empty())
        {
            return Err(expected());
        }
        if fraction_digits.len() > PERCENT_DECIMALS {
            return Err(format!(
                "{s} is finer than 0.0001%, the smallest step of fees and percentages"
            ));
        }
        let more_than_hundred = || format!("{s} is more than 100%");
        let whole_percents: FeeFraction =
            integer_digits.parse().map_err(|_| more_than_hundred())?;
        let fraction_millionths: FeeFraction = format!("{fraction_digits:0<PERCENT_DECIMALS$}")
            .parse()
            .map_err(|_| expected())?;
        let millionths = whole_percents
            .checked_mul(MILLIONTHS_PER_PERCENT)
            .and_then(|whole_millionths| whole_millionths.checked_add(fraction_millionths))
            .filter(|millionths| *millionths <= FULL_FEE_FRACTION)
            .ok_or_else(more_than_hundred)?;
        Ok(Self(millionths))
    }
}

impl interactive_clap::ToCli for PercentArg {
    type CliVariant = PercentArg;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentages_are_millionths() {
        assert_eq!("0.3%".parse(), Ok(PercentArg(3_000)));
        assert_eq!("0.0001%".parse(), Ok(PercentArg(1)));
        assert_eq!("100%".parse(), Ok(PercentArg(1_000_000)));
        assert_eq!("2%".parse::<PercentArg>().unwrap().to_string(), "2%");
        assert!("0.00001%".parse::<PercentArg>().is_err());
        assert!("100.0001%".parse::<PercentArg>().is_err());
        assert!("0.3".parse::<PercentArg>().is_err());
        assert!("1.%".parse::<PercentArg>().is_err());
        assert!("-1%".parse::<PercentArg>().is_err());
    }
}
