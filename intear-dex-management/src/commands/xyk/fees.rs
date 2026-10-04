use intear_dex_types::{AccountOrDexId, AssetId, DexId, Operation, WithdrawAmount};
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
use xyk_dex_types::WithdrawFeesArgs;

use super::XykContext;
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::{engine, fungible_token, xyk};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::inputs::asset_ids::AssetSelectionArg;
use crate::inputs::funds::FundsDestinationArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan};

const WITHDRAW_FEES_GAS: Gas = Gas::from_teragas(60);
/// A withdrawal to the wallet calls the asset's contract, and the engine back
const WITHDRAWAL_TO_WALLET_GAS: Gas = Gas::from_teragas(20);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
pub struct FeesCommands {
    #[interactive_clap(subcommand)]
    action: FeesAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do with collected fees?
pub enum FeesAction {
    #[strum_discriminants(strum(
        message = "pending   - The fees an account collected and hasn't withdrawn"
    ))]
    /// The fees an account collected and hasn't withdrawn
    Pending(Pending),
    #[strum_discriminants(strum(message = "withdraw  - Withdraw the fees the signer collected"))]
    /// Withdraw the fees the signer collected
    Withdraw(Withdraw),
}

/// `all` means every asset that a pool of the dex holds
fn selected_asset_ids(
    selection: &AssetSelectionArg,
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<Vec<AssetId>> {
    match selection {
        AssetSelectionArg::All => {
            super::all_pool_asset_ids(network_config, block_reference, dex_id)
        }
        AssetSelectionArg::Listed(asset_ids) => Ok(asset_ids.clone()),
    }
}

/// The fees an account collected in each of `asset_ids` that it collects
/// fees in, in the order of `asset_ids`
fn pending_fees_in_order(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    account_id: &AccountId,
    asset_ids: Vec<AssetId>,
) -> color_eyre::eyre::Result<Vec<(AssetId, u128)>> {
    let mut pending_fees = xyk::pending_fees(
        network_config,
        block_reference,
        dex_id,
        account_id,
        asset_ids.clone(),
    )?;
    Ok(asset_ids
        .into_iter()
        .filter_map(|asset_id| {
            pending_fees
                .remove(&asset_id)
                .map(|U128(amount)| (asset_id, amount))
        })
        .collect())
}

fn selection_label(
    selection: &AssetSelectionArg,
    metadata_cache: &AssetMetadataCache,
) -> color_eyre::eyre::Result<String> {
    match selection {
        AssetSelectionArg::All => Ok("any pool asset".to_string()),
        AssetSelectionArg::Listed(asset_ids) => {
            let mut asset_labels = Vec::new();
            for asset_id in asset_ids {
                asset_labels.push(display::asset_label(
                    asset_id,
                    metadata_cache.get(asset_id)?.as_ref(),
                ));
            }
            Ok(asset_labels.join(", "))
        }
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = PendingContext)]
pub struct Pending {
    #[interactive_clap(skip_default_input_arg)]
    /// Whose fees?
    account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// Which assets: comma-separated asset ids, or all
    assets: AssetSelectionArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl Pending {
    fn input_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_non_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Whose fees?",
        )
    }

    fn input_assets(_context: &XykContext) -> color_eyre::eyre::Result<Option<AssetSelectionArg>> {
        crate::inputs::prompt(
            "Which assets? Comma-separated asset ids, e.g. near,nep141:usdt.tether-token.near, or all",
        )
    }
}

#[derive(Clone)]
pub struct PendingContext(ArgsForViewContext);

