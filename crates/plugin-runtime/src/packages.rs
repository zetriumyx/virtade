use crate::{ComponentEngine, WasmPlugin};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, VecDeque};
use std::sync::Mutex as StdMutex;
use std::{
    collections::BTreeMap,
    path::{Component as PathComponent, Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use virt::{Context, Flow, Plugin, async_trait};
use virt::{Contribution, Kernel, PluginState, UnloadMode};
use virt_protocol::{Capability, Manifest, Permissions, ServiceKey};
use virtade_storage::Store;

struct EventRelay;
#[async_trait]
impl Plugin for EventRelay {
    fn manifest(&self) -> Manifest {
        let mut manifest = Manifest::new("virtade-event-relay");
        manifest.permissions = ["codex", "claude", "dsh"]
            .into_iter()
            .map(|provider| Capability::EventPublish(format!("virtade.agent.{provider}.event.v1")))
            .collect();
        manifest
            .permissions
            .insert(Capability::EventPublish("virtade.agent.event.v1".into()));
        manifest
            .permissions
            .insert(Capability::ServiceCall(ServiceKey::new(
                "virtade.workbench.internal",
                1,
            )));
        manifest
    }
    async fn start(&self, _: Context) -> virt_protocol::Result<()> {
        Ok(())
    }
}
#[derive(Default)]
struct UiQueue {
    next: u64,
    dropped: u64,
    events: VecDeque<serde_json::Value>,
    topics: BTreeSet<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum UiRequest {
    Contribute {
        definition: Contribution,
    },
    Resource {
        request: crate::ResourceRequest,
    },
    ServiceCall {
        key: ServiceKey,
        request: serde_json::Value,
    },
    ConfigGet {
        key: String,
    },
    ConfigSet {
        key: String,
        value: serde_json::Value,
    },
    EventSubscribe {
        topic: String,
    },
    EventPoll {
        after: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMetadata {
    pub schema_version: u32,
    pub component: String,
    pub ui: Option<String>,
}
impl Default for PackageMetadata {
    fn default() -> Self {
        Self {
            schema_version: 1,
            component: "component.wasm".into(),
            ui: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub manifest: Manifest,
    pub metadata: PackageMetadata,
    pub digest: String,
    pub directory: PathBuf,
    pub granted: Permissions,
    pub enabled: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct PluginSnapshot {
    pub package: InstalledPlugin,
    pub state: String,
    pub generation: Option<String>,
    pub diagnostics: Vec<String>,
}
struct ManagerState {
    kernel: Kernel,
    packages: BTreeMap<String, InstalledPlugin>,
    plugins: BTreeMap<String, Arc<WasmPlugin>>,
    generations: BTreeMap<String, String>,
    errors: BTreeMap<String, String>,
}
pub struct PluginManager {
    root: PathBuf,
    database: Store,
    engine: Arc<ComponentEngine>,
    state: Mutex<ManagerState>,
    ui_queues: StdMutex<BTreeMap<String, Arc<StdMutex<UiQueue>>>>,
    observer_errors: Arc<StdMutex<Vec<String>>>,
    event_tx: tokio::sync::mpsc::Sender<(String, serde_json::Value)>,
    event_task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    pending_enabled: Mutex<Vec<String>>,
}
type PackageResult<T> = std::result::Result<T, String>;

fn entry(root: &Path, name: &str, limit: usize) -> PackageResult<Vec<u8>> {
    let path = Path::new(name);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, PathComponent::Normal(_)))
    {
        return Err("package entry must be a relative path without traversal".into());
    }
    let mut current = root.to_path_buf();
    for part in path.components() {
        current.push(part);
        let metadata = current
            .symlink_metadata()
            .map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("package symlinks are forbidden".into());
        }
    }
    let metadata = current.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err("package entry exceeds its size limit".into());
    }
    {
        use std::io::Read;
        let directory = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
            .map_err(|error| error.to_string())?;
        let file = directory.open(path).map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > limit {
            return Err("package entry grew beyond its limit".into());
        }
        Ok(bytes)
    }
}
type PackageFiles = Vec<(String, Vec<u8>)>;
type ReadPackage = (Manifest, PackageMetadata, PackageFiles, String);
fn read_package(root: &Path) -> PackageResult<ReadPackage> {
    let manifest_bytes = entry(root, "manifest.json", 64 * 1024)?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| error.to_string())?;
    manifest.validate().map_err(|error| error.to_string())?;
    if manifest.id.starts_with("virt") || manifest.id.starts_with("agent-") {
        return Err("reserved builtin plugin namespace".into());
    }
    let metadata = if root.join("package.json").exists() {
        serde_json::from_slice(&entry(root, "package.json", 16 * 1024)?)
            .map_err(|error| error.to_string())?
    } else {
        PackageMetadata::default()
    };
    if metadata.schema_version != 1 {
        return Err("unsupported package schema".into());
    }
    if matches!(
        metadata.component.as_str(),
        "manifest.json" | "package.json"
    ) || metadata.ui.as_ref().is_some_and(|ui| {
        ui == &metadata.component || ui == "manifest.json" || ui == "package.json"
    }) {
        return Err("package entry names conflict".into());
    }
    let mut files = vec![
        ("manifest.json".into(), manifest_bytes),
        (
            "package.json".into(),
            serde_json::to_vec(&metadata).map_err(|error| error.to_string())?,
        ),
        (
            metadata.component.clone(),
            entry(root, &metadata.component, 16 * 1024 * 1024)?,
        ),
    ];
    if let Some(ui) = &metadata.ui {
        let bytes = entry(root, ui, 1024 * 1024)?;
        std::str::from_utf8(&bytes).map_err(|_| "UI entry must be UTF-8")?;
        files.push((ui.clone(), bytes));
    }
    let mut hash = Sha256::new();
    for (name, bytes) in &files {
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok((manifest, metadata, files, format!("{:x}", hash.finalize())))
}

impl PluginManager {
    pub async fn register_builtin(&self, plugin: Arc<dyn Plugin>) -> PackageResult<()> {
        let mut state = self.state.lock().await;
        let manifest = plugin.manifest();
        let id = manifest.id.clone();
        state
            .kernel
            .install(plugin, manifest.permissions)
            .map_err(|error| error.to_string())?;
        state
            .kernel
            .start(&id)
            .await
            .map_err(|error| error.to_string())
    }
    pub async fn host_call(
        &self,
        key: ServiceKey,
        request: serde_json::Value,
    ) -> PackageResult<serde_json::Value> {
        let context = self
            .state
            .lock()
            .await
            .kernel
            .context("virtade-event-relay")
            .map_err(|error| error.to_string())?;
        context
            .call(&key, request, std::time::Duration::from_secs(15))
            .await
            .map_err(|error| error.to_string())
    }
    pub async fn open(root: PathBuf, database: Store) -> PackageResult<Arc<Self>> {
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let root = root.canonicalize().map_err(|error| error.to_string())?;
        let packages: Vec<InstalledPlugin> = database
            .get_setting("host.plugins".into(), "installed".into())
            .await
            .map_err(|error| error.to_string())?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let engine = ComponentEngine::new().map_err(|error| error.to_string())?;
        let mut state = ManagerState {
            kernel: Kernel::default(),
            packages: BTreeMap::new(),
            plugins: BTreeMap::new(),
            generations: BTreeMap::new(),
            errors: BTreeMap::new(),
        };
        state
            .kernel
            .install(Arc::new(EventRelay), EventRelay.manifest().permissions)
            .map_err(|error| error.to_string())?;
        state
            .kernel
            .start("virtade-event-relay")
            .await
            .map_err(|error| error.to_string())?;
        let mut enabled = vec![];
        for mut package in packages {
            // Never execute a moved or tampered package using persisted permissions.
            let verified = (|| {
                let directory = package
                    .directory
                    .canonicalize()
                    .map_err(|error| error.to_string())?;
                if !directory.starts_with(&root) {
                    return Err("installed package escaped its storage root".to_owned());
                }
                let (_, _, _, digest) = read_package(&directory)?;
                if digest != package.digest {
                    return Err("plugin failed integrity verification".into());
                }
                Ok::<_, String>(directory)
            })();
            let directory = match verified {
                Ok(directory) => directory,
                Err(error) => {
                    package.enabled = false;
                    state.errors.insert(package.manifest.id.clone(), error);
                    state.packages.insert(package.manifest.id.clone(), package);
                    continue;
                }
            };
            let id = package.manifest.id.clone();
            if package.enabled {
                enabled.push(id.clone());
            }
            package.enabled = false;
            let plugin = Arc::new(WasmPlugin::new(
                package.manifest.clone(),
                directory.join(&package.metadata.component),
                engine.clone(),
                database.clone(),
            ));
            if let Err(error) = state
                .kernel
                .install_isolated(plugin.clone(), package.granted.clone())
            {
                package.enabled = false;
                state.errors.insert(id.clone(), error.to_string());
                state.packages.insert(id, package);
                continue;
            }
            state.plugins.insert(id.clone(), plugin);
            state.packages.insert(id, package);
        }
        let (event_tx, mut events) = tokio::sync::mpsc::channel::<(String, serde_json::Value)>(128);
        let observer_errors = Arc::new(StdMutex::new(Vec::new()));
        let errors = observer_errors.clone();
        let context = state
            .kernel
            .context("virtade-event-relay")
            .map_err(|error| error.to_string())?;
        let event_task = tokio::spawn(async move {
            while let Some((provider, payload)) = events.recv().await {
                let topics = if matches!(provider.as_str(), "codex" | "claude" | "dsh") {
                    vec![
                        format!("virtade.agent.{provider}.event.v1"),
                        "virtade.agent.event.v1".into(),
                    ]
                } else {
                    vec!["virtade.agent.event.v1".into()]
                };
                for topic in topics {
                    if let Ok(failures) = context
                        .notify(
                            &topic,
                            payload.clone(),
                            std::time::Duration::from_millis(500),
                        )
                        .await
                    {
                        let mut errors = errors.lock().expect("observer errors");
                        for error in failures {
                            if errors.len() == 32 {
                                errors.remove(0);
                            }
                            errors.push(error.to_string());
                        }
                    }
                }
            }
        });
        let manager = Arc::new(Self {
            root,
            database,
            engine,
            state: Mutex::new(state),
            ui_queues: StdMutex::new(BTreeMap::new()),
            observer_errors,
            event_tx,
            event_task: StdMutex::new(Some(event_task)),
            pending_enabled: Mutex::new(enabled),
        });
        Ok(manager)
    }
    /// Restore third-party plugins after all built-in services have been registered.
    pub async fn activate_saved(&self) {
        let enabled = std::mem::take(&mut *self.pending_enabled.lock().await);
        for id in enabled {
            if let Err(error) = self.enable(&id).await {
                self.state.lock().await.errors.insert(id, error);
            }
        }
    }
    async fn persist(&self, state: &ManagerState) -> PackageResult<()> {
        self.database
            .set_setting(
                "host.plugins".into(),
                "installed".into(),
                serde_json::to_value(state.packages.values().collect::<Vec<_>>())
                    .map_err(|error| error.to_string())?,
            )
            .await
            .map_err(|error| error.to_string())
    }
    pub async fn inspect(source: PathBuf) -> PackageResult<Manifest> {
        tokio::task::spawn_blocking(move || {
            read_package(&source).map(|(manifest, _, _, _)| manifest)
        })
        .await
        .map_err(|error| error.to_string())?
    }
    pub async fn install(
        &self,
        source: PathBuf,
        granted: Permissions,
    ) -> PackageResult<InstalledPlugin> {
        // Serialize installation before creating files, including duplicate checks.
        let mut state = self.state.lock().await;
        let installed: std::collections::BTreeSet<_> = state.packages.keys().cloned().collect();
        let root = self.root.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            let (manifest, metadata, files, digest) = read_package(&source)?;
            if installed.contains(&manifest.id) {
                return Err(
                    "plugin already installed; uninstall the inactive version first".into(),
                );
            }
            let directory = root.join(format!(
                "{}-{}-{}",
                manifest.id,
                manifest.version,
                &digest[..16]
            ));
            if !directory.exists() {
                let stage = root.join(format!(".staging-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir(&stage).map_err(|error| error.to_string())?;
                let result = (|| {
                    for (name, bytes) in files {
                        let path = stage.join(name);
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                        }
                        std::fs::write(path, bytes).map_err(|error| error.to_string())?;
                    }
                    std::fs::rename(&stage, &directory).map_err(|error| error.to_string())
                })();
                if result.is_err() {
                    let _ = std::fs::remove_dir_all(&stage);
                }
                result?;
            }
            Ok::<_, String>(InstalledPlugin {
                manifest,
                metadata,
                digest,
                directory,
                granted,
                enabled: false,
            })
        })
        .await
        .map_err(|error| error.to_string())??;
        let id = prepared.manifest.id.clone();
        if state.packages.contains_key(&id) {
            return Err("plugin already installed; uninstall the inactive version first".into());
        }
        let plugin = Arc::new(WasmPlugin::new(
            prepared.manifest.clone(),
            prepared.directory.join(&prepared.metadata.component),
            self.engine.clone(),
            self.database.clone(),
        ));
        state
            .kernel
            .install_isolated(plugin.clone(), prepared.granted.clone())
            .map_err(|error| error.to_string())?;
        state.plugins.insert(id.clone(), plugin);
        state.packages.insert(id.clone(), prepared.clone());
        if let Err(error) = self.persist(&state).await {
            state.packages.remove(&id);
            state.plugins.remove(&id);
            let _ = state.kernel.forget(&id);
            return Err(error);
        }
        Ok(prepared)
    }
    pub async fn enable(&self, id: &str) -> PackageResult<()> {
        let mut state = self.state.lock().await;
        let package = state.packages.get(id).ok_or("plugin not installed")?;
        let (_, _, _, digest) = read_package(&package.directory)?;
        if digest != package.digest {
            return Err("plugin integrity changed since installation".into());
        }
        state
            .kernel
            .start(id)
            .await
            .map_err(|error| error.to_string())?;
        let ids: Vec<_> = state.packages.keys().cloned().collect();
        for id in ids {
            if state.kernel.state(&id).map_err(|error| error.to_string())? == PluginState::Active {
                state.packages.get_mut(&id).expect("package").enabled = true;
                state
                    .generations
                    .entry(id)
                    .or_insert_with(|| uuid::Uuid::new_v4().to_string());
            }
        }
        self.persist(&state).await
    }
    pub async fn disable(&self, id: &str) -> PackageResult<()> {
        let mut state = self.state.lock().await;
        let result = state
            .kernel
            .unload(id, UnloadMode::RefuseDependents)
            .await
            .map_err(|error| error.to_string());
        if state
            .kernel
            .state(id)
            .is_ok_and(|state| state != PluginState::Active)
        {
            if let Some(package) = state.packages.get_mut(id) {
                package.enabled = false;
            }
            state.generations.remove(id);
            self.ui_queues.lock().expect("UI queues").remove(id);
            self.persist(&state).await?;
        }
        result
    }
    pub async fn uninstall(&self, id: &str) -> PackageResult<()> {
        let disabled = self.disable(id).await;
        let mut state = self.state.lock().await;
        if state
            .kernel
            .state(id)
            .is_ok_and(|state| state == PluginState::Active)
        {
            return disabled;
        }
        if state.kernel.state(id).is_ok() {
            state.kernel.forget(id).map_err(|error| error.to_string())?;
        }
        let package = state.packages.remove(id).ok_or("plugin not installed")?;
        state.plugins.remove(id);
        state.errors.remove(id);
        state.generations.remove(id);
        self.ui_queues.lock().expect("UI queues").remove(id);
        self.persist(&state).await?;
        if package.directory.parent() == Some(self.root.as_path()) && package.directory.exists() {
            std::fs::remove_dir_all(package.directory).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    pub async fn reload(&self, id: &str) -> PackageResult<()> {
        self.disable(id).await?;
        self.enable(id).await
    }
    pub async fn snapshot(&self) -> Vec<PluginSnapshot> {
        let state = self.state.lock().await;
        state
            .packages
            .values()
            .map(|package| PluginSnapshot {
                package: package.clone(),
                state: format!(
                    "{:?}",
                    state
                        .kernel
                        .state(&package.manifest.id)
                        .unwrap_or(PluginState::Failed)
                ),
                generation: state.generations.get(&package.manifest.id).cloned(),
                diagnostics: state
                    .errors
                    .get(&package.manifest.id)
                    .cloned()
                    .into_iter()
                    .chain(
                        state
                            .plugins
                            .get(&package.manifest.id)
                            .map(|plugin| plugin.diagnostics())
                            .unwrap_or_default(),
                    )
                    .collect(),
            })
            .collect()
    }
    pub async fn resource_counts(&self) -> serde_json::Value {
        let state = self.state.lock().await;
        let resources = state.kernel.resources();
        serde_json::json!({"services":resources.service_implementations,"listeners":resources.listeners,"tasks":resources.tasks,"contributions":state.kernel.contributions().len()})
    }
    pub async fn contributions(&self) -> Vec<(String, Contribution)> {
        self.state.lock().await.kernel.contributions()
    }
    pub async fn ui(&self, id: &str, generation: &str) -> PackageResult<String> {
        let state = self.state.lock().await;
        if state.generations.get(id).map(String::as_str) != Some(generation) {
            return Err("stale UI plugin generation".into());
        }
        let package = state.packages.get(id).ok_or("plugin missing")?;
        String::from_utf8(entry(
            &package.directory,
            package
                .metadata
                .ui
                .as_deref()
                .ok_or("plugin has no UI entry")?,
            1024 * 1024,
        )?)
        .map_err(|error| error.to_string())
    }
    pub async fn call(
        &self,
        id: &str,
        generation: &str,
        key: ServiceKey,
        request: serde_json::Value,
    ) -> PackageResult<serde_json::Value> {
        let context = {
            let state = self.state.lock().await;
            if state.generations.get(id).map(String::as_str) != Some(generation) {
                return Err("stale plugin generation".into());
            }
            state
                .kernel
                .context(id)
                .map_err(|error| error.to_string())?
        };
        context
            .call(&key, request, std::time::Duration::from_secs(2))
            .await
            .map_err(|error| error.to_string())
    }
    pub async fn config_get(
        &self,
        id: &str,
        generation: &str,
        key: String,
    ) -> PackageResult<Option<serde_json::Value>> {
        let state = self.state.lock().await;
        if state.generations.get(id).map(String::as_str) != Some(generation) {
            return Err("stale plugin generation".into());
        }
        state
            .kernel
            .context(id)
            .map_err(|error| error.to_string())?
            .granted(Capability::ConfigRead)
            .map_err(|error| error.to_string())?;
        self.database
            .get_setting(format!("plugin:{id}"), key)
            .await
            .map_err(|error| error.to_string())
    }
    pub async fn config_set(
        &self,
        id: &str,
        generation: &str,
        key: String,
        value: serde_json::Value,
    ) -> PackageResult<()> {
        let state = self.state.lock().await;
        if state.generations.get(id).map(String::as_str) != Some(generation) {
            return Err("stale plugin generation".into());
        }
        state
            .kernel
            .context(id)
            .map_err(|error| error.to_string())?
            .granted(Capability::ConfigWrite)
            .map_err(|error| error.to_string())?;
        self.database
            .set_setting(format!("plugin:{id}"), key, value)
            .await
            .map_err(|error| error.to_string())
    }
    pub async fn shutdown(&self) {
        let event_task = self.event_task.lock().expect("event task").take();
        if let Some(task) = event_task {
            task.abort();
            let _ = task.await;
        }
        let mut state = self.state.lock().await;
        let _ = state.kernel.shutdown().await;
        state.generations.clear();
    }

    /// Agent ingestion never waits for third-party observers. This notification stream is lossy;
    /// durable auditing uses the SQLite event cursor instead.
    pub async fn publish(&self, provider: &str, payload: serde_json::Value) {
        if self.event_tx.try_send((provider.into(), payload)).is_err() {
            let mut log = self.observer_errors.lock().expect("observer errors");
            if log.len() == 32 {
                log.remove(0);
            }
            log.push("Plugin observer queue full or closed; notification dropped".into());
        }
    }
    pub fn diagnostics(&self) -> Vec<String> {
        self.observer_errors
            .lock()
            .expect("observer errors")
            .clone()
    }
    pub async fn ui_request(
        &self,
        id: &str,
        generation: &str,
        request: UiRequest,
    ) -> PackageResult<serde_json::Value> {
        match request {
            UiRequest::Contribute { definition } => {
                let state = self.state.lock().await;
                if state.generations.get(id).map(String::as_str) != Some(generation) {
                    return Err("stale plugin generation".into());
                }
                state
                    .kernel
                    .context(id)
                    .map_err(|error| error.to_string())?
                    .contribute(definition)
                    .map_err(|error| error.to_string())?;
                Ok(serde_json::Value::Null)
            }
            UiRequest::Resource { request } => {
                let context = {
                    let state = self.state.lock().await;
                    if state.generations.get(id).map(String::as_str) != Some(generation) {
                        return Err("stale plugin generation".into());
                    }
                    state
                        .kernel
                        .context(id)
                        .map_err(|error| error.to_string())?
                };
                crate::broker::execute(context, request, self.engine.transports.clone()).await
            }
            UiRequest::ServiceCall { key, request } => {
                self.call(id, generation, key, request).await
            }
            UiRequest::ConfigGet { key } => Ok(serde_json::json!(
                self.config_get(id, generation, key).await?
            )),
            UiRequest::ConfigSet { key, value } => {
                self.config_set(id, generation, key, value).await?;
                Ok(serde_json::Value::Null)
            }
            UiRequest::EventSubscribe { topic } => {
                let context = {
                    let state = self.state.lock().await;
                    if state.generations.get(id).map(String::as_str) != Some(generation) {
                        return Err("stale plugin generation".into());
                    }
                    state
                        .kernel
                        .context(id)
                        .map_err(|error| error.to_string())?
                };
                context
                    .granted(Capability::EventSubscribe(topic.clone()))
                    .map_err(|error| error.to_string())?;
                let queue = self
                    .ui_queues
                    .lock()
                    .expect("UI queues")
                    .entry(id.into())
                    .or_default()
                    .clone();
                let mut guard = queue.lock().expect("UI queue");
                if !guard.topics.contains(&topic) {
                    let name = topic.clone();
                    let callback_queue = queue.clone();
                    context.on(&topic, 0, move |payload| {
                        let queue = callback_queue.clone();
                        let topic = name.clone();
                        async move {
                            let mut queue = queue.lock().expect("UI queue");
                            queue.next += 1;
                            let sequence = queue.next;
                            if queue.events.len() == 128 { queue.events.pop_front(); queue.dropped += 1; }
                            queue.events.push_back(serde_json::json!({"sequence":sequence,"topic":topic,"payload":payload}));
                            Ok(Flow::Continue)
                        }
                    }).map_err(|error| error.to_string())?;
                    guard.topics.insert(topic);
                }
                Ok(serde_json::Value::Null)
            }
            UiRequest::EventPoll { after } => {
                let state = self.state.lock().await;
                if state.generations.get(id).map(String::as_str) != Some(generation) {
                    return Err("stale plugin generation".into());
                }
                let queues = self.ui_queues.lock().expect("UI queues");
                let Some(queue) = queues.get(id) else {
                    return Ok(serde_json::json!({"events":[],"dropped":0}));
                };
                let queue = queue.lock().expect("UI queue");
                Ok(
                    serde_json::json!({"events":queue.events.iter().filter(|event|event["sequence"].as_u64().unwrap_or(0)>after).collect::<Vec<_>>(),"dropped":queue.dropped}),
                )
            }
        }
    }
}

impl Drop for PluginManager {
    fn drop(&mut self) {
        if let Ok(mut task) = self.event_task.lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }
}
