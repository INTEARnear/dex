use base64::{Engine, prelude::BASE64_STANDARD};
use borsh::{BorshDeserialize, BorshSchema, BorshSerialize};
use clap::{Parser, Subcommand};
use near_api::{
    Contract, NearGas, NearToken, NetworkConfig, RPCEndpoint, Signer, Transaction,
    types::{AccountId, Action, PublicKey, json::U128, transaction::actions::FunctionCallAction},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeMap};
use serde_json::json;
use std::{
    collections::HashMap, fmt::Display, num::NonZeroU128, process::Stdio, str::FromStr, sync::Arc,
};
use tokio::process::Command;

#[derive(Parser)]
#[command(name = "manage")]
#[command(about = "Management CLI for intear-dex", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Otc {
        #[command(subcommand)]
        action: OtcAction,
    },
    Xyk {
        #[command(subcommand)]
        action: XykAction,
    },
    RegisterAssets {
        account_id: AccountId,
        asset_ids: Vec<AssetId>,
        /// Register assets for a specific account or dex. Format: "account:alice.near" or "dex:deployer.near/xyk"
        #[arg(long)]
        r#for: Option<AccountOrDexId>,
    },
    DepositAsset {
        account_id: AccountId,
        asset_id: AssetId,
        amount: u128,
    },
    GenerateSchema,
}

#[derive(Clone, Debug)]
enum AccountOrDexId {
    Account(AccountId),
    Dex(String),
}

impl FromStr for AccountOrDexId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(account) = s.strip_prefix("account:") {
            Ok(Self::Account(
                account
                    .parse()
                    .map_err(|e| format!("Invalid account id: {e}"))?,
            ))
        } else if let Some(dex) = s.strip_prefix("dex:") {
            Ok(Self::Dex(dex.to_string()))
        } else {
            Err(
                "Invalid format. Use 'account:<account_id>' or 'dex:<deployer>/<dex_name>'"
                    .to_string(),
            )
        }
    }
}

impl Serialize for AccountOrDexId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Account(account_id) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Account", account_id)?;
                map.end()
            }
            Self::Dex(dex_id) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Dex", dex_id)?;
                map.end()
            }
        }
    }
}

#[derive(Subcommand)]
enum OtcAction {
    Deploy {
        /// Include storage deposit (5 NEAR) in the deployment transaction
        #[arg(long)]
        storage: bool,
    },
    Initialize,
    SetAuthorizedKey {
        account_id: AccountId,
        key: PublicKey,
    },
    StorageDeposit {
        account_id: AccountId,
        amount: NearToken,
    },
    DepositAssets {
        account_id: AccountId,
        asset_id: AssetId,
        amount: u128,
    },
}

#[derive(Subcommand)]
enum XykAction {
    Deploy {
        /// Include storage deposit (5 NEAR) in the deployment transaction
        #[arg(long)]
        storage: bool,
    },
    Initialize,
    CreatePool {
        #[command(subcommand)]
        action: XykCreatePoolAction,
    },
    GetPool {
        pool_id: XykPoolId,
    },
    UpgradePool {
        account_id: AccountId,
        pool_id: XykPoolId,
    },
    AddLiquidity {
        account_id: AccountId,
        pool_id: XykPoolId,
        amount_0: u128,
        amount_1: u128,
    },
    RemoveLiquidity {
        account_id: AccountId,
        pool_id: XykPoolId,
        /// Shares to remove (for public pools). If not provided, removes all liquidity.
        #[arg(long)]
        shares: Option<u128>,
    },
    EditFees {
        account_id: AccountId,
        pool_id: XykPoolId,
        /// Fee receivers in format "account_id:fee_fraction" (fee_fraction is 10000 = 1%)
        #[arg(long, value_delimiter = ',')]
        fees: Vec<FeeReceiverArg>,
    },
    GetPendingFees {
        account_id: AccountId,
        #[arg(value_delimiter = ',')]
        asset_ids: Vec<AssetId>,
    },
    WithdrawFees {
        account_id: AccountId,
        #[arg(value_delimiter = ',')]
        asset_ids: Vec<AssetId>,
    },
    SimulateTrade {
        #[arg(value_enum)]
        direction: TradeDirection,
        amount: u128,
        pool_id: XykPoolId,
        asset_in: AssetId,
    },
    Trade {
        account_id: AccountId,
        #[arg(value_enum)]
        direction: TradeDirection,
        amount: u128,
        pool_id: XykPoolId,
        asset_in: AssetId,
        /// Slippage tolerance (e.g. "1%" or "0.5%"). Sets min amount out for exact-in, max amount in for exact-out.
        #[arg(long)]
        slippage: Option<SlippagePercent>,
        /// Withdraw the output asset after the swap
        #[arg(long)]
        withdraw: bool,
        /// Deposit the input asset before the swap (implies --withdraw)
        #[arg(long)]
        deposit: bool,
    },
}

#[derive(Clone, Copy, Debug)]
struct SlippagePercent(f64);

