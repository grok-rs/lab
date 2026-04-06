use std::path::Path;

use indexmap::IndexMap;
use serde::Deserialize;

use crate::config::Config;
use crate::error::Result;

/// CI/CD variables map — ordered to preserve declaration order.
/// Ref: <https://docs.gitlab.com/ci/variables/>
pub type Variables = IndexMap<String, VariableValue>;

/// A CI/CD variable value with optional metadata.
/// Ref: <https://docs.gitlab.com/ci/yaml/#variables>
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum VariableValue {
    /// Simple string value.
    Simple(String),
    /// Detailed variable with description and options.
    Detailed {
        value: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default = "default_true")]
        expand: bool,
        #[serde(default)]
        options: Option<Vec<String>>,
    },
}

fn default_true() -> bool {
    true
}

impl VariableValue {
    pub fn value(&self) -> &str {
        match self {
            Self::Simple(v) => v,
            Self::Detailed { value, .. } => value,
        }
    }

    pub fn should_expand(&self) -> bool {
        match self {
            Self::Simple(_) => true,
            Self::Detailed { expand, .. } => *expand,
        }
    }
}

impl From<String> for VariableValue {
    fn from(s: String) -> Self {
        Self::Simple(s)
    }
}

impl From<&str> for VariableValue {
    fn from(s: &str) -> Self {
        Self::Simple(s.to_string())
    }
}

/// Expand `$VAR` and `${VAR}` references in a string.
/// Ref: <https://docs.gitlab.com/ci/variables/where_variables_can_be_used/>
///
/// Handles:
/// - `$VAR_NAME` — simple variable reference
/// - `${VAR_NAME}` — braced variable reference
/// - `$$` — escaped literal `$`
///
/// Expansion is recursive: variable values may themselves contain references.
pub fn expand_variables(input: &str, vars: &Variables) -> String {
    expand_recursive(input, vars, 0)
}

const MAX_EXPANSION_DEPTH: u32 = 10;

fn expand_recursive(input: &str, vars: &Variables, depth: u32) -> String {
    if depth >= MAX_EXPANSION_DEPTH {
        return input.to_string();
    }

    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '$' {
            result.push(ch);
            continue;
        }

        // Check for escaped $$
        if chars.peek() == Some(&'$') {
            chars.next();
            result.push('$');
            continue;
        }

        // Check for ${VAR} syntax
        if chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let var_name: String = chars.by_ref().take_while(|c| *c != '}').collect();
            if let Some(val) = lookup_var(&var_name, vars) {
                let expanded = expand_recursive(val, vars, depth + 1);
                result.push_str(&expanded);
            }
            continue;
        }

        // $VAR syntax — variable name is [A-Za-z_][A-Za-z0-9_]*
        let mut var_name = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                var_name.push(c);
                chars.next();
            } else {
                break;
            }
        }

        if var_name.is_empty() {
            result.push('$');
        } else if let Some(val) = lookup_var(&var_name, vars) {
            let expanded = expand_recursive(val, vars, depth + 1);
            result.push_str(&expanded);
        } else {
            // Leave unresolved variables as-is (GitLab behavior)
            result.push('$');
            result.push_str(&var_name);
        }
    }

    result
}

fn lookup_var<'a>(name: &str, vars: &'a Variables) -> Option<&'a str> {
    vars.get(name).map(|v| v.value())
}

/// Build the flat string map from Variables for passing to containers.
pub fn to_env_map(vars: &Variables) -> IndexMap<String, String> {
    vars.iter()
        .map(|(k, v)| (k.clone(), v.value().to_string()))
        .collect()
}

