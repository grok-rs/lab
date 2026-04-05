use std::sync::Arc;

use crate::config::Config;
use crate::docker::client::DockerClient;
use crate::model::variables::Variables;
use crate::secrets::SecretMasker;

/// Per-job execution context holding all state needed to run a job.
#[derive(Debug)]
pub struct JobContext {
    /// Job name.
    pub name: String,

    /// Stage name.
    pub stage: String,

    /// Docker image to use.
    pub image: String,

    /// Merged variables (predefined + global + job-level).
    pub variables: Variables,

    /// Docker client.
    pub docker: Arc<DockerClient>,

    /// Runtime configuration.
    pub config: Arc<Config>,

    /// Container ID once created.
    pub container_id: Option<String>,

    /// Job exit code (set after execution).
    pub exit_code: Option<i64>,

    /// Secret masker — replaces secret values in output with [MASKED].
    pub masker: SecretMasker,
}

impl JobContext {
    pub fn new(
        name: String,
        stage: String,
        image: String,
        variables: Variables,
        docker: Arc<DockerClient>,
        config: Arc<Config>,
        masker: SecretMasker,
    ) -> Self {
        Self {
            name,
            stage,
            image,
            variables,
            docker,
            config,
            container_id: None,
            exit_code: None,
            masker,
        }
    }
}
