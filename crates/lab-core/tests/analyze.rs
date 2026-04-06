use indexmap::IndexMap;
use lab_core::analyze::{Severity, analyze};
use lab_core::model::job::{AllowFailure, ArtifactConfig, ImageConfig, Job, ServiceConfig, When};
use lab_core::model::pipeline::{Pipeline, WorkflowConfig};
use lab_core::model::rules::Rule;
use lab_core::model::variables::{VariableValue, Variables};

fn empty_pipeline() -> Pipeline {
    Pipeline {
        stages: vec!["test".into()],
        variables: Variables::new(),
        defaults: Default::default(),
        jobs: IndexMap::new(),
        workflow: None,
    }
}

fn make_job(name: &str, stage: &str) -> (String, Job) {
    let yaml = format!(
        r#"
        stage: {stage}
        script:
          - echo hello
    "#
    );
    let mut job: Job = serde_yaml::from_str(&yaml).unwrap();
    job.stage = stage.to_string();
    (name.to_string(), job)
}

// ============================================================
// Global checks
// ============================================================

#[test]
fn missing_workflow_rules() {
    let pipeline = empty_pipeline();
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "missing-workflow-rules"));
}

#[test]
fn workflow_rules_present_no_warning() {
    let mut pipeline = empty_pipeline();
    pipeline.workflow = Some(WorkflowConfig {
        rules: vec![],
        name: None,
        auto_cancel: None,
    });
    let findings = analyze(&pipeline);
    assert!(!findings.iter().any(|f| f.rule == "missing-workflow-rules"));
}

#[test]
fn no_dag_needs_warning() {
    let mut pipeline = empty_pipeline();
    pipeline.stages = vec!["build".into(), "test".into()];
    for i in 0..5 {
        let (name, job) = make_job(&format!("job-{i}"), "build");
        pipeline.jobs.insert(name, job);
    }
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "no-dag-needs"));
}

#[test]
fn dag_needs_no_warning_when_few_jobs() {
    let mut pipeline = empty_pipeline();
    let (name, job) = make_job("build", "build");
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(!findings.iter().any(|f| f.rule == "no-dag-needs"));
}

// ============================================================
// Image checks
// ============================================================

#[test]
fn unpinned_image_tag_latest() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("build", "build");
    job.image = Some(ImageConfig::Simple("node:latest".into()));
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "unpinned-image-tag"));
}

#[test]
fn unpinned_image_tag_no_tag() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("build", "build");
    job.image = Some(ImageConfig::Simple("node".into()));
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "unpinned-image-tag"));
}

#[test]
fn pinned_image_tag_ok() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("build", "build");
    job.image = Some(ImageConfig::Simple("node:20-alpine".into()));
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(!findings.iter().any(|f| f.rule == "unpinned-image-tag"));
}

#[test]
fn large_base_image_warning() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("build", "build");
    job.image = Some(ImageConfig::Simple("node:20".into()));
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "large-base-image"));
}

#[test]
fn alpine_image_no_large_warning() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("build", "build");
    job.image = Some(ImageConfig::Simple("node:20-alpine".into()));
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(!findings.iter().any(|f| f.rule == "large-base-image"));
}

// ============================================================
// Timeout and retry checks
// ============================================================

#[test]
fn missing_timeout_for_build_jobs() {
    let mut pipeline = empty_pipeline();
    pipeline.stages = vec!["build".into()];
    let (name, job) = make_job("compile", "build");
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "missing-timeout"));
}

#[test]
fn missing_timeout_not_for_test_jobs() {
    let mut pipeline = empty_pipeline();
    let (name, job) = make_job("lint", "test");
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(!findings.iter().any(|f| f.rule == "missing-timeout"));
}

#[test]
fn missing_retry_for_deploy_jobs() {
    let mut pipeline = empty_pipeline();
    pipeline.stages = vec!["deploy".into()];
    let (name, mut job) = make_job("deploy-prod", "deploy");
    job.when = When::Manual;
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "missing-retry"));
}

// ============================================================
// Cache checks
// ============================================================

#[test]
fn missing_cache_for_npm_install() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("test", "test");
    job.script = vec!["npm install".into(), "npm test".into()];
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "missing-cache"));
}

