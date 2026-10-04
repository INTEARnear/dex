use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use serde_json::json;

use crate::chain::engine;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::inputs::dex_id::DexIdArg;

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = ViewContext)]
pub struct View {
    #[interactive_clap(skip_default_input_arg)]
    /// The dex as <deployer>/<name>
    dex: DexIdArg,
    /// The view method
    method: String,
    /// The arguments in base64, encoded as the dex reads them (empty for none)
    args: near_cli_rs::types::base64_bytes::Base64Bytes,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl View {
    fn input_dex(_context: &crate::GlobalContext) -> color_eyre::eyre::Result<Option<DexIdArg>> {
        super::input_dex_id()
    }
}

#[derive(Clone)]
pub struct ViewContext(ArgsForViewContext);

impl ViewContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<View as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let DexIdArg(dex_id) = scope.dex.clone();
        let method = scope.method.clone();
        let args = scope.args.clone().into_bytes();
        let output_format = previous_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let result = engine::dex_view_bytes(
                    network_config,
                    &block_reference,
                    &dex_id,
                    &method,
                    args.clone(),
                )?;
                let result_base64 = near_primitives::serialize::to_base64(&result);
                match output_format {
                    OutputFormat::Table => print!(
                        "{}",
                        display::key_value_table(vec![
                            ("Dex", dex_id.to_string()),
                            ("Method", method.clone()),
                            ("Result (base64)", result_base64),
                            ("Result size", format!("{} bytes", result.len())),
                        ])
                    ),
                    OutputFormat::Json => display::print_json(&json!({
                        "dex_id": dex_id,
                        "method": method,
                        "result_base64": result_base64,
                    }))?,
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

impl From<ViewContext> for ArgsForViewContext {
    fn from(item: ViewContext) -> Self {
        item.0
    }
}
