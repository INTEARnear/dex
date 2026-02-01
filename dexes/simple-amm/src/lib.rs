#![deny(clippy::arithmetic_side_effects)]

use std::collections::HashMap;

use crypto_bigint::U256;
use intear_dex_types::{
    AssetId, AssetWithdrawRequest, AssetWithdrawalType, Dex, DexCallResponse, SwapRequest,
    SwapRequestAmount, SwapResponse, expect,
};
use near_sdk::{
    AccountId, BorshStorageKey, NearToken, PanicOnDefault, assert_one_yocto, json_types::U128,
    near, store::LookupMap,
};

#[global_allocator]
static ALLOCATOR: talc::Talck<talc::locking::AssumeUnlockable, talc::ClaimOnOom> = {
    static mut MEMORY: [u8; 0x8000] = [0; 0x8000]; // 32KB
    let span = talc::Span::from_array(core::ptr::addr_of!(MEMORY).cast_mut());
    talc::Talc::new(unsafe { talc::ClaimOnOom::new(span) }).lock()
};

type PoolId = u64;

/// The simplest possible x*y=k pool, with just one
/// liquidity provider. Demonstrates the basic functionality
/// of swaps, adding / withdrawing liquidity, storage
/// management, and event emission.
#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct SimpleAmmDex {
    pools: LookupMap<PoolId, SimplePool>,
    pool_counter: PoolId,
}

#[near(serializers=[borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Pools,
}

#[near]
impl Dex for SimpleAmmDex {
    #[result_serializer(borsh)]
    fn swap(&mut self, #[serializer(borsh)] request: SwapRequest) -> SwapResponse {
        #[near(serializers=[borsh])]
        struct SwapArgs {
            pool_id: PoolId,
        }
        let Ok(SwapArgs { pool_id }) = near_sdk::borsh::from_slice(&request.message.0) else {
            panic!("Invalid message");
        };
        let Some(pool) = self.pools.get_mut(&pool_id) else {
            panic!("Pool not found");
        };
        expect!(
            pool.assets.0.asset_id == request.asset_in
                || pool.assets.1.asset_id == request.asset_in,
            "Invalid asset in"
        );
        expect!(
            pool.assets.0.asset_id == request.asset_out
                || pool.assets.1.asset_id == request.asset_out,
            "Invalid asset out"
        );
        expect!(
            pool.assets.0.balance.0 > 0 && pool.assets.1.balance.0 > 0,
            "Pool is empty"
        );
        let first_in = pool.assets.0.asset_id == request.asset_in;

        match request.amount {
            SwapRequestAmount::ExactIn(exact_amount_in) => {
                expect!(exact_amount_in.0 > 0, "Amount must be greater than 0");
                let (in_balance, out_balance) = if first_in {
                    (&mut pool.assets.0.balance.0, &mut pool.assets.1.balance.0)
                } else {
                    (&mut pool.assets.1.balance.0, &mut pool.assets.0.balance.0)
                };
                #[allow(clippy::arithmetic_side_effects)]
                let amount_out = u128::from_le_bytes(
                    *(U256::from(exact_amount_in.0) * U256::from(*out_balance)
                        / (U256::from(*in_balance) + U256::from(exact_amount_in.0)))
                    .to_le_bytes()
                    .first_chunk()
                    .unwrap(),
                );
                *in_balance = in_balance.checked_add(exact_amount_in.0).expect("Overflow");
                *out_balance = out_balance.checked_sub(amount_out).expect("Underflow");
                SwapResponse {
                    amount_in: exact_amount_in,
                    amount_out: U128(amount_out),
                }
            }
            SwapRequestAmount::ExactOut(exact_amount_out) => {
                expect!(exact_amount_out.0 > 0, "Amount must be greater than 0");
                let (in_balance, out_balance) = if first_in {
                    (&mut pool.assets.0.balance.0, &mut pool.assets.1.balance.0)
                } else {
                    (&mut pool.assets.1.balance.0, &mut pool.assets.0.balance.0)
                };
                expect!(
                    exact_amount_out.0 < *out_balance,
                    "Amount must be less than out balance"
                );
                #[allow(clippy::arithmetic_side_effects)]
                let amount_in = u128::from_le_bytes(
                    *((U256::from(*in_balance) * U256::from(exact_amount_out.0))
                        / (U256::from(*out_balance) - U256::from(exact_amount_out.0)))
                    .saturating_add(&U256::ONE)
                    .to_le_bytes()
                    .first_chunk()
                    .unwrap(),
                );
                *in_balance = in_balance.checked_add(amount_in).expect("Overflow");
                *out_balance = out_balance
                    .checked_sub(exact_amount_out.0)
                    .expect("Underflow");
                SwapResponse {
                    amount_in: U128(amount_in),
                    amount_out: U128(exact_amount_out.0),
                }
            }
        }
    }
}

