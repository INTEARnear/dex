use intear_dex_types::{AccountOrDexId, AssetId};
use near_cli_rs::commands::ActionContext;
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::types::AccountId;
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::engine;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::inputs::account_or_dex_id::AccountOrDexIdArg;
use crate::inputs::asset_ids::AssetIdListArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan, format_near};

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
pub struct AssetsCommands {
    #[interactive_clap(subcommand)]
    action: AssetsAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do with assets?
pub enum AssetsAction {
    #[strum_discriminants(strum(
        message = "status    - Whether assets are registered for an account or a dex"
    ))]
    /// Whether assets are registered for an account or a dex
    Status(Status),
    #[strum_discriminants(strum(
        message = "register  - Register assets so an account or a dex can hold them"
    ))]
    /// Register assets so an account or a dex can hold them
    Register(Register),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = StatusContext)]
pub struct Status {
    #[interactive_clap(skip_default_input_arg)]
    /// An account (alice.near) or a dex (slimedragon.near/xyk)
    owner: AccountOrDexIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// Comma-separated asset ids
    assets: AssetIdListArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl Status {
    fn input_owner(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AccountOrDexIdArg>> {
        crate::inputs::prompt("For whom? An account (alice.near) or a dex (slimedragon.near/xyk)")
    }

    fn input_assets(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AssetIdListArg>> {
        crate::inputs::prompt(
            "Which assets? Comma-separated asset ids, e.g. near,nep141:usdt.tether-token.near",
        )
    }
}

#[derive(Clone)]
pub struct StatusContext(ArgsForViewContext);

impl StatusContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Status as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let owner = scope.owner.clone();
        let AssetIdListArg(asset_ids) = scope.assets.clone();
        let output_format = previous_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let metadata_cache =
                    AssetMetadataCache::new(network_config, block_reference.clone());
                let mut table = display::table(&["Asset", "Registered", "Balance"]);
                let mut statuses_json = Vec::new();
                for asset_id in &asset_ids {
                    let balance = engine::asset_balance_of(
                        network_config,
                        &block_reference,
                        &owner.0,
                        asset_id,
                    )?;
                    match output_format {
                        OutputFormat::Table => {
                            let row = match balance {
                                Some(balance) => prettytable::row![
                                    asset_id,
                                    "yes",
                                    display::format_amount(
                                        balance.0,
                                        metadata_cache.get(asset_id)?.as_ref()
                                    )
                                ],
                                None => prettytable::row![asset_id, "no", ""],
                            };
                            table.add_row(row);
                        }
                        OutputFormat::Json => {
                            let balance = match balance {
                                Some(balance) => Some(display::amount_json(
                                    balance.0,
                                    metadata_cache.get(asset_id)?.as_ref(),
                                )),
                                None => None,
                            };
                            statuses_json.push(json!({
                                "asset_id": asset_id,
                                "registered": balance.is_some(),
                                "balance": balance,
                            }));
                        }
                    }
                }
                match output_format {
                    OutputFormat::Table => print!("{table}"),
                    OutputFormat::Json => display::print_json(&json!({
                        "owner": owner.to_string(),
                        "assets": statuses_json,
                    }))?,
                }
                Ok(())
            });
        Ok(Self(ArgsForViewContext {
            config: previous_context.config,
            interacting_with_account_ids: vec![ENGINE_ACCOUNT_ID.to_owned()],
            on_after_getting_block_reference_callback,
        }))
    }
}

impl From<StatusContext> for ArgsForViewContext {
    fn from(item: StatusContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = RegisterContext)]
pub struct Register {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account pays for the registration?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// Comma-separated asset ids
    assets: AssetIdListArg,
    #[interactive_clap(long = "for")]
    #[interactive_clap(skip_interactive_input)]
    /// The account or dex to register them for, if not the signer
    owner: Option<AccountOrDexIdArg>,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Register {
    fn input_signer_account_id(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Which account pays for the registration?",
        )
    }

    fn input_assets(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AssetIdListArg>> {
        crate::inputs::prompt(
            "Which assets? Comma-separated asset ids, e.g. near,nep141:usdt.tether-token.near",
        )
    }
}

#[derive(Clone)]
pub struct RegisterContext(ActionContext);

impl RegisterContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Register as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let signer_id: AccountId = scope.signer_account_id.clone().into();
        let AssetIdListArg(asset_ids) = scope.assets.clone();
        let owner = scope.owner.clone().map_or_else(
            || AccountOrDexId::Account(signer_id.clone()),
            |AccountOrDexIdArg(owner)| owner,
        );
        Ok(Self(planning::write_action_context(
            &previous_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                match &owner {
                    AccountOrDexId::Account(owner_account_id)
                        if *owner_account_id == preflight.signer_id => {}
                    AccountOrDexId::Account(owner_account_id) => {
                        preflight.ensure_account_exists(owner_account_id)?;
                    }
                    AccountOrDexId::Dex(dex_id) => preflight.ensure_dex_exists(dex_id)?,
                }
                let owner_label = AccountOrDexIdArg(owner.clone()).to_string();
                let mut unregistered_asset_ids: Vec<AssetId> = Vec::new();
                for asset_id in &asset_ids {
                    let balance = engine::asset_balance_of(
                        &preflight.network_config,
                        &preflight.block_reference,
                        &owner,
                        asset_id,
                    )?;
                    if balance.is_none() {
                        unregistered_asset_ids.push(asset_id.clone());
                    }
                }
                if unregistered_asset_ids.is_empty() {
                    return Err(PreflightError::AlreadyRegistered {
                        owner: owner_label,
                        assets: match asset_ids.as_slice() {
                            [asset_id] => format!("{asset_id} is"),
                            _ => format!("{} are", AssetIdListArg(asset_ids.clone())),
                        },
                    }
                    .into_report());
                }
                let registrations = unregistered_asset_ids
                    .iter()
                    .map(|asset_id| (owner.clone(), asset_id.clone()))
                    .collect::<Vec<_>>();
                let registration_storage = preflight.storage_for_registrations(&registrations)?;
                let asset_list = unregistered_asset_ids
                    .iter()
                    .map(AssetId::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let gas = planning::register_assets_gas(unregistered_asset_ids.len())?;
                let register_for = match &owner {
                    AccountOrDexId::Account(owner_account_id)
                        if *owner_account_id == preflight.signer_id =>
                    {
                        None
                    }
                    _ => Some(owner.clone()),
                };
                let mut steps = Vec::new();
                let mut function_calls = Vec::new();
                if let Some(storage_top_up) = &registration_storage.top_up {
                    steps.push(storage_top_up.step(&preflight.signer_id));
                    function_calls.push(storage_top_up.function_call());
                }
                steps.push(format!(
                    "Register {asset_list} for {owner_label}, which takes about {} of {}'s storage balance",
                    format_near(registration_storage.needed),
                    preflight.signer_id
                ));
                function_calls.push(FunctionCall {
                    method_name: "register_assets",
                    args: json!({ "asset_ids": unregistered_asset_ids, "for": register_for }),
                    deposit: ONE_YOCTO_NEAR,
                    gas,
                });
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "registration",
                        success_message: format!(
                            "Registered {asset_list} for {owner_label} on {ENGINE_ACCOUNT_ID}"
                        ),
                        transfer_call: None,
                        dex_id: None,
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<RegisterContext> for ActionContext {
    fn from(item: RegisterContext) -> Self {
        item.0
    }
}
