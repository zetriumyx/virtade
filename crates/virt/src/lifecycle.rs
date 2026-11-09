use crate::{Context, Host, Resources, scope::Scope};
use async_trait::async_trait;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use virt_protocol::{Error, Manifest, Permissions, Result};

/// Statically linked, trusted plugin interface. Never exported as a Rust ABI.
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    fn manifest(&self) -> Manifest;
    async fn start(&self, context: Context) -> Result<()>;
    /// Called after revocation and task cleanup. Context may be used for identity/diagnostics,
    /// but its registrations and calls are closed. External shutdown must be cooperative.
    async fn stop(&self, _context: Context) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginState {
    Registered,
    Starting,
    Active,
    Stopping,
    Paused,
    Unloaded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnloadMode {
    RefuseDependents,
    Cascade,
}

struct Record {
    plugin: Arc<dyn Plugin>,
    manifest: Manifest,
    grants: Permissions,
    trusted: bool,
    state: PluginState,
    context: Option<Context>,
    last_error: Option<Error>,
}

pub struct Kernel {
    pub(crate) host: Arc<Host>,
    plugins: BTreeMap<String, Record>,
    lifecycle_timeout: Duration,
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new(Duration::from_secs(5))
    }
}

impl Kernel {
    /// Remove an inactive factory. Call unload first to revoke all generation resources.
    pub fn forget(&mut self, id: &str) -> Result<()> {
        if self.record(id)?.context.is_some() {
            return Err(Error::InvalidState(
                "unload before forgetting a plugin".into(),
            ));
        }
        self.plugins.remove(id);
        Ok(())
    }
    pub fn new(lifecycle_timeout: Duration) -> Self {
        Self {
            host: Arc::default(),
            plugins: BTreeMap::new(),
            lifecycle_timeout,
        }
    }

