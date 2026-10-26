use intear_dex_types::{AccountOrDexId, AssetId};
use near_cli_rs::commands::ActionContext;
use otc_dex_types::DepositAssetsArgs;

use super::OtcContext;
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::otc;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::PreflightError;
use crate::inputs::asset_amounts::AssetAmountsArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, TransactionPlan, otc_storage};

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = OtcContext)]
#[interactive_clap(output_context = DepositContext)]
pub struct Deposit {
    #[interactive_clap(skip_default_input_arg)]
    /// Whose balance on dex.intear.near do the assets come from?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The assets and amounts, e.g. 'near=1 NEAR,nep141:usdt.tether-token.near=25 USDt'
    assets: AssetAmountsArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Deposit {
    fn input_signer_account_id(
        context: &OtcContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Whose balance on dex.intear.near do the assets come from?",
        )
    }

    fn input_assets(_context: &OtcContext) -> color_eyre::eyre::Result<Option<AssetAmountsArg>> {
        crate::inputs::prompt(
            "Which assets and how much? e.g. 'near=1 NEAR,nep141:usdt.tether-token.near=25 USDt'",
        )
    }
}

#[derive(Clone)]
pub struct DepositContext(ActionContext);

impl DepositContext {
    pub fn from_previous_context(
        previous_context: OtcContext,
        scope: &<Deposit as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let AssetAmountsArg(asset_amounts) = scope.assets.clone();
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
                let mut attached_assets: Vec<(AssetId, u128)> = Vec::new();
                let mut amount_labels = Vec::new();
                let mut new_bytes = 0u64;
                for (asset_id, amount) in &asset_amounts {
                    let metadata = metadata_cache.get(asset_id)?;
                    let (_, raw_amount) =
                        amount.resolve(&[(asset_id.clone(), metadata.clone())])?;
                    if raw_amount == 0 {
                        return Err(PreflightError::ZeroAmount.into_report());
                    }
                    preflight.ensure_engine_balance(asset_id, metadata.as_ref(), raw_amount)?;
                    // The first deposit of an asset stores the signer's balance of it
                    let has_otc_balance = otc::balance(
                        &preflight.network_config,
                        &preflight.block_reference,
                        &dex_id,
                        &signer_id,
                        asset_id,
                    )?
                    .is_some();
                    if !has_otc_balance {
                        new_bytes = new_bytes
                            .checked_add(otc_storage::balance_bytes(
                                &dex_id,
                                &signer_id,
                                asset_id,
                                preflight.protocol_limits.extra_bytes_per_record,
                            )?)
                            .ok_or_else(|| color_eyre::eyre::eyre!("Storage size overflow"))?;
                    }
                    amount_labels.push(display::format_amount(raw_amount, metadata.as_ref()));
                    attached_assets.push((asset_id.clone(), raw_amount));
                }
                // The engine moves the attached assets to the dex's balances
                let registrations = attached_assets
                    .iter()
                    .map(|(asset_id, _)| (AccountOrDexId::Dex(dex_id.clone()), asset_id.clone()))
                    .collect::<Vec<_>>();
                let amounts_label = amount_labels.join(" and ");
                let (steps, function_calls) = super::otc_call_paying_for_storage(
                    preflight,
                    &dex_id,
                    format!(
                        "Move {amounts_label} from {signer_id}'s balance on {ENGINE_ACCOUNT_ID} to its balance on {dex_id}"
                    ),
                    ("deposit_assets", &DepositAssetsArgs),
                    &attached_assets,
                    &registrations,
                    new_bytes,
                )?;
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "deposit",
                        success_message: format!(
                            "Deposited {amounts_label} into {signer_id}'s balance on {dex_id}"
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

impl From<DepositContext> for ActionContext {
    fn from(item: DepositContext) -> Self {
        item.0
    }
}
