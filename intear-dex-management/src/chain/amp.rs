//! The pool views of amp dexes, by the pool ids that the tool takes, which
//! are xyk's. Their share, fee and fee discount views are xyk's, in
//! [`super::xyk`].

use amp_dex_types::{
    Amplification, AssetWithBalance, FeeFraction, GetPoolArgs, GetPoolsArgs, PoolView, Price,
    TotalShares,
};
use color_eyre::eyre::{Context, eyre};
use intear_dex_common::DexId;
use near_cli_rs::config::NetworkConfig;
use near_primitives::types::BlockReference;
use near_sdk::PublicKeyHandle;
use xyk_dex_types::PoolId;

use super::ViewPanicked;
use super::engine::{dex_view, dex_view_bytes};
pub use super::xyk::{pool_count, pool_shares};
use crate::errors;

/// The pool view of code from before oracle fees: its assets, decimals, amplification, oracle key,
/// last oracle price, fee and total shares
type PoolViewBeforeOracleFees = (
    (AssetWithBalance, AssetWithBalance),
    (u8, u8),
    Amplification,
    PublicKeyHandle,
    Option<Price>,
    FeeFraction,
    TotalShares,
);

/// Whether the pools of an amp dex are stored as they were before oracle fees. Code from before
/// them answers get_pool in that layout, and code with them can't read those pools.
pub fn pools_before_oracle_fees(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<bool> {
    if pool_count(network_config, block_reference, dex_id)? == 0 {
        return Ok(false);
    }
    let first_pool = match dex_view_bytes(
        network_config,
        block_reference,
        dex_id,
        "get_pool",
        near_sdk::borsh::to_vec(&GetPoolArgs {
            pool_id: amp_dex_types::PoolId::new(0),
        })?,
    ) {
        Ok(first_pool) => first_pool,
        Err(error) => {
            return match error.downcast_ref::<ViewPanicked>() {
                Some(view_panicked)
                    if view_panicked
                        .panic_message
                        .contains(errors::UNREADABLE_ELEMENT) =>
                {
                    Ok(true)
                }
                _ => Err(error),
            };
        }
    };
    if near_sdk::borsh::from_slice::<Option<PoolView>>(&first_pool).is_ok() {
        return Ok(false);
    }
    near_sdk::borsh::from_slice::<Option<PoolViewBeforeOracleFees>>(&first_pool).wrap_err_with(
        || {
            format!(
                "Pool #0 of {dex_id} is neither in the layout with oracle fees nor in the one before"
            )
        },
    )?;
    Ok(true)
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
                start_index: amp_dex_types::PoolId::new(start_index),
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
        &GetPoolArgs {
            pool_id: amp_dex_types::PoolId::new(pool_id.get()),
        },
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
