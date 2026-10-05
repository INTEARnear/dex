mod common;

use common::{
    Cli, fixture_account, start_sandbox_with_mainnet_engine, start_sandbox_with_xyk_pools,
};
use intear_dex_test_support::get_compiled_wasms;
use intear_dex_types::{
    AccountOrDexId, AssetId, Operation, SwapOperationAmount, SwapRequestAmount, WithdrawAmount,
};
use near_sdk::json_types::{Base64VecU8, U128};
use near_workspaces::network::Sandbox;
use near_workspaces::types::NearToken;
use near_workspaces::{AccountId, Worker};
use serde_json::json;
use xyk_dex_types::SwapArgs;

const ENGINE: &str = "dex.intear.near";
const USDT: &str = "usdt.tether-token.near";
const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);

async fn call(
    sandbox: &Worker<Sandbox>,
    signer: &str,
    receiver_id: &str,
    method_name: &str,
    args: serde_json::Value,
) {
    let receiver_id: AccountId = receiver_id.parse().unwrap();
    fixture_account(sandbox, signer)
        .call(&receiver_id, method_name)
        .args_json(args)
        .deposit(ONE_YOCTO)
        .max_gas()
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    // The CLI reads final blocks
    sandbox.fast_forward(10).await.unwrap();
}

async fn total_storage_balances(sandbox: &Worker<Sandbox>) -> serde_json::Value {
    let engine_id: AccountId = ENGINE.parse().unwrap();
    sandbox
        .view(&engine_id, "total_storage_balances")
        .await
        .unwrap()
        .json()
        .unwrap()
}

