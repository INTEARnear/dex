use crate::common::*;

use amp_dex_types::{
    AddLiquidityArgs, AmpDexEvent, Amplification, AssetIndex, AssetWithBalance, CreatePoolArgs,
    ExchangePrices, FULL_FEE_DISCOUNT, FeeDiscount, FeeDiscountSignaturePayload, FeeFraction,
    FeePayoutType, FeeReceiver, GetLiquidityPositionArgs, GetPendingFeesArgs, GetPoolArgs,
    GetPoolSharesArgs, INITIAL_SHARES, PROTOCOL_FEE_RECEIVER_ID, PoolId, PoolView, Price,
    PricesRequest, PricesResponse, ProviderShares, RegisterLiquidityArgs, RemoveLiquidityArgs,
    RouteFraction, SetAmplificationArgs, SetFeeDiscountSignerArgs, SetLiquidityFeePayoutArgs,
    SetOracleKeyArgs, SignedFeeDiscount, SwapArgs, SwapRoute, TokenAmount, WithdrawFeesArgs,
};
use intear_dex_common::{AssetId, DexId, SwapRequestAmount};
use intear_dex_types::{AccountOrDexId, DexEventData, Operation, SwapOperationAmount};
use near_crypto::{KeyType, SecretKey, Signature};
use near_sdk::serde_json::json;
use near_sdk::{
    AccountId, NearToken,
    base64::{Engine, prelude::BASE64_STANDARD},
    json_types::{Base64VecU8, U128},
};
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::{Account, Contract};
use std::collections::HashMap;

/// ft1 is rated as NEAR, ft2 as USDC, and ft3 as XAUT
const NEAR_DECIMALS: u8 = 24;
const USDC_DECIMALS: u8 = 6;
const XAUT_DECIMALS: u8 = 6;

fn near(amount: u128) -> u128 {
    amount * 10u128.pow(u32::from(NEAR_DECIMALS))
}

fn usdc(amount: u128) -> u128 {
    amount * 10u128.pow(u32::from(USDC_DECIMALS))
}

fn xaut(amount: u128) -> u128 {
    amount * 10u128.pow(u32::from(XAUT_DECIMALS))
}

async fn dex_view<T: near_sdk::borsh::BorshDeserialize>(
    dex_engine_contract: &Contract,
    dex_id: &DexId,
    method: &str,
    args: Vec<u8>,
) -> T {
    let result = dex_engine_contract
        .view("dex_view")
        .args_json(json!({
            "dex_id": dex_id,
            "method": method,
            "args": BASE64_STANDARD.encode(args),
        }))
        .await
        .unwrap();
    near_sdk::borsh::from_slice(&result.json::<Base64VecU8>().unwrap().0).unwrap()
}

async fn get_pool(dex_engine_contract: &Contract, dex_id: &DexId, pool_id: PoolId) -> PoolView {
    dex_view::<Option<PoolView>>(
        dex_engine_contract,
        dex_id,
        "get_pool",
        near_sdk::borsh::to_vec(&GetPoolArgs { pool_id }).unwrap(),
    )
    .await
    .unwrap()
}

async fn get_pool_shares(
    dex_engine_contract: &Contract,
    dex_id: &DexId,
    pool_id: PoolId,
    account_id: &AccountId,
) -> Option<ProviderShares> {
    dex_view::<Vec<Option<ProviderShares>>>(
        dex_engine_contract,
        dex_id,
        "get_pool_shares",
        near_sdk::borsh::to_vec(&GetPoolSharesArgs {
            pool_ids: vec![pool_id],
            account_id: account_id.clone(),
        })
        .unwrap(),
    )
    .await[0]
}

async fn get_pending_fees(
    dex_engine_contract: &Contract,
    dex_id: &DexId,
    account_id: &AccountId,
    asset_id: &AssetId,
) -> u128 {
    dex_view::<HashMap<AssetId, TokenAmount>>(
        dex_engine_contract,
        dex_id,
        "get_pending_fees",
        near_sdk::borsh::to_vec(&GetPendingFeesArgs {
            account_id: account_id.clone(),
            asset_ids: vec![asset_id.clone()],
        })
        .unwrap(),
    )
    .await[asset_id]
        .get()
}

/// The amp events in the `dex_events_borsh` logs of the transaction
fn amp_events(result: &ExecutionFinalResult) -> Vec<AmpDexEvent> {
    result
        .logs()
        .into_iter()
        .filter_map(|log| log.strip_prefix("EVENT_JSON:"))
        .map(|log| near_sdk::serde_json::from_str::<near_sdk::serde_json::Value>(log).unwrap())
        .filter(|outer| outer["standard"] == "inteardex" && outer["event"] == "dex_events_borsh")
        .flat_map(|outer| {
            let compressed = BASE64_STANDARD
                .decode(outer["data"].as_str().unwrap())
                .unwrap();
            near_sdk::borsh::from_slice::<Vec<DexEventData>>(
                &lz4_flex::decompress_size_prepended(&compressed).unwrap(),
            )
            .unwrap()
        })
        .map(|dex_event| near_sdk::borsh::from_slice(&dex_event.event).unwrap())
        .collect()
}

async fn inner_balance(
    dex_engine_contract: &Contract,
    of: AccountOrDexId,
    asset_id: &AssetId,
) -> u128 {
    get_inner_asset_balance(dex_engine_contract, of, asset_id.clone())
        .await
        .unwrap()
        .map_or(0, |balance| balance.0)
}

/// Prices of the requested assets on exchanges, as the oracle responds with them
fn prices(exchanges: &[(&str, &[&str])]) -> PricesResponse {
    PricesResponse {
        exchanges: exchanges
            .iter()
            .map(|(exchange, prices)| ExchangePrices {
                exchange: exchange.to_string(),
                prices: prices.iter().map(|price| price.parse().unwrap()).collect(),
            })
            .collect(),
    }
}

/// Prices of NEAR and USDC that agree, with NEAR at 3.748 USDC on all of them
fn agreeing_prices() -> PricesResponse {
    prices(&[
        ("binance", &["3.75", "1.0005"]),
        ("kraken", &["3.748", "1"]),
        ("okx", &["3.7518", "1.001"]),
    ])
}

fn dex_call(
    dex_id: &DexId,
    method: &str,
    args: Vec<u8>,
    attached_assets: &[(AssetId, u128)],
) -> Operation {
    Operation::DexCall {
        dex_id: dex_id.clone(),
        method: method.to_string(),
        args: Base64VecU8(args),
        attached_assets: attached_assets
            .iter()
            .map(|(asset_id, amount)| (asset_id.clone(), TokenAmount::new(*amount)))
            .collect(),
    }
}

async fn execute_operations(
    account: &Account,
    dex_engine_contract: &Contract,
    operations: Vec<Operation>,
) -> ExecutionFinalResult {
    account
        .call(dex_engine_contract.id(), "execute_operations")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "operations": operations,
        }))
        .transact()
        .await
        .unwrap()
}

/// Deposit `amount` of an FT that the deployer has to the engine balance of `account`
async fn deposit_ft(
    dex_engine_contract: &Contract,
    deployer: &Account,
    ft: &Contract,
    account: &Account,
    amount: u128,
) {
    if account.id() != deployer.id() {
        let result = deployer
            .call(ft.id(), "ft_transfer")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "receiver_id": account.id(),
                "amount": U128(amount),
            }))
            .transact()
            .await
            .unwrap();
        assert_success(&result).unwrap();
    }
    let result = account
        .call(ft.id(), "ft_transfer_call")
        .max_gas()
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": dex_engine_contract.id(),
            "amount": U128(amount),
            "msg": "",
        }))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
}

async fn deposit_near(dex_engine_contract: &Contract, account: &Account, amount: NearToken) {
    let result = account
        .call(dex_engine_contract.id(), "deposit_near")
        .max_gas()
        .deposit(amount)
        .args_json(json!({}))
        .transact()
        .await
        .unwrap();
    assert_success(&result).unwrap();
}

