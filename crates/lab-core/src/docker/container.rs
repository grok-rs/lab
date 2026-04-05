/// Result of running a command inside a container.
#[derive(Debug)]
pub struct RunResult {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}
