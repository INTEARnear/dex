// Each test binary uses part of these helpers
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

use intear_dex_test_support::get_compiled_wasms;
use intear_dex_types::{
    AccountOrDexId, AssetId, DexId, Operation, SwapOperationAmount, SwapRequestAmount,
};
use near_sdk::json_types::{Base64VecU8, U128};
use near_workspaces::network::Sandbox;
use near_workspaces::types::{AccessKey, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, AccountDetailsPatch, AccountId, Worker};
use serde_json::json;
use xyk_dex_types::{
    AddLiquidityArgs, CreatePoolArgs, CurrentFees, FeeAmount, FeeConfiguration, FeeReceiver,
    LockPoolArgs, PoolId, PoolType, RegisterLiquidityArgs, ScheduledFeeCurve, SwapArgs,
    V2FeeConfiguration,
};

const ENGINE_ACCOUNT_ID: &str = "dex.intear.near";
const USDT_ACCOUNT_ID: &str = "usdt.tether-token.near";
const INTEL_ACCOUNT_ID: &str = "intel.tkn.near";
const ONE_USDT: u128 = 10u128.pow(6);
const ONE_INTEL: u128 = 10u128.pow(18);
const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);

/// Accounts get keys derived from their ids, so commands that sign with them
/// print the same thing in every run.
async fn create_account_with_key_from_id(
    sandbox: &Worker<Sandbox>,
    account_id: &str,
    balance: NearToken,
) -> Account {
    let account_id: AccountId = account_id.parse().unwrap();
    let secret_key = SecretKey::from_seed(KeyType::ED25519, account_id.as_str());
    sandbox
        .patch(&account_id)
        .account(AccountDetailsPatch::default().balance(balance))
        .access_key(secret_key.public_key(), AccessKey::full_access())
        .transact()
        .await
        .unwrap();
    Account::from_secret_key(account_id, secret_key, sandbox)
}

/// An account of the sandbox's fixture, signing with its key
pub fn fixture_account(sandbox: &Worker<Sandbox>, account_id: &str) -> Account {
    Account::from_secret_key(
        account_id.parse().unwrap(),
        SecretKey::from_seed(KeyType::ED25519, account_id),
        sandbox,
    )
}

async fn call(
    signer: &Account,
    receiver_id: &str,
    method_name: &str,
    args: serde_json::Value,
    deposit: NearToken,
) {
    let result = signer
        .call(&receiver_id.parse().unwrap(), method_name)
        .args_json(args)
        .deposit(deposit)
        .max_gas()
        .transact()
        .await
        .unwrap();
    assert!(
        result.failures().is_empty(),
        "{method_name} on {receiver_id} by {} failed: {result:#?}",
        signer.id()
    );
}