#[tokio::test]
async fn admin() {
    let sandbox = start_sandbox_with_xyk_pools().await;
    let cli = Cli::connected_to(&sandbox);

    insta::assert_snapshot!(
        "set_trusted_code_deployer_by_account_other_than_the_deployer",
        cli.transaction(
            "alice.near",
            &[
                "engine",
                "admin",
                "set-trusted-code-deployer",
                "alice.near",
                "bob.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "set_trusted_code_deployer_to_the_deployer",
        cli.transaction(
            "slimedragon.near",
            &[
                "engine",
                "admin",
                "set-trusted-code-deployer",
                "slimedragon.near",
                "slimedragon.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "set_trusted_code_deployer_to_missing_account",
        cli.transaction(
            "slimedragon.near",
            &[
                "engine",
                "admin",
                "set-trusted-code-deployer",
                "slimedragon.near",
                "nobody.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "set_trusted_code_deployer",
        cli.transaction(
            "slimedragon.near",
            &[
                "engine",
                "admin",
                "set-trusted-code-deployer",
                "slimedragon.near",
                "bob.near"
            ]
        )
        .await
    );
    let handed_back = cli
        .transaction(
            "bob.near",
            &[
                "engine",
                "admin",
                "set-trusted-code-deployer",
                "bob.near",
                "slimedragon.near",
            ],
        )
        .await;
    assert!(
        handed_back.contains("slimedragon.near is now the trusted code deployer"),
        "{handed_back}"
    );

    // Tokens sent with ft_transfer instead of ft_transfer_call reach the
    // engine without a balance on it
    call(
        &sandbox,
        "slimedragon.near",
        USDT,
        "ft_transfer",
        json!({ "receiver_id": ENGINE, "amount": U128(7_500_000) }),
    )
    .await;
    insta::assert_snapshot!(
        "custody_of_token",
        cli.view(&[
            "engine",
            "admin",
            "custody",
            "nep141:usdt.tether-token.near"
        ])
    );
    insta::assert_snapshot!(
        "custody_of_token_json",
        cli.view(&[
            "--json",
            "engine",
            "admin",
            "custody",
            "nep141:usdt.tether-token.near"
        ])
    );
    // The engine earns a share of the gas burned in it, which changes with
    // every build
    insta::with_settings!({filters => vec![
        (r"(Untracked \(beyond custody and storage\) +)\S+ NEAR", "$1[BUILD DEPENDENT] NEAR"),
    ]}, {
        insta::assert_snapshot!(
            "custody_of_near",
            cli.view(&["engine", "admin", "custody", "near"])
        );
    });

    insta::assert_snapshot!(
        "rescue_while_not_paused",
        cli.transaction(
            ENGINE,
            &[
                "engine",
                "admin",
                "rescue",
                "nep141:usdt.tether-token.near",
                "all",
                "to",
                "alice.near"
            ]
        )
        .await
    );
    call(
        &sandbox,
        "pause.slimedragon.near",
        ENGINE,
        "pause",
        json!({}),
    )
    .await;
    insta::assert_snapshot!(
        "rescue_more_than_untracked",
        cli.transaction(
            ENGINE,
            &[
                "engine",
                "admin",
                "rescue",
                "nep141:usdt.tether-token.near",
                "10 USDt",
                "to",
                "alice.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "rescue_of_asset_without_untracked_amount",
        cli.transaction(
            ENGINE,
            &[
                "engine",
                "admin",
                "rescue",
                "nep141:intel.tkn.near",
                "all",
                "to",
                "alice.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "rescue_to_account_without_token_storage",
        cli.transaction(
            ENGINE,
            &[
                "engine",
                "admin",
                "rescue",
                "nep141:usdt.tether-token.near",
                "all",
                "to",
                "carol.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "rescue",
        cli.transaction(
            ENGINE,
            &[
                "engine",
                "admin",
                "rescue",
                "nep141:usdt.tether-token.near",
                "2.5 USDt",
                "to",
                "alice.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "rescue_all",
        cli.transaction(
            ENGINE,
            &[
                "engine",
                "admin",
                "rescue",
                "nep141:usdt.tether-token.near",
                "all",
                "to",
                "alice.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "rescue_of_near",
        cli.transaction(
            ENGINE,
            &[
                "engine", "admin", "rescue", "near", "1 NEAR", "to", "bob.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "custody_of_token_after_rescue",
        cli.view(&[
            "engine",
            "admin",
            "custody",
            "nep141:usdt.tether-token.near"
        ])
    );
}

#[tokio::test]
async fn operations() {
    let sandbox = start_sandbox_with_xyk_pools().await;
    let cli = Cli::connected_to(&sandbox);
    let operations_directory = tempfile::tempdir().unwrap();
    let operations_file = |file_name: &str, execute_operations_args: serde_json::Value| {
        let path = operations_directory.path().join(file_name);
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&execute_operations_args).unwrap(),
        )
        .unwrap();
        path.to_str().unwrap().to_string()
    };
    let batch = operations_file(
        "batch.json",
        json!({
            "operations": [
                Operation::RegisterAssets {
                    asset_ids: vec![AssetId::Near],
                    r#for: Some(AccountOrDexId::Account("bob.near".parse().unwrap())),
                },
                Operation::TransferAsset {
                    to: AccountOrDexId::Account("bob.near".parse().unwrap()),
                    asset_id: AssetId::Near,
                    amount: U128(NearToken::from_near(1).as_yoctonear()),
                },
                Operation::SwapSimple {
                    dex_id: "slimedragon.near/xyk".parse().unwrap(),
                    message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs { pool_id: 0 }).unwrap()),
                    asset_in: AssetId::Near,
                    asset_out: AssetId::Nep141(USDT.parse().unwrap()),
                    amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(
                        NearToken::from_near(1).as_yoctonear(),
                    ))),
                    constraint: Some(U128(2_000_000)),
                },
                Operation::Withdraw {
                    asset_id: AssetId::Nep141(USDT.parse().unwrap()),
                    amount: WithdrawAmount::PreviousSwapOutput,
                    to: None,
                    rescue_address: None,
                },
            ],
            "referrer": null,
        }),
    );
    let missing_field = operations_file(
        "missing-field.json",
        json!({ "operations": [{ "Withdraw": { "asset_id": "near" } }] }),
    );
    let misspelled_field = operations_file("misspelled-field.json", json!({ "operation": [] }));
    let no_operations = operations_file("no-operations.json", json!({ "operations": [] }));

    let missing_field_record = cli
        .transaction(
            "alice.near",
            &["engine", "operations", "run", "alice.near", &missing_field],
        )
        .await;
    let misspelled_field_record = cli
        .transaction(
            "alice.near",
            &[
                "engine",
                "operations",
                "run",
                "alice.near",
                &misspelled_field,
            ],
        )
        .await;
    let no_operations_record = cli
        .transaction(
            "alice.near",
            &["engine", "operations", "run", "alice.near", &no_operations],
        )
        .await;
    let batch_record = cli
        .transaction(
            "alice.near",
            &[
                "engine",
                "operations",
                "run",
                "alice.near",
                &batch,
                "--deposit",
                "2 NEAR",
            ],
        )
        .await;
    insta::with_settings!({filters => vec![
        (regex::escape(operations_directory.path().to_str().unwrap()).as_str(), "[TEMPORARY DIRECTORY]"),
    ]}, {
        insta::assert_snapshot!("operations_run_with_missing_field", missing_field_record);
        insta::assert_snapshot!("operations_run_with_misspelled_field", misspelled_field_record);
        insta::assert_snapshot!("operations_run_without_operations", no_operations_record);
        insta::assert_snapshot!("operations_run", batch_record);
    });
}

/// The engine's code as a file, for `engine admin migrate`
async fn engine_code_file() -> tempfile::NamedTempFile {
    let engine_code = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        engine_code.path(),
        &get_compiled_wasms().await.contract_wasm,
    )
    .unwrap();
    engine_code
}

#[tokio::test]
async fn migrate_checks() {
    let sandbox = start_sandbox_with_xyk_pools().await;
    let cli = Cli::connected_to(&sandbox);
    let engine_code = engine_code_file().await;
    let engine_code_path = engine_code.path().to_str().unwrap();
    insta::with_settings!({filters => vec![
        (regex::escape(engine_code_path).as_str(), "[ENGINE CODE]"),
        // The code changes with every build
        (r"to code \S+ \(\d+ bytes\)", "to code [HASH] ([BUILD DEPENDENT] bytes)"),
    ]}, {
        insta::assert_snapshot!(
            "migrate_by_account_outside_can_pause",
            cli.run_as_given(&[
                "engine",
                "admin",
                "migrate",
                "alice.near",
                "slimedragon.near",
                engine_code_path,
                "sign-with-legacy-keychain",
                "network-config",
                "sandbox",
            ])
        );
        // The sandbox's engine already runs this code with balances by owner
        insta::assert_snapshot!(
            "migrate_of_migrated_engine",
            cli.run_as_given(&[
                "engine",
                "admin",
                "migrate",
                "pause.slimedragon.near",
                "slimedragon.near",
                engine_code_path,
                "sign-with-legacy-keychain",
                "network-config",
                "sandbox",
            ])
        );
    });
}

/// The whole migration of the engine as it is on mainnet: pause, deploy with
/// migrate, move every balance, finish with the sums of storage balances,
/// unpause. Mainnet's state changes, so this checks the result instead of a
/// snapshot.
#[tokio::test]
async fn migrate_of_mainnet_engine() {
    let sandbox = start_sandbox_with_mainnet_engine().await;
    let cli = Cli::connected_to(&sandbox);
    cli.save_key_to_legacy_keychain("pause.slimedragon.near");
    cli.save_key_to_legacy_keychain(ENGINE);
    let engine_code = engine_code_file().await;
    let migrate = [
        "--json",
        "engine",
        "admin",
        "migrate",
        "pause.slimedragon.near",
        "slimedragon.near",
        engine_code.path().to_str().unwrap(),
        "sign-with-legacy-keychain",
        "network-config",
        "sandbox",
    ];
    let record = cli.run_as_given(&migrate);
    assert!(record.contains("exit code: 0"), "{record}");
    let summary: serde_json::Value = serde_json::from_str(
        record
            .split_once("--- stdout ---\n")
            .and_then(|(_, rest)| rest.split_once("--- stderr ---"))
            .map(|(stdout, _)| stdout)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(summary["paused"], json!(false));
    assert_eq!(summary["trusted_code_deployer"], json!("slimedragon.near"));

    sandbox.fast_forward(3).await.unwrap();
    let engine_id: AccountId = ENGINE.parse().unwrap();
    let view = |method_name: &'static str| {
        let sandbox = &sandbox;
        let engine_id = &engine_id;
        async move {
            sandbox
                .view(engine_id, method_name)
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .unwrap()
        }
    };
    assert_eq!(view("migration_progress").await, json!(null));
    assert_eq!(view("is_paused").await, json!(false));
    // The engine owes no more NEAR than it has
    view("untracked_near").await;
    let total_storage_balances = total_storage_balances(&sandbox).await;
    assert_eq!(
        total_storage_balances["users"]["total"],
        summary["user_storage_balances"]["total"]
    );
    assert_eq!(
        total_storage_balances["dexes"]["total"],
        summary["dex_storage_balances"]["total"]
    );

    // Running it again finds nothing left to do
    let record = cli.run_as_given(&migrate);
    assert!(
        record.contains("exit code: 0")
            && record.contains("already runs this code, and its balances are migrated"),
        "{record}"
    );
}
