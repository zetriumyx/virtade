use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, AgentError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AgentError {
    #[error("I/O: {0}")]
    Io(String),
    #[error("invalid protocol frame: {0}")]
    Protocol(String),
    #[error("native request {method} failed ({code}): {message}")]
    Remote {
        method: String,
        code: i64,
        message: String,
    },
    #[error("deadline exceeded; native outcome may be unknown: {0}")]
    Timeout(String),
    #[error("provider closed: {0}")]
    Closed(String),
    #[error("bounded event queue is full; connection terminated")]
    Backpressure,
    #[error("unsupported capability: {0}")]
    Unsupported(String),
    #[error("invalid operation: {0}")]
    InvalidState(String),
}
impl From<std::io::Error> for AgentError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "support", content = "reason", rename_all = "snake_case")]
pub enum Support {
    Supported,
    Experimental(String),
    Unsupported(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub native_version: String,
    pub capabilities: BTreeMap<String, Support>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    pub cwd: PathBuf,
    /// Only options explicitly accepted by the concrete provider are allowed.
    #[serde(default)]
    pub provider_options: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub native_id: String,
    pub cwd: PathBuf,
    /// True until Claude's first query creates its native transcript.
    #[serde(default)]
    pub reserved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Provider-opaque ID. Codex returns its native turn ID; Claude returns a bridge query UUID,
    /// which is not a vendor-resumable turn ID. Consult ProviderInfo capabilities.
    pub native_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    MessageDelta,
    TaskState,
    ToolState,
    ApprovalRequested,
    ProviderEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

/// Lossless native data plus a small normalized projection. Sequence is connection-local.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub schema_version: u32,
    pub provider: String,
    pub native_version: String,
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub kind: EventKind,
    pub native_method: String,
    pub payload: Value,
    /// Opaque host-issued approval token, never the native request ID.
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TaskOutcome>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalResponse {
    pub approval_id: String,
    pub session_id: String,
    pub task_id: String,
    pub decision: ApprovalDecision,
}

/// Host control only. Stream ownership is returned separately by concrete connect().
#[async_trait]
pub trait AgentProvider: Send + Sync {
    fn info(&self) -> &ProviderInfo;
    async fn create_session(&self, config: SessionConfig) -> Result<Session>;
    async fn resume_session(&self, id: &str, config: SessionConfig) -> Result<Session>;
    async fn submit(&self, session_id: &str, text: &str) -> Result<Task>;
    /// This acknowledges a cancellation request; only a native terminal event confirms it.
    async fn cancel(&self, task: &Task) -> Result<()>;
    async fn respond_approval(&self, response: ApprovalResponse) -> Result<()>;
    async fn shutdown(&self) -> Result<()>;
}
