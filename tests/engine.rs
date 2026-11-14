use crate::common::*;

use intear_dex::oracle::{
    GAS_FOR_AFTER_ORACLE_OPERATIONS_IN_SANDBOX, MAX_ORACLE_REQUEST_BYTES, MAX_ORACLE_RESPONSE_BYTES,
};
use intear_dex_common::{AssetId, DexId, FeeFraction, SwapRequestAmount, TokenAmount};
use intear_dex_types::{
    AccountOrDexId, DexCallAfterDeploy, Operation, SwapAdvancedMode, SwapOperationAmount,
    SwapOption, WithdrawAmount,
};
use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds};
use near_crypto::{KeyType, SecretKey};
use near_jsonrpc_client::errors::{JsonRpcError, JsonRpcServerError};
use near_jsonrpc_client::methods;
use near_jsonrpc_primitives::types::query::{QueryResponseKind, RpcQueryError};
use near_primitives::types::{BlockReference, Finality};
use near_primitives::views::{AccessKeyPermissionView, AccessKeyView, QueryRequest};
use near_sdk::json_types::Base58CryptoHash;
use near_sdk::serde_json::json;
use near_sdk::{
    AccountId, Gas, NearToken,
    base64::{Engine, prelude::BASE64_STANDARD},
    json_types::{Base64VecU8, U128},
    near,
};
use near_workspaces::network::Sandbox;
use near_workspaces::operations::{Function, TransactionStatus};
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::{Contract, Worker};
use std::collections::HashMap;
use std::time::Duration;
use test_dex_types::{
    AddLiquidityArgs, AssetWithBalance, CreatePoolArgs, CreatePoolResponse, PoolId,
    RemoveLiquidityArgs, SimplePool, SwapMessage,
};

#[tokio::test]
async fn test_minimal_flow() {
    let storage_deposit_amount = NearToken::from_near(5);
    let initial_near_deposit = NearToken::from_near(20);
    let transfer_amount = 1000u128;
    let swap_amount = 10u128;

    let TestContext {
        sandbox,
        dex_engine_contract,
        deployer,
        ..
    } = setup_test_environment().await;
    let wasms = get_compiled_wasms().await;
    let dex_wasm = &wasms.test_dex_wasm;

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: dex_id_string.clone(),
    };

    let result = deployer
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
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
    assert_success(&result).unwrap();

    let result = deployer
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
    assert_success(&result).unwrap();

    let initial_near_balance = near_balance_after_refunds(&sandbox, &deployer).await;
    let mut total_near_burnt = NearToken::from_yoctonear(0);
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        None,
    )
    .await
    .unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near],
            "for": AccountOrDexId::Account(deployer.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);

    let result = deployer
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert_near_balance(
        &deployer,
        initial_near_balance
            .saturating_sub(initial_near_deposit)
            .saturating_sub(total_near_burnt)
            .saturating_sub(NearToken::from_yoctonear(1)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(initial_near_deposit.as_yoctonear())),
    )
    .await
    .unwrap();

    let result = deployer
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
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        AssetId::Near,
        Some(U128(0)),
    )
    .await
    .unwrap();
    let result = deployer
        .call(dex_engine_contract.id(), "transfer_asset")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "to": AccountOrDexId::Dex(dex_id.clone()),
            "asset_id": AssetId::Near,
            "amount": U128(transfer_amount),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(initial_near_deposit.as_yoctonear() - transfer_amount)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        AssetId::Near,
        Some(U128(transfer_amount)),
    )
    .await
    .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        AssetId::Near,
        Some(U128(transfer_amount)),
    )
    .await
    .unwrap();
    let result = deployer
        .call(dex_engine_contract.id(), "swap_simple")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapMessage::Same).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Near,
            "amount": SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    let result: (U128, U128) = result.json().unwrap();
    assert_eq!(result, (U128(swap_amount), U128(swap_amount)));
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        AssetId::Near,
        Some(U128(transfer_amount)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_storage_actions() {
    let storage_deposit_amount = NearToken::from_near(1);
    let min_storage_bound = NearToken::from_millinear(5);

    let TestContext {
        dex_engine_contract,
        user1,
        ..
    } = setup_test_environment().await;

    let storage_bounds = dex_engine_contract
        .view("storage_balance_bounds")
        .args_json(json!({}))
        .await
        .unwrap()
        .json::<StorageBalanceBounds>()
        .unwrap();
    assert_eq!(storage_bounds.min, min_storage_bound);
    assert!(storage_bounds.max.is_none());

    let storage_balance_before = dex_engine_contract
        .view("storage_balance_of")
        .args_json(json!({ "account_id": user1.id() }))
        .await
        .unwrap()
        .json::<Option<StorageBalance>>()
        .unwrap();
    assert!(storage_balance_before.is_none());

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({
            "account_id": user1.id(),
            "registration_only": true,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let deposited_balance = result.json::<StorageBalance>().unwrap();
    assert_eq!(deposited_balance.total, storage_bounds.min);
    assert!(deposited_balance.available < deposited_balance.total);
    assert!(deposited_balance.available > NearToken::from_yoctonear(0));

    let storage_balance_after_deposit = dex_engine_contract
        .view("storage_balance_of")
        .args_json(json!({ "account_id": user1.id() }))
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

    let result = user1
        .call(dex_engine_contract.id(), "storage_withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "amount": deposited_balance.available,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let balance_after_withdraw = result.json::<StorageBalance>().unwrap();
    assert_eq!(
        balance_after_withdraw.available,
        NearToken::from_yoctonear(0)
    );
}

#[tokio::test]
async fn test_register_no_assets() {
    let TestContext {
        dex_engine_contract,
        user1,
        ..
    } = setup_test_environment().await;

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(NearToken::from_millinear(10))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let expected_error = "No assets to register";
    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": Vec::<AssetId>::new(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(
        format!("{result:?}").contains(expected_error),
        "Expected \"{expected_error}\", got {result:#?}"
    );

    let result = user1
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![Operation::RegisterAssets {
                asset_ids: Vec::new(),
                r#for: Some(AccountOrDexId::Account(user1.id().clone())),
            }],
        }))
        .transact()
        .await
        .unwrap();
    assert!(
        format!("{result:?}").contains(expected_error),
        "Expected \"{expected_error}\", got {result:#?}"
    );
}

