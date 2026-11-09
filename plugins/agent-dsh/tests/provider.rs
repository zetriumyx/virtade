use std::{collections::BTreeMap, time::Duration};
use virtade_agent_core::{rpc::ProcessConfig, *};
use virtade_agent_dsh::DshProvider;
async fn connect() -> (
    tempfile::TempDir,
    std::sync::Arc<DshProvider>,
    tokio::sync::mpsc::Receiver<Result<AgentEvent>>,
) {
    let root = tempfile::TempDir::new().unwrap();
    let (provider, events) = DshProvider::connect(ProcessConfig::new(
        env!("CARGO_BIN_EXE_dsh-protocol-fixture"),
        root.path(),
    ))
    .await
    .unwrap();
    (root, provider, events)
}
#[tokio::test]
async fn native_acp_permissions_are_bound_single_use_and_updates_precede_terminal() {
    let (root, provider, mut events) = connect().await;
    let session = provider
        .create_session(SessionConfig {
            cwd: root.path().into(),
            provider_options: BTreeMap::new(),
        })
        .await
        .unwrap();
    let task = provider
        .submit(&session.native_id, "fixture")
        .await
        .unwrap();
    assert!(
        provider
            .submit(&session.native_id, "overlap")
            .await
            .is_err()
    );
    let first = events.recv().await.unwrap().unwrap();
    assert!(matches!(first.kind, EventKind::MessageDelta));
    let approval = events.recv().await.unwrap().unwrap();
    let token = approval.approval_id.unwrap();
    assert_ne!(token, "permission-1");
    let response = ApprovalResponse {
        approval_id: token,
        session_id: session.native_id.clone(),
        task_id: task.native_id.clone(),
        decision: ApprovalDecision::AllowOnce,
    };
    let mut wrong = response.clone();
    wrong.task_id = "wrong".into();
    assert!(provider.respond_approval(wrong).await.is_err());
    provider.respond_approval(response.clone()).await.unwrap();
    assert!(provider.respond_approval(response).await.is_err());
    let terminal = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(terminal.outcome, Some(TaskOutcome::Completed));
    assert!(terminal.sequence > approval.sequence && approval.sequence > first.sequence);
    provider.shutdown().await.unwrap();
}
#[tokio::test]
async fn native_acp_resume_and_cancel_preserve_session() {
    let (root, provider, mut events) = connect().await;
    let session = provider
        .resume_session(
            "native-session",
            SessionConfig {
                cwd: root.path().into(),
                provider_options: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
    let task = provider
        .submit(&session.native_id, "fixture")
        .await
        .unwrap();
    provider.cancel(&task).await.unwrap();
    let terminal = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if event.outcome.is_some() {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(terminal.outcome, Some(TaskOutcome::Cancelled));
    provider.shutdown().await.unwrap();
}
