use indexmap::IndexMap;

use super::job::{Job, JobDefaults};
use super::rules::Rule;
use super::variables::Variables;

/// A fully parsed and resolved GitLab CI pipeline.
/// Ref: <https://docs.gitlab.com/ci/yaml/>
#[derive(Debug, Clone)]
pub struct Pipeline {
    /// Ordered list of stage names.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#stages>
    pub stages: Vec<String>,

    /// Global variables.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#variables>
    pub variables: Variables,

    /// Default settings applied to all jobs.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#default>
    pub defaults: JobDefaults,

    /// Job definitions keyed by name (insertion-ordered).
    pub jobs: IndexMap<String, Job>,

    /// Pipeline-level workflow configuration.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#workflow>
    pub workflow: Option<WorkflowConfig>,
}

/// Workflow-level configuration controlling pipeline creation.
/// Ref: <https://docs.gitlab.com/ci/yaml/#workflow>
#[derive(Debug, Clone)]
pub struct WorkflowConfig {
    /// Rules that determine whether the pipeline is created.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#workflowrules>
    pub rules: Vec<Rule>,

    /// Pipeline name (can contain CI/CD variables).
    pub name: Option<String>,

    /// Auto-cancel configuration.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#workflowauto_cancelon_new_commit>
    pub auto_cancel: Option<AutoCancelConfig>,
}

/// Auto-cancel configuration for workflow.
/// Ref: <https://docs.gitlab.com/ci/yaml/#workflowauto_cancel>
#[derive(Debug, Clone, Default)]
pub struct AutoCancelConfig {
    pub on_new_commit: Option<String>,
    pub on_job_failure: Option<String>,
}

/// Default stages if not specified.
/// Ref: <https://docs.gitlab.com/ci/yaml/#stages>
pub fn default_stages() -> Vec<String> {
    vec![
        ".pre".to_string(),
        "build".to_string(),
        "test".to_string(),
        "deploy".to_string(),
        ".post".to_string(),
    ]
}

/// Execution plan — DAG resolved into ordered stages with parallel jobs.
#[derive(Debug, Clone)]
pub struct Plan {
    pub stages: Vec<Stage>,
}

/// A group of jobs that can run in parallel.
#[derive(Debug, Clone)]
pub struct Stage {
    pub name: String,
    pub jobs: Vec<PlannedJob>,
}

/// A job scheduled for execution, potentially expanded from a matrix.
#[derive(Debug, Clone)]
pub struct PlannedJob {
    pub name: String,
    pub job: Job,
    pub matrix_entry: Option<IndexMap<String, String>>,
}
