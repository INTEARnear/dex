use color_eyre::Section;
use color_eyre::eyre::Report;
use intear_dex_types::{AssetId, CAN_PAUSE, DexId};
use near_primitives::types::AccountId;
use xyk_dex_types::{
    CAN_MIGRATE, FeeAmountError, FeeConfigurationError, MAX_FEE_RECEIVERS,
    PROTOCOL_FEE_RECEIVER_ID, PoolId,
};

use crate::deployment::ENGINE_ACCOUNT_ID;

/// Problems found in chain state before signing. Suggested commands end with
/// the network step, so near-cli-rs only asks how to sign.
#[derive(Debug, thiserror::Error)]
pub enum PreflightError {
    #[error("{account_id} doesn't exist on network {network_name}")]
    AccountMissing {
        account_id: AccountId,
        network_name: String,
    },
    #[error("Dex {dex_id} doesn't exist on {ENGINE_ACCOUNT_ID} (network {network_name})")]
    DexMissing { dex_id: DexId, network_name: String },
    #[error("{ENGINE_ACCOUNT_ID} is paused, so nothing can change until it's unpaused")]
    EnginePaused,
    #[error("{signer_id} can't pause or unpause {ENGINE_ACCOUNT_ID}")]
    NotAllowedToPause { signer_id: AccountId },
    #[error("{ENGINE_ACCOUNT_ID} is already paused")]
    AlreadyPaused,
    #[error("{ENGINE_ACCOUNT_ID} isn't paused")]
    NotPaused,
    #[error(
        "{account_id} can spend {available} from its wallet, but this transaction needs up to {needed}"
    )]
    InsufficientWalletBalance {
        account_id: AccountId,
        available: String,
        needed: String,
    },
    #[error("{account_id} has {available} in its wallet, but this needs {needed}")]
    InsufficientTokenBalance {
        account_id: AccountId,
        available: String,
        needed: String,
    },
    #[error("{owner} has {available} on {ENGINE_ACCOUNT_ID}, but this needs {needed}")]
    InsufficientEngineBalance {
        owner: String,
        available: String,
        needed: String,
    },
    #[error("{asset_label} isn't registered for {signer_id} on {ENGINE_ACCOUNT_ID}")]
    AssetNotRegisteredForSigner {
        signer_id: AccountId,
        asset_label: String,
        register_command: String,
    },
    #[error("{assets} already registered for {owner}")]
    AlreadyRegistered { owner: String, assets: String },
    #[error("{owner} has no storage balance on {ENGINE_ACCOUNT_ID}")]
    NoStorageBalance { owner: String },
    #[error("{owner} has no storage balance available to withdraw on {ENGINE_ACCOUNT_ID}")]
    NoStorageAvailable { owner: String },
    #[error("{owner} has no {asset_label} on {ENGINE_ACCOUNT_ID}")]
    NoBalance { owner: String, asset_label: String },
    #[error("{signer_id} can't transfer to itself")]
    TransferToSelf { signer_id: AccountId },
    #[error("Depositing {asset_id} isn't supported yet")]
    UnsupportedDeposit { asset_id: String },
    #[error("{owner} has {available} of storage balance available, less than {requested}")]
    StorageWithdrawalTooLarge {
        owner: String,
        available: String,
        requested: String,
    },
    #[error("Storage deposits on {ENGINE_ACCOUNT_ID} must be at least {minimum}")]
    StorageDepositTooSmall { minimum: String },
    #[error("The amount must be more than zero")]
    ZeroAmount,
    #[error("This transaction needs {needed} of gas, more than the {limit} a transaction can have")]
    TooMuchGas { needed: String, limit: String },
    #[error("Only {deployer}, which deployed {dex_id}, can withdraw its storage balance")]
    NotDexDeployer { dex_id: DexId, deployer: AccountId },
    #[error(
        "{signer_id} can't deploy dex code: {ENGINE_ACCOUNT_ID} only takes code from its trusted code deployer, {trusted_code_deployer}"
    )]
    NotTrustedCodeDeployer {
        signer_id: AccountId,
        trusted_code_deployer: AccountId,
    },
    #[error(
        "{signer_id} can't hand over the permission to deploy dex code: only the trusted code deployer, {trusted_code_deployer}, can"
    )]
    CantHandOverCodeDeployment {
        signer_id: AccountId,
        trusted_code_deployer: AccountId,
    },
    #[error("{account_id} is already the trusted code deployer of {ENGINE_ACCOUNT_ID}")]
    AlreadyTrustedCodeDeployer { account_id: AccountId },
    #[error(
        "{ENGINE_ACCOUNT_ID} must be paused to rescue assets, so that no withdrawal is in flight while it counts what it holds"
    )]
    NotPausedForRescue,
    #[error(
        "{ENGINE_ACCOUNT_ID} holds {held}, less than the {in_custody} that balances on it add up to, so none of it is untracked"
    )]
    CustodyDeficit { held: String, in_custody: String },
    #[error("{ENGINE_ACCOUNT_ID} has no untracked {asset_label} to rescue")]
    NothingToRescue { asset_label: String },
    #[error("{ENGINE_ACCOUNT_ID} has {untracked} untracked, less than {requested}")]
    RescueTooLarge {
        untracked: String,
        requested: String,
    },
    #[error("{receiver_id} has no storage on {token_id}, so it can't receive the rescued tokens")]
    RescueReceiverNotRegisteredWithToken {
        receiver_id: AccountId,
        token_id: AccountId,
    },
    #[error("The swap method of dexes is only for swaps, which dex call doesn't make")]
    ReservedDexMethod,
    #[error("{dex_id} is already initialized")]
    XykAlreadyInitialized { dex_id: DexId },
    #[error("{dex_id} has no state yet, so there's nothing to migrate")]
    NoStateToMigrate { dex_id: DexId, init_command: String },
    #[error("{signer_id} can't migrate xyk dexes; only {CAN_MIGRATE} can")]
    NotAllowedToMigrate { signer_id: AccountId },
    #[error("These fees aren't allowed: {problem}")]
    FeesNotAllowed { problem: String },
    #[error("A pool needs two different assets, not {asset_id} twice")]
    SameAssets { asset_id: AssetId },
    #[error("{asset_id} can't be in a pool: XYK pools take NEAR, NEP-141 and NEP-245 assets")]
    UnsupportedPoolAsset { asset_id: AssetId },
    #[error("Launch pools pair NEAR with a token, so the token can't be NEAR itself")]
    LaunchOfNear,
    #[error("Pool #{pool_id} of {dex_id} is owned by {owner_id}, and {signer_id} signed")]
    NotPoolOwner {
        dex_id: DexId,
        pool_id: PoolId,
        owner_id: AccountId,
        signer_id: AccountId,
    },
    #[error("Pool #{pool_id} of {dex_id} is locked, so its fees and liquidity can't change")]
    PoolLocked { dex_id: DexId, pool_id: PoolId },
    #[error("Pool #{pool_id} of {dex_id} is a {kind} pool, and {reason}")]
    PoolKindNotSupported {
        dex_id: DexId,
        pool_id: PoolId,
        kind: &'static str,
        reason: &'static str,
    },
    #[error("Pool #{pool_id} of {dex_id} is already of the latest version")]
    PoolIsLatest { dex_id: DexId, pool_id: PoolId },
    #[error("Pool #{pool_id} of {dex_id} is of an old version, which {reason}")]
    PoolNeedsUpgrade {
        dex_id: DexId,
        pool_id: PoolId,
        reason: &'static str,
        upgrade_command: String,
    },
    #[error("Pool #{pool_id} of {dex_id} is already locked")]
    PoolAlreadyLocked { dex_id: DexId, pool_id: PoolId },
    #[error("{owner} has no shares in pool #{pool_id} of {dex_id}")]
    NoShares {
        owner: AccountId,
        dex_id: DexId,
        pool_id: PoolId,
    },
    #[error(
        "{owner} has {available} shares in pool #{pool_id} of {dex_id}, fewer than {requested}"
    )]
    NotEnoughShares {
        owner: AccountId,
        dex_id: DexId,
        pool_id: PoolId,
        available: u128,
        requested: u128,
    },
    #[error("{share} of the position is less than one share")]
    ShareTooSmall { share: String },
    #[error("Give one amount of each asset of the pool, {pair}")]
    AmountsOfOneAsset { pair: String },
    #[error("{owner} has no fees to withdraw in {assets} on {dex_id}")]
    NoPendingFees {
        owner: AccountId,
        assets: String,
        dex_id: DexId,
    },
    #[error("{account_id} has no community fees to withdraw on {dex_id}")]
    NoCommunityFees {
        account_id: AccountId,
        dex_id: DexId,
    },
    #[error("Referral fees must be less than 5%")]
    ReferralFeeTooHigh,
    #[error("{owner} already collects fees in {assets} on {dex_id}")]
    FeeAssetsAlreadyRegistered {
        owner: AccountId,
        assets: String,
        dex_id: DexId,
    },
    #[error(
        "{account_id} has no storage on {token_id}, so it can't receive the tokens in its wallet"
    )]
    NotRegisteredWithToken {
        account_id: AccountId,
        token_id: AccountId,
    },
}

