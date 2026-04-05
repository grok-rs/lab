use std::path::Path;

use indexmap::IndexMap;
use serde_yaml::Value;

use crate::error::{LabError, Result};
use crate::model::job::{Job, JobDefaults};
use crate::model::pipeline::{Pipeline, WorkflowConfig, default_stages};
use crate::model::rules::Rule;
use crate::model::variables::Variables;

/// Reserved top-level keywords that are NOT job definitions.
/// Ref: <https://docs.gitlab.com/ci/yaml/>
const RESERVED_KEYWORDS: &[&str] = &[
    "default",
    "include",
    "stages",
    "variables",
    "workflow",
    "spec",
];

/// Parse a `.gitlab-ci.yml` file into a fully resolved Pipeline.
pub fn parse_pipeline(path: &Path) -> Result<Pipeline> {
    let value = super::yaml::load_and_resolve(path)?;

    let mapping = value
        .as_mapping()
        .ok_or_else(|| LabError::InvalidConfig("root must be a YAML mapping".into()))?;

    // Extract global config
    let stages = parse_stages(mapping);
    let variables = parse_global_variables(mapping)?;
    let defaults = parse_defaults(mapping)?;
    let workflow = parse_workflow(mapping)?;

    // Separate jobs from global keywords
    let mut jobs = parse_jobs(mapping)?;

    // Apply default: values to jobs that don't override them
    // Ref: <https://docs.gitlab.com/ci/yaml/#default>
    apply_defaults(&mut jobs, &defaults);

    // Validate all jobs reference known stages
    for (name, job) in &jobs {
        if !stages.contains(&job.stage) {
            return Err(LabError::InvalidConfig(format!(
                "job {name:?} references unknown stage {:?}",
                job.stage,
            )));
        }
    }

    Ok(Pipeline {
        stages,
        variables,
        defaults,
        jobs,
        workflow,
    })
}

fn parse_stages(mapping: &serde_yaml::Mapping) -> Vec<String> {
    let key = Value::String("stages".into());
    match mapping.get(&key) {
        Some(Value::Sequence(seq)) => seq
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => default_stages(),
    }
}

fn parse_global_variables(mapping: &serde_yaml::Mapping) -> Result<Variables> {
    let key = Value::String("variables".into());
    match mapping.get(&key) {
        Some(value) => serde_yaml::from_value(value.clone()).map_err(Into::into),
        None => Ok(Variables::new()),
    }
}

fn parse_defaults(mapping: &serde_yaml::Mapping) -> Result<JobDefaults> {
    let key = Value::String("default".into());
    match mapping.get(&key) {
        Some(value) => serde_yaml::from_value(value.clone()).map_err(Into::into),
        None => Ok(JobDefaults::default()),
    }
}

fn parse_workflow(mapping: &serde_yaml::Mapping) -> Result<Option<WorkflowConfig>> {
    let key = Value::String("workflow".into());
    let workflow_value = match mapping.get(&key) {
        Some(v) => v,
        None => return Ok(None),
    };

    let workflow_map = workflow_value
        .as_mapping()
        .ok_or_else(|| LabError::InvalidConfig("workflow must be a mapping".into()))?;

    let rules_key = Value::String("rules".into());
    let rules: Vec<Rule> = match workflow_map.get(&rules_key) {
        Some(v) => serde_yaml::from_value(v.clone())?,
        None => vec![],
    };

    let name_key = Value::String("name".into());
    let name = workflow_map
        .get(&name_key)
        .and_then(|v| v.as_str())
        .map(String::from);

    // Parse auto_cancel
    let auto_cancel_key = Value::String("auto_cancel".into());
    let auto_cancel = workflow_map.get(&auto_cancel_key).and_then(|v| {
        let m = v.as_mapping()?;
        Some(crate::model::pipeline::AutoCancelConfig {
            on_new_commit: m
                .get(Value::String("on_new_commit".into()))
                .and_then(|v| v.as_str())
                .map(String::from),
            on_job_failure: m
                .get(Value::String("on_job_failure".into()))
                .and_then(|v| v.as_str())
                .map(String::from),
        })
    });

    Ok(Some(WorkflowConfig {
        rules,
        name,
        auto_cancel,
    }))
}

fn parse_jobs(mapping: &serde_yaml::Mapping) -> Result<IndexMap<String, Job>> {
    let mut jobs = IndexMap::new();

    for (key, value) in mapping {
        let name = match key.as_str() {
            Some(n) => n,
            None => continue,
        };

        // Skip reserved keywords
        if RESERVED_KEYWORDS.contains(&name) {
            continue;
        }

        // Skip hidden jobs (templates starting with '.')
        if name.starts_with('.') {
            continue;
        }

        // Parse as a job
        let job: Job = serde_yaml::from_value(value.clone())
            .map_err(|e| LabError::InvalidConfig(format!("error parsing job {name:?}: {e}")))?;

        jobs.insert(name.to_string(), job);
    }

    Ok(jobs)
}

