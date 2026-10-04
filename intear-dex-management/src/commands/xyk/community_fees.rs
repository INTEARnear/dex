use near_cli_rs::commands::ActionContext;
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use xyk_dex_types::WithdrawCommunityFeeArgs;

use super::XykContext;
use crate::chain::asset_metadata::AssetMetadata;
use crate::chain::{engine, xyk};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan, format_near};

const WITHDRAW_COMMUNITY_FEE_GAS: Gas = Gas::from_teragas(60);

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
        message = "show      - The NEAR that launch pools collected for a community account"
    ))]
    /// The NEAR that launch pools collected for a community account
    Show(Show),
    #[strum_discriminants(strum(
        message = "withdraw  - Send a community account its fees, which anyone can"
    ))]
    /// Send a community account its fees, which anyone can
    Withdraw(Withdraw),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = ShowContext)]
pub struct Show {
    #[interactive_clap(skip_default_input_arg)]
    /// Which community account?
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
            "Which community account?",
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
                let community_fees = xyk::community_owned_fees(
                    network_config,
                    &block_reference,
                    &dex_id,
                    &account_id,
                )?;
                match output_format {
                    OutputFormat::Table => print!(
                        "{}",
                        display::key_value_table(vec![
                            ("Account", account_id.to_string()),
                            ("Dex", dex_id.to_string()),
                            ("Community fees", format_near(community_fees)),
                        ])
                    ),
                    OutputFormat::Json => display::print_json(&json!({
                        "account_id": account_id,
                        "dex_id": dex_id,
                        "amount": display::amount_json(
                            community_fees.as_yoctonear(),
                            Some(&AssetMetadata::near())
                        ),
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
#[interactive_clap(output_context = WithdrawContext)]
pub struct Withdraw {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account signs? Anyone can send a community account its fees
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// Which community account gets its fees?
    account_id: near_cli_rs::types::account_id::AccountId,
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
}

#[derive(Clone)]
pub struct WithdrawContext(ActionContext);

impl WithdrawContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Withdraw as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let account_id: AccountId = scope.account_id.clone().into();
        let dex_id = previous_context.dex_id;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                // The fees go to the account's wallet
                preflight.ensure_account_exists(&account_id)?;
                let community_fees = xyk::community_owned_fees(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                    &account_id,
                )?;
                if community_fees.is_zero() {
                    return Err(PreflightError::NoCommunityFees {
                        account_id: account_id.clone(),
                        dex_id: dex_id.clone(),
                    }
                    .into_report());
                }
                let community_fees_label = format_near(community_fees);
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: vec![format!(
                        "Send the {community_fees_label} of community fees that {account_id} collected on {dex_id} to its wallet"
                    )],
                    function_calls: vec![FunctionCall {
                        method_name: "execute_operations",
                        args: json!({
                            "operations": [planning::dex_call_operation(
                                &dex_id,
                                "withdraw_community_fee",
                                &WithdrawCommunityFeeArgs {
                                    account_id: account_id.clone(),
                                },
                                &[],
                            )?],
                        }),
                        deposit: ONE_YOCTO_NEAR,
                        gas: WITHDRAW_COMMUNITY_FEE_GAS,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "community fee withdrawal",
                        success_message: format!(
                            "Sent {community_fees_label} of community fees from {dex_id} to {account_id}'s wallet"
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
