//! The pool views of amp dexes, by the pool ids that the tool takes, which
//! are xyk's. Their share, fee and fee discount views are xyk's, in
//! [`super::xyk`].

use amp_dex_types::{GetPoolArgs, GetPoolsArgs, PoolView};
use color_eyre::eyre::eyre;
use intear_dex_common::DexId;
use near_cli_rs::config::NetworkConfig;
use near_primitives::types::BlockReference;
use xyk_dex_types::PoolId;

use super::engine::dex_view;
pub use super::xyk::{pool_count, pool_shares};

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
