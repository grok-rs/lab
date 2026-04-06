use std::sync::Arc;
use std::time::Instant;

use tracing::{info, warn};

use crate::config::Config;
use crate::docker::client::DockerClient;
use crate::error::Result;
use crate::model::pipeline::Plan;
use crate::model::variables::{Variables, merge_variables, predefined_variables};

use super::executor::{self, Executor, ExecutorCtx, from_fn};
use super::job_context::JobContext;
use super::output::{JobStatus, PipelineResult};
use super::script;

/// The main runner that converts a Plan into an async pipeline.
pub struct Runner {
    config: Arc<Config>,
    docker: Arc<DockerClient>,
    global_variables: Variables,
    /// Secret variables (kept separate for per-job scoping and masking).
    secret_variables: Variables,
    result: PipelineResult,
}

impl Runner {
    pub fn new(config: Config, global_variables: Variables) -> Result<Self> {
        let docker = DockerClient::new()?;
        Ok(Self {
            config: Arc::new(config),
            docker: Arc::new(docker),
            global_variables,
            secret_variables: Variables::new(),
            result: PipelineResult::new(),
        })
    }

    /// Create runner with separate secret variables for masking and scoping.
    pub fn with_secrets(
        config: Config,
        global_variables: Variables,
        secret_variables: Variables,
    ) -> Result<Self> {
        let docker = DockerClient::new()?;
        Ok(Self {
            config: Arc::new(config),
            docker: Arc::new(docker),
            global_variables,
            secret_variables,
            result: PipelineResult::new(),
        })
    }

    /// Convert a Plan into a composable Executor.
    pub fn build_plan_executor(&self, plan: &Plan) -> Executor {
        let stage_executors: Vec<Executor> = plan
            .stages
            .iter()
            .map(|stage| {
                let job_executors: Vec<Executor> = stage
                    .jobs
                    .iter()
                    .map(|pj| self.build_job_executor(&pj.name, &pj.job))
                    .collect();

                let stage_name = stage.name.clone();
                let max_par = self.config.max_parallel;

                let stage_run = executor::parallel(job_executors, max_par);
                from_fn(move |ctx| async move {
                    info!(stage = %stage_name, "starting stage");
                    stage_run(ctx).await
                })
            })
            .collect();

        executor::pipeline(stage_executors)
    }

