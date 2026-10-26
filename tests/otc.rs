mod common;
use common::*;

use intear_dex_types::{AccountOrDexId, AssetId, DexId, Operation};
use near_crypto::{KeyType, SecretKey, Signature};
use near_sdk::AccountId;
use near_sdk::serde_json::json;
use near_sdk::{
    NearToken,
    base64::{Engine, prelude::BASE64_STANDARD},
    json_types::{Base64VecU8, U128},
};
use near_workspaces::{Account, Contract};
use otc_dex_types::{
    AuthorizationMethod, AuthorizedTradeIntent, DepositAssetsArgs, ExpiryCondition, GetBalanceArgs,
    MatchArgs, OutputDestination, RegisterAssetsArgs, SetAuthorizedKeyArgs, StorageDepositArgs,
    TradeIntent, Validity,
};
use std::collections::HashMap;

/// Assert the OTC inner balance of asset of an account.
async fn assert_otc_inner_asset_balance(
    dex_engine_contract: &Contract,
    dex_id: &DexId,
    user_id: &AccountId,
    asset: AssetId,
    amount: Option<U128>,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = dex_engine_contract
        .view("dex_view")
        .args_json(json!({
            "dex_id": dex_id,
            "method": "get_balance",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&GetBalanceArgs {
                account_id: user_id.clone(),
                asset_id: asset.clone(),
            }).unwrap()),
        }))
        .await?;

    let balance = near_sdk::borsh::from_slice::<Option<U128>>(&result.json::<Base64VecU8>()?.0)?;

    if balance != amount {
        return Err(format!(
            "OTC inner asset balance mismatch for {asset}: expected {amount:?}, actual {balance:?}"
        )
        .into());
    }
    Ok(())
}

/// Register OTC inner balances of assets for an account, so that trades can credit them.
async fn otc_register_assets(
    dex_engine_contract: &Contract,
    dex_id: &DexId,
    account: &Account,
    asset_ids: Vec<AssetId>,
) {
    let result = account
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id,
            "method": "register_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&RegisterAssetsArgs { asset_ids }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
}

