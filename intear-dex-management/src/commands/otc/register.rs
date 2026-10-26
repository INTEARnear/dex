use intear_dex_types::AssetId;
use near_cli_rs::commands::ActionContext;
use otc_dex_types::RegisterAssetsArgs;

use super::OtcContext;
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::otc;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::PreflightError;
use crate::inputs::asset_ids::AssetIdListArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, TransactionPlan, otc_storage};

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = OtcContext)]
#[interactive_clap(output_context = RegisterAssetsContext)]
pub struct RegisterAssets {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account registers the assets?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// Which assets: comma-separated asset ids
    assets: AssetIdListArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl RegisterAssets {
    fn input_signer_account_id(
        context: &OtcContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account registers the assets?",
        )
    }

    fn input_assets(_context: &OtcContext) -> color_eyre::eyre::Result<Option<AssetIdListArg>> {
        crate::inputs::prompt(
            "Which assets? Comma-separated asset ids, e.g. near,nep141:usdt.tether-token.near",
        )
    }
}

#[derive(Clone)]
pub struct RegisterAssetsContext(ActionContext);

impl RegisterAssetsContext {
    pub fn from_previous_context(
        previous_context: OtcContext,
        scope: &<RegisterAssets as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let AssetIdListArg(asset_ids) = scope.assets.clone();
        let dex_id = previous_context.dex_id;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                preflight.ensure_dex_exists(&dex_id)?;
                let signer_id = preflight.signer_id.clone();
                let metadata_cache = AssetMetadataCache::new(
                    &preflight.network_config,
                    preflight.block_reference.clone(),
                );
                let mut missing_asset_ids: Vec<AssetId> = Vec::new();
                for asset_id in &asset_ids {
                    if otc::balance(
                        &preflight.network_config,
                        &preflight.block_reference,
                        &dex_id,
                        &signer_id,
                        asset_id,
                    )?
                    .is_none()
                    {
                        missing_asset_ids.push(asset_id.clone());
                    }
                }
                if missing_asset_ids.is_empty() {
                    return Err(PreflightError::AlreadyRegistered {
                        owner: format!("{signer_id} on {dex_id}"),
                        assets: AssetIdListArg(asset_ids.clone()).to_string(),
                    }
                    .into_report());
                }
                let mut asset_labels = Vec::new();
                let mut new_bytes = 0u64;
                for asset_id in &missing_asset_ids {
                    asset_labels.push(display::asset_label(
                        asset_id,
                        metadata_cache.get(asset_id)?.as_ref(),
                    ));
                    new_bytes = new_bytes
                        .checked_add(otc_storage::balance_bytes(
                            &dex_id,
                            &signer_id,
                            asset_id,
                            preflight.protocol_limits.extra_bytes_per_record,
                        )?)
                        .ok_or_else(|| color_eyre::eyre::eyre!("Storage size overflow"))?;
                }
                let assets_label = asset_labels.join(", ");
                let (steps, function_calls) = super::otc_call_paying_for_storage(
                    preflight,
                    &dex_id,
                    format!("Register {assets_label} for {signer_id} on {dex_id}"),
                    (
                        "register_assets",
                        &RegisterAssetsArgs {
                            asset_ids: missing_asset_ids.clone(),
                        },
                    ),
                    &[],
                    &[],
                    new_bytes,
                )?;
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "registration",
                        success_message: format!(
                            "Registered {assets_label} for {signer_id} on {dex_id}"
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

impl From<RegisterAssetsContext> for ActionContext {
    fn from(item: RegisterAssetsContext) -> Self {
        item.0
    }
}
