use near_cli_rs::config::NetworkConfig;
use near_primitives::types::{AccountId, BlockReference};
use near_sdk::NearToken;
use near_sdk::json_types::U128;
use serde_json::json;

pub fn ft_balance_of(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    token_id: &AccountId,
    account_id: &AccountId,
) -> color_eyre::eyre::Result<u128> {
    let U128(balance) = super::view_json(
        network_config,
        block_reference,
        token_id,
        "ft_balance_of",
        json!({ "account_id": account_id }),
    )?;
    Ok(balance)
}

/// Whether `account_id` has storage on the token contract, which it needs to
/// receive tokens
pub fn is_registered(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    token_id: &AccountId,
    account_id: &AccountId,
) -> color_eyre::eyre::Result<bool> {
    let storage_balance: Option<serde_json::Value> = super::view_json(
        network_config,
        block_reference,
        token_id,
        "storage_balance_of",
        json!({ "account_id": account_id }),
    )?;
    Ok(storage_balance.is_some())
}

pub fn minimum_storage_deposit(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    token_id: &AccountId,
) -> color_eyre::eyre::Result<NearToken> {
    #[derive(serde::Deserialize)]
    struct StorageBalanceBounds {
        min: NearToken,
    }
    let bounds: StorageBalanceBounds = super::view_json(
        network_config,
        block_reference,
        token_id,
        "storage_balance_bounds",
        json!({}),
    )?;
    Ok(bounds.min)
}