#[tokio::test]
async fn test_dex_storage_actions() {
    let min_storage_bound = NearToken::from_millinear(5);

    let TestContext {
        dex_engine_contract,
        deployer,
        user1,
        ..
    } = setup_test_environment().await;

    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    let storage_bounds = dex_engine_contract
        .view("dex_storage_balance_bounds")
        .args_json(json!({}))
        .await
        .unwrap()
        .json::<StorageBalanceBounds>()
        .unwrap();
    assert_eq!(storage_bounds.min, min_storage_bound);
    assert!(storage_bounds.max.is_none());

    let result = deployer
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "registration_only": true,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let deposited_balance = result.json::<StorageBalance>().unwrap();
    assert_eq!(deposited_balance.total, storage_bounds.min);
    assert!(deposited_balance.available < deposited_balance.total);
    assert!(deposited_balance.available > NearToken::from_yoctonear(0));

    let result = user1
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

    let result = deployer
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
    assert_success(&result).unwrap();
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
    let ft_total_supply = NearToken::from_near(1_000_000_000);
    let storage_deposit_amount = NearToken::from_near(1);
    let initial_near_deposit = NearToken::from_near(2);
    let ft_deposit_amount = 1_000_000_000u128;
    let ft_withdraw_attempt = 100_000_000u128;
    let near_withdraw_amount = NearToken::from_near(1);

    let TestContext {
        dex_engine_contract,
        ft1,
        user1,
        user2,
        deployer,
        ..
    } = setup_test_environment().await;
    let nonexistent_account: AccountId = "nonexistent.test.near".parse().unwrap();

    ft_storage_deposit(&ft1, &user1).await;

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(ft_total_supply.as_yoctonear()),
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Account(user1.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    ft_storage_deposit_for(&ft1, &user1, dex_engine_contract.id()).await;

    let result = user1
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft_deposit_amount),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Near,
        Some(U128(initial_near_deposit.as_yoctonear())),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_deposit_amount)),
    )
    .await
    .unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft1.id().clone()),
            "amount": WithdrawAmount::Exact(TokenAmount::new(ft_withdraw_attempt)),
            "withdraw_to": user2.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert!(!result.json::<bool>().unwrap());
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_deposit_amount)),
    )
    .await
    .unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Near,
            "amount": WithdrawAmount::Exact(TokenAmount::new(near_withdraw_amount.as_yoctonear())),
            "withdraw_to": nonexistent_account,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert!(!result.json::<bool>().unwrap());
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Near,
        Some(U128(initial_near_deposit.as_yoctonear())),
    )
    .await
    .unwrap();

    // A failed withdrawal goes to the recipient's balance instead, if it registered the asset
    let result = user2
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let result = user2
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Account(user2.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let result = user1
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft1.id().clone()),
            "amount": WithdrawAmount::Exact(TokenAmount::new(ft_withdraw_attempt)),
            "withdraw_to": user2.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert!(!result.json::<bool>().unwrap());
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_deposit_amount - ft_withdraw_attempt)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user2.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_withdraw_attempt)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_total_in_custody_consistency() {
    let ft_total_supply = NearToken::from_near(1_000_000_000);
    let storage_deposit_amount = NearToken::from_near(1);
    let initial_near_deposit = NearToken::from_near(2);
    let ft_deposit_amount = 1_000_000_000u128;
    let ft_withdraw_attempt = 100_000_000u128;
    let near_withdraw_amount = NearToken::from_near(1);
    let ft_successful_withdraw = 500_000_000u128;

    let TestContext {
        dex_engine_contract,
        ft1: ft,
        user1,
        user2,
        deployer,
        ..
    } = setup_test_environment().await;

    ft_storage_deposit(&ft, &user1).await;

    let result = deployer
        .call(ft.id(), "ft_transfer")
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(ft_total_supply.as_yoctonear()),
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft.id().clone())],
            "for": AccountOrDexId::Account(user1.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

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

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    ft_storage_deposit_for(&ft, &user1, dex_engine_contract.id()).await;

    let result = user1
        .call(ft.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft_deposit_amount),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(initial_near_deposit.as_yoctonear())),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(ft_deposit_amount)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(ft_deposit_amount)),
    )
    .await
    .unwrap();
    let result = user1
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "amount": WithdrawAmount::Exact(TokenAmount::new(ft_withdraw_attempt)),
            "withdraw_to": user2.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert!(!result.json::<bool>().unwrap());
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(ft_deposit_amount)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(ft_deposit_amount)),
    )
    .await
    .unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Near,
            "amount": WithdrawAmount::Exact(TokenAmount::new(near_withdraw_amount.as_yoctonear())),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert!(result.json::<bool>().unwrap());

    let result = user1
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "amount": WithdrawAmount::Exact(TokenAmount::new(ft_successful_withdraw)),
            "withdraw_to": user1.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert!(result.json::<bool>().unwrap());

    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(near_withdraw_amount.as_yoctonear())),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Near,
        Some(U128(near_withdraw_amount.as_yoctonear())),
    )
    .await
    .unwrap();

    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(ft_successful_withdraw)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(ft_successful_withdraw)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_execute_operations() {
    let storage_deposit_amount = NearToken::from_near(5);
    let initial_near_deposit = NearToken::from_near(3);
    let transfer_amount = NearToken::from_millinear(1);
    let withdraw_amount = NearToken::from_millinear(2);
    let swap_amount = NearToken::from_millinear(1);

    let TestContext {
        deployer,
        dex_engine_contract,
        user1,
        ..
    } = setup_test_environment().await;
    set_trusted_code_deployer(&dex_engine_contract, &deployer, &user1).await;
    let wasms = get_compiled_wasms().await;
    let dex_wasm = &wasms.test_dex_wasm;

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: user1.id().clone(),
        id: dex_id_string.clone(),
    };

    let result = user1
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near],
            "for": AccountOrDexId::Account(user1.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
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
    assert_success(&result).unwrap();

    let operations = vec![
        Operation::RegisterAssets {
            asset_ids: vec![AssetId::Near],
            r#for: Some(AccountOrDexId::Dex(DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            })),
        },
        Operation::DexCall {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            method: "new".to_string(),
            args: Base64VecU8(vec![]),
            attached_assets: HashMap::new(),
        },
        Operation::TransferAsset {
            to: AccountOrDexId::Dex(DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            }),
            asset_id: AssetId::Near,
            amount: TokenAmount::new(transfer_amount.as_yoctonear()),
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(near_sdk::borsh::to_vec(&SwapMessage::Same).unwrap()),
            asset_in: AssetId::Near,
            asset_out: AssetId::Near,
            amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(TokenAmount::new(
                swap_amount.as_yoctonear(),
            ))),
            constraint: None,
        },
        Operation::Withdraw {
            asset_id: AssetId::Near,
            amount: WithdrawAmount::Exact(TokenAmount::new(withdraw_amount.as_yoctonear())),
            to: None,
            rescue_address: None,
        },
    ];

    let result = user1
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": operations,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Near,
        Some(U128(
            initial_near_deposit
                .saturating_sub(transfer_amount)
                .saturating_sub(withdraw_amount)
                .as_yoctonear(),
        )),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(DexId {
            deployer: user1.id().clone(),
            id: dex_id_string,
        }),
        AssetId::Near,
        Some(U128(transfer_amount.as_yoctonear())),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(
            initial_near_deposit
                .saturating_sub(withdraw_amount)
                .as_yoctonear(),
        )),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_execute_operations_failure_reverts() {
    let deposit_amount = NearToken::from_near(1);

    let TestContext {
        dex_engine_contract,
        user1,
        ..
    } = setup_test_environment().await;

    let operations = vec![Operation::Withdraw {
        asset_id: AssetId::Near,
        amount: WithdrawAmount::Full { at_least: None },
        to: None,
        rescue_address: None,
    }];

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(deposit_amount)
        .args_json(json!({
            "operations": operations,
        }))
        .transact()
        .await
        .unwrap();
    assert!(!result.is_success());

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
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
    let ft_total_supply = NearToken::from_near(10);
    let storage_deposit_amount = NearToken::from_near(1);
    let ft_transfer_amount = 1_000_000u128;
    let ft_withdraw_attempt = 1_000_000_000_000u128;

    let TestContext {
        dex_engine_contract,
        ft1,
        user1,
        deployer,
        ..
    } = setup_test_environment().await;

    ft_storage_deposit(&ft1, &user1).await;

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(ft_total_supply.as_yoctonear()),
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Account(user1.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    ft_storage_deposit_for(&ft1, &user1, dex_engine_contract.id()).await;

    let operations = vec![Operation::Withdraw {
        asset_id: AssetId::Nep141(ft1.id().clone()),
        amount: WithdrawAmount::Exact(TokenAmount::new(ft_withdraw_attempt)),
        to: None,
        rescue_address: None,
    }];

    let initial_ft_balance = ft1
        .view("ft_balance_of")
        .args_json(json!({
            "account_id": user1.id(),
        }))
        .await
        .unwrap()
        .json::<U128>()
        .unwrap();

    let result = user1
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft_transfer_amount),
            "msg": near_sdk::serde_json::to_string(&operations).unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert!(result.json::<U128>().unwrap() == U128(0));

    assert_ft_balance(&user1, ft1.clone(), initial_ft_balance)
        .await
        .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(0)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(0)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_execute_operations_liquidity_and_swaps() {
    let ft_total_supply = NearToken::from_near(1_000_000_000);
    let storage_deposit_amount = NearToken::from_near(5);
    let initial_near_deposit = NearToken::from_near(5);
    let ft_deposit_amount = 1_000_000u128;
    let pool_creation_fee = NearToken::from_millinear(10);
    let swap_amount_in = NearToken::from_millinear(1);
    let lp1_near_amount = NearToken::from_near(1);
    let lp1_ft1_amount = 500_000u128;
    let lp2_ft1_amount = 200_000u128;
    let lp2_ft2_amount = 600_000u128;

    let TestContext {
        dex_engine_contract,
        ft1,
        ft2,
        user1,
        deployer,
        ..
    } = setup_test_environment().await;
    set_trusted_code_deployer(&dex_engine_contract, &deployer, &user1).await;
    let wasms = get_compiled_wasms().await;
    let dex_wasm = &wasms.test_dex_wasm;

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: user1.id().clone(),
        id: dex_id_string.clone(),
    };

    ft_storage_deposit(&ft1, &user1).await;
    ft_storage_deposit(&ft2, &user1).await;

    for ft in [&ft1, &ft2] {
        let result = deployer
            .call(ft.id(), "ft_transfer")
            .args_json(json!({
                "receiver_id": user1.id(),
                "amount": U128(ft_total_supply.as_yoctonear()),
            }))
            .deposit(NearToken::from_yoctonear(1))
            .max_gas()
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();
    }

    let result = user1
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [
                AssetId::Near,
                AssetId::Nep141(ft1.id().clone()),
                AssetId::Nep141(ft2.id().clone())
            ],
            "for": AccountOrDexId::Account(user1.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
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
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            }),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    ft_storage_deposit_for(&ft1, &user1, dex_engine_contract.id()).await;
    ft_storage_deposit_for(&ft2, &user1, dex_engine_contract.id()).await;

    let result = user1
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
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    for ft in [&ft1, &ft2] {
        let result = user1
            .call(ft.id(), "ft_transfer_call")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": dex_engine_contract.id(),
                "amount": U128(ft_deposit_amount),
                "msg": "",
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();
    }

    let operations = vec![
        Operation::DexCall {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            method: "new".to_string(),
            args: Base64VecU8(vec![]),
            attached_assets: HashMap::new(),
        },
        Operation::DexCall {
            dex_id: DexId {
                deployer: user1.id().clone(),
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
                TokenAmount::new(pool_creation_fee.as_yoctonear()),
            )]),
        },
        Operation::DexCall {
            dex_id: DexId {
                deployer: user1.id().clone(),
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
                TokenAmount::new(pool_creation_fee.as_yoctonear()),
            )]),
        },
        Operation::DexCall {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            method: "add_liquidity".to_string(),
            args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: 0 }).unwrap()),
            attached_assets: HashMap::from_iter([
                (
                    AssetId::Near,
                    TokenAmount::new(lp1_near_amount.as_yoctonear()),
                ),
                (
                    AssetId::Nep141(ft1.id().clone()),
                    TokenAmount::new(lp1_ft1_amount),
                ),
            ]),
        },
        Operation::DexCall {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            method: "add_liquidity".to_string(),
            args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: 1 }).unwrap()),
            attached_assets: HashMap::from_iter([
                (
                    AssetId::Nep141(ft1.id().clone()),
                    TokenAmount::new(lp2_ft1_amount),
                ),
                (
                    AssetId::Nep141(ft2.id().clone()),
                    TokenAmount::new(lp2_ft2_amount),
                ),
            ]),
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(
                near_sdk::borsh::to_vec(&SwapMessage::Pool { pool_id: 0 }).unwrap(),
            ),
            asset_in: AssetId::Near,
            asset_out: AssetId::Nep141(ft1.id().clone()),
            amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(TokenAmount::new(
                swap_amount_in.as_yoctonear(),
            ))),
            constraint: None,
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(
                near_sdk::borsh::to_vec(&SwapMessage::Pool { pool_id: 1 }).unwrap(),
            ),
            asset_in: AssetId::Nep141(ft1.id().clone()),
            asset_out: AssetId::Nep141(ft2.id().clone()),
            amount: SwapOperationAmount::OutputOfLastIn,
            constraint: None,
        },
    ];

    let ft2_balance_before = user1
        .view(dex_engine_contract.id(), "asset_balance_of")
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft2.id().clone()),
            "of": AccountOrDexId::Account(user1.id().clone()),
        }))
        .await
        .unwrap()
        .json::<U128>()
        .unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": operations,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let ft2_balance_after = user1
        .view(dex_engine_contract.id(), "asset_balance_of")
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft2.id().clone()),
            "of": AccountOrDexId::Account(user1.id().clone()),
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
        Some(U128(
            initial_near_deposit.as_yoctonear() - 3250000000000000000000,
        )), // converted to storage deposit
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_deposit_amount)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(ft_deposit_amount)),
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
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(ft_deposit_amount - lp2_ft2_amount + amount_out)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_operations_with_ft_deposit() {
    let ft_total_supply = NearToken::from_near(1_000_000_000);
    let storage_deposit_amount = NearToken::from_near(5);
    let ft_deposit_amount = 1_000_000u128;
    let ft_transfer_to_dex = 200_000u128;
    let ft_swap_amount = 100_000u128;

    let TestContext {
        dex_engine_contract,
        ft1,
        user1,
        user2,
        deployer,
        ..
    } = setup_test_environment().await;
    set_trusted_code_deployer(&dex_engine_contract, &deployer, &user1).await;
    let wasms = get_compiled_wasms().await;
    let dex_wasm = &wasms.test_dex_wasm;

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: user1.id().clone(),
        id: dex_id_string.clone(),
    };

    ft_storage_deposit(&ft1, &user1).await;

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(ft_total_supply.as_yoctonear()),
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Dex(DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            }),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Account(user2.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
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
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            "method": "new",
            "args": BASE64_STANDARD.encode([]),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    ft_storage_deposit_for(&ft1, &user1, dex_engine_contract.id()).await;

    let operations = vec![
        Operation::TransferAsset {
            to: AccountOrDexId::Dex(DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            }),
            asset_id: AssetId::Nep141(ft1.id().clone()),
            amount: TokenAmount::new(ft_transfer_to_dex),
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(near_sdk::borsh::to_vec(&SwapMessage::Same).unwrap()),
            asset_in: AssetId::Nep141(ft1.id().clone()),
            asset_out: AssetId::Nep141(ft1.id().clone()),
            amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(TokenAmount::new(
                ft_swap_amount,
            ))),
            constraint: None,
        },
        Operation::Withdraw {
            asset_id: AssetId::Nep141(ft1.id().clone()),
            amount: WithdrawAmount::Full { at_least: None },
            to: Some(user1.id().clone()),
            rescue_address: Some(user2.id().clone()),
        },
    ];

    let result = user1
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft_deposit_amount),
            "msg": near_sdk::serde_json::to_string(&operations).unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        None,
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(DexId {
            deployer: user1.id().clone(),
            id: dex_id_string,
        }),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_transfer_to_dex)),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_transfer_to_dex)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_regular_flow() {
    let storage_deposit_amount = NearToken::from_near(5);
    let initial_near_deposit = NearToken::from_near(20);
    let ft_deposit_amount = 1_000_000_000u128;
    let pool_creation_fee = NearToken::from_millinear(10);
    let add_liquidity_near = NearToken::from_near(1);
    let add_liquidity_ft = 1_000_000u128;
    let swap_amount_in = NearToken::from_millinear(100);
    let remove_liquidity_near = NearToken::from_millinear(500);
    let remove_liquidity_ft = 500_000u128;
    let withdraw_near_amount = NearToken::from_near(1);
    let withdraw_ft_amount = 100_000_000u128;

    let TestContext {
        sandbox,
        dex_engine_contract,
        ft1,
        deployer,
        ..
    } = setup_test_environment().await;
    let wasms = get_compiled_wasms().await;
    let dex_wasm = &wasms.test_dex_wasm;

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: dex_id_string.clone(),
    };

    let result = deployer
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
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
    assert_success(&result).unwrap();

    let initial_near_balance = near_balance_after_refunds(&sandbox, &deployer).await;
    let mut total_near_burnt = NearToken::from_yoctonear(0);
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        None,
    )
    .await
    .unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Dex(dex_id.clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        None,
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        None,
    )
    .await
    .unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Account(deployer.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);

    let result = deployer
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert_near_balance(
        &deployer,
        initial_near_balance
            .saturating_sub(initial_near_deposit)
            .saturating_sub(total_near_burnt)
            .saturating_sub(NearToken::from_yoctonear(2)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(initial_near_deposit.as_yoctonear())),
    )
    .await
    .unwrap();

    ft_storage_deposit_for(&ft1, ft1.as_account(), dex_engine_contract.id()).await;

    let initial_ft_balance = ft1
        .view("ft_balance_of")
        .args_json(json!({
            "account_id": deployer.id(),
        }))
        .await
        .unwrap()
        .json::<U128>()
        .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(0)),
    )
    .await
    .unwrap();
    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft_deposit_amount),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert_ft_balance(
        &deployer,
        ft1.clone(),
        U128(initial_ft_balance.0 - ft_deposit_amount),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_deposit_amount)),
    )
    .await
    .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(initial_near_deposit.as_yoctonear())),
    )
    .await
    .unwrap();

    let result = deployer
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
    assert_success(&result).unwrap();
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
        NearToken::from_yoctonear(20000000000000000000000000),
    );
    let storage_usage_by_engine_before_pool_creation = dex_engine_contract
        .view_account()
        .await
        .unwrap()
        .storage_usage;

    let near_for_create_pool = pool_creation_fee;
    let result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "create_pool",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&CreatePoolArgs {
                assets: (AssetId::Near, AssetId::Nep141(ft1.id().clone())),
            }).unwrap()),
            "attached_assets": {
                "near": near_for_create_pool,
            },
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
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
        NearToken::from_yoctonear(20002100000000000000000000),
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
        &deployer,
        initial_near_balance
            .saturating_sub(total_near_burnt)
            .saturating_sub(initial_near_deposit)
            .saturating_sub(NearToken::from_yoctonear(5)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(
            initial_near_deposit.as_yoctonear() - pool_storage_cost.as_yoctonear(),
        )),
    )
    .await
    .unwrap();

    #[near(serializers=[borsh])]
    struct AddLiquidityResponse;
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_deposit_amount)),
    )
    .await
    .unwrap();
    let result = deployer
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
                "near": U128(add_liquidity_near.as_yoctonear()),
                format!("nep141:{}", ft1.id()): U128(add_liquidity_ft),
            },
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    let response = result.json::<Base64VecU8>().unwrap();
    let _ = near_sdk::borsh::from_slice::<AddLiquidityResponse>(&response.0).unwrap();
    assert_near_balance(
        &deployer,
        initial_near_balance
            .saturating_sub(total_near_burnt)
            .saturating_sub(initial_near_deposit)
            .saturating_sub(NearToken::from_yoctonear(6)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(
            initial_near_deposit.as_yoctonear()
                - pool_storage_cost.as_yoctonear()
                - add_liquidity_near.as_yoctonear(),
        )),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_deposit_amount - add_liquidity_ft)),
    )
    .await
    .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_deposit_amount - add_liquidity_ft)),
    )
    .await
    .unwrap();
    let result = deployer
        .call(dex_engine_contract.id(), "swap_simple")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapMessage::Pool {
                pool_id,
            }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount_in.as_yoctonear())),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    let result: (U128, U128) = result.json().unwrap();
    assert_eq!(result, (U128(100000000000000000000000), U128(90909)));
    assert_near_balance(
        &deployer,
        initial_near_balance
            .saturating_sub(total_near_burnt)
            .saturating_sub(initial_near_deposit)
            .saturating_sub(NearToken::from_yoctonear(7)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(
            initial_near_deposit.as_yoctonear()
                - pool_storage_cost.as_yoctonear()
                - add_liquidity_near.as_yoctonear()
                - swap_amount_in.as_yoctonear(),
        )),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_deposit_amount - add_liquidity_ft + 90909)),
    )
    .await
    .unwrap();

    #[near(serializers=[borsh])]
    struct RemoveLiquidityResponse;
    let result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "remove_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                pool_id,
                assets_to_remove: (TokenAmount::new(remove_liquidity_near.as_yoctonear()), TokenAmount::new(remove_liquidity_ft)),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    let response = result.json::<Base64VecU8>().unwrap();
    let _ = near_sdk::borsh::from_slice::<RemoveLiquidityResponse>(&response.0).unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(
            initial_near_deposit.as_yoctonear()
                - pool_storage_cost.as_yoctonear()
                - add_liquidity_near.as_yoctonear()
                - swap_amount_in.as_yoctonear()
                + remove_liquidity_near.as_yoctonear(),
        )),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(
            ft_deposit_amount - add_liquidity_ft + 90909 + remove_liquidity_ft,
        )),
    )
    .await
    .unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Near,
            "amount": WithdrawAmount::Exact(TokenAmount::new(withdraw_near_amount.as_yoctonear())),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert!(result.json::<bool>().unwrap());
    wait_for_blocks(&sandbox, 1).await;
    assert_near_balance(
        &deployer,
        initial_near_balance
            .saturating_sub(total_near_burnt)
            .saturating_sub(initial_near_deposit)
            .saturating_sub(NearToken::from_yoctonear(9))
            .saturating_add(withdraw_near_amount),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(
            initial_near_deposit.as_yoctonear()
                - pool_storage_cost.as_yoctonear()
                - add_liquidity_near.as_yoctonear()
                - swap_amount_in.as_yoctonear()
                + remove_liquidity_near.as_yoctonear()
                - withdraw_near_amount.as_yoctonear(),
        )),
    )
    .await
    .unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft1.id().clone()),
            "amount": WithdrawAmount::Exact(TokenAmount::new(withdraw_ft_amount)),
            "withdraw_to": null,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert!(result.json::<bool>().unwrap());
    assert_ft_balance(
        &deployer,
        ft1.clone(),
        U128(initial_ft_balance.0 - ft_deposit_amount + withdraw_ft_amount),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(
            ft_deposit_amount - add_liquidity_ft + 90909 + remove_liquidity_ft - withdraw_ft_amount,
        )),
    )
    .await
    .unwrap();

    #[near(serializers=[borsh])]
    struct GetPoolArgs {
        pool_id: PoolId,
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
                    balance: TokenAmount::new(
                        add_liquidity_near.as_yoctonear() + swap_amount_in.as_yoctonear()
                            - remove_liquidity_near.as_yoctonear()
                    ),
                },
                AssetWithBalance {
                    asset_id: AssetId::Nep141(ft1.id().clone()),
                    balance: TokenAmount::new(add_liquidity_ft - 90909 - remove_liquidity_ft),
                },
            ),
            owner_id: deployer.id().clone(),
        })
    );
}

