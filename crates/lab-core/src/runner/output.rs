use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Tracks the results of all jobs in a pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineResult {
    inner: Arc<Mutex<PipelineResultInner>>,
}

#[derive(Debug)]
struct PipelineResultInner {
    jobs: BTreeMap<String, JobResult>,
    start_time: Instant,
}

#[derive(Debug, Clone)]
pub struct JobResult {
    pub name: String,
    pub stage: String,
    pub status: JobStatus,
    pub duration: Duration,
    pub coverage: Option<f64>,
    /// Offset from pipeline start when this job began.
    pub start_offset: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Success,
    Failed,
    AllowedFailure,
}

impl PipelineResult {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PipelineResultInner {
                jobs: BTreeMap::new(),
                start_time: Instant::now(),
            })),
        }
    }

    pub fn record(&self, name: &str, stage: &str, status: JobStatus, duration: Duration) {
        self.record_with_coverage(name, stage, status, duration, None);
    }

    pub fn record_with_coverage(
        &self,
        name: &str,
        stage: &str,
        status: JobStatus,
        duration: Duration,
        coverage: Option<f64>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        let start_offset = inner.start_time.elapsed() - duration;
        inner.jobs.insert(
            name.to_string(),
            JobResult {
                name: name.to_string(),
                stage: stage.to_string(),
                status,
                duration,
                coverage,
                start_offset,
            },
        );
    }

    /// Extract coverage percentage from job output using a regex pattern.
    /// Ref: <https://docs.gitlab.com/ci/yaml/#coverage>
    pub fn extract_coverage(output: &str, pattern: &str) -> Option<f64> {
        let re = regex::Regex::new(pattern).ok()?;
        let num_re = regex::Regex::new(r"(\d+\.?\d*)").ok()?;
        // Search for the pattern in output, extract the first capture group or full match
        for cap in re.captures_iter(output) {
            // Try to find a floating point number in the match
            let matched = cap.get(1).or_else(|| cap.get(0))?.as_str();
            if let Some(num_match) = num_re.captures(matched) {
                if let Ok(val) = num_match[1].parse::<f64>() {
                    return Some(val);
                }
            }
        }
        None
    }

    pub fn total_duration(&self) -> Duration {
        self.inner.lock().unwrap().start_time.elapsed()
    }

    pub fn jobs(&self) -> Vec<JobResult> {
        self.inner.lock().unwrap().jobs.values().cloned().collect()
    }

    pub fn has_failures(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .jobs
            .values()
            .any(|j| j.status == JobStatus::Failed)
    }
}

impl Default for PipelineResult {
    fn default() -> Self {
        Self::new()
    }
}
