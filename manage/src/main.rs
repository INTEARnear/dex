use base64::{Engine, prelude::BASE64_STANDARD};
use borsh::BorshSerialize;
use clap::{Parser, Subcommand};
use near_api::{
    Contract, NearToken, NetworkConfig, RPCEndpoint, Signer,
    types::{AccountId, PublicKey, json::U128},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::json;
use std::{collections::HashMap, fmt::Display, str::FromStr, sync::Arc};
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
}

#[derive(Subcommand)]
enum OtcAction {
    Deploy,
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
    Deploy,
    CreatePool {
        account_id: AccountId,
        asset_0: AssetId,
        asset_1: AssetId,
        #[arg(long)]
        public: bool,
        /// Fee receivers in format "account_id:fee_fraction" (fee_fraction is 10000 = 1%)
        #[arg(long, value_delimiter = ',')]
        fees: Vec<FeeReceiverArg>,
    },
    GetPool {
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let config = load_config().await?;

    println!("Loaded config: deployer_id = {}", config.deployer_id);

    match cli.command {
        Commands::Otc { action } => match action {
            OtcAction::Deploy => {
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
                let result = Contract(config.dex_contract_id.clone())
                    .call_function(
                        "deploy_dex_code",
                        json!({
                            "last_part_of_id": "otc",
                            "code_base64": wasm_base64,
                        }),
                    )
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(config.deployer_id.clone(), Arc::clone(&config.signer))
                    .send_to(&network())
                    .await?;
                println!("Deployed. Result: {:?}", result.outcome());
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
            XykAction::Deploy => {
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
                let result = Contract(config.dex_contract_id.clone())
                    .call_function(
                        "deploy_dex_code",
                        json!({
                            "last_part_of_id": "xyk",
                            "code_base64": wasm_base64,
                        }),
                    )
                    .transaction()
                    .max_gas()
                    .deposit(NearToken::from_yoctonear(1))
                    .with_signer(config.deployer_id.clone(), Arc::clone(&config.signer))
                    .send_to(&network())
                    .await?;
                println!("Deployed. Result: {:?}", result.outcome());
            }
            XykAction::CreatePool {
                account_id,
                asset_0,
                asset_1,
                public,
                fees,
            } => {
                let account_signer =
                    Signer::from_keystore_with_search_for_keys(account_id.clone(), &network())
                        .await?;
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                #[derive(BorshSerialize)]
                struct CreatePoolArgs {
                    assets: (AssetId, AssetId),
                    fees: XykFeeConfiguration,
                    is_public: bool,
                }
                let result = Contract(config.dex_contract_id.clone())
                    .call_function("deposit_near", json!({
                        "operations": [{
                            "DexCall": {
                                "dex_id": dex_id,
                                "method": "create_pool",
                                "args": BASE64_STANDARD.encode(borsh::to_vec(&CreatePoolArgs {
                                    assets: (asset_0, asset_1),
                                    fees: XykFeeConfiguration {
                                        receivers: fees.iter().map(|f| (XykFeeReceiver::User(f.account_id.clone()), f.fee_fraction)).collect(),
                                    },
                                    is_public: public,
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
                println!("Pool created. Result: {:?}", result.outcome());
            }
            XykAction::GetPool { pool_id } => {
                let dex_id = format!("{}/{}", config.deployer_id, "xyk");
                match xyk_fetch_pool(config.dex_contract_id.clone(), &dex_id, pool_id).await? {
                    Some(pool) => println!("Pool: {:#?}", pool),
                    None => println!("Pool not found"),
                }
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
                let (asset_0, asset_1) = match pool {
                    XykPoolView::Private { assets, .. } | XykPoolView::Public { assets, .. } => {
                        (assets.0.asset_id, assets.1.asset_id)
                    }
                };

                #[derive(BorshSerialize)]
                struct RegisterLiquidityArgs {
                    pool_id: XykPoolId,
                }
                #[derive(BorshSerialize)]
                struct AddLiquidityArgs {
                    pool_id: XykPoolId,
                }

                let result = Contract(config.dex_contract_id.clone())
                    .call_function(
                        "deposit_near",
                        json!({
                            "operations": [
                                {
                                    "DexCall": {
                                        "dex_id": dex_id,
                                        "method": "register_liquidity",
                                        "args": BASE64_STANDARD.encode(borsh::to_vec(&RegisterLiquidityArgs { pool_id }).unwrap()),
                                        "attached_assets": HashMap::<AssetId, U128>::from_iter([(AssetId::Near, U128("0.01 NEAR".parse::<NearToken>().unwrap().as_yoctonear()))]),
                                    }
                                },
                                {
                                    "DexCall": {
                                        "dex_id": dex_id,
                                        "method": "add_liquidity",
                                        "args": BASE64_STANDARD.encode(borsh::to_vec(&AddLiquidityArgs { pool_id }).unwrap()),
                                        "attached_assets": HashMap::<AssetId, U128>::from_iter([
                                            (asset_0, U128(amount_0)),
                                            (asset_1, U128(amount_1)),
                                        ]),
                                    }
                                },
                            ]
                        }),
                    )
                    .transaction()
                    .max_gas()
                    .deposit("0.01 NEAR".parse::<NearToken>().unwrap())
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
                #[derive(BorshSerialize)]
                struct RemoveLiquidityArgs {
                    pool_id: XykPoolId,
                    shares_to_remove: Option<std::num::NonZeroU128>,
                }
                let result = Contract(config.dex_contract_id.clone())
                    .call_function("execute_operations", json!({
                        "operations": [{
                            "DexCall": {
                                "dex_id": dex_id,
                                "method": "remove_liquidity",
                                "args": BASE64_STANDARD.encode(borsh::to_vec(&RemoveLiquidityArgs {
                                    pool_id,
                                    shares_to_remove: shares.and_then(std::num::NonZeroU128::new),
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
                #[derive(BorshSerialize)]
                struct EditFeesArgs {
                    pool_id: XykPoolId,
                    fees: XykFeeConfiguration,
                }
                let result = Contract(config.dex_contract_id.clone())
                    .call_function("deposit_near", json!({
                        "operations": [{
                            "DexCall": {
                                "dex_id": dex_id,
                                "method": "edit_fees",
                                "args": BASE64_STANDARD.encode(borsh::to_vec(&EditFeesArgs {
                                    pool_id,
                                    fees: XykFeeConfiguration {
                                        receivers: fees.iter().map(|f| (XykFeeReceiver::User(f.account_id.clone()), f.fee_fraction)).collect(),
                                    },
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
                #[derive(BorshSerialize)]
                struct GetPendingFeesArgs {
                    account_id: AccountId,
                    asset_ids: Vec<AssetId>,
                }
                let result: near_api::Data<serde_json::Value> =
                    Contract(config.dex_contract_id.clone())
                        .call_function(
                            "dex_view",
                            json!({
                                "dex_id": dex_id,
                                "method": "get_pending_fees",
                                "args": BASE64_STANDARD.encode(borsh::to_vec(&GetPendingFeesArgs {
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
                #[derive(BorshSerialize)]
                struct WithdrawFeesArgs {
                    assets: Vec<AssetId>,
                }
                let result = Contract(config.dex_contract_id.clone())
                    .call_function(
                        "execute_operations",
                        json!({
                            "operations": [{
                                "DexCall": {
                                    "dex_id": dex_id,
                                    "method": "withdraw_fees",
                                    "args": BASE64_STANDARD.encode(borsh::to_vec(&WithdrawFeesArgs {
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
        },
    }

    Ok(())
}

#[derive(
    PartialEq, Eq, Hash, Clone, PartialOrd, Ord, Debug, BorshSerialize, borsh::BorshDeserialize,
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

#[derive(BorshSerialize, borsh::BorshDeserialize, Clone, Debug)]
struct XykFeeConfiguration {
    receivers: Vec<(XykFeeReceiver, u32)>,
}

#[derive(BorshSerialize, borsh::BorshDeserialize, Clone, Debug)]
enum XykFeeReceiver {
    User(AccountId),
}

#[allow(dead_code)]
#[derive(borsh::BorshDeserialize, Debug)]
enum XykPoolView {
    Private {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: XykFeeConfiguration,
        owner_id: AccountId,
    },
    Public {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: XykFeeConfiguration,
        total_shares: Option<U128>,
    },
}

#[allow(dead_code)]
#[derive(borsh::BorshDeserialize, Clone, Debug)]
struct AssetWithBalance {
    asset_id: AssetId,
    balance: U128,
}

type XykPoolId = u32;

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
