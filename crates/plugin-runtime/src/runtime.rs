//! Versioned WIT boundary with per-instance limits and revocable native resource registration.
use async_trait::async_trait;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use virt::{Context, Contribution, Flow, Plugin, Registration, Service};
use virt_protocol::{Capability, Error, Manifest, Result, ServiceKey};
use virtade_storage::Store as Database;
use wasmtime::{
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
    component::{Component, Linker},
};

mod bindings {
    wasmtime::component::bindgen!({path:"wit",world:"plugin",imports:{default:async},exports:{default:async}});
}

pub struct ComponentEngine {
    pub(crate) transports: Arc<crate::transport::Transports>,
    engine: Engine,
    cancel: CancellationToken,
}
impl ComponentEngine {
    pub fn new() -> anyhow::Result<Arc<Self>> {
        let mut config = Config::new();
        config
            .wasm_component_model(true)
            .consume_fuel(true)
            .epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let cancel = CancellationToken::new();
        let ticks = engine.clone();
        let stop = cancel.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(10));
            loop {
                tokio::select! {_=stop.cancelled()=>break,_=interval.tick()=>ticks.increment_epoch()}
            }
        });
        Ok(Arc::new(Self {
            engine,
            cancel,
            transports: Arc::default(),
        }))
    }
}
impl Drop for ComponentEngine {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Clone)]
pub struct WasmPlugin {
    manifest: Manifest,
    component: PathBuf,
    engine: Arc<ComponentEngine>,
    database: Database,
    instance: Arc<Mutex<Option<Arc<Guest>>>>,
    log: Arc<Mutex<Vec<String>>>,
}
impl WasmPlugin {
    pub fn new(
        manifest: Manifest,
        component: PathBuf,
        engine: Arc<ComponentEngine>,
        database: Database,
    ) -> Self {
        Self {
            manifest,
            component,
            engine,
            database,
            instance: Arc::default(),
            log: Arc::default(),
        }
    }
    pub fn diagnostics(&self) -> Vec<String> {
        self.log.lock().expect("wasm log").clone()
    }
    fn failure(&self, error: impl std::fmt::Display) -> Error {
        failure(&self.manifest.id, error)
    }
}
fn failure(owner: &str, error: impl std::fmt::Display) -> Error {
    Error::PluginFailure {
        plugin: owner.into(),
        message: error.to_string(),
    }
}
fn bounded(text: &str) -> std::result::Result<(), String> {
    if text.len() > 1024 * 1024 {
        Err("WIT payload exceeds 1 MiB".into())
    } else {
        Ok(())
    }
}

struct HostState {
    transports: Arc<crate::transport::Transports>,
    context: Context,
    instance: Arc<OnceLock<Weak<Guest>>>,
    limits: StoreLimits,
    database: Database,
    log: Arc<Mutex<Vec<String>>>,
}
impl HostState {
    fn guest(&self) -> std::result::Result<Arc<Guest>, String> {
        self.instance
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| "guest is not activated".into())
    }
}
struct Guest {
    owner: String,
    state: AsyncMutex<(Store<HostState>, bindings::Plugin)>,
}
impl Guest {
    async fn service(&self, name: &str, request: &str) -> Result<String> {
        bounded(request).map_err(|error| failure(&self.owner, error))?;
        let mut guard = self
            .state
            .try_lock()
            .map_err(|_| failure(&self.owner, "plugin busy or reentrant call"))?;
        let (store, plugin) = &mut *guard;
        store
            .set_fuel(10_000_000)
            .map_err(|error| failure(&self.owner, error))?;
        store.set_epoch_deadline(200);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            plugin.call_service(store, name, request),
        )
        .await
        .map_err(|_| Error::Timeout(self.owner.clone()))?;
        result
            .map_err(|error| failure(&self.owner, error))?
            .map_err(|error| failure(&self.owner, error))
    }
    async fn event(&self, topic: &str, payload: &str) -> Result<bool> {
        let mut guard = self
            .state
            .try_lock()
            .map_err(|_| failure(&self.owner, "plugin busy or reentrant event"))?;
        let (store, plugin) = &mut *guard;
        store
            .set_fuel(10_000_000)
            .map_err(|error| failure(&self.owner, error))?;
        store.set_epoch_deadline(200);
        tokio::time::timeout(
            Duration::from_secs(2),
            plugin.call_event(store, topic, payload),
        )
        .await
        .map_err(|_| Error::Timeout(self.owner.clone()))?
        .map_err(|error| failure(&self.owner, error))?
        .map_err(|error| failure(&self.owner, error))
    }
}
struct GuestService {
    guest: Arc<Guest>,
    name: String,
}
#[async_trait]
impl Service for GuestService {
    async fn call(&self, request: Value) -> Result<Value> {
        let response = self.guest.service(&self.name, &request.to_string()).await?;
        serde_json::from_str(&response).map_err(|error| failure(&self.guest.owner, error))
    }
}

