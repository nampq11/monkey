use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

pub mod pi;
pub mod pi_protocol;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to spawn pi binary `{binary}`: {source}")]
    Spawn {
        binary: String,
        #[source]
        source: std::io::Error,
    },
    #[error("RPC framing error: {0}")]
    Framing(String),
    #[error("pi exited before {0}")]
    PrematureExit(String),
    #[error("timed out waiting for {0}")]
    Timeout(String),
    /// pi answered a command with `success: false`. Carrying the engine's own
    /// `error` text here is what keeps a rejection from surfacing later as a
    /// misleading "response missing data object" framing error.
    #[error("pi rejected {command}: {error}")]
    EngineRejected { command: String, error: String },
}

#[derive(Debug, Clone)]
pub struct RunParams<'a> {
    pub prompt: &'a str,
    pub worktree: &'a Path,
    pub session_dir: &'a Path,
    pub model: &'a str,
    pub thinking: &'a str,
    pub provider: &'a str,
    pub timeout: Duration,
}

/// Whether the engine run produced a trustworthy result.
///
/// A run can settle cleanly at the process level and still be a failure: pi
/// emits `auto_retry_end` with `success: false` once it has exhausted its
/// provider retries, then settles normally. Modelling that here instead of as
/// a free-form string is what stops `worker.rs` from writing back a failed
/// triage as if it succeeded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    #[default]
    Ok,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub session_dir: PathBuf,
    pub status: OutcomeStatus,
    pub summary: String,
    pub pr_body: String,
    pub comment: String,
    pub branch: String,
    pub artifact_paths: Vec<PathBuf>,
    pub raw_events: Vec<Value>,
}

impl Default for Outcome {
    fn default() -> Self {
        Self {
            session_dir: PathBuf::new(),
            status: OutcomeStatus::default(),
            summary: String::new(),
            pr_body: String::new(),
            comment: String::new(),
            branch: String::new(),
            artifact_paths: Vec::new(),
            raw_events: Vec::new(),
        }
    }
}

pub trait EngineAdapter: Send + Sync {
    fn run(
        &self,
        params: RunParams<'_>,
    ) -> impl std::future::Future<Output = Result<Outcome, EngineError>> + Send;

    fn resume(
        &self,
        params: RunParams<'_>,
    ) -> impl std::future::Future<Output = Result<Outcome, EngineError>> + Send;

    fn session_artifacts(&self, session_dir: &Path) -> Value;
}
