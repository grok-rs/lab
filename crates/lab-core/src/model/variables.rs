use indexmap::IndexMap;
use serde::Deserialize;

use crate::config::Config;
use crate::error::Result;

/// CI/CD variables map — ordered to preserve declaration order.
/// Ref: <https://docs.gitlab.com/ci/variables/>
pub type Variables = IndexMap<String, VariableValue>;

/// A CI/CD variable value with optional metadata.
/// Ref: <https://docs.gitlab.com/ci/yaml/#variables>
#[derive(Debug, Clone)]
pub enum VariableValue {
    /// Simple string value.
    Simple(String),
    /// Detailed variable with description and options.
    Detailed {
        value: String,
        description: Option<String>,
        expand: bool,
        options: Option<Vec<String>>,
    },
}

impl<'de> Deserialize<'de> for VariableValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match &value {
            serde_yaml::Value::String(s) => Ok(VariableValue::Simple(s.clone())),
            serde_yaml::Value::Number(n) => Ok(VariableValue::Simple(n.to_string())),
            serde_yaml::Value::Bool(b) => Ok(VariableValue::Simple(b.to_string())),
            serde_yaml::Value::Null => Ok(VariableValue::Simple(String::new())),
            serde_yaml::Value::Mapping(m) => {
                let val = m
                    .get(serde_yaml::Value::String("value".into()))
                    .and_then(|v| match v {
                        serde_yaml::Value::String(s) => Some(s.clone()),
                        serde_yaml::Value::Number(n) => Some(n.to_string()),
                        serde_yaml::Value::Bool(b) => Some(b.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let description = m
                    .get(serde_yaml::Value::String("description".into()))
                    .and_then(|v| v.as_str().map(String::from));
                let expand = m
                    .get(serde_yaml::Value::String("expand".into()))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let options = m
                    .get(serde_yaml::Value::String("options".into()))
                    .and_then(|v| v.as_sequence())
                    .map(|seq| {
                        seq.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    });
                Ok(VariableValue::Detailed {
                    value: val,
                    description,
                    expand,
                    options,
                })
            }
            _ => Err(serde::de::Error::custom("invalid variable value")),
        }
    }
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
        // Don't expand bash parameter expansion like ${VAR:-default} or ${VAR:+alt}
        // — pass those through to the shell.
        if chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let var_name: String = chars.by_ref().take_while(|c| *c != '}').collect();
            // If content contains bash parameter expansion operators, pass through to shell.
            // Operators: :- :+ :? (default/alt/error), %% % ## # (trim), / (replace)
            if var_name.contains(':')
                || var_name.contains(":-")
                || var_name.contains('+')
                || var_name.contains('%')
                || var_name.contains('#')
                || var_name.contains('/')
            {
                result.push_str("${");
                result.push_str(&var_name);
                result.push('}');
            } else if let Some(val) = lookup_var(&var_name, vars) {
                let expanded = expand_recursive(val, vars, depth + 1);
                result.push_str(&expanded);
            } else {
                // Leave unresolved ${VAR} as-is for the shell to handle
                result.push_str("${");
                result.push_str(&var_name);
                result.push('}');
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
    // GitLab Runner expands variable references in values before injecting them
    // as environment variables. We must do the same — bash does NOT recursively
    // expand env var values, so `ECR_REGISTRY=${AWS_ACCOUNT_ID_ECR}.dkr.ecr...`
    // must be expanded to the literal value before passing to the container.
    vars.iter()
        .map(|(k, v)| (k.clone(), expand_variables(v.value(), vars)))
        .collect()
}

/// Build predefined CI/CD variables for local execution.
/// Implements all simulatable variables from the official GitLab spec:
/// <https://docs.gitlab.com/ci/variables/predefined_variables/>
pub fn predefined_variables(config: &Config, job_name: &str, stage: &str) -> Result<Variables> {
    let workdir = config
        .workdir
        .canonicalize()
        .map_err(|e| crate::error::LabError::FileRead {
            path: config.workdir.clone(),
            source: e,
        })?;

    let mut vars = Variables::new();

    // --- Helper: run git command and return trimmed stdout ---
    let git = |args: &[&str]| -> String {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&workdir)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };

    // --- Git info ---
    let commit_sha = git(&["rev-parse", "HEAD"]);
    let commit_branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let commit_message = git(&["log", "-1", "--format=%B"]);
    let commit_title = git(&["log", "-1", "--format=%s"]);
    let commit_author = git(&["log", "-1", "--format=%aN <%aE>"]);
    let commit_timestamp = git(&["log", "-1", "--format=%aI"]);
    let commit_before = git(&["rev-parse", "HEAD~1"]);
    let remote_url = git(&["remote", "get-url", "origin"]);

    let default_branch = git(&["symbolic-ref", "refs/remotes/origin/HEAD", "--short"])
        .trim_start_matches("origin/")
        .to_string();
    let default_branch = if default_branch.is_empty() {
        "main".to_string()
    } else {
        default_branch
    };

    // --- Parse server info from remote URL ---
    let (server_host, server_protocol, project_path) = parse_remote_url(&remote_url);
    let server_port = if server_protocol == "https" {
        "443"
    } else {
        "80"
    };
    let server_url = format!("{server_protocol}://{server_host}");

    let project_name = workdir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let project_namespace = project_path
        .rsplit_once('/')
        .map(|(ns, _)| ns)
        .unwrap_or("local");
    let project_root_namespace = project_path.split('/').next().unwrap_or("local");

    // Commit description: message without first line
    let commit_description = commit_message
        .split_once('\n')
        .map(|x| x.1)
        .unwrap_or("")
        .trim()
        .to_string();

    // --- Pipeline source detection ---
    let pipeline_source = if commit_branch == default_branch {
        "push"
    } else {
        "merge_request_event"
    };

    // ================================================================
    // Core
    // ================================================================
    set(&mut vars, "CI", "true");
    set(&mut vars, "GITLAB_CI", "true");
    set(&mut vars, "CI_LOCAL", "true");
    set(&mut vars, "CI_SERVER", "yes");

    // ================================================================
    // Server
    // ================================================================
    set(&mut vars, "CI_SERVER_URL", &server_url);
    set(&mut vars, "CI_SERVER_HOST", &server_host);
    set(&mut vars, "CI_SERVER_PORT", server_port);
    set(&mut vars, "CI_SERVER_PROTOCOL", &server_protocol);
    set(&mut vars, "CI_SERVER_FQDN", &server_host);
    set(&mut vars, "CI_API_V4_URL", &format!("{server_url}/api/v4"));
    set(
        &mut vars,
        "CI_API_GRAPHQL_URL",
        &format!("{server_url}/api/graphql"),
    );

    // ================================================================
    // Project
    // ================================================================
    set(&mut vars, "CI_PROJECT_NAME", &project_name);
    set(&mut vars, "CI_PROJECT_PATH", &project_path);
    set(&mut vars, "CI_PROJECT_PATH_SLUG", &slugify(&project_path));
    set(&mut vars, "CI_PROJECT_NAMESPACE", project_namespace);
    set(
        &mut vars,
        "CI_PROJECT_NAMESPACE_SLUG",
        &slugify(project_namespace),
    );
    set(
        &mut vars,
        "CI_PROJECT_ROOT_NAMESPACE",
        project_root_namespace,
    );
    set(
        &mut vars,
        "CI_PROJECT_URL",
        &format!("{server_url}/{project_path}"),
    );
    set(&mut vars, "CI_PROJECT_DIR", "/workspace");
    set(&mut vars, "CI_PROJECT_ID", "0");

    // ================================================================
    // Commit
    // ================================================================
    set(&mut vars, "CI_COMMIT_SHA", &commit_sha);
    set(
        &mut vars,
        "CI_COMMIT_SHORT_SHA",
        &commit_sha.chars().take(8).collect::<String>(),
    );
    set(&mut vars, "CI_COMMIT_BRANCH", &commit_branch);
    set(&mut vars, "CI_COMMIT_REF_NAME", &commit_branch);
    set(&mut vars, "CI_COMMIT_REF_SLUG", &slugify(&commit_branch));
    set(&mut vars, "CI_COMMIT_MESSAGE", &commit_message);
    set(&mut vars, "CI_COMMIT_TITLE", &commit_title);
    set(&mut vars, "CI_COMMIT_DESCRIPTION", &commit_description);
    set(&mut vars, "CI_COMMIT_AUTHOR", &commit_author);
    set(&mut vars, "CI_COMMIT_TIMESTAMP", &commit_timestamp);
    set(
        &mut vars,
        "CI_COMMIT_BEFORE_SHA",
        if commit_before.is_empty() {
            "0000000000000000000000000000000000000000"
        } else {
            &commit_before
        },
    );

    // Tag info (if on a tag)
    let tag = git(&["describe", "--exact-match", "--tags", "HEAD"]);
    if !tag.is_empty() {
        set(&mut vars, "CI_COMMIT_TAG", &tag);
        let tag_message = git(&["tag", "-l", &tag, "-n100", "--format=%(contents)"]);
        if !tag_message.is_empty() {
            set(&mut vars, "CI_COMMIT_TAG_MESSAGE", &tag_message);
        }
    }

    // ================================================================
    // Default branch
    // ================================================================
    set(&mut vars, "CI_DEFAULT_BRANCH", &default_branch);
    set(
        &mut vars,
        "CI_DEFAULT_BRANCH_SLUG",
        &slugify(&default_branch),
    );

    // ================================================================
    // Pipeline
    // ================================================================
    set(&mut vars, "CI_PIPELINE_ID", "0");
    set(&mut vars, "CI_PIPELINE_IID", "0");
    set(&mut vars, "CI_PIPELINE_SOURCE", pipeline_source);
    set(
        &mut vars,
        "CI_PIPELINE_URL",
        &format!("{server_url}/{project_path}/-/pipelines/0"),
    );
    set(&mut vars, "CI_PIPELINE_CREATED_AT", &commit_timestamp);

    // ================================================================
    // Job
    // ================================================================
    set(&mut vars, "CI_JOB_NAME", job_name);
    set(&mut vars, "CI_JOB_NAME_SLUG", &slugify(job_name));
    set(&mut vars, "CI_JOB_STAGE", stage);
    set(&mut vars, "CI_JOB_ID", "0");
    set(
        &mut vars,
        "CI_JOB_URL",
        &format!("{server_url}/{project_path}/-/jobs/0"),
    );

    // ================================================================
    // Runner
    // ================================================================
    set(&mut vars, "CI_RUNNER_ID", "0");
    set(&mut vars, "CI_RUNNER_DESCRIPTION", "lab-local");
    set(&mut vars, "CI_RUNNER_TAGS", "[]");

    // ================================================================
    // Build paths
    // ================================================================
    set(&mut vars, "CI_BUILDS_DIR", "/workspace");
    set(&mut vars, "CI_CONFIG_PATH", ".gitlab-ci.yml");
    set(
        &mut vars,
        "CI_REPOSITORY_URL",
        &format!("{server_url}/{project_path}.git"),
    );
    set(
        &mut vars,
        "CI_TEMPLATE_REGISTRY_HOST",
        "registry.gitlab.com",
    );
    set(&mut vars, "CI_NODE_TOTAL", "1");

    // ================================================================
    // MR-specific (feature branch)
    // ================================================================
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
        set(&mut vars, "CI_MERGE_REQUEST_IID", "0");
        set(&mut vars, "CI_MERGE_REQUEST_PROJECT_PATH", &project_path);
        set(
            &mut vars,
            "CI_MERGE_REQUEST_PROJECT_URL",
            &format!("{server_url}/{project_path}"),
        );
        set(
            &mut vars,
            "CI_MERGE_REQUEST_SOURCE_PROJECT_PATH",
            &project_path,
        );

        let diff_base = git(&["merge-base", "HEAD", &format!("origin/{default_branch}")]);
        if !diff_base.is_empty() {
            set(&mut vars, "CI_MERGE_REQUEST_DIFF_BASE_SHA", &diff_base);
        }
    }

