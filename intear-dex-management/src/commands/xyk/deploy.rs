use color_eyre::Section;
use color_eyre::eyre::eyre;
use intear_dex_types::DexId;
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::json_types::Base64VecU8;
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use tracing_indicatif::span_ext::IndicatifSpanExt;
use xyk_dex_types::{CAN_MIGRATE, MigrateArgs};

use super::XykContext;
use crate::chain::engine;
use crate::chain::xyk::{self, XykState};
use crate::commands::dex_code;
use crate::deployment::{DEFAULT_XYK_DEX_NAME, ENGINE_ACCOUNT_ID};
use crate::errors::PreflightError;
use crate::outcome::{ExpectedDeployment, ExpectedOutcome};
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan};

const MIGRATE_GAS: Gas = Gas::from_teragas(50);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = DeployContext)]
#[interactive_clap(skip_default_from_cli)]
pub struct Deploy {
    #[interactive_clap(skip_default_input_arg)]
    /// The trusted code deployer of dex.intear.near, which the dex id starts with
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// The name of the dex after <signer>/ (default: xyk)
    name: Option<String>,
    #[interactive_clap(long)]
    #[interactive_clap(skip_default_input_arg)]
    /// Migrate the dex's state to the new code's layout in the same receipt, so that a failed migration reverts the deploy
    migrate: bool,
    #[interactive_clap(subcommand)]
    code: DeployCode,
}

