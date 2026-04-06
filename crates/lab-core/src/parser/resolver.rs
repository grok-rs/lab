use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use serde_yaml::Value;
use tracing::{debug, info};

use crate::error::{LabError, Result};

/// In-memory cache for fetched include content (URL/key → YAML string).
/// Avoids re-fetching the same remote/project/template file within a single parse.
static INCLUDE_CACHE: std::sync::LazyLock<Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Local project path mappings (GitLab project path → local filesystem path).
/// Set via `set_project_mappings()` before parsing.
static PROJECT_MAPPINGS: std::sync::LazyLock<Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Configure local project path mappings for `include:project` resolution.
/// When set, project includes will read from local filesystem instead of GitLab API.
pub fn set_project_mappings(mappings: HashMap<String, String>) {
    *PROJECT_MAPPINGS.lock().unwrap() = mappings;
}

fn cached_fetch(key: &str, fetch: impl FnOnce() -> Result<String>) -> Result<String> {
    {
        let cache = INCLUDE_CACHE.lock().unwrap();
        if let Some(content) = cache.get(key) {
            debug!(key = %key, "include cache hit");
            return Ok(content.clone());
        }
    }
    let content = fetch()?;
    INCLUDE_CACHE
        .lock()
        .unwrap()
        .insert(key.to_string(), content.clone());
    Ok(content)
}

