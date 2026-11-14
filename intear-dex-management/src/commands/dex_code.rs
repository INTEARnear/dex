//! Building dex code and deploying it to the engine, which every dex's
//! deploy command does the same way

use std::path::PathBuf;
use std::process::Command;

use color_eyre::Section;
use color_eyre::eyre::{Context, eyre};
use intear_dex_common::{AssetId, DexId, TokenAmount};
use intear_dex_types::DexCallAfterDeploy;
use near_primitives::gas::Gas;
use near_sdk::NearToken;
use near_sdk::json_types::{Base58CryptoHash, Base64VecU8};
use serde_json::json;

use crate::chain::{account, engine};
use crate::commands::engine::deposit::DEPOSIT_NEAR_GAS;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::errors::PreflightError;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, Preflight, format_near, storage};

// The translator validates and translates the code, and the engine stores both after it
const DEPLOY_DEX_CODE_GAS: Gas = Gas::from_teragas(300);
const WASM_MAGIC_BYTES: &[u8] = b"\0asm";
const CALL_AFTER_DEPLOY_ARG: &[u8] = b"call_after_deploy";

/// The dex that deploy creates or replaces: <signer>/<name>
pub fn deployed_dex_id(
    signer_account_id: &near_cli_rs::types::account_id::AccountId,
    name: &str,
) -> color_eyre::eyre::Result<DexId> {
    if name.is_empty() || name.contains('/') {
        return Err(eyre!(
            "Invalid dex name '{name}': it's the part of the dex id after <signer>/, so it can't be empty or contain /"
        ));
    }
    Ok(DexId {
        deployer: signer_account_id.clone().into(),
        id: name.to_string(),
    })
}

pub fn read_wasm_file(
    file: &near_cli_rs::types::path_buf::PathBuf,
) -> color_eyre::eyre::Result<Vec<u8>> {
    let code = file.read_bytes()?;
    if !code.starts_with(WASM_MAGIC_BYTES) {
        return Err(eyre!("{file} isn't a WebAssembly module"));
    }
    Ok(code)
}