/// A sandbox where the engine and the xyk dex have their mainnet ids:
///
/// - USDt (`usdt.tether-token.near`, 6 decimals) and INTEL (`intel.tkn.near`,
///   18 decimals), both minted to slimedragon.near
/// - slimedragon.near deploys slimedragon.near/xyk, and has 200 NEAR, 5,000 USDt
///   and 10,000,000 INTEL on the engine before creating the pools
/// - alice.near has 5 NEAR and 250.5 USDt on the engine, and INTEL registered
/// - pool #0: private, created as V1 so it can be upgraded, NEAR / USDt
///   100 / 300, V1 fees: pool 0.2%, alice.near 0.05%
/// - pool #1: private and locked, USDt / INTEL 1,000 / 2,000,000, V2 fees:
///   alice.near 2% to 1% during the first half of 2025, pool 0.1%
/// - pool #2: public, NEAR / INTEL 50 / 1,000,000, V2 fees: pool 0.3%
/// - pool #3: launch, 10 NEAR of phantom liquidity and 5,000,000 INTEL, no fees
///   other than the protocol fee
/// - pause.slimedragon.near can pause the engine; bob.near and carol.near have
///   nothing on the engine or the tokens
pub async fn start_sandbox_with_xyk_pools() -> Worker<Sandbox> {
    let wasms = get_compiled_wasms().await;
    let sandbox = near_workspaces::sandbox().await.unwrap();

    let engine =
        create_account_with_key_from_id(&sandbox, ENGINE_ACCOUNT_ID, NearToken::from_near(100))
            .await;
    let slimedragon =
        create_account_with_key_from_id(&sandbox, "slimedragon.near", NearToken::from_near(1_000))
            .await;
    let alice =
        create_account_with_key_from_id(&sandbox, "alice.near", NearToken::from_near(100)).await;
    create_account_with_key_from_id(&sandbox, "pause.slimedragon.near", NearToken::from_near(10))
        .await;
    create_account_with_key_from_id(&sandbox, "bob.near", NearToken::from_near(100)).await;
    create_account_with_key_from_id(&sandbox, "carol.near", NearToken::from_near(100)).await;
    let usdt =
        create_account_with_key_from_id(&sandbox, USDT_ACCOUNT_ID, NearToken::from_near(100)).await;
    let intel =
        create_account_with_key_from_id(&sandbox, INTEL_ACCOUNT_ID, NearToken::from_near(100))
            .await;

    engine
        .deploy(&wasms.contract_wasm)
        .await
        .unwrap()
        .into_result()
        .unwrap();
    call(
        &engine,
        ENGINE_ACCOUNT_ID,
        "new",
        json!({ "trusted_code_deployer": slimedragon.id() }),
        NearToken::from_yoctonear(0),
    )
    .await;

    for (token, name, symbol, decimals, total_supply) in [
        (&usdt, "Tether USD", "USDt", 6, 1_000_000 * ONE_USDT),
        (&intel, "INTEAR", "INTEL", 18, 1_000_000_000 * ONE_INTEL),
    ] {
        token
            .deploy(&wasms.ft_wasm)
            .await
            .unwrap()
            .into_result()
            .unwrap();
        call(
            token,
            token.id().as_str(),
            "new",
            json!({
                "owner_id": slimedragon.id(),
                "total_supply": U128(total_supply),
                "metadata": {
                    "spec": "ft-1.0.0",
                    "name": name,
                    "symbol": symbol,
                    "decimals": decimals,
                },
            }),
            NearToken::from_yoctonear(0),
        )
        .await;
        for account_id in [ENGINE_ACCOUNT_ID, "alice.near"] {
            call(
                &slimedragon,
                token.id().as_str(),
                "storage_deposit",
                json!({ "account_id": account_id }),
                NearToken::from_millinear(10),
            )
            .await;
        }
    }

    let usdt_asset_id = AssetId::Nep141(usdt.id().clone());
    let intel_asset_id = AssetId::Nep141(intel.id().clone());
    let all_asset_ids = [AssetId::Near, usdt_asset_id.clone(), intel_asset_id.clone()];
    for user in [&slimedragon, &alice] {
        call(
            user,
            ENGINE_ACCOUNT_ID,
            "storage_deposit",
            json!({}),
            NearToken::from_near(1),
        )
        .await;
        call(
            user,
            ENGINE_ACCOUNT_ID,
            "register_assets",
            json!({ "asset_ids": all_asset_ids }),
            ONE_YOCTO,
        )
        .await;
    }

    let xyk_dex_id: DexId = "slimedragon.near/xyk".parse().unwrap();
    call(
        &slimedragon,
        ENGINE_ACCOUNT_ID,
        "dex_storage_deposit",
        json!({ "dex_id": xyk_dex_id }),
        NearToken::from_near(20),
    )
    .await;
    call(
        &slimedragon,
        ENGINE_ACCOUNT_ID,
        "deploy_dex_code",
        json!({
            "last_part_of_id": xyk_dex_id.id,
            "code_base64": Base64VecU8(wasms.xyk_dex_wasm.clone()),
        }),
        ONE_YOCTO,
    )
    .await;
    call(
        &slimedragon,
        ENGINE_ACCOUNT_ID,
        "register_assets",
        json!({
            "asset_ids": all_asset_ids,
            "for": AccountOrDexId::Dex(xyk_dex_id.clone()),
        }),
        ONE_YOCTO,
    )
    .await;
    call(
        &slimedragon,
        ENGINE_ACCOUNT_ID,
        "dex_call",
        json!({
            "dex_id": xyk_dex_id,
            "method": "new",
            "args": Base64VecU8(Vec::new()),
            "attached_assets": {},
        }),
        ONE_YOCTO,
    )
    .await;

    call(
        &slimedragon,
        ENGINE_ACCOUNT_ID,
        "deposit_near",
        json!({}),
        NearToken::from_near(200),
    )
    .await;
    for (token_account_id, amount) in [
        (USDT_ACCOUNT_ID, 5_000 * ONE_USDT),
        (INTEL_ACCOUNT_ID, 10_000_000 * ONE_INTEL),
    ] {
        call(
            &slimedragon,
            token_account_id,
            "ft_transfer_call",
            json!({ "receiver_id": ENGINE_ACCOUNT_ID, "amount": U128(amount), "msg": "" }),
            ONE_YOCTO,
        )
        .await;
    }
    call(
        &alice,
        ENGINE_ACCOUNT_ID,
        "deposit_near",
        json!({}),
        NearToken::from_near(5),
    )
    .await;
    let alice_usdt_amount = 250_500_000;
    call(
        &slimedragon,
        USDT_ACCOUNT_ID,
        "ft_transfer",
        json!({ "receiver_id": alice.id(), "amount": U128(alice_usdt_amount) }),
        ONE_YOCTO,
    )
    .await;
    call(
        &alice,
        USDT_ACCOUNT_ID,
        "ft_transfer_call",
        json!({ "receiver_id": ENGINE_ACCOUNT_ID, "amount": U128(alice_usdt_amount), "msg": "" }),
        ONE_YOCTO,
    )
    .await;

    let xyk_dex_call =
        |method: &str, args: Vec<u8>, attached_assets: Vec<(AssetId, u128)>| Operation::DexCall {
            dex_id: xyk_dex_id.clone(),
            method: method.to_string(),
            args: Base64VecU8(args),
            attached_assets: attached_assets
                .into_iter()
                .map(|(asset_id, amount)| (asset_id, U128(amount)))
                .collect::<HashMap<_, _>>(),
        };
    let storage_for_pool = (AssetId::Near, NearToken::from_millinear(50).as_yoctonear());
    let pool_operations = [
        vec![
            xyk_dex_call(
                "create_pool",
                near_sdk::borsh::to_vec(&CreatePoolArgs {
                    assets: (AssetId::Near, usdt_asset_id.clone()),
                    fees: FeeConfiguration::V1(CurrentFees {
                        receivers: vec![
                            (FeeReceiver::Pool, 2_000),
                            (FeeReceiver::Account(alice.id().clone()), 500),
                        ],
                    }),
                    pool_type: PoolType::PrivateV1,
                })
                .unwrap(),
                vec![storage_for_pool.clone()],
            ),
            xyk_dex_call(
                "add_liquidity",
                near_sdk::borsh::to_vec(&AddLiquidityArgs {
                    pool_id: 0,
                    min_shares_received: None,
                })
                .unwrap(),
                vec![
                    (AssetId::Near, NearToken::from_near(100).as_yoctonear()),
                    (usdt_asset_id.clone(), 300 * ONE_USDT),
                ],
            ),
        ],
        vec![
            xyk_dex_call(
                "create_pool",
                near_sdk::borsh::to_vec(&CreatePoolArgs {
                    assets: (usdt_asset_id.clone(), intel_asset_id.clone()),
                    fees: FeeConfiguration::V2(V2FeeConfiguration {
                        receivers: vec![
                            (
                                FeeReceiver::Account(alice.id().clone()),
                                FeeAmount::Scheduled {
                                    start: (rfc3339_to_nanoseconds("2025-01-01T00:00:00Z"), 20_000),
                                    end: (rfc3339_to_nanoseconds("2025-07-01T00:00:00Z"), 10_000),
                                    curve: ScheduledFeeCurve::Linear,
                                },
                            ),
                            (FeeReceiver::Pool, FeeAmount::Fixed(1_000)),
                        ],
                    }),
                    pool_type: PoolType::PrivateLatest,
                })
                .unwrap(),
                vec![storage_for_pool.clone()],
            ),
            xyk_dex_call(
                "add_liquidity",
                near_sdk::borsh::to_vec(&AddLiquidityArgs {
                    pool_id: 1,
                    min_shares_received: None,
                })
                .unwrap(),
                vec![
                    (usdt_asset_id.clone(), 1_000 * ONE_USDT),
                    (intel_asset_id.clone(), 2_000_000 * ONE_INTEL),
                ],
            ),
        ],
        vec![
            xyk_dex_call(
                "create_pool",
                near_sdk::borsh::to_vec(&CreatePoolArgs {
                    assets: (AssetId::Near, intel_asset_id.clone()),
                    fees: FeeConfiguration::V2(V2FeeConfiguration {
                        receivers: vec![(FeeReceiver::Pool, FeeAmount::Fixed(3_000))],
                    }),
                    pool_type: PoolType::PublicLatest,
                })
                .unwrap(),
                vec![storage_for_pool.clone()],
            ),
            xyk_dex_call(
                "register_liquidity",
                near_sdk::borsh::to_vec(&RegisterLiquidityArgs { pool_id: 2 }).unwrap(),
                vec![(AssetId::Near, NearToken::from_millinear(10).as_yoctonear())],
            ),
            xyk_dex_call(
                "add_liquidity",
                near_sdk::borsh::to_vec(&AddLiquidityArgs {
                    pool_id: 2,
                    min_shares_received: None,
                })
                .unwrap(),
                vec![
                    (AssetId::Near, NearToken::from_near(50).as_yoctonear()),
                    (intel_asset_id.clone(), 1_000_000 * ONE_INTEL),
                ],
            ),
        ],
        vec![xyk_dex_call(
            "create_pool",
            near_sdk::borsh::to_vec(&CreatePoolArgs {
                assets: (AssetId::Near, intel_asset_id.clone()),
                fees: FeeConfiguration::V1(CurrentFees {
                    receivers: Vec::new(),
                }),
                pool_type: PoolType::LaunchLatest {
                    phantom_liquidity: U128(NearToken::from_near(10).as_yoctonear()),
                },
            })
            .unwrap(),
            vec![
                storage_for_pool.clone(),
                (intel_asset_id.clone(), 5_000_000 * ONE_INTEL),
            ],
        )],
    ];
    for operations in pool_operations {
        call(
            &slimedragon,
            ENGINE_ACCOUNT_ID,
            "execute_operations",
            json!({ "operations": operations }),
            ONE_YOCTO,
        )
        .await;
    }
    call(
        &slimedragon,
        ENGINE_ACCOUNT_ID,
        "dex_call",
        json!({
            "dex_id": xyk_dex_id,
            "method": "lock_pool",
            "args": Base64VecU8(near_sdk::borsh::to_vec(&LockPoolArgs { pool_id: 1 }).unwrap()),
            "attached_assets": {},
        }),
        ONE_YOCTO,
    )
    .await;

    // Views read the final block, which lags a few blocks behind
    sandbox.fast_forward(3).await.unwrap();
    sandbox
}

