use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptions, RemoveContainerOptions, StartContainerOptions,
    StopContainerOptions,
};
use futures::StreamExt;
use indexmap::IndexMap;
use tracing::{debug, info};

use crate::error::{LabError, Result};

use super::container::RunResult;

/// Options for creating a job container.
pub struct CreateJobOpts<'a> {
    pub image: &'a str,
    pub env: &'a IndexMap<String, String>,
    pub workdir: &'a str,
    pub network: Option<&'a str>,
    pub entrypoint: Option<&'a [String]>,
    pub secrets_file: Option<&'a str>,
    pub cpus: Option<f64>,
    pub memory: Option<i64>,
}

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

    /// Pull a Docker image with progress indicator.
    pub async fn pull_image(&self, image: &str, force: bool) -> Result<()> {
        if !force && self.docker.inspect_image(image).await.is_ok() {
            debug!(image, "image already available locally");
            return Ok(());
        }

        let spinner = indicatif::ProgressBar::new_spinner();
        spinner.set_style(
            indicatif::ProgressStyle::default_spinner()
                .template("  {spinner:.cyan} {msg}")
                .unwrap(),
        );
        spinner.set_message(format!("Pulling {image}..."));
        spinner.enable_steady_tick(std::time::Duration::from_millis(100));

        let options = CreateImageOptions {
            from_image: Some(image.to_string()),
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(options), None, None);
        let mut layers_done = 0usize;
        while let Some(result) = stream.next().await {
            let info = result.map_err(LabError::Docker)?;
            if let Some(status) = &info.status {
                if status.contains("Pull complete") || status.contains("Already exists") {
                    layers_done += 1;
                    spinner.set_message(format!("Pulling {image}... ({layers_done} layers)"));
                }
            }
        }

        spinner.finish_and_clear();
        info!(image, "pulled image");
        Ok(())
    }

    /// Create a job container with all options.
    pub async fn create_job_container(&self, opts: &CreateJobOpts<'_>) -> Result<String> {
        let mut env_vec: Vec<String> = opts.env.iter().map(|(k, v)| format!("{k}={v}")).collect();

        let mut binds = vec![format!("{}:/workspace", opts.workdir)];
        if let Some(sf) = opts.secrets_file {
            binds.push(format!("{sf}:/run/secrets/env:ro"));
        }
        if let Ok(ssh_sock) = std::env::var("SSH_AUTH_SOCK") {
            if std::path::Path::new(&ssh_sock).exists() {
                binds.push(format!("{ssh_sock}:/ssh-agent:ro"));
                env_vec.push("SSH_AUTH_SOCK=/ssh-agent".to_string());
            }
        }
        if host_docker_socket_available() {
            binds.push("/var/run/docker.sock:/var/run/docker.sock".to_string());
            override_dind_env(&mut env_vec);
        }

        let host_config = HostConfig {
            binds: Some(binds),
            network_mode: opts.network.map(String::from),
            nano_cpus: opts.cpus.map(|c| (c * 1_000_000_000.0) as i64),
            memory: opts.memory,
            ..Default::default()
        };

        let mut body = ContainerCreateBody {
            image: Some(opts.image.to_string()),
            env: Some(env_vec),
            working_dir: Some("/workspace".to_string()),
            host_config: Some(host_config),
            cmd: Some(vec!["sleep".to_string(), "3600".to_string()]),
            ..Default::default()
        };

        if let Some(ep) = opts.entrypoint {
            body.entrypoint = Some(ep.to_vec());
        }

        let response = self
            .docker
            .create_container(Some(CreateContainerOptions::default()), body)
            .await
            .map_err(LabError::Docker)?;

        debug!(id = %response.id, image = opts.image, "container created");
        Ok(response.id)
    }

    /// Start a container.
    pub async fn start_container(&self, id: &str) -> Result<()> {
        self.docker
            .start_container(id, None::<StartContainerOptions>)
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
        self.run_in_container_full(id, cmd, env, None, None).await
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
            .stop_container(
                id,
                Some(StopContainerOptions {
                    t: Some(5),
                    signal: None,
                }),
            )
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
    std::process::Command::new("sh")
        .args(["-c", "echo $(id -u):$(id -g)"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "1000:1000".to_string())
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