#[tokio::test]
async fn test_otc_regular_flow() {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_otc = NearToken::from_millinear(10);
    let assets_deposit_to_otc_near = NearToken::from_near(5);
    let assets_deposit_to_otc_ft = 500_000_000u128;
    let trade_amount_near = NearToken::from_near(2);
    let trade_amount_ft = 100_000_000u128;
    let ft_initial_deposit = 1_000_000_000u128;

    let wasms = get_compiled_wasms().await;

    let TestContext {
        sandbox,
        dex_engine_contract,
        ft1,
        deployer,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.otc_dex_wasm.clone(),
            init_method: None,
        }),
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
    })
    .await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    let initial_near_balance = near_balance_after_refunds(&sandbox, &deployer).await;
    let mut total_near_burnt = NearToken::from_yoctonear(0);

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

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(initial_near_deposit.as_yoctonear())),
    )
    .await
    .unwrap();

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
            "amount": U128(ft_initial_deposit),
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
        U128(initial_ft_balance.0 - ft_initial_deposit),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_initial_deposit)),
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
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "storage_deposit",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap()),
            "attached_assets": {
                "near": U128(storage_deposit_for_otc.as_yoctonear()),
            },
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert!(result.json::<Base64VecU8>().unwrap().0.is_empty());
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(
            initial_near_deposit
                .saturating_sub(storage_deposit_for_otc)
                .as_yoctonear(),
        )),
    )
    .await
    .unwrap();

    let result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "deposit_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            "attached_assets": {
                "near": U128(assets_deposit_to_otc_near.as_yoctonear()),
                format!("nep141:{}", ft1.id()): U128(assets_deposit_to_otc_ft),
            },
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert!(result.json::<Base64VecU8>().unwrap().0.is_empty());
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(
            initial_near_deposit
                .saturating_sub(storage_deposit_for_otc)
                .saturating_sub(assets_deposit_to_otc_near)
                .as_yoctonear(),
        )),
    )
    .await
    .unwrap();
    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_initial_deposit - assets_deposit_to_otc_ft)),
    )
    .await
    .unwrap();

    let deployer_key = SecretKey::from_random(KeyType::ED25519);
    let result = deployer
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "set_authorized_key",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
                key: deployer_key.public_key().to_string().parse().unwrap(),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    track_tokens_burnt(&result, &mut total_near_burnt);
    assert!(result.json::<Base64VecU8>().unwrap().0.is_empty());

    let user2 = sandbox.dev_create_account().await.unwrap();
    ft_storage_deposit(&ft1, &user2).await;

    // User 1's trade intent: sell 100_000_000 FT for 2 NEAR
    let user1_trade_intent = TradeIntent {
        user_id: deployer.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: U128(trade_amount_ft),
        amount_out: U128(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user1_trade_intent).unwrap();
    let user1_signature: Base64VecU8 = Base64VecU8(
        match deployer_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user1_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent,
        authorization_method: AuthorizationMethod::Signature(user1_signature),
    };

    // User 2's trade intent: sell 2 NEAR for 100_000_000 FT
    let user2_trade_intent = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Near,
        asset_out: AssetId::Nep141(ft1.id().clone()),
        amount_in: U128(trade_amount_near.as_yoctonear()),
        amount_out: U128(trade_amount_ft),
        validity: Validity::default(),
    };

    let user2_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent,
        authorization_method: AuthorizationMethod::Predecessor,
    };

    let operations = vec![Operation::DexCall {
        dex_id: dex_id.clone(),
        method: "match".to_string(),
        args: Base64VecU8(
            near_sdk::borsh::to_vec(&MatchArgs {
                authorized_trade_intents: vec![user1_authorized_intent, user2_authorized_intent],
                output_destination: OutputDestination::WithdrawToUser,
            })
            .unwrap(),
        ),
        attached_assets: HashMap::from_iter([(
            AssetId::Near,
            U128(trade_amount_near.as_yoctonear()),
        )]),
    }];

    let intent_with_nonce = AuthorizedTradeIntent {
        trade_intent: TradeIntent {
            user_id: user2.id().clone(),
            asset_in: AssetId::Near,
            asset_out: AssetId::Near,
            amount_in: U128(0),
            amount_out: U128(0),
            validity: Validity {
                nonce: Some(U128(1)),
                ..Default::default()
            },
        },
        authorization_method: AuthorizationMethod::Predecessor,
    };
    let result = user2
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": [Operation::DexCall {
                dex_id: dex_id.clone(),
                method: "match".to_string(),
                args: Base64VecU8(
                    near_sdk::borsh::to_vec(&MatchArgs {
                        authorized_trade_intents: vec![intent_with_nonce],
                        output_destination: OutputDestination::WithdrawToUser,
                    })
                    .unwrap(),
                ),
                attached_assets: HashMap::from_iter([(AssetId::Near, U128(0))]),
            }],
        }))
        .transact()
        .await
        .unwrap();
    assert!(
        format!("{:?}", result.failures())
            .contains("AuthorizationMethod::Predecessor can't use a nonce in a transfer call")
    );

    let result = user2
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(trade_amount_near)
        .args_json(json!({
            "operations": operations,
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    assert_ft_balance(&user2, ft1.clone(), U128(trade_amount_ft))
        .await
        .unwrap();

    assert_near_balance(
        &deployer,
        initial_near_balance
            .saturating_sub(initial_near_deposit)
            .saturating_sub(total_near_burnt)
            .saturating_sub(NearToken::from_yoctonear(4)),
    )
    .await
    .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Near,
        Some(U128(
            initial_near_deposit
                .saturating_sub(assets_deposit_to_otc_near)
                .saturating_sub(storage_deposit_for_otc)
                .as_yoctonear(),
        )),
    )
    .await
    .unwrap();

    assert_inner_asset_balance(
        &dex_engine_contract,
        AccountOrDexId::Account(deployer.id().clone()),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(ft_initial_deposit - assets_deposit_to_otc_ft)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_otc_relayed_by_third_party() {
    let initial_near_deposit_user1 = NearToken::from_near(20);
    let storage_deposit_for_otc = NearToken::from_millinear(10);
    let assets_deposit_to_otc_near_user1 = NearToken::from_near(5);
    let assets_deposit_to_otc_ft_user2 = 500_000_000u128;
    let trade_amount_near = NearToken::from_near(2);
    let trade_amount_ft = 100_000_000u128;
    let ft_initial_deposit = 1_000_000_000u128;

    let wasms = get_compiled_wasms().await;

    let TestContext {
        dex_engine_contract,
        ft1,
        deployer,
        user1,
        user1_key,
        user2,
        user2_key,
        user3,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.otc_dex_wasm.clone(),
            init_method: None,
        }),
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
    })
    .await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit_user1)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user2.id(),
            "amount": U128(ft_initial_deposit),
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
            "amount": U128(ft_initial_deposit),
            "msg": "",
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
            "method": "storage_deposit",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap()),
            "attached_assets": {
                "near": U128(storage_deposit_for_otc.as_yoctonear()),
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
            "method": "deposit_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            "attached_assets": {
                "near": U128(assets_deposit_to_otc_near_user1.as_yoctonear()),
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
            "method": "set_authorized_key",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
                key: user1_key.public_key().to_string().parse().unwrap(),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user2
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(storage_deposit_for_otc)
        .args_json(json!({
            "operations": [Operation::DexCall {
                dex_id: dex_id.clone(),
                method: "storage_deposit".to_string(),
                args: Base64VecU8(near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap()),
                attached_assets: HashMap::from_iter([(AssetId::Near, U128(storage_deposit_for_otc.as_yoctonear()))]),
            }],
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
            "method": "deposit_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            "attached_assets": {
                format!("nep141:{}", ft1.id()): U128(assets_deposit_to_otc_ft_user2),
            },
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
            "method": "set_authorized_key",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
                key: user2_key.public_key().to_string().parse().unwrap(),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    otc_register_assets(
        &dex_engine_contract,
        &dex_id,
        &user1,
        vec![AssetId::Nep141(ft1.id().clone())],
    )
    .await;
    otc_register_assets(&dex_engine_contract, &dex_id, &user2, vec![AssetId::Near]).await;

    let user1_trade_intent = TradeIntent {
        user_id: user1.id().clone(),
        asset_in: AssetId::Near,
        asset_out: AssetId::Nep141(ft1.id().clone()),
        amount_in: U128(trade_amount_near.as_yoctonear()),
        amount_out: U128(trade_amount_ft),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user1_trade_intent).unwrap();
    let user1_signature: Base64VecU8 = Base64VecU8(
        match user1_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user1_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent,
        authorization_method: AuthorizationMethod::Signature(user1_signature),
    };

    // User 2's trade intent: sell 100_000_000 FT for 2 NEAR
    let user2_trade_intent = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: U128(trade_amount_ft),
        amount_out: U128(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user2_trade_intent).unwrap();
    let user2_signature: Base64VecU8 = Base64VecU8(
        match user2_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user2_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent,
        authorization_method: AuthorizationMethod::Signature(user2_signature),
    };

    let operations = vec![Operation::DexCall {
        dex_id: dex_id.clone(),
        method: "match".to_string(),
        args: Base64VecU8(
            near_sdk::borsh::to_vec(&MatchArgs {
                authorized_trade_intents: vec![user1_authorized_intent, user2_authorized_intent],
                output_destination: OutputDestination::InternalOtcBalance,
            })
            .unwrap(),
        ),
        attached_assets: HashMap::new(),
    }];

    let result = user3
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

    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        user1.id(),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(trade_amount_ft)),
    )
    .await
    .unwrap();

    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        user1.id(),
        AssetId::Near,
        Some(U128(
            assets_deposit_to_otc_near_user1
                .saturating_sub(trade_amount_near)
                .as_yoctonear(),
        )),
    )
    .await
    .unwrap();

    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        user2.id(),
        AssetId::Near,
        Some(U128(trade_amount_near.as_yoctonear())),
    )
    .await
    .unwrap();

    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        user2.id(),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(assets_deposit_to_otc_ft_user2 - trade_amount_ft)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_otc_single_intent_from_one_account_fails() {
    let initial_near_deposit_user1 = NearToken::from_near(20);
    let storage_deposit_for_otc = NearToken::from_millinear(10);
    let assets_deposit_to_otc_near_user1 = NearToken::from_near(5);
    let trade_amount_near = NearToken::from_near(2);
    let trade_amount_ft = 100_000_000u128;

    let wasms = get_compiled_wasms().await;

    let TestContext {
        dex_engine_contract,
        ft1,
        user1,
        deployer,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.otc_dex_wasm.clone(),
            init_method: None,
        }),
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
    })
    .await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit_user1)
        .args_json(json!({}))
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
            "method": "storage_deposit",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap()),
            "attached_assets": {
                "near": U128(storage_deposit_for_otc.as_yoctonear()),
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
            "method": "deposit_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            "attached_assets": {
                "near": U128(assets_deposit_to_otc_near_user1.as_yoctonear()),
            },
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let user1_trade_intent = TradeIntent {
        user_id: user1.id().clone(),
        asset_in: AssetId::Near,
        asset_out: AssetId::Nep141(ft1.id().clone()),
        amount_in: U128(trade_amount_near.as_yoctonear()),
        amount_out: U128(trade_amount_ft),
        validity: Validity::default(),
    };

    let user1_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent,
        authorization_method: AuthorizationMethod::Predecessor,
    };

    let operations = vec![Operation::DexCall {
        dex_id: dex_id.clone(),
        method: "match".to_string(),
        args: Base64VecU8(
            near_sdk::borsh::to_vec(&MatchArgs {
                authorized_trade_intents: vec![user1_authorized_intent],
                output_destination: OutputDestination::InternalOtcBalance,
            })
            .unwrap(),
        ),
        attached_assets: HashMap::new(),
    }];

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
    assert!(format!("{:?}", result.failures()).contains("net sum is not zero"));
}

