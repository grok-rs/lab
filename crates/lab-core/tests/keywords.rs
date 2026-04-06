//! Comprehensive keyword-level tests for GitLab CI/CD YAML parsing.
//! Tests every keyword from the official spec that lab supports.

use std::io::Write;
use tempfile::NamedTempFile;

use lab_core::model::job::When;
use lab_core::model::variables::VariableValue;
use lab_core::parser::parse_pipeline;
use lab_core::planner::build_plan;

fn write_yaml(content: &str) -> NamedTempFile {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(content.as_bytes()).unwrap();
    file
}

// ============================================================
// Global Keywords
// ============================================================

#[test]
fn test_stages_ordering() {
    let file = write_yaml(
        r#"
stages:
  - prepare
  - build
  - test
  - deploy

prepare:
  stage: prepare
  script: [echo prepare]

build:
  stage: build
  script: [echo build]

test:
  stage: test
  script: [echo test]

deploy:
  stage: deploy
  script: [echo deploy]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.stages, vec!["prepare", "build", "test", "deploy"]);
    assert_eq!(pipeline.jobs.len(), 4);
}

#[test]
fn test_default_stages_when_omitted() {
    let file = write_yaml(
        r#"
test:
  script: [echo hi]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.stages.contains(&"test".to_string()));
    assert!(pipeline.stages.contains(&"build".to_string()));
    assert!(pipeline.stages.contains(&"deploy".to_string()));
}

#[test]
fn test_global_variables() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  APP_NAME: myapp
  VERSION: "1.0"
test:
  script: [echo $APP_NAME]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.variables.get("APP_NAME").unwrap().value(), "myapp");
    assert_eq!(pipeline.variables.get("VERSION").unwrap().value(), "1.0");
}

#[test]
fn test_default_keyword_application() {
    let file = write_yaml(
        r#"
stages: [test]
default:
  image: node:20
  before_script:
    - npm ci
  timeout: 30m

test:
  script: [npm test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["test"];
    assert_eq!(job.image.as_ref().unwrap().name(), "node:20");
    assert!(job.before_script.is_some());
    assert!(job.timeout.is_some());
}

#[test]
fn test_workflow_rules() {
    let file = write_yaml(
        r#"
stages: [test]
workflow:
  rules:
    - if: '$CI_COMMIT_BRANCH == "main"'
      when: always
    - when: never
  name: "Pipeline for $CI_COMMIT_BRANCH"

test:
  script: [echo hi]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.workflow.is_some());
    let wf = pipeline.workflow.unwrap();
    assert_eq!(wf.rules.len(), 2);
    assert_eq!(wf.name.unwrap(), "Pipeline for $CI_COMMIT_BRANCH");
}

// ============================================================
// Job Keywords — Core
// ============================================================

#[test]
fn test_script_required() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script:
    - echo "step 1"
    - echo "step 2"
    - echo "step 3"
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs["test"].script.len(), 3);
}

#[test]
fn test_before_after_script() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  before_script: [echo before]
  script: [echo main]
  after_script: [echo after]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["test"];
    assert_eq!(job.before_script.as_ref().unwrap().len(), 1);
    assert_eq!(job.after_script.as_ref().unwrap().len(), 1);
}

#[test]
fn test_image_simple_and_detailed() {
    let file = write_yaml(
        r#"
stages: [test]
simple:
  image: ruby:3.2
  script: [echo simple]

detailed:
  image:
    name: python:3.12
    entrypoint: [""]
  script: [echo detailed]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.jobs["simple"].image.as_ref().unwrap().name(),
        "ruby:3.2"
    );
    let detailed = pipeline.jobs["detailed"].image.as_ref().unwrap();
    assert_eq!(detailed.name(), "python:3.12");
    assert!(detailed.entrypoint().is_some());
}

#[test]
fn test_stage_assignment() {
    let file = write_yaml(
        r#"
stages: [build, test]
build_job:
  stage: build
  script: [echo build]

test_job:
  stage: test
  script: [echo test]

default_stage:
  script: [echo default]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs["build_job"].stage, "build");
    assert_eq!(pipeline.jobs["test_job"].stage, "test");
    assert_eq!(pipeline.jobs["default_stage"].stage, "test"); // default is "test"
}

#[test]
fn test_extends_single_and_multiple() {
    let file = write_yaml(
        r#"
stages: [test]
.base:
  image: node:18
  variables:
    NODE_ENV: test

.with_coverage:
  after_script: [echo coverage]

test:
  extends:
    - .base
    - .with_coverage
  script: [npm test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["test"];
    assert_eq!(job.image.as_ref().unwrap().name(), "node:18");
    assert!(job.after_script.is_some());
    assert!(job.variables.contains_key("NODE_ENV"));
}

#[test]
fn test_when_values() {
    let file = write_yaml(
        r#"
stages: [test]
auto:
  script: [echo auto]
  when: on_success

manual:
  script: [echo manual]
  when: manual

always_run:
  script: [echo always]
  when: always

never_run:
  script: [echo never]
  when: never
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs["auto"].when, When::OnSuccess);
    assert_eq!(pipeline.jobs["manual"].when, When::Manual);
    assert_eq!(pipeline.jobs["always_run"].when, When::Always);
    assert_eq!(pipeline.jobs["never_run"].when, When::Never);
}

#[test]
fn test_inherit_default_false() {
    let file = write_yaml(
        r#"
stages: [test]
default:
  image: node:20
  before_script: [echo setup]

no_inherit:
  inherit:
    default: false
  script: [echo test]

partial_inherit:
  inherit:
    default:
      - image
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    // inherit:default:false → no defaults applied
    let no_inherit = &pipeline.jobs["no_inherit"];
    assert!(no_inherit.image.is_none());
    assert!(no_inherit.before_script.is_none());

    // inherit:default:[image] → only image inherited
    let partial = &pipeline.jobs["partial_inherit"];
    assert_eq!(partial.image.as_ref().unwrap().name(), "node:20");
    assert!(partial.before_script.is_none());
}

// ============================================================
// Job Keywords — Execution
// ============================================================

#[test]
fn test_needs_simple_and_detailed() {
    let file = write_yaml(
        r#"
stages: [build, test]
build:
  stage: build
  script: [echo build]

test_simple:
  stage: test
  script: [echo test]
  needs: [build]

test_detailed:
  stage: test
  script: [echo test]
  needs:
    - job: build
      artifacts: false
      optional: true
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let simple = &pipeline.jobs["test_simple"];
    let needs = simple.needs.as_ref().unwrap();
    assert_eq!(needs[0].job_name(), "build");
    assert!(needs[0].wants_artifacts());

    let detailed = &pipeline.jobs["test_detailed"];
    let needs = detailed.needs.as_ref().unwrap();
    assert!(!needs[0].wants_artifacts());
    assert!(needs[0].is_optional());
}

#[test]
fn test_needs_optional_missing_dep() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  needs:
    - job: nonexistent
      optional: true
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    // Should not error because the dep is optional
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None);
    assert!(plan.is_ok());
}

#[test]
fn test_dependencies() {
    let file = write_yaml(
        r#"
stages: [build, test]
build_a:
  stage: build
  script: [echo a]

build_b:
  stage: build
  script: [echo b]

test:
  stage: test
  script: [echo test]
  dependencies:
    - build_a
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.jobs["test"].dependencies.as_ref().unwrap(),
        &vec!["build_a".to_string()]
    );
}

#[test]
fn test_parallel_count() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  parallel: 3
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // 3 parallel copies
    let total_jobs: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    assert_eq!(total_jobs, 3);
}

#[test]
fn test_parallel_matrix() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo "$DB $VER"]
  parallel:
    matrix:
      - DB: [postgres, mysql]
        VER: ["14", "15"]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    let total_jobs: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    assert_eq!(total_jobs, 4); // 2 DB x 2 VER
}

