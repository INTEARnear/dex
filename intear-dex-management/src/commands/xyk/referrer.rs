use intear_dex_types::AssetId;
use near_cli_rs::commands::ActionContext;
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use xyk_dex_types::{ReferralSettings, RegisterFeeAssetsArgs, SetReferrerSettingsArgs};

use super::XykContext;
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::{engine, xyk};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::inputs::asset_ids::AssetIdListArg;
use crate::inputs::percent::PercentArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, TransactionPlan, storage, xyk_storage};

const SET_REFERRER_SETTINGS_GAS: Gas = Gas::from_teragas(100);
const REGISTER_FEE_ASSETS_GAS: Gas = Gas::from_teragas(100);

/// The pairs that the reduced referral fee applies to, after the xyk
/// contract's list of assets that reduce the protocol fee
const REDUCED_FEE_PAIRS: &str =
    "pairs of NEAR, USDC, USDT and tokens bridged through omft.near or omni.hot.tg";

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
pub struct ReferrerCommands {
    #[interactive_clap(subcommand)]
    action: ReferrerAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do as a referrer?
pub enum ReferrerAction {
    #[strum_discriminants(strum(
        message = "show                 - A referrer's fees and the assets it collects fees in"
    ))]
    /// A referrer's fees and the assets it collects fees in
    Show(Show),
    #[strum_discriminants(strum(
        message = "set-fees             - Set the signer's referral fees"
    ))]
    /// Set the signer's referral fees
    SetFees(SetFees),
    #[strum_discriminants(strum(
        message = "register-fee-assets  - Collect fees in more assets, which referral fees need"
    ))]
    /// Collect fees in more assets, which referral fees need
    RegisterFeeAssets(RegisterFeeAssets),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = ShowContext)]
