use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::json_types::Base64VecU8;
use serde_json::json;
use xyk_dex_types::CAN_MIGRATE;

use super::XykContext;
use crate::chain::xyk::{self, XykState};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::errors::PreflightError;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan, xyk_storage};

const INIT_GAS: Gas = Gas::from_teragas(50);
const MIGRATE_GAS: Gas = Gas::from_teragas(50);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = InitContext)]
pub struct Init {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account initializes the dex? Any account can
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Init {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account initializes the dex? Any account can",
        )
    }
}

#[derive(Clone)]
pub struct InitContext(ActionContext);

impl InitContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Init as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let signer_id: AccountId = scope.signer_account_id.clone().into();
        let dex_id = previous_context.dex_id;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                preflight.ensure_dex_exists(&dex_id)?;
                match xyk::state(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                )? {
                    XykState::Missing => {}
                    XykState::Readable | XykState::Unreadable => {
                        return Err(PreflightError::XykAlreadyInitialized {
                            dex_id: dex_id.clone(),
                        }
                        .into_report());
                    }
                }
                let mut steps = Vec::new();
                let mut function_calls = Vec::new();
                // The engine charges the dex's storage balance for its state
                let state_bytes = xyk_storage::initial_state_bytes(
                    &dex_id,
                    preflight.protocol_limits.extra_bytes_per_record,
                )?;
                if let Some(storage_top_up) = preflight.dex_storage_top_up(&dex_id, state_bytes)? {
                    steps.push(storage_top_up.step("the state"));
                    function_calls.push(storage_top_up.function_call());
                }
                steps.push(format!(
                    "Initialize {dex_id} with no pools, so that pools can be created on it"
                ));
                function_calls.push(FunctionCall {
                    method_name: "dex_call",
                    args: json!({
                        "dex_id": dex_id,
                        "method": "new",
                        "args": Base64VecU8(Vec::new()),
                        "attached_assets": {},
                        "referrer": null,
                    }),
                    deposit: ONE_YOCTO_NEAR,
                    gas: INIT_GAS,
                });
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "initialization",
                        success_message: format!("Initialized {dex_id}"),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<InitContext> for ActionContext {
    fn from(item: InitContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = MigrateContext)]
pub struct Migrate {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account migrates? Only slimedragon.near can
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Migrate {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account migrates? Only slimedragon.near can",
        )
    }
}

#[derive(Clone)]
pub struct MigrateContext(ActionContext);

impl MigrateContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Migrate as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let signer_id: AccountId = scope.signer_account_id.clone().into();
        let dex_id = previous_context.dex_id;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                if preflight.signer_id != CAN_MIGRATE {
                    return Err(PreflightError::NotAllowedToMigrate {
                        signer_id: preflight.signer_id.clone(),
                    }
                    .into_report());
                }
                preflight.ensure_dex_exists(&dex_id)?;
                match xyk::state(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                )? {
                    XykState::Unreadable => {}
                    XykState::Readable => {
                        return Err(PreflightError::XykStateCurrent {
                            dex_id: dex_id.clone(),
                        }
                        .into_report());
                    }
                    XykState::Missing => {
                        return Err(PreflightError::XykNotInitialized {
                            dex_id: dex_id.clone(),
                            init_command: shell_words::join([
                                "near",
                                "intear-dex-management",
                                "xyk",
                                "--dex",
                                &dex_id.to_string(),
                                "init",
                                preflight.signer_id.as_str(),
                                "network-config",
                                &preflight.connection_name,
                            ]),
                        }
                        .into_report());
                    }
                }
                let mut steps = Vec::new();
                let mut function_calls = Vec::new();
                if let Some(storage_top_up) =
                    preflight.dex_storage_top_up(&dex_id, xyk_storage::migration_growth_bytes())?
                {
                    steps.push(storage_top_up.step("the migrated state"));
                    function_calls.push(storage_top_up.function_call());
                }
                steps.push(format!(
                    "Migrate the state of {dex_id} to the layout of its code"
                ));
                function_calls.push(FunctionCall {
                    method_name: "dex_call",
                    args: json!({
                        "dex_id": dex_id,
                        "method": "migrate",
                        "args": Base64VecU8(Vec::new()),
                        "attached_assets": {},
                        "referrer": null,
                    }),
                    deposit: ONE_YOCTO_NEAR,
                    gas: MIGRATE_GAS,
                });
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "migration",
                        success_message: format!("Migrated the state of {dex_id}"),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<MigrateContext> for ActionContext {
    fn from(item: MigrateContext) -> Self {
        item.0
    }
}
