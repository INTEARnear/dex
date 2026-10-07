pub mod account;
pub mod asset_metadata;
pub mod custody;
pub mod engine;
pub mod fungible_token;
pub mod networks;
pub mod protocol;
pub mod xyk;

use color_eyre::Section;
use color_eyre::eyre::{Context, Report, eyre};
use intear_dex_types::DexId;
use near_cli_rs::common::JsonRpcClientExt;
use near_cli_rs::config::NetworkConfig;
use near_jsonrpc_client::methods::query::RpcQueryRequest;
use near_jsonrpc_primitives::types::query::{QueryResponseKind, RpcQueryError};
use near_primitives::errors::{FunctionCallError, MethodResolveError};
use near_primitives::types::{AccountId, BlockReference, FunctionArgs};
use near_primitives::views::QueryRequest;

use crate::errors::{self, FailureContext};

#[derive(Debug, thiserror::Error)]
#[error("{contract_id} failed in {method_name} on network {network_name}: {panic_message}")]
pub struct ViewPanicked {
    pub contract_id: AccountId,
    pub method_name: String,
    pub network_name: String,
    pub panic_message: String,
}

/// The error of a view that panicked, with a hint when the message is known
pub fn view_panicked_report(view_panicked: ViewPanicked, dex_id: Option<&DexId>) -> Report {
    let hint = errors::contract_panic_hint(&FailureContext {
        panic_message: &view_panicked.panic_message,
        signer_and_connection: None,
        dex_id,
    });
    let report = Report::new(view_panicked);
    match hint {
        Some(hint) => report.suggestion(hint),
        None => report,
    }
}

/// The result of a view function, or `None` when the contract has no such
/// method or the account has no contract.
pub fn view_function_result(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    contract_id: &AccountId,
    method_name: &str,
    args: Vec<u8>,
) -> color_eyre::eyre::Result<Option<Vec<u8>>> {
    let response = network_config
        .json_rpc_client()
        .blocking_call(RpcQueryRequest {
            block_reference: block_reference.clone(),
            request: QueryRequest::CallFunction {
                account_id: contract_id.clone(),
                method_name: method_name.to_string(),
                args: FunctionArgs::from(args),
            },
        });
    match response {
        Ok(response) => match response.kind {
            QueryResponseKind::CallResult(call_result) => Ok(Some(call_result.result)),
            _ => Err(eyre!(
                "RPC {} answered the view {method_name} on {contract_id} with something other than a call result",
                network_config.rpc_url
            )),
        },
        Err(error)
            if error
                .handler_error()
                .is_some_and(is_missing_method_or_contract) =>
        {
            Ok(None)
        }
        Err(error) => {
            if let Some(RpcQueryError::ContractExecutionError { vm_error, .. }) =
                error.handler_error()
                && let Some(panic_message) = errors::panic_message_in_error_text(vm_error)
            {
                return Err(view_panicked_report(
                    ViewPanicked {
                        contract_id: contract_id.clone(),
                        method_name: method_name.to_string(),
                        network_name: network_config.network_name.clone(),
                        panic_message,
                    },
                    None,
                ));
            }
            Err(color_eyre::eyre::Report::new(error)).wrap_err_with(|| {
                format!(
                    "The view {method_name} on {contract_id} failed (network {}, RPC {})",
                    network_config.network_name, network_config.rpc_url
                )
            })
        }
    }
}

fn is_missing_method_or_contract(error: &RpcQueryError) -> bool {
    match error {
        RpcQueryError::ContractExecutionError {
            error: FunctionCallError::MethodResolveError(MethodResolveError::MethodNotFound),
            ..
        }
        | RpcQueryError::NoContractCode { .. } => true,
        // Nodes that answer in the legacy error format only give the message
        RpcQueryError::ContractExecutionError { vm_error, .. } => {
            vm_error.contains("MethodResolveError(MethodNotFound)")
                || vm_error.contains("CompilationError(CodeDoesNotExist")
        }
        _ => false,
    }
}

pub fn view_json<ViewResult: serde::de::DeserializeOwned>(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    contract_id: &AccountId,
    method_name: &str,
    args: serde_json::Value,
) -> color_eyre::eyre::Result<ViewResult> {
    let result = view_function_result(
        network_config,
        block_reference,
        contract_id,
        method_name,
        serde_json::to_vec(&args)?,
    )?
    .ok_or_else(|| {
        eyre!(
            "{contract_id} has no method {method_name} on network {} ({}). Its contract may be older than this tool.",
            network_config.network_name,
            network_config.rpc_url
        )
    })?;
    serde_json::from_slice(&result).wrap_err_with(|| {
        format!(
            "Unexpected result of {method_name} on {contract_id}: {}",
            String::from_utf8_lossy(&result)
        )
    })
}
