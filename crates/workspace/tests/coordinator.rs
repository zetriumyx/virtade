use async_trait::async_trait;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;
use virtade_agent_core::*;
use virtade_storage::{StorageError, Store, TaskState};
use virtade_workspace::{WorkspaceError, WorkspaceSession};

struct Provider {
    info: ProviderInfo,
    store: Store,
    submits: AtomicUsize,
    resumes: AtomicUsize,
    local_id: Mutex<Option<String>>,
}
#[async_trait]
impl AgentProvider for Provider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }
    async fn create_session(&self, config: SessionConfig) -> Result<Session> {
        Ok(Session {
            native_id: "native-session".into(),
            cwd: config.cwd,
            reserved: false,
        })
    }
    async fn resume_session(&self, id: &str, config: SessionConfig) -> Result<Session> {
        assert_eq!(id, "native-session");
        self.resumes.fetch_add(1, Ordering::SeqCst);
        self.create_session(config).await
    }
    async fn submit(&self, id: &str, text: &str) -> Result<Task> {
        let id_local = self
            .local_id
            .lock()
            .unwrap()
            .clone()
            .expect("local session set");
        let tasks = self
            .store
            .tasks(id_local)
            .await
            .map_err(|error| AgentError::Io(error.to_string()))?;
        assert_eq!(tasks.last().unwrap().state, TaskState::Submitting);
        self.submits.fetch_add(1, Ordering::SeqCst);
        if text == "error" {
            return Err(AgentError::Closed("synthetic submit failure".into()));
        }
        if text == "pending" {
            std::future::pending::<()>().await;
        }
        Ok(Task {
            native_id: format!("task-{}", self.submits.load(Ordering::SeqCst)),
            session_id: id.into(),
        })
    }
    async fn cancel(&self, _: &Task) -> Result<()> {
        Ok(())
    }
    async fn respond_approval(&self, _: ApprovalResponse) -> Result<()> {
        Ok(())
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

// Test provider errors are kept separate from storage errors in production.
fn provider(store: Store) -> Arc<Provider> {
    Arc::new(Provider {
        info: ProviderInfo {
            id: "codex".into(),
            name: "Fixture".into(),
            native_version: "fixture".into(),
            capabilities: Default::default(),
        },
        store,
        submits: AtomicUsize::new(0),
        resumes: AtomicUsize::new(0),
        local_id: Mutex::new(None),
    })
}
async fn setup() -> (TempDir, Store, Arc<Provider>, WorkspaceSession) {
    let temp = TempDir::new().unwrap();
    let store = Store::open(temp.path().join("virtade.db")).await.unwrap();
    let provider = provider(store.clone());
    let host = WorkspaceSession::open(store.clone(), provider.clone(), temp.path().into(), None)
        .await
        .unwrap();
    *provider.local_id.lock().unwrap() = Some(host.session.id.clone());
    (temp, store, provider, host)
}
fn terminal(native: &Task) -> AgentEvent {
    AgentEvent {
        schema_version: 1,
        provider: "codex".into(),
        native_version: "fixture".into(),
        sequence: 1,
        timestamp_ms: 1,
        session_id: Some(native.session_id.clone()),
        task_id: Some(native.native_id.clone()),
        kind: EventKind::TaskState,
        native_method: "turn/completed".into(),
        payload: serde_json::json!({"fixture":true}),
        approval_id: None,
        outcome: Some(TaskOutcome::Completed),
    }
}

#[tokio::test]
async fn submitting_is_committed_before_native_call_and_terminal_is_durable() {
    let (_temp, store, provider, host) = setup().await;
    let (task, native) = host
        .submit("request".into(), "fixture".into())
        .await
        .unwrap();
    assert_eq!(task.state, TaskState::Running);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
    assert!(
        host.submit("request".into(), "no replay".into())
            .await
            .is_err()
    );
    host.ingest(terminal(&native)).await.unwrap();
    assert_eq!(
        store.tasks(host.session.id.clone()).await.unwrap()[0].state,
        TaskState::Completed
    );
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
    store.close().await.unwrap();
}

#[tokio::test]
async fn provider_failure_leaves_unknown_and_blocks_second_native_submit() {
    let (_temp, store, provider, host) = setup().await;
    assert!(matches!(
        host.submit("request".into(), "error".into()).await,
        Err(WorkspaceError::Agent(_))
    ));
    assert_eq!(
        store.tasks(host.session.id.clone()).await.unwrap()[0].state,
        TaskState::Unknown
    );
    assert!(matches!(
        host.submit("other".into(), "fixture".into()).await,
        Err(WorkspaceError::Storage(StorageError::Unresolved(_)))
    ));
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
    store.close().await.unwrap();
}

#[tokio::test]
async fn restart_resumes_stored_native_id_without_resubmitting() {
    let (temp, store, _, host) = setup().await;
    let id = host.session.id.clone();
    store.close().await.unwrap();
    drop(host);
    let store = Store::open(temp.path().join("virtade.db")).await.unwrap();
    let provider = provider(store.clone());
    let resumed = WorkspaceSession::open(
        store.clone(),
        provider.clone(),
        temp.path().into(),
        Some(id.clone()),
    )
    .await
    .unwrap();
    assert_eq!(resumed.session.id, id);
    assert_eq!(provider.resumes.load(Ordering::SeqCst), 1);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
    store.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_submit_future_preserves_intent_and_never_replays() {
    let (_temp, store, provider, host) = setup().await;
    {
        let submit = host.submit("request".into(), "pending".into());
        tokio::pin!(submit);
        tokio::select! { _=&mut submit=>panic!("unexpected completion"), _=async { while provider.submits.load(Ordering::SeqCst)==0 { tokio::task::yield_now().await; } }=>() }
    }
    assert_eq!(
        store.tasks(host.session.id.clone()).await.unwrap()[0].state,
        TaskState::Submitting
    );
    assert!(host.submit("other".into(), "fixture".into()).await.is_err());
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
    store.close().await.unwrap();
}

#[tokio::test]
async fn foreign_workspace_resume_fails_before_native_resume() {
    let (_temp, store, provider, host) = setup().await;
    let other = TempDir::new().unwrap();
    assert!(
        WorkspaceSession::open(
            store.clone(),
            provider.clone(),
            PathBuf::from(other.path()),
            Some(host.session.id.clone())
        )
        .await
        .is_err()
    );
    assert_eq!(provider.resumes.load(Ordering::SeqCst), 0);
    store.close().await.unwrap();
}
