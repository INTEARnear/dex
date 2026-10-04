use intear_dex_types::AssetId;
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_sdk::NearToken;
use near_sdk::json_types::U128;
use serde_json::json;

use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::{custody, fungible_token};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::PreflightError;
use crate::inputs::amount::AmountOrAllArg;
use crate::inputs::asset_ids::AssetIdArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, TransactionPlan};

/// Covers the balance query on the asset's contract, the engine's callback
/// and the transfer it makes
const RESCUE_GAS: Gas = Gas::from_teragas(100);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = RescueContext)]
pub struct Rescue {
    #[interactive_clap(skip_default_input_arg)]
    /// The asset: near, nep141:<token contract>, …
    asset: AssetIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How much, e.g. '10 NEAR' or '25.5 USDT', or all that's untracked
    amount: AmountOrAllArg,
    #[interactive_clap(named_arg)]
    /// Who receives it?
    to: RescueReceiver,
}

impl Rescue {
    fn input_asset(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AssetIdArg>> {
        crate::inputs::prompt("Which asset? e.g. near or nep141:usdt.tether-token.near")
    }

    fn input_amount(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AmountOrAllArg>> {
        crate::inputs::prompt(
            "How much? With the asset's symbol, e.g. '10 NEAR' or '25.5 USDT', or all that's untracked",
        )
    }
}

#[derive(Clone)]
pub struct RescueContext {
    global_context: crate::GlobalContext,
    asset_id: AssetId,
    amount: AmountOrAllArg,
}

impl RescueContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Rescue as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let AssetIdArg(asset_id) = scope.asset.clone();
        Ok(Self {
            global_context: previous_context,
            asset_id,
            amount: scope.amount.clone(),
        })
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = RescueContext)]
#[interactive_clap(output_context = RescueReceiverContext)]
pub struct RescueReceiver {
    #[interactive_clap(skip_default_input_arg)]
    /// The account that receives it
    receiver: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl RescueReceiver {
    fn input_receiver(
        context: &RescueContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_non_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "To whom?",
        )
    }
}

#[derive(Clone)]
pub struct RescueReceiverContext(ActionContext);

impl RescueReceiverContext {
    pub fn from_previous_context(
        previous_context: RescueContext,
        scope: &<RescueReceiver as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let RescueContext {
            global_context,
            asset_id,
            amount,
        } = previous_context;
        let receiver_id: near_primitives::types::AccountId = scope.receiver.clone().into();
        // rescue is private: only the engine's own account can call it
        Ok(Self(planning::write_action_context(
            &global_context,
            ENGINE_ACCOUNT_ID.to_owned(),
            move |preflight| {
                if !preflight.engine_paused {
                    return Err(PreflightError::NotPausedForRescue.into_report());
                }
                preflight.ensure_account_exists(&receiver_id)?;
                let metadata = AssetMetadataCache::new(
                    &preflight.network_config,
                    preflight.block_reference.clone(),
                )
                .get(&asset_id)?;
                let asset_label = display::asset_label(&asset_id, metadata.as_ref());
                let asset_custody = custody::custody_of(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &asset_id,
                )?;
                if let Some(held) = asset_custody.held
                    && asset_custody.deficit > 0
                {
                    return Err(PreflightError::CustodyDeficit {
                        held: display::format_amount(held, metadata.as_ref()),
                        in_custody: display::format_amount(
                            asset_custody.in_custody,
                            metadata.as_ref(),
                        ),
                    }
                    .into_report());
                }
                if asset_custody.untracked == 0 {
                    return Err(PreflightError::NothingToRescue { asset_label }.into_report());
                }
                let (requested_amount, amount_label) = match &amount {
                    AmountOrAllArg::All => (
                        None,
                        format!(
                            "all untracked {asset_label} ({})",
                            display::format_amount(asset_custody.untracked, metadata.as_ref())
                        ),
                    ),
                    AmountOrAllArg::Amount(amount) => {
                        let (_, raw_amount) =
                            amount.resolve(&[(asset_id.clone(), metadata.clone())])?;
                        if raw_amount == 0 {
                            return Err(PreflightError::ZeroAmount.into_report());
                        }
                        if raw_amount > asset_custody.untracked {
                            return Err(PreflightError::RescueTooLarge {
                                untracked: display::format_amount(
                                    asset_custody.untracked,
                                    metadata.as_ref(),
                                ),
                                requested: display::format_amount(raw_amount, metadata.as_ref()),
                            }
                            .into_report());
                        }
                        (
                            Some(U128(raw_amount)),
                            display::format_amount(raw_amount, metadata.as_ref()),
                        )
                    }
                };
                if let AssetId::Nep141(token_id) = &asset_id
                    && !fungible_token::is_registered(
                        &preflight.network_config,
                        &preflight.block_reference,
                        token_id,
                        &receiver_id,
                    )?
                {
                    return Err(PreflightError::RescueReceiverNotRegisteredWithToken {
                        receiver_id: receiver_id.clone(),
                        token_id: token_id.clone(),
                    }
                    .into_report());
                }
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: vec![format!(
                        "Send {amount_label} from {ENGINE_ACCOUNT_ID} to {receiver_id}"
                    )],
                    function_calls: vec![FunctionCall {
                        method_name: "rescue",
                        args: json!({
                            "asset_id": asset_id,
                            "amount": requested_amount,
                            "to": receiver_id,
                        }),
                        deposit: NearToken::from_yoctonear(0),
                        gas: RESCUE_GAS,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "rescue",
                        success_message: format!("Rescued {amount_label} to {receiver_id}"),
                        transfer_call: None,
                        dex_id: None,
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<RescueReceiverContext> for ActionContext {
    fn from(item: RescueReceiverContext) -> Self {
        item.0
    }
}
