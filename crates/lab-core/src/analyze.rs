//! Pipeline static analysis — checks for security, performance, and best practices.
//!
//! These are deterministic rules that don't require AI. They catch common mistakes
//! and suggest improvements based on DevOps best practices.

use serde::Serialize;

use crate::model::job::{Job, When};
use crate::model::pipeline::Pipeline;

/// Severity of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

/// Category of a finding.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Security,
    Performance,
    BestPractice,
}

/// A single analysis finding.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub category: Category,
    pub job: Option<String>,
    pub rule: String,
    pub message: String,
    pub suggestion: String,
}

/// Run all analysis rules on a pipeline.
pub fn analyze(pipeline: &Pipeline) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Global checks
    check_workflow_rules(pipeline, &mut findings);
    check_stages_optimization(pipeline, &mut findings);

    // Per-job checks
    for (name, job) in &pipeline.jobs {
        check_image_tag(name, job, &mut findings);
        check_missing_timeout(name, job, &mut findings);
        check_missing_retry(name, job, &mut findings);
        check_missing_cache(name, job, &mut findings);
        check_missing_interruptible(name, job, &mut findings);
        check_deploy_without_rules(name, job, &mut findings);
        check_deploy_allow_failure(name, job, &mut findings);
        check_artifact_expiry(name, job, &mut findings);
        check_secret_in_variables(name, job, &mut findings);
        check_dag_optimization(name, job, pipeline, &mut findings);
        check_missing_coverage(name, job, &mut findings);
        check_dind_security(name, job, &mut findings);
        check_duplicate_scripts(name, job, pipeline, &mut findings);
        check_manual_without_confirmation(name, job, &mut findings);
    }

    // Sort by severity (critical first)
    findings.sort_by_key(|f| match f.severity {
        Severity::Critical => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    });

    findings
}

// ============================================================
// Global checks
// ============================================================

fn check_workflow_rules(pipeline: &Pipeline, findings: &mut Vec<Finding>) {
    if pipeline.workflow.is_none() {
        findings.push(Finding {
            severity: Severity::Warning,
            category: Category::BestPractice,
            job: None,
            rule: "missing-workflow-rules".into(),
            message: "No workflow:rules defined — pipelines run for every event".into(),
            suggestion: "Add workflow:rules to control when pipelines are created. \
                         This prevents duplicate pipelines for MRs."
                .into(),
        });
    }
}

fn check_stages_optimization(pipeline: &Pipeline, findings: &mut Vec<Finding>) {
    // Check if DAG (needs:) could improve parallelism
    let mut jobs_without_needs = 0;
    let mut total_jobs = 0;
    for job in pipeline.jobs.values() {
        total_jobs += 1;
        if job.needs.is_none() {
            jobs_without_needs += 1;
        }
    }
    if total_jobs > 3 && jobs_without_needs == total_jobs {
        findings.push(Finding {
            severity: Severity::Info,
            category: Category::Performance,
            job: None,
            rule: "no-dag-needs".into(),
            message: format!("All {total_jobs} jobs use stage ordering — no needs: keyword used"),
            suggestion: "Use needs: to create a DAG. Jobs can start as soon as \
                         their dependencies finish, not waiting for entire stages."
                .into(),
        });
    }
}

// ============================================================
// Per-job checks
// ============================================================

fn check_image_tag(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    let image = job
        .image
        .as_ref()
        .map(|i| i.name().to_string())
        .unwrap_or_default();

    if image.is_empty() {
        return;
    }

    if !image.contains(':') || image.ends_with(":latest") {
        findings.push(Finding {
            severity: Severity::Warning,
            category: Category::Security,
            job: Some(name.into()),
            rule: "unpinned-image-tag".into(),
            message: format!(
                "Image '{image}' uses :latest or no tag — builds are non-deterministic"
            ),
            suggestion: "Pin to a specific version (e.g., node:20-alpine) or use SHA256 digest"
                .into(),
        });
    }

    // Check for full OS images
    if (image.starts_with("node:") || image.starts_with("python:") || image.starts_with("ruby:"))
        && !image.contains("alpine")
        && !image.contains("slim")
        && !image.contains("distroless")
    {
        findings.push(Finding {
            severity: Severity::Info,
            category: Category::Performance,
            job: Some(name.into()),
            rule: "large-base-image".into(),
            message: format!(
                "Image '{image}' is a full OS image — larger attack surface and slower pulls"
            ),
            suggestion: "Use alpine or slim variant (e.g., node:20-alpine) for smaller images"
                .into(),
        });
    }
}

