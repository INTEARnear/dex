mod common;
use common::*;

use intear_dex::internal_asset_operations::AccountOrDexId;
use intear_dex::internal_operations::{Operation, SwapOperationAmount, WithdrawAmount};
use intear_dex_types::{AssetId, DexId, SwapRequestAmount};
use near_sdk::AccountId;
use near_sdk::serde_json::json;
use near_sdk::{
    NearToken,
    base64::{Engine, prelude::BASE64_STANDARD},
    json_types::{Base64VecU8, U128},
    near,
};
use std::collections::HashMap;

#[near(serializers=[borsh])]
struct CreatePoolArgs {
    assets: (AssetId, AssetId),
    fees: FeeConfiguration,
    pool_type: PoolType,
}

#[near(serializers=[borsh])]
enum PoolType {
    PrivateLatest,
    PublicLatest,
    LaunchLatest { phantom_liquidity_near: U128 },
    LaunchV1 { phantom_liquidity_near: U128 },
    PrivateV1,
    PublicV1,
    PrivateV2,
    PublicV2,
}

type PoolId = u32;

#[near(serializers=[borsh])]
struct RegisterLiquidityArgs {
    pool_id: PoolId,
}

type SharesBalance = U128;

#[near(serializers=[borsh])]
struct AddLiquidityArgs {
    pool_id: PoolId,
    min_shares_received: Option<SharesBalance>,
}

#[near(serializers=[borsh])]
struct RemoveLiquidityArgs {
    pool_id: PoolId,
    shares_to_remove: Option<SharesBalance>,
    min_assets_received: Option<(U128, U128)>,
}

#[near(serializers=[borsh])]
struct UpgradePoolArgs {
    pool_id: PoolId,
}

#[near(serializers=[borsh])]
struct LockPoolArgs {
    pool_id: PoolId,
}

#[near(serializers=[borsh])]
struct SwapArgs {
    pool_id: PoolId,
}

#[near(serializers=[borsh, json])]
enum FeeConfiguration {
    V1(CurrentFees),
    V2(V1FeeConfiguration),
}

#[near(serializers=[borsh, json])]
struct CurrentFees {
    receivers: Vec<(FeeReceiver, u32)>,
}

#[near(serializers=[borsh, json])]
struct V1FeeConfiguration {
    receivers: Vec<(FeeReceiver, FeeAmount)>,
}

#[near(serializers=[borsh, json])]
#[derive(Clone, Copy)]
enum FeeAmount {
    Fixed(u32),
    Scheduled {
        start: (u64, u32),
        end: (u64, u32),
        curve: ScheduledFeeCurve,
    },
    Dynamic {
        min: u32,
        max: u32,
    },
}

#[near(serializers=[borsh, json])]
#[derive(Clone, Copy)]
enum ScheduledFeeCurve {
    Linear,
}

#[near(serializers=[borsh, json])]
#[derive(PartialEq, Eq, Hash, Clone, PartialOrd, Ord)]
enum FeeReceiver {
    Account(AccountId),
    Pool,
}

#[near(serializers=[borsh])]
struct CreatePoolResponse {
    pool_id: PoolId,
}

#[near(serializers=[borsh])]
struct GetPoolArgs {
    pool_id: PoolId,
}

#[derive(PartialEq, Debug)]
#[near(serializers=[borsh])]
struct AssetWithBalance {
    asset_id: AssetId,
    balance: U128,
}

#[near(serializers=[borsh])]
enum PoolView {
    Private {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        fee_configuration: FeeConfiguration,
        owner_id: AccountId,
        locked: bool,
    },
    Public {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        fee_configuration: FeeConfiguration,
        total_shares: Option<U128>,
    },
    Launch {
        near_amount: U128,
        launched_asset: AssetWithBalance,
        fees: CurrentFees,
        fee_configuration: FeeConfiguration,
        phantom_liquidity_near: U128,
    },
}

#[near(serializers=[borsh])]
struct WithdrawFeesArgs {
    assets: Vec<AssetId>,
}

async fn get_pool(
    dex_engine_contract: &near_workspaces::Contract,
    dex_id: &DexId,
    pool_id: PoolId,
) -> Option<PoolView> {
    let result = dex_engine_contract
        .view("dex_view")
        .args_json(json!({
            "dex_id": dex_id,
            "method": "get_pool",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&GetPoolArgs { pool_id }).unwrap()),
        }))
        .await
        .unwrap();
    near_sdk::borsh::from_slice(&result.json::<Base64VecU8>().unwrap().0).unwrap()
}

async fn get_pool_shares(
    dex_engine_contract: &near_workspaces::Contract,
    dex_id: &DexId,
    pool_id: PoolId,
    account_id: &AccountId,
) -> Option<U128> {
    let result = dex_engine_contract
        .view("dex_view")
        .args_json(json!({
            "dex_id": dex_id,
            "method": "get_pool_shares",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&(vec![pool_id], account_id.clone())).unwrap()),
        }))
        .await
        .unwrap();
    near_sdk::borsh::from_slice::<Vec<Option<U128>>>(&result.json::<Base64VecU8>().unwrap().0)
        .unwrap()[0]
}

async fn get_ft_balance(token: &near_workspaces::Contract, account_id: &AccountId) -> U128 {
    token
        .view("ft_balance_of")
        .args_json(json!({
            "account_id": account_id,
        }))
        .await
        .unwrap()
        .json::<U128>()
        .unwrap()
}