/// A swap on slimedragon.near/xyk from `trader`'s balance on the engine,
/// for state that the CLI's commands work on, like collected fees
pub async fn swap_through_engine(
    sandbox: &Worker<Sandbox>,
    trader: &str,
    pool_id: PoolId,
    (asset_in, asset_out): (AssetId, AssetId),
    amount: SwapRequestAmount,
    referrer: Option<&str>,
) {
    let swap = Operation::SwapSimple {
        dex_id: "slimedragon.near/xyk".parse().unwrap(),
        message: Base64VecU8(
            near_sdk::borsh::to_vec(&SwapArgs {
                pool_id,
                fee_discount: None,
            })
            .unwrap(),
        ),
        asset_in,
        asset_out,
        amount: SwapOperationAmount::Amount(amount),
        constraint: None,
    };
    call(
        &fixture_account(sandbox, trader),
        ENGINE_ACCOUNT_ID,
        "execute_operations",
        json!({ "operations": [swap], "referrer": referrer }),
        ONE_YOCTO,
    )
    .await;
}

/// Moves an asset between two accounts' balances on the engine
pub async fn transfer_on_engine(
    sandbox: &Worker<Sandbox>,
    from: &str,
    to: &str,
    asset_id: AssetId,
    amount: u128,
) {
    let transfer = Operation::TransferAsset {
        to: AccountOrDexId::Account(to.parse().unwrap()),
        asset_id,
        amount: U128(amount),
    };
    call(
        &fixture_account(sandbox, from),
        ENGINE_ACCOUNT_ID,
        "execute_operations",
        json!({ "operations": [transfer] }),
        ONE_YOCTO,
    )
    .await;
}

