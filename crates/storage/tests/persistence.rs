use serde_json::json;
use std::path::PathBuf;
use tempfile::TempDir;
use virtade_agent_core::{AgentEvent, EventKind, ProviderInfo, Session, TaskOutcome};
use virtade_storage::*;

async fn setup() -> (TempDir, Store, Workspace, StoredSession) {
    let temp = TempDir::new().unwrap();
    let store = Store::open(temp.path().join("virtade.db")).await.unwrap();
    let workspace = store
        .workspace(temp.path().into(), "Fixture".into())
        .await
        .unwrap();
    let session = store
        .add_session(
            workspace.id.clone(),
            ProviderInfo {
                id: "codex".into(),
                name: "Fixture".into(),
                native_version: "fixture/1".into(),
                capabilities: Default::default(),
            },
            Session {
                native_id: "native-session".into(),
                cwd: workspace.root.clone(),
                reserved: false,
            },
        )
        .await
        .unwrap();
    (temp, store, workspace, session)
}
fn event(sequence: u64, outcome: Option<TaskOutcome>) -> AgentEvent {
    AgentEvent {
        schema_version: 1,
        provider: "codex".into(),
        native_version: "fixture/1".into(),
        sequence,
        timestamp_ms: 1,
        session_id: Some("native-session".into()),
        task_id: Some("native-task".into()),
        kind: EventKind::TaskState,
        native_method: "fixture/event".into(),
        payload: json!({"native_extra":"preserved"}),
        approval_id: None,
        outcome,
    }
}

