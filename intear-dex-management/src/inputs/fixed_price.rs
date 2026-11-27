use std::str::FromStr;

use amp_dex_types::Price;

/// The fixed price of a whole token of an amp pool's first asset in its second
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedPriceArg(pub Price);

impl std::fmt::Display for FixedPriceArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for FixedPriceArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let price = Price::from_str(s).map_err(|error| {
            format!("Invalid price: {error}. Expected a price of a whole token of the first asset in the second, like 1")
        })?;
        if price.is_zero() {
            return Err("The fixed price must be greater than 0".to_string());
        }
        Ok(Self(price))
    }
}

impl interactive_clap::ToCli for FixedPriceArg {
    type CliVariant = FixedPriceArg;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_price_is_more_than_0() {
        assert_eq!("1".parse::<FixedPriceArg>(), Ok(FixedPriceArg(Price::ONE)));
        assert_eq!(
            "0.999000999000999001".parse::<FixedPriceArg>(),
            Ok(FixedPriceArg(Price::from_units(999_000_999_000_999_001)))
        );
        assert_eq!(
            "0".parse::<FixedPriceArg>(),
            Err("The fixed price must be greater than 0".to_string())
        );
        assert!("abc".parse::<FixedPriceArg>().is_err());
    }
}