#[test]
fn test_timeout_parsing() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  timeout: 1h30m
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let timeout = pipeline.jobs["test"].timeout.unwrap();
    assert_eq!(timeout.as_secs(), 5400); // 1h30m = 5400s
}

#[test]
fn test_retry_simple_and_detailed() {
    let file = write_yaml(
        r#"
stages: [test]
simple_retry:
  script: [echo test]
  retry: 2

detailed_retry:
  script: [echo test]
  retry:
    max: 3
    when:
      - script_failure
      - stuck_or_timeout_failure
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.jobs["simple_retry"]
            .retry
            .as_ref()
            .unwrap()
            .max_retries(),
        2
    );
    let detailed = pipeline.jobs["detailed_retry"].retry.as_ref().unwrap();
    assert_eq!(detailed.max_retries(), 3);
    assert!(detailed.should_retry("script_failure"));
    assert!(!detailed.should_retry("api_failure"));
}

#[test]
fn test_allow_failure_variants() {
    let file = write_yaml(
        r#"
stages: [test]
bool_allow:
  script: [echo test]
  allow_failure: true

exit_code_allow:
  script: [echo test]
  allow_failure:
    exit_codes:
      - 137
      - 143
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.jobs["bool_allow"].allow_failure.is_allowed(1));

    let ec = &pipeline.jobs["exit_code_allow"].allow_failure;
    assert!(ec.is_allowed(137));
    assert!(ec.is_allowed(143));
    assert!(!ec.is_allowed(1));
}

#[test]
fn test_start_in() {
    let file = write_yaml(
        r#"
stages: [test]
delayed:
  script: [echo delayed]
  when: delayed
  start_in: 30 seconds
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.jobs["delayed"].start_in.as_deref(),
        Some("30 seconds")
    );
    assert_eq!(pipeline.jobs["delayed"].when, When::Delayed);
}

#[test]
fn test_coverage_regex() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  coverage: '/Coverage: (\d+\.\d+)%/'
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.jobs["test"].coverage.is_some());
}

// ============================================================
// Job Keywords — Rules & Conditions
// ============================================================

#[test]
fn test_rules_if_evaluation() {
    use lab_core::model::rules::evaluate_if_expression;

    let mut vars = lab_core::model::variables::Variables::new();
    vars.insert("BRANCH".into(), VariableValue::Simple("main".into()));
    vars.insert("SOURCE".into(), VariableValue::Simple("push".into()));

    assert!(evaluate_if_expression("$BRANCH == \"main\"", &vars));
    assert!(!evaluate_if_expression("$BRANCH == \"dev\"", &vars));
    assert!(evaluate_if_expression("$BRANCH != \"dev\"", &vars));
    assert!(evaluate_if_expression("$BRANCH =~ /^main$/", &vars));
    assert!(evaluate_if_expression("$BRANCH !~ /^dev$/", &vars));
    assert!(evaluate_if_expression(
        "$BRANCH == \"main\" && $SOURCE == \"push\"",
        &vars
    ));
    assert!(evaluate_if_expression(
        "$BRANCH == \"dev\" || $SOURCE == \"push\"",
        &vars
    ));
    assert!(!evaluate_if_expression("$MISSING", &vars)); // undefined → falsy
    assert!(evaluate_if_expression("$MISSING == null", &vars));
    assert!(evaluate_if_expression("($BRANCH == \"main\")", &vars));
}

#[test]
fn test_rules_variables_applied() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  CI_COMMIT_BRANCH: main

test:
  script: [echo $DEPLOY_ENV]
  rules:
    - if: '$CI_COMMIT_BRANCH == "main"'
      variables:
        DEPLOY_ENV: production
    - when: always
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // The matching rule should have injected DEPLOY_ENV into the job
    let job = &plan.stages[0].jobs[0].job;
    assert_eq!(
        job.variables.get("DEPLOY_ENV").unwrap().value(),
        "production"
    );
}

#[test]
fn test_rules_changes_config() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  rules:
    - changes:
        paths:
          - "src/**/*.rs"
        compare_to: refs/heads/main
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let rules = pipeline.jobs["test"].rules.as_ref().unwrap();
    assert!(rules[0].changes.is_some());
}

#[test]
fn test_rules_exists_config() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  rules:
    - exists:
        - Dockerfile
        - docker-compose.yml
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let rules = pipeline.jobs["test"].rules.as_ref().unwrap();
    assert!(rules[0].exists.is_some());
}

// ============================================================
// Job Keywords — Artifacts & Cache
// ============================================================

#[test]
fn test_artifacts_full_config() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  artifacts:
    paths:
      - dist/
      - build/*.js
    exclude:
      - "**/*.map"
    expire_in: 1 week
    name: my-artifacts
    when: always
    untracked: false
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let artifacts = pipeline.jobs["test"].artifacts.as_ref().unwrap();
    assert_eq!(artifacts.paths.len(), 2);
    assert_eq!(artifacts.exclude.len(), 1);
    assert_eq!(artifacts.expire_in.as_deref(), Some("1 week"));
    assert_eq!(artifacts.name.as_deref(), Some("my-artifacts"));
}

#[test]
fn test_cache_full_config() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  cache:
    - key:
        files:
          - Cargo.lock
        prefix: rust
      paths:
        - target/
      policy: pull-push
      when: on_success
      fallback_keys:
        - default-cache
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let caches = pipeline.jobs["test"].cache.as_ref().unwrap();
    assert_eq!(caches.len(), 1);
    let cache = &caches[0];
    assert!(cache.key.is_some());
    assert_eq!(cache.paths, vec!["target/"]);
    assert_eq!(cache.fallback_keys, vec!["default-cache"]);
}

// ============================================================
// Job Keywords — Services
// ============================================================

#[test]
fn test_services_simple_and_detailed() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  image: python:3.12
  services:
    - postgres:16
    - name: redis:7-alpine
      alias: cache
      command: ["--maxmemory", "256mb"]
      variables:
        REDIS_ARGS: "--appendonly yes"
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let services = pipeline.jobs["test"].services.as_ref().unwrap();
    assert_eq!(services.len(), 2);
    assert_eq!(services[0].image_name(), "postgres:16");
    assert_eq!(services[0].hostname(), "postgres");
    assert_eq!(services[1].hostname(), "cache");
}

