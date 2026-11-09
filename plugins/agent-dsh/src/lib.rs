//! Native DeepSeek Harness ACP v1 client. DSH owns its tools, model loop and persistence.
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::mpsc, task::JoinHandle};
use virtade_agent_core::{
    rpc::{ProcessConfig, RpcClient, RpcEvents},
    *,
};
const DEADLINE: Duration = Duration::from_secs(15);
struct Approval {
    native: Value,
    session: String,
    task: String,
    allow: Option<String>,
    reject: Option<String>,
    expires: Instant,
}
#[derive(Default)]
struct State {
    sessions: HashMap<String, Option<String>>,
    approvals: HashMap<String, Approval>,
}
pub struct DshProvider {
    rpc: RpcClient,
    info: ProviderInfo,
    state: Arc<Mutex<State>>,
    completions: mpsc::Sender<(String, String, Result<Value>)>,
    pump: Mutex<Option<JoinHandle<()>>>,
    prompts: Mutex<Vec<JoinHandle<()>>>,
}
impl DshProvider {
    pub async fn connect(
        mut process: ProcessConfig,
    ) -> Result<(Arc<Self>, mpsc::Receiver<Result<AgentEvent>>)> {
        process.json_rpc_2 = true;
        let (rpc, events) = RpcClient::spawn(process)?;
        let response=match rpc.request("initialize",json!({"protocolVersion":1,"clientInfo":{"name":"virtade","version":env!("CARGO_PKG_VERSION")},"clientCapabilities":{}}),DEADLINE).await{Ok(value)=>value,Err(error)=>{let _=rpc.shutdown().await;return Err(error);}};
        if response["protocolVersion"] != 1 {
            let _ = rpc.shutdown().await;
            return Err(AgentError::Protocol("DSH must support ACP v1".into()));
        }
        let resume = if response["agentCapabilities"]["sessionCapabilities"]
            .get("resume")
            .is_some()
        {
            Support::Supported
        } else {
            Support::Unsupported("Server did not advertise session/resume".into())
        };
        let info = ProviderInfo {
            id: "dsh".into(),
            name: "DeepSeek Harness".into(),
            native_version: response["agentInfo"]["version"]
                .as_str()
                .unwrap_or("ACP v1; version not advertised")
                .into(),
            capabilities: BTreeMap::from([
                ("sessions.create".into(), Support::Supported),
                ("sessions.resume".into(), resume),
                ("tasks.stream".into(), Support::Supported),
                ("tasks.cancel".into(), Support::Supported),
                ("approvals.once".into(), Support::Supported),
                (
                    "tasks.resume".into(),
                    Support::Unsupported(
                        "ACP prompts have host correlation UUIDs, not resumable native turn IDs"
                            .into(),
                    ),
                ),
                (
                    "native.raw_deltas".into(),
                    Support::Unsupported("DSH ACP exposes committed semantic updates only".into()),
                ),
            ]),
        };
        let (state, sequence) = (
            Arc::new(Mutex::new(State::default())),
            Arc::new(AtomicU64::new(0)),
        );
        let (sender, receiver) = mpsc::channel(128);
        let (completions, completed) = mpsc::channel(128);
        let pump = tokio::spawn(pump(
            rpc.clone(),
            events,
            state.clone(),
            info.clone(),
            sequence.clone(),
            sender.clone(),
            completed,
        ));
        Ok((
            Arc::new(Self {
                rpc,
                info,
                state,
                completions,
                pump: Mutex::new(Some(pump)),
                prompts: Mutex::new(vec![]),
            }),
            receiver,
        ))
    }
    async fn session(&self, id: Option<&str>, config: SessionConfig) -> Result<Session> {
        if !config.provider_options.is_empty() {
            return Err(AgentError::Unsupported(
                "DSH options are configured by the native ACP profile".into(),
            ));
        }
        if id.is_some()
            && !matches!(
                self.info.capabilities.get("sessions.resume"),
                Some(Support::Supported)
            )
        {
            return Err(AgentError::Unsupported("session/resume".into()));
        }
        let cwd = config.cwd.canonicalize()?;
        let mut params = json!({"cwd":cwd,"mcpServers":[]});
        if let Some(id) = id {
            params["sessionId"] = json!(id);
        }
        let response = self
            .rpc
            .request(
                if id.is_some() {
                    "session/resume"
                } else {
                    "session/new"
                },
                params,
                DEADLINE,
            )
            .await?;
        let session = id
            .map(String::from)
            .or_else(|| response["sessionId"].as_str().map(String::from))
            .filter(|id| !id.is_empty())
            .ok_or_else(|| AgentError::Protocol("session/new missing sessionId".into()))?;
        let mut state = self.state.lock().expect("DSH state");
        if state.sessions.contains_key(&session) {
            return Err(AgentError::InvalidState("Session already open".into()));
        }
        state.sessions.insert(session.clone(), None);
        Ok(Session {
            native_id: session,
            cwd,
            reserved: false,
        })
    }
}
fn event(
    info: &ProviderInfo,
    sequence: &AtomicU64,
    session: Option<String>,
    task: Option<String>,
    kind: EventKind,
    method: &str,
    payload: Value,
) -> AgentEvent {
    AgentEvent {
        schema_version: 1,
        provider: "dsh".into(),
        native_version: info.native_version.clone(),
        sequence: sequence.fetch_add(1, Ordering::SeqCst) + 1,
        timestamp_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        session_id: session,
        task_id: task,
        kind,
        native_method: method.into(),
        payload,
        approval_id: None,
        outcome: None,
    }
}
#[async_trait]
impl AgentProvider for DshProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }
    async fn create_session(&self, config: SessionConfig) -> Result<Session> {
        self.session(None, config).await
    }
    async fn resume_session(&self, id: &str, config: SessionConfig) -> Result<Session> {
        self.session(Some(id), config).await
    }
    async fn submit(&self, session: &str, text: &str) -> Result<Task> {
        if text.is_empty() || text.len() > 512 * 1024 {
            return Err(AgentError::InvalidState(
                "Prompt must be nonempty and at most 512 KiB".into(),
            ));
        }
        let task = uuid::Uuid::new_v4().to_string();
        {
            let mut state = self.state.lock().expect("DSH state");
            let slot = state
                .sessions
                .get_mut(session)
                .ok_or_else(|| AgentError::InvalidState("Unknown session".into()))?;
            if slot.is_some() {
                return Err(AgentError::InvalidState(
                    "Session is active or outcome is unknown".into(),
                ));
            }
            *slot = Some(task.clone());
        }
        let rpc = self.rpc.clone();
        let completions = self.completions.clone();
        let session = session.to_owned();
        let prompt = text.to_owned();
        let native_task = task.clone();
        let task_session = session.clone();
        let (started, queued) = tokio::sync::oneshot::channel();
        let join = tokio::spawn(async move {
            // ACP holds the request until the native task and descendant work settle.
            let response = rpc
                .request_started(
                    "session/prompt",
                    json!({"sessionId":session,"prompt":[{"type":"text","text":prompt}]}),
                    Duration::from_secs(24 * 60 * 60),
                    started,
                )
                .await;
            if completions
                .try_send((session, native_task, response))
                .is_err()
            {
                rpc.close();
            }
        });
        {
            let mut prompts = self.prompts.lock().expect("DSH prompts");
            prompts.retain(|handle| !handle.is_finished());
            prompts.push(join);
        }
        queued
            .await
            .map_err(|_| AgentError::Closed("Native prompt was not queued".into()))?;
        Ok(Task {
            native_id: task,
            session_id: task_session,
        })
    }
    async fn cancel(&self, task: &Task) -> Result<()> {
        let matches = self
            .state
            .lock()
            .expect("DSH state")
            .sessions
            .get(&task.session_id)
            .is_some_and(|slot| slot.as_deref() == Some(&task.native_id));
        if !matches {
            return Err(AgentError::InvalidState("Unknown task".into()));
        }
        self.rpc
            .notify("session/cancel", json!({"sessionId":task.session_id}))
            .await
    }
    async fn respond_approval(&self, response: ApprovalResponse) -> Result<()> {
        let approval = {
            let mut state = self.state.lock().expect("DSH state");
            let approval = state
                .approvals
                .get(&response.approval_id)
                .ok_or_else(|| AgentError::InvalidState("Expired or used approval".into()))?;
            if approval.session != response.session_id
                || approval.task != response.task_id
                || approval.expires <= Instant::now()
            {
                return Err(AgentError::InvalidState(
                    "Approval identity or lifetime mismatch".into(),
                ));
            }
            state
                .approvals
                .remove(&response.approval_id)
                .expect("checked")
        };
        let option = match response.decision {
            ApprovalDecision::AllowOnce => approval.allow,
            ApprovalDecision::Deny => approval.reject,
        };
        let outcome = option
            .map(|option| json!({"outcome":"selected","optionId":option}))
            .unwrap_or_else(|| json!({"outcome":"cancelled"}));
        self.rpc
            .respond(approval.native, Ok(json!({"outcome":outcome})))
            .await
    }
    async fn shutdown(&self) -> Result<()> {
        let result = self.rpc.shutdown().await;
        let pump = self.pump.lock().expect("DSH pump").take();
        if let Some(pump) = pump {
            pump.abort();
            let _ = pump.await;
        }
        let prompts = std::mem::take(&mut *self.prompts.lock().expect("DSH prompts"));
        for prompt in prompts {
            prompt.abort();
            let _ = prompt.await;
        }
        result
    }
}
impl Drop for DshProvider {
    fn drop(&mut self) {
        self.rpc.close();
        if let Ok(mut pump) = self.pump.lock()
            && let Some(pump) = pump.take()
        {
            pump.abort();
        }
        if let Ok(mut tasks) = self.prompts.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}
async fn pump(
    rpc: RpcClient,
    mut events: RpcEvents,
    state: Arc<Mutex<State>>,
    info: ProviderInfo,
    sequence: Arc<AtomicU64>,
    sender: mpsc::Sender<Result<AgentEvent>>,
    mut completed: mpsc::Receiver<(String, String, Result<Value>)>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        let frame = tokio::select! {biased;result=events.recv()=>match result{Ok(frame)=>frame,Err(error)=>{let _=sender.try_send(Err(error));break;}},Some((session,task,response))=completed.recv()=>{
         let outcome=match &response{Ok(value)=>match value["stopReason"].as_str(){Some("cancelled")=>TaskOutcome::Cancelled,Some("end_turn"|"max_tokens"|"max_turn_requests"|"refusal")=>TaskOutcome::Completed,_=>TaskOutcome::Unknown},Err(AgentError::Remote{..})=>TaskOutcome::Failed,Err(_)=>TaskOutcome::Unknown};
         {let mut state=state.lock().expect("DSH state");if outcome!=TaskOutcome::Unknown&& let Some(slot)=state.sessions.get_mut(&session){*slot=None;}state.approvals.retain(|_,approval|approval.session!=session||approval.task!=task);}
         let payload=match response{Ok(value)=>value,Err(error)=>json!({"error":error.to_string()})};let mut terminal=event(&info,&sequence,Some(session),Some(task),EventKind::TaskState,"session/prompt/result",payload);terminal.outcome=Some(outcome);if sender.try_send(Ok(terminal)).is_err(){rpc.close();break;}continue;
        },_=sender.closed()=>{rpc.close();break;},_=tick.tick()=>{let expired={let mut state=state.lock().expect("DSH state");let keys:Vec<_>=state.approvals.iter().filter(|(_,a)|a.expires<=Instant::now()).map(|(key,_)|key.clone()).collect();keys.into_iter().filter_map(|key|state.approvals.remove(&key)).collect::<Vec<_>>()};for approval in expired{let _=rpc.respond(approval.native,Ok(json!({"outcome":{"outcome":"cancelled"}}))).await;}continue;}};
        let method = frame["method"].as_str().unwrap_or("");
        let payload = frame.get("params").cloned().unwrap_or(Value::Null);
        let session = payload["sessionId"].as_str().map(String::from);
        let task = session.as_ref().and_then(|session| {
            state
                .lock()
                .expect("DSH state")
                .sessions
                .get(session)
                .cloned()
                .flatten()
        });
        let kind = match payload["update"]["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => EventKind::MessageDelta,
            Some("tool_call" | "tool_call_update") => EventKind::ToolState,
            _ => EventKind::ProviderEvent,
        };
        let mut projected = event(
            &info,
            &sequence,
            session.clone(),
            task.clone(),
            kind,
            method,
            payload.clone(),
        );
        if let Some(id) = frame.get("id") {
            if method == "session/request_permission"
                && let (Some(session), Some(task)) = (session, task)
            {
                let options = payload["options"].as_array();
                let choice = |kind: &str| {
                    options
                        .and_then(|options| options.iter().find(|option| option["kind"] == kind))
                        .and_then(|option| option["optionId"].as_str())
                        .map(String::from)
                };
                let token = uuid::Uuid::new_v4().to_string();
                let mut state = state.lock().expect("DSH state");
                if state.approvals.len() >= 128 {
                    rpc.close();
                    break;
                }
                state.approvals.insert(
                    token.clone(),
                    Approval {
                        native: id.clone(),
                        session,
                        task,
                        allow: choice("allow_once"),
                        reject: choice("reject_once"),
                        expires: Instant::now() + Duration::from_secs(120),
                    },
                );
                projected.kind = EventKind::ApprovalRequested;
                projected.approval_id = Some(token);
            } else {
                let _ = rpc
                    .respond(id.clone(), Err((-32601, "Unsupported ACP client request")))
                    .await;
            }
        }
        if sender.try_send(Ok(projected)).is_err() {
            rpc.close();
            break;
        }
    }
}
