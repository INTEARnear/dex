mod common;
use common::*;

use intear_dex::internal_operations::{SwapOperationAmount, WithdrawAmount};
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
use std::collections::HashMap;

#[tokio::test]
async fn test_minimal() {
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
    let dex_wasm = &wasms.minimal_dex_wasm;

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: dex_id_string.clone(),
    };

    let result = deployer
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_user_storage_deposit())
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
            "message": BASE64_STANDARD.encode(vec![]),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Near,
            "amount": SwapRequestAmount::ExactIn(U128(swap_amount)),
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
        .deposit(engine_user_storage_deposit())
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
            "amount": WithdrawAmount::Exact(U128(ft_withdraw_attempt)),
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
            "amount": WithdrawAmount::Exact(U128(near_withdraw_amount.as_yoctonear())),
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
            "amount": WithdrawAmount::Exact(U128(ft_withdraw_attempt)),
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
            "amount": WithdrawAmount::Exact(U128(near_withdraw_amount.as_yoctonear())),
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
            "amount": WithdrawAmount::Exact(U128(ft_successful_withdraw)),
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
    let dex_wasm = &wasms.minimal_dex_wasm;

    let dex_id_string = "dex".to_string();
    let dex_id = DexId {
        deployer: user1.id().clone(),
        id: dex_id_string.clone(),
    };

    let result = user1
        .call(dex_engine_contract.id(), "dex_storage_deposit")
        .max_gas()
        .deposit(engine_user_storage_deposit())
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

    let operations = vec![
        Operation::RegisterAssets {
            asset_ids: vec![AssetId::Near],
            r#for: Some(AccountOrDexId::Dex(DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            })),
        },
        Operation::DeployDexCode {
            last_part_of_id: dex_id_string.clone(),
            code_base64: Base64VecU8(dex_wasm.to_vec()),
        },
        Operation::TransferAsset {
            to: AccountOrDexId::Dex(DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            }),
            asset_id: AssetId::Near,
            amount: U128(transfer_amount.as_yoctonear()),
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(vec![]),
            asset_in: AssetId::Near,
            asset_out: AssetId::Near,
            amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(
                swap_amount.as_yoctonear(),
            ))),
            constraint: None,
        },
        Operation::Withdraw {
            asset_id: AssetId::Near,
            amount: WithdrawAmount::Exact(U128(withdraw_amount.as_yoctonear())),
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
        amount: WithdrawAmount::Exact(U128(ft_withdraw_attempt)),
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
    let dex_wasm = &wasms.simple_amm_dex_wasm;

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
                U128(pool_creation_fee.as_yoctonear()),
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
                U128(pool_creation_fee.as_yoctonear()),
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
                (AssetId::Near, U128(lp1_near_amount.as_yoctonear())),
                (AssetId::Nep141(ft1.id().clone()), U128(lp1_ft1_amount)),
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
                (AssetId::Nep141(ft1.id().clone()), U128(lp2_ft1_amount)),
                (AssetId::Nep141(ft2.id().clone()), U128(lp2_ft2_amount)),
            ]),
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs { pool_id: 0 }).unwrap()),
            asset_in: AssetId::Near,
            asset_out: AssetId::Nep141(ft1.id().clone()),
            amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(
                swap_amount_in.as_yoctonear(),
            ))),
            constraint: None,
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs { pool_id: 1 }).unwrap()),
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
    let dex_wasm = &wasms.minimal_dex_wasm;

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
        .deposit(engine_user_storage_deposit())
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

    ft_storage_deposit_for(&ft1, &user1, dex_engine_contract.id()).await;

    let operations = vec![
        Operation::TransferAsset {
            to: AccountOrDexId::Dex(DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            }),
            asset_id: AssetId::Nep141(ft1.id().clone()),
            amount: U128(ft_transfer_to_dex),
        },
        Operation::SwapSimple {
            dex_id: DexId {
                deployer: user1.id().clone(),
                id: dex_id_string.clone(),
            },
            message: Base64VecU8(vec![]),
            asset_in: AssetId::Nep141(ft1.id().clone()),
            asset_out: AssetId::Nep141(ft1.id().clone()),
            amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(ft_swap_amount))),
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
    let dex_wasm = &wasms.simple_amm_dex_wasm;

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
    struct AddLiquidityArgs {
        pool_id: PoolId,
    }
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

    #[near(serializers=[borsh])]
    struct SwapArgs {
        pool_id: PoolId,
    }
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
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapArgs {
                pool_id,
            }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactIn(U128(swap_amount_in.as_yoctonear())),
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
    struct RemoveLiquidityArgs {
        pool_id: PoolId,
        assets_to_remove: (U128, U128),
    }
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
                assets_to_remove: (U128(remove_liquidity_near.as_yoctonear()), U128(remove_liquidity_ft)),
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
            "amount": WithdrawAmount::Exact(U128(withdraw_near_amount.as_yoctonear())),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert!(result.json::<bool>().unwrap());
    sandbox.fast_forward(1).await.unwrap();
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
            "amount": WithdrawAmount::Exact(U128(withdraw_ft_amount)),
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
                        add_liquidity_near.as_yoctonear() + swap_amount_in.as_yoctonear()
                            - remove_liquidity_near.as_yoctonear()
                    ),
                },
                AssetWithBalance {
                    asset_id: AssetId::Nep141(ft1.id().clone()),
                    balance: U128(add_liquidity_ft - 90909 - remove_liquidity_ft),
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
    let dex_wasm = &wasms.simple_amm_dex_wasm;

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
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapArgs { pool_id: 0 }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactIn(U128(swap_amount_in.as_yoctonear())),
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
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapArgs { pool_id: 0 }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactIn(U128(swap_amount_in.as_yoctonear())),
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
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapArgs { pool_id: 0 }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactOut(U128(exact_output)),
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
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapArgs { pool_id: 0 }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactOut(U128(exact_output)),
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
    let dex_wasm = &wasms.simple_amm_dex_wasm;

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
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapArgs { pool_id: 0 }).unwrap()),
            "trader": user1.id().clone(),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactIn(U128(swap_amount_in.as_yoctonear())),
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
            "message": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SwapArgs { pool_id: 0 }).unwrap()),
            "asset_in": AssetId::Near,
            "asset_out": AssetId::Nep141(ft1.id().clone()),
            "amount": SwapRequestAmount::ExactIn(U128(swap_amount_in.as_yoctonear())),
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
            "amount": WithdrawAmount::Full { at_least: Some(U128(initial_near_deposit.as_yoctonear())) },
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
            "amount": WithdrawAmount::Full { at_least: Some(U128(NearToken::from_near(4).as_yoctonear())) },
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
        ft1,
        user1,
        user2,
        deployer,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
        ..Default::default()
    })
    .await;
    let dex_wasm = &get_compiled_wasms().await.minimal_dex_wasm;

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

    // Operations in ft_transfer_call are executed on behalf of the sender
    // that the token contract claims, so they can't deploy code even if
    // the claimed sender is trusted
    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(1),
            "msg": near_sdk::serde_json::to_string(&vec![Operation::DeployDexCode {
                last_part_of_id: "dex".to_string(),
                code_base64: Base64VecU8(dex_wasm.to_vec()),
            }])
            .unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("Operation only available in execute_actions"));

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
            code: get_compiled_wasms().await.minimal_dex_wasm.clone(),
            init_method: None,
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
                    amount: U128(dex_storage_deposit_amount.as_yoctonear()),
                    r#for: Some(AccountOrDexId::Dex(dex_id.clone())),
                },
                Operation::StorageDeposit {
                    amount: U128(user_storage_deposit_amount.as_yoctonear()),
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
            "code_base64": BASE64_STANDARD.encode(&wasms.simple_amm_dex_wasm),
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

    sandbox.fast_forward(3).await.unwrap();
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

    sandbox.fast_forward(3).await.unwrap();
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
    sandbox.fast_forward(3).await.unwrap();
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
    sandbox.fast_forward(3).await.unwrap();
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
    sandbox.fast_forward(3).await.unwrap();
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

/// Must match `NEAR_CUSTODY_OVERCOUNT` in the contract
const NEAR_CUSTODY_OVERCOUNT: u128 = 2_455_360_000_000_000_000_000_001;

/// Sums of raw dex engine state before the storage balance sums migration
struct EngineStateSums {
    users_storage_total: u128,
    users_storage_used: u128,
    dexes_storage_total: u128,
    dexes_storage_used: u128,
    near_in_balances: u128,
    dex_balances: Vec<(DexId, AssetId, u128)>,
}

/// Parse a state key without its collection prefix. total_in_custody values
/// are stored under 32-byte hashes that can start with any byte, so these
/// are allowed to fail to parse.
fn parse_state_key<T: near_sdk::borsh::BorshDeserialize>(key: &[u8]) -> Option<T> {
    let result = near_sdk::borsh::from_slice::<T>(&key[1..]);
    if key.len() == 32 {
        result.ok()
    } else {
        Some(result.unwrap())
    }
}

fn sum_engine_state(state: &HashMap<Vec<u8>, Vec<u8>>) -> EngineStateSums {
    let mut sums = EngineStateSums {
        users_storage_total: 0,
        users_storage_used: 0,
        dexes_storage_total: 0,
        dexes_storage_used: 0,
        near_in_balances: 0,
        dex_balances: Vec::new(),
    };
    for (key, value) in state {
        match key[0] {
            0 => {
                if let Some((dex_id, asset_id)) = parse_state_key::<(DexId, AssetId)>(key) {
                    let balance = near_sdk::borsh::from_slice::<u128>(value).unwrap();
                    if asset_id == AssetId::Near {
                        sums.near_in_balances += balance;
                    }
                    sums.dex_balances.push((dex_id, asset_id, balance));
                }
            }
            4 => {
                if let Some((_, asset_id)) = parse_state_key::<(AccountId, AssetId)>(key) {
                    let balance = near_sdk::borsh::from_slice::<u128>(value).unwrap();
                    if asset_id == AssetId::Near {
                        sums.near_in_balances += balance;
                    }
                }
            }
            3 if parse_state_key::<DexId>(key).is_some() => {
                let (total, used) = near_sdk::borsh::from_slice::<(u128, u128)>(value).unwrap();
                sums.dexes_storage_total += total;
                sums.dexes_storage_used += used;
            }
            5 if parse_state_key::<AccountId>(key).is_some() => {
                let (total, used) = near_sdk::borsh::from_slice::<(u128, u128)>(value).unwrap();
                sums.users_storage_total += total;
                sums.users_storage_used += used;
            }
            _ => {}
        }
    }
    sums
}

#[tokio::test]
async fn test_migration_of_mainnet_state() {
    let storage_deposit_amount = NearToken::from_millinear(100);

    let dex_engine_id: AccountId = "dex.intear.near".parse().unwrap();
    let wasms = get_compiled_wasms().await;
    let mainnet = mainnet().await;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let block_height = mainnet.view_block().await.unwrap().height();
    let mainnet_state = mainnet
        .view_state(&dex_engine_id)
        .block_height(block_height)
        .await
        .unwrap();
    let dex_engine_contract = sandbox
        .import_contract(&dex_engine_id, &mainnet)
        .block_height(block_height)
        .transact()
        .await
        .unwrap();
    let mut batches = Vec::new();
    let mut current_batch = Vec::new();
    let mut current_batch_size: usize = 0;
    const MAX_BATCH_SIZE: usize = 50000;
    for (key, value) in mainnet_state.iter() {
        current_batch.push((key.as_slice(), value.as_slice()));
        current_batch_size += 40 + key.len() + value.len();
        if current_batch_size >= MAX_BATCH_SIZE {
            batches.push(current_batch);
            current_batch = Vec::new();
            current_batch_size = 0;
        }
    }
    batches.push(current_batch);
    for batch in batches {
        sandbox
            .patch(&dex_engine_id)
            .states(batch)
            .transact()
            .await
            .unwrap();
    }
    let (user1, _) = create_user(&sandbox, "user1").await;
    let pauser = create_pauser(&sandbox).await;
    let deployer = sandbox.dev_create_account().await.unwrap();
    let sums = sum_engine_state(&mainnet_state);

    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(sums.near_in_balances + NEAR_CUSTODY_OVERCOUNT)),
    )
    .await
    .unwrap();

    set_paused(&dex_engine_contract, &pauser, true).await;

    let result = dex_engine_contract
        .as_account()
        .batch(dex_engine_contract.id())
        .deploy(&wasms.contract_wasm)
        .call(
            near_workspaces::operations::Function::new("migrate")
                .args_json(json!({
                    "trusted_code_deployer": deployer.id(),
                    "dex_storage_balances_total": NearToken::from_yoctonear(sums.dexes_storage_total),
                    "dex_storage_balances_used": NearToken::from_yoctonear(sums.dexes_storage_used),
                    "user_storage_balances_total": NearToken::from_yoctonear(sums.users_storage_total),
                    "user_storage_balances_used": NearToken::from_yoctonear(sums.users_storage_used),
                }))
                .max_gas(),
        )
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    assert_total_in_custody(
        &dex_engine_contract,
        AssetId::Near,
        Some(U128(sums.near_in_balances)),
    )
    .await
    .unwrap();
    for (dex_id, asset_id, balance) in &sums.dex_balances {
        assert_inner_asset_balance(
            &dex_engine_contract,
            AccountOrDexId::Dex(dex_id.clone()),
            asset_id.clone(),
            Some(U128(*balance)),
        )
        .await
        .unwrap();
    }
    let total_storage_balances = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    assert_eq!(
        total_storage_balances.users.total.as_yoctonear(),
        sums.users_storage_total,
    );
    assert_eq!(
        total_storage_balances.users.available.as_yoctonear(),
        sums.users_storage_total - sums.users_storage_used,
    );
    assert_eq!(
        total_storage_balances.dexes.total.as_yoctonear(),
        sums.dexes_storage_total,
    );
    assert_eq!(
        total_storage_balances.dexes.available.as_yoctonear(),
        sums.dexes_storage_total - sums.dexes_storage_used,
    );
    let trusted_code_deployer: AccountId = dex_engine_contract
        .view("get_trusted_code_deployer")
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(&trusted_code_deployer, deployer.id());
    assert_untracked_near(&dex_engine_contract).await.unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near],
        }))
        .transact()
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("Contract is paused"));

    set_paused(&dex_engine_contract, &pauser, false).await;

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
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let user1_storage_balance = get_storage_balance(&dex_engine_contract, user1.id())
        .await
        .unwrap()
        .unwrap();
    let total_storage_balances = get_total_storage_balances(&dex_engine_contract)
        .await
        .unwrap();
    assert_eq!(
        total_storage_balances.users.total.as_yoctonear(),
        sums.users_storage_total + storage_deposit_amount.as_yoctonear(),
    );
    assert_eq!(
        total_storage_balances.users.available.as_yoctonear(),
        sums.users_storage_total - sums.users_storage_used
            + user1_storage_balance.available.as_yoctonear(),
    );
    sandbox.fast_forward(3).await.unwrap();
    assert_untracked_near(&dex_engine_contract).await.unwrap();
}