#[tokio::test]
async fn durable_intents_native_ids_event_cursor_and_terminal_state() {
    let (temp, store, workspace, session) = setup().await;
    assert_eq!(
        store
            .workspace(temp.path().into(), "Other name".into())
            .await
            .unwrap()
            .id,
        workspace.id
    );
    let task = store
        .prepare_task(
            session.id.clone(),
            "request-1".into(),
            "fixture prompt".into(),
        )
        .await
        .unwrap();
    store
        .acknowledge_task(task.id, "native-task".into())
        .await
        .unwrap();
    assert_eq!(
        store
            .append_event(session.id.clone(), "stream-1".into(), event(1, None))
            .await
            .unwrap(),
        1
    );
    store
        .append_event(
            session.id.clone(),
            "stream-1".into(),
            event(2, Some(TaskOutcome::Completed)),
        )
        .await
        .unwrap();
    store.close().await.unwrap();
    let store = Store::open(temp.path().join("virtade.db")).await.unwrap();
    assert_eq!(store.recovered_tasks(), 0);
    assert_eq!(
        store.tasks(session.id.clone()).await.unwrap()[0].state,
        TaskState::Completed
    );
    let page = store.events(session.id.clone(), 1, 100).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].sequence, 2);
    assert_eq!(page[0].event.payload["native_extra"], "preserved");
    assert_eq!(
        store.sessions(workspace.id).await.unwrap()[0].native_id,
        "native-session"
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn restart_marks_incomplete_tasks_unknown_and_prevents_replay() {
    let (temp, store, _, session) = setup().await;
    store
        .prepare_task(session.id.clone(), "request".into(), "fixture".into())
        .await
        .unwrap();
    store.close().await.unwrap();
    let store = Store::open(temp.path().join("virtade.db")).await.unwrap();
    assert_eq!(store.recovered_tasks(), 1);
    assert_eq!(
        store.tasks(session.id.clone()).await.unwrap()[0].state,
        TaskState::Unknown
    );
    assert!(matches!(
        store
            .prepare_task(session.id.clone(), "request".into(), "duplicate".into())
            .await,
        Err(StorageError::DuplicateRequest(_))
    ));
    assert!(matches!(
        store
            .prepare_task(session.id, "other".into(), "no replay".into())
            .await,
        Err(StorageError::Unresolved(_))
    ));
    store.close().await.unwrap();
}

#[tokio::test]
async fn operating_system_lock_prevents_second_owner_and_false_recovery() {
    let (temp, store, _, _) = setup().await;
    assert!(matches!(
        Store::open(temp.path().join("virtade.db")).await,
        Err(StorageError::Locked)
    ));
    store.close().await.unwrap();
    let second = Store::open(temp.path().join("virtade.db")).await.unwrap();
    second.close().await.unwrap();
}

#[tokio::test]
async fn events_are_idempotent_and_conflicting_duplicates_roll_back() {
    let (_temp, store, _, session) = setup().await;
    let task = store
        .prepare_task(session.id.clone(), "req".into(), "fixture".into())
        .await
        .unwrap();
    store
        .acknowledge_task(task.id, "native-task".into())
        .await
        .unwrap();
    assert_eq!(
        store
            .append_event(session.id.clone(), "stream".into(), event(1, None))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .append_event(session.id.clone(), "stream".into(), event(1, None))
            .await
            .unwrap(),
        1
    );
    let mut conflicting = event(1, Some(TaskOutcome::Completed));
    conflicting.payload = json!({"different":true});
    assert!(
        store
            .append_event(session.id.clone(), "stream".into(), conflicting)
            .await
            .is_err()
    );
    assert_eq!(
        store.tasks(session.id.clone()).await.unwrap()[0].state,
        TaskState::Running
    );
    assert_eq!(
        store
            .events(session.id.clone(), 0, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .append_event(session.id, "new-connection".into(), event(1, None))
            .await
            .unwrap(),
        2
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn mismatched_event_identity_and_terminal_changes_are_rejected() {
    let (_temp, store, _, session) = setup().await;
    let task = store
        .prepare_task(session.id.clone(), "req".into(), "fixture".into())
        .await
        .unwrap();
    store
        .acknowledge_task(task.id, "native-task".into())
        .await
        .unwrap();
    let mut wrong = event(1, None);
    wrong.provider = "claude".into();
    assert!(
        store
            .append_event(session.id.clone(), "s".into(), wrong)
            .await
            .is_err()
    );
    let mut wrong = event(1, None);
    wrong.session_id = Some("another".into());
    assert!(
        store
            .append_event(session.id.clone(), "s".into(), wrong)
            .await
            .is_err()
    );
    store
        .append_event(
            session.id.clone(),
            "s".into(),
            event(1, Some(TaskOutcome::Completed)),
        )
        .await
        .unwrap();
    assert!(
        store
            .append_event(
                session.id.clone(),
                "s".into(),
                event(2, Some(TaskOutcome::Failed))
            )
            .await
            .is_err()
    );
    assert_eq!(store.events(session.id, 0, 100).await.unwrap().len(), 1);
    store.close().await.unwrap();
}

#[tokio::test]
async fn concurrent_prepares_have_only_one_winner() {
    let (_temp, store, _, session) = setup().await;
    let (a, b) = tokio::join!(
        store.prepare_task(session.id.clone(), "a".into(), "fixture".into()),
        store.prepare_task(session.id, "b".into(), "fixture".into())
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    store.close().await.unwrap();
}

#[tokio::test]
async fn future_schema_is_refused_without_downgrading() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("future.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);
    assert!(matches!(
        Store::open(&path).await,
        Err(StorageError::FutureSchema(99))
    ));
    let conn = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        99
    );
}

#[tokio::test]
async fn invalid_paths_and_closed_stores_fail_explicitly() {
    let (_temp, store, _, _) = setup().await;
    assert!(
        store
            .workspace(PathBuf::from("/not/a/virtade/workspace"), "invalid".into())
            .await
            .is_err()
    );
    store.close().await.unwrap();
    assert!(matches!(
        store.sessions("x".into()).await,
        Err(StorageError::Closed)
    ));
}

#[tokio::test]
async fn unrelated_sqlite_database_is_refused_without_mutating_its_schema() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("foreign.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE important(value TEXT); INSERT INTO important VALUES('preserve');",
    )
    .unwrap();
    drop(conn);
    assert!(matches!(
        Store::open(&path).await,
        Err(StorageError::Invalid(_))
    ));
    let conn = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        conn.query_row("SELECT value FROM important", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn unknown_requires_native_history_binding_and_terminal_evidence_to_resolve() {
    let (_temp, store, _, session) = setup().await;
    let task = store
        .prepare_task(session.id.clone(), "req".into(), "fixture".into())
        .await
        .unwrap();
    store.mark_unknown(task.id.clone()).await.unwrap();
    assert!(
        store
            .append_event(
                session.id.clone(),
                "recover".into(),
                event(1, Some(TaskOutcome::Completed))
            )
            .await
            .is_err()
    );
    store
        .bind_recovered_task(task.id.clone(), "native-task".into())
        .await
        .unwrap();
    assert_eq!(
        store.tasks(session.id.clone()).await.unwrap()[0].state,
        TaskState::Unknown
    );
    store
        .append_event(
            session.id.clone(),
            "recover".into(),
            event(1, Some(TaskOutcome::Completed)),
        )
        .await
        .unwrap();
    assert!(
        store
            .bind_recovered_task(task.id, "wrong-task".into())
            .await
            .is_err()
    );
    assert!(
        store
            .prepare_task(session.id, "follow-up".into(), "fixture".into())
            .await
            .is_ok()
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn settings_survive_reopen_and_namespace_byte_quota_is_enforced() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("settings.db");
    let store = Store::open(path.clone()).await.unwrap();
    let value = json!("x".repeat(1024 * 1024 - 10));
    for key in ["a", "b", "c", "d"] {
        store
            .set_setting("plugin:test".into(), key.into(), value.clone())
            .await
            .unwrap();
    }
    assert!(
        store
            .set_setting("plugin:test".into(), "e".into(), json!("x".repeat(100)))
            .await
            .is_err()
    );
    store
        .set_setting("plugin:test".into(), "a".into(), json!("small"))
        .await
        .unwrap();
    store
        .set_setting("plugin:other".into(), "e".into(), json!("independent"))
        .await
        .unwrap();
    store.close().await.unwrap();
    let store = Store::open(path).await.unwrap();
    assert_eq!(
        store
            .get_setting("plugin:test".into(), "a".into())
            .await
            .unwrap(),
        Some(json!("small"))
    );
    store.close().await.unwrap();
}
