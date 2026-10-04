//! Runs every `sh` block of the repository's README in order, as written,
//! with a `mainnet` connection to the sandbox. `near` there is a script that
//! runs the CLI binary for `near intear-dex-management`, and refuses anything
//! else, which the README keeps in `shell` blocks.
// The blocks run in bash
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use common::{fixture_account, start_sandbox_with_xyk_pools, write_near_cli_config};
use intear_dex_test_support::get_compiled_wasms;
use near_sdk::json_types::U128;
use near_workspaces::AccountId;
use near_workspaces::types::{KeyType, NearToken, SecretKey};
use serde_json::json;

const README: &str = include_str!("../../README.md");
/// The accounts that sign in the README's examples
const SIGNERS: [&str; 3] = ["alice.near", "slimedragon.near", "dex.intear.near"];

/// The contents of the README's code blocks in `language`, without the
/// indentation of blocks in lists
fn code_blocks(markdown: &str, language: &str) -> Vec<String> {
    let opening_fence = format!("```{language}");
    let mut blocks = Vec::new();
    let mut open_block: Option<(usize, Vec<&str>)> = None;
    for line in markdown.lines() {
        let indentation = line.len() - line.trim_start().len();
        match &mut open_block {
            None if line.trim_start() == opening_fence => {
                open_block = Some((indentation, Vec::new()));
            }
            None => {}
            Some((_, block_lines)) if line.trim_start() == "```" => {
                blocks.push(block_lines.join("\n"));
                open_block = None;
            }
            Some((block_indentation, block_lines)) => {
                block_lines.push(line.get(*block_indentation..).unwrap_or(""));
            }
        }
    }
    assert!(
        open_block.is_none(),
        "The README has an unclosed code block"
    );
    blocks
}

#[tokio::test]
async fn readme_examples_run_as_written() {
    let sandbox = start_sandbox_with_xyk_pools().await;
    // Beyond the common sandbox, the examples hand code deployment to
    // deployer.slimedragon.near and rescue USDt that reached the engine
    // without a balance on it
    let slimedragon = fixture_account(&sandbox, "slimedragon.near");
    slimedragon
        .create_subaccount("deployer")
        .initial_balance(NearToken::from_near(1))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let usdt_id: AccountId = "usdt.tether-token.near".parse().unwrap();
    slimedragon
        .call(&usdt_id, "ft_transfer")
        .args_json(json!({ "receiver_id": "dex.intear.near", "amount": U128(5_000_000) }))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    sandbox.fast_forward(10).await.unwrap();

    let home = tempfile::tempdir().unwrap();
    write_near_cli_config(home.path(), &sandbox.rpc_addr(), "mainnet");
    // Keys where sign-with-keychain looks when the system keychain has none
    let mainnet_keys_directory = home.path().join("credentials/mainnet");
    std::fs::create_dir_all(&mainnet_keys_directory).unwrap();
    for signer in SIGNERS {
        let secret_key = SecretKey::from_seed(KeyType::ED25519, signer);
        std::fs::write(
            mainnet_keys_directory.join(format!("{signer}.json")),
            json!({
                "account_id": signer,
                "public_key": secret_key.public_key().to_string(),
                "private_key": secret_key.to_string(),
            })
            .to_string(),
        )
        .unwrap();
    }
    let near_directory = home.path().join("bin");
    std::fs::create_dir_all(&near_directory).unwrap();
    let near_script = near_directory.join("near");
    std::fs::write(
        &near_script,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = intear-dex-management ]; then\n\
             \tshift\n\
             \texec '{}' \"$@\"\n\
             fi\n\
             echo \"The README test only runs near intear-dex-management, not near $1\" >&2\n\
             exit 1\n",
            env!("CARGO_BIN_EXE_near-intear-dex-management")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&near_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let working_directory = tempfile::tempdir().unwrap();
    std::fs::write(
        working_directory.path().join("xyk_dex.wasm"),
        &get_compiled_wasms().await.xyk_dex_wasm,
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        near_directory.display(),
        std::env::var("PATH").unwrap()
    );
    let blocks = code_blocks(README, "sh");
    assert!(!blocks.is_empty(), "The README has no sh blocks");
    for block in blocks {
        let output = Command::new("bash")
            .args(["-e", "-c", &block])
            .current_dir(working_directory.path())
            .env("PATH", &path)
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "This README block failed:\n{block}\n--- stdout ---\n{}--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
