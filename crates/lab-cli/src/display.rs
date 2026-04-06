use console::style;
use lab_core::analyze::{Finding, Severity};
use lab_core::model::pipeline::{Pipeline, Plan};
use lab_core::model::variables::Variables;
use lab_core::runner::output::{JobResult, JobStatus, PipelineResult};

/// Print pipeline jobs grouped by stage.
pub fn print_pipeline_list(pipeline: &Pipeline) {
    println!("{}", style("Stages and jobs:").bold());
    println!();

    for stage_name in &pipeline.stages {
        let jobs_in_stage: Vec<&str> = pipeline
            .jobs
            .iter()
            .filter(|(_, job)| job.stage == *stage_name)
            .map(|(name, _)| name.as_str())
            .collect();

        if jobs_in_stage.is_empty() {
            continue;
        }

        println!("  {}", style(format!("Stage: {stage_name}")).cyan().bold());
        for job_name in &jobs_in_stage {
            let job = &pipeline.jobs[*job_name];
            let image = job.image.as_ref().map(|i| i.name()).unwrap_or("(default)");
            let needs = job
                .needs
                .as_ref()
                .map(|n| {
                    n.iter()
                        .map(|need| need.job_name())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();

            print!(
                "    {} {}  {}",
                style("-").dim(),
                job_name,
                style(format!("[{image}]")).dim()
            );
            if !needs.is_empty() {
                print!("  {}", style(format!("(needs: {needs})")).yellow());
            }
            println!();
        }
        println!();
    }
}

/// Print the dependency graph in a simple text format.
pub fn print_dependency_graph(pipeline: &Pipeline) {
    use std::collections::{HashMap, HashSet};

    // Group jobs by stage, preserving stage order
    let mut stage_jobs: Vec<(&str, Vec<&str>)> = Vec::new();
    for stage_name in &pipeline.stages {
        let jobs: Vec<&str> = pipeline
            .jobs
            .iter()
            .filter(|(_, j)| j.stage == *stage_name)
            .map(|(name, _)| name.as_str())
            .collect();
        if !jobs.is_empty() {
            stage_jobs.push((stage_name, jobs));
        }
    }

    if stage_jobs.is_empty() {
        println!("No jobs in pipeline.");
        return;
    }

    // Collect dependency edges: from → [to, ...]
    let mut deps_from: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut deps_to: HashMap<&str, Vec<&str>> = HashMap::new();
    for (name, job) in &pipeline.jobs {
        if let Some(needs) = &job.needs {
            for need in needs {
                deps_from
                    .entry(need.job_name())
                    .or_default()
                    .push(name.as_str());
                deps_to
                    .entry(name.as_str())
                    .or_default()
                    .push(need.job_name());
            }
        }
    }

    // Jobs that feed into the next stage
    let feeds_next: HashSet<&str> = deps_from.keys().copied().collect();
    // Jobs that receive from previous stage
    let receives: HashSet<&str> = deps_to.keys().copied().collect();

    // Max job name length for consistent box widths
    let max_name_len = pipeline.jobs.keys().map(|n| n.len()).max().unwrap_or(10);
    let box_inner = max_name_len + 2; // padding inside box
    let box_outer = box_inner + 2; // + border chars

    for (stage_idx, (stage_name, jobs)) in stage_jobs.iter().enumerate() {
        // ── Stage header ──
        let label = format!(" {} ", stage_name);
        let rule_len =
            (box_outer * jobs.len().min(4) + (jobs.len().min(4) - 1) * 3).max(label.len() + 4);
        let left = (rule_len - label.len()) / 2;
        let right = rule_len - label.len() - left;
        println!(
            "  {}{}{}  {}",
            style("─".repeat(left)).dim(),
            style(&label).bold().cyan(),
            style("─".repeat(right)).dim(),
            style(if jobs.len() > 1 { "parallel" } else { "single" }).dim(),
        );
        println!();

        // ── Job boxes ──
        // Render in rows of up to 4 jobs
        for chunk in jobs.chunks(4) {
            // Top border
            let top: String = chunk
                .iter()
                .map(|_| format!("  ┌{}┐", "─".repeat(box_inner)))
                .collect::<Vec<_>>()
                .join(" ");
            println!("{top}");

            // Job name + annotations
            let mid: String = chunk
                .iter()
                .map(|&job_name| {
                    let job = &pipeline.jobs[job_name];
                    // Pick icon based on when/type
                    let icon = if job.when == lab_core::model::job::When::Manual {
                        "⏸"
                    } else if feeds_next.contains(job_name) && receives.contains(job_name) {
                        "◆"
                    } else if feeds_next.contains(job_name) {
                        "●"
                    } else if receives.contains(job_name) {
                        "◀"
                    } else {
                        "○"
                    };
                    let padded = format!("{} {:width$}", icon, job_name, width = max_name_len);
                    let styled = if receives.contains(job_name) && feeds_next.contains(job_name) {
                        format!("{}", style(&padded).cyan())
                    } else if feeds_next.contains(job_name) {
                        format!("{}", style(&padded).green())
                    } else if receives.contains(job_name) {
                        format!("{}", style(&padded).yellow())
                    } else if job.when == lab_core::model::job::When::Manual {
                        format!("{}", style(&padded).dim())
                    } else {
                        format!("{}", style(&padded).white())
                    };
                    format!("  │{styled}│")
                })
                .collect::<Vec<_>>()
                .join(" ");
            println!("{mid}");

            // Image line
            let img_line: String = chunk
                .iter()
                .map(|&job_name| {
                    let job = &pipeline.jobs[job_name];
                    let img = job.image.as_ref().map(|i| i.name()).unwrap_or("-");
                    // Truncate long image names
                    let truncated = if img.len() > max_name_len {
                        format!("{}…", &img[..max_name_len - 1])
                    } else {
                        img.to_string()
                    };
                    let padded = format!("  {:width$}", truncated, width = max_name_len);
                    format!("  │{}│", style(&padded).dim())
                })
                .collect::<Vec<_>>()
                .join(" ");
            println!("{img_line}");

            // Bottom border
            let bot: String = chunk
                .iter()
                .map(|_| format!("  └{}┘", "─".repeat(box_inner)))
                .collect::<Vec<_>>()
                .join(" ");
            println!("{bot}");
        }

        // ── Dependency arrows between this stage and the next ──
        if stage_idx < stage_jobs.len() - 1 {
            // Find jobs in this stage that feed into the next stage
            let connectors: Vec<&str> = jobs
                .iter()
                .filter(|j| feeds_next.contains(**j))
                .copied()
                .collect();

            if connectors.is_empty() {
                // Pure stage ordering, no explicit needs
                println!("  {}", style("    │").dim());
                println!("  {}", style("    ▼").dim());
            } else {
                println!();
                for &from in &connectors {
                    if let Some(targets) = deps_from.get(from) {
                        let target_list = targets
                            .iter()
                            .map(|t| format!("{}", style(t).yellow()))
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!(
                            "  {} {} {} {}",
                            style("    ╰──").dim(),
                            style(from).green(),
                            style("→").bold(),
                            target_list,
                        );
                    }
                }
                println!("  {}", style("    │").dim());
                println!("  {}", style("    ▼").dim());
            }
            println!();
        }
    }

    // ── Legend ──
    println!();
    println!(
        "  {} {}  {} {}  {} {}  {} {}",
        style("●").green(),
        style("upstream").dim(),
        style("◀").yellow(),
        style("has deps").dim(),
        style("◆").cyan(),
        style("both").dim(),
        style("⏸").white(),
        style("manual").dim(),
    );
}

/// Print a summary of the pipeline run with colored status indicators.
pub fn print_pipeline_summary(result: &PipelineResult) {
    let jobs = result.jobs();
    if jobs.is_empty() {
        return;
    }

    println!();
    println!("{}", style("Pipeline Summary").bold().underlined());
    println!();

    for job in &jobs {
        let status_icon = match job.status {
            JobStatus::Success => style("✓").green().bold(),
            JobStatus::Failed => style("✗").red().bold(),
            JobStatus::AllowedFailure => style("!").yellow().bold(),
        };

        let duration = format_duration(job.duration);
        let name_style = match job.status {
            JobStatus::Success => style(job.name.as_str()).green(),
            JobStatus::Failed => style(job.name.as_str()).red(),
            JobStatus::AllowedFailure => style(job.name.as_str()).yellow(),
        };

        let coverage_str = job
            .coverage
            .map(|c| format!("  {}%", style(format!("{c:.1}")).cyan()))
            .unwrap_or_default();

        println!(
            "  {status_icon} {name_style}  {}  {duration}{coverage_str}",
            style(format!("[{}]", job.stage)).dim(),
        );
    }

    let total = result.total_duration();
    let passed = jobs
        .iter()
        .filter(|j| j.status == JobStatus::Success)
        .count();
    let failed = jobs
        .iter()
        .filter(|j| j.status == JobStatus::Failed)
        .count();
    let allowed = jobs
        .iter()
        .filter(|j| j.status == JobStatus::AllowedFailure)
        .count();

    // Timeline visualization
    if jobs.len() > 1 {
        print_timeline(&jobs, total);
    }

    println!();
    if failed == 0 {
        print!("{}", style("Pipeline passed").green().bold());
    } else {
        print!("{}", style("Pipeline failed").red().bold());
    }
    print!(" — {passed} passed");
    if failed > 0 {
        print!(", {}", style(format!("{failed} failed")).red());
    }
    if allowed > 0 {
        print!(
            ", {}",
            style(format!("{allowed} allowed failures")).yellow()
        );
    }
    println!(" in {}", format_duration(total));
}

/// Print a Gantt-style timeline showing job execution over time.
fn print_timeline(jobs: &[JobResult], total: std::time::Duration) {
    let total_secs = total.as_secs_f64().max(1.0);
    let bar_width: usize = 50;
    let max_name = jobs.iter().map(|j| j.name.len()).max().unwrap_or(10);

    println!();
    println!("{}", style("Timeline:").bold());
    println!();

    for job in jobs {
        let start_frac = job.start_offset.as_secs_f64() / total_secs;
        let dur_frac = job.duration.as_secs_f64() / total_secs;

        let start_col = (start_frac * bar_width as f64) as usize;
        let dur_cols = ((dur_frac * bar_width as f64) as usize).max(1);
        let end_col = (start_col + dur_cols).min(bar_width);

        let bar_char = match job.status {
            JobStatus::Success => "█",
            JobStatus::Failed => "█",
            JobStatus::AllowedFailure => "▒",
        };

        let bar: String = (0..bar_width)
            .map(|i| {
                if i >= start_col && i < end_col {
                    bar_char
                } else {
                    "·"
                }
            })
            .collect();

        let colored_bar = match job.status {
            JobStatus::Success => format!("{}", style(&bar).green()),
            JobStatus::Failed => format!("{}", style(&bar).red()),
            JobStatus::AllowedFailure => format!("{}", style(&bar).yellow()),
        };

        println!(
            "  {:width$}  │{colored_bar}│ {}",
            job.name,
            style(format_duration(job.duration)).dim(),
            width = max_name,
        );
    }

    // Time axis
    let axis_label = format!(
        "  {:width$}  │{:<bar_width$}│",
        "",
        format!(
            "0s{}{}",
            " ".repeat(bar_width / 2 - 4),
            format_duration(total)
        ),
        width = max_name,
        bar_width = bar_width,
    );
    println!("{}", style(axis_label).dim());
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// Print dry-run execution plan without running containers.
pub fn print_dry_run(
    plan: &Plan,
    _pipeline: &Pipeline,
    global_vars: &Variables,
    secret_vars: &Variables,
) {
    let total_jobs: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    let var_re = regex::Regex::new(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?").unwrap();

    println!("{}", style("Dry Run — Execution Plan").bold().underlined());
    println!();
    println!(
        "Stages: {}",
        plan.stages
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(" → ")
    );
    println!();

    for stage in &plan.stages {
        println!(
            "  {}",
            style(format!("Stage: {}", stage.name)).cyan().bold()
        );

        for pj in &stage.jobs {
            let job = &pj.job;
            let image = job.image.as_ref().map(|i| i.name()).unwrap_or("(default)");

            let needs = job
                .needs
                .as_ref()
                .map(|n| {
                    format!(
                        "  needs:[{}]",
                        n.iter()
                            .map(|nd| nd.job_name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .unwrap_or_default();

            let timeout = job
                .timeout
                .map(|d| format!("  timeout:{}s", d.as_secs()))
                .unwrap_or_default();

            println!(
                "    {} {}  {}{}{}",
                style("◦").dim(),
                style(&pj.name).white().bold(),
                style(format!("[{image}]")).dim(),
                style(&needs).yellow(),
                style(&timeout).dim(),
            );

            // Show services
            if let Some(services) = &job.services {
                if !services.is_empty() {
                    let svc_names: Vec<&str> = services.iter().map(|s| s.image_name()).collect();
                    println!("      Services: {}", style(svc_names.join(", ")).dim());
                }
            }

            // Check which secrets this job needs
            let job_script_text =
                job.script.join(" ") + &job.before_script.as_deref().unwrap_or(&[]).join(" ");
            let mut needed_secrets = Vec::new();
            let mut missing_secrets = Vec::new();

            for cap in var_re.captures_iter(&job_script_text) {
                let var_name = &cap[1];
                if !global_vars.contains_key(var_name)
                    && !job.variables.contains_key(var_name)
                    && !var_name.starts_with("CI_")
                    && !var_name.starts_with("GITLAB_")
                {
                    if secret_vars.contains_key(var_name) {
                        needed_secrets.push(var_name.to_string());
                    } else {
                        missing_secrets.push(var_name.to_string());
                    }
                }
            }

            needed_secrets.sort();
            needed_secrets.dedup();
            missing_secrets.sort();
            missing_secrets.dedup();

            if !needed_secrets.is_empty() || !missing_secrets.is_empty() {
                let mut parts = Vec::new();
                for s in &needed_secrets {
                    parts.push(format!("{} {}", style("✓").green(), s));
                }
                for s in &missing_secrets {
                    parts.push(format!("{} {}", style("✗").red(), s));
                }
                println!("      Secrets: {}", parts.join("  "));
            }

            println!("      Script: {} command(s)", style(job.script.len()).dim());
        }
        println!();
    }

    println!(
        "{} job(s), {} secret(s) loaded",
        style(total_jobs).bold(),
        style(secret_vars.len()).cyan()
    );
}

/// Pre-flight check: report which jobs have missing variables.
/// Returns the number of jobs with missing variables.
pub fn print_preflight_report(plan: &Plan, global_vars: &Variables) -> usize {
    let var_re = regex::Regex::new(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?").unwrap();
    let skip_prefixes = [
        "CI_", "GITLAB_", "DOCKER_", "HOME", "PATH", "USER", "SHELL", "PWD", "TERM",
    ];

    println!("{}", style("Pre-flight variable check:").bold());
    println!();

    let mut jobs_with_missing = 0;

    for stage in &plan.stages {
        for pj in &stage.jobs {
            let job = &pj.job;

            // Collect all text where variables might be referenced
            let mut texts = job.script.clone();
            if let Some(bs) = &job.before_script {
                texts.extend(bs.iter().cloned());
            }
            if let Some(a_s) = &job.after_script {
                texts.extend(a_s.iter().cloned());
            }
            let combined = texts.join("\n");

            // Find referenced variables
            let mut missing = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for cap in var_re.captures_iter(&combined) {
                let var_name = &cap[1];
                if seen.contains(var_name) {
                    continue;
                }
                seen.insert(var_name.to_string());

                // Skip predefined/system vars
                if skip_prefixes.iter().any(|p| var_name.starts_with(p)) {
                    continue;
                }
                // Skip vars defined in the pipeline or job
                if global_vars.contains_key(var_name) || job.variables.contains_key(var_name) {
                    continue;
                }
                // Skip vars that look like they're computed in-script
                // (assigned via VAR=... or export VAR=... in the same script)
                let assign_pattern = format!("{}=", var_name);
                let export_pattern = format!("export {}=", var_name);
                if combined.contains(&assign_pattern) || combined.contains(&export_pattern) {
                    continue;
                }

                missing.push(var_name.to_string());
            }

            if missing.is_empty() {
                println!(
                    "  {} {} — all variables available",
                    style("✓").green().bold(),
                    pj.name
                );
            } else {
                jobs_with_missing += 1;
                println!(
                    "  {} {} — {} missing variable(s):",
                    style("✗").red().bold(),
                    style(&pj.name).red(),
                    missing.len()
                );
                for var in &missing {
                    println!("      {} {}", style("·").dim(), style(var).yellow());
                }
            }
        }
    }

    println!();

    if jobs_with_missing > 0 {
        println!(
            "{} job(s) may fail due to missing variables.",
            style(jobs_with_missing).red().bold()
        );
        println!();
    }

    jobs_with_missing
}

/// Print pipeline analysis report with colored severity indicators.
pub fn print_analysis_report(findings: &[Finding]) {
    if findings.is_empty() {
        println!(
            "{}",
            style("No issues found — pipeline looks good!")
                .green()
                .bold()
        );
        return;
    }

    let critical = findings
        .iter()
        .filter(|f| f.severity == Severity::Critical)
        .count();
    let warnings = findings
        .iter()
        .filter(|f| f.severity == Severity::Warning)
        .count();
    let info = findings
        .iter()
        .filter(|f| f.severity == Severity::Info)
        .count();

    println!(
        "{} found {} issue(s): {} critical, {} warnings, {} info\n",
        style("Pipeline Analysis").bold().underlined(),
        findings.len(),
        style(critical).red().bold(),
        style(warnings).yellow().bold(),
        style(info).cyan(),
    );

    for finding in findings {
        let icon = match finding.severity {
            Severity::Critical => style("CRITICAL").red().bold(),
            Severity::Warning => style("WARNING ").yellow().bold(),
            Severity::Info => style("INFO    ").cyan(),
        };

        let job_str = finding
            .job
            .as_ref()
            .map(|j| format!(" [{}]", j))
            .unwrap_or_default();

        println!("  {icon} {}{job_str}", style(&finding.rule).dim(),);
        println!("         {}", finding.message);
        println!(
            "         {} {}\n",
            style("Fix:").green(),
            finding.suggestion
        );
    }

    if critical > 0 {
        println!(
            "{}",
            style(format!(
                "{critical} critical issue(s) should be fixed before deploying."
            ))
            .red()
            .bold()
        );
    }
}

/// Print actionable suggestions for common pipeline failures.
pub fn print_error_suggestions(err: &lab_core::error::LabError) {
    let suggestions: Vec<(&str, &str)> = match err {
        lab_core::error::LabError::ContainerFailed { code } => match code {
            127 => vec![(
                "command not found",
                "the image may be missing required tools — check the job's image or before_script",
            )],
            126 => vec![(
                "permission denied",
                "script may not be executable — check file permissions or use `sh script.sh`",
            )],
            137 => vec![(
                "killed (OOM or timeout)",
                "container was killed — increase timeout or check memory usage",
            )],
            _ => vec![],
        },
        lab_core::error::LabError::Docker(e) => {
            let msg = e.to_string();
            if msg.contains("No such image") || msg.contains("not found") {
                vec![(
                    "image not found",
                    "try --pull-policy always or check image name",
                )]
            } else if msg.contains("permission denied") || msg.contains("connect") {
                vec![(
                    "docker access error",
                    "ensure Docker is running and your user is in the docker group",
                )]
            } else {
                vec![]
            }
        }
        lab_core::error::LabError::Other(msg) => {
            if msg.contains("glab") {
                vec![(
                    "glab error",
                    "ensure glab is installed and authenticated: glab auth login",
                )]
            } else {
                vec![]
            }
        }
        _ => vec![],
    };

    for (issue, fix) in suggestions {
        println!("  {} {} — {}", style("tip:").cyan().bold(), issue, fix,);
    }
}
