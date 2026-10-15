use intear_dex_types::DexId;
use near_cli_rs::commands::ActionContext;
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::PublicKey;
use near_sdk::json_types::Base64VecU8;
use serde_json::json;
use std::str::FromStr;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use xyk_dex_types::{CAN_MIGRATE, SetFeeDiscountSignerArgs};

use super::XykContext;
use crate::chain::{engine, xyk};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan};

const SET_FEE_DISCOUNT_SIGNER_GAS: Gas = Gas::from_teragas(50);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
pub struct FeeDiscountsCommands {
    #[interactive_clap(subcommand)]
    action: FeeDiscountsAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
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
        message = "disable     - Remove the signer, so that the dex accepts no fee discount"
    ))]
    /// Remove the signer, so that the dex accepts no fee discount
    Disable(Disable),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
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
        previous_context: XykContext,
        _scope: &<Show as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = previous_context.dex_id;
        let output_format = previous_context.global_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let fee_discount_signer =
                    xyk::fee_discount_signer(network_config, &block_reference, &dex_id)?;
                match output_format {
                    OutputFormat::Table => {
                        print!(
                            "{}",
                            display::key_value_table(vec![
                                ("Dex", dex_id.to_string()),
                                (
                                    "Fee discount signer",
                                    match &fee_discount_signer {
                                        Some(public_key) => public_key.to_string(),
                                        None => "none, so fee discounts are disabled".to_string(),
                                    },
                                ),
                            ])
                        );
                    }
                    OutputFormat::Json => display::print_json(&json!({
                        "dex_id": dex_id,
                        "fee_discount_signer": fee_discount_signer,
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
#[interactive_clap(output_context = SetSignerContext)]
pub struct SetSigner {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account sets the signer? Only slimedragon.near can
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The key that signs fee discounts from now on, e.g. 'ed25519:…'
    fee_discount_signer: near_cli_rs::types::public_key::PublicKey,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl SetSigner {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        input_signer_account_id(context)
    }

    fn input_fee_discount_signer(
        _context: &XykContext,
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
        previous_context: XykContext,
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
            scope.signer_account_id.clone().into(),
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
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = DisableContext)]
pub struct Disable {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account disables fee discounts? Only slimedragon.near can
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Disable {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        input_signer_account_id(context)
    }
}

#[derive(Clone)]
pub struct DisableContext(ActionContext);

impl DisableContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Disable as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self(change_fee_discount_signer_action_context(
            &previous_context,
            scope.signer_account_id.clone().into(),
            None,
        )))
    }
}

impl From<DisableContext> for ActionContext {
    fn from(item: DisableContext) -> Self {
        item.0
    }
}

fn input_signer_account_id(
    context: &XykContext,
) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
    near_cli_rs::common::input_signer_account_id_from_used_account_list(
        &context.global_context.config.credentials_home_dir,
        &format!("Which account signs? It has to be {CAN_MIGRATE}"),
    )
}

/// What `fee_discount_signer` makes of fee discounts: who signs them, or that there are none
fn fee_discounts_label(fee_discount_signer: Option<&PublicKey>) -> String {
    match fee_discount_signer {
        Some(public_key) => format!("fee discounts signed by {public_key}"),
        None => "fee discounts disabled".to_string(),
    }
}

/// The transaction that sets the key signing fee discounts on the dex to
/// `new_fee_discount_signer`, or removes it with `None`
fn change_fee_discount_signer_action_context(
    context: &XykContext,
    signer_id: AccountId,
    new_fee_discount_signer: Option<PublicKey>,
) -> ActionContext {
    let dex_id: DexId = context.dex_id.clone();
    planning::write_action_context(&context.global_context, signer_id, move |preflight| {
        preflight.ensure_engine_not_paused()?;
        if preflight.signer_id != CAN_MIGRATE {
            return Err(PreflightError::NotAllowedToSetFeeDiscountSigner {
                signer_id: preflight.signer_id.clone(),
            }
            .into_report());
        }
        let current_fee_discount_signer = xyk::fee_discount_signer(
            &preflight.network_config,
            &preflight.block_reference,
            &dex_id,
        )?;
        if current_fee_discount_signer == new_fee_discount_signer {
            return Err(PreflightError::FeeDiscountSignerUnchanged {
                dex_id: dex_id.clone(),
                fee_discounts: fee_discounts_label(current_fee_discount_signer.as_ref()),
            }
            .into_report());
        }
        let step = format!(
            "Change {dex_id} from {} to {}",
            fee_discounts_label(current_fee_discount_signer.as_ref()),
            fee_discounts_label(new_fee_discount_signer.as_ref())
        );
        let function_calls = vec![FunctionCall {
            method_name: "dex_call",
            args: json!({
                "dex_id": dex_id,
                "method": "set_fee_discount_signer",
                "args": Base64VecU8(near_sdk::borsh::to_vec(&SetFeeDiscountSignerArgs {
                    fee_discount_signer_public_key: new_fee_discount_signer.clone(),
                })?),
                "attached_assets": {},
                "referrer": null,
            }),
            deposit: ONE_YOCTO_NEAR,
            gas: SET_FEE_DISCOUNT_SIGNER_GAS,
        }];
        Ok(TransactionPlan {
            receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
            steps: vec![step],
            function_calls,
            expected_outcome: ExpectedOutcome {
                action_name: "fee discount signer change",
                success_message: format!(
                    "{dex_id} now has {}",
                    fee_discounts_label(new_fee_discount_signer.as_ref())
                ),
                transfer_call: None,
                dex_id: Some(dex_id.clone()),
                deployment: None,
            },
        })
    })
}
