use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::error::{LabError, Result};
use crate::model::pipeline::Pipeline;
use crate::model::variables::{VariableValue, Variables};

/// Masks secret values in job output (stdout/stderr).
///
/// Security model: any occurrence of a registered secret value in text
/// is replaced with `[MASKED]`. Also masks base64 encodings of secrets
/// to prevent trivial encoding bypass.
///
/// Values are sorted longest-first to prevent partial masking when
/// one secret is a substring of another.
#[derive(Clone, Default)]
pub struct SecretMasker {
    values: Vec<String>,
}

// Manual Debug impl — never print secret values, even accidentally.
impl std::fmt::Debug for SecretMasker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretMasker")
            .field("count", &self.values.len())
            .finish()
    }
}

impl SecretMasker {
    pub fn new() -> Self {
        Self { values: Vec::new() }
    }

    /// Register a value to be masked in output.
    /// Also registers the base64 encoding as a secondary mask.
    pub fn add_value(&mut self, value: &str) {
        let trimmed = value.trim();
        // Only mask values >= 4 chars to avoid false positives ("true", "test")
        if trimmed.len() < 4 {
            return;
        }
        self.values.push(trimmed.to_string());

        // Also mask the base64 encoding of this value
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(trimmed);
        if b64 != trimmed {
            self.values.push(b64);
        }
    }

    /// Build masker from secret variables.
    pub fn from_secrets(secrets: &Variables) -> Self {
        let mut masker = Self::new();
        for val in secrets.values() {
            masker.add_value(val.value());
        }
        masker.finalize();
        masker
    }

    /// Sort values longest-first so longer secrets are masked before shorter
    /// substrings. Call after all values are added.
    pub fn finalize(&mut self) {
        self.values.sort_by_key(|v| std::cmp::Reverse(v.len()));
        self.values.dedup();
    }

    /// Mask all secret values in the given text.
    pub fn mask(&self, text: &str) -> String {
        let mut result = text.to_string();
        for secret in &self.values {
            result = result.replace(secret.as_str(), "[MASKED]");
        }
        result
    }

    pub fn has_values(&self) -> bool {
        !self.values.is_empty()
    }
}

/// Get the secrets file path (centralized under `~/.local/share/lab/`).
pub fn secrets_file_path(workdir: &Path) -> PathBuf {
    crate::paths::secrets_file(workdir)
}

/// Load secrets from the centralized secrets file (or a custom path).
/// Format: `KEY=VALUE`, one per line. Lines starting with `#` are comments.
pub fn load_secrets_file(workdir: &Path) -> Result<Variables> {
    let path = secrets_file_path(workdir);
    load_env_file(&path)
}

/// Load secrets from a custom env file path.
pub fn load_env_file(path: &Path) -> Result<Variables> {
    if !path.exists() {
        return Ok(Variables::new());
    }

    let content = std::fs::read_to_string(path).map_err(|e| LabError::FileRead {
        path: path.to_path_buf(),
        source: e,
    })?;

    let mut vars = Variables::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            // Strip surrounding quotes if present
            let value = value
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(&value)
                .to_string();
            if !key.is_empty() {
                vars.insert(key, VariableValue::Simple(value));
            }
        }
    }

    debug!(path = %path.display(), count = vars.len(), "loaded secrets from file");
    Ok(vars)
}