fn check_missing_timeout(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    if job.timeout.is_none() && (job.stage == "build" || job.stage == "deploy") {
        findings.push(Finding {
            severity: Severity::Info,
            category: Category::BestPractice,
            job: Some(name.into()),
            rule: "missing-timeout".into(),
            message: format!("Job '{name}' has no timeout — could hang indefinitely"),
            suggestion: "Add timeout: (e.g., timeout: 30m) to prevent stuck jobs".into(),
        });
    }
}

fn check_missing_retry(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    if job.retry.is_none() && (job.stage == "build" || job.stage == "deploy") {
        findings.push(Finding {
            severity: Severity::Info,
            category: Category::BestPractice,
            job: Some(name.into()),
            rule: "missing-retry".into(),
            message: format!(
                "Job '{name}' has no retry — transient failures will stop the pipeline"
            ),
            suggestion: "Add retry: {max: 2, when: [runner_system_failure]} for resilience".into(),
        });
    }
}

fn check_missing_cache(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    if job.cache.is_none() {
        // Check if script installs dependencies
        let script_text = job.script.join(" ").to_lowercase();
        let installs_deps = script_text.contains("npm ci")
            || script_text.contains("npm install")
            || script_text.contains("yarn install")
            || script_text.contains("pnpm install")
            || script_text.contains("pip install")
            || script_text.contains("bundle install")
            || script_text.contains("cargo build");

        if installs_deps {
            findings.push(Finding {
                severity: Severity::Warning,
                category: Category::Performance,
                job: Some(name.into()),
                rule: "missing-cache".into(),
                message: format!("Job '{name}' installs dependencies but has no cache"),
                suggestion: "Add cache: with key based on lockfile to speed up builds".into(),
            });
        }
    }
}

fn check_missing_interruptible(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    if job.interruptible.is_none()
        && (job.stage == "test" || job.stage == "lint" || job.stage == "quality")
    {
        findings.push(Finding {
            severity: Severity::Info,
            category: Category::Performance,
            job: Some(name.into()),
            rule: "missing-interruptible".into(),
            message: format!("Job '{name}' is not marked interruptible — wastes resources on superseded MRs"),
            suggestion: "Add interruptible: true to test/lint jobs so they're canceled on new pushes".into(),
        });
    }
}

fn check_deploy_without_rules(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    if job.stage.contains("deploy") && job.rules.is_none() && job.when == When::OnSuccess {
        findings.push(Finding {
            severity: Severity::Critical,
            category: Category::Security,
            job: Some(name.into()),
            rule: "deploy-without-rules".into(),
            message: format!("Deploy job '{name}' has no rules: — runs on every pipeline"),
            suggestion: "Add rules: to restrict deploys to specific branches (e.g., main only)"
                .into(),
        });
    }
}

fn check_deploy_allow_failure(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    if job.stage.contains("deploy") && job.allow_failure.is_allowed(1) {
        findings.push(Finding {
            severity: Severity::Warning,
            category: Category::Security,
            job: Some(name.into()),
            rule: "deploy-allow-failure".into(),
            message: format!(
                "Deploy job '{name}' has allow_failure: true — failed deploys won't block pipeline"
            ),
            suggestion: "Remove allow_failure from deploy jobs to catch deployment failures".into(),
        });
    }
}

fn check_artifact_expiry(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    if let Some(artifacts) = &job.artifacts {
        if !artifacts.paths.is_empty() && artifacts.expire_in.is_none() {
            findings.push(Finding {
                severity: Severity::Info,
                category: Category::Performance,
                job: Some(name.into()),
                rule: "artifact-no-expiry".into(),
                message: format!(
                    "Job '{name}' artifacts have no expire_in — use storage indefinitely"
                ),
                suggestion: "Add expire_in: (e.g., 1 week) to automatically clean up old artifacts"
                    .into(),
            });
        }
    }
}

