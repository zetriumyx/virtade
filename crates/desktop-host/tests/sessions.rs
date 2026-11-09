use async_trait::async_trait;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::sync::mpsc;
use virtade_agent_core::*;
use virtade_desktop_host::*;

struct FixtureConnector {
    providers: Mutex<Vec<Arc<FixtureProvider>>>,
}
struct FixtureProvider {
    native_session: String,
    info: ProviderInfo,
    events: mpsc::Sender<Result<AgentEvent>>,
    answers: AtomicUsize,
}
fn event(
    sequence: u64,
    kind: EventKind,
    token: Option<&str>,
    outcome: Option<TaskOutcome>,
) -> AgentEvent {
    AgentEvent {
        schema_version: 1,
        provider: "codex".into(),
        native_version: "fixture".into(),
        sequence,
        timestamp_ms: 1,
        session_id: Some("native-session".into()),
        task_id: Some("native-task".into()),
        kind,
        native_method: "fixture".into(),
        payload: json!({"command":"fixture"}),
        approval_id: token.map(String::from),
        outcome,
    }
}
#[async_trait]
impl Connector for FixtureConnector {
    async fn connect(&self, _: &str, _: PathBuf, _: ProviderSettings) -> Result<ConnectedProvider> {
        let (tx, rx) = mpsc::channel(16);
        let index = self.providers.lock().unwrap().len();
        let provider = Arc::new(FixtureProvider {
            native_session: if index == 0 {
                "native-session".into()
            } else {
                format!("native-session-{index}")
            },
            info: ProviderInfo {
                id: "codex".into(),
                name: "Fixture".into(),
                native_version: "fixture".into(),
                capabilities: Default::default(),
            },
            events: tx,
            answers: AtomicUsize::new(0),
        });
        self.providers.lock().unwrap().push(provider.clone());
        Ok((provider, rx))
    }
}
impl FixtureProvider {
    fn event(
        &self,
        sequence: u64,
        kind: EventKind,
        token: Option<&str>,
        outcome: Option<TaskOutcome>,
    ) -> AgentEvent {
        let mut value = event(sequence, kind, token, outcome);
        value.session_id = Some(self.native_session.clone());
        value
    }
}
#[async_trait]
impl AgentProvider for FixtureProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }
    async fn create_session(&self, config: SessionConfig) -> Result<Session> {
        Ok(Session {
            native_id: self.native_session.clone(),
            cwd: config.cwd,
            reserved: false,
        })
    }
    async fn resume_session(&self, id: &str, config: SessionConfig) -> Result<Session> {
        assert_eq!(id, self.native_session);
        self.create_session(config).await
    }
    async fn submit(&self, id: &str, _: &str) -> Result<Task> {
        self.events
            .send(Ok(self.event(
                1,
                EventKind::ApprovalRequested,
                Some("provider-token"),
                None,
            )))
            .await
            .unwrap();
        Ok(Task {
            native_id: "native-task".into(),
            session_id: id.into(),
        })
    }
    async fn cancel(&self, _: &Task) -> Result<()> {
        self.events
            .send(Ok(self.event(
                2,
                EventKind::TaskState,
                None,
                Some(TaskOutcome::Cancelled),
            )))
            .await
            .unwrap();
        Ok(())
    }
    async fn respond_approval(&self, response: ApprovalResponse) -> Result<()> {
        assert_eq!(response.approval_id, "provider-token");
        assert_eq!(response.session_id, self.native_session);
        self.answers.fetch_add(1, Ordering::SeqCst);
        self.events
            .send(Ok(self.event(
                2,
                EventKind::TaskState,
                None,
                Some(TaskOutcome::Completed),
            )))
            .await
            .unwrap();
        Ok(())
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
async fn setup() -> (TempDir, Arc<AppHost>, Arc<FixtureConnector>, String) {
    let temp = TempDir::new().unwrap();
    let connector = Arc::new(FixtureConnector {
        providers: Mutex::new(vec![]),
    });
    let host = AppHost::with_connector(temp.path().join("desktop.db"), connector.clone())
        .await
        .unwrap();
    let workspace = host
        .request(
            1,
            DesktopRequest::OpenWorkspace {
                root: temp.path().into(),
            },
        )
        .await
        .unwrap();
    let session = host
        .request(
            1,
            DesktopRequest::OpenSession {
                workspace_id: workspace["id"].as_str().unwrap().into(),
                provider: "codex".into(),
                resume: None,
            },
        )
        .await
        .unwrap();
    (
        temp,
        host,
        connector,
        session["id"].as_str().unwrap().into(),
    )
}
async fn until(host: &AppHost, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = host.snapshot().await.unwrap();
            if predicate(&snapshot) {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn human_approval_is_bound_single_use_and_terminal_is_saved_before_visible() {
    let (_temp, host, connector, id) = setup().await;
    // Spawning the command also verifies IPC futures are Send; std mutex guards may not cross await.
    let task_host = host.clone();
    let session_id = id.clone();
    tokio::spawn(async move {
        task_host
            .request(
                1,
                DesktopRequest::Submit {
                    session_id,
                    request_key: "request".into(),
                    text: "fixture prompt".into(),
                },
            )
            .await
    })
    .await
    .unwrap()
    .unwrap();
    let snapshot = until(&host, |s| !s.live[0].approvals.is_empty()).await;
    let token = snapshot.live[0].approvals[0].id.clone();
    assert_ne!(token, "provider-token");
    assert_eq!(
        connector.providers.lock().unwrap()[0]
            .answers
            .load(Ordering::SeqCst),
        0
    );
    assert!(
        host.request(
            1,
            DesktopRequest::Approve {
                session_id: "wrong".into(),
                approval_id: token.clone(),
                decision: ApprovalDecision::AllowOnce
            }
        )
        .await
        .is_err()
    );
    host.request(
        1,
        DesktopRequest::Approve {
            session_id: id.clone(),
            approval_id: token.clone(),
            decision: ApprovalDecision::AllowOnce,
        },
    )
    .await
    .unwrap();
    assert!(
        host.request(
            1,
            DesktopRequest::Approve {
                session_id: id.clone(),
                approval_id: token,
                decision: ApprovalDecision::AllowOnce
            }
        )
        .await
        .is_err()
    );
    until(&host, |s| s.live[0].active.is_none()).await;
    let history = host
        .request(
            1,
            DesktopRequest::History {
                session_id: id,
                after: 0,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(history["tasks"][0]["state"], "completed");
    assert_eq!(history["tasks"][0]["prompt"], "fixture prompt");
    assert_eq!(history["events"].as_array().unwrap().len(), 2);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn api_versions_and_settings_are_validated_and_persisted() {
    let (_temp, host, _, _) = setup().await;
    assert!(host.request(99, DesktopRequest::Snapshot).await.is_err());
    host.request(
        1,
        DesktopRequest::SaveSettings {
            settings: ProviderSettings {
                codex_executable: "/configured/codex".into(),
                ..Default::default()
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(
        host.snapshot().await.unwrap().settings.codex_executable,
        "/configured/codex"
    );
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn broken_stream_marks_active_task_unknown_and_expires_approvals() {
    let (_temp, host, connector, id) = setup().await;
    host.request(
        1,
        DesktopRequest::Submit {
            session_id: id.clone(),
            request_key: "request".into(),
            text: "fixture".into(),
        },
    )
    .await
    .unwrap();
    until(&host, |s| !s.live[0].approvals.is_empty()).await;
    let sender = connector.providers.lock().unwrap()[0].events.clone();
    sender
        .send(Err(AgentError::Closed("fixture crash".into())))
        .await
        .unwrap();
    until(&host, |s| !s.live[0].connected).await;
    let history = host
        .request(
            1,
            DesktopRequest::History {
                session_id: id,
                after: 0,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(history["tasks"][0]["state"], "unknown");
    assert!(host.snapshot().await.unwrap().live[0].approvals.is_empty());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn parallel_sessions_keep_approval_authority_and_cancellation_separate() {
    let (_temp, host, connector, first) = setup().await;
    let workspace_id = host.snapshot().await.unwrap().workspaces[0].id.clone();
    let second = host
        .request(
            1,
            DesktopRequest::OpenSession {
                workspace_id,
                provider: "codex".into(),
                resume: None,
            },
        )
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let submit = |session_id: String| {
        host.request(
            1,
            DesktopRequest::Submit {
                session_id,
                request_key: "same-request-key-per-session".into(),
                text: "fixture".into(),
            },
        )
    };
    let (a, b) = tokio::join!(submit(first.clone()), submit(second.clone()));
    a.unwrap();
    b.unwrap();
    let snapshot = until(&host, |s| {
        s.live.len() == 2 && s.live.iter().all(|l| !l.approvals.is_empty())
    })
    .await;
    let token = snapshot
        .live
        .iter()
        .find(|l| l.session_id == first)
        .unwrap()
        .approvals[0]
        .id
        .clone();
    assert!(
        host.request(
            1,
            DesktopRequest::Approve {
                session_id: second.clone(),
                approval_id: token.clone(),
                decision: ApprovalDecision::AllowOnce,
            }
        )
        .await
        .is_err()
    );
    host.request(
        1,
        DesktopRequest::Approve {
            session_id: first.clone(),
            approval_id: token,
            decision: ApprovalDecision::AllowOnce,
        },
    )
    .await
    .unwrap();
    host.request(
        1,
        DesktopRequest::Cancel {
            session_id: second.clone(),
        },
    )
    .await
    .unwrap();
    until(&host, |s| s.live.iter().all(|l| l.active.is_none())).await;
    assert_eq!(
        host.store.tasks(first).await.unwrap()[0].state,
        virtade_storage::TaskState::Completed
    );
    assert_eq!(
        host.store.tasks(second).await.unwrap()[0].state,
        virtade_storage::TaskState::Cancelled
    );
    assert_eq!(
        connector
            .providers
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.answers.load(Ordering::SeqCst))
            .sum::<usize>(),
        1
    );
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn isolated_workflow_needs_exact_agent_scope_and_cannot_call_private_workbench() {
    use virt_protocol::{Capability, Manifest, ServiceKey};
    let (temp, host, _connector, _id) = setup().await;
    let source = temp.path().join("plugin-source");
    std::fs::create_dir(&source).unwrap();
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/wasm-hello");
    let mut manifest: Manifest =
        serde_json::from_slice(&std::fs::read(example.join("manifest.json")).unwrap()).unwrap();
    let agent_key = ServiceKey::new("virtade.agents", 1);
    let private_key = ServiceKey::new("virtade.workbench.internal", 1);
    let root = temp.path().canonicalize().unwrap();
    manifest.permissions.extend([
        Capability::ServiceCall(agent_key.clone()),
        Capability::ServiceCall(private_key.clone()),
        Capability::AgentControl {
            provider: "codex".into(),
            root: root.to_str().unwrap().into(),
        },
    ]);
    std::fs::write(
        source.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::copy(
        example.join("component.wasm"),
        source.join("component.wasm"),
    )
    .unwrap();
    host.plugins
        .install(source, manifest.permissions)
        .await
        .unwrap();
    host.plugins.enable("hello").await.unwrap();
    let generation = host.plugins.snapshot().await[0].generation.clone().unwrap();
    assert!(
        host.plugins
            .call("hello", &generation, private_key, json!({}))
            .await
            .is_err()
    );
    assert!(
        host.plugins
            .call(
                "hello",
                &generation,
                agent_key.clone(),
                json!({"method":"open_session","params":{"provider":"claude","root":root}})
            )
            .await
            .is_err()
    );
    assert!(host.plugins.call("hello",&generation,agent_key.clone(),json!({"method":"open_session","params":{"provider":"codex","root":source_path(&root)}})).await.is_err());
    let session = host
        .plugins
        .call(
            "hello",
            &generation,
            agent_key.clone(),
            json!({"method":"open_session","params":{"provider":"codex","root":root}}),
        )
        .await
        .unwrap();
    assert_eq!(session["provider"], "codex");
    assert!(
        host.plugins
            .call(
                "hello",
                &generation,
                agent_key,
                json!({"method":"approve","params":{}})
            )
            .await
            .is_err()
    );
    host.shutdown().await.unwrap();
    fn source_path(root: &std::path::Path) -> PathBuf {
        root.join("plugin-source")
    }
}

#[tokio::test]
async fn exported_diagnostics_omit_prompts_paths_and_approval_tokens() {
    let (temp, host, _connector, id) = setup().await;
    host.request(
        1,
        DesktopRequest::SaveSettings {
            settings: ProviderSettings {
                codex_executable: "private-executable-path-marker".into(),
                ..Default::default()
            },
        },
    )
    .await
    .unwrap();
    host.request(
        1,
        DesktopRequest::Submit {
            session_id: id,
            request_key: "diagnostic-test".into(),
            text: "private-prompt-marker".into(),
        },
    )
    .await
    .unwrap();
    until(&host, |s| !s.live[0].approvals.is_empty()).await;
    let report = host.request(1, DesktopRequest::Diagnostics).await.unwrap();
    assert_eq!(report["schema_version"], 1);
    assert!(report["resources"]["services"].as_u64().unwrap() > 0);
    let text = serde_json::to_string(&report).unwrap();
    for private in [
        "private-executable-path-marker",
        "private-prompt-marker",
        "provider-token",
        temp.path().to_str().unwrap(),
    ] {
        assert!(!text.contains(private));
    }
    host.shutdown().await.unwrap();
}
