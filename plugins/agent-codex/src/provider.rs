use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::mpsc, task::JoinHandle, time::Instant};
use uuid::Uuid;
use virtade_agent_core::{
    rpc::{ProcessConfig, RpcClient, RpcEvents},
    *,
};

pub const EVENT_TOPIC: &str = "virtade.agent.codex.event.v1";

#[derive(Debug, Clone)]
pub struct CodexConfig {
    pub process: ProcessConfig,
    pub request_timeout: Duration,
    pub approval_timeout: Duration,
}

impl CodexConfig {
    pub fn new(cwd: impl Into<std::path::PathBuf>) -> Self {
        let mut process = ProcessConfig::new("codex", cwd);
        process.args = vec!["app-server".into(), "--listen".into(), "stdio://".into()];
        Self {
            process,
            request_timeout: Duration::from_secs(30),
            approval_timeout: Duration::from_secs(120),
        }
    }
}

#[derive(Clone)]
enum Turn {
    Pending(String),
    Running(String),
    Unknown,
}
struct Approval {
    native_id: Value,
    session: String,
    task: String,
    expires: Instant,
}
#[derive(Default)]
struct Shared {
    sessions: HashMap<String, Option<Turn>>,
    finished: HashMap<String, String>,
    approvals: HashMap<String, Approval>,
}

pub struct CodexProvider {
    rpc: RpcClient,
    info: ProviderInfo,
    shared: Arc<Mutex<Shared>>,
    event_task: Mutex<Option<JoinHandle<()>>>,
    timeout: Duration,
}

