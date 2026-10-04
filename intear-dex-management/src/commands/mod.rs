// Commands are built once per run, and interactive-clap can't box variants;
// near-cli-rs allows the same for its commands
#![allow(clippy::large_enum_variant)]

use strum::{EnumDiscriminants, EnumIter, EnumMessage};

pub mod dex;
pub mod engine;
pub mod xyk;

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// Which contract?
pub enum TopLevelCommand {
    #[strum_discriminants(strum(
        message = "engine  - dex.intear.near, the engine that hosts every dex"
    ))]
    /// dex.intear.near, the engine that hosts every dex
    Engine(self::engine::EngineCommands),
    #[strum_discriminants(strum(
        message = "dex     - Any dex on the engine: its storage balance, views and calls"
    ))]
    /// Any dex on the engine: its storage balance, views and calls
    Dex(self::dex::DexCommands),
    #[strum_discriminants(strum(
        message = "xyk     - The XYK dex (slimedragon.near/xyk unless --dex says otherwise)"
    ))]
    /// The XYK dex (slimedragon.near/xyk unless --dex says otherwise)
    Xyk(self::xyk::XykCommands),
}
