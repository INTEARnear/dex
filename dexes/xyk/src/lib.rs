#![deny(clippy::arithmetic_side_effects)]

use core::panic;
use std::{collections::HashMap, num::NonZeroU128};

use intear_dex_types::{
    AssetId, AssetWithdrawRequest, AssetWithdrawalType, Caller, Dex, DexCallResponse, SwapRequest,
    SwapRequestAmount, SwapResponse, env, expect,
};
use near_sdk::{
    AccountId, BorshStorageKey, CurveType, NearToken, PanicOnDefault, PublicKey, assert_one_yocto,
    json_types::U128,
    near,
    store::{LookupMap, Vector},
};
use xyk_dex_types::{
    AddLiquidityArgs, AddLiquidityResponse, AssetWithBalance, CAN_MIGRATE, CreatePoolArgs,
    CreatePoolResponse, CurrentFees, EditFeesArgs, FULL_SPLIT_FRACTION, FeeConfiguration,
    FeeFraction, FeeFractionFraction, FeeReceiver, GetCommunityOwnedFeesArgs, GetPendingFeesArgs,
    GetPoolArgs, GetPoolSharesArgs, GetPoolsArgs, INITIAL_SHARES, LAST_CREATED_POOL_ID_MARKER,
    LockPoolArgs, PROTOCOL_FEE_RECEIVER_ID, Pool, PoolId, PoolNeedsUpgradeArgs, PoolType, PoolView,
    RegisterFeeAssetsArgs, RegisterLiquidityArgs, RemoveLiquidityArgs, RemoveLiquidityResponse,
    SetFeeDiscountSignerArgs, SignedFeeDiscount, SwapArgs, SwapRoute, UpgradePoolArgs,
    WithdrawCommunityFeeArgs, WithdrawFeesArgs, XykDexEvent, asset_account_ids,
    math::{
        exact_in_amount_out, exact_out_amount_in, fee_converted_to_quote_asset, mul_div,
        shares_to_tokens, tokens_to_shares,
    },
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
    PublicPoolUserShares { pool_id: PoolId },
    FeesCollectedByUsers,
    UnusedReferralSettings,
    CommunityOwnedFees,
}

/// A x*y=k pool with two assets
#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct XykDex {
    pools: Vector<Pool>,
    fees_collected_by_users: LookupMap<(AccountId, AssetId), U128>,
    _unused_referral_settings: LookupMap<AccountId, ()>,
    community_owned_fees: LookupMap<AccountId, NearToken>,
    fee_discount_signer_public_key: Option<PublicKey>,
}

#[near]
impl Dex for XykDex {
    #[result_serializer(borsh)]
    fn swap(&mut self, #[serializer(borsh)] request: SwapRequest) -> SwapResponse {
        let Ok(SwapArgs {
            routes,
            fee_discount,
        }) = near_sdk::borsh::from_slice(&request.message.0)
        else {
            panic!("Invalid message");
        };
        expect!(!routes.is_empty(), "No routes");
        expect!(
            routes
                .iter()
                .map(|route| route.fraction as u64)
                .sum::<u64>()
                == FULL_SPLIT_FRACTION as u64,
            "Route fractions must add up to 100%"
        );
        let fee_discount = fee_discount.map(|signed_fee_discount| {
            self.verify_fee_discount(signed_fee_discount, &routes, &request)
        });

        let (SwapRequestAmount::ExactIn(requested_amount)
        | SwapRequestAmount::ExactOut(requested_amount)) = request.amount;
        let mut route_amounts = routes
            .into_iter()
            .map(|route| {
                let amount = mul_div(
                    requested_amount.0,
                    route.fraction as u128,
                    NonZeroU128::new(FULL_SPLIT_FRACTION as u128).unwrap(),
                );
                (route, amount)
            })
            .collect::<Vec<_>>();
        // Fractions round down, so the first route also swaps what's left
        let unsplit_amount = requested_amount
            .0
            .checked_sub(route_amounts.iter().map(|(_route, amount)| amount).sum())
            .expect("Routes exceed the requested amount");
        route_amounts[0].1 = route_amounts[0]
            .1
            .checked_add(unsplit_amount)
            .expect("Overflow");

        let mut response = SwapResponse {
            amount_in: U128(0),
            amount_out: U128(0),
        };
        let mut updated_pool_ids = Vec::new();
        for (route, amount) in route_amounts {
            let route_response = self.swap_route(
                &route.pool_ids,
                &request,
                amount,
                fee_discount,
                &mut updated_pool_ids,
            );
            response.amount_in.0 = response
                .amount_in
                .0
                .checked_add(route_response.amount_in.0)
                .expect("Overflow");
            response.amount_out.0 = response
                .amount_out
                .0
                .checked_add(route_response.amount_out.0)
                .expect("Overflow");
        }
        self.fees_collected_by_users.flush();
        self.community_owned_fees.flush();
        for pool_id in updated_pool_ids {
            XykDexEvent::PoolUpdated {
                pool_id,
                pool: self
                    .pools
                    .get(pool_id)
                    .unwrap()
                    .view(near_sdk::env::block_timestamp()),
            }
            .emit();
        }
        response
    }
}

impl XykDex {
    /// Returns the verified fee discount fraction
    fn verify_fee_discount(
        &self,
        signed_fee_discount: SignedFeeDiscount,
        routes: &[SwapRoute],
        request: &SwapRequest,
    ) -> FeeFractionFraction {
        let SignedFeeDiscount {
            discount,
            signature,
        } = signed_fee_discount;
        expect!(
            let Some(fee_discount_signer_public_key) = &self.fee_discount_signer_public_key,
            "Fee discounts are disabled on this dex"
        );
        let hash = near_sdk::env::sha256_array(near_sdk::borsh::to_vec(&discount).unwrap());
        expect!(
            match fee_discount_signer_public_key.curve_type() {
                CurveType::ED25519 => {
                    near_sdk::env::ed25519_verify(
                        &signature.try_into().unwrap(),
                        hash,
                        fee_discount_signer_public_key.as_bytes()[1..]
                            .try_into()
                            .unwrap(),
                    )
                }
                CurveType::SECP256K1 => {
                    near_sdk::env::ecrecover(
                        &hash,
                        &signature[..signature.len().saturating_sub(1)],
                        *signature.last().expect("Invalid signature"),
                        true,
                    ) == Some(
                        fee_discount_signer_public_key.as_bytes()[1..]
                            .try_into()
                            .unwrap(),
                    )
                }
                CurveType::MLDSA65 => {
                    unimplemented!()
                }
            },
            "Fee discount signature verification failed"
        );
        expect!(
            discount.routes == routes,
            "Fee discount is for another route"
        );
        expect!(
            discount.asset_in == request.asset_in && discount.asset_out == request.asset_out,
            "Fee discount is for other assets"
        );
        if let Some(trader_id) = &discount.trader_id {
            expect!(
                match env::caller() {
                    Caller::Direct(account_id) => account_id == *trader_id,
                    Caller::OnTransfer {
                        alleged_sender,
                        transfer_of,
                    } => alleged_sender == *trader_id && transfer_of == request.asset_in,
                },
                "Fee discount is for another trader"
            );
        }
        expect!(
            discount.amount == request.amount,
            "Fee discount is for another amount"
        );
        expect!(
            near_sdk::env::block_height() <= discount.expiration_block_height,
            "Fee discount expired"
        );
        discount.fee_discount
    }