#[tokio::test]
async fn test_otc_mismatching_intents_fail() {
    let initial_near_deposit_user1 = NearToken::from_near(20);
    let storage_deposit_for_otc = NearToken::from_millinear(10);
    let assets_deposit_to_otc_near_user1 = NearToken::from_near(5);
    let assets_deposit_to_otc_ft_user2 = 500_000_000u128;
    let trade_amount_near = NearToken::from_near(2);
    let trade_amount_ft = 100_000_000u128;
    let mismatched_ft_amount = 200_000_000u128;
    let ft_initial_deposit = 1_000_000_000u128;

    let wasms = get_compiled_wasms().await;

    let TestContext {
        dex_engine_contract,
        ft1,
        deployer,
        user1,
        user1_key,
        user2,
        user2_key,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.otc_dex_wasm.clone(),
            init_method: None,
        }),
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
    })
    .await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit_user1)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user2.id(),
            "amount": U128(ft_initial_deposit),
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
            "amount": U128(ft_initial_deposit),
            "msg": "",
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
            "method": "storage_deposit",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap()),
            "attached_assets": {
                "near": U128(storage_deposit_for_otc.as_yoctonear()),
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
            "method": "deposit_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            "attached_assets": {
                "near": U128(assets_deposit_to_otc_near_user1.as_yoctonear()),
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
            "method": "set_authorized_key",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
                key: user1_key.public_key().to_string().parse().unwrap(),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user2
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(storage_deposit_for_otc)
        .args_json(json!({
            "operations": [Operation::DexCall {
                dex_id: dex_id.clone(),
                method: "storage_deposit".to_string(),
                args: Base64VecU8(near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap()),
                attached_assets: HashMap::from_iter([(AssetId::Near, U128(storage_deposit_for_otc.as_yoctonear()))]),
            }],
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
            "method": "deposit_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            "attached_assets": {
                format!("nep141:{}", ft1.id()): U128(assets_deposit_to_otc_ft_user2),
            },
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
            "method": "set_authorized_key",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
                key: user2_key.public_key().to_string().parse().unwrap(),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let user1_trade_intent = TradeIntent {
        user_id: user1.id().clone(),
        asset_in: AssetId::Near,
        asset_out: AssetId::Nep141(ft1.id().clone()),
        amount_in: U128(trade_amount_near.as_yoctonear()),
        amount_out: U128(trade_amount_ft),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user1_trade_intent).unwrap();
    let user1_signature: Base64VecU8 = Base64VecU8(
        match user1_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user1_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent,
        authorization_method: AuthorizationMethod::Signature(user1_signature),
    };

    // User 2's trade intent: sell mismatched FT amount for 2 NEAR
    let user2_trade_intent = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: U128(mismatched_ft_amount),
        amount_out: U128(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user2_trade_intent).unwrap();
    let user2_signature: Base64VecU8 = Base64VecU8(
        match user2_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user2_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent,
        authorization_method: AuthorizationMethod::Signature(user2_signature),
    };

    let operations = vec![Operation::DexCall {
        dex_id: dex_id.clone(),
        method: "match".to_string(),
        args: Base64VecU8(
            near_sdk::borsh::to_vec(&MatchArgs {
                authorized_trade_intents: vec![user1_authorized_intent, user2_authorized_intent],
                output_destination: OutputDestination::InternalOtcBalance,
            })
            .unwrap(),
        ),
        attached_assets: HashMap::new(),
    }];

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
    assert!(format!("{:?}", result.failures()).contains("net sum is not zero"));
}

#[tokio::test]
async fn test_otc_three_intents_with_same_value() {
    let initial_near_deposit_user1 = NearToken::from_near(5);
    let storage_deposit_for_otc = NearToken::from_millinear(10);
    let assets_deposit_to_otc_near_user1 = NearToken::from_near(2);
    let assets_deposit_to_otc_ft1_user2 = 500_000u128;
    let assets_deposit_to_otc_ft2_user3 = 50_000u128;
    let trade_amount_near = NearToken::from_near(1);
    let trade_amount_ft1 = 100_000u128;
    let trade_amount_ft2 = 5_000u128;
    let ft1_initial_deposit = 1_000_000u128;
    let ft2_initial_deposit = 100_000u128;

    let wasms = get_compiled_wasms().await;

    let TestContext {
        dex_engine_contract,
        ft1,
        ft2,
        deployer,
        user1,
        user1_key,
        user2,
        user2_key,
        user3,
        user3_key,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.otc_dex_wasm.clone(),
            init_method: None,
        }),
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
    })
    .await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit_user1)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

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

    let result = deployer
        .call(ft2.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user3.id(),
            "amount": U128(ft2_initial_deposit),
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
            "amount": U128(ft1_initial_deposit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user3
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

    let result = user1
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "storage_deposit",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap()),
            "attached_assets": {
                "near": U128(storage_deposit_for_otc.as_yoctonear()),
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
            "method": "deposit_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            "attached_assets": {
                "near": U128(assets_deposit_to_otc_near_user1.as_yoctonear()),
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
            "method": "set_authorized_key",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
                key: user1_key.public_key().to_string().parse().unwrap(),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user2
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(storage_deposit_for_otc)
        .args_json(json!({
            "operations": [Operation::DexCall {
                dex_id: dex_id.clone(),
                method: "storage_deposit".to_string(),
                args: Base64VecU8(near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap()),
                attached_assets: HashMap::from_iter([(AssetId::Near, U128(storage_deposit_for_otc.as_yoctonear()))]),
            }],
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
            "method": "deposit_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            "attached_assets": {
                format!("nep141:{}", ft1.id()): U128(assets_deposit_to_otc_ft1_user2),
            },
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
            "method": "set_authorized_key",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
                key: user2_key.public_key().to_string().parse().unwrap(),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user3
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(storage_deposit_for_otc)
        .args_json(json!({
            "operations": [Operation::DexCall {
                dex_id: dex_id.clone(),
                method: "storage_deposit".to_string(),
                args: Base64VecU8(near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap()),
                attached_assets: HashMap::from_iter([(AssetId::Near, U128(storage_deposit_for_otc.as_yoctonear()))]),
            }],
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user3
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "deposit_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            "attached_assets": {
                format!("nep141:{}", ft2.id()): U128(assets_deposit_to_otc_ft2_user3),
            },
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user3
        .call(dex_engine_contract.id(), "dex_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "dex_id": dex_id.clone(),
            "method": "set_authorized_key",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
                key: user3_key.public_key().to_string().parse().unwrap(),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    otc_register_assets(
        &dex_engine_contract,
        &dex_id,
        &user1,
        vec![AssetId::Nep141(ft1.id().clone())],
    )
    .await;
    otc_register_assets(
        &dex_engine_contract,
        &dex_id,
        &user2,
        vec![AssetId::Nep141(ft2.id().clone())],
    )
    .await;
    otc_register_assets(&dex_engine_contract, &dex_id, &user3, vec![AssetId::Near]).await;

    // User 1's trade intent: sell 1 NEAR for 100_000 FT1
    let user1_trade_intent = TradeIntent {
        user_id: user1.id().clone(),
        asset_in: AssetId::Near,
        asset_out: AssetId::Nep141(ft1.id().clone()),
        amount_in: U128(trade_amount_near.as_yoctonear()),
        amount_out: U128(trade_amount_ft1),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user1_trade_intent).unwrap();
    let user1_signature: Base64VecU8 = Base64VecU8(
        match user1_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user1_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent,
        authorization_method: AuthorizationMethod::Signature(user1_signature),
    };

    // User 2's trade intent: sell 100_000 FT1 for 5_000 FT2
    let user2_trade_intent = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Nep141(ft2.id().clone()),
        amount_in: U128(trade_amount_ft1),
        amount_out: U128(trade_amount_ft2),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user2_trade_intent).unwrap();
    let user2_signature: Base64VecU8 = Base64VecU8(
        match user2_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user2_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent,
        authorization_method: AuthorizationMethod::Signature(user2_signature),
    };

    // User 3's trade intent: sell 5_000 FT2 for 1 NEAR
    let user3_trade_intent = TradeIntent {
        user_id: user3.id().clone(),
        asset_in: AssetId::Nep141(ft2.id().clone()),
        asset_out: AssetId::Near,
        amount_in: U128(trade_amount_ft2),
        amount_out: U128(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user3_trade_intent).unwrap();
    let user3_signature: Base64VecU8 = Base64VecU8(
        match user3_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user3_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user3_trade_intent,
        authorization_method: AuthorizationMethod::Signature(user3_signature),
    };

    let operations = vec![Operation::DexCall {
        dex_id: dex_id.clone(),
        method: "match".to_string(),
        args: Base64VecU8(
            near_sdk::borsh::to_vec(&MatchArgs {
                authorized_trade_intents: vec![
                    user1_authorized_intent,
                    user2_authorized_intent,
                    user3_authorized_intent,
                ],
                output_destination: OutputDestination::InternalOtcBalance,
            })
            .unwrap(),
        ),
        attached_assets: HashMap::new(),
    }];

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

    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        user1.id(),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(trade_amount_ft1)),
    )
    .await
    .unwrap();

    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        user1.id(),
        AssetId::Near,
        Some(U128(
            assets_deposit_to_otc_near_user1
                .saturating_sub(trade_amount_near)
                .as_yoctonear(),
        )),
    )
    .await
    .unwrap();

    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        user2.id(),
        AssetId::Nep141(ft1.id().clone()),
        Some(U128(assets_deposit_to_otc_ft1_user2 - trade_amount_ft1)),
    )
    .await
    .unwrap();

    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        user2.id(),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(trade_amount_ft2)),
    )
    .await
    .unwrap();

    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        user3.id(),
        AssetId::Near,
        Some(U128(trade_amount_near.as_yoctonear())),
    )
    .await
    .unwrap();

    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        user3.id(),
        AssetId::Nep141(ft2.id().clone()),
        Some(U128(assets_deposit_to_otc_ft2_user3 - trade_amount_ft2)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_otc_nonces() {
    let initial_near_deposit = NearToken::from_near(20);
    let storage_deposit_for_otc = NearToken::from_millinear(15);
    let assets_deposit_to_otc_near_user1 = NearToken::from_near(10);
    let assets_deposit_to_otc_ft_user2 = 500_000_000u128;
    let trade_amount_near = NearToken::from_near(1);
    let trade_amount_ft = 100_000_000u128;
    let ft_initial_deposit = 1_000_000_000u128;

    let wasms = get_compiled_wasms().await;

    let TestContext {
        sandbox,
        dex_engine_contract,
        ft1,
        deployer,
        user1,
        user1_key,
        user2,
        user2_key,
        ..
    } = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "dex".to_string(),
            code: wasms.otc_dex_wasm.clone(),
            init_method: None,
        }),
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
    })
    .await;
    let dex_id = DexId {
        deployer: deployer.id().clone(),
        id: "dex".to_string(),
    };

    let result = user1
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(initial_near_deposit)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = deployer
        .call(ft1.id(), "ft_transfer")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": user2.id(),
            "amount": U128(ft_initial_deposit),
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
            "amount": U128(ft_initial_deposit),
            "msg": "",
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
            "method": "storage_deposit",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap()),
            "attached_assets": {
                "near": U128(storage_deposit_for_otc.as_yoctonear()),
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
            "method": "deposit_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            "attached_assets": {
                "near": U128(assets_deposit_to_otc_near_user1.as_yoctonear()),
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
            "method": "set_authorized_key",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
                key: user1_key.public_key().to_string().parse().unwrap(),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    let result = user2
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(storage_deposit_for_otc)
        .args_json(json!({
            "operations": [Operation::DexCall {
                dex_id: dex_id.clone(),
                method: "storage_deposit".to_string(),
                args: Base64VecU8(near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap()),
                attached_assets: HashMap::from_iter([(AssetId::Near, U128(storage_deposit_for_otc.as_yoctonear()))]),
            }],
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
            "method": "deposit_assets",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            "attached_assets": {
                format!("nep141:{}", ft1.id()): U128(assets_deposit_to_otc_ft_user2),
            },
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
            "method": "set_authorized_key",
            "args": BASE64_STANDARD.encode(near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
                key: user2_key.public_key().to_string().parse().unwrap(),
            }).unwrap()),
            "attached_assets": {},
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();

    otc_register_assets(
        &dex_engine_contract,
        &dex_id,
        &user1,
        vec![AssetId::Nep141(ft1.id().clone())],
    )
    .await;
    otc_register_assets(&dex_engine_contract, &dex_id, &user2, vec![AssetId::Near]).await;

    // Test 1: Reuse of the same signature with nonce (should fail on second use)
    let nonce1 = U128(12345);
    let user1_trade_intent_with_nonce = TradeIntent {
        user_id: user1.id().clone(),
        asset_in: AssetId::Near,
        asset_out: AssetId::Nep141(ft1.id().clone()),
        amount_in: U128(trade_amount_near.as_yoctonear()),
        amount_out: U128(trade_amount_ft),
        validity: Validity {
            expiry: None,
            nonce: Some(nonce1),
            only_for_whitelisted_parties: None,
        },
    };

    let serialized = near_sdk::borsh::to_vec(&user1_trade_intent_with_nonce).unwrap();
    let user1_signature: Base64VecU8 = Base64VecU8(
        match user1_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user1_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent_with_nonce,
        authorization_method: AuthorizationMethod::Signature(user1_signature),
    };

    let user2_trade_intent = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: U128(trade_amount_ft),
        amount_out: U128(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user2_trade_intent).unwrap();
    let user2_signature: Base64VecU8 = Base64VecU8(
        match user2_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user2_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent,
        authorization_method: AuthorizationMethod::Signature(user2_signature),
    };

    let operations = vec![Operation::DexCall {
        dex_id: dex_id.clone(),
        method: "match".to_string(),
        args: Base64VecU8(
            near_sdk::borsh::to_vec(&MatchArgs {
                authorized_trade_intents: vec![user1_authorized_intent, user2_authorized_intent],
                output_destination: OutputDestination::InternalOtcBalance,
            })
            .unwrap(),
        ),
        attached_assets: HashMap::new(),
    }];

    // First use should succeed
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

    // Second use with the same nonce should fail
    let user1_trade_intent_with_nonce_reuse = TradeIntent {
        user_id: user1.id().clone(),
        asset_in: AssetId::Near,
        asset_out: AssetId::Nep141(ft1.id().clone()),
        amount_in: U128(trade_amount_near.as_yoctonear()),
        amount_out: U128(trade_amount_ft),
        validity: Validity {
            expiry: None,
            nonce: Some(nonce1),
            only_for_whitelisted_parties: None,
        },
    };

    let serialized = near_sdk::borsh::to_vec(&user1_trade_intent_with_nonce_reuse).unwrap();
    let user1_signature_reuse: Base64VecU8 = Base64VecU8(
        match user1_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user1_authorized_intent_reuse = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent_with_nonce_reuse,
        authorization_method: AuthorizationMethod::Signature(user1_signature_reuse),
    };

    let user2_trade_intent_2 = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: U128(trade_amount_ft),
        amount_out: U128(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user2_trade_intent_2).unwrap();
    let user2_signature_2: Base64VecU8 = Base64VecU8(
        match user2_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user2_authorized_intent_2 = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent_2,
        authorization_method: AuthorizationMethod::Signature(user2_signature_2),
    };

    let operations = vec![Operation::DexCall {
        dex_id: dex_id.clone(),
        method: "match".to_string(),
        args: Base64VecU8(
            near_sdk::borsh::to_vec(&MatchArgs {
                authorized_trade_intents: vec![
                    user1_authorized_intent_reuse,
                    user2_authorized_intent_2,
                ],
                output_destination: OutputDestination::InternalOtcBalance,
            })
            .unwrap(),
        ),
        attached_assets: HashMap::new(),
    }];

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
    assert!(result.is_failure());

    // Test 2: Submit expired intent after block height expired
    let current_block_height = sandbox.view_block().await.unwrap().height();
    let expiry_block = current_block_height + 5;

    let user1_trade_intent_with_block_expiry = TradeIntent {
        user_id: user1.id().clone(),
        asset_in: AssetId::Near,
        asset_out: AssetId::Nep141(ft1.id().clone()),
        amount_in: U128(trade_amount_near.as_yoctonear()),
        amount_out: U128(trade_amount_ft),
        validity: Validity {
            expiry: Some(ExpiryCondition::BlockHeight(expiry_block)),
            nonce: Some(U128(54321)),
            only_for_whitelisted_parties: None,
        },
    };

    let serialized = near_sdk::borsh::to_vec(&user1_trade_intent_with_block_expiry).unwrap();
    let user1_signature_expiry: Base64VecU8 = Base64VecU8(
        match user1_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user1_authorized_intent_expiry = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent_with_block_expiry,
        authorization_method: AuthorizationMethod::Signature(user1_signature_expiry),
    };

    let user2_trade_intent_3 = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: U128(trade_amount_ft),
        amount_out: U128(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user2_trade_intent_3).unwrap();
    let user2_signature_3: Base64VecU8 = Base64VecU8(
        match user2_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user2_authorized_intent_3 = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent_3,
        authorization_method: AuthorizationMethod::Signature(user2_signature_3),
    };

    sandbox.fast_forward(10).await.unwrap();

    let operations = vec![Operation::DexCall {
        dex_id: dex_id.clone(),
        method: "match".to_string(),
        args: Base64VecU8(
            near_sdk::borsh::to_vec(&MatchArgs {
                authorized_trade_intents: vec![
                    user1_authorized_intent_expiry,
                    user2_authorized_intent_3,
                ],
                output_destination: OutputDestination::InternalOtcBalance,
            })
            .unwrap(),
        ),
        attached_assets: HashMap::new(),
    }];

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
    assert!(result.is_failure());

    // Test 3: Use a nonce with expiration, wait, use the same nonce after the previous one expired (should succeed)
    let current_block_height = sandbox.view_block().await.unwrap().height();
    let expiry_block_short = current_block_height + 5;
    let nonce3 = U128(99999);

    let user1_trade_intent_with_expiring_nonce = TradeIntent {
        user_id: user1.id().clone(),
        asset_in: AssetId::Near,
        asset_out: AssetId::Nep141(ft1.id().clone()),
        amount_in: U128(trade_amount_near.as_yoctonear()),
        amount_out: U128(trade_amount_ft),
        validity: Validity {
            expiry: Some(ExpiryCondition::BlockHeight(expiry_block_short)),
            nonce: Some(nonce3),
            only_for_whitelisted_parties: None,
        },
    };

    let serialized = near_sdk::borsh::to_vec(&user1_trade_intent_with_expiring_nonce).unwrap();
    let user1_signature_expiring_nonce: Base64VecU8 = Base64VecU8(
        match user1_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user1_authorized_intent_expiring_nonce = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent_with_expiring_nonce,
        authorization_method: AuthorizationMethod::Signature(user1_signature_expiring_nonce),
    };

    let user2_trade_intent_4 = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: U128(trade_amount_ft),
        amount_out: U128(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user2_trade_intent_4).unwrap();
    let user2_signature_4: Base64VecU8 = Base64VecU8(
        match user2_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user2_authorized_intent_4 = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent_4,
        authorization_method: AuthorizationMethod::Signature(user2_signature_4),
    };

    // First use should succeed
    let operations = vec![Operation::DexCall {
        dex_id: dex_id.clone(),
        method: "match".to_string(),
        args: Base64VecU8(
            near_sdk::borsh::to_vec(&MatchArgs {
                authorized_trade_intents: vec![
                    user1_authorized_intent_expiring_nonce,
                    user2_authorized_intent_4,
                ],
                output_destination: OutputDestination::InternalOtcBalance,
            })
            .unwrap(),
        ),
        attached_assets: HashMap::new(),
    }];

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

    sandbox.fast_forward(10).await.unwrap();

    // Now create a new intent with the same nonce but after expiry - should succeed
    let user1_trade_intent_reuse_nonce_after_expiry = TradeIntent {
        user_id: user1.id().clone(),
        asset_in: AssetId::Near,
        asset_out: AssetId::Nep141(ft1.id().clone()),
        amount_in: U128(trade_amount_near.as_yoctonear()),
        amount_out: U128(trade_amount_ft),
        validity: Validity {
            expiry: None,
            nonce: Some(nonce3),
            only_for_whitelisted_parties: None,
        },
    };

    let serialized = near_sdk::borsh::to_vec(&user1_trade_intent_reuse_nonce_after_expiry).unwrap();
    let user1_signature_reuse_nonce: Base64VecU8 = Base64VecU8(
        match user1_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user1_authorized_intent_reuse_nonce = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent_reuse_nonce_after_expiry,
        authorization_method: AuthorizationMethod::Signature(user1_signature_reuse_nonce),
    };

    let user2_trade_intent_5 = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: U128(trade_amount_ft),
        amount_out: U128(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let serialized = near_sdk::borsh::to_vec(&user2_trade_intent_5).unwrap();
    let user2_signature_5: Base64VecU8 = Base64VecU8(
        match user2_key.sign(&near_sdk::env::sha256_array(serialized)) {
            Signature::ED25519(sig) => sig.to_bytes().to_vec(),
            Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        },
    );

    let user2_authorized_intent_5 = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent_5,
        authorization_method: AuthorizationMethod::Signature(user2_signature_5),
    };

    let operations = vec![Operation::DexCall {
        dex_id: dex_id.clone(),
        method: "match".to_string(),
        args: Base64VecU8(
            near_sdk::borsh::to_vec(&MatchArgs {
                authorized_trade_intents: vec![
                    user1_authorized_intent_reuse_nonce,
                    user2_authorized_intent_5,
                ],
                output_destination: OutputDestination::InternalOtcBalance,
            })
            .unwrap(),
        ),
        attached_assets: HashMap::new(),
    }];

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
}
