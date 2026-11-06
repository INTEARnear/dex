use color_eyre::eyre::Context;
use near_cli_rs::common::JsonRpcClientExt;
use near_cli_rs::config::NetworkConfig;
use near_jsonrpc_client::methods::EXPERIMENTAL_protocol_config::RpcProtocolConfigError;
use near_jsonrpc_client::methods::RpcHandlerResponse;
use near_primitives::gas::Gas;
use near_primitives::types::BlockReference;
use near_sdk::NearToken;
use serde::Deserialize;

pub struct ProtocolLimits {
    pub storage_byte_cost: NearToken,
    /// What every key-value record costs on top of its key and value bytes
    pub extra_bytes_per_record: u64,
    /// For all function calls of one transaction together
    pub max_prepaid_gas: Gas,
}

/// Only the fields that the CLI reads, so it doesn't break when nearcore
/// removes others
#[derive(Deserialize)]
struct ProtocolConfig {
    runtime_config: RuntimeConfig,
}

impl RpcHandlerResponse for ProtocolConfig {}

#[derive(Deserialize)]
struct RuntimeConfig {
    storage_amount_per_byte: NearToken,
    transaction_costs: TransactionCosts,
    wasm_config: WasmConfig,
}

#[derive(Deserialize)]
struct TransactionCosts {
    storage_usage_config: StorageUsageConfig,
}

#[derive(Deserialize)]
struct StorageUsageConfig {
    num_extra_bytes_record: u64,
}

#[derive(Deserialize)]
struct WasmConfig {
    limit_config: LimitConfig,
}

#[derive(Deserialize)]
struct LimitConfig {
    max_total_prepaid_gas: u64,
}

pub fn protocol_limits(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<ProtocolLimits> {
    let protocol_config = network_config
        .json_rpc_client()
        .blocking_call(near_jsonrpc_client::methods::any::<
            Result<ProtocolConfig, RpcProtocolConfigError>,
        >(
            "EXPERIMENTAL_protocol_config",
            serde_json::to_value(block_reference)?,
        ))
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
        max_prepaid_gas: Gas::from_gas(
            runtime_config
                .wasm_config
                .limit_config
                .max_total_prepaid_gas,
        ),
    })
}
