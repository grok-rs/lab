use lab_core::config::{Config, ManualMode, ProjectConfig, PullPolicy};
use std::io::Write;

#[test]
fn config_default_values() {
    let config = Config::default();
    assert_eq!(config.ci_file.to_str().unwrap(), ".gitlab-ci.yml");
    assert_eq!(config.workdir.to_str().unwrap(), ".");
    assert!(config.job_filter.is_none());
    assert!(config.stage_filter.is_none());
    assert!(config.variables.is_empty());
    assert!(!config.privileged);
    assert!(!config.no_artifacts);
    assert!(!config.no_cache);
    assert!(config.platform_overrides.is_empty());
    assert!(config.max_parallel > 0);
}

#[test]
fn project_config_load_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let config = ProjectConfig::load(dir.path());
    assert!(config.variables.is_empty());
    assert!(config.image.is_none());
    assert!(config.pull_policy.is_none());
}

#[test]
fn project_config_load_valid_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".lab.yml");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(
        b"variables:\n  NX_NO_CLOUD: 'true'\npull_policy: always\nprivileged: true\nmax_parallel: 2\n",
    )
    .unwrap();

    let config = ProjectConfig::load(dir.path());
    assert_eq!(config.variables.get("NX_NO_CLOUD").unwrap(), "true");
    assert_eq!(config.pull_policy.as_deref(), Some("always"));
    assert_eq!(config.privileged, Some(true));
    assert_eq!(config.max_parallel, Some(2));
}

#[test]
fn project_config_load_invalid_yaml_returns_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".lab.yml");
    std::fs::write(&path, "{{invalid yaml").unwrap();

    let config = ProjectConfig::load(dir.path());
    assert!(config.variables.is_empty());
}

#[test]
fn project_config_apply_to_sets_defaults() {
    let project = ProjectConfig {
        variables: [("KEY".into(), "from_project".into())]
            .into_iter()
            .collect(),
        pull_policy: Some("always".into()),
        privileged: Some(true),
        max_parallel: Some(8),
        ..Default::default()
    };

    let mut config = Config::default();
    project.apply_to(&mut config);

    assert_eq!(config.variables.get("KEY").unwrap(), "from_project");
    assert!(matches!(config.pull_policy, PullPolicy::Always));
    assert!(config.privileged);
    assert_eq!(config.max_parallel, 8);
}

#[test]
fn project_config_cli_variables_take_precedence() {
    let project = ProjectConfig {
        variables: [("KEY".into(), "from_project".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    };

    let mut config = Config::default();
    config.variables.insert("KEY".into(), "from_cli".into());
    project.apply_to(&mut config);

    // CLI value should win
    assert_eq!(config.variables.get("KEY").unwrap(), "from_cli");
}

#[test]
fn project_config_pull_policy_variants() {
    let mut config = Config::default();

    let project_always = ProjectConfig {
        pull_policy: Some("always".into()),
        ..Default::default()
    };
    project_always.apply_to(&mut config);
    assert!(matches!(config.pull_policy, PullPolicy::Always));

    let project_never = ProjectConfig {
        pull_policy: Some("never".into()),
        ..Default::default()
    };
    project_never.apply_to(&mut config);
    assert!(matches!(config.pull_policy, PullPolicy::Never));

    let project_default = ProjectConfig {
        pull_policy: Some("if-not-present".into()),
        ..Default::default()
    };
    project_default.apply_to(&mut config);
    assert!(matches!(config.pull_policy, PullPolicy::IfNotPresent));
}

#[test]
fn project_config_platforms() {
    let project = ProjectConfig {
        platforms: [("build".into(), "arm64v8/node:20".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    };

    let mut config = Config::default();
    project.apply_to(&mut config);

    assert_eq!(
        config.platform_overrides.get("build").unwrap(),
        "arm64v8/node:20"
    );
}

#[test]
fn manual_mode_default_is_prompt() {
    let mode = ManualMode::default();
    assert!(matches!(mode, ManualMode::Prompt));
}
