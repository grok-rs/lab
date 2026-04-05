#![allow(deprecated)]

use std::sync::Arc;
use std::time::Duration;

use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, RemoveContainerOptions,
    StartContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::HostConfig;
use futures::StreamExt;
use tracing::{debug, info, warn};

use crate::error::{LabError, Result};
use crate::model::job::ServiceConfig;

use super::client::DockerClient;

/// Manages the lifecycle of service containers for a job.
/// Ref: <https://docs.gitlab.com/ci/services/>
pub struct ServiceOrchestrator {
    docker: Arc<DockerClient>,
}

/// Running service context — must be stopped after job completes.
pub struct ServiceContext {
    pub network_name: String,
    pub containers: Vec<RunningService>,
}

pub struct RunningService {
    pub id: String,
    pub hostname: String,
    pub image: String,
}

impl ServiceOrchestrator {
    pub fn new(docker: Arc<DockerClient>) -> Self {
        Self { docker }
    }

    /// Start all services on an existing network.
    /// The job container should already be on this network.
    /// Returns a ServiceContext that must be cleaned up after the job.
    pub async fn start_services(
        &self,
        _job_name: &str,
        services: &[ServiceConfig],
        network_name: &str,
    ) -> Result<ServiceContext> {
        // Start each service on the existing network
        let mut running = Vec::new();
        for svc in services {
            match self.start_service(svc, network_name).await {
                Ok(rs) => running.push(rs),
                Err(e) => {
                    warn!(service = %svc.image_name(), error = %e, "failed to start service");
                    for rs in &running {
                        let _ = self.stop_service(rs).await;
                    }
                    return Err(e);
                }
            }
        }

        // Wait for services to be healthy
        for rs in &running {
            self.wait_for_ready(rs).await;
        }

        Ok(ServiceContext {
            network_name: network_name.to_string(),
            containers: running,
        })
    }

    /// Stop all service containers. Network cleanup is handled by the caller.
    pub async fn stop_services(&self, ctx: ServiceContext) -> Result<()> {
        for rs in &ctx.containers {
            if let Err(e) = self.stop_service(rs).await {
                warn!(service = %rs.hostname, error = %e, "failed to stop service");
            }
        }
        Ok(())
    }

    async fn start_service(
        &self,
        svc: &ServiceConfig,
        network_name: &str,
    ) -> Result<RunningService> {
        let image = svc.image_name();
        let hostname = svc.hostname();

        info!(service = %hostname, image, "starting service");

        // Pull image
        let pull_options = CreateImageOptions {
            from_image: image,
            ..Default::default()
        };
        let mut stream = self
            .docker
            .inner()
            .create_image(Some(pull_options), None, None);
        while let Some(result) = stream.next().await {
            result.map_err(LabError::Docker)?;
        }

        // Build env
        let env_vec: Vec<String> = match svc {
            ServiceConfig::Detailed { variables, .. } => variables
                .iter()
                .map(|(k, v)| format!("{k}={}", v.value()))
                .collect(),
            _ => Vec::new(),
        };

        // Build entrypoint/command overrides
        let (entrypoint, cmd) = match svc {
            ServiceConfig::Detailed {
                entrypoint,
                command,
                ..
            } => (entrypoint.clone(), command.clone()),
            _ => (None, None),
        };

        let host_config = HostConfig {
            network_mode: Some(network_name.to_string()),
            ..Default::default()
        };

        let mut config = ContainerConfig {
            image: Some(image.to_string()),
            env: Some(env_vec),
            host_config: Some(host_config),
            hostname: Some(hostname.clone()),
            ..Default::default()
        };

        if let Some(ep) = entrypoint {
            config.entrypoint = Some(ep);
        }
        if let Some(c) = cmd {
            config.cmd = Some(c);
        }

        let create_opts = CreateContainerOptions::<String>::default();
        let response = self
            .docker
            .inner()
            .create_container(Some(create_opts), config)
            .await
            .map_err(LabError::Docker)?;

        let container_id = response.id;

        // Start
        self.docker
            .inner()
            .start_container(&container_id, None::<StartContainerOptions<String>>)
            .await
            .map_err(LabError::Docker)?;

        debug!(service = %hostname, id = %container_id, "service started");

        Ok(RunningService {
            id: container_id,
            hostname,
            image: image.to_string(),
        })
    }

    async fn stop_service(&self, rs: &RunningService) -> Result<()> {
        let opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        self.docker
            .inner()
            .remove_container(&rs.id, Some(opts))
            .await
            .map_err(LabError::Docker)?;
        debug!(service = %rs.hostname, "service stopped");
        Ok(())
    }

    /// Wait for a service container to be ready (up to 60s).
    /// Ref: <https://docs.gitlab.com/ci/services/#how-services-are-linked-to-the-job>
    async fn wait_for_ready(&self, rs: &RunningService) {
        let max_wait = Duration::from_secs(60);
        let interval = Duration::from_secs(2);
        let start = std::time::Instant::now();

        while start.elapsed() < max_wait {
            match self
                .docker
                .inner()
                .inspect_container(&rs.id, None::<bollard::container::InspectContainerOptions>)
                .await
            {
                Ok(info) => {
                    let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                    if running {
                        // Check health status if available
                        let health = info
                            .state
                            .as_ref()
                            .and_then(|s| s.health.as_ref())
                            .and_then(|h| h.status.as_ref());

                        match health {
                            Some(status) => {
                                let status_str = format!("{status:?}");
                                if status_str.contains("HEALTHY") {
                                    info!(service = %rs.hostname, "service healthy");
                                    return;
                                } else if status_str.contains("UNHEALTHY") {
                                    warn!(service = %rs.hostname, "service unhealthy");
                                    return;
                                }
                                // Starting — continue waiting
                            }
                            None => {
                                // No health check — if running, assume ready
                                debug!(service = %rs.hostname, "service running (no health check)");
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(service = %rs.hostname, error = %e, "failed to inspect service");
                }
            }
            tokio::time::sleep(interval).await;
        }

        warn!(service = %rs.hostname, "service readiness timeout after 60s");
    }
}

/// Derive secondary hostname from image name.
/// Ref: <https://docs.gitlab.com/ci/services/#accessing-the-services>
/// "registry.example.com/my/postgres:14" → "registry.example.com-my-postgres"
#[allow(dead_code)]
fn secondary_hostname(image: &str) -> String {
    let without_tag = image.split(':').next().unwrap_or(image);
    without_tag.replace('/', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secondary_hostname() {
        assert_eq!(secondary_hostname("postgres:14"), "postgres");
        assert_eq!(
            secondary_hostname("registry.example.com/my/postgres:14"),
            "registry.example.com-my-postgres"
        );
        assert_eq!(secondary_hostname("redis"), "redis");
    }
}
