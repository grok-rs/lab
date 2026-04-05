//! Tests derived directly from examples in the official GitLab CI/CD YAML reference.
//! Source: gitlab.com/gitlab-org/gitlab/-/blob/master/doc/ci/yaml/_index.md
//!
//! Each test references the exact section of the spec it comes from.

use std::io::Write;
use tempfile::NamedTempFile;

use lab_core::model::job::When;
use lab_core::parser::parse_pipeline;
use lab_core::planner::build_plan;

fn write_yaml(content: &str) -> NamedTempFile {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(content.as_bytes()).unwrap();
    file
}

// ============================================================
// Spec: `default` (line ~108)
// "Default configuration does not merge with job configuration.
//  If the job already has a keyword defined, the job keyword takes
//  precedence and the default configuration for that keyword is not used."
// ============================================================

#[test]
fn spec_default_example() {
    let file = write_yaml(
        r#"
default:
  image: ruby:3.0
  retry: 2

rspec:
  script: bundle exec rspec

rspec 2.7:
  image: ruby:2.7
  script: bundle exec rspec
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();

    // rspec uses defaults: image ruby:3.0, retry 2
    let rspec = &pipeline.jobs["rspec"];
    assert_eq!(rspec.image.as_ref().unwrap().name(), "ruby:3.0");
    assert_eq!(rspec.retry.as_ref().unwrap().max_retries(), 2);

    // rspec 2.7 overrides image but uses default retry
    let rspec27 = &pipeline.jobs["rspec 2.7"];
    assert_eq!(rspec27.image.as_ref().unwrap().name(), "ruby:2.7");
    assert_eq!(rspec27.retry.as_ref().unwrap().max_retries(), 2);
}

// ============================================================
// Spec: `stages` (line ~595)
// "If stages is not defined, the default pipeline stages are:
//  .pre, build, test, deploy, .post"
// "Jobs in the same stage run in parallel."
// "Jobs in the next stage run after the jobs from the previous stage
//  complete successfully."
// ============================================================

#[test]
fn spec_stages_default_order() {
    let file = write_yaml(
        r#"
test_job:
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.stages,
        vec![".pre", "build", "test", "deploy", ".post"]
    );
}

#[test]
fn spec_stages_custom_order() {
    let file = write_yaml(
        r#"
stages:
  - build
  - test
  - deploy

build_job:
  stage: build
  script: echo build

test_job:
  stage: test
  script: echo test

deploy_job:
  stage: deploy
  script: echo deploy
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();

    assert_eq!(plan.stages.len(), 3);
    assert_eq!(plan.stages[0].name, "build");
    assert_eq!(plan.stages[1].name, "test");
    assert_eq!(plan.stages[2].name, "deploy");
}

#[test]
fn spec_stages_job_default_is_test() {
    // "If a job does not specify a stage, the job is assigned the test stage."
    let file = write_yaml(
        r#"
stages: [build, test, deploy]
no_stage:
  script: echo hi
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs["no_stage"].stage, "test");
}

// ============================================================
// Spec: `extends` (line ~3139)
// "Performs a reverse deep merge based on the keys."
// "You can use multiple parents for extends."
// ============================================================

#[test]
fn spec_extends_basic() {
    let file = write_yaml(
        r#"
stages: [test]
.tests:
  stage: test
  image: ruby:3.0

rspec:
  extends: .tests
  script: rake rspec

rubocop:
  extends: .tests
  script: bundle exec rubocop
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();

    let rspec = &pipeline.jobs["rspec"];
    assert_eq!(rspec.stage, "test");
    assert_eq!(rspec.image.as_ref().unwrap().name(), "ruby:3.0");
    assert_eq!(rspec.script, vec!["rake rspec"]);

    let rubocop = &pipeline.jobs["rubocop"];
    assert_eq!(rubocop.stage, "test");
    assert_eq!(rubocop.image.as_ref().unwrap().name(), "ruby:3.0");
    assert_eq!(rubocop.script, vec!["bundle exec rubocop"]);
}

#[test]
fn spec_extends_multiple_parents() {
    let file = write_yaml(
        r#"
stages: [test]
.base_image:
  image: node:20

.base_scripts:
  before_script: [npm ci]

test:
  extends:
    - .base_image
    - .base_scripts
  script: [npm test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["test"];
    assert_eq!(job.image.as_ref().unwrap().name(), "node:20");
    assert!(job.before_script.is_some());
}

// ============================================================
// Spec: `parallel:matrix` (line ~4710)
// The deploystacks example generates 7 parallel jobs
// ============================================================

#[test]
fn spec_parallel_matrix_deploystacks() {
    let file = write_yaml(
        r#"
stages: [deploy]
deploystacks:
  stage: deploy
  script: bin/deploy
  parallel:
    matrix:
      - PROVIDER: aws
        STACK:
          - monitoring
          - app1
          - app2
      - PROVIDER: [gcp, vultr]
        STACK: [data, processing]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    let total: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    // aws×3 + gcp×2 + vultr×2 = 7
    assert_eq!(total, 7);
}

#[test]
fn spec_parallel_simple_naming() {
    // "Parallel jobs are named sequentially from job_name 1/N to job_name N/N"
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: rspec
  parallel: 5
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    let total: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    assert_eq!(total, 5);

    // Check naming format: "test 1/5" to "test 5/5"
    let names: Vec<&str> = plan.stages[0]
        .jobs
        .iter()
        .map(|j| j.name.as_str())
        .collect();
    assert!(names.contains(&"test 1/5"));
    assert!(names.contains(&"test 5/5"));
}

#[test]
fn spec_parallel_ci_node_variables() {
    // "Every parallel job has CI_NODE_INDEX and CI_NODE_TOTAL set"
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  parallel: 3
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();

    for pj in &plan.stages[0].jobs {
        assert!(pj.job.variables.contains_key("CI_NODE_INDEX"));
        assert!(pj.job.variables.contains_key("CI_NODE_TOTAL"));
        assert_eq!(pj.job.variables.get("CI_NODE_TOTAL").unwrap().value(), "3");
    }
}

// ============================================================
// Spec: `allow_failure` (line ~1613)
// ============================================================

#[test]
fn spec_allow_failure_default_false() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(!pipeline.jobs["test"].allow_failure.is_allowed(1));
}

#[test]
fn spec_allow_failure_exit_codes() {
    // "allow_failure:exit_codes: Use with allow_failure to control which
    //  exit codes cause the job to be set to allow failure."
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: exit 137
  allow_failure:
    exit_codes:
      - 137
      - 255
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let af = &pipeline.jobs["test"].allow_failure;
    assert!(af.is_allowed(137));
    assert!(af.is_allowed(255));
    assert!(!af.is_allowed(1));
    assert!(!af.is_allowed(0));
}

// ============================================================
// Spec: `artifacts` (line ~1712)
// ============================================================

#[test]
fn spec_artifacts_paths() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  artifacts:
    paths:
      - binaries/
      - .config
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let artifacts = pipeline.jobs["test"].artifacts.as_ref().unwrap();
    assert_eq!(artifacts.paths, vec!["binaries/", ".config"]);
}

#[test]
fn spec_artifacts_exclude() {
    // "Use artifacts:exclude to prevent files from being added to an artifacts archive."
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  artifacts:
    paths:
      - binaries/
    exclude:
      - binaries/**/*.o
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let artifacts = pipeline.jobs["test"].artifacts.as_ref().unwrap();
    assert_eq!(artifacts.paths, vec!["binaries/"]);
    assert_eq!(artifacts.exclude, vec!["binaries/**/*.o"]);
}

#[test]
fn spec_artifacts_expire_in() {
    // "If expire_in is not defined, it defaults to the instance-wide setting."
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  artifacts:
    paths: [build/]
    expire_in: 1 week
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.jobs["test"]
            .artifacts
            .as_ref()
            .unwrap()
            .expire_in
            .as_deref(),
        Some("1 week")
    );
}

#[test]
fn spec_artifacts_when() {
    // "Use artifacts:when to upload artifacts on job failure or despite the failure."
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: exit 1
  artifacts:
    when: on_failure
    paths:
      - test-output/
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.jobs["test"].artifacts.is_some());
}

// ============================================================
// Spec: `cache` (line ~2188)
// ============================================================

#[test]
fn spec_cache_basic() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  cache:
    - key: binaries-cache
      paths:
        - binaries/*.apk
        - .config
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let cache = &pipeline.jobs["test"].cache.as_ref().unwrap()[0];
    match &cache.key {
        Some(lab_core::model::job::CacheKey::Simple(s)) => {
            assert_eq!(s, "binaries-cache");
        }
        other => panic!("Expected Simple cache key, got {other:?}"),
    }
}

#[test]
fn spec_cache_key_files() {
    // "Use cache:key:files to generate a new key when one or two specific files change."
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  cache:
    - key:
        files:
          - Gemfile.lock
          - package.json
      paths:
        - vendor/ruby
        - node_modules
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let cache = &pipeline.jobs["test"].cache.as_ref().unwrap()[0];
    match &cache.key {
        Some(lab_core::model::job::CacheKey::Detailed { files, .. }) => {
            assert_eq!(files, &vec!["Gemfile.lock", "package.json"]);
        }
        other => panic!("Expected Detailed cache key, got {other:?}"),
    }
}

#[test]
fn spec_cache_key_prefix() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  cache:
    - key:
        files:
          - Gemfile.lock
        prefix: $CI_JOB_NAME
      paths:
        - vendor/ruby
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let cache = &pipeline.jobs["test"].cache.as_ref().unwrap()[0];
    match &cache.key {
        Some(lab_core::model::job::CacheKey::Detailed { prefix, .. }) => {
            assert_eq!(prefix.as_deref(), Some("$CI_JOB_NAME"));
        }
        other => panic!("Expected Detailed cache key with prefix, got {other:?}"),
    }
}

#[test]
fn spec_cache_policy() {
    // "cache:policy: pull means never upload, push means never download"
    let file = write_yaml(
        r#"
stages: [build, test]
build:
  stage: build
  script: echo build
  cache:
    - key: gems
      paths: [vendor/]
      policy: push

test:
  stage: test
  script: echo test
  cache:
    - key: gems
      paths: [vendor/]
      policy: pull
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    use lab_core::model::job::CachePolicy;
    let build_cache = &pipeline.jobs["build"].cache.as_ref().unwrap()[0];
    assert!(matches!(build_cache.policy, Some(CachePolicy::Push)));
    let test_cache = &pipeline.jobs["test"].cache.as_ref().unwrap()[0];
    assert!(matches!(test_cache.policy, Some(CachePolicy::Pull)));
}

// ============================================================
// Spec: `coverage` (line ~2641)
// "Use coverage with a custom regular expression to configure how
//  code coverage is extracted from the job output."
// ============================================================

#[test]
fn spec_coverage_regex() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  coverage: '/Code coverage: \d+\.\d+/'
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.jobs["test"].coverage.as_deref(),
        Some("/Code coverage: \\d+\\.\\d+/")
    );
}

// ============================================================
// Spec: `needs` (line ~3985)
// "Execute jobs earlier than the stage ordering."
// ============================================================

#[test]
fn spec_needs_basic() {
    let file = write_yaml(
        r#"
stages: [build, test, deploy]

build_a:
  stage: build
  script: echo build_a

build_b:
  stage: build
  script: echo build_b

test_a:
  stage: test
  needs: [build_a]
  script: echo test_a

test_b:
  stage: test
  needs: [build_b]
  script: echo test_b

deploy_a:
  stage: deploy
  needs: [test_a]
  script: echo deploy_a
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();

    // build_a and build_b run first (parallel)
    assert_eq!(plan.stages[0].name, "build");
    assert_eq!(plan.stages[0].jobs.len(), 2);
}

#[test]
fn spec_needs_artifacts() {
    // "Use needs:artifacts to specify that a job does not need to download artifacts."
    let file = write_yaml(
        r#"
stages: [build, test]
build:
  stage: build
  script: echo build

test_with_artifacts:
  stage: test
  needs:
    - job: build
      artifacts: true
  script: echo test

test_without_artifacts:
  stage: test
  needs:
    - job: build
      artifacts: false
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let with = &pipeline.jobs["test_with_artifacts"].needs.as_ref().unwrap()[0];
    assert!(with.wants_artifacts());
    let without = &pipeline.jobs["test_without_artifacts"]
        .needs
        .as_ref()
        .unwrap()[0];
    assert!(!without.wants_artifacts());
}

#[test]
fn spec_needs_optional() {
    // "If needs:optional is true and the needed job is not added to the pipeline,
    //  the job that has the optional need does not fail."
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  needs:
    - job: missing_job
      optional: true
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    // Should not error — optional dep missing is OK
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None);
    assert!(plan.is_ok());
}

// ============================================================
// Spec: `retry` (line ~5086)
// ============================================================

#[test]
fn spec_retry_basic() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  retry: 2
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.jobs["test"].retry.as_ref().unwrap().max_retries(),
        2
    );
}

#[test]
fn spec_retry_when() {
    // "retry:when with a list of failure types"
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  retry:
    max: 2
    when:
      - runner_system_failure
      - stuck_or_timeout_failure
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let retry = pipeline.jobs["test"].retry.as_ref().unwrap();
    assert_eq!(retry.max_retries(), 2);
    assert!(retry.should_retry("runner_system_failure"));
    assert!(retry.should_retry("stuck_or_timeout_failure"));
    assert!(!retry.should_retry("script_failure"));
}

// ============================================================
// Spec: `rules` (line ~5240)
// ============================================================

#[test]
fn spec_rules_if_basic() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  CI_PIPELINE_SOURCE: push

test:
  script: echo test
  rules:
    - if: $CI_PIPELINE_SOURCE == "push"
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // Rule matches, job should be in plan
    let total: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    assert_eq!(total, 1);
}

#[test]
fn spec_rules_if_no_match_excludes_job() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  CI_PIPELINE_SOURCE: web

test:
  script: echo test
  rules:
    - if: $CI_PIPELINE_SOURCE == "push"
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // Rule doesn't match → job excluded
    let total: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    assert_eq!(total, 0);
}

#[test]
fn spec_rules_when_never() {
    // "when: never — Do not add the job to the pipeline."
    let file = write_yaml(
        r#"
stages: [test]
variables:
  CI_COMMIT_BRANCH: my-feature-draft

test:
  script: echo test
  rules:
    - if: $CI_COMMIT_BRANCH =~ /-draft$/
      when: never
    - when: on_success
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // First rule matches (branch ends with -draft) → when: never → job excluded
    assert!(plan.stages.is_empty());
}

#[test]
fn spec_rules_fallthrough_to_when() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  CI_COMMIT_BRANCH: feature

test:
  script: echo test
  rules:
    - if: $CI_COMMIT_BRANCH == "main"
      when: always
    - when: manual
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // First rule doesn't match, second is unconditional → when: manual
    assert_eq!(plan.stages[0].jobs[0].job.when, When::Manual);
}

#[test]
fn spec_rules_variables() {
    // "Use rules:variables to define variables for specific conditions."
    let file = write_yaml(
        r#"
stages: [test]
variables:
  CI_COMMIT_BRANCH: main

test:
  script: echo $DEPLOY_VARIABLE
  rules:
    - if: $CI_COMMIT_BRANCH == "main"
      variables:
        DEPLOY_VARIABLE: "deploy-production"
    - if: $CI_COMMIT_BRANCH
      variables:
        DEPLOY_VARIABLE: "deploy-staging"
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // First rule matches → DEPLOY_VARIABLE = "deploy-production"
    let job = &plan.stages[0].jobs[0].job;
    assert_eq!(
        job.variables.get("DEPLOY_VARIABLE").unwrap().value(),
        "deploy-production"
    );
}

// ============================================================
// Spec: `services` (line ~6198)
// ============================================================

#[test]
fn spec_services_basic() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  image: python:3
  services:
    - name: postgres:16
      alias: db
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let services = pipeline.jobs["test"].services.as_ref().unwrap();
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].hostname(), "db");
}

#[test]
fn spec_services_hostname_derivation() {
    // Hostname derived from image: strip tag, replace / with __
    let file = write_yaml(
        r#"
stages: [test]
test:
  image: alpine
  services:
    - postgres:16
    - name: registry.example.com/group/my-service:latest
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let services = pipeline.jobs["test"].services.as_ref().unwrap();
    assert_eq!(services[0].hostname(), "postgres");
    assert_eq!(
        services[1].hostname(),
        "registry.example.com__group__my-service"
    );
}

// ============================================================
// Spec: `stage` (line ~6535)
// "Use stage to define which stage a job runs in."
// ============================================================

#[test]
fn spec_stage_pre_and_post() {
    let file = write_yaml(
        r#"
stages:
  - .pre
  - build
  - test
  - .post

setup:
  stage: .pre
  script: echo setup

cleanup:
  stage: .post
  script: echo cleanup

build:
  stage: build
  script: echo build
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs["setup"].stage, ".pre");
    assert_eq!(pipeline.jobs["cleanup"].stage, ".post");

    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // .pre runs first, build in middle, .post last
    assert_eq!(plan.stages[0].name, ".pre");
    assert_eq!(plan.stages[2].name, ".post");
}

// ============================================================
// Spec: `timeout` (line ~6719)
// ============================================================

#[test]
fn spec_timeout() {
    let file = write_yaml(
        r#"
stages: [build, test]
build:
  script: echo build
  timeout: 3h30m

test:
  script: echo test
  timeout: 1h30m
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs["build"].timeout.unwrap().as_secs(), 12600); // 3h30m
    assert_eq!(pipeline.jobs["test"].timeout.unwrap().as_secs(), 5400); // 1h30m
}

// ============================================================
// Spec: `trigger` (line ~6750)
// ============================================================

#[test]
fn spec_trigger_include() {
    let file = write_yaml(
        r#"
stages: [test]
trigger_child:
  trigger:
    include: path/to/child-pipeline.yml
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.jobs["trigger_child"].trigger.is_some());
    // trigger jobs don't need script
    assert!(pipeline.jobs["trigger_child"].script.is_empty());
}

#[test]
fn spec_trigger_strategy_depend() {
    // "trigger:strategy: depend — Makes the trigger job wait for the downstream pipeline
    //  to complete before marking the trigger job as complete."
    let file = write_yaml(
        r#"
stages: [test]
trigger_child:
  trigger:
    include: child.yml
    strategy: depend
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    match &pipeline.jobs["trigger_child"].trigger {
        Some(lab_core::model::job::TriggerConfig::Detailed { strategy, .. }) => {
            assert_eq!(strategy.as_deref(), Some("depend"));
        }
        other => panic!("Expected Detailed trigger, got {other:?}"),
    }
}

// ============================================================
// Spec: `when` (line ~7092)
// Supported values: on_success, manual, always, on_failure, delayed, never
// ============================================================

#[test]
fn spec_when_all_values() {
    let file = write_yaml(
        r#"
stages: [test]
on_success_job:
  script: echo test
  when: on_success

manual_job:
  script: echo test
  when: manual

always_job:
  script: echo test
  when: always

on_failure_job:
  script: echo test
  when: on_failure

delayed_job:
  script: echo test
  when: delayed
  start_in: 30 minutes

never_job:
  script: echo test
  when: never
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs["on_success_job"].when, When::OnSuccess);
    assert_eq!(pipeline.jobs["manual_job"].when, When::Manual);
    assert_eq!(pipeline.jobs["always_job"].when, When::Always);
    assert_eq!(pipeline.jobs["on_failure_job"].when, When::OnFailure);
    assert_eq!(pipeline.jobs["delayed_job"].when, When::Delayed);
    assert_eq!(pipeline.jobs["never_job"].when, When::Never);
}

// ============================================================
// Spec: `variables` (line ~7261)
// ============================================================

#[test]
fn spec_variables_global_and_job() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  GLOBAL_VAR: "global"

test:
  variables:
    JOB_VAR: "job"
    GLOBAL_VAR: "overridden"
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.variables.get("GLOBAL_VAR").unwrap().value(),
        "global"
    );
    assert_eq!(
        pipeline.jobs["test"]
            .variables
            .get("GLOBAL_VAR")
            .unwrap()
            .value(),
        "overridden"
    );
    assert_eq!(
        pipeline.jobs["test"]
            .variables
            .get("JOB_VAR")
            .unwrap()
            .value(),
        "job"
    );
}

#[test]
fn spec_variables_expand_false() {
    // "variables:expand: false — prevents the variable from being expanded"
    let file = write_yaml(
        r#"
stages: [test]
variables:
  DEPLOY_VARIABLE:
    value: "deploy-value"
    expand: false
    description: "A deploy variable"

test:
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let var = pipeline.variables.get("DEPLOY_VARIABLE").unwrap();
    assert_eq!(var.value(), "deploy-value");
    assert!(!var.should_expand());
}

#[test]
fn spec_variables_with_description_and_options() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  DEPLOY_ENVIRONMENT:
    value: "staging"
    description: "Deployment target"
    options:
      - staging
      - production
      - canary

test:
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let var = pipeline.variables.get("DEPLOY_ENVIRONMENT").unwrap();
    assert_eq!(var.value(), "staging");
}

// ============================================================
// Spec: `inherit` (line ~3786)
// ============================================================

#[test]
fn spec_inherit_default_false() {
    // "inherit:default: false — No default keywords are inherited by any job."
    let file = write_yaml(
        r#"
stages: [test]
default:
  image: ruby:3.0
  retry: 2

rspec:
  inherit:
    default: false
  script: bundle exec rspec
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["rspec"];
    // Should NOT have inherited image or retry
    assert!(job.image.is_none());
    assert!(job.retry.is_none());
}

#[test]
fn spec_inherit_default_selective() {
    // "inherit:default with a list selects which defaults to inherit"
    let file = write_yaml(
        r#"
stages: [test]
default:
  image: ruby:3.0
  retry: 2
  before_script: [echo setup]

rspec:
  inherit:
    default:
      - retry
  script: bundle exec rspec
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["rspec"];
    // Should inherit ONLY retry
    assert!(job.image.is_none());
    assert!(job.before_script.is_none());
    assert_eq!(job.retry.as_ref().unwrap().max_retries(), 2);
}

// ============================================================
// Spec: `image:entrypoint` (line ~3437)
// ============================================================

#[test]
fn spec_image_entrypoint() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  image:
    name: super/sql:experimental
    entrypoint: [""]
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let image = pipeline.jobs["test"].image.as_ref().unwrap();
    assert_eq!(image.name(), "super/sql:experimental");
    assert_eq!(image.entrypoint().unwrap(), &["".to_string()]);
}

// ============================================================
// Spec: `workflow:rules` (line ~823)
// "When no rules evaluate to true, the pipeline does not run."
// ============================================================

#[test]
fn spec_workflow_rules_example() {
    let file = write_yaml(
        r#"
stages: [test]
workflow:
  rules:
    - if: $CI_COMMIT_TITLE =~ /-draft$/
      when: never
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH

test:
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let wf = pipeline.workflow.as_ref().unwrap();
    assert_eq!(wf.rules.len(), 3);
}

// ============================================================
// Spec: `dependencies` (line ~2738)
// "Restrict which artifacts are passed to a specific job"
// ============================================================

#[test]
fn spec_dependencies_empty() {
    // "dependencies: [] prevents downloading any artifacts"
    let file = write_yaml(
        r#"
stages: [build, test]
build:
  stage: build
  script: echo build
  artifacts:
    paths: [dist/]

test:
  stage: test
  dependencies: []
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.jobs["test"].dependencies.as_ref().unwrap().len(),
        0
    );
}

// ============================================================
// Spec: `resource_group` (line ~5046)
// ============================================================

#[test]
fn spec_resource_group() {
    let file = write_yaml(
        r#"
stages: [deploy]
deploy:
  stage: deploy
  script: echo deploy
  resource_group: production
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.jobs["deploy"].resource_group.as_deref(),
        Some("production")
    );
}

// ============================================================
// Spec: `interruptible` (line ~3868)
// ============================================================

#[test]
fn spec_interruptible() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  interruptible: true
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs["test"].interruptible, Some(true));
}

// ============================================================
// Spec: Complete pipeline from the doc's complex example
// ============================================================

#[test]
fn spec_complex_pipeline() {
    let file = write_yaml(
        r#"
stages:
  - build
  - test
  - deploy

default:
  image: ruby:3.0
  retry: 2

variables:
  DEPLOY_SITE: "https://example.com/"

build_a:
  stage: build
  script:
    - echo "This job builds something."
  artifacts:
    paths:
      - build/

build_b:
  stage: build
  script:
    - echo "This job builds something else."
  artifacts:
    paths:
      - other-build/

test_a:
  stage: test
  script:
    - echo "This job tests something."
    - echo "It only runs when build_a succeeds."
  needs:
    - build_a

test_b:
  stage: test
  script:
    - echo "This job tests something."
    - echo "It only runs when build_b succeeds."
  needs:
    - build_b

deploy_a:
  stage: deploy
  script:
    - echo "Deploying to $DEPLOY_SITE"
  rules:
    - if: $CI_COMMIT_BRANCH == "main"
      when: always
    - when: never
  needs:
    - test_a

deploy_b:
  stage: deploy
  script:
    - echo "Deploying to $DEPLOY_SITE"
  rules:
    - if: $CI_COMMIT_BRANCH == "main"
      when: always
    - when: never
  needs:
    - test_b
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.stages.len(), 3);
    assert_eq!(pipeline.jobs.len(), 6);

    // All jobs should have default image and retry
    for (name, job) in &pipeline.jobs {
        assert_eq!(
            job.image.as_ref().unwrap().name(),
            "ruby:3.0",
            "Job {name} should have default image"
        );
        assert_eq!(
            job.retry.as_ref().unwrap().max_retries(),
            2,
            "Job {name} should have default retry"
        );
    }

    // Check DAG
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // build_a,build_b → test_a,test_b → deploy_a,deploy_b (but deploy filtered by rules)
    assert!(!plan.stages.is_empty());
}

// ============================================================
// Additional spec-derived tests for remaining gaps
// ============================================================

#[test]
fn spec_script_single_string() {
    let file = write_yaml(
        r#"
stages: [test]
job1:
  script: "bundle exec rspec"
job2:
  script:
    - uname -a
    - bundle exec rspec
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs["job1"].script, vec!["bundle exec rspec"]);
    assert_eq!(pipeline.jobs["job2"].script.len(), 2);
}

#[test]
fn spec_before_after_script_single_string() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  before_script: echo "setup"
  script: echo "test"
  after_script: echo "cleanup"
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["test"];
    assert_eq!(job.before_script.as_ref().unwrap(), &vec!["echo \"setup\""]);
    assert_eq!(
        job.after_script.as_ref().unwrap(),
        &vec!["echo \"cleanup\""]
    );
}

#[test]
fn spec_needs_empty_starts_immediately() {
    let file = write_yaml(
        r#"
stages: [build, test]
build:
  stage: build
  script: echo build
lint:
  stage: test
  needs: []
  script: echo lint
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let lint_needs = pipeline.jobs["lint"].needs.as_ref().unwrap();
    assert!(lint_needs.is_empty());
}

#[test]
fn spec_environment_parsing() {
    let file = write_yaml(
        r#"
stages: [deploy]
deploy:
  stage: deploy
  script: echo deploy
  environment:
    name: production
    url: https://prod.example.com
    deployment_tier: production
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs.len(), 1);
}

#[test]
fn spec_tags_list() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: echo test
  tags: [ruby, postgres, docker]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs["test"].tags.as_ref().unwrap().len(), 3);
}

#[test]
fn spec_services_in_default_block() {
    let file = write_yaml(
        r#"
stages: [test]
default:
  image:
    name: ruby:2.6
    entrypoint: ["/bin/bash"]
  services:
    - name: my-postgres:11.7
      alias: db-postgres
      command: ["start"]

test:
  script: bundle exec rake spec
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["test"];
    assert_eq!(job.image.as_ref().unwrap().name(), "ruby:2.6");
    assert_eq!(job.services.as_ref().unwrap()[0].hostname(), "db-postgres");
}

#[test]
fn spec_trigger_simple_project() {
    let file = write_yaml(
        r#"
stages: [test]
downstream:
  trigger: my-group/my-project
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    match &pipeline.jobs["downstream"].trigger {
        Some(lab_core::model::job::TriggerConfig::Simple(p)) => {
            assert_eq!(p, "my-group/my-project")
        }
        other => panic!("Expected Simple trigger, got {other:?}"),
    }
}

#[test]
fn spec_trigger_with_strategy_mirror() {
    let file = write_yaml(
        r#"
stages: [test]
trigger_job:
  trigger:
    project: my-group/my-project
    strategy: mirror
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    match &pipeline.jobs["trigger_job"].trigger {
        Some(lab_core::model::job::TriggerConfig::Detailed {
            project, strategy, ..
        }) => {
            assert_eq!(project.as_deref(), Some("my-group/my-project"));
            assert_eq!(strategy.as_deref(), Some("mirror"));
        }
        other => panic!("Expected Detailed trigger, got {other:?}"),
    }
}

#[test]
fn spec_unsupported_keywords_graceful() {
    // All these keywords should parse without error even though we don't use them
    let file = write_yaml(
        r#"
stages: [test]
job:
  script: echo test
  environment: production
  release:
    tag_name: v1.0
    description: "A release"
  secrets:
    DB_PASS:
      vault: prod/db/password
  identity: google_cloud
  id_tokens:
    TOKEN:
      aud: https://example.com
  pages: true
  dast_configuration:
    site_profile: "test"
  manual_confirmation: "Are you sure?"
  hooks:
    pre_get_sources_script: [echo "pre"]
  run:
    - name: step1
      script: echo hello
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs.len(), 1);
}

#[test]
fn spec_job_names_with_spaces() {
    let file = write_yaml(
        r#"
stages: [test]
"rspec 2.7":
  script: bundle exec rspec
"test job":
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.jobs.contains_key("rspec 2.7"));
    assert!(pipeline.jobs.contains_key("test job"));
}

#[test]
fn spec_variables_expand_false_in_yaml() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  VAR1: value1
  VAR2: value2 $VAR1
  VAR3:
    value: value3 $VAR1
    expand: false
test:
  script: echo test
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let var3 = pipeline.variables.get("VAR3").unwrap();
    assert!(!var3.should_expand());
    assert_eq!(var3.value(), "value3 $VAR1");
}

#[test]
fn spec_full_realistic_pipeline() {
    let file = write_yaml(
        r#"
stages: [build, test, deploy]

default:
  image: ruby:3.0
  retry: 2
  before_script: [bundle install]

variables:
  RAILS_ENV: test

workflow:
  rules:
    - if: $CI_COMMIT_BRANCH

build:
  stage: build
  script: [bundle exec rake build]
  artifacts:
    paths: [build/]
    expire_in: 1 hour

unit_test:
  stage: test
  services:
    - name: postgres:16
      alias: db
  script: [bundle exec rspec]
  coverage: '/Coverage: (\d+\.\d+)%/'
  needs: [build]
  cache:
    - key: gems
      paths: [vendor/bundle/]
  artifacts:
    paths: [coverage/]
    when: always

lint:
  stage: test
  needs: []
  inherit:
    default:
      - image
  script: bundle exec rubocop
  allow_failure: true

deploy_staging:
  stage: deploy
  script: ./deploy.sh staging
  environment:
    name: staging
  rules:
    - if: '$CI_COMMIT_BRANCH == "main"'
      variables:
        DEPLOY_TARGET: staging
    - when: never
  needs: [unit_test]
  timeout: 30m
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.stages.len(), 3);
    assert_eq!(pipeline.jobs.len(), 4);

    // defaults applied
    assert_eq!(
        pipeline.jobs["build"].image.as_ref().unwrap().name(),
        "ruby:3.0"
    );
    assert_eq!(
        pipeline.jobs["build"].retry.as_ref().unwrap().max_retries(),
        2
    );

    // lint: inherit only image
    let lint = &pipeline.jobs["lint"];
    assert_eq!(lint.image.as_ref().unwrap().name(), "ruby:3.0");
    assert!(lint.before_script.is_none());

    // services on unit_test
    assert_eq!(
        pipeline.jobs["unit_test"].services.as_ref().unwrap().len(),
        1
    );

    // plan builds
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    assert!(!plan.stages.is_empty());
}
