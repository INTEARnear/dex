use std::{collections::HashMap, fmt::Display};

use near_sdk::{
    AccountId,
    json_types::{Base58CryptoHash, Base64VecU8, U128},
    near,
    serde::Deserialize,
};

use crate::{AssetId, DexId, SwapRequest, SwapRequestAmount};

#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[json])]
pub enum AccountOrDexId {
    Account(AccountId),
    Dex(DexId),
}

impl Display for AccountOrDexId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Account(account) => write!(f, "Account({account})"),
            Self::Dex(dex_id) => write!(f, "Dex({dex_id})"),
        }
    }
}

#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[json])]
pub enum SwapOperationAmount {
    Amount(SwapRequestAmount),
    OutputOfLastIn,
    EntireBalanceIn,
}

#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[json])]
pub enum Operation {
    /// Register storage for assets. No-op if assets are
    /// already registered for the given account or dex.
    RegisterAssets {
        asset_ids: Vec<AssetId>,
        r#for: Option<AccountOrDexId>,
    },
    /// Deploy new code to your dex.
    DeployDexCode {
        last_part_of_id: String,
        code_base64: Base64VecU8,
    },
    /// Withdraw assets from the dex engine contract's inner
    /// balance to the user. If amount is None, the entire
    /// balance of the asset will be withdrawn.
    Withdraw {
        asset_id: AssetId,
        amount: WithdrawAmount,
        to: Option<AccountId>,
        /// If the withdrawal fails and current user doesn't have
        /// a registerd balance in this asset, the assets will be
        /// refunded to this address. It's required that either
        /// the user address or rescue address is registered.
        rescue_address: Option<AccountId>,
    },
    /// Swap assets between two assets on the selected dex.
    SwapSimple {
        dex_id: DexId,
        message: Base64VecU8,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapOperationAmount,
        /// Either minimum amount out (for ExactIn) or maximum amount in (for ExactOut)
        constraint: Option<U128>,
    },
    /// Call a method on a dex.
    DexCall {
        dex_id: DexId,
        method: String,
        args: Base64VecU8,
        attached_assets: HashMap<AssetId, U128>,
    },
    /// Transfer assets to a different account or dex.
    TransferAsset {
        to: AccountOrDexId,
        asset_id: AssetId,
        amount: U128,
    },
    /// Convert some of AssetId::Near to storage for an account
    /// or a dex.
    StorageDeposit {
        amount: U128,
        r#for: Option<AccountOrDexId>,
    },
}

#[near(serializers=[json])]
#[derive(Clone, Copy)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum WithdrawAmount {
    Full { at_least: Option<U128> },
    Exact(U128),
    PreviousSwapOutput,
}

#[near(serializers=[json])]
#[derive(Clone, Copy)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum DirectWithdrawAmount {
    Full { at_least: Option<U128> },
    Exact(U128),
}

#[near(serializers=[json])]
#[serde(untagged)]
pub enum DepositMessage {
    Operations(Vec<Operation>),
    Advanced {
        operations: Vec<Operation>,
        referrer: Option<AccountId>,
    },
}

impl DepositMessage {
    pub fn operations(&self) -> Vec<Operation> {
        match self {
            Self::Operations(operations) => operations.clone(),
            Self::Advanced { operations, .. } => operations.clone(),
        }
    }

    pub fn referrer(&self) -> Option<AccountId> {
        match self {
            Self::Advanced { referrer, .. } => referrer.clone(),
            _ => None,
        }
    }
}

#[near(event_json(standard = "inteardex"))]
#[derive(Deserialize)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum IntearDexEvent {
    #[event_version("1.0.0")]
    DexDeployed {
        dex_id: DexId,
        code_hash: Base58CryptoHash,
    },
    #[event_version("1.0.0")]
    DexEvent {
        dex_id: DexId,
        event: near_sdk::serde_json::Value,
        referrer: Option<AccountId>,
        user: Option<AccountId>,
    },
    #[event_version("1.0.0")]
    UserDeposit {
        account_id: AccountId,
        asset_id: AssetId,
        amount: U128,
    },
    #[event_version("1.0.0")]
    Withdraw {
        from: AccountOrDexId,
        to: AccountId,
        asset_id: AssetId,
        amount: U128,
    },
    #[event_version("1.0.0")]
    UserBalanceUpdate {
        account_id: AccountId,
        asset_id: AssetId,
        balance: U128,
    },
    #[event_version("1.0.0")]
    DexBalanceUpdate {
        dex_id: DexId,
        asset_id: AssetId,
        balance: U128,
    },
    #[event_version("1.0.0")]
    Swap {
        dex_id: DexId,
        request: SwapRequest,
        amount_in: U128,
        amount_out: U128,
        trader: AccountId,
        referrer: Option<AccountId>,
    },
}
