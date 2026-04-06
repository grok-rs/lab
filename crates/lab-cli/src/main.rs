use anyhow::{Context, Result};
use clap::Parser;
use console::style;

mod cli;
mod display;
mod logging;
mod mcp;

use cli::{Cli, Command, SecretsAction};
use lab_core::config::{Config, PullPolicy};
use lab_core::model::variables::merge_variables;
use lab_core::parser::parse_pipeline;
use lab_core::planner::build_plan;
use lab_core::runner::Runner;
use lab_core::secrets;

/// Check that required tools are available before running.
fn check_tools() -> Result<()> {
    // Docker daemon
    let docker = std::process::Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output();
    match docker {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            anyhow::bail!(
                "Docker daemon is not running.\n  Error: {}\n  Fix: start Docker Desktop or run `sudo systemctl start docker`",
                stderr.trim()
            );
        }
        Err(_) => {
            anyhow::bail!(
                "docker not found.\n  Fix: install Docker — https://docs.docker.com/get-docker/"
            );
        }
    }

    // Git
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        anyhow::bail!("git not found.\n  Fix: install git — https://git-scm.com/downloads");
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            jobs,
            stage,
            file,
            mut variables,
            event,
            tag,
            pull_policy,
            privileged,
            cpus,
            memory,
            no_artifacts,
            no_cache,
            platforms,
            max_parallel,
            approve_manual,
            skip_manual,
            pull_secrets,
            no_secrets,
            secrets_file,
            dry_run,
            no_preflight,
            clean,
            retry_failed,
            verbose,
        } => {
            logging::init(verbose);
            check_tools()?;

            // Handle --tag shorthand: simulates a tag push pipeline
            if let Some(tag_value) = &tag {
                variables.push(("CI_COMMIT_TAG".into(), tag_value.clone()));
                variables.push(("CI_PIPELINE_SOURCE".into(), "push".into()));
                // Extract ref name from tag
                let ref_name = tag_value
                    .rsplit('/')
                    .next()
                    .unwrap_or(tag_value)
                    .to_string();
                variables.push(("CI_COMMIT_REF_NAME".into(), ref_name));
            }

            // Handle --event: set CI_PIPELINE_SOURCE and related vars per event type.
            // All possible values from GitLab docs:
            // https://docs.gitlab.com/ci/jobs/job_rules/#ci_pipeline_source-predefined-variable
            if let Some(evt) = &event {
                variables.push(("CI_PIPELINE_SOURCE".into(), evt.clone()));
                match evt.as_str() {
                    "push" => {
                        // Default for branch pushes — auto-detected already
                    }
                    "merge_request_event" => {
                        // MR pipeline — needs MR-specific vars
                        variables.push(("CI_MERGE_REQUEST_IID".into(), "0".into()));
                    }
                    "schedule" => {
                        variables.push(("CI_PIPELINE_SCHEDULE".into(), "true".into()));
                    }
                    "web" => {
                        // Manual trigger via GitLab UI "Run pipeline"
                    }
                    "api" => {
                        // Triggered via /api/v4/projects/:id/pipeline
                    }
                    "trigger" => {
                        // Triggered via trigger token
                        variables.push(("CI_PIPELINE_TRIGGERED".into(), "true".into()));
                    }
                    "pipeline" => {
                        // Multi-project pipeline (downstream)
                    }
                    "parent_pipeline" => {
                        // Child pipeline triggered by parent
                    }
                    "chat" => {
                        // GitLab ChatOps command
                    }
                    "webide" => {
                        // Web IDE pipeline
                    }
                    "external_pull_request_event" => {
                        // GitHub external PR
                    }
                    "ondemand_dast_scan" | "ondemand_dast_validation" => {
                        // DAST scan pipelines
                    }
                    "security_orchestration_policy" => {
                        // Scheduled scan execution policy
                    }
                    other => {
                        eprintln!(
                            "Warning: unknown event '{}'. Valid events: push, merge_request_event, \
                             schedule, web, api, trigger, pipeline, parent_pipeline, chat, webide, \
                             external_pull_request_event",
                            other
                        );
                    }
                }
            }

            let workdir = std::env::current_dir()?;
            let project_config = lab_core::config::ProjectConfig::load(&workdir);

            let job_filter = if retry_failed {
                // Load failed job names from last run
                let last_run = lab_core::paths::last_run_file(&workdir);
                if last_run.exists() {
                    let content = std::fs::read_to_string(&last_run)
                        .context("failed to read last run file")?;
                    let failed: Vec<String> =
                        serde_json::from_str(&content).context("failed to parse last run file")?;
                    if failed.is_empty() {
                        println!("No failed jobs in last run.");
                        return Ok(());
                    }
                    println!(
                        "Retrying {} failed job(s): {}",
                        style(failed.len()).cyan().bold(),
                        failed.join(", ")
                    );
                    Some(failed)
                } else {
                    anyhow::bail!("no previous run found — run the pipeline first");
                }
            } else if jobs.is_empty() {
                None
            } else {
                Some(jobs)
            };

            let mut config = Config {
                ci_file: file.clone(),
                workdir: workdir.clone(),
                job_filter,
                stage_filter: stage,
                variables: variables.into_iter().collect(),
                pull_policy: PullPolicy::from(pull_policy),
                privileged,
                no_artifacts,
                no_cache,
                platform_overrides: platforms.into_iter().collect(),
                max_parallel: max_parallel.unwrap_or_else(|| {
                    std::thread::available_parallelism()
                        .map(|n| n.get())
                        .unwrap_or(4)
                }),
                cpus,
                memory: memory.as_deref().map(parse_memory_string).transpose()?,
                manual_mode: if approve_manual {
                    lab_core::config::ManualMode::Approve
                } else if skip_manual {
                    lab_core::config::ManualMode::Skip
                } else {
                    lab_core::config::ManualMode::Prompt
                },
            };

            project_config.apply_to(&mut config);

            // Configure local project path mappings for include:project resolution
            if !project_config.projects.is_empty() {
                lab_core::parser::resolver::set_project_mappings(project_config.projects.clone());
            }

            let pipeline = parse_pipeline(&file).context("failed to parse pipeline")?;

            // Load secrets
            let mut secret_vars = if no_secrets {
                lab_core::model::variables::Variables::new()
            } else if pull_secrets {
                // Pull fresh from GitLab
                match secrets::pull_secrets_from_gitlab(&workdir) {
                    Ok(vars) => {
                        if let Err(e) = secrets::save_secrets_file(&workdir, &vars) {
                            eprintln!("Warning: failed to save secrets: {e}");
                        }
                        vars
                    }
                    Err(e) => {
                        eprintln!("Warning: failed to pull secrets from GitLab: {e}");
                        secrets::load_secrets_file(&workdir).unwrap_or_default()
                    }
                }
            } else if let Some(path) = &secrets_file {
                secrets::load_env_file(path).unwrap_or_default()
            } else {
                secrets::load_secrets_file(&workdir).unwrap_or_default()
            };

            // Build predefined vars early so workflow:rules can use CI_PIPELINE_SOURCE etc.
            let predefined_vars = lab_core::model::variables::predefined_variables(&config, "", "")
                .unwrap_or_default();

            // Merge variables: predefined < pipeline < secrets < .lab.yml < CLI
            let user_vars: lab_core::model::variables::Variables = config
                .variables
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        lab_core::model::variables::VariableValue::Simple(v.clone()),
                    )
                })
                .collect();
            let global_vars = merge_variables(&[
                &predefined_vars,
                &pipeline.variables,
                &secret_vars,
                &user_vars,
            ]);

            // Evaluate workflow:rules — gate pipeline + merge matched rule variables
            // Ref: https://docs.gitlab.com/ci/yaml/#workflowrulesvariables
            let mut global_vars = global_vars;
            if let Some(workflow) = &pipeline.workflow {
                if !workflow.rules.is_empty() {
                    use lab_core::model::job::When;
                    use lab_core::model::rules::{RuleResult, evaluate_rules};
                    match evaluate_rules(&workflow.rules, &global_vars, When::Always) {
                        RuleResult::Matched {
                            when: When::Never, ..
                        }
                        | RuleResult::NotMatched => {
                            println!("Pipeline blocked by workflow:rules — no matching rule.");
                            return Ok(());
                        }
                        RuleResult::Matched { variables, .. } => {
                            // Merge workflow:rules:variables into global context
                            if let Some(wf_vars) = variables {
                                for (k, v) in wf_vars {
                                    global_vars.insert(k, v);
                                }
                            }
                        }
                    }
                }
            }

            // Build execution plan
            let plan = build_plan(
                &pipeline.stages,
                &pipeline.jobs,
                &global_vars,
                config.job_filter.as_deref(),
                config.stage_filter.as_deref(),
            )
            .context("failed to build execution plan")?;

            if plan.stages.is_empty() {
                println!("No jobs to run.");
                return Ok(());
            }

            let total_jobs: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();

            // Display workflow:name if set
            if let Some(wf) = &pipeline.workflow {
                if let Some(name) = &wf.name {
                    let expanded = lab_core::model::variables::expand_variables(name, &global_vars);
                    if !expanded.trim().is_empty() {
                        println!("Pipeline: {}", console::style(expanded.trim()).bold());
                    }
                }
            }

            // Dry run mode — show plan without executing
            if dry_run {
                display::print_dry_run(&plan, &pipeline, &global_vars, &secret_vars);
                return Ok(());
            }

            // Pre-flight variable check
            if !no_preflight && !dry_run {
                let missing_count = display::print_preflight_report(&plan, &global_vars);

                if missing_count > 0 {
                    eprintln!(
                        "  [{}] Pull secrets from GitLab    [{}] Continue anyway    [{}] Abort",
                        style("p").cyan().bold(),
                        style("c").yellow().bold(),
                        style("a").red().bold(),
                    );
                    eprint!("\n  Choice: ");
                    let mut input = String::new();
                    let _ = std::io::stdin().read_line(&mut input);
                    match input.trim().to_lowercase().as_str() {
                        "p" => {
                            println!();
                            println!("Pulling secrets from GitLab...");
                            match secrets::pull_secrets_from_gitlab(&workdir) {
                                Ok(pulled) => {
                                    let count = pulled.len();
                                    if let Err(e) = secrets::save_secrets_file(&workdir, &pulled) {
                                        eprintln!("Warning: failed to save secrets: {e}");
                                    }
                                    // Merge pulled secrets into global vars
                                    for (k, v) in &pulled {
                                        global_vars.entry(k.clone()).or_insert_with(|| v.clone());
                                    }
                                    secret_vars = pulled;
                                    println!(
                                        "{} {} secret(s) loaded\n",
                                        style("✓").green().bold(),
                                        count
                                    );
                                }
                                Err(e) => {
                                    eprintln!(
                                        "{} Failed to pull secrets: {e}\n",
                                        style("✗").red().bold()
                                    );
                                }
                            }
                        }
                        "c" | "y" => {} // continue
                        _ => {
                            println!("Aborted.");
                            return Ok(());
                        }
                    }
                }
            }

            println!(
                "Running {total_jobs} job(s) across {} stage(s)",
                plan.stages.len()
            );
            if !secret_vars.is_empty() {
                println!("  {} secret(s) loaded", style(secret_vars.len()).cyan());
            }
            println!();

            // Snapshot untracked files before running (for cleanup detection)
            let pre_untracked = get_untracked_files(&workdir);

            // Run with signal handling
            let runner = Runner::with_secrets(config, global_vars, secret_vars)
                .context("failed to initialize runner")?;

            let pipeline_err = tokio::select! {
                result = runner.run(&plan) => result.err(),
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("\nInterrupted — cleaning up containers...");
                    Some(lab_core::error::LabError::Other("interrupted by user".into()))
                }
            };

            display::print_pipeline_summary(runner.result());

            // Save failed job names for --retry-failed
            {
                let failed_jobs: Vec<String> = runner
                    .result()
                    .jobs()
                    .iter()
                    .filter(|j| j.status == lab_core::runner::output::JobStatus::Failed)
                    .map(|j| j.name.clone())
                    .collect();
                let run_file = lab_core::paths::last_run_file(&workdir);
                if let Some(parent) = run_file.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(
                    &run_file,
                    serde_json::to_string(&failed_jobs).unwrap_or_default(),
                );
            }

            // Detect files created by job execution
            let post_untracked = get_untracked_files(&workdir);
            let new_files: Vec<String> = post_untracked
                .into_iter()
                .filter(|f| !pre_untracked.contains(f))
                .collect();

            if !new_files.is_empty() {
                println!();
                if clean {
                    println!(
                        "{} Cleaning {} file(s)/dir(s) created by jobs:",
                        style("⟳").cyan().bold(),
                        new_files.len()
                    );
                    for f in &new_files {
                        let path = workdir.join(f);
                        if path.is_dir() {
                            let _ = std::fs::remove_dir_all(&path);
                        } else {
                            let _ = std::fs::remove_file(&path);
                        }
                        println!("    {} {}", style("✗").red(), style(f).dim());
                    }
                } else {
                    println!(
                        "{} {} file(s)/dir(s) created by jobs (use {} to auto-remove):",
                        style("!").yellow().bold(),
                        new_files.len(),
                        style("--clean").cyan(),
                    );
                    for f in &new_files {
                        println!("    {} {}", style("·").dim(), style(f).yellow());
                    }
                }
            }

            if let Some(err) = pipeline_err {
                display::print_error_suggestions(&err);
                cleanup_docker_resources().await;
                return Err(err).context("pipeline failed");
            }
        }

        Command::Artifacts { job, clean } => {
            let workdir = std::env::current_dir()?;
            let artifacts_base = lab_core::paths::artifacts_dir(&workdir);

            if clean {
                if artifacts_base.exists() {
                    std::fs::remove_dir_all(&artifacts_base)?;
                    println!("{} Artifacts cleaned", style("✓").green().bold());
                } else {
                    println!("No artifacts to clean.");
                }
                return Ok(());
            }

            if !artifacts_base.exists() {
                println!("No artifacts found. Run a pipeline first.");
                return Ok(());
            }

            let entries: Vec<_> = std::fs::read_dir(&artifacts_base)?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter(|e| {
                    job.as_ref()
                        .map(|j| e.file_name().to_string_lossy() == *j)
                        .unwrap_or(true)
                })
                .collect();

            if entries.is_empty() {
                println!(
                    "No artifacts found{}.",
                    job.as_ref()
                        .map(|j| format!(" for job '{j}'"))
                        .unwrap_or_default()
                );
                return Ok(());
            }

            println!("{}", style("Artifacts:").bold());
            println!();

            for entry in entries {
                let job_name = entry.file_name().to_string_lossy().to_string();
                let job_dir = entry.path();
                let mut total_size: u64 = 0;
                let mut file_count: usize = 0;

                fn walk_dir(
                    dir: &std::path::Path,
                    base: &std::path::Path,
                    total_size: &mut u64,
                    file_count: &mut usize,
                    files: &mut Vec<(String, u64)>,
                ) {
                    if let Ok(entries) = std::fs::read_dir(dir) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.is_dir() {
                                walk_dir(&path, base, total_size, file_count, files);
                            } else if let Ok(meta) = path.metadata() {
                                let rel = path.strip_prefix(base).unwrap_or(&path);
                                let size = meta.len();
                                *total_size += size;
                                *file_count += 1;
                                files.push((rel.display().to_string(), size));
                            }
                        }
                    }
                }

                let mut files = Vec::new();
                walk_dir(
                    &job_dir,
                    &job_dir,
                    &mut total_size,
                    &mut file_count,
                    &mut files,
                );

                println!(
                    "  {} {} — {} file(s), {}",
                    style("●").green(),
                    style(&job_name).bold(),
                    file_count,
                    format_size(total_size),
                );

                for (path, size) in &files {
                    println!(
                        "      {} {}",
                        style(path).dim(),
                        style(format_size(*size)).dim(),
                    );
                }
            }
        }

        Command::List { file, output } => {
            let pipeline = parse_pipeline(&file).context("failed to parse pipeline")?;
            match output {
                cli::OutputFormat::Json => {
                    let data: Vec<serde_json::Value> = pipeline
                        .jobs
                        .iter()
                        .map(|(name, job)| {
                            serde_json::json!({
                                "name": name,
                                "stage": job.stage,
                                "image": job.image.as_ref().map(|i| i.name()),
                                "needs": job.needs.as_ref().map(|n| n.iter().map(|d| d.job_name()).collect::<Vec<_>>()),
                                "when": format!("{:?}", job.when),
                            })
                        })
                        .collect();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "stages": pipeline.stages,
                            "jobs": data,
                        }))
                        .unwrap()
                    );
                }
                cli::OutputFormat::Text => {
                    display::print_pipeline_list(&pipeline);
                }
            }
        }

        Command::Validate { file, output } => match parse_pipeline(&file) {
            Ok(pipeline) => match output {
                cli::OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "valid": true,
                            "stages": pipeline.stages.len(),
                            "jobs": pipeline.jobs.len(),
                        })
                    );
                }
                cli::OutputFormat::Text => {
                    println!(
                        "Valid: {} stages, {} jobs",
                        pipeline.stages.len(),
                        pipeline.jobs.len()
                    );
                }
            },
            Err(e) => {
                match output {
                    cli::OutputFormat::Json => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "valid": false,
                                "error": e.to_string(),
                            })
                        );
                    }
                    cli::OutputFormat::Text => {
                        eprintln!("Invalid: {e}");
                    }
                }
                std::process::exit(1);
            }
        },

        Command::Graph { file } => {
            let pipeline = parse_pipeline(&file).context("failed to parse pipeline")?;
            display::print_dependency_graph(&pipeline);
        }

        Command::Explain { job, file } => {
            let pipeline = parse_pipeline(&file).context("failed to parse pipeline")?;
            let j = pipeline
                .jobs
                .get(&job)
                .ok_or_else(|| anyhow::anyhow!("job '{job}' not found"))?;

            println!("{}", style(format!("Job: {job}")).bold());
            println!();
            println!("  {}  {}", style("Stage:").dim(), j.stage);
            println!(
                "  {}  {}",
                style("Image:").dim(),
                j.image.as_ref().map(|i| i.name()).unwrap_or("(default)")
            );
            println!("  {}   {:?}", style("When:").dim(), j.when);
            if let Some(timeout) = j.timeout {
                println!("  {} {}s", style("Timeout:").dim(), timeout.as_secs());
            }
            if let Some(retry) = &j.retry {
                println!("  {}  max {}", style("Retry:").dim(), retry.max_retries());
            }
            if let Some(coverage) = &j.coverage {
                println!("  {} {}", style("Coverage:").dim(), coverage);
            }
            if let Some(rg) = &j.resource_group {
                println!("  {} {}", style("Resource group:").dim(), rg);
            }

            if let Some(needs) = &j.needs {
                println!();
                println!("  {}", style("Dependencies:").dim());
                for n in needs {
                    let opt = if n.is_optional() { " (optional)" } else { "" };
                    let art = if !n.wants_artifacts() {
                        " (no artifacts)"
                    } else {
                        ""
                    };
                    println!("    {} {}{}{}", style("→").dim(), n.job_name(), opt, art);
                }
            }

            if let Some(services) = &j.services {
                println!();
                println!("  {}", style("Services:").dim());
                for svc in services {
                    println!("    {} {}", style("●").cyan(), svc.image_name());
                }
            }

            if !j.variables.is_empty() {
                println!();
                println!("  {}", style("Variables:").dim());
                for (k, v) in &j.variables {
                    println!("    {}={}", style(k).green(), v.value());
                }
            }

            if let Some(rules) = &j.rules {
                println!();
                println!("  {}", style("Rules:").dim());
                for rule in rules {
                    if let Some(expr) = &rule.if_expr {
                        let when = rule.when.map(|w| format!(" → {w:?}")).unwrap_or_default();
                        println!("    {} if: {}{}", style("·").dim(), expr, style(when).dim());
                    }
                    if rule.changes.is_some() {
                        println!("    {} changes: [...]", style("·").dim());
                    }
                    if rule.exists.is_some() {
                        println!("    {} exists: [...]", style("·").dim());
                    }
                }
            }

            println!();
            println!("  {}", style("Script:").dim());
            if let Some(before) = &j.before_script {
                for cmd in before {
                    println!("    {} {}", style("(before)").dim(), cmd);
                }
            }
            for cmd in &j.script {
                println!("    {}", cmd);
            }
            if let Some(after) = &j.after_script {
                for cmd in after {
                    println!("    {} {}", style("(after)").dim(), cmd);
                }
            }
        }

        Command::Watch {
            jobs,
            file,
            event,
            interval,
        } => {
            println!(
                "{} Watching {} for changes (every {}s)...",
                style("⟳").cyan().bold(),
                file.display(),
                interval,
            );

            let mut last_modified = file.metadata().ok().and_then(|m| m.modified().ok());

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;

                let current = file.metadata().ok().and_then(|m| m.modified().ok());

                if current != last_modified {
                    last_modified = current;
                    println!();
                    println!(
                        "{} Change detected — re-parsing...",
                        style("⟳").cyan().bold()
                    );

                    // Validate first
                    match parse_pipeline(&file) {
                        Ok(pipeline) => {
                            println!(
                                "{} Valid: {} stages, {} jobs",
                                style("✓").green().bold(),
                                pipeline.stages.len(),
                                pipeline.jobs.len()
                            );

                            // If jobs specified, do a dry run
                            if !jobs.is_empty() {
                                let workdir = std::env::current_dir()?;
                                let predefined = lab_core::model::variables::predefined_variables(
                                    &Config::default(),
                                    "",
                                    "",
                                )
                                .unwrap_or_default();
                                let mut vars = merge_variables(&[&predefined, &pipeline.variables]);
                                if let Some(evt) = &event {
                                    vars.insert(
                                        "CI_PIPELINE_SOURCE".into(),
                                        lab_core::model::variables::VariableValue::Simple(
                                            evt.clone(),
                                        ),
                                    );
                                }
                                let secret_vars =
                                    secrets::load_secrets_file(&workdir).unwrap_or_default();
                                let vars = merge_variables(&[&vars, &secret_vars]);

                                let filter = jobs.clone();
                                match lab_core::planner::build_plan(
                                    &pipeline.stages,
                                    &pipeline.jobs,
                                    &vars,
                                    Some(&filter),
                                    None,
                                ) {
                                    Ok(plan) => {
                                        let n: usize =
                                            plan.stages.iter().map(|s| s.jobs.len()).sum();
                                        println!("  {} {} job(s) matched", style("→").dim(), n);
                                    }
                                    Err(e) => {
                                        eprintln!("{} Plan error: {e}", style("✗").red().bold());
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("{} Parse error: {e}", style("✗").red().bold());
                        }
                    }
                }
            }
        }

        Command::Analyze { file, output } => {
            let pipeline = parse_pipeline(&file).context("failed to parse pipeline")?;
            let findings = lab_core::analyze::analyze(&pipeline);

            match output {
                cli::OutputFormat::Json => {
                    let json = serde_json::to_string_pretty(&findings)
                        .context("failed to serialize findings")?;
                    println!("{json}");
                }
                cli::OutputFormat::Text => {
                    display::print_analysis_report(&findings);
                }
            }
        }

        Command::Report {
            file: _,
            count,
            output,
        } => {
            let workdir = std::env::current_dir()?;
            let (project, _) = lab_core::secrets::detect_gitlab_paths(&workdir)
                .context("failed to detect GitLab project")?;

            let encoded = project.replace('/', "%2F");
            let api_path =
                format!("projects/{encoded}/jobs?per_page={count}&scope[]=success&scope[]=failed");

            let output_data = std::process::Command::new("glab")
                .args(["api", &api_path])
                .output()
                .context("failed to run glab")?;

            if !output_data.status.success() {
                anyhow::bail!(
                    "glab error: {}",
                    String::from_utf8_lossy(&output_data.stderr)
                );
            }

            let jobs: Vec<serde_json::Value> =
                serde_json::from_slice(&output_data.stdout).context("failed to parse jobs")?;

            match output {
                cli::OutputFormat::Json => {
                    let report = build_performance_report(&jobs);
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                cli::OutputFormat::Text => {
                    print_performance_report(&jobs);
                }
            }
        }

        Command::McpServer => {
            mcp::run_server();
        }

        Command::Completions { shell } => {
            let mut cmd = <cli::Cli as clap::CommandFactory>::command();
            clap_complete::generate(shell, &mut cmd, "lab", &mut std::io::stdout());
        }

        Command::Shell { job, file, shell } => {
            logging::init(false);

            let workdir = std::env::current_dir()?;
            let pipeline = parse_pipeline(&file).context("failed to parse pipeline")?;

            let job_config = pipeline
                .jobs
                .get(&job)
                .ok_or_else(|| anyhow::anyhow!("job '{}' not found", job))?;

            // Build variables
            let predefined = lab_core::model::variables::predefined_variables(
                &Config::default(),
                &job,
                &job_config.stage,
            )?;
            let global_vars = merge_variables(&[&pipeline.variables, &predefined]);
            let secret_vars = secrets::load_secrets_file(&workdir).unwrap_or_default();
            let all_vars = merge_variables(&[&global_vars, &secret_vars, &job_config.variables]);

            // Determine image
            let raw_image = job_config
                .image
                .as_ref()
                .map(|i| i.name())
                .unwrap_or("alpine:latest");
            let image = lab_core::model::variables::expand_variables(raw_image, &all_vars);

            let env_map = lab_core::model::variables::to_env_map(&all_vars);

            println!(
                "Starting shell in {} for job {}...",
                style(&image).cyan(),
                style(&job).bold()
            );

            // Use docker run -it directly for true interactive shell
            let shell_cmd = shell.unwrap_or_else(|| "sh".to_string());
            let workdir_str = workdir.to_str().unwrap_or(".");

            let mut cmd = std::process::Command::new("docker");
            cmd.args(["run", "--rm", "-it"]);
            // Run as current user to prevent root-owned files
            let uid = std::process::Command::new("id")
                .args(["-u"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or("1000".into());
            let gid = std::process::Command::new("id")
                .args(["-g"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or("1000".into());
            cmd.args(["-u", &format!("{uid}:{gid}")]);
            cmd.args(["-v", &format!("{workdir_str}:/workspace")]);
            cmd.args(["-w", "/workspace"]);

            for (k, v) in &env_map {
                cmd.args(["-e", &format!("{k}={v}")]);
            }

            cmd.args([&image, &shell_cmd]);

            let status = cmd.status().context("failed to start docker container")?;

            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
        }

        Command::Secrets { action, file } => {
            let workdir = std::env::current_dir()?;

            match action {
                SecretsAction::Pull { group: _ } => {
                    println!("Pulling variables from GitLab...");
                    let result =
                        secrets::pull_secrets_full(&workdir).context("failed to pull secrets")?;

                    secrets::save_secrets_file(&workdir, &result.included)
                        .context("failed to save secrets")?;

                    let secrets_path = secrets::secrets_file_path(&workdir);
                    println!(
                        "\n{} {} secret(s) saved to {}",
                        style("✓").green().bold(),
                        result.included.len(),
                        style(secrets_path.display()).cyan()
                    );

                    if !result.masked_keys.is_empty() {
                        println!(
                            "  {} {} masked variable(s) — will be hidden in job output",
                            style("⊛").cyan(),
                            result.masked_keys.len()
                        );
                    }

                    if !result.skipped_protected.is_empty() {
                        println!(
                            "  {} {} protected variable(s) skipped — branch is not protected",
                            style("⊘").yellow(),
                            result.skipped_protected.len()
                        );
                        for name in &result.skipped_protected {
                            println!("    - {}", style(name).yellow());
                        }
                    }

                    if !result.skipped_hidden.is_empty() {
                        println!(
                            "  {} {} hidden variable(s) — add manually to secrets file",
                            style("⊘").red(),
                            result.skipped_hidden.len()
                        );
                        for name in &result.skipped_hidden {
                            println!("    - {}", style(name).red());
                        }
                    }

                    if !result.skipped_scope.is_empty() {
                        let scoped: Vec<_> = result
                            .skipped_scope
                            .iter()
                            .map(|(k, s)| format!("{k} (scope: {s})"))
                            .collect();
                        println!(
                            "  {} {} environment-scoped variable(s) included (scope noted)",
                            style("◉").dim(),
                            scoped.len()
                        );
                    }
                }

                SecretsAction::Check => {
                    let pipeline = parse_pipeline(&file).context("failed to parse pipeline")?;
                    let available = secrets::load_secrets_file(&workdir).unwrap_or_default();

                    // Merge pipeline vars + secrets for checking
                    let all_vars = merge_variables(&[&pipeline.variables, &available]);

                    let missing = secrets::check_secrets(&pipeline, &all_vars);

                    println!("{}", style("Secret Status:").bold());
                    println!();

                    // Show available secrets
                    for (key, val) in &available {
                        let len = val.value().len();
                        println!(
                            "  {} {}  ({})",
                            style("✓").green().bold(),
                            key,
                            style(format!("{len} chars")).dim()
                        );
                    }

                    // Show missing
                    for m in &missing {
                        let jobs = m.used_in_jobs.join(", ");
                        println!(
                            "  {} {}  {}",
                            style("✗").red().bold(),
                            style(&m.name).red(),
                            style(format!("(used in: {jobs})")).dim()
                        );
                    }

                    println!();
                    if missing.is_empty() {
                        println!("{}", style("All secrets available.").green());
                    } else {
                        println!(
                            "{} missing. Run {} to fetch from GitLab.",
                            style(format!("{} secret(s)", missing.len())).red(),
                            style("lab secrets pull").cyan()
                        );
                    }
                }

                SecretsAction::Init => {
                    let pipeline = parse_pipeline(&file).context("failed to parse pipeline")?;
                    secrets::generate_secrets_example(&pipeline, &workdir)
                        .context("failed to generate secrets example")?;

                    println!("{} Created secrets.env.example", style("✓").green().bold());
                    println!(
                        "\nFor DevOps: run {} to fetch from GitLab",
                        style("lab secrets pull").cyan()
                    );
                }
            }
        }
    }

    Ok(())
}

fn build_performance_report(jobs: &[serde_json::Value]) -> serde_json::Value {
    use std::collections::BTreeMap;

    let mut stats: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut failures: BTreeMap<String, u32> = BTreeMap::new();
    let mut runners: BTreeMap<String, u32> = BTreeMap::new();
    let mut queue_times: Vec<f64> = Vec::new();

    for j in jobs {
        let name = j["name"].as_str().unwrap_or("unknown").to_string();
        let dur = j.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let queued = j
            .get("queued_duration")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let status = j["status"].as_str().unwrap_or("");
        let runner = j
            .get("runner")
            .and_then(|r| r.get("description"))
            .and_then(|d| d.as_str())
            .unwrap_or("unknown");

        stats.entry(name.clone()).or_default().push(dur);
        if status == "failed" {
            *failures.entry(name).or_insert(0) += 1;
        }
        *runners.entry(runner.to_string()).or_insert(0) += 1;
        queue_times.push(queued);
    }

    let job_stats: Vec<serde_json::Value> = stats
        .iter()
        .map(|(name, durations)| {
            let mut sorted = durations.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let avg = sorted.iter().sum::<f64>() / sorted.len() as f64;
            let p90 =
                sorted[((sorted.len() as f64) * 0.9) as usize].min(*sorted.last().unwrap_or(&0.0));
            let fails = failures.get(name).copied().unwrap_or(0);
            serde_json::json!({
                "name": name,
                "runs": sorted.len(),
                "avg_seconds": avg.round(),
                "min_seconds": sorted.first().unwrap_or(&0.0).round(),
                "max_seconds": sorted.last().unwrap_or(&0.0).round(),
                "p90_seconds": p90.round(),
                "failures": fails,
                "failure_rate": format!("{:.0}%", (fails as f64 / sorted.len() as f64) * 100.0),
            })
        })
        .collect();

    let avg_queue = if queue_times.is_empty() {
        0.0
    } else {
        queue_times.iter().sum::<f64>() / queue_times.len() as f64
    };

    serde_json::json!({
        "total_jobs_analyzed": jobs.len(),
        "jobs": job_stats,
        "avg_queue_time_seconds": avg_queue.round(),
        "runner_distribution": runners,
    })
}

fn print_performance_report(jobs: &[serde_json::Value]) {
    use std::collections::BTreeMap;

    let mut stats: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut failures: BTreeMap<String, u32> = BTreeMap::new();
    let mut _total_pipeline_time: f64 = 0.0;

    for j in jobs {
        let name = j["name"].as_str().unwrap_or("unknown").to_string();
        let dur = j.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let status = j["status"].as_str().unwrap_or("");
        stats.entry(name.clone()).or_default().push(dur);
        if status == "failed" {
            *failures.entry(name).or_insert(0) += 1;
        }
        _total_pipeline_time += dur;
    }

    println!(
        "{}",
        console::style("Pipeline Performance Report")
            .bold()
            .underlined()
    );
    println!("  Analyzed {} jobs\n", console::style(jobs.len()).cyan());

    // Table header
    println!(
        "  {:<25} {:>5} {:>8} {:>8} {:>8} {:>8} {:>6}",
        "Job", "Runs", "Avg", "Min", "Max", "P90", "Fail%"
    );
    println!("  {}", "-".repeat(75));

    let mut bottleneck_name = String::new();
    let mut bottleneck_avg = 0.0f64;

    for (name, durations) in &stats {
        let mut sorted = durations.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let avg = sorted.iter().sum::<f64>() / sorted.len() as f64;
        let p90_idx = ((sorted.len() as f64) * 0.9) as usize;
        let p90 = sorted.get(p90_idx).copied().unwrap_or(0.0);
        let fails = failures.get(name).copied().unwrap_or(0);
        let fail_rate = (fails as f64 / sorted.len() as f64) * 100.0;

        let fmt = |s: f64| -> String { format!("{}m {:02}s", (s as u64) / 60, (s as u64) % 60) };

        let avg_style = if avg > 300.0 {
            console::style(fmt(avg)).red()
        } else if avg > 120.0 {
            console::style(fmt(avg)).yellow()
        } else {
            console::style(fmt(avg)).green()
        };

        let fail_style = if fail_rate > 10.0 {
            console::style(format!("{fail_rate:.0}%")).red()
        } else {
            console::style(format!("{fail_rate:.0}%")).dim()
        };

        println!(
            "  {:<25} {:>5} {:>8} {:>8} {:>8} {:>8} {:>6}",
            name,
            sorted.len(),
            avg_style,
            fmt(*sorted.first().unwrap_or(&0.0)),
            fmt(*sorted.last().unwrap_or(&0.0)),
            fmt(p90),
            fail_style,
        );

        if avg > bottleneck_avg {
            bottleneck_avg = avg;
            bottleneck_name = name.clone();
        }
    }

    // Bottleneck analysis
    println!();
    println!("{}", console::style("Bottleneck Analysis:").bold());
    println!(
        "  Slowest job: {} (avg {}m {}s)",
        console::style(&bottleneck_name).red().bold(),
        (bottleneck_avg as u64) / 60,
        (bottleneck_avg as u64) % 60,
    );

    // Suggestions
    println!();
    println!("{}", console::style("Optimization Suggestions:").bold());

    for (name, durations) in &stats {
        let avg = durations.iter().sum::<f64>() / durations.len() as f64;
        let max = durations.iter().cloned().fold(0.0f64, f64::max);
        let variance = max - durations.iter().cloned().fold(f64::MAX, f64::min);

        if avg > 300.0 {
            println!(
                "  {} {} — avg {}m, consider splitting into parallel jobs or using needs: for DAG",
                console::style("!").red().bold(),
                name,
                (avg as u64) / 60
            );
        }
        if variance > avg * 0.5 && durations.len() > 2 {
            println!(
                "  {} {} — high variance ({}m..{}m), check cache hit rate or runner consistency",
                console::style("~").yellow(),
                name,
                (durations.iter().cloned().fold(f64::MAX, f64::min) as u64) / 60,
                (max as u64) / 60,
            );
        }
        let fails = failures.get(name).copied().unwrap_or(0);
        if fails > 0 && (fails as f64 / durations.len() as f64) > 0.1 {
            println!(
                "  {} {} — {:.0}% failure rate, investigate flaky tests or infra issues",
                console::style("✗").red().bold(),
                name,
                (fails as f64 / durations.len() as f64) * 100.0,
            );
        }
    }
}

fn parse_memory_string(s: &str) -> anyhow::Result<i64> {
    let s = s.trim().to_lowercase();
    if let Some(num) = s.strip_suffix('g') {
        Ok(num.parse::<f64>()? as i64 * 1024 * 1024 * 1024)
    } else if let Some(num) = s.strip_suffix('m') {
        Ok(num.parse::<f64>()? as i64 * 1024 * 1024)
    } else if let Some(num) = s.strip_suffix('k') {
        Ok(num.parse::<f64>()? as i64 * 1024)
    } else {
        Ok(s.parse::<i64>()?)
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Get untracked files/dirs in the git working directory (top-level entries only).
fn get_untracked_files(workdir: &std::path::Path) -> std::collections::HashSet<String> {
    std::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "--directory"])
        .current_dir(workdir)
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim_end_matches('/').to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Clean up any leftover lab Docker containers and networks.
async fn cleanup_docker_resources() {
    let _ = std::process::Command::new("docker")
        .args(["ps", "-aq", "--filter", "name=lab-"])
        .output()
        .ok()
        .and_then(|o| {
            let ids = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !ids.is_empty() {
                std::process::Command::new("docker")
                    .args(["rm", "-f"])
                    .args(ids.split_whitespace())
                    .output()
                    .ok()
            } else {
                None
            }
        });

    let _ = std::process::Command::new("docker")
        .args(["network", "ls", "-q", "--filter", "name=lab-"])
        .output()
        .ok()
        .and_then(|o| {
            let ids = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !ids.is_empty() {
                std::process::Command::new("docker")
                    .args(["network", "rm"])
                    .args(ids.split_whitespace())
                    .output()
                    .ok()
            } else {
                None
            }
        });
}