async fn run_xyk_private_flow(pool_type: PoolType) {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let add_liquidity_ft1 = 1_000_000_000u128;
    let add_liquidity_ft2 = 2_000_000_000u128;
    let swap_amount_ft1 = 100_000_000u128;
    let ft1_initial_deposit = 2_000_000_000u128;
    let ft2_initial_deposit = 3_000_000_000u128;
    let first_pool_id = 0u32; // First pool will have ID 0

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
            code: wasms.xyk_dex_wasm.clone(),
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
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft1_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft2_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V1(CurrentFees {
                                receivers: vec![],
                            }),
                            pool_type,
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_pool.as_yoctonear()),
                    )]),
                },
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "add_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: first_pool_id, min_shares_received: None }).unwrap()),
                    attached_assets: HashMap::from_iter([
                        (AssetId::Nep141(ft1.id().clone()), U128(add_liquidity_ft1)),
                        (AssetId::Nep141(ft2.id().clone()), U128(add_liquidity_ft2)),
                    ]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Private {
            assets, owner_id, ..
        } => {
            assert_eq!(assets.0.balance.0, add_liquidity_ft1);
            assert_eq!(assets.1.balance.0, add_liquidity_ft2);
            assert_eq!(owner_id, deployer.id());
        }
        _ => panic!("Expected private pool"),
    }

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(ft1_initial_deposit),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    // User1 swaps FT1 for FT2 and withdraw FT2
    let result = user1
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(swap_amount_ft1),
            "msg": near_sdk::serde_json::to_string(&vec![
                Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs {
                        pool_id: first_pool_id,
                    }).unwrap()),
                    asset_in: AssetId::Nep141(ft1.id().clone()),
                    asset_out: AssetId::Nep141(ft2.id().clone()),
                    amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(swap_amount_ft1))),
                    constraint: None,
                },
                Operation::Withdraw {
                    asset_id: AssetId::Nep141(ft2.id().clone()),
                    amount: WithdrawAmount::Full { at_least: None },
                    to: None,
                    rescue_address: None,
                },
            ]).unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    // (100,000,000 * 2,000,000,000) / (1,000,000,000 + 100,000,000) = 181,818,181 FT2
    let expected_ft2_out = 181_818_181u128;

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Private { assets, .. } => {
            assert_eq!(assets.0.balance.0, add_liquidity_ft1 + swap_amount_ft1);
            assert_eq!(assets.1.balance.0, add_liquidity_ft2 - expected_ft2_out);
        }
        _ => panic!("Expected private pool"),
    }

    assert_ft_balance(&user1, ft2.clone(), U128(expected_ft2_out))
        .await
        .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(0)),
    )
    .await
    .unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "remove_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                pool_id: first_pool_id,
                shares_to_remove: None,
                min_assets_received: None,
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Private { assets, .. } => {
            assert_eq!(assets.0.balance.0, 0);
            assert_eq!(assets.1.balance.0, 0);
        }
        _ => panic!("Expected private pool"),
    }

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft1_initial_deposit - add_liquidity_ft1)),
    )
    .await
    .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(ft2_initial_deposit - add_liquidity_ft2)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_xyk_private_flow_v1() {
    run_xyk_private_flow(PoolType::PrivateV1).await;
}

#[tokio::test]
async fn test_xyk_private_flow_v2() {
    run_xyk_private_flow(PoolType::PrivateV2).await;
}

