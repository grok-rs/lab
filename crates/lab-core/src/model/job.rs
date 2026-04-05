use std::time::Duration;

use indexmap::IndexMap;
use serde::Deserialize;

use super::rules::Rule;
use super::variables::Variables;

/// A GitLab CI job definition.
/// Ref: <https://docs.gitlab.com/ci/yaml/#job-keywords>
#[derive(Debug, Clone, Deserialize)]
pub struct Job {
    /// Docker image to run the job in.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#image>
    #[serde(default)]
    pub image: Option<ImageConfig>,

    /// Pipeline stage this job belongs to (default: "test").
    /// Ref: <https://docs.gitlab.com/ci/yaml/#stage>
    #[serde(default = "default_stage")]
    pub stage: String,

    /// Shell commands to execute.
    /// Required for regular jobs, optional for trigger jobs.
    /// Can be a string (single command) or array of strings.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#script>
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub script: Vec<String>,

    /// Commands to run before `script`.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#before_script>
    #[serde(default, deserialize_with = "deserialize_optional_string_or_vec")]
    pub before_script: Option<Vec<String>>,

    /// Commands to run after `script` (always executed, even on failure).
    /// Ref: <https://docs.gitlab.com/ci/yaml/#after_script>
    #[serde(default, deserialize_with = "deserialize_optional_string_or_vec")]
    pub after_script: Option<Vec<String>>,

    /// Job-level variables (override global).
    /// Ref: <https://docs.gitlab.com/ci/yaml/#variables>
    #[serde(default)]
    pub variables: Variables,

    /// Conditional rules for when the job runs.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#rules>
    #[serde(default)]
    pub rules: Option<Vec<Rule>>,

    /// DAG dependencies — jobs that must complete before this one.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#needs>
    #[serde(default)]
    pub needs: Option<Vec<Need>>,

    /// Restrict artifact download to specific jobs.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#dependencies>
    #[serde(default)]
    pub dependencies: Option<Vec<String>>,

    /// Artifact configuration.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#artifacts>
    #[serde(default)]
    pub artifacts: Option<ArtifactConfig>,

    /// Cache configuration.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#cache>
    #[serde(default, deserialize_with = "deserialize_cache_config")]
    pub cache: Option<Vec<CacheConfig>>,

    /// Service containers (sidecars).
    /// Ref: <https://docs.gitlab.com/ci/yaml/#services>
    #[serde(default)]
    pub services: Option<Vec<ServiceConfig>>,

    /// When to run the job.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#when>
    #[serde(default)]
    pub when: When,

    /// Allow this job to fail without failing the pipeline.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#allow_failure>
    #[serde(default)]
    pub allow_failure: AllowFailure,

    /// Job timeout.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#timeout>
    #[serde(default, deserialize_with = "deserialize_duration_opt")]
    pub timeout: Option<Duration>,

    /// Retry configuration.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#retry>
    #[serde(default)]
    pub retry: Option<RetryConfig>,

    /// Run multiple instances of this job.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#parallel>
    #[serde(default)]
    pub parallel: Option<ParallelConfig>,

    /// Inherit configuration from other jobs.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#extends>
    #[serde(default)]
    pub extends: Option<StringOrVec>,

    /// Runner tags (ignored locally, but parsed for compatibility).
    /// Ref: <https://docs.gitlab.com/ci/yaml/#tags>
    #[serde(default)]
    pub tags: Option<Vec<String>>,

    /// Concurrency control.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#resource_group>
    #[serde(default)]
    pub resource_group: Option<String>,

    /// Whether the job can be cancelled when a newer pipeline starts.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#interruptible>
    #[serde(default)]
    pub interruptible: Option<bool>,

    /// Control inheritance from global defaults.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#inherit>
    #[serde(default)]
    pub inherit: Option<InheritConfig>,

    /// Code coverage regex pattern.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#coverage>
    #[serde(default)]
    pub coverage: Option<String>,

