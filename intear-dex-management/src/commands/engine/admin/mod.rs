use strum::{EnumDiscriminants, EnumIter, EnumMessage};

pub mod custody;
pub mod migrate;
pub mod rescue;
pub mod trusted_code_deployer;

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
pub struct AdminCommands {
    #[interactive_clap(subcommand)]
    action: AdminAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do?
pub enum AdminAction {
    #[strum_discriminants(strum(
        message = "set-trusted-code-deployer  - Hand the permission to deploy dex code to another account"
    ))]
    /// Hand the permission to deploy dex code to another account
    SetTrustedCodeDeployer(self::trusted_code_deployer::SetTrustedCodeDeployer),
    #[strum_discriminants(strum(
        message = "custody                    - What the engine holds of an asset, against what balances on it add up to"
    ))]
    /// What the engine holds of an asset, against what balances on it add up to
    Custody(self::custody::Custody),
    #[strum_discriminants(strum(
        message = "rescue                     - Send out what the engine holds of an asset that no balance accounts for"
    ))]
    /// Send out what the engine holds of an asset that no balance accounts for
    Rescue(self::rescue::Rescue),
    #[strum_discriminants(strum(
        message = "migrate                    - Pause, migrate the engine to code that keeps balances by owner, unpause"
    ))]
    /// Pause, migrate the engine to code that keeps balances by owner, unpause
    Migrate(self::migrate::Migrate),
}
