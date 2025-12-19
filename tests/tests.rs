use intear_dex::{internal_asset_operations::AccountOrDexId, internal_operations::Operation};
use intear_dex_types::{AssetId, DexId, SwapRequestAmount};
use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds};
use near_sdk::serde_json::json;
use near_sdk::{
    AccountId, NearToken,
    base64::{Engine, prelude::BASE64_STANDARD},
    json_types::{Base64VecU8, U128},
    near,
};
use near_workspaces::{Account, Contract};
use std::collections::HashMap;
use tokio::process::Command;
use tokio::sync::OnceCell;

struct CompiledWasms {
    contract_wasm: Vec<u8>,
    example_dex_wasm: Vec<u8>,
    minimal_dex_wasm: Vec<u8>,
    ft_wasm: Vec<u8>,
}

static COMPILED_WASMS: OnceCell<CompiledWasms> = OnceCell::const_new();

async fn get_compiled_wasms() -> &'static CompiledWasms {
    COMPILED_WASMS
        .get_or_init(|| async {
            println!("Compiling intear-dex");
            let contract_wasm = near_workspaces::compile_project("./").await.unwrap();

            println!("Compiling example-dex");
            assert!(
                Command::new("cargo")
                    .args([
                        "build",
                        "--package=example-dex",
                        "--release",
                        "--target",
                        "wasm32-unknown-unknown"
                    ])
                    .status()
                    .await
                    .unwrap()
                    .success()
            );

            println!("Compiling smallest-possible-dex");
            assert!(
                Command::new("cargo")
                    .args([
                        "build",
                        "--package=smallest-possible-dex",
                        "--release",
                        "--target",
                        "wasm32-unknown-unknown"
                    ])
                    .status()
                    .await
                    .unwrap()
                    .success()
            );
            println!("Compilation complete");

            let example_dex_wasm =
                std::fs::read("./target/wasm32-unknown-unknown/release/example_dex.wasm").unwrap();
            let minimal_dex_wasm =
                std::fs::read("./target/wasm32-unknown-unknown/release/smallest_possible_dex.wasm")
                    .unwrap();
            let ft_wasm = include_bytes!("./ft.wasm").to_vec();

            CompiledWasms {
                contract_wasm,
                example_dex_wasm,
                minimal_dex_wasm,
                ft_wasm,
            }
        })
        .await
}

