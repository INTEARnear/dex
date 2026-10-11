#![allow(unused)]

use intear_dex::storage_management::TotalStorageBalances;
use intear_dex_types::{AccountOrDexId, AssetId, DexId, Operation};
use near_contract_standards::storage_management::StorageBalance;
use near_crypto::{KeyType, SecretKey};
use near_sdk::base64::{Engine, prelude::BASE64_STANDARD};
use near_sdk::json_types::Base64VecU8;
use near_sdk::serde_json::json;
use near_sdk::{AccountId, NearToken, json_types::U128};
use near_workspaces::network::{Mainnet, Sandbox};
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::{Account, AccountDetailsPatch, Contract, Worker};
use std::collections::HashMap;
use std::time::Duration;

pub use intear_dex_test_support::get_compiled_wasms;

/// Track tokens burnt from a transaction result and add to total_near_burnt.
pub fn track_tokens_burnt(result: &ExecutionFinalResult, total_near_burnt: &mut NearToken) {
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

/// Returns the balance of NEAR of an account after refunds of unused gas
/// for previous transactions are received.
pub async fn near_balance_after_refunds(sandbox: &Worker<Sandbox>, account: &Account) -> NearToken {
    sandbox.fast_forward(3).await.unwrap();
    account.view_account().await.unwrap().balance
}

/// Assert the balance of NEAR of an account.
pub async fn assert_near_balance(
    account: &Account,
    amount: NearToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut balance = account.view_account().await?.balance;
    for _ in 0..50 {
        if balance == amount {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        balance = account.view_account().await?.balance;
    }
    Err(format!(
        "NEAR balance mismatch: expected {}, actual {}",
        amount.as_yoctonear(),
        balance.as_yoctonear()
    )
    .into())
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

pub async fn get_registered_assets(
    dex_engine_contract: &Contract,
    of: &AccountOrDexId,
    from_index: u32,
    limit: u32,
) -> Result<Vec<(AssetId, U128)>, Box<dyn std::error::Error>> {
    let registered_assets = dex_engine_contract
        .view("registered_assets_of")
        .args_json(json!({
            "account_id": if let AccountOrDexId::Account(account_id) = of {
                Some(account_id)
            } else {
                None
            },
            "dex_id": if let AccountOrDexId::Dex(dex_id) = of {
                Some(dex_id)
            } else {
                None
            },
            "from_index": from_index,
            "limit": limit,
        }))
        .await?
        .json::<Vec<(AssetId, U128)>>()?;
    Ok(registered_assets)
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
pub async fn create_user(sandbox: &Worker<Sandbox>, name: &str) -> (Account, SecretKey) {
    let key = SecretKey::from_random(KeyType::ED25519);
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
    pub sandbox: Worker<Sandbox>,
    pub dex_engine_contract: Contract,
    pub user1: Account,
    pub user1_key: SecretKey,
    pub user2: Account,
    pub user2_key: SecretKey,
    pub user3: Account,
    pub user3_key: SecretKey,
    pub user4: Account,
    pub user4_key: SecretKey,
    pub user5: Account,
    pub user5_key: SecretKey,
    pub deployer: Account,
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
    let deployer = sandbox.dev_create_account().await.unwrap();

    let result = dex_engine_contract
        .call("new")
        .args_json(json!({
            "trusted_code_deployer": deployer.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    setup_test_environment_around_engine(sandbox, dex_engine_contract, deployer, config).await
}

/// Set up the test environment around dex.intear.near with its mainnet state.
pub async fn setup_test_environment_with_mainnet_engine(config: TestSetupConfig) -> TestContext {
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let dex_engine_contract = import_mainnet_engine(&sandbox).await;

    let deployer_id: AccountId = xyk_dex_types::CAN_MIGRATE.to_owned();
    let deployer_key =
        near_workspaces::types::SecretKey::from_random(near_workspaces::types::KeyType::ED25519);
    sandbox
        .patch(&deployer_id)
        .account(AccountDetailsPatch::default().balance(NearToken::from_near(1_000)))
        .access_key(
            deployer_key.public_key(),
            near_workspaces::types::AccessKey::full_access(),
        )
        .transact()
        .await
        .unwrap();
    let deployer = Account::from_secret_key(deployer_id, deployer_key, &sandbox);

    setup_test_environment_around_engine(sandbox, dex_engine_contract, deployer, config).await
}

async fn setup_test_environment_around_engine(
    sandbox: Worker<Sandbox>,
    dex_engine_contract: Contract,
    deployer: Account,
    config: TestSetupConfig,
) -> TestContext {
    let wasms = get_compiled_wasms().await;

    let (user1, user1_key) = create_user(&sandbox, "user1").await;
    let (user2, user2_key) = create_user(&sandbox, "user2").await;
    let (user3, user3_key) = create_user(&sandbox, "user3").await;
    let (user4, user4_key) = create_user(&sandbox, "user4").await;
    let (user5, user5_key) = create_user(&sandbox, "user5").await;

    let ft_total_supply = NearToken::from_near(1_000_000_000_000);

    let ft1 = sandbox
        .create_root_account_subaccount_and_deploy(
            "ft1".parse().unwrap(),
            SecretKey::from_random(KeyType::ED25519)
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
            SecretKey::from_random(KeyType::ED25519)
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
            SecretKey::from_random(KeyType::ED25519)
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

/// Allow an account to deploy dex code
pub async fn set_trusted_code_deployer(
    dex_engine_contract: &Contract,
    current: &Account,
    new: &Account,
) {
    let result = current
        .call(dex_engine_contract.id(), "set_trusted_code_deployer")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "account_id": new.id(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
}

pub async fn mainnet() -> Worker<Mainnet> {
    near_workspaces::mainnet()
        .rpc_addr(&std::env::var("RPC_URL").unwrap_or_else(|_| "https://rpc.intea.rs".to_string()))
        .await
        .unwrap()
}

/// Import dex.intear.near into the sandbox with its code and all of its
/// state on mainnet, as of the latest block
pub async fn import_mainnet_engine(sandbox: &Worker<Sandbox>) -> Contract {
    let dex_engine_id: AccountId = "dex.intear.near".parse().unwrap();
    let mainnet = mainnet().await;
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
    dex_engine_contract
}

/// Create `pause.slimedragon.near`, which is allowed to pause the dex engine.
pub async fn create_pauser(sandbox: &Worker<Sandbox>) -> Account {
    let account_id: AccountId = "pause.slimedragon.near".parse().unwrap();
    let key =
        near_workspaces::types::SecretKey::from_random(near_workspaces::types::KeyType::ED25519);
    sandbox
        .patch(&account_id)
        .account(AccountDetailsPatch::default().balance(NearToken::from_near(100)))
        .access_key(
            key.public_key(),
            near_workspaces::types::AccessKey::full_access(),
        )
        .transact()
        .await
        .unwrap();
    Account::from_secret_key(account_id, key, sandbox)
}

/// Pause or unpause the dex engine
pub async fn set_paused(dex_engine_contract: &Contract, pauser: &Account, paused: bool) {
    let result = pauser
        .call(
            dex_engine_contract.id(),
            if paused { "pause" } else { "unpause" },
        )
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
}

pub async fn is_paused(dex_engine_contract: &Contract) -> Result<bool, Box<dyn std::error::Error>> {
    let paused = dex_engine_contract
        .view("is_paused")
        .args_json(json!({}))
        .await?
        .json::<bool>()?;
    Ok(paused)
}

/// Get the sum of storage balances of all users and all dexes.
pub async fn get_total_storage_balances(
    dex_engine_contract: &Contract,
) -> Result<TotalStorageBalances, Box<dyn std::error::Error>> {
    let total_storage_balances = dex_engine_contract
        .view("total_storage_balances")
        .args_json(json!({}))
        .await?
        .json::<TotalStorageBalances>()?;
    Ok(total_storage_balances)
}

/// Get the storage balance of a user.
pub async fn get_storage_balance(
    dex_engine_contract: &Contract,
    account_id: &AccountId,
) -> Result<Option<StorageBalance>, Box<dyn std::error::Error>> {
    let storage_balance = dex_engine_contract
        .view("storage_balance_of")
        .args_json(json!({
            "account_id": account_id,
        }))
        .await?
        .json::<Option<StorageBalance>>()?;
    Ok(storage_balance)
}

/// Get the storage balance of a dex.
pub async fn get_dex_storage_balance(
    dex_engine_contract: &Contract,
    dex_id: &DexId,
) -> Result<Option<StorageBalance>, Box<dyn std::error::Error>> {
    let storage_balance = dex_engine_contract
        .view("dex_storage_balance_of")
        .args_json(json!({
            "dex_id": dex_id,
        }))
        .await?
        .json::<Option<StorageBalance>>()?;
    Ok(storage_balance)
}

/// Assert that the sum of storage balances of all users and all dexes
/// equals the sum of storage balances of the given accounts and dexes,
/// which must be all accounts and dexes that have a storage balance.
pub async fn assert_total_storage_balances(
    dex_engine_contract: &Contract,
    accounts: &[&AccountId],
    dexes: &[&DexId],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut users_total = NearToken::from_yoctonear(0);
    let mut users_available = NearToken::from_yoctonear(0);
    for account_id in accounts {
        if let Some(storage_balance) = get_storage_balance(dex_engine_contract, account_id).await? {
            users_total = users_total.saturating_add(storage_balance.total);
            users_available = users_available.saturating_add(storage_balance.available);
        }
    }
    let mut dexes_total = NearToken::from_yoctonear(0);
    let mut dexes_available = NearToken::from_yoctonear(0);
    for dex_id in dexes {
        if let Some(storage_balance) = get_dex_storage_balance(dex_engine_contract, dex_id).await? {
            dexes_total = dexes_total.saturating_add(storage_balance.total);
            dexes_available = dexes_available.saturating_add(storage_balance.available);
        }
    }
    let total_storage_balances = get_total_storage_balances(dex_engine_contract).await?;
    if total_storage_balances.users.total != users_total
        || total_storage_balances.users.available != users_available
        || total_storage_balances.dexes.total != dexes_total
        || total_storage_balances.dexes.available != dexes_available
    {
        return Err(format!(
            "Total storage balances mismatch: expected users {}/{}, dexes {}/{}, actual users {}/{}, dexes {}/{}",
            users_total.as_yoctonear(),
            users_available.as_yoctonear(),
            dexes_total.as_yoctonear(),
            dexes_available.as_yoctonear(),
            total_storage_balances.users.total.as_yoctonear(),
            total_storage_balances.users.available.as_yoctonear(),
            total_storage_balances.dexes.total.as_yoctonear(),
            total_storage_balances.dexes.available.as_yoctonear(),
        )
        .into());
    }
    Ok(())
}

/// Assert that `untracked_near()` equals the NEAR balance of the dex engine
/// minus NEAR in custody, storage balances, and storage that storage
/// balances don't pay for. Returns the untracked NEAR.
pub async fn assert_untracked_near(
    dex_engine_contract: &Contract,
) -> Result<NearToken, Box<dyn std::error::Error>> {
    let account = dex_engine_contract.view_account().await?;
    let total_storage_balances = get_total_storage_balances(dex_engine_contract).await?;
    let near_in_custody = dex_engine_contract
        .view("total_in_custody")
        .args_json(json!({
            "asset_id": AssetId::Near,
        }))
        .await?
        .json::<Option<U128>>()?
        .map_or(0, |amount| amount.0);
    let storage_balances_total = total_storage_balances
        .users
        .total
        .saturating_add(total_storage_balances.dexes.total);
    let storage_paid_by_storage_balances = storage_balances_total
        .saturating_sub(total_storage_balances.users.available)
        .saturating_sub(total_storage_balances.dexes.available);
    let storage_locked = near_sdk::env::storage_byte_cost()
        .checked_mul(account.storage_usage as u128)
        .unwrap();
    let expected = account
        .balance
        .checked_sub(NearToken::from_yoctonear(near_in_custody))
        .and_then(|b| b.checked_sub(storage_balances_total))
        .and_then(|b| {
            b.checked_sub(storage_locked.saturating_sub(storage_paid_by_storage_balances))
        })
        .ok_or("NEAR deficit")?;
    let untracked_near = dex_engine_contract
        .view("untracked_near")
        .args_json(json!({}))
        .await?
        .json::<NearToken>()?;
    if untracked_near != expected {
        return Err(format!(
            "Untracked NEAR mismatch: expected {}, actual {}",
            expected.as_yoctonear(),
            untracked_near.as_yoctonear()
        )
        .into());
    }
    Ok(untracked_near)
}