impl PreflightError {
    pub fn into_report(self) -> Report {
        let suggestion = match &self {
            Self::AccountMissing { .. } => Some(
                "Check the account id, or choose a connection to the network it's on".to_string(),
            ),
            Self::DexMissing { .. } => {
                Some("Check the dex id, or choose a connection to the network it's on".to_string())
            }
            Self::NotTrustedCodeDeployer {
                trusted_code_deployer,
                ..
            }
            | Self::CantHandOverCodeDeployment {
                trusted_code_deployer,
                ..
            } => Some(format!("Sign with {trusted_code_deployer}")),
            Self::NotPausedForRescue => Some(format!(
                "Pause it first with `engine pause`, signed by {}",
                CAN_PAUSE.join(" or ")
            )),
            Self::RescueReceiverNotRegisteredWithToken {
                receiver_id,
                token_id,
            } => Some(format!(
                "Rescue to an account that has storage on {token_id}, or pay for {receiver_id}'s storage there with its storage_deposit first"
            )),
            Self::XykAlreadyInitialized { .. } => Some(
                "New code that changes its state layout deploys with `xyk deploy … --migrate`"
                    .to_string(),
            ),
            Self::NoStateToMigrate { init_command, .. } => Some(format!(
                "Deploy without --migrate, then initialize it:\n    {init_command}"
            )),
            Self::NotAllowedToMigrate { .. } => Some(format!("Sign with {CAN_MIGRATE}")),
            Self::NotPoolOwner { owner_id, .. } => Some(format!("Sign with {owner_id}")),
            Self::PoolNeedsUpgrade {
                upgrade_command, ..
            } => Some(format!(
                "Upgrade it first, which anyone can:\n    {upgrade_command}"
            )),
            Self::NoShares { .. } | Self::NotEnoughShares { .. } => {
                Some("`xyk liquidity show` shows the shares of an account".to_string())
            }
            Self::EnginePaused => Some(format!(
                "`engine info` shows whether it's still paused; {} can unpause it",
                CAN_PAUSE.join(" or ")
            )),
            Self::NotAllowedToPause { .. } => Some(format!("Sign with {}", CAN_PAUSE.join(" or "))),
            Self::AssetNotRegisteredForSigner {
                register_command, ..
            } => Some(format!("Register it first:\n    {register_command}")),
            Self::NotRegisteredWithToken { token_id, .. } => Some(format!(
                "Use to-balance instead, or pay for storage on {token_id} with its storage_deposit first"
            )),
            Self::UnsupportedDeposit { .. } => Some(format!(
                "Send it to {ENGINE_ACCOUNT_ID} with nft_transfer_call or mt_transfer_call on its contract"
            )),
            Self::AlreadyPaused
            | Self::NotPaused
            | Self::NoStorageAvailable { .. }
            | Self::NoBalance { .. }
            | Self::TransferToSelf { .. }
            | Self::InsufficientWalletBalance { .. }
            | Self::InsufficientTokenBalance { .. }
            | Self::InsufficientEngineBalance { .. }
            | Self::AlreadyRegistered { .. }
            | Self::NoStorageBalance { .. }
            | Self::StorageWithdrawalTooLarge { .. }
            | Self::StorageDepositTooSmall { .. }
            | Self::ZeroAmount
            | Self::TooMuchGas { .. }
            | Self::NotDexDeployer { .. }
            | Self::AlreadyTrustedCodeDeployer { .. }
            | Self::CustodyDeficit { .. }
            | Self::NothingToRescue { .. }
            | Self::RescueTooLarge { .. }
            | Self::ReservedDexMethod
            | Self::FeesNotAllowed { .. }
            | Self::SameAssets { .. }
            | Self::UnsupportedPoolAsset { .. }
            | Self::LaunchOfNear
            | Self::PoolLocked { .. }
            | Self::PoolKindNotSupported { .. }
            | Self::PoolIsLatest { .. }
            | Self::PoolAlreadyLocked { .. }
            | Self::ShareTooSmall { .. }
            | Self::AmountsOfOneAsset { .. }
            | Self::NoPendingFees { .. }
            | Self::NoCommunityFees { .. }
            | Self::ReferralFeeTooHigh
            | Self::FeeAssetsAlreadyRegistered { .. } => None,
        };
        let report = Report::new(self);
        match suggestion {
            Some(suggestion) => report.suggestion(suggestion),
            None => report,
        }
    }
}