    fn build_job_executor(&self, name: &str, job: &crate::model::job::Job) -> Executor {
        let config = self.config.clone();
        let docker = self.docker.clone();
        let job = job.clone();
        let job_name = name.to_string();
        let global_vars = self.global_variables.clone();
        let result_tracker = self.result.clone();

        // Security: scope secrets to only those this job references
        let job_secrets = crate::secrets::scope_secrets_for_job(&job, &self.secret_variables);
        // Build masker from the scoped secrets
        let masker = crate::secrets::SecretMasker::from_secrets(&job_secrets);

        from_fn(move |_ctx: ExecutorCtx| async move {
            let start = Instant::now();

            // Handle resource_group — mutual exclusion via lock file
            // Ref: <https://docs.gitlab.com/ci/yaml/#resource_group>
            let _resource_lock = if let Some(rg) = &job.resource_group {
                let lock_dir = config.workdir.join(".lab/locks");
                let _ = std::fs::create_dir_all(&lock_dir);
                let lock_path = lock_dir.join(format!("{rg}.lock"));
                // Wait for lock (simple polling — adequate for local use)
                for attempt in 0..60 {
                    if std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&lock_path)
                        .is_ok()
                    {
                        break;
                    }
                    if attempt == 0 {
                        info!(job = %job_name, resource_group = %rg, "waiting for resource lock");
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Some(lock_path)
            } else {
                None
            };

            // Ensure lock is released when job completes (via Drop-like cleanup at end)

            // Handle manual jobs — GitLab pauses and waits for user approval.
            // Ref: <https://docs.gitlab.com/ci/yaml/#when>
            //
            // Behavior:
            // - `when: manual` → prompt user to approve (play button equivalent)
            // - `manual_confirmation` → show custom message in the prompt
            // - User says no → job is skipped (not failed)
            // - `allow_failure: true` (default for manual) → pipeline continues
            if job.when == crate::model::job::When::Manual {
                use crate::config::ManualMode;

                let approved = match config.manual_mode {
                    ManualMode::Approve => {
                        info!(job = %job_name, "manual job auto-approved (--approve-manual)");
                        true
                    }
                    ManualMode::Skip => {
                        info!(job = %job_name, "manual job auto-skipped (--skip-manual)");
                        false
                    }
                    ManualMode::Prompt => {
                        let prompt_msg = job
                            .manual_confirmation
                            .as_deref()
                            .unwrap_or("Manual job requires approval to run.");

                        eprintln!();
                        eprintln!(
                            "  \x1b[33m⏸ Manual job: {}\x1b[0m  [{}]",
                            job_name, job.stage
                        );
                        eprintln!("  {prompt_msg}");
                        eprint!("  \x1b[33mRun this job? [y/N]\x1b[0m ");

                        let mut input = String::new();
                        if std::io::stdin().read_line(&mut input).is_ok() {
                            input.trim().eq_ignore_ascii_case("y")
                        } else {
                            false
                        }
                    }
                };

                if !approved {
                    result_tracker.record(
                        &job_name,
                        &job.stage,
                        JobStatus::Success, // Skipped manual = not a failure
                        start.elapsed(),
                    );
                    return Ok(());
                }
                info!(job = %job_name, "manual job approved");
            }

            // Handle start_in delay (for delayed jobs)
            // Ref: <https://docs.gitlab.com/ci/yaml/#start_in>
            if let Some(delay_str) = &job.start_in {
                if let Ok(secs) = parse_delay(delay_str) {
                    info!(job = %job_name, delay = %delay_str, "delaying job start");
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                }
            }

            // Handle trigger:include — child pipeline execution
            // Ref: <https://docs.gitlab.com/ci/yaml/#trigger>
            if let Some(trigger) = &job.trigger {
                info!(job = %job_name, "running child pipeline (trigger)");
                let result = run_child_pipeline(trigger, &config, &global_vars).await;
                let duration = start.elapsed();
                match &result {
                    Ok(()) => {
                        info!(job = %job_name, duration = ?duration, "child pipeline succeeded");
                        result_tracker.record(&job_name, &job.stage, JobStatus::Success, duration);
                    }
                    Err(e) => {
                        result_tracker.record(&job_name, &job.stage, JobStatus::Failed, duration);
                        warn!(job = %job_name, error = %e, "child pipeline failed");
                    }
                }
                return result;
            }

            info!(job = %job_name, stage = %job.stage, "starting job");

            // Build variables: predefined < global < scoped secrets < job-level
            let predefined = predefined_variables(&config, &job_name, &job.stage)?;
            let job_vars = merge_variables(&[
                &predefined,
                &global_vars,
                &job_secrets, // Only secrets this job references
                &job.variables,
            ]);

            // Determine image — expand variables in image name
            // Ref: image names can contain $VAR (e.g., node:${NODE_VERSION})
            let raw_image = config
                .platform_overrides
                .get(&job_name)
                .map(String::as_str)
                .or_else(|| job.image.as_ref().map(|i| i.name()))
                .unwrap_or("alpine:latest");
            let image = crate::model::variables::expand_variables(raw_image, &job_vars);

            // Create job context with masker for output protection
            let mut job_ctx = JobContext::new(
                job_name.clone(),
                job.stage.clone(),
                image.clone(),
                job_vars,
                docker.clone(),
                config.clone(),
                masker,
            );

            // Run the job
            let result = script::run_job(&mut job_ctx, &job).await;
            let duration = start.elapsed();

            match &result {
                Ok(()) => {
                    info!(job = %job_name, duration = ?duration, "job succeeded");
                    result_tracker.record(&job_name, &job.stage, JobStatus::Success, duration);
                }
                Err(e) => {
                    if job.allow_failure.is_allowed(1) {
                        warn!(job = %job_name, error = %e, duration = ?duration, "job failed (allowed)");
                        result_tracker.record(
                            &job_name,
                            &job.stage,
                            JobStatus::AllowedFailure,
                            duration,
                        );
                        return Ok(());
                    }
                    result_tracker.record(&job_name, &job.stage, JobStatus::Failed, duration);
                }
            }

            // Release resource_group lock
            if let Some(lock_path) = _resource_lock {
                let _ = std::fs::remove_file(&lock_path);
            }

            result
        })
    }

    /// Run the plan to completion.
    pub async fn run(&self, plan: &Plan) -> Result<()> {
        let ex = self.build_plan_executor(plan);
        let ctx = ExecutorCtx::new();
        ex(ctx).await
    }

    /// Get the pipeline result tracker.
    pub fn result(&self) -> &PipelineResult {
        &self.result
    }
}

/// Execute a child pipeline defined by `trigger:include`.
/// Ref: <https://docs.gitlab.com/ci/yaml/#trigger>
async fn run_child_pipeline(
    trigger: &crate::model::job::TriggerConfig,
    config: &Config,
    global_vars: &Variables,
) -> crate::error::Result<()> {
    use crate::model::job::TriggerConfig;
    use crate::parser::parse_pipeline;
    use crate::planner::build_plan;

    let child_files = match trigger {
        TriggerConfig::Detailed {
            include: Some(files),
            ..
        } => files
            .as_slice()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        TriggerConfig::Simple(project) => {
            warn!(project = %project, "trigger:project not yet supported for local execution");
            return Ok(());
        }
        _ => {
            warn!("trigger config has no include path, skipping");
            return Ok(());
        }
    };

    for child_file in &child_files {
        let child_path = config.workdir.join(child_file);
        info!(file = %child_path.display(), "running child pipeline");

        let child_pipeline = parse_pipeline(&child_path)?;
        let child_vars =
            crate::model::variables::merge_variables(&[&child_pipeline.variables, global_vars]);

        let child_plan = build_plan(
            &child_pipeline.stages,
            &child_pipeline.jobs,
            &child_vars,
            None,
            None,
        )?;

        if child_plan.stages.is_empty() {
            info!("child pipeline has no jobs to run");
            continue;
        }

        let child_runner = Runner::new((*config).clone(), child_vars)?;
        child_runner.run(&child_plan).await?;
    }

    Ok(())
}

/// Parse a delay string like "30 seconds", "5 minutes", "1 hour" into seconds.
fn parse_delay(s: &str) -> std::result::Result<u64, ()> {
    let s = s.trim().to_lowercase();
    // Try "N unit" format
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() == 2 {
        if let Ok(n) = parts[0].parse::<u64>() {
            return match parts[1] {
                "second" | "seconds" | "s" => Ok(n),
                "minute" | "minutes" | "m" => Ok(n * 60),
                "hour" | "hours" | "h" => Ok(n * 3600),
                _ => Err(()),
            };
        }
    }
    // Try compact format: "30s", "5m", "1h"
    if let Some(n) = s
        .strip_suffix('s')
        .and_then(|n| n.trim().parse::<u64>().ok())
    {
        return Ok(n);
    }
    if let Some(n) = s
        .strip_suffix('m')
        .and_then(|n| n.trim().parse::<u64>().ok())
    {
        return Ok(n * 60);
    }
    if let Some(n) = s
        .strip_suffix('h')
        .and_then(|n| n.trim().parse::<u64>().ok())
    {
        return Ok(n * 3600);
    }
    s.parse::<u64>().map_err(|_| ())
}
