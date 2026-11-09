use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use virt::{Context, Flow, Kernel, Plugin};
use virt_protocol::{Capability, Manifest, Result};
use virtade_agent_codex::{CodexConfig, CodexPlugin, CodexProvider, EVENT_TOPIC, service_key};
use virtade_agent_core::{rpc::ProcessConfig, *};

fn config() -> CodexConfig {
    CodexConfig {
        process: ProcessConfig::new(
            env!("CARGO_BIN_EXE_codex-protocol-fixture"),
            std::env::current_dir().unwrap(),
        ),
        request_timeout: Duration::from_secs(3),
        approval_timeout: Duration::from_secs(3),
    }
}
fn session_config() -> SessionConfig {
    SessionConfig {
        cwd: std::env::current_dir().unwrap(),
        provider_options: Default::default(),
    }
}
async fn approval(events: &mut tokio::sync::mpsc::Receiver<ResultEvent>) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if matches!(event.kind, EventKind::MessageDelta) {
                assert_eq!(event.payload["native_extra"], json!({"preserve":true}));
            }
            if matches!(event.kind, EventKind::ApprovalRequested) {
                return event;
            }
        }
    })
    .await
    .unwrap()
}
type ResultEvent = virtade_agent_core::Result<AgentEvent>;

#[tokio::test]
async fn native_contract_covers_session_stream_approval_and_terminal() {
    let (provider, mut events) = CodexProvider::connect(config()).await.unwrap();
    assert_eq!(provider.info().native_version, "protocol-fixture/1");
    let session = provider.create_session(session_config()).await.unwrap();
    assert_eq!(
        provider
            .resume_session(&session.native_id, session_config())
            .await
            .unwrap()
            .native_id,
        session.native_id
    );
    assert!(
        provider
            .resume_session("missing", session_config())
            .await
            .is_err()
    );
    let task = provider.submit(&session.native_id, "test").await.unwrap();
    assert!(
        provider
            .submit(&session.native_id, "duplicate")
            .await
            .is_err()
    );
    let request = approval(&mut events).await;
    let response = ApprovalResponse {
        approval_id: request.approval_id.unwrap(),
        session_id: session.native_id.clone(),
        task_id: task.native_id.clone(),
        decision: ApprovalDecision::AllowOnce,
    };
    let mut wrong = response.clone();
    wrong.task_id = "wrong".into();
    assert!(provider.respond_approval(wrong).await.is_err());
    provider.respond_approval(response.clone()).await.unwrap();
    assert!(provider.respond_approval(response).await.is_err());
    loop {
        let event = events.recv().await.unwrap().unwrap();
        if event.native_method == "fixture/approvalResult" {
            assert_eq!(event.payload["original"]["result"]["decision"], "accept");
        }
        if event.native_method == "turn/completed" {
            break;
        }
    }
    provider.shutdown().await.unwrap();
}

#[tokio::test]
async fn approval_expires_to_native_denial() {
    let mut config = config();
    config.approval_timeout = Duration::from_millis(100);
    let (provider, mut events) = CodexProvider::connect(config).await.unwrap();
    let session = provider.create_session(session_config()).await.unwrap();
    let task = provider.submit(&session.native_id, "test").await.unwrap();
    let request = approval(&mut events).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if event.native_method == "fixture/approvalResult" {
                assert_eq!(event.payload["original"]["result"]["decision"], "decline");
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        provider
            .respond_approval(ApprovalResponse {
                approval_id: request.approval_id.unwrap(),
                session_id: session.native_id,
                task_id: task.native_id,
                decision: ApprovalDecision::AllowOnce
            })
            .await
            .is_err()
    );
    provider.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_is_confirmed_by_native_terminal_event() {
    let (provider, mut events) = CodexProvider::connect(config()).await.unwrap();
    let session = provider.create_session(session_config()).await.unwrap();
    let task = provider.submit(&session.native_id, "test").await.unwrap();
    approval(&mut events).await;
    provider.cancel(&task).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if event.native_method == "turn/completed" {
                assert_eq!(event.payload["turn"]["status"], "interrupted");
                break;
            }
        }
    })
    .await
    .unwrap();
    provider.cancel(&task).await.unwrap();
    provider.shutdown().await.unwrap();
}

#[tokio::test]
async fn timeout_never_automatically_resubmits_unknown_task() {
    let (provider, _events) = CodexProvider::connect(config()).await.unwrap();
    let session = provider.create_session(session_config()).await.unwrap();
    assert!(matches!(
        provider.submit(&session.native_id, "unknown").await,
        Err(AgentError::Timeout(_))
    ));
    assert!(matches!(
        provider.submit(&session.native_id, "test").await,
        Err(AgentError::InvalidState(_))
    ));
    provider.shutdown().await.unwrap();
}

#[tokio::test]
async fn unsupported_options_fail_instead_of_being_silently_ignored() {
    let (provider, _events) = CodexProvider::connect(config()).await.unwrap();
    let mut session = session_config();
    session
        .provider_options
        .insert("unknown".into(), Value::Bool(true));
    assert!(matches!(
        provider.create_session(session).await,
        Err(AgentError::Unsupported(_))
    ));
    provider.shutdown().await.unwrap();
}

struct Consumer;
#[virt::async_trait]
impl Plugin for Consumer {
    fn manifest(&self) -> Manifest {
        let mut manifest = Manifest::new("consumer");
        manifest.permissions.extend([
            Capability::ServiceCall(service_key()),
            Capability::EventSubscribe(EVENT_TOPIC.into()),
        ]);
        manifest
    }
    async fn start(&self, ctx: Context) -> Result<()> {
        ctx.hook(EVENT_TOPIC, 0, |_| Ok(Flow::Continue))
    }
}

#[tokio::test]
async fn provider_is_a_virt_plugin_and_cannot_be_approved_via_public_service() {
    let mut kernel = Kernel::default();
    let plugin = Arc::new(CodexPlugin::new(config()));
    kernel
        .install(plugin.clone(), plugin.manifest().permissions)
        .unwrap();
    kernel
        .install(Arc::new(Consumer), Consumer.manifest().permissions)
        .unwrap();
    kernel.start_all().await.unwrap();
    let ctx = kernel.context("consumer").unwrap();
    let info = ctx
        .call(
            &service_key(),
            json!({"method":"info"}),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert_eq!(info["id"], "codex");
    assert!(
        ctx.call(
            &service_key(),
            json!({"method":"approval/respond"}),
            Duration::from_secs(2)
        )
        .await
        .is_err()
    );
    kernel.shutdown().await.unwrap();
    assert_eq!(kernel.resources().service_implementations, 0);
    assert_eq!(kernel.resources().tasks, 0);
}