// ============================================================
// Job Keywords — Advanced
// ============================================================

#[test]
fn test_trigger_include() {
    let file = write_yaml(
        r#"
stages: [test]
child:
  trigger:
    include: child-pipeline.yml
    strategy: depend
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.jobs["child"].trigger.is_some());
}

#[test]
fn test_unsupported_keywords_dont_break_parsing() {
    // Real-world YAML with keywords lab doesn't actively use
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  coverage: '/Coverage: \d+%/'
  environment:
    name: staging
    url: https://staging.example.com
  resource_group: production
  interruptible: true
  tags:
    - docker
    - linux
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs.len(), 1);
}

// ============================================================
// YAML Optimization
// ============================================================

#[test]
fn test_yaml_anchors() {
    let file = write_yaml(
        r#"
stages: [test]

.vars: &common_vars
  NODE_ENV: test
  CI: "true"

test:
  variables:
    <<: *common_vars
    EXTRA: "value"
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = &pipeline.jobs["test"].variables;
    assert_eq!(vars.get("NODE_ENV").unwrap().value(), "test");
    assert_eq!(vars.get("EXTRA").unwrap().value(), "value");
}

#[test]
fn test_extends_chain() {
    // A extends B extends C
    let file = write_yaml(
        r#"
stages: [test]

.level1:
  image: alpine:latest

.level2:
  extends: .level1
  before_script: [echo setup]

test:
  extends: .level2
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["test"];
    assert_eq!(job.image.as_ref().unwrap().name(), "alpine:latest");
    assert!(job.before_script.is_some());
}

// ============================================================
// Variable Expansion
// ============================================================

#[test]
fn test_variable_expansion() {
    use lab_core::model::variables::{VariableValue, Variables, expand_variables};

    let mut vars = Variables::new();
    vars.insert("NAME".into(), VariableValue::Simple("world".into()));
    vars.insert(
        "GREETING".into(),
        VariableValue::Simple("hello $NAME".into()),
    );

    assert_eq!(expand_variables("$NAME", &vars), "world");
    assert_eq!(expand_variables("${NAME}", &vars), "world");
    assert_eq!(expand_variables("$GREETING", &vars), "hello world"); // recursive
    assert_eq!(expand_variables("$$LITERAL", &vars), "$LITERAL"); // escaped
    assert_eq!(expand_variables("$MISSING", &vars), "$MISSING"); // passthrough
}

// ============================================================
// DAG Planner
// ============================================================

#[test]
fn test_dag_stage_ordering() {
    let file = write_yaml(
        r#"
stages: [build, test, deploy]
build:
  stage: build
  script: [echo build]
test:
  stage: test
  script: [echo test]
deploy:
  stage: deploy
  script: [echo deploy]
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
fn test_dag_needs_bypass_stage() {
    let file = write_yaml(
        r#"
stages: [build, test]
build_a:
  stage: build
  script: [echo a]

build_b:
  stage: build
  script: [echo b]

test_a:
  stage: test
  script: [echo test_a]
  needs: [build_a]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // build_a and build_b in first stage, test_a after
    assert!(plan.stages.len() >= 2);
}

#[test]
fn test_dag_circular_dependency() {
    let file = write_yaml(
        r#"
stages: [test]
a:
  script: [echo a]
  needs: [b]
b:
  script: [echo b]
  needs: [a]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None);
    assert!(plan.is_err()); // Should detect cycle
}

#[test]
fn test_dag_job_filter() {
    let file = write_yaml(
        r#"
stages: [test]
a:
  script: [echo a]
b:
  script: [echo b]
c:
  script: [echo c]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let filter = vec!["b".to_string()];
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, Some(&filter), None).unwrap();
    let total: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    assert_eq!(total, 1);
    assert_eq!(plan.stages[0].jobs[0].name, "b");
}

#[test]
fn test_dag_stage_filter() {
    let file = write_yaml(
        r#"
stages: [build, test]
build_job:
  stage: build
  script: [echo build]
test_a:
  stage: test
  script: [echo a]
test_b:
  stage: test
  script: [echo b]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, Some("test")).unwrap();
    let total: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    assert_eq!(total, 2);
}

// ============================================================
// Coverage Extraction
// ============================================================

#[test]
fn test_coverage_extraction() {
    use lab_core::runner::output::PipelineResult;

    let output = "Running tests...\nCoverage: 85.7%\nDone.";
    let coverage = PipelineResult::extract_coverage(output, r"Coverage: (\d+\.\d+)%");
    assert_eq!(coverage, Some(85.7));

    let output2 = "Total: 42/50 (84.00%)";
    let coverage2 = PipelineResult::extract_coverage(output2, r"(\d+\.\d+)%");
    assert_eq!(coverage2, Some(84.0));

    let output3 = "No coverage data";
    let coverage3 = PipelineResult::extract_coverage(output3, r"Coverage: (\d+\.\d+)%");
    assert_eq!(coverage3, None);
}

// ============================================================
// Include
// ============================================================

#[test]
fn test_include_local() {
    use std::fs;
    let dir = tempfile::tempdir().unwrap();

    // Write the included file
    let included_path = dir.path().join("base.yml");
    fs::write(
        &included_path,
        r#"
.shared:
  image: node:20

lint:
  stage: test
  script: [npm run lint]
"#,
    )
    .unwrap();

    // Write the main file that includes it
    let main_path = dir.path().join(".gitlab-ci.yml");
    fs::write(
        &main_path,
        r#"
stages: [test]

include:
  - local: base.yml

test:
  script: [npm test]
"#,
    )
    .unwrap();

    let pipeline = parse_pipeline(&main_path).unwrap();
    // lint job should come from the included file
    assert!(pipeline.jobs.contains_key("lint"));
    assert!(pipeline.jobs.contains_key("test"));
}

// ============================================================
// Workflow:rules blocking
// ============================================================

#[test]
fn test_workflow_rules_evaluation() {
    use lab_core::model::job::When;
    use lab_core::model::rules::{Rule, RuleResult, evaluate_rules};

    // Simulate workflow:rules with no matching rule
    let rules = vec![Rule {
        if_expr: Some("$CI_BRANCH == \"release\"".to_string()),
        changes: None,
        exists: None,
        when: Some(When::Always),
        allow_failure: None,
        variables: None,
    }];

    let mut vars = lab_core::model::variables::Variables::new();
    vars.insert("CI_BRANCH".into(), VariableValue::Simple("feature".into()));

    match evaluate_rules(&rules, &vars, When::Always) {
        RuleResult::NotMatched => {} // Expected — pipeline should be blocked
        _ => panic!("Expected NotMatched when branch doesn't match"),
    }
}

// ============================================================
// Default keyword sub-fields
// ============================================================

#[test]
fn test_default_services() {
    let file = write_yaml(
        r#"
stages: [test]
default:
  services:
    - postgres:16

test:
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let services = pipeline.jobs["test"].services.as_ref().unwrap();
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].image_name(), "postgres:16");
}

#[test]
fn test_default_cache() {
    let file = write_yaml(
        r#"
stages: [test]
default:
  cache:
    - key: default-key
      paths: [node_modules/]

test:
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let cache = pipeline.jobs["test"].cache.as_ref().unwrap();
    assert_eq!(cache.len(), 1);
    assert_eq!(cache[0].paths, vec!["node_modules/"]);
}

#[test]
fn test_default_artifacts() {
    let file = write_yaml(
        r#"
stages: [test]
default:
  artifacts:
    paths: [dist/]
    expire_in: 1 week

test:
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let artifacts = pipeline.jobs["test"].artifacts.as_ref().unwrap();
    assert_eq!(artifacts.paths, vec!["dist/"]);
    assert_eq!(artifacts.expire_in.as_deref(), Some("1 week"));
}

#[test]
fn test_default_retry() {
    let file = write_yaml(
        r#"
stages: [test]
default:
  retry: 2

test:
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.jobs["test"].retry.as_ref().unwrap().max_retries(),
        2
    );
}

// ============================================================
// Inherit
// ============================================================

#[test]
fn test_inherit_variables_false() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  GLOBAL: "yes"

test:
  inherit:
    variables: false
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    // inherit:variables:false means global vars shouldn't be merged by runner
    // (the parsing itself doesn't enforce this — it's a runtime concern)
    let job = &pipeline.jobs["test"];
    let inherit = job.inherit.as_ref().unwrap();
    match &inherit.variables {
        Some(lab_core::model::job::InheritToggle::Bool(false)) => {}
        other => panic!("Expected InheritToggle::Bool(false), got {other:?}"),
    }
}

#[test]
fn test_inherit_default_selective_list() {
    let file = write_yaml(
        r#"
stages: [test]
default:
  image: node:20
  before_script: [echo setup]
  after_script: [echo cleanup]

test:
  inherit:
    default:
      - image
      - before_script
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["test"];
    // image and before_script should be inherited
    assert_eq!(job.image.as_ref().unwrap().name(), "node:20");
    assert!(job.before_script.is_some());
    // after_script should NOT be inherited (not in the list)
    assert!(job.after_script.is_none());
}

// ============================================================
// Services detailed
// ============================================================

#[test]
fn test_services_with_command_and_variables() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  image: python:3.12
  services:
    - name: redis:7
      alias: cache
      command: ["--maxmemory", "128mb"]
      entrypoint: ["/usr/local/bin/docker-entrypoint.sh"]
      variables:
        REDIS_PASSWORD: secret
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let svc = &pipeline.jobs["test"].services.as_ref().unwrap()[0];
    assert_eq!(svc.hostname(), "cache");
    match svc {
        lab_core::model::job::ServiceConfig::Detailed {
            command,
            entrypoint,
            variables,
            ..
        } => {
            assert_eq!(command.as_ref().unwrap().len(), 2);
            assert!(entrypoint.is_some());
            assert!(variables.contains_key("REDIS_PASSWORD"));
        }
        _ => panic!("Expected Detailed service config"),
    }
}

// ============================================================
// Artifacts detailed
// ============================================================

#[test]
fn test_artifacts_exclude() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  artifacts:
    paths: [dist/]
    exclude:
      - "**/*.map"
      - "**/*.tmp"
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let artifacts = pipeline.jobs["test"].artifacts.as_ref().unwrap();
    assert_eq!(artifacts.exclude.len(), 2);
}

#[test]
fn test_artifacts_when_variants() {
    let file = write_yaml(
        r#"
stages: [test]
on_success:
  script: [echo test]
  artifacts:
    paths: [result.txt]
    when: on_success

always_upload:
  script: [echo test]
  artifacts:
    paths: [logs/]
    when: always

on_failure_upload:
  script: [echo test]
  artifacts:
    paths: [crash.log]
    when: on_failure
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.jobs["on_success"].artifacts.is_some());
    assert!(pipeline.jobs["always_upload"].artifacts.is_some());
    assert!(pipeline.jobs["on_failure_upload"].artifacts.is_some());
}

// ============================================================
// Cache detailed
// ============================================================

#[test]
fn test_cache_policy_variants() {
    use lab_core::model::job::CachePolicy;

    let file = write_yaml(
        r#"
stages: [test]
pull_only:
  script: [echo test]
  cache:
    - key: deps
      paths: [vendor/]
      policy: pull

push_only:
  script: [echo test]
  cache:
    - key: deps
      paths: [vendor/]
      policy: push
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let pull_cache = &pipeline.jobs["pull_only"].cache.as_ref().unwrap()[0];
    assert!(matches!(pull_cache.policy, Some(CachePolicy::Pull)));

    let push_cache = &pipeline.jobs["push_only"].cache.as_ref().unwrap()[0];
    assert!(matches!(push_cache.policy, Some(CachePolicy::Push)));
}

#[test]
fn test_cache_when_variants() {
    use lab_core::model::job::CacheWhen;

    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  cache:
    - key: always-cache
      paths: [.cache/]
      when: always
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let cache = &pipeline.jobs["test"].cache.as_ref().unwrap()[0];
    assert!(matches!(cache.when_upload, Some(CacheWhen::Always)));
}

#[test]
fn test_cache_fallback_keys() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  cache:
    - key: $CI_COMMIT_REF_SLUG
      paths: [vendor/]
      fallback_keys:
        - main
        - default
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let cache = &pipeline.jobs["test"].cache.as_ref().unwrap()[0];
    assert_eq!(cache.fallback_keys, vec!["main", "default"]);
}

#[test]
fn test_cache_key_files() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  cache:
    - key:
        files:
          - Gemfile.lock
          - package-lock.json
        prefix: deps
      paths: [vendor/]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let cache = &pipeline.jobs["test"].cache.as_ref().unwrap()[0];
    match &cache.key {
        Some(lab_core::model::job::CacheKey::Detailed { files, prefix, .. }) => {
            assert_eq!(files.len(), 2);
            assert_eq!(prefix.as_deref(), Some("deps"));
        }
        other => panic!("Expected CacheKey::Detailed, got {other:?}"),
    }
}

// ============================================================
// Retry:when filtering
// ============================================================

#[test]
fn test_retry_when_filtering() {
    use lab_core::model::job::RetryConfig;

    let detailed = RetryConfig::Detailed {
        max: 2,
        when_retry: vec!["script_failure".into(), "stuck_or_timeout_failure".into()],
    };

    assert!(detailed.should_retry("script_failure"));
    assert!(detailed.should_retry("stuck_or_timeout_failure"));
    assert!(!detailed.should_retry("runner_system_failure"));
    assert!(!detailed.should_retry("api_failure"));

    // "always" should match everything
    let always = RetryConfig::Detailed {
        max: 1,
        when_retry: vec!["always".into()],
    };
    assert!(always.should_retry("script_failure"));
    assert!(always.should_retry("anything"));

    // Empty when_retry should match everything
    let empty = RetryConfig::Detailed {
        max: 1,
        when_retry: vec![],
    };
    assert!(empty.should_retry("script_failure"));

    // Simple count always retries
    let simple = RetryConfig::Count(2);
    assert!(simple.should_retry("anything"));
}

// ============================================================
// Rules detailed
// ============================================================

#[test]
fn test_rules_when_override() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  DEPLOY: "true"

test:
  script: [echo test]
  rules:
    - if: '$DEPLOY == "true"'
      when: manual
    - when: on_success
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // Rule matched → when should be overridden to manual
    assert_eq!(plan.stages[0].jobs[0].job.when, When::Manual);
}

#[test]
fn test_rules_allow_failure_override() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  EXPERIMENTAL: "true"

test:
  script: [echo test]
  rules:
    - if: '$EXPERIMENTAL == "true"'
      allow_failure: true
    - when: on_success
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    assert!(plan.stages[0].jobs[0].job.allow_failure.is_allowed(1));
}

#[test]
fn test_rules_changes_with_compare_to() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  rules:
    - changes:
        paths: ["src/**/*.rs"]
        compare_to: refs/heads/main
      when: always
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let rules = pipeline.jobs["test"].rules.as_ref().unwrap();
    match &rules[0].changes {
        Some(lab_core::model::rules::ChangesConfig::Detailed {
            paths, compare_to, ..
        }) => {
            assert_eq!(paths, &vec!["src/**/*.rs"]);
            assert_eq!(compare_to.as_deref(), Some("refs/heads/main"));
        }
        other => panic!("Expected ChangesConfig::Detailed, got {other:?}"),
    }
}

