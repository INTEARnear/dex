#![deny(clippy::arithmetic_side_effects)]

use core::panic;
use std::collections::HashMap;

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
use xyk_dex_types::{
    AddLiquidityArgs, AddLiquidityResponse, AssetIndex, AssetWithBalance, CAN_MIGRATE,
    ClaimFeesArgs, ClaimMode, CollectLiquidityFeesArgs, CreatePoolArgs, CreatePoolResponse,
    DEFAULT_FEE_PAYOUT_TYPE, EditFeesArgs, FeeConfigurationV2, FeeDiscountFraction, FeePayoutType,
    FeeReceiver, GetClaimableFeesArgs, GetLiquidityPositionArgs, GetPendingFeesArgs, GetPoolArgs,
    GetPoolSharesArgs, GetPoolsArgs, INITIAL_SHARES, LAST_CREATED_POOL_ID_MARKER,
    LiquidityPosition, LiquidityPositionView, LockPoolArgs, PRIVATE_POOL_TOTAL_SHARES,
    PROTOCOL_FEE_RECEIVER_ID, PayoutGroupShares, PayoutGroups, Pool, PoolAssets, PoolId, PoolType,
    PoolView, ProviderShares, RegisterFeeAssetsArgs, RegisterLiquidityArgs, RemoveLiquidityArgs,
    RemoveLiquidityResponse, RouteFraction, SetFeeDiscountSignerArgs, SetLiquidityFeePayoutArgs,
    SwapArgs, SwapReserves, TokenAmount, TotalShares, WithdrawFeesArgs, XykDexEvent,
    math::{self, exact_in_amount_out, exact_out_amount_in},
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
    UnusedCommunityOwnedFees,
    FeesClaimableByAnyone,
}

/// A x*y=k pool with two assets
#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct XykDex {
    pools: Vector<Pool>,
    fees_collected_by_users: FeeBalances,
    fees_claimable_by_anyone: FeeBalances,
    fee_discount_signer_public_key: Option<PublicKey>,
}

#[near]
impl Dex for XykDex {
    #[payable]
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
        self.fees_claimable_by_anyone.flush();
        for pool_id in updated_pool_ids {
            XykDexEvent::PoolUpdated {
                pool_id,
                pool: self
                    .pools
                    .get(pool_id.get())
                    .unwrap()
                    .view(near_sdk::env::block_timestamp()),
            }
            .emit();
        }
        response
    }
}

impl XykDex {
    /// Starts the fee balances of the account receivers of `fees`, in the assets that their
    /// payouts take fees in and the balances that their claim modes use, and flushes every
    /// started fee balance to storage
    fn register_fee_receivers(&mut self, fees: &FeeConfigurationV2, assets: (&AssetId, &AssetId)) {
        let pool_assets = PoolAssets([assets.0, assets.1]);
        for (fee_receiver, _) in fees.receivers_at(near_sdk::env::block_timestamp()) {
            let (account_id, fee_balances, payout_type) = match fee_receiver {
                FeeReceiver::Account {
                    account_id,
                    payout_type,
                    claim_mode: ClaimMode::OnlyOwner,
                } => (account_id, &mut self.fees_collected_by_users, payout_type),
                FeeReceiver::Account {
                    account_id,
                    payout_type,
                    claim_mode: ClaimMode::Anyone,
                } => (account_id, &mut self.fees_claimable_by_anyone, payout_type),
                FeeReceiver::Pool => continue,
                FeeReceiver::Protocol => panic!("Protocol fee receiver can't be changed"),
            };
            start_fee_balances(fee_balances, &account_id, pool_assets, payout_type);
        }
        self.fees_collected_by_users.flush();
        self.fees_claimable_by_anyone.flush();
    }

