use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use virtade_agent_core::{
    AgentError,
    rpc::{ProcessConfig, RpcClient},
};

const TIMEOUT: Duration = Duration::from_secs(3);
fn config(mode: &str) -> ProcessConfig {
    let mut config = ProcessConfig::new(
        env!("CARGO_BIN_EXE_protocol-fixture"),
        std::env::current_dir().unwrap(),
    );
    config.args.push(mode.into());
    config
}

#[tokio::test]
async fn rpc_correlates_out_of_order_responses_and_native_errors() {
    let (rpc, _events) = RpcClient::spawn(config("rpc")).unwrap();
    let (slow, fast) = tokio::join!(
        rpc.request("delay", json!({"ms":30}), TIMEOUT),
        rpc.request("delay", json!({"ms":1}), TIMEOUT)
    );
    assert_eq!(slow.unwrap(), json!(30));
    assert_eq!(fast.unwrap(), json!(1));
    assert!(matches!(
        rpc.request("error", Value::Null, TIMEOUT).await,
        Err(AgentError::Remote { code: -123, .. })
    ));
    rpc.shutdown().await.unwrap();
}

#[tokio::test]
async fn request_deadline_does_not_break_following_requests() {
    let (rpc, _events) = RpcClient::spawn(config("rpc")).unwrap();
    assert!(matches!(
        rpc.request("never", Value::Null, Duration::from_millis(20))
            .await,
        Err(AgentError::Timeout(_))
    ));
    assert_eq!(
        rpc.request("echo", json!("still working"), TIMEOUT)
            .await
            .unwrap(),
        json!("still working")
    );
    rpc.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_requests_and_late_responses_are_discarded() {
    let (rpc, _events) = RpcClient::spawn(config("rpc")).unwrap();
    assert!(matches!(
        rpc.request("delay", json!({"ms":50}), Duration::from_millis(5))
            .await,
        Err(AgentError::Timeout(_))
    ));
    assert_eq!(
        rpc.request("delay", json!({"ms":70}), TIMEOUT)
            .await
            .unwrap(),
        json!(70)
    );
    rpc.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_oversized_and_truncated_frames_close_connection() {
    for mode in ["invalid", "oversized", "truncated"] {
        let mut config = config(mode);
        config.max_frame_bytes = 1024;
        let (rpc, mut events) = RpcClient::spawn(config).unwrap();
        assert!(matches!(
            tokio::time::timeout(TIMEOUT, events.recv()).await.unwrap(),
            Err(AgentError::Protocol(_))
        ));
        rpc.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn process_crash_unblocks_pending_requests() {
    let (rpc, _events) = RpcClient::spawn(config("rpc")).unwrap();
    assert!(matches!(
        rpc.request("crash", Value::Null, TIMEOUT).await,
        Err(AgentError::Closed(_))
    ));
    rpc.shutdown().await.unwrap();
}

#[tokio::test]
async fn slow_consumer_gets_explicit_backpressure_failure() {
    let mut config = config("flood");
    config.queue_capacity = 2;
    let (rpc, mut events) = RpcClient::spawn(config).unwrap();
    // A later request cannot complete before the flood has filled the event queue.
    let _ = rpc.request("echo", Value::Null, TIMEOUT).await;
    let terminal = loop {
        if let Err(error) = events.recv().await {
            break error;
        }
    };
    assert_eq!(terminal, AgentError::Backpressure);
    rpc.shutdown().await.unwrap();
}

#[tokio::test]
async fn stderr_is_drained_and_bounded_independently_of_stdout() {
    let (rpc, _events) = RpcClient::spawn(config("stderr")).unwrap();
    rpc.request("echo", Value::Null, TIMEOUT).await.unwrap();
    assert_eq!(rpc.stderr_tail().len(), 8192);
    rpc.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_env_and_argv_are_preserved_without_shell_interpretation() {
    let mut config = config("rpc");
    config.inherit_env.clear();
    config.env = BTreeMap::from([("FIXTURE_ALLOWED".into(), "allowed".into())]);
    config.args.push("a b; $(not-a-command)".into());
    let (rpc, _events) = RpcClient::spawn(config).unwrap();
    let result = rpc
        .request("environment", Value::Null, TIMEOUT)
        .await
        .unwrap();
    assert_eq!(result["allowed"], json!("allowed"));
    assert!(result["forbidden"].is_null());
    assert_eq!(result["args"][2], json!("a b; $(not-a-command)"));
    rpc.shutdown().await.unwrap();
}

#[tokio::test]
async fn process_tree_shutdown_stops_descendant_heartbeat() {
    let marker = std::env::temp_dir().join(format!("virtade-heartbeat-{}.txt", std::process::id()));
    let mut config = config("descendant-parent");
    config.args.push(marker.as_os_str().into());
    let (rpc, mut events) = RpcClient::spawn(config).unwrap();
    events.recv().await.unwrap();
    tokio::time::timeout(TIMEOUT, async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    rpc.shutdown().await.unwrap();
    let before = std::fs::read(&marker).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(std::fs::read(&marker).unwrap(), before);
    std::fs::remove_file(marker).unwrap();
}