async fn run_xyk_public_flow(pool_type: PoolType) {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let add_liquidity_ft1_deployer = 1_000_000_000u128;
    let add_liquidity_ft2_deployer = 2_000_000_000u128;
    let add_liquidity_ft1_user1 = 500_000_000u128;
    let add_liquidity_ft2_user1 = 1_100_000_000u128;
    let added_liquidity_ft2_user1 = 1_000_000_000u128; // 100_000_000 should be refunded
    let swap_amount_ft1 = 100_000_000u128;
    let ft1_initial_deposit = 2_000_000_000u128;
    let ft2_initial_deposit = 4_000_000_000u128;
    let first_pool_id = 0u32;

    let wasms = get_compiled_wasms().await;

    let TestContext {
        dex_engine_contract,
        ft1,
        ft2,
        deployer,
        user1,
        user2,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.xyk_dex_wasm.clone(),
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
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft1_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft2_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V1(CurrentFees {
                                receivers: vec![],
                            }),
                            pool_type,
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_pool.as_yoctonear()),
                    )]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "register_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&RegisterLiquidityArgs { pool_id: first_pool_id }).unwrap()),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(NearToken::from_millinear(10).as_yoctonear()),
                    )]),
                },
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "add_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: first_pool_id, min_shares_received: None }).unwrap()),
                    attached_assets: HashMap::from_iter([
                        (AssetId::Nep141(ft1.id().clone()), U128(add_liquidity_ft1_deployer)),
                        (AssetId::Nep141(ft2.id().clone()), U128(add_liquidity_ft2_deployer)),
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
            "amount": U128(ft1_initial_deposit),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(ft2_initial_deposit),
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
            "amount": U128(add_liquidity_ft1_user1),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(add_liquidity_ft2_user1),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_millinear(50))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "register_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&RegisterLiquidityArgs { pool_id: first_pool_id }).unwrap()),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(NearToken::from_millinear(10).as_yoctonear()),
                    )]),
                },
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "add_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: first_pool_id, min_shares_received: None }).unwrap()),
                    attached_assets: HashMap::from_iter([
                        (AssetId::Nep141(ft1.id().clone()), U128(add_liquidity_ft1_user1)),
                        (AssetId::Nep141(ft2.id().clone()), U128(add_liquidity_ft2_user1)),
                    ]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Public {
            assets,
            total_shares,
            ..
        } => {
            assert_eq!(
                assets.0.balance.0,
                add_liquidity_ft1_deployer + add_liquidity_ft1_user1
            );
            assert_eq!(
                assets.1.balance.0,
                add_liquidity_ft2_deployer + added_liquidity_ft2_user1 - 1
            );
            assert!(total_shares.is_some());
        }
        _ => panic!("Expected public pool"),
    }

    let deployer_shares =
        get_pool_shares(&dex_engine_contract, &dex_id, first_pool_id, deployer.id())
            .await
            .unwrap();
    assert_eq!(deployer_shares.0, 10u128.pow(18));

    let user1_shares = get_pool_shares(&dex_engine_contract, &dex_id, first_pool_id, user1.id())
        .await
        .unwrap();
    assert_eq!(user1_shares.0, 10u128.pow(18) / 2 - 1000000000);

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user2.id(),
            "amount": U128(ft1_initial_deposit),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user2
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(swap_amount_ft1),
            "msg": near_sdk::serde_json::to_string(&vec![
                Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs {
                        pool_id: first_pool_id,
                    }).unwrap()),
                    asset_in: AssetId::Nep141(ft1.id().clone()),
                    asset_out: AssetId::Nep141(ft2.id().clone()),
                    amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(swap_amount_ft1))),
                    constraint: None,
                },
                Operation::Withdraw {
                    asset_id: AssetId::Nep141(ft2.id().clone()),
                    amount: WithdrawAmount::Full { at_least: None },
                    to: None,
                    rescue_address: None,
                },
            ]).unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let expected_ft2_out = 187_500_000u128;
    let pool_ft1_after_swap =
        add_liquidity_ft1_deployer + add_liquidity_ft1_user1 + swap_amount_ft1;
    let pool_ft2_after_swap =
        add_liquidity_ft2_deployer + added_liquidity_ft2_user1 - expected_ft2_out;

    assert_ft_balance(&user2, ft2.clone(), U128(expected_ft2_out - 1))
        .await
        .unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Public {
            assets,
            total_shares,
            ..
        } => {
            assert_eq!(assets.0.balance.0, pool_ft1_after_swap);
            assert_eq!(assets.1.balance.0, pool_ft2_after_swap);
            assert_eq!(
                *total_shares,
                Some(U128(10u128.pow(18) + 10u128.pow(18) / 2 - 1000000000))
            );
        }
        _ => panic!("Expected public pool"),
    }

    let result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "remove_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                pool_id: first_pool_id,
                shares_to_remove: None,
                min_assets_received: None,
            }).unwrap()),
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
            "method": "remove_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                pool_id: first_pool_id,
                shares_to_remove: None,
                min_assets_received: None,
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Public {
            assets,
            total_shares,
            ..
        } => {
            assert_eq!(assets.0.balance.0, 0);
            assert_eq!(assets.1.balance.0, 0);
            assert!(total_shares.is_none());
        }
        _ => panic!("Expected public pool"),
    }

    let deployer_shares =
        get_pool_shares(&dex_engine_contract, &dex_id, first_pool_id, deployer.id()).await;
    assert_eq!(deployer_shares, Some(U128(0)));

    let user1_shares =
        get_pool_shares(&dex_engine_contract, &dex_id, first_pool_id, user1.id()).await;
    assert_eq!(user1_shares, Some(U128(0)));

    let deployer_ft1_expected_received = pool_ft1_after_swap * 2 / 3;
    let deployer_ft2_expected_received = pool_ft2_after_swap * 2 / 3;
    let user1_ft1_expected_received = pool_ft1_after_swap - deployer_ft1_expected_received;
    let user1_ft2_expected_received = pool_ft2_after_swap - deployer_ft2_expected_received;

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft1_initial_deposit - add_liquidity_ft1_deployer)),
    )
    .await
    .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(ft2_initial_deposit - add_liquidity_ft2_deployer)),
    )
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

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(0)),
    )
    .await
    .unwrap();

    let user1_ft2_add_liquidity_refund = add_liquidity_ft2_user1 - added_liquidity_ft2_user1;
    assert_ft_balance(
        &user1,
        ft1.clone(),
        U128((ft1_initial_deposit - add_liquidity_ft1_user1) + user1_ft1_expected_received - 1),
    )
    .await
    .unwrap();
    assert_ft_balance(
        &user1,
        ft2.clone(),
        U128(
            (ft2_initial_deposit - add_liquidity_ft2_user1)
                + user1_ft2_add_liquidity_refund
                + user1_ft2_expected_received,
        ),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_xyk_public_flow_v1() {
    run_xyk_public_flow(PoolType::PublicV1).await;
}

#[tokio::test]
async fn test_xyk_public_flow_v2() {
    run_xyk_public_flow(PoolType::PublicV2).await;
}

#[tokio::test]
async fn test_xyk_upgrade_private_v1() {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let add_liquidity_ft1 = 1_000_000_000u128;
    let add_liquidity_ft2 = 2_000_000_000u128;
    let ft1_initial_deposit = 2_000_000_000u128;
    let ft2_initial_deposit = 3_000_000_000u128;
    let first_pool_id = 0u32;
    let storage_deposit_for_upgrade = NearToken::from_millinear(5);

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
            code: wasms.xyk_dex_wasm.clone(),
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
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft1_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft2_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V1(CurrentFees { receivers: vec![] }),
                            pool_type: PoolType::PrivateV1,
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_pool.as_yoctonear()),
                    )]),
                },
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "add_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs {
                        pool_id: first_pool_id,
                        min_shares_received: None,
                    }).unwrap()),
                    attached_assets: HashMap::from_iter([
                        (AssetId::Nep141(ft1.id().clone()), U128(add_liquidity_ft1)),
                        (AssetId::Nep141(ft2.id().clone()), U128(add_liquidity_ft2)),
                    ]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let upgrade_result = user1
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(storage_deposit_for_upgrade)
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "upgrade_pool".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&UpgradePoolArgs {
                        pool_id: first_pool_id,
                    }).unwrap()),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_upgrade.as_yoctonear()),
                    )]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&upgrade_result).unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Private { assets, locked, .. } => {
            assert_eq!(assets.0.balance.0, add_liquidity_ft1);
            assert_eq!(assets.1.balance.0, add_liquidity_ft2);
            assert!(!locked);
        }
        _ => panic!("Expected private pool"),
    }

    let deployer_ft1_before_remove = get_ft_balance(&ft1, deployer.id()).await.0;
    let deployer_ft2_before_remove = get_ft_balance(&ft2, deployer.id()).await.0;
    let upgrader_ft1_before_remove = get_ft_balance(&ft1, user1.id()).await.0;
    let upgrader_ft2_before_remove = get_ft_balance(&ft2, user1.id()).await.0;

    let remove_result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "remove_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                pool_id: first_pool_id,
                shares_to_remove: None,
                min_assets_received: None,
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&remove_result).unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Private { assets, .. } => {
            assert_eq!(assets.0.balance.0, 0);
            assert_eq!(assets.1.balance.0, 0);
        }
        _ => panic!("Expected private pool"),
    }

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft1_initial_deposit - add_liquidity_ft1)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(ft2_initial_deposit - add_liquidity_ft2)),
    )
    .await
    .unwrap();

    let deployer_ft1_after_remove = get_ft_balance(&ft1, deployer.id()).await.0;
    let deployer_ft2_after_remove = get_ft_balance(&ft2, deployer.id()).await.0;
    assert_eq!(
        deployer_ft1_after_remove,
        deployer_ft1_before_remove + add_liquidity_ft1
    );
    assert_eq!(
        deployer_ft2_after_remove,
        deployer_ft2_before_remove + add_liquidity_ft2
    );

    let upgrader_ft1_after_remove = get_ft_balance(&ft1, user1.id()).await.0;
    let upgrader_ft2_after_remove = get_ft_balance(&ft2, user1.id()).await.0;
    assert_eq!(upgrader_ft1_after_remove, upgrader_ft1_before_remove);
    assert_eq!(upgrader_ft2_after_remove, upgrader_ft2_before_remove);
}