impl CodexProvider {
    pub async fn connect(
        config: CodexConfig,
    ) -> Result<(Arc<Self>, mpsc::Receiver<Result<AgentEvent>>)> {
        let (rpc, events) = RpcClient::spawn(config.process)?;
        let initialized = rpc.request("initialize", json!({
            "clientInfo": {"name": "virtade", "title": "virtade", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": false}
        }), config.request_timeout).await;
        let initialized = match initialized {
            Ok(value) => value,
            Err(error) => {
                let _ = rpc.shutdown().await;
                return Err(error);
            }
        };
        let Some(version) = initialized["userAgent"].as_str() else {
            let _ = rpc.shutdown().await;
            return Err(AgentError::Protocol(
                "initialize result missing userAgent".into(),
            ));
        };
        rpc.notify("initialized", json!({})).await?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let info = ProviderInfo {
            id: "codex".into(),
            name: "OpenAI Codex".into(),
            native_version: version.into(),
            capabilities: BTreeMap::from([
                ("sessions.create".into(), Support::Supported),
                ("sessions.resume".into(), Support::Supported),
                ("tasks.stream".into(), Support::Supported),
                ("tasks.cancel".into(), Support::Supported),
                ("approvals.command_file.once".into(), Support::Supported),
                (
                    "approvals.permissions".into(),
                    Support::Unsupported(
                        "not mapped; native request receives explicit unsupported error".into(),
                    ),
                ),
                (
                    "sessions.list".into(),
                    Support::Unsupported("not exposed in this increment".into()),
                ),
            ]),
        };
        let (sender, receiver) = mpsc::channel(128);
        let event_task = tokio::spawn(event_loop(
            rpc.clone(),
            events,
            shared.clone(),
            info.clone(),
            sender,
            config.approval_timeout,
        ));
        Ok((
            Arc::new(Self {
                rpc,
                info,
                shared,
                event_task: Mutex::new(Some(event_task)),
                timeout: config.request_timeout,
            }),
            receiver,
        ))
    }

    fn session_params(&self, config: &SessionConfig) -> Result<Value> {
        if !config.cwd.is_absolute() || !config.cwd.is_dir() {
            return Err(AgentError::InvalidState(
                "cwd must be an existing absolute directory".into(),
            ));
        }
        let mut params =
            json!({"cwd": config.cwd, "approvalPolicy": "untrusted", "sandbox": "workspace-write"});
        for (key, value) in &config.provider_options {
            match key.as_str() {
                "model" if value.is_string() => params[key] = value.clone(),
                _ => return Err(AgentError::Unsupported(format!("codex option {key}"))),
            }
        }
        Ok(params)
    }

    fn accept_session(&self, response: Value, config: SessionConfig) -> Result<Session> {
        let id = response["thread"]["id"]
            .as_str()
            .ok_or_else(|| AgentError::Protocol("missing thread.id".into()))?
            .to_owned();
        self.shared
            .lock()
            .expect("state lock")
            .sessions
            .entry(id.clone())
            .or_insert(None);
        Ok(Session {
            native_id: id,
            cwd: config.cwd,
            reserved: false,
        })
    }
}

struct SubmissionGuard {
    shared: Arc<Mutex<Shared>>,
    session: String,
    token: String,
}
impl Drop for SubmissionGuard {
    fn drop(&mut self) {
        let mut shared = self.shared.lock().expect("state lock");
        if let Some(Some(Turn::Pending(token))) = shared.sessions.get(&self.session)
            && token == &self.token
        {
            shared
                .sessions
                .insert(self.session.clone(), Some(Turn::Unknown));
        }
    }
}

#[async_trait]
impl AgentProvider for CodexProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }
    async fn create_session(&self, config: SessionConfig) -> Result<Session> {
        let params = self.session_params(&config)?;
        let response = self
            .rpc
            .request("thread/start", params, self.timeout)
            .await?;
        self.accept_session(response, config)
    }
    async fn resume_session(&self, id: &str, config: SessionConfig) -> Result<Session> {
        let mut params = self.session_params(&config)?;
        params["threadId"] = json!(id);
        let response = self
            .rpc
            .request("thread/resume", params, self.timeout)
            .await?;
        if response["thread"]["id"].as_str() != Some(id) {
            return Err(AgentError::Protocol(
                "resume changed native session ID".into(),
            ));
        }
        self.accept_session(response, config)
    }
    async fn submit(&self, session_id: &str, text: &str) -> Result<Task> {
        if text.trim().is_empty() {
            return Err(AgentError::InvalidState("empty prompt".into()));
        }
        let token = Uuid::new_v4().to_string();
        {
            let mut shared = self.shared.lock().expect("state lock");
            let slot = shared
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| AgentError::InvalidState("session not created or resumed".into()))?;
            if slot.is_some() {
                return Err(AgentError::InvalidState(
                    "session has a running or unknown task; reconcile before resubmitting".into(),
                ));
            }
            *slot = Some(Turn::Pending(token.clone()));
        }
        let guard = SubmissionGuard {
            shared: self.shared.clone(),
            session: session_id.into(),
            token: token.clone(),
        };
        let response = self
            .rpc
            .request(
                "turn/start",
                json!({"threadId": session_id, "input": [{"type": "text", "text": text}]}),
                self.timeout,
            )
            .await;
        let response = match response {
            Ok(value) => value,
            Err(error) => return Err(error),
        };
        let task_id = response["turn"]["id"]
            .as_str()
            .ok_or_else(|| AgentError::Protocol("missing turn.id; task outcome unknown".into()))?
            .to_owned();
        {
            let mut shared = self.shared.lock().expect("state lock");
            if matches!(shared.sessions.get(session_id), Some(Some(Turn::Pending(current))) if current == &token)
            {
                shared
                    .sessions
                    .insert(session_id.into(), Some(Turn::Running(task_id.clone())));
            }
        }
        drop(guard);
        Ok(Task {
            native_id: task_id,
            session_id: session_id.into(),
        })
    }
    async fn cancel(&self, task: &Task) -> Result<()> {
        {
            let shared = self.shared.lock().expect("state lock");
            match shared.sessions.get(&task.session_id) {
                Some(Some(Turn::Running(id))) if id == &task.native_id => (),
                Some(None) if shared.finished.get(&task.session_id) == Some(&task.native_id) => {
                    return Ok(());
                } // Already terminal; idempotent cancellation.
                _ => {
                    return Err(AgentError::InvalidState(
                        "task does not match the active native turn".into(),
                    ));
                }
            }
        }
        self.rpc
            .request(
                "turn/interrupt",
                json!({"threadId": task.session_id, "turnId": task.native_id}),
                self.timeout,
            )
            .await?;
        Ok(()) // Do not set terminal until turn/completed arrives.
    }
    async fn respond_approval(&self, response: ApprovalResponse) -> Result<()> {
        let approval = {
            let mut shared = self.shared.lock().expect("state lock");
            let approval = shared.approvals.get(&response.approval_id).ok_or_else(|| {
                AgentError::InvalidState("approval expired or already resolved".into())
            })?;
            if approval.session != response.session_id || approval.task != response.task_id {
                return Err(AgentError::InvalidState(
                    "approval session/task mismatch".into(),
                ));
            }
            if approval.expires <= Instant::now() {
                return Err(AgentError::InvalidState("approval expired".into()));
            }
            shared
                .approvals
                .remove(&response.approval_id)
                .expect("checked approval")
        };
        let decision = match response.decision {
            ApprovalDecision::AllowOnce => "accept",
            ApprovalDecision::Deny => "decline",
        };
        self.rpc
            .respond(approval.native_id, Ok(json!({"decision": decision})))
            .await
    }
    async fn shutdown(&self) -> Result<()> {
        let result = self.rpc.shutdown().await;
        let task = self.event_task.lock().expect("task lock").take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        result
    }
}

impl Drop for CodexProvider {
    fn drop(&mut self) {
        self.rpc.close();
        if let Some(task) = self.event_task.lock().expect("task lock").take() {
            task.abort();
        }
    }
}