/// Gives an xyk dex without pools the state layout from before fee
/// discounts, which its `migrate` converts. The engine keeps a dex's state in
/// its dex storage collection, whose prefix is 1, under the dex id and
/// near-sdk's STATE key. The engine's state is too large to read through RPC,
/// so the old state is written out: the pool vector (length, then prefix 0),
/// and the maps of collected fees, referral settings and community fees
/// (prefixes 2, 3 and 4).
pub async fn rewind_xyk_state_to_before_fee_discounts(sandbox: &Worker<Sandbox>, dex_id: &DexId) {
    let engine_id: AccountId = ENGINE_ACCOUNT_ID.parse().unwrap();
    let key = [
        vec![1u8],
        near_sdk::borsh::to_vec(&(dex_id, b"STATE".to_vec())).unwrap(),
    ]
    .concat();
    let collection_prefix = |prefix: u8| near_sdk::borsh::to_vec(&vec![prefix]).unwrap();
    let state_before_fee_discounts = [
        0u32.to_le_bytes().to_vec(),
        collection_prefix(0),
        collection_prefix(2),
        collection_prefix(3),
        collection_prefix(4),
    ]
    .concat();
    sandbox
        .patch_state(
            &engine_id,
            &key,
            &near_sdk::borsh::to_vec(&state_before_fee_discounts).unwrap(),
        )
        .await
        .unwrap();
}

