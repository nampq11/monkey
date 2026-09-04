use rusqlite::{Connection, params, types::Type};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("database connection lock is poisoned")]
    Poisoned,
    #[error("failed to create database directory: {0}")]
    CreateDbDir(#[source] std::io::Error),
    #[error("background database task failed: {0}")]
    BlockingTask(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Error)]
#[error("unknown event status: {0}")]
pub struct InvalidEventStatus(pub String);

/// Event lifecycle state. Stored in SQLite as lowercase text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventStatus {
    Pending,
    Running,
    Done,
    Error,
}

impl EventStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Error => "error",
        }
    }
}

impl std::str::FromStr for EventStatus {
    type Err = InvalidEventStatus;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "error" => Ok(Self::Error),
            other => Err(InvalidEventStatus(other.to_string())),
        }
    }
}

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    delivery_id   TEXT PRIMARY KEY,
    event_type    TEXT NOT NULL,
    owner         TEXT NOT NULL,
    repo          TEXT NOT NULL,
    number        INTEGER NOT NULL,
    payload       TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'pending',  -- pending|running|done|error
    session_dir   TEXT,
    created_at    REAL NOT NULL,
    updated_at    REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS tool_calls (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    owner      TEXT NOT NULL,
    repo       TEXT NOT NULL,
    number     INTEGER NOT NULL,
    tool       TEXT NOT NULL,
    args       TEXT NOT NULL,   -- credential-redacted
    result     TEXT NOT NULL,   -- credential-redacted
    created_at REAL NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_status ON events(status);
CREATE INDEX IF NOT EXISTS idx_events_owner_repo_num ON events(owner, repo, number);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub delivery_id: String,
    pub event_type: String,
    pub owner: String,
    pub repo: String,
    pub number: i64,
    pub payload: String,
    pub status: EventStatus,
    pub session_dir: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: i64,
    pub owner: String,
    pub repo: String,
    pub number: i64,
    pub tool: String,
    pub args: String,
    pub result: String,
    pub created_at: f64,
}

#[derive(Debug, Clone)]
pub struct Store {
    pub path: String,
    conn: Arc<Mutex<Connection>>,
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn row_to_event(row: &rusqlite::Row<'_>) -> SqlResult<Event> {
    let status_raw: String = row.get(6)?;
    let status = status_raw
        .parse::<EventStatus>()
        .map_err(|_| rusqlite::Error::InvalidColumnType(6, "status".into(), Type::Text))?;

    Ok(Event {
        delivery_id: row.get(0)?,
        event_type: row.get(1)?,
        owner: row.get(2)?,
        repo: row.get(3)?,
        number: row.get(4)?,
        payload: row.get(5)?,
        status,
        session_dir: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

use rusqlite::Result as SqlResult;

// SQLite calls are synchronous; async wrappers below push each operation onto
// the blocking thread pool so webhook handlers and the worker never stall a
// Tokio runtime worker while SQLite (or the WAL fsync) blocks.
async fn run_db_task<T, F>(task: F) -> StoreResult<T>
where
    F: FnOnce() -> StoreResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|error| StoreError::BlockingTask(error.to_string()))?
}

impl Store {
    pub fn new<P: AsRef<Path>>(path: P) -> StoreResult<Self> {
        let path_ref = path.as_ref();
        if let Some(parent) = path_ref.parent() {
            std::fs::create_dir_all(parent).map_err(StoreError::CreateDbDir)?;
        }
        let conn = Connection::open(path_ref)?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch("PRAGMA busy_timeout = 5000;")?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;

        Ok(Self {
            path: path_ref.to_string_lossy().to_string(),
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock_conn(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }

    pub async fn enqueue(
        &self,
        delivery_id: &str,
        event_type: &str,
        owner: &str,
        repo: &str,
        number: i64,
        payload: &str,
    ) -> StoreResult<bool> {
        let store = self.clone();
        let delivery_id = delivery_id.to_string();
        let event_type = event_type.to_string();
        let owner = owner.to_string();
        let repo = repo.to_string();
        let payload = payload.to_string();
        run_db_task(move || {
            store.enqueue_blocking(&delivery_id, &event_type, &owner, &repo, number, &payload)
        })
        .await
    }

    fn enqueue_blocking(
        &self,
        delivery_id: &str,
        event_type: &str,
        owner: &str,
        repo: &str,
        number: i64,
        payload: &str,
    ) -> StoreResult<bool> {
        let now = now_secs();
        let conn = self.lock_conn()?;
        let rowcount = conn.execute(
            "INSERT OR IGNORE INTO events \
             (delivery_id, event_type, owner, repo, number, payload, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                delivery_id,
                event_type,
                owner,
                repo,
                number,
                payload,
                now,
                now
            ],
        )?;
        Ok(rowcount > 0)
    }

    pub async fn pending_events(&self, limit: usize) -> StoreResult<Vec<Event>> {
        let store = self.clone();
        run_db_task(move || store.pending_events_blocking(limit)).await
    }

    fn pending_events_blocking(&self, limit: usize) -> StoreResult<Vec<Event>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT delivery_id, event_type, owner, repo, number, payload, status, session_dir, created_at, updated_at \
             FROM events WHERE status = ?1 ORDER BY created_at LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![EventStatus::Pending.as_str(), limit as i64],
            row_to_event,
        )?;

        Ok(rows.collect::<SqlResult<Vec<Event>>>()?)
    }

    pub async fn claim(&self, delivery_id: &str) -> StoreResult<bool> {
        let store = self.clone();
        let delivery_id = delivery_id.to_string();
        run_db_task(move || store.claim_blocking(&delivery_id)).await
    }

    fn claim_blocking(&self, delivery_id: &str) -> StoreResult<bool> {
        let now = now_secs();
        let conn = self.lock_conn()?;
        let rowcount = conn.execute(
            "UPDATE events SET status=?1, updated_at=?2 WHERE delivery_id=?3 AND status=?4",
            params![
                EventStatus::Running.as_str(),
                now,
                delivery_id,
                EventStatus::Pending.as_str()
            ],
        )?;
        Ok(rowcount > 0)
    }

    pub async fn done(&self, delivery_id: &str, session_dir: Option<&str>) -> StoreResult<()> {
        let store = self.clone();
        let delivery_id = delivery_id.to_string();
        let session_dir = session_dir.map(str::to_string);
        run_db_task(move || store.done_blocking(&delivery_id, session_dir.as_deref())).await
    }

    fn done_blocking(&self, delivery_id: &str, session_dir: Option<&str>) -> StoreResult<()> {
        let now = now_secs();
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE events SET status=?1, session_dir=?2, updated_at=?3 WHERE delivery_id=?4",
            params![EventStatus::Done.as_str(), session_dir, now, delivery_id],
        )?;
        Ok(())
    }

    pub async fn fail(&self, delivery_id: &str) -> StoreResult<()> {
        let store = self.clone();
        let delivery_id = delivery_id.to_string();
        run_db_task(move || store.fail_blocking(&delivery_id)).await
    }

    fn fail_blocking(&self, delivery_id: &str) -> StoreResult<()> {
        let now = now_secs();
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE events SET status=?1, updated_at=?2 WHERE delivery_id=?3",
            params![EventStatus::Error.as_str(), now, delivery_id],
        )?;
        Ok(())
    }

