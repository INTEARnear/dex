#![deny(clippy::arithmetic_side_effects)]

use core::panic;
use std::{collections::HashMap, num::NonZeroU128};

use crypto_bigint::U256;
use intear_dex_types::{
    AssetId, AssetWithdrawRequest, AssetWithdrawalType, Dex, DexCallResponse, SwapRequest,
    SwapRequestAmount, SwapResponse, expect,
};
use near_sdk::{
    AccountId, BorshStorageKey, NearToken, PanicOnDefault, Timestamp, assert_one_yocto,
    json_types::U128,
    near,
    store::{LookupMap, Vector},
};

#[global_allocator]
static ALLOCATOR: talc::Talck<talc::locking::AssumeUnlockable, talc::ClaimOnOom> = {
    const MEMORY_SIZE: usize = 0x8000; // 32KB
    static mut MEMORY: [u8; MEMORY_SIZE] = [0; MEMORY_SIZE];
    let span = talc::Span::from_array(core::ptr::addr_of!(MEMORY).cast_mut());
    talc::Talc::new(unsafe { talc::ClaimOnOom::new(span) }).lock()
};

const MAX_FEE_FRACTION: FeeFraction = 1000000;
const INITIAL_SHARES: SharesBalance = NonZeroU128::new(10u128.pow(18)).unwrap();
const PROTOCOL_FEE: FeeFraction = MAX_FEE_FRACTION / 1000; // 0.1%
const PROTOCOL_FEE_RECEIVER: &str = "plach.intear.near";
const PROTOCOL_FEE_BYPASS_PARENT_ACCOUNTS: &[&str] = &["user.intear.near"];
const PROTOCOL_FEE_REDUCE_ASSET_PARENT_ACCOUNTS: &[&str] =
    &["omft.near", "omni.hot.tg", "tether-token.near"];
const PROTOCOL_FEE_REDUCE_ASSET_ACCOUNTS: &[&str] = &[
    "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1",
    "near",
];
const PROTOCOL_FEE_REDUCED: FeeFraction = 1; // 0.0001%

#[near(serializers=[borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Pools,
    PublicPoolUserShares { pool_id: PoolId },
    FeesCollectedByUsers,
}

type PoolId = u32;

/// A x*y=k pool with two assets
#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct XykDex {
    pools: Vector<Pool>,
    fees_collected_by_users: LookupMap<(AccountId, AssetId), U128>,
}

#[near(event_json(standard = "xyk"))]
enum XykDexEvent {
    #[event_version("1.0.0")]
    PoolUpdated { pool_id: PoolId, pool: PoolView },
    #[event_version("1.0.0")]
    Swap {
        pool_id: PoolId,
        request: SwapRequest,
        amount_in: U128,
        fees_breakdown: Vec<(FeeReceiver, U128)>,
        amount_out: U128,
    },
    #[event_version("1.0.0")]
    LiquidityAdded {
        pool_id: PoolId,
        asset_0: AssetId,
        asset_1: AssetId,

        added_amount_0: U128,
        added_amount_1: U128,
        minted_shares: U128,

        new_owned_asset_0: U128,
        new_owned_asset_1: U128,
        new_owned_shares: U128,

        new_total_asset_0: U128,
        new_total_asset_1: U128,
        new_total_shares: U128,
    },
    #[event_version("1.0.0")]
    LiquidityRemoved {
        pool_id: PoolId,
        asset_0: AssetId,
        asset_1: AssetId,

        removed_amount_0: U128,
        removed_amount_1: U128,
        burned_shares: U128,

        new_owned_asset_0: U128,
        new_owned_asset_1: U128,
        new_owned_shares: U128,

        new_total_asset_0: U128,
        new_total_asset_1: U128,
        new_total_shares: U128,
    },
}

fn u128_to_u256(value: u128) -> U256 {
    U256::from(value)
}

fn u256_to_u128(value: U256) -> u128 {
    expect!(value.bits() <= 128, "Value must be less than 128 bits");
    let bytes = value.to_le_bytes();
    let first_chunk = bytes.first_chunk().unwrap();
    u128::from_le_bytes(*first_chunk)
}

fn shares_to_tokens(
    shares: SharesBalance,
    total_shares: SharesBalance,
    total_tokens: NonZeroU128,
) -> u128 {
    // total_shares is NonZeroU128; multiplication of u128's can not overflow u256
    #[allow(clippy::arithmetic_side_effects)]
    u256_to_u128(
        u128_to_u256(total_tokens.get()) * u128_to_u256(shares.get())
            / u128_to_u256(total_shares.get()),
    )
}

fn tokens_to_shares(tokens: u128, total_shares: SharesBalance, total_tokens: NonZeroU128) -> u128 {
    // total_shares is NonZeroU128; multiplication of u128's can not overflow u256
    #[allow(clippy::arithmetic_side_effects)]
    u256_to_u128(
        u128_to_u256(tokens) * u128_to_u256(total_shares.get()) / u128_to_u256(total_tokens.get()),
    )
}