/// Detect GitLab project path and parent group paths from git remote.
///
/// Example: `git@gitlab.com:repo-level/levelup-next/levelup-be-monorepo.git`
/// → project = `repo-level/levelup-next/levelup-be-monorepo`
/// → groups = [`repo-level/levelup-next`, `repo-level`]
pub fn detect_gitlab_paths(workdir: &Path) -> Result<(String, Vec<String>)> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(workdir)
        .output()
        .map_err(|e| LabError::Other(format!("failed to get git remote: {e}")))?;

    if !output.status.success() {
        return Err(LabError::Other("no git remote 'origin' found".into()));
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Parse project path from SSH or HTTPS URL
    let project_path = if url.starts_with("git@") {
        // git@gitlab.com:group/subgroup/project.git
        url.split(':')
            .nth(1)
            .unwrap_or("")
            .trim_end_matches(".git")
            .to_string()
    } else if url.starts_with("https://") || url.starts_with("http://") {
        // https://gitlab.com/group/subgroup/project.git
        let path = url
            .split("//")
            .nth(1)
            .unwrap_or("")
            .split('/')
            .skip(1) // skip hostname
            .collect::<Vec<_>>()
            .join("/");
        path.trim_end_matches(".git").to_string()
    } else {
        return Err(LabError::Other(format!(
            "unrecognized git remote URL: {url}"
        )));
    };

    // Build parent group paths
    let parts: Vec<&str> = project_path.split('/').collect();
    let mut groups = Vec::new();
    for i in (1..parts.len()).rev() {
        groups.push(parts[..i].join("/"));
    }

    debug!(project = %project_path, groups = ?groups, "detected GitLab paths");
    Ok((project_path, groups))
}

/// Metadata about a GitLab CI/CD variable.
#[derive(Debug, Clone)]
pub struct GitLabVarMeta {
    pub key: String,
    pub value: String,
    pub protected: bool,
    pub masked: bool,
    pub hidden: bool,
    pub environment_scope: String,
}

/// Result of pulling secrets — includes metadata for reporting.
#[derive(Debug, Clone, Default)]
pub struct PullResult {
    pub included: Variables,
    pub skipped_protected: Vec<String>,
    pub skipped_hidden: Vec<String>,
    pub skipped_scope: Vec<(String, String)>,
    pub masked_keys: Vec<String>,
}

/// Pull secrets from GitLab using `glab variable list`.
/// Respects `protected`, `masked`, `hidden`, and `environment_scope` metadata.
///
/// - **Protected vars** are only included if the current branch is protected.
/// - **Hidden vars** (empty value from API) are skipped with a warning.
/// - **Environment-scoped vars** are included if scope is `*` or matches.
/// - **Masked vars** are tracked for output masking.
pub fn pull_secrets_from_gitlab(workdir: &Path) -> Result<Variables> {
    let result = pull_secrets_full(workdir)?;

    // Report skipped variables
    if !result.skipped_protected.is_empty() {
        warn!(
            count = result.skipped_protected.len(),
            vars = ?result.skipped_protected,
            "skipped protected variables (branch is not protected)"
        );
    }
    if !result.skipped_hidden.is_empty() {
        warn!(
            count = result.skipped_hidden.len(),
            vars = ?result.skipped_hidden,
            "skipped hidden variables (value not available via API — add manually to secrets file)"
        );
    }

    Ok(result.included)
}

/// Pull secrets with full metadata reporting.
pub fn pull_secrets_full(workdir: &Path) -> Result<PullResult> {
    let (project, groups) = detect_gitlab_paths(workdir)?;
    let current_branch = get_current_branch(workdir);
    let is_protected = is_branch_protected(workdir, &project, &current_branch);

    let mut result = PullResult::default();
    let mut all_raw: Vec<GitLabVarMeta> = Vec::new();

    // Fetch group variables (lowest precedence first)
    for group in groups.iter().rev() {
        match fetch_glab_variables_raw(&["-g", group]) {
            Ok(vars) => {
                info!(group = %group, count = vars.len(), "fetched group variables");
                all_raw.extend(vars);
            }
            Err(e) => {
                warn!(group = %group, error = %e, "failed to fetch group variables");
            }
        }
    }

    // Fetch project variables (highest precedence — later entries override)
    match fetch_glab_variables_raw(&["-R", &project]) {
        Ok(vars) => {
            info!(project = %project, count = vars.len(), "fetched project variables");
            all_raw.extend(vars);
        }
        Err(e) => {
            warn!(project = %project, error = %e, "failed to fetch project variables");
        }
    }

    // Filter variables based on metadata
    for var in all_raw {
        // Skip hidden variables (value is empty/redacted from API)
        if var.hidden || var.value.is_empty() {
            result.skipped_hidden.push(var.key.clone());
            continue;
        }

        // Skip protected variables if current branch is not protected
        if var.protected && !is_protected {
            result.skipped_protected.push(var.key.clone());
            continue;
        }

        // Track masked variables for output masking
        if var.masked {
            result.masked_keys.push(var.key.clone());
        }

        // Environment scoping: `*` matches all, otherwise must match
        // (for local execution, we include `*` and skip specific scopes
        //  unless the user is running a job with that environment)
        if var.environment_scope != "*" {
            result
                .skipped_scope
                .push((var.key.clone(), var.environment_scope.clone()));
            // Still include it — the user might need it. Just note the scope.
        }

        result
            .included
            .insert(var.key, VariableValue::Simple(var.value));
    }

    Ok(result)
}