    /// Delay job execution after manual trigger.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#start_in>
    #[serde(default)]
    pub start_in: Option<String>,

    /// Downstream pipeline trigger.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#trigger>
    #[serde(default)]
    pub trigger: Option<TriggerConfig>,

    /// Custom confirmation message for manual jobs.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#manual_confirmation>
    #[serde(default)]
    pub manual_confirmation: Option<String>,
}

/// Trigger configuration for downstream/child pipelines.
/// Ref: <https://docs.gitlab.com/ci/yaml/#trigger>
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum TriggerConfig {
    /// Simple project path trigger
    Simple(String),
    /// Detailed trigger config
    Detailed {
        #[serde(default)]
        include: Option<StringOrVec>,
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        strategy: Option<String>,
    },
}

fn default_stage() -> String {
    "test".to_string()
}

/// Image can be a simple string or a detailed config.
/// Ref: <https://docs.gitlab.com/ci/yaml/#image>
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ImageConfig {
    Simple(String),
    Detailed {
        name: String,
        #[serde(default)]
        entrypoint: Option<Vec<String>>,
    },
}

impl ImageConfig {
    pub fn name(&self) -> &str {
        match self {
            Self::Simple(name) => name,
            Self::Detailed { name, .. } => name,
        }
    }

    pub fn entrypoint(&self) -> Option<&[String]> {
        match self {
            Self::Simple(_) => None,
            Self::Detailed { entrypoint, .. } => entrypoint.as_deref(),
        }
    }
}

/// Job dependency via `needs:`.
/// Ref: <https://docs.gitlab.com/ci/yaml/#needs>
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Need {
    Simple(String),
    Detailed {
        job: String,
        #[serde(default = "default_true")]
        artifacts: bool,
        #[serde(default)]
        optional: bool,
    },
}

impl Need {
    pub fn job_name(&self) -> &str {
        match self {
            Self::Simple(name) => name,
            Self::Detailed { job, .. } => job,
        }
    }

    pub fn wants_artifacts(&self) -> bool {
        match self {
            Self::Simple(_) => true,
            Self::Detailed { artifacts, .. } => *artifacts,
        }
    }

    /// Whether this dependency is optional (won't fail if the dep doesn't exist).
    /// Ref: <https://docs.gitlab.com/ci/yaml/#needsoptional>
    pub fn is_optional(&self) -> bool {
        match self {
            Self::Simple(_) => false,
            Self::Detailed { optional, .. } => *optional,
        }
    }
}

fn default_true() -> bool {
    true
}

/// When to execute a job.
/// Ref: <https://docs.gitlab.com/ci/yaml/#when>
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum When {
    #[default]
    OnSuccess,
    OnFailure,
    Always,
    Manual,
    Delayed,
    Never,
}

/// Whether a job is allowed to fail.
/// Ref: <https://docs.gitlab.com/ci/yaml/#allow_failure>
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AllowFailure {
    Bool(bool),
    ExitCodes { exit_codes: Vec<i32> },
}

impl Default for AllowFailure {
    fn default() -> Self {
        Self::Bool(false)
    }
}

impl AllowFailure {
    pub fn is_allowed(&self, exit_code: i32) -> bool {
        match self {
            Self::Bool(allowed) => *allowed,
            Self::ExitCodes { exit_codes } => exit_codes.contains(&exit_code),
        }
    }
}

/// Artifact configuration.
/// Ref: <https://docs.gitlab.com/ci/yaml/#artifacts>
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ArtifactConfig {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub expire_in: Option<String>,
    #[serde(default, rename = "when")]
    pub when_upload: Option<ArtifactWhen>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub untracked: bool,
    /// Test/coverage/security report artifacts.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#artifactsreports>
    #[serde(default)]
    pub reports: Option<ArtifactReports>,
    /// Public access flag.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#artifactspublic>
    #[serde(default)]
    pub public: Option<bool>,
    /// Access level.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#artifactsaccess>
    #[serde(default)]
    pub access: Option<String>,
    /// Expose as link in MR.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#artifactsexpose_as>
    #[serde(default)]
    pub expose_as: Option<String>,
}

