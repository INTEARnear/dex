use std::collections::BTreeMap;
use std::sync::Mutex;

use color_eyre::Section;
use color_eyre::eyre::{Context, Report, eyre};
use intear_dex_types::{
    AccountOrDexId, AssetId, BalanceUpdate, DexEventData, DexId, IntearDexEvent,
};
use near_cli_rs::config::NetworkConfig;
use near_primitives::errors::{ActionErrorKind, FunctionCallError, TxExecutionError};
use near_primitives::types::{AccountId, BlockReference, Finality};
use near_primitives::views::{
    ExecutionStatusView, FinalExecutionOutcomeView, FinalExecutionStatus,
};
use near_sdk::base64::{Engine, prelude::BASE64_STANDARD};
use near_sdk::json_types::{Base58CryptoHash, U128};
use serde_json::json;
use xyk_dex_types::{FeePayoutType, XykDexEvent, XykDexEventV1};

use crate::chain::asset_metadata::{AssetMetadata, AssetMetadataCache};
use crate::chain::xyk::{self, XykState};
use crate::commands::xyk::presentation::pool_assets;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::{self, FailureContext};

/// What a command expects its transaction to do, to compare with what happened
pub struct ExpectedOutcome {
    /// The main action, for failure messages, e.g. "withdrawal"
    pub action_name: &'static str,
    pub success_message: String,
    pub transfer_call: Option<ExpectedTransferCall>,
    /// The dex the transaction calls, for hints about its failures
    pub dex_id: Option<DexId>,
    pub deployment: Option<ExpectedDeployment>,
}

/// The engine reports the hash of the code it stored, which has to be the
/// hash of the code that was sent
pub struct ExpectedDeployment {
    pub dex_id: DexId,
    pub code_hash: Base58CryptoHash,
    /// What to run once the code is deployed
    pub next_step: Option<String>,
    /// For an upgrade that keeps the state as it is: the command that
    /// deploys the code again with a migration, for when the new code can't
    /// read the state
    pub migrating_redeploy_command: Option<String>,
}

/// New code that can't read the dex's state fails every call to the dex
#[derive(Debug, thiserror::Error)]
#[error("The new code of {dex_id} can't read its state, so every call to the dex fails")]
pub struct DeployedCodeCantReadState {
    dex_id: DexId,
}

/// ft_transfer_call returns how much the receiver kept, and the token contract
/// sends the rest back to the sender
pub struct ExpectedTransferCall {
    pub token_id: AccountId,
    pub sender_id: AccountId,
    pub amount: u128,
    pub metadata: Option<AssetMetadata>,
}

/// near-cli-rs reports a failed top-level call as text, without the contract
/// or the transaction; this is what it leaves out
#[derive(Clone)]
pub struct SentTransaction {
    pub signer_id: AccountId,
    pub receiver_id: AccountId,
    pub connection_name: String,
    pub action_name: &'static str,
    pub dex_id: Option<DexId>,
    pub url: String,
}

static SENT_TRANSACTION: Mutex<Option<SentTransaction>> = Mutex::new(None);