    fn swap_route(
        &mut self,
        pool_ids: &[PoolId],
        request: &SwapRequest,
        amount: TokenAmount,
        fee_discount: Option<FeeDiscountFraction>,
        updated_pool_ids: &mut Vec<PoolId>,
    ) -> SwapResponse {
        expect!(!pool_ids.is_empty(), "Route has no pools");
        let mut hops = Vec::with_capacity(pool_ids.len());
        let mut asset_in = request.asset_in.clone();
        for pool_id in pool_ids {
            let pool_id = if *pool_id == LAST_CREATED_POOL_ID_MARKER {
                // Last created pool. This is used to batch "create launch pool + buy"
                // without having to wait to know the pool ID
                PoolId::new(self.pools.len().checked_sub(1).expect("No pools created"))
            } else {
                *pool_id
            };
            let Some(pool) = self.pools.get(pool_id.get()) else {
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
                let mut hop_amount = amount;
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

    fn swap_in_pool(
        &mut self,
        pool_id: PoolId,
        fee_discount: Option<FeeDiscountFraction>,
        updated_pool_ids: &mut Vec<PoolId>,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
    ) -> SwapResponse {
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };
        let pool_asset_ids = {
            let (asset0_id, asset1_id) = pool.asset_ids();
            PoolAssets([asset0_id.clone(), asset1_id.clone()])
        };
        expect!(pool_asset_ids.0.contains(&asset_in), "Invalid asset in");
        expect!(pool_asset_ids.0.contains(&asset_out), "Invalid asset out");
        let mut balances = pool.balances();
        expect!(
            !balances[AssetIndex::First].is_zero() && !balances[AssetIndex::Second].is_zero(),
            "Pool is empty"
        );
        let (in_index, out_index) = match (
            pool_asset_ids[AssetIndex::First] == asset_in
                && pool_asset_ids[AssetIndex::Second] == asset_out,
            pool_asset_ids[AssetIndex::Second] == asset_in
                && pool_asset_ids[AssetIndex::First] == asset_out,
        ) {
            (true, false) => (AssetIndex::First, AssetIndex::Second),
            (false, true) => (AssetIndex::Second, AssetIndex::First),
            _ => panic!("Invalid assets or pool ID"),
        };

        let pool_and_protocol_fees = pool.current_fees(near_sdk::env::block_timestamp());
        let current_fees = match fee_discount {
            Some(fee_discount) => pool_and_protocol_fees.discounted(fee_discount),
            None => pool_and_protocol_fees,
        };

        let amount_in = match amount {
            SwapRequestAmount::ExactIn(exact_amount_in) => {
                expect!(!exact_amount_in.is_zero(), "Amount must be greater than 0");
                exact_amount_in
            }
            SwapRequestAmount::ExactOut(exact_amount_out) => {
                expect!(!exact_amount_out.is_zero(), "Amount must be greater than 0");
                expect!(
                    exact_amount_out < balances[out_index],
                    "Amount must be less than out balance"
                );
                exact_out_amount_in(
                    exact_amount_out,
                    SwapReserves {
                        input: balances[in_index],
                        output: balances[out_index],
                    },
                    &current_fees,
                )
            }
        };
        // The parts of each receiver's fee that are paid out, and with which payout type, which
        // pays the providers that reinvest in shares. The rest of the pool's fee goes into its
        // liquidity: all of it in pools without providers that take it out, and what rounding
        // leaves in the others.
        let receiver_fees = current_fees
            .amounts(amount_in)
            .map(|(receiver, fee_amount)| {
                let paid_out_fees = match (receiver, &*pool) {
                    (
                        FeeReceiver::Pool,
                        Pool::PublicV3 {
                            payout_groups,
                            total_shares,
                            ..
                        },
                    ) => payout_groups
                        .split_pool_fee(fee_amount, *total_shares)
                        .into_iter()
                        .filter(|(_, paid_out_fee)| !paid_out_fee.is_zero())
                        .collect(),
                    (FeeReceiver::Pool, _) => Vec::new(),
                    (FeeReceiver::Account { .. } | FeeReceiver::Protocol, _) => {
                        vec![(pool.account_fee_payout_type(receiver), fee_amount)]
                    }
                };
                (receiver, fee_amount, paid_out_fees)
            })
            .collect::<Vec<_>>();
        let total_fees = receiver_fees
            .iter()
            .try_fold(TokenAmount::ZERO, |total, (_, fee_amount, _)| {
                total.checked_add(*fee_amount)
            })
            .expect("Overflow");
        let fees_added_to_liquidity = receiver_fees
            .iter()
            .map(|(_, fee_amount, paid_out_fees)| {
                paid_out_fees
                    .iter()
                    .try_fold(*fee_amount, |rest, (_, paid_out_fee)| {
                        rest.checked_sub(*paid_out_fee)
                    })
                    .expect("Paid out more than the fee")
            })
            .try_fold(TokenAmount::ZERO, |total, fee_added_to_liquidity| {
                total.checked_add(fee_added_to_liquidity)
            })
            .expect("Overflow");
        let amount_in_after_fees = amount_in.checked_sub(total_fees).expect("Fee exceeds 100%");
        let amount_out = match amount {
            // in_balance was checked to be positive
            SwapRequestAmount::ExactIn(_) => exact_in_amount_out(
                amount_in_after_fees,
                SwapReserves {
                    input: balances[in_index],
                    output: balances[out_index],
                },
            ),
            SwapRequestAmount::ExactOut(exact_amount_out) => exact_amount_out,
        };
        balances[in_index] = balances[in_index]
            .checked_add(amount_in_after_fees)
            .expect("Overflow")
            .checked_add(fees_added_to_liquidity)
            .expect("Overflow");
        balances[out_index] = balances[out_index]
            .checked_sub(amount_out)
            .expect("Underflow");

        let mut fees_breakdown = Vec::new();
        for (receiver, fee_amount, paid_out_fees) in receiver_fees {
            // What the receiver took in each asset, after converting what its payouts convert
            let mut fees_taken = PoolAssets([TokenAmount::ZERO; 2]);
            fees_taken[in_index] = fee_amount;
            for (payout_type, paid_out_fee) in paid_out_fees {
                let payout_asset_index = payout_type.asset_index(in_index);
                let payout_amount = if payout_asset_index == in_index {
                    paid_out_fee
                } else {
                    fees_taken[in_index] = fees_taken[in_index]
                        .checked_sub(paid_out_fee)
                        .expect("Underflow");
                    // The pool swaps the fee into the other asset without fees. in_balance and
                    // out_balance are positive after the swap.
                    let converted_fee = exact_in_amount_out(
                        paid_out_fee,
                        SwapReserves {
                            input: balances[in_index],
                            output: balances[out_index],
                        },
                    );
                    balances[in_index] = balances[in_index]
                        .checked_add(paid_out_fee)
                        .expect("Overflow");
                    balances[out_index] = balances[out_index]
                        .checked_sub(converted_fee)
                        .expect("Underflow");
                    fees_taken[out_index] = fees_taken[out_index]
                        .checked_add(converted_fee)
                        .expect("Overflow");
                    converted_fee
                };
                let payout_asset_id = &pool_asset_ids[payout_asset_index];
                let (account_id, fee_balances) = match receiver {
                    FeeReceiver::Pool => {
                        expect!(
                            let Pool::PublicV3 {
                                payout_groups,
                                total_shares,
                                ..
                            } = &mut *pool,
                            "Only public pools pay out pool fees; this is a bug"
                        );
                        let fee_added_to_liquidity = match payout_type {
                            FeePayoutType::Reinvest => {
                                let reinvested_shares = math::shares_minted_for_reinvested(
                                    payout_amount,
                                    *total_shares,
                                    balances[payout_asset_index],
                                );
                                *total_shares =
                                    total_shares.minted(reinvested_shares).expect("Overflow");
                                payout_groups.reinvest.reinvest(reinvested_shares);
                                payout_amount
                            }
                            FeePayoutType::KeepBoth
                            | FeePayoutType::ConvertToFirstAsset
                            | FeePayoutType::ConvertToSecondAsset => payout_groups
                                .group_mut(payout_type)
                                .share_fee(payout_asset_index, payout_amount),
                        };
                        balances[payout_asset_index] = balances[payout_asset_index]
                            .checked_add(fee_added_to_liquidity)
                            .expect("Overflow");
                        continue;
                    }
                    FeeReceiver::Account {
                        account_id,
                        claim_mode: ClaimMode::OnlyOwner,
                        ..
                    } => (account_id.clone(), &mut self.fees_collected_by_users),
                    FeeReceiver::Account {
                        account_id,
                        claim_mode: ClaimMode::Anyone,
                        ..
                    } => (account_id.clone(), &mut self.fees_claimable_by_anyone),
                    FeeReceiver::Protocol => (
                        PROTOCOL_FEE_RECEIVER_ID.to_owned(),
                        &mut self.fees_collected_by_users,
                    ),
                };
                fees_breakdown.push((receiver.clone(), payout_asset_id.clone(), payout_amount));
                fee_balances.credit(&account_id, payout_asset_id, payout_amount);
            }
            // The pool's liquidity and providers took both what they kept of the asset in, and
            // what they converted to the asset out
            if let FeeReceiver::Pool = receiver {
                fees_breakdown.push((FeeReceiver::Pool, asset_in.clone(), fees_taken[in_index]));
                if !fees_taken[out_index].is_zero() {
                    fees_breakdown.push((
                        FeeReceiver::Pool,
                        asset_out.clone(),
                        fees_taken[out_index],
                    ));
                }
            }
        }
        pool.set_balances(balances);

        if let Pool::LaunchV2 {
            quote_asset,
            phantom_liquidity,
            ..
        } = pool
        {
            expect!(
                quote_asset.balance >= *phantom_liquidity,
                "Quote asset liquidity can't go lower than the initial phantom liquidity"
            );
        }

        if !updated_pool_ids.contains(&pool_id) {
            updated_pool_ids.push(pool_id);
        }
        let response = SwapResponse {
            amount_in,
            amount_out,
        };
        XykDexEvent::Swap {
            pool_id,
            asset_in,
            asset_out,
            amount_in,
            amount_out,
            fees_breakdown,
        }
        .emit();
        response
    }
}

/// Starts the balances of `account_id` in `fee_balances` in the assets of a pool of `asset_ids`
/// that `payout_type` takes fees in, which reinvesting takes in none
fn start_fee_balances(
    fee_balances: &mut FeeBalances,
    account_id: &AccountId,
    asset_ids: PoolAssets<&AssetId>,
    payout_type: FeePayoutType,
) {
    if payout_type == FeePayoutType::Reinvest {
        return;
    }
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
    XykDexEvent::LiquidityFeesCollected {
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
impl XykDex {
    #[init]
    #[payable]
    pub fn new() -> Self {
        expect!(
            matches!(env::caller(), Caller::Direct(_)),
            "This method can only be called directly, not in a transfer call"
        );
        Self {
            pools: Vector::new(StorageKey::Pools),
            fees_collected_by_users: FeeBalances::new(StorageKey::FeesCollectedByUsers),
            fee_discount_signer_public_key: None,
            fees_claimable_by_anyone: FeeBalances::new(StorageKey::FeesClaimableByAnyone),
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn set_fee_discount_signer(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
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
        mut attached_assets: HashMap<AssetId, TokenAmount>,
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
            creator_fee_payout_type,
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

        fees.validate(near_sdk::env::block_timestamp())
            .unwrap_or_else(|error| near_sdk::env::panic_str(&error.to_string()));
        expect!(
            self.pools.len() < LAST_CREATED_POOL_ID_MARKER.get(),
            "Too many pools"
        );

        let storage_usage_before = near_sdk::env::storage_usage();
        let protocol_fee_receiver_id = PROTOCOL_FEE_RECEIVER_ID.to_owned();
        self.fees_collected_by_users
            .start(&protocol_fee_receiver_id, &assets.0);
        self.fees_collected_by_users
            .start(&protocol_fee_receiver_id, &assets.1);
        self.register_fee_receivers(&fees, (&assets.0, &assets.1));

        let mut attached_near = NearToken::from_yoctonear(
            attached_assets
                .remove(&AssetId::Near)
                .expect("Near should be attached for storage")
                .get(),
        );

        let is_public_pool = match pool_type {
            PoolType::PublicLatest
            | PoolType::PublicV3
            | PoolType::PublicV2
            | PoolType::PublicV1 => true,
            PoolType::PrivateLatest
            | PoolType::PrivateV1
            | PoolType::PrivateV2
            | PoolType::LaunchLatest { .. }
            | PoolType::LaunchV1 { .. }
            | PoolType::LaunchV2 { .. } => false,
        };
        expect!(
            is_public_pool || creator_fee_payout_type.is_none(),
            "Only public pools have liquidity providers that take fees"
        );
        let initial_balances = match initial_liquidity {
            Some((initial_amount_0, initial_amount_1)) => {
                expect!(
                    !pool_type.is_launch(),
                    "Launch pools take the launched asset from attached assets, not initial liquidity"
                );
                expect!(
                    !initial_amount_0.is_zero() && !initial_amount_1.is_zero(),
                    "Initial liquidity amounts must be greater than 0"
                );
                for (asset_id, amount) in
                    [(&assets.0, initial_amount_0), (&assets.1, initial_amount_1)]
                {
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
                (initial_amount_0, initial_amount_1)
            }
            None => {
                expect!(
                    !is_public_pool,
                    "Public pools must be created with initial liquidity"
                );
                (TokenAmount::ZERO, TokenAmount::ZERO)
            }
        };

        let pool_id = PoolId::new(self.pools.len());
        // Older pool constructors are created as their latest version
        self.pools.push(match pool_type {
            PoolType::PublicLatest
            | PoolType::PublicV3
            | PoolType::PublicV2
            | PoolType::PublicV1 => {
                expect!(
                    attached_assets.is_empty(),
                    "No assets other than NEAR and initial liquidity should be attached"
                );
                expect!(
                    let Some(creator_fee_payout_type) = creator_fee_payout_type,
                    "Public pools must be created with the creator's fee payout type specified"
                );
                let mut payout_groups = PayoutGroups::default();
                payout_groups.group_mut(creator_fee_payout_type).shares = PayoutGroupShares::NONE
                    .joined_by(INITIAL_SHARES)
                    .expect("Overflow");
                let mut providers = LookupMap::new(StorageKey::PublicPoolUserShares { pool_id });
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

                XykDexEvent::LiquidityFeePayoutSet {
                    pool_id,
                    account_id: caller_id.clone(),
                    payout_type: creator_fee_payout_type,
                }
                .emit();

                self.fees_collected_by_users.flush();
                Pool::PublicV3 {
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
                    providers,
                    total_shares: TotalShares::NONE.minted(INITIAL_SHARES).expect("Overflow"),
                    payout_groups: Box::new(payout_groups),
                }
            }
            PoolType::PrivateLatest | PoolType::PrivateV2 | PoolType::PrivateV1 => {
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
            | PoolType::LaunchV2 { phantom_liquidity }
            | PoolType::LaunchV1 {
                phantom_liquidity_near: phantom_liquidity,
            } => {
                if let PoolType::LaunchV1 { .. } = pool_type {
                    expect!(
                        assets.0 == AssetId::Near,
                        "First asset in Launch pools must be NEAR"
                    );
                }
                expect!(
                    !phantom_liquidity.is_zero(),
                    "Phantom liquidity must be greater than 0"
                );
                let attached_launched_asset = attached_assets
                    .remove(&assets.1)
                    .expect("Launched asset not found");
                expect!(
                    !attached_launched_asset.is_zero(),
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
                        balance: attached_launched_asset,
                    },
                    fees: fees.clone(),
                    phantom_liquidity,
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
                .get(pool_id.get())
                .unwrap()
                .view(near_sdk::env::block_timestamp()),
        }
        .emit();
        if initial_liquidity.is_some() {
            let minted_shares = if is_public_pool {
                INITIAL_SHARES
            } else {
                ProviderShares::ZERO
            };
            XykDexEvent::LiquidityAdded {
                pool_id,
                asset_0: assets.0.clone(),
                asset_1: assets.1.clone(),

                added_amount_0: initial_balances.0,
                added_amount_1: initial_balances.1,
                minted_shares,

                new_owned_asset_0: initial_balances.0,
                new_owned_asset_1: initial_balances.1,
                new_owned_shares: minted_shares,

                new_total_asset_0: initial_balances.0,
                new_total_asset_1: initial_balances.1,
                new_total_shares: U128(minted_shares.get()),
            }
            .emit();
        }

        let response = CreatePoolResponse { pool_id };
        DexCallResponse {
            asset_withdraw_requests: if let Some(leftover) = attached_near.checked_sub(storage_cost)
            {
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
            response: near_sdk::borsh::to_vec(&response).expect("Failed to serialize response"),
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
        let pool_asset_ids = {
            let (asset_0, asset_1) = pool.asset_ids();
            PoolAssets([asset_0.clone(), asset_1.clone()])
        };
        expect!(
            let Pool::PublicV3 {
                providers,
                payout_groups,
                ..
            } = pool,
            "Liquidity registration is only needed for public pools"
        );
        // Providers keep their fees until they choose another payout
        if !providers.contains_key(&caller_id) {
            providers.insert(
                caller_id.clone(),
                LiquidityPosition::new(
                    DEFAULT_FEE_PAYOUT_TYPE,
                    ProviderShares::ZERO,
                    payout_groups.group(DEFAULT_FEE_PAYOUT_TYPE),
                ),
            );
            providers.flush();

            start_fee_balances(
                &mut self.fees_collected_by_users,
                &caller_id,
                pool_asset_ids.each_ref(),
                DEFAULT_FEE_PAYOUT_TYPE,
            );
            self.fees_collected_by_users.flush();
            XykDexEvent::LiquidityFeePayoutSet {
                pool_id,
                account_id: caller_id.clone(),
                payout_type: DEFAULT_FEE_PAYOUT_TYPE,
            }
            .emit();
        }
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
    pub fn add_liquidity(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, TokenAmount>,
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
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
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
            Pool::PrivateV2 {
                assets, owner_id, ..
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
                assets.0.balance = assets
                    .0
                    .balance
                    .checked_add(attached_amount_0)
                    .expect("Overflow");
                assets.1.balance = assets
                    .1
                    .balance
                    .checked_add(attached_amount_1)
                    .expect("Overflow");

                (
                    Vec::new(),
                    attached_amount_0,
                    attached_amount_1,
                    ProviderShares::ZERO,
                    assets.0.balance,
                    assets.1.balance,
                    ProviderShares::ZERO,
                    assets.0.balance,
                    assets.1.balance,
                    0,
                    assets,
                )
            }
            Pool::PublicV3 {
                assets,
                providers,
                total_shares,
                payout_groups,
                ..
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
                        "Min shares received must not be specified for new pools"
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
                let new_owned_asset_0 = new_owned_shares.worth_in(*total_shares, assets.0.balance);
                let new_owned_asset_1 = new_owned_shares.worth_in(*total_shares, assets.1.balance);
                (
                    asset_withdraw_requests,
                    added_amount_0,
                    added_amount_1,
                    shares_to_mint,
                    new_owned_asset_0,
                    new_owned_asset_1,
                    new_owned_shares,
                    assets.0.balance,
                    assets.1.balance,
                    total_shares.get(),
                    assets,
                )
            }
            Pool::LaunchV2 { .. } => {
                panic!("Launch pools don't support adding liquidity after creation");
            }
        };

        XykDexEvent::LiquidityAdded {
            pool_id,
            asset_0: assets.0.asset_id.clone(),
            asset_1: assets.1.asset_id.clone(),

            added_amount_0,
            added_amount_1,
            minted_shares,

            new_owned_asset_0,
            new_owned_asset_1,
            new_owned_shares,

            new_total_asset_0,
            new_total_asset_1,
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
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
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
        expect!(
            shares_to_remove != Some(ProviderShares::ZERO),
            "Can't remove zero shares"
        );
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };

        expect!(
            !matches!(pool, Pool::PrivateV2 { locked: true, .. }),
            "Pool is locked and liquidity cannot be removed"
        );

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
            Pool::PrivateV2 {
                assets, owner_id, ..
            } => {
                expect!(
                    *owner_id == caller_id,
                    "Only pool owner can remove liquidity"
                );
                let shares_to_remove = shares_to_remove.unwrap_or(INITIAL_SHARES);
                expect!(
                    shares_to_remove <= INITIAL_SHARES,
                    "Shares must be less than {INITIAL_SHARES}. {INITIAL_SHARES} means remove 100% of pool"
                );
                let withdrawn_amount_0 =
                    shares_to_remove.worth_in(PRIVATE_POOL_TOTAL_SHARES, assets.0.balance);
                let withdrawn_amount_1 =
                    shares_to_remove.worth_in(PRIVATE_POOL_TOTAL_SHARES, assets.1.balance);
                if let Some((min_asset_0_received, min_asset_1_received)) = min_assets_received {
                    expect!(
                        withdrawn_amount_0 >= min_asset_0_received
                            && withdrawn_amount_1 >= min_asset_1_received,
                        "Slippage error"
                    );
                }
                assets.0.balance = assets
                    .0
                    .balance
                    .checked_sub(withdrawn_amount_0)
                    .expect("Underflow");
                assets.1.balance = assets
                    .1
                    .balance
                    .checked_sub(withdrawn_amount_1)
                    .expect("Underflow");
                (
                    owner_id.clone(),
                    (assets.0.asset_id.clone(), withdrawn_amount_0),
                    (assets.1.asset_id.clone(), withdrawn_amount_1),
                    ProviderShares::ZERO,
                    TokenAmount::ZERO,
                    TokenAmount::ZERO,
                    ProviderShares::ZERO,
                    TokenAmount::ZERO,
                    TokenAmount::ZERO,
                    0,
                )
            }
            Pool::PublicV3 {
                assets,
                providers,
                total_shares,
                payout_groups,
                ..
            } => {
                let mut position = providers
                    .get(&caller_id)
                    .cloned()
                    .expect("User has no shares");
                let current_shares = payout_groups
                    .group(position.payout_type)
                    .current_shares(&position);
                expect!(!current_shares.is_zero(), "User has no shares");
                let shares_to_remove = shares_to_remove.unwrap_or(current_shares);
                expect!(
                    shares_to_remove <= current_shares,
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
                (
                    caller_id.clone(),
                    (assets.0.asset_id.clone(), withdrawn_amount_0),
                    (assets.1.asset_id.clone(), withdrawn_amount_1),
                    shares_to_remove,
                    new_owned_asset_0,
                    new_owned_asset_1,
                    new_owned_shares,
                    assets.0.balance,
                    assets.1.balance,
                    total_shares.get(),
                )
            }
            Pool::LaunchV2 { .. } => {
                panic!("Launch pools don't support removing liquidity after creation");
            }
        };

        XykDexEvent::LiquidityRemoved {
            pool_id,
            asset_0: asset_id_0.clone(),
            asset_1: asset_id_1.clone(),

            removed_amount_0: withdrawn_amount_0,
            removed_amount_1: withdrawn_amount_1,
            burned_shares,

            new_owned_asset_0,
            new_owned_asset_1,
            new_owned_shares,

            new_total_asset_0,
            new_total_asset_1,
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
                    amount: withdrawn_amount_0,
                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(
                        withdraw_to.clone(),
                    ),
                },
                AssetWithdrawRequest {
                    asset_id: asset_id_1,
                    amount: withdrawn_amount_1,
                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(withdraw_to),
                },
            ],
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
            let Pool::PublicV3 {
                providers,
                payout_groups,
                ..
            } = pool,
            "Only public pools have liquidity providers"
        );
        expect!(
            let Some(mut position) = providers.get(&caller_id).cloned(),
            "User has not registered using register_liquidity"
        );
        expect!(
            position.payout_type != payout_type,
            "Liquidity fees are already taken this way"
        );
        let collected_fees = payout_groups.change_payout_type(&mut position, payout_type);
        providers.insert(caller_id.clone(), position);
        providers.flush();
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

        XykDexEvent::LiquidityFeePayoutSet {
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
        let pool_asset_ids = {
            let (asset_0, asset_1) = pool.asset_ids();
            PoolAssets([asset_0.clone(), asset_1.clone()])
        };
        expect!(
            let Pool::PublicV3 {
                providers,
                payout_groups,
                ..
            } = pool,
            "Only public pools have liquidity providers"
        );
        expect!(
            let Some(mut position) = providers.get(&caller_id).cloned(),
            "User has not registered using register_liquidity"
        );
        let collected_fees = position.collect(payout_groups.group(position.payout_type));
        providers.insert(caller_id.clone(), position);
        credit_liquidity_fees(
            &mut self.fees_collected_by_users,
            pool_id,
            &caller_id,
            pool_asset_ids.each_ref(),
            collected_fees,
        );
        DexCallResponse::default()
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn edit_fees(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(EditFeesArgs { pool_id, fees }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };

        fees.validate(near_sdk::env::block_timestamp())
            .unwrap_or_else(|error| near_sdk::env::panic_str(&error.to_string()));

        let assets = match pool {
            Pool::PublicV3 { .. } => {
                panic!("Fees cannot be edited for public pools");
            }
            Pool::LaunchV2 { .. } => {
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
        let assets = (assets.0.asset_id.clone(), assets.1.asset_id.clone());

        let storage_usage_before = near_sdk::env::storage_usage();
        self.register_fee_receivers(&fees, (&assets.0, &assets.1));
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool not found");
        };
        let Pool::PrivateV2 {
            fees: current_fees, ..
        } = pool
        else {
            unreachable!("Checked that the pool is private");
        };
        *current_fees = fees;

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

        let attached_near = NearToken::from_yoctonear(
            attached_assets
                .remove(&AssetId::Near)
                .unwrap_or_default()
                .get(),
        );
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
    pub fn withdraw_fees(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
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

        DexCallResponse {
            asset_withdraw_requests: self
                .fees_collected_by_users
                .withdraw_to_engine(&caller_id, assets),
            ..Default::default()
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn lock_pool(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
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
        let Some(pool) = self.pools.get_mut(pool_id.get()) else {
            panic!("Pool {pool_id} not found");
        };
        match pool {
            Pool::PrivateV2 {
                owner_id, locked, ..
            } => {
                expect!(*owner_id == caller_id, "Only pool owner can lock the pool");
                *locked = true;
            }
            Pool::PublicV3 { .. } => {
                panic!("Public pools cannot be locked");
            }
            Pool::LaunchV2 { .. } => {
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
        mut attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(RegisterFeeAssetsArgs { asset_ids }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let attached_near = NearToken::from_yoctonear(
            attached_assets
                .remove(&AssetId::Near)
                .unwrap_or_default()
                .get(),
        );
        let storage_usage_before = near_sdk::env::storage_usage();
        expect!(attached_assets.is_empty(), "No assets should be attached");
        for asset_id in asset_ids {
            expect!(
                self.fees_collected_by_users.start(&caller_id, &asset_id),
                "Asset {asset_id} already registered"
            );
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
    pub fn claim_fees_anyone(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            matches!(env::caller(), Caller::Direct(_)),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(ClaimFeesArgs {
            account_id,
            asset_ids,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");

        let mut asset_withdraw_requests = Vec::new();
        for asset_id in asset_ids {
            expect!(
                let Some(balance) = self.fees_claimable_by_anyone.take(&account_id, &asset_id),
                "{account_id} has no fees in {asset_id} that anyone can claim"
            );
            if !balance.is_zero() {
                asset_withdraw_requests.push(AssetWithdrawRequest {
                    asset_id,
                    amount: balance,
                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(
                        account_id.clone(),
                    ),
                });
            }
        }
        DexCallResponse {
            asset_withdraw_requests,
            ..Default::default()
        }
    }

    #[result_serializer(borsh)]
    pub fn get_pool(&self, #[serializer(borsh)] args: GetPoolArgs) -> Option<PoolView> {
        let GetPoolArgs { pool_id } = args;
        self.pools
            .get(pool_id.get())
            .map(|pool| pool.view(near_sdk::env::block_timestamp()))
    }

    #[result_serializer(borsh)]
    pub fn get_pools(&self, #[serializer(borsh)] args: GetPoolsArgs) -> Vec<PoolView> {
        let GetPoolsArgs { start_index, limit } = args;
        self.pools
            .iter()
            .skip(start_index.get() as usize)
            .take(limit as usize)
            .map(|pool| pool.view(near_sdk::env::block_timestamp()))
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
        match pool {
            Pool::PublicV3 {
                providers,
                payout_groups,
                ..
            } => providers.get(&account_id).map(|position| {
                let PoolAssets([uncollected_fee_0, uncollected_fee_1]) = payout_groups
                    .group(position.payout_type)
                    .uncollected_fees(position);
                LiquidityPositionView {
                    payout_type: position.payout_type,
                    uncollected_fees: (uncollected_fee_0, uncollected_fee_1),
                }
            }),
            Pool::PrivateV2 { .. } | Pool::LaunchV2 { .. } => None,
        }
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
    pub fn get_claimable_fees(
        &self,
        #[serializer(borsh)] args: GetClaimableFeesArgs,
    ) -> HashMap<AssetId, TokenAmount> {
        let GetClaimableFeesArgs {
            account_id,
            asset_ids,
        } = args;
        self.fees_claimable_by_anyone
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