impl FromStr for SlippagePercent {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if !s.ends_with('%') {
            return Err(format!("Slippage must end with '%', got: {s}"));
        }
        let num_str = &s[..s.len() - 1];
        let percent: f64 = num_str
            .parse()
            .map_err(|e| format!("Invalid slippage percentage: {e}"))?;
        if !(0.0..=100.0).contains(&percent) {
            return Err(format!(
                "Slippage must be between 0% and 100%, got: {percent}%"
            ));
        }
        Ok(Self(percent))
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum TradeDirection {
    ExactIn,
    ExactOut,
}

#[derive(Subcommand)]
enum XykCreatePoolAction {
    Private {
        account_id: AccountId,
        asset_0: AssetId,
        asset_1: AssetId,
        /// Fee receivers in format "account_id:fee_fraction" (fee_fraction is 10000 = 1%)
        #[arg(long, value_delimiter = ',')]
        fees: Vec<FeeReceiverArg>,
    },
    Public {
        account_id: AccountId,
        asset_0: AssetId,
        asset_1: AssetId,
        /// Fee receivers in format "account_id:fee_fraction" (fee_fraction is 10000 = 1%)
        #[arg(long, value_delimiter = ',')]
        fees: Vec<FeeReceiverArg>,
    },
    Launch {
        account_id: AccountId,
        asset_0: AssetId,
        asset_1: AssetId,
        /// Synthetic NEAR depth used in launch pricing.
        #[arg(long)]
        phantom_liquidity_near: u128,
        /// Amount of asset_1 to seed the pool with.
        #[arg(long)]
        launched_asset_amount: u128,
        /// Fee receivers in format "account_id:fee_fraction" (fee_fraction is 10000 = 1%)
        #[arg(long, value_delimiter = ',')]
        fees: Vec<FeeReceiverArg>,
    },
}

#[derive(Clone, Debug)]
struct FeeReceiverArg {
    account_id: AccountId,
    fee_fraction: u32,
}

impl FromStr for FeeReceiverArg {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (account_id, fee_fraction) = s
            .split_once(':')
            .ok_or_else(|| format!("Invalid fee receiver format: {s}"))?;
        Ok(Self {
            account_id: account_id
                .parse()
                .map_err(|e| format!("Invalid account id: {e}"))?,
            fee_fraction: fee_fraction
                .parse()
                .map_err(|e| format!("Invalid fee fraction: {e}"))?,
        })
    }
}

struct Config {
    deployer_id: AccountId,
    signer: Arc<Signer>,
    dex_contract_id: AccountId,
}

async fn load_config() -> Result<Config, Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let deployer_id =
        std::env::var("DEPLOYER_ID").map_err(|_| "DEPLOYER_ID environment variable not set")?;
    let deployer_id =
        AccountId::from_str(&deployer_id).map_err(|e| format!("Invalid DEPLOYER_ID: {}", e))?;
    let signer =
        Signer::from_keystore_with_search_for_keys(deployer_id.clone(), &network()).await?;
    let dex_contract_id = std::env::var("DEX_CONTRACT_ID").unwrap_or("dex.intear.near".to_string());
    let dex_contract_id = AccountId::from_str(&dex_contract_id)
        .map_err(|e| format!("Invalid DEX_CONTRACT_ID: {}", e))?;

    Ok(Config {
        deployer_id,
        signer,
        dex_contract_id,
    })
}

fn network() -> NetworkConfig {
    NetworkConfig {
        rpc_endpoints: vec![RPCEndpoint::new("https://rpc.intea.rs".parse().unwrap())],
        ..NetworkConfig::mainnet()
    }
}