#[async_trait]
impl Plugin for WasmPlugin {
    fn manifest(&self) -> Manifest {
        self.manifest.clone()
    }
    async fn start(&self, context: Context) -> Result<()> {
        let path = self.component.clone();
        let engine = self.engine.engine.clone();
        let component = tokio::task::spawn_blocking(move || -> anyhow::Result<Component> {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(path)?
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes.len() <= 16 * 1024 * 1024, "component exceeds 16 MiB");
            Component::new(&engine, bytes).map_err(|error| anyhow::anyhow!(error.to_string()))
        })
        .await
        .map_err(|error| self.failure(error))?
        .map_err(|error| self.failure(error))?;
        let slot = Arc::new(OnceLock::new());
        let state = HostState {
            transports: self.engine.transports.clone(),
            context,
            instance: slot.clone(),
            database: self.database.clone(),
            log: self.log.clone(),
            limits: StoreLimitsBuilder::new()
                .memory_size(32 * 1024 * 1024)
                .table_elements(10_000)
                .instances(32)
                .memories(4)
                .tables(4)
                .build(),
        };
        let mut store = Store::new(&self.engine.engine, state);
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(10_000_000)
            .map_err(|error| self.failure(error))?;
        store.set_epoch_deadline(200);
        let mut linker = Linker::new(&self.engine.engine);
        bindings::Plugin::add_to_linker::<HostState, wasmtime::component::HasSelf<HostState>>(
            &mut linker,
            |state: &mut HostState| state,
        )
        .map_err(|error| self.failure(error))?;
        let plugin = bindings::Plugin::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|error| self.failure(error))?;
        let guest = Arc::new(Guest {
            owner: self.manifest.id.clone(),
            state: AsyncMutex::new((store, plugin)),
        });
        slot.set(Arc::downgrade(&guest))
            .map_err(|_| self.failure("activation slot already filled"))?;
        {
            let mut state = guest.state.lock().await;
            let (store, plugin) = &mut *state;
            tokio::time::timeout(Duration::from_secs(2), plugin.call_activate(store))
                .await
                .map_err(|_| Error::Timeout(self.manifest.id.clone()))?
                .map_err(|error| self.failure(error))?
                .map_err(|error| self.failure(error))?;
        }
        *self.instance.lock().expect("instance") = Some(guest);
        Ok(())
    }
    async fn stop(&self, _: Context) -> Result<()> {
        let guest = self.instance.lock().expect("instance").take();
        if let Some(guest) = guest {
            let mut state = guest.state.lock().await;
            let (store, plugin) = &mut *state;
            store
                .set_fuel(1_000_000)
                .map_err(|error| self.failure(error))?;
            store.set_epoch_deadline(100);
            tokio::time::timeout(Duration::from_secs(1), plugin.call_deactivate(store))
                .await
                .map_err(|_| Error::Timeout(self.manifest.id.clone()))?
                .map_err(|error| self.failure(error))?
                .map_err(|error| self.failure(error))?;
        }
        Ok(())
    }
}

impl bindings::virt::plugin::host::Host for HostState {
    async fn resource_request(&mut self, request: String) -> std::result::Result<String, String> {
        bounded(&request)?;
        let request = serde_json::from_str(&request).map_err(|error| error.to_string())?;
        crate::broker::execute(self.context.clone(), request, self.transports.clone())
            .await
            .map(|value| value.to_string())
    }
    async fn service_register(
        &mut self,
        name: String,
        major: u32,
        replaceable: bool,
        replace: bool,
    ) -> std::result::Result<(), String> {
        let guest = self.guest()?;
        let key = ServiceKey::new(name.clone(), major);
        self.context
            .provide(
                key,
                Arc::new(GuestService { guest, name }),
                if replace {
                    Registration::Replace
                } else {
                    Registration::New { replaceable }
                },
            )
            .map_err(|error| error.to_string())
    }
    async fn service_call(
        &mut self,
        name: String,
        major: u32,
        request: String,
    ) -> std::result::Result<String, String> {
        bounded(&request)?;
        let value = serde_json::from_str(&request).map_err(|error| error.to_string())?;
        self.context
            .call(
                &ServiceKey::new(name, major),
                value,
                Duration::from_millis(750),
            )
            .await
            .map(|value| value.to_string())
            .map_err(|error| error.to_string())
    }
    async fn event_subscribe(
        &mut self,
        topic: String,
        priority: i32,
    ) -> std::result::Result<(), String> {
        let guest = self.guest()?;
        let name = topic.clone();
        self.context
            .on(&topic, priority, move |payload| {
                let guest = guest.clone();
                let topic = name.clone();
                async move {
                    Ok(if guest.event(&topic, &payload.to_string()).await? {
                        Flow::Intercept
                    } else {
                        Flow::Continue
                    })
                }
            })
            .map_err(|error| error.to_string())
    }
    async fn event_emit(
        &mut self,
        topic: String,
        payload: String,
    ) -> std::result::Result<(), String> {
        bounded(&payload)?;
        let value = serde_json::from_str(&payload).map_err(|error| error.to_string())?;
        self.context
            .emit(&topic, value, Duration::from_millis(750))
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    async fn contribute(&mut self, definition: String) -> std::result::Result<(), String> {
        bounded(&definition)?;
        let value: Contribution =
            serde_json::from_str(&definition).map_err(|error| error.to_string())?;
        self.context
            .contribute(value)
            .map_err(|error| error.to_string())
    }
    async fn config_get(&mut self, key: String) -> std::result::Result<Option<String>, String> {
        self.context
            .granted(Capability::ConfigRead)
            .map_err(|error| error.to_string())?;
        self.database
            .get_setting(format!("plugin:{}", self.context.owner()), key)
            .await
            .map(|value| value.map(|value| value.to_string()))
            .map_err(|error| error.to_string())
    }
    async fn config_set(&mut self, key: String, value: String) -> std::result::Result<(), String> {
        self.context
            .granted(Capability::ConfigWrite)
            .map_err(|error| error.to_string())?;
        bounded(&value)?;
        let value = serde_json::from_str(&value).map_err(|error| error.to_string())?;
        self.database
            .set_setting(format!("plugin:{}", self.context.owner()), key, value)
            .await
            .map_err(|error| error.to_string())
    }
    async fn log(&mut self, message: String) {
        let mut log = self.log.lock().expect("log");
        if log.len() == 32 {
            log.remove(0);
        }
        log.push(message.chars().take(1024).collect());
    }
}
