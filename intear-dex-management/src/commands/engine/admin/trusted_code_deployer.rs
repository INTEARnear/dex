use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use serde_json::json;

use crate::chain::engine;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::errors::PreflightError;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan};

const SET_TRUSTED_CODE_DEPLOYER_GAS: Gas = Gas::from_teragas(10);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = SetTrustedCodeDeployerContext)]
pub struct SetTrustedCodeDeployer {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account signs? Only the current trusted code deployer can
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The account that deploys dex code from now on
    new_trusted_code_deployer: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl SetTrustedCodeDeployer {
    fn input_signer_account_id(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Which account signs? Only the current trusted code deployer can",
        )
    }

    fn input_new_trusted_code_deployer(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_non_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Which account deploys dex code from now on?",
        )
    }
}

#[derive(Clone)]
pub struct SetTrustedCodeDeployerContext(ActionContext);

impl SetTrustedCodeDeployerContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<SetTrustedCodeDeployer as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let signer_id: AccountId = scope.signer_account_id.clone().into();
        let new_trusted_code_deployer: AccountId = scope.new_trusted_code_deployer.clone().into();
        Ok(Self(planning::write_action_context(
            &previous_context,
            signer_id,
            move |preflight| {
                let trusted_code_deployer = engine::trusted_code_deployer(
                    &preflight.network_config,
                    &preflight.block_reference,
                )?;
                if preflight.signer_id != trusted_code_deployer {
                    return Err(PreflightError::CantHandOverCodeDeployment {
                        signer_id: preflight.signer_id.clone(),
                        trusted_code_deployer,
                    }
                    .into_report());
                }
                if new_trusted_code_deployer == trusted_code_deployer {
                    return Err(PreflightError::AlreadyTrustedCodeDeployer {
                        account_id: trusted_code_deployer,
                    }
                    .into_report());
                }
                preflight.ensure_account_exists(&new_trusted_code_deployer)?;
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: vec![format!(
                        "Make {new_trusted_code_deployer} the trusted code deployer of {ENGINE_ACCOUNT_ID}, the only account that can deploy dex code; {trusted_code_deployer} no longer can"
                    )],
                    function_calls: vec![FunctionCall {
                        method_name: "set_trusted_code_deployer",
                        args: json!({ "account_id": new_trusted_code_deployer }),
                        deposit: ONE_YOCTO_NEAR,
                        gas: SET_TRUSTED_CODE_DEPLOYER_GAS,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "change of the trusted code deployer",
                        success_message: format!(
                            "{new_trusted_code_deployer} is now the trusted code deployer of {ENGINE_ACCOUNT_ID}"
                        ),
                        transfer_call: None,
                        dex_id: None,
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<SetTrustedCodeDeployerContext> for ActionContext {
    fn from(item: SetTrustedCodeDeployerContext) -> Self {
        item.0
    }
}
