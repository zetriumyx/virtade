//! Plugin kernel: revocable contexts for trusted built-ins and isolated component adapters.
mod contributions;
mod events;
mod lifecycle;
mod scope;
mod services;
pub use contributions::{Contribution, ContributionKind};

pub use async_trait::async_trait;
pub use events::Flow;
pub use lifecycle::{Kernel, Plugin, PluginState, UnloadMode};
pub use services::Caller;
pub use services::{Registration, Service};
pub use virt_protocol as protocol;

use scope::Scope;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use virt_protocol::{Capability, Error, Permissions, Result, ServiceKey};

#[derive(Default)]
pub(crate) struct Host {
    services: Mutex<BTreeMap<ServiceKey, Vec<services::Entry>>>,
    listeners: Mutex<BTreeMap<String, Vec<events::Listener>>>,
    next: AtomicU64,
    contributions: Mutex<BTreeMap<String, contributions::Entry>>,
}

impl Host {
    fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

/// A generation-specific, revocable facade; construction and registries are private.
#[derive(Clone)]
pub struct Context {
    owner: String,
    grants: Permissions,
    trusted: bool,
    scope: Scope,
    host: Arc<Host>,
}

impl Context {
    /// Trusted native cleanup hook. Guests never receive arbitrary closures or OS handles.
    pub fn defer(&self, cleanup: impl FnOnce() + Send + 'static) -> Result<()> {
        self.scope
            .register(|| Ok(((), Box::new(cleanup) as Box<dyn FnOnce() + Send>)))
    }

    pub fn granted(&self, capability: Capability) -> Result<()> {
        self.require(capability)
    }
    pub fn owner(&self) -> &str {
        &self.owner
    }

    fn require(&self, capability: Capability) -> Result<()> {
        self.scope.ensure_open()?;
        if self.grants.contains(&capability) {
            Ok(())
        } else {
            Err(Error::PermissionDenied {
                plugin: self.owner.clone(),
                capability,
            })
        }
    }

    pub fn provide(
        &self,
        key: ServiceKey,
        service: Arc<dyn Service>,
        mode: Registration,
    ) -> Result<()> {
        self.require(Capability::ServiceProvide(key.clone()))?;
        if matches!(mode, Registration::Replace) {
            self.require(Capability::ServiceReplace(key.clone()))?;
        }
        self.register_service(key, service, mode, false)
    }

    pub fn provide_private(&self, key: ServiceKey, service: Arc<dyn Service>) -> Result<()> {
        self.require(Capability::ServiceProvide(key.clone()))?;
        if !self.trusted {
            return Err(Error::PermissionDenied {
                plugin: self.owner.clone(),
                capability: Capability::ServiceProvide(key),
            });
        }
        self.register_service(key, service, Registration::New { replaceable: false }, true)
    }

    pub async fn call(
        &self,
        key: &ServiceKey,
        request: Value,
        deadline: Duration,
    ) -> Result<Value> {
        self.require(Capability::ServiceCall(key.clone()))?;
        tokio::select! {
            biased;
            _ = self.scope.cancelled.cancelled() => Err(Error::ScopeClosed(self.owner.clone())),
            result = self.call_service(key, request, deadline) => result,
        }
    }

    pub fn on<F, Fut>(&self, topic: &str, priority: i32, handler: F) -> Result<()>
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Flow>> + Send + 'static,
    {
        self.require(Capability::EventSubscribe(topic.into()))?;
        self.register_listener(
            topic,
            priority,
            Arc::new(move |payload| Box::pin(handler(payload))),
        )
    }

    /// A synchronous callback adapter. Callbacks must be short and nonblocking.
    pub fn hook<F>(&self, topic: &str, priority: i32, handler: F) -> Result<()>
    where
        F: Fn(Value) -> Result<Flow> + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        self.on(topic, priority, move |payload| {
            let handler = handler.clone();
            async move { handler(payload) }
        })
    }

    /// Ordered dispatch with a total deadline; a slow hook cannot extend it per listener.
    pub async fn emit(&self, topic: &str, payload: Value, deadline: Duration) -> Result<Flow> {
        self.require(Capability::EventPublish(topic.into()))?;
        tokio::select! {
            biased;
            _ = self.scope.cancelled.cancelled() => Err(Error::ScopeClosed(self.owner.clone())),
            result = tokio::time::timeout(deadline, self.dispatch(topic, payload, deadline)) =>
                result.unwrap_or_else(|_| Err(Error::Timeout(self.owner.clone()))),
        }
    }

    /// Only async, cooperative work is supported. Do not detach threads/processes here.
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) -> Result<()> {
        self.scope.background(future)
    }

    /// Bounded diagnostic history for background panics in this generation.
    pub fn background_failures(&self) -> Vec<Error> {
        self.scope.failures()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resources {
    pub service_implementations: usize,
    pub listeners: usize,
    pub tasks: usize,
}
