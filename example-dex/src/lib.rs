#![no_main]

use near_sdk::{BorshStorageKey, json_types::U128, near, require, store::LookupMap};
use tear_sdk::{AssetId, PoolId, SwapRequest, SwapRequestAmount, SwapResponse};

#[near(contract_state)]
pub struct SimpleSwap {
    pools: LookupMap<PoolId, SimplePool>,
}

#[near(serializers=[borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Pools,
}

uint::construct_uint! {
    pub struct U256(4);
}

impl Default for SimpleSwap {
    fn default() -> Self {
        Self {
            pools: LookupMap::new(StorageKey::Pools),
        }
    }
}

#[near]
impl SimpleSwap {
    pub fn swap(&mut self, request: SwapRequest) -> SwapResponse {
        if request.pool_id == "1" {
            return SwapResponse::Error {
                message: "Everything ok, this message comes from wasm, but the dex itself is not implemented".to_string(),
            };
        }
        let Some(pool) = self.pools.get(&request.pool_id) else {
            return SwapResponse::Error {
                message: "Pool not found".to_string(),
            };
        };
        require!(
            pool.assets.0.asset_id == request.asset_in
                || pool.assets.1.asset_id == request.asset_in,
            "Invalid asset in"
        );
        require!(
            pool.assets.0.asset_id == request.asset_out
                || pool.assets.1.asset_id == request.asset_out,
            "Invalid asset out"
        );
        let first_in = pool.assets.0.asset_id == request.asset_in;

        match request.amount {
            SwapRequestAmount::ExactIn(amount_in) => {
                require!(amount_in.0 > 0, "Amount must be greater than 0");
                let in_balance = U256::from(if first_in {
                    pool.assets.0.balance.0
                } else {
                    pool.assets.1.balance.0
                });
                let out_balance = U256::from(if first_in {
                    pool.assets.1.balance.0
                } else {
                    pool.assets.0.balance.0
                });
                let amount_out = (U256::from(amount_in.0) * out_balance
                    / (in_balance + U256::from(amount_in.0)))
                .as_u128();
                SwapResponse::Ok {
                    amount_in,
                    amount_out: U128(amount_out),
                }
            }
            SwapRequestAmount::ExactOut(amount_out) => {
                require!(amount_out.0 > 0, "Amount must be greater than 0");
                let in_balance = U256::from(if first_in {
                    pool.assets.0.balance.0
                } else {
                    pool.assets.1.balance.0
                });
                let out_balance = U256::from(if first_in {
                    pool.assets.1.balance.0
                } else {
                    pool.assets.0.balance.0
                });
                let amount_in = ((in_balance * U256::from(amount_out.0))
                    / (out_balance - U256::from(amount_out.0))
                    + U256::one())
                .as_u128();
                SwapResponse::Ok {
                    amount_in: U128(amount_in),
                    amount_out: U128(amount_out.0),
                }
            }
        }
    }
}

#[near(serializers=[borsh])]
struct SimplePool {
    assets: (SimplePoolAsset, SimplePoolAsset),
}

#[near(serializers=[borsh])]
struct SimplePoolAsset {
    asset_id: AssetId,
    balance: U128,
}
