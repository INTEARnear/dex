use color_eyre::eyre::Context;
use near_cli_rs::common::JsonRpcClientExt;
use near_cli_rs::config::NetworkConfig;
use near_jsonrpc_client::methods::EXPERIMENTAL_protocol_config::RpcProtocolConfigRequest;
use near_primitives::gas::Gas;
use near_primitives::types::BlockReference;
use near_sdk::NearToken;

pub struct ProtocolLimits {
    pub storage_byte_cost: NearToken,
    /// What every key-value record costs on top of its key and value bytes
    pub extra_bytes_per_record: u64,
    /// For all function calls of one transaction together
    pub max_prepaid_gas: Gas,
}

pub fn protocol_limits(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<ProtocolLimits> {
    let protocol_config = network_config
        .json_rpc_client()
        .blocking_call(RpcProtocolConfigRequest {
            block_reference: block_reference.clone(),
        })
        .map_err(color_eyre::eyre::Report::new)
        .wrap_err_with(|| {
            format!(
                "Couldn't read the protocol config of network {} (RPC {})",
                network_config.network_name, network_config.rpc_url
            )
        })?;
    let runtime_config = protocol_config.runtime_config;
    Ok(ProtocolLimits {
        storage_byte_cost: runtime_config.storage_amount_per_byte,
        extra_bytes_per_record: runtime_config
            .transaction_costs
            .storage_usage_config
            .num_extra_bytes_record,
        max_prepaid_gas: runtime_config
            .wasm_config
            .limit_config
            .max_total_prepaid_gas,
    })
}
