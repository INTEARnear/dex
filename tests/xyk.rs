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
    is_public: bool,
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
struct SwapArgs {
    pool_id: PoolId,
}

#[near(serializers=[borsh, json])]
struct FeeConfiguration {
    receivers: HashMap<FeeReceiver, u32>,
}

#[near(serializers=[borsh, json])]
#[derive(PartialEq, Eq, Hash, Clone, PartialOrd, Ord)]
enum FeeReceiver {
    User(AccountId),
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
        fees: FeeConfiguration,
        owner_id: AccountId,
    },
    Public {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: FeeConfiguration,
        total_shares: Option<SharesBalance>,
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

#[tokio::test]
async fn test_xyk_private_flow() {
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
                            fees: FeeConfiguration {
                                receivers: HashMap::new(),
                            },
                            is_public: false,
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
        PoolView::Public { .. } => panic!("Expected private pool"),
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
        PoolView::Public { .. } => panic!("Expected private pool"),
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
        PoolView::Public { .. } => panic!("Expected private pool"),
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
async fn test_xyk_public_flow() {
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
                            fees: FeeConfiguration {
                                receivers: HashMap::new(),
                            },
                            is_public: true,
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
        PoolView::Private { .. } => panic!("Expected public pool"),
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
        PoolView::Private { .. } => panic!("Expected public pool"),
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
        PoolView::Private { .. } => panic!("Expected public pool"),
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
        // +1 due to rounding in add_liquidity
        Some(U128(
            add_liquidity_ft2_user1 - added_liquidity_ft2_user1 + 1,
        )),
    )
    .await
    .unwrap();

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
        U128((ft2_initial_deposit - add_liquidity_ft2_user1) + user1_ft2_expected_received - 1),
    )
    .await
    .unwrap();
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
                            fees: FeeConfiguration {
                                receivers: HashMap::new(),
                            },
                            is_public: true,
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
        PoolView::Private { .. } => panic!("Expected public pool"),
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
        PoolView::Private { .. } => panic!("Expected public pool"),
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

    // I guess rounding errors?
    assert_eq!(withdrawn_ft1, total_ft1 - 3);
    assert_eq!(withdrawn_ft2, total_ft2 - 8);
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
                            fees: FeeConfiguration {
                                receivers: HashMap::from_iter([(
                                    FeeReceiver::User(user2.id().clone()),
                                    fee_fraction,
                                )]),
                            },
                            is_public: false,
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
        PoolView::Public { .. } => panic!("Expected private pool"),
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
                            fees: FeeConfiguration {
                                receivers: HashMap::new(),
                            },
                            is_public: false,
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
        PoolView::Public { .. } => panic!("Expected private pool"),
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
        PoolView::Public { .. } => panic!("Expected private pool"),
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
                            fees: FeeConfiguration {
                                receivers: HashMap::from_iter([(
                                    FeeReceiver::Pool,
                                    fee_fraction,
                                )]),
                            },
                            is_public: true,
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
        PoolView::Private { .. } => panic!("Expected public pool"),
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
        PoolView::Private { .. } => panic!("Expected public pool"),
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