#[tokio::test]
async fn test_minimal() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let dex_wasm = &wasms.minimal_dex_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let dex_deployer_account = sandbox.dev_create_account().await.unwrap();

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: dex_deployer_account.id().clone(),
        id: dex_id_string.clone(),
    };

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "deploy_dex_code")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "last_part_of_id": dex_id_string,
            "code_base64": BASE64_STANDARD.encode(dex_wasm),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let initial_near_balance = dex_deployer_account.view_account().await.unwrap().balance;
    let mut total_near_burnt = NearToken::from_yoctonear(0);
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        None,
    )
    .await
    .unwrap();

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near],
            "for": AccountOrDexId::Account(dex_deployer_account.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_near(20))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert_near_balance(
        &dex_deployer_account,
        initial_near_balance
            .saturating_sub(NearToken::from_near(20))
            .saturating_sub(total_near_burnt)
            .saturating_sub(NearToken::from_yoctonear(1)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        Some(U128(NearToken::from_near(20).as_yoctonear())),
    )
    .await
    .unwrap();

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near],
            "for": AccountOrDexId::Dex(dex_id.clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        AssetId::Near,
        Some(U128(0)),
    )
    .await
    .unwrap();
    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "transfer_asset")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "to": AccountOrDexId::Dex(dex_id.clone()),
            "asset_id": AssetId::Near,
            "amount": U128(1000),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        Some(U128(NearToken::from_near(20).as_yoctonear() - 1000)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        AssetId::Near,
        Some(U128(1000)),
    )
    .await
    .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        AssetId::Near,
        Some(U128(1000)),
    )
    .await
    .unwrap();
    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "swap_simple")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "message": BASE64_STANDARD.encode(vec![]),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Near,
            "amount": SwapRequestAmount::ExactIn(U128(10)),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    let result: (U128, U128) = result.json().unwrap();
    assert_eq!(result, (U128(10), U128(10)));
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        AssetId::Near,
        Some(U128(1000)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_storage_actions() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let user = sandbox.dev_create_account().await.unwrap();

    let storage_bounds = dex_engine_contract
        .view("storage_balance_bounds")
        .args_json(json!({}))
        .await
        .unwrap()
        .json::<StorageBalanceBounds>()
        .unwrap();
    assert_eq!(storage_bounds.min, NearToken::from_millinear(10));
    assert!(storage_bounds.max.is_none());

    let storage_balance_before = dex_engine_contract
        .view("storage_balance_of")
        .args_json(json!({ "account_id": user.id() }))
        .await
        .unwrap()
        .json::<Option<StorageBalance>>()
        .unwrap();
    assert!(storage_balance_before.is_none());

    let result = user
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(1))
        .args_json(json!({
            "account_id": user.id(),
            "registration_only": true,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    let deposited_balance = result.json::<StorageBalance>().unwrap();
    assert_eq!(deposited_balance.total, storage_bounds.min);
    assert!(deposited_balance.available < deposited_balance.total);
    assert!(deposited_balance.available > NearToken::from_yoctonear(0));

    let storage_balance_after_deposit = dex_engine_contract
        .view("storage_balance_of")
        .args_json(json!({ "account_id": user.id() }))
        .await
        .unwrap()
        .json::<Option<StorageBalance>>()
        .unwrap()
        .unwrap();
    assert_eq!(storage_balance_after_deposit.total, deposited_balance.total);
    assert_eq!(
        storage_balance_after_deposit.available,
        deposited_balance.available
    );

    let result = user
        .call(dex_engine_contract.id(), "storage_withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "amount": deposited_balance.available,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    let balance_after_withdraw = result.json::<StorageBalance>().unwrap();
    assert_eq!(
        balance_after_withdraw.available,
        NearToken::from_yoctonear(0)
    );

    let result = user
        .call(dex_engine_contract.id(), "storage_unregister")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "force": false,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    assert!(result.json::<bool>().unwrap());

    let storage_balance_after_unregister = dex_engine_contract
        .view("storage_balance_of")
        .args_json(json!({ "account_id": user.id() }))
        .await
        .unwrap()
        .json::<Option<StorageBalance>>()
        .unwrap();
    assert!(storage_balance_after_unregister.is_none());
}

#[tokio::test]
async fn test_dex_storage_actions() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let dex_deployer_account = sandbox.dev_create_account().await.unwrap();
    let intruder_account = sandbox.dev_create_account().await.unwrap();

    let dex_id = DexId {
        deployer: dex_deployer_account.id().clone(),
        id: "dex".to_string(),
    };

    let storage_bounds = dex_engine_contract
        .view("dex_storage_balance_bounds")
        .args_json(json!({}))
        .await
        .unwrap()
        .json::<StorageBalanceBounds>()
        .unwrap();
    assert_eq!(storage_bounds.min, NearToken::from_millinear(10));
    assert!(storage_bounds.max.is_none());

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "registration_only": true,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    let deposited_balance = result.json::<StorageBalance>().unwrap();
    assert_eq!(deposited_balance.total, storage_bounds.min);
    assert!(deposited_balance.available < deposited_balance.total);
    assert!(deposited_balance.available > NearToken::from_yoctonear(0));

    let result = intruder_account
        .call(dex_engine_contract.id(), "dex_storage_withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "amount": deposited_balance.available,
        }))
        .transact()
        .await
        .unwrap();
    assert!(!result.is_success());

    let storage_balance_after_deposit = dex_engine_contract
        .view("dex_storage_balance_of")
        .args_json(json!({ "dex_id": dex_id.clone() }))
        .await
        .unwrap()
        .json::<Option<StorageBalance>>()
        .unwrap()
        .unwrap();
    assert_eq!(
        storage_balance_after_deposit.available,
        deposited_balance.available
    );
    assert!(storage_balance_after_deposit.available > NearToken::from_yoctonear(0));

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "dex_storage_withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "amount": storage_balance_after_deposit.available,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    let balance_after_withdraw = result.json::<StorageBalance>().unwrap();
    assert_eq!(
        balance_after_withdraw.available,
        NearToken::from_yoctonear(0)
    );

    let storage_balance_after_withdraw = dex_engine_contract
        .view("dex_storage_balance_of")
        .args_json(json!({ "dex_id": dex_id }))
        .await
        .unwrap()
        .json::<Option<StorageBalance>>()
        .unwrap()
        .unwrap();
    assert_eq!(
        storage_balance_after_withdraw.available,
        balance_after_withdraw.available
    );
}

