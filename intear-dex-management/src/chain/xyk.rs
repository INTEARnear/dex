use color_eyre::eyre::{Context, Report, bail, eyre};
use intear_dex_types::DexId;
use near_cli_rs::common::JsonRpcClientExt;
use near_cli_rs::config::NetworkConfig;
use near_jsonrpc_client::methods::query::RpcQueryRequest;
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use near_primitives::types::{BlockReference, StoreKey};
use near_primitives::views::QueryRequest;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;

use intear_dex_types::AssetId;
use near_primitives::types::AccountId;
use near_sdk::PublicKey;
use near_sdk::json_types::U128;
use xyk_dex_types::{
    GetClaimableFeesArgs, GetPendingFeesArgs, GetPoolArgs, GetPoolSharesArgs, GetPoolsArgs, PoolId,
    PoolView, SharesBalance,
};

use super::ViewPanicked;
use super::engine::dex_view;
use crate::deployment::ENGINE_ACCOUNT_ID;
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

/// The liquidity providers of public pools that are still stored as their shares, the way
/// public pools stored them before the migration that gave them positions, by pool. The dex
/// can't list a map's entries, so this reads them from the engine's state, a page at a time.
pub fn providers_from_before_migration(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<BTreeMap<PoolId, Vec<AccountId>>> {
    // The engine keeps the storage of every dex under this prefix, keyed by the dex id and the
    // dex's key as a byte vector, with the dex's value as a byte vector
    const ENGINE_DEX_STORAGE_PREFIX: u8 = 1;
    // The dex's key of a public pool's provider: this, the pool id, and the account id
    const PUBLIC_POOL_PROVIDERS_PREFIX: u8 = 1;
    // Nodes return fewer when the page would be too big
    const PAGE_LIMIT: NonZeroU32 = NonZeroU32::new(1000).unwrap();
    let dex_prefix = [
        vec![ENGINE_DEX_STORAGE_PREFIX],
        near_sdk::borsh::to_vec(dex_id)?,
    ]
    .concat();
    let mut providers: BTreeMap<PoolId, Vec<AccountId>> = BTreeMap::new();
    let mut after_key = None;
    loop {
        let response = network_config
            .json_rpc_client()
            .blocking_call(RpcQueryRequest {
                block_reference: block_reference.clone(),
                request: QueryRequest::ViewState {
                    account_id: ENGINE_ACCOUNT_ID.to_owned(),
                    prefix: StoreKey::from(dex_prefix.clone()),
                    after_key,
                    limit: Some(PAGE_LIMIT),
                    include_proof: false,
                },
            })
            .map_err(Report::new)
            .wrap_err_with(|| {
                format!(
                    "Couldn't read the state of {dex_id} on network {} (RPC {})",
                    network_config.network_name, network_config.rpc_url
                )
            })?;
        let QueryResponseKind::ViewState(state) = response.kind else {
            bail!(
                "RPC {} answered a state query with something other than state",
                network_config.rpc_url
            );
        };
        for item in state.values {
            let dex_key: Vec<u8> = near_sdk::borsh::from_slice(&item.key[dex_prefix.len()..])?;
            let dex_value: Vec<u8> = near_sdk::borsh::from_slice(&item.value)?;
            // A position doesn't read as shares
            if dex_key.first() == Some(&PUBLIC_POOL_PROVIDERS_PREFIX)
                && near_sdk::borsh::from_slice::<Option<SharesBalance>>(&dex_value).is_ok()
            {
                let (_, pool_id, account_id): (u8, PoolId, AccountId) =
                    near_sdk::borsh::from_slice(&dex_key)?;
                providers.entry(pool_id).or_default().push(account_id);
            }
        }
        match state.last_key {
            Some(last_key) => after_key = Some(last_key),
            None => return Ok(providers),
        }
    }
}
