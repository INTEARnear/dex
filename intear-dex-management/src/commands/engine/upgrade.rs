//! Building the engine reproducibly and deploying it to dex.intear.near, in
//! chunks if it doesn't fit in one transaction

use std::path::PathBuf;
use std::process::Command;

use color_eyre::Section;
use color_eyre::eyre::{Context, eyre};
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::hash::CryptoHash;
use near_primitives::types::AccountId;
use near_sdk::NearToken;
use near_sdk::json_types::Base58CryptoHash;
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use crate::chain::{account, engine};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::errors::PreflightError;
use crate::outcome::{DexEvents, ExpectedOutcome};
use crate::planning::{self, CallArgs, FunctionCall, Preflight, TransactionPlan, storage};

/// NEAR's 1.5 MiB limit on a transaction, less 4 KiB for the rest of it
const CHUNK_SIZE: usize = 1_568_768;
const THEN_CALL_GAS: Gas = Gas::from_teragas(100);
const ENGINE_PACKAGE: &str = "intear-dex";
const WASM_MAGIC_BYTES: &[u8] = b"\0asm";

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
pub struct UpgradeCommands {
    #[interactive_clap(subcommand)]
    action: UpgradeAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do?
pub enum UpgradeAction {
    #[strum_discriminants(strum(
        message = "build                - Build the engine in the checkout you're in, reproducibly in Docker"
    ))]
    /// Build the engine in the checkout you're in, reproducibly in Docker
    Build(Build),
    #[strum_discriminants(strum(
        message = "store-chunk          - Send a chunk of the built code, when it's too large for one transaction"
    ))]
    /// Send a chunk of the built code, when it's too large for one transaction
    StoreChunk(StoreChunk),
    #[strum_discriminants(strum(
        message = "deploy               - Deploy the built code, with its last chunk if it has chunks"
    ))]
    /// Deploy the built code, with its last chunk if it has chunks
    Deploy(Deploy),
    #[strum_discriminants(strum(
        message = "deploy-and-translate - Deploy the built code like deploy, and translate the code of every dex for it"
    ))]
    /// Deploy the built code like deploy, and translate the code of every dex for it
    DeployAndTranslate(DeployAndTranslate),
}

struct EngineCheckout {
    root: PathBuf,
    built_code_path: PathBuf,
}

impl EngineCheckout {
    fn in_current_directory() -> color_eyre::eyre::Result<Self> {
        let metadata_output = Command::new("cargo")
            .args(["metadata", "--format-version=1", "--no-deps"])
            .output()
            .wrap_err("Couldn't run cargo metadata")?;
        if !metadata_output.status.success() {
            return Err(eyre!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&metadata_output.stderr).trim()
            )
            .suggestion("Run it in a checkout of the dex repository"));
        }
        let metadata: serde_json::Value = serde_json::from_slice(&metadata_output.stdout)
            .wrap_err("cargo metadata printed something other than JSON")?;
        let (Some(workspace_root), Some(target_directory)) = (
            metadata["workspace_root"].as_str(),
            metadata["target_directory"].as_str(),
        ) else {
            return Err(eyre!(
                "cargo metadata doesn't name the workspace root and target directory"
            ));
        };
        let has_engine = metadata["packages"].as_array().is_some_and(|packages| {
            packages
                .iter()
                .any(|package| package["name"] == ENGINE_PACKAGE)
        });
        if !has_engine {
            return Err(
                eyre!("The workspace at {workspace_root} has no {ENGINE_PACKAGE} package")
                    .suggestion("Run it in a checkout of the dex repository"),
            );
        }
        Ok(Self {
            root: PathBuf::from(workspace_root),
            built_code_path: PathBuf::from(target_directory).join("near/intear_dex.wasm"),
        })
    }

    fn built_code(&self) -> color_eyre::eyre::Result<BuiltCode> {
        let code = std::fs::read(&self.built_code_path)
            .wrap_err_with(|| format!("Couldn't read {}", self.built_code_path.display()))
            .suggestion("Build it first with `engine upgrade build`")?;
        if !code.starts_with(WASM_MAGIC_BYTES) {
            return Err(eyre!(
                "{} isn't a WebAssembly module",
                self.built_code_path.display()
            ));
        }
        let hash = Base58CryptoHash::from(near_primitives::hash::hash(&code).0);
        Ok(BuiltCode { code, hash })
    }
}