struct AmpTest {
    context: TestContext,
    dex_id: DexId,
    oracle_key: SecretKey,
    near_asset_id: AssetId,
    usdc_asset_id: AssetId,
}

/// The engine with an amp dex that the deployer owns, and an oracle key
async fn setup_amp() -> AmpTest {
    let wasms = get_compiled_wasms().await;
    let context = setup_test_environment_with_config(TestSetupConfig {
        dex: Some(DexSetupConfig {
            id: "amp".to_string(),
            code: wasms.amp_dex_wasm.clone(),
            init_method: Some(("new".to_string(), vec![])),
        }),
        register_assets_for_all: true,
        ft_storage_deposit_for_all: true,
    })
    .await;
    let dex_id = DexId {
        deployer: context.deployer.id().clone(),
        id: "amp".to_string(),
    };
    let oracle_key = create_oracle_key(
        &context.dex_engine_contract,
        &context.deployer,
        KeyType::ED25519,
        NearToken::from_near(2),
    )
    .await;
    deposit_near(
        &context.dex_engine_contract,
        &context.deployer,
        NearToken::from_near(20),
    )
    .await;
    AmpTest {
        near_asset_id: AssetId::Nep141(context.ft1.id().clone()),
        usdc_asset_id: AssetId::Nep141(context.ft2.id().clone()),
        context,
        dex_id,
        oracle_key,
    }
}

impl AmpTest {
    fn create_pool_args(&self, fee: FeeFraction, liquidity: (u128, u128)) -> CreatePoolArgs {
        CreatePoolArgs {
            assets: (self.near_asset_id.clone(), self.usdc_asset_id.clone()),
            decimals: (NEAR_DECIMALS, USDC_DECIMALS),
            amplification: Amplification::new(100),
            oracle_key: oracle_key_handle(&self.oracle_key),
            fee,
            initial_liquidity: (TokenAmount::new(liquidity.0), TokenAmount::new(liquidity.1)),
            creator_fee_payout_type: FeePayoutType::KeepBoth,
        }
    }

    /// The deployer creates a pool with `liquidity` that it deposits first
    async fn create_pool(&self, args: CreatePoolArgs) -> PoolId {
        let TestContext {
            dex_engine_contract,
            deployer,
            ft1,
            ft2,
            ft3,
            ..
        } = &self.context;
        let liquidity = (
            args.initial_liquidity.0.get(),
            args.initial_liquidity.1.get(),
        );
        let ft_of = |asset_id: &AssetId| {
            [ft1, ft2, ft3]
                .into_iter()
                .find(|ft| AssetId::Nep141(ft.id().clone()) == *asset_id)
                .unwrap()
        };
        deposit_ft(
            dex_engine_contract,
            deployer,
            ft_of(&args.assets.0),
            deployer,
            liquidity.0,
        )
        .await;
        deposit_ft(
            dex_engine_contract,
            deployer,
            ft_of(&args.assets.1),
            deployer,
            liquidity.1,
        )
        .await;
        let pool_id = PoolId::new(
            dex_view(dex_engine_contract, &self.dex_id, "get_pool_count", vec![]).await,
        );
        let result = execute_operations(
            deployer,
            dex_engine_contract,
            vec![dex_call(
                &self.dex_id,
                "create_pool",
                near_sdk::borsh::to_vec(&args).unwrap(),
                &[
                    (AssetId::Near, NearToken::from_millinear(50).as_yoctonear()),
                    (args.assets.0.clone(), liquidity.0),
                    (args.assets.1.clone(), liquidity.1),
                ],
            )],
        )
        .await;
        assert_success(&result).unwrap();
        pool_id
    }

    /// Swap in one pool, with operations that ask `oracle_key` for the prices of the pool first,
    /// which it responds to with `prices`
    #[allow(clippy::too_many_arguments)]
    async fn swap_with_prices(
        &self,
        trader: &Account,
        pool_id: PoolId,
        oracle_key: &SecretKey,
        prices: &PricesResponse,
        asset_in: &AssetId,
        amount: SwapRequestAmount,
        fee_discount: Option<SignedFeeDiscount>,
    ) -> ExecutionFinalResult {
        let pool = get_pool(&self.context.dex_engine_contract, &self.dex_id, pool_id).await;
        let asset_out = if *asset_in == pool.assets.0.asset_id {
            &pool.assets.1.asset_id
        } else {
            &pool.assets.0.asset_id
        };
        self.swap_on_routes(
            trader,
            SwapArgs {
                fee_discount,
                ..SwapArgs::single_pool(pool_id)
            },
            oracle_key,
            prices,
            asset_in,
            asset_out,
            amount,
        )
        .await
    }

    /// Swap on `swap_args.routes`, with operations that ask `oracle_key` for the prices of their
    /// pools first, which it responds to with `prices`
    #[allow(clippy::too_many_arguments)]
    async fn swap_on_routes(
        &self,
        trader: &Account,
        swap_args: SwapArgs,
        oracle_key: &SecretKey,
        prices: &PricesResponse,
        asset_in: &AssetId,
        asset_out: &AssetId,
        amount: SwapRequestAmount,
    ) -> ExecutionFinalResult {
        let TestContext {
            sandbox,
            dex_engine_contract,
            ..
        } = &self.context;
        let mut route_pools = Vec::new();
        for pool_id in swap_args.routes.iter().flat_map(|route| &route.pool_ids) {
            route_pools.push(get_pool(dex_engine_contract, &self.dex_id, *pool_id).await);
        }
        let request = near_sdk::borsh::to_vec(&PricesRequest::of_pools(
            route_pools
                .iter()
                .map(|pool| (&pool.assets.0.asset_id, &pool.assets.1.asset_id)),
        ))
        .unwrap();
        let waiting = trader
            .call(dex_engine_contract.id(), "execute_operations")
            .max_gas()
            .deposit(NearToken::from_yoctonear(1))
            .args_json(json!({
                "operations": [
                    Operation::OracleRequest {
                        oracle_key: oracle_key_handle(oracle_key),
                        dex_id: self.dex_id.clone(),
                        request: Base64VecU8(request.clone()),
                    },
                    Operation::SwapSimple {
                        dex_id: self.dex_id.clone(),
                        message: Base64VecU8(near_sdk::borsh::to_vec(&swap_args).unwrap()),
                        asset_in: asset_in.clone(),
                        asset_out: asset_out.clone(),
                        amount: SwapOperationAmount::Amount(amount),
                        constraint: None,
                    },
                ],
            }))
            .transact_async()
            .await
            .unwrap();
        let nonces = waiting_for_oracle_request(sandbox, &[&waiting], &request).await;
        let outcome = oracle_respond(
            sandbox,
            dex_engine_contract,
            oracle_key,
            &request,
            near_sdk::borsh::to_vec(prices).unwrap(),
            nonces,
        )
        .await;
        assert_gas_key_call_success(&outcome).unwrap();
        waiting.await.unwrap()
    }
}

/// What `near_amount` is worth in USDC at `price`, rounded down
fn usdc_at(price: Price, near_amount: u128) -> u128 {
    // In two steps, since the product doesn't fit in u128
    near_amount / 10u128.pow(12) * price.units() / 10u128.pow(24)
}

/// The amount after the fees of a pool whose fees are `fee_fractions`, each rounded down
fn after_fees(amount: u128, fee_fractions: &[u128]) -> u128 {
    amount
        - fee_fractions
            .iter()
            .map(|fraction| amount * fraction / 1_000_000)
            .sum::<u128>()
}

