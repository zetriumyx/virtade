use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;
use virtade_agent_core::AgentEvent;

pub type Result<T> = std::result::Result<T, StorageError>;
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem: {0}")]
    Io(#[from] std::io::Error),
    #[error("database is already owned by another virtade process")]
    Locked,
    #[error("database schema version {0} is newer than supported version 2")]
    FutureSchema(i64),
    #[error("storage is closed")]
    Closed,
    #[error("storage worker failed: {0}")]
    Worker(String),
    #[error("invalid storage operation: {0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("task outcome is unresolved: {0}")]
    Unresolved(String),
    #[error("request already recorded as task {0}; do not resubmit")]
    DuplicateRequest(String),
    #[error("invalid serialized event: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub root: PathBuf,
    pub name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub id: String,
    pub workspace_id: String,
    pub provider: String,
    pub native_id: String,
    pub native_version: String,
    pub reserved: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Submitting,
    Running,
    Unknown,
    Completed,
    Failed,
    Cancelled,
}
impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Submitting => "submitting",
            Self::Running => "running",
            Self::Unknown => "unknown",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    pub(crate) fn parse(value: &str) -> rusqlite::Result<Self> {
        match value {
            "submitting" => Ok(Self::Submitting),
            "running" => Ok(Self::Running),
            "unknown" => Ok(Self::Unknown),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTask {
    pub id: String,
    pub session_id: String,
    pub native_id: Option<String>,
    pub state: TaskState,
    pub request_key: String,
    pub prompt: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEvent {
    pub sequence: u64,
    pub task_id: Option<String>,
    pub event: AgentEvent,
}
