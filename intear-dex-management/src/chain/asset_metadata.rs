use std::cell::RefCell;
use std::collections::HashMap;

use color_eyre::eyre::Context;
use intear_dex_types::AssetId;
use near_cli_rs::config::NetworkConfig;
use near_primitives::types::BlockReference;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetMetadata {
    pub symbol: String,
    pub decimals: u8,
}

impl AssetMetadata {
    pub fn near() -> Self {
        Self {
            symbol: "NEAR".to_string(),
            decimals: 24,
        }
    }
}

pub struct AssetMetadataCache<'network> {
    network_config: &'network NetworkConfig,
    block_reference: BlockReference,
    metadata_by_asset: RefCell<HashMap<AssetId, Option<AssetMetadata>>>,
}

impl<'network> AssetMetadataCache<'network> {
    pub fn new(network_config: &'network NetworkConfig, block_reference: BlockReference) -> Self {
        Self {
            network_config,
            block_reference,
            metadata_by_asset: RefCell::new(HashMap::new()),
        }
    }

    /// `None` when the asset's contract doesn't provide metadata.
    pub fn get(&self, asset_id: &AssetId) -> color_eyre::eyre::Result<Option<AssetMetadata>> {
        if let Some(metadata) = self.metadata_by_asset.borrow().get(asset_id) {
            return Ok(metadata.clone());
        }
        let metadata = match asset_id {
            AssetId::Near => Some(AssetMetadata::near()),
            AssetId::Nep141(contract_id) => {
                #[derive(serde::Deserialize)]
                struct FungibleTokenMetadata {
                    symbol: String,
                    decimals: u8,
                }
                super::view_function_result(
                    self.network_config,
                    &self.block_reference,
                    contract_id,
                    "ft_metadata",
                    b"{}".to_vec(),
                )?
                .map(|result| serde_json::from_slice::<FungibleTokenMetadata>(&result))
                .transpose()
                .wrap_err_with(|| format!("Unexpected ft_metadata of {contract_id}"))?
                .map(|metadata| AssetMetadata {
                    symbol: metadata.symbol,
                    decimals: metadata.decimals,
                })
            }
            AssetId::Nep245(contract_id, token_id) => {
                #[derive(serde::Deserialize)]
                struct MultiTokenBaseMetadata {
                    symbol: Option<String>,
                    decimals: Option<String>,
                }
                super::view_function_result(
                    self.network_config,
                    &self.block_reference,
                    contract_id,
                    "mt_metadata_base_by_token_id",
                    serde_json::to_vec(&serde_json::json!({ "token_ids": [token_id] }))?,
                )?
                .map(|result| {
                    serde_json::from_slice::<Vec<Option<MultiTokenBaseMetadata>>>(&result)
                })
                .transpose()
                .wrap_err_with(|| {
                    format!("Unexpected mt_metadata_base_by_token_id of {contract_id}")
                })?
                .and_then(|metadata| metadata.into_iter().next().flatten())
                .and_then(|metadata| {
                    Some(AssetMetadata {
                        symbol: metadata.symbol?,
                        decimals: metadata.decimals?.parse().ok()?,
                    })
                })
            }
            AssetId::Nep171(_, _) => None,
        };
        self.metadata_by_asset
            .borrow_mut()
            .insert(asset_id.clone(), metadata.clone());
        Ok(metadata)
    }
}
