use std::str::FromStr;

use intear_dex_types::AssetId;

use super::amount::AmountArg;
use super::asset_ids::AssetIdArg;

const ASSET_AMOUNTS_FORMAT: &str = "<ASSET_ID>=<AMOUNT>, comma-separated, e.g. 'near=1 NEAR,nep141:usdt.tether-token.near=25 USDT'";

/// Amounts of several assets, each with its asset id
#[derive(Debug, Clone)]
pub struct AssetAmountsArg(pub Vec<(AssetId, AmountArg)>);

impl std::fmt::Display for AssetAmountsArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let asset_amounts = self
            .0
            .iter()
            .map(|(asset_id, amount)| format!("{asset_id}={amount}"))
            .collect::<Vec<_>>();
        write!(f, "{}", asset_amounts.join(","))
    }
}

impl FromStr for AssetAmountsArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut asset_amounts: Vec<(AssetId, AmountArg)> = Vec::new();
        for asset_amount in s.split(',') {
            let (asset_id, amount) = asset_amount.split_once('=').ok_or_else(|| {
                format!("Invalid asset amount '{asset_amount}'. Expected {ASSET_AMOUNTS_FORMAT}")
            })?;
            let AssetIdArg(asset_id) = AssetIdArg::from_str(asset_id.trim())?;
            if asset_amounts
                .iter()
                .any(|(listed_asset_id, _)| *listed_asset_id == asset_id)
            {
                return Err(format!("{asset_id} is listed twice"));
            }
            asset_amounts.push((asset_id, AmountArg::from_str(amount.trim())?));
        }
        Ok(Self(asset_amounts))
    }
}

impl interactive_clap::ToCli for AssetAmountsArg {
    type CliVariant = AssetAmountsArg;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_amounts_pair_ids_with_amounts() {
        let AssetAmountsArg(asset_amounts) = "near=1 NEAR, nep141:usdt.tether-token.near=25.5 USDt"
            .parse()
            .unwrap();
        assert_eq!(asset_amounts.len(), 2);
        assert_eq!(asset_amounts[0].0, AssetId::Near);
        assert_eq!(asset_amounts[1].1.to_string(), "25.5 USDt");
        assert!(
            "near=1 NEAR,near=2 NEAR"
                .parse::<AssetAmountsArg>()
                .is_err()
        );
        assert!("near 1 NEAR".parse::<AssetAmountsArg>().is_err());
    }
}
