use std::path::{Path, PathBuf};

/// XDG data directory for lab (`$XDG_DATA_HOME/lab` or `~/.local/share/lab`).
pub fn data_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("lab");
    }
    home_dir().join(".local/share/lab")
}

/// XDG cache directory for lab (`$XDG_CACHE_HOME/lab` or `~/.cache/lab`).
pub fn cache_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("lab");
    }
    home_dir().join(".cache/lab")
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// Derive a project key from the GitLab remote URL (e.g. `group/project`).
/// Falls back to a hash of the canonical working directory path.
fn project_key(workdir: &Path) -> String {
    match crate::secrets::detect_gitlab_paths(workdir) {
        Ok((project_path, _)) => project_path,
        Err(_) => workdir_key(workdir),
    }
}

/// Derive a stable key from the canonical working directory path.
/// Returns a 16-char hex hash.
fn workdir_key(workdir: &Path) -> String {
    let canonical = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let input = canonical.to_string_lossy();
    let hash = simple_hash(&input);
    format!("{hash:016x}")
}

/// Centralized secrets directory for a project.
pub fn secrets_dir(workdir: &Path) -> PathBuf {
    data_dir().join("secrets").join(project_key(workdir))
}

/// Centralized secrets file path.
pub fn secrets_file(workdir: &Path) -> PathBuf {
    secrets_dir(workdir).join("secrets.env")
}

/// Centralized artifacts directory (per working copy).
pub fn artifacts_dir(workdir: &Path) -> PathBuf {
    cache_dir().join("artifacts").join(workdir_key(workdir))
}

/// Centralized cache directory (per working copy).
pub fn cache_base_dir(workdir: &Path) -> PathBuf {
    cache_dir().join("cache").join(workdir_key(workdir))
}

/// Centralized locks directory (per working copy).
pub fn locks_dir(workdir: &Path) -> PathBuf {
    data_dir().join("locks").join(workdir_key(workdir))
}

/// Last run result file (per working copy).
pub fn last_run_file(workdir: &Path) -> PathBuf {
    data_dir()
        .join("runs")
        .join(workdir_key(workdir))
        .join("last.json")
}

/// Centralized tmp directory (per working copy).
pub fn tmp_dir(workdir: &Path) -> PathBuf {
    data_dir().join("tmp").join(workdir_key(workdir))
}

pub(crate) fn simple_hash(input: &str) -> u64 {
    let mut hash: u64 = 5381;
    for byte in input.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workdir_key_deterministic() {
        let key1 = workdir_key(Path::new("/tmp/test-project"));
        let key2 = workdir_key(Path::new("/tmp/test-project"));
        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 16);
    }

    #[test]
    fn test_workdir_key_different_paths() {
        let key1 = workdir_key(Path::new("/tmp/project-a"));
        let key2 = workdir_key(Path::new("/tmp/project-b"));
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_data_dir_default() {
        // With HOME set, should produce ~/.local/share/lab
        let dir = data_dir();
        assert!(dir.to_string_lossy().ends_with("/lab"));
    }

    #[test]
    fn test_cache_dir_default() {
        let dir = cache_dir();
        assert!(dir.to_string_lossy().ends_with("/lab"));
    }

    #[test]
    fn test_secrets_file_path() {
        let path = secrets_file(Path::new("/tmp/some-project"));
        assert!(path.to_string_lossy().contains("secrets"));
        assert!(path.to_string_lossy().ends_with("secrets.env"));
    }
}