impl Deploy {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account deploys? It has to be the engine's trusted code deployer",
        )
    }

    fn input_migrate(
        context: &XykContext,
        dex_id: &DexId,
    ) -> color_eyre::eyre::Result<Option<bool>> {
        let networks_with_state = networks_where_dex_has_state(context, dex_id)?;
        if networks_with_state.is_empty() {
            return Ok(Some(false));
        }
        match inquire::Confirm::new(&format!(
            "{dex_id} has state on {}. Migrate it to the new code's layout in the same receipt as the deploy?",
            networks_with_state.join(" and ")
        ))
        .with_default(false)
        .prompt()
        {
            Ok(migrates) => Ok(Some(migrates)),
            Err(
                inquire::error::InquireError::OperationCanceled
                | inquire::error::InquireError::OperationInterrupted,
            ) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

/// What the derive generates, with the migration asked for between the flags
/// and the code: only when the command doesn't name the code, so that a whole
/// command never stops for it, and only for a dex that has state to migrate
impl interactive_clap::FromCli for Deploy {
    type FromCliContext = XykContext;
    type FromCliError = color_eyre::eyre::Error;

    fn from_cli(
        optional_clap_variant: Option<<Self as interactive_clap::ToCli>::CliVariant>,
        context: Self::FromCliContext,
    ) -> interactive_clap::ResultFromCli<
        <Self as interactive_clap::ToCli>::CliVariant,
        Self::FromCliError,
    >
    where
        Self: Sized + interactive_clap::ToCli,
    {
        let mut clap_variant = optional_clap_variant.unwrap_or_default();
        if clap_variant.signer_account_id.is_none() {
            clap_variant.signer_account_id = match Self::input_signer_account_id(&context) {
                Ok(Some(signer_account_id)) => Some(signer_account_id),
                Ok(None) => return interactive_clap::ResultFromCli::Cancel(Some(clap_variant)),
                Err(err) => return interactive_clap::ResultFromCli::Err(Some(clap_variant), err),
            };
        }
        let Some(signer_account_id) = clap_variant.signer_account_id.clone() else {
            return interactive_clap::ResultFromCli::Cancel(Some(clap_variant));
        };
        let dex_id =
            match deployed_dex_id(&context, &signer_account_id, clap_variant.name.as_deref()) {
                Ok(dex_id) => dex_id,
                Err(err) => return interactive_clap::ResultFromCli::Err(Some(clap_variant), err),
            };
        if !clap_variant.migrate && clap_variant.code.is_none() {
            clap_variant.migrate = match Self::input_migrate(&context, &dex_id) {
                Ok(Some(migrates)) => migrates,
                Ok(None) => return interactive_clap::ResultFromCli::Cancel(Some(clap_variant)),
                Err(err) => return interactive_clap::ResultFromCli::Err(Some(clap_variant), err),
            };
        }
        let new_context_scope = InteractiveClapContextScopeForDeploy {
            signer_account_id,
            name: clap_variant.name.clone(),
            migrate: clap_variant.migrate,
        };
        let context = match DeployContext::from_previous_context(context, &new_context_scope) {
            Ok(context) => context,
            Err(err) => return interactive_clap::ResultFromCli::Err(Some(clap_variant), err),
        };
        match <DeployCode as interactive_clap::FromCli>::from_cli(clap_variant.code.take(), context)
        {
            interactive_clap::ResultFromCli::Ok(code) => {
                clap_variant.code = Some(code);
            }
            interactive_clap::ResultFromCli::Cancel(code) => {
                clap_variant.code = code;
                return interactive_clap::ResultFromCli::Cancel(Some(clap_variant));
            }
            interactive_clap::ResultFromCli::Back => return interactive_clap::ResultFromCli::Back,
            interactive_clap::ResultFromCli::Err(code, err) => {
                clap_variant.code = code;
                return interactive_clap::ResultFromCli::Err(Some(clap_variant), err);
            }
        }
        interactive_clap::ResultFromCli::Ok(clap_variant)
    }
}

/// The names of the networks where `dex_id` has code and state
#[tracing::instrument(name = "Looking for the state of", skip_all)]
fn networks_where_dex_has_state(
    context: &XykContext,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<Vec<String>> {
    tracing::Span::current().pb_set_message(&dex_id.to_string());
    let has_state_by_network = crate::chain::networks::read_on_every_network(
        &context.global_context.config,
        |network_config, block_reference| {
            Ok(engine::dex_exists(network_config, block_reference, dex_id)?
                && !matches!(
                    xyk::state(network_config, block_reference, dex_id)?,
                    XykState::Missing
                ))
        },
    )?;
    Ok(has_state_by_network
        .into_iter()
        .filter_map(|(network_name, has_state)| has_state.then_some(network_name))
        .collect())
}

/// The dex that deploy creates or replaces: <signer>/<name>
fn deployed_dex_id(
    context: &XykContext,
    signer_account_id: &near_cli_rs::types::account_id::AccountId,
    name: Option<&str>,
) -> color_eyre::eyre::Result<DexId> {
    if context.is_dex_chosen {
        return Err(
            eyre!("xyk deploy creates <signer>/<name>, so it doesn't take --dex")
                .suggestion("Name the dex with --name after deploy"),
        );
    }
    dex_code::deployed_dex_id(signer_account_id, name.unwrap_or(DEFAULT_XYK_DEX_NAME))
}

#[derive(Clone)]
pub struct DeployContext {
    global_context: crate::GlobalContext,
    dex_id: DexId,
    migrates: bool,
    migrate_args: Vec<u8>,
}

impl DeployContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Deploy as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = deployed_dex_id(
            &previous_context,
            &scope.signer_account_id,
            scope.name.as_deref(),
        )?;
        let migrate_args = if scope.migrate {
            near_sdk::borsh::to_vec(&MigrateArgs)?
        } else {
            Vec::new()
        };
        Ok(Self {
            global_context: previous_context.global_context,
            dex_id,
            migrates: scope.migrate,
            migrate_args,
        })
    }
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = DeployContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// Where does the code come from?
pub enum DeployCode {
    #[strum_discriminants(strum(
        message = "build     - Build xyk-dex in the checkout you're in, with cargo and wasm-opt"
    ))]
    /// Build xyk-dex in the checkout you're in, with cargo and wasm-opt
    Build(Build),
    #[strum_discriminants(strum(message = "use-file  - Deploy a wasm file that's already built"))]
    /// Deploy a wasm file that's already built
    UseFile(UseFile),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = DeployContext)]