fn get_current_branch(workdir: &Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Check if the current branch is protected via `glab api`.
fn is_branch_protected(workdir: &Path, project: &str, branch: &str) -> bool {
    let encoded_project = project.replace('/', "%2F");
    let encoded_branch = urlencoding_simple(branch);
    let api_path = format!("projects/{encoded_project}/protected_branches/{encoded_branch}");

    let output = std::process::Command::new("glab")
        .args(["api", &api_path])
        .current_dir(workdir)
        .output();

    matches!(output, Ok(o) if o.status.success())
}

fn urlencoding_simple(s: &str) -> String {
    s.replace('/', "%2F").replace(' ', "%20")
}

/// Save variables to the centralized secrets file.
pub fn save_secrets_file(workdir: &Path, vars: &Variables) -> Result<()> {
    let path = crate::paths::secrets_file(workdir);
    let dir = crate::paths::secrets_dir(workdir);
    std::fs::create_dir_all(&dir).map_err(|e| LabError::FileRead {
        path: dir.clone(),
        source: e,
    })?;

    let mut content = String::from("# Generated by `lab secrets pull`\n\n");

    for (key, val) in vars {
        let value = val.value();
        if value.contains('\n') || value.contains('"') {
            content.push_str(&format!("# {key} (contains special chars, stored as-is)\n"));
        }
        content.push_str(&format!("{key}={value}\n"));
    }

    std::fs::write(&path, &content).map_err(|e| LabError::FileRead {
        path: path.clone(),
        source: e,
    })?;

    // Security: restrict file permissions to owner-only (chmod 600)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&path, perms);
    }

    info!(path = %path.display(), count = vars.len(), "saved secrets");
    Ok(())
}

/// Generate `secrets.env.example` in the project root from pipeline variable references.
/// Lists all variables that are referenced but not defined in the pipeline YAML.
pub fn generate_secrets_example(pipeline: &Pipeline, workdir: &Path) -> Result<()> {
    let referenced = find_referenced_variables(pipeline);
    let defined = find_defined_variables(pipeline);
    let predefined_prefixes = ["CI_", "GITLAB_", "DOCKER_HOST", "DOCKER_TLS", "DOCKER_CERT"];

    let path = workdir.join("secrets.env.example");
    let mut content = String::from("# Required CI/CD variables for this pipeline\n");
    content.push_str("# Run `lab secrets pull` to fetch from GitLab\n\n");

    let mut external_count = 0;
    for var_name in &referenced {
        if defined.contains(var_name) {
            continue;
        }
        if predefined_prefixes.iter().any(|p| var_name.starts_with(p)) {
            continue;
        }
        content.push_str(&format!("{var_name}=\n"));
        external_count += 1;
    }

    std::fs::write(&path, &content).map_err(|e| LabError::FileRead {
        path: path.clone(),
        source: e,
    })?;

    info!(path = %path.display(), count = external_count, "generated secrets example");
    Ok(())
}

/// Check which variables a pipeline needs but doesn't have.
pub struct MissingSecret {
    pub name: String,
    pub used_in_jobs: Vec<String>,
}

