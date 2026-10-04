use color_eyre::eyre::bail;
use intear_dex_types::{AccountOrDexId, AssetId};
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::types::AccountId;
use serde_json::json;

use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::engine;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::inputs::account_or_dex_id::AccountOrDexIdArg;
use crate::inputs::asset_ids::AssetSelectionArg;

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = BalanceContext)]
pub struct Balance {
    #[interactive_clap(skip_default_input_arg)]
    /// An account (alice.near) or a dex (slimedragon.near/xyk)
    owner: AccountOrDexIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// Comma-separated asset ids, or all to list every asset registered for an account
    assets: AssetSelectionArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl Balance {
    fn input_owner(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AccountOrDexIdArg>> {
        crate::inputs::prompt(
            "Whose balances? An account (alice.near) or a dex (slimedragon.near/xyk)",
        )
    }

    fn input_assets(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AssetSelectionArg>> {
        crate::inputs::prompt(
            "Which assets? Comma-separated asset ids (near,nep141:usdt.tether-token.near), or all for every asset registered for an account",
        )
    }
}

enum BalanceQuery {
    AllRegisteredAssetsOf(AccountId),
    ListedAssets(AccountOrDexId, Vec<AssetId>),
}

#[derive(Clone)]
pub struct BalanceContext(ArgsForViewContext);

impl BalanceContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Balance as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let query = match (&scope.owner.0, &scope.assets) {
            (AccountOrDexId::Account(account_id), AssetSelectionArg::All) => {
                BalanceQuery::AllRegisteredAssetsOf(account_id.clone())
            }
            (AccountOrDexId::Dex(dex_id), AssetSelectionArg::All) => bail!(
                "The assets of a dex can't be listed, so name them, e.g. `engine balance {dex_id} near,nep141:usdt.tether-token.near`"
            ),
            (owner, AssetSelectionArg::Listed(asset_ids)) => {
                BalanceQuery::ListedAssets(owner.clone(), asset_ids.clone())
            }
        };
        let owner = scope.owner.clone();
        let output_format = previous_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let balances: Vec<(AssetId, Option<u128>)> = match &query {
                    BalanceQuery::AllRegisteredAssetsOf(account_id) => {
                        engine::registered_assets_of(network_config, &block_reference, account_id)?
                            .into_iter()
                            .map(|(asset_id, balance)| (asset_id, Some(balance.0)))
                            .collect()
                    }
                    BalanceQuery::ListedAssets(owner, asset_ids) => {
                        let mut balances = Vec::new();
                        for asset_id in asset_ids {
                            let balance = engine::asset_balance_of(
                                network_config,
                                &block_reference,
                                owner,
                                asset_id,
                            )?;
                            balances.push((asset_id.clone(), balance.map(|balance| balance.0)));
                        }
                        balances
                    }
                };

                let metadata_cache = AssetMetadataCache::new(network_config, block_reference);
                match output_format {
                    OutputFormat::Table => {
                        if balances.is_empty() {
                            println!("{owner} has no assets registered on {ENGINE_ACCOUNT_ID}");
                            return Ok(());
                        }
                        let mut table = display::table(&["Asset", "Balance"]);
                        for (asset_id, balance) in &balances {
                            let balance = match balance {
                                Some(balance) => display::format_amount(
                                    *balance,
                                    metadata_cache.get(asset_id)?.as_ref(),
                                ),
                                None => "not registered".to_string(),
                            };
                            table.add_row(prettytable::row![asset_id, balance]);
                        }
                        print!("{table}");
                    }
                    OutputFormat::Json => {
                        let mut balances_json = Vec::new();
                        for (asset_id, balance) in &balances {
                            let balance = match balance {
                                Some(balance) => Some(display::amount_json(
                                    *balance,
                                    metadata_cache.get(asset_id)?.as_ref(),
                                )),
                                None => None,
                            };
                            balances_json.push(json!({
                                "asset_id": asset_id,
                                "registered": balance.is_some(),
                                "balance": balance,
                            }));
                        }
                        display::print_json(&json!({
                            "owner": owner.to_string(),
                            "balances": balances_json,
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

impl From<BalanceContext> for ArgsForViewContext {
    fn from(item: BalanceContext) -> Self {
        item.0
    }
}
