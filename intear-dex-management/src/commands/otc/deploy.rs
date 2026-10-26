use color_eyre::Section;
use color_eyre::eyre::eyre;
use intear_dex_types::DexId;
use near_cli_rs::commands::ActionContext;
use near_primitives::types::AccountId;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use super::OtcContext;
use crate::commands::dex_code;
use crate::deployment::{DEFAULT_OTC_DEX_NAME, ENGINE_ACCOUNT_ID};
use crate::outcome::{ExpectedDeployment, ExpectedOutcome};
use crate::planning::{self, TransactionPlan, otc_storage};

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = OtcContext)]
#[interactive_clap(output_context = DeployContext)]
pub struct Deploy {
    #[interactive_clap(skip_default_input_arg)]
    /// The trusted code deployer of dex.intear.near, which the dex id starts with
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// The name of the dex after <signer>/ (default: otc)
    name: Option<String>,
    #[interactive_clap(subcommand)]
    code: DeployCode,
}

impl Deploy {
    fn input_signer_account_id(
        context: &OtcContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account deploys? It has to be the engine's trusted code deployer",
        )
    }
}

#[derive(Clone)]
pub struct DeployContext {
    global_context: crate::GlobalContext,
    dex_id: DexId,
}

impl DeployContext {
    pub fn from_previous_context(
        previous_context: OtcContext,
        scope: &<Deploy as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        if previous_context.is_dex_chosen {
            return Err(
                eyre!("otc deploy creates <signer>/<name>, so it doesn't take --dex")
                    .suggestion("Name the dex with --name after deploy"),
            );
        }
        let dex_id = dex_code::deployed_dex_id(
            &scope.signer_account_id,
            scope.name.as_deref().unwrap_or(DEFAULT_OTC_DEX_NAME),
        )?;
        Ok(Self {
            global_context: previous_context.global_context,
            dex_id,
        })
    }
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = DeployContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// Where does the code come from?
pub enum DeployCode {
    #[strum_discriminants(strum(
        message = "build     - Build otc-dex in the checkout you're in, with cargo and wasm-opt"
    ))]
    /// Build otc-dex in the checkout you're in, with cargo and wasm-opt
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
        let code = dex_code::build_dex_package("otc-dex")?;
        Ok(Self(deploy_action_context(previous_context, code)))
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
        Ok(Self(deploy_action_context(previous_context, code)))
    }
}

impl From<UseFileContext> for ActionContext {
    fn from(item: UseFileContext) -> Self {
        item.0
    }
}

fn deploy_action_context(deploy_context: DeployContext, code: Vec<u8>) -> ActionContext {
    let DeployContext {
        global_context,
        dex_id,
    } = deploy_context;
    let signer_id: AccountId = dex_id.deployer.clone();
    planning::write_action_context(&global_context, signer_id, move |preflight| {
        preflight.ensure_engine_not_paused()?;
        // The OTC dex has no initialization: its first call stores the state
        let initial_state_bytes = otc_storage::initial_state_bytes(
            &dex_id,
            preflight.protocol_limits.extra_bytes_per_record,
        )?;
        let dex_code::CodeDeployment {
            is_upgrade,
            code_hash,
            code_label,
            mut steps,
            function_calls,
        } = dex_code::plan_code_deployment(preflight, &dex_id, &code, initial_state_bytes)?;
        if is_upgrade {
            steps.push("Keep the state as it is".to_string());
        }
        let storage_deposit_command = shell_words::join([
            "near",
            "intear-dex-management",
            "otc",
            "--dex",
            &dex_id.to_string(),
            "storage-deposit",
            preflight.signer_id.as_str(),
            "0.01 NEAR",
            "network-config",
            &preflight.connection_name,
        ]);
        Ok(TransactionPlan {
            receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
            steps,
            function_calls,
            expected_outcome: ExpectedOutcome {
                action_name: "deploy",
                success_message: format!("Deployed {code_label} to {dex_id}"),
                transfer_call: None,
                dex_id: Some(dex_id.clone()),
                deployment: Some(ExpectedDeployment {
                    dex_id: dex_id.clone(),
                    code_hash,
                    next_step: (!is_upgrade).then(|| {
                        format!(
                            "It needs no initialization. Each trader pays for their records on it, e.g.: {storage_deposit_command}"
                        )
                    }),
                    migrating_redeploy_command: None,
                }),
            },
        })
    })
}
