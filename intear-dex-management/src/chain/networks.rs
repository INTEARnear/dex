use color_eyre::eyre::{bail, eyre};
use near_cli_rs::config::{Config, NetworkConfig};
use near_primitives::types::{BlockReference, Finality};

use crate::deployment::ENGINE_ACCOUNT_ID;

enum NetworkReadOutcome<Value> {
    Read(Value),
    NoEngine,
    AllConnectionsFailed,
}

/// Runs `read` once per configured network where the engine exists, all
/// networks in parallel, for prompts that show chain data before the network
/// is chosen. Each network is read through its first connection that answers;
/// connections that fail are named in a warning.
pub fn read_on_every_network<Value: Send>(
    config: &Config,
    read: impl Fn(&NetworkConfig, &BlockReference) -> color_eyre::eyre::Result<Value> + Sync,
) -> color_eyre::eyre::Result<Vec<(String, Value)>> {
    let mut connections_by_network: Vec<(&str, Vec<(&str, &NetworkConfig)>)> = Vec::new();
    for (connection_name, network_config) in config.network_connection.iter() {
        match connections_by_network
            .iter_mut()
            .find(|(network_name, _)| *network_name == network_config.network_name)
        {
            Some((_, connections)) => connections.push((connection_name, network_config)),
            None => connections_by_network.push((
                &network_config.network_name,
                vec![(connection_name, network_config)],
            )),
        }
    }

    let outcomes = std::thread::scope(|scope| {
        let network_reads = connections_by_network
            .iter()
            .map(|(network_name, connections)| {
                let read = &read;
                scope.spawn(move || {
                    (
                        *network_name,
                        read_through_first_answering_connection(connections, read),
                    )
                })
            })
            .collect::<Vec<_>>();
        network_reads
            .into_iter()
            .map(|network_read| network_read.join())
            .collect::<Vec<_>>()
    });

    let mut values = Vec::new();
    let mut failed_network_names = Vec::new();
    for outcome in outcomes {
        let (network_name, outcome) = outcome.map_err(|_| {
            eyre!("Reading a network panicked, this is a bug in intear-dex-management")
        })?;
        match outcome {
            NetworkReadOutcome::Read(value) => values.push((network_name.to_string(), value)),
            NetworkReadOutcome::NoEngine => {}
            NetworkReadOutcome::AllConnectionsFailed => failed_network_names.push(network_name),
        }
    }
    if values.is_empty() {
        if failed_network_names.is_empty() {
            bail!(
                "{ENGINE_ACCOUNT_ID} doesn't exist on any configured network. Add a mainnet connection with `near config add-connection`."
            );
        }
        bail!(
            "No connection answered on the networks where {ENGINE_ACCOUNT_ID} may exist: {}",
            failed_network_names.join(", ")
        );
    }
    Ok(values)
}

fn read_through_first_answering_connection<Value>(
    connections: &[(&str, &NetworkConfig)],
    read: &impl Fn(&NetworkConfig, &BlockReference) -> color_eyre::eyre::Result<Value>,
) -> NetworkReadOutcome<Value> {
    let block_reference = BlockReference::Finality(Finality::Final);
    for (connection_name, network_config) in connections {
        match super::engine::engine_exists(network_config, &block_reference) {
            Ok(false) => return NetworkReadOutcome::NoEngine,
            Ok(true) => match read(network_config, &block_reference) {
                Ok(value) => return NetworkReadOutcome::Read(value),
                Err(error) => tracing::warn!("Connection {connection_name} failed: {error:#}"),
            },
            Err(error) => tracing::warn!("Connection {connection_name} failed: {error:#}"),
        }
    }
    NetworkReadOutcome::AllConnectionsFailed
}