fn rfc3339_to_nanoseconds(time: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(time)
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap()
        .try_into()
        .unwrap()
}

pub async fn engine_balance(
    sandbox: &Worker<Sandbox>,
    owner: serde_json::Value,
    asset_id: &str,
) -> Option<u128> {
    let engine_id: AccountId = ENGINE_ACCOUNT_ID.parse().unwrap();
    sandbox
        .view(&engine_id, "asset_balance_of")
        .args_json(json!({ "of": owner, "asset_id": asset_id }))
        .await
        .unwrap()
        .json::<Option<U128>>()
        .unwrap()
        .map(|U128(balance)| balance)
}

fn yocto_near_field(storage_balance: &serde_json::Value, field: &str) -> u128 {
    storage_balance[field]
        .as_str()
        .unwrap()
        .parse::<u128>()
        .unwrap()
}

/// What an account's storage balance on the engine has paid for so far
async fn storage_used(sandbox: &Worker<Sandbox>, account_id: &str) -> u128 {
    let engine_id: AccountId = ENGINE_ACCOUNT_ID.parse().unwrap();
    let storage_balance = sandbox
        .view(&engine_id, "storage_balance_of")
        .args_json(json!({ "account_id": account_id }))
        .await
        .unwrap()
        .json::<Option<serde_json::Value>>()
        .unwrap();
    match storage_balance {
        Some(storage_balance) => {
            yocto_near_field(&storage_balance, "total")
                - yocto_near_field(&storage_balance, "available")
        }
        None => 0,
    }
}

/// Everything a dex's storage balance on the engine received so far
async fn dex_storage_total(sandbox: &Worker<Sandbox>, dex_id: &str) -> u128 {
    let engine_id: AccountId = ENGINE_ACCOUNT_ID.parse().unwrap();
    let storage_balance = sandbox
        .view(&engine_id, "dex_storage_balance_of")
        .args_json(json!({ "dex_id": dex_id }))
        .await
        .unwrap()
        .json::<Option<serde_json::Value>>()
        .unwrap();
    storage_balance.map_or(0, |storage_balance| {
        yocto_near_field(&storage_balance, "total")
    })
}

/// An amount of NEAR that a record printed, in yoctoNEAR
fn printed_near(record: &str, pattern: &str) -> Option<u128> {
    let captures = regex::Regex::new(pattern).unwrap().captures(record)?;
    Some(
        format!("{}{:0<24}", captures[1].replace(',', ""), &captures[2])
            .parse()
            .unwrap(),
    )
}

/// Runs a registering transaction and checks that its storage estimate
/// covered what the payer's storage balance actually paid
pub async fn transaction_with_registration(
    sandbox: &Worker<Sandbox>,
    cli: &Cli,
    payer: &str,
    args: &[&str],
) -> String {
    let used_before = storage_used(sandbox, payer).await;
    let record = cli.transaction(payer, args).await;
    let used_by_registration = storage_used(sandbox, payer).await - used_before;
    let estimate = printed_near(&record, r"which takes about ([\d,]+)\.?(\d*) NEAR")
        .unwrap_or_else(|| panic!("No storage estimate in:\n{record}"));
    assert!(
        used_by_registration <= estimate,
        "The registration used {used_by_registration} yoctoNEAR of storage, more than its estimate:\n{record}"
    );
    record
}