#[tokio::test]
async fn test_swap_constraints() {
    let ft_total_supply = NearToken::from_near(1_000_000_000);
    let storage_deposit_amount = NearToken::from_near(5);
    let initial_near_deposit = NearToken::from_near(5);
    let ft_deposit_amount = 1_000_000u128;
    let pool_creation_fee = NearToken::from_millinear(10);
    let lp_near_amount = NearToken::from_near(1);
    let lp_ft_amount = 500_000u128;
    let swap_amount_in = NearToken::from_millinear(1);

    let TestContext {
        dex_engine_contract,
        ft1,
        user1,
        deployer,
        ..
    } = setup_test_environment().await;
    set_trusted_code_deployer(&dex_engine_contract, &deployer, &user1).await;
    let wasms = get_compiled_wasms().await;
    let dex_wasm = &wasms.test_dex_wasm;

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: user1.id().clone(),
        id: dex_id_string.clone(),
    };

    ft_storage_deposit(&ft1, &user1).await;

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(ft_total_supply.as_yoctonear()),
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Account(user1.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Dex(dex_id.clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    ft_storage_deposit_for(&ft1, &user1, dex_engine_contract.id()).await;

    let result = user1
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
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft_deposit_amount),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
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
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "create_pool",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&CreatePoolArgs {
                assets: (AssetId::Near, AssetId::Nep141(ft1.id().clone())),
            }).unwrap()),
            "attached_assets": {
                "near": pool_creation_fee,
            },
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "add_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: 0 }).unwrap()),
            "attached_assets": {
                "near": U128(lp_near_amount.as_yoctonear()),
                format!("nep141:{}", ft1.id()): U128(lp_ft_amount),
            },
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    // (1_000_000_000_000_000_000_000 * 500_000) / (1_000_000_000_000_000_000_000_000 + 1_000_000_000_000_000_000_000) = 499
    let expected_output = 499u128;
    let result = user1
        .call(dex_engine_contract.id(), "swap_simple")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapMessage::Pool { pool_id: 0 }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount_in.as_yoctonear())),
            "constraint": U128(expected_output),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let result: (U128, U128) = result.json().unwrap();
    assert_eq!(result.1.0, expected_output);

    // Should fail (min output too high)
    let result = user1
        .call(dex_engine_contract.id(), "swap_simple")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapMessage::Pool { pool_id: 0 }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount_in.as_yoctonear())),
            "constraint": U128(1_000_000),
        }))
        .transact()
        .await
        .unwrap();
    assert!(!result.is_success());

    let exact_output = 100u128;

    let result = user1
        .call(dex_engine_contract.id(), "swap_simple")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapMessage::Pool { pool_id: 0 }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactOut(TokenAmount::new(exact_output)),
            // 1 near should be more than enough
            "constraint": U128(NearToken::from_near(1).as_yoctonear()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let result: (U128, U128) = result.json().unwrap();
    assert_eq!(result.1.0, exact_output);
    assert!(result.0.0 < NearToken::from_near(1).as_yoctonear());

    // Should fail (max input too low)
    let result = user1
        .call(dex_engine_contract.id(), "swap_simple")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapMessage::Pool { pool_id: 0 }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactOut(TokenAmount::new(exact_output)),
            "constraint": U128(1),
        }))
        .transact()
        .await
        .unwrap();
    assert!(!result.is_success());
}

#[tokio::test]
async fn test_simulate_swap_simple() {
    let ft_total_supply = NearToken::from_near(1_000_000_000);
    let storage_deposit_amount = NearToken::from_near(5);
    let initial_near_deposit = NearToken::from_near(5);
    let ft_deposit_amount = 1_000_000u128;
    let pool_creation_fee = NearToken::from_millinear(10);
    let lp_near_amount = NearToken::from_near(1);
    let lp_ft_amount = 500_000u128;
    let swap_amount_in = NearToken::from_millinear(1);

    let TestContext {
        dex_engine_contract,
        ft1,
        user1,
        deployer,
        ..
    } = setup_test_environment().await;
    set_trusted_code_deployer(&dex_engine_contract, &deployer, &user1).await;
    let wasms = get_compiled_wasms().await;
    let dex_wasm = &wasms.test_dex_wasm;

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: user1.id().clone(),
        id: dex_id_string.clone(),
    };

    ft_storage_deposit(&ft1, &user1).await;

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(ft_total_supply.as_yoctonear()),
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Account(user1.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Dex(dex_id.clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    ft_storage_deposit_for(&ft1, &user1, dex_engine_contract.id()).await;

    let result = user1
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
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft_deposit_amount),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
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
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "create_pool",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&CreatePoolArgs {
                assets: (AssetId::Near, AssetId::Nep141(ft1.id().clone())),
            }).unwrap()),
            "attached_assets": {
                "near": pool_creation_fee,
            },
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "add_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: 0 }).unwrap()),
            "attached_assets": {
                "near": U128(lp_near_amount.as_yoctonear()),
                format!("nep141:{}", ft1.id()): U128(lp_ft_amount),
            },
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let simulated_result = dex_engine_contract
        .view("simulate_swap_simple")
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapMessage::Pool { pool_id: 0 }).unwrap()),
            "trader": user1.id().clone(),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount_in.as_yoctonear())),
        }))
        .await
        .unwrap()
        .json::<(U128, U128)>()
        .unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "swap_simple")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapMessage::Pool { pool_id: 0 }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount_in.as_yoctonear())),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let actual_result: (U128, U128) = result.json().unwrap();

    assert_eq!(
        simulated_result, actual_result,
        "Simulated swap result should match actual swap result"
    );
}

