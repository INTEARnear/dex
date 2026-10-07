use intear_dex_types::DexId;
use near_cli_rs::commands::ActionContext;
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use xyk_dex_types::{
    EditFeesArgs, FeeConfiguration, FeeReceiver, LockPoolArgs, PoolId, PoolType, PoolView,
    UpgradePoolArgs,
};

use super::presentation::{
    fee_receiver_label, format_fee_configuration, pool_assets, pool_fees, pool_json, pool_kind,
    total_fee_fraction,
};
use super::{PoolToWrite, XykContext};
use crate::chain::asset_metadata::{AssetMetadata, AssetMetadataCache};
use crate::chain::{engine, xyk};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::{self, PreflightError};
use crate::inputs::fees::FeesArg;
use crate::inputs::pool_id::PoolIdArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{
    self, FunctionCall, ONE_YOCTO_NEAR, Preflight, TransactionPlan, storage, xyk_storage,
};

const UPGRADE_POOL_GAS: Gas = Gas::from_teragas(100);
const LOCK_POOL_GAS: Gas = Gas::from_teragas(60);
const EDIT_FEES_GAS: Gas = Gas::from_teragas(100);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
pub struct PoolCommands {
    #[interactive_clap(subcommand)]
    action: PoolAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do with the pool?
pub enum PoolAction {
    #[strum_discriminants(strum(
        message = "show       - Reserves, owner or shares, fees and fee schedule of a pool"
    ))]
    /// Reserves, owner or shares, fees and fee schedule of a pool
    Show(Show),
    #[strum_discriminants(strum(
        message = "upgrade    - Upgrade a pool of an old version, which anyone can"
    ))]
    /// Upgrade a pool of an old version, which anyone can
    Upgrade(Upgrade),
    #[strum_discriminants(strum(
        message = "lock       - Lock a private pool's liquidity and fees for good"
    ))]
    /// Lock a private pool's liquidity and fees for good
    Lock(Lock),
    #[strum_discriminants(strum(message = "edit-fees  - Change the fees of a private pool"))]
    /// Change the fees of a private pool
    EditFees(EditFees),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = ShowContext)]
