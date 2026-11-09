//! Repeatable local baseline, not a release performance claim. JSON output has no payloads.
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use virt::{Context, Flow, Kernel, Plugin, Registration, Service, async_trait};
use virt_protocol::{Capability, Manifest, Result, ServiceKey};
struct Bench;
#[async_trait]
impl Service for Bench {
    async fn call(&self, value: Value) -> Result<Value> {
        Ok(value)
    }
}
#[async_trait]
impl Plugin for Bench {
    fn manifest(&self) -> Manifest {
        let mut m = Manifest::new("baseline");
        m.permissions.extend([
            Capability::ServiceProvide(ServiceKey::new("baseline.echo", 1)),
            Capability::ServiceCall(ServiceKey::new("baseline.echo", 1)),
            Capability::EventPublish("baseline.event".into()),
            Capability::EventSubscribe("baseline.event".into()),
        ]);
        m
    }
    async fn start(&self, c: Context) -> Result<()> {
        c.provide(
            ServiceKey::new("baseline.echo", 1),
            Arc::new(Bench),
            Registration::New { replaceable: false },
        )?;
        c.hook("baseline.event", 0, |_| Ok(Flow::Continue))
    }
}
fn distribution(mut samples: Vec<u128>) -> Value {
    samples.sort_unstable();
    json!({"iterations":samples.len(),"median_ns":samples[samples.len()/2],"p95_ns":samples[samples.len()*95/100],"p99_ns":samples[samples.len()*99/100]})
}
#[tokio::main]
async fn main() -> Result<()> {
    let mut k = Kernel::default();
    k.install(Arc::new(Bench), Bench.manifest().permissions)?;
    let start = Instant::now();
    k.start_all().await?;
    let startup = start.elapsed().as_nanos();
    let c = k.context("baseline")?;
    let key = ServiceKey::new("baseline.echo", 1);
    let timeout = Duration::from_secs(1);
    for _ in 0..100 {
        c.call(&key, Value::Null, timeout).await?;
    }
    let mut services = Vec::with_capacity(10000);
    let mut events = Vec::with_capacity(10000);
    for _ in 0..10000 {
        let t = Instant::now();
        c.call(&key, Value::Null, timeout).await?;
        services.push(t.elapsed().as_nanos());
        let t = Instant::now();
        c.emit("baseline.event", Value::Null, timeout).await?;
        events.push(t.elapsed().as_nanos());
    }
    let t = Instant::now();
    k.shutdown().await?;
    let shutdown = t.elapsed().as_nanos();
    assert_eq!(k.resources().listeners, 0);
    assert_eq!(k.resources().service_implementations, 0);
    println!(
        "{}",
        json!({"schema_version":1,"profile":"use cargo run --release for comparable optimized results","os":std::env::consts::OS,"arch":std::env::consts::ARCH,"service":distribution(services),"ordered_event_one_listener":distribution(events),"startup_ns":startup,"shutdown_ns":shutdown})
    );
    Ok(())
}