/// What's wrong with fees, in the user's terms: the shared validation counts
/// in millionths
pub fn fee_configuration_problem(error: FeeConfigurationError) -> String {
    match error {
        FeeConfigurationError::InvalidFeeAmount(FeeAmountError::FixedFeeTooHigh)
        | FeeConfigurationError::ReceiverFeeTooHigh => {
            "every fee must be less than 100%".to_string()
        }
        FeeConfigurationError::InvalidFeeAmount(FeeAmountError::ScheduleStartNotBeforeEnd) => {
            "a fee schedule must start before it ends".to_string()
        }
        FeeConfigurationError::InvalidFeeAmount(FeeAmountError::ScheduledFeeNotDecreasing) => {
            "a scheduled fee must decrease, so its start fee must be higher than its end fee"
                .to_string()
        }
        FeeConfigurationError::InvalidFeeAmount(FeeAmountError::ScheduledStartFeeTooHigh) => {
            "a scheduled fee must start below 100%".to_string()
        }
        FeeConfigurationError::InvalidFeeAmount(
            FeeAmountError::DynamicMinNotBelowMax
            | FeeAmountError::DynamicMaxTooHigh
            | FeeAmountError::DynamicFeeNotImplemented,
        ) => "dynamic fees aren't supported".to_string(),
        FeeConfigurationError::TooManyReceivers => {
            format!("a pool can have at most {MAX_FEE_RECEIVERS} fee receivers")
        }
        FeeConfigurationError::TotalFeeTooHigh => {
            "the fees must add up to less than 50%".to_string()
        }
        FeeConfigurationError::ProtocolFeeReceiverSet => format!(
            "{PROTOCOL_FEE_RECEIVER_ID} collects the protocol fee, so it can't be a fee receiver"
        ),
        FeeConfigurationError::CommunityReceiverOutsideLaunchPool => {
            "community: receivers are only for launch pools".to_string()
        }
    }
}

