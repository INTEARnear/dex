use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::types::AccountId;
use serde_json::json;

use super::OtcContext;
use crate::chain::{engine, otc};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = OtcContext)]
#[interactive_clap(output_context = NonceUsedContext)]
pub struct NonceUsed {
    #[interactive_clap(skip_default_input_arg)]
    /// Whose signed intents?
    account_id: near_cli_rs::types::account_id::AccountId,
    /// The nonce, a whole number
    nonce: u128,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl NonceUsed {
    fn input_account_id(
        context: &OtcContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_non_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Whose signed intents?",
        )
    }
}

#[derive(Clone)]
pub struct NonceUsedContext(ArgsForViewContext);

impl NonceUsedContext {
    pub fn from_previous_context(
        previous_context: OtcContext,
        scope: &<NonceUsed as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let account_id: AccountId = scope.account_id.clone().into();
        let nonce = scope.nonce;
        let dex_id = previous_context.dex_id;
        let output_format = previous_context.global_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let is_used = otc::is_nonce_used(
                    network_config,
                    &block_reference,
                    &dex_id,
                    &account_id,
                    nonce,
                )?;
                match output_format {
                    OutputFormat::Table if is_used => {
                        println!("{account_id} already used nonce {nonce} on {dex_id}")
                    }
                    OutputFormat::Table => {
                        println!("{account_id} can sign an intent with nonce {nonce} on {dex_id}")
                    }
                    OutputFormat::Json => display::print_json(&json!({
                        "dex_id": dex_id,
                        "account_id": account_id,
                        "nonce": nonce.to_string(),
                        "is_used": is_used,
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

impl From<NonceUsedContext> for ArgsForViewContext {
    fn from(item: NonceUsedContext) -> Self {
        item.0
    }
}
