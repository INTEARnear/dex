use crate::common::*;

use intear_dex_common::{AssetId, Caller, DexId, FeeFraction, SwapRequestAmount, TokenAmount};
use intear_dex_types::{
    AccountOrDexId, Operation, ReferralFees, SwapOperationAmount, WithdrawAmount,
};
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
    MatchArgs, Nonce, ORACLE_SWAP_MESSAGE_PREFIX, OracleRequest, OracleResponse, OracleSwapArgs,
    OutputDestination, QuoteId, RegisterAssetsArgs, SetAuthorizedKeyArgs, StorageDepositArgs,
    TradeIntent, TradeIntentSignaturePayload, Validity,
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

/// What the authorized `key` of `trade_intent`'s user signs to authorize it on `dex_id`
/// with `nonce`
fn sign_trade_intent(
    key: &SecretKey,
    chain_id: &str,
    engine_contract_id: &AccountId,
    dex_id: &DexId,
    trade_intent: &TradeIntent,
    nonce: Nonce,
) -> Base64VecU8 {
    let signature_payload = TradeIntentSignaturePayload {
        chain_id: chain_id.to_string(),
        engine_contract_id: engine_contract_id.clone(),
        dex_id: dex_id.clone(),
        trade_intent: trade_intent.clone(),
        nonce,
    };
    let hash = near_sdk::env::sha256_array(near_sdk::borsh::to_vec(&signature_payload).unwrap());
    Base64VecU8(match key.sign(&hash) {
        Signature::ED25519(sig) => sig.to_bytes().to_vec(),
        Signature::SECP256K1(sig) => <[u8; 65]>::from(sig).to_vec(),
        Signature::MLDSA65(sig) => sig.0.to_vec(),
    })
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
    let chain_id = sandbox.status().await.unwrap().chain_id;

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
        amount_in: TokenAmount::new(trade_amount_ft),
        amount_out: TokenAmount::new(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let user1_signature = sign_trade_intent(
        &deployer_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user1_trade_intent,
        U128(1001),
    );

    let user1_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent,
        authorization_method: AuthorizationMethod::Signature {
            signature: user1_signature,
            nonce: U128(1001),
        },
    };

    // User 2's trade intent: sell 2 NEAR for 100_000_000 FT
    let user2_trade_intent = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Near,
        asset_out: AssetId::Nep141(ft1.id().clone()),
        amount_in: TokenAmount::new(trade_amount_near.as_yoctonear()),
        amount_out: TokenAmount::new(trade_amount_ft),
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
            TokenAmount::new(trade_amount_near.as_yoctonear()),
        )]),
    }];

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
        sandbox,
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
    let chain_id = sandbox.status().await.unwrap().chain_id;

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
                attached_assets: HashMap::from_iter([(AssetId::Near, TokenAmount::new(storage_deposit_for_otc.as_yoctonear()))]),
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
        amount_in: TokenAmount::new(trade_amount_near.as_yoctonear()),
        amount_out: TokenAmount::new(trade_amount_ft),
        validity: Validity::default(),
    };

    let user1_signature = sign_trade_intent(
        &user1_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user1_trade_intent,
        U128(1002),
    );

    let user1_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent,
        authorization_method: AuthorizationMethod::Signature {
            signature: user1_signature,
            nonce: U128(1002),
        },
    };

    // User 2's trade intent: sell 100_000_000 FT for 2 NEAR
    let user2_trade_intent = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: TokenAmount::new(trade_amount_ft),
        amount_out: TokenAmount::new(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let user2_signature = sign_trade_intent(
        &user2_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user2_trade_intent,
        U128(1003),
    );

    let user2_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent,
        authorization_method: AuthorizationMethod::Signature {
            signature: user2_signature,
            nonce: U128(1003),
        },
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
        amount_in: TokenAmount::new(trade_amount_near.as_yoctonear()),
        amount_out: TokenAmount::new(trade_amount_ft),
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
    let chain_id = sandbox.status().await.unwrap().chain_id;

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
                attached_assets: HashMap::from_iter([(AssetId::Near, TokenAmount::new(storage_deposit_for_otc.as_yoctonear()))]),
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
        amount_in: TokenAmount::new(trade_amount_near.as_yoctonear()),
        amount_out: TokenAmount::new(trade_amount_ft),
        validity: Validity::default(),
    };

    let user1_signature = sign_trade_intent(
        &user1_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user1_trade_intent,
        U128(1004),
    );

    let user1_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent,
        authorization_method: AuthorizationMethod::Signature {
            signature: user1_signature,
            nonce: U128(1004),
        },
    };

    // User 2's trade intent: sell mismatched FT amount for 2 NEAR
    let user2_trade_intent = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: TokenAmount::new(mismatched_ft_amount),
        amount_out: TokenAmount::new(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let user2_signature = sign_trade_intent(
        &user2_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user2_trade_intent,
        U128(1005),
    );

    let user2_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent,
        authorization_method: AuthorizationMethod::Signature {
            signature: user2_signature,
            nonce: U128(1005),
        },
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
        sandbox,
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
    let chain_id = sandbox.status().await.unwrap().chain_id;

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
                attached_assets: HashMap::from_iter([(AssetId::Near, TokenAmount::new(storage_deposit_for_otc.as_yoctonear()))]),
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
                attached_assets: HashMap::from_iter([(AssetId::Near, TokenAmount::new(storage_deposit_for_otc.as_yoctonear()))]),
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
        amount_in: TokenAmount::new(trade_amount_near.as_yoctonear()),
        amount_out: TokenAmount::new(trade_amount_ft1),
        validity: Validity::default(),
    };

    let user1_signature = sign_trade_intent(
        &user1_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user1_trade_intent,
        U128(1006),
    );

    let user1_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent,
        authorization_method: AuthorizationMethod::Signature {
            signature: user1_signature,
            nonce: U128(1006),
        },
    };

    // User 2's trade intent: sell 100_000 FT1 for 5_000 FT2
    let user2_trade_intent = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Nep141(ft2.id().clone()),
        amount_in: TokenAmount::new(trade_amount_ft1),
        amount_out: TokenAmount::new(trade_amount_ft2),
        validity: Validity::default(),
    };

    let user2_signature = sign_trade_intent(
        &user2_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user2_trade_intent,
        U128(1007),
    );

    let user2_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent,
        authorization_method: AuthorizationMethod::Signature {
            signature: user2_signature,
            nonce: U128(1007),
        },
    };

    // User 3's trade intent: sell 5_000 FT2 for 1 NEAR
    let user3_trade_intent = TradeIntent {
        user_id: user3.id().clone(),
        asset_in: AssetId::Nep141(ft2.id().clone()),
        asset_out: AssetId::Near,
        amount_in: TokenAmount::new(trade_amount_ft2),
        amount_out: TokenAmount::new(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let user3_signature = sign_trade_intent(
        &user3_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user3_trade_intent,
        U128(1008),
    );

    let user3_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user3_trade_intent,
        authorization_method: AuthorizationMethod::Signature {
            signature: user3_signature,
            nonce: U128(1008),
        },
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
    } = setup_test_environment_on_own_sandbox(TestSetupConfig {
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
    let chain_id = sandbox.status().await.unwrap().chain_id;

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
                attached_assets: HashMap::from_iter([(AssetId::Near, TokenAmount::new(storage_deposit_for_otc.as_yoctonear()))]),
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
        amount_in: TokenAmount::new(trade_amount_near.as_yoctonear()),
        amount_out: TokenAmount::new(trade_amount_ft),
        validity: Validity {
            expiry: None,
            only_for_whitelisted_parties: None,
        },
    };

    let user1_signature = sign_trade_intent(
        &user1_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user1_trade_intent_with_nonce,
        nonce1,
    );

    let user1_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent_with_nonce,
        authorization_method: AuthorizationMethod::Signature {
            signature: user1_signature,
            nonce: nonce1,
        },
    };

    let user2_trade_intent = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: TokenAmount::new(trade_amount_ft),
        amount_out: TokenAmount::new(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let user2_signature = sign_trade_intent(
        &user2_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user2_trade_intent,
        U128(1009),
    );

    let user2_authorized_intent = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent,
        authorization_method: AuthorizationMethod::Signature {
            signature: user2_signature,
            nonce: U128(1009),
        },
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
        amount_in: TokenAmount::new(trade_amount_near.as_yoctonear()),
        amount_out: TokenAmount::new(trade_amount_ft),
        validity: Validity {
            expiry: None,
            only_for_whitelisted_parties: None,
        },
    };

    let user1_signature_reuse = sign_trade_intent(
        &user1_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user1_trade_intent_with_nonce_reuse,
        nonce1,
    );

    let user1_authorized_intent_reuse = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent_with_nonce_reuse,
        authorization_method: AuthorizationMethod::Signature {
            signature: user1_signature_reuse,
            nonce: nonce1,
        },
    };

    let user2_trade_intent_2 = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: TokenAmount::new(trade_amount_ft),
        amount_out: TokenAmount::new(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let user2_signature_2 = sign_trade_intent(
        &user2_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user2_trade_intent_2,
        U128(1010),
    );

    let user2_authorized_intent_2 = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent_2,
        authorization_method: AuthorizationMethod::Signature {
            signature: user2_signature_2,
            nonce: U128(1010),
        },
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
        amount_in: TokenAmount::new(trade_amount_near.as_yoctonear()),
        amount_out: TokenAmount::new(trade_amount_ft),
        validity: Validity {
            expiry: Some(ExpiryCondition::BlockHeight(expiry_block)),
            only_for_whitelisted_parties: None,
        },
    };

    let user1_signature_expiry = sign_trade_intent(
        &user1_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user1_trade_intent_with_block_expiry,
        U128(54321),
    );

    let user1_authorized_intent_expiry = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent_with_block_expiry,
        authorization_method: AuthorizationMethod::Signature {
            signature: user1_signature_expiry,
            nonce: U128(54321),
        },
    };

    let user2_trade_intent_3 = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: TokenAmount::new(trade_amount_ft),
        amount_out: TokenAmount::new(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let user2_signature_3 = sign_trade_intent(
        &user2_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user2_trade_intent_3,
        U128(1011),
    );

    let user2_authorized_intent_3 = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent_3,
        authorization_method: AuthorizationMethod::Signature {
            signature: user2_signature_3,
            nonce: U128(1011),
        },
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
        amount_in: TokenAmount::new(trade_amount_near.as_yoctonear()),
        amount_out: TokenAmount::new(trade_amount_ft),
        validity: Validity {
            expiry: Some(ExpiryCondition::BlockHeight(expiry_block_short)),
            only_for_whitelisted_parties: None,
        },
    };

    let user1_signature_expiring_nonce = sign_trade_intent(
        &user1_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user1_trade_intent_with_expiring_nonce,
        nonce3,
    );

    let user1_authorized_intent_expiring_nonce = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent_with_expiring_nonce,
        authorization_method: AuthorizationMethod::Signature {
            signature: user1_signature_expiring_nonce,
            nonce: nonce3,
        },
    };

    let user2_trade_intent_4 = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: TokenAmount::new(trade_amount_ft),
        amount_out: TokenAmount::new(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let user2_signature_4 = sign_trade_intent(
        &user2_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user2_trade_intent_4,
        U128(1012),
    );

    let user2_authorized_intent_4 = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent_4,
        authorization_method: AuthorizationMethod::Signature {
            signature: user2_signature_4,
            nonce: U128(1012),
        },
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
        amount_in: TokenAmount::new(trade_amount_near.as_yoctonear()),
        amount_out: TokenAmount::new(trade_amount_ft),
        validity: Validity {
            expiry: None,
            only_for_whitelisted_parties: None,
        },
    };

    let user1_signature_reuse_nonce = sign_trade_intent(
        &user1_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user1_trade_intent_reuse_nonce_after_expiry,
        nonce3,
    );

    let user1_authorized_intent_reuse_nonce = AuthorizedTradeIntent {
        trade_intent: user1_trade_intent_reuse_nonce_after_expiry,
        authorization_method: AuthorizationMethod::Signature {
            signature: user1_signature_reuse_nonce,
            nonce: nonce3,
        },
    };

    let user2_trade_intent_5 = TradeIntent {
        user_id: user2.id().clone(),
        asset_in: AssetId::Nep141(ft1.id().clone()),
        asset_out: AssetId::Near,
        amount_in: TokenAmount::new(trade_amount_ft),
        amount_out: TokenAmount::new(trade_amount_near.as_yoctonear()),
        validity: Validity::default(),
    };

    let user2_signature_5 = sign_trade_intent(
        &user2_key,
        &chain_id,
        dex_engine_contract.id(),
        &dex_id,
        &user2_trade_intent_5,
        U128(1013),
    );

    let user2_authorized_intent_5 = AuthorizedTradeIntent {
        trade_intent: user2_trade_intent_5,
        authorization_method: AuthorizationMethod::Signature {
            signature: user2_signature_5,
            nonce: U128(1013),
        },
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

#[tokio::test]
async fn test_otc_swap_ed25519() {
    check_otc_swap(KeyType::ED25519).await;
}

#[tokio::test]
async fn test_otc_swap_ml_dsa_65() {
    check_otc_swap(KeyType::MLDSA65).await;
}

async fn check_otc_swap(maker_key_type: KeyType) {
    // Each filled intent stores its nonce until it expires, paid from this
    let maker_otc_storage_deposit = NearToken::from_millinear(50);
    let maker_otc_near = NearToken::from_near(5);
    let maker_otc_ft = 500_000_000u128;
    let trade_amount_near = NearToken::from_near(2);
    let trade_amount_ft = 100_000_000u128;
    let fee_fraction = FeeFraction::new(10_000); // 1%

    let wasms = get_compiled_wasms().await;
    let TestContext {
        sandbox,
        dex_engine_contract,
        ft1,
        deployer: maker,
        user1: trader,
        user2: referrer,
        user3: outsider,
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
        deployer: maker.id().clone(),
        id: "dex".to_string(),
    };
    let chain_id = sandbox.status().await.unwrap().chain_id;
    let ft_asset = AssetId::Nep141(ft1.id().clone());

    // The maker deposits NEAR and FT into its OTC balance and authorizes a key
    let deposit_near = |account: &Account, amount: NearToken| {
        account
            .call(dex_engine_contract.id(), "deposit_near")
            .max_gas()
            .deposit(amount)
            .args_json(json!({}))
            .transact()
    };
    let dex_call = |account: &Account,
                    method: &str,
                    args: Vec<u8>,
                    attached_assets: near_sdk::serde_json::Value| {
        account
            .call(dex_engine_contract.id(), "dex_call")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "dex_id": dex_id.clone(),
                "method": method,
                "args": BASE64_STANDARD.encode(args),
                "attached_assets": attached_assets,
            }))
            .transact()
    };
    assert_success(
        &deposit_near(&maker, NearToken::from_near(10))
            .await
            .unwrap(),
    )
    .unwrap();
    let result = maker
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(maker_otc_ft),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let result = dex_call(
        &maker,
        "storage_deposit",
        near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap(),
        json!({ "near": U128(maker_otc_storage_deposit.as_yoctonear()) }),
    )
    .await
    .unwrap();
    assert_success(&result).unwrap();
    let result = dex_call(
        &maker,
        "deposit_assets",
        near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap(),
        json!({
            "near": U128(maker_otc_near.as_yoctonear()),
            format!("nep141:{}", ft1.id()): U128(maker_otc_ft),
        }),
    )
    .await
    .unwrap();
    assert_success(&result).unwrap();
    let maker_key = SecretKey::from_random(maker_key_type);
    let result = dex_call(
        &maker,
        "set_authorized_key",
        near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
            key: maker_key.public_key().to_string().parse().unwrap(),
        })
        .unwrap(),
        json!({}),
    )
    .await
    .unwrap();
    assert_success(&result).unwrap();

    // The maker sells FT for NEAR, only to the trader, once per nonce
    let expiry_block_height = sandbox.view_block().await.unwrap().height() + 10_000;
    let maker_trade = || TradeIntent {
        user_id: maker.id().clone(),
        asset_in: ft_asset.clone(),
        asset_out: AssetId::Near,
        amount_in: TokenAmount::new(trade_amount_ft),
        amount_out: TokenAmount::new(trade_amount_near.as_yoctonear()),
        validity: Validity {
            expiry: Some(ExpiryCondition::BlockHeight(expiry_block_height)),
            only_for_whitelisted_parties: Some(vec![trader.id().clone()]),
        },
    };
    let signed = |trade_intent: TradeIntent, nonce: u128| {
        let signature = sign_trade_intent(
            &maker_key,
            &chain_id,
            dex_engine_contract.id(),
            &dex_id,
            &trade_intent,
            U128(nonce),
        );
        Base64VecU8(
            near_sdk::borsh::to_vec(&AuthorizedTradeIntent {
                trade_intent,
                authorization_method: AuthorizationMethod::Signature {
                    signature,
                    nonce: U128(nonce),
                },
            })
            .unwrap(),
        )
    };
    let referral_fees = ReferralFees(vec![(referrer.id().clone(), fee_fraction)]);
    let fee = referral_fees
        .amounts_on_top_of(TokenAmount::new(trade_amount_near.as_yoctonear()))
        .into_iter()
        .map(|(_, fee)| fee.get())
        .sum::<u128>();
    let near_in_with_fee = trade_amount_near.as_yoctonear() + fee;
    let swap = |trader: &Account, message: &Base64VecU8, amount_out: u128| {
        trader
            .call(dex_engine_contract.id(), "execute_operations")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "operations": [Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: message.clone(),
                    asset_in: AssetId::Near,
                    asset_out: ft_asset.clone(),
                    amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactOut(TokenAmount::new(amount_out))),
                    constraint: Some(TokenAmount::new(near_in_with_fee)),
                    quote_improvement_fee: None,
                }],
                "referral_fees": referral_fees,
            }))
            .transact()
    };
    let assert_failed_with = |result: near_workspaces::result::ExecutionFinalResult,
                              message: &str| {
        let failures = format!("{:?}", result.failures());
        assert!(
            failures.contains(message),
            "Expected {message:?}, got {failures}"
        );
    };
    let inner_balance = |of: AccountOrDexId, asset_id: AssetId| {
        let dex_engine_contract = dex_engine_contract.clone();
        async move {
            get_inner_asset_balance(&dex_engine_contract, of, asset_id)
                .await
                .unwrap()
                .unwrap()
                .0
        }
    };

    assert_success(
        &deposit_near(&trader, NearToken::from_near(5))
            .await
            .unwrap(),
    )
    .unwrap();
    let trader_near_before =
        inner_balance(AccountOrDexId::Account(trader.id().clone()), AssetId::Near).await;
    let referrer_near_before = inner_balance(
        AccountOrDexId::Account(referrer.id().clone()),
        AssetId::Near,
    )
    .await;
    let first_intent = signed(maker_trade(), 1);

    // A simulation doesn't use up the nonce
    let simulation = dex_engine_contract
        .view("simulate_swap_simple")
        .args_json(json!({
            "dex_id": dex_id,
            "message": first_intent,
            "trader": trader.id(),
            "asset_in": AssetId::Near,
            "asset_out": ft_asset,
            "amount": SwapRequestAmount::ExactOut(TokenAmount::new(trade_amount_ft)),
            "referral_fees": referral_fees,
        }))
        .await
        .unwrap()
        .json::<(U128, U128)>()
        .unwrap();
    assert_eq!(simulation, (U128(near_in_with_fee), U128(trade_amount_ft)));

    // The trader pays the intent's NEAR and the fee on top, and gets the intent's FT
    let result = swap(&trader, &first_intent, trade_amount_ft).await.unwrap();
    assert_success(&result).unwrap();
    assert_eq!(
        inner_balance(AccountOrDexId::Account(trader.id().clone()), AssetId::Near).await,
        trader_near_before - near_in_with_fee
    );
    assert_eq!(
        inner_balance(
            AccountOrDexId::Account(trader.id().clone()),
            ft_asset.clone()
        )
        .await,
        trade_amount_ft
    );
    assert_eq!(
        inner_balance(
            AccountOrDexId::Account(referrer.id().clone()),
            AssetId::Near
        )
        .await,
        referrer_near_before + fee
    );
    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        maker.id(),
        AssetId::Near,
        Some(U128(
            maker_otc_near
                .saturating_add(trade_amount_near)
                .as_yoctonear(),
        )),
    )
    .await
    .unwrap();
    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        maker.id(),
        ft_asset.clone(),
        Some(U128(maker_otc_ft - trade_amount_ft)),
    )
    .await
    .unwrap();

    // The same intent can't be filled twice
    let result = swap(&trader, &first_intent, trade_amount_ft).await.unwrap();
    assert_failed_with(result, "Nonce already used");

    let second_intent = signed(maker_trade(), 2);
    // Only the whitelisted trader can take it
    assert_success(
        &deposit_near(&outsider, NearToken::from_near(5))
            .await
            .unwrap(),
    )
    .unwrap();
    let result = swap(&outsider, &second_intent, trade_amount_ft)
        .await
        .unwrap();
    assert_failed_with(result, "not whitelisted");
    // Only for exactly the intent's amounts
    let result = swap(&trader, &second_intent, trade_amount_ft / 2)
        .await
        .unwrap();
    assert_failed_with(result, "Amount out doesn't match the intent");
    // Not by the maker itself
    let result = swap(&maker, &second_intent, trade_amount_ft).await.unwrap();
    assert_failed_with(result, "a trader can't swap with their own intent");
    // And only signed
    let unsigned_intent = Base64VecU8(
        near_sdk::borsh::to_vec(&AuthorizedTradeIntent {
            trade_intent: maker_trade(),
            authorization_method: AuthorizationMethod::Predecessor,
        })
        .unwrap(),
    );
    let result = swap(&trader, &unsigned_intent, trade_amount_ft)
        .await
        .unwrap();
    assert_failed_with(result, "AuthorizationMethod::Predecessor cannot be used");

    // A signature for another dex isn't valid on this one
    let other_dex_id = DexId {
        deployer: maker.id().clone(),
        id: "other-dex".to_string(),
    };
    let trade_intent_for_other_dex = maker_trade();
    let signature_for_other_dex = sign_trade_intent(
        &maker_key,
        &chain_id,
        dex_engine_contract.id(),
        &other_dex_id,
        &trade_intent_for_other_dex,
        U128(4),
    );
    let intent_for_other_dex = Base64VecU8(
        near_sdk::borsh::to_vec(&AuthorizedTradeIntent {
            trade_intent: trade_intent_for_other_dex,
            authorization_method: AuthorizationMethod::Signature {
                signature: signature_for_other_dex,
                nonce: U128(4),
            },
        })
        .unwrap(),
    );
    let result = swap(&trader, &intent_for_other_dex, trade_amount_ft)
        .await
        .unwrap();
    assert_failed_with(result, "Signature verification failed");

    // The intent that failed to be taken can still be
    let result = swap(&trader, &second_intent, trade_amount_ft)
        .await
        .unwrap();
    assert_success(&result).unwrap();

    // Any contract can claim in ft_on_transfer that the whitelisted trader sent its token, so a
    // whitelisted intent can't be taken in a transfer call of another asset
    let fake_token_asset = AssetId::Nep141(outsider.id().clone());
    let result = outsider
        .call(dex_engine_contract.id(), "register_assets")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({ "asset_ids": [fake_token_asset] }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    let result = outsider
        .call(dex_engine_contract.id(), "ft_on_transfer")
        .max_gas()
        .args_json(json!({
            "sender_id": trader.id(),
            "amount": U128(1),
            "msg": near_sdk::serde_json::to_string(&[Operation::SwapSimple {
                dex_id: dex_id.clone(),
                message: signed(maker_trade(), 5),
                asset_in: AssetId::Near,
                asset_out: ft_asset.clone(),
                amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactOut(TokenAmount::new(trade_amount_ft))),
                constraint: None,
                quote_improvement_fee: None,
            }])
            .unwrap(),
        }))
        .transact()
        .await
        .unwrap();
    assert_failed_with(
        result,
        "a whitelisted intent can't be swapped in a transfer call of another asset",
    );

    // But it can be in a transfer call of the asset in, as its sender is real
    let result = trader
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(trade_amount_near)
        .args_json(json!({
            "operations": [
                Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: signed(maker_trade(), 6),
                    asset_in: AssetId::Near,
                    asset_out: ft_asset.clone(),
                    amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactOut(TokenAmount::new(trade_amount_ft))),
                    constraint: Some(TokenAmount::new(trade_amount_near.as_yoctonear())),
                    quote_improvement_fee: None,
                },
                Operation::Withdraw {
                    asset_id: ft_asset.clone(),
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
}

#[tokio::test]
async fn test_romi_swap() {
    let solver_otc_storage_deposit = NearToken::from_millinear(50);
    let ft_unit = 100_000_000u128;
    let near_in = NearToken::from_near(1).as_yoctonear();
    let worst_case_ft = 90_000_000u128;

    let wasms = get_compiled_wasms().await;
    let TestContext {
        sandbox,
        dex_engine_contract,
        ft1,
        ft2,
        deployer,
        user1: trader,
        user3: solver,
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
    let chain_id = sandbox.status().await.unwrap().chain_id;
    let ft_asset = AssetId::Nep141(ft1.id().clone());
    let ft_unregistered_asset = AssetId::Nep141(ft2.id().clone());
    let dex_call_args =
        |method: &str, args: Vec<u8>, attached_assets: near_sdk::serde_json::Value| {
            json!({
                "dex_id": dex_id.clone(),
                "method": method,
                "args": BASE64_STANDARD.encode(args),
                "attached_assets": attached_assets,
            })
        };
    let dex_call = |account: &Account,
                    method: &str,
                    args: Vec<u8>,
                    attached_assets: near_sdk::serde_json::Value| {
        account
            .call(dex_engine_contract.id(), "dex_call")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(dex_call_args(method, args, attached_assets))
            .transact()
    };
    let deposit_near = |account: &Account, amount: NearToken| {
        account
            .call(dex_engine_contract.id(), "deposit_near")
            .max_gas()
            .deposit(amount)
            .args_json(json!({}))
            .transact()
    };
    let assert_failed_with = |result: &near_workspaces::result::ExecutionFinalResult,
                              message: &str| {
        let failures = format!("{:?}", result.failures());
        assert!(
            failures.contains(message),
            "Expected {message:?}, got {failures}"
        );
    };
    let inner_balance = |of: &Account, asset_id: AssetId| {
        let dex_engine_contract = dex_engine_contract.clone();
        let of = AccountOrDexId::Account(of.id().clone());
        async move {
            get_inner_asset_balance(&dex_engine_contract, of, asset_id)
                .await
                .unwrap()
                .map_or(0, |balance| balance.0)
        }
    };

    // The solver has 4 units of FT in its OTC balance, and 1 more and some of an FT that it has
    // no OTC balance of in its wallet. Its oracle key is its authorized key.
    for (token, amount) in [(&ft1, 5 * ft_unit), (&ft2, ft_unit)] {
        let result = deployer
            .call(token.id(), "ft_transfer")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": solver.id(),
                "amount": U128(amount),
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();
    }
    let result = solver
        .call(ft1.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(4 * ft_unit),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
    assert_success(
        &deposit_near(&solver, NearToken::from_near(1))
            .await
            .unwrap(),
    )
    .unwrap();
    let result = dex_call(
        &solver,
        "storage_deposit",
        near_sdk::borsh::to_vec(&StorageDepositArgs).unwrap(),
        json!({ "near": U128(solver_otc_storage_deposit.as_yoctonear()) }),
    )
    .await
    .unwrap();
    assert_success(&result).unwrap();
    otc_register_assets(
        &dex_engine_contract,
        &dex_id,
        &solver,
        vec![AssetId::Near, ft_asset.clone()],
    )
    .await;
    let result = dex_call(
        &solver,
        "deposit_assets",
        near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap(),
        json!({ format!("nep141:{}", ft1.id()): U128(4 * ft_unit) }),
    )
    .await
    .unwrap();
    assert_success(&result).unwrap();
    let oracle_key = create_oracle_key(
        &dex_engine_contract,
        &solver,
        KeyType::ED25519,
        NearToken::from_near(1),
    )
    .await;
    let result = set_oracle_fees(
        &dex_engine_contract,
        &solver,
        &oracle_key,
        &[(AssetId::Near, 0)],
    )
    .await;
    assert_success(&result).unwrap();
    let result = dex_call(
        &solver,
        "set_authorized_key",
        near_sdk::borsh::to_vec(&SetAuthorizedKeyArgs {
            key: oracle_key.public_key().to_string().parse().unwrap(),
        })
        .unwrap(),
        json!({}),
    )
    .await
    .unwrap();
    assert_success(&result).unwrap();
    assert_success(
        &deposit_near(&trader, NearToken::from_near(10))
            .await
            .unwrap(),
    )
    .unwrap();

    // The trader swaps NEAR for the FT of the solver
    let expiry_block_height = sandbox.view_block().await.unwrap().height() + 10_000;
    let romi_request = |quote_id: u128| OracleRequest {
        trader: Caller::Direct(trader.id().clone()),
        quote_id: QuoteId(U128(quote_id)),
    };
    let romi_request_bytes =
        |quote_id: u128| near_sdk::borsh::to_vec(&romi_request(quote_id)).unwrap();
    let oracle_request = |request: Vec<u8>| Operation::OracleRequest {
        oracle_key: oracle_key_handle(&oracle_key),
        dex_id: dex_id.clone(),
        request: Base64VecU8(request),
        fee: (AssetId::Near, TokenAmount::ZERO),
        swap: None,
        rescue_address: None,
    };
    let romi_swap = |quote_id: u128, amount: SwapRequestAmount, worst_case_amount: u128| {
        let mut message = ORACLE_SWAP_MESSAGE_PREFIX.to_vec();
        message.extend(
            near_sdk::borsh::to_vec(&OracleSwapArgs {
                solver_id: solver.id().clone(),
                quote_id: QuoteId(U128(quote_id)),
                worst_case_amount: TokenAmount::new(worst_case_amount),
            })
            .unwrap(),
        );
        Operation::SwapSimple {
            dex_id: dex_id.clone(),
            message: Base64VecU8(message),
            asset_in: AssetId::Near,
            asset_out: ft_asset.clone(),
            amount: SwapOperationAmount::Amount(amount),
            constraint: None,
            quote_improvement_fee: None,
        }
    };
    let exact_in = SwapRequestAmount::ExactIn(TokenAmount::new(near_in));
    let solver_trade = |gives_ft: u128, takes_near: u128| TradeIntent {
        user_id: solver.id().clone(),
        asset_in: ft_asset.clone(),
        asset_out: AssetId::Near,
        amount_in: TokenAmount::new(gives_ft),
        amount_out: TokenAmount::new(takes_near),
        validity: Validity {
            expiry: Some(ExpiryCondition::BlockHeight(expiry_block_height)),
            only_for_whitelisted_parties: None,
        },
    };
    let fill = |trade_intent: TradeIntent, nonce: u128| {
        near_sdk::borsh::to_vec(&OracleResponse::Fill {
            trade_intent,
            nonce: U128(nonce),
        })
        .unwrap()
    };
    let execute_operations = |operations: Vec<Operation>| {
        trader
            .call(dex_engine_contract.id(), "execute_operations")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({ "operations": operations }))
            .transact_async()
    };
    // The operations wait for the solver, which responds to the request with its gas key
    let respond = async |operations: Vec<Operation>, request: Vec<u8>, response: Vec<u8>| {
        let waiting = execute_operations(operations).await.unwrap();
        let nonces = waiting_for_oracle_request(&sandbox, &[&waiting], &request).await;
        let outcome = oracle_respond(
            &sandbox,
            &dex_engine_contract,
            &oracle_key,
            &request,
            response,
            nonces,
        )
        .await;
        assert_gas_key_call_success(&outcome).unwrap();
        waiting.await.unwrap()
    };
    let assert_solver_otc_balance = async |asset_id: AssetId, amount: u128| {
        assert_otc_inner_asset_balance(
            &dex_engine_contract,
            &dex_id,
            solver.id(),
            asset_id,
            Some(U128(amount)),
        )
        .await
        .unwrap();
    };

    // The solver fills the quote with better than the worst case
    let request = romi_request_bytes(1);
    let result = respond(
        vec![
            oracle_request(request.clone()),
            romi_swap(1, exact_in, worst_case_ft),
        ],
        request.clone(),
        fill(solver_trade(ft_unit, near_in), 1),
    )
    .await;
    assert_success(&result).unwrap();
    assert_eq!(inner_balance(&trader, ft_asset.clone()).await, ft_unit);
    assert_solver_otc_balance(ft_asset.clone(), 3 * ft_unit).await;
    assert_solver_otc_balance(AssetId::Near, near_in).await;
    let trader_near = inner_balance(&trader, AssetId::Near).await;

    // A solver that rejects fails the swap with its reason
    let request = romi_request_bytes(2);
    let result = respond(
        vec![
            oracle_request(request.clone()),
            romi_swap(2, exact_in, worst_case_ft),
        ],
        request.clone(),
        near_sdk::borsh::to_vec(&OracleResponse::Reject {
            reason: "no inventory".to_string(),
        })
        .unwrap(),
    )
    .await;
    assert_failed_with(&result, "rejected the swap: no inventory");

    // The solver can't give less than the worst case, or take more for an exact-out swap
    let request = romi_request_bytes(3);
    let result = respond(
        vec![
            oracle_request(request.clone()),
            romi_swap(3, exact_in, worst_case_ft),
        ],
        request.clone(),
        fill(solver_trade(worst_case_ft - 1, near_in), 3),
    )
    .await;
    assert_failed_with(&result, "less than the worst case");
    let exact_out = SwapRequestAmount::ExactOut(TokenAmount::new(ft_unit));
    let request = romi_request_bytes(4);
    let result = respond(
        vec![
            oracle_request(request.clone()),
            romi_swap(4, exact_out, near_in),
        ],
        request.clone(),
        fill(solver_trade(ft_unit, near_in + 1), 4),
    )
    .await;
    assert_failed_with(&result, "more than the worst case");

    // A fill can't be used twice in the operations that its request started
    let request = romi_request_bytes(5);
    let result = respond(
        vec![
            oracle_request(request.clone()),
            romi_swap(5, exact_in, worst_case_ft),
            romi_swap(5, exact_in, worst_case_ft),
        ],
        request.clone(),
        fill(solver_trade(ft_unit, near_in), 5),
    )
    .await;
    assert_failed_with(&result, "Nonce already used");
    assert_eq!(inner_balance(&trader, AssetId::Near).await, trader_near);
    assert_solver_otc_balance(ft_asset.clone(), 3 * ft_unit).await;

    // An oracle response authorizes the solver's intent in a match too
    let match_request = romi_request(6);
    let solver_intent = solver_trade(ft_unit, near_in);
    let match_args = MatchArgs {
        authorized_trade_intents: vec![
            AuthorizedTradeIntent {
                trade_intent: solver_intent.clone(),
                authorization_method: AuthorizationMethod::OracleResponse {
                    request: match_request.clone(),
                    nonce: U128(6),
                },
            },
            AuthorizedTradeIntent {
                trade_intent: TradeIntent {
                    user_id: trader.id().clone(),
                    asset_in: AssetId::Near,
                    asset_out: ft_asset.clone(),
                    amount_in: TokenAmount::new(near_in),
                    amount_out: TokenAmount::new(ft_unit),
                    validity: Validity::default(),
                },
                authorization_method: AuthorizationMethod::Predecessor,
            },
        ],
        output_destination: OutputDestination::IntearDexBalance,
    };
    let result = respond(
        vec![
            oracle_request(romi_request_bytes(6)),
            Operation::DexCall {
                dex_id: dex_id.clone(),
                method: "match".to_string(),
                args: Base64VecU8(near_sdk::borsh::to_vec(&match_args).unwrap()),
                attached_assets: HashMap::from([(AssetId::Near, TokenAmount::new(near_in))]),
            },
        ],
        romi_request_bytes(6),
        fill(solver_intent, 6),
    )
    .await;
    assert_success(&result).unwrap();
    assert_eq!(inner_balance(&trader, ft_asset.clone()).await, 2 * ft_unit);
    assert_solver_otc_balance(ft_asset.clone(), 2 * ft_unit).await;
    assert_solver_otc_balance(AssetId::Near, 2 * near_in).await;

    // The solver fills 3 units with the 2 it has and 1 that it deposits in the transfer call
    // that responds
    let request = romi_request_bytes(7);
    let waiting = execute_operations(vec![
        oracle_request(request.clone()),
        romi_swap(7, exact_in, worst_case_ft),
    ])
    .await
    .unwrap();
    let nonces = waiting_for_oracle_request(&sandbox, &[&waiting], &request).await;
    let deliver_and_respond = |token: &Contract, asset_id: &AssetId, respond: Option<Operation>| {
        let mut operations = vec![Operation::DexCall {
            dex_id: dex_id.clone(),
            method: "deposit_assets".to_string(),
            args: Base64VecU8(near_sdk::borsh::to_vec(&DepositAssetsArgs).unwrap()),
            attached_assets: HashMap::from([(asset_id.clone(), TokenAmount::new(ft_unit))]),
        }];
        operations.extend(respond);
        solver
            .call(token.id(), "ft_transfer_call")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": dex_engine_contract.id(),
                "amount": U128(ft_unit),
                "msg": near_sdk::serde_json::to_string(&operations).unwrap(),
            }))
            .transact()
    };
    let result = deliver_and_respond(
        &ft1,
        &ft_asset,
        Some(respond_to_oracle_request(
            &oracle_key,
            &oracle_key,
            &chain_id,
            dex_engine_contract.id(),
            &request,
            fill(solver_trade(3 * ft_unit, near_in), 7),
            nonces,
        )),
    )
    .await
    .unwrap();
    assert_success(&result).unwrap();
    let result = waiting.await.unwrap();
    assert_success(&result).unwrap();
    assert_eq!(inner_balance(&trader, ft_asset.clone()).await, 5 * ft_unit);
    assert_solver_otc_balance(ft_asset.clone(), 0).await;
    assert_solver_otc_balance(AssetId::Near, 3 * near_in).await;
    assert_ft_balance(&solver, ft1.clone(), U128(0))
        .await
        .unwrap();

    // A transfer call only deposits into a registered balance, since its sender can be faked, and
    // otherwise its tokens go back
    let result = deliver_and_respond(&ft2, &ft_unregistered_asset, None)
        .await
        .unwrap();
    assert_failed_with(
        &result,
        &format!("has no registered balance for asset {ft_unregistered_asset}"),
    );
    assert_ft_balance(&solver, ft2.clone(), U128(ft_unit))
        .await
        .unwrap();
    assert_otc_inner_asset_balance(
        &dex_engine_contract,
        &dex_id,
        solver.id(),
        ft_unregistered_asset,
        None,
    )
    .await
    .unwrap();
}
