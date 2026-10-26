use std::str::FromStr;

use near_cli_rs::commands::ActionContext;
use near_primitives::types::AccountId;
use near_sdk::PublicKey;
use otc_dex_types::SetAuthorizedKeyArgs;

use super::OtcContext;
use crate::chain::otc;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::errors::PreflightError;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, TransactionPlan, otc_storage};

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = OtcContext)]
#[interactive_clap(output_context = SetAuthorizedKeyContext)]
pub struct SetAuthorizedKey {
    #[interactive_clap(skip_default_input_arg)]
    /// Whose trade intents does the key authorize?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The key that signs the signer's trade intents from now on, e.g. 'ed25519:…'
    key: near_cli_rs::types::public_key::PublicKey,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl SetAuthorizedKey {
    fn input_signer_account_id(
        context: &OtcContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Whose trade intents does the key authorize?",
        )
    }

    fn input_key(
        _context: &OtcContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::public_key::PublicKey>> {
        match inquire::CustomType::new(
            "Which key signs the signer's trade intents from now on? e.g. ed25519:…",
        )
        .with_error_message("That isn't a public key like ed25519:…")
        .prompt()
        {
            Ok(key) => Ok(Some(key)),
            Err(
                inquire::error::InquireError::OperationCanceled
                | inquire::error::InquireError::OperationInterrupted,
            ) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Clone)]
pub struct SetAuthorizedKeyContext(ActionContext);

impl SetAuthorizedKeyContext {
    pub fn from_previous_context(
        previous_context: OtcContext,
        scope: &<SetAuthorizedKey as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let key = PublicKey::from_str(&scope.key.to_string()).map_err(|error| {
            color_eyre::eyre::eyre!("Invalid authorized key {}: {error:?}", scope.key)
        })?;
        let dex_id = previous_context.dex_id;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                preflight.ensure_dex_exists(&dex_id)?;
                let signer_id: AccountId = preflight.signer_id.clone();
                let current_key = otc::authorized_key(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                    &signer_id,
                )?;
                // Replacing a key only stores more when the new one is longer
                let new_bytes = match &current_key {
                    Some(current_key) if *current_key == key => {
                        return Err(PreflightError::OtcAuthorizedKeyUnchanged {
                            account_id: signer_id,
                            dex_id: dex_id.clone(),
                        }
                        .into_report());
                    }
                    Some(current_key) => u64::try_from(
                        near_sdk::borsh::to_vec(&key)?
                            .len()
                            .saturating_sub(near_sdk::borsh::to_vec(current_key)?.len()),
                    )?,
                    None => otc_storage::authorized_key_bytes(
                        &dex_id,
                        &signer_id,
                        &key,
                        preflight.protocol_limits.extra_bytes_per_record,
                    )?,
                };
                let action_step = match &current_key {
                    Some(current_key) => format!(
                        "Authorize {} to sign {signer_id}'s trade intents on {dex_id}, instead of {}",
                        String::from(&key),
                        String::from(current_key)
                    ),
                    None => format!(
                        "Authorize {} to sign {signer_id}'s trade intents on {dex_id}",
                        String::from(&key)
                    ),
                };
                let (steps, function_calls) = super::otc_call_paying_for_storage(
                    preflight,
                    &dex_id,
                    action_step,
                    (
                        "set_authorized_key",
                        &SetAuthorizedKeyArgs { key: key.clone() },
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
                        action_name: "authorized key change",
                        success_message: format!(
                            "{} now signs {signer_id}'s trade intents on {dex_id}",
                            String::from(&key)
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

impl From<SetAuthorizedKeyContext> for ActionContext {
    fn from(item: SetAuthorizedKeyContext) -> Self {
        item.0
    }
}