async fn execute_operations_with_deposit(
    dex_contract_id: AccountId,
    operations: Vec<Operation>,
    deposit: Option<(AssetId, u128)>,
    account_id: AccountId,
    account_signer: Arc<Signer>,
) -> Result<(), Box<dyn std::error::Error>> {
    match deposit {
        Some((asset_id, amount)) => match asset_id {
            AssetId::Near => {
                let result = Contract(dex_contract_id)
                    .call_function(
                        "deposit_near",
                        if operations.is_empty() {
                            json!({})
                        } else {
                            json!({
                                "operations": operations,
                            })
                        },
                    )
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(amount))
                    .with_signer(account_id, account_signer)
                    .send_to(&network())
                    .await?;
                println!(
                    "Deposit + operations executed. Result: {:?}",
                    result.outcome()
                );
            }
            AssetId::Nep141(token_contract_id) => {
                let _ = Contract(token_contract_id.clone())
                    .call_function(
                        "storage_deposit",
                        json!({
                            "account_id": dex_contract_id,
                        }),
                    )
                    .transaction()
                    .gas(NearGas::from_tgas(10))
                    .deposit("0.01 NEAR".parse::<NearToken>().unwrap())
                    .with_signer(account_id.clone(), Arc::clone(&account_signer))
                    .send_to(&network())
                    .await;
                let result = Contract(token_contract_id)
                    .call_function(
                        "ft_transfer_call",
                        json!({
                            "receiver_id": dex_contract_id,
                            "amount": U128(amount),
                            "msg": if operations.is_empty() {
                                "".to_string()
                            } else {
                                serde_json::to_string(&operations).unwrap()
                            },
                        }),
                    )
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(account_id, account_signer)
                    .send_to(&network())
                    .await?;
                println!(
                    "Deposit + operations executed. Result: {:?}",
                    result.outcome()
                );
            }
            AssetId::Nep171(nft_contract_id, token_id) => {
                let _ = Contract(nft_contract_id.clone())
                    .call_function(
                        "storage_deposit",
                        json!({
                            "account_id": dex_contract_id,
                        }),
                    )
                    .transaction()
                    .gas(NearGas::from_tgas(10))
                    .deposit("0.01 NEAR".parse::<NearToken>().unwrap())
                    .with_signer(account_id.clone(), Arc::clone(&account_signer))
                    .send_to(&network())
                    .await;
                let result = Contract(nft_contract_id)
                    .call_function(
                        "nft_transfer_call",
                        json!({
                            "receiver_id": dex_contract_id,
                            "token_id": token_id,
                            "msg": if operations.is_empty() {
                                "".to_string()
                            } else {
                                serde_json::to_string(&operations).unwrap()
                            },
                        }),
                    )
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(account_id, account_signer)
                    .send_to(&network())
                    .await?;
                println!(
                    "Deposit + operations executed. Result: {:?}",
                    result.outcome()
                );
            }
            AssetId::Nep245(mt_contract_id, token_id) => {
                let _ = Contract(mt_contract_id.clone())
                    .call_function(
                        "storage_deposit",
                        json!({
                            "account_id": dex_contract_id,
                        }),
                    )
                    .transaction()
                    .gas(NearGas::from_tgas(10))
                    .deposit("0.01 NEAR".parse::<NearToken>().unwrap())
                    .with_signer(account_id.clone(), Arc::clone(&account_signer))
                    .send_to(&network())
                    .await;
                let result = Contract(mt_contract_id)
                    .call_function(
                        "mt_transfer_call",
                        json!({
                            "receiver_id": dex_contract_id,
                            "token_id": token_id,
                            "amount": U128(amount),
                            "msg": if operations.is_empty() {
                                "".to_string()
                            } else {
                                serde_json::to_string(&operations).unwrap() },
                        }),
                    )
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(account_id, account_signer)
                    .send_to(&network())
                    .await?;
                println!(
                    "Deposit + operations executed. Result: {:?}",
                    result.outcome()
                );
            }
        },
        None => {
            let result = Contract(dex_contract_id)
                .call_function(
                    "execute_operations",
                    json!({
                        "operations": operations,
                        "referrer": null,
                    }),
                )
                .transaction()
                .max_gas()
                .deposit(NearToken::from_yoctonear(1))
                .with_signer(account_id, account_signer)
                .send_to(&network())
                .await?;
            println!("Operations executed. Result: {:?}", result.outcome());
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let config = load_config().await?;

    println!("Loaded config: deployer_id = {}", config.deployer_id);

    match cli.command {
        Commands::GenerateSchema => {
            let mut schemas = HashMap::new();
            let schema = borsh::schema_container_of::<XykCreatePoolArgs>();
            schemas.insert("XykCreatePoolArgs", schema);
            let schema = borsh::schema_container_of::<XykEditFeesArgs>();
            schemas.insert("XykEditFeesArgs", schema);
            let schema = borsh::schema_container_of::<XykGetPendingFeesArgs>();
            schemas.insert("XykGetPendingFeesArgs", schema);
            let schema = borsh::schema_container_of::<XykWithdrawFeesArgs>();
            schemas.insert("XykWithdrawFeesArgs", schema);
            let schema = borsh::schema_container_of::<XykSwapArgs>();
            schemas.insert("XykSwapArgs", schema);
            let schema = borsh::schema_container_of::<XykRegisterLiquidityArgs>();
            schemas.insert("XykRegisterLiquidityArgs", schema);
            let schema = borsh::schema_container_of::<XykAddLiquidityArgs>();
            schemas.insert("XykAddLiquidityArgs", schema);
            let schema = borsh::schema_container_of::<XykRemoveLiquidityArgs>();
            schemas.insert("XykRemoveLiquidityArgs", schema);
            let schema = borsh::schema_container_of::<XykUpgradePoolArgs>();
            schemas.insert("XykUpgradePoolArgs", schema);
            let schema = borsh::schema_container_of::<XykPoolNeedsUpgradeArgs>();
            schemas.insert("XykPoolNeedsUpgradeArgs", schema);
            let schema = borsh::schema_container_of::<XykUpgradePoolArgs>();
            schemas.insert("XykUpgradePoolArgs", schema);

            tokio::fs::create_dir_all("./schemas").await?;
            for (name, schema) in schemas {
                let schema_serialized = borsh::to_vec(&schema).unwrap();
                tokio::fs::write(format!("./schemas/{name}.borsh"), schema_serialized).await?;
                Command::new("zorsh-schema-gen")
                    .args([
                        "generate",
                        &format!("schemas/{name}.borsh"),
                        "-o",
                        &format!("schemas/{name}.ts"),
                        "--prettier",
                    ])
                    .stdout(Stdio::null())
                    .status()
                    .await?;
                println!("Generated schemas/{name}.borsh and schemas/{name}.ts");
            }
        }
        Commands::Otc { action } => match action {
            OtcAction::Deploy { storage } => {
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
                println!("Optimizing otc-dex");
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
                println!("Deploying otc-dex");
                let wasm =
                    std::fs::read("./target/wasm32-unknown-unknown/release/otc_dex.wasm").unwrap();
                let wasm_base64 = BASE64_STANDARD.encode(&wasm);

                let mut transaction = Transaction::construct(
                    config.deployer_id.clone(),
                    config.dex_contract_id.clone(),
                );

                if storage {
                    transaction = transaction.add_action(Action::FunctionCall(Box::new(
                        FunctionCallAction {
                            method_name: "dex_storage_deposit".to_string(),
                            args: serde_json::to_vec(&json!({
                                "dex_id": format!("{}/{}", config.deployer_id, "otc"),
                            }))
                            .unwrap(),
                            gas: NearGas::from_tgas(10),
                            deposit: "5 NEAR".parse::<NearToken>().unwrap(),
                        },
                    )));
                }

                let result = transaction
                    .add_action(Action::FunctionCall(Box::new(FunctionCallAction {
                        method_name: "deploy_dex_code".to_string(),
                        args: serde_json::to_vec(&json!({
                            "last_part_of_id": "otc",
                            "code_base64": wasm_base64,
                        }))
                        .unwrap(),
                        gas: NearGas::from_tgas(290),
                        deposit: NearToken::from_yoctonear(1),
                    })))
                    .with_signer(Arc::clone(&config.signer))
                    .send_to(&network())
                    .await?;

                println!("Deployed. Result: {:?}", result.outcome());
            }
            OtcAction::Initialize => {
                let dex_id = format!("{}/{}", config.deployer_id, "otc");
                let result = Contract(config.dex_contract_id.clone())
                    .call_function(
                        "dex_call",
                        json!({
                            "dex_id": dex_id,
                            "method": "new",
                            "args": "",
                            "attached_assets": {},
                        }),
                    )
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(config.deployer_id.clone(), Arc::clone(&config.signer))
                    .send_to(&network())
                    .await?;
                println!("Initialized OTC dex. Result: {:?}", result.outcome());
            }
            OtcAction::SetAuthorizedKey { account_id, key } => {
                let account_signer =
                    Signer::from_keystore_with_search_for_keys(account_id.clone(), &network())
                        .await?;
                let dex_id = format!("{}/{}", config.deployer_id, "otc");
                #[derive(BorshSerialize)]
                struct OtcSetAuthorizedKeyArgs {
                    key_bytes: Vec<u8>,
                }
                let result = Contract(config.dex_contract_id.clone())
                    .call_function("dex_call", json!({
                        "dex_id": dex_id,
                        "method": "set_authorized_key",
                        "args": BASE64_STANDARD.encode(borsh::to_vec(&OtcSetAuthorizedKeyArgs {
                            key_bytes: match key {
                                PublicKey::ED25519(public_key) => [vec![0], public_key.0.to_vec()].concat(),
                                PublicKey::SECP256K1(public_key) => [vec![1], public_key.0.to_vec()].concat(),
                            },
                        }).unwrap()),
                        "attached_assets": {},
                    }))
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(account_id.clone(), account_signer)
                    .send_to(&network())
                    .await?;
                println!("Set the authorized key. Result: {:?}", result.outcome());
            }
            OtcAction::StorageDeposit { account_id, amount } => {
                let account_signer =
                    Signer::from_keystore_with_search_for_keys(account_id.clone(), &network())
                        .await?;
                let dex_id = format!("{}/{}", config.deployer_id, "otc");
                #[derive(BorshSerialize)]
                struct OtcStorageDepositArgs;
                let result = Contract(config.dex_contract_id.clone())
                    .call_function("deposit_near", json!({
                        "operations": [{
                            "DexCall": {
                                "dex_id": dex_id,
                                "method": "storage_deposit",
                                "args": BASE64_STANDARD.encode(borsh::to_vec(&OtcStorageDepositArgs).unwrap()),
                                "attached_assets": {
                                    "near": amount,
                                },
                            }
                        }]
                    }))
                    .transaction()
                    .max_gas()
                    .deposit(amount)
                    .with_signer(account_id.clone(), account_signer)
                    .send_to(&network())
                    .await?;
                println!("Storage deposit completed. Result: {:?}", result.outcome());
            }
            OtcAction::DepositAssets {
                account_id,
                asset_id,
                amount,
            } => {
                let account_signer =
                    Signer::from_keystore_with_search_for_keys(account_id.clone(), &network())
                        .await?;
                let dex_id = format!("{}/{}", config.deployer_id, "otc");
                #[derive(BorshSerialize)]
                struct OtcDepositAssetsArgs;
                // U128 serializes as a number
                let result = Contract(config.dex_contract_id.clone())
                    .call_function("dex_call", json!({
                        "dex_id": dex_id,
                        "method": "deposit_assets",
                        "args": BASE64_STANDARD.encode(borsh::to_vec(&OtcDepositAssetsArgs).unwrap()),
                        "attached_assets": HashMap::<AssetId, U128>::from_iter([(asset_id, U128(amount))]),
                    }))
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(account_id.clone(), account_signer)
                    .send_to(&network())
                    .await?;
                println!("Deposit assets completed. Result: {:?}", result.outcome());
            }
        },
        Commands::Xyk { action } => match action {
            XykAction::Deploy { storage } => {
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
                println!("Optimizing xyk-dex");
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
                println!("Deploying xyk-dex");
                let wasm =
                    std::fs::read("./target/wasm32-unknown-unknown/release/xyk_dex.wasm").unwrap();
                let wasm_base64 = BASE64_STANDARD.encode(&wasm);

                let mut transaction = Transaction::construct(
                    config.deployer_id.clone(),
                    config.dex_contract_id.clone(),
                );

                if storage {
                    transaction = transaction.add_action(Action::FunctionCall(Box::new(
                        FunctionCallAction {
                            method_name: "dex_storage_deposit".to_string(),
                            args: serde_json::to_vec(&json!({
                                "dex_id": format!("{}/{}", config.deployer_id, "xyk"),
                            }))
                            .unwrap(),
                            gas: NearGas::from_tgas(10),
                            deposit: "5 NEAR".parse::<NearToken>().unwrap(),
                        },
                    )));
                }

                let result = transaction
                    .add_action(Action::FunctionCall(Box::new(FunctionCallAction {
                        method_name: "deploy_dex_code".to_string(),
                        args: serde_json::to_vec(&json!({
                            "last_part_of_id": "xyk",
                            "code_base64": wasm_base64,
                        }))
                        .unwrap(),
                        gas: NearGas::from_tgas(290),
                        deposit: NearToken::from_yoctonear(1),
                    })))
                    .with_signer(Arc::clone(&config.signer))
                    .send_to(&network())
                    .await?;

                println!("Deployed. Result: {:?}", result.outcome());
            }
            XykAction::Initialize => {
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                let result = Contract(config.dex_contract_id.clone())
                    .call_function(
                        "dex_call",
                        json!({
                            "dex_id": dex_id,
                            "method": "new",
                            "args": "",
                            "attached_assets": {},
                        }),
                    )
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(config.deployer_id.clone(), Arc::clone(&config.signer))
                    .send_to(&network())
                    .await?;
                println!("Initialized XYK dex. Result: {:?}", result.outcome());
            }
            XykAction::CreatePool { action } => {
                let (account_id, asset_0, asset_1, pool_type, fees, attached_assets) = match action
                {
                    XykCreatePoolAction::Private {
                        account_id,
                        asset_0,
                        asset_1,
                        fees,
                    } => (
                        account_id,
                        asset_0,
                        asset_1,
                        XykPoolType::PrivateLatest,
                        fees,
                        HashMap::<AssetId, U128>::from_iter([(
                            AssetId::Near,
                            U128("0.01 NEAR".parse::<NearToken>().unwrap().as_yoctonear()),
                        )]),
                    ),
                    XykCreatePoolAction::Public {
                        account_id,
                        asset_0,
                        asset_1,
                        fees,
                    } => (
                        account_id,
                        asset_0,
                        asset_1,
                        XykPoolType::PublicLatest,
                        fees,
                        HashMap::<AssetId, U128>::from_iter([(
                            AssetId::Near,
                            U128("0.01 NEAR".parse::<NearToken>().unwrap().as_yoctonear()),
                        )]),
                    ),
                    XykCreatePoolAction::Launch {
                        account_id,
                        asset_0,
                        asset_1,
                        phantom_liquidity_near,
                        launched_asset_amount,
                        fees,
                    } => {
                        assert!(
                            asset_0 == AssetId::Near,
                            "Launch pools require asset_0 to be NEAR",
                        );
                        assert!(
                            phantom_liquidity_near > 0,
                            "--phantom-liquidity-near must be greater than 0",
                        );
                        (
                            account_id,
                            asset_0,
                            asset_1.clone(),
                            XykPoolType::LaunchLatest {
                                phantom_liquidity_near,
                            },
                            fees,
                            HashMap::<AssetId, U128>::from_iter([
                                (
                                    AssetId::Near,
                                    U128("0.01 NEAR".parse::<NearToken>().unwrap().as_yoctonear()),
                                ),
                                (asset_1, U128(launched_asset_amount)),
                            ]),
                        )
                    }
                };
                let account_signer =
                    Signer::from_keystore_with_search_for_keys(account_id.clone(), &network())
                        .await?;
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                let result = Contract(config.dex_contract_id.clone())
                    .call_function("execute_operations", json!({
                        "operations": [{
                            "DexCall": {
                                "dex_id": dex_id,
                                "method": "create_pool",
                                "args": BASE64_STANDARD.encode(borsh::to_vec(&XykCreatePoolArgs {
                                    assets: (asset_0, asset_1),
                                    fees: XykFeeConfiguration::V1(XykCurrentFees {
                                        receivers: fees.iter().map(|f| (XykFeeReceiver::Account(f.account_id.clone()), f.fee_fraction)).collect(),
                                    }),
                                    pool_type,
                                }).unwrap()),
                                "attached_assets": attached_assets,
                            }
                        }]
                    }))
                    .transaction()
                    .max_gas()
                    .deposit("0.01 NEAR".parse::<NearToken>().unwrap())
                    .with_signer(account_id.clone(), account_signer)
                    .send_to(&network())
                    .await?;
                println!("Pool created. Result: {:?}", result.outcome());
            }
            XykAction::GetPool { pool_id } => {
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                match xyk_fetch_pool(config.dex_contract_id.clone(), &dex_id, pool_id).await? {
                    Some(pool) => {
                        println!("Pool: {:#?}", pool);
                        let needs_upgrade = xyk_pool_needs_upgrade(
                            config.dex_contract_id.clone(),
                            &dex_id,
                            pool_id,
                        )
                        .await?;
                        if needs_upgrade {
                            println!("\n⚠️  This pool needs an upgrade to the latest version.");
                            println!("   Run: xyk upgrade-pool <account_id> {pool_id}");
                        }
                    }
                    None => println!("Pool not found"),
                }
            }
            XykAction::UpgradePool {
                account_id,
                pool_id,
            } => {
                let account_signer =
                    Signer::from_keystore_with_search_for_keys(account_id.clone(), &network())
                        .await?;
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                let result = Contract(config.dex_contract_id.clone())
                    .call_function(
                        "execute_operations",
                        json!({
                            "operations": [{
                                "DexCall": {
                                    "dex_id": dex_id,
                                    "method": "upgrade_pool",
                                    "args": BASE64_STANDARD.encode(borsh::to_vec(&XykUpgradePoolArgs {
                                        pool_id,
                                    }).unwrap()),
                                    "attached_assets": {},
                                }
                            }]
                        }),
                    )
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(account_id.clone(), account_signer)
                    .send_to(&network())
                    .await?;
                println!("Pool upgraded. Result: {:?}", result.outcome());
            }
            XykAction::AddLiquidity {
                account_id,
                pool_id,
                amount_0,
                amount_1,
            } => {
                let account_signer =
                    Signer::from_keystore_with_search_for_keys(account_id.clone(), &network())
                        .await?;
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");

                let pool = xyk_fetch_pool(config.dex_contract_id.clone(), &dex_id, pool_id).await?;
                let pool = pool.expect("Pool not found");
                let (asset_0, asset_1, should_register_liquidity) = match pool {
                    XykPoolView::Private { assets, .. } => {
                        (assets.0.asset_id, assets.1.asset_id, false)
                    }
                    XykPoolView::Public { assets, .. } => {
                        (assets.0.asset_id, assets.1.asset_id, true)
                    }
                    XykPoolView::Launch { .. } => {
                        panic!("Launch pools don't support adding liquidity after creation")
                    }
                };

                let mut operations = vec![json!({
                    "DexCall": {
                        "dex_id": dex_id,
                        "method": "add_liquidity",
                        "args": BASE64_STANDARD.encode(borsh::to_vec(&XykAddLiquidityArgs { pool_id, min_shares_received: None }).unwrap()),
                        "attached_assets": HashMap::<AssetId, U128>::from_iter([
                            (asset_0, U128(amount_0)),
                            (asset_1, U128(amount_1)),
                        ]),
                    }
                })];
                if should_register_liquidity {
                    operations.push(json!({
                        "DexCall": {
                            "dex_id": dex_id,
                            "method": "register_liquidity",
                            "args": BASE64_STANDARD.encode(borsh::to_vec(&XykRegisterLiquidityArgs { pool_id }).unwrap()),
                            "attached_assets": HashMap::<AssetId, U128>::from_iter([(AssetId::Near, U128("0.01 NEAR".parse::<NearToken>().unwrap().as_yoctonear()))]),
                        }
                    }));
                }
                let result = Contract(config.dex_contract_id.clone())
                    .call_function(
                        "execute_operations",
                        json!({
                            "operations": operations,
                        }),
                    )
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(account_id.clone(), account_signer)
                    .send_to(&network())
                    .await?;
                println!("Liquidity added. Result: {:?}", result.outcome());
            }
            XykAction::RemoveLiquidity {
                account_id,
                pool_id,
                shares,
            } => {
                let account_signer =
                    Signer::from_keystore_with_search_for_keys(account_id.clone(), &network())
                        .await?;
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                let result = Contract(config.dex_contract_id.clone())
                    .call_function("execute_operations", json!({
                        "operations": [{
                            "DexCall": {
                                "dex_id": dex_id,
                                "method": "remove_liquidity",
                                "args": BASE64_STANDARD.encode(borsh::to_vec(&XykRemoveLiquidityArgs {
                                    pool_id,
                                    shares_to_remove: shares.and_then(NonZeroU128::new),
                                    min_assets_received: None,
                                }).unwrap()),
                                "attached_assets": {},
                            }
                        }]
                    }))
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(account_id.clone(), account_signer)
                    .send_to(&network())
                    .await?;
                println!("Liquidity removed. Result: {:?}", result.outcome());
            }
            XykAction::EditFees {
                account_id,
                pool_id,
                fees,
            } => {
                let account_signer =
                    Signer::from_keystore_with_search_for_keys(account_id.clone(), &network())
                        .await?;
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                let result = Contract(config.dex_contract_id.clone())
                    .call_function("execute_operations", json!({
                        "operations": [{
                            "DexCall": {
                                "dex_id": dex_id,
                                "method": "edit_fees",
                                "args": BASE64_STANDARD.encode(borsh::to_vec(&XykEditFeesArgs {
                                    pool_id,
                                    fees: XykFeeConfiguration::V1(XykCurrentFees {
                                        receivers: fees.iter().map(|f| (XykFeeReceiver::Account(f.account_id.clone()), f.fee_fraction)).collect(),
                                    }),
                                }).unwrap()),
                                "attached_assets": HashMap::<AssetId, U128>::from_iter([(AssetId::Near, U128("0.01 NEAR".parse::<NearToken>().unwrap().as_yoctonear()))]),
                            }
                        }]
                    }))
                    .transaction()
                    .max_gas()
                    .deposit("0.01 NEAR".parse::<NearToken>().unwrap())
                    .with_signer(account_id.clone(), account_signer)
                    .send_to(&network())
                    .await?;
                println!("Fees updated. Result: {:?}", result.outcome());
            }
            XykAction::GetPendingFees {
                account_id,
                asset_ids,
            } => {
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                let result: near_api::Data<serde_json::Value> = Contract(
                    config.dex_contract_id.clone(),
                )
                .call_function(
                    "dex_view",
                    json!({
                        "dex_id": dex_id,
                        "method": "get_pending_fees",
                        "args": BASE64_STANDARD.encode(borsh::to_vec(&XykGetPendingFeesArgs {
                            account_id,
                            asset_ids,
                        }).unwrap()),
                    }),
                )
                .read_only()
                .fetch_from(&network())
                .await?;
                let response_base64 = result.data.as_str().expect("Expected base64 response");
                let response_bytes = BASE64_STANDARD.decode(response_base64)?;
                let pending_fees: Vec<(AssetId, U128)> = borsh::from_slice(&response_bytes)?;
                if pending_fees.is_empty() {
                    println!("No pending fees");
                } else {
                    println!("Pending fees:");
                    for (asset_id, amount) in pending_fees {
                        println!("  {}: {}", asset_id, amount.0);
                    }
                }
            }
            XykAction::WithdrawFees {
                account_id,
                asset_ids,
            } => {
                let account_signer =
                    Signer::from_keystore_with_search_for_keys(account_id.clone(), &network())
                        .await?;
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                let result = Contract(config.dex_contract_id.clone())
                    .call_function(
                        "execute_operations",
                        json!({
                            "operations": [{
                                "DexCall": {
                                    "dex_id": dex_id,
                                    "method": "withdraw_fees",
                                    "args": BASE64_STANDARD.encode(borsh::to_vec(&XykWithdrawFeesArgs {
                                        assets: asset_ids,
                                    }).unwrap()),
                                    "attached_assets": {},
                                }
                            }]
                        }),
                    )
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(account_id.clone(), account_signer)
                    .send_to(&network())
                    .await?;
                println!("Fees withdrawn. Result: {:?}", result.outcome());
            }
            XykAction::SimulateTrade {
                direction,
                amount,
                pool_id,
                asset_in,
            } => {
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                let pool = xyk_fetch_pool(config.dex_contract_id.clone(), &dex_id, pool_id).await?;
                let pool = pool.expect("Pool not found");
                let (asset_0, asset_1) = match &pool {
                    XykPoolView::Private { assets, .. } | XykPoolView::Public { assets, .. } => {
                        (assets.0.asset_id.clone(), assets.1.asset_id.clone())
                    }
                    XykPoolView::Launch { launched_asset, .. } => {
                        (AssetId::Near, launched_asset.asset_id.clone())
                    }
                };
                let asset_out = if asset_in == asset_0 {
                    asset_1
                } else if asset_in == asset_1 {
                    asset_0
                } else {
                    panic!("Asset in not found in pool");
                };
                let swap_amount = match direction {
                    TradeDirection::ExactIn => SwapRequestAmount::ExactIn(U128(amount)),
                    TradeDirection::ExactOut => SwapRequestAmount::ExactOut(U128(amount)),
                };
                let result: near_api::Data<(U128, U128)> =
                    Contract(config.dex_contract_id.clone())
                        .call_function(
                            "simulate_swap_simple",
                            json!({
                                "dex_id": dex_id,
                                "message": BASE64_STANDARD.encode(borsh::to_vec(&XykSwapArgs { pool_id }).unwrap()),
                                "trader": "simulate.near",
                                "asset_in": asset_in,
                                "asset_out": asset_out,
                                "amount": swap_amount,
                            }),
                        )
                        .read_only()
                        .fetch_from(&network())
                        .await?;
                let (amount_in, amount_out) = result.data;
                println!("Simulated swap result:");
                println!("  Amount in:  {} ({})", amount_in.0, asset_in);
                println!("  Amount out: {} ({})", amount_out.0, asset_out);
            }
            XykAction::Trade {
                account_id,
                direction,
                amount,
                pool_id,
                asset_in,
                slippage,
                withdraw,
                deposit,
            } => {
                let account_signer =
                    Signer::from_keystore_with_search_for_keys(account_id.clone(), &network())
                        .await?;
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                let pool = xyk_fetch_pool(config.dex_contract_id.clone(), &dex_id, pool_id).await?;
                let pool = pool.expect("Pool not found");
                let (asset_0, asset_1) = match &pool {
                    XykPoolView::Private { assets, .. } | XykPoolView::Public { assets, .. } => {
                        (assets.0.asset_id.clone(), assets.1.asset_id.clone())
                    }
                    XykPoolView::Launch { launched_asset, .. } => {
                        (AssetId::Near, launched_asset.asset_id.clone())
                    }
                };
                let asset_out = if asset_in == asset_0 {
                    asset_1
                } else if asset_in == asset_1 {
                    asset_0
                } else {
                    panic!("Asset in not found in pool");
                };
                let swap_amount = match direction {
                    TradeDirection::ExactIn => SwapRequestAmount::ExactIn(U128(amount)),
                    TradeDirection::ExactOut => SwapRequestAmount::ExactOut(U128(amount)),
                };
                let constraint: Option<U128> = if let Some(SlippagePercent(slippage_pct)) = slippage
                {
                    let sim_result: near_api::Data<(U128, U128)> =
                        Contract(config.dex_contract_id.clone())
                            .call_function(
                                "simulate_swap_simple",
                                json!({
                                    "dex_id": dex_id,
                                    "message": BASE64_STANDARD.encode(borsh::to_vec(&XykSwapArgs { pool_id }).unwrap()),
                                    "trader": account_id,
                                    "asset_in": asset_in,
                                    "asset_out": asset_out,
                                    "amount": swap_amount,
                                }),
                            )
                            .read_only()
                            .fetch_from(&network())
                            .await?;
                    let (sim_amount_in, sim_amount_out) = sim_result.data;
                    let multiplier = slippage_pct / 100.0;
                    match direction {
                        TradeDirection::ExactIn => {
                            let min_out = (sim_amount_out.0 as f64 * (1.0 - multiplier)) as u128;
                            println!(
                                "Simulated amount out: {}, min out with {slippage_pct}% slippage: {min_out}",
                                sim_amount_out.0
                            );
                            Some(U128(min_out))
                        }
                        TradeDirection::ExactOut => {
                            let max_in = (sim_amount_in.0 as f64 * (1.0 + multiplier)) as u128;
                            println!(
                                "Simulated amount in: {}, max in with {slippage_pct}% slippage: {max_in}",
                                sim_amount_in.0
                            );
                            Some(U128(max_in))
                        }
                    }
                } else {
                    None
                };

                // --deposit implies --withdraw
                let should_withdraw = withdraw || deposit;

                let mut operations = vec![Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: BASE64_STANDARD
                        .encode(borsh::to_vec(&XykSwapArgs { pool_id }).unwrap()),
                    asset_in: asset_in.clone(),
                    asset_out: asset_out.clone(),
                    amount: SwapOperationAmount::Amount(swap_amount),
                    constraint,
                }];

                if should_withdraw {
                    operations.push(Operation::Withdraw {
                        asset_id: asset_out.clone(),
                        amount: WithdrawAmount::PreviousSwapOutput,
                        to: None,
                        rescue_address: None,
                    });
                }

                let deposit_asset = if deposit {
                    Some((asset_in.clone(), amount))
                } else {
                    None
                };

                execute_operations_with_deposit(
                    config.dex_contract_id.clone(),
                    operations,
                    deposit_asset,
                    account_id.clone(),
                    account_signer,
                )
                .await?;
            }
        },
        Commands::RegisterAssets {
            account_id,
            asset_ids,
            r#for,
        } => {
            let account_signer =
                Signer::from_keystore_with_search_for_keys(account_id.clone(), &network()).await?;
            let result = Contract(config.dex_contract_id.clone())
                .call_function(
                    "register_assets",
                    json!({
                        "asset_ids": asset_ids,
                        "for": r#for,
                    }),
                )
                .transaction()
                .max_gas()
                .deposit(NearToken::from_yoctonear(1))
                .with_signer(account_id.clone(), account_signer)
                .send_to(&network())
                .await?;
            println!("Assets registered. Result: {:?}", result.outcome());
        }
        Commands::DepositAsset {
            account_id,
            asset_id,
            amount,
        } => {
            let account_signer =
                Signer::from_keystore_with_search_for_keys(account_id.clone(), &network()).await?;
            execute_operations_with_deposit(
                config.dex_contract_id.clone(),
                vec![],
                Some((asset_id, amount)),
                account_id,
                account_signer,
            )
            .await?;
        }
    }

    Ok(())
}

