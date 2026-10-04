use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use serde_json::json;

use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::{custody, engine};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::inputs::asset_ids::AssetIdArg;

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = CustodyContext)]
pub struct Custody {
    #[interactive_clap(skip_default_input_arg)]
    /// The asset: near, nep141:<token contract>, …
    asset: AssetIdArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl Custody {
    fn input_asset(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AssetIdArg>> {
        crate::inputs::prompt("Which asset? e.g. near or nep141:usdt.tether-token.near")
    }
}

#[derive(Clone)]
pub struct CustodyContext(ArgsForViewContext);

impl CustodyContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Custody as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let output_format = previous_context.output_format;
        let AssetIdArg(asset_id) = scope.asset.clone();
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let metadata = AssetMetadataCache::new(network_config, block_reference.clone())
                    .get(&asset_id)?;
                let asset_custody =
                    custody::custody_of(network_config, &block_reference, &asset_id)?;
                match output_format {
                    OutputFormat::Table => {
                        let amount = |raw_amount: u128| {
                            display::format_amount(raw_amount, metadata.as_ref())
                        };
                        let held_key = format!("Held by {ENGINE_ACCOUNT_ID}");
                        let mut rows = vec![("In custody", amount(asset_custody.in_custody))];
                        match asset_custody.held {
                            Some(held) => {
                                rows.push((held_key.as_str(), amount(held)));
                                rows.push(("Untracked", amount(asset_custody.untracked)));
                            }
                            None => rows.push((
                                "Untracked (beyond custody and storage)",
                                amount(asset_custody.untracked),
                            )),
                        }
                        if asset_custody.deficit > 0 {
                            rows.push(("Deficit", amount(asset_custody.deficit)));
                        }
                        print!("{}", display::key_value_table(rows));
                    }
                    OutputFormat::Json => {
                        let amount =
                            |raw_amount: u128| display::amount_json(raw_amount, metadata.as_ref());
                        display::print_json(&json!({
                            "asset_id": asset_id,
                            "in_custody": amount(asset_custody.in_custody),
                            "held": asset_custody.held.map(amount),
                            "untracked": amount(asset_custody.untracked),
                            "deficit": amount(asset_custody.deficit),
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

impl From<CustodyContext> for ArgsForViewContext {
    fn from(item: CustodyContext) -> Self {
        item.0
    }
}
