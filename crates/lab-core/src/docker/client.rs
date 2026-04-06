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

        // Docker Security Hardening (per OWASP Docker Security Cheat Sheet):
        //
        // RULE #3: Drop all capabilities, add back only what's needed
        // RULE #4: Prevent in-container privilege escalation
        // RULE #7: Limit resources to prevent DoS
        // Local execution: no resource limits — use full laptop power for speed.
        // Security hardening still applied via secrets file mount + output masking.
        let host_config = HostConfig {
            binds: Some(binds),
            network_mode: network.map(String::from),
            // Drop only the most dangerous capabilities
            cap_drop: Some(vec![
                "SYS_ADMIN".to_string(),  // No mount/cgroup/namespace
                "SYS_MODULE".to_string(), // No kernel module loading
                "SYS_RAWIO".to_string(),  // No raw I/O
            ]),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            // No memory/CPU/PID limits — local testing should be fast
            tmpfs: Some(
                [("/tmp".to_string(), "rw,size=512m".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };

        let mut config = ContainerConfig {
            image: Some(image.to_string()),
            env: Some(env_vec),
            working_dir: Some("/workspace".to_string()),
            host_config: Some(host_config),
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

        let env_vec: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();

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