#[derive(
    PartialEq,
    Eq,
    Hash,
    Clone,
    PartialOrd,
    Ord,
    Debug,
    BorshSerialize,
    BorshDeserialize,
    BorshSchema,
)]
pub enum AssetId {
    Near,
    Nep141(AccountId),
    Nep245(AccountId, String),
    Nep171(AccountId, String),
}

impl Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Near => write!(f, "near"),
            Self::Nep141(contract_id) => write!(f, "nep141:{contract_id}"),
            Self::Nep245(contract_id, token_id) => write!(f, "nep245:{contract_id}:{token_id}"),
            Self::Nep171(contract_id, token_id) => write!(f, "nep171:{contract_id}:{token_id}"),
        }
    }
}

impl FromStr for AssetId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "near" => Ok(Self::Near),
            _ => match s.split_once(':') {
                Some(("nep141", contract_id)) => {
                    Ok(Self::Nep141(contract_id.parse().map_err(|e| {
                        format!("Invalid account id {contract_id}: {e}")
                    })?))
                }
                Some(("nep245", rest)) => {
                    if let Some((contract_id, token_id)) = rest.split_once(':') {
                        Ok(Self::Nep245(
                            contract_id
                                .parse()
                                .map_err(|e| format!("Invalid account id {contract_id}: {e}"))?,
                            token_id.to_string(),
                        ))
                    } else {
                        Err(format!("Invalid asset id: {s}"))
                    }
                }
                Some(("nep171", rest)) => {
                    if let Some((contract_id, token_id)) = rest.split_once(':') {
                        Ok(Self::Nep171(
                            contract_id
                                .parse()
                                .map_err(|e| format!("Invalid account id {contract_id}: {e}"))?,
                            token_id.to_string(),
                        ))
                    } else {
                        Err(format!("Invalid asset id: {s}"))
                    }
                }
                _ => Err(format!("Invalid asset id: {s}")),
            },
        }
    }
}