/// Resolve `include:` directives in a GitLab CI YAML document.
/// Currently supports `include:local` only.
///
/// Ref: <https://docs.gitlab.com/ee/ci/yaml/includes.html>
///
/// Processing:
/// 1. Find `include:` key in the root mapping
/// 2. For each include entry, load the referenced file
/// 3. Deep-merge included content into the main document
/// 4. Remove the `include:` key from the result
pub fn resolve_includes(value: &mut Value, base_dir: &Path) -> Result<()> {
    let mapping = match value.as_mapping_mut() {
        Some(m) => m,
        None => return Ok(()),
    };

    let include_key = Value::String("include".into());
    let includes = match mapping.remove(&include_key) {
        Some(v) => v,
        None => return Ok(()), // No includes
    };

    let include_entries = parse_include_entries(&includes);

    // Check for include:rules — conditionally skip includes
    // Ref: <https://docs.gitlab.com/ci/yaml/#includerules>
    let include_entries = filter_includes_by_rules(include_entries, &includes);

    for entry in include_entries {
        match entry {
            IncludeEntry::Local(path) => {
                resolve_local_include(value, base_dir, &path)?;
            }
            IncludeEntry::Remote(url) => {
                resolve_remote_include(value, &url)?;
            }
            IncludeEntry::Template(name) => {
                resolve_template_include(value, &name)?;
            }
            IncludeEntry::Project {
                project,
                file,
                r#ref,
            } => {
                resolve_project_include(value, &project, &file, r#ref.as_deref())?;
            }
            IncludeEntry::Component { fqdn } => {
                resolve_component_include(value, &fqdn)?;
            }
        }
    }

    Ok(())
}

#[derive(Debug)]
enum IncludeEntry {
    Local(String),
    Remote(String),
    Template(String),
    Project {
        project: String,
        file: String,
        r#ref: Option<String>,
    },
    /// GitLab CI component: `include:component: gitlab.com/org/component@version`
    /// Ref: <https://docs.gitlab.com/ci/components/>
    Component {
        fqdn: String, // e.g. gitlab.com/org/component@1.0
    },
}

fn parse_include_entries(value: &Value) -> Vec<IncludeEntry> {
    match value {
        // Single string: include: 'path.yml'
        Value::String(s) => vec![classify_include(s)],
        // Sequence of strings or mappings
        Value::Sequence(seq) => seq.iter().flat_map(parse_single_include).collect(),
        // Single mapping
        Value::Mapping(_) => parse_single_include(value),
        _ => vec![],
    }
}

fn parse_single_include(value: &Value) -> Vec<IncludeEntry> {
    match value {
        Value::String(s) => vec![classify_include(s)],
        Value::Mapping(m) => {
            if let Some(Value::String(path)) = m.get(Value::String("local".into())) {
                vec![IncludeEntry::Local(path.clone())]
            } else if let Some(Value::String(url)) = m.get(Value::String("remote".into())) {
                vec![IncludeEntry::Remote(url.clone())]
            } else if let Some(Value::String(name)) = m.get(Value::String("template".into())) {
                vec![IncludeEntry::Template(name.clone())]
            } else if let Some(Value::String(component)) = m.get(Value::String("component".into()))
            {
                vec![IncludeEntry::Component {
                    fqdn: component.clone(),
                }]
            } else if let Some(Value::String(project)) = m.get(Value::String("project".into())) {
                let r#ref = m
                    .get(Value::String("ref".into()))
                    .and_then(|v| v.as_str())
                    .map(String::from);
                // file: can be a string or a list of strings
                // Ref: <https://docs.gitlab.com/ci/yaml/#includeproject>
                let files: Vec<String> = match m.get(Value::String("file".into())) {
                    Some(Value::String(s)) => vec![s.clone()],
                    Some(Value::Sequence(seq)) => seq
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect(),
                    _ => vec![],
                };
                files
                    .into_iter()
                    .map(|file| IncludeEntry::Project {
                        project: project.clone(),
                        file,
                        r#ref: r#ref.clone(),
                    })
                    .collect()
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}

fn classify_include(s: &str) -> IncludeEntry {
    if s.starts_with("http://") || s.starts_with("https://") {
        IncludeEntry::Remote(s.to_string())
    } else {
        IncludeEntry::Local(s.to_string())
    }
}

fn resolve_local_include(value: &mut Value, base_dir: &Path, path: &str) -> Result<()> {
    let file_path = if path.starts_with('/') {
        base_dir.join(path.trim_start_matches('/'))
    } else {
        base_dir.join(path)
    };

    debug!(path = %file_path.display(), "resolving local include");

    let content = std::fs::read_to_string(&file_path).map_err(|e| LabError::FileRead {
        path: file_path.clone(),
        source: e,
    })?;

    let included: Value = serde_yaml::from_str(&content)?;

    // Deep-merge included content into main document
    if let (Some(main_map), Some(included_map)) = (value.as_mapping_mut(), included.as_mapping()) {
        deep_merge_included(main_map, included_map);
    }

    Ok(())
}

/// Deep merge included YAML into the main document.
/// Included values are added first; main document values take precedence.
///
/// Ref: <https://docs.gitlab.com/ee/ci/yaml/includes.html>
fn deep_merge_included(main: &mut serde_yaml::Mapping, included: &serde_yaml::Mapping) {
    for (key, included_val) in included {
        match (main.get(key), included_val) {
            // Both are mappings → recursive merge
            (Some(Value::Mapping(existing)), Value::Mapping(incoming)) => {
                let mut merged = existing.clone();
                // For nested mappings, included values fill in gaps
                for (k, v) in incoming {
                    if !merged.contains_key(k) {
                        merged.insert(k.clone(), v.clone());
                    }
                }
                main.insert(key.clone(), Value::Mapping(merged));
            }
            // Main doesn't have this key → add from included
            (None, _) => {
                main.insert(key.clone(), included_val.clone());
            }
            // Main already has a value → keep main's value (main takes precedence)
            _ => {}
        }
    }
}

/// Filter include entries by `include:rules`.
/// Ref: <https://docs.gitlab.com/ci/yaml/#includerules>
fn filter_includes_by_rules(entries: Vec<IncludeEntry>, raw_includes: &Value) -> Vec<IncludeEntry> {
    // If the raw includes have rules, we need to evaluate them
    // For now, check if any include entries in the raw YAML have rules: blocks
    let raw_seq = match raw_includes {
        Value::Sequence(seq) => seq.clone(),
        Value::Mapping(_) => vec![raw_includes.clone()],
        _ => return entries,
    };

    let mut filtered = Vec::new();
    for (i, entry) in entries.into_iter().enumerate() {
        if i < raw_seq.len() {
            if let Value::Mapping(m) = &raw_seq[i] {
                let rules_key = Value::String("rules".into());
                if let Some(Value::Sequence(rules)) = m.get(&rules_key) {
                    // Has rules — evaluate if any rule matches
                    if !evaluate_include_rules(rules) {
                        debug!("include skipped by rules");
                        continue;
                    }
                }
            }
        }
        filtered.push(entry);
    }
    filtered
}

/// Simple evaluation of include:rules (supports rules:if with env vars).
fn evaluate_include_rules(rules: &[Value]) -> bool {
    for rule in rules {
        if let Value::Mapping(m) = rule {
            let if_key = Value::String("if".into());
            if let Some(Value::String(expr)) = m.get(&if_key) {
                // Evaluate against environment variables
                let vars = crate::model::variables::Variables::new();
                if crate::model::rules::evaluate_if_expression(expr, &vars) {
                    return true;
                }
            } else {
                // Rule without if — always matches
                return true;
            }
        }
    }
    false
}

/// Resolve `include:template` by fetching from GitLab's template repository.
/// Ref: <https://docs.gitlab.com/ci/yaml/#includetemplate>
///
/// Templates are sourced from:
/// https://gitlab.com/gitlab-org/gitlab/-/raw/master/lib/gitlab/ci/templates/<name>
fn resolve_template_include(value: &mut Value, template_name: &str) -> Result<()> {
    let url = format!(
        "https://gitlab.com/gitlab-org/gitlab/-/raw/master/lib/gitlab/ci/templates/{template_name}"
    );

    info!(template = %template_name, "fetching GitLab CI template");

    let yaml_content = cached_fetch(&format!("template:{template_name}"), || {
        let output = std::process::Command::new("curl")
            .args(["-sSfL", &url])
            .output()
            .map_err(|e| {
                LabError::Other(format!("failed to fetch template {template_name}: {e}"))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(LabError::Other(format!(
                "failed to fetch template {template_name}: {stderr}"
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    })?;

    let included: Value = serde_yaml::from_str(&yaml_content)?;

    if let (Some(main_map), Some(included_map)) = (value.as_mapping_mut(), included.as_mapping()) {
        deep_merge_included(main_map, included_map);
    }

    Ok(())
}

/// Fetch a single file from a GitLab project.
/// Checks local project mappings first, falls back to `glab api` (cached).
fn fetch_project_file(project: &str, file_path: &str, git_ref: &str) -> Result<String> {
    // Check local project mapping
    let local_path = {
        let mappings = PROJECT_MAPPINGS.lock().unwrap();
        mappings.get(project).map(|base| {
            let trimmed = file_path.trim_start_matches('/');
            std::path::PathBuf::from(base).join(trimmed)
        })
    };

    if let Some(path) = local_path {
        if path.exists() {
            info!(project = %project, file = %file_path, path = %path.display(), "using local project mapping");
            return std::fs::read_to_string(&path).map_err(|e| LabError::FileRead {
                path: path.clone(),
                source: e,
            });
        }
        debug!(project = %project, path = %path.display(), "local mapping file not found, falling back to API");
    }

    let cache_key = format!("project:{project}:{file_path}:{git_ref}");
    cached_fetch(&cache_key, || {
        let trimmed_path = file_path.trim_start_matches('/');
        let encoded_path = trimmed_path.replace('/', "%2F");

        let api_path = format!(
            "projects/{}/repository/files/{}/raw?ref={}",
            project.replace('/', "%2F"),
            encoded_path,
            git_ref,
        );

        let output = std::process::Command::new("glab")
            .args(["api", &api_path])
            .output()
            .map_err(|e| LabError::Other(format!("failed to run glab: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(LabError::Other(format!(
                "failed to fetch {file_path} from project {project}: {stderr}"
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    })
}

/// Resolve `include:project` by fetching the file via `glab api`.
/// Recursively resolves nested includes (local includes are fetched from the same project).
/// Ref: <https://docs.gitlab.com/ci/yaml/#includeproject>
fn resolve_project_include(
    value: &mut Value,
    project: &str,
    file_path: &str,
    git_ref: Option<&str>,
) -> Result<()> {
    let ref_param = git_ref.unwrap_or("HEAD");

    info!(project = %project, file = %file_path, ref_ = %ref_param, "fetching project include via glab");

    let content = fetch_project_file(project, file_path, ref_param)?;
    let mut included: Value = serde_yaml::from_str(&content)?;

    // Recursively resolve nested includes within the fetched YAML.
    // `include:local` inside a project-included file resolves relative to that project.
    resolve_project_nested_includes(&mut included, project, ref_param)?;

    if let (Some(main_map), Some(included_map)) = (value.as_mapping_mut(), included.as_mapping()) {
        deep_merge_included(main_map, included_map);
    }

    Ok(())
}

/// Resolve nested includes inside a project-fetched YAML.
/// Local includes are fetched from the same remote project.
fn resolve_project_nested_includes(value: &mut Value, project: &str, git_ref: &str) -> Result<()> {
    let mapping = match value.as_mapping_mut() {
        Some(m) => m,
        None => return Ok(()),
    };

    let include_key = Value::String("include".into());
    let includes = match mapping.remove(&include_key) {
        Some(v) => v,
        None => return Ok(()),
    };

    let entries = parse_include_entries(&includes);
    let entries = filter_includes_by_rules(entries, &includes);

    for entry in entries {
        match entry {
            IncludeEntry::Local(path) => {
                // Local includes inside project files resolve from the same project
                info!(project = %project, file = %path, "fetching nested local include from project");
                let content = fetch_project_file(project, &path, git_ref)?;
                let mut nested: Value = serde_yaml::from_str(&content)?;
                // Recurse for further nested includes
                resolve_project_nested_includes(&mut nested, project, git_ref)?;
                if let (Some(main_map), Some(nested_map)) =
                    (value.as_mapping_mut(), nested.as_mapping())
                {
                    deep_merge_included(main_map, nested_map);
                }
            }
            IncludeEntry::Remote(url) => {
                resolve_remote_include(value, &url)?;
            }
            IncludeEntry::Template(name) => {
                resolve_template_include(value, &name)?;
            }
            IncludeEntry::Project {
                project: nested_project,
                file,
                r#ref,
            } => {
                resolve_project_include(value, &nested_project, &file, r#ref.as_deref())?;
            }
            IncludeEntry::Component { fqdn } => {
                resolve_component_include(value, &fqdn)?;
            }
        }
    }

    Ok(())
}

/// Resolve `include:component` by fetching the component template.
/// Format: `gitlab.com/group/project/component-name@version`
/// Fetches: `https://gitlab.com/group/project/-/raw/version/component-name/template.yml`
/// Ref: <https://docs.gitlab.com/ci/components/>
fn resolve_component_include(value: &mut Value, fqdn: &str) -> Result<()> {
    info!(component = %fqdn, "fetching CI component");

    // Parse: hostname/group/project/component@version
    let (path, version) = fqdn.split_once('@').ok_or_else(|| {
        LabError::Other(format!(
            "invalid component reference '{fqdn}' — expected format: gitlab.com/group/project/name@version"
        ))
    })?;

    let parts: Vec<&str> = path.splitn(4, '/').collect();
    if parts.len() < 4 {
        return Err(LabError::Other(format!(
            "invalid component path '{path}' — expected: hostname/group/project/component-name"
        )));
    }

    let hostname = parts[0];
    let project_path = parts[1..parts.len() - 1].join("/");
    let component_name = parts[parts.len() - 1];

    // Try fetching as a project file via glab first (works for private repos)
    let file_path = format!("{component_name}/template.yml");
    let project = &project_path;

    let yaml_content = cached_fetch(&format!("component:{fqdn}"), || {
        // Try glab api first
        match fetch_project_file(project, &file_path, version) {
            Ok(content) => Ok(content),
            Err(_) => {
                // Fall back to raw HTTPS fetch (public repos)
                let url = format!(
                    "https://{hostname}/{project_path}/-/raw/{version}/{component_name}/template.yml"
                );
                let output = std::process::Command::new("curl")
                    .args(["-sSfL", &url])
                    .output()
                    .map_err(|e| LabError::Other(format!("failed to fetch component: {e}")))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(LabError::Other(format!(
                        "failed to fetch component {fqdn}: {stderr}"
                    )));
                }
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            }
        }
    })?;

    let included: Value = serde_yaml::from_str(&yaml_content)?;

    if let (Some(main_map), Some(included_map)) = (value.as_mapping_mut(), included.as_mapping()) {
        deep_merge_included(main_map, included_map);
    }

    Ok(())
}

/// Resolve a remote include by fetching it via HTTP.
/// Ref: <https://docs.gitlab.com/ee/ci/yaml/includes.html#includeremote>
fn resolve_remote_include(value: &mut Value, url: &str) -> Result<()> {
    info!(url = %url, "fetching remote include");

    let yaml_content = cached_fetch(&format!("remote:{url}"), || {
        let output = std::process::Command::new("curl")
            .args(["-sSfL", url])
            .output()
            .map_err(|e| LabError::Other(format!("failed to fetch {url}: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(LabError::Other(format!("failed to fetch {url}: {stderr}")));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    })?;

    let included: Value = serde_yaml::from_str(&yaml_content)?;

    if let (Some(main_map), Some(included_map)) = (value.as_mapping_mut(), included.as_mapping()) {
        deep_merge_included(main_map, included_map);
    }

    Ok(())
}
