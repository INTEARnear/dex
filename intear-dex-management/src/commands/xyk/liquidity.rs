use std::num::NonZeroU128;

use intear_dex_types::{AccountOrDexId, DexId};
use near_cli_rs::commands::ActionContext;
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::json_types::U128;
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use xyk_dex_types::{
    AddLiquidityArgs, INITIAL_SHARES, FULL_FEE_FRACTION, PoolId, PoolView, RegisterLiquidityArgs,
    RemoveLiquidityArgs,
};

use super::presentation::{pool_assets, pool_kind};
use super::{PoolToWrite, XykContext};
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::{engine, fungible_token, xyk};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::inputs::amount::AmountArg;
use crate::inputs::percent::PercentArg;
use crate::inputs::pool_id::{PoolIdArg, PoolIdListArg};
use crate::inputs::share::ShareArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::xyk_math::{
    at_least_with_slippage, liquidity_for_shares, mul_div_floor, shares_for_liquidity,
};
use crate::planning::{
    self, FunctionCall, ONE_YOCTO_NEAR, Preflight, TransactionPlan, format_near, storage,
    xyk_storage,
};

const ADD_LIQUIDITY_GAS: Gas = Gas::from_teragas(100);
const REMOVE_LIQUIDITY_GAS: Gas = Gas::from_teragas(100);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
pub struct LiquidityCommands {
    #[interactive_clap(subcommand)]
    action: LiquidityAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do with liquidity?
pub enum LiquidityAction {
    #[strum_discriminants(strum(
        message = "show    - The pools an account owns or has shares in, and what its part holds"
    ))]
    /// The pools an account owns or has shares in, and what its part holds
    Show(Show),
    #[strum_discriminants(strum(
        message = "add     - Add both assets of a pool from the signer's balance on dex.intear.near"
    ))]
    /// Add both assets of a pool from the signer's balance on dex.intear.near
    Add(Add),
    #[strum_discriminants(strum(
        message = "remove  - Take liquidity out of a pool to the signer's wallet"
    ))]
    /// Take liquidity out of a pool to the signer's wallet
    Remove(Remove),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = ShowContext)]
pub struct Show {
    #[interactive_clap(skip_default_input_arg)]
    /// Whose liquidity?
    account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// Only these pools, comma-separated ids (default: every pool)
    pools: Option<PoolIdListArg>,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl Show {
    fn input_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_non_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Whose liquidity?",
        )
    }
}

/// An account's part of a pool
struct Position {
    pool_id: PoolId,
    pool: PoolView,
    /// `None` for the owner of a private pool, which has no shares
    shares: Option<u128>,
    share_of_pool: String,
    amounts: (u128, u128),
}

fn positions(
    network_config: &near_cli_rs::config::NetworkConfig,
    block_reference: &near_primitives::types::BlockReference,
    dex_id: &DexId,
    account_id: &AccountId,
    listed_pool_ids: Option<&[PoolId]>,
) -> color_eyre::eyre::Result<Vec<Position>> {
    let pools = match listed_pool_ids {
        Some(pool_ids) => pool_ids
            .iter()
            .map(|pool_id| {
                xyk::pool(network_config, block_reference, dex_id, *pool_id)
                    .map(|pool| (*pool_id, pool))
            })
            .collect::<color_eyre::eyre::Result<Vec<_>>>()?,
        None => xyk::all_pools(network_config, block_reference, dex_id)?,
    };
    let public_pool_ids = pools
        .iter()
        .filter(|(_, pool)| matches!(pool, PoolView::Public { .. }))
        .map(|(pool_id, _)| *pool_id)
        .collect::<Vec<_>>();
    let public_pool_shares = if public_pool_ids.is_empty() {
        Vec::new()
    } else {
        xyk::pool_shares(
            network_config,
            block_reference,
            dex_id,
            public_pool_ids.clone(),
            account_id,
        )?
    };
    let mut positions = Vec::new();
    for (pool_id, pool) in pools {
        let [(_, reserve_0), (_, reserve_1)] = pool_assets(&pool);
        match &pool {
            PoolView::Private { owner_id, .. } if owner_id == account_id => {
                positions.push(Position {
                    pool_id,
                    shares: None,
                    share_of_pool: "100%".to_string(),
                    amounts: (reserve_0, reserve_1),
                    pool,
                });
            }
            PoolView::Public {
                total_shares: Some(U128(total_shares)),
                ..
            } => {
                let shares = public_pool_ids
                    .iter()
                    .position(|public_pool_id| *public_pool_id == pool_id)
                    .and_then(|index| public_pool_shares.get(index).copied().flatten());
                if let Some(U128(shares)) = shares.filter(|U128(shares)| *shares > 0) {
                    positions.push(Position {
                        pool_id,
                        shares: Some(shares),
                        share_of_pool: display::format_share(shares, *total_shares)?,
                        amounts: liquidity_for_shares(
                            shares,
                            *total_shares,
                            (reserve_0, reserve_1),
                        )?,
                        pool,
                    });
                }
            }
            PoolView::Private { .. } | PoolView::Public { .. } | PoolView::Launch { .. } => {}
        }
    }
    Ok(positions)
}