    Ok(vars)
}

/// Convert a string to a URL-safe slug (GitLab style).
/// Lowercase, non-alphanumeric → `-`, no leading/trailing `-`, max 63 chars.
fn slugify(s: &str) -> String {
    let slug: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    // Collapse consecutive dashes
    let mut result = String::new();
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash {
                result.push(c);
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }
    // Max 63 chars
    result.chars().take(63).collect()
}

/// Parse GitLab server info from git remote URL.
fn parse_remote_url(url: &str) -> (String, String, String) {
    if url.starts_with("git@") {
        // git@gitlab.com:group/project.git
        let host = url
            .trim_start_matches("git@")
            .split(':')
            .next()
            .unwrap_or("gitlab.com");
        let path = url.split(':').nth(1).unwrap_or("").trim_end_matches(".git");
        (host.to_string(), "https".to_string(), path.to_string())
    } else if url.starts_with("https://") || url.starts_with("http://") {
        let protocol = if url.starts_with("https") {
            "https"
        } else {
            "http"
        };
        let without_proto = url.split("//").nth(1).unwrap_or("");
        let host = without_proto.split('/').next().unwrap_or("gitlab.com");
        let path = without_proto
            .split_once('/')
            .map(|x| x.1)
            .unwrap_or("")
            .trim_end_matches(".git");
        (host.to_string(), protocol.to_string(), path.to_string())
    } else {
        (
            "gitlab.com".to_string(),
            "https".to_string(),
            "local/project".to_string(),
        )
    }
}

