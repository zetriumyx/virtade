//! SQLite metadata persistence, executed on a single dedicated bounded worker.
mod types;
pub use types::*;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
use virtade_agent_core::{AgentEvent, ProviderInfo, Session, TaskOutcome};

type Job = Box<dyn FnOnce(&mut Connection) + Send>;
enum Message {
    Run(Job),
    Close(oneshot::Sender<()>),
}
struct Handle {
    sender: mpsc::Sender<Message>,
    thread: Mutex<Option<JoinHandle<()>>>,
    recovered: usize,
}
#[derive(Clone)]
pub struct Store {
    handle: Arc<Handle>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
fn invalid(message: impl Into<String>) -> StorageError {
    StorageError::Invalid(message.into())
}
fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(String::from)
        .ok_or_else(|| invalid("non-Unicode paths are not supported by the current IPC schema"))
}
fn task(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredTask> {
    Ok(StoredTask {
        id: row.get(0)?,
        session_id: row.get(1)?,
        native_id: row.get(2)?,
        state: TaskState::parse(&row.get::<_, String>(3)?)?,
        request_key: row.get(4)?,
        prompt: row.get(5)?,
    })
}
fn session(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredSession> {
    Ok(StoredSession {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        provider: row.get(2)?,
        native_id: row.get(3)?,
        native_version: row.get(4)?,
        reserved: row.get(5)?,
    })
}

fn initialize(path: PathBuf) -> Result<(Connection, File, usize)> {
    let path = if path.exists() {
        path.canonicalize()?
    } else {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        parent.canonicalize()?.join(
            path.file_name()
                .ok_or_else(|| invalid("database path needs a file name"))?,
        )
    };
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => StorageError::Locked,
        std::fs::TryLockError::Error(error) => StorageError::Io(error),
    })?;
    if !path.exists() {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        drop(options.open(&path)?);
    }
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > 2 {
        return Err(StorageError::FutureSchema(version));
    }
    const APPLICATION_ID: i64 = 0x56525441;
    let application: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let tables: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if application != APPLICATION_ID && !(application == 0 && version == 0 && tables == 0) {
        return Err(invalid("database belongs to another application"));
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if version == 0 {
        transaction.execute_batch(include_str!("schema.sql"))?;
        transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    }
    if version < 2 {
        transaction.execute_batch("CREATE TABLE settings(namespace TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(namespace,key)); PRAGMA user_version=2;")?;
    }
    // The OS lock proves no live owner is running these tasks in another Store.
    let recovered = transaction.execute(
        "UPDATE tasks SET state='unknown', updated_ms=?1 WHERE state IN ('submitting','running')",
        [now()],
    )?;
    transaction.commit()?;
    Ok((conn, lock, recovered))
}

impl Store {
    pub async fn workspaces(&self) -> Result<Vec<Workspace>> {
        self.run(|conn| {
            let mut query =
                conn.prepare("SELECT id,root,name FROM workspaces ORDER BY created_ms,id")?;
            Ok(query
                .query_map([], |row| {
                    Ok(Workspace {
                        id: row.get(0)?,
                        root: PathBuf::from(row.get::<_, String>(1)?),
                        name: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
    }
    pub async fn get_setting(
        &self,
        namespace: String,
        key: String,
    ) -> Result<Option<serde_json::Value>> {
        self.run(move |conn| {
            let value: Option<String> = conn
                .query_row(
                    "SELECT value FROM settings WHERE namespace=?1 AND key=?2",
                    params![namespace, key],
                    |row| row.get(0),
                )
                .optional()?;
            value
                .map(|value| Ok(serde_json::from_str(&value)?))
                .transpose()
        })
        .await
    }
    pub async fn set_setting(
        &self,
        namespace: String,
        key: String,
        value: serde_json::Value,
    ) -> Result<()> {
        self.run(move|conn|{
            let value=serde_json::to_string(&value)?;
            if namespace.is_empty() || namespace.len()>200 || key.is_empty() || key.len()>200 || value.len()>1024*1024 {return Err(invalid("configuration key/value exceeds limits"));}
            let transaction=conn.transaction()?;
            let (count,total):(i64,i64)=transaction.query_row("SELECT count(*),coalesce(sum(length(CAST(value AS BLOB))),0) FROM settings WHERE namespace=?1 AND key<>?2",params![namespace,key],|row|Ok((row.get(0)?,row.get(1)?)))?;
            if count>=1000 || total+value.len() as i64>4*1024*1024{return Err(invalid("configuration namespace quota exceeded"));}
            transaction.execute("INSERT INTO settings(namespace,key,value) VALUES(?1,?2,?3) ON CONFLICT(namespace,key) DO UPDATE SET value=excluded.value",params![namespace,key,value])?;transaction.commit()?;Ok(())
        }).await
    }
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let (sender, mut receiver) = mpsc::channel(64);
        let (ready, initialized) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("virtade-sqlite".into())
            .spawn(move || {
                let (mut conn, _lock, recovered) = match initialize(path) {
                    Ok(result) => result,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(recovered)).is_err() {
                    return;
                }
                while let Some(message) = receiver.blocking_recv() {
                    match message {
                        Message::Run(job) => job(&mut conn),
                        Message::Close(closed) => {
                            receiver.close();
                            drop(conn);
                            let _ = closed.send(());
                            break;
                        }
                    }
                }
            })?;
        let recovered = initialized
            .await
            .map_err(|_| StorageError::Worker("initialization thread stopped".into()))??;
        Ok(Self {
            handle: Arc::new(Handle {
                sender,
                thread: Mutex::new(Some(thread)),
                recovered,
            }),
        })
    }

    /// Tasks converted to Unknown on this open; nothing is automatically replayed.
    pub fn recovered_tasks(&self) -> usize {
        self.handle.recovered
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (sender, receiver) = oneshot::channel();
        self.handle
            .sender
            .send(Message::Run(Box::new(move |conn| {
                let _ = sender.send(operation(conn));
            })))
            .await
            .map_err(|_| StorageError::Closed)?;
        receiver.await.map_err(|_| StorageError::Closed)?
    }

    pub async fn close(&self) -> Result<()> {
        let (sender, receiver) = oneshot::channel();
        if self
            .handle
            .sender
            .send(Message::Close(sender))
            .await
            .is_ok()
        {
            let _ = receiver.await;
        }
        let thread = self.handle.thread.lock().expect("worker lock").take();
        if let Some(thread) = thread {
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(|error| StorageError::Worker(error.to_string()))?
                .map_err(|_| StorageError::Worker("database worker panicked".into()))?;
        }
        Ok(())
    }

    pub async fn workspace(&self, root: PathBuf, name: String) -> Result<Workspace> {
        self.run(move |conn| {
            if name.trim().is_empty() || name.len() > 200 {
                return Err(invalid("workspace name must be 1..200 bytes"));
            }
            let root = root.canonicalize()?;
            if !root.is_dir() {
                return Err(invalid("workspace root is not a directory"));
            }
            let text = path_text(&root)?;
            let id: Option<String> = conn
                .query_row("SELECT id FROM workspaces WHERE root=?1", [&text], |row| {
                    row.get(0)
                })
                .optional()?;
            let id = match id {
                Some(id) => id,
                None => {
                    let id = Uuid::new_v4().to_string();
                    conn.execute(
                        "INSERT INTO workspaces(id,root,name,created_ms) VALUES(?1,?2,?3,?4)",
                        params![id, text, name, now()],
                    )?;
                    id
                }
            };
            let name = conn.query_row("SELECT name FROM workspaces WHERE id=?1", [&id], |row| {
                row.get(0)
            })?;
            Ok(Workspace { id, root, name })
        })
        .await
    }

    pub async fn add_session(
        &self,
        workspace_id: String,
        info: ProviderInfo,
        native: Session,
    ) -> Result<StoredSession> {
        self.run(move |conn| {
            if info.id.is_empty() || native.native_id.is_empty() { return Err(invalid("provider and native session ID are required")); }
            let root: String = conn.query_row("SELECT root FROM workspaces WHERE id=?1", [&workspace_id], |row| row.get(0)).optional()?.ok_or_else(|| StorageError::NotFound(workspace_id.clone()))?;
            if path_text(&native.cwd.canonicalize()?)? != root { return Err(invalid("native session cwd does not match workspace")); }
            let result = StoredSession { id: Uuid::new_v4().to_string(), workspace_id, provider: info.id, native_id: native.native_id, native_version: info.native_version, reserved: native.reserved };
            conn.execute("INSERT INTO sessions(id,workspace_id,provider,native_id,native_version,reserved,created_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![result.id,result.workspace_id,result.provider,result.native_id,result.native_version,result.reserved,now()])?;
            Ok(result)
        }).await
    }

    pub async fn session(&self, id: String) -> Result<StoredSession> {
        self.run(move |conn| conn.query_row("SELECT id,workspace_id,provider,native_id,native_version,reserved FROM sessions WHERE id=?1", [&id], session).optional()?.ok_or(StorageError::NotFound(id))).await
    }
    pub async fn confirm_native_session(&self, id: String, native_version: String) -> Result<()> {
        self.run(move |conn| {
            if conn.execute(
                "UPDATE sessions SET reserved=0,native_version=?1 WHERE id=?2",
                params![native_version, id],
            )? != 1
            {
                return Err(StorageError::NotFound(id));
            }
            Ok(())
        })
        .await
    }

    pub async fn sessions(&self, workspace_id: String) -> Result<Vec<StoredSession>> {
        self.run(move |conn| {
            let mut statement = conn.prepare("SELECT id,workspace_id,provider,native_id,native_version,reserved FROM sessions WHERE workspace_id=?1 ORDER BY created_ms,id")?;
            Ok(statement.query_map([workspace_id], session)?.collect::<rusqlite::Result<_>>()?)
        }).await
    }

    /// Must commit before calling the native provider. Duplicate keys never return permission to submit.
    pub async fn prepare_task(
        &self,
        session_id: String,
        request_key: String,
        prompt: String,
    ) -> Result<StoredTask> {
        self.run(move |conn| {
            if request_key.is_empty() || prompt.trim().is_empty() || prompt.len() > 1024*1024 { return Err(invalid("request key and bounded nonempty prompt required")); }
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(id) = tx.query_row("SELECT id FROM tasks WHERE session_id=?1 AND request_key=?2", params![session_id,request_key], |row| row.get(0)).optional()? { return Err(StorageError::DuplicateRequest(id)); }
            if let Some(id) = tx.query_row("SELECT id FROM tasks WHERE session_id=?1 AND state IN ('submitting','running','unknown')", [&session_id], |row| row.get(0)).optional()? { return Err(StorageError::Unresolved(id)); }
            let result = StoredTask { id: Uuid::new_v4().to_string(), session_id, native_id: None, state: TaskState::Submitting, request_key, prompt:prompt.clone() };
            tx.execute("INSERT INTO tasks(id,session_id,request_key,prompt,state,created_ms,updated_ms) VALUES(?1,?2,?3,?4,'submitting',?5,?5)", params![result.id,result.session_id,result.request_key,prompt,now()])?;
            tx.commit()?; Ok(result)
        }).await
    }

    pub async fn acknowledge_task(&self, id: String, native_id: String) -> Result<StoredTask> {
        self.run(move |conn| {
            if native_id.is_empty() { return Err(invalid("native task id required")); }
            if conn.execute("UPDATE tasks SET native_id=?1,state='running',updated_ms=?2 WHERE id=?3 AND state='submitting'", params![native_id,now(),id])? != 1 { return Err(invalid("task is not submitting")); }
            Ok(conn.query_row("SELECT id,session_id,native_id,state,request_key,prompt FROM tasks WHERE id=?1", [&id], task)?)
        }).await
    }

    pub async fn mark_unknown(&self, id: String) -> Result<()> {
        self.run(move |conn| { conn.execute("UPDATE tasks SET state='unknown',updated_ms=?1 WHERE id=?2 AND state IN ('submitting','running')", params![now(),id])?; Ok(()) }).await
    }

    /// Explicit recovery step: bind a native ID found in Agent history, without resolving Unknown.
    /// A matching native terminal event is still required to unblock the session.
    pub async fn bind_recovered_task(&self, id: String, native_id: String) -> Result<()> {
        self.run(move |conn| {
            if native_id.is_empty() { return Err(invalid("native task ID required")); }
            if conn.execute("UPDATE tasks SET native_id=?1,updated_ms=?2 WHERE id=?3 AND state='unknown' AND (native_id IS NULL OR native_id=?1)", params![native_id,now(),id])? != 1 {
                return Err(invalid("only Unknown tasks with matching/unset native IDs can be rebound"));
            }
            Ok(())
        }).await
    }
    pub async fn tasks(&self, session_id: String) -> Result<Vec<StoredTask>> {
        self.run(move |conn| {
            let mut statement = conn.prepare("SELECT id,session_id,native_id,state,request_key,prompt FROM tasks WHERE session_id=?1 ORDER BY created_ms,id")?;
            Ok(statement.query_map([session_id], task)?.collect::<rusqlite::Result<_>>()?)
        }).await
    }

    /// Append native data, advance the durable session cursor and apply terminal state atomically.
    pub async fn append_event(
        &self,
        session_id: String,
        stream_id: String,
        event: AgentEvent,
    ) -> Result<u64> {
        self.run(move |conn| {
            if event.schema_version != 1 || stream_id.is_empty() || event.sequence == 0 || event.sequence > i64::MAX as u64 { return Err(invalid("valid schema version/stream ID/sequence required")); }
            let payload = serde_json::to_string(&event)?;
            if payload.len() > 1024*1024 { return Err(invalid("event exceeds 1 MiB")); }
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (provider, native_session): (String,String) = tx.query_row("SELECT provider,native_id FROM sessions WHERE id=?1", [&session_id], |row| Ok((row.get(0)?,row.get(1)?)))?;
            if event.provider != provider || event.session_id.as_deref() != Some(&native_session) { return Err(invalid("event provider/session mismatch")); }
            let prior: Option<(i64,String)> = tx.query_row("SELECT sequence,payload FROM events WHERE session_id=?1 AND stream_id=?2 AND provider_sequence=?3", params![session_id,stream_id,event.sequence as i64], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
            if let Some((sequence, prior)) = prior { return if prior == payload { Ok(sequence as u64) } else { Err(invalid("conflicting duplicate event")) }; }
            let sequence: i64 = tx.query_row("SELECT COALESCE(MAX(sequence),0)+1 FROM events WHERE session_id=?1", [&session_id], |row| row.get(0))?;
            let task_id: Option<String> = match &event.task_id {
                Some(native) => tx.query_row("SELECT id FROM tasks WHERE session_id=?1 AND native_id=?2", params![session_id,native], |row| row.get(0)).optional()?, None => None,
            };
            if let Some(outcome) = event.outcome {
                let id = task_id.as_ref().ok_or_else(|| invalid("terminal event must match a known native task"))?;
                let state = match outcome { TaskOutcome::Completed => "completed", TaskOutcome::Failed => "failed", TaskOutcome::Cancelled => "cancelled", TaskOutcome::Unknown => "unknown" };
                let current: String = tx.query_row("SELECT state FROM tasks WHERE id=?1", [id], |row| row.get(0))?;
                if matches!(current.as_str(), "completed" | "failed" | "cancelled") && current != state { return Err(invalid("terminal state cannot change")); }
                tx.execute("UPDATE tasks SET state=?1,updated_ms=?2 WHERE id=?3", params![state,now(),id])?;
            }
            if event.native_method == "sdk/message" && event.payload["message"]["type"] == "system" && event.payload["message"]["subtype"] == "init" {
                tx.execute("UPDATE sessions SET reserved=0 WHERE id=?1", [&session_id])?;
            }
            tx.execute("INSERT INTO events(session_id,task_id,sequence,stream_id,provider_sequence,payload,created_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![session_id,task_id,sequence,stream_id,event.sequence as i64,payload,now()])?;
            tx.commit()?; Ok(sequence as u64)
        }).await
    }

    pub async fn events(
        &self,
        session_id: String,
        after: u64,
        limit: u32,
    ) -> Result<Vec<StoredEvent>> {
        self.run(move |conn| {
            if after > i64::MAX as u64 || limit == 0 || limit > 1000 { return Err(invalid("invalid cursor or page size (1..1000)")); }
            let mut statement = conn.prepare("SELECT sequence,task_id,payload FROM events WHERE session_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?;
            let rows = statement.query_map(params![session_id,after as i64,limit], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(|(sequence,task_id,payload)| Ok(StoredEvent { sequence: sequence as u64,task_id,event: serde_json::from_str(&payload)? })).collect()
        }).await
    }
}