/// Where a contract panicked, for hints that suggest a command or name the dex
pub struct FailureContext<'a> {
    pub panic_message: &'a str,
    /// Known for transactions, which have a signer and a chosen connection
    pub signer_and_connection: Option<(&'a AccountId, &'a str)>,
    pub dex_id: Option<&'a DexId>,
}

/// The engine's panic for a dex id without code
pub const DEX_CODE_NOT_FOUND: &str = "Dex code not found";
/// near-sdk's panic when a contract that needs initialization has no state
pub const NOT_INITIALIZED: &str = "The contract is not initialized";
/// near-sdk's panic when a contract's code can't read its state
pub const UNREADABLE_STATE: &str = "Cannot deserialize the contract state.";
/// How the engine names the migrate method in the panics of a dex call to it
const MIGRATE_METHOD_FRAGMENT: &str = "function 'migrate'";

struct ContractPanicHint {
    /// Part of the panic message as the contract sources spell it
    message_fragment: &'static str,
    hint: fn(&FailureContext) -> String,
}

const CONTRACT_PANIC_HINTS: &[ContractPanicHint] = &[
    ContractPanicHint {
        message_fragment: "Contract is paused",
        hint: |_| {
            format!(
                "{ENGINE_ACCOUNT_ID} was paused after the checks before signing. Nothing changes until {} unpauses it",
                CAN_PAUSE.join(" or ")
            )
        },
    },
    ContractPanicHint {
        message_fragment: "Only authorized accounts can pause the contract",
        hint: |_| {
            format!(
                "Only {} can pause or unpause {ENGINE_ACCOUNT_ID}",
                CAN_PAUSE.join(" and ")
            )
        },
    },
    ContractPanicHint {
        message_fragment: " is not registered for ",
        hint: |context| {
            // "Asset {asset_id} is not registered for Account({account_id})" or "… for Dex({dex_id})"
            let unregistered = context
                .panic_message
                .split_once("Asset ")
                .and_then(|(_, rest)| rest.split_once(" is not registered for "))
                .map(|(asset_id, owner)| {
                    let owner = owner
                        .trim_start_matches("Account(")
                        .trim_start_matches("Dex(")
                        .trim_end_matches(')');
                    (asset_id.to_string(), owner.to_string())
                });
            match (unregistered, context.signer_and_connection) {
                (Some((asset_id, owner)), Some((signer_id, connection_name))) => {
                    let mut command = vec![
                        "near".to_string(),
                        "intear-dex-management".to_string(),
                        "engine".to_string(),
                        "assets".to_string(),
                        "register".to_string(),
                        signer_id.to_string(),
                        asset_id,
                    ];
                    if owner != signer_id.as_str() {
                        command.extend(["--for".to_string(), owner]);
                    }
                    command.extend(["network-config".to_string(), connection_name.to_string()]);
                    format!(
                        "Register the asset first:\n    {}",
                        shell_words::join(command)
                    )
                }
                _ => "Register the asset first with `engine assets register`".to_string(),
            }
        },
    },
    ContractPanicHint {
        message_fragment: ") exceeds total (",
        hint: |context| match (context.dex_id, context.signer_and_connection) {
            // Calls of a dex charge its storage balance as well as the signer's
            (Some(dex_id), Some((signer_id, _))) => format!(
                "The storage balance of {signer_id} or of {dex_id} on {ENGINE_ACCOUNT_ID} is too small; `engine storage show` and `dex storage show` show them"
            ),
            (None, Some((signer_id, connection_name))) => format!(
                "The storage balance on {ENGINE_ACCOUNT_ID} is too small. Deposit more:\n    near intear-dex-management engine storage deposit {signer_id} '0.01 NEAR' network-config {connection_name}"
            ),
            (_, None) => format!(
                "The storage balance on {ENGINE_ACCOUNT_ID} is too small; `engine storage deposit` adds to it"
            ),
        },
    },
    ContractPanicHint {
        message_fragment: "Insufficient balance",
        hint: |context| match context.signer_and_connection {
            Some((signer_id, connection_name)) => format!(
                "Check the balances on {ENGINE_ACCOUNT_ID}:\n    near intear-dex-management engine balance {signer_id} all network-config {connection_name} now"
            ),
            None => format!("`engine balance` shows the balances on {ENGINE_ACCOUNT_ID}"),
        },
    },
    ContractPanicHint {
        message_fragment: DEX_CODE_NOT_FOUND,
        hint: |context| match context.dex_id {
            Some(dex_id) => {
                format!("There's no dex {dex_id} on {ENGINE_ACCOUNT_ID}. Check --dex")
            }
            None => format!("There's no dex with this id on {ENGINE_ACCOUNT_ID}"),
        },
    },
    ContractPanicHint {
        message_fragment: "Failed to get function",
        hint: |context| {
            if context.panic_message.contains(MIGRATE_METHOD_FRAGMENT) {
                return "The new code has no migrate method, so the deploy was reverted with the migration; deploy it without --migrate".to_string();
            }
            let dex = context
                .dex_id
                .map_or("The dex".to_string(), |dex_id| dex_id.to_string());
            format!(
                "{dex} has no such method: it's a different kind of dex, or its code is older than this tool"
            )
        },
    },
    ContractPanicHint {
        message_fragment: "Only the trusted code deployer can deploy dex code",
        hint: |_| {
            format!(
                "`engine info` shows the trusted code deployer of {ENGINE_ACCOUNT_ID}, the only account that can deploy dex code"
            )
        },
    },
    ContractPanicHint {
        message_fragment: "Only the trusted code deployer can transfer this permission",
        hint: |_| {
            format!(
                "`engine info` shows the trusted code deployer of {ENGINE_ACCOUNT_ID}, the only account that can hand over the permission"
            )
        },
    },
    ContractPanicHint {
        message_fragment: "Contract must be paused to rescue assets",
        hint: |_| {
            format!(
                "{ENGINE_ACCOUNT_ID} was unpaused after the checks before signing; pause it again with `engine pause`, signed by {}",
                CAN_PAUSE.join(" or ")
            )
        },
    },
    ContractPanicHint {
        message_fragment: "Only the deployer can withdraw dex storage",
        hint: |context| match context.dex_id {
            Some(dex_id) => format!(
                "Only {}, which deployed {dex_id}, can withdraw its storage balance",
                dex_id.deployer
            ),
            None => {
                "Only the account that deployed a dex can withdraw its storage balance".to_string()
            }
        },
    },
    ContractPanicHint {
        message_fragment: "Method name 'swap' is reserved",
        hint: |_| "Dexes take swaps only through the engine's swap operations".to_string(),
    },
    ContractPanicHint {
        message_fragment: "Slippage error",
        hint: |_| {
            "The pool's reserves moved beyond your max slippage; check the pool with `xyk pool show`, then rerun or allow more slippage".to_string()
        },
    },
    ContractPanicHint {
        message_fragment: "Only pool owner can",
        hint: |_| "Only the pool's owner can do this; `xyk pool show` shows it".to_string(),
    },
    ContractPanicHint {
        message_fragment: "Pool is locked",
        hint: |_| "A locked pool keeps its fees and liquidity for good".to_string(),
    },
    ContractPanicHint {
        message_fragment: "Not enough near attached for storage",
        hint: |_| {
            "This tool attached less NEAR for the dex's storage than it needed, which is a bug in its estimate; please report it with the transaction".to_string()
        },
    },
    ContractPanicHint {
        message_fragment: "Pool not found",
        hint: |_| "`xyk pools count` shows how many pools there are".to_string(),
    },
    ContractPanicHint {
        message_fragment: "User has not registered using register_liquidity",
        hint: |_| {
            "Public pools need a registration before liquidity; `xyk liquidity add` adds it"
                .to_string()
        },
    },
    ContractPanicHint {
        message_fragment: "Not enough shares to remove",
        hint: |_| "`xyk liquidity show` shows the shares of an account".to_string(),
    },
    ContractPanicHint {
        message_fragment: "Account does not have a registered community fee",
        hint: |_| "Community fees come from launch pools that name the account".to_string(),
    },
    ContractPanicHint {
        message_fragment: "is less than the minimum bound",
        hint: |_| format!("Storage deposits on {ENGINE_ACCOUNT_ID} must be at least 0.005 NEAR"),
    },
    ContractPanicHint {
        message_fragment: "Amount exceeds storage used",
        hint: |_| {
            "That's more than the available storage balance; `engine storage show` shows it"
                .to_string()
        },
    },
];