#[tokio::test]
async fn test_withdraw_at_least() {
    let storage_deposit_amount = NearToken::from_near(5);
    let initial_near_deposit = NearToken::from_near(3);

    let TestContext {
        dex_engine_contract,
        user1,
        ..
    } = setup_test_environment().await;

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near],
            "for": AccountOrDexId::Account(user1.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Near,
            "amount": WithdrawAmount::Full { at_least: Some(TokenAmount::new(initial_near_deposit.as_yoctonear())) },
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert!(result.json::<bool>().unwrap());

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Near,
        Some(U128(0)),
    )
    .await
    .unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    // Should fail (4 NEAR requested > 3 NEAR initial deposit)
    let result = user1
        .call(dex_engine_contract.id(), "withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_id": AssetId::Near,
            "amount": WithdrawAmount::Full { at_least: Some(TokenAmount::new(NearToken::from_near(4).as_yoctonear())) },
        }))
        .transact()
        .await
        .unwrap();
    assert!(!result.is_success());

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Near,
        Some(U128(initial_near_deposit.as_yoctonear())),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_dex_code_deployment_permissions() {
    let TestContext {
        dex_engine_contract,
        user1,
        user2,
        deployer,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        register_assets_for_all: true,
        ..Default::default()
    })
    .await;
    let dex_wasm = &get_compiled_wasms().await.test_dex_wasm;

    let deploy = |account: &near_workspaces::Account| {
        account
            .call(dex_engine_contract.id(), "deploy_dex_code")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "last_part_of_id": "dex",
                "code_base64": BASE64_STANDARD.encode(dex_wasm),
            }))
            .transact()
    };
    for account in [&deployer, &user1] {
        let result = account
            .call(dex_engine_contract.id(), "dex_storage_deposit")
            .max_gas()
            .deposit(engine_dex_storage_deposit())
            .args_json(json!({
                "dex_id": DexId {
                    deployer: account.id().clone(),
                    id: "dex".to_string(),
                },
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();
    }

    let trusted_code_deployer: AccountId = dex_engine_contract
        .view("get_trusted_code_deployer")
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(&trusted_code_deployer, deployer.id());

    // Untrusted accounts can't deploy code or take the permission
    let result = deploy(&user1).await.unwrap();
    assert!(format!("{result:?}").contains("Only the trusted code deployer can deploy dex code"));
    let result = user1
        .call(dex_engine_contract.id(), "set_trusted_code_deployer")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({ "account_id": user1.id() }))
        .transact()
        .await
        .unwrap();
    assert!(!result.is_success());

    let result = deploy(&deployer).await.unwrap();
    assert_success(&result).unwrap();

    // The permission can be transferred
    set_trusted_code_deployer(&dex_engine_contract, &deployer, &user1).await;
    let result = deploy(&user1).await.unwrap();
    assert_success(&result).unwrap();
    let result = deploy(&deployer).await.unwrap();
    assert!(format!("{result:?}").contains("Only the trusted code deployer can deploy dex code"));
    let result = deploy(&user2).await.unwrap();
    assert!(!result.is_success());
}

#[tokio::test]
async fn test_deploy_invalid_dex_code() {
    let TestContext {
        dex_engine_contract,
        deployer,
        ..
    } = setup_test_environment().await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };
    // Parses, but its only function returns an i64 where its type says i32
    let invalid_code = [
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic and version
        0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type: () -> i32
        0x03, 0x02, 0x01, 0x00, // function of the type
        0x0a, 0x06, 0x01, 0x04, 0x00, 0x42, 0x00, 0x0b, // body: i64.const 0
    ];

    let result = deployer
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "deploy_dex_code")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "last_part_of_id": "dex",
            "code_base64": BASE64_STANDARD.encode(invalid_code),
        }))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("Invalid dex code"));

    let result = dex_engine_contract
        .view("dex_code")
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_total_storage_balances() {
    let storage_deposit_amount = NearToken::from_millinear(500);
    let registration_deposit_amount = NearToken::from_millinear(10);
    let storage_withdraw_amount = NearToken::from_millinear(100);
    let initial_near_deposit = NearToken::from_near(1);
    let dex_storage_deposit_amount = NearToken::from_millinear(300);
    let user_storage_deposit_amount = NearToken::from_millinear(200);
    let dex_storage_withdraw_amount = NearToken::from_near(1);

    let TestContext {
        sandbox,
        dex_engine_contract,
        user1,
        user2,
        user3,
        user4,
        user5,
        deployer,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: get_compiled_wasms().await.test_dex_wasm.clone(),
            init_method: Some(("new".to_string(), vec![])),
        }),
        register_assets_for_all: true,
        ..Default::default()
    })
    .await;
    let wasms = get_compiled_wasms().await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };
    let new_account_id: AccountId = "new-account.test.near".parse().unwrap();
    let accounts = [
        user1.id(),
        user2.id(),
        user3.id(),
        user4.id(),
        user5.id(),
        deployer.id(),
        &new_account_id,
    ];
    let dexes = [&dex_id];

    assert_total_storage_balances(&dex_engine_contract, &accounts, &dexes)
        .await
        .unwrap();
    assert_untracked_near(&dex_engine_contract).await.unwrap();

    let total_storage_balances_before = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(storage_deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let total_storage_balances_after = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    assert_eq!(
        total_storage_balances_after.users.total,
        total_storage_balances_before
            .users
            .total
            .saturating_add(storage_deposit_amount),
    );
    assert_total_storage_balances(&dex_engine_contract, &accounts, &dexes)
        .await
        .unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(registration_deposit_amount)
        .args_json(json!({
            "account_id": new_account_id,
            "registration_only": true,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert_total_storage_balances(&dex_engine_contract, &accounts, &dexes)
        .await
        .unwrap();

    let total_storage_balances_before = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    let result = user2
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(registration_deposit_amount)
        .args_json(json!({
            "registration_only": true,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let total_storage_balances_after = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    assert_eq!(
        total_storage_balances_after.users.total,
        total_storage_balances_before.users.total,
    );
    assert_eq!(
        total_storage_balances_after.users.available,
        total_storage_balances_before.users.available,
    );

    let total_storage_balances_before = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    let result = user2
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Nep141("some-token.test.near".parse().unwrap())],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let total_storage_balances_after = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    assert_eq!(
        total_storage_balances_after.users.total,
        total_storage_balances_before.users.total,
    );
    assert!(
        total_storage_balances_after.users.available
            < total_storage_balances_before.users.available
    );
    assert_total_storage_balances(&dex_engine_contract, &accounts, &dexes)
        .await
        .unwrap();

    let total_storage_balances_before = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    let result = user2
        .call(dex_engine_contract.id(), "storage_withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "amount": storage_withdraw_amount,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let total_storage_balances_after = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    assert_eq!(
        total_storage_balances_after.users.total,
        total_storage_balances_before
            .users
            .total
            .saturating_sub(storage_withdraw_amount),
    );
    assert_total_storage_balances(&dex_engine_contract, &accounts, &dexes)
        .await
        .unwrap();

    let total_storage_balances_before = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    let result = user3
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({
            "operations": [
                Operation::StorageDeposit {
                    amount: TokenAmount::new(dex_storage_deposit_amount.as_yoctonear()),
                    r#for: Some(AccountOrDexId::Dex(dex_id.clone())),
                },
                Operation::StorageDeposit {
                    amount: TokenAmount::new(user_storage_deposit_amount.as_yoctonear()),
                    r#for: Some(AccountOrDexId::Account(user4.id().clone())),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let total_storage_balances_after = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    assert_eq!(
        total_storage_balances_after.dexes.total,
        total_storage_balances_before
            .dexes
            .total
            .saturating_add(dex_storage_deposit_amount),
    );
    assert_eq!(
        total_storage_balances_after.users.total,
        total_storage_balances_before
            .users
            .total
            .saturating_add(user_storage_deposit_amount),
    );
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(
            initial_near_deposit
                .saturating_sub(dex_storage_deposit_amount)
                .saturating_sub(user_storage_deposit_amount)
                .as_yoctonear(),
        )),
    )
    .await
    .unwrap();
    assert_total_storage_balances(&dex_engine_contract, &accounts, &dexes)
        .await
        .unwrap();
    assert_untracked_near(&dex_engine_contract).await.unwrap();

    let total_storage_balances_before = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    let result = deployer
        .call(dex_engine_contract.id(), "deploy_dex_code")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "last_part_of_id": "dex",
            "code_base64": BASE64_STANDARD.encode(with_padding_section(&wasms.test_dex_wasm, 1000)),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let total_storage_balances_after = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    assert_eq!(
        total_storage_balances_after.dexes.total,
        total_storage_balances_before.dexes.total,
    );
    assert_ne!(
        total_storage_balances_after.dexes.available,
        total_storage_balances_before.dexes.available,
    );
    assert_total_storage_balances(&dex_engine_contract, &accounts, &dexes)
        .await
        .unwrap();

    let total_storage_balances_before = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    let result = deployer
        .call(dex_engine_contract.id(), "dex_storage_withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id,
            "amount": dex_storage_withdraw_amount,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let total_storage_balances_after = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    assert_eq!(
        total_storage_balances_after.dexes.total,
        total_storage_balances_before
            .dexes
            .total
            .saturating_sub(dex_storage_withdraw_amount),
    );
    assert_total_storage_balances(&dex_engine_contract, &accounts, &dexes)
        .await
        .unwrap();

    wait_for_blocks(&sandbox, 3).await;
    assert_untracked_near(&dex_engine_contract).await.unwrap();
}

#[tokio::test]
async fn test_untracked_near() {
    let transfer_amount = NearToken::from_near(3);
    let deposit_amount = NearToken::from_near(2);
    let max_gas_reward = NearToken::from_millinear(10);

    let TestContext {
        sandbox,
        dex_engine_contract,
        user1,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        register_assets_for_all: true,
        ..Default::default()
    })
    .await;

    wait_for_blocks(&sandbox, 3).await;
    let untracked_near_initial = assert_untracked_near(&dex_engine_contract).await.unwrap();

    let result = user1
        .transfer_near(dex_engine_contract.id(), transfer_amount)
        .await
        .unwrap();
    assert!(result.is_success());
    let untracked_near_after_transfer = assert_untracked_near(&dex_engine_contract).await.unwrap();
    assert_eq!(
        untracked_near_after_transfer,
        untracked_near_initial.saturating_add(transfer_amount),
    );

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(deposit_amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    wait_for_blocks(&sandbox, 3).await;
    let untracked_near_after_deposit = assert_untracked_near(&dex_engine_contract).await.unwrap();
    assert!(untracked_near_after_deposit >= untracked_near_after_transfer);
    assert!(
        untracked_near_after_deposit < untracked_near_after_transfer.saturating_add(max_gas_reward)
    );
}

#[tokio::test]
async fn test_rescue() {
    let untracked_ft_amount = 1000u128;
    let tracked_ft_amount = 500u128;
    let partial_rescue_amount = 400u128;
    let near_transfer_amount = NearToken::from_near(2);

    let TestContext {
        sandbox,
        dex_engine_contract,
        user1,
        user2,
        user3,
        deployer,
        ft1: ft,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
        ..Default::default()
    })
    .await;
    let pauser = create_pauser(&sandbox).await;
    let dex_engine_account = dex_engine_contract.as_account();

    let result = deployer
        .call(ft.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(untracked_ft_amount),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(tracked_ft_amount),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(tracked_ft_amount)),
    )
    .await
    .unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "rescue")
        .max_gas()
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "to": user1.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("Method rescue is private"));

    let result = dex_engine_account
        .call(dex_engine_contract.id(), "rescue")
        .max_gas()
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "to": user2.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("Contract must be paused to rescue assets"));

    set_paused(&dex_engine_contract, &pauser, true).await;

    let result = dex_engine_account
        .call(dex_engine_contract.id(), "rescue")
        .max_gas()
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "amount": U128(untracked_ft_amount + 1),
            "to": user2.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains(&format!("only {untracked_ft_amount} is untracked")));
    assert_ft_balance(&user2, ft.clone(), U128(0))
        .await
        .unwrap();

    let result = dex_engine_account
        .call(dex_engine_contract.id(), "rescue")
        .max_gas()
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "amount": U128(partial_rescue_amount),
            "to": user2.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert_ft_balance(&user2, ft.clone(), U128(partial_rescue_amount))
        .await
        .unwrap();

    let result = dex_engine_account
        .call(dex_engine_contract.id(), "rescue")
        .max_gas()
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "to": user2.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert_ft_balance(&user2, ft.clone(), U128(untracked_ft_amount))
        .await
        .unwrap();
    assert_ft_balance(dex_engine_account, ft.clone(), U128(tracked_ft_amount))
        .await
        .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Nep141(ft.id().clone()),
        Some(U128(tracked_ft_amount)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft.id().clone()),
        Some(U128(tracked_ft_amount)),
    )
    .await
    .unwrap();

    let result = dex_engine_account
        .call(dex_engine_contract.id(), "rescue")
        .max_gas()
        .args_json(json!({
            "asset_id": AssetId::Nep141(ft.id().clone()),
            "to": user2.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("Nothing to rescue"));

    let result = user1
        .transfer_near(dex_engine_contract.id(), near_transfer_amount)
        .await
        .unwrap();
    assert!(result.is_success());
    wait_for_blocks(&sandbox, 3).await;
    let untracked_near = assert_untracked_near(&dex_engine_contract).await.unwrap();
    assert!(untracked_near >= near_transfer_amount);

    let result = dex_engine_account
        .call(dex_engine_contract.id(), "rescue")
        .max_gas()
        .args_json(json!({
            "asset_id": AssetId::Near,
            "amount": U128(untracked_near.as_yoctonear() + 1),
            "to": user3.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("Can't rescue"));

    let user3_balance_before = user3.view_account().await.unwrap().balance;
    let result = dex_engine_account
        .call(dex_engine_contract.id(), "rescue")
        .max_gas()
        .args_json(json!({
            "asset_id": AssetId::Near,
            "amount": U128(near_transfer_amount.as_yoctonear()),
            "to": user3.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert_near_balance(
        &user3,
        user3_balance_before.saturating_add(near_transfer_amount),
    )
    .await
    .unwrap();
    wait_for_blocks(&sandbox, 3).await;
    assert_untracked_near(&dex_engine_contract).await.unwrap();

    set_paused(&dex_engine_contract, &pauser, false).await;

    let result = dex_engine_account
        .call(dex_engine_contract.id(), "rescue")
        .max_gas()
        .args_json(json!({
            "asset_id": AssetId::Near,
            "to": user3.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("Contract must be paused to rescue assets"));
}

#[tokio::test]
async fn test_registered_assets_index() {
    let near_deposit = NearToken::from_near(1);

    let TestContext {
        dex_engine_contract,
        user1,
        user2,
        ft1,
        ft2,
        ft3,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        register_assets_for_all: true,
        ..Default::default()
    })
    .await;
    let new_account_id: AccountId = "new-account.test.near".parse().unwrap();
    let dex_id: DexId = format!("{}/unlisted", user2.id()).parse().unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let user1_registered_assets = vec![
        (AssetId::Near, U128(near_deposit.as_yoctonear())),
        (AssetId::Nep141(ft1.id().clone()), U128(0)),
        (AssetId::Nep141(ft2.id().clone()), U128(0)),
        (AssetId::Nep141(ft3.id().clone()), U128(0)),
    ];
    let user1_of = AccountOrDexId::Account(user1.id().clone());
    assert_eq!(
        get_registered_assets(&dex_engine_contract, &user1_of, 0, 10)
            .await
            .unwrap(),
        user1_registered_assets,
    );
    assert_eq!(
        get_registered_assets(&dex_engine_contract, &user1_of, 1, 2)
            .await
            .unwrap(),
        user1_registered_assets[1..3],
    );
    assert!(
        get_registered_assets(&dex_engine_contract, &user1_of, 4, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        get_registered_assets(
            &dex_engine_contract,
            &AccountOrDexId::Account(new_account_id.clone()),
            0,
            10
        )
        .await
        .unwrap()
        .is_empty()
    );

    // Registering what's registered changes nothing
    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Nep141(ft2.id().clone()), AssetId::Near],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert_eq!(
        get_registered_assets(&dex_engine_contract, &user1_of, 0, 10)
            .await
            .unwrap(),
        user1_registered_assets,
    );

    let result = user2
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Nep141(ft1.id().clone())],
            "for": AccountOrDexId::Account(new_account_id.clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert_eq!(
        get_registered_assets(
            &dex_engine_contract,
            &AccountOrDexId::Account(new_account_id.clone()),
            0,
            10
        )
        .await
        .unwrap(),
        vec![(AssetId::Nep141(ft1.id().clone()), U128(0))],
    );

    // The assets of a dex can be listed too
    let dex_of = AccountOrDexId::Dex(dex_id.clone());
    assert!(
        get_registered_assets(&dex_engine_contract, &dex_of, 0, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let result = user2
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Nep141(ft3.id().clone()), AssetId::Near],
            "for": dex_of.clone(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert_eq!(
        get_registered_assets(&dex_engine_contract, &dex_of, 0, 10)
            .await
            .unwrap(),
        vec![
            (AssetId::Nep141(ft3.id().clone()), U128(0)),
            (AssetId::Near, U128(0)),
        ],
    );
    let are_registered: bool = dex_engine_contract
        .view("are_assets_registered")
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft3.id().clone())],
            "for": dex_of.clone(),
        }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert!(are_registered);
    let are_registered: bool = dex_engine_contract
        .view("are_assets_registered")
        .args_json(json!({
            "asset_ids": [AssetId::Nep141(ft1.id().clone())],
            "for": dex_of,
        }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert!(!are_registered);
    assert_untracked_near(&dex_engine_contract).await.unwrap();
}

#[tokio::test]
async fn test_is_paused() {
    let TestContext {
        sandbox,
        dex_engine_contract,
        user1,
        ..
    } = setup_test_environment().await;
    let pauser = create_pauser(&sandbox).await;

    assert!(!is_paused(&dex_engine_contract).await.unwrap());

    let result = user1
        .call(dex_engine_contract.id(), "pause")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    println!("{result:?}");
    assert!(format!("{result:?}").contains("Only authorized accounts can pause the contract"));
    assert!(!is_paused(&dex_engine_contract).await.unwrap());

    set_paused(&dex_engine_contract, &pauser, true).await;
    assert!(is_paused(&dex_engine_contract).await.unwrap());

    // Storage balances don't change while paused either
    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(engine_user_storage_deposit())
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("Contract is paused"));
    let result = user1
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({ "dex_id": format!("{}/dex", user1.id()) }))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("Contract is paused"));

    set_paused(&dex_engine_contract, &pauser, false).await;
    assert!(!is_paused(&dex_engine_contract).await.unwrap());

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(engine_user_storage_deposit())
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    set_paused(&dex_engine_contract, &pauser, true).await;
    let result = user1
        .call(dex_engine_contract.id(), "storage_withdraw")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("Contract is paused"));
}
#[tokio::test]
async fn test_dex_code_view() {
    let TestContext {
        dex_engine_contract,
        deployer,
        ..
    } = setup_test_environment().await;
    let wasms = get_compiled_wasms().await;
    let dex_wasm = &wasms.test_dex_wasm;

    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    let result = deployer
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "deploy_dex_code")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "last_part_of_id": dex_id.id,
            "code_base64": BASE64_STANDARD.encode(dex_wasm),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let code: Base64VecU8 = dex_engine_contract
        .view("dex_code")
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(&code.0, dex_wasm);

    let missing_dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "missing".to_string(),
    };
    let error = dex_engine_contract
        .view("dex_code")
        .args_json(json!({
            "dex_id": missing_dex_id,
        }))
        .await
        .unwrap_err();
    assert!(
        format!("{error:?}").contains("Dex code not found"),
        "{error:?}"
    );
}

#[tokio::test]
async fn test_call_after_dex_code_deploy() {
    let TestContext {
        dex_engine_contract,
        deployer,
        ..
    } = setup_test_environment().await;
    let wasms = get_compiled_wasms().await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };
    let result = deployer
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let deploy = |code: &[u8], method: &str| {
        deployer
            .call(dex_engine_contract.id(), "deploy_dex_code")
            .gas(Gas::from_tgas(1_000))
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "last_part_of_id": dex_id.id,
                "code_base64": BASE64_STANDARD.encode(code),
                "call_after_deploy": DexCallAfterDeploy {
                    method: method.to_string(),
                    args: Base64VecU8(Vec::new()),
                    attached_assets: HashMap::new(),
                },
            }))
            .transact()
    };
    let dex_code = || async {
        dex_engine_contract
            .view("dex_code")
            .args_json(json!({
                "dex_id": dex_id,
            }))
            .await
            .unwrap()
            .json::<Base64VecU8>()
            .unwrap()
            .0
    };

    // The call runs on the new code as a dex call of the deployer, such as the dex's initialization
    let result = deploy(&wasms.test_dex_wasm, "new").await.unwrap();
    assert_success(&result).unwrap();
    assert_eq!(dex_code().await, wasms.test_dex_wasm);
    let result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id,
            "method": "new",
            "args": Base64VecU8(Vec::new()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert!(
        format!("{result:?}").contains("The contract has already been initialized"),
        "{result:?}"
    );

    // The code isn't deployed if the call fails
    let result = deploy(&wasms.otc_dex_wasm, "create_pool").await.unwrap();
    assert!(
        format!("{result:?}").contains("Failed to get function 'create_pool'"),
        "{result:?}"
    );
    assert_eq!(dex_code().await, wasms.test_dex_wasm);
}

#[tokio::test]
async fn test_translation_of_replaced_dex_code() {
    let TestContext {
        dex_engine_contract,
        deployer,
        user1,
        ..
    } = setup_test_environment().await;
    let wasms = get_compiled_wasms().await;

    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    let result = deployer
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_dex_storage_deposit())
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let deploy = |code: &[u8]| {
        Function::new("deploy_dex_code")
            .args_json(json!({
                "last_part_of_id": dex_id.id,
                "code_base64": BASE64_STANDARD.encode(code),
            }))
            .deposit(NearToken::from_yoctonear(1))
            .gas(Gas::from_tgas(450))
    };
    let translate = || {
        Function::new("translate_dex_codes")
            .args_json(json!({
                "dex_ids": [dex_id],
            }))
            .gas(Gas::from_tgas(450))
    };
    let otc_only_view = || async {
        dex_engine_contract
            .view("dex_view")
            .args_json(json!({
                "dex_id": dex_id,
                "method": "get_balance",
                "args": Base64VecU8(Vec::new()),
            }))
            .await
            .unwrap_err()
    };

    let result = deployer
        .batch(dex_engine_contract.id())
        .call(deploy(&wasms.otc_dex_wasm))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    // The translation reads the otc code before the deployment stores the test dex's code, and
    // returns after it, so it's of code that the dex doesn't have anymore
    let result = deployer
        .batch(dex_engine_contract.id())
        .call(deploy(&wasms.test_dex_wasm))
        .call(translate())
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert!(
        format!("{result:?}").contains(&format!(
            "The code of {dex_id} changed while it was translated"
        )),
        "{result:?}"
    );
    let code: Base64VecU8 = dex_engine_contract
        .view("dex_code")
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(&code.0, &wasms.test_dex_wasm);
    let error = otc_only_view().await;
    assert!(
        format!("{error:?}").contains("Failed to get function 'get_balance'"),
        "{error:?}"
    );

    // Anyone can translate the code that the dex has again
    let result = user1
        .batch(dex_engine_contract.id())
        .call(translate())
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert!(result.failures().is_empty(), "{result:?}");
    let error = otc_only_view().await;
    assert!(
        format!("{error:?}").contains("Failed to get function 'get_balance'"),
        "{error:?}"
    );
}

