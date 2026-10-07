use color_eyre::eyre::{Context, eyre};
use near_cli_rs::common::JsonRpcClientExt;
use near_cli_rs::config::NetworkConfig;
use near_jsonrpc_client::methods::query::RpcQueryRequest;
use near_jsonrpc_primitives::types::query::{QueryResponseKind, RpcQueryError};
use near_primitives::types::{AccountId, BlockReference};
use near_primitives::views::{AccountView, QueryRequest};

/// `None` when the account doesn't exist
pub fn view_account(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    account_id: &AccountId,
) -> color_eyre::eyre::Result<Option<AccountView>> {
    let response = network_config
        .json_rpc_client()
        .blocking_call(RpcQueryRequest {
            block_reference: block_reference.clone(),
            request: QueryRequest::ViewAccount {
                account_id: account_id.clone(),
            },
        });
    match response {
        Ok(response) => match response.kind {
            QueryResponseKind::ViewAccount(account_view) => Ok(Some(account_view)),
            _ => Err(eyre!(
                "RPC {} answered the account lookup of {account_id} with something other than an account",
                network_config.rpc_url
            )),
        },
        Err(error)
            if matches!(
                error.handler_error(),
                Some(RpcQueryError::UnknownAccount { .. })
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(color_eyre::eyre::Report::new(error)).wrap_err_with(|| {
            format!(
                "Couldn't look up {account_id} on network {} (RPC {})",
                network_config.network_name, network_config.rpc_url
            )
        }),
    }
}