/// Apply `default:` values to jobs that don't override them.
/// Respects `inherit:default` to control which defaults a job receives.
///
/// Ref: <https://docs.gitlab.com/ci/yaml/#default>
/// Ref: <https://docs.gitlab.com/ci/yaml/#inherit>
fn apply_defaults(jobs: &mut IndexMap<String, Job>, defaults: &JobDefaults) {
    use crate::model::job::InheritToggle;

    for job in jobs.values_mut() {
        // Check inherit:default — controls which default: keywords this job inherits
        let should_inherit = |keyword: &str| -> bool {
            match &job.inherit {
                Some(cfg) => match &cfg.default {
                    Some(InheritToggle::Bool(false)) => false, // inherit nothing
                    Some(InheritToggle::Bool(true)) | None => true, // inherit all
                    Some(InheritToggle::List(list)) => list.iter().any(|k| k == keyword),
                },
                None => true, // no inherit config → inherit all
            }
        };

        if job.image.is_none() && should_inherit("image") {
            job.image.clone_from(&defaults.image);
        }
        if job.before_script.is_none() && should_inherit("before_script") {
            job.before_script.clone_from(&defaults.before_script);
        }
        if job.after_script.is_none() && should_inherit("after_script") {
            job.after_script.clone_from(&defaults.after_script);
        }
        if job.services.is_none() && should_inherit("services") {
            job.services.clone_from(&defaults.services);
        }
        if job.cache.is_none() && should_inherit("cache") {
            job.cache.clone_from(&defaults.cache);
        }
        if job.artifacts.is_none() && should_inherit("artifacts") {
            job.artifacts.clone_from(&defaults.artifacts);
        }
        if job.retry.is_none() && should_inherit("retry") {
            job.retry.clone_from(&defaults.retry);
        }
        if job.timeout.is_none() && should_inherit("timeout") {
            job.timeout = defaults.timeout;
        }
        if job.interruptible.is_none() && should_inherit("interruptible") {
            job.interruptible = defaults.interruptible;
        }
        if job.tags.is_none() && should_inherit("tags") {
            job.tags.clone_from(&defaults.tags);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_yaml(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file
    }

    #[test]
    fn test_simple_pipeline() {
        let file = write_yaml(
            r#"
stages:
  - build
  - test

variables:
  RUST_VERSION: "1.80"

build:
  stage: build
  image: rust:latest
  script:
    - cargo build

test:
  stage: test
  image: rust:latest
  script:
    - cargo test
  needs:
    - build
"#,
        );

        let pipeline = parse_pipeline(file.path()).unwrap();
        assert_eq!(pipeline.stages, vec!["build", "test"]);
        assert_eq!(pipeline.jobs.len(), 2);
        assert!(pipeline.jobs.contains_key("build"));
        assert!(pipeline.jobs.contains_key("test"));
        assert_eq!(
            pipeline.variables.get("RUST_VERSION").unwrap().value(),
            "1.80"
        );
    }

    #[test]
    fn test_default_stages() {
        let file = write_yaml(
            r#"
test:
  script:
    - echo "hello"
"#,
        );

        let pipeline = parse_pipeline(file.path()).unwrap();
        assert_eq!(pipeline.stages, default_stages());
        assert_eq!(pipeline.jobs["test"].stage, "test"); // default stage
    }

    #[test]
    fn test_hidden_jobs_excluded() {
        let file = write_yaml(
            r#"
stages:
  - test

.template:
  image: node:18

test:
  extends: .template
  script:
    - npm test
"#,
        );

        let pipeline = parse_pipeline(file.path()).unwrap();
        assert_eq!(pipeline.jobs.len(), 1);
        assert!(!pipeline.jobs.contains_key(".template"));
    }

    #[test]
    fn test_defaults_applied() {
        let file = write_yaml(
            r#"
stages:
  - test

default:
  image: node:20
  before_script:
    - echo "setup"

test_with_default:
  script:
    - echo "test"

test_with_override:
  image: python:3.12
  script:
    - echo "test"
"#,
        );

        let pipeline = parse_pipeline(file.path()).unwrap();
        // Job without image gets default
        let job1 = &pipeline.jobs["test_with_default"];
        assert_eq!(job1.image.as_ref().unwrap().name(), "node:20");
        assert_eq!(
            job1.before_script.as_ref().unwrap(),
            &vec!["echo \"setup\"".to_string()]
        );

        // Job with own image keeps it
        let job2 = &pipeline.jobs["test_with_override"];
        assert_eq!(job2.image.as_ref().unwrap().name(), "python:3.12");
        // Still gets before_script from default
        assert!(job2.before_script.is_some());
    }

    #[test]
    fn test_extends_with_after_script() {
        let file = write_yaml(
            r#"
stages:
  - test

.base:
  after_script:
    - echo "cleanup"

test:
  extends: .base
  script:
    - echo "test"
"#,
        );

        let pipeline = parse_pipeline(file.path()).unwrap();
        let job = &pipeline.jobs["test"];
        assert_eq!(
            job.after_script.as_ref().unwrap(),
            &vec!["echo \"cleanup\"".to_string()]
        );
    }
}
