mod common;

use common::{Cli, rewind_xyk_state_to_before_fee_discounts, start_sandbox_with_xyk_pools};
use intear_dex_test_support::get_compiled_wasms;
use intear_dex_types::{AssetId, DexId};
use near_sdk::json_types::Base64VecU8;
use near_workspaces::types::{KeyType, SecretKey};
use xyk_dex_types::RegisterFeeAssetsArgs;

/// Base64 of the borsh encoding, the way dexes take arguments
fn dex_args(args: &impl near_sdk::borsh::BorshSerialize) -> String {
    serde_json::to_value(Base64VecU8(near_sdk::borsh::to_vec(args).unwrap()))
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

/// Deploys of the xyk code change size with every change to it, and so do
/// the storage deposits for it
fn without_code_size(record: String) -> String {
    [
        (r"\d+ bytes of code", "[SIZE] bytes of code"),
        (r"<\d+ characters>", "<[LENGTH] characters>"),
        (r"\d+(\.\d+)? NEAR", "[AMOUNT] NEAR"),
        (r"\d[\d,]*(\.\d+)? NEAR", "[AMOUNT] NEAR"),
    ]
    .iter()
    .fold(record, |record, (pattern, replacement)| {
        regex::Regex::new(pattern)
            .unwrap()
            .replace_all(&record, *replacement)
            .into_owned()
    })
}

/// cargo's progress lines change with what it has to rebuild
fn without_cargo_progress(record: String) -> String {
    record
        .lines()
        .filter(|line| {
            !regex::Regex::new(r"^\s+(Compiling|Finished|Blocking|Updating|Downloaded|Downloading|Locking|Adding)\b")
                .unwrap()
                .is_match(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn dex_lifecycle() {
    let sandbox = start_sandbox_with_xyk_pools().await;
    let cli = Cli::connected_to(&sandbox);

    insta::assert_snapshot!(
        "dex_storage_show",
        cli.view(&["dex", "storage", "show", "slimedragon.near/xyk"])
    );
    insta::assert_snapshot!(
        "dex_storage_show_of_dex_without_storage",
        cli.view(&["dex", "storage", "show", "alice.near/xyk"])
    );
    insta::assert_snapshot!(
        "dex_storage_deposit_for_missing_dex_of_another_deployer",
        cli.transaction(
            "alice.near",
            &[
                "dex",
                "storage",
                "deposit",
                "alice.near",
                "slimedragon.near/xykk",
                "1 NEAR"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "dex_storage_deposit",
        cli.transaction(
            "alice.near",
            &[
                "dex",
                "storage",
                "deposit",
                "alice.near",
                "slimedragon.near/xyk",
                "0.5 NEAR"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "dex_storage_withdraw_by_account_other_than_deployer",
        cli.transaction(
            "alice.near",
            &[
                "dex",
                "storage",
                "withdraw",
                "alice.near",
                "slimedragon.near/xyk",
                "all"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "dex_storage_withdraw",
        cli.transaction(
            "slimedragon.near",
            &[
                "dex",
                "storage",
                "withdraw",
                "slimedragon.near",
                "slimedragon.near/xyk",
                "0.5 NEAR"
            ]
        )
        .await
    );

    insta::assert_snapshot!(
        "dex_view",
        cli.view(&["dex", "view", "slimedragon.near/xyk", "get_pool_count", ""])
    );
    insta::assert_snapshot!(
        "dex_view_json",
        cli.view(&[
            "--json",
            "dex",
            "view",
            "slimedragon.near/xyk",
            "get_pool_count",
            ""
        ])
    );
    insta::assert_snapshot!(
        "dex_call_of_swap",
        cli.transaction(
            "alice.near",
            &[
                "dex",
                "call",
                "alice.near",
                "slimedragon.near/xyk",
                "swap",
                ""
            ]
        )
        .await
    );
    // alice.near already collects fees in the assets of its pools
    let register_wrapped_near_fees = dex_args(&RegisterFeeAssetsArgs {
        asset_ids: vec![AssetId::Nep141("wrap.near".parse().unwrap())],
    });
    insta::assert_snapshot!(
        "dex_call_with_attached_near",
        cli.transaction(
            "alice.near",
            &[
                "dex",
                "call",
                "alice.near",
                "slimedragon.near/xyk",
                "register_fee_assets",
                &register_wrapped_near_fees,
                "--attach",
                "near=0.01 NEAR",
                "--prepaid-gas",
                "60 Tgas",
            ]
        )
        .await
    );

    let wasm_directory = tempfile::tempdir().unwrap();
    let xyk_wasm_path = wasm_directory.path().join("xyk_dex.wasm");
    std::fs::write(&xyk_wasm_path, &get_compiled_wasms().await.xyk_dex_wasm).unwrap();
    let xyk_wasm_path = xyk_wasm_path.to_str().unwrap();
    let not_wasm_path = wasm_directory.path().join("readme.txt");
    std::fs::write(&not_wasm_path, "not a wasm module").unwrap();
    let fee_discount_signer = SecretKey::from_seed(KeyType::ED25519, "fee-discount-signer")
        .public_key()
        .to_string();

    insta::assert_snapshot!(
        "xyk_deploy_of_file_that_is_not_wasm",
        cli.transaction(
            "slimedragon.near",
            &[
                "xyk",
                "deploy",
                "slimedragon.near",
                "use-file",
                not_wasm_path.to_str().unwrap()
            ]
        )
        .await
        .replace(wasm_directory.path().to_str().unwrap(), "[TEMP]")
    );
    insta::assert_snapshot!(
        "xyk_deploy_by_account_other_than_trusted_code_deployer",
        without_code_size(
            cli.transaction(
                "alice.near",
                &["xyk", "deploy", "alice.near", "use-file", xyk_wasm_path]
            )
            .await
        )
        .replace(xyk_wasm_path, "[TEMP]/xyk_dex.wasm")
    );
    insta::assert_snapshot!(
        "xyk_deploy_with_dex_option",
        cli.transaction(
            "slimedragon.near",
            &[
                "xyk",
                "--dex",
                "slimedragon.near/xyk-test",
                "deploy",
                "slimedragon.near",
                "use-file",
                xyk_wasm_path
            ]
        )
        .await
        .replace(xyk_wasm_path, "[TEMP]/xyk_dex.wasm")
    );
    insta::assert_snapshot!(
        "xyk_deploy_new_dex",
        without_code_size(
            cli.transaction(
                "slimedragon.near",
                &[
                    "xyk",
                    "deploy",
                    "slimedragon.near",
                    "--name",
                    "xyk-test",
                    "use-file",
                    xyk_wasm_path
                ]
            )
            .await
        )
        .replace(xyk_wasm_path, "[TEMP]/xyk_dex.wasm")
    );
    insta::assert_snapshot!(
        "xyk_pools_count_before_init",
        cli.view(&[
            "xyk",
            "--dex",
            "slimedragon.near/xyk-test",
            "pools",
            "count"
        ])
    );
    insta::assert_snapshot!(
        "xyk_deploy_with_migration_of_dex_without_state",
        cli.transaction(
            "slimedragon.near",
            &[
                "xyk",
                "deploy",
                "slimedragon.near",
                "--name",
                "xyk-test",
                "--migrate",
                "--fee-discount-signer",
                &fee_discount_signer,
                "use-file",
                xyk_wasm_path
            ]
        )
        .await
        .replace(xyk_wasm_path, "[TEMP]/xyk_dex.wasm")
    );
    insta::assert_snapshot!(
        "xyk_init",
        cli.transaction(
            "alice.near",
            &[
                "xyk",
                "--dex",
                "slimedragon.near/xyk-test",
                "init",
                "alice.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "xyk_init_when_initialized",
        cli.transaction(
            "alice.near",
            &[
                "xyk",
                "--dex",
                "slimedragon.near/xyk-test",
                "init",
                "alice.near"
            ]
        )
        .await
    );

    let test_dex_id: DexId = "slimedragon.near/xyk-test".parse().unwrap();
    rewind_xyk_state_to_before_fee_discounts(&sandbox, &test_dex_id).await;
    // Views read the final block, a few blocks after the one with the patch
    sandbox.fast_forward(10).await.unwrap();
    insta::assert_snapshot!(
        "xyk_pools_count_of_state_before_migration",
        cli.view(&[
            "xyk",
            "--dex",
            "slimedragon.near/xyk-test",
            "pools",
            "count"
        ])
    );
    let deploy_test_dex_code = |code_path: &str, migrates: bool| {
        let mut args = vec!["xyk", "deploy", "slimedragon.near", "--name", "xyk-test"];
        if migrates {
            args.extend(["--migrate", "--fee-discount-signer", &fee_discount_signer]);
        }
        args.extend(["use-file", code_path]);
        args.into_iter().map(String::from).collect::<Vec<_>>()
    };
    insta::assert_snapshot!(
        "xyk_deploy_upgrade_that_cant_read_the_state",
        without_code_size(
            cli.transaction(
                "slimedragon.near",
                &deploy_test_dex_code(xyk_wasm_path, false)
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            )
            .await
        )
        .replace(xyk_wasm_path, "[TEMP]/xyk_dex.wasm")
    );
    insta::assert_snapshot!(
        "xyk_deploy_with_migration",
        without_code_size(
            cli.transaction(
                "slimedragon.near",
                &deploy_test_dex_code(xyk_wasm_path, true)
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            )
            .await
        )
        .replace(xyk_wasm_path, "[TEMP]/xyk_dex.wasm")
    );
    insta::assert_snapshot!(
        "xyk_pools_count_after_migration",
        cli.view(&[
            "xyk",
            "--dex",
            "slimedragon.near/xyk-test",
            "pools",
            "count"
        ])
    );

    // The smallest valid wasm module: no functions, so no migrate method
    let empty_wasm_path = wasm_directory.path().join("empty.wasm");
    std::fs::write(&empty_wasm_path, b"\0asm\x01\0\0\0").unwrap();
    let empty_wasm_path = empty_wasm_path.to_str().unwrap();
    insta::assert_snapshot!(
        "xyk_deploy_with_migration_of_code_without_migrate_method",
        cli.transaction(
            "slimedragon.near",
            &deploy_test_dex_code(empty_wasm_path, true)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        )
        .await
        .replace(empty_wasm_path, "[TEMP]/empty.wasm")
    );
    // The migration failed in the deploy's receipt, so the xyk code stayed
    insta::assert_snapshot!(
        "xyk_pools_count_after_failed_migration",
        cli.view(&[
            "xyk",
            "--dex",
            "slimedragon.near/xyk-test",
            "pools",
            "count"
        ])
    );

    insta::assert_snapshot!(
        "xyk_deploy_upgrade",
        without_code_size(
            cli.transaction(
                "slimedragon.near",
                &[
                    "xyk",
                    "deploy",
                    "slimedragon.near",
                    "--name",
                    "xyk-test",
                    "use-file",
                    xyk_wasm_path
                ]
            )
            .await
        )
        .replace(xyk_wasm_path, "[TEMP]/xyk_dex.wasm")
    );
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_str()
        .unwrap();
    insta::assert_snapshot!(
        "xyk_deploy_build",
        without_cargo_progress(without_code_size(
            cli.transaction(
                "slimedragon.near",
                &["xyk", "deploy", "slimedragon.near", "build"]
            )
            .await
        ))
        .replace(workspace_root, "[WORKSPACE]")
    );
}