/// Messages of the token standards' reference implementations, which most
/// token contracts use
const TOKEN_STANDARD_PANIC_HINTS: &[ContractPanicHint] = &[ContractPanicHint {
    message_fragment: " is not registered",
    hint: |_| {
        "The receiver has no storage on the token contract, so it can't receive the tokens. It needs a storage_deposit there first".to_string()
    },
}];

/// Messages of near-sdk itself, which contracts don't spell out
const NEAR_SDK_PANIC_HINTS: &[ContractPanicHint] = &[
    ContractPanicHint {
        message_fragment: NOT_INITIALIZED,
        hint: |_| "The dex has code but no state yet; `xyk init` creates it".to_string(),
    },
    ContractPanicHint {
        message_fragment: UNREADABLE_STATE,
        hint: |context| {
            if context.panic_message.contains(MIGRATE_METHOD_FRAGMENT) {
                "The migration couldn't read the state in the layout it converts from, so the deploy was reverted with it; if the state already has the new code's layout, deploy without --migrate".to_string()
            } else {
                "The dex's code can't read its state; deploying code that converts it with `xyk deploy … --migrate` fixes that".to_string()
            }
        },
    },
];

pub fn contract_panic_hint(context: &FailureContext) -> Option<String> {
    CONTRACT_PANIC_HINTS
        .iter()
        .chain(TOKEN_STANDARD_PANIC_HINTS)
        .chain(NEAR_SDK_PANIC_HINTS)
        .find(|hint| context.panic_message.contains(hint.message_fragment))
        .map(|hint| (hint.hint)(context))
}