impl Serialize for AssetId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serde::Serialize::serialize(&self.to_string(), serializer)
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = Deserialize::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, BorshSchema)]
enum XykFeeConfiguration {
    V1(XykCurrentFees),
    V2(XykV1FeeConfiguration),
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, BorshSchema)]
struct XykCurrentFees {
    receivers: Vec<(XykFeeReceiver, u32)>,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, BorshSchema)]
struct XykV1FeeConfiguration {
    receivers: Vec<(XykFeeReceiver, XykFeeAmount)>,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, BorshSchema)]
enum XykFeeAmount {
    Fixed(u32),
    Scheduled {
        start: (u64, u32),
        end: (u64, u32),
        curve: XykScheduledFeeCurve,
    },
    Dynamic {
        min: u32,
        max: u32,
    },
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, BorshSchema)]
enum XykScheduledFeeCurve {
    Linear,
}

#[derive(BorshSerialize, Serialize, Clone, Debug)]
enum SwapRequestAmount {
    ExactIn(U128),
    ExactOut(U128),
}

#[derive(Serialize, Clone, Debug)]
#[allow(dead_code)]
enum Operation {
    RegisterAssets {
        asset_ids: Vec<AssetId>,
        r#for: Option<AccountOrDexId>,
    },
    Withdraw {
        asset_id: AssetId,
        amount: WithdrawAmount,
        to: Option<AccountId>,
        rescue_address: Option<AccountId>,
    },
    SwapSimple {
        dex_id: String,
        message: String,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapOperationAmount,
        constraint: Option<U128>,
    },
    DexCall {
        dex_id: String,
        method: String,
        args: String,
        attached_assets: HashMap<AssetId, U128>,
    },
    TransferAsset {
        to: AccountOrDexId,
        asset_id: AssetId,
        amount: U128,
    },
    StorageDeposit {
        amount: U128,
        r#for: Option<AccountOrDexId>,
    },
}

