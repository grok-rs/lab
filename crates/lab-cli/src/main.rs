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
            variables,
            pull_policy,
            privileged,
            no_artifacts,
            no_cache,
            platforms,
            max_parallel,
            pull_secrets,
            no_secrets,
            secrets_file,
            dry_run,
            no_preflight,
            verbose,
        } => {
            logging::init(verbose);

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

            // Evaluate workflow:rules
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
                        _ => {}
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
