use strum::{EnumDiscriminants, EnumIter, EnumMessage};

pub mod admin;
pub mod assets;
pub mod balance;
pub mod deposit;
pub mod info;
pub mod operations;
pub mod pause;
pub mod storage;
pub mod transfer;
pub mod withdraw;

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
pub struct EngineCommands {
    #[interactive_clap(subcommand)]
    action: EngineAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do?
pub enum EngineAction {
    #[strum_discriminants(strum(
        message = "info      - Paused state, trusted code deployer, storage and NEAR totals"
    ))]
    /// Paused state, trusted code deployer, storage and NEAR totals
    Info(self::info::Info),
    #[strum_discriminants(strum(
        message = "balance   - Balances of an account or a dex on the engine"
    ))]
    /// Balances of an account or a dex on the engine
    Balance(self::balance::Balance),
    #[strum_discriminants(strum(message = "storage   - Storage balances of accounts"))]
    /// Storage balances of accounts
    Storage(self::storage::StorageCommands),
    #[strum_discriminants(strum(message = "assets    - Asset registrations"))]
    /// Asset registrations
    Assets(self::assets::AssetsCommands),
    #[strum_discriminants(strum(
        message = "deposit   - Move NEAR or tokens from a wallet into its balance on the engine"
    ))]
    /// Move NEAR or tokens from a wallet into its balance on the engine
    Deposit(self::deposit::Deposit),
    #[strum_discriminants(strum(
        message = "withdraw  - Move NEAR or tokens from a balance on the engine to a wallet"
    ))]
    /// Move NEAR or tokens from a balance on the engine to a wallet
    Withdraw(self::withdraw::Withdraw),
    #[strum_discriminants(strum(
        message = "transfer  - Move NEAR or tokens to another balance on the engine"
    ))]
    /// Move NEAR or tokens to another balance on the engine
    Transfer(self::transfer::Transfer),
    #[strum_discriminants(strum(
        message = "operations - Run a batch of operations from a JSON file in one transaction"
    ))]
    /// Run a batch of operations from a JSON file in one transaction
    Operations(self::operations::OperationsCommands),
    #[strum_discriminants(strum(
        message = "pause     - Stop deposits, withdrawals, swaps and dex calls"
    ))]
    /// Stop deposits, withdrawals, swaps and dex calls
    Pause(self::pause::Pause),
    #[strum_discriminants(strum(message = "unpause   - Let the engine run again"))]
    /// Let the engine run again
    Unpause(self::pause::Unpause),
    #[strum_discriminants(strum(
        message = "admin     - Trusted code deployer, custody and rescue of untracked assets"
    ))]
    /// Trusted code deployer, custody and rescue of untracked assets
    Admin(self::admin::AdminCommands),
}
