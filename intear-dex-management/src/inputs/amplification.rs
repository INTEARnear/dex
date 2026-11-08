use std::str::FromStr;

use amp_dex_types::{Amplification, MAX_AMPLIFICATION, MIN_AMPLIFICATION};

/// The amplification of an amp pool, in the range that the contract accepts
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmplificationArg(pub Amplification);

impl std::fmt::Display for AmplificationArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for AmplificationArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let out_of_range =
            || format!("Amplification must be from {MIN_AMPLIFICATION} to {MAX_AMPLIFICATION}");
        let amplification = s
            .parse::<u32>()
            .map(Amplification::new)
            .map_err(|_| format!("Invalid amplification '{s}'. {}, e.g. 100", out_of_range()))?;
        if !(MIN_AMPLIFICATION..=MAX_AMPLIFICATION).contains(&amplification) {
            return Err(out_of_range());
        }
        Ok(Self(amplification))
    }
}

impl interactive_clap::ToCli for AmplificationArg {
    type CliVariant = AmplificationArg;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amplification_is_in_the_contract_range() {
        assert_eq!(
            "100".parse::<AmplificationArg>(),
            Ok(AmplificationArg(Amplification::new(100)))
        );
        assert_eq!(
            "1000000".parse::<AmplificationArg>(),
            Ok(AmplificationArg(MAX_AMPLIFICATION))
        );
        assert_eq!(
            "0".parse::<AmplificationArg>(),
            Err("Amplification must be from 1 to 1000000".to_string())
        );
        assert_eq!(
            "1000001".parse::<AmplificationArg>(),
            Err("Amplification must be from 1 to 1000000".to_string())
        );
        assert_eq!(
            "a lot".parse::<AmplificationArg>(),
            Err(
                "Invalid amplification 'a lot'. Amplification must be from 1 to 1000000, e.g. 100"
                    .to_string()
            )
        );
    }
}
