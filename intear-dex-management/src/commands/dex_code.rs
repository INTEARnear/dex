//! Building dex code and deploying it to the engine, which every dex's
//! deploy command does the same way

use std::path::PathBuf;
use std::process::Command;

use color_eyre::Section;
use color_eyre::eyre::{Context, eyre};
use intear_dex_types::DexId;
use near_primitives::gas::Gas;
use near_sdk::json_types::{Base58CryptoHash, Base64VecU8};
use serde_json::json;

use crate::chain::engine;
use crate::errors::PreflightError;
use crate::planning::{FunctionCall, ONE_YOCTO_NEAR, Preflight, storage};

const DEPLOY_DEX_CODE_GAS: Gas = Gas::from_teragas(100);
const WASM_MAGIC_BYTES: &[u8] = b"\0asm";

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

/// Builds `package_name` in the workspace of the current directory the way
/// the tests build dexes: a release build for wasm, optimized with wasm-opt
pub fn build_dex_package(package_name: &str) -> color_eyre::eyre::Result<Vec<u8>> {
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
    tracing::info!("Building {package_name} in {workspace_root}");
    let build_status = Command::new("cargo")
        .current_dir(workspace_root)
        .args([
            "build",
            &format!("--package={package_name}"),
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
/// of the code, for state that the dex stores as soon as it's called
pub fn plan_code_deployment(
    preflight: &Preflight,
    dex_id: &DexId,
    code: &[u8],
    extra_state_bytes: u64,
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
    let is_upgrade = engine::dex_exists(
        &preflight.network_config,
        &preflight.block_reference,
        dex_id,
    )?;
    let code_hash = Base58CryptoHash::from(near_primitives::hash::hash(code).0);
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
        dex_id,
        code.len(),
        preflight.protocol_limits.extra_bytes_per_record,
    )?;
    let bytes = if is_upgrade {
        code_bytes
    } else {
        code_bytes
            .checked_add(extra_state_bytes)
            .ok_or_else(|| eyre!("Storage size overflow"))?
    };
    if let Some(storage_top_up) = preflight.dex_storage_top_up(dex_id, bytes)? {
        steps.push(storage_top_up.step(if is_upgrade {
            "all of the new code, since the old code's size can't be read; what the old code used becomes available again"
        } else {
            "the code"
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
            "code_base64": Base64VecU8(code.to_vec()),
        }),
        deposit: ONE_YOCTO_NEAR,
        gas: DEPLOY_DEX_CODE_GAS,
    });
    Ok(CodeDeployment {
        is_upgrade,
        code_hash,
        code_label,
        steps,
        function_calls,
    })
}