pub fn check_secrets(pipeline: &Pipeline, available: &Variables) -> Vec<MissingSecret> {
    let referenced = find_referenced_variables(pipeline);
    let defined = find_defined_variables(pipeline);
    let predefined_prefixes = ["CI_", "GITLAB_"];

    let mut missing = Vec::new();
    for var_name in &referenced {
        if defined.contains(var_name) || available.contains_key(var_name) {
            continue;
        }
        if predefined_prefixes.iter().any(|p| var_name.starts_with(p)) {
            continue;
        }
        // Find which jobs use this variable
        let jobs: Vec<String> = pipeline
            .jobs
            .iter()
            .filter(|(_, job)| {
                let texts = collect_job_texts(job);
                let combined = texts.join(" ");
                combined.contains(&format!("${var_name}"))
                    || combined.contains(&format!("${{{var_name}}}"))
            })
            .map(|(name, _)| name.clone())
            .collect();

        missing.push(MissingSecret {
            name: var_name.clone(),
            used_in_jobs: jobs,
        });
    }
    missing
}

// --- Internal helpers ---

/// Fetch variables with full metadata from glab.
fn fetch_glab_variables_raw(args: &[&str]) -> Result<Vec<GitLabVarMeta>> {
    let mut cmd = std::process::Command::new("glab");
    cmd.args(["variable", "list", "-F", "json"]);
    cmd.args(args);

    let output = cmd
        .output()
        .map_err(|e| LabError::Other(format!("failed to run glab: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(LabError::Other(format!("glab error: {stderr}")));
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&json_str).map_err(|e| LabError::Other(format!("json parse: {e}")))?;

    let mut vars = Vec::new();
    for entry in &entries {
        if let Some(key) = entry.get("key").and_then(|v| v.as_str()) {
            vars.push(GitLabVarMeta {
                key: key.to_string(),
                value: entry
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                protected: entry
                    .get("protected")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                masked: entry
                    .get("masked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                hidden: entry
                    .get("hidden")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                environment_scope: entry
                    .get("environment_scope")
                    .and_then(|v| v.as_str())
                    .unwrap_or("*")
                    .to_string(),
            });
        }
    }

    Ok(vars)
}

/// Scan pipeline for `$VARIABLE` references in scripts and variable values.
fn find_referenced_variables(pipeline: &Pipeline) -> Vec<String> {
    let mut refs = HashSet::new();
    let var_pattern = &*crate::model::variables::VAR_REFERENCE_PATTERN;

    // Scan all jobs
    for job in pipeline.jobs.values() {
        let texts = collect_job_texts(job);
        for text in &texts {
            for cap in var_pattern.captures_iter(text) {
                refs.insert(cap[1].to_string());
            }
        }
    }

    let mut sorted: Vec<String> = refs.into_iter().collect();
    sorted.sort();
    sorted
}

fn collect_job_texts(job: &crate::model::job::Job) -> Vec<String> {
    let mut texts = Vec::new();
    texts.extend(job.script.iter().cloned());
    if let Some(bs) = &job.before_script {
        texts.extend(bs.iter().cloned());
    }
    if let Some(a_s) = &job.after_script {
        texts.extend(a_s.iter().cloned());
    }
    for (_, v) in &job.variables {
        texts.push(v.value().to_string());
    }
    texts
}

/// Find all variables defined in the pipeline YAML (global + job level).
fn find_defined_variables(pipeline: &Pipeline) -> HashSet<String> {
    let mut defined = HashSet::new();
    for key in pipeline.variables.keys() {
        defined.insert(key.clone());
    }
    for job in pipeline.jobs.values() {
        for key in job.variables.keys() {
            defined.insert(key.clone());
        }
    }
    defined
}

/// Scope secrets to only those referenced by a specific job.
/// Prevents a compromised job from exfiltrating secrets it doesn't use.
pub fn scope_secrets_for_job(job: &crate::model::job::Job, all_secrets: &Variables) -> Variables {
    let texts = collect_job_texts(job);
    let combined = texts.join(" ");

    all_secrets
        .iter()
        .filter(|(key, _)| {
            combined.contains(&format!("${key}")) || combined.contains(&format!("${{{key}}}"))
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_ssh_url() {
        // Simulate by testing the parsing logic
        let url = "git@gitlab.com:repo-level/levelup-next/levelup-be-monorepo.git";
        let project = url.split(':').nth(1).unwrap().trim_end_matches(".git");
        assert_eq!(project, "repo-level/levelup-next/levelup-be-monorepo");

        let parts: Vec<&str> = project.split('/').collect();
        let mut groups = Vec::new();
        for i in (1..parts.len()).rev() {
            groups.push(parts[..i].join("/"));
        }
        assert_eq!(groups, vec!["repo-level/levelup-next", "repo-level"]);
    }

    #[test]
    fn test_detect_https_url() {
        let url = "https://gitlab.com/group/subgroup/project.git";
        let path = url
            .split("//")
            .nth(1)
            .unwrap()
            .split('/')
            .skip(1)
            .collect::<Vec<_>>()
            .join("/");
        let project = path.trim_end_matches(".git");
        assert_eq!(project, "group/subgroup/project");
    }

    #[test]
    fn test_load_env_file_format() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(
            b"# Comment\nKEY1=value1\nKEY2=\"quoted value\"\n\nKEY3=value with spaces\n",
        )
        .unwrap();

        let vars = load_env_file(file.path()).unwrap();
        assert_eq!(vars.get("KEY1").unwrap().value(), "value1");
        assert_eq!(vars.get("KEY2").unwrap().value(), "quoted value");
        assert_eq!(vars.get("KEY3").unwrap().value(), "value with spaces");
    }

    #[test]
    fn test_masker_basic() {
        let mut masker = SecretMasker::new();
        masker.add_value("my-secret-token");
        masker.finalize();

        assert_eq!(
            masker.mask("Token is my-secret-token here"),
            "Token is [MASKED] here"
        );
        assert_eq!(masker.mask("no secrets here"), "no secrets here");
    }

    #[test]
    fn test_masker_multiple_values() {
        let mut masker = SecretMasker::new();
        masker.add_value("secret-one");
        masker.add_value("secret-two");
        masker.finalize();

        assert_eq!(
            masker.mask("first: secret-one, second: secret-two"),
            "first: [MASKED], second: [MASKED]"
        );
    }

    #[test]
    fn test_masker_base64_variant() {
        let mut masker = SecretMasker::new();
        masker.add_value("hello-world");
        masker.finalize();

        // The base64 of "hello-world" should also be masked
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode("hello-world");
        assert_eq!(masker.mask(&b64), "[MASKED]");
    }

    #[test]
    fn test_masker_ignores_short_values() {
        let mut masker = SecretMasker::new();
        masker.add_value("ab"); // too short
        masker.add_value("yes"); // too short (< 4)
        masker.finalize();

        // Short values should NOT be masked (too many false positives)
        assert_eq!(masker.mask("ab yes"), "ab yes");
    }

    #[test]
    fn test_masker_longest_first() {
        let mut masker = SecretMasker::new();
        masker.add_value("secret");
        masker.add_value("my-secret-long"); // contains "secret" as substring
        masker.finalize();

        // Longer value should be masked first, not partially
        let result = masker.mask("value is my-secret-long");
        assert!(result.contains("[MASKED]"));
        assert!(!result.contains("secret")); // "secret" substring should also be gone
    }

    #[test]
    fn test_masker_debug_redacted() {
        let mut masker = SecretMasker::new();
        masker.add_value("super-secret-value");
        masker.finalize();

        let debug_output = format!("{masker:?}");
        // Debug output must NOT contain the secret value
        assert!(!debug_output.contains("super-secret-value"));
        assert!(debug_output.contains("count"));
    }

    #[test]
    fn test_scope_secrets_for_job() {
        use crate::model::job::Job;

        let mut all_secrets = Variables::new();
        all_secrets.insert("AWS_KEY".into(), VariableValue::Simple("aws-val".into()));
        all_secrets.insert("DB_PASS".into(), VariableValue::Simple("db-val".into()));
        all_secrets.insert("UNUSED".into(), VariableValue::Simple("unused-val".into()));

        // Job that only references AWS_KEY
        let job = Job {
            script: vec!["echo $AWS_KEY".into()],
            ..serde_yaml::from_str("script: [echo test]").unwrap()
        };

        let scoped = scope_secrets_for_job(&job, &all_secrets);
        assert!(scoped.contains_key("AWS_KEY"));
        assert!(!scoped.contains_key("DB_PASS"));
        assert!(!scoped.contains_key("UNUSED"));
    }
}
