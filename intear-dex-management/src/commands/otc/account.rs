use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::types::AccountId;
use near_sdk::json_types::U128;
use serde_json::json;

use super::OtcContext;
use crate::chain::asset_metadata::{AssetMetadata, AssetMetadataCache};
use crate::chain::{engine, otc};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::inputs::asset_ids::AssetIdListArg;

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = OtcContext)]
#[interactive_clap(output_context = AccountContext)]
pub struct Account {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account?
    account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// The assets to show balances of, comma-separated, since the dex can't list them (default: near)
    assets: Option<AssetIdListArg>,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl Account {
    fn input_account_id(
        context: &OtcContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_non_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account?",
        )
    }
}

#[derive(Clone)]
pub struct AccountContext(ArgsForViewContext);

impl AccountContext {
    pub fn from_previous_context(
        previous_context: OtcContext,
        scope: &<Account as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let account_id: AccountId = scope.account_id.clone().into();
        let asset_ids = scope.assets.clone().map_or_else(
            || vec![intear_dex_types::AssetId::Near],
            |AssetIdListArg(asset_ids)| asset_ids,
        );
        let dex_id = previous_context.dex_id;
        let output_format = previous_context.global_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let storage_balance = otc::storage_balance_of(
                    network_config,
                    &block_reference,
                    &dex_id,
                    &account_id,
                )?;
                let authorized_key =
                    otc::authorized_key(network_config, &block_reference, &dex_id, &account_id)?;
                let metadata_cache =
                    AssetMetadataCache::new(network_config, block_reference.clone());
                let mut balances = Vec::new();
                for asset_id in &asset_ids {
                    let balance = otc::balance(
                        network_config,
                        &block_reference,
                        &dex_id,
                        &account_id,
                        asset_id,
                    )?;
                    balances.push((asset_id, metadata_cache.get(asset_id)?, balance));
                }
                let near = AssetMetadata::near();
                match output_format {
                    OutputFormat::Table => {
                        let storage_label = match &storage_balance {
                            Some(storage_balance) => format!(
                                "{} total, {} used, {} available",
                                display::format_amount(
                                    storage_balance.total.as_yoctonear(),
                                    Some(&near)
                                ),
                                display::format_amount(
                                    storage_balance.used.as_yoctonear(),
                                    Some(&near)
                                ),
                                display::format_amount(
                                    storage_balance
                                        .total
                                        .saturating_sub(storage_balance.used)
                                        .as_yoctonear(),
                                    Some(&near)
                                ),
                            ),
                            None => "none".to_string(),
                        };
                        print!(
                            "{}",
                            display::key_value_table(vec![
                                ("Dex", dex_id.to_string()),
                                ("Account", account_id.to_string()),
                                ("Storage balance", storage_label),
                                (
                                    "Authorized key",
                                    authorized_key
                                        .as_ref()
                                        .map_or_else(|| "none".to_string(), String::from),
                                ),
                            ])
                        );
                        let mut table = display::table(&["Asset", "Balance"]);
                        for (asset_id, metadata, balance) in &balances {
                            table.add_row(prettytable::row![
                                asset_id,
                                match balance {
                                    Some(U128(amount)) =>
                                        display::format_amount(*amount, metadata.as_ref()),
                                    None => "not registered".to_string(),
                                },
                            ]);
                        }
                        print!("{table}");
                    }
                    OutputFormat::Json => {
                        let mut balances_json = Vec::new();
                        for (asset_id, metadata, balance) in &balances {
                            balances_json.push(json!({
                                "asset_id": asset_id,
                                "amount": balance.map(|U128(amount)| display::amount_json(amount, metadata.as_ref())),
                            }));
                        }
                        display::print_json(&json!({
                            "dex_id": dex_id,
                            "account_id": account_id,
                            "storage_balance": storage_balance.as_ref().map(|storage_balance| json!({
                                "total": storage_balance.total,
                                "used": storage_balance.used,
                            })),
                            "authorized_key": authorized_key,
                            "balances": balances_json,
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

impl From<AccountContext> for ArgsForViewContext {
    fn from(item: AccountContext) -> Self {
        item.0
    }
}
