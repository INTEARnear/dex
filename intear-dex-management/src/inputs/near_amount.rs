use std::str::FromStr;

use near_sdk::NearToken;

/// A NEAR amount in near-cli-rs's format (1.5 NEAR, 100 yoctoNEAR), or
/// everything there is
#[derive(Debug, Clone, Copy)]
pub enum NearAmountOrAllArg {
    All,
    Amount(NearToken),
}

impl std::fmt::Display for NearAmountOrAllArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::Amount(amount) => {
                write!(f, "{}", near_cli_rs::types::near_token::NearToken(*amount))
            }
        }
    }
}

impl FromStr for NearAmountOrAllArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "all" {
            return Ok(Self::All);
        }
        near_cli_rs::types::near_token::NearToken::from_str(s)
            .map(|near_cli_rs::types::near_token::NearToken(amount)| Self::Amount(amount))
            .map_err(|error| format!("{error}. Expected a NEAR amount like '0.01 NEAR', or all"))
    }
}

impl interactive_clap::ToCli for NearAmountOrAllArg {
    type CliVariant = NearAmountOrAllArg;
}
