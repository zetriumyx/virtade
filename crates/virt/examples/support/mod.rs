use semver::VersionReq;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use virt::{Context, Flow, Kernel, Plugin, Registration, Service, UnloadMode, async_trait};
use virt_protocol::{Capability, Dependency, Manifest, Permissions, Result, ServiceKey};

pub fn key() -> ServiceKey {
    ServiceKey::new("example.greeting", 1)
}
pub const TOPIC: &str = "example.message";

struct Greeting(&'static str);
#[async_trait]
impl Service for Greeting {
    async fn call(&self, _request: Value) -> Result<Value> {
        Ok(json!(self.0))
    }
}

pub struct Base;
#[async_trait]
impl Plugin for Base {
    fn manifest(&self) -> Manifest {
        let mut manifest = Manifest::new("base");
        manifest
            .permissions
            .insert(Capability::ServiceProvide(key()));
        manifest
    }
    async fn start(&self, ctx: Context) -> Result<()> {
        ctx.provide(
            key(),
            Arc::new(Greeting("hello from base")),
            Registration::New { replaceable: true },
        )
    }
}

pub struct Consumer {
    pub messages: Arc<Mutex<Vec<Value>>>,
    pub released: Arc<AtomicUsize>,
}

struct TaskResource(Arc<AtomicUsize>);
impl Drop for TaskResource {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Plugin for Consumer {
    fn manifest(&self) -> Manifest {
        let mut manifest = Manifest::new("consumer");
        manifest.dependencies.push(Dependency {
            id: "base".into(),
            version: VersionReq::parse("^0.1").unwrap(),
        });
        manifest.permissions = Permissions::from([
            Capability::ServiceCall(key()),
            Capability::EventPublish(TOPIC.into()),
            Capability::EventSubscribe(TOPIC.into()),
        ]);
        manifest
    }
    async fn start(&self, ctx: Context) -> Result<()> {
        let messages = self.messages.clone();
        ctx.on(TOPIC, 0, move |payload| {
            let messages = messages.clone();
            async move {
                messages.lock().unwrap().push(payload);
                Ok(Flow::Continue)
            }
        })?;
        // Captured resource is released even if task is aborted before its first poll.
        let resource = TaskResource(self.released.clone());
        ctx.spawn(async move {
            let _resource = resource;
            std::future::pending::<()>().await
        })
    }
}

pub struct Override;
#[async_trait]
impl Plugin for Override {
    fn manifest(&self) -> Manifest {
        let mut manifest = Manifest::new("override");
        manifest.dependencies.push(Dependency {
            id: "base".into(),
            version: VersionReq::parse("^0.1").unwrap(),
        });
        manifest.permissions = Permissions::from([
            Capability::ServiceProvide(key()),
            Capability::ServiceReplace(key()),
        ]);
        manifest
    }
    async fn start(&self, ctx: Context) -> Result<()> {
        ctx.provide(
            key(),
            Arc::new(Greeting("hello from override")),
            Registration::Replace,
        )
    }
}

pub async fn scenario() -> Result<()> {
    let mut kernel = Kernel::default();
    let messages = Arc::new(Mutex::new(vec![]));
    let released = Arc::new(AtomicUsize::new(0));
    let plugins: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(Override),
        Arc::new(Consumer {
            messages: messages.clone(),
            released: released.clone(),
        }),
        Arc::new(Base),
    ];
    for plugin in plugins {
        let grants = plugin.manifest().permissions;
        kernel.install(plugin, grants)?;
    }
    assert_eq!(kernel.load_order()?, ["base", "consumer", "override"]);
    kernel.start_all().await?;
    let consumer = kernel.context("consumer")?;
    let timeout = std::time::Duration::from_secs(1);
    assert_eq!(
        consumer.call(&key(), Value::Null, timeout).await?,
        json!("hello from override")
    );
    consumer
        .emit(TOPIC, json!({"text": "event delivered"}), timeout)
        .await?;
    assert_eq!(messages.lock().unwrap().len(), 1);
    kernel
        .unload("override", UnloadMode::RefuseDependents)
        .await?;
    assert_eq!(
        consumer.call(&key(), Value::Null, timeout).await?,
        json!("hello from base")
    );
    kernel.unload("base", UnloadMode::Cascade).await?;
    assert_eq!(released.load(Ordering::SeqCst), 1);
    assert_eq!(kernel.resources().service_implementations, 0);
    assert_eq!(kernel.resources().listeners, 0);
    assert_eq!(kernel.resources().tasks, 0);
    assert!(consumer.emit(TOPIC, Value::Null, timeout).await.is_err());
    Ok(())
}
