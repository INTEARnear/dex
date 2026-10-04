mod common;

use common::{
    Cli, engine_balance, fixture_account, start_sandbox_with_xyk_pools,
    transaction_with_registration,
};
use near_sdk::json_types::U128;
use near_workspaces::network::Sandbox;
use near_workspaces::{AccountId, Worker};
use serde_json::json;

const ENGINE: &str = "dex.intear.near";

async fn engine_is_paused(sandbox: &Worker<Sandbox>) -> bool {
    let engine_id: AccountId = ENGINE.parse().unwrap();
    sandbox
        .view(&engine_id, "is_paused")
        .await
        .unwrap()
        .json::<bool>()
        .unwrap()
}

#[tokio::test]
async fn writes() {
    let sandbox = start_sandbox_with_xyk_pools().await;
    let cli = Cli::connected_to(&sandbox);

    insta::assert_snapshot!(
        "missing_argument_without_terminal",
        cli.run_as_given(&["engine", "pause"])
    );

    insta::assert_snapshot!(
        "pause_by_account_outside_can_pause",
        cli.transaction("alice.near", &["engine", "pause", "alice.near"])
            .await
    );
    insta::assert_snapshot!(
        "unpause_when_not_paused",
        cli.transaction(
            "pause.slimedragon.near",
            &["engine", "unpause", "pause.slimedragon.near"]
        )
        .await
    );
    insta::assert_snapshot!(
        "pause",
        cli.transaction(
            "pause.slimedragon.near",
            &["engine", "pause", "pause.slimedragon.near"]
        )
        .await
    );
    assert!(engine_is_paused(&sandbox).await);
    insta::assert_snapshot!(
        "pause_when_paused",
        cli.transaction(
            "pause.slimedragon.near",
            &["engine", "pause", "pause.slimedragon.near"]
        )
        .await
    );
    insta::assert_snapshot!(
        "withdraw_while_paused",
        cli.transaction(
            "alice.near",
            &["engine", "withdraw", "alice.near", "near", "1 NEAR"]
        )
        .await
    );
    insta::assert_snapshot!(
        "unpause",
        cli.transaction(
            "slimedragon.near",
            &["engine", "unpause", "slimedragon.near"]
        )
        .await
    );
    assert!(!engine_is_paused(&sandbox).await);

    insta::assert_snapshot!(
        "storage_deposit_below_minimum",
        cli.transaction(
            "bob.near",
            &["engine", "storage", "deposit", "bob.near", "0.001 NEAR"]
        )
        .await
    );
    insta::assert_snapshot!(
        "storage_deposit",
        cli.transaction(
            "bob.near",
            &["engine", "storage", "deposit", "bob.near", "0.01 NEAR"]
        )
        .await
    );
    insta::assert_snapshot!(
        "storage_withdraw_more_than_available",
        cli.transaction(
            "bob.near",
            &["engine", "storage", "withdraw", "bob.near", "1 NEAR"]
        )
        .await
    );
    insta::assert_snapshot!(
        "storage_withdraw_all",
        cli.transaction(
            "bob.near",
            &["engine", "storage", "withdraw", "bob.near", "all"]
        )
        .await
    );
    insta::assert_snapshot!(
        "storage_withdraw_when_nothing_is_available",
        cli.transaction(
            "bob.near",
            &["engine", "storage", "withdraw", "bob.near", "all"]
        )
        .await
    );

    insta::assert_snapshot!(
        "assets_register_with_storage_top_up",
        transaction_with_registration(
            &sandbox,
            &cli,
            "bob.near",
            &[
                "engine",
                "assets",
                "register",
                "bob.near",
                "near,nep141:usdt.tether-token.near"
            ]
        )
        .await
    );
    assert_eq!(
        engine_balance(&sandbox, json!({ "Account": "bob.near" }), "near").await,
        Some(0)
    );
    insta::assert_snapshot!(
        "assets_register_when_registered",
        cli.transaction(
            "alice.near",
            &["engine", "assets", "register", "alice.near", "near"]
        )
        .await
    );
    insta::assert_snapshot!(
        "assets_register_for_another_account",
        transaction_with_registration(
            &sandbox,
            &cli,
            "slimedragon.near",
            &[
                "engine",
                "assets",
                "register",
                "slimedragon.near",
                "nep141:intel.tkn.near",
                "--for",
                "carol.near",
            ]
        )
        .await
    );
    assert_eq!(
        engine_balance(
            &sandbox,
            json!({ "Account": "carol.near" }),
            "nep141:intel.tkn.near"
        )
        .await,
        Some(0)
    );

    insta::assert_snapshot!(
        "deposit_token",
        cli.transaction(
            "slimedragon.near",
            &[
                "engine",
                "deposit",
                "slimedragon.near",
                "nep141:usdt.tether-token.near",
                "10 USDt"
            ]
        )
        .await
    );
    assert_eq!(
        engine_balance(
            &sandbox,
            json!({ "Account": "slimedragon.near" }),
            "nep141:usdt.tether-token.near"
        )
        .await,
        Some(3_710_000_000)
    );
    insta::assert_snapshot!(
        "deposit_unregistered_token",
        cli.transaction(
            "bob.near",
            &[
                "engine",
                "deposit",
                "bob.near",
                "nep141:intel.tkn.near",
                "1 INTEL"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "deposit_more_than_the_wallet_has",
        cli.transaction(
            "alice.near",
            &[
                "engine",
                "deposit",
                "alice.near",
                "nep141:usdt.tether-token.near",
                "10 USDt"
            ]
        )
        .await
    );
    // carol.near hasn't paid for any transaction yet, so its balance is
    // the same in every run
    insta::assert_snapshot!(
        "deposit_more_near_than_the_wallet_has",
        cli.transaction(
            "carol.near",
            &["engine", "deposit", "carol.near", "near", "1000 NEAR"]
        )
        .await
    );
    insta::assert_snapshot!(
        "deposit_near_with_registration",
        transaction_with_registration(
            &sandbox,
            &cli,
            "carol.near",
            &["engine", "deposit", "carol.near", "near", "1 NEAR"]
        )
        .await
    );
    assert_eq!(
        engine_balance(&sandbox, json!({ "Account": "carol.near" }), "near").await,
        Some(10u128.pow(24))
    );
    insta::assert_snapshot!(
        "deposit_unsupported_asset",
        cli.transaction(
            "alice.near",
            &[
                "engine",
                "deposit",
                "alice.near",
                "nep171:nft.near:1",
                "1 raw"
            ]
        )
        .await
    );

    insta::assert_snapshot!(
        "withdraw_to_account_not_registered_with_token",
        cli.transaction(
            "alice.near",
            &[
                "engine",
                "withdraw",
                "alice.near",
                "nep141:usdt.tether-token.near",
                "1 USDt",
                "--to",
                "bob.near",
            ]
        )
        .await
    );
    // bob.near has USDt registered on the engine, so the failed withdrawal
    // went into its balance there
    assert_eq!(
        engine_balance(
            &sandbox,
            json!({ "Account": "alice.near" }),
            "nep141:usdt.tether-token.near"
        )
        .await,
        Some(249_500_000)
    );
    assert_eq!(
        engine_balance(
            &sandbox,
            json!({ "Account": "bob.near" }),
            "nep141:usdt.tether-token.near"
        )
        .await,
        Some(1_000_000)
    );
    insta::assert_snapshot!(
        "withdraw",
        cli.transaction(
            "alice.near",
            &[
                "engine",
                "withdraw",
                "alice.near",
                "nep141:usdt.tether-token.near",
                "1 USDt"
            ]
        )
        .await
    );
    assert_eq!(
        engine_balance(
            &sandbox,
            json!({ "Account": "alice.near" }),
            "nep141:usdt.tether-token.near"
        )
        .await,
        Some(248_500_000)
    );
    insta::assert_snapshot!(
        "withdraw_more_than_the_balance",
        cli.transaction(
            "alice.near",
            &["engine", "withdraw", "alice.near", "near", "1000 NEAR"]
        )
        .await
    );
    insta::assert_snapshot!(
        "withdraw_unregistered_asset",
        cli.transaction(
            "bob.near",
            &[
                "engine",
                "withdraw",
                "bob.near",
                "nep141:intel.tkn.near",
                "all"
            ]
        )
        .await
    );

    insta::assert_snapshot!(
        "transfer",
        cli.transaction(
            "alice.near",
            &[
                "engine",
                "transfer",
                "alice.near",
                "nep141:usdt.tether-token.near",
                "5 USDt",
                "to",
                "bob.near",
            ]
        )
        .await
    );
    assert_eq!(
        engine_balance(
            &sandbox,
            json!({ "Account": "bob.near" }),
            "nep141:usdt.tether-token.near"
        )
        .await,
        Some(6_000_000)
    );
    insta::assert_snapshot!(
        "transfer_with_receiver_registration",
        transaction_with_registration(
            &sandbox,
            &cli,
            "alice.near",
            &[
                "engine",
                "transfer",
                "alice.near",
                "nep141:usdt.tether-token.near",
                "0.5 USDt",
                "to",
                "carol.near",
            ]
        )
        .await
    );
    assert_eq!(
        engine_balance(
            &sandbox,
            json!({ "Account": "carol.near" }),
            "nep141:usdt.tether-token.near"
        )
        .await,
        Some(500_000)
    );
    insta::assert_snapshot!(
        "transfer_to_self",
        cli.transaction(
            "alice.near",
            &[
                "engine",
                "transfer",
                "alice.near",
                "near",
                "1 NEAR",
                "to",
                "alice.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "transfer_to_missing_dex",
        cli.transaction(
            "alice.near",
            &[
                "engine",
                "transfer",
                "alice.near",
                "near",
                "0.1 NEAR",
                "to",
                "slimedragon.near/xykk",
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "transfer_json",
        cli.transaction(
            "alice.near",
            &[
                "--json",
                "engine",
                "transfer",
                "alice.near",
                "near",
                "0.1 NEAR",
                "to",
                "slimedragon.near/xyk",
            ]
        )
        .await
    );

    insta::assert_snapshot!(
        "signed_with_a_key_the_account_does_not_have",
        cli.transaction(
            "bob.near",
            &["engine", "storage", "deposit", "alice.near", "0.01 NEAR"]
        )
        .await
    );
}

#[tokio::test]
async fn writes_without_engine() {
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let cli = Cli::connected_to(&sandbox);

    insta::assert_snapshot!(
        "pause_without_engine",
        cli.transaction(
            "pause.slimedragon.near",
            &["engine", "pause", "pause.slimedragon.near"]
        )
        .await
    );
}

/// Records the outcome of a deposit that the engine refuses in
/// ft_on_transfer, which preflight checks keep the CLI from sending, for the
/// unit tests of outcome/:
/// cargo test --test writes record_refused_deposit_outcome -- --ignored
#[tokio::test]
#[ignore]
async fn record_refused_deposit_outcome() {
    let sandbox = start_sandbox_with_xyk_pools().await;
    let intel_id: AccountId = "intel.tkn.near".parse().unwrap();
    let slimedragon = fixture_account(&sandbox, "slimedragon.near");
    let bob = fixture_account(&sandbox, "bob.near");
    let one_intel = U128(10u128.pow(18));
    // bob.near holds INTEL but hasn't registered it on the engine
    slimedragon
        .call(&intel_id, "storage_deposit")
        .args_json(json!({ "account_id": "bob.near" }))
        .deposit(near_workspaces::types::NearToken::from_millinear(10))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    slimedragon
        .call(&intel_id, "ft_transfer")
        .args_json(json!({ "receiver_id": "bob.near", "amount": one_intel }))
        .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let refused_deposit = bob
        .call(&intel_id, "ft_transfer_call")
        .args_json(json!({ "receiver_id": ENGINE, "amount": one_intel, "msg": "" }))
        .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
        .gas(near_workspaces::types::Gas::from_tgas(50))
        .transact()
        .await
        .unwrap();
    let response = near_jsonrpc_client::JsonRpcClient::connect(sandbox.rpc_addr())
        .call(
            near_jsonrpc_client::methods::tx::RpcTransactionStatusRequest {
                transaction_info:
                    near_jsonrpc_client::methods::tx::TransactionInfo::TransactionId {
                        tx_hash: refused_deposit
                            .outcome()
                            .transaction_hash
                            .to_string()
                            .parse()
                            .unwrap(),
                        sender_account_id: "bob.near".parse().unwrap(),
                    },
                wait_until: near_primitives::views::TxExecutionStatus::Final,
            },
        )
        .await
        .unwrap();
    std::fs::write(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/outcome/refused_deposit_outcome.json"
        ),
        serde_json::to_string_pretty(&response.final_execution_outcome.unwrap().into_outcome())
            .unwrap(),
    )
    .unwrap();
}