#[tokio::test]
async fn test_referral_fees() {
    let pool_creation_fee = NearToken::from_millinear(10);
    let add_liquidity_ft1 = 1_000_000_000u128;
    let add_liquidity_ft2 = 2_000_000_000u128;
    let trader_ft1_for_swaps = 500_000_000u128;
    let first_swap_amount = 100_000_000u128;
    let second_swap_amount = 200_000_000u128;
    let exact_out_amount = 10_000_000u128;
    let deposited_swap_amount = 50_000_000u128;
    let user2_fee_fraction = FeeFraction::new(20_000); // 2%
    let user3_fee_fraction = FeeFraction::new(10_000); // 1%

    let wasms = get_compiled_wasms().await;
    let TestContext {
        dex_engine_contract,
        ft1,
        ft2,
        deployer,
        user1,
        user2,
        user3,
        user4,
        user5,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.test_dex_wasm.clone(),
            init_method: Some(("new".to_string(), vec![])),
        }),
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
    })
    .await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    let result = deployer
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(pool_creation_fee)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    for (ft, amount) in [(&ft1, add_liquidity_ft1), (&ft2, add_liquidity_ft2)] {
        let result = deployer
            .call(ft.id(), "ft_transfer_call")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": dex_engine_contract.id(),
                "amount": U128(amount),
                "msg": "",
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();
    }
    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": [
                Operation::DexCall {
                    dex_id: dex_id.clone(),
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
                        TokenAmount::new(pool_creation_fee.as_yoctonear()),
                    )]),
                },
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "add_liquidity".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: 0 }).unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([
                        (AssetId::Nep141(ft1.id().clone()), TokenAmount::new(add_liquidity_ft1)),
                        (AssetId::Nep141(ft2.id().clone()), TokenAmount::new(add_liquidity_ft2)),
                    ]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(trader_ft1_for_swaps),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let result = user1
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(trader_ft1_for_swaps),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let ft1_asset_id = AssetId::Nep141(ft1.id().clone());
    let ft2_asset_id = AssetId::Nep141(ft2.id().clone());
    let unregistered_referrer: AccountId = "unregistered.test.near".parse().unwrap();
    let swap_message =
        Base64VecU8(near_sdk::borsh::to_vec(&SwapMessage::Pool { pool_id: 0 }).unwrap());
    let ft1_to_ft2 =
        |amount: SwapRequestAmount, constraint: Option<TokenAmount>| Operation::SwapSimple {
            dex_id: dex_id.clone(),
            message: swap_message.clone(),
            asset_in: ft1_asset_id.clone(),
            asset_out: ft2_asset_id.clone(),
            amount: SwapOperationAmount::Amount(amount),
            constraint,
        };
    let execute = |operations: Vec<Operation>, referral_fees: Vec<(AccountId, FeeFraction)>| {
        user1
            .call(dex_engine_contract.id(), "execute_operations")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "operations": operations,
                "referrer": user2.id(),
                "referral_fees": referral_fees,
            }))
            .transact()
    };
    let simulate = |amount: SwapRequestAmount, referral_fees: Vec<(AccountId, FeeFraction)>| {
        let view = dex_engine_contract
            .view("simulate_swap_simple")
            .args_json(json!({
                "dex_id": dex_id,
                "message": swap_message,
                "trader": user1.id(),
                "asset_in": ft1_asset_id,
                "asset_out": ft2_asset_id,
                "amount": amount,
                "referral_fees": referral_fees,
            }));
        async move { view.await.unwrap().json::<(U128, U128)>().unwrap() }
    };
    let balance = |of: AccountOrDexId, asset_id: &AssetId| {
        let dex_engine_contract = dex_engine_contract.clone();
        let asset_id = asset_id.clone();
        async move {
            get_inner_asset_balance(&dex_engine_contract, of, asset_id)
                .await
                .unwrap()
                .unwrap()
                .0
        }
    };
    let user1_balance = AccountOrDexId::Account(user1.id().clone());
    let user2_balance = AccountOrDexId::Account(user2.id().clone());
    let user3_balance = AccountOrDexId::Account(user3.id().clone());
    let dex_balance = AccountOrDexId::Dex(dex_id.clone());
    let fee_of = |amount: u128, fee_fraction: FeeFraction| {
        amount
            .checked_mul(u128::from(fee_fraction.get()))
            .unwrap()
            .checked_div(1_000_000)
            .unwrap()
    };

    // The fee is taken out of an exact input before the dex gets it, and only paid to
    // referrers who registered the input asset, so the trader keeps the unregistered one's
    let referral_fees = vec![
        (user2.id().clone(), user2_fee_fraction),
        (unregistered_referrer.clone(), user3_fee_fraction),
    ];
    let user2_fee = fee_of(first_swap_amount, user2_fee_fraction);
    let (simulated_amount_in, simulated_amount_out) = simulate(
        SwapRequestAmount::ExactIn(TokenAmount::new(first_swap_amount)),
        referral_fees.clone(),
    )
    .await;
    assert_eq!(simulated_amount_in, U128(first_swap_amount));
    let (_, amount_out_without_fee) = simulate(
        SwapRequestAmount::ExactIn(TokenAmount::new(first_swap_amount - user2_fee)),
        vec![],
    )
    .await;
    assert_eq!(simulated_amount_out, amount_out_without_fee);
    let dex_ft1_before = balance(dex_balance.clone(), &ft1_asset_id).await;
    let result = execute(
        vec![ft1_to_ft2(
            SwapRequestAmount::ExactIn(TokenAmount::new(first_swap_amount)),
            None,
        )],
        referral_fees,
    )
    .await
    .unwrap();
    assert_success(&result).unwrap();
    assert_eq!(
        balance(user2_balance.clone(), &ft1_asset_id).await,
        user2_fee
    );
    assert_eq!(
        balance(dex_balance.clone(), &ft1_asset_id).await,
        dex_ft1_before + first_swap_amount - user2_fee
    );
    let mut user1_ft1 = trader_ft1_for_swaps - first_swap_amount;
    assert_eq!(
        balance(user1_balance.clone(), &ft1_asset_id).await,
        user1_ft1
    );
    assert_eq!(
        balance(user1_balance.clone(), &ft2_asset_id).await,
        simulated_amount_out.0
    );

    // Only the first swap of the operations pays referral fees
    let result = execute(
        vec![
            ft1_to_ft2(
                SwapRequestAmount::ExactIn(TokenAmount::new(second_swap_amount)),
                None,
            ),
            Operation::SwapSimple {
                dex_id: dex_id.clone(),
                message: swap_message.clone(),
                asset_in: ft2_asset_id.clone(),
                asset_out: ft1_asset_id.clone(),
                amount: SwapOperationAmount::OutputOfLastIn,
                constraint: None,
            },
        ],
        vec![(user2.id().clone(), user2_fee_fraction)],
    )
    .await
    .unwrap();
    assert_success(&result).unwrap();
    let mut user2_ft1 = user2_fee + fee_of(second_swap_amount, user2_fee_fraction);
    assert_eq!(
        balance(user2_balance.clone(), &ft1_asset_id).await,
        user2_ft1
    );
    assert_eq!(balance(user2_balance.clone(), &ft2_asset_id).await, 0);
    user1_ft1 = balance(user1_balance.clone(), &ft1_asset_id).await;

    // For an exact output, the fee is added on top of what the dex takes, rounded up, so it's
    // the referrer's fraction of the total input, which the constraint limits
    let (dex_amount_in, _) = simulate(
        SwapRequestAmount::ExactOut(TokenAmount::new(exact_out_amount)),
        vec![],
    )
    .await;
    let user3_fee = (dex_amount_in.0 * u128::from(user3_fee_fraction.get()))
        .div_ceil(1_000_000 - u128::from(user3_fee_fraction.get()));
    let total_amount_in = dex_amount_in.0 + user3_fee;
    let (simulated_amount_in, simulated_amount_out) = simulate(
        SwapRequestAmount::ExactOut(TokenAmount::new(exact_out_amount)),
        vec![(user3.id().clone(), user3_fee_fraction)],
    )
    .await;
    assert_eq!(
        (simulated_amount_in, simulated_amount_out),
        (U128(total_amount_in), U128(exact_out_amount))
    );
    let result = execute(
        vec![ft1_to_ft2(
            SwapRequestAmount::ExactOut(TokenAmount::new(exact_out_amount)),
            Some(TokenAmount::new(total_amount_in - 1)),
        )],
        vec![(user3.id().clone(), user3_fee_fraction)],
    )
    .await
    .unwrap();
    assert!(
        format!("{result:?}").contains("Input amount is greater than constraint"),
        "Expected the constraint to fail, got {result:#?}"
    );
    let result = execute(
        vec![ft1_to_ft2(
            SwapRequestAmount::ExactOut(TokenAmount::new(exact_out_amount)),
            Some(TokenAmount::new(total_amount_in)),
        )],
        vec![(user3.id().clone(), user3_fee_fraction)],
    )
    .await
    .unwrap();
    assert_success(&result).unwrap();
    assert_eq!(
        balance(user3_balance.clone(), &ft1_asset_id).await,
        user3_fee
    );
    user1_ft1 -= total_amount_in;
    assert_eq!(
        balance(user1_balance.clone(), &ft1_asset_id).await,
        user1_ft1
    );

    // Assets deposited with operations pay referral fees from the deposit
    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(deposited_swap_amount),
            "msg": json!({
                "operations": [
                    ft1_to_ft2(SwapRequestAmount::ExactIn(TokenAmount::new(deposited_swap_amount)), None),
                    Operation::Withdraw {
                        asset_id: ft2_asset_id.clone(),
                        amount: WithdrawAmount::PreviousSwapOutput,
                        to: None,
                        rescue_address: None,
                    },
                ],
                "referral_fees": [(user2.id().clone(), user2_fee_fraction)],
            })
            .to_string(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    user2_ft1 += fee_of(deposited_swap_amount, user2_fee_fraction);
    assert_eq!(
        balance(user2_balance.clone(), &ft1_asset_id).await,
        user2_ft1
    );

    let referrers =
        [&user2, &user3, &user4, &user5, &deployer, &user1].map(|referrer| referrer.id().clone());
    for (referral_fees, error) in [
        (
            referrers
                .iter()
                .map(|referrer| (referrer.clone(), FeeFraction::new(1_000)))
                .collect(),
            "At most 5 referrers are allowed",
        ),
        (
            vec![
                (referrers[0].clone(), FeeFraction::new(30_000)),
                (referrers[1].clone(), FeeFraction::new(20_001)),
            ],
            "Referral fees must add up to at most 50000",
        ),
    ] {
        let result = execute(
            vec![ft1_to_ft2(
                SwapRequestAmount::ExactIn(TokenAmount::new(first_swap_amount)),
                None,
            )],
            referral_fees,
        )
        .await
        .unwrap();
        assert!(
            format!("{result:?}").contains(error),
            "Expected {error:?}, got {result:#?}"
        );
    }
    assert_eq!(balance(user2_balance, &ft1_asset_id).await, user2_ft1);
}