#[tokio::test]
async fn test_xyk_upgrade_public_v1() {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let add_liquidity_ft1_deployer = 1_000_000_000u128;
    let add_liquidity_ft2_deployer = 2_000_000_000u128;
    let ft1_initial_deposit = 2_000_000_000u128;
    let ft2_initial_deposit = 4_000_000_000u128;
    let first_pool_id = 0u32;
    let storage_deposit_for_upgrade = NearToken::from_millinear(5);

    let wasms = get_compiled_wasms().await;

    let TestContext {
        dex_engine_contract,
        ft1,
        ft2,
        deployer,
        user2,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.xyk_dex_wasm.clone(),
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
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft1_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft2_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V1(CurrentFees { receivers: vec![] }),
                            pool_type: PoolType::PublicV1,
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_pool.as_yoctonear()),
                    )]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "register_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&RegisterLiquidityArgs { pool_id: first_pool_id }).unwrap()),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(NearToken::from_millinear(10).as_yoctonear()),
                    )]),
                },
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "add_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: first_pool_id, min_shares_received: None }).unwrap()),
                    attached_assets: HashMap::from_iter([
                        (AssetId::Nep141(ft1.id().clone()), U128(add_liquidity_ft1_deployer)),
                        (AssetId::Nep141(ft2.id().clone()), U128(add_liquidity_ft2_deployer)),
                    ]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let deployer_shares_before =
        get_pool_shares(&dex_engine_contract, &dex_id, first_pool_id, deployer.id())
            .await
            .unwrap();

    let upgrade_result = user2
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(storage_deposit_for_upgrade)
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "upgrade_pool".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&UpgradePoolArgs {
                        pool_id: first_pool_id,
                    }).unwrap()),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_upgrade.as_yoctonear()),
                    )]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&upgrade_result).unwrap();

    let deployer_shares_after =
        get_pool_shares(&dex_engine_contract, &dex_id, first_pool_id, deployer.id())
            .await
            .unwrap();
    assert_eq!(deployer_shares_after, deployer_shares_before);

    let deployer_ft1_before_remove = get_ft_balance(&ft1, deployer.id()).await.0;
    let deployer_ft2_before_remove = get_ft_balance(&ft2, deployer.id()).await.0;
    let upgrader_ft1_before_remove = get_ft_balance(&ft1, user2.id()).await.0;
    let upgrader_ft2_before_remove = get_ft_balance(&ft2, user2.id()).await.0;

    let remove_result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "remove_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                pool_id: first_pool_id,
                shares_to_remove: None,
                min_assets_received: None,
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&remove_result).unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Public { total_shares, .. } => {
            assert!(total_shares.is_none());
        }
        _ => panic!("Expected public pool"),
    }

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft1_initial_deposit - add_liquidity_ft1_deployer)),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(ft2_initial_deposit - add_liquidity_ft2_deployer)),
    )
    .await
    .unwrap();

    let deployer_ft1_after_remove = get_ft_balance(&ft1, deployer.id()).await.0;
    let deployer_ft2_after_remove = get_ft_balance(&ft2, deployer.id()).await.0;
    assert_eq!(
        deployer_ft1_after_remove,
        deployer_ft1_before_remove + add_liquidity_ft1_deployer
    );
    assert_eq!(
        deployer_ft2_after_remove,
        deployer_ft2_before_remove + add_liquidity_ft2_deployer
    );

    let upgrader_ft1_after_remove = get_ft_balance(&ft1, user2.id()).await.0;
    let upgrader_ft2_after_remove = get_ft_balance(&ft2, user2.id()).await.0;
    assert_eq!(upgrader_ft1_after_remove, upgrader_ft1_before_remove);
    assert_eq!(upgrader_ft2_after_remove, upgrader_ft2_before_remove);
}

#[tokio::test]
async fn test_xyk_lock_pool() {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let add_liquidity_ft1 = 1_000_000_000u128;
    let add_liquidity_ft2 = 2_000_000_000u128;
    let ft1_initial_deposit = 2_000_000_000u128;
    let ft2_initial_deposit = 3_000_000_000u128;
    let first_pool_id = 0u32;

    let wasms = get_compiled_wasms().await;

    let TestContext {
        dex_engine_contract,
        ft1,
        ft2,
        deployer,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.xyk_dex_wasm.clone(),
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
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft1_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft2_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V1(CurrentFees { receivers: vec![] }),
                            pool_type: PoolType::PrivateLatest,
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_pool.as_yoctonear()),
                    )]),
                },
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "add_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs {
                        pool_id: first_pool_id,
                        min_shares_received: None,
                    }).unwrap()),
                    attached_assets: HashMap::from_iter([
                        (AssetId::Nep141(ft1.id().clone()), U128(add_liquidity_ft1)),
                        (AssetId::Nep141(ft2.id().clone()), U128(add_liquidity_ft2)),
                    ]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let lock_result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "lock_pool",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&LockPoolArgs {
                pool_id: first_pool_id,
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&lock_result).unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Private { locked, .. } => {
            assert!(*locked);
        }
        _ => panic!("Expected private pool"),
    }

    let remove_result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "remove_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                pool_id: first_pool_id,
                shares_to_remove: None,
                min_assets_received: None,
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert!(!remove_result.is_success());
}

