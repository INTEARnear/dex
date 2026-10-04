use intear_dex_types::AssetId;
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use serde_json::json;

use crate::chain::asset_metadata::AssetMetadata;
use crate::chain::engine;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = InfoContext)]
pub struct Info {
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

#[derive(Clone)]
pub struct InfoContext(ArgsForViewContext);

impl InfoContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        _scope: &<Info as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let output_format = previous_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let paused = engine::is_paused(network_config, &block_reference)?;
                let trusted_code_deployer =
                    engine::trusted_code_deployer(network_config, &block_reference)?;
                let storage = engine::total_storage_balances(network_config, &block_reference)?;
                let near_in_custody =
                    engine::total_in_custody(network_config, &block_reference, &AssetId::Near)?
                        .map_or(0, |amount| amount.0);
                let untracked_near = engine::untracked_near(network_config, &block_reference)?;

                let near = AssetMetadata::near();
                match output_format {
                    OutputFormat::Table => {
                        let near_amount =
                            |yocto_near: u128| display::format_amount(yocto_near, Some(&near));
                        let table = display::key_value_table(vec![
                            ("Paused", if paused { "yes" } else { "no" }.to_string()),
                            ("Trusted code deployer", trusted_code_deployer.to_string()),
                            (
                                "User storage",
                                format!(
                                    "{} total, {} available",
                                    near_amount(storage.users.total.as_yoctonear()),
                                    near_amount(storage.users.available.as_yoctonear())
                                ),
                            ),
                            (
                                "Dex storage",
                                format!(
                                    "{} total, {} available",
                                    near_amount(storage.dexes.total.as_yoctonear()),
                                    near_amount(storage.dexes.available.as_yoctonear())
                                ),
                            ),
                            ("NEAR in custody", near_amount(near_in_custody)),
                            ("Untracked NEAR", near_amount(untracked_near.as_yoctonear())),
                        ]);
                        print!("{table}");
                    }
                    OutputFormat::Json => {
                        let near_amount =
                            |yocto_near: u128| display::amount_json(yocto_near, Some(&near));
                        display::print_json(&json!({
                            "paused": paused,
                            "trusted_code_deployer": trusted_code_deployer,
                            "storage": {
                                "users": {
                                    "total": near_amount(storage.users.total.as_yoctonear()),
                                    "available": near_amount(storage.users.available.as_yoctonear()),
                                },
                                "dexes": {
                                    "total": near_amount(storage.dexes.total.as_yoctonear()),
                                    "available": near_amount(storage.dexes.available.as_yoctonear()),
                                },
                            },
                            "near_in_custody": near_amount(near_in_custody),
                            "untracked_near": near_amount(untracked_near.as_yoctonear()),
                        }))?;
                    }
                }
                Ok(())
            });
        Ok(Self(ArgsForViewContext {
            config: previous_context.config,
            interacting_with_account_ids: vec![ENGINE_ACCOUNT_ID.to_owned()],
            on_after_getting_block_reference_callback,
        }))
    }
}

impl From<InfoContext> for ArgsForViewContext {
    fn from(item: InfoContext) -> Self {
        item.0
    }
}
