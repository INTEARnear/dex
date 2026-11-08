#![deny(clippy::arithmetic_side_effects)]

use std::collections::HashMap;

use amp_dex_types::{
    AddLiquidityArgs, AddLiquidityResponse, AmpDexEvent, AssetIndex, AssetWithBalance,
    CollectLiquidityFeesArgs, CreatePoolArgs, CreatePoolResponse, FULL_FEE_FRACTION,
    FeeDiscountFraction, FeePayoutType, FeeReceiver, GetLiquidityPositionArgs, GetPendingFeesArgs,
    GetPoolArgs, GetPoolSharesArgs, GetPoolsArgs, INITIAL_SHARES, LiquidityPosition,
    LiquidityPositionView, MAX_AMPLIFICATION, MAX_DECIMALS, MIN_AMPLIFICATION,
    PROTOCOL_FEE_RECEIVER_ID, PayoutGroupShares, PayoutGroups, Pool, PoolAssets, PoolId, PoolView,
    Price, PricesRequest, PricesResponse, ProviderShares, RegisterLiquidityArgs,
    RemoveLiquidityArgs, RemoveLiquidityResponse, RouteFraction, SetAmplificationArgs,
    SetFeeDiscountSignerArgs, SetLiquidityFeePayoutArgs, SetOracleKeyArgs, SwapArgs, SwapRoute,
    TokenAmount, TotalShares, WithdrawFeesArgs,
    math::{RatedPool, SwapAmounts},
};
use intear_dex_common::{
    AssetId, AssetWithdrawRequest, AssetWithdrawalType, Caller, Dex, DexCallResponse, FeeBalances,
    SwapRequest, SwapRequestAmount, SwapResponse, env, expect,
};
use near_sdk::{
    AccountId, BorshStorageKey, NearToken, PanicOnDefault, PublicKey,
    json_types::U128,
    near,
    store::{LookupMap, Vector},
};

#[global_allocator]
static ALLOCATOR: talc::Talck<talc::locking::AssumeUnlockable, talc::ClaimOnOom> = {
    const MEMORY_SIZE: usize = 0x10000; // 64KB
    static mut MEMORY: [u8; MEMORY_SIZE] = [0; MEMORY_SIZE];
    let span = talc::Span::from_array(core::ptr::addr_of!(MEMORY).cast_mut());
    talc::Talc::new(unsafe { talc::ClaimOnOom::new(span) }).lock()
};

#[near(serializers=[borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Pools,
    PoolProviders { pool_id: PoolId },
    FeesCollectedByUsers,
}

/// Pools of two assets that swap on a StableSwap curve, with the assets rated at the median of
/// the prices that an oracle collects from exchanges. Only the owner of the dex, the account
/// that deployed it, creates pools and changes their curve and oracle.
#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct AmpDex {
    pools: Vector<Pool>,
    fees_collected_by_users: FeeBalances,
    fee_discount_signer_public_key: Option<PublicKey>,
}

#[near]
impl Dex for AmpDex {
    #[payable]
    #[result_serializer(borsh)]
    fn swap(&mut self, #[serializer(borsh)] request: SwapRequest) -> SwapResponse {
        assert_private_beta_access();
        let Ok(SwapArgs {
            routes,
            fee_discount,
        }) = near_sdk::borsh::from_slice(&request.message.0)
        else {
            panic!("Invalid message");
        };
        expect!(!routes.is_empty(), "No routes");
        expect!(
            routes.iter().all(|route| !route.pool_ids.is_empty()),
            "Route has no pools"
        );
        expect!(
            RouteFraction::add_up_to_full(routes.iter().map(|route| route.fraction)),
            "Route fractions must add up to 100%"
        );
        let fee_discount = fee_discount.map(|signed_fee_discount| {
            signed_fee_discount.verify(
                self.fee_discount_signer_public_key.as_ref(),
                &routes,
                &request,
            )
        });
        let oracle_prices = OraclePrices::of_routes(&self.pools, &routes);

        let (SwapRequestAmount::ExactIn(requested_amount)
        | SwapRequestAmount::ExactOut(requested_amount)) = request.amount;
        let route_amounts =
            RouteFraction::split(requested_amount, routes.iter().map(|route| route.fraction));
        let mut response = SwapResponse {
            amount_in: TokenAmount::ZERO,
            amount_out: TokenAmount::ZERO,
        };
        let mut updated_pool_ids = Vec::new();
        for (route, amount) in routes.iter().zip(route_amounts) {
            let route_response = self.swap_route(
                &route.pool_ids,
                &request,
                amount,
                fee_discount,
                &oracle_prices,
                &mut updated_pool_ids,
            );
            response.amount_in = response
                .amount_in
                .checked_add(route_response.amount_in)
                .expect("Overflow");
            response.amount_out = response
                .amount_out
                .checked_add(route_response.amount_out)
                .expect("Overflow");
        }
        self.fees_collected_by_users.flush();
        for pool_id in updated_pool_ids {
            AmpDexEvent::PoolUpdated {
                pool_id,
                pool: self.pools.get(pool_id.get()).unwrap().view(),
            }
            .emit();
        }
        response
    }
}