    /// Grants come from the trusted host. A manifest request alone grants nothing.
    pub fn install(&mut self, plugin: Arc<dyn Plugin>, grants: Permissions) -> Result<()> {
        self.install_origin(plugin, grants, true)
    }
    /// Isolated plugins cannot call or replace host-private services, regardless of manifest claims.
    pub fn install_isolated(&mut self, plugin: Arc<dyn Plugin>, grants: Permissions) -> Result<()> {
        self.install_origin(plugin, grants, false)
    }
    fn install_origin(
        &mut self,
        plugin: Arc<dyn Plugin>,
        grants: Permissions,
        trusted: bool,
    ) -> Result<()> {
        let manifest = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| plugin.manifest()))
            .map_err(|_| Error::InvalidManifest("manifest callback panicked".into()))?;
        manifest.validate()?;
        if self.plugins.contains_key(&manifest.id) {
            return Err(Error::DuplicatePlugin(manifest.id));
        }
        let grants = manifest
            .permissions
            .intersection(&grants)
            .cloned()
            .collect();
        self.plugins.insert(
            manifest.id.clone(),
            Record {
                plugin,
                manifest,
                grants,
                trusted,
                state: PluginState::Registered,
                context: None,
                last_error: None,
            },
        );
        Ok(())
    }

    pub fn state(&self, id: &str) -> Result<PluginState> {
        Ok(self.record(id)?.state)
    }
    pub fn last_error(&self, id: &str) -> Result<Option<&Error>> {
        Ok(self.record(id)?.last_error.as_ref())
    }

    /// Host-only access; third-party guests do not receive Kernel or other contexts.
    pub fn context(&self, id: &str) -> Result<Context> {
        self.record(id)?
            .context
            .clone()
            .ok_or_else(|| Error::InvalidState(format!("{id} is not active")))
    }

    pub fn resources(&self) -> Resources {
        Resources {
            service_implementations: self
                .host
                .services
                .lock()
                .expect("service lock")
                .values()
                .map(Vec::len)
                .sum(),
            listeners: self
                .host
                .listeners
                .lock()
                .expect("event lock")
                .values()
                .map(Vec::len)
                .sum(),
            tasks: self
                .plugins
                .values()
                .filter_map(|record| record.context.as_ref())
                .map(|context| context.scope.task_count())
                .sum(),
        }
    }

    fn record(&self, id: &str) -> Result<&Record> {
        self.plugins
            .get(id)
            .ok_or_else(|| Error::PluginNotFound(id.into()))
    }

    fn visit(
        &self,
        id: &str,
        visiting: &mut Vec<String>,
        done: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) -> Result<()> {
        if done.contains(id) {
            return Ok(());
        }
        if let Some(position) = visiting.iter().position(|item| item == id) {
            let mut cycle = visiting[position..].to_vec();
            cycle.push(id.into());
            return Err(Error::DependencyCycle(cycle));
        }
        let record = self.record(id)?;
        visiting.push(id.into());
        let mut dependencies = record.manifest.dependencies.iter().collect::<Vec<_>>();
        dependencies.sort_by(|a, b| a.id.cmp(&b.id));
        for dep in dependencies {
            let target = self
                .plugins
                .get(&dep.id)
                .ok_or_else(|| Error::MissingDependency {
                    plugin: id.into(),
                    dependency: dep.id.clone(),
                })?;
            if !dep.version.matches(&target.manifest.version) {
                return Err(Error::DependencyVersion {
                    plugin: id.into(),
                    dependency: dep.id.clone(),
                });
            }
            self.visit(&dep.id, visiting, done, order)?;
        }
        visiting.pop();
        done.insert(id.into());
        order.push(id.into());
        Ok(())
    }

    pub fn load_order(&self) -> Result<Vec<String>> {
        let (mut done, mut order) = (BTreeSet::new(), Vec::new());
        for id in self.plugins.keys() {
            self.visit(id, &mut vec![], &mut done, &mut order)?;
        }
        Ok(order)
    }

    /// Preflight the entire graph before any side effects. Roll back only new starts.
    pub async fn start_all(&mut self) -> Result<()> {
        let order = self.load_order()?;
        self.start_order(order).await
    }

    /// Start/resume this plugin and its transitive dependencies, lazily.
    pub async fn start(&mut self, id: &str) -> Result<()> {
        let mut order = vec![];
        self.visit(id, &mut vec![], &mut BTreeSet::new(), &mut order)?;
        self.start_order(order).await
    }

    async fn start_order(&mut self, order: Vec<String>) -> Result<()> {
        // Validate interrupted states before starting any plugin in this transaction.
        for id in &order {
            if matches!(
                self.record(id)?.state,
                PluginState::Starting | PluginState::Stopping
            ) {
                return Err(Error::InvalidState(format!(
                    "{id}: interrupted lifecycle; unload before restarting"
                )));
            }
        }
        let mut started: Vec<String> = vec![];
        for id in order {
            if self.record(&id)?.state == PluginState::Active {
                continue;
            }
            if let Err(error) = self.start_one(&id).await {
                for id in started.iter().rev() {
                    let _ = self.stop_one(id, PluginState::Unloaded).await;
                }
                return Err(error);
            }
            started.push(id);
        }
        Ok(())
    }

    async fn start_one(&mut self, id: &str) -> Result<()> {
        let record = self.plugins.get_mut(id).expect("preflight record");
        record.state = PluginState::Starting;
        record.last_error = None;
        let context = Context {
            owner: id.into(),
            grants: record.grants.clone(),
            trusted: record.trusted,
            scope: Scope::new(id.into()),
            host: self.host.clone(),
        };
        // Store before awaiting so dropping a lifecycle future still permits shutdown cleanup.
        record.context = Some(context.clone());
        let mut guard = CloseOnDrop(Some(context.scope.clone()));
        let plugin = record.plugin.clone();
        let start_context = context.clone();
        let result = context
            .scope
            .spawn(async move { plugin.start(start_context).await })?
            .wait(self.lifecycle_timeout)
            .await;
        if let Err(error) = result {
            context.scope.shutdown().await;
            let record = self.plugins.get_mut(id).expect("record");
            record.state = PluginState::Failed;
            record.last_error = Some(error.clone());
            record.context = None;
            return Err(error);
        }
        self.plugins.get_mut(id).expect("record").state = PluginState::Active;
        guard.0 = None;
        Ok(())
    }

    fn stop_order(&self, id: &str, mode: UnloadMode) -> Result<Vec<String>> {
        self.record(id)?;
        let mut selected = BTreeSet::from([id.to_string()]);
        loop {
            let old_len = selected.len();
            for (name, record) in &self.plugins {
                if record.context.is_some()
                    && record
                        .manifest
                        .dependencies
                        .iter()
                        .any(|dep| selected.contains(&dep.id))
                {
                    selected.insert(name.clone());
                }
            }
            if selected.len() == old_len {
                break;
            }
        }
        if mode == UnloadMode::RefuseDependents && selected.len() > 1 {
            return Err(Error::DependentsActive {
                plugin: id.into(),
                dependents: selected.into_iter().filter(|name| name != id).collect(),
            });
        }
        // Only visit the selected subgraph: unrelated broken registrations must not block cleanup.
        let mut order = vec![];
        let mut done = BTreeSet::new();
        for name in &selected {
            self.visit(name, &mut vec![], &mut done, &mut order)?;
        }
        Ok(order
            .into_iter()
            .rev()
            .filter(|name| selected.contains(name))
            .collect())
    }

    pub async fn unload(&mut self, id: &str, mode: UnloadMode) -> Result<()> {
        self.stop_selected(id, mode, PluginState::Unloaded).await
    }

    pub async fn pause(&mut self, id: &str, mode: UnloadMode) -> Result<()> {
        self.stop_selected(id, mode, PluginState::Paused).await
    }

    async fn stop_selected(
        &mut self,
        id: &str,
        mode: UnloadMode,
        target: PluginState,
    ) -> Result<()> {
        let order = self.stop_order(id, mode)?;
        let mut first_error = None;
        for id in order {
            if let Err(error) = self.stop_one(&id, target).await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn stop_one(&mut self, id: &str, target: PluginState) -> Result<()> {
        let record = self.plugins.get_mut(id).expect("record");
        let Some(context) = record.context.clone() else {
            record.state = target;
            return Ok(());
        };
        record.state = PluginState::Stopping;
        let plugin = record.plugin.clone();
        let stop_context = context.clone();
        // Revoke first, so no new operations enter while a stop hook is pending.
        let mut guard = CloseOnDrop(Some(context.scope.clone()));
        context.scope.shutdown().await;
        let stop_scope = Scope::new(id.into());
        let _stop_guard = CloseOnDrop(Some(stop_scope.clone()));
        let result = match stop_scope.spawn(async move { plugin.stop(stop_context).await }) {
            Ok(task) => task.wait(self.lifecycle_timeout).await,
            Err(error) => Err(error),
        };
        // A failed stop hook must never skip forced cleanup.
        stop_scope.shutdown().await;
        guard.0 = None;
        let record = self.plugins.get_mut(id).expect("record");
        record.context = None;
        record.state = if result.is_ok() {
            target
        } else {
            PluginState::Failed
        };
        record.last_error = result.as_ref().err().cloned();
        result
    }

    /// Same factory, fresh scope. Disk package replacement is a P2 concern.
    pub async fn reload(&mut self, id: &str) -> Result<()> {
        self.unload(id, UnloadMode::RefuseDependents).await?;
        self.start(id).await
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        // Installed manifests are immutable, but inactive registrations can be invalid.
        // Clean every live scope even if a graph error prevents orderly stop.
        let order = self.load_order();
        let mut first_error = order.as_ref().err().cloned();
        let ids = order.unwrap_or_else(|_| self.plugins.keys().cloned().collect());
        for id in ids.into_iter().rev() {
            if let Err(error) = self.stop_one(&id, PluginState::Unloaded).await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

// Revocation is synchronous even if an outer lifecycle future is cancelled.
// Interrupted records retain Starting/Stopping and must be unloaded before restart.
struct CloseOnDrop(Option<Scope>);
impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if let Some(scope) = &self.0 {
            scope.close();
        }
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        // Best effort synchronous revocation. Await shutdown() for confirmed task destruction.
        for record in self.plugins.values() {
            if let Some(context) = &record.context {
                context.scope.close();
            }
        }
    }
}
