use intear_dex_types::CAN_PAUSE;
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use serde_json::json;

use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::errors::PreflightError;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan};

const PAUSE_GAS: Gas = Gas::from_teragas(10);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = PauseContext)]
pub struct Pause {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account pauses dex.intear.near?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Pause {
    fn input_signer_account_id(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Which account pauses dex.intear.near?",
        )
    }
}

#[derive(Clone)]
pub struct PauseContext(ActionContext);

impl PauseContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Pause as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self(pause_action_context(
            &previous_context,
            scope.signer_account_id.clone().into(),
            true,
        )))
    }
}

impl From<PauseContext> for ActionContext {
    fn from(item: PauseContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = UnpauseContext)]
pub struct Unpause {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account unpauses dex.intear.near?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Unpause {
    fn input_signer_account_id(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Which account unpauses dex.intear.near?",
        )
    }
}

#[derive(Clone)]
pub struct UnpauseContext(ActionContext);

impl UnpauseContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Unpause as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self(pause_action_context(
            &previous_context,
            scope.signer_account_id.clone().into(),
            false,
        )))
    }
}

impl From<UnpauseContext> for ActionContext {
    fn from(item: UnpauseContext) -> Self {
        item.0
    }
}

fn pause_action_context(
    global_context: &crate::GlobalContext,
    signer_id: AccountId,
    pause: bool,
) -> ActionContext {
    planning::write_action_context(global_context, signer_id, move |preflight| {
        if !CAN_PAUSE.contains(&preflight.signer_id.as_str()) {
            return Err(PreflightError::NotAllowedToPause {
                signer_id: preflight.signer_id.clone(),
            }
            .into_report());
        }
        match (pause, preflight.engine_paused) {
            (true, true) => return Err(PreflightError::AlreadyPaused.into_report()),
            (false, false) => return Err(PreflightError::NotPaused.into_report()),
            (true, false) | (false, true) => {}
        }
        let (method_name, step, success_message) = if pause {
            (
                "pause",
                format!(
                    "Pause {ENGINE_ACCOUNT_ID}: deposits, withdrawals, swaps and dex calls stop until it's unpaused"
                ),
                format!("Paused {ENGINE_ACCOUNT_ID}"),
            )
        } else {
            (
                "unpause",
                format!("Unpause {ENGINE_ACCOUNT_ID}"),
                format!("Unpaused {ENGINE_ACCOUNT_ID}"),
            )
        };
        Ok(TransactionPlan {
            receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
            steps: vec![step],
            function_calls: vec![FunctionCall {
                method_name,
                args: json!({}),
                deposit: ONE_YOCTO_NEAR,
                gas: PAUSE_GAS,
            }],
            expected_outcome: ExpectedOutcome {
                action_name: method_name,
                success_message,
                transfer_call: None,
                dex_id: None,
                deployment: None,
            },
        })
    })
}
