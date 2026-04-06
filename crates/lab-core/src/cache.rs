use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::error::{LabError, Result};
use crate::model::job::{CacheConfig, CacheKey, CachePolicy};
use crate::model::variables::{Variables, expand_variables};

/// Get the cache directory for a specific key.
fn cache_dir(workdir: &Path, key: &str) -> PathBuf {
    let sanitized = key.replace(['/', '\\', ' '], "_");
    crate::paths::cache_base_dir(workdir).join(sanitized)
}

/// Resolve the cache key from config and variables.
///
/// Ref: <https://docs.gitlab.com/ci/yaml/#cachekey>
pub fn resolve_cache_key(config: &CacheConfig, variables: &Variables) -> String {
    match &config.key {
        Some(CacheKey::Simple(s)) => expand_variables(s, variables),
        Some(CacheKey::Detailed {
            files,
            prefix,
            files_commits,
        }) => {
            let mut hasher_input = String::new();
            for file in files {
                let expanded = expand_variables(file, variables);
                if let Ok(content) = std::fs::read_to_string(&expanded) {
                    hasher_input.push_str(&content);
                }
                // Include commit SHA for this file if files_commits is enabled
                if files_commits.unwrap_or(false) {
                    if let Ok(output) = std::process::Command::new("git")
                        .args(["log", "-1", "--format=%H", "--", &expanded])
                        .output()
                    {
                        if output.status.success() {
                            hasher_input.push_str(String::from_utf8_lossy(&output.stdout).trim());
                        }
                    }
                }
            }
            let hash = simple_hash(&hasher_input);
            match prefix {
                Some(p) => format!("{}-{hash}", expand_variables(p, variables)),
                None => hash,
            }
        }
        None => "default".to_string(),
    }
}

/// Restore cache into a container before job execution.
///
/// Ref: <https://docs.gitlab.com/ci/yaml/#cache>
pub fn restore_cache(
    container_id: &str,
    configs: &[CacheConfig],
    variables: &Variables,
    workdir: &Path,
) -> Result<()> {
    for config in configs {
        let policy = config.policy.clone().unwrap_or_default();
        if matches!(policy, CachePolicy::Push) {
            continue; // push-only, don't restore
        }

        let key = resolve_cache_key(config, variables);
        let source = cache_dir(workdir, &key);

        if !source.exists() {
            // Try fallback keys
            let mut found = false;
            for fallback in &config.fallback_keys {
                let fallback_key = expand_variables(fallback, variables);
                let fallback_dir = cache_dir(workdir, &fallback_key);
                if fallback_dir.exists() {
                    info!(key = %fallback_key, "restoring cache (fallback)");
                    copy_cache_to_container(container_id, &fallback_dir, &config.paths)?;
                    found = true;
                    break;
                }
            }
            if !found {
                debug!(key = %key, "no cache found");
            }
            continue;
        }

        info!(key = %key, "restoring cache");
        copy_cache_to_container(container_id, &source, &config.paths)?;
    }
    Ok(())
}

/// Save cache from a container after job execution.
/// Respects `cache:when` to control when cache is uploaded.
/// Ref: <https://docs.gitlab.com/ci/yaml/#cachewhen>
pub fn save_cache(
    container_id: &str,
    configs: &[CacheConfig],
    variables: &Variables,
    workdir: &Path,
    job_succeeded: bool,
) -> Result<()> {
    use crate::model::job::CacheWhen;

    for config in configs {
        let policy = config.policy.clone().unwrap_or_default();
        if matches!(policy, CachePolicy::Pull) {
            continue; // pull-only, don't save
        }

        // Check cache:when condition
        let when = config.when_upload.clone().unwrap_or_default();
        let should_save = match when {
            CacheWhen::OnSuccess => job_succeeded,
            CacheWhen::OnFailure => !job_succeeded,
            CacheWhen::Always => true,
        };
        if !should_save {
            continue;
        }

        let key = resolve_cache_key(config, variables);
        let dest = cache_dir(workdir, &key);

        info!(key = %key, "saving cache");

        std::fs::create_dir_all(&dest).map_err(|e| LabError::FileRead {
            path: dest.clone(),
            source: e,
        })?;

        // Copy each path from container
        for path in &config.paths {
            let container_path = format!("/workspace/{path}");
            let output = std::process::Command::new("docker")
                .args([
                    "cp",
                    &format!("{container_id}:{container_path}"),
                    dest.to_str().unwrap_or("."),
                ])
                .output();

            match output {
                Ok(o) if o.status.success() => {
                    debug!(key = %key, path = %path, "cache path saved");
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    debug!(path = %path, error = %stderr, "cache path not found, skipping");
                }
                Err(e) => {
                    warn!(path = %path, error = %e, "docker cp failed for cache");
                }
            }
        }
    }
    Ok(())
}

fn copy_cache_to_container(container_id: &str, source_dir: &Path, _paths: &[String]) -> Result<()> {
    for entry in std::fs::read_dir(source_dir).map_err(|e| LabError::FileRead {
        path: source_dir.to_path_buf(),
        source: e,
    })? {
        let entry = entry.map_err(|e| LabError::Other(e.to_string()))?;
        let output = std::process::Command::new("docker")
            .args([
                "cp",
                entry.path().to_str().unwrap_or(""),
                &format!("{container_id}:/workspace/"),
            ])
            .output();

        if let Err(e) = output {
            warn!(error = %e, "failed to restore cache entry");
        }
    }
    Ok(())
}

/// Simple string hash for cache keys.
fn simple_hash(input: &str) -> String {
    let mut hash: u64 = 5381;
    for byte in input.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
    }
    format!("{hash:016x}")
}

/// Clean up all cache data.
pub fn cleanup_cache(workdir: &Path) {
    let dir = crate::paths::cache_base_dir(workdir);
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}
