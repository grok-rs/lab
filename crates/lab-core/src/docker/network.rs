#![allow(deprecated)]

use bollard::Docker;
use bollard::models::EndpointSettings;
use bollard::network::{ConnectNetworkOptions, CreateNetworkOptions};
use tracing::debug;

use crate::error::{LabError, Result};

/// Create a Docker network for job + service communication.
/// Ref: <https://docs.gitlab.com/ci/services/#accessing-the-services>
pub async fn create_network(docker: &Docker, name: &str) -> Result<String> {
    let options = CreateNetworkOptions {
        name: name.to_string(),
        driver: "bridge".to_string(),
        ..Default::default()
    };

    let response = docker
        .create_network(options)
        .await
        .map_err(LabError::Docker)?;

    let id = response.id;
    debug!(network = %id, name, "network created");
    Ok(id)
}

/// Connect a container to a network with optional aliases.
pub async fn connect_to_network(
    docker: &Docker,
    network_id: &str,
    container_id: &str,
    aliases: &[String],
) -> Result<()> {
    let config = ConnectNetworkOptions {
        container: container_id.to_string(),
        endpoint_config: EndpointSettings {
            aliases: Some(aliases.to_vec()),
            ..Default::default()
        },
    };

    docker
        .connect_network(network_id, config)
        .await
        .map_err(LabError::Docker)?;

    debug!(network = %network_id, container = %container_id, "connected to network");
    Ok(())
}

/// Remove a Docker network.
pub async fn remove_network(docker: &Docker, network_id: &str) -> Result<()> {
    docker
        .remove_network(network_id)
        .await
        .map_err(LabError::Docker)?;
    debug!(network = %network_id, "network removed");
    Ok(())
}
