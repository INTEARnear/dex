use std::str::FromStr;

use amp_dex_types::{NewPoolRateSource, Price};
use near_sdk::PublicKeyHandle;

const RATE_SOURCE_FORMAT: &str = "an oracle key like ed25519:…, or a fixed price of a whole token of the first asset in the second, like 1";

/// What rates the assets of a new amp pool: an oracle on the engine, or a
/// fixed price
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateSourceArg(pub NewPoolRateSource);

impl std::fmt::Display for RateSourceArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            NewPoolRateSource::Oracle(oracle_key) => write!(f, "{oracle_key}"),
            NewPoolRateSource::Fixed(price) => write!(f, "{price}"),
        }
    }
}

impl FromStr for RateSourceArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.contains(':') {
            return PublicKeyHandle::from_str(s)
                .map(|oracle_key| Self(NewPoolRateSource::Oracle(oracle_key)))
                .map_err(|error| {
                    format!("Invalid oracle key '{s}': {error}. Expected {RATE_SOURCE_FORMAT}")
                });
        }
        let price = Price::from_str(s).map_err(|error| {
            format!("Invalid rate source: {error}. Expected {RATE_SOURCE_FORMAT}")
        })?;
        if price.is_zero() {
            return Err("The fixed price must be greater than 0".to_string());
        }
        Ok(Self(NewPoolRateSource::Fixed(price)))
    }
}

impl interactive_clap::ToCli for RateSourceArg {
    type CliVariant = RateSourceArg;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_oracle_keys_and_fixed_prices() {
        let oracle_key = "ed25519:6E8sCci9badyRkXb3JoRpBj5p8C6Tw41ELDZoiihKEtp";
        assert_eq!(
            oracle_key.parse::<RateSourceArg>().unwrap(),
            RateSourceArg(NewPoolRateSource::Oracle(oracle_key.parse().unwrap()))
        );
        assert_eq!(
            "1".parse::<RateSourceArg>().unwrap(),
            RateSourceArg(NewPoolRateSource::Fixed(Price::ONE))
        );
        assert_eq!("0.5".parse::<RateSourceArg>().unwrap().to_string(), "0.5");
        assert!("0".parse::<RateSourceArg>().is_err());
        assert!("ed25519:nope".parse::<RateSourceArg>().is_err());
        assert!("abc".parse::<RateSourceArg>().is_err());
    }
}
