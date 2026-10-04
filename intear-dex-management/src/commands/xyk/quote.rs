use intear_dex_types::{SwapRequest, SwapRequestAmount};
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_sdk::json_types::{Base64VecU8, U128};
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use xyk_dex_types::{PoolId, SwapArgs};

use super::XykContext;
use super::presentation::{pool_assets, pool_fees, total_fee_fraction};
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::{engine, xyk};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::inputs::amount::AmountArg;
use crate::inputs::pool_id::PoolIdArg;

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = QuoteContext)]
pub struct Quote {
    #[interactive_clap(skip_default_input_arg)]
    /// The pool id
    pool_id: PoolIdArg,
    #[interactive_clap(subcommand)]
    direction: QuoteDirection,
}

impl Quote {
    fn input_pool_id(context: &XykContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
        super::input_pool_id(context)
    }
}

#[derive(Clone)]
pub struct QuoteContext {
    xyk_context: XykContext,
    pool_id: PoolId,
}

impl QuoteContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Quote as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self {
            xyk_context: previous_context,
            pool_id: scope.pool_id.0,
        })
    }
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = QuoteContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// Sell an exact amount or buy an exact amount?
pub enum QuoteDirection {
    #[strum_discriminants(strum(
        message = "sell  - Sell an exact amount and see what you would receive"
    ))]
    /// Sell an exact amount and see what you would receive
    Sell(Sell),
    #[strum_discriminants(strum(
        message = "buy   - Buy an exact amount and see what you would pay"
    ))]
    /// Buy an exact amount and see what you would pay
    Buy(Buy),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = QuoteContext)]
#[interactive_clap(output_context = SellContext)]
pub struct Sell {
    #[interactive_clap(skip_default_input_arg)]
    /// The amount to sell, e.g. '10 NEAR'
    amount: AmountArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl Sell {
    fn input_amount(_context: &QuoteContext) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt(
            "How much to sell? e.g. '10 NEAR', '25.5 USDT' or '1000000 raw nep141:usdt.tether-token.near'",
        )
    }
}

#[derive(Clone)]
pub struct SellContext(ArgsForViewContext);

impl SellContext {
    pub fn from_previous_context(
        previous_context: QuoteContext,
        scope: &<Sell as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self(quote_view_context(
            previous_context,
            scope.amount.clone(),
            SwapDirection::Sell,
        )))
    }
}

impl From<SellContext> for ArgsForViewContext {
    fn from(item: SellContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = QuoteContext)]
#[interactive_clap(output_context = BuyContext)]
pub struct Buy {
    #[interactive_clap(skip_default_input_arg)]
    /// The amount to buy, e.g. '30 USDT'
    amount: AmountArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl Buy {
    fn input_amount(_context: &QuoteContext) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt(
            "How much to buy? e.g. '30 USDT', '1.5 NEAR' or '1000000 raw nep141:usdt.tether-token.near'",
        )
    }
}

#[derive(Clone)]
pub struct BuyContext(ArgsForViewContext);

impl BuyContext {
    pub fn from_previous_context(
        previous_context: QuoteContext,
        scope: &<Buy as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self(quote_view_context(
            previous_context,
            scope.amount.clone(),
            SwapDirection::Buy,
        )))
    }
}

impl From<BuyContext> for ArgsForViewContext {
    fn from(item: BuyContext) -> Self {
        item.0
    }
}

#[derive(Clone, Copy)]
enum SwapDirection {
    Sell,
    Buy,
}

fn quote_view_context(
    quote_context: QuoteContext,
    amount: AmountArg,
    direction: SwapDirection,
) -> ArgsForViewContext {
    let QuoteContext {
        xyk_context,
        pool_id,
    } = quote_context;
    let dex_id = xyk_context.dex_id;
    let output_format = xyk_context.global_context.output_format;
    let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
        std::sync::Arc::new(move |network_config, block_reference| {
            let block_reference = engine::engine_view_block(network_config, block_reference)?;
            let pool = xyk::pool(network_config, &block_reference, &dex_id, pool_id)?;
            let metadata_cache = AssetMetadataCache::new(network_config, block_reference.clone());
            let [(asset_id_0, _), (asset_id_1, _)] = pool_assets(&pool);
            let candidates = [
                (asset_id_0.clone(), metadata_cache.get(&asset_id_0)?),
                (asset_id_1.clone(), metadata_cache.get(&asset_id_1)?),
            ];
            let (amount_asset_id, raw_amount) = amount.resolve(&candidates)?;
            let other_asset_id = if amount_asset_id == asset_id_0 {
                asset_id_1
            } else {
                asset_id_0
            };
            let (asset_in, asset_out, request_amount) = match direction {
                SwapDirection::Sell => (
                    amount_asset_id,
                    other_asset_id,
                    SwapRequestAmount::ExactIn(U128(raw_amount)),
                ),
                SwapDirection::Buy => (
                    other_asset_id,
                    amount_asset_id,
                    SwapRequestAmount::ExactOut(U128(raw_amount)),
                ),
            };
            let (U128(amount_in), U128(amount_out)) = engine::simulate_swap_simple(
                network_config,
                &block_reference,
                &dex_id,
                &SwapRequest {
                    message: Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs { pool_id })?),
                    asset_in: asset_in.clone(),
                    asset_out: asset_out.clone(),
                    amount: request_amount,
                    referrer: None,
                },
                // xyk prices a swap the same for every trader
                &ENGINE_ACCOUNT_ID.to_owned(),
            )?;
            let total_fee = total_fee_fraction(pool_fees(&pool).0)?;
            let metadata_in = metadata_cache.get(&asset_in)?;
            let metadata_out = metadata_cache.get(&asset_out)?;
            match output_format {
                OutputFormat::Table => {
                    print!(
                        "{}",
                        display::key_value_table(vec![
                            (
                                "Pool",
                                format!(
                                    "#{pool_id} ({} / {}) on {dex_id}",
                                    display::asset_label(&asset_in, metadata_in.as_ref()),
                                    display::asset_label(&asset_out, metadata_out.as_ref())
                                ),
                            ),
                            (
                                "You pay",
                                display::format_amount(amount_in, metadata_in.as_ref())
                            ),
                            (
                                "You receive",
                                display::format_amount(amount_out, metadata_out.as_ref())
                            ),
                            (
                                "Fees",
                                format!(
                                    "{} of the input, included",
                                    display::format_fee(total_fee)
                                )
                            ),
                        ])
                    );
                }
                OutputFormat::Json => display::print_json(&json!({
                    "dex_id": dex_id,
                    "pool_id": pool_id,
                    "direction": match direction {
                        SwapDirection::Sell => "sell",
                        SwapDirection::Buy => "buy",
                    },
                    "amount_in": {
                        "asset_id": asset_in,
                        "amount": display::amount_json(amount_in, metadata_in.as_ref()),
                    },
                    "amount_out": {
                        "asset_id": asset_out,
                        "amount": display::amount_json(amount_out, metadata_out.as_ref()),
                    },
                    "fee": display::fee_json(total_fee),
                }))?,
            }
            Ok(())
        });
    ArgsForViewContext {
        config: xyk_context.global_context.config,
        interacting_with_account_ids: vec![ENGINE_ACCOUNT_ID.to_owned()],
        on_after_getting_block_reference_callback,
    }
}
