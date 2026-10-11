use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;

use color_eyre::Section;
use color_eyre::eyre::{Context, eyre};
use intear_dex_types::DexId;
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::json_types::{Base58CryptoHash, Base64VecU8};
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use xyk_dex_types::{CAN_MIGRATE, MigrateArgs};

use super::XykContext;
use crate::chain::engine;
use crate::chain::xyk::{self, XykState};
use crate::deployment::{DEFAULT_XYK_DEX_NAME, ENGINE_ACCOUNT_ID};
use crate::errors::PreflightError;
use crate::outcome::{ExpectedDeployment, ExpectedOutcome};
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan, storage, xyk_storage};

const DEPLOY_DEX_CODE_GAS: Gas = Gas::from_teragas(100);
const MIGRATE_GAS: Gas = Gas::from_teragas(50);
const WASM_MAGIC_BYTES: &[u8] = b"\0asm";

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
    #[interactive_clap(long)]
    /// Migrate the dex's state to the new code's layout in the same receipt, so that a failed migration reverts the deploy
    migrate: bool,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// The key that the migration sets for signing fee discounts, e.g. 'ed25519:…'
    fee_discount_signer: Option<near_cli_rs::types::public_key::PublicKey>,
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
        if previous_context.is_dex_chosen {
            return Err(
                eyre!("xyk deploy creates <signer>/<name>, so it doesn't take --dex")
                    .suggestion("Name the dex with --name after deploy"),
            );
        }
        let name = scope
            .name
            .clone()
            .unwrap_or_else(|| DEFAULT_XYK_DEX_NAME.to_string());
        if name.is_empty() || name.contains('/') {
            return Err(eyre!(
                "Invalid dex name '{name}': it's the part of the dex id after <signer>/, so it can't be empty or contain /"
            ));
        }
        let migrate_args = match (scope.migrate, &scope.fee_discount_signer) {
            (true, Some(fee_discount_signer)) => near_sdk::borsh::to_vec(&MigrateArgs {
                fee_discount_signer_public_key: near_sdk::PublicKey::from_str(
                    &fee_discount_signer.to_string(),
                )
                .unwrap(),
            })?,
            (true, None) => {
                return Err(eyre!("The migration needs the fee discount signer's key")
                    .suggestion("Add --fee-discount-signer ed25519:… after --migrate"));
            }
            (false, Some(_)) => {
                return Err(eyre!(
                    "--fee-discount-signer is set by the migration, so it only goes with --migrate"
                ));
            }
            (false, None) => Vec::new(),
        };
        Ok(Self {
            global_context: previous_context.global_context,
            dex_id: DexId {
                deployer: scope.signer_account_id.clone().into(),
                id: name,
            },
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
        let code = build_xyk_dex()?;
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
        let code = scope.file.read_bytes()?;
        if !code.starts_with(WASM_MAGIC_BYTES) {
            return Err(eyre!("{} isn't a WebAssembly module", scope.file));
        }
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

/// Builds xyk-dex in the workspace of the current directory the way its
/// tests do: a release build for wasm, optimized with wasm-opt
fn build_xyk_dex() -> color_eyre::eyre::Result<Vec<u8>> {
    let deploy_built_file_instead = "Or deploy a wasm file that's already built with use-file";
    // A missing wasm-opt fails before the build, which takes a while
    let is_wasm_opt_installed = Command::new("wasm-opt")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !is_wasm_opt_installed {
        return Err(eyre!("wasm-opt isn't installed, and the build needs it")
            .suggestion("Install binaryen, which provides wasm-opt")
            .suggestion(deploy_built_file_instead));
    }
    let metadata_output = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .output()
        .wrap_err("Couldn't run cargo metadata")?;
    if !metadata_output.status.success() {
        return Err(eyre!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&metadata_output.stderr).trim()
        )
        .suggestion("Run build in a checkout of the dex repository")
        .suggestion(deploy_built_file_instead));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&metadata_output.stdout)
        .wrap_err("cargo metadata printed something other than JSON")?;
    let has_xyk_dex_package = metadata["packages"]
        .as_array()
        .is_some_and(|packages| packages.iter().any(|package| package["name"] == "xyk-dex"));
    let (Some(workspace_root), Some(target_directory)) = (
        metadata["workspace_root"].as_str(),
        metadata["target_directory"].as_str(),
    ) else {
        return Err(eyre!(
            "cargo metadata doesn't name the workspace root and target directory"
        ));
    };
    if !has_xyk_dex_package {
        return Err(
            eyre!("The workspace at {workspace_root} has no xyk-dex package")
                .suggestion("Run build in a checkout of the dex repository")
                .suggestion(deploy_built_file_instead),
        );
    }
    tracing::info!("Building xyk-dex in {workspace_root}");
    let build_status = Command::new("cargo")
        .current_dir(workspace_root)
        .args([
            "build",
            "--package=xyk-dex",
            "--release",
            "--target=wasm32-unknown-unknown",
        ])
        .status()
        .wrap_err("Couldn't run cargo build")?;
    if !build_status.success() {
        return Err(eyre!("cargo build of xyk-dex failed with {build_status}"));
    }
    let wasm_path =
        PathBuf::from(target_directory).join("wasm32-unknown-unknown/release/xyk_dex.wasm");
    let wasm_opt_status = Command::new("wasm-opt")
        .arg("-O")
        .arg(&wasm_path)
        .arg("-o")
        .arg(&wasm_path)
        .status()
        .wrap_err("Couldn't run wasm-opt")?;
    if !wasm_opt_status.success() {
        return Err(eyre!(
            "wasm-opt failed on {} with {wasm_opt_status}",
            wasm_path.display()
        ));
    }
    std::fs::read(&wasm_path).wrap_err_with(|| format!("Couldn't read {}", wasm_path.display()))
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
    let code_hash = Base58CryptoHash::from(near_primitives::hash::hash(&code).0);
    let signer_id: AccountId = dex_id.deployer.clone();
    planning::write_action_context(&global_context, signer_id, move |preflight| {
        preflight.ensure_engine_not_paused()?;
        let trusted_code_deployer =
            engine::trusted_code_deployer(&preflight.network_config, &preflight.block_reference)?;
        if trusted_code_deployer != preflight.signer_id {
            return Err(PreflightError::NotTrustedCodeDeployer {
                signer_id: preflight.signer_id.clone(),
                trusted_code_deployer,
            }
            .into_report());
        }
        let is_upgrade = engine::dex_exists(
            &preflight.network_config,
            &preflight.block_reference,
            &dex_id,
        )?;
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
        }
        let code_label = format!(
            "{} bytes of code (sha256 {})",
            code.len(),
            String::from(&code_hash)
        );
        let mut steps = Vec::new();
        let mut function_calls = Vec::new();
        // The code of an existing dex isn't readable, so an upgrade is
        // estimated as if the dex had none; the old code's storage is freed
        let code_bytes = storage::dex_code_record_bytes(
            &dex_id,
            code.len(),
            preflight.protocol_limits.extra_bytes_per_record,
        )?;
        let stored_bytes = if migrates {
            code_bytes
                .checked_add(xyk_storage::migration_growth_bytes())
                .ok_or_else(|| eyre!("Storage size overflow"))?
        } else {
            code_bytes
        };
        if let Some(storage_top_up) = preflight.dex_storage_top_up(&dex_id, stored_bytes)? {
            steps.push(storage_top_up.step(match (is_upgrade, migrates) {
                (false, _) => "the code",
                (true, false) => {
                    "all of the new code, since the old code's size can't be read; what the old code used becomes available again"
                }
                (true, true) => {
                    "all of the new code and the migrated state, since the old code's size can't be read; what the old code used becomes available again"
                }
            }));
            function_calls.push(storage_top_up.function_call());
        }
        steps.push(if is_upgrade {
            format!("Deploy {code_label} to {dex_id}, replacing its code")
        } else {
            format!("Deploy {code_label} to {dex_id}, a new dex")
        });
        function_calls.push(FunctionCall {
            method_name: "deploy_dex_code",
            args: json!({
                "last_part_of_id": dex_id.id,
                "code_base64": Base64VecU8(code.clone()),
            }),
            deposit: ONE_YOCTO_NEAR,
            gas: DEPLOY_DEX_CODE_GAS,
        });
        if migrates {
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
