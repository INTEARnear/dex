use color_eyre::Section;
use color_eyre::eyre::eyre;
use intear_dex_types::DexId;
use near_cli_rs::commands::ActionContext;
use near_primitives::types::AccountId;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use super::XykContext;
use crate::commands::dex_code;
use crate::deployment::{DEFAULT_XYK_DEX_NAME, ENGINE_ACCOUNT_ID};
use crate::outcome::{ExpectedDeployment, ExpectedOutcome};
use crate::planning::{self, TransactionPlan};

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = DeployContext)]
pub struct Deploy {
    #[interactive_clap(skip_default_input_arg)]
    /// The trusted code deployer of dex.intear.near, which the dex id starts with
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// The name of the dex after <signer>/ (default: xyk)
    name: Option<String>,
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
        // Initializing tops up for the state itself, and an upgrade tops up for
        // all of the new code while the old code's storage gets freed
        let dex_code::CodeDeployment {
            is_upgrade,
            code_hash,
            code_label,
            mut steps,
            function_calls,
        } = dex_code::plan_code_deployment(preflight, &dex_id, &code, 0)?;
        if is_upgrade {
            steps.push("Keep the state as it is".to_string());
        }
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
                    next_step: (!is_upgrade)
                        .then(|| format!("Next, initialize it: {init_command}")),
                }),
            },
            deploy_code: None,
        })
    })
}
