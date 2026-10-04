use color_eyre::eyre::eyre;
use intear_dex_types::DexId;
use near_cli_rs::config::NetworkConfig;
use near_primitives::types::BlockReference;
use std::collections::HashMap;

use intear_dex_types::AssetId;
use near_primitives::types::AccountId;
use near_sdk::NearToken;
use near_sdk::json_types::U128;
use xyk_dex_types::{
    GetCommunityOwnedFeesArgs, GetPendingFeesArgs, GetPoolArgs, GetPoolSharesArgs, GetPoolsArgs,
    GetReferralSettingsArgs, PoolId, PoolNeedsUpgradeArgs, PoolView, ReferralSettings,
};

use super::ViewPanicked;
use super::engine::dex_view;
use crate::errors;

/// Whether an xyk dex has state, and whether its code can read it
pub enum XykState {
    Missing,
    Readable,
    Unreadable,
}

pub fn pool_count(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<PoolId> {
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
    const PAGE_SIZE: PoolId = 50;
    let pool_count = pool_count(network_config, block_reference, dex_id)?;
    let mut pools = Vec::new();
    let mut start_index: PoolId = 0;
    while start_index < pool_count {
        let page: Vec<PoolView> = dex_view(
            network_config,
            block_reference,
            dex_id,
            "get_pools",
            &GetPoolsArgs {
                start_index,
                limit: PAGE_SIZE,
            },
        )?;
        for (pool_id, pool) in (start_index..).zip(page) {
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

pub fn pool_needs_upgrade(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    pool_id: PoolId,
) -> color_eyre::eyre::Result<bool> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "pool_needs_upgrade",
        &PoolNeedsUpgradeArgs { pool_id },
    )
}

/// Its views tell: they panic without state, and when the code can't read it
pub fn state(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<XykState> {
    let error = match pool_count(network_config, block_reference, dex_id) {
        Ok(_) => return Ok(XykState::Readable),
        Err(error) => error,
    };
    match error.downcast_ref::<ViewPanicked>() {
        Some(view_panicked)
            if view_panicked
                .panic_message
                .contains(errors::NOT_INITIALIZED) =>
        {
            Ok(XykState::Missing)
        }
        Some(view_panicked)
            if view_panicked
                .panic_message
                .contains(errors::UNREADABLE_STATE) =>
        {
            Ok(XykState::Unreadable)
        }
        _ => Err(error),
    }
}

/// The shares an account has in each of `pool_ids`: `None` where it isn't
/// registered in the pool, as in every pool that isn't public
pub fn pool_shares(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    pool_ids: Vec<PoolId>,
    account_id: &AccountId,
) -> color_eyre::eyre::Result<Vec<Option<U128>>> {
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

/// The NEAR that launch pools collected for a community account
pub fn community_owned_fees(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    account_id: &AccountId,
) -> color_eyre::eyre::Result<NearToken> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "get_community_owned_fees",
        &GetCommunityOwnedFeesArgs {
            account_id: account_id.clone(),
        },
    )
}

pub fn referral_settings(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    account_id: &AccountId,
) -> color_eyre::eyre::Result<Option<ReferralSettings>> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "get_referral_settings",
        &GetReferralSettingsArgs {
            account_id: account_id.clone(),
        },
    )
}