/// Build predefined CI/CD variables for local execution.
/// Ref: <https://docs.gitlab.com/ci/variables/predefined_variables/>
pub fn predefined_variables(config: &Config, job_name: &str, stage: &str) -> Result<Variables> {
    let workdir = config
        .workdir
        .canonicalize()
        .map_err(|e| crate::error::LabError::FileRead {
            path: config.workdir.clone(),
            source: e,
        })?;

    let project_name = workdir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let (commit_sha, commit_branch, commit_message) = read_git_info(&workdir);

    let mut vars = Variables::new();

    // Core identification
    set(&mut vars, "GITLAB_CI", "true");
    set(&mut vars, "CI", "true");
    set(&mut vars, "CI_LOCAL", "true"); // lab-specific flag (like ACT=true)
    set(&mut vars, "CI_SERVER", "yes");

    // Project
    set(&mut vars, "CI_PROJECT_NAME", &project_name);
    set(
        &mut vars,
        "CI_PROJECT_DIR",
        workdir.to_str().unwrap_or("/builds/project"),
    );
    set(
        &mut vars,
        "CI_PROJECT_PATH",
        &format!("local/{project_name}"),
    );
    set(&mut vars, "CI_PROJECT_NAMESPACE", "local");

    // Commit
    set(&mut vars, "CI_COMMIT_SHA", &commit_sha);
    set(
        &mut vars,
        "CI_COMMIT_SHORT_SHA",
        &commit_sha.chars().take(8).collect::<String>(),
    );
    set(&mut vars, "CI_COMMIT_BRANCH", &commit_branch);
    set(&mut vars, "CI_COMMIT_REF_NAME", &commit_branch);
    set(&mut vars, "CI_COMMIT_MESSAGE", &commit_message);

    // Set CI_COMMIT_BEFORE_SHA for Nx affected detection
    // Use the merge-base with the default branch (like GitLab MR pipelines)
    let before_sha = std::process::Command::new("git")
        .args(["merge-base", "HEAD", "origin/main"])
        .current_dir(&workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "0000000000000000000000000000000000000000".to_string());
    set(&mut vars, "CI_COMMIT_BEFORE_SHA", &before_sha);

    // Detect default branch
    let default_branch = std::process::Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD", "--short"])
        .current_dir(&workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_start_matches("origin/").to_string())
        .unwrap_or_else(|| "main".to_string());

    // Auto-detect pipeline source based on branch
    // On default branch → push, on other branches → merge_request_event
    let pipeline_source = if commit_branch == default_branch {
        "push"
    } else {
        "merge_request_event"
    };

    // Pipeline
    set(&mut vars, "CI_PIPELINE_ID", "0");
    set(&mut vars, "CI_PIPELINE_SOURCE", pipeline_source);

    // Job
    set(&mut vars, "CI_JOB_NAME", job_name);
    set(&mut vars, "CI_JOB_STAGE", stage);
    set(&mut vars, "CI_JOB_ID", "0");

    // Runner
    set(&mut vars, "CI_RUNNER_ID", "0");
    set(&mut vars, "CI_RUNNER_DESCRIPTION", "lab-local");

    // Default branch
    set(&mut vars, "CI_DEFAULT_BRANCH", &default_branch);

    // MR-specific variables (when on a feature branch)
    if pipeline_source == "merge_request_event" {
        set(
            &mut vars,
            "CI_MERGE_REQUEST_SOURCE_BRANCH_NAME",
            &commit_branch,
        );
        set(
            &mut vars,
            "CI_MERGE_REQUEST_TARGET_BRANCH_NAME",
            &default_branch,
        );
    }

    Ok(vars)
}

fn set(vars: &mut Variables, key: &str, value: &str) {
    vars.insert(key.to_string(), VariableValue::Simple(value.to_string()));
}

/// Read basic git info from the working directory.
fn read_git_info(workdir: &Path) -> (String, String, String) {
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "0000000000000000000000000000000000000000".to_string());

    let branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "main".to_string());

    let message = std::process::Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    (sha, branch, message)
}

/// Merge multiple variable sources with proper precedence.
/// Later sources override earlier ones.
/// Ref: <https://docs.gitlab.com/ci/variables/#cicd-variable-precedence>
pub fn merge_variables(sources: &[&Variables]) -> Variables {
    let mut merged = Variables::new();
    for source in sources {
        for (k, v) in *source {
            merged.insert(k.clone(), v.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Variables {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), VariableValue::Simple(v.to_string())))
            .collect()
    }

    #[test]
    fn test_simple_expansion() {
        let v = vars(&[("NAME", "world")]);
        assert_eq!(expand_variables("hello $NAME", &v), "hello world");
        assert_eq!(expand_variables("hello ${NAME}", &v), "hello world");
    }

    #[test]
    fn test_escaped_dollar() {
        let v = vars(&[]);
        assert_eq!(expand_variables("$$HOME", &v), "$HOME");
    }

    #[test]
    fn test_recursive_expansion() {
        let v = vars(&[("A", "$B"), ("B", "resolved")]);
        assert_eq!(expand_variables("$A", &v), "resolved");
    }

    #[test]
    fn test_unresolved_passthrough() {
        let v = vars(&[]);
        assert_eq!(expand_variables("$UNKNOWN", &v), "$UNKNOWN");
    }

    #[test]
    fn test_nested_braces() {
        let v = vars(&[("BUILD_ROOT", "/builds"), ("OUT_PATH", "${BUILD_ROOT}/out")]);
        assert_eq!(expand_variables("${OUT_PATH}/pkg", &v), "/builds/out/pkg");
    }

    #[test]
    fn test_merge_precedence() {
        let global = vars(&[("KEY", "global"), ("ONLY_GLOBAL", "yes")]);
        let job = vars(&[("KEY", "job")]);
        let merged = merge_variables(&[&global, &job]);
        assert_eq!(merged.get("KEY").unwrap().value(), "job");
        assert_eq!(merged.get("ONLY_GLOBAL").unwrap().value(), "yes");
    }
}