fn check_secret_in_variables(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    let secret_patterns = [
        "PASSWORD",
        "SECRET",
        "TOKEN",
        "KEY",
        "CREDENTIAL",
        "AUTH",
        "PRIVATE",
        "API_KEY",
        "ACCESS_KEY",
    ];

    for (var_name, var_val) in &job.variables {
        let upper = var_name.to_uppercase();
        let value = var_val.value();
        if secret_patterns.iter().any(|p| upper.contains(p))
            && !value.is_empty()
            && !value.starts_with('$')
        {
            findings.push(Finding {
                severity: Severity::Critical,
                category: Category::Security,
                job: Some(name.into()),
                rule: "hardcoded-secret".into(),
                message: format!(
                    "Variable '{var_name}' in job '{name}' looks like a hardcoded secret"
                ),
                suggestion: "Move secrets to CI/CD variables (Settings > CI/CD > Variables) \
                             with masked and protected flags"
                    .into(),
            });
        }
    }
}

fn check_dag_optimization(name: &str, job: &Job, pipeline: &Pipeline, findings: &mut Vec<Finding>) {
    // If job has no needs: and is not in the first stage, it might benefit from DAG
    if job.needs.is_none() && !pipeline.stages.is_empty() {
        let first_stage = &pipeline.stages[0];
        if job.stage != *first_stage && pipeline.jobs.len() > 4 {
            // Check if this job only actually depends on one or two jobs
            // (heuristic: look for artifact references)
            if job.dependencies.is_some() {
                findings.push(Finding {
                    severity: Severity::Info,
                    category: Category::Performance,
                    job: Some(name.into()),
                    rule: "use-needs-instead-of-dependencies".into(),
                    message: format!(
                        "Job '{name}' uses dependencies: but not needs: — it waits for entire previous stage"
                    ),
                    suggestion: "Replace dependencies: with needs: to start the job as soon as its dependencies finish".into(),
                });
            }
        }
    }
}

fn check_missing_coverage(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    let script_text = job.script.join(" ").to_lowercase();
    let runs_tests = script_text.contains("rspec")
        || script_text.contains("pytest")
        || script_text.contains("jest")
        || script_text.contains("vitest")
        || script_text.contains("npm test")
        || script_text.contains("cargo test")
        || script_text.contains("go test");

    if runs_tests && job.coverage.is_none() {
        findings.push(Finding {
            severity: Severity::Info,
            category: Category::BestPractice,
            job: Some(name.into()),
            rule: "missing-coverage".into(),
            message: format!("Job '{name}' runs tests but has no coverage: regex"),
            suggestion: "Add coverage: '/regex/' to extract coverage percentage from output".into(),
        });
    }
}

fn check_dind_security(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    let has_dind = job
        .services
        .as_ref()
        .is_some_and(|s| s.iter().any(|svc| svc.image_name().contains("dind")));

    if has_dind {
        let has_tls = job.variables.contains_key("DOCKER_TLS_VERIFY");
        if !has_tls {
            findings.push(Finding {
                severity: Severity::Warning,
                category: Category::Security,
                job: Some(name.into()),
                rule: "dind-without-tls".into(),
                message: format!("Job '{name}' uses Docker-in-Docker without TLS"),
                suggestion:
                    "Set DOCKER_TLS_VERIFY: '1' and DOCKER_CERT_PATH: /certs/client for secure DinD"
                        .into(),
            });
        }
    }
}

fn check_duplicate_scripts(
    name: &str,
    job: &Job,
    pipeline: &Pipeline,
    findings: &mut Vec<Finding>,
) {
    if job.before_script.is_none() && job.script.len() > 3 {
        // Check if the same script block appears in another job
        for (other_name, other_job) in &pipeline.jobs {
            if other_name == name {
                continue;
            }
            if other_job.script == job.script && other_job.before_script.is_none() {
                findings.push(Finding {
                    severity: Severity::Info,
                    category: Category::BestPractice,
                    job: Some(name.into()),
                    rule: "duplicate-script".into(),
                    message: format!("Jobs '{name}' and '{other_name}' have identical scripts"),
                    suggestion:
                        "Extract common scripts into a hidden job (.template) and use extends:"
                            .into(),
                });
                break; // Only report once per job
            }
        }
    }
}

fn check_manual_without_confirmation(name: &str, job: &Job, findings: &mut Vec<Finding>) {
    if job.when == When::Manual && job.stage.contains("deploy") && job.manual_confirmation.is_none()
    {
        findings.push(Finding {
            severity: Severity::Info,
            category: Category::BestPractice,
            job: Some(name.into()),
            rule: "manual-deploy-no-confirmation".into(),
            message: format!("Manual deploy job '{name}' has no confirmation message"),
            suggestion: "Add manual_confirmation: 'Are you sure you want to deploy to production?'"
                .into(),
        });
    }
}
