use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// Run GitLab CI/CD pipelines locally.
#[derive(Parser, Debug)]
#[command(name = "lab", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run pipeline jobs locally in Docker containers.
    Run {
        /// Specific job to run (runs all if omitted).
        #[arg()]
        job: Option<String>,

        /// Run all jobs in a specific stage.
        #[arg(long)]
        stage: Option<String>,

        /// Path to .gitlab-ci.yml.
        #[arg(short, long, default_value = ".gitlab-ci.yml")]
        file: PathBuf,

        /// Set CI/CD variable (can be repeated).
        #[arg(short = 'v', long = "var", value_parser = parse_key_val)]
        variables: Vec<(String, String)>,

        /// Simulate a pipeline event type. Sets CI_PIPELINE_SOURCE and related variables.
        /// Values: push, merge_request_event, schedule, web, api, trigger, pipeline,
        /// parent_pipeline, chat, webide, external_pull_request_event.
        #[arg(long)]
        event: Option<String>,

        /// Simulate a tag pipeline. Sets CI_COMMIT_TAG and CI_PIPELINE_SOURCE=push.
        /// Example: --tag myapp/v1.2.3
        #[arg(long)]
        tag: Option<String>,

        /// Image pull policy.
        #[arg(long, default_value = "if-not-present")]
        pull_policy: PullPolicyArg,

        /// Run containers in privileged mode (for Docker-in-Docker).
        #[arg(long)]
        privileged: bool,

        /// Disable artifact passing between jobs.
        #[arg(long)]
        no_artifacts: bool,

        /// Disable cache.
        #[arg(long)]
        no_cache: bool,

        /// Override image for a job (job=image, can be repeated).
        #[arg(short = 'P', long = "platform", value_parser = parse_key_val)]
        platforms: Vec<(String, String)>,

        /// Maximum number of parallel jobs.
        #[arg(long)]
        max_parallel: Option<usize>,

        /// Auto-approve all manual jobs (no interactive prompt).
        #[arg(long, conflicts_with = "skip_manual")]
        approve_manual: bool,

        /// Auto-skip all manual jobs (no interactive prompt).
        #[arg(long, conflicts_with = "approve_manual")]
        skip_manual: bool,

        /// Pull secrets from GitLab before running (via glab).
        #[arg(long)]
        pull_secrets: bool,

        /// Skip loading secrets from .lab/secrets.env.
        #[arg(long)]
        no_secrets: bool,

        /// Use a custom secrets file instead of .lab/secrets.env.
        #[arg(long = "secrets")]
        secrets_file: Option<PathBuf>,

        /// Show execution plan without running containers.
        #[arg(long)]
        dry_run: bool,

        /// Skip pre-flight variable check (run even with missing secrets).
        #[arg(long)]
        no_preflight: bool,

        /// Verbose output.
        #[arg(long)]
        verbose: bool,
    },

    /// List all jobs and stages in the pipeline.
    List {
        /// Path to .gitlab-ci.yml.
        #[arg(short, long, default_value = ".gitlab-ci.yml")]
        file: PathBuf,
    },

    /// Parse and validate .gitlab-ci.yml without running.
    Validate {
        /// Path to .gitlab-ci.yml.
        #[arg(short, long, default_value = ".gitlab-ci.yml")]
        file: PathBuf,
    },

    /// Show the job dependency graph.
    Graph {
        /// Path to .gitlab-ci.yml.
        #[arg(short, long, default_value = ".gitlab-ci.yml")]
        file: PathBuf,
    },

    /// Analyze pipeline for security, performance, and best practice issues.
    Analyze {
        /// Path to .gitlab-ci.yml.
        #[arg(short, long, default_value = ".gitlab-ci.yml")]
        file: PathBuf,

        /// Output format: text or json.
        #[arg(long, default_value = "text")]
        output: OutputFormat,
    },

    /// Drop into an interactive shell inside a job's container for debugging.
    Shell {
        /// Job name to create a container for.
        #[arg()]
        job: String,

        /// Path to .gitlab-ci.yml.
        #[arg(short, long, default_value = ".gitlab-ci.yml")]
        file: PathBuf,

        /// Shell to use inside container (default: auto-detect).
        #[arg(long)]
        shell: Option<String>,
    },

    /// Start MCP (Model Context Protocol) server for AI agent integration.
    /// Reads JSON-RPC from stdin, writes responses to stdout.
    #[command(name = "mcp-server")]
    McpServer,

    /// Generate shell completions for bash, zsh, or fish.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Manage CI/CD secrets for local pipeline execution.
    Secrets {
        #[command(subcommand)]
        action: SecretsAction,

        /// Path to .gitlab-ci.yml.
        #[arg(short, long, default_value = ".gitlab-ci.yml")]
        file: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
pub enum SecretsAction {
    /// Pull secrets from GitLab project and group variables (via glab).
    Pull {
        /// Fetch from a specific group instead of auto-detecting.
        #[arg(short, long)]
        group: Option<String>,
    },

    /// Check which secrets are available vs missing.
    Check,

    /// Generate .lab/secrets.env.example template from pipeline.
    Init,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum PullPolicyArg {
    Always,
    IfNotPresent,
    Never,
}

impl From<PullPolicyArg> for lab_core::config::PullPolicy {
    fn from(arg: PullPolicyArg) -> Self {
        match arg {
            PullPolicyArg::Always => Self::Always,
            PullPolicyArg::IfNotPresent => Self::IfNotPresent,
            PullPolicyArg::Never => Self::Never,
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=VALUE: no '=' found in '{s}'"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}
