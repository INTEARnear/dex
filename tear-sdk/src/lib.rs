use near_sdk::{AccountId, json_types::U128, near};

#[near(serializers=[json])]
pub struct SwapRequest {
    pub pool_id: PoolId,
    pub asset_in: AssetId,
    pub asset_out: AssetId,
    pub amount: SwapRequestAmount,
}

#[near(serializers=[json])]
pub enum SwapResponse {
    Ok { amount_in: U128, amount_out: U128 },
    Error { message: String },
}

pub type PoolId = String;

#[derive(PartialEq, Eq, Hash, Clone, PartialOrd, Ord)]
#[near(serializers=[json, borsh])]
pub enum AssetId {
    Near,
    Nep141(AccountId),
    Nep245(AccountId, String),
    Nep171(AccountId, String),
}

#[near(serializers=[json])]
pub enum SwapRequestAmount {
    ExactIn(U128),
    ExactOut(U128),
}