async fn event_loop(
    rpc: RpcClient,
    mut events: RpcEvents,
    shared: Arc<Mutex<Shared>>,
    info: ProviderInfo,
    sender: mpsc::Sender<Result<AgentEvent>>,
    approval_timeout: Duration,
) {
    let mut sequence = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    loop {
        let frame = tokio::select! {
            result = events.recv() => match result { Ok(frame) => frame, Err(error) => { let _ = sender.send(Err(error)).await; break; } },
            _ = sender.closed() => { rpc.close(); break; },
            _ = tick.tick() => {
                let expired = {
                    let mut state = shared.lock().expect("state lock");
                    let ids: Vec<_> = state.approvals.iter().filter(|(_, approval)| approval.expires <= Instant::now()).map(|(id, _)| id.clone()).collect();
                    ids.into_iter().filter_map(|id| state.approvals.remove(&id)).collect::<Vec<_>>()
                };
                for approval in expired { let _ = rpc.respond(approval.native_id, Ok(json!({"decision": "decline"}))).await; }
                continue;
            }
        };
        let method = frame["method"]
            .as_str()
            .expect("validated method")
            .to_owned();
        let payload = frame.get("params").cloned().unwrap_or(Value::Null);
        let session = payload["threadId"]
            .as_str()
            .or_else(|| payload["thread"]["id"].as_str())
            .map(str::to_owned);
        let task = payload["turnId"]
            .as_str()
            .or_else(|| payload["turn"]["id"].as_str())
            .map(str::to_owned);
        let mut approval_id = None;
        let kind = if frame.get("id").is_some() {
            if matches!(
                method.as_str(),
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
            ) {
                if let (Some(session), Some(task)) = (&session, &task) {
                    let id = Uuid::new_v4().to_string();
                    let mut state = shared.lock().expect("state lock");
                    if state.approvals.len() >= 128 {
                        drop(state);
                        rpc.close();
                        break;
                    }
                    state.approvals.insert(
                        id.clone(),
                        Approval {
                            native_id: frame["id"].clone(),
                            session: session.clone(),
                            task: task.clone(),
                            expires: Instant::now() + approval_timeout,
                        },
                    );
                    approval_id = Some(id);
                    EventKind::ApprovalRequested
                } else {
                    let _ = rpc
                        .respond(
                            frame["id"].clone(),
                            Err((-32602, "approval missing thread/turn identity")),
                        )
                        .await;
                    EventKind::ProviderEvent
                }
            } else {
                let _ = rpc
                    .respond(
                        frame["id"].clone(),
                        Err((-32601, "request unsupported by virtade")),
                    )
                    .await;
                EventKind::ProviderEvent
            }
        } else {
            match method.as_str() {
                "item/agentMessage/delta" => EventKind::MessageDelta,
                "turn/started" | "turn/completed" => {
                    if let (Some(session), Some(task)) = (&session, &task) {
                        let mut state = shared.lock().expect("state lock");
                        if let Some(slot) = state.sessions.get_mut(session) {
                            if method == "turn/started" {
                                *slot = Some(Turn::Running(task.clone()));
                            } else if matches!(slot, Some(Turn::Running(current)) if current == task)
                                || matches!(slot, Some(Turn::Pending(_)) | Some(Turn::Unknown))
                            {
                                *slot = None;
                            }
                        }
                        if method == "turn/completed" {
                            state.finished.insert(session.clone(), task.clone());
                            state.approvals.retain(|_, approval| {
                                approval.session != *session || approval.task != *task
                            });
                        }
                    }
                    EventKind::TaskState
                }
                "item/started" | "item/completed"
                    if matches!(
                        payload["item"]["type"].as_str(),
                        Some(
                            "commandExecution"
                                | "fileChange"
                                | "mcpToolCall"
                                | "dynamicToolCall"
                                | "webSearch"
                        )
                    ) =>
                {
                    EventKind::ToolState
                }
                _ => EventKind::ProviderEvent,
            }
        };
        sequence += 1;
        let outcome = if method == "turn/completed" {
            Some(match payload["turn"]["status"].as_str() {
                Some("completed") => TaskOutcome::Completed,
                Some("failed") => TaskOutcome::Failed,
                Some("interrupted") => TaskOutcome::Cancelled,
                _ => TaskOutcome::Unknown,
            })
        } else {
            None
        };
        let event = AgentEvent {
            schema_version: 1,
            provider: info.id.clone(),
            native_version: info.native_version.clone(),
            sequence,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            session_id: session,
            task_id: task,
            kind,
            native_method: method,
            payload,
            approval_id,
            outcome,
        };
        if let Err(error) = sender.try_send(Ok(event)) {
            rpc.close();
            if matches!(error, mpsc::error::TrySendError::Full(_)) {
                let _ = sender.send(Err(AgentError::Backpressure)).await;
            }
            break;
        }
    }
    shared.lock().expect("state lock").approvals.clear();
}
