use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use super::XykContext;
use super::presentation::{pool_assets, pool_fees, pool_json, pool_kind, total_fee_fraction};
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::{engine, xyk};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::inputs::asset_ids::AssetIdArg;

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
pub struct PoolsCommands {
    #[interactive_clap(subcommand)]
    action: PoolsAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to see?
pub enum PoolsAction {
    #[strum_discriminants(strum(message = "list   - Every pool with its pair, reserves and fees"))]
    /// Every pool with its pair, reserves and fees
    List(List),
    #[strum_discriminants(strum(message = "count  - The number of pools"))]
    /// The number of pools
    Count(Count),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = ListContext)]
pub struct List {
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// Only pools with this asset
    asset: Option<AssetIdArg>,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

#[derive(Clone)]
pub struct ListContext(ArgsForViewContext);

impl ListContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<List as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let asset_filter = scope.asset.clone().map(|AssetIdArg(asset_id)| asset_id);
        let output_format = previous_context.global_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let pools = xyk::all_pools(network_config, &block_reference, &dex_id)?
                    .into_iter()
                    .filter(|(_, pool)| {
                        asset_filter.as_ref().is_none_or(|asset_filter| {
                            pool_assets(pool)
                                .iter()
                                .any(|(asset_id, _)| asset_id == asset_filter)
                        })
                    })
                    .collect::<Vec<_>>();
                let metadata_cache = AssetMetadataCache::new(network_config, block_reference);
                match output_format {
                    OutputFormat::Table => {
                        if pools.is_empty() {
                            println!("{dex_id} has no pools that match");
                            return Ok(());
                        }
                        let mut table =
                            display::table(&["Pool", "Kind", "Pair", "Reserves", "Fee"]);
                        for (pool_id, pool) in &pools {
                            let [(asset_id_0, reserve_0), (asset_id_1, reserve_1)] =
                                pool_assets(pool);
                            let metadata_0 = metadata_cache.get(&asset_id_0)?;
                            let metadata_1 = metadata_cache.get(&asset_id_1)?;
                            table.add_row(prettytable::row![
                                format!("#{pool_id}"),
                                pool_kind(pool),
                                format!(
                                    "{} / {}",
                                    display::asset_label(&asset_id_0, metadata_0.as_ref()),
                                    display::asset_label(&asset_id_1, metadata_1.as_ref())
                                ),
                                format!(
                                    "{} / {}",
                                    display::format_amount(reserve_0, metadata_0.as_ref()),
                                    display::format_amount(reserve_1, metadata_1.as_ref())
                                ),
                                display::format_fee(total_fee_fraction(pool_fees(pool).0)?),
                            ]);
                        }
                        print!("{table}");
                    }
                    OutputFormat::Json => {
                        let mut pools_json = Vec::new();
                        for (pool_id, pool) in &pools {
                            pools_json.push(pool_json(*pool_id, pool, &metadata_cache)?);
                        }
                        display::print_json(&json!({
                            "dex_id": dex_id,
                            "pools": pools_json,
                        }))?;
                    }
                }
                Ok(())
            });
        Ok(Self(ArgsForViewContext {
            config: previous_context.global_context.config,
            interacting_with_account_ids: vec![ENGINE_ACCOUNT_ID.to_owned()],
            on_after_getting_block_reference_callback,
        }))
    }
}

impl From<ListContext> for ArgsForViewContext {
    fn from(item: ListContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = CountContext)]
pub struct Count {
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

#[derive(Clone)]
pub struct CountContext(ArgsForViewContext);

impl CountContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        _scope: &<Count as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let output_format = previous_context.global_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let pool_count = xyk::pool_count(network_config, &block_reference, &dex_id)?;
                match output_format {
                    OutputFormat::Table => println!("{pool_count}"),
                    OutputFormat::Json => display::print_json(&json!({
                        "dex_id": dex_id,
                        "pool_count": pool_count,
                    }))?,
                }
                Ok(())
            });
        Ok(Self(ArgsForViewContext {
            config: previous_context.global_context.config,
            interacting_with_account_ids: vec![ENGINE_ACCOUNT_ID.to_owned()],
            on_after_getting_block_reference_callback,
        }))
    }
}

impl From<CountContext> for ArgsForViewContext {
    fn from(item: CountContext) -> Self {
        item.0
    }
}
