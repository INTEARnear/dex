use amp_dex_types::{SetAmplificationArgs, SetOracleKeyArgs};
use near_cli_rs::commands::ActionContext;
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::gas::Gas;
use near_sdk::base64::{Engine, prelude::BASE64_STANDARD};
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use super::presentation::{fee_label, pool_assets, pool_json};
use super::{AmpContext, PoolToWrite, amp_pool_id};
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::{amp, engine};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::inputs::amplification::AmplificationArg;
use crate::inputs::oracle_key::OracleKeyArg;
use crate::inputs::pool_id::PoolIdArg;
use crate::outcome::{DexEvents, ExpectedOutcome};
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan, amp_storage, storage};

const SET_AMPLIFICATION_GAS: Gas = Gas::from_teragas(60);
const SET_ORACLE_KEY_GAS: Gas = Gas::from_teragas(60);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = AmpContext)]
pub struct PoolCommands {
    #[interactive_clap(subcommand)]
    action: PoolAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = AmpContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do with the pool?
pub enum PoolAction {
    #[strum_discriminants(strum(
        message = "show               - Reserves, amplification, oracle, shares and fees of a pool"
    ))]
    /// Reserves, amplification, oracle, shares and fees of a pool
    Show(Show),
    #[strum_discriminants(strum(
        message = "set-amplification  - Change how flat the curve of a pool is around the oracle's price"
    ))]
    /// Change how flat the curve of a pool is around the oracle's price
    SetAmplification(SetAmplification),
    #[strum_discriminants(strum(
        message = "set-oracle-key     - Change the oracle that swaps on a pool ask for prices"
    ))]
    /// Change the oracle that swaps on a pool ask for prices
    SetOracleKey(SetOracleKey),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = AmpContext)]
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
    fn input_pool_id(context: &AmpContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
        super::input_pool_id(context)
    }
}

#[derive(Clone)]
pub struct ShowContext(ArgsForViewContext);

impl ShowContext {
    pub fn from_previous_context(
        previous_context: AmpContext,
        scope: &<Show as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let PoolIdArg(pool_id) = scope.pool_id;
        let output_format = previous_context.global_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let pool = amp::pool(network_config, &block_reference, &dex_id, pool_id)?;
                let metadata_cache = AssetMetadataCache::new(network_config, block_reference);
                // What the oracle request operation that swaps start with asks for
                let oracle_request =
                    BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&pool.prices_request())?);
                match output_format {
                    OutputFormat::Table => {
                        let [(asset_id_0, reserve_0), (asset_id_1, reserve_1)] = pool_assets(&pool);
                        let metadata_0 = metadata_cache.get(&asset_id_0)?;
                        let metadata_1 = metadata_cache.get(&asset_id_1)?;
                        let last_oracle_price = match pool.last_oracle_price {
                            Some(price) => format!(
                                "1 {} = {price} {}",
                                display::asset_label(&asset_id_0, metadata_0.as_ref()),
                                display::asset_label(&asset_id_1, metadata_1.as_ref())
                            ),
                            None => "no swaps yet".to_string(),
                        };
                        let rows = vec![
                            ("Pool", format!("#{pool_id} on {dex_id}")),
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
                                "Decimals",
                                format!("{} / {}", pool.decimals.0, pool.decimals.1),
                            ),
                            (
                                "Reserves",
                                format!(
                                    "{} / {}",
                                    display::format_amount(reserve_0, metadata_0.as_ref()),
                                    display::format_amount(reserve_1, metadata_1.as_ref())
                                ),
                            ),
                            ("Amplification", pool.amplification.to_string()),
                            ("Oracle key", pool.oracle_key.to_string()),
                            ("Oracle request", oracle_request.clone()),
                            ("Last oracle price", last_oracle_price),
                            (
                                "Total shares",
                                if pool.total_shares.is_none() {
                                    "none".to_string()
                                } else {
                                    pool.total_shares.get().to_string()
                                },
                            ),
                            ("Fee", fee_label(&pool)),
                        ];
                        print!("{}", display::key_value_table(rows));
                    }
                    OutputFormat::Json => {
                        let mut pool_json = pool_json(pool_id, &pool, &metadata_cache)?;
                        pool_json["dex_id"] = serde_json::json!(dex_id);
                        pool_json["oracle_request"] = serde_json::json!(oracle_request);
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

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = AmpContext)]
#[interactive_clap(output_context = SetAmplificationContext)]
pub struct SetAmplification {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account, the owner of the dex, changes the amplification?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The pool id
    pool_id: PoolIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// The new amplification, from 1 to 1000000, e.g. 100
    amplification: AmplificationArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl SetAmplification {
    fn input_signer_account_id(
        context: &AmpContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        super::input_dex_owner(context, "Which account changes the amplification?")
    }

    fn input_pool_id(context: &AmpContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
        super::input_pool_id(context)
    }

    fn input_amplification(
        _context: &AmpContext,
    ) -> color_eyre::eyre::Result<Option<AmplificationArg>> {
        crate::inputs::prompt(
            "The new amplification? From 1 to 1000000; the higher, the less the pool's imbalance moves its price away from the oracle's. e.g. 100",
        )
    }
}

#[derive(Clone)]
pub struct SetAmplificationContext(ActionContext);

impl SetAmplificationContext {
    pub fn from_previous_context(
        previous_context: AmpContext,
        scope: &<SetAmplification as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let PoolIdArg(pool_id) = scope.pool_id;
        let AmplificationArg(amplification) = scope.amplification;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                super::ensure_dex_owner(
                    preflight,
                    &dex_id,
                    "change the amplification of its pools",
                )?;
                let pool = PoolToWrite::read(preflight, &dex_id, pool_id)?;
                if pool.pool.amplification == amplification {
                    return Err(PreflightError::PoolSettingUnchanged {
                        dex_id: dex_id.clone(),
                        pool_id,
                        setting: format!("amplification {amplification}"),
                    }
                    .into_report());
                }
                let pool_label = format!("pool #{pool_id} ({}) of {dex_id}", pool.pair_label());
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: vec![format!(
                        "Change the amplification of {pool_label} from {} to {amplification}",
                        pool.pool.amplification
                    )],
                    function_calls: vec![FunctionCall {
                        method_name: "execute_operations",
                        args: serde_json::json!({
                            "operations": [planning::dex_call_operation(
                                &dex_id,
                                "set_amplification",
                                &SetAmplificationArgs {
                                    pool_id: amp_pool_id(pool_id),
                                    amplification,
                                },
                                &[],
                            )?],
                        })
                        .into(),
                        deposit: ONE_YOCTO_NEAR,
                        gas: SET_AMPLIFICATION_GAS,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "amplification change",
                        success_message: format!(
                            "Changed the amplification of {pool_label} to {amplification}"
                        ),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        dex_events: DexEvents::Amp,
                        deployment: None,
                    },
                    deploy_code: None,
                })
            },
        )))
    }
}