#[interactive_clap(output_context = BuildContext)]
pub struct Build {
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

#[derive(Clone)]
pub struct BuildContext(ActionContext);

impl BuildContext {
    pub fn from_previous_context(
        previous_context: DeployContext,
        _scope: &<Build as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let code = dex_code::build_dex_package("xyk-dex")?;
        Ok(Self(deploy_action_context(
            previous_context,
            code,
            vec!["build".to_string()],
        )))
    }
}

impl From<BuildContext> for ActionContext {
    fn from(item: BuildContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = DeployContext)]
#[interactive_clap(output_context = UseFileContext)]
pub struct UseFile {
    /// The wasm file
    file: near_cli_rs::types::path_buf::PathBuf,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

#[derive(Clone)]
pub struct UseFileContext(ActionContext);

impl UseFileContext {
    pub fn from_previous_context(
        previous_context: DeployContext,
        scope: &<UseFile as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let code = dex_code::read_wasm_file(&scope.file)?;
        Ok(Self(deploy_action_context(
            previous_context,
            code,
            vec!["use-file".to_string(), scope.file.to_string()],
        )))
    }
}

impl From<UseFileContext> for ActionContext {
    fn from(item: UseFileContext) -> Self {
        item.0
    }
}

/// `code_source_args` are the words of the command that pick the code, for
/// the command that deploys it again
fn deploy_action_context(
    deploy_context: DeployContext,
    code: Vec<u8>,
    code_source_args: Vec<String>,
) -> ActionContext {
    let DeployContext {
        global_context,
        dex_id,
        migrates,
        migrate_args,
    } = deploy_context;
    let signer_id: AccountId = dex_id.deployer.clone();
    planning::write_action_context(&global_context, signer_id, move |preflight| {
        preflight.ensure_engine_not_paused()?;
        // Initializing stores the state, and migrating drops the map of
        // referral settings from it, so it only frees storage
        let dex_code::CodeDeployment {
            is_upgrade,
            code_hash,
            code_label,
            mut steps,
            mut function_calls,
        } = dex_code::plan_code_deployment(preflight, &dex_id, &code, 0)?;
        let init_command = shell_words::join([
            "near",
            "intear-dex-management",
            "xyk",
            "--dex",
            &dex_id.to_string(),
            "init",
            preflight.signer_id.as_str(),
            "network-config",
            &preflight.connection_name,
        ]);
        if migrates {
            if preflight.signer_id != CAN_MIGRATE {
                return Err(PreflightError::NotAllowedToMigrate {
                    signer_id: preflight.signer_id.clone(),
                }
                .into_report());
            }
            let has_state = is_upgrade
                && !matches!(
                    xyk::state(
                        &preflight.network_config,
                        &preflight.block_reference,
                        &dex_id
                    )?,
                    XykState::Missing
                );
            if !has_state {
                return Err(PreflightError::NoStateToMigrate {
                    dex_id: dex_id.clone(),
                    init_command,
                }
                .into_report());
            }
            steps.push(format!(
                "Migrate the state of {dex_id} to the new code's layout in the same receipt; if the migration fails, the deploy is reverted too"
            ));
            function_calls.push(FunctionCall {
                method_name: "dex_call",
                args: json!({
                    "dex_id": dex_id,
                    "method": "migrate",
                    "args": Base64VecU8(migrate_args.clone()),
                    "attached_assets": {},
                    "referrer": null,
                }),
                deposit: ONE_YOCTO_NEAR,
                gas: MIGRATE_GAS,
            });
        } else if is_upgrade {
            steps.push(
                "Keep the state as it is; code that changes its layout deploys with --migrate"
                    .to_string(),
            );
        }
        let migrating_redeploy_command = (is_upgrade && !migrates).then(|| {
            let mut command = vec![
                "near".to_string(),
                "intear-dex-management".to_string(),
                "xyk".to_string(),
                "deploy".to_string(),
                preflight.signer_id.to_string(),
                "--name".to_string(),
                dex_id.id.clone(),
                "--migrate".to_string(),
            ];
            command.extend(code_source_args.iter().cloned());
            command.extend([
                "network-config".to_string(),
                preflight.connection_name.clone(),
            ]);
            shell_words::join(command)
        });
        Ok(TransactionPlan {
            receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
            steps,
            function_calls,
            expected_outcome: ExpectedOutcome {
                action_name: "deploy",
                success_message: if migrates {
                    format!("Deployed {code_label} to {dex_id} and migrated its state")
                } else {
                    format!("Deployed {code_label} to {dex_id}")
                },
                transfer_call: None,
                dex_id: Some(dex_id.clone()),
                deployment: Some(ExpectedDeployment {
                    dex_id: dex_id.clone(),
                    code_hash,
                    next_step: (!is_upgrade)
                        .then(|| format!("Next, initialize it: {init_command}")),
                    migrating_redeploy_command,
                }),
            },
        })
    })
}
