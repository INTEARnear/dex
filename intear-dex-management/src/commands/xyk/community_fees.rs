use intear_dex_types::{AssetId, DexId};
use near_cli_rs::commands::ActionContext;
use near_cli_rs::config::NetworkConfig;
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::gas::Gas;
use near_primitives::types::{AccountId, BlockReference};
use near_sdk::json_types::U128;
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use xyk_dex_types::ClaimFeesArgs;

use super::XykContext;
use super::fees::{WITHDRAWAL_TO_WALLET_GAS, selected_asset_ids, selection_label};
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::{engine, fungible_token, xyk};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::inputs::asset_ids::AssetSelectionArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan};

const CLAIM_FEES_ANYONE_GAS: Gas = Gas::from_teragas(60);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
pub struct CommunityFeesCommands {
    #[interactive_clap(subcommand)]
    action: CommunityFeesAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do with community fees?
pub enum CommunityFeesAction {
    #[strum_discriminants(strum(
        message = "show      - The fees that anyone can claim for a community account"
    ))]
    /// The fees that anyone can claim for a community account
    Show(Show),
    #[strum_discriminants(strum(
        message = "withdraw  - Send a community account its fees, which anyone can"
    ))]
    /// Send a community account its fees, which anyone can
    Withdraw(Withdraw),
}

/// The fees that anyone can claim for an account in each of `asset_ids`
/// that it has a balance in, in the order of `asset_ids`
fn claimable_fees_in_order(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    account_id: &AccountId,
    asset_ids: Vec<AssetId>,
) -> color_eyre::eyre::Result<Vec<(AssetId, u128)>> {
    let mut claimable_fees = xyk::claimable_fees(
        network_config,
        block_reference,
        dex_id,
        account_id,
        asset_ids.clone(),
    )?;
    Ok(asset_ids
        .into_iter()
        .filter_map(|asset_id| {
            claimable_fees
                .remove(&asset_id)
                .map(|U128(amount)| (asset_id, amount))
        })
        .collect())
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = ShowContext)]
pub struct Show {
    #[interactive_clap(skip_default_input_arg)]
    /// Which community account?
    account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// Which assets: comma-separated asset ids, or all
    assets: AssetSelectionArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl Show {
    fn input_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_non_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which community account?",
        )
    }

    fn input_assets(_context: &XykContext) -> color_eyre::eyre::Result<Option<AssetSelectionArg>> {
        crate::inputs::prompt(
            "Which assets? Comma-separated asset ids, e.g. near,nep141:usdt.tether-token.near, or all",
        )
    }
}

#[derive(Clone)]
pub struct ShowContext(ArgsForViewContext);

