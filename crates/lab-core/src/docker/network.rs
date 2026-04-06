use bollard::models::{EndpointSettings, NetworkConnectRequest, NetworkCreateRequest};
use bollard::Docker;
use tracing::debug;

use crate::error::{LabError, Result};

/// Create a Docker network for job + service communication.
pub async fn create_network(docker: &Docker, name: &str) -> Result<String> {
    let request = NetworkCreateRequest {
        name: name.to_string(),
        driver: Some("bridge".to_string()),
        ..Default::default()
    };

    let response = docker
        .create_network(request)
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
    let request = NetworkConnectRequest {
        container: Some(container_id.to_string()),
        endpoint_config: Some(EndpointSettings {
            aliases: Some(aliases.to_vec()),
            ..Default::default()
        }),
    };

    docker
        .connect_network(network_id, request)
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