#[tokio::test]
async fn test_xyk_multi_user_liquidity() {
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let first_pool_id = 0u32;

    let user_liquidity = [
        (100_000_000u128, 200_000_000u128),
        (50_000_000u128, 100_000_000u128),
        (200_000_000u128, 400_000_000u128),
        (75_000_000u128, 150_000_000u128),
        (125_000_000u128, 250_000_000u128),
    ];

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
            code: wasms.xyk_dex_wasm.clone(),
            init_method: Some(("new".to_string(), vec![])),
        }),
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
    })
    .await;

    let users = [&user1, &user2, &user3, &user4, &user5];

    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    let result = deployer
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_near(20))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V1(CurrentFees {
                                receivers: vec![],
                            }),
                            pool_type: PoolType::PublicLatest,
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_pool.as_yoctonear()),
                    )]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let mut total_ft1 = 0u128;
    let mut total_ft2 = 0u128;

    for (i, user) in users.iter().enumerate() {
        let (ft1_amount, ft2_amount) = user_liquidity[i];
        total_ft1 += ft1_amount;
        total_ft2 += ft2_amount;

        let result = deployer
            .call(ft1.id(), "ft_transfer")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": user.id(),
                "amount": U128(ft1_amount),
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();

        let result = deployer
            .call(ft2.id(), "ft_transfer")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": user.id(),
                "amount": U128(ft2_amount),
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();

        let result = user
            .call(ft1.id(), "ft_transfer_call")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": dex_engine_contract.id(),
                "amount": U128(ft1_amount),
                "msg": "",
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();

        let result = user
            .call(ft2.id(), "ft_transfer_call")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": dex_engine_contract.id(),
                "amount": U128(ft2_amount),
                "msg": "",
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();

        let result = user
            .call(dex_engine_contract.id(), "deposit_near")
            .max_gas()
            .deposit(NearToken::from_millinear(50))
            .args_json(json!({}))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();

        let result = user
            .call(dex_engine_contract.id(), "execute_operations")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "operations": vec![
                    Operation::DexCall {
                        dex_id: dex_id.clone(),
                        method: "register_liquidity".to_string(),
                        args: Base64VecU8(near_sdk::borsh::to_vec(&RegisterLiquidityArgs { pool_id: first_pool_id }).unwrap()),
                        attached_assets: HashMap::from_iter([(
                            AssetId::Near,
                            U128(NearToken::from_millinear(10).as_yoctonear()),
                        )]),
                    },
                    Operation::DexCall {
                        dex_id: dex_id.clone(),
                        method: "add_liquidity".to_string(),
                        args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: first_pool_id, min_shares_received: None }).unwrap()),
                        attached_assets: HashMap::from_iter([
                            (AssetId::Nep141(ft1.id().clone()), U128(ft1_amount)),
                            (AssetId::Nep141(ft2.id().clone()), U128(ft2_amount)),
                        ]),
                    },
                ],
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();
    }

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Public { assets, .. } => {
            assert_eq!(assets.0.balance.0, total_ft1 - 3);
            assert_eq!(assets.1.balance.0, total_ft2 - 8);
        }
        _ => panic!("Expected public pool"),
    }

    let removal_order = [&user3, &user1, &user5, &user2, &user4];
    for user in removal_order {
        let result = user
            .call(dex_engine_contract.id(), "dex_call")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "dex_id": dex_id.clone(),
                "method": "remove_liquidity",
                "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                    pool_id: first_pool_id,
                    shares_to_remove: None,
                    min_assets_received: None,
                }).unwrap()),
                "attached_assets": {},
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();
    }

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Public {
            assets,
            total_shares,
            ..
        } => {
            assert_eq!(assets.0.balance.0, 0);
            assert_eq!(assets.1.balance.0, 0);
            assert!(total_shares.is_none());
        }
        _ => panic!("Expected public pool"),
    }

    let mut withdrawn_ft1 = 0u128;
    let mut withdrawn_ft2 = 0u128;
    for user in users {
        let ft1_balance = ft1
            .view("ft_balance_of")
            .args_json(json!({
                "account_id": user.id(),
            }))
            .await
            .unwrap()
            .json::<U128>()
            .unwrap();

        let ft2_balance = ft2
            .view("ft_balance_of")
            .args_json(json!({
                "account_id": user.id(),
            }))
            .await
            .unwrap()
            .json::<U128>()
            .unwrap();

        withdrawn_ft1 += ft1_balance.0;
        withdrawn_ft2 += ft2_balance.0;
    }

    assert_eq!(withdrawn_ft1, total_ft1);
    assert_eq!(withdrawn_ft2, total_ft2);
}