struct BuiltCode {
    code: Vec<u8>,
    hash: Base58CryptoHash,
}

impl BuiltCode {
    fn chunks(&self) -> Vec<&[u8]> {
        self.code.chunks(CHUNK_SIZE).collect()
    }

    fn hash_up_to(&self, end: usize) -> [u8; 32] {
        near_primitives::hash::hash(&self.code[..end]).0
    }

    fn label(&self) -> String {
        format!(
            "{} bytes of code (sha256 {})",
            self.code.len(),
            String::from(&self.hash)
        )
    }
}

fn ensure_untracked_near_covers(
    preflight: &Preflight,
    added_bytes: u64,
) -> color_eyre::eyre::Result<()> {
    let needed = storage::storage_cost(added_bytes, preflight.protocol_limits.storage_byte_cost)?;
    let untracked_near =
        engine::untracked_near(&preflight.network_config, &preflight.block_reference)?;
    if untracked_near < needed {
        return Err(PreflightError::UntrackedNearShort {
            available: planning::format_near(untracked_near),
            needed: planning::format_near(needed),
        }
        .into_report());
    }
    Ok(())
}

fn upgrade_command(arguments: &[&str]) -> String {
    shell_words::join(
        ["near", "intear-dex-management", "engine", "upgrade"]
            .iter()
            .chain(arguments),
    )
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = BuildContext)]
pub struct Build;

#[derive(Debug, Clone)]
pub struct BuildContext;

