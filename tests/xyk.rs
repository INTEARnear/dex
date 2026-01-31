mod common;
use common::*;

use intear_dex::internal_asset_operations::AccountOrDexId;
use intear_dex::internal_operations::{Operation, SwapOperationAmount};
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

#[near(serializers=[borsh])]
struct RegisterLiquidityArgs {
    pool_id: u64,
}

#[near(serializers=[borsh])]
struct AddLiquidityArgs {
    pool_id: u64,
}

#[near(serializers=[borsh])]
struct RemoveLiquidityArgs {
    pool_id: u64,
    shares_to_remove: Option<std::num::NonZeroU128>,
}

#[near(serializers=[borsh])]
struct SwapArgs {
    pool_id: u64,
}

#[near(serializers=[borsh, json])]
struct FeeConfiguration {
    receivers: HashMap<FeeReceiver, u32>,
}

#[near(serializers=[borsh, json])]
#[derive(PartialEq, Eq, Hash, Clone, PartialOrd, Ord)]
enum FeeReceiver {
    User(AccountId),
}

#[near(serializers=[borsh])]
struct CreatePoolResponse {
    pool_id: u64,
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
    let first_pool_id = 0u64; // First pool will have ID 0

    let TestContext {
        dex_engine_contract,
        ft1,
        ft2,
        deployer,
        user1,
        ..
    } = setup_test_environment().await;
    let wasms = get_compiled_wasms().await;
    let dex_wasm = &wasms.xyk_dex_wasm;

    let dex_id_string = "xyk".to_string();
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
        .deposit(engine_user_storage_deposit())
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
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())],
            "for": AccountOrDexId::Dex(dex_id.clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "asset_ids": [AssetId::Near, AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())],
            "for": AccountOrDexId::Account(deployer.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    ft_storage_deposit_for(&ft1, ft1.as_account(), dex_engine_contract.id()).await;
    ft_storage_deposit_for(&ft2, ft2.as_account(), dex_engine_contract.id()).await;

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
                    method: "new".to_string(),
                    args: Base64VecU8(vec![]),
                    attached_assets: HashMap::from_iter([(
                        AssetId::Near,
                        U128(NearToken::from_yoctonear(1).as_yoctonear()),
                    )]),
                },
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
                    args: Base64VecU8(near_sdk::borsh::to_vec(&AddLiquidityArgs { pool_id: first_pool_id }).unwrap()),
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

    let result = user1
        .call(dex_engine_contract.id(), "storage_deposit")
        .max_gas()
        .deposit(engine_user_storage_deposit())
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
            "asset_ids": [AssetId::Nep141(ft1.id().clone()), AssetId::Nep141(ft2.id().clone())],
            "for": AccountOrDexId::Account(user1.id().clone()),
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    ft_storage_deposit(&ft1, &user1).await;
    ft_storage_deposit(&ft2, &user1).await;
    ft_storage_deposit_for(&ft1, &deployer, dex_engine_contract.id()).await;
    ft_storage_deposit_for(&ft2, &deployer, dex_engine_contract.id()).await;

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
                },
                Operation::Withdraw {
                    asset_id: AssetId::Nep141(ft2.id().clone()),
                    amount: None,
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
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft1_initial_deposit + swap_amount_ft1)),
    )
    .await
    .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(ft2_initial_deposit - expected_ft2_out)),
    )
    .await
    .unwrap();
}
