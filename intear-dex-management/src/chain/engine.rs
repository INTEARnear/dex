use color_eyre::eyre::{Context, bail};
use intear_dex_types::{AccountOrDexId, AssetId, DexId};
use near_cli_rs::common::JsonRpcClientExt;
use near_cli_rs::config::NetworkConfig;
use near_jsonrpc_client::methods::block::RpcBlockRequest;
use near_primitives::types::{AccountId, BlockId, BlockReference};
use near_primitives::views::BlockView;
use near_sdk::NearToken;
use near_sdk::json_types::{Base64VecU8, U128};
use serde_json::json;

use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::errors;

#[derive(serde::Deserialize)]
pub struct StorageBalance {
    pub total: NearToken,
    pub available: NearToken,
}

#[derive(serde::Deserialize)]
pub struct TotalStorageBalances {
    pub users: StorageBalance,
    pub dexes: StorageBalance,
}

pub fn engine_exists(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<bool> {
    let engine_account = super::account::view_account(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
    )?;
    Ok(engine_account.is_some())
}

/// The block that `block_reference` points to, once the engine is known to
/// exist in it
pub fn engine_block(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<BlockView> {
    let block = network_config
        .json_rpc_client()
        .blocking_call(RpcBlockRequest {
            block_reference: block_reference.clone(),
        })
        .map_err(color_eyre::eyre::Report::new)
        .wrap_err_with(|| {
            format!(
                "Couldn't get the block to read from on network {} (RPC {})",
                network_config.network_name, network_config.rpc_url
            )
        })?;
    if !engine_exists(
        network_config,
        &BlockReference::BlockId(BlockId::Hash(block.header.hash)),
    )? {
        bail!(
            "{ENGINE_ACCOUNT_ID} doesn't exist on network {} (RPC {})",
            network_config.network_name,
            network_config.rpc_url
        );
    }
    Ok(block)
}

/// Pins `block_reference` to a block hash, so that every view of one command
/// reads the same state, and checks that the engine exists in that block.
pub fn engine_view_block(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<BlockReference> {
    let block = engine_block(network_config, block_reference)?;
    Ok(BlockReference::BlockId(BlockId::Hash(block.header.hash)))
}

pub fn is_paused(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<bool> {
    super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "is_paused",
        json!({}),
    )
}

pub fn trusted_code_deployer(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<AccountId> {
    super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "get_trusted_code_deployer",
        json!({}),
    )
}

pub fn total_storage_balances(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<TotalStorageBalances> {
    super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "total_storage_balances",
        json!({}),
    )
}

pub fn total_in_custody(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    asset_id: &AssetId,
) -> color_eyre::eyre::Result<Option<U128>> {
    super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "total_in_custody",
        json!({ "asset_id": asset_id }),
    )
}

pub fn untracked_near(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<NearToken> {
    super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "untracked_near",
        json!({}),
    )
}

pub fn registered_assets_of(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    owner: &AccountOrDexId,
) -> color_eyre::eyre::Result<Vec<(AssetId, U128)>> {
    const PAGE_SIZE: u32 = 100;
    let mut registered_assets = Vec::new();
    loop {
        let from_index = u32::try_from(registered_assets.len())?;
        let page: Vec<(AssetId, U128)> = super::view_json(
            network_config,
            block_reference,
            &ENGINE_ACCOUNT_ID.to_owned(),
            "registered_assets_of",
            json!({ "of": owner, "from_index": from_index, "limit": PAGE_SIZE }),
        )?;
        let is_last_page = page.len() < PAGE_SIZE as usize;
        registered_assets.extend(page);
        if is_last_page {
            return Ok(registered_assets);
        }
    }
}

/// Whether an account or a dex has balances on the engine yet, which the
/// first registration for it creates
pub fn has_registered_assets(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    owner: &AccountOrDexId,
) -> color_eyre::eyre::Result<bool> {
    let first_asset: Vec<(AssetId, U128)> = super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "registered_assets_of",
        json!({ "of": owner, "from_index": 0, "limit": 1 }),
    )?;
    Ok(!first_asset.is_empty())
}

pub fn asset_balance_of(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    owner: &AccountOrDexId,
    asset_id: &AssetId,
) -> color_eyre::eyre::Result<Option<U128>> {
    super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "asset_balance_of",
        json!({ "of": owner, "asset_id": asset_id }),
    )
}

pub fn storage_balance_of(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    account_id: &AccountId,
) -> color_eyre::eyre::Result<Option<StorageBalance>> {
    super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "storage_balance_of",
        json!({ "account_id": account_id }),
    )
}

pub fn dex_storage_balance_of(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<Option<StorageBalance>> {
    super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "dex_storage_balance_of",
        json!({ "dex_id": dex_id }),
    )
}

pub fn minimum_dex_storage_deposit(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<NearToken> {
    #[derive(serde::Deserialize)]
    struct StorageBalanceBounds {
        min: NearToken,
    }
    let bounds: StorageBalanceBounds = super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "dex_storage_balance_bounds",
        json!({}),
    )?;
    Ok(bounds.min)
}

pub fn minimum_storage_deposit(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<NearToken> {
    #[derive(serde::Deserialize)]
    struct StorageBalanceBounds {
        min: NearToken,
    }
    let bounds: StorageBalanceBounds = super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "storage_balance_bounds",
        json!({}),
    )?;
    Ok(bounds.min)
}

/// The engine has no view for whether a dex exists, but `dex_view` looks up
/// the dex's code before the method, and no dex has a method without a name
pub fn dex_exists(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<bool> {
    let nameless_method_view = super::view_json::<Base64VecU8>(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "dex_view",
        json!({
            "dex_id": dex_id,
            "method": "",
            "args": Base64VecU8(Vec::new()),
        }),
    );
    match nameless_method_view {
        Ok(_) => Ok(true),
        Err(error) => match error.downcast::<super::ViewPanicked>() {
            Ok(view_panicked) => Ok(view_panicked.panic_message != errors::DEX_CODE_NOT_FOUND),
            Err(error) => Err(error),
        },
    }
}

/// A view method of a dex, with arguments and result as the dex encodes them
pub fn dex_view_bytes(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    method_name: &str,
    args: Vec<u8>,
) -> color_eyre::eyre::Result<Vec<u8>> {
    let Base64VecU8(result) = super::view_json(
        network_config,
        block_reference,
        &ENGINE_ACCOUNT_ID.to_owned(),
        "dex_view",
        json!({
            "dex_id": dex_id,
            "method": method_name,
            "args": Base64VecU8(args),
        }),
    )
    .map_err(|error| match error.downcast::<super::ViewPanicked>() {
        Ok(view_panicked) => super::view_panicked_report(view_panicked, Some(dex_id)),
        Err(error) => error,
    })?;
    Ok(result)
}

pub fn dex_view<ViewResult: near_sdk::borsh::BorshDeserialize>(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    method_name: &str,
    args: &impl near_sdk::borsh::BorshSerialize,
) -> color_eyre::eyre::Result<ViewResult> {
    let result = dex_view_bytes(
        network_config,
        block_reference,
        dex_id,
        method_name,
        near_sdk::borsh::to_vec(args)?,
    )?;
    near_sdk::borsh::from_slice(&result)
        .wrap_err_with(|| format!("Unexpected result of {method_name} on dex {dex_id}"))
}