#[tokio::test]
async fn test_xyk_fees() {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let add_liquidity_ft1 = 1_000_000_000u128;
    let add_liquidity_ft2 = 2_000_000_000u128;
    let swap_amount_ft1 = 100_000_000u128;
    let first_pool_id = 0u32;

    let fee_fraction = 10_000u32; // 1%
    let protocol_fee_fraction = 1_000u32; // 0.1%

    let wasms = get_compiled_wasms().await;

    let TestContext {
        dex_engine_contract,
        ft1,
        ft2,
        deployer,
        user1,
        user2,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.xyk_dex_wasm.clone(),
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
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(add_liquidity_ft1),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(add_liquidity_ft2),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V1(CurrentFees {
                                receivers: vec![(
                                    FeeReceiver::Account(user2.id().clone()),
                                    fee_fraction,
                                )],
                            }),
                            pool_type: PoolType::PrivateLatest,
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_pool.as_yoctonear()),
                    )]),
                },
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "add_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: first_pool_id, min_shares_received: None }).unwrap()),
                    attached_assets: HashMap::from_iter([
                        (AssetId::Nep141(ft1.id().clone()), U128(add_liquidity_ft1)),
                        (AssetId::Nep141(ft2.id().clone()), U128(add_liquidity_ft2)),
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
            "amount": U128(swap_amount_ft1),
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
            "amount": U128(swap_amount_ft1),
            "msg": near_sdk::serde_json::to_string(&vec![
                Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs {
                        pool_id: first_pool_id,
                    }).unwrap()),
                    asset_in: AssetId::Nep141(ft1.id().clone()),
                    asset_out: AssetId::Nep141(ft2.id().clone()),
                    amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(swap_amount_ft1))),
                    constraint: None,
                },
                Operation::Withdraw {
                    asset_id: AssetId::Nep141(ft2.id().clone()),
                    amount: WithdrawAmount::Full { at_least: None },
                    to: None,
                    rescue_address: None,
                },
            ]).unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    println!("{result:#?}");

    // fee = swap_amount_ft1 * fee_fraction / 1_000_000 = 100_000_000 * 10_000 / 1_000_000 = 1_000_000
    let fee_amount = swap_amount_ft1 * fee_fraction as u128 / 1_000_000;
    let protocol_fee_amount = swap_amount_ft1 * protocol_fee_fraction as u128 / 1_000_000;
    let amount_after_fee = swap_amount_ft1 - fee_amount - protocol_fee_amount;
    // out = (98_900_000 * 2_000_000_000) / (1_000_000_000 + 98_900_000) = 179_998_179
    let expected_ft2_out = 179_998_179;

    assert_ft_balance(&user1, ft2.clone(), U128(expected_ft2_out))
        .await
        .unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Private { assets, fees, .. } => {
            assert_eq!(assets.0.balance.0, add_liquidity_ft1 + amount_after_fee);
            assert_eq!(assets.1.balance.0, add_liquidity_ft2 - expected_ft2_out);
            assert_eq!(fees.receivers.len(), 2);
        }
        _ => panic!("Expected private pool"),
    }

    let result = user2
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "withdraw_fees",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&WithdrawFeesArgs {
                assets: vec![AssetId::Nep141(ft1.id().clone())],
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user2.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(fee_amount)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_xyk_scheduled_fees() {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let add_liquidity_ft1 = 1_000_000_000u128;
    let add_liquidity_ft2 = 2_000_000_000u128;
    let swap_amount_ft1 = 100_000_000u128;
    let first_pool_id = 0u32;

    let scheduled_fee_start = 20_000u32; // 2%
    let scheduled_fee_end = 10_000u32; // 1%
    let protocol_fee_fraction = 1_000u32; // 0.1%

    let wasms = get_compiled_wasms().await;

    let TestContext {
        sandbox,
        dex_engine_contract,
        ft1,
        ft2,
        deployer,
        user1,
        user2,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.xyk_dex_wasm.clone(),
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

    let current_timestamp = sandbox
        .view_block()
        .await
        .unwrap()
        .header()
        .timestamp_nanosec();
    let scheduled_start_timestamp = current_timestamp + 60_000_000_000; // +60s
    let scheduled_end_timestamp = scheduled_start_timestamp + 1_200_000_000_000; // +20m

    let result = deployer
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(add_liquidity_ft1),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(add_liquidity_ft2),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V2(V1FeeConfiguration {
                                receivers: vec![(
                                    FeeReceiver::Account(user2.id().clone()),
                                    FeeAmount::Scheduled {
                                        start: (scheduled_start_timestamp, scheduled_fee_start),
                                        end: (scheduled_end_timestamp, scheduled_fee_end),
                                        curve: ScheduledFeeCurve::Linear,
                                    },
                                )],
                            }),
                            pool_type: PoolType::PrivateV2,
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_pool.as_yoctonear()),
                    )]),
                },
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "add_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: first_pool_id, min_shares_received: None }).unwrap()),
                    attached_assets: HashMap::from_iter([
                        (AssetId::Nep141(ft1.id().clone()), U128(add_liquidity_ft1)),
                        (AssetId::Nep141(ft2.id().clone()), U128(add_liquidity_ft2)),
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
            "amount": U128(swap_amount_ft1),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    sandbox.fast_forward(1000).await.unwrap();
    let timestamp_after_fast_forward = sandbox
        .view_block()
        .await
        .unwrap()
        .header()
        .timestamp_nanosec();
    let elapsed = timestamp_after_fast_forward - scheduled_start_timestamp;
    let duration = scheduled_end_timestamp - scheduled_start_timestamp;
    let fee_range = scheduled_fee_start - scheduled_fee_end;
    let fee_decrease = (fee_range as u128 * elapsed as u128 / duration as u128) as u32;
    let expected_fee_fraction_after_fast_forward = scheduled_fee_start - fee_decrease;

    let result = user1
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(swap_amount_ft1),
            "msg": near_sdk::serde_json::to_string(&vec![
                Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs {
                        pool_id: first_pool_id,
                    }).unwrap()),
                    asset_in: AssetId::Nep141(ft1.id().clone()),
                    asset_out: AssetId::Nep141(ft2.id().clone()),
                    amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(swap_amount_ft1))),
                    constraint: None,
                },
                Operation::Withdraw {
                    asset_id: AssetId::Nep141(ft2.id().clone()),
                    amount: WithdrawAmount::Full { at_least: None },
                    to: None,
                    rescue_address: None,
                },
            ]).unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user2
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "withdraw_fees",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&WithdrawFeesArgs {
                assets: vec![AssetId::Nep141(ft1.id().clone())],
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let user2_fee_balance = dex_engine_contract
        .view("asset_balance_of")
        .args_json(json!({
            "of": AccountOrDexId::Account(user2.id().clone()),
            "asset_id": AssetId::Nep141(ft1.id().clone()),
        }))
        .await
        .unwrap()
        .json::<Option<U128>>()
        .unwrap()
        .unwrap()
        .0;
    let min_fee_fraction = expected_fee_fraction_after_fast_forward as u128 * 95 / 100;
    let max_fee_fraction = expected_fee_fraction_after_fast_forward as u128 * 105 / 100;
    let min_fee_amount = swap_amount_ft1 * min_fee_fraction / 1_000_000;
    let max_fee_amount = swap_amount_ft1 * max_fee_fraction / 1_000_000;
    assert!(
        (min_fee_amount..=max_fee_amount).contains(&user2_fee_balance),
        "Scheduled fee {} out of expected range [{}, {}]",
        user2_fee_balance,
        min_fee_amount,
        max_fee_amount
    );

    let protocol_fee_amount = swap_amount_ft1 * protocol_fee_fraction as u128 / 1_000_000;
    let amount_after_fee = swap_amount_ft1 - user2_fee_balance - protocol_fee_amount;
    let expected_ft2_out =
        amount_after_fee * add_liquidity_ft2 / (add_liquidity_ft1 + amount_after_fee);

    assert_ft_balance(&user1, ft2.clone(), U128(expected_ft2_out))
        .await
        .unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Private { assets, fees, .. } => {
            assert_eq!(assets.0.balance.0, add_liquidity_ft1 + amount_after_fee);
            assert_eq!(assets.1.balance.0, add_liquidity_ft2 - expected_ft2_out);
            assert_eq!(fees.receivers.len(), 2);
        }
        _ => panic!("Expected private pool"),
    }
}

