use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::mpsc, task::JoinHandle, time::Instant};
use uuid::Uuid;
use virtade_agent_core::{
    rpc::{ProcessConfig, RpcClient, RpcEvents},
    *,
};

pub const EVENT_TOPIC: &str = "virtade.agent.claude.event.v1";

#[derive(Clone, Debug)]
pub struct ClaudeConfig {
    pub process: ProcessConfig,
    pub request_timeout: Duration,
    pub approval_timeout: Duration,
}
impl ClaudeConfig {
    /// Pass an absolute built bridge entry point; executable and env are host-controlled.
    pub fn new(cwd: impl Into<PathBuf>, bridge: impl Into<PathBuf>) -> Self {
        let mut process = ProcessConfig::new("node", cwd);
        process.args.push(bridge.into().into_os_string());
        process.inherit_env.extend(
            [
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_BASE_URL",
                "CLAUDE_CONFIG_DIR",
                "VIRTADE_CLAUDE_EXECUTABLE",
            ]
            .map(String::from),
        );
        Self {
            process,
            request_timeout: Duration::from_secs(30),
            approval_timeout: Duration::from_secs(110),
        }
    }
}

enum Turn {
    Pending(String),
    Running(String),
    Unknown,
}
struct Approval {
    id: Value,
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

pub struct ClaudeProvider {
    rpc: RpcClient,
    info: ProviderInfo,
    shared: Arc<Mutex<Shared>>,
    event_task: Mutex<Option<JoinHandle<()>>>,
    timeout: Duration,
}

impl ClaudeProvider {
    pub async fn connect(
        config: ClaudeConfig,
    ) -> Result<(Arc<Self>, mpsc::Receiver<Result<AgentEvent>>)> {
        let (rpc, raw) = RpcClient::spawn(config.process)?;
        let init = rpc
            .request("initialize", json!({}), config.request_timeout)
            .await;
        let init = match init {
            Ok(value) => value,
            Err(error) => {
                let _ = rpc.shutdown().await;
                return Err(error);
            }
        };
        let required = [
            "session.reserve",
            "session.resume",
            "task.stream",
            "task.interrupt",
            "tool.approval.once",
        ];
        if init["protocol_version"] != json!(1)
            || !init["sdk_version"].is_string()
            || required.iter().any(|cap| {
                !init["capabilities"]
                    .as_array()
                    .is_some_and(|caps| caps.contains(&json!(cap)))
            })
        {
            let _ = rpc.shutdown().await;
            return Err(AgentError::Protocol(
                "incompatible Claude bridge protocol/capabilities".into(),
            ));
        }
        let info = ProviderInfo {
            id: "claude".into(),
            name: "Claude Code".into(),
            native_version: format!(
                "claude-agent-sdk/{}",
                init["sdk_version"].as_str().expect("validated version")
            ),
            capabilities: BTreeMap::from([
                (
                    "sessions.create".into(),
                    Support::Experimental("UUID reserved until first native query".into()),
                ),
                ("sessions.resume".into(), Support::Supported),
                ("tasks.stream".into(), Support::Supported),
                (
                    "tasks.resume".into(),
                    Support::Unsupported(
                        "query UUIDs identify bridge operations, not vendor-resumable turns".into(),
                    ),
                ),
                (
                    "tasks.cancel".into(),
                    Support::Experimental(
                        "interrupt acknowledged; missing native result remains unknown".into(),
                    ),
                ),
                ("approvals.tool.once".into(), Support::Supported),
                (
                    "sessions.list".into(),
                    Support::Unsupported("use durable host session index".into()),
                ),
            ]),
        };
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (sender, receiver) = mpsc::channel(128);
        let event_task = tokio::spawn(event_loop(
            rpc.clone(),
            raw,
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

    fn params(&self, config: &SessionConfig) -> Result<Value> {
        if !config.cwd.is_absolute() || !config.cwd.is_dir() {
            return Err(AgentError::InvalidState(
                "cwd must be an existing absolute directory".into(),
            ));
        }
        for (key, value) in &config.provider_options {
            if key != "model" || !value.is_string() {
                return Err(AgentError::Unsupported(format!("claude option {key}")));
            }
        }
        Ok(json!({"cwd": config.cwd, "provider_options": config.provider_options}))
    }

    fn session(
        &self,
        value: Value,
        config: SessionConfig,
        expected: Option<&str>,
    ) -> Result<Session> {
        let id = value["session_id"]
            .as_str()
            .ok_or_else(|| AgentError::Protocol("missing session_id".into()))?
            .to_owned();
        let reserved = value["reserved"]
            .as_bool()
            .ok_or_else(|| AgentError::Protocol("missing session reservation state".into()))?;
        if expected.is_some_and(|expected| expected != id) || (expected.is_some() && reserved) {
            return Err(AgentError::Protocol(
                "resume did not return the requested native session".into(),
            ));
        }
        {
            let mut shared = self.shared.lock().expect("state lock");
            if matches!(
                shared.sessions.get(&id),
                Some(Some(Turn::Pending(_) | Turn::Running(_)))
            ) {
                return Err(AgentError::InvalidState(
                    "session became busy during resume".into(),
                ));
            }
            if expected.is_none() && shared.sessions.contains_key(&id) {
                return Err(AgentError::Protocol(
                    "new session reused an existing native ID".into(),
                ));
            }
            shared.sessions.insert(id.clone(), None);
        }
        Ok(Session {
            native_id: id,
            cwd: config.cwd,
            reserved,
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
        if matches!(shared.sessions.get(&self.session), Some(Some(Turn::Pending(token))) if token == &self.token)
        {
            shared
                .sessions
                .insert(self.session.clone(), Some(Turn::Unknown));
        }
    }
}

#[async_trait]
impl AgentProvider for ClaudeProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }
    async fn create_session(&self, config: SessionConfig) -> Result<Session> {
        let value = self
            .rpc
            .request("session/create", self.params(&config)?, self.timeout)
            .await?;
        self.session(value, config, None)
    }
    async fn resume_session(&self, id: &str, config: SessionConfig) -> Result<Session> {
        {
            let shared = self.shared.lock().expect("state lock");
            if matches!(
                shared.sessions.get(id),
                Some(Some(Turn::Running(_) | Turn::Pending(_)))
            ) {
                return Err(AgentError::InvalidState("session busy".into()));
            }
        }
        let mut params = self.params(&config)?;
        params["session_id"] = json!(id);
        let value = self
            .rpc
            .request("session/resume", params, self.timeout)
            .await?;
        self.session(value, config, Some(id))
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
                .ok_or_else(|| AgentError::InvalidState("session not created/resumed".into()))?;
            if slot.is_some() {
                return Err(AgentError::InvalidState(
                    "task active or unknown; reconcile before submitting".into(),
                ));
            }
            *slot = Some(Turn::Pending(token.clone()));
        }
        let guard = SubmissionGuard {
            shared: self.shared.clone(),
            session: session_id.into(),
            token: token.clone(),
        };
        let value = self
            .rpc
            .request(
                "task/submit",
                json!({"session_id": session_id, "text": text}),
                self.timeout,
            )
            .await?;
        if value["session_id"].as_str() != Some(session_id) {
            return Err(AgentError::Protocol("task session mismatch".into()));
        }
        let id = value["task_id"]
            .as_str()
            .ok_or_else(|| AgentError::Protocol("task id missing; outcome unknown".into()))?
            .to_owned();
        {
            let mut shared = self.shared.lock().expect("state lock");
            if matches!(shared.sessions.get(session_id), Some(Some(Turn::Pending(current))) if current == &token)
            {
                shared
                    .sessions
                    .insert(session_id.into(), Some(Turn::Running(id.clone())));
            }
        }
        drop(guard);
        Ok(Task {
            native_id: id,
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
                }
                _ => {
                    return Err(AgentError::InvalidState(
                        "task not active or outcome unknown".into(),
                    ));
                }
            }
        }
        self.rpc
            .request(
                "task/cancel",
                json!({"session_id": task.session_id, "task_id": task.native_id}),
                self.timeout,
            )
            .await?;
        Ok(())
    }
    async fn respond_approval(&self, response: ApprovalResponse) -> Result<()> {
        let approval = {
            let mut shared = self.shared.lock().expect("state lock");
            let approval = shared
                .approvals
                .get(&response.approval_id)
                .ok_or_else(|| AgentError::InvalidState("approval expired/resolved".into()))?;
            if approval.session != response.session_id
                || approval.task != response.task_id
                || approval.expires <= Instant::now()
            {
                return Err(AgentError::InvalidState(
                    "approval identity mismatch or expired".into(),
                ));
            }
            shared
                .approvals
                .remove(&response.approval_id)
                .expect("validated approval")
        };
        self.rpc.respond(approval.id, Ok(json!({"decision": match response.decision { ApprovalDecision::AllowOnce => "allow_once", ApprovalDecision::Deny => "deny" }}))).await
    }
    async fn shutdown(&self) -> Result<()> {
        let result = self.rpc.shutdown().await;
        let task = self.event_task.lock().expect("event task lock").take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        result
    }
}
impl Drop for ClaudeProvider {
    fn drop(&mut self) {
        self.rpc.close();
        if let Some(task) = self.event_task.lock().expect("event task lock").take() {
            task.abort();
        }
    }
}

async fn event_loop(
    rpc: RpcClient,
    mut raw: RpcEvents,
    shared: Arc<Mutex<Shared>>,
    info: ProviderInfo,
    sender: mpsc::Sender<Result<AgentEvent>>,
    timeout: Duration,
) {
    let mut sequence = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    loop {
        let frame = tokio::select! {
            _ = sender.closed() => { rpc.close(); break; },
            result = raw.recv() => match result { Ok(value) => value, Err(error) => { let _ = sender.send(Err(error)).await; break; } },
            _ = tick.tick() => {
                let expired = {
                    let mut shared = shared.lock().expect("state lock");
                    let ids: Vec<_> = shared.approvals.iter().filter(|(_, value)| value.expires <= Instant::now()).map(|(id, _)| id.clone()).collect();
                    ids.into_iter().filter_map(|id| shared.approvals.remove(&id)).collect::<Vec<_>>()
                };
                for approval in expired { let _ = rpc.respond(approval.id, Ok(json!({"decision":"deny"}))).await; }
                continue;
            }
        };
        let method = frame["method"]
            .as_str()
            .expect("validated method")
            .to_owned();
        let payload = frame.get("params").cloned().unwrap_or(Value::Null);
        let session = payload["session_id"].as_str().map(str::to_owned);
        let task = payload["task_id"].as_str().map(str::to_owned);
        let mut approval_id = None;
        let mut outcome = None;
        let kind = if let Some(id) = frame.get("id") {
            if method == "tool/approval" {
                if let (Some(session), Some(task)) = (&session, &task) {
                    let token = Uuid::new_v4().to_string();
                    let accepted = {
                        let mut shared = shared.lock().expect("state lock");
                        let known = matches!(shared.sessions.get(session), Some(Some(Turn::Running(current))) if current == task)
                            || matches!(shared.sessions.get(session), Some(Some(Turn::Pending(_))));
                        if known && shared.approvals.len() < 128 {
                            shared.approvals.insert(
                                token.clone(),
                                Approval {
                                    id: id.clone(),
                                    session: session.clone(),
                                    task: task.clone(),
                                    expires: Instant::now() + timeout,
                                },
                            );
                            true
                        } else {
                            false
                        }
                    };
                    if accepted {
                        approval_id = Some(token);
                    }
                }
                if approval_id.is_none() {
                    let _ = rpc
                        .respond(id.clone(), Ok(json!({"decision":"deny"})))
                        .await;
                }
                EventKind::ApprovalRequested
            } else {
                let _ = rpc
                    .respond(
                        id.clone(),
                        Err((-32601, "unsupported Claude bridge request")),
                    )
                    .await;
                EventKind::ProviderEvent
            }
        } else if method == "task/ended" {
            outcome = Some(
                if payload["outcome"] != "native_result"
                    || !payload["native_result"]["is_error"].is_boolean()
                {
                    TaskOutcome::Unknown
                } else if payload["native_result"]["is_error"] == false
                    && payload["native_result"]["subtype"] == "success"
                {
                    TaskOutcome::Completed
                } else {
                    TaskOutcome::Failed
                },
            );
            if let (Some(session), Some(task)) = (&session, &task) {
                let mut shared = shared.lock().expect("state lock");
                if let Some(slot) = shared.sessions.get_mut(session)
                    && (matches!(slot, Some(Turn::Running(current)) if current == task)
                        || matches!(slot, Some(Turn::Pending(_))))
                {
                    *slot = if outcome == Some(TaskOutcome::Unknown) {
                        Some(Turn::Unknown)
                    } else {
                        None
                    };
                }
                shared.finished.insert(session.clone(), task.clone());
                shared
                    .approvals
                    .retain(|_, approval| approval.session != *session || approval.task != *task);
            }
            EventKind::TaskState
        } else if method == "sdk/message"
            && payload["message"]["type"] == "stream_event"
            && payload["message"]["event"]["type"] == "content_block_delta"
        {
            EventKind::MessageDelta
        } else {
            EventKind::ProviderEvent
        };
        sequence += 1;
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
