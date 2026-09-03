use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod pi;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub session_dir: PathBuf,
    pub status: String,
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
            status: "ok".to_string(),
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
    ) -> impl std::future::Future<Output = Result<Outcome, String>> + Send;

    fn resume(
        &self,
        params: RunParams<'_>,
    ) -> impl std::future::Future<Output = Result<Outcome, String>> + Send;

    fn session_artifacts(&self, session_dir: &Path) -> Value;
}