impl AmpDex {
    fn swap_route(
        &mut self,
        pool_ids: &[PoolId],
        request: &SwapRequest,
        amount: TokenAmount,
        fee_discount: Option<FeeDiscountFraction>,
        oracle_prices: &OraclePrices,
        updated_pool_ids: &mut Vec<PoolId>,
    ) -> SwapResponse {
        let mut hops = Vec::with_capacity(pool_ids.len());
        let mut asset_in = request.asset_in.clone();
        for pool_id in pool_ids {
            let Some(pool) = self.pools.get(pool_id.get()) else {
                panic!("Pool not found");
            };
            let asset_out = match pool.asset_ids() {
                (asset_0, asset_1) if *asset_0 == asset_in => asset_1.clone(),
                (asset_0, asset_1) if *asset_1 == asset_in => asset_0.clone(),
                _ => panic!("Pool {pool_id} doesn't have {asset_in}"),
            };
            hops.push((*pool_id, asset_in, asset_out.clone()));
            asset_in = asset_out;
        }
        expect!(
            asset_in == request.asset_out,
            "Route doesn't end in asset out"
        );
        match request.amount {
            SwapRequestAmount::ExactIn(_) => {
                let mut hop_amount = amount;
                for (pool_id, asset_in, asset_out) in hops {
                    hop_amount = self
                        .swap_in_pool(
                            pool_id,
                            fee_discount,
                            oracle_prices,
                            updated_pool_ids,
                            asset_in,
                            asset_out,
                            SwapRequestAmount::ExactIn(hop_amount),
                        )
                        .amount_out;
                }
                SwapResponse {
                    amount_in: amount,
                    amount_out: hop_amount,
                }
            }
            SwapRequestAmount::ExactOut(_) => {
                let mut hop_amount = amount;
                for (pool_id, asset_in, asset_out) in hops.into_iter().rev() {
                    hop_amount = self
                        .swap_in_pool(
                            pool_id,
                            fee_discount,
                            oracle_prices,
                            updated_pool_ids,
                            asset_in,
                            asset_out,
                            SwapRequestAmount::ExactOut(hop_amount),
                        )
                        .amount_in;
                }
                SwapResponse {
                    amount_in: hop_amount,
                    amount_out: amount,
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn swap_in_pool(
        &mut self,
        pool_id: PoolId,
        fee_discount: Option<FeeDiscountFraction>,
        oracle_prices: &OraclePrices,
        updated_pool_ids: &mut Vec<PoolId>,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
    ) -> SwapResponse {
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };
        let in_index = if *pool.asset_ids().0 == asset_in {
            AssetIndex::First
        } else {
            AssetIndex::Second
        };
        let out_index = in_index.other();
        let mut balances = pool.balances();
        expect!(
            !balances[AssetIndex::First].is_zero() && !balances[AssetIndex::Second].is_zero(),
            "Pool is empty"
        );
        let price = oracle_prices.of_pool(pool);
        // Rated at the oracle's price with the balances that each step of the swap leaves
        let rated_pool = pool.rated(balances, price);

        let swap_fees = match fee_discount {
            Some(fee_discount) => pool.swap_fees().discounted(fee_discount),
            None => pool.swap_fees(),
        };
        let swap_amounts = rated_pool
            .swap_amounts(in_index, amount, swap_fees)
            .unwrap_or_else(|error| near_sdk::env::panic_str(&error.to_string()));
        let SwapAmounts {
            amount_in,
            amount_out,
            pool_fee,
            protocol_fee,
        } = swap_amounts;
        // The part of the pool's fee that each payout group takes. What rounding leaves goes into
        // the pool's liquidity.
        let paid_out_pool_fees = pool
            .payout_groups
            .split_pool_fee(pool_fee, pool.total_shares)
            .into_iter()
            .filter(|(_, paid_out_fee)| !paid_out_fee.is_zero())
            .collect::<Vec<_>>();
        let pool_fee_added_to_liquidity = paid_out_pool_fees
            .iter()
            .try_fold(pool_fee, |rest, (_, paid_out_fee)| {
                rest.checked_sub(*paid_out_fee)
            })
            .expect("Paid out more than the fee");
        let amount_in_after_fees = swap_amounts.amount_in_after_fees();
        balances[in_index] = balances[in_index]
            .checked_add(amount_in_after_fees)
            .expect("Overflow")
            .checked_add(pool_fee_added_to_liquidity)
            .expect("Overflow");
        balances[out_index] = balances[out_index]
            .checked_sub(amount_out)
            .expect("Underflow");

        // What the pool's liquidity and providers took in each asset, after converting what their
        // payouts convert
        let mut pool_fees_taken = PoolAssets([TokenAmount::ZERO; 2]);
        pool_fees_taken[in_index] = pool_fee;
        for (payout_type, paid_out_fee) in paid_out_pool_fees {
            let payout_asset_index = payout_type.asset_index(in_index);
            let payout_amount = if payout_asset_index == in_index {
                paid_out_fee
            } else {
                pool_fees_taken[in_index] = pool_fees_taken[in_index]
                    .checked_sub(paid_out_fee)
                    .expect("Underflow");
                // The pool swaps the fee into the other asset without fees, on its curve at the
                // oracle's price
                let converted_fee = RatedPool {
                    balances,
                    ..rated_pool
                }
                .exact_in_amount_out(in_index, paid_out_fee);
                balances[in_index] = balances[in_index]
                    .checked_add(paid_out_fee)
                    .expect("Overflow");
                balances[out_index] = balances[out_index]
                    .checked_sub(converted_fee)
                    .expect("Underflow");
                pool_fees_taken[out_index] = pool_fees_taken[out_index]
                    .checked_add(converted_fee)
                    .expect("Overflow");
                converted_fee
            };
            let fee_added_to_liquidity = pool
                .payout_groups
                .group_mut(payout_type)
                .share_fee(payout_asset_index, payout_amount);
            balances[payout_asset_index] = balances[payout_asset_index]
                .checked_add(fee_added_to_liquidity)
                .expect("Overflow");
        }
        pool.set_balances(balances);
        pool.last_oracle_price = price;

        self.fees_collected_by_users.credit(
            &PROTOCOL_FEE_RECEIVER_ID.to_owned(),
            &asset_in,
            protocol_fee,
        );
        let mut fees_breakdown = vec![(
            FeeReceiver::Pool,
            asset_in.clone(),
            pool_fees_taken[in_index],
        )];
        if !pool_fees_taken[out_index].is_zero() {
            fees_breakdown.push((
                FeeReceiver::Pool,
                asset_out.clone(),
                pool_fees_taken[out_index],
            ));
        }
        fees_breakdown.push((FeeReceiver::Protocol, asset_in.clone(), protocol_fee));

        let response = SwapResponse {
            amount_in,
            amount_out,
        };
        AmpDexEvent::Swap {
            pool_id,
            asset_in,
            asset_out,
            amount_in,
            amount_out,
            fees_breakdown,
            price,
        }
        .emit();
        if !updated_pool_ids.contains(&pool_id) {
            updated_pool_ids.push(pool_id);
        }
        response
    }
}

/// The oracle's prices of the assets of every pool of a swap, which the swap's operations started
/// with an oracle request of
struct OraclePrices {
    request: PricesRequest,
    response: PricesResponse,
}

impl OraclePrices {
    /// Of the pools of `routes`, which all ask the same oracle
    fn of_routes(pools: &Vector<Pool>, routes: &[SwapRoute]) -> Self {
        let route_pools = routes
            .iter()
            .flat_map(|route| &route.pool_ids)
            .map(|pool_id| {
                let Some(pool) = pools.get(pool_id.get()) else {
                    panic!("Pool not found");
                };
                pool
            })
            .collect::<Vec<_>>();
        let oracle_key = &route_pools[0].oracle_key;
        expect!(
            route_pools
                .iter()
                .all(|pool| pool.oracle_key == *oracle_key),
            "Pools of a swap must ask the same oracle"
        );
        let request = PricesRequest::of_pools(route_pools.iter().map(|pool| pool.asset_ids()));
        expect!(
            let Some(response) = env::oracle_request(
                oracle_key,
                near_sdk::borsh::to_vec(&request).unwrap()
            ),
            "Swaps on these pools must start with an oracle request of the prices of {} from {oracle_key}",
            request
                .asset_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        let Ok(response) = near_sdk::borsh::from_slice(&response) else {
            panic!("Invalid oracle response");
        };
        Self { request, response }
    }

    /// The median of the prices of a whole token of the pool's first asset in its second, which
    /// are at most 0.5% apart
    fn of_pool(&self, pool: &Pool) -> Price {
        let (asset_0, asset_1) = pool.asset_ids();
        self.response
            .median_price(&self.request, asset_0, asset_1)
            .unwrap_or_else(|error| near_sdk::env::panic_str(&error.to_string()))
    }
}

fn assert_private_beta_access() {
    #[cfg(feature = "private_beta")]
    expect!(
        env::caller().alleged_account_id() == "slimedragon.near",
        "Only slimedragon.near can use this dex in its private beta"
    );
}

/// Starts the balances of `account_id` in `fee_balances` in the assets of a pool of `asset_ids`
/// that `payout_type` takes fees in
fn start_fee_balances(
    fee_balances: &mut FeeBalances,
    account_id: &AccountId,
    asset_ids: PoolAssets<&AssetId>,
    payout_type: FeePayoutType,
) {
    for fee_asset_index in AssetIndex::BOTH {
        fee_balances.start(
            account_id,
            asset_ids[payout_type.asset_index(fee_asset_index)],
        );
    }
}

/// Adds pool fees that a liquidity provider earned to the fees it collected
fn credit_liquidity_fees(
    fees_collected_by_users: &mut FeeBalances,
    pool_id: PoolId,
    account_id: &AccountId,
    asset_ids: PoolAssets<&AssetId>,
    fees: PoolAssets<TokenAmount>,
) {
    if fees.0.iter().all(|fee| fee.is_zero()) {
        return;
    }
    for (asset_id, amount) in asset_ids.0.into_iter().zip(fees.0) {
        // The provider's payout doesn't take fees in every asset, which it may not collect
        if amount.is_zero() {
            continue;
        }
        fees_collected_by_users.credit(account_id, asset_id, amount);
    }
    AmpDexEvent::LiquidityFeesCollected {
        pool_id,
        account_id: account_id.clone(),
        asset_0: asset_ids[AssetIndex::First].clone(),
        asset_1: asset_ids[AssetIndex::Second].clone(),
        amount_0: fees[AssetIndex::First],
        amount_1: fees[AssetIndex::Second],
    }
    .emit();
}

#[near]
impl AmpDex {
    #[init]
    #[payable]
    pub fn new() -> Self {
        assert_private_beta_access();
        expect!(
            matches!(env::caller(), Caller::Direct(_)),
            "This method can only be called directly, not in a transfer call"
        );
        Self {
            pools: Vector::new(StorageKey::Pools),
            fees_collected_by_users: FeeBalances::new(StorageKey::FeesCollectedByUsers),
            fee_discount_signer_public_key: None,
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn set_fee_discount_signer(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_private_beta_access();
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        expect!(
            caller_id == env::dex_id().deployer,
            "Only the owner of the dex can set the fee discount signer"
        );
        let Ok(SetFeeDiscountSignerArgs {
            fee_discount_signer_public_key,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");
        self.fee_discount_signer_public_key = fee_discount_signer_public_key;
        DexCallResponse::default()
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn create_pool(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_private_beta_access();
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        expect!(
            caller_id == env::dex_id().deployer,
            "Only the owner of the dex can create pools"
        );
        let Ok(CreatePoolArgs {
            assets,
            decimals,
            amplification,
            oracle_key,
            fee,
            initial_liquidity,
            creator_fee_payout_type,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(assets.0 != assets.1, "Assets must be different");
        for asset_id in [&assets.0, &assets.1] {
            expect!(
                matches!(
                    asset_id,
                    AssetId::Near | AssetId::Nep141(_) | AssetId::Nep245(_, _)
                ),
                "Only NEAR, NEP-141, and NEP-245 assets are supported"
            );
        }
        expect!(
            decimals.0 <= MAX_DECIMALS && decimals.1 <= MAX_DECIMALS,
            "Assets can have at most {MAX_DECIMALS} decimals"
        );
        expect!(
            (MIN_AMPLIFICATION..=MAX_AMPLIFICATION).contains(&amplification),
            "Amplification must be from {MIN_AMPLIFICATION} to {MAX_AMPLIFICATION}"
        );
        expect!(fee < FULL_FEE_FRACTION, "The fee must be less than 100%");
        expect!(
            !initial_liquidity.0.is_zero() && !initial_liquidity.1.is_zero(),
            "Initial liquidity amounts must be greater than 0"
        );

        let storage_usage_before = near_sdk::env::storage_usage();
        let protocol_fee_receiver_id = PROTOCOL_FEE_RECEIVER_ID.to_owned();
        self.fees_collected_by_users
            .start(&protocol_fee_receiver_id, &assets.0);
        self.fees_collected_by_users
            .start(&protocol_fee_receiver_id, &assets.1);
        self.fees_collected_by_users.flush();

        let mut attached_near = NearToken::from_yoctonear(
            attached_assets
                .remove(&AssetId::Near)
                .expect("Near should be attached for storage")
                .get(),
        );
        for (asset_id, amount) in [
            (&assets.0, initial_liquidity.0),
            (&assets.1, initial_liquidity.1),
        ] {
            if *asset_id == AssetId::Near {
                attached_near = attached_near
                    .checked_sub(NearToken::from_yoctonear(amount.get()))
                    .expect("Not enough NEAR attached for initial liquidity and storage");
            } else {
                expect!(
                    attached_assets.remove(asset_id) == Some(amount),
                    "Attached {asset_id} must be equal to its initial liquidity"
                );
            }
        }
        expect!(
            attached_assets.is_empty(),
            "No assets other than NEAR and initial liquidity should be attached"
        );

        let pool_id = PoolId::new(self.pools.len());
        let mut payout_groups = PayoutGroups::default();
        payout_groups.group_mut(creator_fee_payout_type).shares = PayoutGroupShares::NONE
            .joined_by(INITIAL_SHARES)
            .expect("Overflow");
        let mut providers = LookupMap::new(StorageKey::PoolProviders { pool_id });
        providers.insert(
            caller_id.clone(),
            LiquidityPosition::new(
                creator_fee_payout_type,
                INITIAL_SHARES,
                payout_groups.group(creator_fee_payout_type),
            ),
        );
        providers.flush();
        start_fee_balances(
            &mut self.fees_collected_by_users,
            &caller_id,
            PoolAssets([&assets.0, &assets.1]),
            creator_fee_payout_type,
        );
        self.fees_collected_by_users.flush();
        self.pools.push(Pool {
            assets: (
                AssetWithBalance {
                    asset_id: assets.0.clone(),
                    balance: initial_liquidity.0,
                },
                AssetWithBalance {
                    asset_id: assets.1.clone(),
                    balance: initial_liquidity.1,
                },
            ),
            decimals,
            amplification,
            oracle_key,
            last_oracle_price: Price::from_units(0),
            fee,
            providers,
            total_shares: TotalShares::NONE.minted(INITIAL_SHARES).expect("Overflow"),
            payout_groups,
        });
        self.pools.flush();

        AmpDexEvent::PoolUpdated {
            pool_id,
            pool: self.pools.get(pool_id.get()).unwrap().view(),
        }
        .emit();
        AmpDexEvent::LiquidityAdded {
            pool_id,
            asset_0: assets.0,
            asset_1: assets.1,

            added_amount_0: initial_liquidity.0,
            added_amount_1: initial_liquidity.1,
            minted_shares: INITIAL_SHARES,

            new_owned_asset_0: initial_liquidity.0,
            new_owned_asset_1: initial_liquidity.1,
            new_owned_shares: INITIAL_SHARES,

            new_total_asset_0: initial_liquidity.0,
            new_total_asset_1: initial_liquidity.1,
            new_total_shares: U128(INITIAL_SHARES.get()),
        }
        .emit();

        let storage_usage_after = near_sdk::env::storage_usage();
        let storage_cost = near_sdk::env::storage_byte_cost().saturating_mul(
            (storage_usage_after as u128).saturating_sub(storage_usage_before as u128),
        );
        expect!(
            let Some(leftover) = attached_near.checked_sub(storage_cost),
            "Not enough near attached for storage. Required: {storage_cost}, attached: {attached_near}"
        );
        DexCallResponse {
            asset_withdraw_requests: if !leftover.is_zero() {
                vec![AssetWithdrawRequest {
                    asset_id: AssetId::Near,
                    amount: TokenAmount::new(leftover.as_yoctonear()),
                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(
                        caller_id.clone(),
                    ),
                }]
            } else {
                vec![]
            },
            add_storage_deposit: storage_cost,
            response: near_sdk::borsh::to_vec(&CreatePoolResponse { pool_id })
                .expect("Failed to serialize response"),
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn set_amplification(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_private_beta_access();
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        expect!(
            caller_id == env::dex_id().deployer,
            "Only the owner of the dex can set the amplification"
        );
        let Ok(SetAmplificationArgs {
            pool_id,
            amplification,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");
        expect!(
            (MIN_AMPLIFICATION..=MAX_AMPLIFICATION).contains(&amplification),
            "Amplification must be from {MIN_AMPLIFICATION} to {MAX_AMPLIFICATION}"
        );
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };
        pool.amplification = amplification;
        AmpDexEvent::PoolUpdated {
            pool_id,
            pool: pool.view(),
        }
        .emit();
        DexCallResponse::default()
    }

    /// Change the oracle of a pool, which may have a key that takes more storage, paid for with
    /// attached NEAR
    #[payable]
    #[result_serializer(borsh)]
    pub fn set_oracle_key(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_private_beta_access();
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        expect!(
            caller_id == env::dex_id().deployer,
            "Only the owner of the dex can set the oracle key"
        );
        let Ok(SetOracleKeyArgs {
            pool_id,
            oracle_key,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };
        let attached_near = NearToken::from_yoctonear(
            attached_assets
                .remove(&AssetId::Near)
                .unwrap_or_default()
                .get(),
        );
        expect!(
            attached_assets.is_empty(),
            "No assets other than NEAR should be attached"
        );
        let storage_usage_before = near_sdk::env::storage_usage();
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };
        pool.oracle_key = oracle_key;
        AmpDexEvent::PoolUpdated {
            pool_id,
            pool: pool.view(),
        }
        .emit();
        self.pools.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        let storage_cost = near_sdk::env::storage_byte_cost().saturating_mul(
            (storage_usage_after as u128).saturating_sub(storage_usage_before as u128),
        );
        expect!(
            let Some(leftover) = attached_near.checked_sub(storage_cost),
            "Not enough near attached for storage. Required: {storage_cost}, attached: {attached_near}"
        );
        DexCallResponse {
            asset_withdraw_requests: if !leftover.is_zero() {
                vec![AssetWithdrawRequest {
                    asset_id: AssetId::Near,
                    amount: TokenAmount::new(leftover.as_yoctonear()),
                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(
                        caller_id.clone(),
                    ),
                }]
            } else {
                vec![]
            },
            add_storage_deposit: storage_cost,
            ..Default::default()
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn register_liquidity(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_private_beta_access();
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(RegisterLiquidityArgs { pool_id }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };
        let attached_near = NearToken::from_yoctonear(
            attached_assets
                .remove(&AssetId::Near)
                .unwrap_or_default()
                .get(),
        );
        expect!(
            attached_assets.is_empty(),
            "No assets other than NEAR should be attached"
        );

        let storage_usage_before = near_sdk::env::storage_usage();
        // Providers keep their fees until they choose another payout
        if !pool.providers.contains_key(&caller_id) {
            pool.providers.insert(
                caller_id.clone(),
                LiquidityPosition::new(
                    FeePayoutType::KeepBoth,
                    ProviderShares::ZERO,
                    &pool.payout_groups.keep_both,
                ),
            );
            pool.providers.flush();
            let (asset_0, asset_1) = pool.asset_ids();
            start_fee_balances(
                &mut self.fees_collected_by_users,
                &caller_id,
                PoolAssets([asset_0, asset_1]),
                FeePayoutType::KeepBoth,
            );
            self.fees_collected_by_users.flush();
        }
        let storage_usage_after = near_sdk::env::storage_usage();
        let storage_cost = near_sdk::env::storage_byte_cost().saturating_mul(
            (storage_usage_after as u128).saturating_sub(storage_usage_before as u128),
        );
        expect!(
            let Some(leftover) = attached_near.checked_sub(storage_cost),
            "Not enough near attached for storage. Required: {storage_cost}, attached: {attached_near}"
        );
        DexCallResponse {
            asset_withdraw_requests: if !leftover.is_zero() {
                vec![AssetWithdrawRequest {
                    asset_id: AssetId::Near,
                    amount: TokenAmount::new(leftover.as_yoctonear()),
                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(
                        caller_id.clone(),
                    ),
                }]
            } else {
                vec![]
            },
            add_storage_deposit: storage_cost,
            ..Default::default()
        }
    }

    /// Add liquidity in the ratio of the pool's balances, which doesn't need the oracle
    #[payable]
    #[result_serializer(borsh)]
    pub fn add_liquidity(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_private_beta_access();
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(AddLiquidityArgs {
            pool_id,
            min_shares_received,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };
        let Pool {
            assets,
            providers,
            total_shares,
            payout_groups,
            ..
        } = &mut *pool;
        let attached_amount_0 = attached_assets
            .remove(&assets.0.asset_id)
            .expect("Asset 1 not found");
        let attached_amount_1 = attached_assets
            .remove(&assets.1.asset_id)
            .expect("Asset 2 not found");
        expect!(
            attached_assets.is_empty(),
            "No assets other than the two pool assets should be attached"
        );
        expect!(
            !attached_amount_0.is_zero() && !attached_amount_1.is_zero(),
            "Amounts must be greater than 0"
        );
        expect!(
            let Some(mut position) = providers.get(&caller_id).cloned(),
            "User has not registered using register_liquidity"
        );
        let mut asset_withdraw_requests = Vec::new();
        let (shares_to_mint, added_amount_0, added_amount_1) = if total_shares.is_none() {
            expect!(
                min_shares_received.is_none(),
                "Min shares received must not be specified for empty pools"
            );
            expect!(
                position.shares.is_zero(),
                "User already has shares but there are no total shares"
            );
            (INITIAL_SHARES, attached_amount_0, attached_amount_1)
        } else {
            let pool_balance_0 = assets.0.balance;
            let pool_balance_1 = assets.1.balance;
            let shares_mintable_from_0 = ProviderShares::minted_for(
                attached_amount_0
                    .checked_sub(TokenAmount::new(1))
                    .expect("Underflow"),
                *total_shares,
                pool_balance_0,
            );
            let shares_mintable_from_1 = ProviderShares::minted_for(
                attached_amount_1
                    .checked_sub(TokenAmount::new(1))
                    .expect("Underflow"),
                *total_shares,
                pool_balance_1,
            );
            let shares_to_mint = shares_mintable_from_0.min(shares_mintable_from_1);
            expect!(!shares_to_mint.is_zero(), "Can't mint zero shares");
            if let Some(min_shares_received) = min_shares_received {
                expect!(shares_to_mint >= min_shares_received, "Slippage error");
            }

            let deposited_amount_0 = shares_to_mint
                .worth_in(*total_shares, pool_balance_0)
                .checked_add(TokenAmount::new(1))
                .expect("Overflow");
            let deposited_amount_1 = shares_to_mint
                .worth_in(*total_shares, pool_balance_1)
                .checked_add(TokenAmount::new(1))
                .expect("Overflow");

            for (asset, attached_amount, deposited_amount) in [
                (&assets.0, attached_amount_0, deposited_amount_0),
                (&assets.1, attached_amount_1, deposited_amount_1),
            ] {
                if let Some(refund_amount) = attached_amount.checked_sub(deposited_amount) {
                    asset_withdraw_requests.push(AssetWithdrawRequest {
                        asset_id: asset.asset_id.clone(),
                        amount: refund_amount,
                        withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(
                            caller_id.clone(),
                        ),
                    });
                }
            }
            (shares_to_mint, deposited_amount_0, deposited_amount_1)
        };
        assets.0.balance = assets
            .0
            .balance
            .checked_add(added_amount_0)
            .expect("Overflow");
        assets.1.balance = assets
            .1
            .balance
            .checked_add(added_amount_1)
            .expect("Overflow");
        let collected_fees = payout_groups.add_shares(&mut position, shares_to_mint);
        *total_shares = total_shares.minted(shares_to_mint).expect("Overflow");
        credit_liquidity_fees(
            &mut self.fees_collected_by_users,
            pool_id,
            &caller_id,
            PoolAssets([&assets.0.asset_id, &assets.1.asset_id]),
            collected_fees,
        );
        let new_owned_shares = position.shares;
        providers.insert(caller_id.clone(), position);

        AmpDexEvent::LiquidityAdded {
            pool_id,
            asset_0: assets.0.asset_id.clone(),
            asset_1: assets.1.asset_id.clone(),

            added_amount_0,
            added_amount_1,
            minted_shares: shares_to_mint,

            new_owned_asset_0: new_owned_shares.worth_in(*total_shares, assets.0.balance),
            new_owned_asset_1: new_owned_shares.worth_in(*total_shares, assets.1.balance),
            new_owned_shares,

            new_total_asset_0: assets.0.balance,
            new_total_asset_1: assets.1.balance,
            new_total_shares: U128(total_shares.get()),
        }
        .emit();
        AmpDexEvent::PoolUpdated {
            pool_id,
            pool: pool.view(),
        }
        .emit();

        let response = AddLiquidityResponse;
        DexCallResponse {
            asset_withdraw_requests,
            response: near_sdk::borsh::to_vec(&response).expect("Failed to serialize response"),
            ..Default::default()
        }
    }

    /// Remove liquidity in the ratio of the pool's balances, which doesn't need the oracle
    #[payable]
    #[result_serializer(borsh)]
    pub fn remove_liquidity(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_private_beta_access();
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(RemoveLiquidityArgs {
            pool_id,
            shares_to_remove,
            min_assets_received,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");
        expect!(
            shares_to_remove != Some(ProviderShares::ZERO),
            "Can't remove zero shares"
        );
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };
        let Pool {
            assets,
            providers,
            total_shares,
            payout_groups,
            ..
        } = &mut *pool;
        let mut position = providers
            .get(&caller_id)
            .cloned()
            .expect("User has no shares");
        expect!(!position.shares.is_zero(), "User has no shares");
        let shares_to_remove = shares_to_remove.unwrap_or(position.shares);
        expect!(
            shares_to_remove <= position.shares,
            "Not enough shares to remove"
        );
        let total_shares_after = total_shares.burned(shares_to_remove).expect("Underflow");
        let (withdrawn_amount_0, withdrawn_amount_1) = if total_shares_after.is_none() {
            (assets.0.balance, assets.1.balance)
        } else {
            (
                shares_to_remove.worth_in(*total_shares, assets.0.balance),
                shares_to_remove.worth_in(*total_shares, assets.1.balance),
            )
        };
        assets.0.balance = assets
            .0
            .balance
            .checked_sub(withdrawn_amount_0)
            .expect("Somehow not enough balance for asset 1 withdrawal");
        assets.1.balance = assets
            .1
            .balance
            .checked_sub(withdrawn_amount_1)
            .expect("Somehow not enough balance for asset 2 withdrawal");
        if let Some((min_asset_0_received, min_asset_1_received)) = min_assets_received {
            expect!(
                withdrawn_amount_0 >= min_asset_0_received
                    && withdrawn_amount_1 >= min_asset_1_received,
                "Slippage error"
            );
        }
        let collected_fees = payout_groups.remove_shares(&mut position, shares_to_remove);
        *total_shares = total_shares_after;
        credit_liquidity_fees(
            &mut self.fees_collected_by_users,
            pool_id,
            &caller_id,
            PoolAssets([&assets.0.asset_id, &assets.1.asset_id]),
            collected_fees,
        );
        let new_owned_shares = position.shares;
        providers.insert(caller_id.clone(), position);
        let (new_owned_asset_0, new_owned_asset_1) = if total_shares.is_none() {
            (TokenAmount::ZERO, TokenAmount::ZERO)
        } else {
            (
                new_owned_shares.worth_in(*total_shares, assets.0.balance),
                new_owned_shares.worth_in(*total_shares, assets.1.balance),
            )
        };

        AmpDexEvent::LiquidityRemoved {
            pool_id,
            asset_0: assets.0.asset_id.clone(),
            asset_1: assets.1.asset_id.clone(),

            removed_amount_0: withdrawn_amount_0,
            removed_amount_1: withdrawn_amount_1,
            burned_shares: shares_to_remove,

            new_owned_asset_0,
            new_owned_asset_1,
            new_owned_shares,

            new_total_asset_0: assets.0.balance,
            new_total_asset_1: assets.1.balance,
            new_total_shares: U128(total_shares.get()),
        }
        .emit();
        let asset_withdraw_requests = vec![
            AssetWithdrawRequest {
                asset_id: assets.0.asset_id.clone(),
                amount: withdrawn_amount_0,
                withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(caller_id.clone()),
            },
            AssetWithdrawRequest {
                asset_id: assets.1.asset_id.clone(),
                amount: withdrawn_amount_1,
                withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(caller_id),
            },
        ];
        AmpDexEvent::PoolUpdated {
            pool_id,
            pool: pool.view(),
        }
        .emit();

        let response = RemoveLiquidityResponse;
        DexCallResponse {
            asset_withdraw_requests,
            response: near_sdk::borsh::to_vec(&response).expect("Failed to serialize response"),
            ..Default::default()
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn set_liquidity_fee_payout(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_private_beta_access();
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(SetLiquidityFeePayoutArgs {
            pool_id,
            payout_type,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };
        let attached_near = NearToken::from_yoctonear(
            attached_assets
                .remove(&AssetId::Near)
                .unwrap_or_default()
                .get(),
        );
        expect!(
            attached_assets.is_empty(),
            "No assets other than NEAR should be attached"
        );
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };
        let storage_usage_before = near_sdk::env::storage_usage();
        let pool_asset_ids = {
            let (asset_0, asset_1) = pool.asset_ids();
            PoolAssets([asset_0.clone(), asset_1.clone()])
        };
        expect!(
            let Some(mut position) = pool.providers.get(&caller_id).cloned(),
            "User has not registered using register_liquidity"
        );
        expect!(
            position.payout_type != payout_type,
            "Liquidity fees are already taken this way"
        );
        let collected_fees = pool
            .payout_groups
            .change_payout_type(&mut position, payout_type);
        pool.providers.insert(caller_id.clone(), position);
        pool.providers.flush();
        credit_liquidity_fees(
            &mut self.fees_collected_by_users,
            pool_id,
            &caller_id,
            pool_asset_ids.each_ref(),
            collected_fees,
        );
        start_fee_balances(
            &mut self.fees_collected_by_users,
            &caller_id,
            pool_asset_ids.each_ref(),
            payout_type,
        );
        self.fees_collected_by_users.flush();
        self.pools.flush();

        AmpDexEvent::LiquidityFeePayoutSet {
            pool_id,
            account_id: caller_id.clone(),
            payout_type,
        }
        .emit();

        let storage_usage_after = near_sdk::env::storage_usage();
        let storage_cost = near_sdk::env::storage_byte_cost().saturating_mul(
            (storage_usage_after as u128).saturating_sub(storage_usage_before as u128),
        );
        expect!(
            let Some(leftover) = attached_near.checked_sub(storage_cost),
            "Not enough near attached for storage. Required: {storage_cost}, attached: {attached_near}"
        );
        DexCallResponse {
            asset_withdraw_requests: if !leftover.is_zero() {
                vec![AssetWithdrawRequest {
                    asset_id: AssetId::Near,
                    amount: TokenAmount::new(leftover.as_yoctonear()),
                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(
                        caller_id.clone(),
                    ),
                }]
            } else {
                vec![]
            },
            add_storage_deposit: storage_cost,
            ..Default::default()
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn collect_liquidity_fees(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_private_beta_access();
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(CollectLiquidityFeesArgs { pool_id }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };
        expect!(
            let Some(mut position) = pool.providers.get(&caller_id).cloned(),
            "User has not registered using register_liquidity"
        );
        let collected_fees = position.collect(pool.payout_groups.group(position.payout_type));
        pool.providers.insert(caller_id.clone(), position);
        let (asset_0, asset_1) = pool.asset_ids();
        credit_liquidity_fees(
            &mut self.fees_collected_by_users,
            pool_id,
            &caller_id,
            PoolAssets([asset_0, asset_1]),
            collected_fees,
        );
        DexCallResponse::default()
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn withdraw_fees(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_private_beta_access();
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(WithdrawFeesArgs { assets }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");

        DexCallResponse {
            asset_withdraw_requests: self
                .fees_collected_by_users
                .withdraw_to_engine(&caller_id, assets),
            ..Default::default()
        }
    }

    #[result_serializer(borsh)]
    pub fn get_pool(&self, #[serializer(borsh)] args: GetPoolArgs) -> Option<PoolView> {
        let GetPoolArgs { pool_id } = args;
        self.pools.get(pool_id.get()).map(|pool| pool.view())
    }

    #[result_serializer(borsh)]
    pub fn get_pools(&self, #[serializer(borsh)] args: GetPoolsArgs) -> Vec<PoolView> {
        let GetPoolsArgs { start_index, limit } = args;
        self.pools
            .iter()
            .skip(start_index.get() as usize)
            .take(limit as usize)
            .map(|pool| pool.view())
            .collect()
    }

    #[result_serializer(borsh)]
    pub fn get_pool_shares(
        &self,
        #[serializer(borsh)] args: GetPoolSharesArgs,
    ) -> Vec<Option<ProviderShares>> {
        let GetPoolSharesArgs {
            pool_ids,
            account_id,
        } = args;
        pool_ids
            .into_iter()
            .map(|pool_id| {
                self.pools
                    .get(pool_id.get())
                    .unwrap_or_else(|| panic!("Pool {pool_id} not found"))
                    .liquidity_shares(&account_id)
            })
            .collect()
    }

    #[result_serializer(borsh)]
    pub fn get_liquidity_position(
        &self,
        #[serializer(borsh)] args: GetLiquidityPositionArgs,
    ) -> Option<LiquidityPositionView> {
        let GetLiquidityPositionArgs {
            pool_id,
            account_id,
        } = args;
        let Some(pool) = self.pools.get(pool_id.get()) else {
            panic!("Pool {pool_id} not found");
        };
        pool.providers.get(&account_id).map(|position| {
            let PoolAssets([uncollected_fee_0, uncollected_fee_1]) = pool
                .payout_groups
                .group(position.payout_type)
                .uncollected_fees(position);
            LiquidityPositionView {
                payout_type: position.payout_type,
                uncollected_fees: (uncollected_fee_0, uncollected_fee_1),
            }
        })
    }

    #[result_serializer(borsh)]
    pub fn get_pending_fees(
        &self,
        #[serializer(borsh)] args: GetPendingFeesArgs,
    ) -> HashMap<AssetId, TokenAmount> {
        let GetPendingFeesArgs {
            account_id,
            asset_ids,
        } = args;
        self.fees_collected_by_users
            .balances(&account_id, asset_ids)
    }

    #[result_serializer(borsh)]
    pub fn get_fee_discount_signer(&self) -> Option<PublicKey> {
        self.fee_discount_signer_public_key.clone()
    }

    #[result_serializer(borsh)]
    pub fn get_pool_count(&self) -> u32 {
        self.pools.len()
    }
}
