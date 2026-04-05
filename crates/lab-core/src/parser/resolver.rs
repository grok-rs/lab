use std::path::Path;

use serde_yaml::Value;
use tracing::{debug, info};

use crate::error::{LabError, Result};

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
}

fn parse_include_entries(value: &Value) -> Vec<IncludeEntry> {
    match value {
        // Single string: include: 'path.yml'
        Value::String(s) => vec![classify_include(s)],
        // Sequence of strings or mappings
        Value::Sequence(seq) => seq.iter().flat_map(parse_single_include).collect(),
        // Single mapping
        Value::Mapping(_) => parse_single_include(value).into_iter().collect(),
        _ => vec![],
    }
}

fn parse_single_include(value: &Value) -> Option<IncludeEntry> {
    match value {
        Value::String(s) => Some(classify_include(s)),
        Value::Mapping(m) => {
            if let Some(Value::String(path)) = m.get(Value::String("local".into())) {
                Some(IncludeEntry::Local(path.clone()))
            } else if let Some(Value::String(url)) = m.get(Value::String("remote".into())) {
                Some(IncludeEntry::Remote(url.clone()))
            } else if let Some(Value::String(name)) = m.get(Value::String("template".into())) {
                Some(IncludeEntry::Template(name.clone()))
            } else if let Some(Value::String(project)) = m.get(Value::String("project".into())) {
                let file = m
                    .get(Value::String("file".into()))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let r#ref = m
                    .get(Value::String("ref".into()))
                    .and_then(|v| v.as_str())
                    .map(String::from);
                Some(IncludeEntry::Project {
                    project: project.clone(),
                    file,
                    r#ref,
                })
            } else {
                None
            }
        }
        _ => None,
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

    let content = std::process::Command::new("curl")
        .args(["-sSfL", &url])
        .output()
        .map_err(|e| LabError::Other(format!("failed to fetch template {template_name}: {e}")))?;

    if !content.status.success() {
        let stderr = String::from_utf8_lossy(&content.stderr);
        return Err(LabError::Other(format!(
            "failed to fetch template {template_name}: {stderr}"
        )));
    }

    let yaml_content = String::from_utf8_lossy(&content.stdout);
    let included: Value = serde_yaml::from_str(&yaml_content)?;

    if let (Some(main_map), Some(included_map)) = (value.as_mapping_mut(), included.as_mapping()) {
        deep_merge_included(main_map, included_map);
    }

    Ok(())
}

/// Resolve `include:project` by fetching the file via `glab api`.
/// Ref: <https://docs.gitlab.com/ci/yaml/#includeproject>
fn resolve_project_include(
    value: &mut Value,
    project: &str,
    file_path: &str,
    git_ref: Option<&str>,
) -> Result<()> {
    let ref_param = git_ref.unwrap_or("HEAD");
    let encoded_path = file_path.replace('/', "%2F");

    info!(project = %project, file = %file_path, ref_ = %ref_param, "fetching project include via glab");

    // Use glab api to fetch raw file content from another project
    let api_path = format!(
        "projects/{}/repository/files/{}/raw?ref={}",
        project.replace('/', "%2F"),
        encoded_path,
        ref_param,
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

    let content = String::from_utf8_lossy(&output.stdout);
    let included: Value = serde_yaml::from_str(&content)?;

    if let (Some(main_map), Some(included_map)) = (value.as_mapping_mut(), included.as_mapping()) {
        deep_merge_included(main_map, included_map);
    }

    Ok(())
}

/// Resolve a remote include by fetching it via HTTP.
/// Ref: <https://docs.gitlab.com/ee/ci/yaml/includes.html#includeremote>
fn resolve_remote_include(value: &mut Value, url: &str) -> Result<()> {
    info!(url = %url, "fetching remote include");

    // Use a blocking HTTP request since the parser is synchronous
    let content = std::process::Command::new("curl")
        .args(["-sSfL", url])
        .output()
        .map_err(|e| LabError::Other(format!("failed to fetch {url}: {e}")))?;

    if !content.status.success() {
        let stderr = String::from_utf8_lossy(&content.stderr);
        return Err(LabError::Other(format!("failed to fetch {url}: {stderr}")));
    }

    let yaml_content = String::from_utf8_lossy(&content.stdout);
    let included: Value = serde_yaml::from_str(&yaml_content)?;

    if let (Some(main_map), Some(included_map)) = (value.as_mapping_mut(), included.as_mapping()) {
        deep_merge_included(main_map, included_map);
    }

    Ok(())
}
