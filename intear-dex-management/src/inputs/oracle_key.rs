use std::str::FromStr;

use near_sdk::PublicKeyHandle;

/// The key of an oracle on the engine, which `create_oracle_key` added
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleKeyArg(pub PublicKeyHandle);

impl std::fmt::Display for OracleKeyArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for OracleKeyArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PublicKeyHandle::from_str(s)
            .map(Self)
            .map_err(|error| format!("Invalid oracle key '{s}': {error}. Expected e.g. ed25519:…"))
    }
}

impl interactive_clap::ToCli for OracleKeyArg {
    type CliVariant = OracleKeyArg;
}
