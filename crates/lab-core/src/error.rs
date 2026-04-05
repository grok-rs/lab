use std::path::PathBuf;

/// Core error type for lab operations.
#[derive(Debug, thiserror::Error)]
pub enum LabError {
    #[error("failed to parse YAML: {0}")]
    YamlParse(#[from] serde_yaml::Error),

    #[error("failed to read {path}: {source}")]
    FileRead {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("invalid pipeline configuration: {0}")]
    InvalidConfig(String),

    #[error("job {0:?} not found")]
    JobNotFound(String),

    #[error("stage {0:?} not found")]
    StageNotFound(String),

    #[error("circular dependency detected involving job {0:?}")]
    CircularDependency(String),

    #[error("job {job:?} references unknown dependency {dependency:?}")]
    UnknownDependency { job: String, dependency: String },

    #[error("docker error: {0}")]
    Docker(#[from] bollard::errors::Error),

    #[error("container exited with code {code}")]
    ContainerFailed { code: i64 },

    #[error("variable expansion error: {0}")]
    VariableExpansion(String),

    #[error("rule evaluation error: {0}")]
    RuleEvaluation(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, LabError>;