#[test]
fn missing_cache_for_pnpm() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("test", "test");
    job.script = vec!["pnpm install --frozen-lockfile".into()];
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "missing-cache"));
}

#[test]
fn no_cache_warning_without_dependency_install() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("test", "test");
    job.script = vec!["echo hello".into()];
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(!findings.iter().any(|f| f.rule == "missing-cache"));
}

// ============================================================
// Interruptible checks
// ============================================================

#[test]
fn missing_interruptible_for_test_jobs() {
    let mut pipeline = empty_pipeline();
    let (name, job) = make_job("unit-tests", "test");
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "missing-interruptible"));
}

#[test]
fn missing_interruptible_for_quality_jobs() {
    let mut pipeline = empty_pipeline();
    pipeline.stages = vec!["quality".into()];
    let (name, mut job) = make_job("lint", "quality");
    job.stage = "quality".into();
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "missing-interruptible"));
}

// ============================================================
// Deploy security checks
// ============================================================

#[test]
fn deploy_without_rules_critical() {
    let mut pipeline = empty_pipeline();
    pipeline.stages = vec!["deploy".into()];
    let (name, mut job) = make_job("deploy-prod", "deploy");
    job.stage = "deploy".into();
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    let finding = findings.iter().find(|f| f.rule == "deploy-without-rules");
    assert!(finding.is_some());
    assert_eq!(finding.unwrap().severity, Severity::Critical);
}

#[test]
fn deploy_with_rules_ok() {
    let mut pipeline = empty_pipeline();
    pipeline.stages = vec!["deploy".into()];
    let (name, mut job) = make_job("deploy-prod", "deploy");
    job.stage = "deploy".into();
    let rule: Rule = serde_yaml::from_str("if: '$CI_COMMIT_BRANCH == \"main\"'").unwrap();
    job.rules = Some(vec![rule]);
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(!findings.iter().any(|f| f.rule == "deploy-without-rules"));
}

#[test]
fn deploy_allow_failure_warning() {
    let mut pipeline = empty_pipeline();
    pipeline.stages = vec!["deploy".into()];
    let (name, mut job) = make_job("deploy-staging", "deploy");
    job.stage = "deploy".into();
    job.allow_failure = AllowFailure::Bool(true);
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "deploy-allow-failure"));
}

// ============================================================
// Artifact expiry
// ============================================================

#[test]
fn artifact_no_expiry_warning() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("build", "test");
    job.artifacts = Some(ArtifactConfig {
        paths: vec!["dist/".into()],
        expire_in: None,
        ..Default::default()
    });
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "artifact-no-expiry"));
}

#[test]
fn artifact_with_expiry_ok() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("build", "test");
    job.artifacts = Some(ArtifactConfig {
        paths: vec!["dist/".into()],
        expire_in: Some("1 week".into()),
        ..Default::default()
    });
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(!findings.iter().any(|f| f.rule == "artifact-no-expiry"));
}

// ============================================================
// Hardcoded secrets
// ============================================================

#[test]
fn hardcoded_secret_in_variable() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("deploy", "test");
    job.variables.insert(
        "API_TOKEN".into(),
        VariableValue::Simple("ghp_1234567890".into()),
    );
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    let finding = findings.iter().find(|f| f.rule == "hardcoded-secret");
    assert!(finding.is_some());
    assert_eq!(finding.unwrap().severity, Severity::Critical);
}

#[test]
fn variable_reference_not_flagged_as_secret() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("deploy", "test");
    job.variables.insert(
        "API_TOKEN".into(),
        VariableValue::Simple("$GITLAB_TOKEN".into()),
    );
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(!findings.iter().any(|f| f.rule == "hardcoded-secret"));
}

// ============================================================
// DinD security
// ============================================================

#[test]
fn dind_without_tls_warning() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("build-docker", "build");
    job.services = Some(vec![ServiceConfig::Simple("docker:27-dind".into())]);
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "dind-without-tls"));
}

#[test]
fn dind_with_tls_ok() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("build-docker", "build");
    job.services = Some(vec![ServiceConfig::Simple("docker:27-dind".into())]);
    job.variables.insert(
        "DOCKER_TLS_VERIFY".into(),
        VariableValue::Simple("1".into()),
    );
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(!findings.iter().any(|f| f.rule == "dind-without-tls"));
}