// ============================================================
// Planner edge cases
// ============================================================

#[test]
fn test_when_never_filtered_from_plan() {
    let file = write_yaml(
        r#"
stages: [test]
active:
  script: [echo active]

inactive:
  script: [echo inactive]
  when: never
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    let total: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    assert_eq!(total, 1); // Only "active" should be in the plan
}

#[test]
fn test_hidden_jobs_not_in_pipeline() {
    let file = write_yaml(
        r#"
stages: [test]
.template:
  image: node:20

.another_hidden:
  before_script: [echo hi]

test:
  extends: .template
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs.len(), 1); // Only "test", not .template or .another_hidden
    assert!(pipeline.jobs.contains_key("test"));
}

#[test]
fn test_parallel_matrix_variable_injection() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo "$DB $VER"]
  parallel:
    matrix:
      - DB: [postgres]
        VER: ["14", "15", "16"]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    let total: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    assert_eq!(total, 3);

    // Each expanded job should have DB and VER variables
    for stage in &plan.stages {
        for pj in &stage.jobs {
            assert!(pj.job.variables.contains_key("DB"));
            assert!(pj.job.variables.contains_key("VER"));
        }
    }
}

// ============================================================
// Variable expansion edge cases
// ============================================================

#[test]
fn test_variable_expansion_in_variables() {
    use lab_core::model::variables::{VariableValue, Variables, expand_variables};

    let mut vars = Variables::new();
    vars.insert("BASE".into(), VariableValue::Simple("/opt".into()));
    vars.insert("BIN".into(), VariableValue::Simple("$BASE/bin".into()));
    vars.insert("APP".into(), VariableValue::Simple("${BIN}/app".into()));

    assert_eq!(expand_variables("$APP", &vars), "/opt/bin/app");
}