impl PendingContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Pending as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
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
                let pending_fees = pending_fees_in_order(
                    network_config,
                    &block_reference,
                    &dex_id,
                    &account_id,
                    asset_ids,
                )?;
                let metadata_cache =
                    AssetMetadataCache::new(network_config, block_reference.clone());
                match output_format {
                    OutputFormat::Table if pending_fees.is_empty() => {
                        println!(
                            "{account_id} collects no fees in {} on {dex_id}",
                            selection_label(&selection, &metadata_cache)?
                        );
                    }
                    OutputFormat::Table => {
                        let mut table = display::table(&["Asset", "Pending"]);
                        for (asset_id, amount) in &pending_fees {
                            let metadata = metadata_cache.get(asset_id)?;
                            table.add_row(prettytable::row![
                                asset_id,
                                display::format_amount(*amount, metadata.as_ref()),
                            ]);
                        }
                        print!("{table}");
                    }
                    OutputFormat::Json => {
                        let mut pending_json = Vec::new();
                        for (asset_id, amount) in &pending_fees {
                            pending_json.push(json!({
                                "asset_id": asset_id,
                                "amount": display::amount_json(*amount, metadata_cache.get(asset_id)?.as_ref()),
                            }));
                        }
                        display::print_json(&json!({
                            "account_id": account_id,
                            "dex_id": dex_id,
                            "pending": pending_json,
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

impl From<PendingContext> for ArgsForViewContext {
    fn from(item: PendingContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = WithdrawContext)]
pub struct Withdraw {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account withdraws the fees it collected?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// Which assets: comma-separated asset ids, or all
    assets: AssetSelectionArg,
    #[interactive_clap(skip_default_input_arg)]
    /// to-wallet, or to-balance on dex.intear.near
    destination: FundsDestinationArg,
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
            "Which account withdraws the fees it collected?",
        )
    }

    fn input_assets(_context: &XykContext) -> color_eyre::eyre::Result<Option<AssetSelectionArg>> {
        crate::inputs::prompt(
            "Which assets? Comma-separated asset ids, e.g. near,nep141:usdt.tether-token.near, or all",
        )
    }

    fn input_destination(
        _context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<FundsDestinationArg>> {
        crate::inputs::select(
            "Withdraw to the wallet, or into the balance on dex.intear.near?",
            vec![
                FundsDestinationArg::ToWallet,
                FundsDestinationArg::ToBalance,
            ],
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
        let selection = scope.assets.clone();
        let destination = scope.destination;
        let dex_id = previous_context.dex_id;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                let signer_id = preflight.signer_id.clone();
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
                let pending_fees = pending_fees_in_order(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                    &signer_id,
                    asset_ids,
                )?
                .into_iter()
                .filter(|(_, amount)| *amount > 0)
                .collect::<Vec<_>>();
                if pending_fees.is_empty() {
                    return Err(PreflightError::NoPendingFees {
                        owner: signer_id.clone(),
                        assets: selection_label(&selection, &metadata_cache)?,
                        dex_id: dex_id.clone(),
                    }
                    .into_report());
                }
                let mut amount_labels = Vec::new();
                for (asset_id, amount) in &pending_fees {
                    amount_labels.push(display::format_amount(
                        *amount,
                        metadata_cache.get(asset_id)?.as_ref(),
                    ));
                    if let (FundsDestinationArg::ToWallet, AssetId::Nep141(token_id)) =
                        (destination, asset_id)
                        && !fungible_token::is_registered(
                            &preflight.network_config,
                            &preflight.block_reference,
                            token_id,
                            &signer_id,
                        )?
                    {
                        return Err(PreflightError::NotRegisteredWithToken {
                            account_id: signer_id.clone(),
                            token_id: token_id.clone(),
                        }
                        .into_report());
                    }
                }
                let amounts_label = amount_labels.join(", ");
                let mut steps = Vec::new();
                let mut function_calls = Vec::new();
                // The dex moves the fees into the signer's balance on the
                // engine, which needs the assets registered
                let registration_calls = preflight.registration_calls(
                    &pending_fees
                        .iter()
                        .map(|(asset_id, _)| {
                            (AccountOrDexId::Account(signer_id.clone()), asset_id.clone())
                        })
                        .collect::<Vec<_>>(),
                )?;
                steps.extend(registration_calls.steps);
                function_calls.extend(registration_calls.function_calls);
                steps.push(format!(
                    "Move the fees {signer_id} collected on {dex_id} into its balance on {ENGINE_ACCOUNT_ID}: {amounts_label}"
                ));
                let mut operations = vec![planning::dex_call_operation(
                    &dex_id,
                    "withdraw_fees",
                    &WithdrawFeesArgs {
                        assets: pending_fees
                            .iter()
                            .map(|(asset_id, _)| asset_id.clone())
                            .collect(),
                    },
                    &[],
                )?];
                let mut gas = WITHDRAW_FEES_GAS;
                let destination_label = match destination {
                    FundsDestinationArg::ToWallet => {
                        steps.push(format!("Withdraw them from there to {signer_id}'s wallet"));
                        for (asset_id, amount) in &pending_fees {
                            operations.push(serde_json::to_value(Operation::Withdraw {
                                asset_id: asset_id.clone(),
                                amount: WithdrawAmount::Exact(U128(*amount)),
                                to: None,
                                rescue_address: None,
                            })?);
                            gas = gas
                                .checked_add(WITHDRAWAL_TO_WALLET_GAS)
                                .ok_or_else(|| color_eyre::eyre::eyre!("Gas overflow"))?;
                        }
                        format!("{signer_id}'s wallet")
                    }
                    FundsDestinationArg::ToBalance => {
                        format!("{signer_id}'s balance on {ENGINE_ACCOUNT_ID}")
                    }
                };
                function_calls.push(FunctionCall {
                    method_name: "execute_operations",
                    args: json!({ "operations": operations }),
                    deposit: ONE_YOCTO_NEAR,
                    gas,
                });
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "fee withdrawal",
                        success_message: format!(
                            "Withdrew fees from {dex_id} to {destination_label}: {amounts_label}"
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
