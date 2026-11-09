//! Durable workspace/session/task coordination independent of concrete providers.
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;
use uuid::Uuid;
use virtade_agent_core::{AgentError, AgentEvent, AgentProvider, SessionConfig, Task};
use virtade_storage::{StorageError, Store, StoredSession, StoredTask, Workspace};

pub type Result<T> = std::result::Result<T, WorkspaceError>;
#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("invalid workspace operation: {0}")]
    Invalid(String),
}

/// Owns metadata mappings, never a replacement Agent loop. Events must be persisted before UI delivery.
pub struct WorkspaceSession {
    store: Store,
    provider: Arc<dyn AgentProvider>,
    pub workspace: Workspace,
    pub session: StoredSession,
    stream_id: String,
}

impl WorkspaceSession {
    pub async fn open(
        store: Store,
        provider: Arc<dyn AgentProvider>,
        cwd: PathBuf,
        resume_local_id: Option<String>,
    ) -> Result<Self> {
        let name = cwd
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Workspace")
            .to_owned();
        let workspace = store.workspace(cwd, name).await?;
        let config = SessionConfig {
            cwd: workspace.root.clone(),
            provider_options: Default::default(),
        };
        let session = if let Some(id) = resume_local_id {
            let mut session = store.session(id).await?;
            if session.workspace_id != workspace.id || session.provider != provider.info().id {
                return Err(WorkspaceError::Invalid(
                    "stored session belongs to a different workspace/provider".into(),
                ));
            }
            let native = provider.resume_session(&session.native_id, config).await?;
            if native.native_id != session.native_id {
                return Err(WorkspaceError::Invalid(
                    "native resume returned a different session".into(),
                ));
            }
            if !native.reserved {
                store
                    .confirm_native_session(
                        session.id.clone(),
                        provider.info().native_version.clone(),
                    )
                    .await?;
                session.reserved = false;
                session.native_version = provider.info().native_version.clone();
            }
            session
        } else {
            let native = provider.create_session(config).await?;
            store
                .add_session(workspace.id.clone(), provider.info().clone(), native)
                .await?
        };
        Ok(Self {
            store,
            provider,
            workspace,
            session,
            stream_id: Uuid::new_v4().to_string(),
        })
    }

    /// The intent commits before native submission. A lost response cannot trigger automatic resubmission.
    pub async fn submit(&self, request_key: String, text: String) -> Result<(StoredTask, Task)> {
        let prepared = self
            .store
            .prepare_task(self.session.id.clone(), request_key, text.clone())
            .await?;
        let native = match self.provider.submit(&self.session.native_id, &text).await {
            Ok(task) => task,
            Err(error) => {
                self.store.mark_unknown(prepared.id).await?;
                return Err(error.into());
            }
        };
        if native.session_id != self.session.native_id {
            self.store.mark_unknown(prepared.id).await?;
            return Err(WorkspaceError::Invalid(
                "native task session mismatch".into(),
            ));
        }
        let task = self
            .store
            .acknowledge_task(prepared.id, native.native_id.clone())
            .await?;
        Ok((task, native))
    }

    pub async fn ingest(&self, event: AgentEvent) -> Result<u64> {
        Ok(self
            .store
            .append_event(self.session.id.clone(), self.stream_id.clone(), event)
            .await?)
    }

    pub async fn mark_unknown(&self, local_task_id: String) -> Result<()> {
        Ok(self.store.mark_unknown(local_task_id).await?)
    }
}
