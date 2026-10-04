use std::str::FromStr;

use intear_dex_types::DexId;

#[derive(Clone)]
pub struct DexIdArg(pub DexId);

impl std::fmt::Display for DexIdArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Debug for DexIdArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for DexIdArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DexId::from_str(s).map(Self).map_err(|error| {
            format!("{error}. Expected <deployer>/<name>, e.g. slimedragon.near/xyk")
        })
    }
}

impl interactive_clap::ToCli for DexIdArg {
    type CliVariant = DexIdArg;
}