#[tokio::test]
async fn test_swap_advanced() {
    let pool_creation_fee = NearToken::from_millinear(10);
    let add_liquidity_ft1 = 1_000_000_000u128;
    let worse_pool_add_liquidity_ft2 = 2_000_000_000u128;
    let better_pool_add_liquidity_ft2 = 3_000_000_000u128;
    let trader_ft1_for_swaps = 500_000_000u128;
    let swap_amount = 1_000_000u128;
    let worse_pool_id = 0u64;
    let better_pool_id = 1u64;
    let missing_pool_id = 2u64;

    let wasms = get_compiled_wasms().await;
    let TestContext {
        dex_engine_contract,
        ft1,
        ft2,
        deployer,
        user1,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.test_dex_wasm.clone(),
            init_method: Some(("new".to_string(), vec![])),
        }),
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
    })
    .await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    #[near(serializers=[borsh])]
    struct GetPoolArgs {
        pool_id: PoolId,
    }

    let ft1_asset_id = AssetId::Nep141(ft1.id().clone());
    let ft2_asset_id = AssetId::Nep141(ft2.id().clone());

    let result = deployer
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(pool_creation_fee.saturating_mul(2))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    for (ft, amount) in [
        (&ft1, add_liquidity_ft1 * 2),
        (
            &ft2,
            worse_pool_add_liquidity_ft2 + better_pool_add_liquidity_ft2,
        ),
    ] {
        let result = deployer
            .call(ft.id(), "ft_transfer_call")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": dex_engine_contract.id(),
                "amount": U128(amount),
                "msg": "",
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();
    }
    // The better pool gives 3 FT2 per FT1, the worse one 2
    for (pool_id, add_liquidity_ft2) in [
        (worse_pool_id, worse_pool_add_liquidity_ft2),
        (better_pool_id, better_pool_add_liquidity_ft2),
    ] {
        let result = deployer
            .call(dex_engine_contract.id(), "execute_operations")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "operations": [
                    Operation::DexCall {
                        dex_id: dex_id.clone(),
                        method: "create_pool".to_string(),
                        args: Base64VecU8(
                            near_sdk::borsh::to_vec(&CreatePoolArgs {
                                assets: (ft1_asset_id.clone(), ft2_asset_id.clone()),
                            })
                            .unwrap(),
                        ),
                        attached_assets: HashMap::from_iter([(
                            AssetId::Near,
                            TokenAmount::new(pool_creation_fee.as_yoctonear()),
                        )]),
                    },
                    Operation::DexCall {
                        dex_id: dex_id.clone(),
                        method: "add_liquidity".to_string(),
                        args: Base64VecU8(
                            near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id }).unwrap(),
                        ),
                        attached_assets: HashMap::from_iter([
                            (ft1_asset_id.clone(), TokenAmount::new(add_liquidity_ft1)),
                            (ft2_asset_id.clone(), TokenAmount::new(add_liquidity_ft2)),
                        ]),
                    },
                ],
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();
    }

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(trader_ft1_for_swaps + swap_amount),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let result = user1
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(trader_ft1_for_swaps),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let option = |pool_id: u64| SwapOption {
        dex_id: dex_id.clone(),
        message: Base64VecU8(near_sdk::borsh::to_vec(&SwapMessage::Pool { pool_id }).unwrap()),
    };
    let swap_advanced = |amount: SwapRequestAmount,
                         constraint: Option<TokenAmount>,
                         pool_ids: &[u64],
                         mode: SwapAdvancedMode| Operation::SwapAdvanced {
        asset_in: ft1_asset_id.clone(),
        asset_out: ft2_asset_id.clone(),
        amount: SwapOperationAmount::Amount(amount),
        constraint,
        options: pool_ids.iter().map(|pool_id| option(*pool_id)).collect(),
        mode,
    };
    let execute = |operations: Vec<Operation>| {
        user1
            .call(dex_engine_contract.id(), "execute_operations")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "operations": operations,
            }))
            .transact()
    };
    let simulate = |pool_id: u64, amount: SwapRequestAmount| {
        let view = dex_engine_contract
            .view("simulate_swap_simple")
            .args_json(json!({
                "dex_id": dex_id,
                "message": option(pool_id).message,
                "trader": user1.id(),
                "asset_in": ft1_asset_id,
                "asset_out": ft2_asset_id,
                "amount": amount,
            }));
        async move { view.await.unwrap().json::<(U128, U128)>().unwrap() }
    };
    let balance = |asset_id: &AssetId| {
        let dex_engine_contract = dex_engine_contract.clone();
        let of = AccountOrDexId::Account(user1.id().clone());
        let asset_id = asset_id.clone();
        async move {
            get_inner_asset_balance(&dex_engine_contract, of, asset_id)
                .await
                .unwrap()
                .unwrap_or_default()
                .0
        }
    };
    let pool_balances = |pool_id: u64| {
        let view = dex_engine_contract.view("dex_view").args_json(json!({
            "dex_id": dex_id,
            "method": "get_pool",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&GetPoolArgs { pool_id }).unwrap()),
        }));
        async move {
            let pool: Option<SimplePool> =
                near_sdk::borsh::from_slice(&view.await.unwrap().json::<Base64VecU8>().unwrap().0)
                    .unwrap();
            let pool = pool.unwrap();
            (pool.assets.0.balance.get(), pool.assets.1.balance.get())
        }
    };

    // FindBest swaps on the option with the most output, and skips the options that fail
    let worse_pool_before = pool_balances(worse_pool_id).await;
    let result = execute(vec![swap_advanced(
        SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
        None,
        &[worse_pool_id, missing_pool_id, better_pool_id],
        SwapAdvancedMode::FindBest,
    )])
    .await
    .unwrap();
    assert_success(&result).unwrap();
    // (1_000_000 * 3_000_000_000) / (1_000_000_000 + 1_000_000) = 2_997_002
    let expected_output = 2_997_002u128;
    assert_eq!(balance(&ft2_asset_id).await, expected_output);
    assert_eq!(
        pool_balances(better_pool_id).await,
        (
            add_liquidity_ft1 + swap_amount,
            better_pool_add_liquidity_ft2 - expected_output
        )
    );
    assert_eq!(pool_balances(worse_pool_id).await, worse_pool_before);
    assert!(
        result.logs().iter().any(|log| log
            .contains(&format!("Skipping swap option on {dex_id}: "))
            && log.contains("Pool not found")),
        "Expected the missing pool to be skipped, got {:#?}",
        result.logs()
    );
    let mut user1_ft1 = trader_ft1_for_swaps - swap_amount;
    let mut user1_ft2 = expected_output;

    // For an exact output, FindBest swaps on the option that takes the least input
    let (worse_amount_in, _) = simulate(
        worse_pool_id,
        SwapRequestAmount::ExactOut(TokenAmount::new(swap_amount)),
    )
    .await;
    let (better_amount_in, _) = simulate(
        better_pool_id,
        SwapRequestAmount::ExactOut(TokenAmount::new(swap_amount)),
    )
    .await;
    assert!(better_amount_in < worse_amount_in);
    let worse_pool_before = pool_balances(worse_pool_id).await;
    let result = execute(vec![swap_advanced(
        SwapRequestAmount::ExactOut(TokenAmount::new(swap_amount)),
        Some(TokenAmount::new(better_amount_in.0)),
        &[worse_pool_id, better_pool_id],
        SwapAdvancedMode::FindBest,
    )])
    .await
    .unwrap();
    assert_success(&result).unwrap();
    user1_ft1 -= better_amount_in.0;
    user1_ft2 += swap_amount;
    assert_eq!(balance(&ft1_asset_id).await, user1_ft1);
    assert_eq!(balance(&ft2_asset_id).await, user1_ft2);
    assert_eq!(pool_balances(worse_pool_id).await, worse_pool_before);

    // FallbackQueue swaps on the first option that works, even if a later one is better
    let (_, worse_amount_out) = simulate(
        worse_pool_id,
        SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
    )
    .await;
    let better_pool_before = pool_balances(better_pool_id).await;
    let result = execute(vec![swap_advanced(
        SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
        None,
        &[missing_pool_id, worse_pool_id, better_pool_id],
        SwapAdvancedMode::FallbackQueue,
    )])
    .await
    .unwrap();
    assert_success(&result).unwrap();
    user1_ft1 -= swap_amount;
    user1_ft2 += worse_amount_out.0;
    assert_eq!(balance(&ft1_asset_id).await, user1_ft1);
    assert_eq!(balance(&ft2_asset_id).await, user1_ft2);
    assert_eq!(pool_balances(better_pool_id).await, better_pool_before);

    // FallbackQueue skips the options that don't meet the constraint
    let (_, worse_amount_out) = simulate(
        worse_pool_id,
        SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
    )
    .await;
    let (_, better_amount_out) = simulate(
        better_pool_id,
        SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
    )
    .await;
    let worse_pool_before = pool_balances(worse_pool_id).await;
    let result = execute(vec![swap_advanced(
        SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
        Some(TokenAmount::new(worse_amount_out.0 + 1)),
        &[worse_pool_id, better_pool_id],
        SwapAdvancedMode::FallbackQueue,
    )])
    .await
    .unwrap();
    assert_success(&result).unwrap();
    user1_ft1 -= swap_amount;
    user1_ft2 += better_amount_out.0;
    assert_eq!(balance(&ft1_asset_id).await, user1_ft1);
    assert_eq!(balance(&ft2_asset_id).await, user1_ft2);
    assert_eq!(pool_balances(worse_pool_id).await, worse_pool_before);
    assert!(
        result
            .logs()
            .iter()
            .any(|log| log.contains("Output amount is less than constraint")),
        "Expected the worse pool to be skipped, got {:#?}",
        result.logs()
    );

    // Should fail, without changing balances
    let (_, better_amount_out) = simulate(
        better_pool_id,
        SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
    )
    .await;
    let unreachable_constraint = Some(TokenAmount::new(better_amount_out.0 + 1));
    for (operation, expected_error) in [
        (
            swap_advanced(
                SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
                None,
                &[],
                SwapAdvancedMode::FindBest,
            ),
            "At least one swap option is required",
        ),
        (
            swap_advanced(
                SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
                None,
                &[missing_pool_id, missing_pool_id],
                SwapAdvancedMode::FindBest,
            ),
            "All swap options failed",
        ),
        (
            swap_advanced(
                SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
                unreachable_constraint,
                &[worse_pool_id, better_pool_id],
                SwapAdvancedMode::FindBest,
            ),
            "Output amount is less than constraint",
        ),
        (
            swap_advanced(
                SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
                unreachable_constraint,
                &[worse_pool_id, better_pool_id],
                SwapAdvancedMode::FallbackQueue,
            ),
            "All swap options failed",
        ),
    ] {
        let result = execute(vec![operation]).await.unwrap();
        assert!(
            format!("{result:?}").contains(expected_error),
            "Expected {expected_error:?}, got {result:#?}"
        );
    }
    assert_eq!(balance(&ft1_asset_id).await, user1_ft1);
    assert_eq!(balance(&ft2_asset_id).await, user1_ft2);

    // SwapAdvanced works with assets transferred with the operations
    let (_, better_amount_out) = simulate(
        better_pool_id,
        SwapRequestAmount::ExactIn(TokenAmount::new(swap_amount)),
    )
    .await;
    let result = user1
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(swap_amount),
            "msg": near_sdk::serde_json::to_string(&vec![
                Operation::SwapAdvanced {
                    asset_in: ft1_asset_id.clone(),
                    asset_out: ft2_asset_id.clone(),
                    amount: SwapOperationAmount::EntireBalanceIn,
                    constraint: None,
                    options: vec![option(worse_pool_id), option(better_pool_id)],
                    mode: SwapAdvancedMode::FindBest,
                },
                Operation::Withdraw {
                    asset_id: ft2_asset_id.clone(),
                    amount: WithdrawAmount::PreviousSwapOutput,
                    to: None,
                    rescue_address: None,
                },
            ]).unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert_ft_balance(&user1, ft2.clone(), better_amount_out)
        .await
        .unwrap();
}

async fn code_hash(contract: &Contract) -> [u8; 32] {
    let near_workspaces::ContractState::LocalHash(code_hash) =
        contract.view_account().await.unwrap().contract_state
    else {
        panic!("{} should have code of its own", contract.id());
    };
    code_hash.0
}

/// Adds a custom section to make the code larger
fn with_padding_section(code: &[u8], padding_length: usize) -> Vec<u8> {
    let section_name = b"padding";
    let mut padding_section = vec![section_name.len() as u8];
    padding_section.extend(section_name);
    padding_section.extend(vec![0; padding_length]);
    let custom_section_id = 0;
    let mut padded_code = [code, &[custom_section_id]].concat();
    // The section's length in LEB128
    let mut section_length = padding_section.len();
    loop {
        let low_bits = (section_length & 0x7f) as u8;
        section_length >>= 7;
        if section_length == 0 {
            padded_code.push(low_bits);
            break;
        }
        padded_code.push(low_bits | 0x80);
    }
    padded_code.extend(padding_section);
    padded_code
}

#[tokio::test]
async fn test_deploy_code_chunks() {
    let TestContext {
        dex_engine_contract,
        user1,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        register_assets_for_all: true,
        ..Default::default()
    })
    .await;
    let old_code = &get_compiled_wasms().await.contract_wasm;
    // Over the 1.5 MiB that a transaction can have, whatever the size of the engine's code
    let new_code = with_padding_section(old_code, 1_700_000 - old_code.len());
    assert!(new_code.len() > 1_572_864);
    let chunk_size = 700_000;
    let [first_chunk, second_chunk, last_chunk] =
        new_code.chunks(chunk_size).collect::<Vec<_>>()[..]
    else {
        panic!("The new code should take 3 chunks");
    };
    let new_code_hash = near_sdk::env::sha256_array(&new_code);
    let hash_up_to = |end: usize| near_sdk::env::sha256_array(&new_code[..end]);
    let store_chunk = |offset: usize, chunk: &[u8], code_hash_so_far: [u8; 32]| {
        dex_engine_contract
            .call("store_code_chunk")
            .args_borsh((offset as u32, chunk.to_vec(), code_hash_so_far))
            .max_gas()
            .transact()
    };
    let deploy = |offset: usize, last_chunk: &[u8], code_hash: [u8; 32], then_call: &str| {
        dex_engine_contract
            .call("deploy_code_chunks")
            .args_borsh((
                offset as u32,
                last_chunk.to_vec(),
                code_hash,
                then_call.to_string(),
                b"{}".to_vec(),
            ))
            .max_gas()
            .transact()
    };
    let old_code_hash = code_hash(&dex_engine_contract).await;
    let old_storage_usage = dex_engine_contract
        .view_account()
        .await
        .unwrap()
        .storage_usage;

    // Only the engine's account can store chunks
    let result = user1
        .call(dex_engine_contract.id(), "store_code_chunk")
        .args_borsh((0u32, first_chunk.to_vec(), hash_up_to(chunk_size)))
        .max_gas()
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("is private"), "{result:?}");

    // A chunk with a wrong hash isn't stored
    let result = store_chunk(0, first_chunk, [0; 32]).await.unwrap();
    assert!(
        format!("{result:?}")
            .contains("The code stored so far doesn't have the hash it should have"),
        "{result:?}"
    );
    let result = store_chunk(chunk_size, second_chunk, hash_up_to(2 * chunk_size))
        .await
        .unwrap();
    assert!(
        format!("{result:?}")
            .contains("Only 0 bytes of the code are stored, so a chunk can't start at byte 700000"),
        "{result:?}"
    );

    // A chunk sent again replaces itself
    for (offset, chunk) in [
        (0, first_chunk),
        (chunk_size, second_chunk),
        (chunk_size, second_chunk),
    ] {
        let result = store_chunk(offset, chunk, hash_up_to(offset + chunk.len()))
            .await
            .unwrap();
        assert_success(&result).unwrap();
    }

    // Code with a wrong hash isn't deployed
    let result = deploy(2 * chunk_size, last_chunk, [0; 32], "is_paused")
        .await
        .unwrap();
    assert!(
        format!("{result:?}")
            .contains("The code stored so far doesn't have the hash it should have"),
        "{result:?}"
    );

    // If the call fails, the old code stays, and the uploaded code is gone
    let result = deploy(2 * chunk_size, last_chunk, new_code_hash, "no_such_method")
        .await
        .unwrap();
    assert!(result.is_failure(), "{result:?}");
    assert_eq!(code_hash(&dex_engine_contract).await, old_code_hash);
    let result = deploy(2 * chunk_size, last_chunk, new_code_hash, "is_paused")
        .await
        .unwrap();
    assert!(
        format!("{result:?}").contains(
            "Only 0 bytes of the code are stored, so a chunk can't start at byte 1400000"
        ),
        "{result:?}"
    );

    // NEAR deploys code that doesn't compile, but the call fails
    let broken_code = [b"\0asm".as_slice(), &[0xff; 100]].concat();
    let broken_code_hash = near_sdk::env::sha256_array(&broken_code);
    let result = deploy(0, &broken_code, broken_code_hash, "is_paused")
        .await
        .unwrap();
    assert!(
        format!("{result:?}").contains("CompilationError"),
        "{result:?}"
    );
    assert_eq!(code_hash(&dex_engine_contract).await, old_code_hash);
    assert!(!is_paused(&dex_engine_contract).await.unwrap());

    // Offset 0 starts over, and then_call gets then_call_args
    let result = store_chunk(0, &broken_code, broken_code_hash)
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let result = deploy_code_in_chunks(
        &dex_engine_contract,
        &new_code,
        chunk_size,
        "storage_balance_of",
        json!({ "account_id": user1.id() }).to_string().as_bytes(),
    )
    .await;
    assert_success(&result).unwrap();
    println!(
        "Deployed {} bytes from chunks of {chunk_size} bytes with {:.1} TGas",
        new_code.len(),
        result.total_gas_burnt.as_gas() as f64 / 1e12
    );
    let storage_balance = get_storage_balance(&dex_engine_contract, user1.id())
        .await
        .unwrap();
    assert!(storage_balance.is_some());
    assert_eq!(
        result.json::<near_sdk::serde_json::Value>().unwrap(),
        near_sdk::serde_json::to_value(storage_balance).unwrap()
    );
    assert_eq!(code_hash(&dex_engine_contract).await, new_code_hash);
    assert!(!is_paused(&dex_engine_contract).await.unwrap());
    // Nothing stays in storage
    assert_eq!(
        dex_engine_contract
            .view_account()
            .await
            .unwrap()
            .storage_usage,
        old_storage_usage + (new_code.len() - old_code.len()) as u64
    );
}

