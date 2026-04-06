#![allow(deprecated)]

use bollard::Docker;
use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::HostConfig;
use futures::StreamExt;
use indexmap::IndexMap;
use tracing::{debug, info};

use crate::error::{LabError, Result};

use super::container::RunResult;

/// Wrapper around the bollard Docker client.
///
/// Ref: <https://docs.gitlab.com/runner/executors/docker/>
#[derive(Debug)]
pub struct DockerClient {
    docker: Docker,
}

impl DockerClient {
    /// Connect to the local Docker daemon.
    pub fn new() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()?;
        Ok(Self { docker })
    }

    /// Access the underlying bollard Docker client.
    pub fn inner(&self) -> &Docker {
        &self.docker
    }

    /// Pull a Docker image.
    pub async fn pull_image(&self, image: &str, force: bool) -> Result<()> {
        if !force && self.docker.inspect_image(image).await.is_ok() {
            debug!(image, "image already available locally");
            return Ok(());
        }

        info!(image, "pulling image");
        let options = CreateImageOptions {
            from_image: image,
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            result.map_err(LabError::Docker)?;
        }

        Ok(())
    }

    /// Create a container and return its ID.
    pub async fn create_container(
        &self,
        image: &str,
        env: &IndexMap<String, String>,
        workdir: &str,
    ) -> Result<String> {
        self.create_container_full(image, env, workdir, None, None)
            .await
    }

    /// Create a container on a specific network.
    pub async fn create_container_with_network(
        &self,
        image: &str,
        env: &IndexMap<String, String>,
        workdir: &str,
        network: Option<&str>,
    ) -> Result<String> {
        self.create_container_full(image, env, workdir, network, None)
            .await
    }

    /// Create a container with full options: network, entrypoint, and secrets file mount.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#image>
    ///
    /// Security: if `secrets_file` is provided, it's bind-mounted read-only at
    /// `/run/secrets/env` instead of passing secrets as environment variables.
    /// This prevents secrets from being visible via `docker inspect`.
    pub async fn create_container_full(
        &self,
        image: &str,
        env: &IndexMap<String, String>,
        workdir: &str,
        network: Option<&str>,
        entrypoint: Option<&[String]>,
    ) -> Result<String> {
        self.create_container_secure(image, env, workdir, network, entrypoint, None)
            .await
    }

    /// Create a container with optional secrets file mount.
    pub async fn create_container_secure(
        &self,
        image: &str,
        env: &IndexMap<String, String>,
        workdir: &str,
        network: Option<&str>,
        entrypoint: Option<&[String]>,
        secrets_file: Option<&str>,
    ) -> Result<String> {
        let mut env_vec: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();

        let mut binds = vec![format!("{workdir}:/workspace")];
        // Mount secrets file read-only if provided
        if let Some(sf) = secrets_file {
            binds.push(format!("{sf}:/run/secrets/env:ro"));
        }
        // Forward SSH agent for git operations (if available)
        if let Ok(ssh_sock) = std::env::var("SSH_AUTH_SOCK") {
            if std::path::Path::new(&ssh_sock).exists() {
                binds.push(format!("{ssh_sock}:/ssh-agent:ro"));
                env_vec.push("SSH_AUTH_SOCK=/ssh-agent".to_string());
            }
        }
        // Mount host Docker socket for DinD jobs.
        // Locally, DinD service is replaced with host Docker — faster and no TLS complexity.
        if host_docker_socket_available() {
            binds.push("/var/run/docker.sock:/var/run/docker.sock".to_string());
            override_dind_env(&mut env_vec);
        }

        // Docker Security Hardening (per OWASP Docker Security Cheat Sheet):
        //
        // RULE #3: Drop all capabilities, add back only what's needed
        // RULE #4: Prevent in-container privilege escalation
        // RULE #7: Limit resources to prevent DoS
        // Local execution: no restrictions — use full laptop power for speed.
        // Security is handled via secrets file mount + output masking, not container hardening.
        let host_config = HostConfig {
            binds: Some(binds),
            network_mode: network.map(String::from),
            ..Default::default()
        };

        let mut config = ContainerConfig {
            image: Some(image.to_string()),
            env: Some(env_vec),
            working_dir: Some("/workspace".to_string()),
            host_config: Some(host_config),
            // Note: containers run as root (needed for apk/apt package install).
            // File ownership is fixed after job completes via chown.
            cmd: Some(vec!["sleep".to_string(), "3600".to_string()]),
            ..Default::default()
        };

        // Wire image:entrypoint override
        if let Some(ep) = entrypoint {
            config.entrypoint = Some(ep.to_vec());
        }

        let response = self
            .docker
            .create_container(Some(CreateContainerOptions::<String>::default()), config)
            .await
            .map_err(LabError::Docker)?;

        debug!(id = %response.id, image, "container created");
        Ok(response.id)
    }

    /// Start a container.
    pub async fn start_container(&self, id: &str) -> Result<()> {
        self.docker
            .start_container(id, None::<StartContainerOptions<String>>)
            .await
            .map_err(LabError::Docker)?;
        debug!(id, "container started");
        Ok(())
    }

    /// Run a command inside a running container and capture the result.
    /// If a `SecretMasker` is provided, secret values are replaced with
    /// `[MASKED]` in both terminal output and the returned `RunResult`.
    pub async fn run_in_container(
        &self,
        id: &str,
        cmd: &[String],
        env: &IndexMap<String, String>,
    ) -> Result<RunResult> {
        self.run_in_container_masked(id, cmd, env, None).await
    }

    /// Run a command with secret masking and optional job-name prefix on output.
    pub async fn run_in_container_masked(
        &self,
        id: &str,
        cmd: &[String],
        env: &IndexMap<String, String>,
        masker: Option<&crate::secrets::SecretMasker>,
    ) -> Result<RunResult> {
        self.run_in_container_full(id, cmd, env, masker, None).await
    }

    /// Run a command with all output options.
    pub async fn run_in_container_full(
        &self,
        id: &str,
        cmd: &[String],
        env: &IndexMap<String, String>,
        masker: Option<&crate::secrets::SecretMasker>,
        job_prefix: Option<&str>,
    ) -> Result<RunResult> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        let mut env_vec: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();

        // Override DinD env vars at exec level too (container env alone isn't enough —
        // exec env takes precedence when the same var is set in both places).
        if host_docker_socket_available() {
            override_dind_env(&mut env_vec);
        }

        let options = CreateExecOptions {
            cmd: Some(cmd.to_vec()),
            env: Some(env_vec),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            working_dir: Some("/workspace".to_string()),
            ..Default::default()
        };

        let instance = self
            .docker
            .create_exec(id, options)
            .await
            .map_err(LabError::Docker)?;

        let output = self
            .docker
            .start_exec(&instance.id, Some(StartExecOptions::default()))
            .await
            .map_err(LabError::Docker)?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let bollard::exec::StartExecResults::Attached {
            output: mut stream,
            input: _,
        } = output
        {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(bollard::container::LogOutput::StdOut { message }) => {
                        let text = String::from_utf8_lossy(&message);
                        let safe = masker
                            .map(|m| m.mask(&text))
                            .unwrap_or_else(|| text.to_string());
                        if let Some(prefix) = job_prefix {
                            // Prefix each line for parallel job output
                            for line in safe.split_inclusive('\n') {
                                print!("\x1b[36m{prefix}\x1b[0m | {line}");
                            }
                        } else {
                            print!("{safe}");
                        }
                        stdout.push_str(&safe);
                    }
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        let text = String::from_utf8_lossy(&message);
                        let safe = masker
                            .map(|m| m.mask(&text))
                            .unwrap_or_else(|| text.to_string());
                        if let Some(prefix) = job_prefix {
                            for line in safe.split_inclusive('\n') {
                                eprint!("\x1b[33m{prefix}\x1b[0m | {line}");
                            }
                        } else {
                            eprint!("{safe}");
                        }
                        stderr.push_str(&safe);
                    }
                    Ok(_) => {}
                    Err(e) => return Err(LabError::Docker(e)),
                }
            }
        }

        let inspect = self
            .docker
            .inspect_exec(&instance.id)
            .await
            .map_err(LabError::Docker)?;
        let code = inspect.exit_code.unwrap_or(-1);

        Ok(RunResult {
            exit_code: code,
            stdout,
            stderr,
        })
    }

    /// Stop a container.
    pub async fn stop_container(&self, id: &str) -> Result<()> {
        self.docker
            .stop_container(id, Some(StopContainerOptions { t: 5 }))
            .await
            .map_err(LabError::Docker)?;
        debug!(id, "container stopped");
        Ok(())
    }

    /// Remove a container.
    pub async fn remove_container(&self, id: &str) -> Result<()> {
        self.docker
            .remove_container(
                id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(LabError::Docker)?;
        debug!(id, "container removed");
        Ok(())
    }
}