/// Artifact report types.
/// Ref: <https://docs.gitlab.com/ci/yaml/#artifactsreports>
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ArtifactReports {
    #[serde(default)]
    pub junit: Option<StringOrVec>,
    #[serde(default)]
    pub coverage_report: Option<serde_yaml::Value>,
    #[serde(default)]
    pub codequality: Option<StringOrVec>,
    #[serde(default)]
    pub sast: Option<StringOrVec>,
    #[serde(default)]
    pub dependency_scanning: Option<StringOrVec>,
    #[serde(default)]
    pub container_scanning: Option<StringOrVec>,
    #[serde(default)]
    pub dast: Option<StringOrVec>,
    #[serde(default)]
    pub license_scanning: Option<StringOrVec>,
    #[serde(default)]
    pub dotenv: Option<StringOrVec>,
    #[serde(default)]
    pub terraform: Option<StringOrVec>,
    #[serde(default)]
    pub metrics: Option<StringOrVec>,
    #[serde(default)]
    pub requirements: Option<StringOrVec>,
    #[serde(default)]
    pub performance: Option<StringOrVec>,
    #[serde(default)]
    pub browser_performance: Option<StringOrVec>,
    #[serde(default)]
    pub load_performance: Option<StringOrVec>,
    #[serde(default)]
    pub accessibility: Option<StringOrVec>,
    #[serde(default)]
    pub annotations: Option<StringOrVec>,
    #[serde(default)]
    pub cyclonedx: Option<StringOrVec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactWhen {
    OnSuccess,
    OnFailure,
    Always,
}

/// Cache configuration.
/// Ref: <https://docs.gitlab.com/ci/yaml/#cache>
#[derive(Debug, Clone, Deserialize)]
pub struct CacheConfig {
    #[serde(default)]
    pub key: Option<CacheKey>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub policy: Option<CachePolicy>,
    #[serde(default)]
    pub untracked: bool,
    #[serde(default)]
    pub fallback_keys: Vec<String>,
    /// When to upload cache: on_success (default), on_failure, or always.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#cachewhen>
    #[serde(default, rename = "when")]
    pub when_upload: Option<CacheWhen>,
}

/// When to upload cache.
/// Ref: <https://docs.gitlab.com/ci/yaml/#cachewhen>
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheWhen {
    #[default]
    OnSuccess,
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum CacheKey {
    Simple(String),
    Detailed {
        files: Vec<String>,
        #[serde(default)]
        prefix: Option<String>,
        /// Include commit SHAs in key hash.
        /// Ref: <https://docs.gitlab.com/ci/yaml/#cachekeyfiles_commits>
        #[serde(default)]
        files_commits: Option<bool>,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CachePolicy {
    #[default]
    PullPush,
    Pull,
    Push,
}

/// Service container configuration.
/// Ref: <https://docs.gitlab.com/ci/yaml/#services>
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ServiceConfig {
    Simple(String),
    Detailed {
        name: String,
        #[serde(default)]
        alias: Option<String>,
        #[serde(default)]
        entrypoint: Option<Vec<String>>,
        #[serde(default)]
        command: Option<Vec<String>>,
        #[serde(default)]
        variables: Variables,
    },
}

impl ServiceConfig {
    pub fn image_name(&self) -> &str {
        match self {
            Self::Simple(name) => name,
            Self::Detailed { name, .. } => name,
        }
    }

    /// Derive the service hostname.
    /// Ref: <https://docs.gitlab.com/ci/services/#accessing-the-services>
    pub fn hostname(&self) -> String {
        match self {
            Self::Detailed {
                alias: Some(alias), ..
            } => alias.clone(),
            _ => {
                let image = self.image_name();
                // Strip tag (everything after ':')
                let without_tag = image.split(':').next().unwrap_or(image);
                // Replace '/' with '__' for primary alias
                without_tag.replace('/', "__")
            }
        }
    }
}

/// Retry configuration.
/// Ref: <https://docs.gitlab.com/ci/yaml/#retry>
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RetryConfig {
    Count(u32),
    Detailed {
        max: u32,
        #[serde(default, rename = "when")]
        when_retry: Vec<String>,
    },
}

impl RetryConfig {
    pub fn max_retries(&self) -> u32 {
        match self {
            Self::Count(n) => *n,
            Self::Detailed { max, .. } => *max,
        }
    }

    /// Check if retry should happen for a given failure type.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#retrywhen>
    ///
    /// If `retry:when` is empty or contains "always", retry on any failure.
    /// Otherwise, only retry if the failure type matches.
    pub fn should_retry(&self, failure_type: &str) -> bool {
        match self {
            Self::Count(_) => true, // No when filter → retry always
            Self::Detailed { when_retry, .. } => {
                if when_retry.is_empty() {
                    return true;
                }
                when_retry
                    .iter()
                    .any(|w| w == "always" || w == failure_type)
            }
        }
    }
}

/// Parallel job configuration.
/// Ref: <https://docs.gitlab.com/ci/yaml/#parallel>
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ParallelConfig {
    Count(u32),
    Matrix {
        matrix: Vec<IndexMap<String, StringOrVec>>,
    },
}

/// A value that can be either a single string or a list of strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StringOrVec {
    Single(String),
    Multiple(Vec<String>),
}

impl StringOrVec {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            Self::Single(s) => vec![s],
            Self::Multiple(v) => v,
        }
    }

    pub fn as_slice(&self) -> Vec<&str> {
        match self {
            Self::Single(s) => vec![s.as_str()],
            Self::Multiple(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

/// Control what global settings a job inherits.
/// Ref: <https://docs.gitlab.com/ci/yaml/#inherit>
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InheritConfig {
    #[serde(default)]
    pub default: Option<InheritToggle>,
    #[serde(default)]
    pub variables: Option<InheritToggle>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum InheritToggle {
    Bool(bool),
    List(Vec<String>),
}

/// Default job settings applied to all jobs.
/// Ref: <https://docs.gitlab.com/ci/yaml/#default>
#[derive(Debug, Clone, Default, Deserialize)]
pub struct JobDefaults {
    #[serde(default)]
    pub image: Option<ImageConfig>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_vec")]
    pub before_script: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_vec")]
    pub after_script: Option<Vec<String>>,
    #[serde(default)]
    pub services: Option<Vec<ServiceConfig>>,
    #[serde(default, deserialize_with = "deserialize_cache_config")]
    pub cache: Option<Vec<CacheConfig>>,
    #[serde(default)]
    pub artifacts: Option<ArtifactConfig>,
    #[serde(default)]
    pub retry: Option<RetryConfig>,
    #[serde(default, deserialize_with = "deserialize_duration_opt")]
    pub timeout: Option<Duration>,
    #[serde(default)]
    pub interruptible: Option<bool>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// Deserialize a value that can be a string or an array of strings into Vec<String>.
fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct StringOrVecVisitor;
    impl<'de> de::Visitor<'de> for StringOrVecVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or array of strings")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Vec<String>, E> {
            Ok(vec![v.to_string()])
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<String>, A::Error> {
            let mut vec = Vec::new();
            while let Some(val) = seq.next_element::<String>()? {
                vec.push(val);
            }
            Ok(vec)
        }

        fn visit_none<E: de::Error>(self) -> Result<Vec<String>, E> {
            Ok(Vec::new())
        }

        fn visit_unit<E: de::Error>(self) -> Result<Vec<String>, E> {
            Ok(Vec::new())
        }
    }

    deserializer.deserialize_any(StringOrVecVisitor)
}

/// Deserialize an optional value that can be a string or array of strings.
fn deserialize_optional_string_or_vec<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct OptStringOrVecVisitor;
    impl<'de> de::Visitor<'de> for OptStringOrVecVisitor {
        type Value = Option<Vec<String>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("null, a string, or array of strings")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Option<Vec<String>>, E> {
            Ok(Some(vec![v.to_string()]))
        }

        fn visit_seq<A: de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Option<Vec<String>>, A::Error> {
            let mut vec = Vec::new();
            while let Some(val) = seq.next_element::<String>()? {
                vec.push(val);
            }
            Ok(Some(vec))
        }

        fn visit_none<E: de::Error>(self) -> Result<Option<Vec<String>>, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Option<Vec<String>>, E> {
            Ok(None)
        }
    }

    deserializer.deserialize_any(OptStringOrVecVisitor)
}

/// Deserialize cache: which can be a single CacheConfig or an array.
fn deserialize_cache_config<'de, D>(deserializer: D) -> Result<Option<Vec<CacheConfig>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct CacheVisitor;
    impl<'de> de::Visitor<'de> for CacheVisitor {
        type Value = Option<Vec<CacheConfig>>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("null, a cache config object, or array of cache configs")
        }

        fn visit_none<E: de::Error>(self) -> Result<Option<Vec<CacheConfig>>, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Option<Vec<CacheConfig>>, E> {
            Ok(None)
        }

        fn visit_seq<A: de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Option<Vec<CacheConfig>>, A::Error> {
            let mut vec = Vec::new();
            while let Some(item) = seq.next_element::<CacheConfig>()? {
                vec.push(item);
            }
            Ok(Some(vec))
        }

        fn visit_map<M: de::MapAccess<'de>>(
            self,
            map: M,
        ) -> Result<Option<Vec<CacheConfig>>, M::Error> {
            let config = CacheConfig::deserialize(de::value::MapAccessDeserializer::new(map))?;
            Ok(Some(vec![config]))
        }
    }

    deserializer.deserialize_any(CacheVisitor)
}

fn deserialize_duration_opt<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(s) => parse_duration(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// Parse GitLab CI duration strings like "1h 30m", "3600", "30 minutes".
fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();

    // Try pure seconds
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }

    let mut total_secs = 0u64;
    let mut num_buf = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num_buf.push(ch);
        } else if ch.is_alphabetic() {
            let n: u64 = num_buf
                .parse()
                .map_err(|_| format!("invalid duration: {s}"))?;
            num_buf.clear();
            match ch {
                'h' | 'H' => total_secs += n * 3600,
                'm' | 'M' => total_secs += n * 60,
                's' | 'S' => total_secs += n,
                _ => return Err(format!("unknown duration unit: {ch}")),
            }
        }
    }

    if !num_buf.is_empty() {
        let n: u64 = num_buf
            .parse()
            .map_err(|_| format!("invalid duration: {s}"))?;
        total_secs += n;
    }

    if total_secs == 0 {
        return Err(format!("invalid duration: {s}"));
    }

    Ok(Duration::from_secs(total_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("3600").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
    }

    #[test]
    fn test_service_hostname() {
        let svc = ServiceConfig::Simple("postgres:14".to_string());
        assert_eq!(svc.hostname(), "postgres");

        let svc = ServiceConfig::Simple("registry.example.com/my/postgres:14".to_string());
        assert_eq!(svc.hostname(), "registry.example.com__my__postgres");

        let svc = ServiceConfig::Detailed {
            name: "postgres:14".to_string(),
            alias: Some("db".to_string()),
            entrypoint: None,
            command: None,
            variables: Variables::new(),
        };
        assert_eq!(svc.hostname(), "db");
    }

    #[test]
    fn test_allow_failure() {
        let af = AllowFailure::Bool(true);
        assert!(af.is_allowed(1));
        assert!(af.is_allowed(0));

        let af = AllowFailure::Bool(false);
        assert!(!af.is_allowed(1));

        let af = AllowFailure::ExitCodes {
            exit_codes: vec![137, 143],
        };
        assert!(af.is_allowed(137));
        assert!(!af.is_allowed(1));
    }
}
