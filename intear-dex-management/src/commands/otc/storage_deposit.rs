use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;

use super::OtcContext;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::errors::PreflightError;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, TransactionPlan, format_near, otc_storage, storage};

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = OtcContext)]
#[interactive_clap(output_context = StorageDepositContext)]
pub struct StorageDeposit {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account pays for its records on the dex?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    /// How much NEAR, e.g. '0.01 NEAR'
    amount: near_cli_rs::types::near_token::NearToken,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl StorageDeposit {
    fn input_signer_account_id(
        context: &OtcContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account pays for its records on the dex?",
        )
    }
}

#[derive(Clone)]
pub struct StorageDepositContext(ActionContext);

impl StorageDepositContext {
    pub fn from_previous_context(
        previous_context: OtcContext,
        scope: &<StorageDeposit as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let signer_id: AccountId = scope.signer_account_id.clone().into();
        let near_cli_rs::types::near_token::NearToken(deposit) = scope.amount;
        let dex_id = previous_context.dex_id;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                preflight.ensure_dex_exists(&dex_id)?;
                if deposit.is_zero() {
                    return Err(PreflightError::ZeroAmount.into_report());
                }
                let has_storage_balance = crate::chain::otc::storage_balance_of(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                    &preflight.signer_id,
                )?
                .is_some();
                // The first deposit pays for the storage balance's own record
                if !has_storage_balance {
                    let minimum = storage::storage_cost(
                        otc_storage::storage_balance_bytes(
                            &dex_id,
                            &preflight.signer_id,
                            preflight.protocol_limits.extra_bytes_per_record,
                        )?,
                        preflight.protocol_limits.storage_byte_cost,
                    )?;
                    if deposit < minimum {
                        return Err(PreflightError::OtcStorageDepositTooSmall {
                            dex_id: dex_id.clone(),
                            minimum: format_near(minimum),
                        }
                        .into_report());
                    }
                }
                let (steps, function_calls) = super::with_storage_deposit(
                    preflight,
                    &dex_id,
                    deposit,
                    "the records it stores for its trades",
                    &[],
                    Vec::new(),
                    Gas::from_gas(0),
                )?;
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "storage deposit",
                        success_message: format!(
                            "Deposited {} into {}'s storage balance on {dex_id}",
                            format_near(deposit),
                            preflight.signer_id
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

impl From<StorageDepositContext> for ActionContext {
    fn from(item: StorageDepositContext) -> Self {
        item.0
    }
}