#[tokio::test]
async fn test_amp_swaps_at_oracle_price() {
    let test = setup_amp().await;
    let TestContext {
        dex_engine_contract,
        deployer,
        user1,
        user2,
        ft1,
        ft2,
        ..
    } = &test.context;
    let dex_id = &test.dex_id;
    let (near_asset_id, usdc_asset_id) = (&test.near_asset_id, &test.usdc_asset_id);
    // 0.1% to the pool, of which the protocol takes 20%
    let fee = FeeFraction::new(1_000);
    let fee_fractions = [800, 200];
    let liquidity = (near(1_000_000), usdc(3_750_000));

    // Only the owner of the dex creates pools
    deposit_near(dex_engine_contract, user1, NearToken::from_near(1)).await;
    deposit_ft(dex_engine_contract, deployer, ft1, user1, liquidity.0).await;
    deposit_ft(dex_engine_contract, deployer, ft2, user1, liquidity.1).await;
    let result = execute_operations(
        user1,
        dex_engine_contract,
        vec![dex_call(
            dex_id,
            "create_pool",
            near_sdk::borsh::to_vec(&test.create_pool_args(fee, liquidity)).unwrap(),
            &[
                (AssetId::Near, NearToken::from_millinear(50).as_yoctonear()),
                (near_asset_id.clone(), liquidity.0),
                (usdc_asset_id.clone(), liquidity.1),
            ],
        )],
    )
    .await;
    assert!(format!("{result:?}").contains("Only the owner of the dex can create pools"));

    let pool_id = test
        .create_pool(test.create_pool_args(fee, liquidity))
        .await;
    let pool = get_pool(dex_engine_contract, dex_id, pool_id).await;
    assert_eq!(pool.assets.0.balance, TokenAmount::new(liquidity.0));
    assert_eq!(pool.assets.1.balance, TokenAmount::new(liquidity.1));
    assert_eq!(pool.decimals, (NEAR_DECIMALS, USDC_DECIMALS));
    assert_eq!(pool.amplification, Amplification::new(100));
    assert_eq!(pool.oracle_key, oracle_key_handle(&test.oracle_key));
    assert_eq!(pool.last_oracle_price, None);
    assert_eq!(pool.fee, fee);
    assert_eq!(
        get_pool_shares(dex_engine_contract, dex_id, pool_id, deployer.id()).await,
        Some(INITIAL_SHARES)
    );

    // Swaps need the oracle's prices
    let result = execute_operations(
        user1,
        dex_engine_contract,
        vec![Operation::SwapSimple {
            dex_id: dex_id.clone(),
            message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs::single_pool(pool_id)).unwrap()),
            asset_in: near_asset_id.clone(),
            asset_out: usdc_asset_id.clone(),
            amount: SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(TokenAmount::new(
                near(10),
            ))),
            constraint: None,
        }],
    )
    .await;
    assert!(format!("{result:?}").contains("must start with an oracle request of the prices of"));

    // A swap rates the pool at the median of the prices, so a balanced pool trades close to it
    let user1_balance = async |asset_id: &AssetId| {
        inner_balance(
            dex_engine_contract,
            AccountOrDexId::Account(user1.id().clone()),
            asset_id,
        )
        .await
    };
    let near_in = near(100);
    let usdc_before = user1_balance(usdc_asset_id).await;
    let median_price = agreeing_prices()
        .median_price(&pool.prices_request(), near_asset_id, usdc_asset_id)
        .unwrap();
    let expected_usdc_out = pool
        .rated(median_price)
        .exact_in_amount_out(
            AssetIndex::First,
            TokenAmount::new(after_fees(near_in, &fee_fractions)),
        )
        .get();
    let result = test
        .swap_with_prices(
            user1,
            pool_id,
            &test.oracle_key,
            &agreeing_prices(),
            near_asset_id,
            SwapRequestAmount::ExactIn(TokenAmount::new(near_in)),
            None,
        )
        .await;
    assert_success(&result).unwrap();
    assert_eq!(
        user1_balance(usdc_asset_id).await,
        usdc_before + expected_usdc_out
    );
    // What it's worth at the price, less 0.1% of fees, give or take a little for the curve
    let usdc_worth = |near_amount: u128| usdc_at(median_price, near_amount);
    let worth_after_fees = usdc_worth(near_in) * 999 / 1000;
    assert!(expected_usdc_out.abs_diff(worth_after_fees) < worth_after_fees / 10_000);
    let swap_prices = amp_events(&result)
        .into_iter()
        .filter_map(|event| match event {
            AmpDexEvent::Swap { price, .. } => Some(price),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(swap_prices, vec![median_price]);
    // Indexers see the price that the pool was last rated at in its updates
    let updated_prices = amp_events(&result)
        .into_iter()
        .filter_map(|event| match event {
            AmpDexEvent::PoolUpdated { pool, .. } => Some(pool.last_oracle_price),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(updated_prices, vec![Some(median_price)]);
    let pool_after_swap = get_pool(dex_engine_contract, dex_id, pool_id).await;
    assert_eq!(pool_after_swap.last_oracle_price, Some(median_price));
    // The creator's fee is held for it, out of the pool's balance
    assert_eq!(
        pool_after_swap.assets.0.balance,
        TokenAmount::new(liquidity.0 + after_fees(near_in, &fee_fractions))
    );
    assert_eq!(
        pool_after_swap.assets.1.balance,
        TokenAmount::new(liquidity.1 - expected_usdc_out)
    );
    assert_eq!(
        get_pending_fees(
            dex_engine_contract,
            dex_id,
            &PROTOCOL_FEE_RECEIVER_ID.to_owned(),
            near_asset_id
        )
        .await,
        near_in * 200 / 1_000_000
    );

    // Swaps are denied if the prices of exchanges are more than 0.5% apart, there are less than 3
    // exchanges, or an exchange doesn't price every requested asset
    for (prices, error) in [
        (
            prices(&[
                ("binance", &["3.75", "1"]),
                ("kraken", &["3.7688", "1"]),
                ("okx", &["3.76", "1"]),
            ]),
            "Prices of exchanges are more than 0.5% apart: from 3.75 to 3.7688",
        ),
        (
            prices(&[("binance", &["3.75", "1"]), ("okx", &["3.75", "1"])]),
            "Oracle has prices of 2 exchanges, but at least 3 are needed",
        ),
        (
            prices(&[
                ("binance", &["3.75", "1"]),
                ("kraken", &["3.75"]),
                ("okx", &["3.75", "1"]),
            ]),
            "Oracle has 1 prices of kraken, but 2 assets were requested",
        ),
    ] {
        let near_before = user1_balance(near_asset_id).await;
        let result = test
            .swap_with_prices(
                user1,
                pool_id,
                &test.oracle_key,
                &prices,
                near_asset_id,
                SwapRequestAmount::ExactIn(TokenAmount::new(near_in)),
                None,
            )
            .await;
        assert!(format!("{result:?}").contains(error), "{result:?}");
        assert_eq!(user1_balance(near_asset_id).await, near_before);
    }
    assert_eq!(
        get_pool(dex_engine_contract, dex_id, pool_id).await,
        pool_after_swap
    );

    // Exact out takes what the curve needs at the median price, with the fees on top
    let near_out = near(50);
    let near_before = user1_balance(near_asset_id).await;
    let usdc_before = user1_balance(usdc_asset_id).await;
    let expected_usdc_in = pool_after_swap
        .rated(median_price)
        .exact_out_amount_in(
            AssetIndex::Second,
            TokenAmount::new(near_out),
            pool.swap_fees().total(),
        )
        .get();
    let result = test
        .swap_with_prices(
            user1,
            pool_id,
            &test.oracle_key,
            &agreeing_prices(),
            usdc_asset_id,
            SwapRequestAmount::ExactOut(TokenAmount::new(near_out)),
            None,
        )
        .await;
    assert_success(&result).unwrap();
    assert_eq!(user1_balance(near_asset_id).await, near_before + near_out);
    assert_eq!(
        user1_balance(usdc_asset_id).await,
        usdc_before - expected_usdc_in
    );
    let worth_with_fees = usdc_worth(near_out) * 1000 / 999;
    assert!(expected_usdc_in.abs_diff(worth_with_fees) < worth_with_fees / 10_000);

    // The prices of another oracle aren't the pool's
    let other_oracle_key = create_oracle_key(
        dex_engine_contract,
        user2,
        KeyType::ED25519,
        NearToken::from_near(2),
    )
    .await;
    let result = test
        .swap_with_prices(
            user1,
            pool_id,
            &other_oracle_key,
            &agreeing_prices(),
            near_asset_id,
            SwapRequestAmount::ExactIn(TokenAmount::new(near_in)),
            None,
        )
        .await;
    assert!(format!("{result:?}").contains("must start with an oracle request of the prices of"));

    // The owner changes the amplification and the oracle
    for (method, args, error) in [
        (
            "set_amplification",
            near_sdk::borsh::to_vec(&SetAmplificationArgs {
                pool_id,
                amplification: Amplification::new(1_000),
            })
            .unwrap(),
            "Only the owner of the dex can set the amplification",
        ),
        (
            "set_oracle_key",
            near_sdk::borsh::to_vec(&SetOracleKeyArgs {
                pool_id,
                oracle_key: oracle_key_handle(&other_oracle_key),
            })
            .unwrap(),
            "Only the owner of the dex can set the oracle key",
        ),
    ] {
        let result = execute_operations(
            user1,
            dex_engine_contract,
            vec![dex_call(dex_id, method, args.clone(), &[])],
        )
        .await;
        assert!(format!("{result:?}").contains(error));
        let result = execute_operations(
            deployer,
            dex_engine_contract,
            vec![dex_call(dex_id, method, args, &[])],
        )
        .await;
        assert_success(&result).unwrap();
    }
    let result = execute_operations(
        deployer,
        dex_engine_contract,
        vec![dex_call(
            dex_id,
            "set_amplification",
            near_sdk::borsh::to_vec(&SetAmplificationArgs {
                pool_id,
                amplification: Amplification::new(1_000_001),
            })
            .unwrap(),
            &[],
        )],
    )
    .await;
    assert!(format!("{result:?}").contains("Amplification must be from 1 to 1000000"));
    let pool = get_pool(dex_engine_contract, dex_id, pool_id).await;
    assert_eq!(pool.amplification, Amplification::new(1_000));
    assert_eq!(pool.oracle_key, oracle_key_handle(&other_oracle_key));
    let usdc_before = user1_balance(usdc_asset_id).await;
    let expected_usdc_out = pool
        .rated(median_price)
        .exact_in_amount_out(
            AssetIndex::First,
            TokenAmount::new(after_fees(near_in, &fee_fractions)),
        )
        .get();
    let result = test
        .swap_with_prices(
            user1,
            pool_id,
            &other_oracle_key,
            &agreeing_prices(),
            near_asset_id,
            SwapRequestAmount::ExactIn(TokenAmount::new(near_in)),
            None,
        )
        .await;
    assert_success(&result).unwrap();
    assert_eq!(
        user1_balance(usdc_asset_id).await,
        usdc_before + expected_usdc_out
    );
    let result = test
        .swap_with_prices(
            user1,
            pool_id,
            &test.oracle_key,
            &agreeing_prices(),
            near_asset_id,
            SwapRequestAmount::ExactIn(TokenAmount::new(near_in)),
            None,
        )
        .await;
    assert!(format!("{result:?}").contains("must start with an oracle request of the prices of"));
}

#[tokio::test]
async fn test_amp_fees_and_liquidity() {
    let test = setup_amp().await;
    let TestContext {
        dex_engine_contract,
        deployer,
        user1,
        user2,
        ft1,
        ft2,
        ..
    } = &test.context;
    let dex_id = &test.dex_id;
    let (near_asset_id, usdc_asset_id) = (&test.near_asset_id, &test.usdc_asset_id);
    // 0.3% to the pool, of which the protocol takes 20%
    let liquidity = (near(1_000_000), usdc(3_750_000));
    let pool_id = test
        .create_pool(test.create_pool_args(FeeFraction::new(3_000), liquidity))
        .await;

    // user2 provides a third of the liquidity, in the ratio of the pool, and gets back the USDC
    // that's more than the ratio
    deposit_near(dex_engine_contract, user2, NearToken::from_near(1)).await;
    deposit_ft(dex_engine_contract, deployer, ft1, user2, liquidity.0 / 2).await;
    deposit_ft(
        dex_engine_contract,
        deployer,
        ft2,
        user2,
        liquidity.1 / 2 + usdc(1),
    )
    .await;
    let result = execute_operations(
        user2,
        dex_engine_contract,
        vec![
            dex_call(
                dex_id,
                "register_liquidity",
                near_sdk::borsh::to_vec(&RegisterLiquidityArgs { pool_id }).unwrap(),
                &[(AssetId::Near, NearToken::from_millinear(10).as_yoctonear())],
            ),
            dex_call(
                dex_id,
                "add_liquidity",
                near_sdk::borsh::to_vec(&AddLiquidityArgs {
                    pool_id,
                    min_shares_received: None,
                })
                .unwrap(),
                &[
                    (near_asset_id.clone(), liquidity.0 / 2),
                    (usdc_asset_id.clone(), liquidity.1 / 2 + usdc(1)),
                ],
            ),
        ],
    )
    .await;
    assert_success(&result).unwrap();
    let added = amp_events(&result)
        .into_iter()
        .find_map(|event| match event {
            AmpDexEvent::LiquidityAdded {
                added_amount_0,
                added_amount_1,
                minted_shares,
                ..
            } => Some((added_amount_0.get(), added_amount_1.get(), minted_shares)),
            _ => None,
        })
        .unwrap();
    let (_, added_usdc, user2_shares) = added;
    assert!(INITIAL_SHARES.get() / 2 - user2_shares.get() < INITIAL_SHARES.get() / 1_000_000);
    assert!(added_usdc <= liquidity.1 / 2 + 1);
    assert_eq!(
        get_pool_shares(dex_engine_contract, dex_id, pool_id, user2.id()).await,
        Some(user2_shares)
    );
    // user2 takes its fees in USDC, and the creator keeps them in the asset they're paid in
    let result = execute_operations(
        user2,
        dex_engine_contract,
        vec![dex_call(
            dex_id,
            "set_liquidity_fee_payout",
            near_sdk::borsh::to_vec(&SetLiquidityFeePayoutArgs {
                pool_id,
                payout_type: FeePayoutType::ConvertToSecondAsset,
            })
            .unwrap(),
            &[(AssetId::Near, NearToken::from_millinear(10).as_yoctonear())],
        )],
    )
    .await;
    assert_success(&result).unwrap();

    let near_in = near(1_000);
    deposit_ft(dex_engine_contract, deployer, ft1, user1, near_in).await;
    let pool_before_swap = get_pool(dex_engine_contract, dex_id, pool_id).await;
    let result = test
        .swap_with_prices(
            user1,
            pool_id,
            &test.oracle_key,
            &agreeing_prices(),
            near_asset_id,
            SwapRequestAmount::ExactIn(TokenAmount::new(near_in)),
            None,
        )
        .await;
    assert_success(&result).unwrap();
    let pool_fee = near_in * 2_400 / 1_000_000;
    let protocol_fee = near_in * 600 / 1_000_000;
    assert_eq!(
        get_pending_fees(
            dex_engine_contract,
            dex_id,
            &PROTOCOL_FEE_RECEIVER_ID.to_owned(),
            near_asset_id
        )
        .await,
        protocol_fee
    );
    let swaps = amp_events(&result)
        .into_iter()
        .filter_map(|event| match event {
            AmpDexEvent::Swap { fees_breakdown, .. } => Some(fees_breakdown),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [fees_breakdown] = swaps.as_slice() else {
        panic!("Expected one swap: {swaps:#?}");
    };
    let fee_taken = |receiver: &FeeReceiver, asset_id: &AssetId| {
        fees_breakdown
            .iter()
            .find_map(|(fee_receiver, fee_asset_id, amount)| {
                (fee_receiver == receiver && fee_asset_id == asset_id).then_some(amount.get())
            })
            .unwrap()
    };
    assert_eq!(
        fee_taken(&FeeReceiver::Protocol, near_asset_id),
        protocol_fee
    );
    // user2 has a third of the shares, and converted its third of the pool fee to USDC at about
    // the median price, which the pool kept the rest of
    let user2_fee_in_near = pool_fee - fee_taken(&FeeReceiver::Pool, near_asset_id);
    let user2_fee_in_usdc = fee_taken(&FeeReceiver::Pool, usdc_asset_id);
    let user2_part_of_shares =
        user2_shares.get() as f64 / pool_before_swap.total_shares.get() as f64;
    assert!(
        (user2_fee_in_near as f64 / (pool_fee as f64 * user2_part_of_shares) - 1.0).abs() < 1e-9
    );
    let median_price = agreeing_prices()
        .median_price(
            &pool_before_swap.prices_request(),
            near_asset_id,
            usdc_asset_id,
        )
        .unwrap();
    let user2_fee_worth = usdc_at(median_price, user2_fee_in_near);
    assert!(user2_fee_in_usdc <= user2_fee_worth);
    assert!(user2_fee_in_usdc >= user2_fee_worth * 999 / 1000);
    let liquidity_position = async |account: &Account| {
        dex_view::<Option<amp_dex_types::LiquidityPositionView>>(
            dex_engine_contract,
            dex_id,
            "get_liquidity_position",
            near_sdk::borsh::to_vec(&GetLiquidityPositionArgs {
                pool_id,
                account_id: account.id().clone(),
            })
            .unwrap(),
        )
        .await
        .unwrap()
    };
    let user2_position = liquidity_position(user2).await;
    assert_eq!(user2_position.uncollected_fees.0, TokenAmount::ZERO);
    assert!(
        user2_position
            .uncollected_fees
            .1
            .get()
            .abs_diff(user2_fee_in_usdc)
            <= 1
    );

    // The creator keeps its two thirds of the pool fee in NEAR, and fees don't mint shares
    let deployer_position = liquidity_position(deployer).await;
    let deployer_part_of_shares =
        INITIAL_SHARES.get() as f64 / pool_before_swap.total_shares.get() as f64;
    assert!(
        (deployer_position.uncollected_fees.0.get() as f64
            / (pool_fee as f64 * deployer_part_of_shares)
            - 1.0)
            .abs()
            < 1e-9
    );
    assert_eq!(deployer_position.uncollected_fees.1, TokenAmount::ZERO);
    let pool_after_swap = get_pool(dex_engine_contract, dex_id, pool_id).await;
    assert_eq!(pool_after_swap.total_shares, pool_before_swap.total_shares);

    // user2 takes its liquidity out to its wallet, and its fees to its balance on the engine
    let wallet_balance = async |ft: &Contract| {
        ft.view("ft_balance_of")
            .args_json(json!({ "account_id": user2.id() }))
            .await
            .unwrap()
            .json::<U128>()
            .unwrap()
            .0
    };
    let wallet_before = (wallet_balance(ft1).await, wallet_balance(ft2).await);
    let usdc_before = inner_balance(
        dex_engine_contract,
        AccountOrDexId::Account(user2.id().clone()),
        usdc_asset_id,
    )
    .await;
    let result = execute_operations(
        user2,
        dex_engine_contract,
        vec![
            dex_call(
                dex_id,
                "remove_liquidity",
                near_sdk::borsh::to_vec(&RemoveLiquidityArgs {
                    pool_id,
                    shares_to_remove: None,
                    min_assets_received: None,
                })
                .unwrap(),
                &[],
            ),
            dex_call(
                dex_id,
                "withdraw_fees",
                near_sdk::borsh::to_vec(&WithdrawFeesArgs {
                    assets: vec![near_asset_id.clone(), usdc_asset_id.clone()],
                })
                .unwrap(),
                &[],
            ),
        ],
    )
    .await;
    assert_success(&result).unwrap();
    let removed = amp_events(&result)
        .into_iter()
        .find_map(|event| match event {
            AmpDexEvent::LiquidityRemoved {
                removed_amount_0,
                removed_amount_1,
                burned_shares,
                ..
            } => Some((
                removed_amount_0.get(),
                removed_amount_1.get(),
                burned_shares,
            )),
            _ => None,
        })
        .unwrap();
    assert_eq!(removed.2, user2_shares);
    let user2_part_of_shares =
        user2_shares.get() as f64 / pool_after_swap.total_shares.get() as f64;
    for (removed_amount, balance) in [
        (removed.0, pool_after_swap.assets.0.balance.get()),
        (removed.1, pool_after_swap.assets.1.balance.get()),
    ] {
        assert!(
            (removed_amount as f64 / (balance as f64 * user2_part_of_shares) - 1.0).abs() < 1e-9
        );
    }
    assert_eq!(wallet_balance(ft1).await, wallet_before.0 + removed.0);
    assert_eq!(wallet_balance(ft2).await, wallet_before.1 + removed.1);
    assert_eq!(
        inner_balance(
            dex_engine_contract,
            AccountOrDexId::Account(user2.id().clone()),
            usdc_asset_id,
        )
        .await,
        usdc_before + user2_position.uncollected_fees.1.get()
    );
    assert_eq!(
        get_pool_shares(dex_engine_contract, dex_id, pool_id, user2.id()).await,
        Some(ProviderShares::ZERO)
    );
}

/// The fee discount signed by `signer` for the amp dex `dex_id`
fn sign_fee_discount(
    discount: FeeDiscount,
    signer: &SecretKey,
    chain_id: &str,
    engine_contract_id: &AccountId,
    dex_id: &DexId,
) -> SignedFeeDiscount {
    let signature_payload = FeeDiscountSignaturePayload {
        chain_id: chain_id.to_string(),
        engine_contract_id: engine_contract_id.clone(),
        dex_id: dex_id.clone(),
        discount: discount.clone(),
    };
    let Signature::ED25519(signature) = signer.sign(&near_sdk::env::sha256_array(
        near_sdk::borsh::to_vec(&signature_payload).unwrap(),
    )) else {
        panic!("Fee discounts are signed with ed25519 keys in this test");
    };
    SignedFeeDiscount {
        discount,
        signature: signature.to_bytes().to_vec(),
    }
}

#[tokio::test]
async fn test_amp_fee_discount() {
    let test = setup_amp().await;
    let TestContext {
        sandbox,
        dex_engine_contract,
        deployer,
        user1,
        ft1,
        ..
    } = &test.context;
    let dex_id = &test.dex_id;
    let (near_asset_id, usdc_asset_id) = (&test.near_asset_id, &test.usdc_asset_id);
    let liquidity = (near(1_000_000), usdc(3_750_000));
    let pool_id = test
        .create_pool(test.create_pool_args(FeeFraction::new(10_000), liquidity))
        .await;
    let fee_discount_signer = SecretKey::from_seed(KeyType::ED25519, "fee-discount-signer");
    let set_fee_discount_signer = async |account: &Account| {
        execute_operations(
            account,
            dex_engine_contract,
            vec![dex_call(
                dex_id,
                "set_fee_discount_signer",
                near_sdk::borsh::to_vec(&SetFeeDiscountSignerArgs {
                    fee_discount_signer_public_key: Some(
                        fee_discount_signer
                            .public_key()
                            .to_string()
                            .parse()
                            .unwrap(),
                    ),
                })
                .unwrap(),
                &[],
            )],
        )
        .await
    };
    let result = set_fee_discount_signer(user1).await;
    assert!(
        format!("{result:?}").contains("Only the owner of the dex can set the fee discount signer")
    );
    let result = set_fee_discount_signer(deployer).await;
    assert_success(&result).unwrap();

    let near_in = near(100);
    deposit_ft(dex_engine_contract, deployer, ft1, user1, near_in * 3).await;
    let chain_id = sandbox.status().await.unwrap().chain_id;
    let full_discount = FeeDiscount {
        route: SwapArgs::single_pool(pool_id).routes,
        asset_in: near_asset_id.clone(),
        asset_out: usdc_asset_id.clone(),
        trader_id: Some(user1.id().clone()),
        amount: SwapRequestAmount::ExactIn(TokenAmount::new(near_in)),
        fee_discount: FULL_FEE_DISCOUNT,
        expiration_block_height: sandbox.view_block().await.unwrap().height() + 1_000,
    };
    let signed = |discount: FeeDiscount| {
        sign_fee_discount(
            discount,
            &fee_discount_signer,
            &chain_id,
            dex_engine_contract.id(),
            dex_id,
        )
    };

    // A discount for another pool doesn't work
    let result = test
        .swap_with_prices(
            user1,
            pool_id,
            &test.oracle_key,
            &agreeing_prices(),
            near_asset_id,
            SwapRequestAmount::ExactIn(TokenAmount::new(near_in)),
            Some(signed(FeeDiscount {
                route: SwapArgs::single_pool(PoolId::new(pool_id.get() + 1)).routes,
                ..full_discount.clone()
            })),
        )
        .await;
    assert!(format!("{result:?}").contains("Fee discount is for another route"));

    // The full discount waives the pool and protocol fees
    let pool = get_pool(dex_engine_contract, dex_id, pool_id).await;
    let usdc_before = inner_balance(
        dex_engine_contract,
        AccountOrDexId::Account(user1.id().clone()),
        usdc_asset_id,
    )
    .await;
    let result = test
        .swap_with_prices(
            user1,
            pool_id,
            &test.oracle_key,
            &agreeing_prices(),
            near_asset_id,
            SwapRequestAmount::ExactIn(TokenAmount::new(near_in)),
            Some(signed(full_discount)),
        )
        .await;
    assert_success(&result).unwrap();
    let expected_usdc_out = pool
        .rated(
            agreeing_prices()
                .median_price(&pool.prices_request(), near_asset_id, usdc_asset_id)
                .unwrap(),
        )
        .exact_in_amount_out(AssetIndex::First, TokenAmount::new(near_in))
        .get();
    assert_eq!(
        inner_balance(
            dex_engine_contract,
            AccountOrDexId::Account(user1.id().clone()),
            usdc_asset_id,
        )
        .await,
        usdc_before + expected_usdc_out
    );
    assert_eq!(
        get_pending_fees(
            dex_engine_contract,
            dex_id,
            &PROTOCOL_FEE_RECEIVER_ID.to_owned(),
            near_asset_id
        )
        .await,
        0
    );
}

#[tokio::test]
async fn test_amp_routes() {
    let test = setup_amp().await;
    let TestContext {
        dex_engine_contract,
        deployer,
        user1,
        user2,
        ft1,
        ft3,
        ..
    } = &test.context;
    let dex_id = &test.dex_id;
    let (near_asset_id, usdc_asset_id) = (&test.near_asset_id, &test.usdc_asset_id);
    let xaut_asset_id = &AssetId::Nep141(ft3.id().clone());
    // 0.1% to the pool, of which the protocol takes 20%
    let fee = FeeFraction::new(1_000);
    let fee_fractions = [800, 200];
    let near_usdc_liquidity = (near(1_000_000), usdc(3_750_000));
    let big_pool_id = test
        .create_pool(test.create_pool_args(fee, near_usdc_liquidity))
        .await;
    let small_pool_id = test
        .create_pool(
            test.create_pool_args(fee, (near_usdc_liquidity.0 / 4, near_usdc_liquidity.1 / 4)),
        )
        .await;
    let xaut_pool_id = test
        .create_pool(CreatePoolArgs {
            assets: (usdc_asset_id.clone(), xaut_asset_id.clone()),
            decimals: (USDC_DECIMALS, XAUT_DECIMALS),
            ..test.create_pool_args(fee, (usdc(4_000_000), xaut(1_000)))
        })
        .await;
    deposit_ft(dex_engine_contract, deployer, ft1, user1, near(1_000)).await;
    let user1_balance = async |asset_id: &AssetId| {
        inner_balance(
            dex_engine_contract,
            AccountOrDexId::Account(user1.id().clone()),
            asset_id,
        )
        .await
    };
    let route = |fraction: u32, pool_ids: &[PoolId]| SwapRoute {
        fraction: RouteFraction::new(fraction),
        pool_ids: pool_ids.to_vec(),
    };
    let swap = async |routes: Vec<SwapRoute>,
                      prices: &PricesResponse,
                      asset_out: &AssetId,
                      amount: SwapRequestAmount| {
        test.swap_on_routes(
            user1,
            SwapArgs {
                routes,
                fee_discount: None,
            },
            &test.oracle_key,
            prices,
            near_asset_id,
            asset_out,
            amount,
        )
        .await
    };

    // A swap splits its amount between routes, and the first one also swaps what the rounding of
    // the fractions leaves
    let near_in = near(100) + 1;
    let big_pool = get_pool(dex_engine_contract, dex_id, big_pool_id).await;
    let small_pool = get_pool(dex_engine_contract, dex_id, small_pool_id).await;
    let median_price = agreeing_prices()
        .median_price(&big_pool.prices_request(), near_asset_id, usdc_asset_id)
        .unwrap();
    let expected_usdc_out = big_pool
        .rated(median_price)
        .exact_in_amount_out(
            AssetIndex::First,
            TokenAmount::new(after_fees(near(80) + 1, &fee_fractions)),
        )
        .get()
        + small_pool
            .rated(median_price)
            .exact_in_amount_out(
                AssetIndex::First,
                TokenAmount::new(after_fees(near(20), &fee_fractions)),
            )
            .get();
    let (near_before, usdc_before) = (
        user1_balance(near_asset_id).await,
        user1_balance(usdc_asset_id).await,
    );
    let result = swap(
        vec![
            route(800_000, &[big_pool_id]),
            route(200_000, &[small_pool_id]),
        ],
        &agreeing_prices(),
        usdc_asset_id,
        SwapRequestAmount::ExactIn(TokenAmount::new(near_in)),
    )
    .await;
    assert_success(&result).unwrap();
    assert_eq!(user1_balance(near_asset_id).await, near_before - near_in);
    assert_eq!(
        user1_balance(usdc_asset_id).await,
        usdc_before + expected_usdc_out
    );
    let updated_pool_ids = amp_events(&result)
        .into_iter()
        .filter_map(|event| match event {
            AmpDexEvent::PoolUpdated { pool_id, .. } => Some(pool_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(updated_pool_ids, vec![big_pool_id, small_pool_id]);

    // One oracle response prices every pool of a route through many pools, with the prices of
    // their assets in the order the pools have them
    let route_request = PricesRequest {
        asset_ids: vec![
            near_asset_id.clone(),
            usdc_asset_id.clone(),
            xaut_asset_id.clone(),
        ],
    };
    let route_prices = prices(&[
        ("binance", &["3.75", "1.0005", "4000"]),
        ("kraken", &["3.748", "1", "3998"]),
        ("okx", &["3.7518", "1.001", "4001"]),
    ]);
    let near_price = route_prices
        .median_price(&route_request, near_asset_id, usdc_asset_id)
        .unwrap();
    let usdc_price = route_prices
        .median_price(&route_request, usdc_asset_id, xaut_asset_id)
        .unwrap();
    let near_to_xaut = || vec![route(1_000_000, &[big_pool_id, xaut_pool_id])];

    // Exact in swaps the output of each pool in the next one
    let near_in = near(100);
    let big_pool = get_pool(dex_engine_contract, dex_id, big_pool_id).await;
    let xaut_pool = get_pool(dex_engine_contract, dex_id, xaut_pool_id).await;
    let usdc_between = big_pool
        .rated(near_price)
        .exact_in_amount_out(
            AssetIndex::First,
            TokenAmount::new(after_fees(near_in, &fee_fractions)),
        )
        .get();
    let expected_xaut_out = xaut_pool
        .rated(usdc_price)
        .exact_in_amount_out(
            AssetIndex::First,
            TokenAmount::new(after_fees(usdc_between, &fee_fractions)),
        )
        .get();
    let (near_before, xaut_before) = (
        user1_balance(near_asset_id).await,
        user1_balance(xaut_asset_id).await,
    );
    let result = swap(
        near_to_xaut(),
        &route_prices,
        xaut_asset_id,
        SwapRequestAmount::ExactIn(TokenAmount::new(near_in)),
    )
    .await;
    assert_success(&result).unwrap();
    assert_eq!(user1_balance(near_asset_id).await, near_before - near_in);
    assert_eq!(
        user1_balance(xaut_asset_id).await,
        xaut_before + expected_xaut_out
    );
    // About 100 NEAR at 3.748 USDC, less 0.1% of fees in each pool, at 3998 USDC per XAUT
    assert!(expected_xaut_out.abs_diff(93_560) < 10);
    let swaps = amp_events(&result)
        .into_iter()
        .filter_map(|event| match event {
            AmpDexEvent::Swap {
                pool_id,
                amount_out,
                price,
                ..
            } => Some((pool_id, amount_out.get(), price)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        swaps,
        vec![
            (big_pool_id, usdc_between, near_price),
            (xaut_pool_id, expected_xaut_out, usdc_price),
        ]
    );

    // Exact out swaps from the last pool back, each one getting what the next one puts in
    let xaut_out = xaut(1) / 10;
    let big_pool = get_pool(dex_engine_contract, dex_id, big_pool_id).await;
    let xaut_pool = get_pool(dex_engine_contract, dex_id, xaut_pool_id).await;
    let usdc_between = xaut_pool.rated(usdc_price).exact_out_amount_in(
        AssetIndex::First,
        TokenAmount::new(xaut_out),
        xaut_pool.swap_fees().total(),
    );
    let expected_near_in = big_pool
        .rated(near_price)
        .exact_out_amount_in(
            AssetIndex::First,
            usdc_between,
            big_pool.swap_fees().total(),
        )
        .get();
    let (near_before, xaut_before) = (
        user1_balance(near_asset_id).await,
        user1_balance(xaut_asset_id).await,
    );
    let result = swap(
        near_to_xaut(),
        &route_prices,
        xaut_asset_id,
        SwapRequestAmount::ExactOut(TokenAmount::new(xaut_out)),
    )
    .await;
    assert_success(&result).unwrap();
    assert_eq!(
        user1_balance(near_asset_id).await,
        near_before - expected_near_in
    );
    assert_eq!(user1_balance(xaut_asset_id).await, xaut_before + xaut_out);

    // Routes are denied if their pools ask different oracles, their fractions don't add up to
    // 100%, they don't end in the asset out, or the oracle doesn't price all of their assets
    let other_oracle_key = create_oracle_key(
        dex_engine_contract,
        user2,
        KeyType::ED25519,
        NearToken::from_near(2),
    )
    .await;
    let other_oracle_pool_id = test
        .create_pool(CreatePoolArgs {
            oracle_key: oracle_key_handle(&other_oracle_key),
            ..test.create_pool_args(fee, near_usdc_liquidity)
        })
        .await;
    for (routes, prices, asset_out, error) in [
        (
            vec![
                route(500_000, &[big_pool_id]),
                route(500_000, &[other_oracle_pool_id]),
            ],
            agreeing_prices(),
            usdc_asset_id,
            "Pools of a swap must ask the same oracle",
        ),
        (
            vec![
                route(800_000, &[big_pool_id]),
                route(100_000, &[small_pool_id]),
            ],
            agreeing_prices(),
            usdc_asset_id,
            "Route fractions must add up to 100%",
        ),
        (
            vec![route(1_000_000, &[big_pool_id])],
            agreeing_prices(),
            xaut_asset_id,
            "Route doesn't end in asset out",
        ),
        (
            near_to_xaut(),
            agreeing_prices(),
            xaut_asset_id,
            "Oracle has 2 prices of binance, but 3 assets were requested",
        ),
    ] {
        let near_before = user1_balance(near_asset_id).await;
        let result = swap(
            routes,
            &prices,
            asset_out,
            SwapRequestAmount::ExactIn(TokenAmount::new(near(10))),
        )
        .await;
        assert!(format!("{result:?}").contains(error), "{result:?}");
        assert_eq!(user1_balance(near_asset_id).await, near_before);
    }
}

#[tokio::test]
async fn test_amp_complex_route() {
    let test = setup_amp().await;
    let TestContext {
        dex_engine_contract,
        deployer,
        user1,
        ft1,
        ft3,
        ..
    } = &test.context;
    let dex_id = &test.dex_id;
    let (near_asset_id, usdc_asset_id) = (&test.near_asset_id, &test.usdc_asset_id);
    let xaut_asset_id = &AssetId::Nep141(ft3.id().clone());
    // 0.1% to the pool, of which the protocol takes 20%
    let fee = FeeFraction::new(1_000);
    let fee_fractions = [800, 200];
    let create_pool = async |assets: (&AssetId, &AssetId), decimals, liquidity| {
        test.create_pool(CreatePoolArgs {
            assets: (assets.0.clone(), assets.1.clone()),
            decimals,
            ..test.create_pool_args(fee, liquidity)
        })
        .await
    };
    let near_usdc = (near_asset_id, usdc_asset_id);
    let big_pool_id = create_pool(
        near_usdc,
        (NEAR_DECIMALS, USDC_DECIMALS),
        (near(1_000_000), usdc(3_750_000)),
    )
    .await;
    let small_pool_id = create_pool(
        near_usdc,
        (NEAR_DECIMALS, USDC_DECIMALS),
        (near(250_000), usdc(937_500)),
    )
    .await;
    let usdc_xaut_pool_id = create_pool(
        (usdc_asset_id, xaut_asset_id),
        (USDC_DECIMALS, XAUT_DECIMALS),
        (usdc(4_000_000), xaut(1_000)),
    )
    .await;
    let near_xaut_pool_id = create_pool(
        (near_asset_id, xaut_asset_id),
        (NEAR_DECIMALS, XAUT_DECIMALS),
        (near(100_000), xaut(375) / 4),
    )
    .await;
    deposit_ft(dex_engine_contract, deployer, ft1, user1, near(3_000)).await;
    let user1_balance = async |asset_id: &AssetId| {
        inner_balance(
            dex_engine_contract,
            AccountOrDexId::Account(user1.id().clone()),
            asset_id,
        )
        .await
    };
    let pool = async |pool_id| get_pool(dex_engine_contract, dex_id, pool_id).await;
    // Half to XAUT through USDC of the big pool, 30% through USDC of the small pool and the same
    // USDC/XAUT pool, and the rest directly
    let routes = vec![
        SwapRoute {
            fraction: RouteFraction::new(500_000),
            pool_ids: vec![big_pool_id, usdc_xaut_pool_id],
        },
        SwapRoute {
            fraction: RouteFraction::new(300_000),
            pool_ids: vec![small_pool_id, usdc_xaut_pool_id],
        },
        SwapRoute {
            fraction: RouteFraction::new(200_000),
            pool_ids: vec![near_xaut_pool_id],
        },
    ];
    // In the order the pools have them: NEAR, USDC, XAUT
    let route_prices = prices(&[
        ("binance", &["3.75", "1.0005", "4000"]),
        ("kraken", &["3.748", "1", "3998"]),
        ("okx", &["3.7518", "1.001", "4001"]),
    ]);
    let route_request = PricesRequest {
        asset_ids: vec![
            near_asset_id.clone(),
            usdc_asset_id.clone(),
            xaut_asset_id.clone(),
        ],
    };
    let median = |base: &AssetId, quote: &AssetId| {
        route_prices
            .median_price(&route_request, base, quote)
            .unwrap()
    };
    let (near_in_usdc, usdc_in_xaut, near_in_xaut) = (
        median(near_asset_id, usdc_asset_id),
        median(usdc_asset_id, xaut_asset_id),
        median(near_asset_id, xaut_asset_id),
    );
    // The pool after a swap of `amount_in` of its first asset for `amount_out` of its second, whose
    // pool fee goes to the creator, who keeps both assets
    let after_swap = |pool: &PoolView, amount_in: u128, amount_out: u128| PoolView {
        assets: (
            AssetWithBalance {
                balance: TokenAmount::new(
                    pool.assets.0.balance.get() + after_fees(amount_in, &fee_fractions),
                ),
                ..pool.assets.0.clone()
            },
            AssetWithBalance {
                balance: TokenAmount::new(pool.assets.1.balance.get() - amount_out),
                ..pool.assets.1.clone()
            },
        ),
        ..pool.clone()
    };
    let swap = async |amount: SwapRequestAmount| {
        test.swap_on_routes(
            user1,
            SwapArgs {
                routes: routes.clone(),
                fee_discount: None,
            },
            &test.oracle_key,
            &route_prices,
            near_asset_id,
            xaut_asset_id,
            amount,
        )
        .await
    };

    // Exact in swaps each route after the one before it, so the second route swaps on the USDC/XAUT
    // pool as the first left it. The first route also swaps the 2 yocto that the fractions round
    // off.
    let near_in = near(1_000) + 2;
    let (big_pool, small_pool, usdc_xaut_pool, near_xaut_pool) = (
        pool(big_pool_id).await,
        pool(small_pool_id).await,
        pool(usdc_xaut_pool_id).await,
        pool(near_xaut_pool_id).await,
    );
    let exact_in_amount_out = |pool: &PoolView, price, amount_in: u128| {
        pool.rated(price)
            .exact_in_amount_out(
                AssetIndex::First,
                TokenAmount::new(after_fees(amount_in, &fee_fractions)),
            )
            .get()
    };
    let first_route_near_in = near(500) + 2;
    let first_route_usdc = exact_in_amount_out(&big_pool, near_in_usdc, first_route_near_in);
    let first_route_xaut = exact_in_amount_out(&usdc_xaut_pool, usdc_in_xaut, first_route_usdc);
    let usdc_xaut_pool_after_first_route =
        after_swap(&usdc_xaut_pool, first_route_usdc, first_route_xaut);
    let second_route_usdc = exact_in_amount_out(&small_pool, near_in_usdc, near(300));
    let second_route_xaut = exact_in_amount_out(
        &usdc_xaut_pool_after_first_route,
        usdc_in_xaut,
        second_route_usdc,
    );
    let third_route_xaut = exact_in_amount_out(&near_xaut_pool, near_in_xaut, near(200));
    let (near_before, xaut_before) = (
        user1_balance(near_asset_id).await,
        user1_balance(xaut_asset_id).await,
    );
    let result = swap(SwapRequestAmount::ExactIn(TokenAmount::new(near_in))).await;
    assert_success(&result).unwrap();
    assert_eq!(user1_balance(near_asset_id).await, near_before - near_in);
    assert_eq!(
        user1_balance(xaut_asset_id).await,
        xaut_before + first_route_xaut + second_route_xaut + third_route_xaut
    );
    // 800 NEAR at 3.748 USDC and 3998 USDC per XAUT, and 200 at 0.0009375 XAUT, less the fees,
    // give or take a little for the curves
    let xaut_out = first_route_xaut + second_route_xaut + third_route_xaut;
    assert!(xaut_out.abs_diff(935_799) < 100);
    let swaps = amp_events(&result)
        .into_iter()
        .filter_map(|event| match event {
            AmpDexEvent::Swap {
                pool_id,
                amount_in,
                amount_out,
                ..
            } => Some((pool_id, amount_in.get(), amount_out.get())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        swaps,
        vec![
            (big_pool_id, first_route_near_in, first_route_usdc),
            (usdc_xaut_pool_id, first_route_usdc, first_route_xaut),
            (small_pool_id, near(300), second_route_usdc),
            (usdc_xaut_pool_id, second_route_usdc, second_route_xaut),
            (near_xaut_pool_id, near(200), third_route_xaut),
        ]
    );
    // Each pool is updated once, after all routes
    let updated_pool_ids = amp_events(&result)
        .into_iter()
        .filter_map(|event| match event {
            AmpDexEvent::PoolUpdated { pool_id, .. } => Some(pool_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        updated_pool_ids,
        vec![
            big_pool_id,
            usdc_xaut_pool_id,
            small_pool_id,
            near_xaut_pool_id
        ]
    );
    let usdc_xaut_pool_after_swap = pool(usdc_xaut_pool_id).await;
    assert_eq!(
        usdc_xaut_pool_after_swap.assets,
        after_swap(
            &usdc_xaut_pool_after_first_route,
            second_route_usdc,
            second_route_xaut
        )
        .assets
    );

    // Exact out swaps each route from its last pool back, and the second route again swaps on the
    // USDC/XAUT pool as the first left it
    let xaut_out = xaut(1);
    let (big_pool, small_pool, usdc_xaut_pool, near_xaut_pool) = (
        pool(big_pool_id).await,
        pool(small_pool_id).await,
        pool(usdc_xaut_pool_id).await,
        pool(near_xaut_pool_id).await,
    );
    let exact_out_amount_in = |pool: &PoolView, price, amount_out: u128| {
        pool.rated(price)
            .exact_out_amount_in(
                AssetIndex::First,
                TokenAmount::new(amount_out),
                pool.swap_fees().total(),
            )
            .get()
    };
    let first_route_xaut = xaut_out / 2;
    let first_route_usdc = exact_out_amount_in(&usdc_xaut_pool, usdc_in_xaut, first_route_xaut);
    let first_route_near = exact_out_amount_in(&big_pool, near_in_usdc, first_route_usdc);
    let usdc_xaut_pool_after_first_route =
        after_swap(&usdc_xaut_pool, first_route_usdc, first_route_xaut);
    let second_route_usdc = exact_out_amount_in(
        &usdc_xaut_pool_after_first_route,
        usdc_in_xaut,
        xaut_out * 3 / 10,
    );
    let second_route_near = exact_out_amount_in(&small_pool, near_in_usdc, second_route_usdc);
    let third_route_near = exact_out_amount_in(&near_xaut_pool, near_in_xaut, xaut_out / 5);
    let (near_before, xaut_before) = (
        user1_balance(near_asset_id).await,
        user1_balance(xaut_asset_id).await,
    );
    let result = swap(SwapRequestAmount::ExactOut(TokenAmount::new(xaut_out))).await;
    assert_success(&result).unwrap();
    assert_eq!(user1_balance(xaut_asset_id).await, xaut_before + xaut_out);
    assert_eq!(
        user1_balance(near_asset_id).await,
        near_before - first_route_near - second_route_near - third_route_near
    );
    assert_eq!(
        pool(usdc_xaut_pool_id).await.assets,
        after_swap(
            &usdc_xaut_pool_after_first_route,
            second_route_usdc,
            xaut_out * 3 / 10
        )
        .assets
    );
}