fn asset_account_ids(asset_ids: &[AssetId]) -> Vec<AccountId> {
    asset_ids
        .iter()
        .map(|asset_id| match asset_id {
            AssetId::Near => "near".parse::<AccountId>().unwrap(),
            AssetId::Nep141(account_id) => account_id.clone(),
            AssetId::Nep171(_, _) => panic!("Nep171 assets are not supported"),
            AssetId::Nep245(account_id, _) => account_id.clone(),
        })
        .collect()
}

#[near(serializers=[borsh])]
pub enum PoolType {
    PrivateLatest,
    PublicLatest,
    LaunchLatest { phantom_liquidity_near: U128 },
    // Supporting previous versions could be useful for writing tests
    LaunchV1 { phantom_liquidity_near: U128 },
    PrivateV1,
    PublicV1,
    PrivateV2,
    PublicV2,
}

#[near]
impl Dex for XykDex {
    #[result_serializer(borsh)]
    fn swap(&mut self, #[serializer(borsh)] request: SwapRequest) -> SwapResponse {
        #[near(serializers=[borsh])]
        struct SwapArgs {
            pool_id: PoolId,
        }
        let Ok(SwapArgs { pool_id }) = near_sdk::borsh::from_slice(&request.message.0) else {
            panic!("Invalid message");
        };
        let Some(pool) = self.pools.get_mut(pool_id) else {
            panic!("Pool not found");
        };
        let (asset0_id, asset0_balance, asset1_id, asset1_balance, fees) = match pool {
            Pool::PrivateV1 {
                assets,
                owner_id: _,
                fees,
            }
            | Pool::PublicV1 {
                assets,
                fees,
                user_shares: _,
                total_shares: _,
            } => (
                assets.0.asset_id.clone(),
                &mut assets.0.balance.0,
                assets.1.asset_id.clone(),
                &mut assets.1.balance.0,
                fees.clone(),
            ),
            Pool::LaunchV1 {
                near_amount,
                launched_asset,
                fees,
                phantom_liquidity_near: _,
            } => (
                AssetId::Near,
                &mut near_amount.0,
                launched_asset.asset_id.clone(),
                &mut launched_asset.balance.0,
                (&*fees).into(),
            ),
            Pool::PrivateV2 {
                assets,
                owner_id: _,
                fees,
                locked: _,
            }
            | Pool::PublicV2 {
                assets,
                fees,
                user_shares: _,
                total_shares: _,
            } => (
                assets.0.asset_id.clone(),
                &mut assets.0.balance.0,
                assets.1.asset_id.clone(),
                &mut assets.1.balance.0,
                (&*fees).into(),
            ),
        };
        expect!(
            asset0_id == request.asset_in || asset1_id == request.asset_in,
            "Invalid asset in"
        );
        expect!(
            asset0_id == request.asset_out || asset1_id == request.asset_out,
            "Invalid asset out"
        );
        expect!(*asset0_balance > 0 && *asset1_balance > 0, "Pool is empty");
        let first_in = match (
            asset0_id == request.asset_in && asset1_id == request.asset_out,
            asset1_id == request.asset_in && asset0_id == request.asset_out,
        ) {
            (true, false) => true,
            (false, true) => false,
            _ => panic!("Invalid assets or pool ID"),
        };

        fn collect_fees(
            amount_in: u128,
            asset_in: &AssetId,
            fees: &CurrentFees,
            fees_collected_by_users: &mut LookupMap<(AccountId, AssetId), U128>,
        ) -> (u128, u128, Vec<(FeeReceiver, U128)>) {
            let mut fees_breakdown = Vec::new();
            let mut total_fees = 0u128;
            let mut pool_fee = 0u128;
            for (receiver, fee_fraction) in fees.receivers.iter() {
                // MAX_FEE_FRACTION is constant, so no zero division; u128 * u128 can't
                // overflow u256
                #[allow(clippy::arithmetic_side_effects)]
                let fee_amount = u256_to_u128(
                    u128_to_u256(amount_in) * u128_to_u256(*fee_fraction as u128)
                        / u128_to_u256(MAX_FEE_FRACTION as u128),
                );
                total_fees = total_fees.checked_add(fee_amount).expect("Overflow");
                fees_breakdown.push((receiver.clone(), U128(fee_amount)));
                match receiver {
                    FeeReceiver::Account(account_id) => {
                        fees_collected_by_users
                            .entry((account_id.clone(), asset_in.clone()))
                            .and_modify(|balance| {
                                balance.0 = balance.0.checked_add(fee_amount).expect("Overflow")
                            })
                            .or_insert(U128(fee_amount));
                    }
                    FeeReceiver::Pool => {
                        pool_fee = pool_fee.checked_add(fee_amount).expect("Overflow");
                    }
                }
            }
            (
                amount_in.checked_sub(total_fees).expect("Fee exceeds 100%"),
                pool_fee,
                fees_breakdown,
            )
        }

        let (fees_breakdown, response) = match request.amount {
            SwapRequestAmount::ExactIn(exact_amount_in) => {
                expect!(exact_amount_in.0 > 0, "Amount must be greater than 0");
                let (in_balance, out_balance) = if first_in {
                    (asset0_balance, asset1_balance)
                } else {
                    (asset1_balance, asset0_balance)
                };
                let (amount_in_after_fees, pool_fee, fees_breakdown) = collect_fees(
                    exact_amount_in.0,
                    &request.asset_in,
                    &fees.with_protocol_fee(
                        Some(near_sdk::env::predecessor_account_id()),
                        asset_account_ids(&[request.asset_in.clone(), request.asset_out.clone()]),
                    ),
                    &mut self.fees_collected_by_users,
                );
                // u128 * u128 or u128 + u128 can't overflow u256; in_balance was checked to be positive
                #[allow(clippy::arithmetic_side_effects)]
                let amount_out = u256_to_u128(
                    u128_to_u256(amount_in_after_fees) * u128_to_u256(*out_balance)
                        / (u128_to_u256(*in_balance) + u128_to_u256(amount_in_after_fees)),
                );
                *in_balance = in_balance
                    .checked_add(amount_in_after_fees)
                    .expect("Overflow")
                    .checked_add(pool_fee)
                    .expect("Overflow");
                *out_balance = out_balance.checked_sub(amount_out).expect("Underflow");
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
                // u128 * u128 can't overflow u256; out_balance was checked to be
                // greater than exact_amount_out so no underflow or zero division
                // can happen
                #[allow(clippy::arithmetic_side_effects)]
                let amount_in_without_fees = u256_to_u128(
                    ((u128_to_u256(*in_balance) * u128_to_u256(exact_amount_out.0))
                        / (u128_to_u256(*out_balance) - u128_to_u256(exact_amount_out.0)))
                    .saturating_add(&U256::ONE),
                );
                let fees_with_protocol_fee = fees.with_protocol_fee(
                    Some(near_sdk::env::predecessor_account_id()),
                    asset_account_ids(&[request.asset_in.clone(), request.asset_out.clone()]),
                );
                let total_fee_fraction = fees_with_protocol_fee
                    .receivers
                    .iter()
                    .map(|(_, fee)| *fee as u128)
                    .sum::<u128>();
                let fee_denominator = (MAX_FEE_FRACTION as u128)
                    .checked_sub(total_fee_fraction)
                    .expect("Fee fraction somehow above 100%");
                let fee_denominator_minus_one = fee_denominator
                    .checked_sub(1)
                    .expect("Fee fraction somehow equals 100%");
                // checked_sub would fail if denominator was 0; MAX_FEE_FRACTION is
                // constant, so multiplication of u128's will not be close to overflowing
                // u256 after adding another u1282
                #[allow(clippy::arithmetic_side_effects)]
                let amount_in = u256_to_u128(
                    (u128_to_u256(amount_in_without_fees) * u128_to_u256(MAX_FEE_FRACTION as u128)
                        + u128_to_u256(fee_denominator_minus_one))
                        / u128_to_u256(fee_denominator),
                );
                let (amount_in_after_fees, pool_fee, fees_breakdown) = collect_fees(
                    amount_in,
                    &request.asset_in,
                    &fees_with_protocol_fee,
                    &mut self.fees_collected_by_users,
                );
                *in_balance = in_balance
                    .checked_add(amount_in_after_fees)
                    .expect("Overflow")
                    .checked_add(pool_fee)
                    .expect("Overflow");
                *out_balance = out_balance
                    .checked_sub(exact_amount_out.0)
                    .expect("Underflow");
                (
                    fees_breakdown,
                    SwapResponse {
                        amount_in: U128(amount_in),
                        amount_out: U128(exact_amount_out.0),
                    },
                )
            }
        };
        self.fees_collected_by_users.flush();

        if let Pool::LaunchV1 {
            near_amount,
            phantom_liquidity_near,
            ..
        } = pool
        {
            expect!(
                near_amount.0 >= phantom_liquidity_near.0,
                "NEAR liquidity can't go lower than the initial phantom liquidity"
            );
        }

        XykDexEvent::PoolUpdated {
            pool_id,
            pool: (&*pool).into(),
        }
        .emit();
        XykDexEvent::Swap {
            pool_id,
            request,
            amount_in: response.amount_in,
            amount_out: response.amount_out,
            fees_breakdown,
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
        }
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
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct CreatePoolArgs {
            assets: (AssetId, AssetId),
            fees: FeeConfiguration,
            pool_type: PoolType,
        }
        let Ok(CreatePoolArgs {
            assets,
            fees,
            pool_type,
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

        fees.validate();

        let storage_usage_before = near_sdk::env::storage_usage();
        let protocol_fee_receiver: AccountId = PROTOCOL_FEE_RECEIVER.parse().unwrap();
        self.fees_collected_by_users
            .entry((protocol_fee_receiver.clone(), assets.0.clone()))
            .or_default();
        self.fees_collected_by_users
            .entry((protocol_fee_receiver, assets.1.clone()))
            .or_default();
        for (fee_receiver, _) in fees.receivers() {
            match fee_receiver {
                FeeReceiver::Account(account_id) => {
                    expect!(
                        account_id != PROTOCOL_FEE_RECEIVER,
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
            }
        }
        self.fees_collected_by_users.flush();

        let attached_near = NearToken::from_yoctonear(
            attached_assets
                .remove(&AssetId::Near)
                .expect("Near should be attached for storage")
                .0,
        );

        let pool_id = self.pools.len();
        self.pools.push(match pool_type {
            PoolType::PublicLatest | PoolType::PublicV2 => {
                expect!(
                    attached_assets.is_empty(),
                    "No assets other than NEAR should be attached"
                );
                Pool::PublicV2 {
                    assets: (
                        AssetWithBalance {
                            asset_id: assets.0.clone(),
                            balance: U128(0),
                        },
                        AssetWithBalance {
                            asset_id: assets.1.clone(),
                            balance: U128(0),
                        },
                    ),
                    fees: fees.clone(),
                    user_shares: LookupMap::new(StorageKey::PublicPoolUserShares { pool_id }),
                    total_shares: None,
                }
            }
            PoolType::PrivateLatest | PoolType::PrivateV2 => {
                expect!(
                    attached_assets.is_empty(),
                    "No assets other than NEAR should be attached"
                );
                Pool::PrivateV2 {
                    assets: (
                        AssetWithBalance {
                            asset_id: assets.0.clone(),
                            balance: U128(0),
                        },
                        AssetWithBalance {
                            asset_id: assets.1.clone(),
                            balance: U128(0),
                        },
                    ),
                    fees: fees.clone(),
                    owner_id: near_sdk::env::predecessor_account_id(),
                    locked: false,
                }
            }
            PoolType::LaunchV1 {
                phantom_liquidity_near,
            }
            | PoolType::LaunchLatest {
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
                    "No assets other than NEAR should be attached"
                );
                expect!(
                    let FeeConfiguration::V1(fees) = fees,
                    "Only V1 fees are supported for PrivateV1 pools"
                );
                Pool::PrivateV1 {
                    assets: (
                        AssetWithBalance {
                            asset_id: assets.0.clone(),
                            balance: U128(0),
                        },
                        AssetWithBalance {
                            asset_id: assets.1.clone(),
                            balance: U128(0),
                        },
                    ),
                    fees: fees.clone(),
                    owner_id: near_sdk::env::predecessor_account_id(),
                }
            }
            PoolType::PublicV1 => {
                expect!(
                    attached_assets.is_empty(),
                    "No assets other than NEAR should be attached"
                );
                expect!(
                    let FeeConfiguration::V1(fees) = fees,
                    "Only V1 fees are supported for PublicV1 pools"
                );
                Pool::PublicV1 {
                    assets: (
                        AssetWithBalance {
                            asset_id: assets.0.clone(),
                            balance: U128(0),
                        },
                        AssetWithBalance {
                            asset_id: assets.1.clone(),
                            balance: U128(0),
                        },
                    ),
                    fees: fees.clone(),
                    user_shares: LookupMap::new(StorageKey::PublicPoolUserShares { pool_id }),
                    total_shares: None,
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
            pool: self.pools.get(pool_id).unwrap().into(),
        }
        .emit();

        #[near(serializers=[borsh])]
        struct CreatePoolResponse {
            pool_id: PoolId,
        }
        let response = CreatePoolResponse { pool_id };
        DexCallResponse {
            asset_withdraw_requests: if let Some(leftover) = attached_near.checked_sub(storage_cost)
            {
                vec![AssetWithdrawRequest {
                    asset_id: AssetId::Near,
                    amount: U128(leftover.as_yoctonear()),
                    withdrawal_type: AssetWithdrawalType::ToInternalUserBalance(
                        near_sdk::env::predecessor_account_id(),
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
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct RegisterLiquidityArgs {
            pool_id: PoolId,
        }
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
        let predecessor_id = near_sdk::env::predecessor_account_id();
        user_shares.entry(predecessor_id).or_insert(None);
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
                    withdrawal_type: AssetWithdrawalType::ToInternalUserBalance(
                        near_sdk::env::predecessor_account_id(),
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
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct AddLiquidityArgs {
            pool_id: PoolId,
            min_shares_received: Option<SharesBalance>,
        }
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
                expect!(
                    *owner_id == near_sdk::env::predecessor_account_id(),
                    "Only pool owner can add liquidity"
                );
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
                    user_shares.contains_key(&near_sdk::env::predecessor_account_id()),
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
                                    .insert(
                                        near_sdk::env::predecessor_account_id(),
                                        Some(INITIAL_SHARES)
                                    )
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
                                .entry(near_sdk::env::predecessor_account_id())
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
                                    withdrawal_type: AssetWithdrawalType::ToInternalUserBalance(
                                        near_sdk::env::predecessor_account_id(),
                                    ),
                                });
                            }
                            let refund_amount_1 =
                                attached_amount_1.0.checked_sub(deposited_amount_1);
                            if let Some(refund_amount_1) = refund_amount_1 {
                                asset_withdraw_requests.push(AssetWithdrawRequest {
                                    asset_id: assets.1.asset_id.clone(),
                                    amount: U128(refund_amount_1),
                                    withdrawal_type: AssetWithdrawalType::ToInternalUserBalance(
                                        near_sdk::env::predecessor_account_id(),
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
                    .get(&near_sdk::env::predecessor_account_id())
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
            Pool::LaunchV1 { .. } => {
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
            pool: (&*pool).into(),
        }
        .emit();

        #[near(serializers=[borsh])]
        struct AddLiquidityResponse;
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
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct RemoveLiquidityArgs {
            pool_id: PoolId,
            shares_to_remove: Option<SharesBalance>,
            min_assets_received: Option<(U128, U128)>,
        }
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
            Pool::LaunchV1 { .. } => false,
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
                    *owner_id == near_sdk::env::predecessor_account_id(),
                    "Only pool owner can remove liquidity"
                );
                let shares_to_remove = shares_to_remove.unwrap_or(INITIAL_SHARES);
                expect!(
                    shares_to_remove.get() <= INITIAL_SHARES.get(),
                    "Shares must be less than {INITIAL_SHARES}. {INITIAL_SHARES} means remove 100% of pool"
                );
                #[allow(clippy::arithmetic_side_effects)]
                // u128 * u128 can't overflow; INITIAL_SHARES is not 0
                let withdrawn_amount_0 = u256_to_u128(
                    u128_to_u256(assets.0.balance.0) * u128_to_u256(shares_to_remove.get())
                        / u128_to_u256(INITIAL_SHARES.get()),
                );
                #[allow(clippy::arithmetic_side_effects)]
                // u128 * u128 can't overflow; INITIAL_SHARES is not 0
                let withdrawn_amount_1 = u256_to_u128(
                    u128_to_u256(assets.1.balance.0) * u128_to_u256(shares_to_remove.get())
                        / u128_to_u256(INITIAL_SHARES.get()),
                );
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
                    .get(&near_sdk::env::predecessor_account_id())
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
                user_shares.insert(near_sdk::env::predecessor_account_id(), user_shares_after);
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
                    near_sdk::env::predecessor_account_id(),
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
            Pool::LaunchV1 { .. } => {
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
            pool: (&*pool).into(),
        }
        .emit();

        #[near(serializers=[borsh])]
        struct RemoveLiquidityResponse;
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
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct EditFeesArgs {
            pool_id: PoolId,
            fees: FeeConfiguration,
        }
        let Ok(EditFeesArgs { pool_id, fees }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let Some(pool) = self.pools.get_mut(pool_id) else {
            panic!("Pool not found");
        };

        let assets = match pool {
            Pool::PrivateV1 {
                assets, owner_id, ..
            } => {
                expect!(
                    *owner_id == near_sdk::env::predecessor_account_id(),
                    "Only pool owner can edit fees"
                );
                assets
            }
            Pool::PublicV1 { .. } | Pool::PublicV2 { .. } => {
                panic!("Fees cannot be edited for public pools");
            }
            Pool::LaunchV1 { .. } => {
                panic!("Fees cannot be edited for launch pools");
            }
            Pool::PrivateV2 {
                assets,
                owner_id,
                locked,
                ..
            } => {
                expect!(!*locked, "Pool is locked");
                expect!(
                    *owner_id == near_sdk::env::predecessor_account_id(),
                    "Only pool owner can edit fees"
                );
                assets
            }
        };

        fees.validate();

        let storage_usage_before = near_sdk::env::storage_usage();
        for (fee_receiver, _) in fees.receivers() {
            match fee_receiver {
                FeeReceiver::Account(account_id) => {
                    expect!(
                        account_id != PROTOCOL_FEE_RECEIVER,
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
            Pool::LaunchV1 { .. } => {
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
            pool: (&*pool).into(),
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
                    withdrawal_type: AssetWithdrawalType::ToInternalUserBalance(
                        near_sdk::env::predecessor_account_id(),
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
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct WithdrawFeesArgs {
            assets: Vec<AssetId>,
        }
        let Ok(WithdrawFeesArgs { assets }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");

        let mut asset_withdraw_requests = Vec::new();
        for asset_id in assets {
            let Some(balance) = self
                .fees_collected_by_users
                .get(&(near_sdk::env::predecessor_account_id(), asset_id.clone()))
                .cloned()
            else {
                continue;
            };
            if balance.0 == 0 {
                continue;
            }
            self.fees_collected_by_users.insert(
                (near_sdk::env::predecessor_account_id(), asset_id.clone()),
                U128(0),
            );
            asset_withdraw_requests.push(AssetWithdrawRequest {
                asset_id,
                amount: balance,
                withdrawal_type: AssetWithdrawalType::ToInternalUserBalance(
                    near_sdk::env::predecessor_account_id(),
                ),
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
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct UpgradePoolArgs {
            pool_id: PoolId,
        }
        let Ok(UpgradePoolArgs { pool_id }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");
        let Some(pool) = self.pools.get_mut(pool_id) else {
            panic!("Pool {pool_id} not found");
        };
        // Anyone can upgrade any pool, even not owned by them
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
            Pool::LaunchV1 { .. } | Pool::PrivateV2 { .. } | Pool::PublicV2 { .. } => {
                panic!("This pool is of the latest version");
            }
        };
        *pool = new_pool;
        XykDexEvent::PoolUpdated {
            pool_id,
            pool: (&*pool).into(),
        }
        .emit();
        self.pools.flush();
        DexCallResponse::default()
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn lock_pool(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct LockPoolArgs {
            pool_id: PoolId,
        }
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
                expect!(
                    *owner_id == near_sdk::env::predecessor_account_id(),
                    "Only pool owner can lock the pool"
                );
                *locked = true;
            }
            Pool::PublicV1 { .. } | Pool::PublicV2 { .. } => {
                panic!("Public pools cannot be locked");
            }
            Pool::LaunchV1 { .. } => {
                panic!("Launch pools are locked by default");
            }
        }
        XykDexEvent::PoolUpdated {
            pool_id,
            pool: (&*pool).into(),
        }
        .emit();
        DexCallResponse::default()
    }

    #[result_serializer(borsh)]
    pub fn get_pool(&self, #[serializer(borsh)] pool_id: PoolId) -> Option<PoolView> {
        self.pools.get(pool_id).map(|pool| pool.into())
    }

    #[result_serializer(borsh)]
    pub fn get_pools(
        &self,
        #[serializer(borsh)] start_index: PoolId,
        #[serializer(borsh)] limit: PoolId,
    ) -> Vec<PoolView> {
        self.pools
            .iter()
            .skip(start_index as usize)
            .take(limit as usize)
            .map(|pool| pool.into())
            .collect()
    }

    #[result_serializer(borsh)]
    pub fn get_pool_shares(
        &self,
        #[serializer(borsh)] pool_ids: Vec<PoolId>,
        #[serializer(borsh)] account_id: AccountId,
    ) -> Vec<Option<U128>> {
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
                    Pool::PrivateV1 { .. } | Pool::LaunchV1 { .. } | Pool::PrivateV2 { .. } => None,
                }
            })
            .collect()
    }

    #[result_serializer(borsh)]
    pub fn get_pending_fees(
        &self,
        #[serializer(borsh)] account_id: AccountId,
        #[serializer(borsh)] asset_ids: Vec<AssetId>,
    ) -> Vec<(AssetId, U128)> {
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
    pub fn get_pool_count(&self) -> PoolId {
        self.pools.len()
    }

    #[result_serializer(borsh)]
    pub fn pool_needs_upgrade(&self, #[serializer(borsh)] pool_id: PoolId) -> bool {
        let Some(pool) = self.pools.get(pool_id) else {
            panic!("Pool {pool_id} not found");
        };
        match pool {
            Pool::PrivateV1 { .. } | Pool::PublicV1 { .. } => true,
            Pool::LaunchV1 { .. } | Pool::PrivateV2 { .. } | Pool::PublicV2 { .. } => false,
        }
    }
}

#[near(serializers=[borsh])]
pub enum Pool {
    PrivateV1 {
        assets: (AssetWithBalance, AssetWithBalance),
        owner_id: AccountId,
        fees: CurrentFees,
    },
    PublicV1 {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        user_shares: LookupMap<AccountId, Option<SharesBalance>>,
        total_shares: Option<SharesBalance>,
    },
    LaunchV1 {
        near_amount: U128,
        launched_asset: AssetWithBalance,
        fees: FeeConfiguration,
        phantom_liquidity_near: U128,
    },
    PrivateV2 {
        assets: (AssetWithBalance, AssetWithBalance),
        owner_id: AccountId,
        fees: FeeConfiguration,
        locked: bool,
    },
    PublicV2 {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: FeeConfiguration,
        user_shares: LookupMap<AccountId, Option<SharesBalance>>,
        total_shares: Option<SharesBalance>,
    },
}

#[near(serializers=[borsh, json])]
pub enum PoolView {
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

impl From<&Pool> for PoolView {
    fn from(pool: &Pool) -> Self {
        match pool {
            Pool::PrivateV1 {
                assets,
                owner_id,
                fees,
            } => PoolView::Private {
                assets: assets.clone(),
                fees: fees.with_protocol_fee(
                    None,
                    asset_account_ids(&[assets.0.asset_id.clone(), assets.1.asset_id.clone()]),
                ),
                fee_configuration: FeeConfiguration::V1(fees.clone()),
                owner_id: owner_id.clone(),
                locked: false,
            },
            Pool::PublicV1 {
                assets,
                fees,
                total_shares,
                user_shares: _,
            } => PoolView::Public {
                assets: assets.clone(),
                fees: fees.with_protocol_fee(
                    None,
                    asset_account_ids(&[assets.0.asset_id.clone(), assets.1.asset_id.clone()]),
                ),
                fee_configuration: FeeConfiguration::V1(fees.clone()),
                total_shares: total_shares.map(|s| U128(s.get())),
            },
            Pool::LaunchV1 {
                near_amount,
                launched_asset,
                fees,
                phantom_liquidity_near,
            } => PoolView::Launch {
                near_amount: *near_amount,
                launched_asset: launched_asset.clone(),
                fees: CurrentFees::from(fees).with_protocol_fee(
                    None,
                    asset_account_ids(&[AssetId::Near, launched_asset.asset_id.clone()]),
                ),
                fee_configuration: fees.clone(),
                phantom_liquidity_near: *phantom_liquidity_near,
            },
            Pool::PrivateV2 {
                assets,
                owner_id,
                fees,
                locked,
            } => PoolView::Private {
                assets: assets.clone(),
                fees: CurrentFees::from(fees).with_protocol_fee(
                    None,
                    asset_account_ids(&[assets.0.asset_id.clone(), assets.1.asset_id.clone()]),
                ),
                fee_configuration: fees.clone(),
                owner_id: owner_id.clone(),
                locked: *locked,
            },
            Pool::PublicV2 {
                assets,
                fees,
                total_shares,
                user_shares: _,
            } => PoolView::Public {
                assets: assets.clone(),
                fees: CurrentFees::from(fees).with_protocol_fee(
                    None,
                    asset_account_ids(&[assets.0.asset_id.clone(), assets.1.asset_id.clone()]),
                ),
                fee_configuration: fees.clone(),
                total_shares: total_shares.map(|s| U128(s.get())),
            },
        }
    }
}

/// When a public pool is created, the creator gets
/// 1e18 shares, which represents 100% of the pool.
/// When someone adds liquidity, the rate of tokens
/// per share is calculated by dividing the total
/// pool value by the total shares, and shares are
/// minted or burnt accordingly.
type SharesBalance = NonZeroU128;

/// Should add up to less than 1000000 (1% = 10000)
#[near(serializers=[borsh, json])]
#[derive(Clone)]
#[serde(untagged)]
pub enum FeeConfiguration {
    V1(CurrentFees),
    V2(V1FeeConfiguration),
}

#[near(serializers=[borsh, json])]
#[derive(Clone)]
pub struct CurrentFees {
    receivers: Vec<(FeeReceiver, FeeFraction)>,
}

#[near(serializers=[borsh, json])]
#[derive(Clone)]
pub struct V1FeeConfiguration {
    receivers: Vec<(FeeReceiver, FeeAmount)>,
}

#[near(serializers=[borsh, json])]
#[derive(Clone, Copy)]
pub enum FeeAmount {
    Fixed(FeeFraction),
    Scheduled {
        start: (Timestamp, FeeFraction),
        end: (Timestamp, FeeFraction),
        curve: ScheduledFeeCurve,
    },
    Dynamic {
        min: FeeFraction,
        max: FeeFraction,
    },
}

impl FeeAmount {
    pub fn validate(&self) {
        match self {
            FeeAmount::Fixed(fee_fraction) => {
                expect!(
                    *fee_fraction < MAX_FEE_FRACTION,
                    "Fee must be less than {MAX_FEE_FRACTION}"
                );
            }
            FeeAmount::Scheduled {
                start,
                end,
                curve: _,
            } => {
                expect!(
                    start.0 < end.0,
                    "Scheduled fee start time must be before end time"
                );
                expect!(
                    start.1 > end.1,
                    "Start fee fraction must be greater than end fee"
                );
                expect!(
                    start.1 < MAX_FEE_FRACTION,
                    "Start fee fraction must be less than {MAX_FEE_FRACTION}"
                );
            }
            FeeAmount::Dynamic { min, max } => {
                expect!(
                    *min < *max,
                    "Min fee fraction must be less than max fee fraction"
                );
                expect!(
                    *max < MAX_FEE_FRACTION,
                    "Max fee fraction must be less than {MAX_FEE_FRACTION}"
                );
                unimplemented!("Dynamic fee configuration is not implemented yet");
            }
        }
    }
}

#[near(serializers=[borsh, json])]
#[derive(Clone, Copy)]
pub enum ScheduledFeeCurve {
    Linear,
}

impl FeeAmount {
    pub fn get_fee_fraction(&self) -> FeeFraction {
        match self {
            FeeAmount::Fixed(fee_fraction) => *fee_fraction,
            FeeAmount::Scheduled { start, end, curve } => {
                let (start_time, start_fee_fraction) = *start;
                let (end_time, end_fee_fraction) = *end;
                let current_timestamp = near_sdk::env::block_timestamp();
                let Some(time_elapsed) = current_timestamp.checked_sub(start_time) else {
                    return start_fee_fraction;
                };
                if current_timestamp >= end_time {
                    return end_fee_fraction;
                }

                // Was checked in .validate() check
                let total_duration = end_time.checked_sub(start_time).unwrap();
                let fee_range = start_fee_fraction.checked_sub(end_fee_fraction).unwrap();

                let fee_decrease = match curve {
                    ScheduledFeeCurve::Linear => {
                        #[allow(clippy::arithmetic_side_effects)]
                        // Multiplying u128 by u128 can't overflow u256, and total_duration
                        // is not 0 due to .validate() check
                        FeeFraction::try_from(u256_to_u128(
                            u128_to_u256(fee_range as u128) * u128_to_u256(time_elapsed as u128)
                                / u128_to_u256(total_duration as u128),
                        ))
                        .expect("Fee decrease overflows u32")
                    }
                };

                expect!(
                    fee_decrease <= fee_range,
                    "Fee decrease must be less than end and start fee difference"
                );

                start_fee_fraction
                    .checked_sub(fee_decrease)
                    .expect("Fee calculation underflow")
            }
            FeeAmount::Dynamic { min: _, max: _ } => {
                unimplemented!("Dynamic fee configuration is not implemented yet");
            }
        }
    }
}

/// 100% = 1000000
type FeeFraction = u32;

impl FeeConfiguration {
    fn receivers(&self) -> Vec<(FeeReceiver, FeeFraction)> {
        match self {
            FeeConfiguration::V1(fees) => fees.receivers.clone(),
            FeeConfiguration::V2(fees) => fees
                .receivers
                .iter()
                .map(|(receiver, fee)| (receiver.clone(), fee.get_fee_fraction()))
                .collect(),
        }
    }

    fn validate(&self) {
        match self {
            FeeConfiguration::V1(_) => {}
            FeeConfiguration::V2(fees) => {
                fees.receivers
                    .iter()
                    .for_each(|(_, fee_amount)| fee_amount.validate());
            }
        }
        let receivers = self.receivers();
        expect!(receivers.len() <= 42, "Too many fee receivers");
        expect!(
            receivers.iter().all(|(_, fee)| *fee < MAX_FEE_FRACTION),
            "Fee must be less than {MAX_FEE_FRACTION} per receiver"
        );
        expect!(
            receivers
                .iter()
                .map(|(_, fee)| *fee)
                .try_fold(0u32, |acc, fee| acc.checked_add(fee))
                .unwrap()
                < MAX_FEE_FRACTION / 2,
            "Fees must add up to less than 50% ({})",
            MAX_FEE_FRACTION / 2,
        );
        expect!(
            !receivers.iter().any(|(receiver, _)| matches!(
                receiver,
                FeeReceiver::Account(account_id) if account_id.as_str() == PROTOCOL_FEE_RECEIVER
            )),
            "Protocol fee receiver can't be set by users"
        );
    }
}

impl From<&FeeConfiguration> for CurrentFees {
    fn from(fee_configuration: &FeeConfiguration) -> Self {
        Self {
            receivers: fee_configuration.receivers(),
        }
    }
}

impl CurrentFees {
    fn with_protocol_fee(
        &self,
        trader_account_id: Option<AccountId>,
        asset_account_ids: Vec<AccountId>,
    ) -> CurrentFees {
        let mut receivers = self.receivers.clone();
        let mut protocol_fee = PROTOCOL_FEE;
        if receivers.is_empty() {
            protocol_fee = 0;
        } else if let Some(trader_account_id) = trader_account_id {
            let mut has_non_stable_assets = false;
            'assets: for asset_account_id in asset_account_ids {
                for reduce_asset_account in PROTOCOL_FEE_REDUCE_ASSET_ACCOUNTS {
                    if asset_account_id == *reduce_asset_account {
                        continue 'assets;
                    }
                }
                for bypass_asset_parent_account in PROTOCOL_FEE_REDUCE_ASSET_PARENT_ACCOUNTS {
                    let protocol_fee_bypass_asset_parent_account =
                        bypass_asset_parent_account.parse::<AccountId>().unwrap();
                    if asset_account_id.is_sub_account_of(&protocol_fee_bypass_asset_parent_account)
                    {
                        continue 'assets;
                    }
                }
                // If not matched by either of the above, it's a non-stable asset
                has_non_stable_assets = true;
            }
            if !has_non_stable_assets {
                protocol_fee = PROTOCOL_FEE_REDUCED;
            }
            for bypass_parent_account in PROTOCOL_FEE_BYPASS_PARENT_ACCOUNTS {
                let protocol_fee_bypass_parent_account =
                    bypass_parent_account.parse::<AccountId>().unwrap();
                if trader_account_id.is_sub_account_of(&protocol_fee_bypass_parent_account) {
                    protocol_fee = 0;
                    break;
                }
            }
        }
        receivers.push((
            FeeReceiver::Account(PROTOCOL_FEE_RECEIVER.parse().unwrap()),
            protocol_fee,
        ));
        Self { receivers }
    }
}

#[near(serializers=[borsh, json])]
#[derive(PartialEq, Clone)]
pub enum FeeReceiver {
    Account(AccountId),
    Pool,
}

#[near(serializers=[borsh, json])]
#[derive(Clone)]
pub struct AssetWithBalance {
    asset_id: AssetId,
    balance: U128,
}
