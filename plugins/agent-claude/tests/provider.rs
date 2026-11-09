use serde_json::json;
use std::{sync::Arc, time::Duration};
use virt::{Context, Kernel, Plugin};
use virt_protocol::{Capability, Manifest};
use virtade_agent_claude::{ClaudeConfig, ClaudePlugin, ClaudeProvider, service_key};
use virtade_agent_core::{rpc::ProcessConfig, *};

fn config() -> ClaudeConfig {
    ClaudeConfig {
        process: ProcessConfig::new(
            env!("CARGO_BIN_EXE_claude-protocol-fixture"),
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
async fn approval(events: &mut tokio::sync::mpsc::Receiver<Result<AgentEvent>>) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if event.native_method == "sdk/message" && event.payload["message"]["type"] == "system"
            {
                assert_eq!(event.payload["message"]["extra"], "preserved");
            }
            if matches!(event.kind, EventKind::ApprovalRequested) {
                return event;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn reservations_native_resume_events_and_bound_one_time_approvals() {
    let (provider, mut events) = ClaudeProvider::connect(config()).await.unwrap();
    let session = provider.create_session(session_config()).await.unwrap();
    assert!(session.reserved);
    assert!(
        provider
            .resume_session("missing", session_config())
            .await
            .is_err()
    );
    assert!(
        !provider
            .resume_session(&session.native_id, session_config())
            .await
            .unwrap()
            .reserved
    );
    let task = provider
        .submit(&session.native_id, "fixture")
        .await
        .unwrap();
    let event = approval(&mut events).await;
    let response = ApprovalResponse {
        approval_id: event.approval_id.unwrap(),
        session_id: session.native_id.clone(),
        task_id: task.native_id,
        decision: ApprovalDecision::AllowOnce,
    };
    let mut wrong = response.clone();
    wrong.session_id = "wrong".into();
    assert!(provider.respond_approval(wrong).await.is_err());
    let mut wrong = response.clone();
    wrong.task_id = "wrong".into();
    assert!(provider.respond_approval(wrong).await.is_err());
    provider.respond_approval(response.clone()).await.unwrap();
    assert!(provider.respond_approval(response).await.is_err());
    loop {
        let event = events.recv().await.unwrap().unwrap();
        if event.outcome.is_some() {
            assert_eq!(event.outcome, Some(TaskOutcome::Completed));
            break;
        }
    }
    provider.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_without_native_result_remains_unknown_and_blocks_resubmission() {
    let (provider, mut events) = ClaudeProvider::connect(config()).await.unwrap();
    let session = provider.create_session(session_config()).await.unwrap();
    let task = provider
        .submit(&session.native_id, "fixture")
        .await
        .unwrap();
    approval(&mut events).await;
    provider.cancel(&task).await.unwrap();
    loop {
        let event = events.recv().await.unwrap().unwrap();
        if event.outcome.is_some() {
            assert_eq!(event.outcome, Some(TaskOutcome::Unknown));
            break;
        }
    }
    assert!(matches!(
        provider.submit(&session.native_id, "do not replay").await,
        Err(AgentError::InvalidState(_))
    ));
    provider.shutdown().await.unwrap();
}

#[tokio::test]
async fn approvals_expire_to_denial_even_without_host_response() {
    let mut config = config();
    config.approval_timeout = Duration::from_millis(50);
    let (provider, mut events) = ClaudeProvider::connect(config).await.unwrap();
    let session = provider.create_session(session_config()).await.unwrap();
    let task = provider
        .submit(&session.native_id, "fixture")
        .await
        .unwrap();
    let event = approval(&mut events).await;
    loop {
        let event = events.recv().await.unwrap().unwrap();
        if event.native_method == "fixture/decision" {
            assert_eq!(event.payload["original"]["result"]["decision"], "deny");
            break;
        }
    }
    assert!(
        provider
            .respond_approval(ApprovalResponse {
                approval_id: event.approval_id.unwrap(),
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
async fn bridge_exit_is_reported_and_never_resubmitted_automatically() {
    let (provider, _events) = ClaudeProvider::connect(config()).await.unwrap();
    let session = provider.create_session(session_config()).await.unwrap();
    assert!(matches!(
        provider.submit(&session.native_id, "crash").await,
        Err(AgentError::Closed(_))
    ));
    assert!(matches!(
        provider.submit(&session.native_id, "again").await,
        Err(AgentError::InvalidState(_))
    ));
    provider.shutdown().await.unwrap();
}

struct Consumer;
#[virt::async_trait]
impl Plugin for Consumer {
    fn manifest(&self) -> Manifest {
        let mut manifest = Manifest::new("consumer");
        manifest
            .permissions
            .insert(Capability::ServiceCall(service_key()));
        manifest
    }
    async fn start(&self, _: Context) -> virt_protocol::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn claude_uses_virt_service_lifecycle_and_keeps_approval_host_only() {
    let mut kernel = Kernel::default();
    let plugin = Arc::new(ClaudePlugin::new(config()));
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
    assert_eq!(info["id"], "claude");
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
