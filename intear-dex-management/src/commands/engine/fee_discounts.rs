use near_cli_rs::commands::ActionContext;
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::gas::Gas;
use near_sdk::{NearToken, PublicKey};
use serde_json::json;
use std::str::FromStr;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use crate::chain::engine;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::outcome::{DexEvents, ExpectedOutcome};
use crate::planning::{self, FunctionCall, TransactionPlan};

const SET_FEE_DISCOUNT_SIGNER_GAS: Gas = Gas::from_teragas(10);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
pub struct FeeDiscountsCommands {
    #[interactive_clap(subcommand)]
    action: FeeDiscountsAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do with fee discounts?
pub enum FeeDiscountsAction {
    #[strum_discriminants(strum(
        message = "show        - The key that signs fee discounts, if any"
    ))]
    /// The key that signs fee discounts, if any
    Show(Show),
    #[strum_discriminants(strum(message = "set-signer  - Set the key that signs fee discounts"))]
    /// Set the key that signs fee discounts
    SetSigner(SetSigner),
    #[strum_discriminants(strum(
        message = "disable     - Remove the signer, so that the engine accepts no fee discount"
    ))]
    /// Remove the signer, so that the engine accepts no fee discount
    Disable(Disable),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = ShowContext)]
pub struct Show {
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

#[derive(Clone)]
pub struct ShowContext(ArgsForViewContext);

impl ShowContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        _scope: &<Show as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let output_format = previous_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let fee_discount_signer =
                    engine::fee_discount_signer(network_config, &block_reference)?;
                match output_format {
                    OutputFormat::Table => {
                        print!(
                            "{}",
                            display::key_value_table(vec![(
                                "Fee discount signer",
                                match &fee_discount_signer {
                                    Some(public_key) => public_key.to_string(),
                                    None => "none, so fee discounts are disabled".to_string(),
                                },
                            ),])
                        );
                    }
                    OutputFormat::Json => display::print_json(&json!({
                        "fee_discount_signer": fee_discount_signer,
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

impl From<ShowContext> for ArgsForViewContext {
    fn from(item: ShowContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = SetSignerContext)]
pub struct SetSigner {
    #[interactive_clap(skip_default_input_arg)]
    /// The key that signs fee discounts from now on, e.g. 'ed25519:…'
    fee_discount_signer: near_cli_rs::types::public_key::PublicKey,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl SetSigner {
    fn input_fee_discount_signer(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::public_key::PublicKey>> {
        match inquire::CustomType::new("Which key signs fee discounts from now on? e.g. ed25519:…")
            .with_error_message("That isn't a public key like ed25519:…")
            .prompt()
        {
            Ok(fee_discount_signer) => Ok(Some(fee_discount_signer)),
            Err(
                inquire::error::InquireError::OperationCanceled
                | inquire::error::InquireError::OperationInterrupted,
            ) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Clone)]
pub struct SetSignerContext(ActionContext);

impl SetSignerContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<SetSigner as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let fee_discount_signer = PublicKey::from_str(&scope.fee_discount_signer.to_string())
            .map_err(|error| {
                color_eyre::eyre::eyre!(
                    "Invalid fee discount signer {}: {error:?}",
                    scope.fee_discount_signer
                )
            })?;
        Ok(Self(change_fee_discount_signer_action_context(
            &previous_context,
            Some(fee_discount_signer),
        )))
    }
}

impl From<SetSignerContext> for ActionContext {
    fn from(item: SetSignerContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = DisableContext)]
pub struct Disable {
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

#[derive(Clone)]
pub struct DisableContext(ActionContext);

impl DisableContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        _scope: &<Disable as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self(change_fee_discount_signer_action_context(
            &previous_context,
            None,
        )))
    }
}

impl From<DisableContext> for ActionContext {
    fn from(item: DisableContext) -> Self {
        item.0
    }
}

/// What `fee_discount_signer` makes of fee discounts: who signs them, or that there are none
fn fee_discounts_label(fee_discount_signer: Option<&PublicKey>) -> String {
    match fee_discount_signer {
        Some(public_key) => format!("fee discounts signed by {public_key}"),
        None => "fee discounts disabled".to_string(),
    }
}

/// The transaction of the engine's own account that sets the key signing fee discounts on it to
/// `new_fee_discount_signer`, or removes it with `None`
fn change_fee_discount_signer_action_context(
    global_context: &crate::GlobalContext,
    new_fee_discount_signer: Option<PublicKey>,
) -> ActionContext {
    planning::write_action_context(
        global_context,
        ENGINE_ACCOUNT_ID.to_owned(),
        move |preflight| {
            let current_fee_discount_signer =
                engine::fee_discount_signer(&preflight.network_config, &preflight.block_reference)?;
            if current_fee_discount_signer == new_fee_discount_signer {
                return Err(PreflightError::FeeDiscountSignerUnchanged {
                    fee_discounts: fee_discounts_label(current_fee_discount_signer.as_ref()),
                }
                .into_report());
            }
            let step = format!(
                "Change {ENGINE_ACCOUNT_ID} from {} to {}",
                fee_discounts_label(current_fee_discount_signer.as_ref()),
                fee_discounts_label(new_fee_discount_signer.as_ref())
            );
            let function_calls = vec![FunctionCall {
                method_name: "set_fee_discount_signer",
                args: json!({ "fee_discount_signer": new_fee_discount_signer }).into(),
                deposit: NearToken::from_yoctonear(1),
                gas: SET_FEE_DISCOUNT_SIGNER_GAS,
            }];
            Ok(TransactionPlan {
                receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                steps: vec![step],
                function_calls,
                expected_outcome: ExpectedOutcome {
                    action_name: "fee discount signer change",
                    success_message: format!(
                        "{ENGINE_ACCOUNT_ID} now has {}",
                        fee_discounts_label(new_fee_discount_signer.as_ref())
                    ),
                    transfer_call: None,
                    dex_id: None,
                    dex_events: DexEvents::NotRead,
                    deployment: None,
                },
                deploy_code: None,
            })
        },
    )
}