/// Builds `package_name` with `features` in the workspace of the current
/// directory the way the tests build dexes: a release build for wasm, optimized
/// with wasm-opt
pub fn build_dex_package(
    package_name: &str,
    features: &[&str],
) -> color_eyre::eyre::Result<Vec<u8>> {
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
    let has_package = metadata["packages"].as_array().is_some_and(|packages| {
        packages
            .iter()
            .any(|package| package["name"] == package_name)
    });
    let (Some(workspace_root), Some(target_directory)) = (
        metadata["workspace_root"].as_str(),
        metadata["target_directory"].as_str(),
    ) else {
        return Err(eyre!(
            "cargo metadata doesn't name the workspace root and target directory"
        ));
    };
    if !has_package {
        return Err(
            eyre!("The workspace at {workspace_root} has no {package_name} package")
                .suggestion("Run build in a checkout of the dex repository")
                .suggestion(deploy_built_file_instead),
        );
    }
    tracing::info!("Building {package_name} with features {features:?} in {workspace_root}");
    let build_status = Command::new("cargo")
        .current_dir(workspace_root)
        .args([
            "build",
            &format!("--package={package_name}"),
            &format!("--features={}", features.join(",")),
            "--release",
            "--target=wasm32-unknown-unknown",
        ])
        .status()
        .wrap_err("Couldn't run cargo build")?;
    if !build_status.success() {
        return Err(eyre!(
            "cargo build of {package_name} failed with {build_status}"
        ));
    }
    let wasm_path = PathBuf::from(target_directory).join(format!(
        "wasm32-unknown-unknown/release/{}.wasm",
        package_name.replace('-', "_")
    ));
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

/// The translation of `code` that the engine stores next to it, as the translator makes it
/// with the same wasmi as this
pub fn translate(code: &[u8]) -> color_eyre::eyre::Result<Vec<u8>> {
    wasmi::Engine::default()
        .precompile_module(code)
        .map_err(|err| eyre!("The translator rejects {} bytes of code: {err}", code.len()))
}

/// A method of the new code that the engine calls in the receipt that stores
/// the code, so that the code stays as it was if the call fails
pub struct CallAfterDeploy {
    pub method: String,
    pub args: Vec<u8>,
    /// What the call takes from the signer's balance on the engine, which the
    /// transaction deposits, such as for the storage it adds
    pub attached_near: NearToken,
    /// What the call does, for the plan
    pub step: String,
}

/// What deploying code to a dex takes: the storage for the code, and the
/// deploy itself
pub struct CodeDeployment {
    /// Whether the dex already has code, which the new code replaces
    pub is_upgrade: bool,
    pub code_hash: Base58CryptoHash,
    pub code_label: String,
    pub steps: Vec<String>,
    pub function_calls: Vec<FunctionCall>,
}

/// `extra_state_bytes` is what the dex's storage balance has to cover on top
/// of the code of a new dex, for state that the dex stores as soon as it's
/// called
pub fn plan_code_deployment(
    preflight: &Preflight,
    dex_id: &DexId,
    code: &[u8],
    extra_state_bytes: u64,
    call_after_deploy: Option<CallAfterDeploy>,
) -> color_eyre::eyre::Result<CodeDeployment> {
    let trusted_code_deployer =
        engine::trusted_code_deployer(&preflight.network_config, &preflight.block_reference)?;
    if trusted_code_deployer != preflight.signer_id {
        return Err(PreflightError::NotTrustedCodeDeployer {
            signer_id: preflight.signer_id.clone(),
            trusted_code_deployer,
        }
        .into_report());
    }
    if let Some(call_after_deploy) = &call_after_deploy {
        // The engine ignores arguments it doesn't take, so code from before call_after_deploy
        // would deploy without making the call. Code that takes it has its name to read it.
        let engine_code = account::view_code(
            &preflight.network_config,
            &preflight.block_reference,
            &ENGINE_ACCOUNT_ID.to_owned(),
        )?;
        if !engine_code
            .windows(CALL_AFTER_DEPLOY_ARG.len())
            .any(|bytes| bytes == CALL_AFTER_DEPLOY_ARG)
        {
            return Err(PreflightError::EngineWithoutCallAfterDeploy {
                method: call_after_deploy.method.clone(),
            }
            .into_report());
        }
    }
    let deployed_code = engine::dex_code(
        &preflight.network_config,
        &preflight.block_reference,
        dex_id,
    )?;
    let is_upgrade = deployed_code.is_some();
    let code_hash = Base58CryptoHash::from(near_primitives::hash::hash(code).0);
    let code_label = format!(
        "{} bytes of code (sha256 {})",
        code.len(),
        String::from(&code_hash)
    );
    let mut steps = Vec::new();
    let mut function_calls = Vec::new();
    // The engine stores the code and its translation
    let stored_bytes = |code: &[u8]| -> color_eyre::eyre::Result<u64> {
        let extra_bytes_per_record = preflight.protocol_limits.extra_bytes_per_record;
        storage::dex_code_record_bytes(dex_id, code.len(), extra_bytes_per_record)?
            .checked_add(storage::dex_code_translation_record_bytes(
                dex_id,
                translate(code)?.len(),
                extra_bytes_per_record,
            )?)
            .ok_or_else(|| eyre!("Storage size overflow"))
    };
    // The engine charges an upgrade only for what the new code and its translation
    // take more than the old, and frees what they take less
    let bytes = match &deployed_code {
        Some(deployed_code) => stored_bytes(code)?.saturating_sub(stored_bytes(deployed_code)?),
        None => stored_bytes(code)?
            .checked_add(extra_state_bytes)
            .ok_or_else(|| eyre!("Storage size overflow"))?,
    };
    if let Some(storage_top_up) = preflight.dex_storage_top_up(dex_id, bytes)? {
        steps.push(storage_top_up.step(&match &deployed_code {
            Some(deployed_code) => format!(
                "what the new code and its translation take more than the old code of {} bytes and its translation",
                deployed_code.len()
            ),
            None => "the code and its translation".to_string(),
        }));
        function_calls.push(storage_top_up.function_call());
    }
    if let Some(call_after_deploy) = &call_after_deploy
        && !call_after_deploy.attached_near.is_zero()
    {
        let registration_calls = preflight.registration_calls(
            &planning::storage_near_registrations(&preflight.signer_id, dex_id),
        )?;
        steps.extend(registration_calls.steps);
        function_calls.extend(registration_calls.function_calls);
        steps.push(format!(
            "Deposit {} from {}'s wallet into its balance on {ENGINE_ACCOUNT_ID}, which {} takes; {dex_id} sends back what it doesn't use",
            format_near(call_after_deploy.attached_near),
            preflight.signer_id,
            call_after_deploy.method
        ));
        function_calls.push(FunctionCall {
            method_name: "deposit_near",
            args: json!({}).into(),
            deposit: call_after_deploy.attached_near,
            gas: DEPOSIT_NEAR_GAS,
        });
    }
    steps.push(if is_upgrade {
        format!("Deploy {code_label} to {dex_id}, replacing its code")
    } else {
        format!("Deploy {code_label} to {dex_id}, a new dex")
    });
    let gas = match &call_after_deploy {
        Some(call_after_deploy) => {
            steps.push(call_after_deploy.step.clone());
            steps.push(format!(
                "{} runs on the new code in the receipt that stores it, so if it fails, the code stays as it was",
                call_after_deploy.method
            ));
            // The translation and the call after it share what's left of the transaction's gas
            let other_calls_gas = function_calls
                .iter()
                .try_fold(Gas::from_gas(0), |total, call: &FunctionCall| {
                    total.checked_add(call.gas)
                })
                .ok_or_else(|| eyre!("Gas overflow"))?;
            preflight
                .protocol_limits
                .max_prepaid_gas
                .checked_sub(other_calls_gas)
                .ok_or_else(|| eyre!("The storage top-up takes all of a transaction's gas"))?
        }
        None => DEPLOY_DEX_CODE_GAS,
    };
    function_calls.push(FunctionCall {
        method_name: "deploy_dex_code",
        args: json!({
            "last_part_of_id": dex_id.id,
            "code_base64": Base64VecU8(code.to_vec()),
            "call_after_deploy": call_after_deploy.map(|call_after_deploy| DexCallAfterDeploy {
                method: call_after_deploy.method,
                args: Base64VecU8(call_after_deploy.args),
                attached_assets: if call_after_deploy.attached_near.is_zero() {
                    Default::default()
                } else {
                    [(
                        AssetId::Near,
                        TokenAmount::new(call_after_deploy.attached_near.as_yoctonear()),
                    )]
                    .into()
                },
            }),
        })
        .into(),
        deposit: ONE_YOCTO_NEAR,
        gas,
    });
    Ok(CodeDeployment {
        is_upgrade,
        code_hash,
        code_label,
        steps,
        function_calls,
    })
}
