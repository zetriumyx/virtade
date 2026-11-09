//! Bounded newline-delimited JSON RPC with bidirectional requests and process-tree ownership.
use crate::{AgentError, Result};
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    ffi::OsString,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{Semaphore, mpsc, oneshot, watch},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ProcessConfig {
    /// Include the mandatory JSON-RPC 2.0 version field for ACP servers.
    pub json_rpc_2: bool,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// Explicit inherited environment keys. Other parent variables are not inherited.
    pub inherit_env: Vec<String>,
    pub env: BTreeMap<String, OsString>,
    pub max_frame_bytes: usize,
    pub queue_capacity: usize,
}

impl ProcessConfig {
    pub fn new(program: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            json_rpc_2: false,
            program: program.into(),
            cwd: cwd.into(),
            args: vec![],
            env: BTreeMap::new(),
            inherit_env: [
                "PATH",
                "HOME",
                "USERPROFILE",
                "APPDATA",
                "LOCALAPPDATA",
                "SystemRoot",
                "TEMP",
                "TMP",
                "LANG",
                "HTTPS_PROXY",
                "HTTP_PROXY",
                "NO_PROXY",
                "https_proxy",
                "http_proxy",
                "no_proxy",
                "SSL_CERT_FILE",
                "SSL_CERT_DIR",
                "NODE_EXTRA_CA_CERTS",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            max_frame_bytes: 1024 * 1024,
            queue_capacity: 128,
        }
    }
}

struct State {
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    next: AtomicU64,
    slots: Semaphore,
    stderr: Mutex<VecDeque<u8>>,
    closed: watch::Sender<Option<AgentError>>,
}

struct ConnectionGuard(CancellationToken);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[derive(Clone)]
pub struct RpcClient {
    state: Arc<State>,
    outgoing: mpsc::Sender<Value>,
    guard: Arc<ConnectionGuard>,
    max_frame: usize,
    json_rpc_2: bool,
}

pub struct RpcEvents {
    receiver: mpsc::Receiver<Value>,
    closed: watch::Receiver<Option<AgentError>>,
}

impl RpcEvents {
    /// Drain already accepted native events before surfacing terminal transport failure.
    pub async fn recv(&mut self) -> Result<Value> {
        if let Some(frame) = self.receiver.recv().await {
            return Ok(frame);
        }
        while self.closed.borrow().is_none() {
            if self.closed.changed().await.is_err() {
                break;
            }
        }
        Err(self
            .closed
            .borrow()
            .clone()
            .unwrap_or_else(|| AgentError::Closed("event stream ended".into())))
    }
}

struct PendingGuard {
    state: Arc<State>,
    id: u64,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.state
            .pending
            .lock()
            .expect("pending lock")
            .remove(&self.id);
    }
}

