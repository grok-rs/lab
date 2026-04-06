use std::collections::HashMap;
use std::path::PathBuf;

/// Runtime configuration for a lab execution.
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to .gitlab-ci.yml (default: .gitlab-ci.yml in working directory)
    pub ci_file: PathBuf,

    /// Working directory (project root)
    pub workdir: PathBuf,

    /// Specific jobs to run (None = run all)
    pub job_filter: Option<Vec<String>>,

    /// Specific stage to run (None = run all)
    pub stage_filter: Option<String>,

    /// User-provided CI/CD variables (-v KEY=VALUE)
    pub variables: HashMap<String, String>,

    /// Image pull policy
    pub pull_policy: PullPolicy,

    /// Run containers in privileged mode (for dind)
    pub privileged: bool,

    /// Disable artifact passing between jobs
    pub no_artifacts: bool,

    /// Disable cache
    pub no_cache: bool,

    /// Platform overrides (job_name=image)
    pub platform_overrides: HashMap<String, String>,

    /// Maximum parallel jobs (default: number of CPUs)
    pub max_parallel: usize,

    /// How to handle manual jobs
    pub manual_mode: ManualMode,

    /// CPU limit for containers (e.g., 1.5 = 1.5 cores).
    pub cpus: Option<f64>,

    /// Memory limit for containers (in bytes).
    pub memory: Option<i64>,
}

/// How to handle `when: manual` jobs.
#[derive(Debug, Clone, Copy, Default)]
pub enum ManualMode {
    /// Prompt the user interactively (default).
    #[default]
    Prompt,
    /// Auto-approve all manual jobs.
    Approve,
    /// Auto-skip all manual jobs.
    Skip,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum PullPolicy {
    Always,
    #[default]
    IfNotPresent,
    Never,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ci_file: PathBuf::from(".gitlab-ci.yml"),
            workdir: PathBuf::from("."),
            job_filter: None,
            stage_filter: None,
            variables: HashMap::new(),
            pull_policy: PullPolicy::default(),
            privileged: false,
            no_artifacts: false,
            no_cache: false,
            platform_overrides: HashMap::new(),
            max_parallel: num_cpus(),
            manual_mode: ManualMode::default(),
            cpus: None,
            memory: None,
        }
    }
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Project-level config file (`.lab.yml`).
/// Applied as defaults — CLI flags override these values.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ProjectConfig {
    /// Default CI/CD variables.
    #[serde(default)]
    pub variables: HashMap<String, String>,

    /// Default image override for all jobs.
    #[serde(default)]
    pub image: Option<String>,

    /// Default pull policy.
    #[serde(default)]
    pub pull_policy: Option<String>,

    /// Run in privileged mode by default.
    #[serde(default)]
    pub privileged: Option<bool>,

    /// Maximum parallel jobs.
    #[serde(default)]
    pub max_parallel: Option<usize>,

    /// Platform overrides (job_name=image).
    #[serde(default)]
    pub platforms: HashMap<String, String>,

    /// Local project path mappings for `include:project`.
    /// Maps GitLab project paths to local filesystem paths,
    /// avoiding API calls for locally cloned repos.
    /// Example: `repo-level/cicd/gitlab-pipelines: /home/user/work/gitlab-pipelines`
    #[serde(default)]
    pub projects: HashMap<String, String>,
}

impl ProjectConfig {
    /// Load from `.lab.yml` in the given directory. Returns default if not found.
    pub fn load(workdir: &std::path::Path) -> Self {
        let path = workdir.join(".lab.yml");
        if !path.exists() {
            return Self::default();
        }

        match std::fs::read_to_string(&path) {
            Ok(content) => serde_yaml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Apply project config as defaults to a Config (CLI values take precedence).
    pub fn apply_to(&self, config: &mut Config) {
        // Variables from .lab.yml are lower precedence than CLI -v flags
        for (k, v) in &self.variables {
            config
                .variables
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }

        if let Some(policy_str) = &self.pull_policy {
            // Only override if CLI didn't set it (we can't tell, so just set it)
            config.pull_policy = match policy_str.as_str() {
                "always" => PullPolicy::Always,
                "never" => PullPolicy::Never,
                _ => PullPolicy::IfNotPresent,
            };
        }

        if let Some(priv_mode) = self.privileged {
            if !config.privileged {
                config.privileged = priv_mode;
            }
        }

        if let Some(max_par) = self.max_parallel {
            config.max_parallel = max_par;
        }

        for (k, v) in &self.platforms {
            config
                .platform_overrides
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
    }
}
