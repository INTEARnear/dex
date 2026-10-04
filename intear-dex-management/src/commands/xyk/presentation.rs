use color_eyre::eyre::eyre;
use intear_dex_types::AssetId;
use serde_json::json;
use xyk_dex_types::{
    CurrentFees, FeeAmount, FeeConfiguration, FeeFraction, FeeReceiver, PROTOCOL_FEE_RECEIVER_ID,
    PoolId, PoolView, ScheduledFeeCurve,
};

use crate::chain::asset_metadata::{AssetMetadata, AssetMetadataCache};
use crate::display::{self, asset_label};

/// The two assets of a pool with their reserves, in pool order.
pub fn pool_assets(pool: &PoolView) -> [(AssetId, u128); 2] {
    match pool {
        PoolView::Private { assets, .. } | PoolView::Public { assets, .. } => [
            (assets.0.asset_id.clone(), assets.0.balance.0),
            (assets.1.asset_id.clone(), assets.1.balance.0),
        ],
        PoolView::Launch {
            near_amount,
            launched_asset,
            ..
        } => [
            (AssetId::Near, near_amount.0),
            (launched_asset.asset_id.clone(), launched_asset.balance.0),
        ],
    }
}

pub fn pool_kind(pool: &PoolView) -> &'static str {
    match pool {
        PoolView::Private { .. } => "private",
        PoolView::Public { .. } => "public",
        PoolView::Launch { .. } => "launch",
    }
}

/// Fees charged now, including the protocol fee, and the configuration they
/// come from.
pub fn pool_fees(pool: &PoolView) -> (&CurrentFees, &FeeConfiguration) {
    match pool {
        PoolView::Private {
            fees,
            fee_configuration,
            ..
        }
        | PoolView::Public {
            fees,
            fee_configuration,
            ..
        }
        | PoolView::Launch {
            fees,
            fee_configuration,
            ..
        } => (fees, fee_configuration),
    }
}

pub fn total_fee_fraction(fees: &CurrentFees) -> color_eyre::eyre::Result<FeeFraction> {
    fees.receivers
        .iter()
        .try_fold(0, |total: FeeFraction, (_, fee)| total.checked_add(*fee))
        .ok_or_else(|| {
            eyre!(
                "The fees of this pool add up to more than {}",
                FeeFraction::MAX
            )
        })
}

pub fn fee_receiver_label(receiver: &FeeReceiver) -> String {
    match receiver {
        FeeReceiver::Account(account_id) if account_id == PROTOCOL_FEE_RECEIVER_ID => {
            "protocol".to_string()
        }
        FeeReceiver::Account(account_id) => account_id.to_string(),
        FeeReceiver::Pool => "pool".to_string(),
        FeeReceiver::Community(account_id) => format!("community:{account_id}"),
    }
}

/// The fees of a pool's configuration, with their schedules
pub fn format_fee_configuration(fee_configuration: &FeeConfiguration) -> String {
    let receiver_fees = match fee_configuration {
        FeeConfiguration::V1(fees) => fees
            .receivers
            .iter()
            .map(|(receiver, fee)| {
                format!(
                    "{} {}",
                    fee_receiver_label(receiver),
                    display::format_fee(*fee)
                )
            })
            .collect::<Vec<_>>(),
        FeeConfiguration::V2(fees) => fees
            .receivers
            .iter()
            .map(|(receiver, fee_amount)| {
                let fee = match fee_amount {
                    FeeAmount::Fixed(fee) => display::format_fee(*fee),
                    FeeAmount::Scheduled {
                        start: (start_time, start_fee),
                        end: (end_time, end_fee),
                        curve: ScheduledFeeCurve::Linear,
                    } => format!(
                        "{}..{} from {} to {} (linear)",
                        display::format_fee(*start_fee),
                        display::format_fee(*end_fee),
                        display::format_timestamp(*start_time),
                        display::format_timestamp(*end_time)
                    ),
                    FeeAmount::Dynamic { min, max } => format!(
                        "dynamic {}..{}",
                        display::format_fee(*min),
                        display::format_fee(*max)
                    ),
                };
                format!("{} {fee}", fee_receiver_label(receiver))
            })
            .collect(),
    };
    if receiver_fees.is_empty() {
        "none".to_string()
    } else {
        receiver_fees.join(", ")
    }
}

