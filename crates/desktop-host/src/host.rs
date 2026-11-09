use crate::workbench::{WorkbenchPlugin, WorkbenchRequest};
use crate::{Connector, NativeConnector, ProviderSettings};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use virt::Contribution;
use virt_protocol::Permissions;
use virtade_agent_core::{
    AgentError, AgentEvent, AgentProvider, ApprovalDecision, ApprovalResponse, ProviderInfo, Task,
};
use virtade_plugin_runtime::{PluginManager, PluginSnapshot, UiRequest};
use virtade_storage::{StorageError, Store, StoredSession, StoredTask, Workspace};
use virtade_workspace::{WorkspaceError, WorkspaceSession};

#[derive(Debug, Serialize, Deserialize)]
pub struct HostError {
    pub code: String,
    pub message: String,
}
impl HostError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for HostError {}
impl From<StorageError> for HostError {
    fn from(error: StorageError) -> Self {
        Self::new(
            match error {
                StorageError::Locked => "database_locked",
                StorageError::Unresolved(_) => "task_unknown",
                _ => "storage",
            },
            error.to_string(),
        )
    }
}
impl From<AgentError> for HostError {
    fn from(error: AgentError) -> Self {
        Self::new("agent", error.to_string())
    }
}
impl From<WorkspaceError> for HostError {
    fn from(error: WorkspaceError) -> Self {
        Self::new("workspace", error.to_string())
    }
}
pub type HostResult<T> = std::result::Result<T, HostError>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingApproval {
    pub id: String,
    pub session_id: String,
    pub task_id: String,
    pub details: Value,
}
struct ApprovalEntry {
    visible: PendingApproval,
    native: ApprovalResponse,
    expires: Instant,
}
struct ActiveTask {
    stored: StoredTask,
    native: Task,
}
struct LiveSession {
    host: WorkspaceSession,
    provider: Arc<dyn AgentProvider>,
    gate: AsyncMutex<()>,
    active: Mutex<Option<ActiveTask>>,
    approvals: Mutex<HashMap<String, ApprovalEntry>>,
    diagnostics: Mutex<Vec<String>>,
    cursor: AtomicU64,
    connected: AtomicBool,
    cancellation: CancellationToken,
    pump: Mutex<Option<JoinHandle<()>>>,
}
impl LiveSession {
    fn diagnostic(&self, message: String) {
        let mut log = self.diagnostics.lock().expect("diagnostics");
        if log.len() == 32 {
            log.remove(0);
        }
        log.push(message);
    }
    async fn close(&self) {
        self.cancellation.cancel();
        let _ = self.provider.shutdown().await;
        let task = self.pump.lock().expect("pump").take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        let active = self.active.lock().expect("active").take();
        if let Some(active) = active {
            let _ = self.host.mark_unknown(active.stored.id).await;
        }
        self.approvals.lock().expect("approvals").clear();
    }
}
impl Drop for LiveSession {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.pump.lock().expect("pump").take() {
            task.abort();
        }
    }
}

