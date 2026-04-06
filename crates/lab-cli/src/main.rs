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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            job,
            stage,
            file,
            mut variables,
            event,
            tag,
            pull_policy,
            privileged,
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
            verbose,
        } => {
            logging::init(verbose);

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

            let mut config = Config {
                ci_file: file.clone(),
                workdir: workdir.clone(),
                job_filter: job,
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
                manual_mode: if approve_manual {
                    lab_core::config::ManualMode::Approve
                } else if skip_manual {
                    lab_core::config::ManualMode::Skip
                } else {
                    lab_core::config::ManualMode::Prompt
                },
            };

            project_config.apply_to(&mut config);

            let pipeline = parse_pipeline(&file).context("failed to parse pipeline")?;

            // Load secrets
            let secret_vars = if no_secrets {
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
                    eprint!("Continue anyway? [y/N] ");
                    let mut input = String::new();
                    if std::io::stdin().read_line(&mut input).is_ok()
                        && !input.trim().eq_ignore_ascii_case("y")
                    {
                        println!("Aborted.");
                        return Ok(());
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

            if let Some(err) = pipeline_err {
                cleanup_docker_resources().await;
                return Err(err).context("pipeline failed");
            }
        }

        Command::List { file } => {
            let pipeline = parse_pipeline(&file).context("failed to parse pipeline")?;
            display::print_pipeline_list(&pipeline);
        }

        Command::Validate { file } => match parse_pipeline(&file) {
            Ok(pipeline) => {
                println!(
                    "Valid: {} stages, {} jobs",
                    pipeline.stages.len(),
                    pipeline.jobs.len()
                );
            }
            Err(e) => {
                eprintln!("Invalid: {e}");
                std::process::exit(1);
            }
        },

        Command::Graph { file } => {
            let pipeline = parse_pipeline(&file).context("failed to parse pipeline")?;
            display::print_dependency_graph(&pipeline);
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

                    println!(
                        "\n{} {} secret(s) saved to .lab/secrets.env",
                        style("✓").green().bold(),
                        result.included.len()
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
                            "  {} {} hidden variable(s) — add manually to .lab/secrets.env",
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

                    println!(
                        "{} Created .lab/secrets.env.example",
                        style("✓").green().bold()
                    );
                    println!(
                        "{} Created .lab/secrets.env (add your secrets here)",
                        style("✓").green().bold()
                    );
                    println!(
                        "\nFor DevOps: run {} to fetch from GitLab",
                        style("lab secrets pull").cyan()
                    );
                    println!("For Devs:   copy .lab/secrets.env.example to .lab/secrets.env");
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