impl ShowContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Show as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let account_id: AccountId = scope.account_id.clone().into();
        let selection = scope.assets.clone();
        let dex_id = previous_context.dex_id;
        let output_format = previous_context.global_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let asset_ids =
                    selected_asset_ids(&selection, network_config, &block_reference, &dex_id)?;
                let claimable_fees = claimable_fees_in_order(
                    network_config,
                    &block_reference,
                    &dex_id,
                    &account_id,
                    asset_ids,
                )?;
                let metadata_cache =
                    AssetMetadataCache::new(network_config, block_reference.clone());
                match output_format {
                    OutputFormat::Table if claimable_fees.is_empty() => {
                        println!(
                            "{account_id} collects no community fees in {} on {dex_id}",
                            selection_label(&selection, &metadata_cache)?
                        );
                    }
                    OutputFormat::Table => {
                        let mut table = display::table(&["Asset", "Claimable"]);
                        for (asset_id, amount) in &claimable_fees {
                            let metadata = metadata_cache.get(asset_id)?;
                            table.add_row(prettytable::row![
                                asset_id,
                                display::format_amount(*amount, metadata.as_ref()),
                            ]);
                        }
                        print!("{table}");
                    }
                    OutputFormat::Json => {
                        let mut claimable_json = Vec::new();
                        for (asset_id, amount) in &claimable_fees {
                            claimable_json.push(json!({
                                "asset_id": asset_id,
                                "amount": display::amount_json(*amount, metadata_cache.get(asset_id)?.as_ref()),
                            }));
                        }
                        display::print_json(&json!({
                            "account_id": account_id,
                            "dex_id": dex_id,
                            "claimable": claimable_json,
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

impl From<ShowContext> for ArgsForViewContext {
    fn from(item: ShowContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = WithdrawContext)]
pub struct Withdraw {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account signs? Anyone can send a community account its fees
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// Which community account gets its fees?
    account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// Which assets: comma-separated asset ids, or all
    assets: AssetSelectionArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Withdraw {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account signs? Anyone can send a community account its fees",
        )
    }

    fn input_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_non_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which community account gets its fees?",
        )
    }

    fn input_assets(_context: &XykContext) -> color_eyre::eyre::Result<Option<AssetSelectionArg>> {
        crate::inputs::prompt(
            "Which assets? Comma-separated asset ids, e.g. near,nep141:usdt.tether-token.near, or all",
        )
    }
}

#[derive(Clone)]
pub struct WithdrawContext(ActionContext);

impl WithdrawContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Withdraw as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let account_id: AccountId = scope.account_id.clone().into();
        let selection = scope.assets.clone();
        let dex_id = previous_context.dex_id;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                // The fees go to the account's wallet
                preflight.ensure_account_exists(&account_id)?;
                let metadata_cache = AssetMetadataCache::new(
                    &preflight.network_config,
                    preflight.block_reference.clone(),
                );
                let asset_ids = selected_asset_ids(
                    &selection,
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                )?;
                let claimable_fees = claimable_fees_in_order(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                    &account_id,
                    asset_ids,
                )?
                .into_iter()
                .filter(|(_, amount)| *amount > 0)
                .collect::<Vec<_>>();
                if claimable_fees.is_empty() {
                    return Err(PreflightError::NoCommunityFees {
                        account_id: account_id.clone(),
                        assets: selection_label(&selection, &metadata_cache)?,
                        dex_id: dex_id.clone(),
                    }
                    .into_report());
                }
                let mut amount_labels = Vec::new();
                let mut gas = CLAIM_FEES_ANYONE_GAS;
                for (asset_id, amount) in &claimable_fees {
                    amount_labels.push(display::format_amount(
                        *amount,
                        metadata_cache.get(asset_id)?.as_ref(),
                    ));
                    if let AssetId::Nep141(token_id) = asset_id
                        && !fungible_token::is_registered(
                            &preflight.network_config,
                            &preflight.block_reference,
                            token_id,
                            &account_id,
                        )?
                    {
                        return Err(PreflightError::NotRegisteredWithToken {
                            account_id: account_id.clone(),
                            token_id: token_id.clone(),
                        }
                        .into_report());
                    }
                    gas = gas
                        .checked_add(WITHDRAWAL_TO_WALLET_GAS)
                        .ok_or_else(|| color_eyre::eyre::eyre!("Gas overflow"))?;
                }
                let amounts_label = amount_labels.join(", ");
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: vec![format!(
                        "Send the community fees that {account_id} collected on {dex_id} to its wallet: {amounts_label}"
                    )],
                    function_calls: vec![FunctionCall {
                        method_name: "execute_operations",
                        args: json!({
                            "operations": [planning::dex_call_operation(
                                &dex_id,
                                "claim_fees_anyone",
                                &ClaimFeesArgs {
                                    account_id: account_id.clone(),
                                    asset_ids: claimable_fees
                                        .iter()
                                        .map(|(asset_id, _)| asset_id.clone())
                                        .collect(),
                                },
                                &[],
                            )?],
                        }),
                        deposit: ONE_YOCTO_NEAR,
                        gas,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "community fee withdrawal",
                        success_message: format!(
                            "Sent community fees from {dex_id} to {account_id}'s wallet: {amounts_label}"
                        ),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<WithdrawContext> for ActionContext {
    fn from(item: WithdrawContext) -> Self {
        item.0
    }
}