#[test]
fn test_variable_expansion_no_infinite_loop() {
    use lab_core::model::variables::{VariableValue, Variables, expand_variables};

    let mut vars = Variables::new();
    vars.insert("A".into(), VariableValue::Simple("$B".into()));
    vars.insert("B".into(), VariableValue::Simple("$A".into()));

    // Should not hang — max depth stops it
    let result = expand_variables("$A", &vars);
    assert!(!result.is_empty()); // Just ensure it terminates
}

// ============================================================
// YAML merge key edge cases
// ============================================================

#[test]
fn test_merge_key_multiple_sources() {
    let file = write_yaml(
        r#"
stages: [test]

.defaults: &defaults
  image: alpine:latest

.env: &env_config
  variables:
    ENV: production

test:
  <<: [*defaults, *env_config]
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["test"];
    assert_eq!(job.image.as_ref().unwrap().name(), "alpine:latest");
    assert!(job.variables.contains_key("ENV"));
}

#[test]
fn test_extends_override_precedence() {
    let file = write_yaml(
        r#"
stages: [test]
.base:
  image: node:16
  variables:
    A: "from-base"
    B: "from-base"

test:
  extends: .base
  image: node:20
  variables:
    B: "from-job"
    C: "from-job"
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["test"];
    // Job's own image overrides base
    assert_eq!(job.image.as_ref().unwrap().name(), "node:20");
    // Variables: job overrides base for B, base provides A, job provides C
    assert_eq!(job.variables.get("A").unwrap().value(), "from-base");
    assert_eq!(job.variables.get("B").unwrap().value(), "from-job");
    assert_eq!(job.variables.get("C").unwrap().value(), "from-job");
}

// ============================================================
// Start_in parsing
// ============================================================

#[test]
fn test_start_in_variants() {
    let file = write_yaml(
        r#"
stages: [test]
secs:
  script: [echo test]
  when: delayed
  start_in: 30 seconds

mins:
  script: [echo test]
  when: delayed
  start_in: 5 minutes

hrs:
  script: [echo test]
  when: delayed
  start_in: 1 hour
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(
        pipeline.jobs["secs"].start_in.as_deref(),
        Some("30 seconds")
    );
    assert_eq!(pipeline.jobs["mins"].start_in.as_deref(), Some("5 minutes"));
    assert_eq!(pipeline.jobs["hrs"].start_in.as_deref(), Some("1 hour"));
}

// ============================================================
// Trigger variants
// ============================================================

#[test]
fn test_trigger_simple_project() {
    let file = write_yaml(
        r#"
stages: [test]
downstream:
  trigger: my-group/my-project
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.jobs["downstream"].trigger.is_some());
}

#[test]
fn test_trigger_detailed_with_strategy() {
    let file = write_yaml(
        r#"
stages: [test]
child:
  trigger:
    include:
      - local/child.yml
      - local/other.yml
    strategy: depend
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let trigger = pipeline.jobs["child"].trigger.as_ref().unwrap();
    match trigger {
        lab_core::model::job::TriggerConfig::Detailed {
            include, strategy, ..
        } => {
            assert!(include.is_some());
            assert_eq!(strategy.as_deref(), Some("depend"));
        }
        _ => panic!("Expected Detailed trigger config"),
    }
}

// ============================================================
// Coverage extraction edge cases
// ============================================================

#[test]
fn test_coverage_extraction_various_formats() {
    use lab_core::runner::output::PipelineResult;

    // Python pytest-cov format
    assert_eq!(
        PipelineResult::extract_coverage("TOTAL    500    50    90.00%", r"(\d+\.\d+)%"),
        Some(90.0)
    );

    // Go coverage format
    assert_eq!(
        PipelineResult::extract_coverage("coverage: 75.3% of statements", r"coverage: (\d+\.\d+)%"),
        Some(75.3)
    );

    // Ruby simplecov
    assert_eq!(
        PipelineResult::extract_coverage("Coverage report: 88.42% covered", r"(\d+\.\d+)%"),
        Some(88.42)
    );

    // Integer percentage
    assert_eq!(
        PipelineResult::extract_coverage("Lines: 95%", r"(\d+)%"),
        Some(95.0)
    );
}

// ============================================================
// Edge Cases from GitLab CI/CD YAML Spec
// Ref: gitlab.com/gitlab-org/gitlab/-/blob/master/doc/ci/yaml/_index.md
// ============================================================

#[test]
fn test_empty_script_array() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: []
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.jobs["test"].script.is_empty());
}

#[test]
fn test_duplicate_stage_names() {
    let file = write_yaml(
        r#"
stages:
  - build
  - build
  - test

build:
  stage: build
  script: [echo build]
test:
  stage: test
  script: [echo test]
"#,
    );
    // Should parse without error even with duplicate stages
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.stages.contains(&"build".to_string()));
}

#[test]
fn test_needs_same_stage() {
    let file = write_yaml(
        r#"
stages: [test]
job1:
  script: [echo first]
job2:
  script: [echo second]
  needs: [job1]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let vars = lab_core::model::variables::Variables::new();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // job2 depends on job1 even though same stage
    let total: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    assert_eq!(total, 2);
}

#[test]
fn test_empty_string_variables() {
    let file = write_yaml(
        r#"
stages: [test]
variables:
  EMPTY_VAR: ""
  ANOTHER: ''
  WITH_VALUE: "hello"

test:
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.variables.get("EMPTY_VAR").unwrap().value(), "");
    assert_eq!(pipeline.variables.get("ANOTHER").unwrap().value(), "");
    assert_eq!(
        pipeline.variables.get("WITH_VALUE").unwrap().value(),
        "hello"
    );
}

#[test]
fn test_variable_dollar_dollar_escape() {
    use lab_core::model::variables::{VariableValue, Variables, expand_variables};

    let mut vars = Variables::new();
    vars.insert("BASE".into(), VariableValue::Simple("hello".into()));

    // $$ escapes to single $
    assert_eq!(expand_variables("$$BASE", &vars), "$BASE");
    assert_eq!(expand_variables("cost: $$100", &vars), "cost: $100");
}

#[test]
fn test_extends_conflict_last_wins() {
    let file = write_yaml(
        r#"
stages: [test]
.base:
  image: ruby:3.0
  retry: 2

.override:
  image: python:3.12
  retry: 1

test:
  extends:
    - .base
    - .override
  script: [echo test]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let job = &pipeline.jobs["test"];
    // Last extend (.override) should win for conflicting keys
    assert_eq!(job.image.as_ref().unwrap().name(), "python:3.12");
    assert_eq!(job.retry.as_ref().unwrap().max_retries(), 1);
}

#[test]
fn test_rules_unconditional_when() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  rules:
    - when: always
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let rules = pipeline.jobs["test"].rules.as_ref().unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].if_expr.is_none());
    assert!(rules[0].changes.is_none());
    assert!(rules[0].exists.is_none());
    // Rule with only when: should always match
}