impl From<SetAmplificationContext> for ActionContext {
    fn from(item: SetAmplificationContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = AmpContext)]
#[interactive_clap(output_context = SetOracleKeyContext)]
pub struct SetOracleKey {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account, the owner of the dex, changes the oracle?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The pool id
    pool_id: PoolIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// The key of the new oracle on dex.intear.near, which create_oracle_key added, e.g. 'ed25519:…'
    oracle_key: OracleKeyArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl SetOracleKey {
    fn input_signer_account_id(
        context: &AmpContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        super::input_dex_owner(context, "Which account changes the oracle?")
    }

    fn input_pool_id(context: &AmpContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
        super::input_pool_id(context)
    }

    fn input_oracle_key(_context: &AmpContext) -> color_eyre::eyre::Result<Option<OracleKeyArg>> {
        crate::inputs::prompt(
            "The key of the new oracle on dex.intear.near, which create_oracle_key added? e.g. ed25519:…",
        )
    }
}

#[derive(Clone)]
pub struct SetOracleKeyContext(ActionContext);

impl SetOracleKeyContext {
    pub fn from_previous_context(
        previous_context: AmpContext,
        scope: &<SetOracleKey as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let PoolIdArg(pool_id) = scope.pool_id;
        let OracleKeyArg(oracle_key) = scope.oracle_key.clone();
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                super::ensure_dex_owner(preflight, &dex_id, "change the oracle of its pools")?;
                let pool = PoolToWrite::read(preflight, &dex_id, pool_id)?;
                if pool.pool.oracle_key == oracle_key {
                    return Err(PreflightError::PoolSettingUnchanged {
                        dex_id: dex_id.clone(),
                        pool_id,
                        setting: format!("the oracle {oracle_key}"),
                    }
                    .into_report());
                }
                let storage_near = storage::storage_cost(
                    amp_storage::oracle_key_growth_bytes(&pool.pool.oracle_key, &oracle_key)?,
                    preflight.protocol_limits.storage_byte_cost,
                )?;
                let pool_label = format!("pool #{pool_id} ({}) of {dex_id}", pool.pair_label());
                let (steps, function_calls) = crate::commands::xyk::dex_call_paying_for_storage(
                    preflight,
                    &dex_id,
                    format!(
                        "Change the oracle that swaps on {pool_label} ask for prices from {} to {oracle_key}",
                        pool.pool.oracle_key
                    ),
                    (
                        "set_oracle_key",
                        &SetOracleKeyArgs {
                            pool_id: amp_pool_id(pool_id),
                            oracle_key: oracle_key.clone(),
                        },
                    ),
                    storage_near,
                    SET_ORACLE_KEY_GAS,
                )?;
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "oracle change",
                        success_message: format!(
                            "Swaps on {pool_label} now ask {oracle_key} for prices"
                        ),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        dex_events: DexEvents::Amp,
                        deployment: None,
                    },
                    deploy_code: None,
                })
            },
        )))
    }
}

impl From<SetOracleKeyContext> for ActionContext {
    fn from(item: SetOracleKeyContext) -> Self {
        item.0
    }
}