#[tokio::test]
async fn test_withdraw_failures() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let ft_wasm = &wasms.ft_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let ft = sandbox.dev_deploy(ft_wasm).await.unwrap();
    let user = sandbox.dev_create_account().await.unwrap();
    let receiver_unregistered = sandbox.dev_create_account().await.unwrap();
    let nonexistent_account: AccountId = "nonexistent.test.near".parse().unwrap();

    let result = ft
        .call("new_default_meta")
        .args_json(json!({
            "owner_id": user.id(),
            "total_supply": U128(NearToken::from_near(1_000_000_000).as_yoctonear()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(1))
        .args_json(json!({
            "account_id": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft.id().clone())],
            "for": AccountOrDexId::Account(user.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_near(2))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(ft.id(), "storage_deposit")
        .max_gas()
        .deposit("0.00125 NEAR".parse().unwrap())
        .args_json(json!({
            "account_id": dex_engine_contract.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    let result = user
        .call(ft.id(), "storage_deposit")
        .max_gas()
        .deposit("0.00125 NEAR".parse().unwrap())
        .args_json(json!({
            "account_id": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(ft.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(1_000_000_000),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Near,
        Some(U128(NearToken::from_near(2).as_yoctonear())),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000)),
    )
    .await
    .unwrap();

    let result = user
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "amount": U128(100_000_000),
            "withdraw_to": receiver_unregistered.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    assert!(!result.json::<bool>().unwrap());
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000)),
    )
    .await
    .unwrap();

    let result = user
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Near,
            "amount": U128(NearToken::from_near(1).as_yoctonear()),
            "withdraw_to": nonexistent_account,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    assert!(!result.json::<bool>().unwrap());
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Near,
        Some(U128(NearToken::from_near(2).as_yoctonear())),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_total_in_custody_consistency() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let ft_wasm = &wasms.ft_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let ft = sandbox.dev_deploy(ft_wasm).await.unwrap();
    let user = sandbox.dev_create_account().await.unwrap();
    let receiver_unregistered = sandbox.dev_create_account().await.unwrap();

    let result = ft
        .call("new_default_meta")
        .args_json(json!({
            "owner_id": user.id(),
            "total_supply": U128(NearToken::from_near(1_000_000_000).as_yoctonear()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(1))
        .args_json(json!({
            "account_id": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft.id().clone())],
            "for": AccountOrDexId::Account(user.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    assert_total_in_custody(&dex_engine_contract, AssetId::Near, Some(U128(0)))
        .await
        .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(0)),
    )
    .await
    .unwrap();

    let result = user
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_near(2))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(ft.id(), "storage_deposit")
        .max_gas()
        .deposit("0.00125 NEAR".parse().unwrap())
        .args_json(json!({
            "account_id": dex_engine_contract.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    let result = user
        .call(ft.id(), "storage_deposit")
        .max_gas()
        .deposit("0.00125 NEAR".parse().unwrap())
        .args_json(json!({
            "account_id": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(ft.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(1_000_000_000),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(NearToken::from_near(2).as_yoctonear())),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000)),
    )
    .await
    .unwrap();
    let result = user
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "amount": U128(100_000_000),
            "withdraw_to": receiver_unregistered.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    assert!(!result.json::<bool>().unwrap());
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000)),
    )
    .await
    .unwrap();

    let result = user
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Near,
            "amount": U128(NearToken::from_near(1).as_yoctonear()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    assert!(result.json::<bool>().unwrap());

    let result = user
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "amount": U128(500_000_000),
            "withdraw_to": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    assert!(result.json::<bool>().unwrap());

    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(NearToken::from_near(1).as_yoctonear())),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Near,
        Some(U128(NearToken::from_near(1).as_yoctonear())),
    )
    .await
    .unwrap();

    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(500_000_000)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(500_000_000)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_operations_with_near_deposit() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let dex_wasm = &wasms.minimal_dex_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let user = sandbox.dev_create_account().await.unwrap();

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: user.id().clone(),
        id: dex_id_string.clone(),
    };

    let result = user
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "account_id": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near],
            "for": AccountOrDexId::Account(user.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let transfer_amount = NearToken::from_millinear(1).as_yoctonear();
    let withdraw_amount = NearToken::from_millinear(2).as_yoctonear();
    let operations = vec![
        Operation::RegisterAssets {
            asset_ids: vec![AssetId::Near],
            r#for: Some(AccountOrDexId::Dex(DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            })),
        },
        Operation::DeployDexCode {
            last_part_of_id: dex_id_string.clone(),
            code_base64: Base64VecU8(dex_wasm.to_vec()),
        },
        Operation::TransferAsset {
            to: AccountOrDexId::Dex(DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            }),
            asset_id: AssetId::Near,
            amount: U128(transfer_amount),
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(vec![]),
            asset_in: AssetId::Near,
            asset_out: AssetId::Near,
            amount: Some(SwapRequestAmount::ExactIn(U128(
                NearToken::from_millinear(1).as_yoctonear(),
            ))),
        },
        Operation::Withdraw {
            asset_id: AssetId::Near,
            amount: Some(U128(withdraw_amount)),
            to: None,
        },
    ];

    let result = user
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_near(3))
        .args_json(json!({
            "operations": operations,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Near,
        Some(U128(
            NearToken::from_near(3)
                .as_yoctonear()
                .saturating_sub(transfer_amount)
                .saturating_sub(withdraw_amount),
        )),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(DexId {
            deployer: user.id().clone(),
            id: dex_id_string,
        }),
        AssetId::Near,
        Some(U128(transfer_amount)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(
            NearToken::from_near(3)
                .as_yoctonear()
                .saturating_sub(withdraw_amount),
        )),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_execute_operations() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let dex_wasm = &wasms.minimal_dex_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let user = sandbox.dev_create_account().await.unwrap();

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: user.id().clone(),
        id: dex_id_string.clone(),
    };

    let result = user
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "account_id": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near],
            "for": AccountOrDexId::Account(user.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_near(3))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let transfer_amount = NearToken::from_millinear(1).as_yoctonear();
    let withdraw_amount = NearToken::from_millinear(2).as_yoctonear();
    let operations = vec![
        Operation::RegisterAssets {
            asset_ids: vec![AssetId::Near],
            r#for: Some(AccountOrDexId::Dex(DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            })),
        },
        Operation::DeployDexCode {
            last_part_of_id: dex_id_string.clone(),
            code_base64: Base64VecU8(dex_wasm.to_vec()),
        },
        Operation::TransferAsset {
            to: AccountOrDexId::Dex(DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            }),
            asset_id: AssetId::Near,
            amount: U128(transfer_amount),
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(vec![]),
            asset_in: AssetId::Near,
            asset_out: AssetId::Near,
            amount: Some(SwapRequestAmount::ExactIn(U128(
                NearToken::from_millinear(1).as_yoctonear(),
            ))),
        },
        Operation::Withdraw {
            asset_id: AssetId::Near,
            amount: Some(U128(withdraw_amount)),
            to: None,
        },
    ];

    let result = user
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": operations,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Near,
        Some(U128(
            NearToken::from_near(3)
                .as_yoctonear()
                .saturating_sub(transfer_amount)
                .saturating_sub(withdraw_amount),
        )),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(DexId {
            deployer: user.id().clone(),
            id: dex_id_string,
        }),
        AssetId::Near,
        Some(U128(transfer_amount)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(
            NearToken::from_near(3)
                .as_yoctonear()
                .saturating_sub(withdraw_amount),
        )),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_execute_operations_failure_reverts() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let user = sandbox.dev_create_account().await.unwrap();

    let operations = vec![Operation::Withdraw {
        asset_id: AssetId::Near,
        amount: None,
        to: None,
    }];

    let result = user
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_near(1))
        .args_json(json!({
            "operations": operations,
        }))
        .transact()
        .await
        .unwrap();
    assert!(!result.is_success());

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Near,
        None,
    )
    .await
    .unwrap();
    assert_total_in_custody(&dex_engine_contract, AssetId::Near, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_ft_transfer_call_failure_reverts() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let ft_wasm = &wasms.ft_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let ft = sandbox.dev_deploy(ft_wasm).await.unwrap();
    let user = sandbox.dev_create_account().await.unwrap();

    let result = ft
        .call("new_default_meta")
        .args_json(json!({
            "owner_id": user.id(),
            "total_supply": U128(NearToken::from_near(10).as_yoctonear()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(1))
        .args_json(json!({
            "account_id": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Nep141(ft.id().clone())],
            "for": AccountOrDexId::Account(user.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(ft.id(), "storage_deposit")
        .max_gas()
        .deposit("0.00125 NEAR".parse().unwrap())
        .args_json(json!({
            "account_id": dex_engine_contract.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    let result = user
        .call(ft.id(), "storage_deposit")
        .max_gas()
        .deposit("0.00125 NEAR".parse().unwrap())
        .args_json(json!({
            "account_id": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let operations = vec![Operation::Withdraw {
        asset_id: AssetId::Nep141(ft.id().clone()),
        amount: Some(U128(1_000_000_000_000)),
        to: None,
    }];

    let initial_ft_balance = ft
        .view("ft_balance_of")
        .args_json(json!({
            "account_id": user.id(),
        }))
        .await
        .unwrap()
        .json::<U128>()
        .unwrap();

    let result = user
        .call(ft.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(1_000_000),
            "msg": near_sdk::serde_json::to_string(&operations).unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    assert!(result.json::<U128>().unwrap() == U128(0));

    assert_ft_balance(&user, ft.clone(), initial_ft_balance)
        .await
        .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(0)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(0)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_execute_operations_liquidity_and_swaps() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let dex_wasm = &wasms.example_dex_wasm;
    let ft_wasm = &wasms.ft_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let ft1 = sandbox.dev_deploy(ft_wasm).await.unwrap();
    let ft2 = sandbox.dev_deploy(ft_wasm).await.unwrap();
    let user = sandbox.dev_create_account().await.unwrap();

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: user.id().clone(),
        id: dex_id_string.clone(),
    };

    for ft in [&ft1, &ft2] {
        let result = ft
            .call("new_default_meta")
            .args_json(json!({
                "owner_id": user.id(),
                "total_supply": U128(NearToken::from_near(1_000_000_000).as_yoctonear()),
            }))
            .transact()
            .await
            .unwrap();
        assert!(result.is_success());
    }

    let result = user
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "account_id": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [
                AssetId::Near,
                AssetId::Nep141(ft1.id().clone()),
                AssetId::Nep141(ft2.id().clone())
            ],
            "for": AccountOrDexId::Account(user.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [
                AssetId::Near,
                AssetId::Nep141(ft1.id().clone()),
                AssetId::Nep141(ft2.id().clone())
            ],
            "for": AccountOrDexId::Dex(DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            }),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    for ft in [&ft1, &ft2] {
        let result = user
            .call(ft.id(), "storage_deposit")
            .max_gas()
            .deposit("0.00125 NEAR".parse().unwrap())
            .args_json(json!({
                "account_id": dex_engine_contract.id(),
            }))
            .transact()
            .await
            .unwrap();
        assert!(result.is_success());
        let result = user
            .call(ft.id(), "storage_deposit")
            .max_gas()
            .deposit("0.00125 NEAR".parse().unwrap())
            .args_json(json!({
                "account_id": user.id(),
            }))
            .transact()
            .await
            .unwrap();
        assert!(result.is_success());
    }

    let result = user
        .call(dex_engine_contract.id(), "deploy_dex_code")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "last_part_of_id": dex_id_string,
            "code_base64": BASE64_STANDARD.encode(dex_wasm),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    for ft in [&ft1, &ft2] {
        let result = user
            .call(ft.id(), "ft_transfer_call")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": dex_engine_contract.id(),
                "amount": U128(1_000_000),
                "msg": "",
            }))
            .transact()
            .await
            .unwrap();
        assert!(result.is_success());
    }

    #[near(serializers=[borsh])]
    struct CreatePoolArgs {
        assets: (AssetId, AssetId),
    }
    #[near(serializers=[borsh])]
    struct AddLiquidityArgs {
        pool_id: u64,
    }
    #[near(serializers=[borsh])]
    struct SwapArgs {
        pool_id: u64,
    }

    let swap_amount_in = NearToken::from_millinear(1);
    let lp1_near_amount = NearToken::from_near(1);
    let lp1_ft1_amount = 500_000;
    let lp2_ft1_amount = 200_000;
    let lp2_ft2_amount = 600_000;

    let operations = vec![
        Operation::DexCall {
            dex_id: DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            },
            method: "new".to_string(),
            args: Base64VecU8(vec![]),
            attached_assets: HashMap::new(),
        },
        Operation::DexCall {
            dex_id: DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            },
            method: "create_pool".to_string(),
            args: Base64VecU8(
                near_sdk::borsh::to_vec(&CreatePoolArgs {
                    assets: (AssetId::Near, AssetId::Nep141(ft1.id().clone())),
                })
                .unwrap(),
            ),
            attached_assets: HashMap::from_iter([(
                AssetId::Near,
                U128(NearToken::from_millinear(10).as_yoctonear()),
            )]),
        },
        Operation::DexCall {
            dex_id: DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            },
            method: "create_pool".to_string(),
            args: Base64VecU8(
                near_sdk::borsh::to_vec(&CreatePoolArgs {
                    assets: (
                        AssetId::Nep141(ft1.id().clone()),
                        AssetId::Nep141(ft2.id().clone()),
                    ),
                })
                .unwrap(),
            ),
            attached_assets: HashMap::from_iter([(
                AssetId::Near,
                U128(NearToken::from_millinear(10).as_yoctonear()),
            )]),
        },
        Operation::DexCall {
            dex_id: DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            },
            method: "add_liquidity".to_string(),
            args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: 0 }).unwrap()),
            attached_assets: HashMap::from_iter([
                (AssetId::Near, U128(lp1_near_amount.as_yoctonear())),
                (AssetId::Nep141(ft1.id().clone()), U128(lp1_ft1_amount)),
            ]),
        },
        Operation::DexCall {
            dex_id: DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            },
            method: "add_liquidity".to_string(),
            args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: 1 }).unwrap()),
            attached_assets: HashMap::from_iter([
                (AssetId::Nep141(ft1.id().clone()), U128(lp2_ft1_amount)),
                (AssetId::Nep141(ft2.id().clone()), U128(lp2_ft2_amount)),
            ]),
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs { pool_id: 0 }).unwrap()),
            asset_in: AssetId::Near,
            asset_out: AssetId::Nep141(ft1.id().clone()),
            amount: Some(SwapRequestAmount::ExactIn(U128(
                swap_amount_in.as_yoctonear(),
            ))),
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs { pool_id: 1 }).unwrap()),
            asset_in: AssetId::Nep141(ft1.id().clone()),
            asset_out: AssetId::Nep141(ft2.id().clone()),
            amount: None, // Use the output of the previous swap
        },
    ];

    let ft2_balance_before = user
        .view(dex_engine_contract.id(), "asset_balance_of")
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft2.id().clone()),
            "of": AccountOrDexId::Account(user.id().clone()),
        }))
        .await
        .unwrap()
        .json::<U128>()
        .unwrap();

    let result = user
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": operations,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let ft2_balance_after = user
        .view(dex_engine_contract.id(), "asset_balance_of")
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft2.id().clone()),
            "of": AccountOrDexId::Account(user.id().clone()),
        }))
        .await
        .unwrap()
        .json::<U128>()
        .unwrap();
    let ft2_balance_after_lp_add = ft2_balance_before.0.checked_sub(lp2_ft2_amount).unwrap();
    let amount_out = ft2_balance_after
        .0
        .checked_sub(ft2_balance_after_lp_add)
        .unwrap();
    assert_eq!(amount_out, 1493);

    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(NearToken::from_near(5).as_yoctonear())),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(1_000_000)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(1_000_000)),
    )
    .await
    .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        AssetId::Near,
        Some(U128(
            lp1_near_amount.as_yoctonear() + swap_amount_in.as_yoctonear(),
        )),
    )
    .await
    .unwrap();
    // same as it was, since it was an intermediate asset
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(lp1_ft1_amount + lp2_ft1_amount)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(lp2_ft2_amount - amount_out)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(1_000_000 - lp2_ft2_amount + amount_out)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_operations_with_ft_deposit() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let dex_wasm = &wasms.minimal_dex_wasm;
    let ft_wasm = &wasms.ft_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let ft = sandbox.dev_deploy(ft_wasm).await.unwrap();
    let user = sandbox.dev_create_account().await.unwrap();

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: user.id().clone(),
        id: dex_id_string.clone(),
    };

    let result = ft
        .call("new_default_meta")
        .args_json(json!({
            "owner_id": user.id(),
            "total_supply": U128(NearToken::from_near(1_000_000_000).as_yoctonear()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "account_id": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Nep141(ft.id().clone())],
            "for": AccountOrDexId::Account(user.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Nep141(ft.id().clone())],
            "for": AccountOrDexId::Dex(DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            }),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(dex_engine_contract.id(), "deploy_dex_code")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "last_part_of_id": dex_id_string,
            "code_base64": BASE64_STANDARD.encode(dex_wasm),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = user
        .call(ft.id(), "storage_deposit")
        .max_gas()
        .deposit("0.00125 NEAR".parse().unwrap())
        .args_json(json!({
            "account_id": dex_engine_contract.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    let result = user
        .call(ft.id(), "storage_deposit")
        .max_gas()
        .deposit("0.00125 NEAR".parse().unwrap())
        .args_json(json!({
            "account_id": user.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let operations = vec![
        Operation::TransferAsset {
            to: AccountOrDexId::Dex(DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            }),
            asset_id: AssetId::Nep141(ft.id().clone()),
            amount: U128(200_000),
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(vec![]),
            asset_in: AssetId::Nep141(ft.id().clone()),
            asset_out: AssetId::Nep141(ft.id().clone()),
            amount: Some(SwapRequestAmount::ExactIn(U128(100_000))),
        },
        Operation::Withdraw {
            asset_id: AssetId::Nep141(ft.id().clone()),
            amount: Some(U128(50_000)),
            to: Some(user.id().clone()),
        },
    ];

    let result = user
        .call(ft.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(1_000_000),
            "msg": near_sdk::serde_json::to_string(&operations).unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(750_000)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(DexId {
            deployer: user.id().clone(),
            id: dex_id_string,
        }),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(200_000)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(950_000)),
    )
    .await
    .unwrap();
}

/// Track tokens burnt from a transaction result and add to total_near_burnt.
fn track_tokens_burnt(
    result: &near_workspaces::result::ExecutionFinalResult,
    total_near_burnt: &mut NearToken,
) {
    let near_burnt = result
        .outcomes()
        .iter()
        .map(|o| o.tokens_burnt)
        .reduce(|a, b| a.saturating_add(b))
        .unwrap();
    *total_near_burnt = total_near_burnt.saturating_add(near_burnt)
}

/// Assert the balance of a NEP-141 token of an account.
async fn assert_ft_balance(
    account: &Account,
    token: Contract,
    amount: U128,
) -> Result<(), Box<dyn std::error::Error>> {
    let balance = token
        .view("ft_balance_of")
        .args_json(json!({
            "account_id": account.id(),
        }))
        .await?
        .json::<U128>()?;
    if balance != amount {
        return Err(format!(
            "FT balance mismatch: expected {}, actual {}",
            amount.0, balance.0
        )
        .into());
    }
    Ok(())
}

/// Assert the balance of NEAR of an account.
async fn assert_near_balance(
    account: &Account,
    amount: NearToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let account_details = account.view_account().await?;
    if account_details.balance != amount {
        return Err(format!(
            "NEAR balance mismatch: expected {}, actual {}",
            amount.as_yoctonear(),
            account_details.balance.as_yoctonear()
        )
        .into());
    }
    Ok(())
}

/// Assert the balance of an asset that is custodied by the dex
/// engine contract for a user or a dex.
async fn assert_inner_asset_balance(
    dex_engine_contract: &Contract,
    of: AccountOrDexId,
    asset: AssetId,
    amount: Option<U128>,
) -> Result<(), Box<dyn std::error::Error>> {
    let balance = dex_engine_contract
        .view("asset_balance_of")
        .args_json(json!({
            "of": of,
            "asset_id": asset,
        }))
        .await?
        .json::<Option<U128>>()?;
    if balance != amount {
        return Err(format!(
            "Inner asset balance mismatch: expected {:?}, actual {:?}",
            amount, balance
        )
        .into());
    }
    Ok(())
}

/// Assert the total amount of an asset tracked in custody.
async fn assert_total_in_custody(
    dex_engine_contract: &Contract,
    asset: AssetId,
    amount: Option<U128>,
) -> Result<(), Box<dyn std::error::Error>> {
    let total = dex_engine_contract
        .view("total_in_custody")
        .args_json(json!({
            "asset_id": asset,
        }))
        .await?
        .json::<Option<U128>>()?;
    if total != amount {
        return Err(format!(
            "Total in custody mismatch: expected {:?}, actual {:?}",
            amount, total
        )
        .into());
    }
    Ok(())
}

#[tokio::test]
async fn test_regular_flow() {
    let wasms = get_compiled_wasms().await;
    let contract_wasm = &wasms.contract_wasm;
    let dex_wasm = &wasms.example_dex_wasm;
    let ft_wasm = &wasms.ft_wasm;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let ft = sandbox.dev_deploy(ft_wasm).await.unwrap();
    let dex_deployer_account = sandbox.dev_create_account().await.unwrap();

    let result = ft
        .call("new_default_meta")
        .args_json(json!({
            "owner_id": dex_deployer_account.id(),
            "total_supply": U128(NearToken::from_near(100_000_000_000).as_yoctonear()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: dex_deployer_account.id().clone(),
        id: dex_id_string.clone(),
    };

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(NearToken::from_near(5))
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "deploy_dex_code")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "last_part_of_id": dex_id_string,
            "code_base64": BASE64_STANDARD.encode(dex_wasm),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());

    let initial_near_balance = dex_deployer_account.view_account().await.unwrap().balance;
    let mut total_near_burnt = NearToken::from_yoctonear(0);
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        None,
    )
    .await
    .unwrap();

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft.id().clone())],
            "for": AccountOrDexId::Dex(dex_id.clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        None,
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        None,
    )
    .await
    .unwrap();

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft.id().clone())],
            "for": AccountOrDexId::Account(dex_deployer_account.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_near(20))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert_near_balance(
        &dex_deployer_account,
        initial_near_balance
            .saturating_sub(NearToken::from_near(20))
            .saturating_sub(total_near_burnt)
            .saturating_sub(NearToken::from_yoctonear(2)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        Some(U128(NearToken::from_near(20).as_yoctonear())),
    )
    .await
    .unwrap();

    let result = dex_deployer_account
        .call(ft.id(), "storage_deposit")
        .max_gas()
        .deposit("0.00125 NEAR".parse().unwrap())
        .args_json(json!({
            "account_id": dex_engine_contract.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);

    let initial_ft_balance = ft
        .view("ft_balance_of")
        .args_json(json!({
            "account_id": dex_deployer_account.id(),
        }))
        .await
        .unwrap()
        .json::<U128>()
        .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(0)),
    )
    .await
    .unwrap();
    let result = dex_deployer_account
        .call(ft.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(1_000_000_000),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert_ft_balance(
        &dex_deployer_account,
        ft.clone(),
        U128(initial_ft_balance.0 - 1_000_000_000),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000)),
    )
    .await
    .unwrap();

    #[near(serializers=[borsh])]
    struct CreatePoolArgs {
        assets: (AssetId, AssetId),
    }
    type PoolId = u64;
    #[near(serializers=[borsh])]
    struct CreatePoolResponse {
        pool_id: PoolId,
    }
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        Some(U128(NearToken::from_near(20).as_yoctonear())),
    )
    .await
    .unwrap();

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "new",
            "args": BASE64_STANDARD.encode([]),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert!(result.json::<Base64VecU8>().unwrap().0.is_empty());

    let storage_usage_by_dex_before_pool_creation = dex_engine_contract
        .view("dex_storage_balance_of")
        .args_json(json!({
            "dex_id": dex_id.clone(),
        }))
        .await
        .unwrap()
        .json::<StorageBalance>()
        .unwrap();
    assert_eq!(
        storage_usage_by_dex_before_pool_creation.total,
        NearToken::from_yoctonear(5000000000000000000000000),
    );
    let storage_usage_by_engine_before_pool_creation = dex_engine_contract
        .view_account()
        .await
        .unwrap()
        .storage_usage;

    let near_for_create_pool = NearToken::from_millinear(10);
    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "create_pool",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&CreatePoolArgs {
                assets: (AssetId::Near, AssetId::Nep141(ft.id().clone())),
            }).unwrap()),
            "attached_assets": {
                "near": near_for_create_pool,
            },
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    let result = result.json::<Base64VecU8>().unwrap();
    let pool_id = near_sdk::borsh::from_slice::<CreatePoolResponse>(&result.0)
        .unwrap()
        .pool_id;

    let storage_usage_by_dex_after_pool_creation = dex_engine_contract
        .view("dex_storage_balance_of")
        .args_json(json!({
            "dex_id": dex_id.clone(),
        }))
        .await
        .unwrap()
        .json::<StorageBalance>()
        .unwrap();
    let storage_usage_by_engine_after_pool_creation = dex_engine_contract
        .view_account()
        .await
        .unwrap()
        .storage_usage;
    assert_eq!(
        storage_usage_by_dex_after_pool_creation.total,
        NearToken::from_yoctonear(5002400000000000000000000),
    );

    let pool_storage_cost = storage_usage_by_dex_after_pool_creation
        .total
        .checked_sub(storage_usage_by_dex_before_pool_creation.total)
        .unwrap();
    assert_eq!(
        storage_usage_by_dex_before_pool_creation.available,
        storage_usage_by_dex_after_pool_creation.available,
    );
    assert_eq!(
        near_sdk::env::storage_byte_cost()
            .checked_mul(
                storage_usage_by_engine_after_pool_creation
                    .checked_sub(storage_usage_by_engine_before_pool_creation)
                    .unwrap() as u128,
            )
            .unwrap(),
        pool_storage_cost,
    );

    assert_near_balance(
        &dex_deployer_account,
        initial_near_balance
            .saturating_sub(total_near_burnt)
            .saturating_sub("0.00125 NEAR".parse().unwrap())
            .saturating_sub(NearToken::from_near(20))
            .saturating_sub(NearToken::from_yoctonear(5)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        Some(U128(
            NearToken::from_near(20).as_yoctonear() - pool_storage_cost.as_yoctonear(),
        )),
    )
    .await
    .unwrap();

    #[near(serializers=[borsh])]
    struct AddLiquidityArgs {
        pool_id: PoolId,
    }
    #[near(serializers=[borsh])]
    struct AddLiquidityResponse;
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000)),
    )
    .await
    .unwrap();
    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "add_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&AddLiquidityArgs {
                pool_id,
            }).unwrap()),
            "attached_assets": {
                "near": U128(NearToken::from_near(1).as_yoctonear()),
                format!("nep141:{}", ft.id()): U128(1_000_000),
            },
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    let response = result.json::<Base64VecU8>().unwrap();
    let _ = near_sdk::borsh::from_slice::<AddLiquidityResponse>(&response.0).unwrap();
    assert_near_balance(
        &dex_deployer_account,
        initial_near_balance
            .saturating_sub(total_near_burnt)
            .saturating_sub("0.00125 NEAR".parse().unwrap())
            .saturating_sub(NearToken::from_near(20))
            .saturating_sub(NearToken::from_yoctonear(6)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        Some(U128(
            NearToken::from_near(20).as_yoctonear()
                - pool_storage_cost.as_yoctonear()
                - NearToken::from_near(1).as_yoctonear(),
        )),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000 - 1_000_000)),
    )
    .await
    .unwrap();

    #[near(serializers=[borsh])]
    struct SwapArgs {
        pool_id: PoolId,
    }
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000 - 1_000_000)),
    )
    .await
    .unwrap();
    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "swap_simple")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapArgs {
                pool_id,
            }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft.id().clone()),
            "amount": SwapRequestAmount::ExactIn(U128(NearToken::from_millinear(100).as_yoctonear())),
        }))
        .transact()
        .await.unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    let result: (U128, U128) = result.json().unwrap();
    assert_eq!(result, (U128(100000000000000000000000), U128(90909)));
    assert_near_balance(
        &dex_deployer_account,
        initial_near_balance
            .saturating_sub(total_near_burnt)
            .saturating_sub("0.00125 NEAR".parse().unwrap())
            .saturating_sub(NearToken::from_near(20))
            .saturating_sub(NearToken::from_yoctonear(7)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        Some(U128(
            NearToken::from_near(20).as_yoctonear()
                - pool_storage_cost.as_yoctonear()
                - NearToken::from_near(1).as_yoctonear()
                - NearToken::from_millinear(100).as_yoctonear(),
        )),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000 - 1_000_000 + 90909)),
    )
    .await
    .unwrap();

    #[near(serializers=[borsh])]
    struct RemoveLiquidityArgs {
        pool_id: PoolId,
        assets_to_remove: (U128, U128),
    }
    #[near(serializers=[borsh])]
    struct RemoveLiquidityResponse;
    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "remove_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                pool_id,
                assets_to_remove: (U128(NearToken::from_millinear(500).as_yoctonear()), U128(500_000)),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    let response = result.json::<Base64VecU8>().unwrap();
    let _ = near_sdk::borsh::from_slice::<RemoveLiquidityResponse>(&response.0).unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        Some(U128(
            NearToken::from_near(20).as_yoctonear()
                - pool_storage_cost.as_yoctonear()
                - NearToken::from_near(1).as_yoctonear()
                - NearToken::from_millinear(100).as_yoctonear()
                + NearToken::from_millinear(500).as_yoctonear(),
        )),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(1_000_000_000 - 1_000_000 + 90909 + 500_000)),
    )
    .await
    .unwrap();

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Near,
            "amount": U128(NearToken::from_near(1).as_yoctonear()),
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert!(result.json::<bool>().unwrap());
    sandbox.fast_forward(1).await.unwrap();
    assert_near_balance(
        &dex_deployer_account,
        initial_near_balance
            .saturating_sub(total_near_burnt)
            .saturating_sub("0.00125 NEAR".parse().unwrap())
            .saturating_sub(NearToken::from_near(20))
            .saturating_sub(NearToken::from_yoctonear(9))
            .saturating_add(NearToken::from_near(1)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Near,
        Some(U128(
            NearToken::from_near(20).as_yoctonear()
                - pool_storage_cost.as_yoctonear()
                - NearToken::from_near(1).as_yoctonear()
                - NearToken::from_millinear(100).as_yoctonear()
                + NearToken::from_millinear(500).as_yoctonear()
                - NearToken::from_near(1).as_yoctonear(),
        )),
    )
    .await
    .unwrap();

    let result = dex_deployer_account
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "amount": U128(100_000_000),
            "withdraw_to": null,
        }))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success());
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert!(result.json::<bool>().unwrap());
    assert_ft_balance(
        &dex_deployer_account,
        ft.clone(),
        U128(initial_ft_balance.0 - 1_000_000_000 + 100_000_000),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(dex_deployer_account.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(
            1_000_000_000 - 1_000_000 + 90909 + 500_000 - 100_000_000,
        )),
    )
    .await
    .unwrap();

    #[near(serializers=[borsh])]
    struct GetPoolArgs {
        pool_id: PoolId,
    }
    #[derive(PartialEq, Debug)]
    #[near(serializers=[borsh])]
    struct SimplePool {
        assets: (AssetWithBalance, AssetWithBalance),
        owner_id: AccountId,
    }
    #[derive(PartialEq, Debug)]
    #[near(serializers=[borsh])]
    struct AssetWithBalance {
        asset_id: AssetId,
        balance: U128,
    }
    let result = dex_engine_contract
        .view("dex_view")
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "get_pool",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&GetPoolArgs {
                pool_id,
            }).unwrap()),
        }))
        .await
        .unwrap();
    let pool_view_result = result.json::<Base64VecU8>().unwrap();
    let pool: Option<SimplePool> = near_sdk::borsh::from_slice(&pool_view_result.0).unwrap();
    assert_eq!(
        pool,
        Some(SimplePool {
            assets: (
                AssetWithBalance {
                    asset_id: AssetId::Near,
                    balance: U128(
                        NearToken::from_near(1).as_yoctonear()
                            + NearToken::from_millinear(100).as_yoctonear()
                            - NearToken::from_millinear(500).as_yoctonear()
                    ),
                },
                AssetWithBalance {
                    asset_id: AssetId::Nep141(ft.id().clone()),
                    balance: U128(1_000_000 - 90909 - 500_000),
                },
            ),
            owner_id: dex_deployer_account.id().clone(),
        })
    );
}