#[derive(Serialize)]
pub struct LiveSnapshot {
    pub session_id: String,
    pub info: ProviderInfo,
    pub active: Option<StoredTask>,
    pub approvals: Vec<PendingApproval>,
    pub cursor: u64,
    pub connected: bool,
    pub diagnostics: Vec<String>,
}
#[derive(Serialize)]
pub struct Snapshot {
    pub workspaces: Vec<Workspace>,
    pub sessions: Vec<StoredSession>,
    pub live: Vec<LiveSnapshot>,
    pub recovered_tasks: usize,
    pub settings: ProviderSettings,
    pub plugins: Vec<PluginSnapshot>,
    pub contributions: Vec<(String, Contribution)>,
    pub plugin_diagnostics: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DesktopRequest {
    Workbench {
        workspace_id: String,
        request: WorkbenchRequest,
    },
    Snapshot,
    Diagnostics,
    PluginInspect {
        source: PathBuf,
    },
    PluginInstall {
        source: PathBuf,
        granted: Permissions,
    },
    PluginEnable {
        id: String,
    },
    PluginDisable {
        id: String,
    },
    PluginReload {
        id: String,
    },
    PluginUninstall {
        id: String,
    },
    PluginUi {
        id: String,
        generation: String,
    },
    PluginRequest {
        id: String,
        generation: String,
        request: UiRequest,
    },
    OpenWorkspace {
        root: PathBuf,
    },
    OpenSession {
        workspace_id: String,
        provider: String,
        resume: Option<String>,
    },
    Submit {
        session_id: String,
        request_key: String,
        text: String,
    },
    Cancel {
        session_id: String,
    },
    Approve {
        session_id: String,
        approval_id: String,
        decision: ApprovalDecision,
    },
    CloseSession {
        session_id: String,
    },
    History {
        session_id: String,
        after: u64,
        limit: u32,
    },
    SaveSettings {
        settings: ProviderSettings,
    },
}

pub struct AppHost {
    pub store: Store,
    pub plugins: Arc<PluginManager>,
    connector: Arc<dyn Connector>,
    live: Mutex<HashMap<String, Arc<LiveSession>>>,
    opening: AsyncMutex<()>,
    closed: CancellationToken,
}
impl AppHost {
    pub async fn open(database: PathBuf) -> HostResult<Arc<Self>> {
        Self::with_connector(database, Arc::new(NativeConnector)).await
    }
    pub async fn with_connector(
        database: PathBuf,
        connector: Arc<dyn Connector>,
    ) -> HostResult<Arc<Self>> {
        let store = Store::open(database.clone()).await?;
        let mut package_root = database;
        package_root.set_extension("plugins");
        let plugins = PluginManager::open(package_root, store.clone())
            .await
            .map_err(|error| HostError::new("plugins", error))?;
        plugins
            .register_builtin(Arc::new(WorkbenchPlugin::new(store.clone())))
            .await
            .map_err(|error| HostError::new("workbench", error))?;
        let host = Arc::new(Self {
            store,
            plugins,
            connector,
            live: Mutex::new(HashMap::new()),
            opening: AsyncMutex::new(()),
            closed: CancellationToken::new(),
        });
        host.plugins
            .register_builtin(Arc::new(crate::agents::AgentServices {
                host: Arc::downgrade(&host),
            }))
            .await
            .map_err(|error| HostError::new("agents", error))?;
        host.plugins.activate_saved().await;
        Ok(host)
    }
    fn session(&self, id: &str) -> HostResult<Arc<LiveSession>> {
        self.live
            .lock()
            .expect("live")
            .get(id)
            .cloned()
            .ok_or_else(|| {
                HostError::new("session_closed", "Resume the session before running a task")
            })
    }
    pub async fn settings(&self) -> HostResult<ProviderSettings> {
        let value = self
            .store
            .get_setting("desktop".into(), "providers".into())
            .await?;
        value
            .map(serde_json::from_value)
            .transpose()
            .map(|value| value.unwrap_or_default())
            .map_err(|error| HostError::new("configuration", error.to_string()))
    }
    pub async fn snapshot(&self) -> HostResult<Snapshot> {
        let workspaces = self.store.workspaces().await?;
        let mut sessions = Vec::new();
        for workspace in &workspaces {
            sessions.extend(self.store.sessions(workspace.id.clone()).await?);
        }
        let live = self
            .live
            .lock()
            .expect("live")
            .values()
            .map(|live| LiveSnapshot {
                session_id: live.host.session.id.clone(),
                info: live.provider.info().clone(),
                active: live
                    .active
                    .lock()
                    .expect("active")
                    .as_ref()
                    .map(|task| task.stored.clone()),
                approvals: live
                    .approvals
                    .lock()
                    .expect("approvals")
                    .values()
                    .map(|entry| entry.visible.clone())
                    .collect(),
                cursor: live.cursor.load(Ordering::Relaxed),
                connected: live.connected.load(Ordering::Relaxed),
                diagnostics: live.diagnostics.lock().expect("diagnostics").clone(),
            })
            .collect();
        Ok(Snapshot {
            workspaces,
            sessions,
            live,
            recovered_tasks: self.store.recovered_tasks(),
            settings: self.settings().await?,
            plugins: self.plugins.snapshot().await,
            contributions: self.plugins.contributions().await,
            plugin_diagnostics: self.plugins.diagnostics(),
        })
    }
    pub async fn request(&self, version: u32, request: DesktopRequest) -> HostResult<Value> {
        if version != 1 {
            return Err(HostError::new(
                "api_version",
                "Unsupported desktop API version",
            ));
        }
        if self.closed.is_cancelled() {
            return Err(HostError::new("closed", "Application is shutting down"));
        }
        match request {
            DesktopRequest::Diagnostics => {
                let snapshot = self.snapshot().await?;
                // Deliberately exclude prompts, raw events, approvals, roots and executable paths.
                Ok(json!({
                    "schema_version":1,"application_version":env!("CARGO_PKG_VERSION"),
                    "os":std::env::consts::OS,"arch":std::env::consts::ARCH,
                    "resources":self.plugins.resource_counts().await,
                    "workspaces":snapshot.workspaces.len(),"sessions":snapshot.sessions.len(),
                    "recovered_tasks":snapshot.recovered_tasks,
                    "agents":snapshot.live.iter().map(|live|json!({"provider":live.info.id,"native_version":live.info.native_version,"connected":live.connected,"active":live.active.is_some(),"diagnostic_count":live.diagnostics.len()})).collect::<Vec<_>>(),
                    "plugins":snapshot.plugins.iter().map(|plugin|json!({"id":plugin.package.manifest.id,"version":plugin.package.manifest.version,"state":plugin.state,"diagnostic_count":plugin.diagnostics.len()})).collect::<Vec<_>>(),
                    "observer_error_count":snapshot.plugin_diagnostics.len()
                }))
            }
            DesktopRequest::Workbench {
                workspace_id,
                request,
            } => self
                .plugins
                .host_call(
                    crate::workbench::key(),
                    json!({"workspace_id":workspace_id,"request":request}),
                )
                .await
                .map_err(|error| HostError::new("workbench", error)),
            DesktopRequest::PluginInspect { source } => Ok(json!(
                PluginManager::inspect(source)
                    .await
                    .map_err(|error| HostError::new("plugins", error))?
            )),
            DesktopRequest::PluginInstall { source, granted } => Ok(json!(
                self.plugins
                    .install(source, granted)
                    .await
                    .map_err(|error| HostError::new("plugins", error))?
            )),
            DesktopRequest::PluginEnable { id } => {
                self.plugins
                    .enable(&id)
                    .await
                    .map_err(|error| HostError::new("plugins", error))?;
                Ok(Value::Null)
            }
            DesktopRequest::PluginDisable { id } => {
                self.plugins
                    .disable(&id)
                    .await
                    .map_err(|error| HostError::new("plugins", error))?;
                Ok(Value::Null)
            }
            DesktopRequest::PluginReload { id } => {
                self.plugins
                    .reload(&id)
                    .await
                    .map_err(|error| HostError::new("plugins", error))?;
                Ok(Value::Null)
            }
            DesktopRequest::PluginUninstall { id } => {
                self.plugins
                    .uninstall(&id)
                    .await
                    .map_err(|error| HostError::new("plugins", error))?;
                Ok(Value::Null)
            }
            DesktopRequest::PluginUi { id, generation } => Ok(json!(
                self.plugins
                    .ui(&id, &generation)
                    .await
                    .map_err(|error| HostError::new("plugins", error))?
            )),
            DesktopRequest::PluginRequest {
                id,
                generation,
                request,
            } => self
                .plugins
                .ui_request(&id, &generation, request)
                .await
                .map_err(|error| HostError::new("plugins", error)),
            DesktopRequest::Snapshot => Ok(json!(self.snapshot().await?)),
            DesktopRequest::OpenWorkspace { root } => {
                let name = root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Workspace")
                    .to_owned();
                Ok(json!(self.store.workspace(root, name).await?))
            }
            DesktopRequest::OpenSession {
                workspace_id,
                provider,
                resume,
            } => {
                // Only the control plane is serialized; running sessions remain independent.
                let _opening = self.opening.lock().await;
                if let Some(id) = &resume {
                    let stored = self.store.session(id.clone()).await?;
                    if stored.workspace_id != workspace_id || stored.provider != provider {
                        return Err(HostError::new(
                            "scope",
                            "Session belongs to another workspace or provider",
                        ));
                    }
                    let existing = self.live.lock().expect("live").get(id).cloned();
                    if let Some(existing) = existing {
                        if existing.connected.load(Ordering::Relaxed) {
                            return Ok(json!(self.store.session(id.clone()).await?));
                        }
                        self.live.lock().expect("live").remove(id);
                        existing.close().await;
                    }
                }
                let workspace = self
                    .store
                    .workspaces()
                    .await?
                    .into_iter()
                    .find(|workspace| workspace.id == workspace_id)
                    .ok_or_else(|| HostError::new("workspace_missing", "Workspace not found"))?;
                if self.live.lock().expect("live").len() >= 32 {
                    return Err(HostError::new(
                        "session_limit",
                        "Close a live session before opening another (limit 32)",
                    ));
                }
                let (native, mut events) =
                    if matches!(provider.as_str(), "codex" | "claude" | "dsh") {
                        self.connector
                            .connect(&provider, workspace.root.clone(), self.settings().await?)
                            .await?
                    } else {
                        crate::contributed::connect(
                            self.plugins.clone(),
                            &provider,
                            workspace.root.clone(),
                        )
                        .await?
                    };
                let host = match WorkspaceSession::open(
                    self.store.clone(),
                    native.clone(),
                    workspace.root,
                    resume,
                )
                .await
                {
                    Ok(host) => host,
                    Err(error) => {
                        let _ = native.shutdown().await;
                        return Err(error.into());
                    }
                };
                let session = host.session.clone();
                let live = Arc::new(LiveSession {
                    host,
                    provider: native,
                    gate: AsyncMutex::new(()),
                    active: Mutex::new(None),
                    approvals: Mutex::new(HashMap::new()),
                    diagnostics: Mutex::new(vec![]),
                    cursor: AtomicU64::new(0),
                    connected: AtomicBool::new(true),
                    cancellation: self.closed.child_token(),
                    pump: Mutex::new(None),
                });
                let plugins = self.plugins.clone();
                let weak = Arc::downgrade(&live);
                let cancellation = live.cancellation.clone();
                let pump = tokio::spawn(async move {
                    let mut expiry = tokio::time::interval(Duration::from_millis(100));
                    loop {
                        tokio::select! {
                            _=cancellation.cancelled()=>break,
                            _=expiry.tick()=>{
                                let Some(live)=weak.upgrade()else{break;};
                                let expired={let mut approvals=live.approvals.lock().expect("approvals");let ids:Vec<_>=approvals.iter().filter(|(_,entry)|entry.expires<=Instant::now()).map(|(id,_)|id.clone()).collect();ids.into_iter().filter_map(|id|approvals.remove(&id)).collect::<Vec<_>>()};
                                for mut entry in expired{entry.native.decision=ApprovalDecision::Deny;let _=live.provider.respond_approval(entry.native).await;}
                            }
                            event=events.recv()=>{
                                let Some(live)=weak.upgrade()else{break;};
                                match event{
                                    Some(Ok(event))=>{if let Err(error)=process_event(&live,event.clone()).await{live.diagnostic(error.to_string());live.connected.store(false,Ordering::Relaxed);let id=live.active.lock().expect("active").take().map(|task|task.stored.id);if let Some(id)=id{let _=live.host.mark_unknown(id).await;}live.approvals.lock().expect("approvals").clear();let _=live.provider.shutdown().await;break;}plugins.publish(&event.provider,json!(event)).await;},
                                    error=>{let message=match error{Some(Err(error))=>error.to_string(),_=>"Agent connection ended".into()};live.diagnostic(message);live.connected.store(false,Ordering::Relaxed);let id=live.active.lock().expect("active").take().map(|task|task.stored.id);if let Some(id)=id{let _=live.host.mark_unknown(id).await;}live.approvals.lock().expect("approvals").clear();break;}
                                }
                            }
                        }
                    }
                });
                *live.pump.lock().expect("pump") = Some(pump);
                self.live
                    .lock()
                    .expect("live")
                    .insert(session.id.clone(), live);
                Ok(json!(session))
            }
            DesktopRequest::Submit {
                session_id,
                request_key,
                text,
            } => {
                let live = self.session(&session_id)?;
                let _gate = live.gate.lock().await;
                let (stored, native) = live.host.submit(request_key, text).await?;
                *live.active.lock().expect("active") = Some(ActiveTask {
                    stored: stored.clone(),
                    native,
                });
                Ok(json!(stored))
            }
            DesktopRequest::Cancel { session_id } => {
                let live = self.session(&session_id)?;
                let native = live
                    .active
                    .lock()
                    .expect("active")
                    .as_ref()
                    .map(|task| task.native.clone())
                    .ok_or_else(|| HostError::new("no_active_task", "No active task to cancel"))?;
                live.provider.cancel(&native).await?;
                Ok(Value::Null)
            }
            DesktopRequest::Approve {
                session_id,
                approval_id,
                decision,
            } => {
                let live = self.session(&session_id)?;
                let entry = live
                    .approvals
                    .lock()
                    .expect("approvals")
                    .remove(&approval_id)
                    .ok_or_else(|| {
                        HostError::new("approval_expired", "Approval expired or already answered")
                    })?;
                if entry.expires <= Instant::now() {
                    return Err(HostError::new("approval_expired", "Approval expired"));
                }
                let mut native = entry.native;
                native.decision = decision;
                live.provider.respond_approval(native).await?;
                Ok(Value::Null)
            }
            DesktopRequest::CloseSession { session_id } => {
                let live = self.live.lock().expect("live").remove(&session_id);
                if let Some(live) = live {
                    live.close().await;
                }
                Ok(Value::Null)
            }
            DesktopRequest::History {
                session_id,
                after,
                limit,
            } => Ok(
                json!({"events":self.store.events(session_id.clone(),after,limit).await?,"tasks":self.store.tasks(session_id).await?}),
            ),
            DesktopRequest::SaveSettings { settings } => {
                self.store
                    .set_setting("desktop".into(), "providers".into(), json!(settings))
                    .await?;
                Ok(Value::Null)
            }
        }
    }
    pub async fn shutdown(&self) -> HostResult<()> {
        self.closed.cancel();
        let lives: Vec<_> = self
            .live
            .lock()
            .expect("live")
            .drain()
            .map(|(_, live)| live)
            .collect();
        for live in lives {
            live.close().await;
        }
        self.plugins.shutdown().await;
        self.store.close().await?;
        Ok(())
    }
}

async fn process_event(live: &LiveSession, event: AgentEvent) -> HostResult<()> {
    let _gate = live.gate.lock().await;
    if event.session_id.as_deref() != Some(&live.host.session.native_id) {
        return Ok(());
    }
    let cursor = live.host.ingest(event.clone()).await?;
    live.cursor.store(cursor, Ordering::Relaxed);
    if let Some(token) = &event.approval_id {
        let task = event
            .task_id
            .clone()
            .ok_or_else(|| HostError::new("protocol", "Approval lacks task identity"))?;
        let id = Uuid::new_v4().to_string();
        live.approvals.lock().expect("approvals").insert(
            id.clone(),
            ApprovalEntry {
                visible: PendingApproval {
                    id,
                    session_id: live.host.session.id.clone(),
                    task_id: task.clone(),
                    details: event.payload.clone(),
                },
                native: ApprovalResponse {
                    approval_id: token.clone(),
                    session_id: live.host.session.native_id.clone(),
                    task_id: task,
                    decision: ApprovalDecision::Deny,
                },
                expires: Instant::now() + Duration::from_secs(90),
            },
        );
    }
    if event.outcome.is_some() {
        let task = event.task_id.as_deref();
        let mut active = live.active.lock().expect("active");
        if active
            .as_ref()
            .is_some_and(|active| Some(active.native.native_id.as_str()) == task)
        {
            *active = None;
        }
        live.approvals
            .lock()
            .expect("approvals")
            .retain(|_, entry| Some(entry.native.task_id.as_str()) != task);
    }
    Ok(())
}
