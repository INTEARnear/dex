#![deny(clippy::arithmetic_side_effects)]

use intear_dex_types::{AssetId, DexId};
use near_sdk::{
    AccountId, BlockHeight, NearToken, PublicKey,
    json_types::{Base64VecU8, U64, U128},
    near,
};

pub type Nonce = U128;

#[derive(Default)]
#[near(serializers=[borsh])]
pub struct StorageBalance {
    pub total: NearToken,
    pub used: NearToken,
}

#[near(serializers=[borsh])]
pub enum AuthorizationMethod {
    /// Signature of the sha256 of the borsh of the [`TradeIntentSignaturePayload`]
    /// of the trade intent and `nonce`, by the authorized key of the user. The
    /// nonce can't be used again while the intent is valid
    Signature {
        signature: Base64VecU8,
        nonce: Nonce,
    },
    /// The user calls the dex directly, or sends the attached assets in a transfer
    /// call
    Predecessor,
}

#[near(serializers=[borsh])]
pub struct AuthorizedTradeIntent {
    pub trade_intent: TradeIntent,
    pub authorization_method: AuthorizationMethod,
}

#[near(serializers=[borsh])]
#[derive(Clone, Debug)]
pub struct TradeIntent {
    pub user_id: AccountId,
    pub asset_in: AssetId,
    pub asset_out: AssetId,
    pub amount_in: U128,
    pub amount_out: U128,
    pub validity: Validity,
}

/// What the authorized key of the user signs for [`AuthorizationMethod::Signature`]
#[near(serializers=[borsh])]
pub struct TradeIntentSignaturePayload {
    pub chain_id: String,
    pub engine_contract_id: AccountId,
    pub dex_id: DexId,
    pub trade_intent: TradeIntent,
    pub nonce: Nonce,
}

#[derive(Default, Clone, PartialEq, Debug)]
#[near(serializers=[borsh])]
pub struct Validity {
    pub expiry: Option<ExpiryCondition>,
    pub only_for_whitelisted_parties: Option<Vec<AccountId>>,
}

#[derive(PartialEq, Clone, Copy, Debug)]
#[near(serializers=[borsh])]
pub enum ExpiryCondition {
    BlockHeight(BlockHeight),
    Timestamp { milliseconds: U64 },
}

/// The events of the dex, logged as borsh with the engine's `dex_event` host function and
/// carried in its `dex_events_borsh` log
#[near(serializers=[borsh])]
pub enum OtcDexEvent {
    AuthorizedKeyChanged {
        account_id: AccountId,
        key: PublicKey,
    },
    Trade {
        authorized_trade_intents: Vec<AuthorizedTradeIntent>,
    },
}

impl OtcDexEvent {
    #[cfg(feature = "dex")]
    pub fn emit(&self) {
        intear_dex_types::env::dex_event(self);
    }
}

/// Where the outputs of the caller's own intents go. The outputs of the other
/// intents go to the OTC balances of their users.
#[near(serializers=[borsh])]
pub enum OutputDestination {
    InternalOtcBalance,
    IntearDexBalance,
    WithdrawToUser,
}

#[near(serializers=[borsh])]
pub struct MatchArgs {
    pub authorized_trade_intents: Vec<AuthorizedTradeIntent>,
    pub output_destination: OutputDestination,
}

#[near(serializers=[borsh])]
pub struct StorageDepositArgs;

#[near(serializers=[borsh])]
pub struct SetAuthorizedKeyArgs {
    pub key: PublicKey,
}

#[near(serializers=[borsh])]
pub struct DepositAssetsArgs;

#[near(serializers=[borsh])]
pub struct RegisterAssetsArgs {
    pub asset_ids: Vec<AssetId>,
}

#[near(serializers=[borsh])]
pub struct WithdrawAssetsArgs {
    pub assets: Vec<WithdrawRequest>,
}

#[near(serializers=[borsh])]
pub struct WithdrawRequest {
    pub asset_id: AssetId,
    /// If None, the entire balance of the asset will be withdrawn.
    pub amount: Option<U128>,
    /// If None, the assets will be withdrawn to the user's own account.
    pub to: Option<AccountId>,
    pub to_inner_balance: bool,
}

#[near(serializers=[borsh])]
pub struct StorageBalanceOfArgs {
    pub account_id: AccountId,
}

#[near(serializers=[borsh])]
pub struct GetAuthorizedKeyArgs {
    pub account_id: AccountId,
}

#[near(serializers=[borsh])]
pub struct IsNonceUsedArgs {
    pub nonce: Nonce,
    pub account_id: AccountId,
}

#[near(serializers=[borsh])]
pub struct GetBalanceArgs {
    pub account_id: AccountId,
    pub asset_id: AssetId,
}
