use intear_dex_types::AssetId;
use near_cli_rs::config::NetworkConfig;
use near_primitives::types::{AccountId, BlockReference};
use near_sdk::json_types::U128;
use serde_json::json;

use crate::deployment::ENGINE_ACCOUNT_ID;

/// What the engine holds of an asset against what balances on it add up to.
/// What it holds beyond that is untracked, and `rescue` can send it out.
pub struct Custody {
    pub in_custody: u128,
    /// What the asset's contract says the engine holds. NEAR has none: the
    /// engine counts its untracked NEAR itself, since storage locks some.
    pub held: Option<u128>,
    pub untracked: u128,
    /// How much less than `in_custody` the engine holds
    pub deficit: u128,
}

pub fn custody_of(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    asset_id: &AssetId,
) -> color_eyre::eyre::Result<Custody> {
    let in_custody = super::engine::total_in_custody(network_config, block_reference, asset_id)?
        .map_or(0, |U128(in_custody)| in_custody);
    let engine_id = ENGINE_ACCOUNT_ID.to_owned();
    let held = match asset_id {
        AssetId::Near => {
            return Ok(Custody {
                in_custody,
                held: None,
                untracked: super::engine::untracked_near(network_config, block_reference)?
                    .as_yoctonear(),
                deficit: 0,
            });
        }
        AssetId::Nep141(token_id) => super::fungible_token::ft_balance_of(
            network_config,
            block_reference,
            token_id,
            &engine_id,
        )?,
        AssetId::Nep245(contract_id, token_id) => {
            let U128(balance) = super::view_json(
                network_config,
                block_reference,
                contract_id,
                "mt_balance_of",
                json!({ "account_id": engine_id, "token_id": token_id }),
            )?;
            balance
        }
        AssetId::Nep171(contract_id, token_id) => {
            #[derive(serde::Deserialize)]
            struct Token {
                owner_id: AccountId,
            }
            let token: Option<Token> = super::view_json(
                network_config,
                block_reference,
                contract_id,
                "nft_token",
                json!({ "token_id": token_id }),
            )?;
            u128::from(token.is_some_and(|token| token.owner_id == engine_id))
        }
    };
    Ok(Custody {
        in_custody,
        held: Some(held),
        untracked: held.saturating_sub(in_custody),
        deficit: in_custody.saturating_sub(held),
    })
}
