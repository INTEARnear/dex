use std::str::FromStr;

/// Where the assets a transaction receives go
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FundsDestinationArg {
    ToWallet,
    ToBalance,
}

impl std::fmt::Display for FundsDestinationArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToWallet => write!(f, "to-wallet"),
            Self::ToBalance => write!(f, "to-balance"),
        }
    }
}

impl FromStr for FundsDestinationArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "to-wallet" => Ok(Self::ToWallet),
            "to-balance" => Ok(Self::ToBalance),
            _ => Err(format!(
                "Invalid destination '{s}'. Expected to-wallet or to-balance (on dex.intear.near)"
            )),
        }
    }
}

impl interactive_clap::ToCli for FundsDestinationArg {
    type CliVariant = FundsDestinationArg;
}