/// Strip DinD-related env vars and replace with host Docker socket config.
///
/// When running locally, the DinD sidecar's TLS certs aren't shared between
/// containers. Instead we mount the host Docker socket and override env vars
/// so the Docker CLI inside the container talks to the host daemon directly.
pub(crate) fn override_dind_env(env_vec: &mut Vec<String>) {
    env_vec.retain(|e| {
        !e.starts_with("DOCKER_HOST=")
            && !e.starts_with("DOCKER_TLS_VERIFY=")
            && !e.starts_with("DOCKER_CERT_PATH=")
            && !e.starts_with("DOCKER_AUTH_CONFIG=")
    });
    env_vec.push("DOCKER_HOST=unix:///var/run/docker.sock".to_string());
    env_vec.push("DOCKER_TLS_VERIFY=".to_string());
}

/// Check whether the host Docker socket is available for DinD passthrough.
fn host_docker_socket_available() -> bool {
    std::path::Path::new("/var/run/docker.sock").exists()
}

/// Get current user's UID:GID without unsafe code.
pub fn get_current_uid_gid() -> String {
    let uid = std::process::Command::new("id")
        .args(["-u"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "1000".to_string());

    let gid = std::process::Command::new("id")
        .args(["-g"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "1000".to_string());

    format!("{uid}:{gid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_dind_env_strips_docker_vars() {
        let mut env = vec![
            "DOCKER_HOST=tcp://docker:2376".to_string(),
            "DOCKER_TLS_VERIFY=1".to_string(),
            "DOCKER_CERT_PATH=/certs/client".to_string(),
            "DOCKER_AUTH_CONFIG={\"auths\":{}}".to_string(),
            "MY_VAR=keep_me".to_string(),
            "PATH=/usr/bin".to_string(),
        ];
        override_dind_env(&mut env);

        // Non-Docker vars preserved
        assert!(env.contains(&"MY_VAR=keep_me".to_string()));
        assert!(env.contains(&"PATH=/usr/bin".to_string()));

        // DinD vars replaced
        assert!(env.contains(&"DOCKER_HOST=unix:///var/run/docker.sock".to_string()));
        assert!(env.contains(&"DOCKER_TLS_VERIFY=".to_string()));

        // Original DinD vars removed
        assert!(!env.iter().any(|e| e == "DOCKER_HOST=tcp://docker:2376"));
        assert!(!env.iter().any(|e| e == "DOCKER_TLS_VERIFY=1"));
        assert!(!env.iter().any(|e| e.starts_with("DOCKER_CERT_PATH=")));
        assert!(!env.iter().any(|e| e.starts_with("DOCKER_AUTH_CONFIG=")));
    }

    #[test]
    fn override_dind_env_no_docker_vars_present() {
        let mut env = vec!["FOO=bar".to_string(), "CI=true".to_string()];
        override_dind_env(&mut env);

        assert!(env.contains(&"FOO=bar".to_string()));
        assert!(env.contains(&"CI=true".to_string()));
        assert!(env.contains(&"DOCKER_HOST=unix:///var/run/docker.sock".to_string()));
        assert!(env.contains(&"DOCKER_TLS_VERIFY=".to_string()));
        assert_eq!(env.len(), 4);
    }

    #[test]
    fn override_dind_env_empty_input() {
        let mut env: Vec<String> = vec![];
        override_dind_env(&mut env);

        assert_eq!(env.len(), 2);
        assert!(env.contains(&"DOCKER_HOST=unix:///var/run/docker.sock".to_string()));
        assert!(env.contains(&"DOCKER_TLS_VERIFY=".to_string()));
    }

    #[test]
    fn override_dind_env_partial_docker_vars() {
        let mut env = vec![
            "DOCKER_HOST=tcp://docker:2375".to_string(),
            "APP_PORT=3000".to_string(),
        ];
        override_dind_env(&mut env);

        assert!(!env.iter().any(|e| e == "DOCKER_HOST=tcp://docker:2375"));
        assert!(env.contains(&"DOCKER_HOST=unix:///var/run/docker.sock".to_string()));
        assert!(env.contains(&"APP_PORT=3000".to_string()));
    }

    #[test]
    fn override_dind_env_preserves_docker_prefixed_non_dind_vars() {
        let mut env = vec![
            "DOCKER_HOST=tcp://docker:2376".to_string(),
            "DOCKER_BUILDKIT=1".to_string(),
            "DOCKER_CLI_EXPERIMENTAL=enabled".to_string(),
        ];
        override_dind_env(&mut env);

        // DOCKER_BUILDKIT and DOCKER_CLI_EXPERIMENTAL should be preserved
        assert!(env.contains(&"DOCKER_BUILDKIT=1".to_string()));
        assert!(env.contains(&"DOCKER_CLI_EXPERIMENTAL=enabled".to_string()));
        // DOCKER_HOST replaced
        assert!(env.contains(&"DOCKER_HOST=unix:///var/run/docker.sock".to_string()));
    }

    #[test]
    fn get_uid_gid_returns_colon_separated() {
        let result = get_current_uid_gid();
        assert!(result.contains(':'));
        let parts: Vec<&str> = result.split(':').collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].parse::<u32>().is_ok());
        assert!(parts[1].parse::<u32>().is_ok());
    }
}