/// One line for pickers: pair, kind, total fee and reserves.
pub fn pool_label(
    pool_id: PoolId,
    pool: &PoolView,
    metadata_cache: &AssetMetadataCache,
) -> color_eyre::eyre::Result<String> {
    let [(asset_id_0, reserve_0), (asset_id_1, reserve_1)] = pool_assets(pool);
    let metadata_0 = metadata_cache.get(&asset_id_0)?;
    let metadata_1 = metadata_cache.get(&asset_id_1)?;
    Ok(format!(
        "#{pool_id}  {} / {} · {} · fees {} · {} / {}",
        asset_label(&asset_id_0, metadata_0.as_ref()),
        asset_label(&asset_id_1, metadata_1.as_ref()),
        pool_kind(pool),
        display::format_fee(total_fee_fraction(pool_fees(pool).0)?),
        display::format_amount(reserve_0, metadata_0.as_ref()),
        display::format_amount(reserve_1, metadata_1.as_ref()),
    ))
}

fn fee_configuration_json(fee_configuration: &FeeConfiguration) -> serde_json::Value {
    let receivers_json = match fee_configuration {
        FeeConfiguration::V1(fees) => fees
            .receivers
            .iter()
            .map(|(receiver, fee)| {
                json!({
                    "receiver": fee_receiver_label(receiver),
                    "fixed": display::fee_json(*fee),
                })
            })
            .collect::<Vec<_>>(),
        FeeConfiguration::V2(fees) => fees
            .receivers
            .iter()
            .map(|(receiver, fee_amount)| {
                let mut receiver_json = json!({ "receiver": fee_receiver_label(receiver) });
                match fee_amount {
                    FeeAmount::Fixed(fee) => receiver_json["fixed"] = display::fee_json(*fee),
                    FeeAmount::Scheduled {
                        start: (start_time, start_fee),
                        end: (end_time, end_fee),
                        curve: ScheduledFeeCurve::Linear,
                    } => {
                        receiver_json["scheduled"] = json!({
                            "start": {
                                "time": display::format_timestamp(*start_time),
                                "fee": display::fee_json(*start_fee),
                            },
                            "end": {
                                "time": display::format_timestamp(*end_time),
                                "fee": display::fee_json(*end_fee),
                            },
                            "curve": "linear",
                        })
                    }
                    FeeAmount::Dynamic { min, max } => {
                        receiver_json["dynamic"] = json!({
                            "min": display::fee_json(*min),
                            "max": display::fee_json(*max),
                        })
                    }
                }
                receiver_json
            })
            .collect(),
    };
    json!({ "receivers": receivers_json })
}

pub fn pool_json(
    pool_id: PoolId,
    pool: &PoolView,
    metadata_cache: &AssetMetadataCache,
) -> color_eyre::eyre::Result<serde_json::Value> {
    let mut assets_json = Vec::new();
    for (asset_id, reserve) in pool_assets(pool) {
        let metadata = metadata_cache.get(&asset_id)?;
        assets_json.push(json!({
            "asset_id": asset_id,
            "reserve": display::amount_json(reserve, metadata.as_ref()),
        }));
    }
    let (fees, fee_configuration) = pool_fees(pool);
    let fee_receivers_json = fees
        .receivers
        .iter()
        .map(|(receiver, fee)| {
            json!({
                "receiver": fee_receiver_label(receiver),
                "fee": display::fee_json(*fee),
            })
        })
        .collect::<Vec<_>>();
    let mut pool_json = json!({
        "pool_id": pool_id,
        "kind": pool_kind(pool),
        "assets": assets_json,
        "fees": {
            "total": display::fee_json(total_fee_fraction(fees)?),
            "receivers": fee_receivers_json,
        },
        "fee_configuration": fee_configuration_json(fee_configuration),
    });
    match pool {
        PoolView::Private {
            owner_id, locked, ..
        } => {
            pool_json["owner_id"] = json!(owner_id);
            pool_json["locked"] = json!(locked);
        }
        PoolView::Public { total_shares, .. } => {
            pool_json["total_shares"] =
                json!(total_shares.map(|total_shares| total_shares.0.to_string()));
        }
        PoolView::Launch {
            phantom_liquidity_near,
            ..
        } => {
            pool_json["phantom_liquidity_near"] =
                display::amount_json(phantom_liquidity_near.0, Some(&AssetMetadata::near()));
        }
    }
    Ok(pool_json)
}
