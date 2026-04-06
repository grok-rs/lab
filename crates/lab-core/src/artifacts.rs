use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::error::{LabError, Result};
use crate::model::job::ArtifactConfig;

/// Get the artifacts directory for a specific job.
pub fn job_artifacts_dir(workdir: &Path, job_name: &str) -> PathBuf {
    crate::paths::artifacts_dir(workdir).join(job_name)
}

/// Collect artifacts from a container after a job completes.
/// Uses `docker exec tar` to archive matching paths, then copies to local storage.
///
/// Ref: <https://docs.gitlab.com/ci/yaml/#artifacts>
pub async fn collect_artifacts(
    _docker: &crate::docker::DockerClient,
    container_id: &str,
    job_name: &str,
    config: &ArtifactConfig,
    workdir: &Path,
) -> Result<()> {
    if config.paths.is_empty() {
        return Ok(());
    }

    let dest_dir = job_artifacts_dir(workdir, job_name);
    std::fs::create_dir_all(&dest_dir).map_err(|e| LabError::FileRead {
        path: dest_dir.clone(),
        source: e,
    })?;

    info!(job = %job_name, paths = ?config.paths, "collecting artifacts");

    // If artifacts:untracked is true, also collect git-untracked files
    // Ref: <https://docs.gitlab.com/ci/yaml/#artifactsuntracked>
    let mut all_paths = config.paths.clone();
    if config.untracked {
        let output = std::process::Command::new("docker")
            .args([
                "exec",
                container_id,
                "sh",
                "-c",
                "cd /workspace && git ls-files --others --exclude-standard 2>/dev/null || true",
            ])
            .output();
        if let Ok(o) = output {
            if o.status.success() {
                for line in String::from_utf8_lossy(&o.stdout).lines() {
                    let line = line.trim();
                    if !line.is_empty() {
                        all_paths.push(line.to_string());
                    }
                }
            }
        }
    }

    // Use docker cp to copy each artifact path
    for artifact_path in &all_paths {
        let container_path = if artifact_path.starts_with('/') {
            artifact_path.to_string()
        } else {
            format!("/workspace/{artifact_path}")
        };

        // Use docker cp via CLI (simpler than bollard's archive API)
        let output = std::process::Command::new("docker")
            .args([
                "cp",
                &format!("{container_id}:{container_path}"),
                dest_dir.to_str().unwrap_or("."),
            ])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                debug!(job = %job_name, path = %artifact_path, "artifact collected");
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                warn!(job = %job_name, path = %artifact_path, error = %stderr, "artifact not found");
            }
            Err(e) => {
                warn!(job = %job_name, path = %artifact_path, error = %e, "docker cp failed");
            }
        }
    }

    // Apply artifacts:exclude — remove files matching exclude patterns
    // Ref: <https://docs.gitlab.com/ci/yaml/#artifactsexclude>
    if !config.exclude.is_empty() {
        remove_excluded_artifacts(&dest_dir, &config.exclude);
    }

    Ok(())
}

/// Remove files from the artifacts directory that match exclude patterns.
fn remove_excluded_artifacts(dir: &Path, exclude_patterns: &[String]) {
    for pattern in exclude_patterns {
        let full_pattern = format!("{}/{}", dir.display(), pattern);
        if let Ok(entries) = glob::glob(&full_pattern) {
            for entry in entries.flatten() {
                if entry.is_file() {
                    debug!(path = %entry.display(), "excluding artifact");
                    let _ = std::fs::remove_file(&entry);
                } else if entry.is_dir() {
                    let _ = std::fs::remove_dir_all(&entry);
                }
            }
        }
    }
}

/// Inject artifacts from dependency jobs into a container.
///
/// Ref: <https://docs.gitlab.com/ci/yaml/#dependencies>
/// Ref: <https://docs.gitlab.com/ci/yaml/#needsartifacts>
pub async fn inject_artifacts(
    _docker: &crate::docker::DockerClient,
    container_id: &str,
    dep_job_names: &[String],
    workdir: &Path,
) -> Result<()> {
    for dep_name in dep_job_names {
        let artifacts_dir = job_artifacts_dir(workdir, dep_name);
        if !artifacts_dir.exists() {
            debug!(dependency = %dep_name, "no artifacts to inject");
            continue;
        }

        info!(dependency = %dep_name, "injecting artifacts");

        // Copy each item in the artifacts dir into the container workspace
        let entries = std::fs::read_dir(&artifacts_dir).map_err(|e| LabError::FileRead {
            path: artifacts_dir.clone(),
            source: e,
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| LabError::Other(e.to_string()))?;
            let path = entry.path();

            let output = std::process::Command::new("docker")
                .args([
                    "cp",
                    path.to_str().unwrap_or(""),
                    &format!("{container_id}:/workspace/"),
                ])
                .output();

            match output {
                Ok(o) if o.status.success() => {
                    debug!(dependency = %dep_name, file = ?entry.file_name(), "artifact injected");
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    warn!(dependency = %dep_name, error = %stderr, "failed to inject artifact");
                }
                Err(e) => {
                    warn!(dependency = %dep_name, error = %e, "docker cp failed");
                }
            }
        }
    }

    Ok(())
}

/// Clean up all artifacts after pipeline completes.
pub fn cleanup_artifacts(workdir: &Path) {
    let dir = crate::paths::artifacts_dir(workdir);
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}
