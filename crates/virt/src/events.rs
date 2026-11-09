use crate::{Context, scope::Scope};
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use virt_protocol::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Intercept,
}

type Handler = Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Flow>> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct Listener {
    id: u64,
    priority: i32,
    scope: Scope,
    handler: Handler,
}

impl Context {
    /// Non-intercepting notifications isolate observer faults from the publishing operation.
    pub async fn notify(
        &self,
        topic: &str,
        payload: Value,
        deadline: Duration,
    ) -> Result<Vec<Error>> {
        self.granted(virt_protocol::Capability::EventPublish(topic.into()))?;
        let listeners = self
            .host
            .listeners
            .lock()
            .expect("event lock")
            .get(topic)
            .cloned()
            .unwrap_or_default();
        let tasks = listeners.into_iter().map(|listener| {
            let payload = payload.clone();
            async move {
                match listener
                    .scope
                    .spawn(async move { (listener.handler)(payload).await })
                {
                    Ok(task) => task.wait(deadline).await.map(|_| ()),
                    Err(Error::ScopeClosed(_)) => Ok(()),
                    Err(error) => Err(error),
                }
            }
        });
        Ok(futures_util::future::join_all(tasks)
            .await
            .into_iter()
            .filter_map(std::result::Result::err)
            .collect())
    }
    pub(crate) fn register_listener(
        &self,
        topic: &str,
        priority: i32,
        handler: Handler,
    ) -> Result<()> {
        self.scope.register(|| {
            let id = self.host.next_id();
            let topic = topic.to_owned();
            self.host
                .listeners
                .lock()
                .expect("event lock")
                .entry(topic.clone())
                .or_default()
                .push(Listener {
                    id,
                    priority,
                    scope: self.scope.clone(),
                    handler,
                });
            let host = Arc::downgrade(&self.host);
            Ok((
                (),
                Box::new(move || {
                    if let Some(host) = host.upgrade() {
                        let mut listeners = host.listeners.lock().expect("event lock");
                        if let Some(entries) = listeners.get_mut(&topic) {
                            entries.retain(|listener| listener.id != id);
                            if entries.is_empty() {
                                listeners.remove(&topic);
                            }
                        }
                    }
                }) as Box<dyn FnOnce() + Send>,
            ))
        })
    }

    pub(crate) async fn dispatch(
        &self,
        topic: &str,
        payload: Value,
        deadline: Duration,
    ) -> Result<Flow> {
        let mut listeners = self
            .host
            .listeners
            .lock()
            .expect("event lock")
            .get(topic)
            .cloned()
            .unwrap_or_default();
        listeners.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
        for listener in listeners {
            let payload = payload.clone();
            // Invoke the closure itself inside the task, so construction panics are caught too.
            let task = listener
                .scope
                .spawn(async move { (listener.handler)(payload).await });
            match task {
                Err(Error::ScopeClosed(_)) => continue,
                Err(error) => return Err(error),
                Ok(task) => match task.wait(deadline).await? {
                    Flow::Intercept => return Ok(Flow::Intercept),
                    Flow::Continue => (),
                },
            }
        }
        Ok(Flow::Continue)
    }
}