pub struct Show {
    #[interactive_clap(skip_default_input_arg)]
    /// Which referrer?
    account_id: near_cli_rs::types::account_id::AccountId,
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
            "Which referrer?",
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
        let dex_id = previous_context.dex_id;
        let output_format = previous_context.global_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let referral_settings =
                    xyk::referral_settings(network_config, &block_reference, &dex_id, &account_id)?;
                let pool_asset_ids =
                    super::all_pool_asset_ids(network_config, &block_reference, &dex_id)?;
                let collected_fees = xyk::pending_fees(
                    network_config,
                    &block_reference,
                    &dex_id,
                    &account_id,
                    pool_asset_ids.clone(),
                )?;
                let fee_asset_ids = pool_asset_ids
                    .into_iter()
                    .filter(|asset_id| collected_fees.contains_key(asset_id))
                    .collect::<Vec<_>>();
                let fees = referral_settings.map(|referral_settings| match referral_settings {
                    ReferralSettings::V1 {
                        fee_fraction,
                        fee_fraction_reduced,
                    } => (fee_fraction, fee_fraction_reduced),
                });
                match output_format {
                    OutputFormat::Table => {
                        let metadata_cache =
                            AssetMetadataCache::new(network_config, block_reference.clone());
                        let mut fee_asset_labels = Vec::new();
                        for asset_id in &fee_asset_ids {
                            fee_asset_labels.push(display::asset_label(
                                asset_id,
                                metadata_cache.get(asset_id)?.as_ref(),
                            ));
                        }
                        let mut rows = vec![
                            ("Referrer", account_id.to_string()),
                            ("Dex", dex_id.to_string()),
                        ];
                        match fees {
                            Some((fee, reduced_fee)) => {
                                rows.push(("Referral fee", display::format_fee(fee)));
                                rows.push((
                                    "Reduced referral fee",
                                    format!(
                                        "{}, for {REDUCED_FEE_PAIRS}",
                                        display::format_fee(reduced_fee)
                                    ),
                                ));
                            }
                            None => rows.push(("Referral fee", "not set".to_string())),
                        }
                        rows.push((
                            "Collects fees in",
                            if fee_asset_labels.is_empty() {
                                "no pool asset, so it takes no referral fee".to_string()
                            } else {
                                fee_asset_labels.join(", ")
                            },
                        ));
                        print!("{}", display::key_value_table(rows));
                    }
                    OutputFormat::Json => display::print_json(&json!({
                        "account_id": account_id,
                        "dex_id": dex_id,
                        "referral_fees": fees.map(|(fee, reduced_fee)| json!({
                            "fee": display::fee_json(fee),
                            "reduced_fee": display::fee_json(reduced_fee),
                        })),
                        "fee_asset_ids": fee_asset_ids,
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

impl From<ShowContext> for ArgsForViewContext {
    fn from(item: ShowContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = SetFeesContext)]
pub struct SetFees {
    #[interactive_clap(skip_default_input_arg)]
    /// Which referrer sets its fees?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The referral fee, less than 5%, e.g. 0.1%
    fee: PercentArg,
    #[interactive_clap(skip_default_input_arg)]
    /// The referral fee for pairs of NEAR, USDC, USDT and bridged tokens, e.g. 0.05%
    reduced_fee: PercentArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl SetFees {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which referrer sets its fees?",
        )
    }

    fn input_fee(_context: &XykContext) -> color_eyre::eyre::Result<Option<PercentArg>> {
        crate::inputs::prompt("The referral fee? Less than 5%, e.g. 0.1%")
    }

    fn input_reduced_fee(_context: &XykContext) -> color_eyre::eyre::Result<Option<PercentArg>> {
        crate::inputs::prompt(&format!(
            "The referral fee for {REDUCED_FEE_PAIRS}? e.g. 0.05%"
        ))
    }
}

#[derive(Clone)]
pub struct SetFeesContext(ActionContext);

impl SetFeesContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<SetFees as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let PercentArg(fee) = scope.fee;
        let PercentArg(reduced_fee) = scope.reduced_fee;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                let signer_id = preflight.signer_id.clone();
                let new_settings = ReferralSettings::V1 {
                    fee_fraction: fee,
                    fee_fraction_reduced: reduced_fee,
                };
                if new_settings.validate().is_err() {
                    return Err(PreflightError::ReferralFeeTooHigh.into_report());
                }
                let has_settings = xyk::referral_settings(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                    &signer_id,
                )?
                .is_some();
                // New settings replace old ones in the same space
                let storage_bytes = if has_settings {
                    0
                } else {
                    xyk_storage::referral_settings_bytes(
                        &dex_id,
                        &signer_id,
                        preflight.protocol_limits.extra_bytes_per_record,
                    )?
                };
                let storage_near = storage::storage_cost(
                    storage_bytes,
                    preflight.protocol_limits.storage_byte_cost,
                )?;
                let fees_label = format!(
                    "{}, and {} for {REDUCED_FEE_PAIRS}",
                    display::format_fee(fee),
                    display::format_fee(reduced_fee)
                );
                let (steps, function_calls) = super::dex_call_paying_for_storage(
                    preflight,
                    &dex_id,
                    format!("Set {signer_id}'s referral fees on {dex_id} to {fees_label}"),
                    (
                        "set_referrer_settings",
                        &SetReferrerSettingsArgs { new_settings },
                    ),
                    storage_near,
                    SET_REFERRER_SETTINGS_GAS,
                )?;
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "referral fee change",
                        success_message: format!(
                            "Set {signer_id}'s referral fees on {dex_id} to {fees_label}"
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

impl From<SetFeesContext> for ActionContext {
    fn from(item: SetFeesContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = RegisterFeeAssetsContext)]
pub struct RegisterFeeAssets {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account collects fees in the assets?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// Comma-separated asset ids, e.g. near,nep141:usdt.tether-token.near
    assets: AssetIdListArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl RegisterFeeAssets {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account collects fees in the assets?",
        )
    }

    fn input_assets(_context: &XykContext) -> color_eyre::eyre::Result<Option<AssetIdListArg>> {
        crate::inputs::prompt(
            "Which assets? Comma-separated asset ids, e.g. near,nep141:usdt.tether-token.near",
        )
    }
}

#[derive(Clone)]
pub struct RegisterFeeAssetsContext(ActionContext);

impl RegisterFeeAssetsContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<RegisterFeeAssets as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let AssetIdListArg(asset_ids) = scope.assets.clone();
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
                let asset_labels = |asset_ids: &[AssetId]| -> color_eyre::eyre::Result<String> {
                    let mut labels = Vec::new();
                    for asset_id in asset_ids {
                        labels.push(display::asset_label(
                            asset_id,
                            metadata_cache.get(asset_id)?.as_ref(),
                        ));
                    }
                    Ok(labels.join(", "))
                };
                let collected_fees = xyk::pending_fees(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                    &signer_id,
                    asset_ids.clone(),
                )?;
                let registered_asset_ids = asset_ids
                    .iter()
                    .filter(|asset_id| collected_fees.contains_key(*asset_id))
                    .cloned()
                    .collect::<Vec<_>>();
                if !registered_asset_ids.is_empty() {
                    return Err(PreflightError::FeeAssetsAlreadyRegistered {
                        owner: signer_id.clone(),
                        assets: asset_labels(&registered_asset_ids)?,
                        dex_id: dex_id.clone(),
                    }
                    .into_report());
                }
                let mut storage_bytes = 0u64;
                for asset_id in &asset_ids {
                    storage_bytes = storage_bytes
                        .checked_add(xyk_storage::fee_balance_bytes(
                            &dex_id,
                            &signer_id,
                            asset_id,
                            preflight.protocol_limits.extra_bytes_per_record,
                        )?)
                        .ok_or_else(|| color_eyre::eyre::eyre!("Storage size overflow"))?;
                }
                let storage_near = storage::storage_cost(
                    storage_bytes,
                    preflight.protocol_limits.storage_byte_cost,
                )?;
                let assets_label = asset_labels(&asset_ids)?;
                let (steps, function_calls) = super::dex_call_paying_for_storage(
                    preflight,
                    &dex_id,
                    format!(
                        "Start fee balances of {signer_id} in {assets_label} on {dex_id}, so it can take referral fees paid in them"
                    ),
                    (
                        "register_fee_assets",
                        &RegisterFeeAssetsArgs {
                            asset_ids: asset_ids.clone(),
                        },
                    ),
                    storage_near,
                    REGISTER_FEE_ASSETS_GAS,
                )?;
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "fee asset registration",
                        success_message: format!(
                            "{signer_id} now collects fees in {assets_label} on {dex_id}"
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

impl From<RegisterFeeAssetsContext> for ActionContext {
    fn from(item: RegisterFeeAssetsContext) -> Self {
        item.0
    }
}