#[derive(Serialize, Clone, Debug)]
#[allow(dead_code)]
enum SwapOperationAmount {
    Amount(SwapRequestAmount),
    OutputOfLastIn,
    EntireBalanceIn,
}

#[derive(Serialize, Clone, Debug)]
#[allow(dead_code)]
enum WithdrawAmount {
    Full { at_least: Option<U128> },
    Exact(U128),
    PreviousSwapOutput,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, BorshSchema)]
enum XykFeeReceiver {
    Account(AccountId),
    Pool,
}

#[allow(dead_code)]
#[derive(BorshDeserialize, Debug)]
enum XykPoolView {
    Private {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: XykCurrentFees,
        fee_configuration: XykFeeConfiguration,
        owner_id: AccountId,
        locked: bool,
    },
    Public {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: XykCurrentFees,
        fee_configuration: XykFeeConfiguration,
        total_shares: Option<U128>,
    },
    Launch {
        near_amount: U128,
        launched_asset: AssetWithBalance,
        fees: XykCurrentFees,
        fee_configuration: XykFeeConfiguration,
        phantom_liquidity_near: U128,
    },
}

#[allow(dead_code)]
#[derive(BorshDeserialize, Clone, Debug, BorshSchema)]
struct AssetWithBalance {
    asset_id: AssetId,
    balance: u128,
}