    fn swap_route(
        &mut self,
        pool_ids: &[PoolId],
        request: &SwapRequest,
        amount: u128,
        fee_discount: Option<FeeFraction>,
        updated_pool_ids: &mut Vec<PoolId>,
    ) -> SwapResponse {
        expect!(!pool_ids.is_empty(), "Route has no pools");
        let mut hops = Vec::with_capacity(pool_ids.len());
        let mut asset_in = request.asset_in.clone();
        for pool_id in pool_ids {
            let pool_id = if *pool_id == LAST_CREATED_POOL_ID_MARKER {
                // Last created pool. This is used to batch "create launch pool + buy"
                // without having to wait to know the pool ID
                self.pools.len().checked_sub(1).expect("No pools created")
            } else {
                *pool_id
            };
            let Some(pool) = self.pools.get(pool_id) else {
                panic!("Pool not found");
            };
            let asset_out = match pool.asset_ids() {
                (asset_0, asset_1) if *asset_0 == asset_in => asset_1.clone(),
                (asset_0, asset_1) if *asset_1 == asset_in => asset_0.clone(),
                _ => panic!("Pool {pool_id} doesn't have {asset_in}"),
            };
            hops.push((pool_id, asset_in, asset_out.clone()));
            asset_in = asset_out;
        }
        expect!(
            asset_in == request.asset_out,
            "Route doesn't end in asset out"
        );
        match request.amount {
            SwapRequestAmount::ExactIn(_) => {
                let mut hop_amount = U128(amount);
                for (pool_id, asset_in, asset_out) in hops {
                    hop_amount = self
                        .swap_in_pool(
                            pool_id,
                            fee_discount,
                            updated_pool_ids,
                            asset_in,
                            asset_out,
                            SwapRequestAmount::ExactIn(hop_amount),
                        )
                        .amount_out;
                }
                SwapResponse {
                    amount_in: U128(amount),
                    amount_out: hop_amount,
                }
            }
            SwapRequestAmount::ExactOut(_) => {
                let mut hop_amount = U128(amount);
                for (pool_id, asset_in, asset_out) in hops.into_iter().rev() {
                    hop_amount = self
                        .swap_in_pool(
                            pool_id,
                            fee_discount,
                            updated_pool_ids,
                            asset_in,
                            asset_out,
                            SwapRequestAmount::ExactOut(hop_amount),
                        )
                        .amount_in;
                }
                SwapResponse {
                    amount_in: hop_amount,
                    amount_out: U128(amount),
                }
            }
        }
    }

