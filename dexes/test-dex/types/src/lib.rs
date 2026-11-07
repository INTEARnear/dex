use intear_dex_types::AssetId;
use near_sdk::{AccountId, PublicKeyHandle, json_types::U128, near};

pub type PoolId = u64;

/// The message of a swap, which picks how the dex swaps
#[near(serializers=[borsh])]
pub enum SwapMessage {
    /// Swap an asset for the same amount of itself
    Same,
    /// Swap on an x * y = k pool
    Pool { pool_id: PoolId },
    /// Swap at the rate that the oracle with `oracle_key` responded to
    /// `request` with: the borsh of a u128 of output per input. The dex
    /// pays the output from its own balance, and trusts any oracle.
    OracleRate {
        oracle_key: PublicKeyHandle,
        request: Vec<u8>,
    },
}

#[near(serializers=[borsh])]
pub struct CreatePoolArgs {
    pub assets: (AssetId, AssetId),
}

#[near(serializers=[borsh])]
pub struct CreatePoolResponse {
    pub pool_id: PoolId,
}

#[near(serializers=[borsh])]
pub struct AddLiquidityArgs {
    pub pool_id: PoolId,
}

#[near(serializers=[borsh])]
pub struct RemoveLiquidityArgs {
    pub pool_id: PoolId,
    pub assets_to_remove: (U128, U128),
}

#[derive(PartialEq, Debug)]
#[near(serializers=[borsh])]
pub struct SimplePool {
    pub assets: (AssetWithBalance, AssetWithBalance),
    pub owner_id: AccountId,
}

#[derive(PartialEq, Debug)]
#[near(serializers=[borsh])]
pub struct AssetWithBalance {
    pub asset_id: AssetId,
    pub balance: U128,
}
