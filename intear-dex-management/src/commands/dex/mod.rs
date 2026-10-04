use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use crate::inputs::dex_id::DexIdArg;

pub mod call;
pub mod storage;
pub mod view;

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
pub struct DexCommands {
    #[interactive_clap(subcommand)]
    action: DexAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do with the dex?
pub enum DexAction {
    #[strum_discriminants(strum(
        message = "storage  - The storage balance of a dex, which pays for its code and state"
    ))]
    /// The storage balance of a dex, which pays for its code and state
    Storage(self::storage::StorageCommands),
    #[strum_discriminants(strum(
        message = "view     - Call a view method of a dex, with arguments as the dex encodes them"
    ))]
    /// Call a view method of a dex, with arguments as the dex encodes them
    View(self::view::View),
    #[strum_discriminants(strum(
        message = "call     - Call a method of a dex as the signer, with assets from its balance"
    ))]
    /// Call a method of a dex as the signer, with assets from its balance
    Call(self::call::Call),
}

pub fn input_dex_id() -> color_eyre::eyre::Result<Option<DexIdArg>> {
    crate::inputs::prompt("Which dex? <deployer>/<name>, e.g. slimedragon.near/xyk")
}
