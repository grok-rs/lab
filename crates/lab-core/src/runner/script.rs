use tracing::{error, info, warn};

use crate::artifacts;
use crate::cache;
use crate::docker::service::ServiceOrchestrator;
use crate::error::{LabError, Result};
use crate::model::job::Job;
use crate::model::variables::{expand_variables, to_env_map};

use super::job_context::JobContext;

/// Run a complete job: services + before_script + script + after_script.
///
/// Ref: <https://docs.gitlab.com/ci/yaml/#script>
/// Ref: <https://docs.gitlab.com/ci/yaml/#before_script>
/// Ref: <https://docs.gitlab.com/ci/yaml/#after_script>
/// Ref: <https://docs.gitlab.com/ci/services/>
pub async fn run_job(ctx: &mut JobContext, job: &Job) -> Result<()> {
    let image = &ctx.image;
    let env = to_env_map(&ctx.variables);
    let workdir = ctx
        .config
        .workdir
        .canonicalize()
        .map_err(|e| LabError::FileRead {
            path: ctx.config.workdir.clone(),
            source: e,
        })?;
    let workdir_str = workdir.to_str().unwrap_or("/workspace");

    // Security: warn about non-deterministic image tags
    warn_image_tag(image);

    // Pull image
    let force_pull = matches!(ctx.config.pull_policy, crate::config::PullPolicy::Always);
    ctx.docker.pull_image(image, force_pull).await?;

    // Determine if services are needed
    let has_services = job.services.as_ref().is_some_and(|s| !s.is_empty());

    // If services exist: create network first, then container on that network
    let network_name = if has_services {
        Some(format!("lab-{}-network", ctx.name))
    } else {
        None
    };

    // Create the network before the container
    let network_id = if let Some(ref name) = network_name {
        Some(crate::docker::network::create_network(ctx.docker.inner(), name).await?)
    } else {
        None
    };

    // Get entrypoint override from image config
    let entrypoint = job
        .image
        .as_ref()
        .and_then(|i| i.entrypoint().map(|e| e.to_vec()));

    // Security: write secrets to a temp file and mount instead of passing as env vars.
    // This prevents `docker inspect` from revealing secret values.
    let secrets_tmpfile = if ctx.masker.has_values() {
        let tmp_dir = workdir.join(".lab/tmp");
        let _ = std::fs::create_dir_all(&tmp_dir);
        let secrets_path = tmp_dir.join(format!("secrets-{}.env", ctx.name));
        // Write secrets as KEY=VALUE, one per line
        let mut content = String::new();
        for (key, val) in &env {
            // Only mount vars that the masker tracks as secrets
            if ctx.masker.mask(val.as_str()) != *val {
                content.push_str(&format!("export {key}='{val}'\n"));
            }
        }
        if !content.is_empty() {
            let _ = std::fs::write(&secrets_path, &content);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&secrets_path, std::fs::Permissions::from_mode(0o600));
            }
            Some(secrets_path)
        } else {
            None
        }
    } else {
        None
    };

    // Create job container
    let container_id = ctx
        .docker
        .create_container_secure(
            image,
            &env,
            workdir_str,
            network_name.as_deref(),
            entrypoint.as_deref(),
            secrets_tmpfile.as_ref().and_then(|p| p.to_str()),
        )
        .await?;
    ctx.container_id = Some(container_id.clone());
    ctx.docker.start_container(&container_id).await?;

    // Start services if configured
    let service_ctx = if let (Some(services), Some(net_name)) = (&job.services, &network_name) {
        if !services.is_empty() {
            let orchestrator = ServiceOrchestrator::new(ctx.docker.clone());
            match orchestrator
                .start_services(&ctx.name, services, net_name)
                .await
            {
                Ok(svc_ctx) => Some((orchestrator, svc_ctx)),
                Err(e) => {
                    warn!(job = %ctx.name, error = %e, "failed to start services");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // Inject artifacts from dependency jobs
    if !ctx.config.no_artifacts {
        let dep_jobs = get_artifact_dependencies(job);
        if !dep_jobs.is_empty() {
            if let Err(e) =
                artifacts::inject_artifacts(&ctx.docker, &container_id, &dep_jobs, &workdir).await
            {
                warn!(job = %ctx.name, error = %e, "failed to inject artifacts");
            }
        }
    }

    // Restore cache
    if !ctx.config.no_cache {
        if let Some(cache_configs) = &job.cache {
            if let Err(e) =
                cache::restore_cache(&container_id, cache_configs, &ctx.variables, &workdir)
            {
                warn!(job = %ctx.name, error = %e, "failed to restore cache");
            }
        }
    }

    // Build the combined script
    let mut commands = Vec::new();
    if let Some(before) = &job.before_script {
        for cmd in before {
            commands.push(expand_variables(cmd, &ctx.variables));
        }
    }
    for cmd in &job.script {
        commands.push(expand_variables(cmd, &ctx.variables));
    }

    // Run main script with retry and timeout
    let timeout_duration = job.timeout;

    info!(job = %ctx.name, "running script ({} commands)", commands.len());
    let main_result = run_with_retry_and_timeout(
        ctx,
        &container_id,
        &commands,
        job.retry.as_ref(),
        timeout_duration,
    )
    .await;

    // after_script (always runs, even on failure)
    if let Some(after) = &job.after_script {
        let after_commands: Vec<String> = after
            .iter()
            .map(|cmd| expand_variables(cmd, &ctx.variables))
            .collect();
        info!(job = %ctx.name, "running after_script ({} commands)", after_commands.len());
        if let Err(e) = run_commands(ctx, &container_id, &after_commands).await {
            error!(job = %ctx.name, error = %e, "after_script failed");
        }
    }

    // Collect artifacts (respects artifacts:when)
    // Ref: <https://docs.gitlab.com/ci/yaml/#artifactswhen>
    if !ctx.config.no_artifacts {
        if let Some(artifact_config) = &job.artifacts {
            let should_collect = match &artifact_config.when_upload {
                Some(crate::model::job::ArtifactWhen::OnSuccess) | None => main_result.is_ok(),
                Some(crate::model::job::ArtifactWhen::OnFailure) => main_result.is_err(),
                Some(crate::model::job::ArtifactWhen::Always) => true,
            };
            if should_collect {
                if let Err(e) = artifacts::collect_artifacts(
                    &ctx.docker,
                    &container_id,
                    &ctx.name,
                    artifact_config,
                    &workdir,
                )
                .await
                {
                    warn!(job = %ctx.name, error = %e, "failed to collect artifacts");
                }
            }
        }
    }

    // Save cache (respects cache:when)
    if !ctx.config.no_cache {
        if let Some(cache_configs) = &job.cache {
            let job_succeeded = main_result.is_ok();
            if let Err(e) = cache::save_cache(
                &container_id,
                cache_configs,
                &ctx.variables,
                &workdir,
                job_succeeded,
            ) {
                warn!(job = %ctx.name, error = %e, "failed to save cache");
            }
        }
    }

    // Stop services
    if let Some((orchestrator, svc_ctx)) = service_ctx {
        if let Err(e) = orchestrator.stop_services(svc_ctx).await {
            warn!(job = %ctx.name, error = %e, "failed to stop services");
        }
    }

    // Fix file ownership — container runs as root but host user needs to own the files.
    // Without this, .nx/, dist/, node_modules/ etc. become root-owned and break local tools.
    let uid_gid = crate::docker::client::get_current_uid_gid();
    let _ = ctx
        .docker
        .run_in_container(
            &container_id,
            &[
                "sh".into(),
                "-c".into(),
                format!("chown -R {uid_gid} /workspace 2>/dev/null || true"),
            ],
            &indexmap::IndexMap::new(),
        )
        .await;

    // Cleanup job container
    let _ = ctx.docker.stop_container(&container_id).await;
    let _ = ctx.docker.remove_container(&container_id).await;

    // Security: remove secrets tmpfile
    if let Some(ref sf) = secrets_tmpfile {
        let _ = std::fs::remove_file(sf);
    }

    // Remove network
    if let Some(ref nid) = network_id {
        let _ = crate::docker::network::remove_network(ctx.docker.inner(), nid).await;
    }

    main_result
}

/// Determine which jobs to download artifacts from.
/// Ref: <https://docs.gitlab.com/ci/yaml/#dependencies>
/// Ref: <https://docs.gitlab.com/ci/yaml/#needsartifacts>
fn get_artifact_dependencies(job: &Job) -> Vec<String> {
    // If dependencies: is set, use that (explicit list)
    if let Some(deps) = &job.dependencies {
        return deps.clone();
    }
    // If needs: is set, use jobs that want artifacts
    if let Some(needs) = &job.needs {
        return needs
            .iter()
            .filter(|n| n.wants_artifacts())
            .map(|n| n.job_name().to_string())
            .collect();
    }
    // No explicit deps — in stage mode, all previous stage jobs' artifacts
    // are downloaded, but we'd need the full plan for that. For now, empty.
    Vec::new()
}

/// Run script with retry and timeout support.
/// Ref: <https://docs.gitlab.com/ci/yaml/#retry>
/// Ref: <https://docs.gitlab.com/ci/yaml/#timeout>
/// Ref: <https://docs.gitlab.com/ci/yaml/#retrywhen>
async fn run_with_retry_and_timeout(
    ctx: &JobContext,
    container_id: &str,
    commands: &[String],
    retry_config: Option<&crate::model::job::RetryConfig>,
    timeout: Option<std::time::Duration>,
) -> Result<()> {
    let max_retries = retry_config.map(|r| r.max_retries()).unwrap_or(0);
    let mut last_err = None;

    for attempt in 0..=max_retries {
        if attempt > 0 {
            warn!(job = %ctx.name, attempt = attempt + 1, "retrying job");
        }

        let result = if let Some(duration) = timeout {
            match tokio::time::timeout(duration, run_commands(ctx, container_id, commands)).await {
                Ok(r) => r,
                Err(_) => Err(LabError::Other(format!(
                    "job timed out after {}s",
                    duration.as_secs()
                ))),
            }
        } else {
            run_commands(ctx, container_id, commands).await
        };

        match result {
            Ok(()) => return Ok(()),
            Err(ref e) => {
                // Determine failure type for retry:when filtering
                let failure_type = match e {
                    LabError::ContainerFailed { .. } => "script_failure",
                    LabError::Other(msg) if msg.contains("timed out") => "stuck_or_timeout_failure",
                    _ => "unknown_failure",
                };

                // Check if retry:when allows retrying this failure type
                let should_retry = retry_config
                    .map(|r| r.should_retry(failure_type))
                    .unwrap_or(true);

                if !should_retry || attempt == max_retries {
                    last_err = Some(result.unwrap_err());
                    break;
                }
                last_err = Some(result.unwrap_err());
            }
        }
    }

    Err(last_err.unwrap_or_else(|| LabError::Other("job failed".into())))
}

/// Run a list of shell commands inside the container.
///
/// Generates a proper shell script matching GitLab Runner behavior:
/// - Uses `bash -eo pipefail` if bash is available
/// - Falls back to `sh -e`
/// - Each command is echoed before execution (like GitLab Runner's trace)
///
/// Ref: <https://docs.gitlab.com/runner/executors/docker/#the-shells>
async fn run_commands(ctx: &JobContext, container_id: &str, commands: &[String]) -> Result<()> {
    if commands.is_empty() {
        return Ok(());
    }

    // Generate shell script with proper error handling
    let script = generate_shell_script(commands);
    let shell = detect_shell(ctx, container_id).await;

    let exec_cmd: Vec<String> = shell
        .iter()
        .map(String::clone)
        .chain(std::iter::once(script))
        .collect();

    let env = to_env_map(&ctx.variables);
    let result = ctx
        .docker
        .run_in_container_full(
            container_id,
            &exec_cmd,
            &env,
            Some(&ctx.masker),
            Some(&ctx.name),
        )
        .await?;

    if result.exit_code != 0 {
        return Err(LabError::ContainerFailed {
            code: result.exit_code,
        });
    }

    Ok(())
}

/// Generate a shell script from commands.
/// Each command is printed before execution (matching GitLab Runner output).
/// Sources /run/secrets/env if it exists (secrets mounted as file).
fn generate_shell_script(commands: &[String]) -> String {
    let mut script = String::new();
    // Fix "dubious ownership" for bind-mounted workspace.
    // Write a gitconfig file directly and point GIT_CONFIG_GLOBAL to it.
    // This works regardless of which git binary is installed (busybox, apk, apt).
    script.push_str("echo '[safe]\n\tdirectory = *' > /tmp/.lab-gitconfig 2>/dev/null || true\n");
    script.push_str("export GIT_CONFIG_GLOBAL=/tmp/.lab-gitconfig\n");
    // Set up SSH for git operations (if SSH agent is forwarded)
    // Install openssh-client if missing and SSH agent is available
    script.push_str(concat!(
        "if [ -S \"$SSH_AUTH_SOCK\" ]; then ",
        "command -v ssh >/dev/null 2>&1 || (apk add --no-cache openssh-client 2>/dev/null || apt-get install -yq openssh-client 2>/dev/null) >/dev/null 2>&1 || true; ",
        "mkdir -p /root/.ssh 2>/dev/null; ",
        "ssh-keyscan gitlab.com github.com bitbucket.org >> /root/.ssh/known_hosts 2>/dev/null || true; ",
        "fi\n",
    ));
    // Source secrets file if mounted (secure alternative to env vars)
    script.push_str("[ -f /run/secrets/env ] && . /run/secrets/env\n");

    // Step-level profiling using epoch seconds (works on Alpine/busybox)
    script.push_str("_LAB_T0=$(date +%s)\n");

    for (i, cmd) in commands.iter().enumerate() {
        let escaped = cmd.replace('\'', "'\\''");
        script.push_str(&format!("printf '\\033[0;36m$ {}\\033[0m\\n'\n", escaped));
        script.push_str(&format!("_LAB_S{i}=$(date +%s)\n"));
        script.push_str(cmd);
        script.push('\n');
        script.push_str(&format!(
            "_LAB_E{i}=$(date +%s); _LAB_D{i}=$((_LAB_E{i} - _LAB_S{i})); \
             printf '\\033[2m  [{step}/{total}] %dm %ds\\033[0m\\n' \
             $((_LAB_D{i} / 60)) $((_LAB_D{i} % 60))\n",
            i = i,
            step = i + 1,
            total = commands.len(),
        ));
    }

    script.push_str(
        "_LAB_TF=$(date +%s); _LAB_TT=$((_LAB_TF - _LAB_T0)); \
         printf '\\n\\033[1;32m  total: %dm %ds\\033[0m\\n' \
         $((_LAB_TT / 60)) $((_LAB_TT % 60))\n",
    );

    script
}

/// Detect if bash is available in the container; fall back to sh.
/// Returns the shell command as a list of arguments (e.g., ["bash", "-eo", "pipefail", "-c"]).
async fn detect_shell(ctx: &JobContext, container_id: &str) -> Vec<String> {
    let env = to_env_map(&ctx.variables);
    let check = ctx
        .docker
        .run_in_container(
            container_id,
            &[
                "sh".into(),
                "-c".into(),
                "command -v bash 2>/dev/null".into(),
            ],
            &env,
        )
        .await;

    match check {
        Ok(r) if r.exit_code == 0 => {
            vec!["bash".into(), "-eo".into(), "pipefail".into(), "-c".into()]
        }
        _ => vec!["sh".into(), "-e".into(), "-c".into()],
    }
}

/// Warn about non-deterministic or insecure Docker image tags.
/// Best practice: use specific tags with SHA256 digest.
/// Ref: Node.js Docker Cheat Sheet — "Use explicit and deterministic Docker base image tags"
fn warn_image_tag(image: &str) {
    let tag = image.split(':').nth(1).unwrap_or("latest");

    if tag == "latest" || !image.contains(':') {
        warn!(
            image = %image,
            "using :latest tag — image builds are non-deterministic. \
             Pin to a specific version (e.g., node:20-alpine)"
        );
    }

    // Warn about full OS images (not alpine/slim/distroless)
    if !image.contains("alpine")
        && !image.contains("slim")
        && !image.contains("distroless")
        && !image.contains("@sha256:")
        && (image.starts_with("node:")
            || image.starts_with("python:")
            || image.starts_with("ruby:"))
    {
        info!(
            image = %image,
            "consider using an alpine/slim variant for smaller attack surface"
        );
    }
}