type XykPoolId = u32;

#[derive(BorshSerialize, BorshSchema)]
#[allow(dead_code)]
enum XykPoolType {
    PrivateLatest,
    PublicLatest,
    LaunchLatest { phantom_liquidity_near: u128 },
    LaunchV1 { phantom_liquidity_near: u128 },
    PrivateV1,
    PublicV1,
    PrivateV2,
    PublicV2,
}

#[derive(BorshSerialize, BorshSchema)]
struct XykCreatePoolArgs {
    assets: (AssetId, AssetId),
    fees: XykFeeConfiguration,
    pool_type: XykPoolType,
}

#[derive(BorshSerialize, BorshSchema)]
struct XykEditFeesArgs {
    pool_id: XykPoolId,
    fees: XykFeeConfiguration,
}

#[derive(BorshSerialize, BorshSchema)]
struct XykGetPendingFeesArgs {
    account_id: AccountId,
    asset_ids: Vec<AssetId>,
}

#[derive(BorshSerialize, BorshSchema)]
struct XykWithdrawFeesArgs {
    assets: Vec<AssetId>,
}

#[derive(BorshSerialize, BorshSchema)]
struct XykSwapArgs {
    pool_id: XykPoolId,
}

#[derive(BorshSerialize, BorshSchema)]
struct XykRegisterLiquidityArgs {
    pool_id: XykPoolId,
}

