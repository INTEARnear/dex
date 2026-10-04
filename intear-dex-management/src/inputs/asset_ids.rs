use std::str::FromStr;

use intear_dex_types::AssetId;

const ASSET_ID_FORMAT: &str =
    "near, nep141:<contract>, nep171:<contract>:<token> or nep245:<contract>:<token>";

#[derive(Debug, Clone)]
pub struct AssetIdArg(pub AssetId);

impl std::fmt::Display for AssetIdArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for AssetIdArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AssetId::from_str(s)
            .map(Self)
            .map_err(|error| format!("{error}. Expected {ASSET_ID_FORMAT}"))
    }
}

impl interactive_clap::ToCli for AssetIdArg {
    type CliVariant = AssetIdArg;
}

#[derive(Debug, Clone)]
pub struct AssetIdListArg(pub Vec<AssetId>);

impl std::fmt::Display for AssetIdListArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let asset_ids = self.0.iter().map(AssetId::to_string).collect::<Vec<_>>();
        write!(f, "{}", asset_ids.join(","))
    }
}

impl FromStr for AssetIdListArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().is_empty() {
            return Err(format!(
                "Expected comma-separated asset ids: {ASSET_ID_FORMAT}"
            ));
        }
        let mut asset_ids = Vec::new();
        for asset_id in s.split(',') {
            let AssetIdArg(asset_id) = AssetIdArg::from_str(asset_id.trim())?;
            if asset_ids.contains(&asset_id) {
                return Err(format!("{asset_id} is listed twice"));
            }
            asset_ids.push(asset_id);
        }
        Ok(Self(asset_ids))
    }
}

impl interactive_clap::ToCli for AssetIdListArg {
    type CliVariant = AssetIdListArg;
}

#[derive(Debug, Clone)]
pub enum AssetSelectionArg {
    All,
    Listed(Vec<AssetId>),
}

impl std::fmt::Display for AssetSelectionArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::Listed(asset_ids) => write!(f, "{}", AssetIdListArg(asset_ids.clone())),
        }
    }
}

impl FromStr for AssetSelectionArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "all" {
            Ok(Self::All)
        } else {
            AssetIdListArg::from_str(s).map(|AssetIdListArg(asset_ids)| Self::Listed(asset_ids))
        }
    }
}

impl interactive_clap::ToCli for AssetSelectionArg {
    type CliVariant = AssetSelectionArg;
}
