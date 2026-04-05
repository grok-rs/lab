use std::path::Path;

use serde_yaml::Value;

use crate::error::{LabError, Result};

/// Load a GitLab CI YAML file and resolve YAML-level features.
///
/// Processing order:
/// 1. Load raw YAML from file
/// 2. Resolve `<<:` merge keys (YAML 1.1 feature used by GitLab)
/// 3. Resolve `extends:` keyword (GitLab-specific deep merge)
///
/// Ref: <https://docs.gitlab.com/ci/yaml/yaml_optimization/>
pub fn load_and_resolve(path: &Path) -> Result<Value> {
    let content = std::fs::read_to_string(path).map_err(|e| LabError::FileRead {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Pre-process !reference tags before YAML parsing.
    // serde_yaml turns `!reference [.job, script]` into a tagged value
    // that's hard to work with. Instead, we parse the YAML, then resolve
    // !reference tags by walking the Value tree.
    let mut value: Value = serde_yaml::from_str(&content)?;

    // Resolve merge keys (<<:) throughout the document
    resolve_merge_keys(&mut value);

    // Resolve include: directives (local files)
    // Ref: <https://docs.gitlab.com/ee/ci/yaml/includes.html>
    let base_dir = path.parent().unwrap_or(Path::new("."));
    super::resolver::resolve_includes(&mut value, base_dir)?;

    // Resolve !reference tags
    // Ref: <https://docs.gitlab.com/ci/yaml/yaml_optimization/#reference-tags>
    resolve_reference_tags(&mut value);

    // Resolve extends: keyword for job inheritance
    if let Value::Mapping(ref mut mapping) = value {
        resolve_extends(mapping);
    }

    Ok(value)
}

/// Resolve `!reference` tags throughout the document.
/// Ref: <https://docs.gitlab.com/ci/yaml/yaml_optimization/#reference-tags>
///
/// `!reference [.job, key]` is replaced with the value at `.job.key` in the root document.
/// `!reference [.job, key, subkey]` navigates deeper.
fn resolve_reference_tags(root: &mut Value) {
    // We need a snapshot of the root for lookups while mutating
    let snapshot = root.clone();
    resolve_references_recursive(root, &snapshot);
}

fn resolve_references_recursive(value: &mut Value, root: &Value) {
    match value {
        Value::Tagged(tagged) => {
            if tagged.tag == "!reference" {
                if let Value::Sequence(path) = &tagged.value {
                    let keys: Vec<&str> = path.iter().filter_map(|v| v.as_str()).collect();
                    if let Some(resolved) = lookup_path(root, &keys) {
                        *value = resolved.clone();
                    }
                }
                // If we can't resolve, leave as-is (will likely error during deser)
            }
        }
        Value::Mapping(mapping) => {
            for (_, v) in mapping.iter_mut() {
                resolve_references_recursive(v, root);
            }
        }
        Value::Sequence(seq) => {
            // Handle !reference in sequences — the resolved value might be a sequence
            // that should be flattened into the parent
            let mut new_seq = Vec::new();
            for item in seq.iter_mut() {
                resolve_references_recursive(item, root);
                // If the resolved reference is itself a sequence, flatten it
                if let Value::Sequence(inner) = item {
                    new_seq.extend(inner.iter().cloned());
                } else {
                    new_seq.push(item.clone());
                }
            }
            *seq = new_seq;
        }
        _ => {}
    }
}

/// Navigate a YAML Value tree by a sequence of keys.
fn lookup_path<'a>(root: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let mut current = root;
    for key in keys {
        current = match current {
            Value::Mapping(m) => m.get(Value::String(key.to_string()))?,
            _ => return None,
        };
    }
    Some(current)
}

/// Resolve YAML merge keys (`<<:`) by flattening them into the parent mapping.
/// serde_yaml represents `<<:` as a literal "<<" key instead of merging,
/// so we handle this manually.
///
/// Ref: <https://docs.gitlab.com/ci/yaml/yaml_optimization/#anchors>
fn resolve_merge_keys(value: &mut Value) {
    match value {
        Value::Mapping(mapping) => {
            // First, recursively process all values
            for (_, v) in mapping.iter_mut() {
                resolve_merge_keys(v);
            }

            // Then handle << merge key
            let merge_key = Value::String("<<".to_string());
            if let Some(merge_value) = mapping.remove(&merge_key) {
                let sources: Vec<&Value> = match &merge_value {
                    Value::Mapping(_) => vec![&merge_value],
                    Value::Sequence(seq) => seq.iter().collect(),
                    _ => vec![],
                };

                // Merge sources in order — existing keys take precedence
                for source in sources {
                    if let Value::Mapping(source_map) = source {
                        for (k, v) in source_map {
                            if !mapping.contains_key(k) {
                                mapping.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
            }
        }
        Value::Sequence(seq) => {
            for item in seq.iter_mut() {
                resolve_merge_keys(item);
            }
        }
        _ => {}
    }
}

/// Resolve `extends:` keyword by deep-merging inherited job configurations.
/// Ref: <https://docs.gitlab.com/ci/yaml/#extends>
///
/// Process: for each job with `extends:`, find the referenced jobs and
/// deep-merge their configuration. The extending job's values take precedence.
fn resolve_extends(mapping: &mut serde_yaml::Mapping) {
    // Collect jobs that have extends
    let job_keys: Vec<Value> = mapping.keys().cloned().collect();
    let extends_key = Value::String("extends".to_string());

    // We need multiple passes since extends can chain (A extends B extends C)
    for _ in 0..10 {
        let mut changed = false;

        for key in &job_keys {
            let job = match mapping.get(key) {
                Some(Value::Mapping(m)) => m.clone(),
                _ => continue,
            };

            let extends = match job.get(&extends_key) {
                Some(v) => extract_extends_list(v),
                None => continue,
            };

            let mut merged = serde_yaml::Mapping::new();

            // Merge base jobs first (in order)
            for base_name in &extends {
                let base_key = Value::String(base_name.clone());
                if let Some(Value::Mapping(base)) = mapping.get(&base_key) {
                    deep_merge_mapping(&mut merged, base);
                }
            }

            // Then overlay the extending job's own values (takes precedence)
            deep_merge_mapping(&mut merged, &job);

            // Remove the extends key from the resolved job
            merged.remove(&extends_key);

            if mapping.get(key) != Some(&Value::Mapping(merged.clone())) {
                mapping.insert(key.clone(), Value::Mapping(merged));
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }
}

fn extract_extends_list(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Sequence(seq) => seq
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => vec![],
    }
}

/// Deep merge `source` into `target`. Source values are added only if
/// the key doesn't already exist in target (for mappings). Arrays are
/// completely replaced, not merged — matching GitLab's behavior.
///
/// Ref: <https://docs.gitlab.com/ee/ci/yaml/includes.html>
/// Deep merge `source` into `target`.
/// - Both mappings → recursive merge
/// - Otherwise → source value overwrites target
///
/// This means the LAST call to deep_merge_mapping wins for scalar/array values,
/// which is correct for GitLab's extends: the extending job overlays the base.
fn deep_merge_mapping(target: &mut serde_yaml::Mapping, source: &serde_yaml::Mapping) {
    for (key, source_val) in source {
        match (target.get(key), source_val) {
            // Both are mappings → recursive merge
            (Some(Value::Mapping(existing)), Value::Mapping(incoming)) => {
                let mut merged = existing.clone();
                deep_merge_mapping(&mut merged, incoming);
                target.insert(key.clone(), Value::Mapping(merged));
            }
            // All other cases → source value wins (overwrite or insert)
            _ => {
                target.insert(key.clone(), source_val.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_keys() {
        let yaml = r#"
.defaults: &defaults
  image: rust:latest
  timeout: 30m

job1:
  <<: *defaults
  script:
    - cargo test
"#;
        let mut value: Value = serde_yaml::from_str(yaml).unwrap();
        resolve_merge_keys(&mut value);

        let job1 = value.get("job1").unwrap();
        assert_eq!(job1.get("image").unwrap().as_str().unwrap(), "rust:latest");
        assert_eq!(job1.get("timeout").unwrap().as_str().unwrap(), "30m");
    }

    #[test]
    fn test_extends_resolution() {
        let yaml = r#"
.base:
  image: node:18
  before_script:
    - npm ci

test:
  extends: .base
  script:
    - npm test
"#;
        let mut value: Value = serde_yaml::from_str(yaml).unwrap();
        resolve_merge_keys(&mut value);
        if let Value::Mapping(ref mut m) = value {
            resolve_extends(m);
        }

        let test = value.get("test").unwrap();
        assert_eq!(test.get("image").unwrap().as_str().unwrap(), "node:18");
        assert!(test.get("extends").is_none()); // extends should be removed
        assert!(test.get("script").is_some());
        assert!(test.get("before_script").is_some());
    }

    #[test]
    fn test_extends_override() {
        let yaml = r#"
.base:
  image: node:16
  variables:
    NODE_ENV: production

test:
  extends: .base
  image: node:20
  script:
    - npm test
"#;
        let mut value: Value = serde_yaml::from_str(yaml).unwrap();
        resolve_merge_keys(&mut value);
        if let Value::Mapping(ref mut m) = value {
            resolve_extends(m);
        }

        let test = value.get("test").unwrap();
        // Extending job's image overrides base
        assert_eq!(test.get("image").unwrap().as_str().unwrap(), "node:20");
        // Variables from base should be present
        assert!(test.get("variables").is_some());
    }

    #[test]
    fn test_reference_tags() {
        let yaml = r#"
.shared:
  script:
    - echo "shared step 1"
    - echo "shared step 2"
  variables:
    SHARED_VAR: "hello"

test:
  script:
    - !reference [.shared, script]
    - echo "own step"
  variables:
    MY_VAR: "world"
"#;
        let mut value: Value = serde_yaml::from_str(yaml).unwrap();
        resolve_merge_keys(&mut value);
        resolve_reference_tags(&mut value);

        let test = value.get("test").unwrap();
        let script = test.get("script").unwrap().as_sequence().unwrap();
        // !reference should have been flattened: shared steps + own step
        assert!(
            script.len() >= 3,
            "expected at least 3 script entries, got {}",
            script.len()
        );
    }
}