    fn swap_in_pool(
        &mut self,
        pool_id: PoolId,
        fee_discount: Option<FeeFraction>,
        updated_pool_ids: &mut Vec<PoolId>,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
    ) -> SwapResponse {
        let Some(pool) = self.pools.get_mut(pool_id) else {
            panic!("Pool not found");
        };
        let launch_quote_asset_id = match pool {
            Pool::LaunchV1 { .. } | Pool::LaunchV2 { .. } => Some(pool.asset_ids().0),
            Pool::PrivateV1 { .. }
            | Pool::PublicV1 { .. }
            | Pool::PrivateV2 { .. }
            | Pool::PublicV2 { .. } => None,
        };
        let should_convert_fees_to_quote_asset =
            launch_quote_asset_id.as_ref() == Some(&&asset_out);
        let (asset0_id, asset1_id) = {
            let (asset0_id, asset1_id) = pool.asset_ids();
            (asset0_id.clone(), asset1_id.clone())
        };
        let fees = pool.fee_configuration();
        let [asset0_balance, asset1_balance] = pool.balances_mut();
        expect!(
            asset0_id == asset_in || asset1_id == asset_in,
            "Invalid asset in"
        );
        expect!(
            asset0_id == asset_out || asset1_id == asset_out,
            "Invalid asset out"
        );
        expect!(*asset0_balance > 0 && *asset1_balance > 0, "Pool is empty");
        let first_in = match (
            asset0_id == asset_in && asset1_id == asset_out,
            asset1_id == asset_in && asset0_id == asset_out,
        ) {
            (true, false) => true,
            (false, true) => false,
            _ => panic!("Invalid assets or pool ID"),
        };

        #[derive(Clone)]
        enum FeeBreakdownEntry {
            Normal {
                receiver: FeeReceiver,
                asset_id: AssetId,
                amount: U128,
            },
            DeferredForConversionToQuoteAsset {
                receiver: AccountId,
                amount_token: U128,
            },
            CommunityFeeDeferredForConversionToQuoteAsset {
                receiver: AccountId,
                amount_token: U128,
            },
        }

        struct CollectFeesReturn {
            amount_in_after_fees: u128,
            pool_fee: u128,
            fees_breakdown: Vec<FeeBreakdownEntry>,
        }

        fn collect_fees(
            amount_in: u128,
            asset_in: &AssetId,
            fees: &CurrentFees,
            fees_collected_by_users: &mut LookupMap<(AccountId, AssetId), U128>,
            community_owned_fees: &mut LookupMap<AccountId, NearToken>,
            convert_to_quote_asset: bool,
        ) -> CollectFeesReturn {
            let mut fees_breakdown = Vec::new();
            let mut total_fees = 0u128;
            let mut pool_fee = 0u128;
            for (receiver, fee_amount) in fees.amounts(amount_in) {
                total_fees = total_fees.checked_add(fee_amount).expect("Overflow");
                match receiver {
                    FeeReceiver::Account(account_id) => {
                        if convert_to_quote_asset {
                            fees_breakdown.push(
                                FeeBreakdownEntry::DeferredForConversionToQuoteAsset {
                                    receiver: account_id.clone(),
                                    amount_token: U128(fee_amount),
                                },
                            );
                        } else {
                            fees_breakdown.push(FeeBreakdownEntry::Normal {
                                receiver: receiver.clone(),
                                asset_id: asset_in.clone(),
                                amount: U128(fee_amount),
                            });
                            fees_collected_by_users
                                .entry((account_id.clone(), asset_in.clone()))
                                .and_modify(|balance| {
                                    balance.0 = balance.0.checked_add(fee_amount).expect("Overflow")
                                })
                                .or_insert_with(|| {
                                    panic!("Fee asset not registered; this is a bug")
                                });
                        }
                    }
                    FeeReceiver::Pool => {
                        fees_breakdown.push(FeeBreakdownEntry::Normal {
                            receiver: receiver.clone(),
                            asset_id: asset_in.clone(),
                            amount: U128(fee_amount),
                        });
                        pool_fee = pool_fee.checked_add(fee_amount).expect("Overflow");
                    }
                    FeeReceiver::Community(account_id) => {
                        if convert_to_quote_asset {
                            fees_breakdown.push(
                                FeeBreakdownEntry::CommunityFeeDeferredForConversionToQuoteAsset {
                                    receiver: account_id.clone(),
                                    amount_token: U128(fee_amount),
                                },
                            );
                        } else {
                            expect!(
                                *asset_in == AssetId::Near,
                                "Community fees can only be collected in NEAR"
                            );
                            fees_breakdown.push(FeeBreakdownEntry::Normal {
                                receiver: receiver.clone(),
                                asset_id: asset_in.clone(),
                                amount: U128(fee_amount),
                            });
                            community_owned_fees
                                .entry(account_id.clone())
                                .and_modify(|total_fee| {
                                    *total_fee = total_fee
                                        .checked_add(NearToken::from_yoctonear(fee_amount))
                                        .expect("Overflow")
                                })
                                .or_insert_with(|| {
                                    panic!("Community fee account not registered; this is a bug")
                                });
                        }
                    }
                }
            }
            CollectFeesReturn {
                amount_in_after_fees: amount_in.checked_sub(total_fees).expect("Fee exceeds 100%"),
                pool_fee,
                fees_breakdown: fees_breakdown.clone(),
            }
        }

        fn convert_fees_to_quote_asset(
            quote_asset_id: &AssetId,
            in_balance: &mut u128,
            out_balance: &mut u128,
            fees_breakdown: &mut Vec<FeeBreakdownEntry>,
            fees_collected_by_users: &mut LookupMap<(AccountId, AssetId), U128>,
            community_owned_fees: &mut LookupMap<AccountId, NearToken>,
        ) {
            for entry in fees_breakdown {
                if let FeeBreakdownEntry::DeferredForConversionToQuoteAsset {
                    receiver,
                    amount_token,
                } = entry
                {
                    // in denominator in_balance or amount can't either be zero.
                    let fee_amount_quote_asset =
                        fee_converted_to_quote_asset(amount_token.0, *in_balance, *out_balance);
                    *in_balance = in_balance.checked_add(amount_token.0).expect("Overflow");
                    *out_balance = out_balance
                        .checked_sub(fee_amount_quote_asset)
                        .expect("Underflow");
                    fees_collected_by_users
                        .entry((receiver.clone(), quote_asset_id.clone()))
                        .and_modify(|balance| {
                            balance.0 = balance
                                .0
                                .checked_add(fee_amount_quote_asset)
                                .expect("Overflow")
                        })
                        .or_insert_with(|| panic!("Quote fee asset not registered; this is a bug"));
                    *entry = FeeBreakdownEntry::Normal {
                        receiver: FeeReceiver::Account(receiver.clone()),
                        asset_id: quote_asset_id.clone(),
                        amount: U128(fee_amount_quote_asset),
                    };
                }
                if let FeeBreakdownEntry::CommunityFeeDeferredForConversionToQuoteAsset {
                    receiver,
                    amount_token,
                } = entry
                {
                    expect!(
                        *quote_asset_id == AssetId::Near,
                        "Community fees can only be collected in NEAR"
                    );
                    // in denominator in_balance or amount can't either be zero.
                    let fee_amount_near =
                        fee_converted_to_quote_asset(amount_token.0, *in_balance, *out_balance);
                    *in_balance = in_balance.checked_add(amount_token.0).expect("Overflow");
                    *out_balance = out_balance.checked_sub(fee_amount_near).expect("Underflow");
                    community_owned_fees
                        .entry(receiver.clone())
                        .and_modify(|total_fee| {
                            *total_fee = total_fee
                                .checked_add(NearToken::from_yoctonear(fee_amount_near))
                                .expect("Overflow")
                        })
                        .or_insert_with(|| {
                            panic!("Community fee account not registered; this is a bug")
                        });
                    *entry = FeeBreakdownEntry::Normal {
                        receiver: FeeReceiver::Community(receiver.clone()),
                        asset_id: AssetId::Near,
                        amount: U128(fee_amount_near),
                    };
                }
            }
        }

        let fee_asset_account_ids = asset_account_ids([&asset_in, &asset_out]);
        let pool_and_protocol_fees =
            fees.with_protocol_fee(&fee_asset_account_ids, near_sdk::env::block_timestamp());
        let current_fees = match fee_discount {
            Some(fee_discount) => pool_and_protocol_fees.discounted(fee_discount),
            None => pool_and_protocol_fees,
        };

        let (fees_breakdown, response) = match amount {
            SwapRequestAmount::ExactIn(exact_amount_in) => {
                expect!(exact_amount_in.0 > 0, "Amount must be greater than 0");
                let (in_balance, out_balance) = if first_in {
                    (asset0_balance, asset1_balance)
                } else {
                    (asset1_balance, asset0_balance)
                };
                let CollectFeesReturn {
                    amount_in_after_fees,
                    pool_fee,
                    mut fees_breakdown,
                } = collect_fees(
                    exact_amount_in.0,
                    &asset_in,
                    &current_fees,
                    &mut self.fees_collected_by_users,
                    &mut self.community_owned_fees,
                    should_convert_fees_to_quote_asset,
                );
                // in_balance was checked to be positive
                let amount_out =
                    exact_in_amount_out(amount_in_after_fees, *in_balance, *out_balance);
                *in_balance = in_balance
                    .checked_add(amount_in_after_fees)
                    .expect("Overflow")
                    .checked_add(pool_fee)
                    .expect("Overflow");
                *out_balance = out_balance.checked_sub(amount_out).expect("Underflow");
                if should_convert_fees_to_quote_asset {
                    convert_fees_to_quote_asset(
                        &asset_out,
                        in_balance,
                        out_balance,
                        &mut fees_breakdown,
                        &mut self.fees_collected_by_users,
                        &mut self.community_owned_fees,
                    );
                }
                (
                    fees_breakdown,
                    SwapResponse {
                        amount_in: exact_amount_in,
                        amount_out: U128(amount_out),
                    },
                )
            }
            SwapRequestAmount::ExactOut(exact_amount_out) => {
                expect!(exact_amount_out.0 > 0, "Amount must be greater than 0");
                let (in_balance, out_balance) = if first_in {
                    (asset0_balance, asset1_balance)
                } else {
                    (asset1_balance, asset0_balance)
                };
                expect!(
                    exact_amount_out.0 < *out_balance,
                    "Amount must be less than out balance"
                );
                let amount_in = exact_out_amount_in(
                    exact_amount_out.0,
                    *in_balance,
                    *out_balance,
                    &current_fees,
                );
                let CollectFeesReturn {
                    amount_in_after_fees,
                    pool_fee,
                    mut fees_breakdown,
                } = collect_fees(
                    amount_in,
                    &asset_in,
                    &current_fees,
                    &mut self.fees_collected_by_users,
                    &mut self.community_owned_fees,
                    should_convert_fees_to_quote_asset,
                );
                *in_balance = in_balance
                    .checked_add(amount_in_after_fees)
                    .expect("Overflow")
                    .checked_add(pool_fee)
                    .expect("Overflow");
                *out_balance = out_balance
                    .checked_sub(exact_amount_out.0)
                    .expect("Underflow");
                if should_convert_fees_to_quote_asset {
                    convert_fees_to_quote_asset(
                        &asset_out,
                        in_balance,
                        out_balance,
                        &mut fees_breakdown,
                        &mut self.fees_collected_by_users,
                        &mut self.community_owned_fees,
                    );
                }
                (
                    fees_breakdown,
                    SwapResponse {
                        amount_in: U128(amount_in),
                        amount_out: U128(exact_amount_out.0),
                    },
                )
            }
        };
        match pool {
            Pool::LaunchV1 {
                near_amount,
                phantom_liquidity_near,
                ..
            } => {
                expect!(
                    near_amount.0 >= phantom_liquidity_near.0,
                    "NEAR liquidity can't go lower than the initial phantom liquidity"
                );
            }
            Pool::LaunchV2 {
                quote_asset,
                phantom_liquidity,
                ..
            } => {
                expect!(
                    quote_asset.balance.0 >= phantom_liquidity.0,
                    "Quote asset liquidity can't go lower than the initial phantom liquidity"
                );
            }
            Pool::PrivateV1 { .. }
            | Pool::PublicV1 { .. }
            | Pool::PrivateV2 { .. }
            | Pool::PublicV2 { .. } => {}
        }

        if !updated_pool_ids.contains(&pool_id) {
            updated_pool_ids.push(pool_id);
        }
        XykDexEvent::Swap {
            pool_id,
            asset_in,
            asset_out,
            amount_in: response.amount_in,
            amount_out: response.amount_out,
            fees_breakdown: fees_breakdown
                .into_iter()
                .map(|entry| {
                    expect!(let FeeBreakdownEntry::Normal { receiver, asset_id, amount } = entry,
                        "Fee breakdown entry is not a normal entry"
                    );
                    (receiver.clone(), asset_id.clone(), amount)
                })
                .collect(),
        }
        .emit();
        response
    }
}