impl RpcClient {
    pub fn spawn(config: ProcessConfig) -> Result<(Self, RpcEvents)> {
        if config.queue_capacity == 0 || config.max_frame_bytes == 0 {
            return Err(AgentError::InvalidState(
                "queue and frame limits must be positive".into(),
            ));
        }
        let mut command = CommandWrap::with_new(&config.program, |command| {
            command
                .args(&config.args)
                .current_dir(&config.cwd)
                .env_clear();
            for key in &config.inherit_env {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
            command
                .envs(&config.env)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        });
        command.wrap(KillOnDrop);
        #[cfg(unix)]
        command.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(process_wrap::tokio::JobObject);
        let mut child = command.spawn()?;
        let stdin = child
            .stdin()
            .take()
            .ok_or_else(|| AgentError::Io("missing stdin".into()))?;
        let stdout = child
            .stdout()
            .take()
            .ok_or_else(|| AgentError::Io("missing stdout".into()))?;
        let stderr = child
            .stderr()
            .take()
            .ok_or_else(|| AgentError::Io("missing stderr".into()))?;
        let (outgoing, mut incoming) = mpsc::channel::<Value>(config.queue_capacity);
        let (events_tx, receiver) = mpsc::channel(config.queue_capacity);
        let (closed, closed_rx) = watch::channel(None);
        let state = Arc::new(State {
            pending: Mutex::default(),
            next: AtomicU64::new(1),
            slots: Semaphore::new(config.queue_capacity),
            stderr: Mutex::default(),
            closed,
        });
        let cancellation = CancellationToken::new();
        let client = Self {
            state: state.clone(),
            outgoing,
            guard: Arc::new(ConnectionGuard(cancellation.clone())),
            max_frame: config.max_frame_bytes,
            json_rpc_2: config.json_rpc_2,
        };
        let mut writer = tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(frame) = incoming.recv().await {
                let mut bytes = serde_json::to_vec(&frame)
                    .map_err(|error| AgentError::Protocol(error.to_string()))?;
                bytes.push(b'\n');
                stdin.write_all(&bytes).await?;
                stdin.flush().await?;
            }
            Ok::<(), AgentError>(())
        });
        let reader_state = state.clone();
        let max_frame = config.max_frame_bytes;
        let mut reader: tokio::task::JoinHandle<Result<()>> = tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout);
            loop {
                let bytes = read_frame(&mut stdout, max_frame).await?;
                let frame: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| AgentError::Protocol("stdout is not valid JSON".into()))?;
                validate_frame(&frame)?;
                if frame.get("method").is_none() {
                    if let Some(id) = frame.get("id").and_then(Value::as_u64)
                        && let Some(sender) = reader_state
                            .pending
                            .lock()
                            .expect("pending lock")
                            .remove(&id)
                    {
                        let _ = sender.send(Ok(frame));
                    }
                } else {
                    events_tx.try_send(frame).map_err(|error| match error {
                        mpsc::error::TrySendError::Full(_) => AgentError::Backpressure,
                        mpsc::error::TrySendError::Closed(_) => {
                            AgentError::Closed("event consumer dropped".into())
                        }
                    })?;
                }
            }
        });
        let diagnostics = state.clone();
        let mut stderr_task = tokio::spawn(async move {
            let mut stderr = stderr;
            let mut bytes = [0; 4096];
            loop {
                let size = stderr.read(&mut bytes).await?;
                if size == 0 {
                    break;
                }
                let mut tail = diagnostics.stderr.lock().expect("stderr lock");
                tail.extend(&bytes[..size]);
                while tail.len() > 8192 {
                    tail.pop_front();
                }
            }
            Ok::<(), std::io::Error>(())
        });
        tokio::spawn(async move {
            let reason = tokio::select! {
                biased;
                _ = cancellation.cancelled() => AgentError::Closed("shutdown requested".into()),
                result = &mut reader => match result {
                    Ok(Err(error)) => error,
                    _ => AgentError::Closed("stdout reader stopped".into()),
                },
                result = &mut writer => match result {
                    Ok(Err(error)) => error,
                    _ => AgentError::Closed("stdin writer stopped".into()),
                },
                result = child.wait() => {
                    // Preserve complete final frames already in the pipe before surfacing exit.
                    let exit = AgentError::Closed(match result { Ok(status) => format!("process exited: {status}"), Err(error) => error.to_string() });
                    match tokio::time::timeout(Duration::from_millis(100), &mut reader).await {
                        Ok(Ok(Err(error @ AgentError::Protocol(_)))) => error,
                        _ => exit,
                    }
                },
            };
            cancellation.cancel();
            // start_kill() targets the process group / Job Object, including descendants.
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            reader.abort();
            writer.abort();
            stderr_task.abort();
            // JoinHandle must not be polled twice if selected above.
            if !reader.is_finished() {
                let _ = reader.await;
            }
            if !writer.is_finished() {
                let _ = writer.await;
            }
            if !stderr_task.is_finished() {
                let _ = (&mut stderr_task).await;
            }
            for (_, sender) in state.pending.lock().expect("pending lock").drain() {
                let _ = sender.send(Err(reason.clone()));
            }
            state.closed.send_replace(Some(reason));
        });
        Ok((
            client,
            RpcEvents {
                receiver,
                closed: closed_rx,
            },
        ))
    }

    pub async fn request(&self, method: &str, params: Value, deadline: Duration) -> Result<Value> {
        self.request_inner(method, params, deadline, None).await
    }
    /// Signal that a long-running request has entered the ordered writer queue.
    /// A following cancel notification can then never overtake its request.
    pub async fn request_started(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
        started: oneshot::Sender<()>,
    ) -> Result<Value> {
        self.request_inner(method, params, deadline, Some(started))
            .await
    }
    async fn request_inner(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
        started: Option<oneshot::Sender<()>>,
    ) -> Result<Value> {
        let method = method.to_owned();
        tokio::time::timeout(deadline, async {
            let _slot = self
                .state
                .slots
                .acquire()
                .await
                .map_err(|_| AgentError::Closed("request slots closed".into()))?;
            let id = self.state.next.fetch_add(1, Ordering::Relaxed);
            let (sender, receiver) = oneshot::channel();
            self.state
                .pending
                .lock()
                .expect("pending lock")
                .insert(id, sender);
            let _pending = PendingGuard {
                state: self.state.clone(),
                id,
            };
            self.send(json!({"id": id, "method": method, "params": params}))
                .await?;
            if let Some(started) = started {
                let _ = started.send(());
            }
            let frame = receiver
                .await
                .map_err(|_| AgentError::Closed("response channel closed".into()))??;
            if let Some(error) = frame.get("error") {
                return Err(AgentError::Remote {
                    method: method.clone(),
                    code: error["code"].as_i64().unwrap_or(-32603),
                    message: error["message"].as_str().unwrap_or("native error").into(),
                });
            }
            Ok(frame["result"].clone())
        })
        .await
        .unwrap_or(Err(AgentError::Timeout(method)))
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.send(json!({"method": method, "params": params})),
        )
        .await
        .unwrap_or_else(|_| Err(AgentError::Timeout(method.into())))
    }

    pub async fn respond(
        &self,
        id: Value,
        result: std::result::Result<Value, (i64, &str)>,
    ) -> Result<()> {
        let frame = match result {
            Ok(result) => json!({"id": id, "result": result}),
            Err((code, message)) => json!({"id": id, "error": {"code": code, "message": message}}),
        };
        tokio::time::timeout(Duration::from_secs(5), self.send(frame))
            .await
            .unwrap_or_else(|_| Err(AgentError::Timeout("approval response".into())))
    }

    async fn send(&self, mut frame: Value) -> Result<()> {
        if self.json_rpc_2 {
            frame["jsonrpc"] = json!("2.0");
        }
        if let Some(reason) = self.state.closed.borrow().clone() {
            return Err(reason);
        }
        if serde_json::to_vec(&frame)
            .map_err(|error| AgentError::Protocol(error.to_string()))?
            .len()
            > self.max_frame
        {
            return Err(AgentError::Protocol("outbound frame exceeds limit".into()));
        }
        tokio::select! {
            _ = self.guard.0.cancelled() => Err(AgentError::Closed("shutdown requested".into())),
            result = self.outgoing.send(frame) => result.map_err(|_| AgentError::Closed("writer unavailable".into())),
        }
    }

    /// Sensitive diagnostic data; never log automatically.
    pub fn stderr_tail(&self) -> Vec<u8> {
        self.state
            .stderr
            .lock()
            .expect("stderr lock")
            .iter()
            .copied()
            .collect()
    }

    pub fn close(&self) {
        self.guard.0.cancel();
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.close();
        let mut closed = self.state.closed.subscribe();
        tokio::time::timeout(Duration::from_secs(6), async {
            while closed.borrow().is_none() {
                closed
                    .changed()
                    .await
                    .map_err(|_| AgentError::Closed("supervisor stopped".into()))?;
            }
            Ok(())
        })
        .await
        .unwrap_or_else(|_| Err(AgentError::Timeout("process shutdown".into())))
    }
}

pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    max: usize,
) -> Result<Vec<u8>> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Err(if frame.is_empty() {
                AgentError::Closed("stdout EOF".into())
            } else {
                AgentError::Protocol("truncated frame".into())
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let size = newline.map_or(available.len(), |index| index + 1);
        if frame.len() + size > max {
            return Err(AgentError::Protocol("inbound frame exceeds limit".into()));
        }
        frame.extend_from_slice(&available[..size]);
        reader.consume(size);
        if newline.is_some() {
            return Ok(frame);
        }
    }
}

fn validate_frame(frame: &Value) -> Result<()> {
    let valid_id = |id: &Value| id.is_string() || id.as_u64().is_some();
    let valid = frame.as_object().is_some_and(|map| {
        if let Some(method) = map.get("method") {
            method.as_str().is_some_and(|method| !method.is_empty())
                && !map.contains_key("result")
                && !map.contains_key("error")
                && map.get("id").is_none_or(valid_id)
        } else {
            map.get("id").is_some_and(valid_id)
                && (map.contains_key("result") ^ map.contains_key("error"))
        }
    });
    if valid {
        Ok(())
    } else {
        Err(AgentError::Protocol("invalid RPC envelope".into()))
    }
}