#[derive(Clone)]
pub struct ShowContext(ArgsForViewContext);

impl ShowContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Show as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let account_id: AccountId = scope.account_id.clone().into();
        let listed_pool_ids = scope.pools.clone().map(|PoolIdListArg(pool_ids)| pool_ids);
        let dex_id = previous_context.dex_id;
        let output_format = previous_context.global_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let positions = positions(
                    network_config,
                    &block_reference,
                    &dex_id,
                    &account_id,
                    listed_pool_ids.as_deref(),
                )?;
                let metadata_cache =
                    AssetMetadataCache::new(network_config, block_reference.clone());
                match output_format {
                    OutputFormat::Table if positions.is_empty() => {
                        println!("{account_id} has no liquidity on {dex_id}");
                    }
                    OutputFormat::Table => {
                        let mut table =
                            display::table(&["Pool", "Pair", "Kind", "Shares", "Share", "Holds"]);
                        for position in &positions {
                            let [(asset_id_0, _), (asset_id_1, _)] = pool_assets(&position.pool);
                            let metadata_0 = metadata_cache.get(&asset_id_0)?;
                            let metadata_1 = metadata_cache.get(&asset_id_1)?;
                            table.add_row(prettytable::row![
                                format!("#{}", position.pool_id),
                                format!(
                                    "{} / {}",
                                    display::asset_label(&asset_id_0, metadata_0.as_ref()),
                                    display::asset_label(&asset_id_1, metadata_1.as_ref())
                                ),
                                pool_kind(&position.pool),
                                position
                                    .shares
                                    .map_or("owner".to_string(), |shares| shares.to_string()),
                                position.share_of_pool,
                                format!(
                                    "{} / {}",
                                    display::format_amount(position.amounts.0, metadata_0.as_ref()),
                                    display::format_amount(position.amounts.1, metadata_1.as_ref())
                                ),
                            ]);
                        }
                        print!("{table}");
                    }
                    OutputFormat::Json => {
                        let mut positions_json = Vec::new();
                        for position in &positions {
                            let [(asset_id_0, _), (asset_id_1, _)] = pool_assets(&position.pool);
                            positions_json.push(json!({
                                "pool_id": position.pool_id,
                                "kind": pool_kind(&position.pool),
                                "shares": position.shares.map(|shares| shares.to_string()),
                                "share_of_pool": position.share_of_pool,
                                "holds": [
                                    {
                                        "asset_id": asset_id_0,
                                        "amount": display::amount_json(position.amounts.0, metadata_cache.get(&asset_id_0)?.as_ref()),
                                    },
                                    {
                                        "asset_id": asset_id_1,
                                        "amount": display::amount_json(position.amounts.1, metadata_cache.get(&asset_id_1)?.as_ref()),
                                    },
                                ],
                            }));
                        }
                        display::print_json(&json!({
                            "account_id": account_id,
                            "dex_id": dex_id,
                            "positions": positions_json,
                        }))?;
                    }
                }
                Ok(())
            });
        Ok(Self(ArgsForViewContext {
            config: previous_context.global_context.config,
            interacting_with_account_ids: vec![ENGINE_ACCOUNT_ID.to_owned()],
            on_after_getting_block_reference_callback,
        }))
    }
}

impl From<ShowContext> for ArgsForViewContext {
    fn from(item: ShowContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = AddContext)]