#[near]
impl XykDex {
    #[init]
    #[payable]
    pub fn new() -> Self {
        assert_one_yocto();
        Self {
            pools: Vector::new(StorageKey::Pools),
            fees_collected_by_users: LookupMap::new(StorageKey::FeesCollectedByUsers),
            _unused_referral_settings: LookupMap::new(StorageKey::UnusedReferralSettings),
            community_owned_fees: LookupMap::new(StorageKey::CommunityOwnedFees),
            fee_discount_signer_public_key: None,
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn set_fee_discount_signer(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        expect!(
            caller_id == CAN_MIGRATE,
            "Only owner can set the fee discount signer"
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
        mut attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(CreatePoolArgs {
            assets,
            fees,
            pool_type,
            initial_liquidity,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(assets.0 != assets.1, "Assets must be different");
        expect!(
            matches!(
                assets.0,
                AssetId::Near | AssetId::Nep141(_) | AssetId::Nep245(_, _)
            ),
            "Only NEAR, NEP-141, and NEP-245 assets are supported"
        );
        expect!(
            matches!(
                assets.1,
                AssetId::Near | AssetId::Nep141(_) | AssetId::Nep245(_, _)
            ),
            "Only NEAR, NEP-141, and NEP-245 assets are supported"
        );

        fees.validate(&pool_type, near_sdk::env::block_timestamp())
            .unwrap_or_else(|error| near_sdk::env::panic_str(&error.to_string()));
        expect!(self.pools.len() < u32::MAX, "Too many pools");

        let storage_usage_before = near_sdk::env::storage_usage();
        self.fees_collected_by_users
            .entry((PROTOCOL_FEE_RECEIVER_ID.to_owned(), assets.0.clone()))
            .or_default();
        self.fees_collected_by_users
            .entry((PROTOCOL_FEE_RECEIVER_ID.to_owned(), assets.1.clone()))
            .or_default();
        // TODO: Register only NEAR fee asset for Launch pools
        for (fee_receiver, _) in fees.receivers_at(near_sdk::env::block_timestamp()) {
            match fee_receiver {
                FeeReceiver::Account(account_id) => {
                    expect!(
                        account_id != PROTOCOL_FEE_RECEIVER_ID,
                        "Protocol fee receiver can't be changed",
                    );
                    self.fees_collected_by_users
                        .entry((account_id.clone(), assets.0.clone()))
                        .or_default();
                    self.fees_collected_by_users
                        .entry((account_id.clone(), assets.1.clone()))
                        .or_default();
                }
                FeeReceiver::Pool => {}
                FeeReceiver::Community(account_id) => {
                    self.community_owned_fees
                        .entry(account_id.clone())
                        .or_default();
                }
            }
        }
        self.fees_collected_by_users.flush();
        self.community_owned_fees.flush();

        let mut attached_near = NearToken::from_yoctonear(
            attached_assets
                .remove(&AssetId::Near)
                .expect("Near should be attached for storage")
                .0,
        );

        let is_public_pool = match pool_type {
            PoolType::PublicLatest | PoolType::PublicV2 | PoolType::PublicV1 => true,
            PoolType::PrivateLatest
            | PoolType::PrivateV1
            | PoolType::PrivateV2
            | PoolType::LaunchLatest { .. }
            | PoolType::LaunchV1 { .. }
            | PoolType::LaunchV2 { .. } => false,
        };
        let initial_balances = match initial_liquidity {
            Some((initial_amount_0, initial_amount_1)) => {
                expect!(
                    match pool_type {
                        PoolType::PrivateLatest
                        | PoolType::PrivateV1
                        | PoolType::PrivateV2
                        | PoolType::PublicLatest
                        | PoolType::PublicV1
                        | PoolType::PublicV2 => true,
                        PoolType::LaunchLatest { .. }
                        | PoolType::LaunchV1 { .. }
                        | PoolType::LaunchV2 { .. } => false,
                    },
                    "Launch pools take the launched asset from attached assets, not initial liquidity"
                );
                expect!(
                    initial_amount_0.0 > 0 && initial_amount_1.0 > 0,
                    "Initial liquidity amounts must be greater than 0"
                );
                for (asset_id, amount) in
                    [(&assets.0, initial_amount_0), (&assets.1, initial_amount_1)]
                {
                    if *asset_id == AssetId::Near {
                        attached_near = attached_near
                            .checked_sub(NearToken::from_yoctonear(amount.0))
                            .expect("Not enough NEAR attached for initial liquidity and storage");
                    } else {
                        expect!(
                            attached_assets.remove(asset_id) == Some(amount),
                            "Attached {asset_id} must be equal to its initial liquidity"
                        );
                    }
                }
                (initial_amount_0, initial_amount_1)
            }
            None => {
                expect!(
                    !is_public_pool,
                    "Public pools must be created with initial liquidity"
                );
                (U128(0), U128(0))
            }
        };

        let pool_id = self.pools.len();
        let new_public_pool_user_shares = || {
            let mut user_shares = LookupMap::new(StorageKey::PublicPoolUserShares { pool_id });
            user_shares.insert(caller_id.clone(), Some(INITIAL_SHARES));
            user_shares.flush();
            user_shares
        };
        self.pools.push(match pool_type {
            PoolType::PublicLatest | PoolType::PublicV2 => {
                expect!(
                    attached_assets.is_empty(),
                    "No assets other than NEAR and initial liquidity should be attached"
                );
                Pool::PublicV2 {
                    assets: (
                        AssetWithBalance {
                            asset_id: assets.0.clone(),
                            balance: initial_balances.0,
                        },
                        AssetWithBalance {
                            asset_id: assets.1.clone(),
                            balance: initial_balances.1,
                        },
                    ),
                    fees: fees.clone(),
                    user_shares: new_public_pool_user_shares(),
                    total_shares: Some(INITIAL_SHARES),
                }
            }
            PoolType::PrivateLatest | PoolType::PrivateV2 => {
                expect!(
                    attached_assets.is_empty(),
                    "No assets other than NEAR and initial liquidity should be attached"
                );
                Pool::PrivateV2 {
                    assets: (
                        AssetWithBalance {
                            asset_id: assets.0.clone(),
                            balance: initial_balances.0,
                        },
                        AssetWithBalance {
                            asset_id: assets.1.clone(),
                            balance: initial_balances.1,
                        },
                    ),
                    fees: fees.clone(),
                    owner_id: caller_id.clone(),
                    locked: false,
                }
            }

            PoolType::LaunchLatest { phantom_liquidity }
            | PoolType::LaunchV2 { phantom_liquidity } => {
                expect!(
                    phantom_liquidity.0 > 0,
                    "Phantom liquidity must be greater than 0"
                );
                if fees
                    .receivers_at(near_sdk::env::block_timestamp())
                    .iter()
                    .any(|(receiver, _)| matches!(receiver, FeeReceiver::Community(_)))
                {
                    expect!(
                        assets.0 == AssetId::Near,
                        "Community fee receiver is only supported in Launch pools quoted in NEAR"
                    );
                }
                let attached_launched_asset = attached_assets
                    .remove(&assets.1)
                    .expect("Launched asset not found");
                expect!(
                    attached_launched_asset.0 > 0,
                    "Launched asset amount must be greater than 0"
                );
                expect!(
                    attached_assets.is_empty(),
                    "No assets other than NEAR and launched asset should be attached"
                );
                Pool::LaunchV2 {
                    quote_asset: AssetWithBalance {
                        asset_id: assets.0.clone(),
                        balance: phantom_liquidity,
                    },
                    launched_asset: AssetWithBalance {
                        asset_id: assets.1.clone(),
                        balance: U128(attached_launched_asset.0),
                    },
                    fees: fees.clone(),
                    phantom_liquidity,
                }
            }
            PoolType::LaunchV1 {
                phantom_liquidity_near,
            } => {
                expect!(
                    assets.0 == AssetId::Near,
                    "First asset in Launch pools must be NEAR"
                );
                expect!(
                    phantom_liquidity_near.0 > 0,
                    "Phantom liquidity NEAR must be greater than 0"
                );
                let attached_launched_asset = attached_assets
                    .remove(&assets.1)
                    .expect("Launched asset not found");
                expect!(
                    attached_launched_asset.0 > 0,
                    "Launched asset amount must be greater than 0"
                );
                expect!(
                    attached_assets.is_empty(),
                    "No assets other than NEAR and launched asset should be attached"
                );
                Pool::LaunchV1 {
                    near_amount: phantom_liquidity_near,
                    launched_asset: AssetWithBalance {
                        asset_id: assets.1.clone(),
                        balance: U128(attached_launched_asset.0),
                    },
                    fees: fees.clone(),
                    phantom_liquidity_near,
                }
            }
            PoolType::PrivateV1 => {
                expect!(
                    attached_assets.is_empty(),
                    "No assets other than NEAR and initial liquidity should be attached"
                );
                expect!(
                    let FeeConfiguration::V1(fees) = fees,
                    "Only V1 fees are supported for PrivateV1 pools"
                );
                Pool::PrivateV1 {
                    assets: (
                        AssetWithBalance {
                            asset_id: assets.0.clone(),
                            balance: initial_balances.0,
                        },
                        AssetWithBalance {
                            asset_id: assets.1.clone(),
                            balance: initial_balances.1,
                        },
                    ),
                    fees: fees.clone(),
                    owner_id: caller_id.clone(),
                }
            }
            PoolType::PublicV1 => {
                expect!(
                    attached_assets.is_empty(),
                    "No assets other than NEAR and initial liquidity should be attached"
                );
                expect!(
                    let FeeConfiguration::V1(fees) = fees,
                    "Only V1 fees are supported for PublicV1 pools"
                );
                Pool::PublicV1 {
                    assets: (
                        AssetWithBalance {
                            asset_id: assets.0.clone(),
                            balance: initial_balances.0,
                        },
                        AssetWithBalance {
                            asset_id: assets.1.clone(),
                            balance: initial_balances.1,
                        },
                    ),
                    fees: fees.clone(),
                    user_shares: new_public_pool_user_shares(),
                    total_shares: Some(INITIAL_SHARES),
                }
            }
        });
        self.pools.flush();

        let storage_usage_after = near_sdk::env::storage_usage();
        let storage_cost = near_sdk::env::storage_byte_cost().saturating_mul(
            (storage_usage_after as u128)
                .checked_sub(storage_usage_before as u128)
                .expect("Can't possibly be lower after inserting"),
        );

        expect!(
            attached_near >= storage_cost,
            "Not enough near attached for storage. Required: {storage_cost}, attached: {attached_near}"
        );

        XykDexEvent::PoolUpdated {
            pool_id,
            pool: self
                .pools
                .get(pool_id)
                .unwrap()
                .view(near_sdk::env::block_timestamp()),
        }
        .emit();
        if initial_liquidity.is_some() {
            let minted_shares = if is_public_pool {
                INITIAL_SHARES.get()
            } else {
                0
            };
            XykDexEvent::LiquidityAdded {
                pool_id,
                asset_0: assets.0.clone(),
                asset_1: assets.1.clone(),

                added_amount_0: initial_balances.0,
                added_amount_1: initial_balances.1,
                minted_shares: U128(minted_shares),

                new_owned_asset_0: initial_balances.0,
                new_owned_asset_1: initial_balances.1,
                new_owned_shares: U128(minted_shares),

                new_total_asset_0: initial_balances.0,
                new_total_asset_1: initial_balances.1,
                new_total_shares: U128(minted_shares),
            }
            .emit();
        }

        let response = CreatePoolResponse { pool_id };
        DexCallResponse {
            asset_withdraw_requests: if let Some(leftover) = attached_near.checked_sub(storage_cost)
            {
                vec![AssetWithdrawRequest {
                    asset_id: AssetId::Near,
                    amount: U128(leftover.as_yoctonear()),
                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(
                        caller_id.clone(),
                    ),
                }]
            } else {
                vec![]
            },
            add_storage_deposit: storage_cost,
            response: near_sdk::borsh::to_vec(&response).expect("Failed to serialize response"),
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn register_liquidity(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(RegisterLiquidityArgs { pool_id }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let Some(pool) = self.pools.get_mut(pool_id) else {
            panic!("Pool not found");
        };
        let user_shares = match pool {
            Pool::PublicV1 { user_shares, .. } | Pool::PublicV2 { user_shares, .. } => user_shares,
            _ => panic!("Liquidity registration is only needed for public pools"),
        };
        let attached_near =
            NearToken::from_yoctonear(attached_assets.remove(&AssetId::Near).unwrap_or_default().0);
        expect!(
            attached_assets.is_empty(),
            "No assets other than NEAR should be attached"
        );

        let storage_usage_before = near_sdk::env::storage_usage();
        user_shares.entry(caller_id.clone()).or_insert(None);
        user_shares.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        let storage_cost = near_sdk::env::storage_byte_cost().saturating_mul(
            (storage_usage_after as u128).saturating_sub(storage_usage_before as u128),
        );

        expect!(
            attached_near >= storage_cost,
            "Not enough near attached for storage. Required: {storage_cost}, attached: {attached_near}"
        );

        DexCallResponse {
            asset_withdraw_requests: if let Some(leftover) = attached_near.checked_sub(storage_cost)
            {
                vec![AssetWithdrawRequest {
                    asset_id: AssetId::Near,
                    amount: U128(leftover.as_yoctonear()),
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
    pub fn add_liquidity(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
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
        let Some(pool) = self.pools.get_mut(pool_id) else {
            panic!("Pool not found");
        };

        let (
            asset_withdraw_requests,
            added_amount_0,
            added_amount_1,
            minted_shares,
            new_owned_asset_0,
            new_owned_asset_1,
            new_owned_shares,
            new_total_asset_0,
            new_total_asset_1,
            new_total_shares,
            assets,
        ) = match pool {
            Pool::PrivateV1 {
                assets,
                owner_id,
                fees: _,
            }
            | Pool::PrivateV2 {
                assets,
                owner_id,
                fees: _,
                locked: _,
            } => {
                expect!(*owner_id == caller_id, "Only pool owner can add liquidity");
                expect!(
                    min_shares_received.is_none(),
                    "Min shares received must not be specified for private pools"
                );
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
                assets.0.balance.0 = assets
                    .0
                    .balance
                    .0
                    .checked_add(attached_amount_0.0)
                    .expect("Overflow");
                assets.1.balance.0 = assets
                    .1
                    .balance
                    .0
                    .checked_add(attached_amount_1.0)
                    .expect("Overflow");

                (
                    Vec::new(),
                    attached_amount_0.0,
                    attached_amount_1.0,
                    0,
                    assets.0.balance.0,
                    assets.1.balance.0,
                    0,
                    assets.0.balance.0,
                    assets.1.balance.0,
                    0,
                    assets,
                )
            }
            Pool::PublicV1 {
                assets,
                fees: _,
                user_shares,
                total_shares,
            }
            | Pool::PublicV2 {
                assets,
                fees: _,
                user_shares,
                total_shares,
            } => {
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
                    attached_amount_0.0 > 0 && attached_amount_1.0 > 0,
                    "Amounts must be greater than 0"
                );
                expect!(
                    user_shares.contains_key(&caller_id),
                    "User has not registered using register_liquidity"
                );
                let mut asset_withdraw_requests = Vec::new();
                let (shares_to_mint, total_shares_after, added_amount_0, added_amount_1) =
                    match total_shares {
                        None => {
                            expect!(
                                min_shares_received.is_none(),
                                "Min shares received must not be specified for new pools"
                            );
                            assets.0.balance.0 = assets
                                .0
                                .balance
                                .0
                                .checked_add(attached_amount_0.0)
                                .expect("Overflow");
                            assets.1.balance.0 = assets
                                .1
                                .balance
                                .0
                                .checked_add(attached_amount_1.0)
                                .expect("Overflow");
                            *total_shares = Some(INITIAL_SHARES);
                            expect!(
                                user_shares
                                    .insert(caller_id.clone(), Some(INITIAL_SHARES))
                                    .is_some_and(|s| s.is_none()),
                                "User already has shares but there are no total shares"
                            );
                            (
                                INITIAL_SHARES,
                                INITIAL_SHARES,
                                attached_amount_0.0,
                                attached_amount_1.0,
                            )
                        }
                        Some(pool_total_shares) => {
                            expect!(
                                let Some(pool_balance_0) = NonZeroU128::new(assets.0.balance.0),
                                let Some(pool_balance_1) = NonZeroU128::new(assets.1.balance.0),
                                "Pool is empty"
                            );

                            let shares_mintable_from_0 = NonZeroU128::new(tokens_to_shares(
                                attached_amount_0.0.checked_sub(1).expect("Underflow"),
                                *pool_total_shares,
                                pool_balance_0,
                            ))
                            .expect("Can't mint zero shares");
                            let shares_mintable_from_1 = NonZeroU128::new(tokens_to_shares(
                                attached_amount_1.0.checked_sub(1).expect("Underflow"),
                                *pool_total_shares,
                                pool_balance_1,
                            ))
                            .expect("Can't mint zero shares");
                            let shares_to_mint = shares_mintable_from_0.min(shares_mintable_from_1);
                            if let Some(min_shares_received) = min_shares_received {
                                expect!(shares_to_mint >= min_shares_received, "Slippage error");
                            }

                            let deposited_amount_0 = shares_to_tokens(
                                shares_to_mint,
                                *pool_total_shares,
                                pool_balance_0,
                            )
                            .checked_add(1)
                            .expect("Overflow");
                            let deposited_amount_1 = shares_to_tokens(
                                shares_to_mint,
                                *pool_total_shares,
                                pool_balance_1,
                            )
                            .checked_add(1)
                            .expect("Overflow");

                            assets.0.balance.0 = assets
                                .0
                                .balance
                                .0
                                .checked_add(deposited_amount_0)
                                .expect("Overflow");
                            assets.1.balance.0 = assets
                                .1
                                .balance
                                .0
                                .checked_add(deposited_amount_1)
                                .expect("Overflow");

                            *pool_total_shares = pool_total_shares
                                .checked_add(shares_to_mint.get())
                                .expect("Overflow");
                            user_shares
                                .entry(caller_id.clone())
                                .and_modify(|shares| match shares {
                                    Some(shares) => {
                                        *shares = shares
                                            .checked_add(shares_to_mint.get())
                                            .expect("Overflow");
                                    }
                                    None => {
                                        *shares = Some(shares_to_mint);
                                    }
                                })
                                .or_insert(Some(shares_to_mint));

                            let refund_amount_0 =
                                attached_amount_0.0.checked_sub(deposited_amount_0);
                            if let Some(refund_amount_0) = refund_amount_0 {
                                asset_withdraw_requests.push(AssetWithdrawRequest {
                                    asset_id: assets.0.asset_id.clone(),
                                    amount: U128(refund_amount_0),
                                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(
                                        caller_id.clone(),
                                    ),
                                });
                            }
                            let refund_amount_1 =
                                attached_amount_1.0.checked_sub(deposited_amount_1);
                            if let Some(refund_amount_1) = refund_amount_1 {
                                asset_withdraw_requests.push(AssetWithdrawRequest {
                                    asset_id: assets.1.asset_id.clone(),
                                    amount: U128(refund_amount_1),
                                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(
                                        caller_id.clone(),
                                    ),
                                });
                            }
                            (
                                shares_to_mint,
                                *pool_total_shares,
                                deposited_amount_0,
                                deposited_amount_1,
                            )
                        }
                    };
                let new_owned_shares = user_shares
                    .get(&caller_id)
                    .copied()
                    .flatten()
                    .expect("User has no shares after adding liquidity");
                let new_owned_asset_0 = shares_to_tokens(
                    new_owned_shares,
                    total_shares_after,
                    NonZeroU128::new(assets.0.balance.0).expect("Zero tokens in pool"),
                );
                let new_owned_asset_1 = shares_to_tokens(
                    new_owned_shares,
                    total_shares_after,
                    NonZeroU128::new(assets.1.balance.0).expect("Zero tokens in pool"),
                );
                (
                    asset_withdraw_requests,
                    added_amount_0,
                    added_amount_1,
                    shares_to_mint.get(),
                    new_owned_asset_0,
                    new_owned_asset_1,
                    new_owned_shares.get(),
                    assets.0.balance.0,
                    assets.1.balance.0,
                    total_shares_after.get(),
                    assets,
                )
            }
            Pool::LaunchV1 { .. } | Pool::LaunchV2 { .. } => {
                panic!("Launch pools don't support adding liquidity after creation");
            }
        };

        XykDexEvent::LiquidityAdded {
            pool_id,
            asset_0: assets.0.asset_id.clone(),
            asset_1: assets.1.asset_id.clone(),

            added_amount_0: U128(added_amount_0),
            added_amount_1: U128(added_amount_1),
            minted_shares: U128(minted_shares),

            new_owned_asset_0: U128(new_owned_asset_0),
            new_owned_asset_1: U128(new_owned_asset_1),
            new_owned_shares: U128(new_owned_shares),

            new_total_asset_0: U128(new_total_asset_0),
            new_total_asset_1: U128(new_total_asset_1),
            new_total_shares: U128(new_total_shares),
        }
        .emit();
        XykDexEvent::PoolUpdated {
            pool_id,
            pool: pool.view(near_sdk::env::block_timestamp()),
        }
        .emit();

        let response = AddLiquidityResponse;
        DexCallResponse {
            asset_withdraw_requests,
            response: near_sdk::borsh::to_vec(&response).expect("Failed to serialize response"),
            ..Default::default()
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn remove_liquidity(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
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
        let Some(pool) = self.pools.get_mut(pool_id) else {
            panic!("Pool not found");
        };

        let is_locked = match pool {
            Pool::PrivateV1 { .. } => false,
            Pool::PrivateV2 { locked, .. } => *locked,
            Pool::PublicV1 { .. } | Pool::PublicV2 { .. } => false,
            Pool::LaunchV1 { .. } | Pool::LaunchV2 { .. } => false,
        };
        expect!(!is_locked, "Pool is locked and liquidity cannot be removed");

        let (
            withdraw_to,
            (asset_id_0, withdrawn_amount_0),
            (asset_id_1, withdrawn_amount_1),
            burned_shares,
            new_owned_asset_0,
            new_owned_asset_1,
            new_owned_shares,
            new_total_asset_0,
            new_total_asset_1,
            new_total_shares,
        ) = match pool {
            Pool::PrivateV1 {
                assets,
                owner_id,
                fees: _,
            }
            | Pool::PrivateV2 {
                assets,
                owner_id,
                fees: _,
                locked: _,
            } => {
                expect!(
                    *owner_id == caller_id,
                    "Only pool owner can remove liquidity"
                );
                let shares_to_remove = shares_to_remove.unwrap_or(INITIAL_SHARES);
                expect!(
                    shares_to_remove.get() <= INITIAL_SHARES.get(),
                    "Shares must be less than {INITIAL_SHARES}. {INITIAL_SHARES} means remove 100% of pool"
                );
                // INITIAL_SHARES is not 0
                let withdrawn_amount_0 =
                    mul_div(assets.0.balance.0, shares_to_remove.get(), INITIAL_SHARES);
                // INITIAL_SHARES is not 0
                let withdrawn_amount_1 =
                    mul_div(assets.1.balance.0, shares_to_remove.get(), INITIAL_SHARES);
                if let Some((min_asset_0_received, min_asset_1_received)) = min_assets_received {
                    expect!(
                        withdrawn_amount_0 >= min_asset_0_received.0
                            && withdrawn_amount_1 >= min_asset_1_received.0,
                        "Slippage error"
                    );
                }
                assets.0.balance.0 = assets
                    .0
                    .balance
                    .0
                    .checked_sub(withdrawn_amount_0)
                    .expect("Underflow");
                assets.1.balance.0 = assets
                    .1
                    .balance
                    .0
                    .checked_sub(withdrawn_amount_1)
                    .expect("Underflow");
                (
                    owner_id.clone(),
                    (assets.0.asset_id.clone(), withdrawn_amount_0),
                    (assets.1.asset_id.clone(), withdrawn_amount_1),
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                )
            }
            Pool::PublicV1 {
                assets,
                fees: _,
                user_shares,
                total_shares,
            }
            | Pool::PublicV2 {
                assets,
                fees: _,
                user_shares,
                total_shares,
            } => {
                let user_shares_before = user_shares
                    .get(&caller_id)
                    .copied()
                    .flatten()
                    .expect("User has no shares");
                let shares_to_remove = shares_to_remove.unwrap_or(user_shares_before);
                expect!(
                    shares_to_remove <= user_shares_before,
                    "Not enough shares to remove"
                );
                expect!(let Some(pool_total_shares) = total_shares, "Pool has no shares");
                let (withdrawn_amount_0, withdrawn_amount_1) =
                    if shares_to_remove == *pool_total_shares {
                        let withdrawn_amount_0 = assets.0.balance.0;
                        let withdrawn_amount_1 = assets.1.balance.0;
                        assets.0.balance.0 = 0;
                        assets.1.balance.0 = 0;
                        (withdrawn_amount_0, withdrawn_amount_1)
                    } else {
                        let withdrawn_amount_0 = NonZeroU128::new(assets.0.balance.0)
                            .map(|balance| {
                                shares_to_tokens(shares_to_remove, *pool_total_shares, balance)
                            })
                            .unwrap_or_default();
                        let withdrawn_amount_1 = NonZeroU128::new(assets.1.balance.0)
                            .map(|balance| {
                                shares_to_tokens(shares_to_remove, *pool_total_shares, balance)
                            })
                            .unwrap_or_default();
                        assets.0.balance.0 = assets
                            .0
                            .balance
                            .0
                            .checked_sub(withdrawn_amount_0)
                            .expect("Somehow not enough balance for asset 1 withdrawal");
                        assets.1.balance.0 = assets
                            .1
                            .balance
                            .0
                            .checked_sub(withdrawn_amount_1)
                            .expect("Somehow not enough balance for asset 2 withdrawal");
                        (withdrawn_amount_0, withdrawn_amount_1)
                    };
                if let Some((min_asset_0_received, min_asset_1_received)) = min_assets_received {
                    expect!(
                        withdrawn_amount_0 >= min_asset_0_received.0
                            && withdrawn_amount_1 >= min_asset_1_received.0,
                        "Slippage error"
                    );
                }
                *total_shares = NonZeroU128::new(
                    pool_total_shares
                        .get()
                        .checked_sub(shares_to_remove.get())
                        .expect("Underflow"),
                );
                let user_shares_after = NonZeroU128::new(
                    user_shares_before
                        .get()
                        .checked_sub(shares_to_remove.get())
                        .expect("Underflow"),
                );
                user_shares.insert(caller_id.clone(), user_shares_after);
                let total_shares_after = total_shares.map(|s| s.get()).unwrap_or_default();
                let (new_owned_asset_0, new_owned_asset_1) = match (
                    user_shares_after,
                    *total_shares,
                    NonZeroU128::new(assets.0.balance.0),
                    NonZeroU128::new(assets.1.balance.0),
                ) {
                    (Some(shares), Some(total), Some(balance_0), Some(balance_1)) => (
                        shares_to_tokens(shares, total, balance_0),
                        shares_to_tokens(shares, total, balance_1),
                    ),
                    _ => (0, 0),
                };
                (
                    caller_id.clone(),
                    (assets.0.asset_id.clone(), withdrawn_amount_0),
                    (assets.1.asset_id.clone(), withdrawn_amount_1),
                    shares_to_remove.get(),
                    new_owned_asset_0,
                    new_owned_asset_1,
                    user_shares_after.map(|s| s.get()).unwrap_or_default(),
                    assets.0.balance.0,
                    assets.1.balance.0,
                    total_shares_after,
                )
            }
            Pool::LaunchV1 { .. } | Pool::LaunchV2 { .. } => {
                panic!("Launch pools don't support removing liquidity after creation");
            }
        };

        XykDexEvent::LiquidityRemoved {
            pool_id,
            asset_0: asset_id_0.clone(),
            asset_1: asset_id_1.clone(),

            removed_amount_0: U128(withdrawn_amount_0),
            removed_amount_1: U128(withdrawn_amount_1),
            burned_shares: U128(burned_shares),

            new_owned_asset_0: U128(new_owned_asset_0),
            new_owned_asset_1: U128(new_owned_asset_1),
            new_owned_shares: U128(new_owned_shares),

            new_total_asset_0: U128(new_total_asset_0),
            new_total_asset_1: U128(new_total_asset_1),
            new_total_shares: U128(new_total_shares),
        }
        .emit();
        XykDexEvent::PoolUpdated {
            pool_id,
            pool: pool.view(near_sdk::env::block_timestamp()),
        }
        .emit();

        let response = RemoveLiquidityResponse;
        DexCallResponse {
            asset_withdraw_requests: vec![
                AssetWithdrawRequest {
                    asset_id: asset_id_0,
                    amount: U128(withdrawn_amount_0),
                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(
                        withdraw_to.clone(),
                    ),
                },
                AssetWithdrawRequest {
                    asset_id: asset_id_1,
                    amount: U128(withdrawn_amount_1),
                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(withdraw_to),
                },
            ],
            response: near_sdk::borsh::to_vec(&response).expect("Failed to serialize response"),
            ..Default::default()
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn edit_fees(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(EditFeesArgs { pool_id, fees }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let Some(pool) = self.pools.get_mut(pool_id) else {
            panic!("Pool not found");
        };

        fees.validate(&PoolType::from(&*pool), near_sdk::env::block_timestamp())
            .unwrap_or_else(|error| near_sdk::env::panic_str(&error.to_string()));

        let assets = match pool {
            Pool::PrivateV1 {
                assets, owner_id, ..
            } => {
                expect!(*owner_id == caller_id, "Only pool owner can edit fees");
                assets
            }
            Pool::PublicV1 { .. } | Pool::PublicV2 { .. } => {
                panic!("Fees cannot be edited for public pools");
            }
            Pool::LaunchV1 { .. } | Pool::LaunchV2 { .. } => {
                panic!("Fees cannot be edited for launch pools");
            }
            Pool::PrivateV2 {
                assets,
                owner_id,
                locked,
                ..
            } => {
                expect!(!*locked, "Pool is locked");
                expect!(*owner_id == caller_id, "Only pool owner can edit fees");
                assets
            }
        };

        // TODO: Register only NEAR fee asset for Launch pools
        let storage_usage_before = near_sdk::env::storage_usage();
        for (fee_receiver, _) in fees.receivers_at(near_sdk::env::block_timestamp()) {
            match fee_receiver {
                FeeReceiver::Account(account_id) => {
                    expect!(
                        account_id != PROTOCOL_FEE_RECEIVER_ID,
                        "Protocol fee receiver can't be changed",
                    );
                    self.fees_collected_by_users
                        .entry((account_id.clone(), assets.0.asset_id.clone()))
                        .or_default();
                    self.fees_collected_by_users
                        .entry((account_id.clone(), assets.1.asset_id.clone()))
                        .or_default();
                }
                FeeReceiver::Pool => {}
                FeeReceiver::Community(_) => unreachable!(),
            }
        }
        self.fees_collected_by_users.flush();

        match pool {
            Pool::PrivateV1 {
                fees: current_fees, ..
            } => match fees {
                FeeConfiguration::V1(simple_fees) => {
                    *current_fees = simple_fees;
                }
                _ => {
                    panic!(
                        "This private pool is too old and doesn't support advanced fee configuration"
                    );
                }
            },
            Pool::PublicV1 { .. } | Pool::PublicV2 { .. } => {
                panic!("Fees cannot be edited for public pools");
            }
            Pool::LaunchV1 { .. } | Pool::LaunchV2 { .. } => {
                panic!("Fees cannot be edited for launch pools");
            }
            Pool::PrivateV2 {
                fees: current_fees, ..
            } => {
                *current_fees = fees.clone();
            }
        }

        XykDexEvent::PoolUpdated {
            pool_id,
            pool: pool.view(near_sdk::env::block_timestamp()),
        }
        .emit();
        self.pools.flush();

        let storage_usage_after = near_sdk::env::storage_usage();
        let storage_cost = near_sdk::env::storage_byte_cost().saturating_mul(
            (storage_usage_after as u128).saturating_sub(storage_usage_before as u128),
        );

        let attached_near =
            NearToken::from_yoctonear(attached_assets.remove(&AssetId::Near).unwrap_or_default().0);
        expect!(
            attached_near >= storage_cost,
            "Not enough near attached for storage. Required: {storage_cost}, attached: {attached_near}"
        );
        expect!(
            attached_assets.is_empty(),
            "No assets other than NEAR should be attached"
        );

        DexCallResponse {
            asset_withdraw_requests: if let Some(leftover) = attached_near.checked_sub(storage_cost)
            {
                vec![AssetWithdrawRequest {
                    asset_id: AssetId::Near,
                    amount: U128(leftover.as_yoctonear()),
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
    pub fn withdraw_fees(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(WithdrawFeesArgs { assets }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");

        let mut asset_withdraw_requests = Vec::new();
        for asset_id in assets {
            let Some(balance) = self
                .fees_collected_by_users
                .get(&(caller_id.clone(), asset_id.clone()))
                .cloned()
            else {
                continue;
            };
            if balance.0 == 0 {
                continue;
            }
            self.fees_collected_by_users
                .insert((caller_id.clone(), asset_id.clone()), U128(0));
            asset_withdraw_requests.push(AssetWithdrawRequest {
                asset_id,
                amount: balance,
                withdrawal_type: AssetWithdrawalType::ToInternalUserBalance(caller_id.clone()),
            });
        }

        DexCallResponse {
            asset_withdraw_requests,
            ..Default::default()
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn upgrade_pool(
        &mut self,
        #[allow(unused_mut)]
        #[serializer(borsh)]
        mut attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(UpgradePoolArgs { pool_id }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let attached_near =
            NearToken::from_yoctonear(attached_assets.remove(&AssetId::Near).unwrap_or_default().0);
        expect!(attached_assets.is_empty(), "No assets should be attached");
        let Some(pool) = self.pools.get_mut(pool_id) else {
            panic!("Pool {pool_id} not found");
        };
        // Anyone can upgrade any pool, even not owned by them
        let storage_usage_before = near_sdk::env::storage_usage();
        let new_pool = match pool {
            Pool::PrivateV1 {
                assets,
                owner_id,
                fees,
            } => Pool::PrivateV2 {
                assets: assets.clone(),
                owner_id: owner_id.clone(),
                fees: FeeConfiguration::V1(fees.clone()),
                locked: false,
            },
            Pool::PublicV1 {
                assets,
                fees,
                user_shares: _,
                total_shares,
            } => Pool::PublicV2 {
                assets: assets.clone(),
                fees: FeeConfiguration::V1(fees.clone()),
                user_shares: LookupMap::new(StorageKey::PublicPoolUserShares { pool_id }),
                total_shares: *total_shares,
            },
            Pool::LaunchV1 {
                near_amount,
                launched_asset,
                fees,
                phantom_liquidity_near,
            } => Pool::LaunchV2 {
                quote_asset: AssetWithBalance {
                    asset_id: AssetId::Near,
                    balance: *near_amount,
                },
                launched_asset: launched_asset.clone(),
                fees: fees.clone(),
                phantom_liquidity: *phantom_liquidity_near,
            },
            Pool::LaunchV2 { .. } | Pool::PrivateV2 { .. } | Pool::PublicV2 { .. } => {
                panic!("This pool is of the latest version");
            }
        };
        *pool = new_pool;
        XykDexEvent::PoolUpdated {
            pool_id,
            pool: pool.view(near_sdk::env::block_timestamp()),
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
                    amount: U128(leftover.as_yoctonear()),
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
    pub fn lock_pool(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(LockPoolArgs { pool_id }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");
        let Some(pool) = self.pools.get_mut(pool_id) else {
            panic!("Pool {pool_id} not found");
        };
        match pool {
            Pool::PrivateV1 { .. } => {
                panic!("Old private pool cannot be locked");
            }
            Pool::PrivateV2 {
                owner_id, locked, ..
            } => {
                expect!(*owner_id == caller_id, "Only pool owner can lock the pool");
                *locked = true;
            }
            Pool::PublicV1 { .. } | Pool::PublicV2 { .. } => {
                panic!("Public pools cannot be locked");
            }
            Pool::LaunchV1 { .. } | Pool::LaunchV2 { .. } => {
                panic!("Launch pools are locked by default");
            }
        }
        XykDexEvent::PoolUpdated {
            pool_id,
            pool: pool.view(near_sdk::env::block_timestamp()),
        }
        .emit();
        DexCallResponse::default()
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn register_fee_assets(
        &mut self,
        #[allow(unused_mut)]
        #[serializer(borsh)]
        mut attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(RegisterFeeAssetsArgs { asset_ids }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let attached_near =
            NearToken::from_yoctonear(attached_assets.remove(&AssetId::Near).unwrap_or_default().0);
        let storage_usage_before = near_sdk::env::storage_usage();
        expect!(attached_assets.is_empty(), "No assets should be attached");
        for asset_id in asset_ids {
            if self
                .fees_collected_by_users
                .insert((caller_id.clone(), asset_id.clone()), U128(0))
                .is_some()
            {
                panic!("Asset {asset_id} already registered");
            }
        }
        self.fees_collected_by_users.flush();
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
                    amount: U128(leftover.as_yoctonear()),
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
    pub fn withdraw_community_fee(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_one_yocto();
        let Ok(WithdrawCommunityFeeArgs { account_id }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");

        expect!(
            self.community_owned_fees.contains_key(&account_id),
            "Account does not have a registered community fee"
        );
        let amount = self
            .community_owned_fees
            .insert(account_id.clone(), NearToken::ZERO)
            .expect("Just checked that the account has a registered community fee");
        DexCallResponse {
            asset_withdraw_requests: vec![AssetWithdrawRequest {
                asset_id: AssetId::Near,
                amount: U128(amount.as_yoctonear()),
                withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(account_id),
            }],
            ..Default::default()
        }
    }

    #[result_serializer(borsh)]
    pub fn get_pool(&self, #[serializer(borsh)] args: GetPoolArgs) -> Option<PoolView> {
        let GetPoolArgs { pool_id } = args;
        self.pools
            .get(pool_id)
            .map(|pool| pool.view(near_sdk::env::block_timestamp()))
    }

    #[result_serializer(borsh)]
    pub fn get_pools(&self, #[serializer(borsh)] args: GetPoolsArgs) -> Vec<PoolView> {
        let GetPoolsArgs { start_index, limit } = args;
        self.pools
            .iter()
            .skip(start_index as usize)
            .take(limit as usize)
            .map(|pool| pool.view(near_sdk::env::block_timestamp()))
            .collect()
    }

    #[result_serializer(borsh)]
    pub fn get_pool_shares(
        &self,
        #[serializer(borsh)] args: GetPoolSharesArgs,
    ) -> Vec<Option<U128>> {
        let GetPoolSharesArgs {
            pool_ids,
            account_id,
        } = args;
        pool_ids
            .into_iter()
            .map(|pool_id| {
                let pool = self
                    .pools
                    .get(pool_id)
                    .unwrap_or_else(|| panic!("Pool {pool_id} not found"));
                match pool {
                    Pool::PublicV1 { user_shares, .. } | Pool::PublicV2 { user_shares, .. } => {
                        user_shares.get(&account_id).map(|shares| {
                            shares.map(|shares| U128(shares.get())).unwrap_or_default()
                        })
                    }
                    Pool::PrivateV1 { .. }
                    | Pool::LaunchV1 { .. }
                    | Pool::LaunchV2 { .. }
                    | Pool::PrivateV2 { .. } => None,
                }
            })
            .collect()
    }

    #[result_serializer(borsh)]
    pub fn get_pending_fees(
        &self,
        #[serializer(borsh)] args: GetPendingFeesArgs,
    ) -> HashMap<AssetId, U128> {
        let GetPendingFeesArgs {
            account_id,
            asset_ids,
        } = args;
        asset_ids
            .into_iter()
            .filter_map(|asset_id| {
                self.fees_collected_by_users
                    .get(&(account_id.clone(), asset_id.clone()))
                    .cloned()
                    .map(|balance| (asset_id, balance))
            })
            .collect()
    }

    #[result_serializer(borsh)]
    pub fn get_fee_discount_signer(&self) -> Option<PublicKey> {
        self.fee_discount_signer_public_key.clone()
    }

    #[result_serializer(borsh)]
    pub fn get_pool_count(&self) -> PoolId {
        self.pools.len()
    }

    #[result_serializer(borsh)]
    pub fn pool_needs_upgrade(&self, #[serializer(borsh)] args: PoolNeedsUpgradeArgs) -> bool {
        let PoolNeedsUpgradeArgs { pool_id } = args;
        let Some(pool) = self.pools.get(pool_id) else {
            panic!("Pool {pool_id} not found");
        };
        match pool {
            Pool::PrivateV1 { .. } | Pool::PublicV1 { .. } | Pool::LaunchV1 { .. } => true,
            Pool::PrivateV2 { .. } | Pool::PublicV2 { .. } | Pool::LaunchV2 { .. } => false,
        }
    }

    #[result_serializer(borsh)]
    pub fn get_community_owned_fees(
        &self,
        #[serializer(borsh)] args: GetCommunityOwnedFeesArgs,
    ) -> NearToken {
        let GetCommunityOwnedFeesArgs { account_id } = args;
        self.community_owned_fees
            .get(&account_id)
            .cloned()
            .unwrap_or_default()
    }
}