impl BuildContext {
    pub fn from_previous_context(
        _previous_context: crate::GlobalContext,
        _scope: &<Build as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let checkout = EngineCheckout::in_current_directory()?;
        let out_dir = checkout
            .built_code_path
            .parent()
            .ok_or_else(|| eyre!("The built code has no directory"))?;
        tracing::info!(
            "Building {ENGINE_PACKAGE} in {} reproducibly",
            checkout.root.display()
        );
        let build_status = Command::new("cargo")
            .current_dir(&checkout.root)
            .args(["near", "build", "reproducible-wasm", "--out-dir"])
            .arg(out_dir)
            .status()
            .wrap_err("Couldn't run cargo near")
            .suggestion("Install cargo-near")?;
        if !build_status.success() {
            return Err(eyre!("The reproducible build failed with {build_status}")
                .suggestion("It needs Docker running, and everything committed to git"));
        }
        let built = checkout.built_code()?;
        let chunks = built.chunks();
        let mut lines = vec![format!(
            "Built {} at {}",
            built.label(),
            checkout.built_code_path.display()
        )];
        if let [_] = chunks.as_slice() {
            lines.push(format!(
                "It fits in one transaction. Deploy it with:\n    {}",
                upgrade_command(&["deploy", "<method to call on it>"])
            ));
        } else {
            lines.push(format!(
                "It takes {} transactions. Store its chunks with:",
                chunks.len()
            ));
            for index in 0..chunks.len().saturating_sub(1) {
                lines.push(format!(
                    "    {}",
                    upgrade_command(&["store-chunk", &index.to_string()])
                ));
            }
            lines.push(format!(
                "and deploy it with the last one:\n    {}",
                upgrade_command(&["deploy", "<method to call on it>"])
            ));
        }
        lines.push(
            "The method runs on the new code right after it's deployed: migrate if the state changed, else a view such as is_paused".to_string(),
        );
        lines.push(format!(
            "If the engine's wasmi changed how it translates dex code, deploy it with {} instead, once the translator with the same wasmi is deployed",
            upgrade_command(&["deploy-and-translate"])
        ));
        tracing::info!("{}", lines.join("\n│  "));
        Ok(Self)
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = StoreChunkContext)]
pub struct StoreChunk {
    /// Which chunk, counting from 0? deploy sends the last one
    index: u64,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

#[derive(Clone)]
pub struct StoreChunkContext(ActionContext);

impl StoreChunkContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<StoreChunk as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let built = EngineCheckout::in_current_directory()?.built_code()?;
        let chunks = built.chunks();
        let last_index = chunks.len().saturating_sub(1);
        let index = usize::try_from(scope.index)?;
        if index >= last_index {
            return Err(eyre!(
                "The built code has {} chunks, and deploy sends the last one, so store-chunk only sends chunks 0 to {}",
                chunks.len(),
                last_index.saturating_sub(1)
            )
            .suggestion(format!(
                "Deploy it with {}",
                upgrade_command(&["deploy", "<method to call on it>"])
            )));
        }
        let chunk = chunks[index].to_vec();
        let offset = index.saturating_mul(CHUNK_SIZE);
        let end = offset.saturating_add(chunk.len());
        let code_hash_so_far = built.hash_up_to(end);
        let chunk_count = chunks.len();
        let next_command = if index.saturating_add(1) < last_index {
            upgrade_command(&["store-chunk", &index.saturating_add(1).to_string()])
        } else {
            upgrade_command(&["deploy", "<method to call on it>"])
        };
        let code_label = built.label();
        Ok(Self(planning::write_action_context(
            &previous_context,
            ENGINE_ACCOUNT_ID.to_owned(),
            move |preflight| {
                ensure_untracked_near_covers(preflight, u64::try_from(chunk.len())?)?;
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: vec![
                        format!(
                            "Store chunk {index} of the {chunk_count} chunks of {code_label} at byte {offset}, which {ENGINE_ACCOUNT_ID} pays storage for until it's deployed"
                        ),
                        format!(
                            "{ENGINE_ACCOUNT_ID} checks that its code up to byte {end} has the same sha256 as the built code up to there, and drops what it had after byte {offset}"
                        ),
                    ],
                    deploy_code: None,
                    function_calls: vec![FunctionCall {
                        method_name: "store_code_chunk",
                        args: CallArgs::Borsh(near_sdk::borsh::to_vec(&(
                            u32::try_from(offset)?,
                            &chunk,
                            code_hash_so_far,
                        ))?),
                        deposit: NearToken::from_yoctonear(0),
                        // Depends on what's already stored, up to 310 Tgas for 3 MiB
                        gas: preflight.protocol_limits.max_prepaid_gas,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "chunk upload",
                        success_message: format!(
                            "Stored chunk {index} of {chunk_count}. Next: {next_command}"
                        ),
                        transfer_call: None,
                        dex_id: None,
                        dex_events: DexEvents::NotRead,
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<StoreChunkContext> for ActionContext {
    fn from(item: StoreChunkContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = DeployContext)]
pub struct Deploy {
    /// Which method of the new code runs right after it's deployed? migrate if the state changed, else a view such as is_paused
    then_call: String,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

#[derive(Clone)]
pub struct DeployContext(ActionContext);

impl DeployContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Deploy as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let then_call: &'static str = scope.then_call.clone().leak();
        Ok(Self(deploy_action_context(
            &previous_context,
            then_call,
            |_| {
                Ok(ThenCallPlan {
                    args: json!({}),
                    steps: Vec::new(),
                    gas: THEN_CALL_GAS,
                })
            },
        )?))
    }
}

/// How the method that runs on the new code right after it's deployed is called
struct ThenCallPlan {
    args: serde_json::Value,
    /// What it does besides running, for the plan
    steps: Vec<String>,
    /// Its gas when the code is deployed in one transaction. With the last chunk, it gets
    /// what's left of the most gas a transaction can have.
    gas: Gas,
}

/// Deploys the built code, in one transaction or with its last chunk, and calls `then_call`
/// on it in the same receipt
fn deploy_action_context(
    previous_context: &crate::GlobalContext,
    then_call: &'static str,
    plan_then_call: impl Fn(&Preflight) -> color_eyre::eyre::Result<ThenCallPlan> + 'static,
) -> color_eyre::eyre::Result<ActionContext> {
    let built = EngineCheckout::in_current_directory()?.built_code()?;
    let code_label = built.label();
    let success_message =
        format!("Deployed {code_label} to {ENGINE_ACCOUNT_ID}, and {then_call} ran on it");
    let stays_if_it_fails = format!(
        "If the new code doesn't compile, which NEAR doesn't check, or {then_call} fails, the old code stays"
    );
    Ok(planning::write_action_context(
        previous_context,
        ENGINE_ACCOUNT_ID.to_owned(),
        move |preflight| {
            let ThenCallPlan {
                args: then_call_args,
                steps: then_call_steps,
                gas: then_call_gas,
            } = plan_then_call(preflight)?;
            let current_code_size = account::view_code_size(
                &preflight.network_config,
                &preflight.block_reference,
                &ENGINE_ACCOUNT_ID.to_owned(),
            )?;
            let expected_outcome = ExpectedOutcome {
                action_name: "upgrade",
                success_message: success_message.clone(),
                transfer_call: None,
                dex_id: None,
                dex_events: DexEvents::NotRead,
                deployment: None,
            };
            let chunks = built.chunks();
            let [.., last_chunk] = chunks.as_slice() else {
                return Err(eyre!("The built code is empty"));
            };
            if let [_] = chunks.as_slice() {
                let added_bytes = built.code.len().saturating_sub(current_code_size);
                ensure_untracked_near_covers(preflight, u64::try_from(added_bytes)?)?;
                return Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: [
                        format!(
                            "Deploy {code_label} to {ENGINE_ACCOUNT_ID}, and call {then_call} on it in the same receipt"
                        ),
                        stays_if_it_fails.clone(),
                    ]
                    .into_iter()
                    .chain(then_call_steps)
                    .collect(),
                    deploy_code: Some(built.code.clone()),
                    function_calls: vec![FunctionCall {
                        method_name: then_call,
                        args: then_call_args.into(),
                        deposit: NearToken::from_yoctonear(0),
                        gas: then_call_gas,
                    }],
                    expected_outcome,
                });
            }
            // The chunks stored before are freed when it's deployed
            let added_bytes = last_chunk.len().saturating_sub(current_code_size);
            ensure_untracked_near_covers(preflight, u64::try_from(added_bytes)?)?;
            let last_index = chunks.len().saturating_sub(1);
            let last_offset = last_index.saturating_mul(CHUNK_SIZE);
            Ok(TransactionPlan {
                receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                steps: [
                    format!(
                        "Store chunk {last_index}, the last of the {} chunks of {code_label}, at byte {last_offset}, after store-chunk sent the others",
                        chunks.len()
                    ),
                    format!(
                        "Deploy the code to {ENGINE_ACCOUNT_ID} if all of it has the built code's sha256, and call {then_call} on it in the same receipt"
                    ),
                    format!("{stays_if_it_fails}, and the code has to be stored again"),
                ]
                .into_iter()
                .chain(then_call_steps)
                .collect(),
                deploy_code: None,
                function_calls: vec![FunctionCall {
                    method_name: "deploy_code_chunks",
                    args: CallArgs::Borsh(near_sdk::borsh::to_vec(&(
                        u32::try_from(last_offset)?,
                        last_chunk,
                        built.hash_up_to(built.code.len()),
                        then_call,
                        serde_json::to_vec(&then_call_args)?,
                    ))?),
                    deposit: NearToken::from_yoctonear(0),
                    // 570 Tgas for 4 MiB, and then_call gets what's left
                    gas: preflight.protocol_limits.max_prepaid_gas,
                }],
                expected_outcome,
            })
        },
    ))
}

impl From<DeployContext> for ActionContext {
    fn from(item: DeployContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = DeployAndTranslateContext)]
pub struct DeployAndTranslate {
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

#[derive(Clone)]
pub struct DeployAndTranslateContext(ActionContext);

impl DeployAndTranslateContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        _scope: &<DeployAndTranslate as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self(deploy_action_context(
            &previous_context,
            "translate_dex_codes",
            |preflight| {
                let translator_id: AccountId = format!("translator.{ENGINE_ACCOUNT_ID}").parse()?;
                let translator = account::view_account(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &translator_id,
                )?;
                if translator.is_none_or(|translator| translator.code_hash == CryptoHash::default())
                {
                    return Err(
                        PreflightError::TranslatorWithoutCode { translator_id }.into_report()
                    );
                }
                let dex_ids =
                    engine::dex_ids(&preflight.network_config, &preflight.block_reference)?;
                Ok(ThenCallPlan {
                    steps: vec![
                        format!(
                            "translate_dex_codes sends the code of {} dexes to {translator_id}, and stores each translation, which its dex pays the storage of: {}",
                            dex_ids.len(),
                            dex_ids
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        "A dex runs again once its translation is stored, a few blocks after the deployment. One whose translation fails, such as for lack of storage, stays unusable until translate_dex_codes translates it again".to_string(),
                    ],
                    args: json!({ "dex_ids": dex_ids }),
                    // The translations get what translate_dex_codes leaves
                    gas: preflight.protocol_limits.max_prepaid_gas,
                })
            },
        )?))
    }
}

impl From<DeployAndTranslateContext> for ActionContext {
    fn from(item: DeployAndTranslateContext) -> Self {
        item.0
    }
}