// ============================================================
// Docker socket mount
// ============================================================

#[test]
fn docker_socket_mount_critical() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("build", "build");
    job.script = vec!["docker -H unix:///var/run/docker.sock build .".into()];
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    let finding = findings.iter().find(|f| f.rule == "docker-socket-mount");
    assert!(finding.is_some());
    assert_eq!(finding.unwrap().severity, Severity::Critical);
}

// ============================================================
// Privileged container
// ============================================================

#[test]
fn privileged_container_critical() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("build", "build");
    job.script = vec!["docker run --privileged my-image".into()];
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    let finding = findings.iter().find(|f| f.rule == "privileged-container");
    assert!(finding.is_some());
    assert_eq!(finding.unwrap().severity, Severity::Critical);
}

// ============================================================
// Coverage
// ============================================================

#[test]
fn missing_coverage_for_test_runner() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("test", "test");
    job.script = vec!["pytest --cov".into()];
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "missing-coverage"));
}

#[test]
fn missing_coverage_jest() {
    let mut pipeline = empty_pipeline();
    let (name, mut job) = make_job("test", "test");
    job.script = vec!["npx jest --coverage".into()];
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "missing-coverage"));
}

// ============================================================
// Duplicate scripts
// ============================================================

#[test]
fn duplicate_scripts_detected() {
    let mut pipeline = empty_pipeline();
    let script = vec![
        "npm ci".into(),
        "npm run build".into(),
        "npm test".into(),
        "npm run lint".into(),
    ];

    let (name1, mut job1) = make_job("test-a", "test");
    job1.script = script.clone();
    pipeline.jobs.insert(name1, job1);

    let (name2, mut job2) = make_job("test-b", "test");
    job2.script = script;
    pipeline.jobs.insert(name2, job2);

    let findings = analyze(&pipeline);
    assert!(findings.iter().any(|f| f.rule == "duplicate-script"));
}

// ============================================================
// Manual deploy without confirmation
// ============================================================

#[test]
fn manual_deploy_no_confirmation() {
    let mut pipeline = empty_pipeline();
    pipeline.stages = vec!["deploy".into()];
    let (name, mut job) = make_job("deploy-prod", "deploy");
    job.stage = "deploy".into();
    job.when = When::Manual;
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(
        findings
            .iter()
            .any(|f| f.rule == "manual-deploy-no-confirmation")
    );
}

#[test]
fn manual_deploy_with_confirmation_ok() {
    let mut pipeline = empty_pipeline();
    pipeline.stages = vec!["deploy".into()];
    let (name, mut job) = make_job("deploy-prod", "deploy");
    job.stage = "deploy".into();
    job.when = When::Manual;
    job.manual_confirmation = Some("Deploy to production?".into());
    pipeline.jobs.insert(name, job);
    let findings = analyze(&pipeline);
    assert!(
        !findings
            .iter()
            .any(|f| f.rule == "manual-deploy-no-confirmation")
    );
}

// ============================================================
// Sorting
// ============================================================

#[test]
fn findings_sorted_by_severity() {
    let mut pipeline = empty_pipeline();
    pipeline.stages = vec!["deploy".into()];

    // Add a critical finding (deploy without rules)
    let (name, mut job) = make_job("deploy-prod", "deploy");
    job.stage = "deploy".into();
    pipeline.jobs.insert(name, job);

    // Add an info finding (missing interruptible)
    let (name2, mut job2) = make_job("test", "test");
    job2.stage = "test".into();
    pipeline.jobs.insert(name2, job2);

    let findings = analyze(&pipeline);
    if findings.len() >= 2 {
        for i in 0..findings.len() - 1 {
            let sev_a = match findings[i].severity {
                Severity::Critical => 0,
                Severity::Warning => 1,
                Severity::Info => 2,
            };
            let sev_b = match findings[i + 1].severity {
                Severity::Critical => 0,
                Severity::Warning => 1,
                Severity::Info => 2,
            };
            assert!(sev_a <= sev_b, "findings not sorted by severity");
        }
    }
}