/// Rust panics start with their location, which changes with every build, so
/// messages are shown and matched without it. A dex's panic comes inside the
/// engine's, with a location of its own after "Dex panicked: ".
fn without_panic_location(message: &str) -> String {
    let mut message = message.to_string();
    while let Some((before_location, after_location)) =
        message
            .split_once("panicked at ")
            .and_then(|(before_location, rest)| {
                rest.split_once(":\n")
                    .map(|(_location, after_location)| (before_location, after_location))
            })
    {
        message = format!("{before_location}{after_location}");
    }
    message
}

/// The panic message of an execution error from a transaction outcome
pub fn panic_message_of_execution_error(execution_error: &str) -> String {
    without_panic_location(
        execution_error
            .strip_prefix("Smart contract panicked: ")
            .unwrap_or(execution_error),
    )
}

/// The panic message inside an error text that quotes it as an escaped Rust
/// string, like near-cli-rs's transaction errors and RPC view errors do
pub fn panic_message_in_error_text(error_text: &str) -> Option<String> {
    let (_, escaped_message) = error_text
        .split_once("Smart contract panicked: ")
        .or_else(|| error_text.split_once("panic_msg: \""))?;
    let mut message = String::new();
    let mut characters = escaped_message.chars();
    while let Some(character) = characters.next() {
        match character {
            '"' => break,
            '\\' => match characters.next() {
                Some('n') => message.push('\n'),
                Some('t') => message.push('\t'),
                Some(escaped_character) => message.push(escaped_character),
                None => break,
            },
            character => message.push(character),
        }
    }
    Some(without_panic_location(&message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_fragment_is_still_in_the_contract_sources() {
        let contract_sources = [
            include_str!("../../src/lib.rs"),
            include_str!("../../src/internal_operations.rs"),
            include_str!("../../src/internal_asset_operations.rs"),
            include_str!("../../src/storage_management.rs"),
            include_str!("../../src/rescue.rs"),
            include_str!("../../dexes/xyk/src/lib.rs"),
            include_str!("../../dexes/xyk/types/src/lib.rs"),
        ]
        .concat();
        for hint in CONTRACT_PANIC_HINTS {
            assert!(
                contract_sources.contains(hint.message_fragment),
                "The contracts no longer panic with \"{}\"; update its hint",
                hint.message_fragment
            );
        }
    }

    #[test]
    fn panic_messages_lose_prefix_location_and_escapes() {
        assert_eq!(
            panic_message_in_error_text(
                "Error: An error occurred during a `FunctionCall` action.\nExecutionError(\"Smart contract panicked: Contract is paused\")"
            )
            .as_deref(),
            Some("Contract is paused")
        );
        assert_eq!(
            panic_message_in_error_text(
                "wasm execution failed with error: HostError(GuestPanic { panic_msg: \"panicked at src/internal_operations.rs:463:48:\\nDex code not found\" })"
            )
            .as_deref(),
            Some("Dex code not found")
        );
        assert_eq!(
            panic_message_of_execution_error(
                "Smart contract panicked: panicked at src/lib.rs:12:5:\nAsset near is not registered for Account(bob.near)"
            ),
            "Asset near is not registered for Account(bob.near)"
        );
        assert_eq!(
            panic_message_of_execution_error(
                "Smart contract panicked: panicked at src/internal_operations.rs:113:23:\nFailed to call function 'add_liquidity': [slimedragon.near/xyk] Dex panicked: panicked at src/lib.rs:896:27:\nPool not found"
            ),
            "Failed to call function 'add_liquidity': [slimedragon.near/xyk] Dex panicked: Pool not found"
        );
    }

    #[test]
    fn unregistered_asset_hint_is_a_runnable_command() {
        let signer_id: AccountId = "alice.near".parse().unwrap();
        let hint = contract_panic_hint(&FailureContext {
            panic_message: "Asset nep141:usdt.tether-token.near is not registered for Account(bob.near)",
            signer_and_connection: Some((&signer_id, "mainnet")),
            dex_id: None,
        });
        assert_eq!(
            hint.as_deref(),
            Some(
                "Register the asset first:\n    near intear-dex-management engine assets register alice.near nep141:usdt.tether-token.near --for bob.near network-config mainnet"
            )
        );
    }
}