#[test]
fn test_multiple_cache_entries() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  cache:
    - key: gems
      paths: [vendor/bundle/]
    - key: node
      paths: [node_modules/]
    - key: pip
      paths: [.venv/]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    let caches = pipeline.jobs["test"].cache.as_ref().unwrap();
    assert_eq!(caches.len(), 3);
}

#[test]
fn test_default_stage_is_test() {
    let file = write_yaml(
        r#"
stages: [build, test, deploy]
no_stage_specified:
  script: [echo hello]
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.jobs["no_stage_specified"].stage, "test");
}

#[test]
fn test_artifacts_expire_in_never() {
    let file = write_yaml(
        r#"
stages: [test]
test:
  script: [echo test]
  artifacts:
    paths: [important.bin]
    expire_in: never
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
        Some("never")
    );
}

#[test]
fn test_job_with_all_keywords() {
    // Comprehensive test with as many keywords as possible on one job
    let file = write_yaml(
        r#"
stages: [build, test]
variables:
  GLOBAL: "yes"

build:
  stage: build
  script: [echo build]
  artifacts:
    paths: [dist/]

full_job:
  stage: test
  image:
    name: python:3.12
    entrypoint: [""]
  before_script:
    - pip install -r requirements.txt
  script:
    - pytest
    - coverage report
  after_script:
    - echo done
  variables:
    LOCAL_VAR: "local"
  rules:
    - if: '$GLOBAL == "yes"'
      when: on_success
      allow_failure: true
      variables:
        EXTRA: "injected"
  needs:
    - job: build
      artifacts: true
      optional: false
  services:
    - name: postgres:16
      alias: db
  cache:
    - key: pip-cache
      paths: [.venv/]
      policy: pull-push
      when: always
  artifacts:
    paths: [htmlcov/]
    exclude: ["**/*.pyc"]
    expire_in: 1 day
    when: always
  timeout: 30m
  retry:
    max: 2
    when: [script_failure]
  allow_failure:
    exit_codes: [42]
  parallel:
    matrix:
      - PYTHON: ["3.11", "3.12"]
  coverage: '/TOTAL.*\s+(\d+)%/'
  tags: [docker, linux]
  interruptible: true
  resource_group: production
  inherit:
    default: true
    variables: true
"#,
    );
    let pipeline = parse_pipeline(file.path()).unwrap();
    assert!(pipeline.jobs.contains_key("full_job"));

    let job = &pipeline.jobs["full_job"];
    assert_eq!(job.image.as_ref().unwrap().name(), "python:3.12");
    assert!(job.image.as_ref().unwrap().entrypoint().is_some());
    assert!(job.before_script.is_some());
    assert_eq!(job.script.len(), 2);
    assert!(job.after_script.is_some());
    assert!(job.variables.contains_key("LOCAL_VAR"));
    assert!(job.rules.is_some());
    assert!(job.needs.is_some());
    assert!(job.services.is_some());
    assert!(job.cache.is_some());
    assert!(job.artifacts.is_some());
    assert!(job.timeout.is_some());
    assert!(job.retry.is_some());
    assert!(job.parallel.is_some());
    assert!(job.coverage.is_some());
    assert!(job.tags.is_some());
    assert_eq!(job.interruptible, Some(true));
    assert_eq!(job.resource_group.as_deref(), Some("production"));
    assert!(job.inherit.is_some());

    // Verify plan expands matrix
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    // build + 2 matrix variants of full_job
    let total: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    assert_eq!(total, 3); // build + full_job×2
}