pub fn record_sent_transaction(sent_transaction: SentTransaction) -> color_eyre::eyre::Result<()> {
    let mut recorded = SENT_TRANSACTION
        .lock()
        .map_err(|_| eyre!("The record of the sent transaction is unusable"))?;
    *recorded = Some(sent_transaction);
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TransactionFailedOnChain(String);

/// Names the contract that refused a failed top-level call, adds the hint for
/// its panic message and links the transaction
pub fn explain_top_level_failure(error: Report) -> Report {
    if error.downcast_ref::<TransactionFailedOnChain>().is_some() {
        return error;
    }
    let Some(sent_transaction) = SENT_TRANSACTION
        .lock()
        .ok()
        .and_then(|recorded| recorded.clone())
    else {
        return error;
    };
    let Some(panic_message) = errors::panic_message_in_error_text(&error.to_string()) else {
        return error;
    };
    let hint = errors::contract_panic_hint(&FailureContext {
        panic_message: &panic_message,
        signer_and_connection: Some((
            &sent_transaction.signer_id,
            &sent_transaction.connection_name,
        )),
        dex_id: sent_transaction.dex_id.as_ref(),
    });
    let report = Report::new(TransactionFailedOnChain(format!(
        "The {} failed on-chain: {} refused it: {panic_message}",
        sent_transaction.action_name, sent_transaction.receiver_id
    )))
    .note(format!("Transaction: {}", sent_transaction.url));
    match hint {
        Some(hint) => report.suggestion(hint),
        None => report,
    }
}

struct ReceiptFailure {
    contract_id: AccountId,
    message: String,
}

fn failure_message(error: &TxExecutionError) -> String {
    match error {
        TxExecutionError::ActionError(action_error) => match &action_error.kind {
            ActionErrorKind::FunctionCallError(FunctionCallError::ExecutionError(message)) => {
                errors::panic_message_of_execution_error(message)
            }
            _ => action_error.to_string(),
        },
        TxExecutionError::InvalidTxError(invalid_transaction_error) => {
            invalid_transaction_error.to_string()
        }
    }
}

/// The account a failed withdrawal went back to, from the engine's log
/// "Refunding to bob.near (recipient) because …" or "Refunding to
/// Account(alice.near) (rescue) because …"
fn withdrawal_refund_target(log: &str) -> Option<String> {
    let (target, _) = log.strip_prefix("Refunding to ")?.split_once(" (")?;
    Some(
        target
            .trim_start_matches("Account(")
            .trim_start_matches("Dex(")
            .trim_end_matches(')')
            .to_string(),
    )
}

pub fn report_outcome(
    outcome: &FinalExecutionOutcomeView,
    network_config: &NetworkConfig,
    expected_outcome: &ExpectedOutcome,
    sent_transaction: &SentTransaction,
    output_format: OutputFormat,
    verbosity: near_cli_rs::Verbosity,
) -> color_eyre::eyre::Result<()> {
    let receipt_failures = outcome
        .receipts_outcome
        .iter()
        .filter_map(|receipt| match &receipt.outcome.status {
            ExecutionStatusView::Failure(error) => Some(ReceiptFailure {
                contract_id: receipt.outcome.executor_id.clone(),
                message: failure_message(error),
            }),
            ExecutionStatusView::Unknown
            | ExecutionStatusView::SuccessValue(_)
            | ExecutionStatusView::SuccessReceiptId(_) => None,
        })
        .collect::<Vec<_>>();
    let engine_logs = outcome
        .receipts_outcome
        .iter()
        .filter(|receipt| receipt.outcome.executor_id == ENGINE_ACCOUNT_ID)
        .flat_map(|receipt| receipt.outcome.logs.iter())
        .collect::<Vec<_>>();
    let withdrawal_refund_targets = engine_logs
        .iter()
        .filter_map(|log| withdrawal_refund_target(log))
        .collect::<Vec<_>>();
    let returned_by_transfer_call = match &expected_outcome.transfer_call {
        Some(expected_transfer_call) => {
            let FinalExecutionStatus::SuccessValue(returned_value) = &outcome.status else {
                return Err(eyre!(
                    "ft_transfer_call ended with {:?} instead of the amount it kept",
                    outcome.status
                ));
            };
            let U128(kept_amount) = serde_json::from_slice(returned_value)
                .wrap_err("ft_transfer_call returned something other than the amount it kept")?;
            expected_transfer_call
                .amount
                .checked_sub(kept_amount)
                .filter(|returned_amount| *returned_amount > 0)
                .map(|returned_amount| (expected_transfer_call, returned_amount))
        }
        None => None,
    };
    let mut event_values = Vec::new();
    let mut events = Vec::new();
    for log in &engine_logs {
        let Some(event_json) = log.strip_prefix("EVENT_JSON:") else {
            continue;
        };
        let event_value: serde_json::Value = serde_json::from_str(event_json)
            .wrap_err_with(|| format!("{ENGINE_ACCOUNT_ID} logged an invalid event: {log}"))?;
        if event_value["standard"] != "inteardex" {
            continue;
        }
        let event: IntearDexEvent = serde_json::from_value(event_value.clone()).wrap_err_with(|| {
            format!(
                "This version of intear-dex-management can't read the event {event_json} of {ENGINE_ACCOUNT_ID}"
            )
        })?;
        event_values.push(event_value);
        events.push(event);
    }

    let metadata_cache =
        AssetMetadataCache::new(network_config, BlockReference::Finality(Finality::Final));
    let mut latest_balances: BTreeMap<(String, String), (AccountId, AssetId, u128)> =
        BTreeMap::new();
    let mut activity_lines = Vec::new();
    let mut deployed_code_hashes = Vec::new();
    for event in events {
        match event {
            IntearDexEvent::BalanceUpdates(balance_updates) => {
                for BalanceUpdate {
                    owner,
                    asset_id,
                    balance: U128(balance),
                } in balance_updates
                {
                    if let AccountOrDexId::Account(account_id) = owner {
                        latest_balances.insert(
                            (account_id.to_string(), asset_id.to_string()),
                            (account_id, asset_id, balance),
                        );
                    }
                }
            }
            // Logged one per event by engines from before dexes logged borsh, and by
            // this one for dexes that still log JSON
            IntearDexEvent::DexEvent { dex_id, event, .. } if event["standard"] == "xyk" => {
                let xyk_event: XykDexEventV1 =
                    serde_json::from_value(event.clone()).wrap_err_with(|| {
                        format!(
                            "This version of intear-dex-management can't read the event {event} of {dex_id}"
                        )
                    })?;
                activity_lines.push(xyk_event_line(&dex_id, xyk_event.into(), &metadata_cache)?);
            }
            IntearDexEvent::DexEventsBorsh(encoded_dex_events) => {
                let dex_events: Vec<DexEventData> =
                    near_sdk::borsh::from_slice(&decompress_dex_events(&encoded_dex_events)?)
                        .wrap_err_with(|| {
                            format!("{ENGINE_ACCOUNT_ID} logged invalid dex events")
                        })?;
                for DexEventData { dex_id, event, .. } in dex_events {
                    // Only the xyk dex logs through the engine's `dex_event`
                    let xyk_event: XykDexEvent = near_sdk::borsh::from_slice(&event).wrap_err_with(|| {
                        format!(
                            "This version of intear-dex-management can't read an event of {dex_id}"
                        )
                    })?;
                    activity_lines.push(xyk_event_line(&dex_id, xyk_event, &metadata_cache)?);
                }
            }
            // Dexes send storage NEAR they didn't use, refunds and removed
            // liquidity to wallets
            IntearDexEvent::Withdraw {
                from: AccountOrDexId::Dex(dex_id),
                to,
                asset_id,
                amount: U128(amount),
            } => {
                let metadata = metadata_cache.get(&asset_id)?;
                activity_lines.push(format!(
                    "{dex_id} sent {} to {to}'s wallet",
                    display::format_amount(amount, metadata.as_ref())
                ));
            }
            IntearDexEvent::DexDeployed { dex_id, code_hash } => {
                deployed_code_hashes.push((dex_id, code_hash));
            }
            #[allow(deprecated)]
            IntearDexEvent::UserBalanceUpdate {
                account_id,
                asset_id,
                balance: U128(balance),
            } => {
                latest_balances.insert(
                    (account_id.to_string(), asset_id.to_string()),
                    (account_id, asset_id, balance),
                );
            }
            #[allow(deprecated)]
            IntearDexEvent::DexEvent { .. }
            | IntearDexEvent::UserDeposit { .. }
            | IntearDexEvent::Withdraw { .. }
            | IntearDexEvent::Swap { .. }
            | IntearDexEvent::DexBalanceUpdate { .. } => {}
        }
    }
    let mut balance_lines = Vec::new();
    for (account_id, asset_id, balance) in latest_balances.into_values() {
        let metadata = metadata_cache.get(&asset_id)?;
        balance_lines.push(format!(
            "{account_id} now has {} on {ENGINE_ACCOUNT_ID}",
            display::format_amount(balance, metadata.as_ref())
        ));
    }
    let mut failure_lines = Vec::new();
    for failure in &receipt_failures {
        failure_lines.push(format!(
            "{} failed: {}",
            failure.contract_id, failure.message
        ));
    }
    for refund_target in &withdrawal_refund_targets {
        failure_lines.push(format!(
            "{ENGINE_ACCOUNT_ID} put the withdrawal back into {refund_target}'s balance"
        ));
    }
    if let Some(expected_deployment) = &expected_outcome.deployment {
        let deployed_code_hash = deployed_code_hashes
            .iter()
            .find(|(dex_id, _)| *dex_id == expected_deployment.dex_id)
            .map(|(_, code_hash)| *code_hash);
        match deployed_code_hash {
            Some(code_hash) if code_hash == expected_deployment.code_hash => {}
            Some(code_hash) => failure_lines.push(format!(
                "{ENGINE_ACCOUNT_ID} stored code with hash {} for {}, but the code sent has hash {}",
                String::from(&code_hash),
                expected_deployment.dex_id,
                String::from(&expected_deployment.code_hash)
            )),
            // A failed deploy is among the receipt failures
            None if receipt_failures.is_empty() => failure_lines.push(format!(
                "{ENGINE_ACCOUNT_ID} didn't report deploying {}",
                expected_deployment.dex_id
            )),
            None => {}
        }
    }
    if let Some((expected_transfer_call, returned_amount)) = returned_by_transfer_call {
        failure_lines.push(format!(
            "{} sent {} back to {}",
            expected_transfer_call.token_id,
            display::format_amount(returned_amount, expected_transfer_call.metadata.as_ref()),
            expected_transfer_call.sender_id
        ));
    }
    let unreadable_state_after_deploy = match &expected_outcome.deployment {
        Some(ExpectedDeployment {
            dex_id,
            migrating_redeploy_command: Some(migrating_redeploy_command),
            ..
        }) if failure_lines.is_empty() => {
            match xyk::state(
                network_config,
                &BlockReference::Finality(Finality::Final),
                dex_id,
            )? {
                XykState::Unreadable => Some((
                    DeployedCodeCantReadState {
                        dex_id: dex_id.clone(),
                    },
                    migrating_redeploy_command,
                )),
                XykState::Readable | XykState::Missing => None,
            }
        }
        _ => None,
    };

    if let OutputFormat::Json = output_format {
        // In quiet mode near-cli-rs prints the returned value without a newline
        if let (near_cli_rs::Verbosity::Quiet, FinalExecutionStatus::SuccessValue(returned_value)) =
            (verbosity, &outcome.status)
            && !returned_value.is_empty()
        {
            println!();
        }
        println!(
            "{}",
            serde_json::to_string(&json!({
                "transaction_hash": outcome.transaction_outcome.id.to_string(),
                "status": if failure_lines.is_empty() && unreadable_state_after_deploy.is_none() {
                    "succeeded"
                } else {
                    "failed"
                },
                "message": match &unreadable_state_after_deploy {
                    _ if !failure_lines.is_empty() => failure_lines.join("\n"),
                    Some((unreadable_state, _)) => unreadable_state.to_string(),
                    None => expected_outcome.success_message.clone(),
                },
                "events": event_values,
            }))?
        );
    }

    if !failure_lines.is_empty() {
        let mut lines = vec![format!(
            "The {} failed on-chain",
            expected_outcome.action_name
        )];
        lines.extend(failure_lines);
        lines.extend(activity_lines);
        lines.extend(balance_lines);
        let hint = receipt_failures.iter().find_map(|failure| {
            errors::contract_panic_hint(&FailureContext {
                panic_message: &failure.message,
                signer_and_connection: Some((
                    &sent_transaction.signer_id,
                    &sent_transaction.connection_name,
                )),
                dex_id: sent_transaction.dex_id.as_ref(),
            })
        });
        let report = Report::new(TransactionFailedOnChain(lines.join("\n")))
            .note(format!("Transaction: {}", sent_transaction.url));
        return Err(match hint {
            Some(hint) => report.suggestion(hint),
            None => report,
        });
    }

    let mut lines = vec![format!("✓ {}", expected_outcome.success_message)];
    lines.extend(activity_lines);
    lines.extend(balance_lines);
    if let Some(next_step) = expected_outcome
        .deployment
        .as_ref()
        .and_then(|expected_deployment| expected_deployment.next_step.clone())
    {
        lines.push(next_step);
    }
    tracing::info!("{}", lines.join("\n│  "));
    if let Some((unreadable_state, migrating_redeploy_command)) = unreadable_state_after_deploy {
        return Err(Report::new(unreadable_state).suggestion(format!(
            "Deploy the code again with --migrate, which converts the state in the same receipt:\n    {migrating_redeploy_command}"
        )));
    }
    Ok(())
}

/// The uncompressed bytes of a `dex_events` or `dex_events_borsh` log
fn decompress_dex_events(encoded_dex_events: &str) -> color_eyre::eyre::Result<Vec<u8>> {
    BASE64_STANDARD
        .decode(encoded_dex_events)
        .ok()
        .and_then(|compressed| lz4_flex::decompress_size_prepended(&compressed).ok())
        .ok_or_else(|| {
            eyre!("{ENGINE_ACCOUNT_ID} logged invalid compressed dex events: {encoded_dex_events}")
        })
}

/// What an xyk event says happened, in one line
fn xyk_event_line(
    dex_id: &DexId,
    event: XykDexEvent,
    metadata_cache: &AssetMetadataCache,
) -> color_eyre::eyre::Result<String> {
    let format_amount = |raw: u128, asset_id: &AssetId| -> color_eyre::eyre::Result<String> {
        Ok(display::format_amount(
            raw,
            metadata_cache.get(asset_id)?.as_ref(),
        ))
    };
    Ok(match event {
        XykDexEvent::PoolUpdated { pool_id, pool } => {
            let [(asset_id_0, reserve_0), (asset_id_1, reserve_1)] = pool_assets(&pool);
            format!(
                "Pool #{pool_id} of {dex_id} now holds {} and {}",
                format_amount(reserve_0, &asset_id_0)?,
                format_amount(reserve_1, &asset_id_1)?
            )
        }
        XykDexEvent::Swap {
            pool_id,
            asset_in,
            asset_out,
            amount_in: U128(amount_in),
            amount_out: U128(amount_out),
            ..
        } => format!(
            "Pool #{pool_id} of {dex_id} swapped {} for {}",
            format_amount(amount_in, &asset_in)?,
            format_amount(amount_out, &asset_out)?
        ),
        XykDexEvent::LiquidityAdded {
            pool_id,
            asset_0,
            asset_1,
            added_amount_0: U128(added_amount_0),
            added_amount_1: U128(added_amount_1),
            minted_shares: U128(minted_shares),
            new_owned_shares: U128(new_owned_shares),
            new_total_shares: U128(new_total_shares),
            ..
        } => {
            let added = format!(
                "Added {} and {} to pool #{pool_id} of {dex_id}",
                format_amount(added_amount_0, &asset_0)?,
                format_amount(added_amount_1, &asset_1)?
            );
            // Only public pools have shares
            if minted_shares == 0 {
                added
            } else {
                format!(
                    "{added} for {minted_shares} shares, {} of the pool",
                    display::format_share(new_owned_shares, new_total_shares)?
                )
            }
        }
        XykDexEvent::LiquidityRemoved {
            pool_id,
            asset_0,
            asset_1,
            removed_amount_0: U128(removed_amount_0),
            removed_amount_1: U128(removed_amount_1),
            burned_shares: U128(burned_shares),
            ..
        } => {
            let removed = format!(
                "Removed {} and {} from pool #{pool_id} of {dex_id}",
                format_amount(removed_amount_0, &asset_0)?,
                format_amount(removed_amount_1, &asset_1)?
            );
            if burned_shares == 0 {
                removed
            } else {
                format!("{removed} for {burned_shares} shares")
            }
        }
        XykDexEvent::LiquidityFeePayoutSet {
            pool_id,
            account_id,
            payout_type,
        } => {
            let (verb, asset) = match payout_type {
                FeePayoutType::KeepBoth => ("keeps", ""),
                FeePayoutType::ConvertToFirstAsset => ("takes", " in its first asset"),
                FeePayoutType::ConvertToSecondAsset => ("takes", " in its second asset"),
            };
            format!(
                "{account_id} now {verb} its part of the fees of pool #{pool_id} of {dex_id}{asset}"
            )
        }
        XykDexEvent::LiquidityFeesCollected {
            pool_id,
            account_id,
            asset_0,
            asset_1,
            amount_0: U128(amount_0),
            amount_1: U128(amount_1),
        } => format!(
            "{account_id} collected {} and {} of fees from pool #{pool_id} of {dex_id}",
            format_amount(amount_0, &asset_0)?,
            format_amount(amount_1, &asset_1)?
        ),
    })
}
