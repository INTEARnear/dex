use intear_dex::internal_asset_operations::AccountOrDexId;
use intear_dex_types::{AssetId, DexId, SwapRequestAmount};
use near_contract_standards::storage_management::StorageBalance;
use near_sdk::{
    NearToken,
    base64::{Engine, prelude::BASE64_STANDARD},
    json_types::{Base64VecU8, U128},
    near,
};
use near_workspaces::{Account, Contract};
use serde_json::json;
use tokio::process::Command;

#[tokio::test]
async fn test_contract_is_operational() -> Result<(), Box<dyn std::error::Error>> {
    let contract_wasm = near_workspaces::compile_project("./").await?;

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
    assert!(
        Command::new("cargo")
            .args([
                "+nightly",
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

    let example_dex_wasm =
        std::fs::read("./target/wasm32-unknown-unknown/release/example_dex.wasm").unwrap();
    let minimal_dex_wasm =
        std::fs::read("./target/wasm32-unknown-unknown/release/smallest_possible_dex.wasm")
            .unwrap();
    let ft_wasm = include_bytes!("./ft.wasm");

    test_minimal_on(&contract_wasm, &minimal_dex_wasm).await;
    test_example_on(&contract_wasm, &example_dex_wasm, ft_wasm).await;
    Ok(())
}

async fn test_minimal_on(contract_wasm: &[u8], dex_wasm: &[u8]) {
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(contract_wasm).await.unwrap();
    let dex_deployer_account = sandbox.dev_create_account().await.unwrap();

    let dex_id_string = "example".to_string();
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
        .call(dex_engine_contract.id(), "deploy_code")
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
        .call(dex_engine_contract.id(), "near_deposit")
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
        .call(dex_engine_contract.id(), "transfer_personal_assets")
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
    let outcome = dex_deployer_account
        .call(dex_engine_contract.id(), "swap_one_dex")
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
    assert!(outcome.is_success());
    track_tokens_burnt(&outcome, &mut total_near_burnt);
    let result: (U128, U128) = outcome.json().unwrap();
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

async fn test_example_on(contract_wasm: &[u8], dex_wasm: &[u8], ft_wasm: &[u8]) {
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

    let dex_id_string = "example".to_string();
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
        .call(dex_engine_contract.id(), "deploy_code")
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
        .call(dex_engine_contract.id(), "near_deposit")
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
    assert_eq!(
        storage_usage_by_dex_before_pool_creation.available,
        NearToken::from_yoctonear(3744720000000000000000000),
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
    println!("result: {:#?}", result);
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
        NearToken::from_yoctonear(5002440000000000000000000),
    );
    assert_eq!(
        storage_usage_by_dex_after_pool_creation.available,
        NearToken::from_yoctonear(3744720000000000000000000),
    );

    let pool_storage_cost = storage_usage_by_dex_after_pool_creation
        .total
        .checked_sub(storage_usage_by_dex_before_pool_creation.total)
        .unwrap();
    assert_eq!(
        storage_usage_by_dex_before_pool_creation.available,
        storage_usage_by_dex_after_pool_creation.available,
    );
    dbg!(storage_usage_by_engine_before_pool_creation);
    dbg!(storage_usage_by_engine_after_pool_creation);
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
    let outcome = dex_deployer_account
        .call(dex_engine_contract.id(), "swap_one_dex")
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
    assert!(outcome.is_success());
    track_tokens_burnt(&outcome, &mut total_near_burnt);
    let result: (U128, U128) = outcome.json().unwrap();
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
}