#[test]
fn test_real_world_ci_config() {
    // Simulates a realistic CI config with all common patterns
    let file = write_yaml(
        r#"
stages:
  - lint
  - build
  - test
  - deploy

variables:
  DOCKER_DRIVER: overlay2
  APP_NAME: myservice

default:
  image: ruby:3.2
  before_script:
    - bundle install
  cache:
    - key: gems-$CI_COMMIT_REF_SLUG
      paths: [vendor/bundle/]

.deploy_template:
  stage: deploy
  before_script:
    - apt-get update && apt-get install -y aws-cli

rubocop:
  stage: lint
  script: [bundle exec rubocop]

rspec:
  stage: test
  script:
    - bundle exec rspec
  coverage: '/Coverage: (\d+\.\d+)%/'
  artifacts:
    paths: [coverage/]
    expire_in: 30 days
    when: always
  services:
    - name: postgres:16
      alias: db
    - redis:7
  needs:
    - build

build:
  stage: build
  script: [bundle exec rake build]
  artifacts:
    paths: [pkg/]

deploy_staging:
  extends: .deploy_template
  script: [./deploy.sh staging]
  environment:
    name: staging
  rules:
    - if: '$CI_COMMIT_BRANCH == "main"'
      when: always

deploy_production:
  extends: .deploy_template
  script: [./deploy.sh production]
  environment:
    name: production
  rules:
    - if: '$CI_COMMIT_BRANCH == "main"'
      when: manual
      allow_failure: false
"#,
    );

    let pipeline = parse_pipeline(file.path()).unwrap();
    assert_eq!(pipeline.stages.len(), 4);
    assert_eq!(pipeline.jobs.len(), 5); // rubocop, rspec, build, deploy_staging, deploy_production

    // Verify extends worked
    let staging = &pipeline.jobs["deploy_staging"];
    assert_eq!(staging.stage, "deploy");
    // deploy_template before_script should be inherited
    assert!(staging.before_script.is_some());

    // Verify default applied
    let rubocop = &pipeline.jobs["rubocop"];
    assert_eq!(rubocop.image.as_ref().unwrap().name(), "ruby:3.2");
    assert!(rubocop.cache.is_some());

    // Verify services
    let rspec = &pipeline.jobs["rspec"];
    assert_eq!(rspec.services.as_ref().unwrap().len(), 2);

    // Verify plan builds correctly
    let vars = pipeline.variables.clone();
    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &vars, None, None).unwrap();
    assert!(!plan.stages.is_empty());
}

// ============================================================
// Pipeline Event Simulation
// ============================================================

/// Helper: build plan with specific CI variables to simulate events.
fn plan_with_vars(
    file: &tempfile::NamedTempFile,
    extra_vars: &[(&str, &str)],
) -> (
    lab_core::model::pipeline::Pipeline,
    lab_core::model::pipeline::Plan,
) {
    use lab_core::model::rules::{RuleResult, evaluate_rules};
    use lab_core::model::variables::{Variables, merge_variables};

    let pipeline = parse_pipeline(file.path()).unwrap();

    let mut vars = Variables::new();
    for (k, v) in extra_vars {
        vars.insert(k.to_string(), VariableValue::Simple(v.to_string()));
    }
    let mut global = merge_variables(&[&pipeline.variables, &vars]);

    // Evaluate workflow:rules and merge matched variables
    if let Some(wf) = &pipeline.workflow {
        if !wf.rules.is_empty() {
            match evaluate_rules(&wf.rules, &global, When::Always) {
                RuleResult::Matched {
                    when: When::Never, ..
                }
                | RuleResult::NotMatched => {
                    return (pipeline, lab_core::model::pipeline::Plan { stages: vec![] });
                }
                RuleResult::Matched { variables, .. } => {
                    if let Some(wf_vars) = variables {
                        for (k, v) in wf_vars {
                            global.insert(k, v);
                        }
                    }
                }
            }
        }
    }

    let plan = build_plan(&pipeline.stages, &pipeline.jobs, &global, None, None).unwrap();
    (pipeline, plan)
}

fn job_names(plan: &lab_core::model::pipeline::Plan) -> Vec<String> {
    plan.stages
        .iter()
        .flat_map(|s| s.jobs.iter().map(|j| j.name.clone()))
        .collect()
}

#[test]
fn test_event_push_triggers_main_jobs() {
    let file = write_yaml(
        r#"
stages: [test, deploy]

workflow:
  rules:
    - if: $CI_PIPELINE_SOURCE == "push"
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"

test:
  stage: test
  script: [echo test]

deploy:
  stage: deploy
  script: [echo deploy]
  rules:
    - if: $CI_COMMIT_BRANCH == "main"
      when: always
    - when: never
"#,
    );
    let (_, plan) = plan_with_vars(
        &file,
        &[("CI_PIPELINE_SOURCE", "push"), ("CI_COMMIT_BRANCH", "main")],
    );
    let names = job_names(&plan);
    assert!(names.contains(&"test".to_string()));
    assert!(names.contains(&"deploy".to_string()));
}

#[test]
fn test_event_mr_excludes_deploy() {
    let file = write_yaml(
        r#"
stages: [test, deploy]

workflow:
  rules:
    - if: $CI_PIPELINE_SOURCE == "push"
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"

test:
  stage: test
  script: [echo test]

deploy:
  stage: deploy
  script: [echo deploy]
  rules:
    - if: $CI_COMMIT_BRANCH == "main"
      when: always
    - when: never
"#,
    );
    let (_, plan) = plan_with_vars(
        &file,
        &[
            ("CI_PIPELINE_SOURCE", "merge_request_event"),
            ("CI_COMMIT_BRANCH", "feature/login"),
        ],
    );
    let names = job_names(&plan);
    assert!(names.contains(&"test".to_string()));
    // deploy should NOT run — branch is not main
    assert!(!names.contains(&"deploy".to_string()));
}

#[test]
fn test_event_tag_triggers_tag_jobs() {
    let file = write_yaml(
        r#"
stages: [build, deploy]

workflow:
  rules:
    - if: $CI_PIPELINE_SOURCE == "push"
    - if: $CI_COMMIT_TAG

build:
  stage: build
  script: [echo build]

build-tagged:
  stage: build
  script: [echo build-tagged]
  rules:
    - if: $CI_COMMIT_TAG =~ /^v\d+/

deploy-prod:
  stage: deploy
  script: [echo deploy]
  rules:
    - if: $CI_COMMIT_TAG =~ /^v\d+/
      when: manual
"#,
    );
    let (_, plan) = plan_with_vars(
        &file,
        &[("CI_PIPELINE_SOURCE", "push"), ("CI_COMMIT_TAG", "v1.2.3")],
    );
    let names = job_names(&plan);
    assert!(names.contains(&"build".to_string()));
    assert!(names.contains(&"build-tagged".to_string()));
    assert!(names.contains(&"deploy-prod".to_string()));
}