#[near]
impl SimpleAmmDex {
    #[init]
    #[payable]
    pub fn new() -> Self {
        assert_one_yocto();
        Self {
            pools: LookupMap::new(StorageKey::Pools),
            pool_counter: 0,
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
        }
        let Ok(CreatePoolArgs { assets }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(assets.0 != assets.1, "Assets must be different");

        let pool_id = self.pool_counter;
        self.pool_counter = self
            .pool_counter
            .checked_add(1)
            .expect("Pool counter overflow");

        let storage_usage_before = near_sdk::env::storage_usage();
        let old_pool_with_same_id = self.pools.insert(
            pool_id,
            SimplePool {
                assets: (
                    AssetWithBalance {
                        asset_id: assets.0.clone(),
                        balance: U128(0),
                    },
                    AssetWithBalance {
                        asset_id: assets.1,
                        balance: U128(0),
                    },
                ),
                owner_id: near_sdk::env::predecessor_account_id(),
            },
        );
        expect!(
            old_pool_with_same_id.is_none(),
            "Pool with same id somehow already exists"
        );

        self.pools.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        let storage_cost = near_sdk::env::storage_byte_cost().saturating_mul(
            (storage_usage_after as u128)
                .checked_sub(storage_usage_before as u128)
                .expect("Can't possibly be lower after inserting"),
        );

        let attached_near = NearToken::from_yoctonear(
            attached_assets
                .remove(&AssetId::Near)
                .expect("Near should be attached for storage")
                .0,
        );
        expect!(
            attached_near >= storage_cost,
            "Not enough near attached for storage. Required: {storage_cost}, attached: {attached_near}"
        );
        expect!(
            attached_assets.is_empty(),
            "No assets other than NEAR should be attached"
        );

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
    pub fn add_liquidity(
        &mut self,
        #[allow(unused_mut)]
        #[serializer(borsh)]
        mut attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct AddLiquidityArgs {
            pool_id: PoolId,
        }
        let Ok(AddLiquidityArgs { pool_id }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let Some(pool) = self.pools.get_mut(&pool_id) else {
            panic!("Pool not found");
        };
        expect!(
            pool.owner_id == near_sdk::env::predecessor_account_id(),
            "Only pool owner can add liquidity"
        );
        let asset_1_amount = attached_assets
            .remove(&pool.assets.0.asset_id)
            .expect("Asset 1 not found");
        let asset_2_amount = attached_assets
            .remove(&pool.assets.1.asset_id)
            .expect("Asset 2 not found");
        expect!(
            attached_assets.is_empty(),
            "No assets other than the two pool assets should be attached"
        );
        pool.assets.0.balance.0 = pool
            .assets
            .0
            .balance
            .0
            .checked_add(asset_1_amount.0)
            .expect("Overflow");
        pool.assets.1.balance.0 = pool
            .assets
            .1
            .balance
            .0
            .checked_add(asset_2_amount.0)
            .expect("Overflow");

        #[near(serializers=[borsh])]
        struct AddLiquidityResponse;
        let response = AddLiquidityResponse;
        DexCallResponse {
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
            assets_to_remove: (U128, U128),
        }
        let Ok(RemoveLiquidityArgs {
            pool_id,
            assets_to_remove,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");
        let Some(pool) = self.pools.get_mut(&pool_id) else {
            panic!("Pool not found");
        };
        expect!(
            pool.owner_id == near_sdk::env::predecessor_account_id(),
            "Only pool owner can remove liquidity"
        );
        pool.assets.0.balance.0 = pool
            .assets
            .0
            .balance
            .0
            .checked_sub(assets_to_remove.0.0)
            .expect("Not enough balance for asset 1 withdrawal");
        pool.assets.1.balance.0 = pool
            .assets
            .1
            .balance
            .0
            .checked_sub(assets_to_remove.1.0)
            .expect("Not enough balance for asset 2 withdrawal");

        #[near(serializers=[borsh])]
        struct RemoveLiquidityResponse;
        let response = RemoveLiquidityResponse;
        DexCallResponse {
            asset_withdraw_requests: vec![
                AssetWithdrawRequest {
                    asset_id: pool.assets.0.asset_id.clone(),
                    amount: assets_to_remove.0,
                    withdrawal_type: AssetWithdrawalType::ToInternalUserBalance(
                        pool.owner_id.clone(),
                    ),
                },
                AssetWithdrawRequest {
                    asset_id: pool.assets.1.asset_id.clone(),
                    amount: assets_to_remove.1,
                    withdrawal_type: AssetWithdrawalType::ToInternalUserBalance(
                        pool.owner_id.clone(),
                    ),
                },
            ],
            response: near_sdk::borsh::to_vec(&response).expect("Failed to serialize response"),
            ..Default::default()
        }
    }

    #[result_serializer(borsh)]
    pub fn get_pool(&self, #[serializer(borsh)] pool_id: PoolId) -> Option<&SimplePool> {
        self.pools.get(&pool_id)
    }
}

#[near(serializers=[borsh])]
pub struct SimplePool {
    assets: (AssetWithBalance, AssetWithBalance),
    owner_id: AccountId,
}

#[near(serializers=[borsh])]
pub struct AssetWithBalance {
    asset_id: AssetId,
    balance: U128,
}