#[derive(BorshSerialize, BorshSchema)]
struct XykAddLiquidityArgs {
    pool_id: XykPoolId,
    min_shares_received: Option<NonZeroU128>,
}

#[derive(BorshSerialize, BorshSchema)]
struct XykRemoveLiquidityArgs {
    pool_id: XykPoolId,
    shares_to_remove: Option<NonZeroU128>,
    min_assets_received: Option<(u128, u128)>,
}

async fn xyk_fetch_pool(
    dex_contract_id: AccountId,
    dex_id: &str,
    pool_id: XykPoolId,
) -> Result<Option<XykPoolView>, Box<dyn std::error::Error>> {
    let result: String = Contract(dex_contract_id)
        .call_function(
            "dex_view",
            json!({
                "dex_id": dex_id,
                "method": "get_pool",
                "args": BASE64_STANDARD.encode(borsh::to_vec(&pool_id).unwrap()),
            }),
        )
        .read_only()
        .fetch_from(&network())
        .await?
        .data;
    let response_bytes = BASE64_STANDARD.decode(&result)?;
    Ok(borsh::from_slice(&response_bytes)?)
}

#[derive(BorshSerialize, BorshSchema)]
struct XykUpgradePoolArgs {
    pool_id: XykPoolId,
}

#[derive(BorshSerialize, BorshSchema)]
struct XykPoolNeedsUpgradeArgs {
    pool_id: XykPoolId,
}

async fn xyk_pool_needs_upgrade(
    dex_contract_id: AccountId,
    dex_id: &str,
    pool_id: XykPoolId,
) -> Result<bool, Box<dyn std::error::Error>> {
    let result: String = Contract(dex_contract_id)
        .call_function(
            "dex_view",
            json!({
                "dex_id": dex_id,
                "method": "pool_needs_upgrade",
                "args": BASE64_STANDARD.encode(borsh::to_vec(&XykPoolNeedsUpgradeArgs { pool_id }).unwrap()),
            }),
        )
        .read_only()
        .fetch_from(&network())
        .await?
        .data;
    let response_bytes = BASE64_STANDARD.decode(&result)?;
    Ok(borsh::from_slice(&response_bytes)?)
}