#[test]
fn test_event_tag_without_match_excludes_tag_jobs() {
    let file = write_yaml(
        r#"
stages: [build]

workflow:
  rules:
    - if: $CI_PIPELINE_SOURCE == "push"

build:
  stage: build
  script: [echo build]

build-tagged:
  stage: build
  script: [echo build-tagged]
  rules:
    - if: $CI_COMMIT_TAG =~ /^v\d+/
"#,
    );
    // No CI_COMMIT_TAG set — tag job should not run
    let (_, plan) = plan_with_vars(&file, &[("CI_PIPELINE_SOURCE", "push")]);
    let names = job_names(&plan);
    assert!(names.contains(&"build".to_string()));
    assert!(!names.contains(&"build-tagged".to_string()));
}

#[test]
fn test_event_schedule() {
    let file = write_yaml(
        r#"
stages: [test, cleanup]

workflow:
  rules:
    - if: $CI_PIPELINE_SOURCE == "push"
    - if: $CI_PIPELINE_SOURCE == "schedule"

test:
  stage: test
  script: [echo test]

nightly-cleanup:
  stage: cleanup
  script: [echo cleanup]
  rules:
    - if: $CI_PIPELINE_SOURCE == "schedule"
"#,
    );
    // Schedule event should trigger nightly-cleanup
    let (_, plan) = plan_with_vars(&file, &[("CI_PIPELINE_SOURCE", "schedule")]);
    let names = job_names(&plan);
    assert!(names.contains(&"test".to_string()));
    assert!(names.contains(&"nightly-cleanup".to_string()));

    // Push event should NOT trigger nightly-cleanup
    let (_, plan) = plan_with_vars(&file, &[("CI_PIPELINE_SOURCE", "push")]);
    let names = job_names(&plan);
    assert!(names.contains(&"test".to_string()));
    assert!(!names.contains(&"nightly-cleanup".to_string()));
}

#[test]
fn test_event_web_with_workflow_variables() {
    // Tests that workflow:rules:variables are merged into the context
    let file = write_yaml(
        r#"
stages: [test, deploy]

workflow:
  rules:
    - if: $CI_PIPELINE_SOURCE == "web"
      variables:
        PIPELINE_TYPE: "manual_deploy"
    - if: $CI_COMMIT_BRANCH

test:
  stage: test
  script: [echo test]
  rules:
    - if: $PIPELINE_TYPE == "manual_deploy"
      when: never
    - when: on_success

manual-deploy:
  stage: deploy
  script: [echo deploy]
  rules:
    - if: $PIPELINE_TYPE == "manual_deploy"
"#,
    );
    // Web trigger should inject PIPELINE_TYPE=manual_deploy
    let (_, plan) = plan_with_vars(
        &file,
        &[("CI_PIPELINE_SOURCE", "web"), ("CI_COMMIT_BRANCH", "main")],
    );
    let names = job_names(&plan);
    // test should be excluded (when: never for manual_deploy)
    assert!(!names.contains(&"test".to_string()));
    // manual-deploy should run
    assert!(names.contains(&"manual-deploy".to_string()));

    // Push trigger should NOT inject PIPELINE_TYPE
    let (_, plan) = plan_with_vars(
        &file,
        &[("CI_PIPELINE_SOURCE", "push"), ("CI_COMMIT_BRANCH", "main")],
    );
    let names = job_names(&plan);
    assert!(names.contains(&"test".to_string()));
    assert!(!names.contains(&"manual-deploy".to_string()));
}

#[test]
fn test_event_workflow_blocks_unknown_source() {
    let file = write_yaml(
        r#"
stages: [test]

workflow:
  rules:
    - if: $CI_PIPELINE_SOURCE == "push"
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"

test:
  stage: test
  script: [echo test]
"#,
    );
    // Unknown source should be blocked by workflow:rules
    let (_, plan) = plan_with_vars(&file, &[("CI_PIPELINE_SOURCE", "chat")]);
    assert!(plan.stages.is_empty());
}

#[test]
fn test_event_trigger_downstream() {
    let file = write_yaml(
        r#"
stages: [test]

workflow:
  rules:
    - if: $CI_PIPELINE_SOURCE == "pipeline"
    - if: $CI_PIPELINE_SOURCE == "push"

test:
  stage: test
  script: [echo test]

downstream-only:
  stage: test
  script: [echo downstream]
  rules:
    - if: $CI_PIPELINE_SOURCE == "pipeline"
"#,
    );
    // Multi-project pipeline source
    let (_, plan) = plan_with_vars(&file, &[("CI_PIPELINE_SOURCE", "pipeline")]);
    let names = job_names(&plan);
    assert!(names.contains(&"test".to_string()));
    assert!(names.contains(&"downstream-only".to_string()));

    // Push should not trigger downstream-only
    let (_, plan) = plan_with_vars(&file, &[("CI_PIPELINE_SOURCE", "push")]);
    let names = job_names(&plan);
    assert!(names.contains(&"test".to_string()));
    assert!(!names.contains(&"downstream-only".to_string()));
}

#[test]
fn test_event_multiple_rules_first_match_wins() {
    let file = write_yaml(
        r#"
stages: [deploy]

workflow:
  rules:
    - if: $CI_PIPELINE_SOURCE == "web"
      variables:
        TARGET: "manual"
    - if: $CI_COMMIT_BRANCH == "main"
      variables:
        TARGET: "auto"
    - when: never

deploy:
  stage: deploy
  script: [echo "deploying to $TARGET"]
"#,
    );
    // Web should match first rule → TARGET=manual
    let (_, plan) = plan_with_vars(
        &file,
        &[("CI_PIPELINE_SOURCE", "web"), ("CI_COMMIT_BRANCH", "main")],
    );
    assert!(!plan.stages.is_empty());

    // Push on main should match second rule → TARGET=auto
    let (_, plan) = plan_with_vars(
        &file,
        &[("CI_PIPELINE_SOURCE", "push"), ("CI_COMMIT_BRANCH", "main")],
    );
    assert!(!plan.stages.is_empty());

    // Push on feature branch → no rule matches → blocked
    let (_, plan) = plan_with_vars(
        &file,
        &[
            ("CI_PIPELINE_SOURCE", "push"),
            ("CI_COMMIT_BRANCH", "feature"),
        ],
    );
    assert!(plan.stages.is_empty());
}

#[test]
fn test_event_draft_commit_blocked() {
    // Simulates: workflow:rules blocks pipelines with -draft suffix in commit title
    let file = write_yaml(
        r#"
stages: [test]

workflow:
  rules:
    - if: $CI_COMMIT_TITLE =~ /-draft$/
      when: never
    - if: $CI_PIPELINE_SOURCE == "push"

test:
  stage: test
  script: [echo test]
"#,
    );
    // Draft commit → blocked
    let (_, plan) = plan_with_vars(
        &file,
        &[
            ("CI_PIPELINE_SOURCE", "push"),
            ("CI_COMMIT_TITLE", "wip: my-draft"),
        ],
    );
    assert!(plan.stages.is_empty());

    // Normal commit → runs
    let (_, plan) = plan_with_vars(
        &file,
        &[
            ("CI_PIPELINE_SOURCE", "push"),
            ("CI_COMMIT_TITLE", "feat: add login"),
        ],
    );
    assert!(!plan.stages.is_empty());
}