pub struct Show {
    #[interactive_clap(skip_default_input_arg)]
    /// The pool id
    pool_id: PoolIdArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl Show {
    fn input_pool_id(context: &XykContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
        super::input_pool_id(context)
    }
}

#[derive(Clone)]
pub struct ShowContext(ArgsForViewContext);

impl ShowContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Show as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let PoolIdArg(pool_id) = scope.pool_id;
        let output_format = previous_context.global_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let pool = xyk::pool(network_config, &block_reference, &dex_id, pool_id)?;
                let upgrade_available =
                    xyk::pool_needs_upgrade(network_config, &block_reference, &dex_id, pool_id)?;
                let metadata_cache = AssetMetadataCache::new(network_config, block_reference);
                match output_format {
                    OutputFormat::Table => {
                        let [(asset_id_0, reserve_0), (asset_id_1, reserve_1)] = pool_assets(&pool);
                        let metadata_0 = metadata_cache.get(&asset_id_0)?;
                        let metadata_1 = metadata_cache.get(&asset_id_1)?;
                        let (fees, fee_configuration) = pool_fees(&pool);
                        let fees_now = fees
                            .receivers
                            .iter()
                            .map(|(receiver, fee)| {
                                format!(
                                    "{} {}",
                                    fee_receiver_label(receiver),
                                    display::format_fee(*fee)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        let mut rows = vec![
                            ("Pool", format!("#{pool_id} on {dex_id}")),
                            ("Kind", pool_kind(&pool).to_string()),
                            (
                                "Pair",
                                format!(
                                    "{} / {}",
                                    display::asset_label(&asset_id_0, metadata_0.as_ref()),
                                    display::asset_label(&asset_id_1, metadata_1.as_ref())
                                ),
                            ),
                            ("Assets", format!("{asset_id_0} / {asset_id_1}")),
                            (
                                "Reserves",
                                format!(
                                    "{} / {}",
                                    display::format_amount(reserve_0, metadata_0.as_ref()),
                                    display::format_amount(reserve_1, metadata_1.as_ref())
                                ),
                            ),
                        ];
                        match &pool {
                            PoolView::Private {
                                owner_id, locked, ..
                            } => {
                                rows.push(("Owner", owner_id.to_string()));
                                rows.push((
                                    "Locked",
                                    if *locked { "yes" } else { "no" }.to_string(),
                                ));
                            }
                            PoolView::Public { total_shares, .. } => rows.push((
                                "Total shares",
                                total_shares.map_or("none".to_string(), |total_shares| {
                                    total_shares.0.to_string()
                                }),
                            )),
                            PoolView::Launch {
                                phantom_liquidity_near,
                                ..
                            } => rows.push((
                                "Phantom liquidity",
                                display::format_amount(
                                    phantom_liquidity_near.0,
                                    Some(&AssetMetadata::near()),
                                ),
                            )),
                            PoolView::LaunchV2 {
                                phantom_liquidity, ..
                            } => rows.push((
                                "Phantom liquidity",
                                display::format_amount(phantom_liquidity.0, metadata_0.as_ref()),
                            )),
                        }
                        rows.push((
                            "Fees now",
                            format!(
                                "{} ({fees_now})",
                                display::format_fee(total_fee_fraction(fees)?)
                            ),
                        ));
                        rows.push((
                            "Fee configuration",
                            format_fee_configuration(fee_configuration),
                        ));
                        rows.push((
                            "Upgrade available",
                            if upgrade_available { "yes" } else { "no" }.to_string(),
                        ));
                        print!("{}", display::key_value_table(rows));
                    }
                    OutputFormat::Json => {
                        let mut pool_json = pool_json(pool_id, &pool, &metadata_cache)?;
                        pool_json["dex_id"] = serde_json::json!(dex_id);
                        pool_json["upgrade_available"] = serde_json::json!(upgrade_available);
                        display::print_json(&pool_json)?;
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

fn input_pool_changer(
    context: &XykContext,
    question: &str,
) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
    near_cli_rs::common::input_signer_account_id_from_used_account_list(
        &context.global_context.config.credentials_home_dir,
        question,
    )
}

fn upgrade_command(preflight: &Preflight, dex_id: &DexId, pool_id: PoolId) -> String {
    shell_words::join([
        "near",
        "intear-dex-management",
        "xyk",
        "--dex",
        &dex_id.to_string(),
        "pool",
        "upgrade",
        preflight.signer_id.as_str(),
        &pool_id.to_string(),
        "network-config",
        &preflight.connection_name,
    ])
}

fn ensure_pool_owner(
    owner_id: &AccountId,
    preflight: &Preflight,
    dex_id: &DexId,
    pool_id: PoolId,
) -> color_eyre::eyre::Result<()> {
    if *owner_id != preflight.signer_id {
        return Err(PreflightError::NotPoolOwner {
            dex_id: dex_id.clone(),
            pool_id,
            owner_id: owner_id.clone(),
            signer_id: preflight.signer_id.clone(),
        }
        .into_report());
    }
    Ok(())
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = UpgradeContext)]
pub struct Upgrade {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account upgrades the pool and pays for the storage that takes?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The pool id
    pool_id: PoolIdArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Upgrade {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        input_pool_changer(
            context,
            "Which account upgrades the pool and pays for the storage that takes?",
        )
    }

    fn input_pool_id(context: &XykContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
        super::input_pool_id(context)
    }
}

#[derive(Clone)]
pub struct UpgradeContext(ActionContext);

impl UpgradeContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Upgrade as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let PoolIdArg(pool_id) = scope.pool_id;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                let pool = PoolToWrite::read(preflight, &dex_id, pool_id)?;
                if !xyk::pool_needs_upgrade(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                    pool_id,
                )? {
                    return Err(PreflightError::PoolIsLatest {
                        dex_id: dex_id.clone(),
                        pool_id,
                    }
                    .into_report());
                }
                let pool_label = format!("pool #{pool_id} ({}) of {dex_id}", pool.pair_label());
                let is_private = matches!(pool.pool, PoolView::Private { .. });
                let storage_near = storage::storage_cost(
                    xyk_storage::pool_upgrade_growth_bytes(is_private),
                    preflight.protocol_limits.storage_byte_cost,
                )?;
                let upgrade_step = if is_private {
                    format!(
                        "Upgrade {pool_label} to the latest version, which takes fee schedules and can be locked"
                    )
                } else {
                    format!("Upgrade {pool_label} to the latest version")
                };
                let (steps, function_calls) = super::dex_call_paying_for_storage(
                    preflight,
                    &dex_id,
                    upgrade_step,
                    ("upgrade_pool", &UpgradePoolArgs { pool_id }),
                    storage_near,
                    UPGRADE_POOL_GAS,
                )?;
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "pool upgrade",
                        success_message: format!("Upgraded {pool_label} to the latest version"),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<UpgradeContext> for ActionContext {
    fn from(item: UpgradeContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = LockContext)]
pub struct Lock {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account, the owner of the pool, locks it?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The pool id
    pool_id: PoolIdArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Lock {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        input_pool_changer(context, "Which account, the owner of the pool, locks it?")
    }

    fn input_pool_id(context: &XykContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
        super::input_pool_id(context)
    }
}

#[derive(Clone)]
pub struct LockContext(ActionContext);

impl LockContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Lock as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let PoolIdArg(pool_id) = scope.pool_id;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                let pool = PoolToWrite::read(preflight, &dex_id, pool_id)?;
                match &pool.pool {
                    PoolView::Public { .. } => {
                        return Err(PreflightError::PoolKindNotSupported {
                            dex_id: dex_id.clone(),
                            pool_id,
                            kind: "public",
                            reason: "only private pools can be locked",
                        }
                        .into_report());
                    }
                    PoolView::Launch { .. } | PoolView::LaunchV2 { .. } => {
                        return Err(PreflightError::PoolKindNotSupported {
                            dex_id: dex_id.clone(),
                            pool_id,
                            kind: "launch",
                            reason: "its liquidity and fees are locked from the start",
                        }
                        .into_report());
                    }
                    PoolView::Private {
                        owner_id, locked, ..
                    } => {
                        ensure_pool_owner(owner_id, preflight, &dex_id, pool_id)?;
                        if *locked {
                            return Err(PreflightError::PoolAlreadyLocked {
                                dex_id: dex_id.clone(),
                                pool_id,
                            }
                            .into_report());
                        }
                        if xyk::pool_needs_upgrade(
                            &preflight.network_config,
                            &preflight.block_reference,
                            &dex_id,
                            pool_id,
                        )? {
                            return Err(PreflightError::PoolNeedsUpgrade {
                                dex_id: dex_id.clone(),
                                pool_id,
                                reason: "can't be locked",
                                upgrade_command: upgrade_command(preflight, &dex_id, pool_id),
                            }
                            .into_report());
                        }
                    }
                }
                let pool_label = format!("pool #{pool_id} ({}) of {dex_id}", pool.pair_label());
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: vec![format!(
                        "Lock {pool_label} for good: from then on its liquidity can't be removed and its fees can't change, and it can't be unlocked"
                    )],
                    function_calls: vec![FunctionCall {
                        method_name: "execute_operations",
                        args: serde_json::json!({
                            "operations": [planning::dex_call_operation(
                                &dex_id,
                                "lock_pool",
                                &LockPoolArgs { pool_id },
                                &[],
                            )?],
                        }),
                        deposit: ONE_YOCTO_NEAR,
                        gas: LOCK_POOL_GAS,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "pool lock",
                        success_message: format!("Locked {pool_label}"),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<LockContext> for ActionContext {
    fn from(item: LockContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = EditFeesContext)]
pub struct EditFees {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account, the owner of the pool, changes its fees?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The pool id
    pool_id: PoolIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// The new fees, e.g. 'alice.near=0.25%,pool=0.05%', a schedule like 'pool=1%..0.3%@now..now+7d', or none
    fees: FeesArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl EditFees {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        input_pool_changer(
            context,
            "Which account, the owner of the pool, changes its fees?",
        )
    }

    fn input_pool_id(context: &XykContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
        super::input_pool_id(context)
    }

    fn input_fees(_context: &XykContext) -> color_eyre::eyre::Result<Option<FeesArg>> {
        crate::inputs::prompt(
            "The new fees? e.g. 'alice.near=0.25%,pool=0.05%', 'pool=1%..0.3%@now..now+7d', or none",
        )
    }
}

#[derive(Clone)]
pub struct EditFeesContext(ActionContext);

impl EditFeesContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<EditFees as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let PoolIdArg(pool_id) = scope.pool_id;
        let fees = scope.fees.clone();
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                let pool = PoolToWrite::read(preflight, &dex_id, pool_id)?;
                let (_, current_fee_configuration) = pool_fees(&pool.pool);
                match &pool.pool {
                    PoolView::Public { .. }
                    | PoolView::Launch { .. }
                    | PoolView::LaunchV2 { .. } => {
                        return Err(PreflightError::PoolKindNotSupported {
                            dex_id: dex_id.clone(),
                            pool_id,
                            kind: pool_kind(&pool.pool),
                            reason: "its fees never change",
                        }
                        .into_report());
                    }
                    PoolView::Private {
                        owner_id, locked, ..
                    } => {
                        ensure_pool_owner(owner_id, preflight, &dex_id, pool_id)?;
                        if *locked {
                            return Err(PreflightError::PoolLocked {
                                dex_id: dex_id.clone(),
                                pool_id,
                            }
                            .into_report());
                        }
                    }
                }
                let now = preflight.block_timestamp_nanoseconds;
                let new_fee_configuration = fees
                    .fee_configuration(now)
                    .map_err(|problem| PreflightError::FeesNotAllowed { problem }.into_report())?;
                let needs_upgrade = xyk::pool_needs_upgrade(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                    pool_id,
                )?;
                if needs_upgrade && matches!(new_fee_configuration, FeeConfiguration::V2(_)) {
                    return Err(PreflightError::PoolNeedsUpgrade {
                        dex_id: dex_id.clone(),
                        pool_id,
                        reason: "only takes fixed fees",
                        upgrade_command: upgrade_command(preflight, &dex_id, pool_id),
                    }
                    .into_report());
                }
                let pool_type = if needs_upgrade {
                    PoolType::PrivateV1
                } else {
                    PoolType::PrivateLatest
                };
                new_fee_configuration
                    .validate(&pool_type, now)
                    .map_err(|error| {
                        PreflightError::FeesNotAllowed {
                            problem: errors::fee_configuration_problem(error),
                        }
                        .into_report()
                    })?;
                // The new fees start a fee balance in both assets for every
                // account receiver that has none yet
                let mut fee_accounts: Vec<AccountId> = Vec::new();
                for (receiver, _) in new_fee_configuration.receivers_at(now) {
                    if let FeeReceiver::Account(account_id) = receiver
                        && !fee_accounts.contains(&account_id)
                    {
                        fee_accounts.push(account_id);
                    }
                }
                let [(asset_id_0, _), (asset_id_1, _)] = &pool.assets;
                let storage_bytes = super::new_fee_balances_bytes(
                    preflight,
                    &dex_id,
                    &fee_accounts,
                    (asset_id_0, asset_id_1),
                )?
                .checked_add(xyk_storage::fee_configuration_growth_bytes(
                    current_fee_configuration,
                    &new_fee_configuration,
                )?)
                .ok_or_else(|| color_eyre::eyre::eyre!("Storage size overflow"))?;
                let storage_near = storage::storage_cost(
                    storage_bytes,
                    preflight.protocol_limits.storage_byte_cost,
                )?;
                let pool_label = format!("pool #{pool_id} ({}) of {dex_id}", pool.pair_label());
                let (steps, function_calls) = super::dex_call_paying_for_storage(
                    preflight,
                    &dex_id,
                    format!(
                        "Change the fees of {pool_label} from {} to {}, plus the protocol fee",
                        format_fee_configuration(current_fee_configuration),
                        format_fee_configuration(&new_fee_configuration)
                    ),
                    (
                        "edit_fees",
                        &EditFeesArgs {
                            pool_id,
                            fees: new_fee_configuration.clone(),
                        },
                    ),
                    storage_near,
                    EDIT_FEES_GAS,
                )?;
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "fee change",
                        success_message: format!("Changed the fees of {pool_label}"),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<EditFeesContext> for ActionContext {
    fn from(item: EditFeesContext) -> Self {
        item.0
    }
}