#[tokio::test]
async fn test_xyk_exact_output() {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let add_liquidity_ft1 = 1_000_000_000u128;
    let add_liquidity_ft2 = 2_000_000_000u128;
    let exact_output_ft2 = 100_000_000u128;
    let ft1_initial_deposit = 2_000_000_000u128;
    let ft2_initial_deposit = 3_000_000_000u128;
    let first_pool_id = 0u32;

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
            code: wasms.xyk_dex_wasm.clone(),
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
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft1_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(ft2_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V1(CurrentFees {
                                receivers: vec![],
                            }),
                            pool_type: PoolType::PrivateLatest,
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_pool.as_yoctonear()),
                    )]),
                },
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "add_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: first_pool_id, min_shares_received: None }).unwrap()),
                    attached_assets: HashMap::from_iter([
                        (AssetId::Nep141(ft1.id().clone()), U128(add_liquidity_ft1)),
                        (AssetId::Nep141(ft2.id().clone()), U128(add_liquidity_ft2)),
                    ]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Private {
            assets, owner_id, ..
        } => {
            assert_eq!(assets.0.balance.0, add_liquidity_ft1);
            assert_eq!(assets.1.balance.0, add_liquidity_ft2);
            assert_eq!(owner_id, deployer.id());
        }
        _ => panic!("Expected private pool"),
    }

    let ft1_for_swap = 200_000_000u128;
    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(ft1_for_swap),
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
            "amount": U128(ft1_for_swap),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs {
                        pool_id: first_pool_id,
                    }).unwrap()),
                    asset_in: AssetId::Nep141(ft1.id().clone()),
                    asset_out: AssetId::Nep141(ft2.id().clone()),
                    amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactOut(U128(exact_output_ft2))),
                    constraint: None,
                },
                Operation::Withdraw {
                    asset_id: AssetId::Nep141(ft2.id().clone()),
                    amount: WithdrawAmount::Full { at_least: None },
                    to: None,
                    rescue_address: None,
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    // amount_in = ceil((1_000_000_000 * 100_000_000) / (2_000_000_000 - 100_000_000)) = 52_631_579
    let expected_ft1_in = 52_631_579u128;

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Private { assets, .. } => {
            assert_eq!(assets.0.balance.0, add_liquidity_ft1 + expected_ft1_in);
            assert_eq!(assets.1.balance.0, add_liquidity_ft2 - exact_output_ft2);
        }
        _ => panic!("Expected private pool"),
    }

    assert_ft_balance(&user1, ft2.clone(), U128(exact_output_ft2))
        .await
        .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft1_for_swap - expected_ft1_in)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_xyk_pool_fees() {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let add_liquidity_ft1 = 1_000_000_000u128;
    let add_liquidity_ft2 = 2_000_000_000u128;
    let swap_amount_ft1 = 100_000_000u128;
    let first_pool_id = 0u32;

    let fee_fraction = 10_000u32; // 1%
    let protocol_fee_fraction = 1_000u32; // 0.1%

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
            code: wasms.xyk_dex_wasm.clone(),
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
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(add_liquidity_ft1),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(add_liquidity_ft2),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V1(CurrentFees {
                                receivers: vec![(
                                    FeeReceiver::Pool,
                                    fee_fraction,
                                )],
                            }),
                            pool_type: PoolType::PublicLatest,
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(storage_deposit_for_pool.as_yoctonear()),
                    )]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "register_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&RegisterLiquidityArgs { pool_id: first_pool_id }).unwrap()),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(NearToken::from_millinear(10).as_yoctonear()),
                    )]),
                },
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "add_liquidity".to_string(),
                    args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: first_pool_id, min_shares_received: None }).unwrap()),
                    attached_assets: HashMap::from_iter([
                        (AssetId::Nep141(ft1.id().clone()), U128(add_liquidity_ft1)),
                        (AssetId::Nep141(ft2.id().clone()), U128(add_liquidity_ft2)),
                    ]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let deployer_shares =
        get_pool_shares(&dex_engine_contract, &dex_id, first_pool_id, deployer.id())
            .await
            .unwrap();
    assert_eq!(deployer_shares.0, 10u128.pow(18));

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user1.id(),
            "amount": U128(swap_amount_ft1),
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
            "amount": U128(swap_amount_ft1),
            "msg": near_sdk::serde_json::to_string(&vec![
                Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs {
                        pool_id: first_pool_id,
                    }).unwrap()),
                    asset_in: AssetId::Nep141(ft1.id().clone()),
                    asset_out: AssetId::Nep141(ft2.id().clone()),
                    amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(swap_amount_ft1))),
                    constraint: None,
                },
                Operation::Withdraw {
                    asset_id: AssetId::Nep141(ft2.id().clone()),
                    amount: WithdrawAmount::Full { at_least: None },
                    to: None,
                    rescue_address: None,
                },
            ]).unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    // out = (98_900_000 * 2_000_000_000) / (1_000_000_000 + 98_900_000) = 179_998_179
    let expected_ft2_out = 179_998_179u128;
    let protocol_fee_amount = swap_amount_ft1 * protocol_fee_fraction as u128 / 1_000_000;

    let pool_ft1_after_swap = add_liquidity_ft1 + swap_amount_ft1 - protocol_fee_amount;
    let pool_ft2_after_swap = add_liquidity_ft2 - expected_ft2_out;

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Public { assets, .. } => {
            assert_eq!(assets.0.balance.0, pool_ft1_after_swap);
            assert_eq!(assets.1.balance.0, pool_ft2_after_swap);
        }
        _ => panic!("Expected public pool"),
    }

    assert_ft_balance(&user1, ft2.clone(), U128(expected_ft2_out))
        .await
        .unwrap();

    // Deployer removes liquidity (should get 100% of pool including fees)
    let result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "remove_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                pool_id: first_pool_id,
                shares_to_remove: None,
                min_assets_received: None,
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Public {
            assets,
            total_shares,
            ..
        } => {
            assert_eq!(assets.0.balance.0, 0);
            assert_eq!(assets.1.balance.0, 0);
            assert!(total_shares.is_none());
        }
        _ => panic!("Expected public pool"),
    }

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(0)),
    )
    .await
    .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(0)),
    )
    .await
    .unwrap();

    assert_eq!(
        pool_ft1_after_swap,
        add_liquidity_ft1 + swap_amount_ft1 - protocol_fee_amount
    );
}