/// Runs an xyk transaction that attaches NEAR for the dex's storage, and
/// checks that what the dex took for storage is no more than the NEAR the
/// plan said it attaches, and that registrations stayed within their
/// estimate
pub async fn xyk_transaction_paying_storage(
    sandbox: &Worker<Sandbox>,
    cli: &Cli,
    signer: &str,
    dex_id: &str,
    args: &[&str],
) -> String {
    let dex_storage_before = dex_storage_total(sandbox, dex_id).await;
    let used_before = storage_used(sandbox, signer).await;
    let record = cli.transaction(signer, args).await;
    let taken_for_dex_storage = dex_storage_total(sandbox, dex_id).await - dex_storage_before;
    let attached = printed_near(&record, r"[Aa]ttach(?:ing)? ([\d,]+)\.?(\d*) NEAR from")
        .unwrap_or_else(|| panic!("No NEAR attached for storage in:\n{record}"));
    assert!(
        taken_for_dex_storage <= attached,
        "{dex_id} took {taken_for_dex_storage} yoctoNEAR for storage, more than the {attached} attached:\n{record}"
    );
    let used_by_registration = storage_used(sandbox, signer).await - used_before;
    if used_by_registration > 0 {
        let estimate = printed_near(&record, r"which takes about ([\d,]+)\.?(\d*) NEAR")
            .unwrap_or_else(|| panic!("Registrations without an estimate in:\n{record}"));
        assert!(
            used_by_registration <= estimate,
            "The registrations used {used_by_registration} yoctoNEAR of storage, more than their estimate:\n{record}"
        );
    }
    record
}

/// Runs the CLI binary with a near-cli config that has one connection,
/// "sandbox", and nothing else from the machine it runs on.
pub struct Cli {
    config_home: tempfile::TempDir,
    rpc_url: String,
}

/// A near-cli config in `config_home` with one connection to the sandbox,
/// named like the network it says it reaches, and keys in
/// `config_home/credentials`
pub fn write_near_cli_config(config_home: &Path, rpc_url: &str, network_name: &str) {
    // Where dirs::config_dir() points when HOME and XDG_CONFIG_HOME are config_home
    let near_cli_config_directory = if cfg!(target_os = "macos") {
        config_home.join("Library/Application Support/near-cli")
    } else {
        config_home.join("near-cli")
    };
    std::fs::create_dir_all(&near_cli_config_directory).unwrap();
    std::fs::write(
        near_cli_config_directory.join("config.toml"),
        format!(
            r#"version = "5"
credentials_home_dir = "{credentials_home_dir}"

[network_connection.{network_name}]
network_name = "{network_name}"
rpc_url = "{rpc_url}"
wallet_url = "{rpc_url}"
explorer_transaction_url = "{rpc_url}transactions/"
"#,
            credentials_home_dir = config_home.join("credentials").display(),
        ),
    )
    .unwrap();
}

impl Cli {
    pub fn connected_to(sandbox: &Worker<Sandbox>) -> Self {
        let config_home = tempfile::tempdir().unwrap();
        let rpc_url = sandbox.rpc_addr();
        write_near_cli_config(config_home.path(), &rpc_url, "sandbox");
        Self {
            config_home,
            rpc_url,
        }
    }

    fn run(&self, args: &[String]) -> (String, std::process::Output) {
        let command_line = shell_words::join(
            std::iter::once("near-intear-dex-management").chain(args.iter().map(String::as_str)),
        );
        let output = Command::new(env!("CARGO_BIN_EXE_near-intear-dex-management"))
            .args(args)
            .env("HOME", self.config_home.path())
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("RUST_BACKTRACE", "0")
            .env_remove("RUST_LIB_BACKTRACE")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (command_line, output)
    }

