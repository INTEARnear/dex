use color_eyre::eyre::eyre;
use intear_dex_common::DexId;
use near_cli_rs::config::NetworkConfig;
use near_primitives::types::BlockReference;
use std::collections::HashMap;

use intear_dex_common::AssetId;
use near_primitives::types::AccountId;
use near_sdk::PublicKey;
use near_sdk::json_types::U128;
use xyk_dex_types::{
    GetClaimableFeesArgs, GetPendingFeesArgs, GetPoolArgs, GetPoolSharesArgs, GetPoolsArgs, PoolId,
    PoolView, ProviderShares,
};

use super::engine::dex_view;

pub fn pool_count(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<u32> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "get_pool_count",
        &(),
    )
}

pub fn all_pools(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<Vec<(PoolId, PoolView)>> {
    const PAGE_SIZE: u32 = 50;
    let pool_count = pool_count(network_config, block_reference, dex_id)?;
    let mut pools = Vec::new();
    let mut start_index: u32 = 0;
    while start_index < pool_count {
        let page: Vec<PoolView> = dex_view(
            network_config,
            block_reference,
            dex_id,
            "get_pools",
            &GetPoolsArgs {
                start_index: PoolId::new(start_index),
                limit: PAGE_SIZE,
            },
        )?;
        for (pool_id, pool) in (start_index..).map(PoolId::new).zip(page) {
            pools.push((pool_id, pool));
        }
        start_index = start_index.saturating_add(PAGE_SIZE);
    }
    Ok(pools)
}

pub fn pool(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    pool_id: PoolId,
) -> color_eyre::eyre::Result<PoolView> {
    let pool: Option<PoolView> = dex_view(
        network_config,
        block_reference,
        dex_id,
        "get_pool",
        &GetPoolArgs { pool_id },
    )?;
    pool.ok_or_else(|| {
        let pool_count = pool_count(network_config, block_reference, dex_id);
        match pool_count {
            Ok(pool_count) => {
                eyre!("Pool #{pool_id} doesn't exist on {dex_id}, which has {pool_count} pools")
            }
            Err(error) => eyre!("Pool #{pool_id} doesn't exist on {dex_id}: {error}"),
        }
    })
}

/// The shares an account has in each of `pool_ids`: `None` where it isn't
/// registered in the pool, as in every pool that isn't public
pub fn pool_shares(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    pool_ids: Vec<PoolId>,
    account_id: &AccountId,
) -> color_eyre::eyre::Result<Vec<Option<ProviderShares>>> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "get_pool_shares",
        &GetPoolSharesArgs {
            pool_ids,
            account_id: account_id.clone(),
        },
    )
}

/// The fees an account collected and hasn't withdrawn, for the assets among
/// `asset_ids` that it collects fees in
pub fn pending_fees(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    account_id: &AccountId,
    asset_ids: Vec<AssetId>,
) -> color_eyre::eyre::Result<HashMap<AssetId, U128>> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "get_pending_fees",
        &GetPendingFeesArgs {
            account_id: account_id.clone(),
            asset_ids,
        },
    )
}

/// The fees that anyone can claim for an account, for the assets among
/// `asset_ids` that it has a balance of them in
pub fn claimable_fees(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    account_id: &AccountId,
    asset_ids: Vec<AssetId>,
) -> color_eyre::eyre::Result<HashMap<AssetId, U128>> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "get_claimable_fees",
        &GetClaimableFeesArgs {
            account_id: account_id.clone(),
            asset_ids,
        },
    )
}

/// The key that signs fee discounts on the dex, if any
pub fn fee_discount_signer(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<Option<PublicKey>> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "get_fee_discount_signer",
        &(),
    )
}
