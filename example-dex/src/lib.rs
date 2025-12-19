#![deny(clippy::arithmetic_side_effects)]

use std::collections::HashMap;

use intear_dex_types::{
    AssetId, AssetWithdrawRequest, AssetWithdrawalType, Dex, DexCallResponse, SwapRequest,
    SwapRequestAmount, SwapResponse, expect,
};
use near_sdk::{
    AccountId, BorshStorageKey, NearToken, PanicOnDefault, json_types::U128, near, store::LookupMap,
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
pub struct SimpleSwap {
    pools: LookupMap<PoolId, SimplePool>,
    pool_counter: PoolId,
}

#[near(serializers=[borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Pools,
}

uint::construct_uint! {
    pub struct U256(4);
}

#[near(event_json(standard = "simpleswap"))]
pub enum SimpleSwapEvent {
    #[event_version("1.0.0")]
    PoolCreated {
        pool_id: PoolId,
        owner_id: AccountId,
        assets: (AssetId, AssetId),
    },
    #[event_version("1.0.0")]
    LiquidityAdded {
        pool_id: PoolId,
        amounts: (U128, U128),
    },
    #[event_version("1.0.0")]
    LiquidityRemoved {
        pool_id: PoolId,
        amounts: (U128, U128),
    },
}

#[near]
impl Dex for SimpleSwap {
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
                // in_balance was checked to be positive
                #[allow(clippy::arithmetic_side_effects)]
                let amount_out = (U256::from(exact_amount_in.0) * U256::from(*out_balance)
                    / (U256::from(*in_balance) + U256::from(exact_amount_in.0)))
                .as_u128();
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
                // amount_out was checked to be less than out_balance
                #[allow(clippy::arithmetic_side_effects)]
                let amount_in = ((U256::from(*in_balance) * U256::from(exact_amount_out.0))
                    / (U256::from(*out_balance) - U256::from(exact_amount_out.0))
                    + U256::one())
                .as_u128();
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
impl SimpleSwap {
    #[init]
    pub fn new() -> Self {
        Self {
            pools: LookupMap::new(StorageKey::Pools),
            pool_counter: 0,
        }
    }

    #[result_serializer(borsh)]
    pub fn create_pool(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        #[near(serializers=[borsh])]
        struct CreatePoolArgs {
            assets: (AssetId, AssetId),
        }
        let Ok(CreatePoolArgs { assets }) = near_sdk::borsh::from_slice(&args) else {
            panic!("Invalid args");
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
                        asset_id: assets.1.clone(),
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

        SimpleSwapEvent::PoolCreated {
            pool_id,
            owner_id: near_sdk::env::predecessor_account_id(),
            assets,
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

    #[result_serializer(borsh)]
    pub fn add_liquidity(
        &mut self,
        #[allow(unused_mut)]
        #[serializer(borsh)]
        mut attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        #[near(serializers=[borsh])]
        struct AddLiquidityArgs {
            pool_id: PoolId,
        }
        let Ok(AddLiquidityArgs { pool_id }) = near_sdk::borsh::from_slice(&args) else {
            panic!("Invalid args");
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

        SimpleSwapEvent::LiquidityAdded {
            pool_id,
            amounts: (asset_1_amount, asset_2_amount),
        }
        .emit();

        #[near(serializers=[borsh])]
        struct AddLiquidityResponse;
        let response = AddLiquidityResponse;
        DexCallResponse {
            asset_withdraw_requests: vec![],
            add_storage_deposit: NearToken::ZERO,
            response: near_sdk::borsh::to_vec(&response).expect("Failed to serialize response"),
        }
    }

    #[result_serializer(borsh)]
    pub fn remove_liquidity(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
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
            panic!("Invalid args");
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

        SimpleSwapEvent::LiquidityRemoved {
            pool_id,
            amounts: assets_to_remove,
        }
        .emit();

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
            add_storage_deposit: NearToken::ZERO,
            response: near_sdk::borsh::to_vec(&response).expect("Failed to serialize response"),
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