pub struct Add {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account adds liquidity, from its balance on dex.intear.near?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The pool id
    pool_id: PoolIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How much of one asset, e.g. '10 NEAR'
    amount_0: AmountArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How much of the other asset, e.g. '30 USDT'
    amount_1: AmountArg,
    #[interactive_clap(named_arg)]
    /// How far the pool's ratio may move before the transaction
    max_slippage: AddMaxSlippage,
}

impl Add {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account adds liquidity, from its balance on dex.intear.near?",
        )
    }

    fn input_pool_id(context: &XykContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
        super::input_pool_id(context)
    }

    fn input_amount_0(_context: &XykContext) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt("How much of one asset? e.g. '10 NEAR'")
    }

    fn input_amount_1(_context: &XykContext) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt("How much of the other asset? e.g. '30 USDT'")
    }
}

#[derive(Clone)]
pub struct AddContext {
    xyk_context: XykContext,
    signer_id: AccountId,
    pool_id: PoolId,
    amounts: (AmountArg, AmountArg),
}

impl AddContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Add as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self {
            xyk_context: previous_context,
            signer_id: scope.signer_account_id.clone().into(),
            pool_id: scope.pool_id.0,
            amounts: (scope.amount_0.clone(), scope.amount_1.clone()),
        })
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = AddContext)]
#[interactive_clap(output_context = AddMaxSlippageContext)]
pub struct AddMaxSlippage {
    #[interactive_clap(skip_default_input_arg)]
    /// The most the share price may move against you, e.g. 0.5%
    percent: PercentArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl AddMaxSlippage {
    fn input_percent(_context: &AddContext) -> color_eyre::eyre::Result<Option<PercentArg>> {
        crate::inputs::prompt("Max slippage? e.g. 0.5%")
    }
}

/// The signer's amounts of the two assets of a pool, in pool order
fn amounts_in_pool_order(
    pool: &PoolToWrite,
    amounts: &(AmountArg, AmountArg),
) -> color_eyre::eyre::Result<(u128, u128)> {
    let (first_asset_id, first_amount) = amounts.0.resolve(&pool.assets)?;
    let (second_asset_id, second_amount) = amounts.1.resolve(&pool.assets)?;
    if first_asset_id == second_asset_id {
        return Err(PreflightError::AmountsOfOneAsset {
            pair: pool.pair_label(),
        }
        .into_report());
    }
    let amounts = if first_asset_id == pool.assets[0].0 {
        (first_amount, second_amount)
    } else {
        (second_amount, first_amount)
    };
    if amounts.0 == 0 || amounts.1 == 0 {
        return Err(PreflightError::ZeroAmount.into_report());
    }
    Ok(amounts)
}

#[derive(Clone)]
pub struct AddMaxSlippageContext(ActionContext);

impl AddMaxSlippageContext {
    pub fn from_previous_context(
        previous_context: AddContext,
        scope: &<AddMaxSlippage as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let AddContext {
            xyk_context,
            signer_id,
            pool_id,
            amounts,
        } = previous_context;
        let dex_id = xyk_context.dex_id;
        let PercentArg(max_slippage) = scope.percent;
        Ok(Self(planning::write_action_context(
            &xyk_context.global_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                let signer_id = preflight.signer_id.clone();
                let pool = PoolToWrite::read(preflight, &dex_id, pool_id)?;
                let (amount_0, amount_1) = amounts_in_pool_order(&pool, &amounts)?;
                for (side, amount) in [(0, amount_0), (1, amount_1)] {
                    let (asset_id, metadata) = &pool.assets[side];
                    preflight.ensure_engine_balance(asset_id, metadata.as_ref(), amount)?;
                }
                let pool_label = format!("pool #{pool_id} ({}) of {dex_id}", pool.pair_label());
                let amounts_label = format!(
                    "{} and {}",
                    pool.format_amount(0, amount_0),
                    pool.format_amount(1, amount_1)
                );
                let attached_assets = [
                    (pool.assets[0].0.clone(), amount_0),
                    (pool.assets[1].0.clone(), amount_1),
                ];
                let mut steps = Vec::new();
                let mut function_calls = Vec::new();
                let mut operations = Vec::new();
                let mut storage_near = near_sdk::NearToken::from_yoctonear(0);
                match &pool.pool {
                    PoolView::Launch { .. } => {
                        return Err(PreflightError::PoolKindNotSupported {
                            dex_id: dex_id.clone(),
                            pool_id,
                            kind: "launch",
                            reason: "it takes no liquidity after it's created",
                        }
                        .into_report());
                    }
                    PoolView::Private { owner_id, .. } => {
                        if *owner_id != signer_id {
                            return Err(PreflightError::NotPoolOwner {
                                dex_id: dex_id.clone(),
                                pool_id,
                                owner_id: owner_id.clone(),
                                signer_id: signer_id.clone(),
                            }
                            .into_report());
                        }
                        steps.push(format!("Add {amounts_label} to {pool_label}"));
                        steps.push(
                            "A private pool takes both amounts as they are, so max-slippage doesn't apply"
                                .to_string(),
                        );
                        operations.push(planning::dex_call_operation(
                            &dex_id,
                            "add_liquidity",
                            &AddLiquidityArgs {
                                pool_id,
                                min_shares_received: None,
                            },
                            &attached_assets,
                        )?);
                    }
                    PoolView::Public { total_shares, .. } => {
                        let is_registered = xyk::pool_shares(
                            &preflight.network_config,
                            &preflight.block_reference,
                            &dex_id,
                            vec![pool_id],
                            &signer_id,
                        )?
                        .first()
                        .copied()
                        .flatten()
                        .is_some();
                        if !is_registered {
                            storage_near = storage::storage_cost(
                                xyk_storage::liquidity_registration_bytes(
                                    &dex_id,
                                    &signer_id,
                                    preflight.protocol_limits.extra_bytes_per_record,
                                )?,
                                preflight.protocol_limits.storage_byte_cost,
                            )?;
                            let registration_calls = preflight.registration_calls(
                                &super::storage_near_registrations(&signer_id, &dex_id),
                            )?;
                            steps.extend(registration_calls.steps);
                            function_calls.extend(registration_calls.function_calls);
                            steps.push(format!(
                                "Register {signer_id} in {pool_label}, attaching {} from its wallet for the storage that takes; {dex_id} sends back what it doesn't use",
                                format_near(storage_near)
                            ));
                            operations.push(planning::dex_call_operation(
                                &dex_id,
                                "register_liquidity",
                                &RegisterLiquidityArgs { pool_id },
                                &[(intear_dex_types::AssetId::Near, storage_near.as_yoctonear())],
                            )?);
                        }
                        let min_shares_received = match total_shares {
                            // The first liquidity sets the price, and gets
                            // all the shares there are
                            None => {
                                steps.push(format!(
                                    "Add {amounts_label} to {pool_label}, the first liquidity, which sets its price"
                                ));
                                None
                            }
                            Some(U128(total_shares)) => {
                                let [(_, reserve_0), (_, reserve_1)] = pool_assets(&pool.pool);
                                let expected_shares = shares_for_liquidity(
                                    (amount_0, amount_1),
                                    (reserve_0, reserve_1),
                                    *total_shares,
                                )?;
                                let min_shares =
                                    at_least_with_slippage(expected_shares, max_slippage)?;
                                steps.push(format!(
                                    "Add up to {amounts_label} to {pool_label} for at least {min_shares} shares, {expected_shares} at the current ratio; what doesn't fit the ratio goes back to {signer_id}'s wallet"
                                ));
                                NonZeroU128::new(min_shares)
                            }
                        };
                        operations.push(planning::dex_call_operation(
                            &dex_id,
                            "add_liquidity",
                            &AddLiquidityArgs {
                                pool_id,
                                min_shares_received,
                            },
                            &attached_assets,
                        )?);
                    }
                }
                function_calls.push(FunctionCall {
                    method_name: "execute_operations",
                    args: json!({ "operations": operations }),
                    deposit: if storage_near.is_zero() {
                        ONE_YOCTO_NEAR
                    } else {
                        storage_near
                    },
                    gas: ADD_LIQUIDITY_GAS,
                });
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "liquidity addition",
                        success_message: format!("Added liquidity to {pool_label}"),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<AddMaxSlippageContext> for ActionContext {
    fn from(item: AddMaxSlippageContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = RemoveContext)]
pub struct Remove {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account removes its liquidity?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The pool id
    pool_id: PoolIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How much: a percentage like 50% (of a private pool, or of your shares of a public one), all, or '<integer> raw-shares'
    share: ShareArg,
    #[interactive_clap(named_arg)]
    /// How far the pool's reserves may move before the transaction
    max_slippage: RemoveMaxSlippage,
}

impl Remove {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account removes its liquidity?",
        )
    }

    fn input_pool_id(context: &XykContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
        super::input_pool_id(context)
    }

    fn input_share(_context: &XykContext) -> color_eyre::eyre::Result<Option<ShareArg>> {
        crate::inputs::prompt(
            "How much? A percentage like 50% (of a private pool, or of your shares of a public one), all, or '<integer> raw-shares'",
        )
    }
}

#[derive(Clone)]
pub struct RemoveContext {
    xyk_context: XykContext,
    signer_id: AccountId,
    pool_id: PoolId,
    share: ShareArg,
}

impl RemoveContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Remove as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self {
            xyk_context: previous_context,
            signer_id: scope.signer_account_id.clone().into(),
            pool_id: scope.pool_id.0,
            share: scope.share,
        })
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = RemoveContext)]
#[interactive_clap(output_context = RemoveMaxSlippageContext)]
pub struct RemoveMaxSlippage {
    #[interactive_clap(skip_default_input_arg)]
    /// The most the amounts may fall before the transaction, e.g. 0.5%
    percent: PercentArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl RemoveMaxSlippage {
    fn input_percent(_context: &RemoveContext) -> color_eyre::eyre::Result<Option<PercentArg>> {
        crate::inputs::prompt("Max slippage? e.g. 0.5%")
    }
}

/// The shares to remove out of `owned`, for `share` of them
fn shares_to_remove(
    share: ShareArg,
    owned: u128,
    preflight: &Preflight,
    dex_id: &DexId,
    pool_id: PoolId,
) -> color_eyre::eyre::Result<u128> {
    let shares = match share {
        ShareArg::All => owned,
        ShareArg::Percent(PercentArg(millionths)) => {
            mul_div_floor(owned, u128::from(millionths), u128::from(FULL_FEE_FRACTION))?
        }
        ShareArg::RawShares(shares) => shares,
    };
    if shares == 0 {
        return Err(PreflightError::ShareTooSmall {
            share: share.to_string(),
        }
        .into_report());
    }
    if shares > owned {
        return Err(PreflightError::NotEnoughShares {
            owner: preflight.signer_id.clone(),
            dex_id: dex_id.clone(),
            pool_id,
            available: owned,
            requested: shares,
        }
        .into_report());
    }
    Ok(shares)
}

#[derive(Clone)]
pub struct RemoveMaxSlippageContext(ActionContext);

impl RemoveMaxSlippageContext {
    pub fn from_previous_context(
        previous_context: RemoveContext,
        scope: &<RemoveMaxSlippage as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let RemoveContext {
            xyk_context,
            signer_id,
            pool_id,
            share,
        } = previous_context;
        let dex_id = xyk_context.dex_id;
        let PercentArg(max_slippage) = scope.percent;
        Ok(Self(planning::write_action_context(
            &xyk_context.global_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                let signer_id = preflight.signer_id.clone();
                let pool = PoolToWrite::read(preflight, &dex_id, pool_id)?;
                let pool_label = format!("pool #{pool_id} ({}) of {dex_id}", pool.pair_label());
                let [(_, reserve_0), (_, reserve_1)] = pool_assets(&pool.pool);
                let (shares, removal_label, removed) = match &pool.pool {
                    PoolView::Launch { .. } => {
                        return Err(PreflightError::PoolKindNotSupported {
                            dex_id: dex_id.clone(),
                            pool_id,
                            kind: "launch",
                            reason: "its liquidity stays in it for good",
                        }
                        .into_report());
                    }
                    PoolView::Private {
                        owner_id, locked, ..
                    } => {
                        if *owner_id != signer_id {
                            return Err(PreflightError::NotPoolOwner {
                                dex_id: dex_id.clone(),
                                pool_id,
                                owner_id: owner_id.clone(),
                                signer_id: signer_id.clone(),
                            }
                            .into_report());
                        }
                        if *locked {
                            return Err(PreflightError::PoolLocked {
                                dex_id: dex_id.clone(),
                                pool_id,
                            }
                            .into_report());
                        }
                        // A private pool counts in shares of the whole pool
                        let shares = shares_to_remove(
                            share,
                            INITIAL_SHARES.get(),
                            preflight,
                            &dex_id,
                            pool_id,
                        )?;
                        (
                            shares,
                            format!(
                                "{} of the liquidity of {pool_label}",
                                display::format_share(shares, INITIAL_SHARES.get())?
                            ),
                            liquidity_for_shares(
                                shares,
                                INITIAL_SHARES.get(),
                                (reserve_0, reserve_1),
                            )?,
                        )
                    }
                    PoolView::Public { total_shares, .. } => {
                        let owned_shares = xyk::pool_shares(
                            &preflight.network_config,
                            &preflight.block_reference,
                            &dex_id,
                            vec![pool_id],
                            &signer_id,
                        )?
                        .first()
                        .copied()
                        .flatten()
                        .map_or(0, |U128(shares)| shares);
                        let (Some(U128(total_shares)), true) = (total_shares, owned_shares > 0)
                        else {
                            return Err(PreflightError::NoShares {
                                owner: signer_id.clone(),
                                dex_id: dex_id.clone(),
                                pool_id,
                            }
                            .into_report());
                        };
                        let shares =
                            shares_to_remove(share, owned_shares, preflight, &dex_id, pool_id)?;
                        (
                            shares,
                            format!(
                                "{shares} shares ({} of the pool) from {pool_label}",
                                display::format_share(shares, *total_shares)?
                            ),
                            liquidity_for_shares(shares, *total_shares, (reserve_0, reserve_1))?,
                        )
                    }
                };
                let minimums = (
                    at_least_with_slippage(removed.0, max_slippage)?,
                    at_least_with_slippage(removed.1, max_slippage)?,
                );
                let mut steps = Vec::new();
                let mut function_calls = Vec::new();
                // A token that can't reach the wallet comes back to the
                // signer's balance on the engine, if the asset is registered
                // there, and is stuck in the dex's balance otherwise
                let registration_calls = preflight.registration_calls(&[
                    (
                        AccountOrDexId::Account(signer_id.clone()),
                        pool.assets[0].0.clone(),
                    ),
                    (
                        AccountOrDexId::Account(signer_id.clone()),
                        pool.assets[1].0.clone(),
                    ),
                ])?;
                steps.extend(registration_calls.steps);
                function_calls.extend(registration_calls.function_calls);
                steps.push(format!(
                    "Remove {removal_label}: about {} and {}, at least {} and {}, to {signer_id}'s wallet",
                    pool.format_amount(0, removed.0),
                    pool.format_amount(1, removed.1),
                    pool.format_amount(0, minimums.0),
                    pool.format_amount(1, minimums.1),
                ));
                for (asset_id, metadata) in &pool.assets {
                    if let intear_dex_types::AssetId::Nep141(token_id) = asset_id
                        && !fungible_token::is_registered(
                            &preflight.network_config,
                            &preflight.block_reference,
                            token_id,
                            &signer_id,
                        )?
                    {
                        steps.push(format!(
                            "{signer_id} has no storage on {token_id}, so its {} goes into its balance on {ENGINE_ACCOUNT_ID} instead",
                            display::asset_label(asset_id, metadata.as_ref())
                        ));
                    }
                }
                let shares_to_remove = match share {
                    ShareArg::All => None,
                    ShareArg::Percent(_) | ShareArg::RawShares(_) => NonZeroU128::new(shares),
                };
                function_calls.push(FunctionCall {
                    method_name: "execute_operations",
                    args: json!({
                        "operations": [planning::dex_call_operation(
                            &dex_id,
                            "remove_liquidity",
                            &RemoveLiquidityArgs {
                                pool_id,
                                shares_to_remove,
                                min_assets_received: Some((U128(minimums.0), U128(minimums.1))),
                            },
                            &[],
                        )?],
                    }),
                    deposit: ONE_YOCTO_NEAR,
                    gas: REMOVE_LIQUIDITY_GAS,
                });
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "liquidity removal",
                        success_message: format!("Removed {removal_label}"),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<RemoveMaxSlippageContext> for ActionContext {
    fn from(item: RemoveMaxSlippageContext) -> Self {
        item.0
    }
}