fn set(vars: &mut Variables, key: &str, value: &str) {
    vars.insert(key.to_string(), VariableValue::Simple(value.to_string()));
}

// read_git_info removed — replaced by inline git() closure in predefined_variables()

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
        // $VAR syntax — unresolved stays as $VAR
        assert_eq!(expand_variables("$UNKNOWN", &v), "$UNKNOWN");
        // ${VAR} syntax — unresolved stays as ${VAR}
        assert_eq!(expand_variables("${UNKNOWN}", &v), "${UNKNOWN}");
    }

    #[test]
    fn test_shell_local_vars_passthrough() {
        // Shell-local variables (set within the script) must pass through
        // because lab doesn't know about them — only the shell does.
        let v = vars(&[("CI_COMMIT_SHORT_SHA", "abc123")]);
        // IMAGE_TAG is set in the script, not in lab's variable map
        assert_eq!(
            expand_variables("develop_${CI_COMMIT_SHORT_SHA}", &v),
            "develop_abc123"
        );
        assert_eq!(expand_variables("${IMAGE_TAG}", &v), "${IMAGE_TAG}");
        assert_eq!(
            expand_variables(".gitlab/scripts/docker-build.sh {} ${IMAGE_TAG}", &v),
            ".gitlab/scripts/docker-build.sh {} ${IMAGE_TAG}"
        );
        // Mixed: known var + unknown shell var in same string
        assert_eq!(
            expand_variables("tag: ${IMAGE_TAG} sha: ${CI_COMMIT_SHORT_SHA}", &v),
            "tag: ${IMAGE_TAG} sha: abc123"
        );
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

    #[test]
    fn test_bash_parameter_expansion_passthrough() {
        // ${VAR:-default} should NOT be expanded by lab — pass to shell
        let v = vars(&[("CI_COMMIT_BEFORE_SHA", "abc123")]);
        assert_eq!(
            expand_variables(
                "${CI_MERGE_REQUEST_DIFF_BASE_SHA:-$CI_COMMIT_BEFORE_SHA}",
                &v
            ),
            "${CI_MERGE_REQUEST_DIFF_BASE_SHA:-$CI_COMMIT_BEFORE_SHA}"
        );

        // ${VAR:+alt} should also pass through
        assert_eq!(expand_variables("${MY_VAR:+yes}", &v), "${MY_VAR:+yes}");

        // Plain ${VAR} should still expand
        assert_eq!(expand_variables("${CI_COMMIT_BEFORE_SHA}", &v), "abc123");
    }

    #[test]
    fn test_slugify() {
        assert_eq!(super::slugify("main"), "main");
        assert_eq!(super::slugify("feature/login-page"), "feature-login-page");
        assert_eq!(super::slugify("UPPER_CASE"), "upper-case");
        assert_eq!(super::slugify("my-group/my-project"), "my-group-my-project");
        // No leading/trailing dashes
        assert_eq!(super::slugify("/path/"), "path");
        // Consecutive dashes collapsed
        assert_eq!(super::slugify("a--b"), "a-b");
    }

    #[test]
    fn test_parse_remote_url_ssh() {
        let (host, proto, path) =
            super::parse_remote_url("git@gitlab.com:group/subgroup/project.git");
        assert_eq!(host, "gitlab.com");
        assert_eq!(proto, "https");
        assert_eq!(path, "group/subgroup/project");
    }

    #[test]
    fn test_parse_remote_url_https() {
        let (host, proto, path) =
            super::parse_remote_url("https://gitlab.example.com/my/project.git");
        assert_eq!(host, "gitlab.example.com");
        assert_eq!(proto, "https");
        assert_eq!(path, "my/project");
    }

    #[test]
    fn test_predefined_variables_complete() {
        // Test that predefined_variables returns all expected keys
        let config = crate::config::Config::default();
        let result = super::predefined_variables(&config, "test-job", "test");
        // This will fail in non-git directories, which is OK for CI
        if let Ok(vars) = result {
            // Core
            assert_eq!(vars.get("CI").unwrap().value(), "true");
            assert_eq!(vars.get("GITLAB_CI").unwrap().value(), "true");
            assert_eq!(vars.get("CI_LOCAL").unwrap().value(), "true");
            assert_eq!(vars.get("CI_SERVER").unwrap().value(), "yes");
            // Job
            assert_eq!(vars.get("CI_JOB_NAME").unwrap().value(), "test-job");
            assert_eq!(vars.get("CI_JOB_STAGE").unwrap().value(), "test");
            assert!(vars.contains_key("CI_JOB_NAME_SLUG"));
            // Server
            assert!(vars.contains_key("CI_SERVER_URL"));
            assert!(vars.contains_key("CI_SERVER_HOST"));
            assert!(vars.contains_key("CI_API_V4_URL"));
            // Commit
            assert!(vars.contains_key("CI_COMMIT_SHA"));
            assert!(vars.contains_key("CI_COMMIT_SHORT_SHA"));
            assert!(vars.contains_key("CI_COMMIT_REF_SLUG"));
            assert!(vars.contains_key("CI_COMMIT_TITLE"));
            assert!(vars.contains_key("CI_COMMIT_AUTHOR"));
            assert!(vars.contains_key("CI_COMMIT_TIMESTAMP"));
            assert!(vars.contains_key("CI_COMMIT_BEFORE_SHA"));
            // Project
            assert!(vars.contains_key("CI_PROJECT_PATH"));
            assert!(vars.contains_key("CI_PROJECT_PATH_SLUG"));
            assert!(vars.contains_key("CI_PROJECT_URL"));
            assert!(vars.contains_key("CI_PROJECT_ROOT_NAMESPACE"));
            // Pipeline
            assert!(vars.contains_key("CI_PIPELINE_SOURCE"));
            assert!(vars.contains_key("CI_PIPELINE_URL"));
            // Default branch
            assert!(vars.contains_key("CI_DEFAULT_BRANCH"));
            assert!(vars.contains_key("CI_DEFAULT_BRANCH_SLUG"));
            // Build paths
            assert_eq!(vars.get("CI_PROJECT_DIR").unwrap().value(), "/workspace");
            assert_eq!(vars.get("CI_BUILDS_DIR").unwrap().value(), "/workspace");
            assert_eq!(
                vars.get("CI_CONFIG_PATH").unwrap().value(),
                ".gitlab-ci.yml"
            );
            // Count: should have 40+ variables
            assert!(
                vars.len() >= 40,
                "Expected 40+ predefined vars, got {}",
                vars.len()
            );
        }
    }

    #[test]
    fn test_to_env_map_expands_variable_references() {
        // GitLab Runner expands refs in variable values before injecting as env.
        // ECR_REGISTRY: "${AWS_ACCOUNT_ID_ECR}.dkr.ecr.${AWS_REGION_ECR}.amazonaws.com"
        let v = vars(&[
            ("AWS_ACCOUNT_ID_ECR", "341304826755"),
            ("AWS_REGION_ECR", "eu-central-1"),
            (
                "ECR_REGISTRY",
                "${AWS_ACCOUNT_ID_ECR}.dkr.ecr.${AWS_REGION_ECR}.amazonaws.com",
            ),
        ]);
        let env = to_env_map(&v);
        assert_eq!(
            env.get("ECR_REGISTRY").unwrap(),
            "341304826755.dkr.ecr.eu-central-1.amazonaws.com"
        );
        // Plain values unchanged
        assert_eq!(env.get("AWS_ACCOUNT_ID_ECR").unwrap(), "341304826755");
    }

    #[test]
    fn test_to_env_map_preserves_bash_expansion() {
        // ${VAR:-default} in variable values should still pass through
        let v = vars(&[(
            "NX_BASE",
            "${CI_MERGE_REQUEST_DIFF_BASE_SHA:-$CI_COMMIT_BEFORE_SHA}",
        )]);
        let env = to_env_map(&v);
        assert_eq!(
            env.get("NX_BASE").unwrap(),
            "${CI_MERGE_REQUEST_DIFF_BASE_SHA:-$CI_COMMIT_BEFORE_SHA}"
        );
    }
}
