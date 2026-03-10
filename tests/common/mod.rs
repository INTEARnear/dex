#![allow(unused)]

use intear_dex::internal_asset_operations::AccountOrDexId;
use intear_dex::internal_operations::Operation;
use intear_dex_types::{AssetId, DexId};
use near_crypto::KeyType;
use near_sdk::base64::{Engine, prelude::BASE64_STANDARD};
use near_sdk::json_types::Base64VecU8;
use near_sdk::serde_json::json;
use near_sdk::{AccountId, NearToken, json_types::U128};
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::{Account, Contract};
use std::collections::HashMap;
use tokio::process::Command;
use tokio::sync::OnceCell;

pub struct CompiledWasms {
    pub contract_wasm: Vec<u8>,
    pub simple_amm_dex_wasm: Vec<u8>,
    pub minimal_dex_wasm: Vec<u8>,
    pub otc_dex_wasm: Vec<u8>,
    pub xyk_dex_wasm: Vec<u8>,
    pub ft_wasm: Vec<u8>,
}

static COMPILED_WASMS: OnceCell<CompiledWasms> = OnceCell::const_new();

pub async fn get_compiled_wasms() -> &'static CompiledWasms {
    COMPILED_WASMS
        .get_or_init(|| async {
            println!("Compiling intear-dex");
            let contract_wasm = near_workspaces::compile_project("./").await.unwrap();

            println!("Compiling simple-amm-dex");
            assert!(
                Command::new("cargo")
                    .args([
                        "build",
                        "--package=simple-amm-dex",
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
                Command::new("wasm-opt")
                    .args([
                        "-O",
                        "./target/wasm32-unknown-unknown/release/simple_amm_dex.wasm",
                        "-o",
                        "./target/wasm32-unknown-unknown/release/simple_amm_dex.wasm"
                    ])
                    .status()
                    .await
                    .unwrap()
                    .success()
            );

            println!("Compiling minimal-dex");
            assert!(
                Command::new("cargo")
                    .args([
                        "build",
                        "--package=minimal-dex",
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
                Command::new("wasm-opt")
                    .args([
                        "-O",
                        "./target/wasm32-unknown-unknown/release/minimal_dex.wasm",
                        "-o",
                        "./target/wasm32-unknown-unknown/release/minimal_dex.wasm"
                    ])
                    .status()
                    .await
                    .unwrap()
                    .success()
            );

            println!("Compiling otc-dex");
            assert!(
                Command::new("cargo")
                    .args([
                        "build",
                        "--package=otc-dex",
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
                Command::new("wasm-opt")
                    .args([
                        "-O",
                        "./target/wasm32-unknown-unknown/release/otc_dex.wasm",
                        "-o",
                        "./target/wasm32-unknown-unknown/release/otc_dex.wasm"
                    ])
                    .status()
                    .await
                    .unwrap()
                    .success()
            );

            println!("Compiling xyk-dex");
            assert!(
                Command::new("cargo")
                    .args([
                        "build",
                        "--package=xyk-dex",
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
                Command::new("wasm-opt")
                    .args([
                        "-O",
                        "./target/wasm32-unknown-unknown/release/xyk_dex.wasm",
                        "-o",
                        "./target/wasm32-unknown-unknown/release/xyk_dex.wasm"
                    ])
                    .status()
                    .await
                    .unwrap()
                    .success()
            );

            println!("Compilation complete");

            let simple_amm_dex_wasm =
                std::fs::read("./target/wasm32-unknown-unknown/release/simple_amm_dex.wasm")
                    .unwrap();
            let minimal_dex_wasm =
                std::fs::read("./target/wasm32-unknown-unknown/release/minimal_dex.wasm").unwrap();
            let otc_dex_wasm =
                std::fs::read("./target/wasm32-unknown-unknown/release/otc_dex.wasm").unwrap();
            let xyk_dex_wasm =
                std::fs::read("./target/wasm32-unknown-unknown/release/xyk_dex.wasm").unwrap();
            let ft_wasm = include_bytes!("../assets/ft.wasm").to_vec();

            CompiledWasms {
                contract_wasm,
                simple_amm_dex_wasm,
                minimal_dex_wasm,
                otc_dex_wasm,
                xyk_dex_wasm,
                ft_wasm,
            }
        })
        .await
}

/// Track tokens burnt from a transaction result and add to total_near_burnt.
pub fn track_tokens_burnt(
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
pub async fn assert_ft_balance(
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
pub async fn assert_near_balance(
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

/// Get the balance of an asset that is custodied by the dex
/// engine contract for a user or a dex.
pub async fn get_inner_asset_balance(
    dex_engine_contract: &Contract,
    of: AccountOrDexId,
    asset: AssetId,
) -> Result<Option<U128>, Box<dyn std::error::Error>> {
    let balance = dex_engine_contract
        .view("asset_balance_of")
        .args_json(json!({
            "of": of,
            "asset_id": asset,
        }))
        .await?
        .json::<Option<U128>>()?;
    Ok(balance)
}

/// Assert the balance of an asset that is custodied by the dex
/// engine contract for a user or a dex.
pub async fn assert_inner_asset_balance(
    dex_engine_contract: &Contract,
    of: AccountOrDexId,
    asset: AssetId,
    amount: Option<U128>,
) -> Result<(), Box<dyn std::error::Error>> {
    let balance = get_inner_asset_balance(dex_engine_contract, of, asset).await?;
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
pub async fn assert_total_in_custody(
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

/// Assert that a result is successful, and print the result if it is not.
pub fn assert_success(result: &ExecutionFinalResult) -> Result<(), String> {
    if !result.is_success() {
        println!("{result:#?}");
        return Err("Not successful".to_string());
    }
    Ok(())
}

/// Create a new user account.
pub async fn create_user(
    sandbox: &near_workspaces::Worker<near_workspaces::network::Sandbox>,
    name: &str,
) -> (near_workspaces::Account, near_crypto::SecretKey) {
    let key = near_crypto::SecretKey::from_random(KeyType::ED25519);
    let account = sandbox
        .create_root_account_subaccount(name.parse().unwrap(), key.to_string().parse().unwrap())
        .await
        .unwrap()
        .result;
    (account, key)
}

/// Storage deposit amount for FT contracts.
fn ft_storage_deposit_amount() -> NearToken {
    "0.00125 NEAR".parse().unwrap()
}

/// Register storage for a user account on an FT contract.
pub async fn ft_storage_deposit(ft: &Contract, account: &Account) {
    account
        .call(ft.id(), "storage_deposit")
        .args_json(json!({}))
        .deposit(ft_storage_deposit_amount())
        .max_gas()
        .transact()
        .await
        .unwrap();
}

/// Register storage for another account on an FT contract (e.g., for dex engine contract).
pub async fn ft_storage_deposit_for(ft: &Contract, account: &Account, for_account: &AccountId) {
    account
        .call(ft.id(), "storage_deposit")
        .args_json(json!({
            "account_id": for_account,
        }))
        .deposit(ft_storage_deposit_amount())
        .max_gas()
        .transact()
        .await
        .unwrap();
}

/// Storage deposit amount for users on engine contract.
pub const fn engine_user_storage_deposit() -> NearToken {
    NearToken::from_near(1)
}

/// Storage deposit amount for DEX deployers on engine contract.
pub const fn engine_dex_storage_deposit() -> NearToken {
    NearToken::from_near(20)
}

pub struct DexSetupConfig {
    pub id: String,
    pub code: Vec<u8>,
    pub init_method: Option<(String, Vec<u8>)>,
}

#[derive(Default)]
pub struct TestSetupConfig {
    pub dex: Option<DexSetupConfig>,
    pub register_assets_for_all: bool,
    pub ft_storage_deposit_for_all: bool,
}

pub struct TestContext {
    pub sandbox: near_workspaces::Worker<near_workspaces::network::Sandbox>,
    pub dex_engine_contract: Contract,
    pub user1: near_workspaces::Account,
    pub user1_key: near_crypto::SecretKey,
    pub user2: near_workspaces::Account,
    pub user2_key: near_crypto::SecretKey,
    pub user3: near_workspaces::Account,
    pub user3_key: near_crypto::SecretKey,
    pub user4: near_workspaces::Account,
    pub user4_key: near_crypto::SecretKey,
    pub user5: near_workspaces::Account,
    pub user5_key: near_crypto::SecretKey,
    pub deployer: near_workspaces::Account,
    pub ft1: Contract,
    pub ft2: Contract,
    pub ft3: Contract,
}

/// Set up the basic test environment
pub async fn setup_test_environment() -> TestContext {
    setup_test_environment_with_config(TestSetupConfig::default()).await
}

/// Set up the test environment with custom configuration.
pub async fn setup_test_environment_with_config(config: TestSetupConfig) -> TestContext {
    let wasms = get_compiled_wasms().await;
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = sandbox.dev_deploy(&wasms.contract_wasm).await.unwrap();

    let (user1, user1_key) = create_user(&sandbox, "user1").await;
    let (user2, user2_key) = create_user(&sandbox, "user2").await;
    let (user3, user3_key) = create_user(&sandbox, "user3").await;
    let (user4, user4_key) = create_user(&sandbox, "user4").await;
    let (user5, user5_key) = create_user(&sandbox, "user5").await;

    let deployer = sandbox.dev_create_account().await.unwrap();

    let ft_total_supply = NearToken::from_near(1_000_000_000_000);

    let ft1 = sandbox
        .create_root_account_subaccount_and_deploy(
            "ft1".parse().unwrap(),
            near_crypto::SecretKey::from_random(KeyType::ED25519)
                .to_string()
                .parse()
                .unwrap(),
            &wasms.ft_wasm,
        )
        .await
        .unwrap()
        .result;
    ft1.call("new_default_meta")
        .args_json(json!({
            "owner_id": deployer.id(),
            "total_supply": U128(ft_total_supply.as_yoctonear()),
        }))
        .max_gas()
        .transact()
        .await
        .unwrap();

    let ft2 = sandbox
        .create_root_account_subaccount_and_deploy(
            "ft2".parse().unwrap(),
            near_crypto::SecretKey::from_random(KeyType::ED25519)
                .to_string()
                .parse()
                .unwrap(),
            &wasms.ft_wasm,
        )
        .await
        .unwrap()
        .result;
    ft2.call("new_default_meta")
        .args_json(json!({
            "owner_id": deployer.id(),
            "total_supply": U128(ft_total_supply.as_yoctonear()),
        }))
        .max_gas()
        .transact()
        .await
        .unwrap();

    let ft3 = sandbox
        .create_root_account_subaccount_and_deploy(
            "ft3".parse().unwrap(),
            near_crypto::SecretKey::from_random(KeyType::ED25519)
                .to_string()
                .parse()
                .unwrap(),
            &wasms.ft_wasm,
        )
        .await
        .unwrap()
        .result;
    ft3.call("new_default_meta")
        .args_json(json!({
            "owner_id": deployer.id(),
            "total_supply": U128(ft_total_supply.as_yoctonear()),
        }))
        .max_gas()
        .transact()
        .await
        .unwrap();

    let all_accounts = [&user1, &user2, &user3, &user4, &user5, &deployer];
    let all_fts = [&ft1, &ft2, &ft3];

    if config.ft_storage_deposit_for_all {
        for ft in &all_fts {
            ft_storage_deposit_for(ft, ft.as_account(), dex_engine_contract.id()).await;
            for account in &all_accounts {
                ft_storage_deposit(ft, account).await;
            }
        }
    }

    if config.register_assets_for_all {
        let asset_ids = vec![
            AssetId::Near,
            AssetId::Nep141(ft1.id().clone()),
            AssetId::Nep141(ft2.id().clone()),
            AssetId::Nep141(ft3.id().clone()),
        ];
        for account in &all_accounts {
            let result = account
                .call(dex_engine_contract.id(), "storage_deposit")
                .max_gas()
                .deposit(engine_user_storage_deposit())
                .args_json(json!({}))
                .transact()
                .await
                .unwrap();
            assert_success(&result).unwrap();

            let result = account
                .call(dex_engine_contract.id(), "register_assets")
                .max_gas()
                .deposit(NearToken::from_yoctonear(1))
                .args_json(json!({
                    "asset_ids": asset_ids,
                    "for": AccountOrDexId::Account(account.id().clone()),
                }))
                .transact()
                .await
                .unwrap();
            assert_success(&result).unwrap();
        }
    }

    if let Some(dex_config) = config.dex {
        let dex_id = DexId {
            deployer: deployer.id().clone(),
            id: dex_config.id.clone(),
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
                "last_part_of_id": dex_config.id,
                "code_base64": BASE64_STANDARD.encode(&dex_config.code),
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();

        if config.register_assets_for_all {
            let asset_ids = vec![
                AssetId::Near,
                AssetId::Nep141(ft1.id().clone()),
                AssetId::Nep141(ft2.id().clone()),
                AssetId::Nep141(ft3.id().clone()),
            ];
            let result = deployer
                .call(dex_engine_contract.id(), "register_assets")
                .max_gas()
                .deposit(NearToken::from_yoctonear(1))
                .args_json(json!({
                    "asset_ids": asset_ids,
                    "for": AccountOrDexId::Dex(dex_id.clone()),
                }))
                .transact()
                .await
                .unwrap();
            assert_success(&result).unwrap();
        }

        if let Some((method, args)) = dex_config.init_method {
            let result = deployer
                .call(dex_engine_contract.id(), "dex_call")
                .max_gas()
                .deposit(NearToken::from_yoctonear(1))
                .args_json(json!({
                    "dex_id": dex_id.clone(),
                    "method": method,
                    "args": BASE64_STANDARD.encode(&args),
                    "attached_assets": {},
                }))
                .transact()
                .await
                .unwrap();
            assert_success(&result).unwrap();
        }
    }

    TestContext {
        sandbox,
        dex_engine_contract,
        user1,
        user1_key,
        user2,
        user2_key,
        user3,
        user3_key,
        user4,
        user4_key,
        user5,
        user5_key,
        deployer,
        ft1,
        ft2,
        ft3,
    }
}
