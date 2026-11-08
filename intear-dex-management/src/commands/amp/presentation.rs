use amp_dex_types::{PROTOCOL_FEE_SHARE, PoolView};
use intear_dex_common::AssetId;
use serde_json::json;
use xyk_dex_types::PoolId;

use crate::chain::asset_metadata::AssetMetadataCache;
use crate::display::{self, asset_label};

/// The two assets of a pool with their reserves, in pool order
pub fn pool_assets(pool: &PoolView) -> [(AssetId, u128); 2] {
    [
        (pool.assets.0.asset_id.clone(), pool.assets.0.balance.get()),
        (pool.assets.1.asset_id.clone(), pool.assets.1.balance.get()),
    ]
}

/// The fee of swaps, and the protocol's part of it
pub fn fee_label(pool: &PoolView) -> String {
    format!(
        "{}, of which the protocol takes {PROTOCOL_FEE_SHARE}",
        display::format_fee(pool.fee)
    )
}

/// One line for pickers: pair, amplification, fee and reserves
pub fn pool_label(
    pool_id: PoolId,
    pool: &PoolView,
    metadata_cache: &AssetMetadataCache,
) -> color_eyre::eyre::Result<String> {
    let [(asset_id_0, reserve_0), (asset_id_1, reserve_1)] = pool_assets(pool);
    let metadata_0 = metadata_cache.get(&asset_id_0)?;
    let metadata_1 = metadata_cache.get(&asset_id_1)?;
    Ok(format!(
        "#{pool_id}  {} / {} · amplification {} · fee {} · {} / {}",
        asset_label(&asset_id_0, metadata_0.as_ref()),
        asset_label(&asset_id_1, metadata_1.as_ref()),
        pool.amplification,
        display::format_fee(pool.fee),
        display::format_amount(reserve_0, metadata_0.as_ref()),
        display::format_amount(reserve_1, metadata_1.as_ref()),
    ))
}

pub fn pool_json(
    pool_id: PoolId,
    pool: &PoolView,
    metadata_cache: &AssetMetadataCache,
) -> color_eyre::eyre::Result<serde_json::Value> {
    let mut assets_json = Vec::new();
    for ((asset_id, reserve), decimals) in pool_assets(pool)
        .iter()
        .zip([pool.decimals.0, pool.decimals.1])
    {
        let metadata = metadata_cache.get(asset_id)?;
        assets_json.push(json!({
            "asset_id": asset_id,
            "decimals": decimals,
            "reserve": display::amount_json(*reserve, metadata.as_ref()),
        }));
    }
    let swap_fees = pool.swap_fees();
    Ok(json!({
        "pool_id": pool_id,
        "assets": assets_json,
        "amplification": pool.amplification,
        "oracle_key": pool.oracle_key,
        "last_oracle_price": pool.last_oracle_price,
        "fee": display::fee_json(pool.fee),
        "fees": {
            "pool": display::fee_json(swap_fees.pool),
            "protocol": display::fee_json(swap_fees.protocol),
        },
        "total_shares": pool.total_shares,
    }))
}