    pub async fn audit_tool_call(
        &self,
        owner: &str,
        repo: &str,
        number: i64,
        tool: &str,
        args: &str,
        result: &str,
    ) -> StoreResult<()> {
        let store = self.clone();
        let owner = owner.to_string();
        let repo = repo.to_string();
        let tool = tool.to_string();
        let args = args.to_string();
        let result = result.to_string();
        run_db_task(move || {
            store.audit_tool_call_blocking(&owner, &repo, number, &tool, &args, &result)
        })
        .await
    }

    fn audit_tool_call_blocking(
        &self,
        owner: &str,
        repo: &str,
        number: i64,
        tool: &str,
        args: &str,
        result: &str,
    ) -> StoreResult<()> {
        let now = now_secs();
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO tool_calls (owner, repo, number, tool, args, result, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![owner, repo, number, tool, args, result, now],
        )?;
        Ok(())
    }

    pub async fn latest_event_for_issue(
        &self,
        owner: &str,
        repo: &str,
        number: i64,
    ) -> StoreResult<Option<Event>> {
        let store = self.clone();
        let owner = owner.to_string();
        let repo = repo.to_string();
        run_db_task(move || store.latest_event_for_issue_blocking(&owner, &repo, number)).await
    }

    fn latest_event_for_issue_blocking(
        &self,
        owner: &str,
        repo: &str,
        number: i64,
    ) -> StoreResult<Option<Event>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT delivery_id, event_type, owner, repo, number, payload, status, session_dir, created_at, updated_at \
             FROM events WHERE owner=?1 AND repo=?2 AND number=?3 ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![owner, repo, number], row_to_event)?;
        Ok(rows.next().transpose()?)
    }

    pub async fn prior_session_dir(
        &self,
        owner: &str,
        repo: &str,
        number: i64,
        exclude_delivery_id: &str,
    ) -> StoreResult<Option<String>> {
        let store = self.clone();
        let owner = owner.to_string();
        let repo = repo.to_string();
        let exclude_delivery_id = exclude_delivery_id.to_string();
        run_db_task(move || {
            store.prior_session_dir_blocking(&owner, &repo, number, &exclude_delivery_id)
        })
        .await
    }

    fn prior_session_dir_blocking(
        &self,
        owner: &str,
        repo: &str,
        number: i64,
        exclude_delivery_id: &str,
    ) -> StoreResult<Option<String>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT session_dir FROM events \
             WHERE owner=?1 AND repo=?2 AND number=?3 AND delivery_id != ?4 \
             AND session_dir IS NOT NULL ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt
            .query_map(params![owner, repo, number, exclude_delivery_id], |row| {
                row.get(0)
            })?;
        Ok(rows.next().transpose()?)
    }

    pub async fn status_counts(&self) -> StoreResult<Vec<(String, i64)>> {
        let store = self.clone();
        run_db_task(move || store.status_counts_blocking()).await
    }

    fn status_counts_blocking(&self) -> StoreResult<Vec<(String, i64)>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare("SELECT status, count(*) AS n FROM events GROUP BY status")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;

        Ok(rows.collect::<SqlResult<Vec<(String, i64)>>>()?)
    }

    // Synchronous escape hatch for one-off access (CLI inspection, tests).
    pub fn with_conn<F, R>(&self, f: F) -> StoreResult<R>
    where
        F: FnOnce(&Connection) -> R,
    {
        let conn = self.lock_conn()?;
        Ok(f(&conn))
    }
}