/// The gas balance of a gas key of the dex engine, or None if it doesn't exist
async fn gas_key_balance(
    sandbox: &Worker<Sandbox>,
    dex_engine_contract: &Contract,
    key: &SecretKey,
) -> Option<NearToken> {
    let response = sandbox_rpc_client(sandbox)
        .call(methods::query::RpcQueryRequest {
            block_reference: BlockReference::Finality(Finality::None),
            request: QueryRequest::ViewAccessKey {
                account_id: dex_engine_contract.id().clone(),
                public_key: key.public_key(),
            },
        })
        .await;
    match response {
        Ok(response) => {
            let QueryResponseKind::AccessKey(AccessKeyView {
                permission: AccessKeyPermissionView::GasKeyFunctionCall { balance, .. },
                ..
            }) = response.kind
            else {
                panic!("{} is not a gas key", key.public_key());
            };
            Some(balance)
        }
        Err(JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
            RpcQueryError::UnknownAccessKey { .. },
        ))) => None,
        Err(err) => panic!("{err}"),
    }
}

/// Wait for a transaction sent with `transact_async`, asking again when the
/// sandbox is too busy to answer, as it is after many receipts at once
async fn wait_for_transaction(transaction: TransactionStatus) -> ExecutionFinalResult {
    for _ in 0..100 {
        match transaction.status().await {
            Ok(std::task::Poll::Ready(result)) => return result,
            Ok(std::task::Poll::Pending) => {}
            Err(err) => println!("Asking again after: {err}"),
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    panic!("Transaction didn't finish");
}

#[tokio::test]
async fn test_oracle() {
    let rate = 2u128;
    let request = b"rate of the swap".to_vec();
    let dex_ft2_liquidity = 1_000_000_000_000_000_000_000_000_000u128;
    let user1_ft1 = 1_000_000u128;
    let ft1_deposit = 1_000u128;
    let swap_amount = 100u128;
    let near_deposit = NearToken::from_near(1);
    let oracle_key_deposit = NearToken::from_near(2);
    let top_up_amount = NearToken::from_millinear(500);
    let max_gas_cost = NearToken::from_millinear(100);

    let TestContext {
        sandbox,
        dex_engine_contract,
        user1,
        user3,
        user4,
        deployer,
        ft1,
        ft2,
        ..
    } = setup_test_environment_on_own_sandbox(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: get_compiled_wasms().await.test_dex_wasm.clone(),
            init_method: Some(("new".to_string(), vec![])),
        }),
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
    })
    .await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };
    let ft1_asset_id = AssetId::Nep141(ft1.id().clone());
    let ft2_asset_id = AssetId::Nep141(ft2.id().clone());
    let storage_usage = || async {
        dex_engine_contract
            .view_account()
            .await
            .unwrap()
            .storage_usage
    };

    // The dex pays swaps at the oracle's rate with ft2, and user1 has ft1
    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(dex_ft2_liquidity),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let result = deployer
        .call(dex_engine_contract.id(), "transfer_asset")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "to": AccountOrDexId::Dex(dex_id.clone()),
            "asset_id": ft2_asset_id.clone(),
            "amount": U128(dex_ft2_liquidity),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(user1_ft1),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let storage_usage_before = storage_usage().await;
    let oracle_key = create_oracle_key(
        &dex_engine_contract,
        &user3,
        KeyType::ED25519,
        oracle_key_deposit,
    )
    .await;
    let oracle_key_balance = gas_key_balance(&sandbox, &dex_engine_contract, &oracle_key)
        .await
        .unwrap();
    let oracle_key_storage_cost = oracle_key_deposit.saturating_sub(oracle_key_balance);
    // The deposit pays exactly for the storage that the key takes
    assert_eq!(
        oracle_key_storage_cost,
        near_sdk::env::storage_byte_cost()
            .saturating_mul((storage_usage().await - storage_usage_before) as u128)
    );

    let oracle_request = |request: Vec<u8>| Operation::OracleRequest {
        oracle_key: oracle_key_handle(&oracle_key),
        dex_id: dex_id.clone(),
        request: Base64VecU8(request),
    };
    let oracle_rate_swap =
        |asset_in: AssetId, asset_out: AssetId, amount: u128| Operation::SwapSimple {
            dex_id: dex_id.clone(),
            message: Base64VecU8(
                near_sdk::borsh::to_vec(&SwapMessage::OracleRate {
                    oracle_key: oracle_key_handle(&oracle_key),
                    request: request.clone(),
                })
                .unwrap(),
            ),
            asset_in,
            asset_out,
            amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(TokenAmount::new(
                amount,
            ))),
            constraint: None,
        };
    let withdraw_swap_output = |asset_id: AssetId| Operation::Withdraw {
        asset_id,
        amount: WithdrawAmount::PreviousSwapOutput,
        to: None,
        rescue_address: None,
    };
    let execute_operations = |operations: Vec<Operation>| {
        user1
            .call(dex_engine_contract.id(), "execute_operations")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "operations": operations,
            }))
    };
    let deposit_near = |operations: Vec<Operation>| {
        user1
            .call(dex_engine_contract.id(), "deposit_near")
            .max_gas()
            .deposit(near_deposit)
            .args_json(json!({
                "operations": operations,
            }))
    };
    let deposit_ft1 = |operations: Vec<Operation>| {
        user1
            .call(ft1.id(), "ft_transfer_call")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": dex_engine_contract.id(),
                "amount": U128(ft1_deposit),
                "msg": near_sdk::serde_json::to_string(&operations).unwrap(),
            }))
    };
    let rate_response = near_sdk::borsh::to_vec(&rate).unwrap();
    let respond =
        |oracle_key: &SecretKey, request: &[u8], response: Vec<u8>, nonces: Vec<Base64VecU8>| {
            let request_hash = Base58CryptoHash::from(near_sdk::env::sha256_array(request));
            let oracle_key = oracle_key.clone();
            let sandbox = &sandbox;
            let dex_engine_contract = &dex_engine_contract;
            async move {
                call_engine_with_gas_key(
                    sandbox,
                    dex_engine_contract,
                    &oracle_key,
                    "oracle_respond",
                    json!({
                        "request_hash": request_hash,
                        "response": Base64VecU8(response),
                        "nonces": nonces,
                    }),
                )
                .await
            }
        };

    // Without a request, the oracle has nothing to respond to
    let result = execute_operations(vec![oracle_rate_swap(
        ft1_asset_id.clone(),
        ft2_asset_id.clone(),
        swap_amount,
    )])
    .transact()
    .await
    .unwrap();
    assert!(format!("{result:?}").contains("Oracle didn't respond"));

    // A swap at the rate the oracle responds with
    let result = user1
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft1_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let waiting = execute_operations(vec![
        oracle_request(request.clone()),
        oracle_rate_swap(ft1_asset_id.clone(), ft2_asset_id.clone(), swap_amount),
    ])
    .transact_async()
    .await
    .unwrap();
    let waiting_operations = waiting_for_oracle_request(&sandbox, &[&waiting], &request).await;
    let outcome = respond(
        &oracle_key,
        &request,
        rate_response.clone(),
        waiting_operations,
    )
    .await;
    assert_gas_key_call_success(&outcome).unwrap();
    let result = waiting.await.unwrap();
    assert_success(&result).unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        ft1_asset_id.clone(),
        Some(U128(ft1_deposit - swap_amount)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        ft2_asset_id.clone(),
        Some(U128(swap_amount * rate)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        ft2_asset_id.clone(),
        Some(U128(dex_ft2_liquidity - swap_amount * rate)),
    )
    .await
    .unwrap();
    let mut user1_ft1_wallet = user1_ft1 - ft1_deposit;
    let mut ft1_in_custody = ft1_deposit;
    let mut ft2_in_custody = dex_ft2_liquidity;

    // Deposits wait for the same request, and one response lets both of them run
    let waiting_near_deposit = deposit_near(vec![
        oracle_request(request.clone()),
        oracle_rate_swap(
            AssetId::Near,
            ft2_asset_id.clone(),
            near_deposit.as_yoctonear(),
        ),
        withdraw_swap_output(ft2_asset_id.clone()),
    ])
    .transact_async()
    .await
    .unwrap();
    let waiting_ft_deposit = deposit_ft1(vec![
        oracle_request(request.clone()),
        oracle_rate_swap(ft1_asset_id.clone(), ft2_asset_id.clone(), ft1_deposit),
        withdraw_swap_output(ft2_asset_id.clone()),
    ])
    .transact_async()
    .await
    .unwrap();
    let waiting_operations = waiting_for_oracle_request(
        &sandbox,
        &[&waiting_near_deposit, &waiting_ft_deposit],
        &request,
    )
    .await;
    // Held deposits stay in custody while they wait
    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(near_deposit.as_yoctonear())),
    )
    .await
    .unwrap();
    assert_total_in_custody(
        &dex_engine_contract,
        ft1_asset_id.clone(),
        Some(U128(ft1_in_custody + ft1_deposit)),
    )
    .await
    .unwrap();
    let outcome = respond(
        &oracle_key,
        &request,
        rate_response.clone(),
        waiting_operations,
    )
    .await;
    assert_gas_key_call_success(&outcome).unwrap();
    let result = waiting_near_deposit.await.unwrap();
    assert_success(&result).unwrap();
    // None of the NEAR was refunded
    assert_eq!(result.json::<U128>().unwrap(), U128(0));
    let result = waiting_ft_deposit.await.unwrap();
    assert_success(&result).unwrap();
    user1_ft1_wallet -= ft1_deposit;
    ft1_in_custody += ft1_deposit;
    ft2_in_custody -= (near_deposit.as_yoctonear() + ft1_deposit) * rate;
    let user1_ft2_wallet = (near_deposit.as_yoctonear() + ft1_deposit) * rate;
    assert_ft_balance(&user1, ft1.clone(), U128(user1_ft1_wallet))
        .await
        .unwrap();
    assert_ft_balance(&user1, ft2.clone(), U128(user1_ft2_wallet))
        .await
        .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Dex(dex_id.clone()),
        AssetId::Near,
        Some(U128(near_deposit.as_yoctonear())),
    )
    .await
    .unwrap();
    let assert_custody = |near: u128, ft1_amount: u128, ft2_amount: u128| {
        let dex_engine_contract = &dex_engine_contract;
        let ft1_asset_id = ft1_asset_id.clone();
        let ft2_asset_id = ft2_asset_id.clone();
        async move {
            assert_total_in_custody(dex_engine_contract, AssetId::Near, Some(U128(near)))
                .await
                .unwrap();
            assert_total_in_custody(dex_engine_contract, ft1_asset_id, Some(U128(ft1_amount)))
                .await
                .unwrap();
            assert_total_in_custody(dex_engine_contract, ft2_asset_id, Some(U128(ft2_amount)))
                .await
                .unwrap();
        }
    };
    let near_in_custody = near_deposit.as_yoctonear();
    assert_custody(near_in_custody, ft1_in_custody, ft2_in_custody).await;

    // Deposits are refunded if the operations after the response fail
    // Swaps more than the deposit
    let failing_swap = |asset_in: AssetId, deposit: u128| {
        oracle_rate_swap(asset_in, ft2_asset_id.clone(), deposit + 1)
    };
    let user1_near_before = near_balance_after_refunds(&sandbox, &user1).await;
    let waiting_near_deposit = deposit_near(vec![
        oracle_request(request.clone()),
        failing_swap(AssetId::Near, near_deposit.as_yoctonear()),
        withdraw_swap_output(ft2_asset_id.clone()),
    ])
    .transact_async()
    .await
    .unwrap();
    let waiting_ft_deposit = deposit_ft1(vec![
        oracle_request(request.clone()),
        failing_swap(ft1_asset_id.clone(), ft1_deposit),
        withdraw_swap_output(ft2_asset_id.clone()),
    ])
    .transact_async()
    .await
    .unwrap();
    // Swaps more than the ft1 that the user has on the engine
    let waiting_without_deposit = execute_operations(vec![
        oracle_request(request.clone()),
        failing_swap(ft1_asset_id.clone(), ft1_deposit - swap_amount),
    ])
    .transact_async()
    .await
    .unwrap();
    let waiting_operations = waiting_for_oracle_request(
        &sandbox,
        &[
            &waiting_near_deposit,
            &waiting_ft_deposit,
            &waiting_without_deposit,
        ],
        &request,
    )
    .await;
    let outcome = respond(
        &oracle_key,
        &request,
        rate_response.clone(),
        waiting_operations,
    )
    .await;
    assert_gas_key_call_success(&outcome).unwrap();
    // deposit_near returns the NEAR it refunded, and ft_transfer_call the tokens it used
    for (waiting, returned) in [
        (waiting_near_deposit, U128(near_deposit.as_yoctonear())),
        (waiting_ft_deposit, U128(0)),
    ] {
        let result = waiting.await.unwrap();
        assert!(format!("{result:?}").contains("Not enough input balance in anonymous assets"));
        let refund_gas = result
            .receipt_outcomes()
            .iter()
            .find(|outcome| {
                outcome
                    .logs
                    .iter()
                    .any(|log| log == "Operations after the oracle request failed")
            })
            .expect("No receipt logged that the operations failed")
            .gas_burnt;
        println!("Refunded after failed operations with {refund_gas}");
        assert!(refund_gas < GAS_FOR_AFTER_ORACLE_OPERATIONS_IN_SANDBOX);
        assert_eq!(result.json::<U128>().unwrap(), returned);
    }
    // With nothing to refund, the transaction fails as the operations did
    let result = waiting_without_deposit.await.unwrap();
    assert!(result.is_failure());
    assert!(format!("{result:?}").contains("Insufficient balance"));
    assert_ft_balance(&user1, ft1.clone(), U128(user1_ft1_wallet))
        .await
        .unwrap();
    assert!(
        user1_near_before.saturating_sub(near_balance_after_refunds(&sandbox, &user1).await)
            < max_gas_cost
    );
    assert_custody(near_in_custody, ft1_in_custody, ft2_in_custody).await;

    // Deposits are refunded if the contract is paused while they wait
    let pauser = create_pauser(&sandbox).await;
    let waiting = deposit_ft1(vec![
        oracle_request(request.clone()),
        oracle_rate_swap(ft1_asset_id.clone(), ft2_asset_id.clone(), ft1_deposit),
        withdraw_swap_output(ft2_asset_id.clone()),
    ])
    .transact_async()
    .await
    .unwrap();
    let waiting_operations = waiting_for_oracle_request(&sandbox, &[&waiting], &request).await;
    set_paused(&dex_engine_contract, &pauser, true).await;
    let outcome = respond(
        &oracle_key,
        &request,
        rate_response.clone(),
        waiting_operations,
    )
    .await;
    assert_gas_key_call_success(&outcome).unwrap();
    let result = waiting.await.unwrap();
    assert!(format!("{result:?}").contains("Contract is paused"));
    set_paused(&dex_engine_contract, &pauser, false).await;
    assert_ft_balance(&user1, ft1.clone(), U128(user1_ft1_wallet))
        .await
        .unwrap();
    assert_custody(near_in_custody, ft1_in_custody, ft2_in_custody).await;

    // Deposits are refunded if the oracle doesn't respond in time, and only the
    // oracle that was asked can respond
    let other_oracle_key = create_oracle_key(
        &dex_engine_contract,
        &user4,
        KeyType::ED25519,
        NearToken::from_near(1),
    )
    .await;
    let storage_usage_before = storage_usage().await;
    let user1_near_before = near_balance_after_refunds(&sandbox, &user1).await;
    let waiting_near_deposit = deposit_near(vec![
        oracle_request(request.clone()),
        oracle_rate_swap(
            AssetId::Near,
            ft2_asset_id.clone(),
            near_deposit.as_yoctonear(),
        ),
        withdraw_swap_output(ft2_asset_id.clone()),
    ])
    .transact_async()
    .await
    .unwrap();
    let waiting_ft_deposit = deposit_ft1(vec![
        oracle_request(request.clone()),
        oracle_rate_swap(ft1_asset_id.clone(), ft2_asset_id.clone(), ft1_deposit),
        withdraw_swap_output(ft2_asset_id.clone()),
    ])
    .transact_async()
    .await
    .unwrap();
    let waiting_without_deposit = execute_operations(vec![oracle_request(request.clone())])
        .transact_async()
        .await
        .unwrap();
    let waiting_operations = waiting_for_oracle_request(
        &sandbox,
        &[
            &waiting_near_deposit,
            &waiting_ft_deposit,
            &waiting_without_deposit,
        ],
        &request,
    )
    .await;
    // Responses of another oracle, or to another request, are for nothing
    let outcome = respond(
        &other_oracle_key,
        &request,
        rate_response.clone(),
        waiting_operations.clone(),
    )
    .await;
    assert_gas_key_call_success(&outcome).unwrap();
    let outcome = respond(
        &oracle_key,
        b"another request",
        rate_response.clone(),
        waiting_operations.clone(),
    )
    .await;
    assert_gas_key_call_success(&outcome).unwrap();
    let outcome = respond(
        &oracle_key,
        &request,
        vec![0; MAX_ORACLE_RESPONSE_BYTES + 1],
        waiting_operations.clone(),
    )
    .await;
    assert!(format!("{outcome:?}").contains(&format!(
        "Response is more than {MAX_ORACLE_RESPONSE_BYTES} bytes"
    )));
    sandbox.fast_forward(250).await.unwrap();
    for (waiting, returned) in [
        (waiting_near_deposit, U128(near_deposit.as_yoctonear())),
        (waiting_ft_deposit, U128(0)),
    ] {
        let result = waiting.await.unwrap();
        assert!(format!("{result:?}").contains("Oracle didn't respond in time"));
        assert_eq!(result.json::<U128>().unwrap(), returned);
    }
    let result = waiting_without_deposit.await.unwrap();
    assert!(result.is_failure());
    assert!(format!("{result:?}").contains("Oracle didn't respond in time"));
    // Too late, so it's for nothing
    let outcome = respond(
        &oracle_key,
        &request,
        rate_response.clone(),
        waiting_operations,
    )
    .await;
    assert_gas_key_call_success(&outcome).unwrap();
    assert_ft_balance(&user1, ft1.clone(), U128(user1_ft1_wallet))
        .await
        .unwrap();
    assert!(
        user1_near_before.saturating_sub(near_balance_after_refunds(&sandbox, &user1).await)
            < max_gas_cost
    );
    assert_custody(near_in_custody, ft1_in_custody, ft2_in_custody).await;
    assert_eq!(storage_usage().await, storage_usage_before);
    wait_for_blocks(&sandbox, 3).await;
    assert_untracked_near(&dex_engine_contract).await.unwrap();

    // Requests that aren't allowed
    let result = execute_operations(vec![
        Operation::RegisterAssets {
            asset_ids: vec![AssetId::Near],
            r#for: None,
        },
        oracle_request(request.clone()),
    ])
    .transact()
    .await
    .unwrap();
    assert!(format!("{result:?}").contains("Only the first operation can be an oracle request"));
    let storage_usage_before = storage_usage().await;
    let result = execute_operations(vec![
        oracle_request(request.clone()),
        oracle_request(request.clone()),
    ])
    .transact()
    .await
    .unwrap();
    assert!(format!("{result:?}").contains("Only the first operation can be an oracle request"));
    assert_eq!(storage_usage().await, storage_usage_before);
    let result = execute_operations(vec![oracle_request(vec![0; MAX_ORACLE_REQUEST_BYTES + 1])])
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains(&format!(
        "Request is more than {MAX_ORACLE_REQUEST_BYTES} bytes"
    )));
    // The gas of refunding a held deposit is paid for up front
    let storage_usage_before = storage_usage().await;
    let result = deposit_near(vec![oracle_request(request.clone())])
        .gas(GAS_FOR_AFTER_ORACLE_OPERATIONS_IN_SANDBOX)
        .transact()
        .await
        .unwrap();
    assert!(!result.is_success());
    assert_eq!(storage_usage().await, storage_usage_before);

    // Any number of operations wait for the same request, and the oracle
    // responds to them in as many calls as it needs
    let storage_usage_before = storage_usage().await;
    let wait_for_request =
        || execute_operations(vec![oracle_request(request.clone())]).gas(Gas::from_tgas(30));
    let mut waiting_first = Vec::new();
    let mut waiting_later = Vec::new();
    for _ in 0..10 {
        waiting_first.push(wait_for_request().transact_async().await.unwrap());
        waiting_later.push(wait_for_request().transact_async().await.unwrap());
    }
    let waiting_operations_first = waiting_for_oracle_request(
        &sandbox,
        &waiting_first.iter().collect::<Vec<_>>(),
        &request,
    )
    .await;
    let waiting_operations_later = waiting_for_oracle_request(
        &sandbox,
        &waiting_later.iter().collect::<Vec<_>>(),
        &request,
    )
    .await;
    // The engine stores nothing for operations while they wait
    assert_eq!(storage_usage().await, storage_usage_before);
    // Responding to the same operations again before they run is for nothing
    let outcome = respond(
        &oracle_key,
        &request,
        rate_response.clone(),
        [waiting_operations_first.clone(), waiting_operations_first].concat(),
    )
    .await;
    assert_gas_key_call_success(&outcome).unwrap();
    for waiting in waiting_first {
        let result = wait_for_transaction(waiting).await;
        assert_success(&result).unwrap();
    }
    for waiting in &waiting_later {
        assert!(waiting.status().await.unwrap().is_pending());
    }
    let outcome = respond(
        &oracle_key,
        &request,
        rate_response.clone(),
        waiting_operations_later,
    )
    .await;
    assert_gas_key_call_success(&outcome).unwrap();
    for waiting in waiting_later {
        let result = wait_for_transaction(waiting).await;
        assert_success(&result).unwrap();
    }
    assert_eq!(storage_usage().await, storage_usage_before);

    // The largest request and response
    let largest_request = vec![1; MAX_ORACLE_REQUEST_BYTES];
    let waiting = execute_operations(vec![oracle_request(largest_request.clone())])
        .transact_async()
        .await
        .unwrap();
    let nonces = waiting_for_oracle_request(&sandbox, &[&waiting], &largest_request).await;
    let outcome = respond(
        &oracle_key,
        &largest_request,
        vec![2; MAX_ORACLE_RESPONSE_BYTES],
        nonces,
    )
    .await;
    assert_gas_key_call_success(&outcome).unwrap();
    let result = wait_for_transaction(waiting).await;
    assert_success(&result).unwrap();

    // The same key can't be added again, and the whole deposit is refunded
    let user3_near_before = near_balance_after_refunds(&sandbox, &user3).await;
    let result = user3
        .call(dex_engine_contract.id(), "create_oracle_key")
        .max_gas()
        .deposit(oracle_key_deposit)
        .args_json(json!({
            "oracle_key": oracle_key.public_key(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(!result.is_success());
    assert!(
        user3_near_before.saturating_sub(near_balance_after_refunds(&sandbox, &user3).await)
            < oracle_key_storage_cost
    );

    // Anyone can top up an oracle key, and only oracle keys
    let balance_before = gas_key_balance(&sandbox, &dex_engine_contract, &oracle_key)
        .await
        .unwrap();
    let result = user4
        .call(dex_engine_contract.id(), "top_up_oracle_key")
        .max_gas()
        .deposit(top_up_amount)
        .args_json(json!({
            "oracle_key": oracle_key.public_key(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert_eq!(
        gas_key_balance(&sandbox, &dex_engine_contract, &oracle_key).await,
        Some(balance_before.saturating_add(top_up_amount))
    );
    let user4_near_before = near_balance_after_refunds(&sandbox, &user4).await;
    let result = user4
        .call(dex_engine_contract.id(), "top_up_oracle_key")
        .max_gas()
        .deposit(top_up_amount)
        .args_json(json!({
            "oracle_key": SecretKey::from_random(KeyType::ED25519).public_key(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(!result.is_success());
    assert!(
        user4_near_before.saturating_sub(near_balance_after_refunds(&sandbox, &user4).await)
            < max_gas_cost
    );

    // Deleting a key would burn more than 1 NEAR of its gas balance
    let outcome = call_engine_with_gas_key(
        &sandbox,
        &dex_engine_contract,
        &oracle_key,
        "remove_oracle_key",
        json!({
            "receiver_id": user4.id(),
        }),
    )
    .await;
    assert!(format!("{outcome:?}").contains("Failed to remove the oracle key"));
    assert!(
        gas_key_balance(&sandbox, &dex_engine_contract, &oracle_key)
            .await
            .is_some()
    );

    // An oracle removes its key, and the NEAR that paid for its storage goes back
    let storage_usage_before = storage_usage().await;
    let small_oracle_key = create_oracle_key(
        &dex_engine_contract,
        &user3,
        KeyType::ED25519,
        // Enough to prepay the gas of a transaction, and less than 1 NEAR
        oracle_key_storage_cost.saturating_add(NearToken::from_millinear(500)),
    )
    .await;
    let user4_near_before = near_balance_after_refunds(&sandbox, &user4).await;
    let outcome = call_engine_with_gas_key(
        &sandbox,
        &dex_engine_contract,
        &small_oracle_key,
        "remove_oracle_key",
        json!({
            "receiver_id": user4.id(),
        }),
    )
    .await;
    assert_gas_key_call_success(&outcome).unwrap();
    assert_eq!(
        gas_key_balance(&sandbox, &dex_engine_contract, &small_oracle_key).await,
        None
    );
    assert_eq!(storage_usage().await, storage_usage_before);
    assert_near_balance(
        &user4,
        user4_near_before.saturating_add(oracle_key_storage_cost),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_oracle_key_types() {
    let request = b"request to an oracle".to_vec();
    // Less than the 1 NEAR of gas balance that deleting a key can burn
    let oracle_key_deposit = NearToken::from_near(1);

    let TestContext {
        sandbox,
        dex_engine_contract,
        user1,
        user3,
        user4,
        deployer,
        ..
    } = setup_test_environment().await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };
    let storage_usage = || async {
        dex_engine_contract
            .view_account()
            .await
            .unwrap()
            .storage_usage
    };

    for key_type in [KeyType::ED25519, KeyType::SECP256K1, KeyType::MLDSA65] {
        let storage_usage_before = storage_usage().await;
        let oracle_key =
            create_oracle_key(&dex_engine_contract, &user3, key_type, oracle_key_deposit).await;
        let oracle_key_storage_cost = oracle_key_deposit.saturating_sub(
            gas_key_balance(&sandbox, &dex_engine_contract, &oracle_key)
                .await
                .unwrap(),
        );
        // The deposit pays exactly for the storage that the key takes
        assert_eq!(
            oracle_key_storage_cost,
            near_sdk::env::storage_byte_cost()
                .saturating_mul((storage_usage().await - storage_usage_before) as u128),
            "{key_type}"
        );

        // The key responds as the oracle that the operations wait for
        let waiting = user1
            .call(dex_engine_contract.id(), "execute_operations")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "operations": [Operation::OracleRequest {
                    oracle_key: oracle_key_handle(&oracle_key),
                    dex_id: dex_id.clone(),
                    request: Base64VecU8(request.clone()),
                }],
            }))
            .transact_async()
            .await
            .unwrap();
        let nonces = waiting_for_oracle_request(&sandbox, &[&waiting], &request).await;
        let outcome = call_engine_with_gas_key(
            &sandbox,
            &dex_engine_contract,
            &oracle_key,
            "oracle_respond",
            json!({
                "request_hash": Base58CryptoHash::from(near_sdk::env::sha256_array(&request)),
                "response": Base64VecU8(b"response".to_vec()),
                "nonces": nonces,
            }),
        )
        .await;
        assert_gas_key_call_success(&outcome).unwrap();
        let result = wait_for_transaction(waiting).await;
        assert_success(&result).unwrap();

        // The key removes itself, and the NEAR that paid for its storage goes back
        let user4_near_before = near_balance_after_refunds(&sandbox, &user4).await;
        let outcome = call_engine_with_gas_key(
            &sandbox,
            &dex_engine_contract,
            &oracle_key,
            "remove_oracle_key",
            json!({
                "receiver_id": user4.id(),
            }),
        )
        .await;
        assert_gas_key_call_success(&outcome).unwrap();
        assert_eq!(
            gas_key_balance(&sandbox, &dex_engine_contract, &oracle_key).await,
            None
        );
        assert_eq!(storage_usage().await, storage_usage_before);
        assert_near_balance(
            &user4,
            user4_near_before.saturating_add(oracle_key_storage_cost),
        )
        .await
        .unwrap();
    }
}