#[tokio::test]
async fn test_xyk_launch_pool_flow() {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let launch_liquidity_ft2 = 2_000_000_000u128;
    let phantom_liquidity_near = 1_000_000_000u128;
    let near_swap_in = 100_000_000u128;
    let first_pool_id = 0u32;

    let wasms = get_compiled_wasms().await;

    let TestContext {
        dex_engine_contract,
        ft2,
        deployer,
        user1,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.xyk_dex_wasm.clone(),
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
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(launch_liquidity_ft2),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Near, AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V1(CurrentFees {
                                receivers: vec![],
                            }),
                            pool_type: PoolType::LaunchLatest {
                                phantom_liquidity_near: U128(phantom_liquidity_near),
                            },
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([
                        (
                            AssetId::Near,
                            U128(storage_deposit_for_pool.as_yoctonear()),
                        ),
                        (
                            AssetId::Nep141(ft2.id().clone()),
                            U128(launch_liquidity_ft2),
                        ),
                    ]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Launch {
            near_amount,
            launched_asset,
            phantom_liquidity_near: pool_phantom_liquidity_near,
            ..
        } => {
            assert_eq!(near_amount.0, phantom_liquidity_near);
            assert_eq!(launched_asset.asset_id, AssetId::Nep141(ft2.id().clone()));
            assert_eq!(launched_asset.balance.0, launch_liquidity_ft2);
            assert_eq!(pool_phantom_liquidity_near.0, phantom_liquidity_near);
        }
        _ => panic!("Expected launch pool"),
    }

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_yoctonear(near_swap_in))
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user1
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs {
                        pool_id: first_pool_id,
                    }).unwrap()),
                    asset_in: AssetId::Near,
                    asset_out: AssetId::Nep141(ft2.id().clone()),
                    amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(near_swap_in))),
                    constraint: None,
                },
                Operation::Withdraw {
                    asset_id: AssetId::Nep141(ft2.id().clone()),
                    amount: WithdrawAmount::Full { at_least: None },
                    to: None,
                    rescue_address: None,
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let expected_ft2_out =
        near_swap_in * launch_liquidity_ft2 / (phantom_liquidity_near + near_swap_in);
    assert_ft_balance(&user1, ft2.clone(), U128(expected_ft2_out))
        .await
        .unwrap();

    let pool = get_pool(&dex_engine_contract, &dex_id, first_pool_id)
        .await
        .unwrap();
    match &pool {
        PoolView::Launch {
            near_amount,
            launched_asset,
            ..
        } => {
            assert_eq!(near_amount.0, phantom_liquidity_near + near_swap_in);
            assert_eq!(
                launched_asset.balance.0,
                launch_liquidity_ft2 - expected_ft2_out
            );
        }
        _ => panic!("Expected launch pool"),
    }
}

#[tokio::test]
async fn test_xyk_launch_pool_restrictions() {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_pool = NearToken::from_millinear(50);
    let launch_liquidity_ft2 = 2_000_000_000u128;
    let phantom_liquidity_near = 1_000_000_000u128;
    let ft2_for_failed_swap = 100_000_000u128;
    let first_pool_id = 0u32;

    let wasms = get_compiled_wasms().await;

    let TestContext {
        dex_engine_contract,
        ft2,
        deployer,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.xyk_dex_wasm.clone(),
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
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft2.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(launch_liquidity_ft2 + ft2_for_failed_swap),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::DexCall {
                    dex_id: dex_id.clone(),
                    method: "create_pool".to_string(),
                    args: Base64VecU8(
                        near_sdk::borsh::to_vec(&CreatePoolArgs {
                            assets: (AssetId::Near, AssetId::Nep141(ft2.id().clone())),
                            fees: FeeConfiguration::V1(CurrentFees {
                                receivers: vec![],
                            }),
                            pool_type: PoolType::LaunchLatest {
                                phantom_liquidity_near: U128(phantom_liquidity_near),
                            },
                        })
                        .unwrap(),
                    ),
                    attached_assets: HashMap::from_iter([
                        (
                            AssetId::Near,
                            U128(storage_deposit_for_pool.as_yoctonear()),
                        ),
                        (
                            AssetId::Nep141(ft2.id().clone()),
                            U128(launch_liquidity_ft2),
                        ),
                    ]),
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let add_liquidity_result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "add_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&AddLiquidityArgs {
                pool_id: first_pool_id,
                min_shares_received: None,
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert!(!add_liquidity_result.is_success());

    let remove_liquidity_result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "remove_liquidity",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                pool_id: first_pool_id,
                shares_to_remove: None,
                min_assets_received: None,
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert!(!remove_liquidity_result.is_success());

    let swap_out_near_result = deployer
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": vec![
                Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs {
                        pool_id: first_pool_id,
                    }).unwrap()),
                    asset_in: AssetId::Nep141(ft2.id().clone()),
                    asset_out: AssetId::Near,
                    amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(ft2_for_failed_swap))),
                    constraint: None,
                },
            ],
        }))
        .transact()
        .await
        .unwrap();
    assert!(!swap_out_near_result.is_success());
}
