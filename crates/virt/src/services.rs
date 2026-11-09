use crate::{Context, scope::Scope};
use async_trait::async_trait;
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use virt_protocol::{Error, Result, ServiceKey};

#[derive(Clone)]
pub struct Caller {
    pub owner: String,
    pub trusted: bool,
    pub grants: virt_protocol::Permissions,
}

#[async_trait]
pub trait Service: Send + Sync + 'static {
    async fn call(&self, request: Value) -> Result<Value>;
    async fn call_scoped(&self, request: Value, _caller: Caller) -> Result<Value> {
        self.call(request).await
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Registration {
    New { replaceable: bool },
    Replace,
}

#[derive(Clone)]
pub(crate) struct Entry {
    id: u64,
    replaceable: bool,
    private: bool,
    scope: Scope,
    service: Arc<dyn Service>,
}

impl Context {
    pub(crate) fn register_service(
        &self,
        key: ServiceKey,
        service: Arc<dyn Service>,
        mode: Registration,
        private: bool,
    ) -> Result<()> {
        self.scope.register(|| {
            let mut registry = self.host.services.lock().expect("service lock");
            let stack = registry.entry(key.clone()).or_default();
            if stack.last().is_some_and(|entry| entry.private) && !self.trusted {
                return Err(Error::PermissionDenied {
                    plugin: self.owner.clone(),
                    capability: virt_protocol::Capability::ServiceReplace(key),
                });
            }
            let replaceable = match (mode, stack.last()) {
                (Registration::New { replaceable }, None) => replaceable,
                (Registration::New { .. }, Some(_)) => return Err(Error::ServiceConflict(key)),
                (Registration::Replace, None) => {
                    registry.remove(&key);
                    return Err(Error::ServiceUnavailable(key));
                }
                (Registration::Replace, Some(entry)) if !entry.replaceable => {
                    return Err(Error::NotReplaceable(key));
                }
                (Registration::Replace, Some(entry)) => entry.replaceable,
            };
            let id = self.host.next_id();
            stack.push(Entry {
                id,
                replaceable,
                private,
                scope: self.scope.clone(),
                service,
            });
            let host = Arc::downgrade(&self.host);
            Ok((
                (),
                Box::new(move || {
                    if let Some(host) = host.upgrade() {
                        let mut registry = host.services.lock().expect("service lock");
                        if let Some(stack) = registry.get_mut(&key) {
                            stack.retain(|entry| entry.id != id);
                            if stack.is_empty() {
                                registry.remove(&key);
                            }
                        }
                    }
                }) as Box<dyn FnOnce() + Send>,
            ))
        })
    }

    pub(crate) async fn call_service(
        &self,
        key: &ServiceKey,
        request: Value,
        deadline: Duration,
    ) -> Result<Value> {
        let entry = self
            .host
            .services
            .lock()
            .expect("service lock")
            .get(key)
            .and_then(|stack| stack.last())
            .cloned()
            .ok_or_else(|| Error::ServiceUnavailable(key.clone()))?;
        if entry.private && !self.trusted {
            return Err(Error::PermissionDenied {
                plugin: self.owner.clone(),
                capability: virt_protocol::Capability::ServiceCall(key.clone()),
            });
        }
        // The registry guard is gone before invoking foreign code or taking a scope lock.
        let caller = Caller {
            owner: self.owner.clone(),
            trusted: self.trusted,
            grants: self.grants.clone(),
        };
        entry
            .scope
            .spawn(async move { entry.service.call_scoped(request, caller).await })?
            .wait(deadline)
            .await
    }
}