    /// The command line, exit code, stdout and stderr, without colors, the
    /// sandbox's RPC port or trailing spaces
    fn record(command_line: &str, output: std::process::Output) -> String {
        let record = format!(
            "$ {command_line}\nexit code: {}\n--- stdout ---\n{}--- stderr ---\n{}",
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        );
        let color_escape_sequence = regex::Regex::new("\x1b\\[[0-9;]*m").unwrap();
        let sandbox_rpc_url = regex::Regex::new(r"http://(127\.0\.0\.1|localhost):\d+/").unwrap();
        let record = color_escape_sequence.replace_all(&record, "");
        let record = sandbox_rpc_url.replace_all(&record, "http://[SANDBOX RPC]/");
        // color-eyre prints "Error: " with a trailing space, which editors strip
        // from snapshot files
        record
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Runs a view on the sandbox at the final block
    pub fn view(&self, args: &[&str]) -> String {
        let args = args
            .iter()
            .copied()
            .chain(["network-config", "sandbox", "now"])
            .map(String::from)
            .collect::<Vec<_>>();
        let (command_line, output) = self.run(&args);
        if args.iter().any(|arg| arg == "--json") && output.status.success() {
            serde_json::from_slice::<serde_json::Value>(&output.stdout)
                .unwrap_or_else(|error| panic!("`{command_line}` printed invalid JSON: {error}"));
        }
        Self::record(&command_line, output)
    }

    /// Runs a command that stops before the network step, like one missing
    /// an argument, or one that takes its network and signing as given
    pub fn run_as_given(&self, args: &[&str]) -> String {
        let args = args.iter().copied().map(String::from).collect::<Vec<_>>();
        let (command_line, output) = self.run(&args);
        Self::record(&command_line, output)
    }

    /// Runs a write signed with the key derived from `key_owner`, and checks
    /// that a sent transaction burned at most 80% of the gas it attached.
    /// Hashes, gas and fees, which change between runs, are redacted.
    pub async fn transaction(&self, key_owner: &str, args: &[&str]) -> String {
        let secret_key = SecretKey::from_seed(KeyType::ED25519, key_owner).to_string();
        let args = args
            .iter()
            .copied()
            .chain([
                "network-config",
                "sandbox",
                "sign-with-plaintext-private-key",
                &secret_key,
                "send",
            ])
            .map(String::from)
            .collect::<Vec<_>>();
        let (command_line, output) = self.run(&args);
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let transaction_hash =
            regex::Regex::new(r"Transaction(?: ID)?: (?:\S*/)?([1-9A-HJ-NP-Za-km-z]{43,44})")
                .unwrap()
                .captures(&stderr)
                .map(|captures| {
                    captures[1]
                        .parse::<near_primitives::hash::CryptoHash>()
                        .unwrap()
                });
        if let Some(transaction_hash) = transaction_hash {
            assert_gas_within_budget(&self.rpc_url, transaction_hash, key_owner).await;
        }
        let record = Self::record(&command_line, output);
        let redactions = [
            (
                r"Signature:  ed25519:\S+",
                "Signature:  ed25519:[SIGNATURE]",
            ),
            (r"\b[1-9A-HJ-NP-Za-km-z]{43,44}\b", "[HASH]"),
            (r"Gas burned: .*", "Gas burned: [GAS]"),
            (r"Transaction fee: .*", "Transaction fee: [FEE]"),
        ];
        redactions
            .iter()
            .fold(record, |record, (pattern, replacement)| {
                regex::Regex::new(pattern)
                    .unwrap()
                    .replace_all(&record, *replacement)
                    .into_owned()
            })
    }
}

async fn assert_gas_within_budget(
    rpc_url: &str,
    transaction_hash: near_primitives::hash::CryptoHash,
    signer_id: &str,
) {
    let response = near_jsonrpc_client::JsonRpcClient::connect(rpc_url)
        .call(
            near_jsonrpc_client::methods::tx::RpcTransactionStatusRequest {
                transaction_info:
                    near_jsonrpc_client::methods::tx::TransactionInfo::TransactionId {
                        tx_hash: transaction_hash,
                        sender_account_id: signer_id.parse().unwrap(),
                    },
                wait_until: near_primitives::views::TxExecutionStatus::Final,
            },
        )
        .await
        .unwrap();
    let outcome = response.final_execution_outcome.unwrap().into_outcome();
    let attached_gas: u64 = outcome
        .transaction
        .actions
        .iter()
        .map(|action| match action {
            near_primitives::views::ActionView::FunctionCall { gas, .. } => gas.as_gas(),
            _ => 0,
        })
        .sum();
    let burned_gas: u64 = outcome
        .receipts_outcome
        .iter()
        .map(|receipt| receipt.outcome.gas_burnt.as_gas())
        .sum();
    assert!(
        burned_gas * 10 <= attached_gas * 8,
        "Transaction {transaction_hash} burned {burned_gas} gas, more than 80% of the {attached_gas} it attached; raise its gas budget"
    );
}
