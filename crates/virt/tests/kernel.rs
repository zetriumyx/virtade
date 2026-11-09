use semver::VersionReq;
use serde_json::{Value, json};
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use virt::{
    Context, Flow, Kernel, Plugin, PluginState, Registration, Service, UnloadMode, async_trait,
};
use virt_protocol::{Capability, Dependency, Error, Manifest, Permissions, Result, ServiceKey};

#[path = "../examples/support/mod.rs"]
mod support;

const DEADLINE: Duration = Duration::from_secs(2);
type Start = Arc<dyn Fn(Context) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync>;
struct TestPlugin {
    manifest: Manifest,
    start: Start,
    stop: Start,
}

fn callback<F, Fut>(f: F) -> Start
where
    F: Fn(Context) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    Arc::new(move |context| Box::pin(f(context)))
}

#[async_trait]
impl Plugin for TestPlugin {
    fn manifest(&self) -> Manifest {
        self.manifest.clone()
    }
    async fn start(&self, context: Context) -> Result<()> {
        (self.start)(context).await
    }
    async fn stop(&self, context: Context) -> Result<()> {
        (self.stop)(context).await
    }
}

fn plugin(manifest: Manifest, start: Start) -> TestPlugin {
    TestPlugin {
        manifest,
        start,
        stop: callback(|_| async { Ok(()) }),
    }
}
fn noop(id: &str) -> TestPlugin {
    plugin(Manifest::new(id), callback(|_| async { Ok(()) }))
}
fn install(kernel: &mut Kernel, plugin: TestPlugin) {
    let grants = plugin.manifest.permissions.clone();
    kernel.install(Arc::new(plugin), grants).unwrap();
}
fn key() -> ServiceKey {
    ServiceKey::new("test.service", 1)
}
fn service_permissions() -> Permissions {
    Permissions::from([
        Capability::ServiceCall(key()),
        Capability::ServiceProvide(key()),
        Capability::ServiceReplace(key()),
    ])
}
fn dependency(plugin: &mut TestPlugin, id: &str) {
    plugin.manifest.dependencies.push(Dependency {
        id: id.into(),
        version: VersionReq::parse("^0.1").unwrap(),
    });
}

struct Echo(&'static str);
#[async_trait]
impl Service for Echo {
    async fn call(&self, _request: Value) -> Result<Value> {
        Ok(json!(self.0))
    }
}
struct WaitService {
    entered: Arc<Notify>,
}
#[async_trait]
impl Service for WaitService {
    async fn call(&self, _request: Value) -> Result<Value> {
        self.entered.notify_one();
        pending().await
    }
}

fn provider(id: &str, text: &'static str, mode: Registration) -> TestPlugin {
    let mut manifest = Manifest::new(id);
    manifest.permissions = service_permissions();
    plugin(
        manifest,
        callback(move |ctx| async move { ctx.provide(key(), Arc::new(Echo(text)), mode) }),
    )
}
async fn caller(kernel: &mut Kernel) -> Context {
    let mut manifest = Manifest::new("caller");
    manifest.permissions = service_permissions();
    install(kernel, plugin(manifest, callback(|_| async { Ok(()) })));
    kernel.start("caller").await.unwrap();
    kernel.context("caller").unwrap()
}

#[tokio::test]
async fn three_plugin_acceptance() {
    support::scenario().await.unwrap();
}

#[tokio::test]
async fn graph_validation_happens_before_start() {
    let mut kernel = Kernel::default();
    let started = Arc::new(AtomicUsize::new(0));
    let count = started.clone();
    install(
        &mut kernel,
        plugin(
            Manifest::new("aaa"),
            callback(move |_| {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        ),
    );
    let mut bad = noop("bad");
    dependency(&mut bad, "missing");
    install(&mut kernel, bad);
    assert!(matches!(
        kernel.start_all().await,
        Err(Error::MissingDependency { .. })
    ));
    assert_eq!(started.load(Ordering::SeqCst), 0);
}

#[test]
fn dependency_cycle_and_version_mismatch_are_diagnostic() {
    let mut kernel = Kernel::default();
    let mut a = noop("a");
    dependency(&mut a, "b");
    let mut b = noop("b");
    dependency(&mut b, "a");
    install(&mut kernel, a);
    install(&mut kernel, b);
    assert_eq!(
        kernel.load_order(),
        Err(Error::DependencyCycle(vec![
            "a".into(),
            "b".into(),
            "a".into()
        ]))
    );
    let mut kernel = Kernel::default();
    let mut b = noop("b");
    dependency(&mut b, "a");
    b.manifest.dependencies[0].version = VersionReq::parse("^2").unwrap();
    install(&mut kernel, noop("a"));
    install(&mut kernel, b);
    assert!(matches!(
        kernel.load_order(),
        Err(Error::DependencyVersion { .. })
    ));
}

#[test]
fn rejects_api_duplicate_and_invalid_manifests() {
    let mut kernel = Kernel::default();
    let mut invalid = noop("bad");
    invalid.manifest.api = VersionReq::parse("^2").unwrap();
    assert!(matches!(
        kernel.install(Arc::new(invalid), Permissions::new()),
        Err(Error::ApiMismatch(_))
    ));
    install(&mut kernel, noop("a"));
    assert!(matches!(
        kernel.install(Arc::new(noop("a")), Permissions::new()),
        Err(Error::DuplicatePlugin(_))
    ));
    assert!(matches!(
        kernel.install(Arc::new(noop("../escape")), Permissions::new()),
        Err(Error::InvalidManifest(_))
    ));
}

#[tokio::test]
async fn replacement_layers_restore_and_non_top_unload_is_safe() {
    let mut kernel = Kernel::default();
    let ctx = caller(&mut kernel).await;
    for (id, text, mode) in [
        ("base", "base", Registration::New { replaceable: true }),
        ("middle", "middle", Registration::Replace),
        ("top", "top", Registration::Replace),
    ] {
        install(&mut kernel, provider(id, text, mode));
        kernel.start(id).await.unwrap();
    }
    kernel
        .unload("middle", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert_eq!(
        ctx.call(&key(), Value::Null, DEADLINE).await.unwrap(),
        json!("top")
    );
    kernel
        .unload("top", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert_eq!(
        ctx.call(&key(), Value::Null, DEADLINE).await.unwrap(),
        json!("base")
    );
    kernel
        .unload("base", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert!(matches!(
        ctx.call(&key(), Value::Null, DEADLINE).await,
        Err(Error::ServiceUnavailable(_))
    ));
}

#[tokio::test]
async fn removing_base_never_resurrects_it() {
    let mut kernel = Kernel::default();
    let ctx = caller(&mut kernel).await;
    install(
        &mut kernel,
        provider("base", "base", Registration::New { replaceable: true }),
    );
    kernel.start("base").await.unwrap();
    install(
        &mut kernel,
        provider("override", "override", Registration::Replace),
    );
    kernel.start("override").await.unwrap();
    kernel
        .unload("base", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert_eq!(
        ctx.call(&key(), Value::Null, DEADLINE).await.unwrap(),
        json!("override")
    );
    kernel
        .unload("override", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert!(matches!(
        ctx.call(&key(), Value::Null, DEADLINE).await,
        Err(Error::ServiceUnavailable(_))
    ));
}

#[tokio::test]
async fn replacement_requires_grant_and_base_consent() {
    let mut kernel = Kernel::default();
    install(
        &mut kernel,
        provider("base", "base", Registration::New { replaceable: false }),
    );
    kernel.start("base").await.unwrap();
    install(
        &mut kernel,
        provider("conflict", "x", Registration::New { replaceable: true }),
    );
    assert!(matches!(
        kernel.start("conflict").await,
        Err(Error::ServiceConflict(_))
    ));
    install(
        &mut kernel,
        provider("override", "x", Registration::Replace),
    );
    assert!(matches!(
        kernel.start("override").await,
        Err(Error::NotReplaceable(_))
    ));
    let p = provider("ungranted", "x", Registration::Replace);
    let grants = Permissions::from([Capability::ServiceProvide(key())]);
    kernel.install(Arc::new(p), grants).unwrap();
    assert!(matches!(
        kernel.start("ungranted").await,
        Err(Error::PermissionDenied { .. })
    ));
    assert_eq!(kernel.resources().service_implementations, 1);
}

#[tokio::test]
async fn grants_are_intersection_not_manifest_or_host_union() {
    let mut kernel = Kernel::default();
    kernel
        .install(Arc::new(noop("a")), service_permissions())
        .unwrap();
    kernel.start("a").await.unwrap();
    let ctx = kernel.context("a").unwrap();
    assert!(matches!(
        ctx.call(&key(), Value::Null, DEADLINE).await,
        Err(Error::PermissionDenied { .. })
    ));
    assert!(matches!(
        ctx.hook("topic", 0, |_| Ok(Flow::Continue)),
        Err(Error::PermissionDenied { .. })
    ));
}

#[tokio::test]
async fn start_failure_rolls_back_new_resources_but_preserves_existing_plugins() {
    let mut kernel = Kernel::default();
    install(&mut kernel, noop("existing"));
    kernel.start("existing").await.unwrap();
    install(
        &mut kernel,
        provider("aaa", "base", Registration::New { replaceable: true }),
    );
    let mut m = Manifest::new("zzz");
    m.permissions = service_permissions();
    install(
        &mut kernel,
        plugin(
            m,
            callback(|ctx| async move {
                ctx.provide(key(), Arc::new(Echo("override")), Registration::Replace)?;
                Err(Error::PluginFailure {
                    plugin: "zzz".into(),
                    message: "intentional".into(),
                })
            }),
        ),
    );
    assert!(kernel.start_all().await.is_err());
    assert_eq!(kernel.resources().service_implementations, 0);
    assert_eq!(kernel.state("existing").unwrap(), PluginState::Active);
    assert_eq!(kernel.state("aaa").unwrap(), PluginState::Unloaded);
    assert_eq!(kernel.state("zzz").unwrap(), PluginState::Failed);
    assert!(kernel.last_error("zzz").unwrap().is_some());
}

#[tokio::test]
async fn panic_during_start_cleans_partial_registrations() {
    let mut kernel = Kernel::default();
    let mut m = Manifest::new("panic");
    m.permissions = service_permissions();
    install(
        &mut kernel,
        plugin(
            m,
            callback(|ctx| async move {
                ctx.provide(
                    key(),
                    Arc::new(Echo("x")),
                    Registration::New { replaceable: true },
                )?;
                panic!("plugin panic");
            }),
        ),
    );
    assert!(matches!(
        kernel.start("panic").await,
        Err(Error::PluginPanic(_))
    ));
    assert_eq!(kernel.resources().service_implementations, 0);
    install(&mut kernel, noop("healthy"));
    kernel.start("healthy").await.unwrap();
}

#[tokio::test]
async fn pause_cascade_and_reload_revoke_old_contexts() {
    let mut kernel = Kernel::default();
    install(
        &mut kernel,
        provider("base", "base", Registration::New { replaceable: true }),
    );
    let mut dependent = noop("dependent");
    dependency(&mut dependent, "base");
    install(&mut kernel, dependent);
    kernel.start_all().await.unwrap();
    let old = kernel.context("base").unwrap();
    assert!(matches!(
        kernel.unload("base", UnloadMode::RefuseDependents).await,
        Err(Error::DependentsActive { .. })
    ));
    kernel.pause("base", UnloadMode::Cascade).await.unwrap();
    assert_eq!(kernel.state("dependent").unwrap(), PluginState::Paused);
    assert!(matches!(
        old.provide(
            key(),
            Arc::new(Echo("stale")),
            Registration::New { replaceable: true }
        ),
        Err(Error::ScopeClosed(_))
    ));
    assert!(matches!(old.spawn(async {}), Err(Error::ScopeClosed(_))));
    kernel.start("base").await.unwrap();
    kernel.reload("base").await.unwrap();
    assert!(matches!(
        old.call(&key(), Value::Null, DEADLINE).await,
        Err(Error::ScopeClosed(_))
    ));
    assert_eq!(kernel.resources().service_implementations, 1);
}

async fn event_context(kernel: &mut Kernel, id: &str) -> Context {
    let mut m = Manifest::new(id);
    m.permissions = Permissions::from([
        Capability::EventSubscribe("topic".into()),
        Capability::EventPublish("topic".into()),
    ]);
    install(kernel, plugin(m, callback(|_| async { Ok(()) })));
    kernel.start(id).await.unwrap();
    kernel.context(id).unwrap()
}

#[tokio::test]
async fn synchronous_and_async_hooks_have_stable_priority_and_interception() {
    let mut kernel = Kernel::default();
    let ctx = event_context(&mut kernel, "events").await;
    let order = Arc::new(Mutex::new(vec![]));
    for (priority, value, intercept) in
        [(0, 4, false), (10, 1, false), (10, 2, false), (5, 3, true)]
    {
        let order = order.clone();
        ctx.hook("topic", priority, move |_| {
            order.lock().unwrap().push(value);
            Ok(if intercept {
                Flow::Intercept
            } else {
                Flow::Continue
            })
        })
        .unwrap();
    }
    assert_eq!(
        ctx.emit("topic", Value::Null, DEADLINE).await.unwrap(),
        Flow::Intercept
    );
    assert_eq!(*order.lock().unwrap(), [1, 2, 3]);
    kernel
        .unload("events", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert_eq!(kernel.resources().listeners, 0);
}

#[tokio::test]
async fn reentrant_event_delivery_does_not_hold_registry_locks() {
    let mut kernel = Kernel::default();
    let ctx = event_context(&mut kernel, "events").await;
    let nested = ctx.clone();
    ctx.on("topic", 0, move |v| {
        let nested = nested.clone();
        async move {
            if v == json!(0) {
                nested.emit("topic", json!(1), DEADLINE).await?;
            }
            Ok(Flow::Continue)
        }
    })
    .unwrap();
    ctx.emit("topic", json!(0), DEADLINE).await.unwrap();
    kernel.shutdown().await.unwrap();
}

#[tokio::test]
async fn event_panic_and_timeout_do_not_break_other_dispatches() {
    let mut kernel = Kernel::default();
    let source = event_context(&mut kernel, "source").await;
    let bad = event_context(&mut kernel, "bad").await;
    bad.on("topic", 0, |_| -> std::future::Ready<Result<Flow>> {
        panic!("closure construction panic")
    })
    .unwrap();
    assert!(matches!(
        source.emit("topic", Value::Null, DEADLINE).await,
        Err(Error::PluginPanic(_))
    ));
    kernel
        .unload("bad", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    let slow = event_context(&mut kernel, "slow").await;
    slow.on("topic", 0, |_| async { pending().await }).unwrap();
    assert!(matches!(
        source
            .emit("topic", Value::Null, Duration::from_millis(20))
            .await,
        Err(Error::Timeout(_))
    ));
    kernel
        .unload("slow", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert_eq!(
        source.emit("topic", Value::Null, DEADLINE).await.unwrap(),
        Flow::Continue
    );
}

#[tokio::test]
async fn unloading_provider_cancels_inflight_calls() {
    let mut kernel = Kernel::default();
    let ctx = caller(&mut kernel).await;
    let entered = Arc::new(Notify::new());
    let signal = entered.clone();
    let mut m = Manifest::new("slow");
    m.permissions = service_permissions();
    install(
        &mut kernel,
        plugin(
            m,
            callback(move |ctx| {
                let entered = signal.clone();
                async move {
                    ctx.provide(
                        key(),
                        Arc::new(WaitService { entered }),
                        Registration::New { replaceable: true },
                    )
                }
            }),
        ),
    );
    kernel.start("slow").await.unwrap();
    let task = tokio::spawn(async move { ctx.call(&key(), Value::Null, DEADLINE).await });
    tokio::time::timeout(DEADLINE, entered.notified())
        .await
        .unwrap();
    kernel
        .unload("slow", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert!(matches!(task.await.unwrap(), Err(Error::ScopeClosed(_))));
}

#[tokio::test]
async fn unloading_caller_cancels_its_inflight_service_work() {
    let mut kernel = Kernel::default();
    let ctx = caller(&mut kernel).await;
    let entered = Arc::new(Notify::new());
    let signal = entered.clone();
    let mut m = Manifest::new("slow");
    m.permissions = service_permissions();
    install(
        &mut kernel,
        plugin(
            m,
            callback(move |ctx| {
                let entered = signal.clone();
                async move {
                    ctx.provide(
                        key(),
                        Arc::new(WaitService { entered }),
                        Registration::New { replaceable: true },
                    )
                }
            }),
        ),
    );
    kernel.start("slow").await.unwrap();
    let task = tokio::spawn(async move { ctx.call(&key(), Value::Null, DEADLINE).await });
    tokio::time::timeout(DEADLINE, entered.notified())
        .await
        .unwrap();
    kernel
        .unload("caller", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert!(matches!(task.await.unwrap(), Err(Error::ScopeClosed(_))));
    kernel.shutdown().await.unwrap();
    assert_eq!(kernel.resources().tasks, 0);
}

#[tokio::test]
async fn stop_failure_still_cleans_every_plugin_in_cascade() {
    let mut kernel = Kernel::new(Duration::from_millis(20));
    let mut base = provider("base", "x", Registration::New { replaceable: true });
    base.stop = callback(|_| async { pending().await });
    install(&mut kernel, base);
    let mut dependent = noop("dependent");
    dependency(&mut dependent, "base");
    dependent.stop = callback(|_| async { panic!("stop failure") });
    install(&mut kernel, dependent);
    kernel.start_all().await.unwrap();
    assert!(kernel.unload("base", UnloadMode::Cascade).await.is_err());
    assert_eq!(kernel.resources().service_implementations, 0);
    assert_eq!(kernel.resources().tasks, 0);
    assert_eq!(kernel.state("base").unwrap(), PluginState::Failed);
    assert_eq!(kernel.state("dependent").unwrap(), PluginState::Failed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_racing_unload_cannot_escape_scope() {
    let mut kernel = Kernel::default();
    let ctx = event_context(&mut kernel, "race").await;
    let worker = ctx.clone();
    let started = Arc::new(Notify::new());
    let signal = started.clone();
    let task = tokio::spawn(async move {
        for _ in 0..1000 {
            match worker.hook("topic", 0, |_| Ok(Flow::Continue)) {
                Ok(()) => signal.notify_one(),
                Err(Error::ScopeClosed(_)) => break,
                Err(error) => panic!("unexpected: {error}"),
            }
            tokio::task::yield_now().await;
        }
    });
    tokio::time::timeout(DEADLINE, started.notified())
        .await
        .unwrap();
    kernel
        .unload("race", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    task.await.unwrap();
    assert_eq!(kernel.resources().listeners, 0);
    assert!(matches!(
        ctx.hook("topic", 0, |_| Ok(Flow::Continue)),
        Err(Error::ScopeClosed(_))
    ));
}

#[tokio::test]
async fn repeated_reload_has_no_registration_growth() {
    let mut kernel = Kernel::default();
    install(
        &mut kernel,
        provider("base", "base", Registration::New { replaceable: true }),
    );
    kernel.start("base").await.unwrap();
    for _ in 0..100 {
        kernel.reload("base").await.unwrap();
        assert_eq!(kernel.resources().service_implementations, 1);
    }
    kernel.shutdown().await.unwrap();
    assert_eq!(kernel.resources().service_implementations, 0);
    assert_eq!(kernel.resources().tasks, 0);
}

#[tokio::test]
async fn background_panic_is_observable_and_kernel_keeps_running() {
    let mut kernel = Kernel::default();
    install(&mut kernel, noop("p"));
    kernel.start("p").await.unwrap();
    let ctx = kernel.context("p").unwrap();
    ctx.spawn(async { panic!("background panic") }).unwrap();
    tokio::time::timeout(DEADLINE, async {
        while ctx.background_failures().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        ctx.background_failures(),
        vec![Error::PluginPanic("p".into())]
    );
    kernel.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_plugin_can_restart_with_a_fresh_scope() {
    let mut kernel = Kernel::default();
    let attempts = Arc::new(AtomicUsize::new(0));
    let mut manifest = Manifest::new("retry");
    manifest.permissions = service_permissions();
    install(
        &mut kernel,
        plugin(
            manifest,
            callback(move |ctx| {
                let attempts = attempts.clone();
                async move {
                    ctx.provide(
                        key(),
                        Arc::new(Echo("recovered")),
                        Registration::New { replaceable: true },
                    )?;
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err(Error::PluginFailure {
                            plugin: "retry".into(),
                            message: "first attempt".into(),
                        });
                    }
                    Ok(())
                }
            }),
        ),
    );
    assert!(kernel.start("retry").await.is_err());
    assert_eq!(kernel.resources().service_implementations, 0);
    kernel.start("retry").await.unwrap();
    assert_eq!(kernel.state("retry").unwrap(), PluginState::Active);
    assert_eq!(kernel.last_error("retry").unwrap(), None);
    assert_eq!(kernel.resources().service_implementations, 1);
}

#[tokio::test]
async fn start_timeout_revokes_partial_resources_and_finishes_tasks() {
    let mut kernel = Kernel::new(Duration::from_millis(20));
    let mut manifest = Manifest::new("timeout");
    manifest.permissions = service_permissions();
    install(
        &mut kernel,
        plugin(
            manifest,
            callback(|ctx| async move {
                ctx.provide(
                    key(),
                    Arc::new(Echo("partial")),
                    Registration::New { replaceable: true },
                )?;
                pending().await
            }),
        ),
    );
    assert!(matches!(
        kernel.start("timeout").await,
        Err(Error::Timeout(_))
    ));
    assert_eq!(kernel.resources().service_implementations, 0);
    assert_eq!(kernel.resources().tasks, 0);
}

#[tokio::test]
async fn cancelled_lifecycle_future_revokes_and_requires_explicit_recovery() {
    let mut kernel = Kernel::default();
    let entered = Arc::new(Notify::new());
    let signal = entered.clone();
    let mut manifest = Manifest::new("cancelled");
    manifest.permissions = service_permissions();
    install(
        &mut kernel,
        plugin(
            manifest,
            callback(move |ctx| {
                let signal = signal.clone();
                async move {
                    ctx.provide(
                        key(),
                        Arc::new(Echo("partial")),
                        Registration::New { replaceable: true },
                    )?;
                    signal.notify_one();
                    pending().await
                }
            }),
        ),
    );
    {
        let start = kernel.start("cancelled");
        tokio::pin!(start);
        tokio::select! {
            _ = entered.notified() => (),
            result = &mut start => panic!("unexpected: {result:?}"),
        }
    }
    assert_eq!(kernel.resources().service_implementations, 0);
    assert!(matches!(
        kernel.start("cancelled").await,
        Err(Error::InvalidState(_))
    ));
    kernel
        .unload("cancelled", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert_eq!(kernel.state("cancelled").unwrap(), PluginState::Unloaded);
}

#[tokio::test]
async fn service_panic_and_timeout_leave_registry_usable() {
    struct PanicService;
    #[async_trait]
    impl Service for PanicService {
        async fn call(&self, _request: Value) -> Result<Value> {
            panic!("service panic")
        }
    }
    let mut kernel = Kernel::default();
    let ctx = caller(&mut kernel).await;
    let mut manifest = Manifest::new("bad");
    manifest.permissions = service_permissions();
    install(
        &mut kernel,
        plugin(
            manifest,
            callback(|ctx| async move {
                ctx.provide(
                    key(),
                    Arc::new(PanicService),
                    Registration::New { replaceable: true },
                )
            }),
        ),
    );
    kernel.start("bad").await.unwrap();
    assert_eq!(
        ctx.call(&key(), Value::Null, DEADLINE).await,
        Err(Error::PluginPanic("bad".into()))
    );
    kernel
        .unload("bad", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    let mut manifest = Manifest::new("slow");
    manifest.permissions = service_permissions();
    install(
        &mut kernel,
        plugin(
            manifest,
            callback(|ctx| async move {
                ctx.provide(
                    key(),
                    Arc::new(WaitService {
                        entered: Arc::new(Notify::new()),
                    }),
                    Registration::New { replaceable: true },
                )
            }),
        ),
    );
    kernel.start("slow").await.unwrap();
    assert!(matches!(
        ctx.call(&key(), Value::Null, Duration::from_millis(20))
            .await,
        Err(Error::Timeout(_))
    ));
    kernel.shutdown().await.unwrap();
    assert_eq!(kernel.resources().tasks, 0);
}

#[tokio::test]
async fn an_unloaded_listener_in_dispatch_snapshot_is_skipped() {
    let mut kernel = Kernel::default();
    let source = event_context(&mut kernel, "source").await;
    let first = event_context(&mut kernel, "first").await;
    let second = event_context(&mut kernel, "second").await;
    let entered = Arc::new(Notify::new());
    let signal = entered.clone();
    let release = Arc::new(Notify::new());
    let gate = release.clone();
    first
        .on("topic", 10, move |_| {
            let signal = signal.clone();
            let gate = gate.clone();
            async move {
                signal.notify_one();
                gate.notified().await;
                Ok(Flow::Continue)
            }
        })
        .unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    second
        .hook("topic", 0, move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(Flow::Continue)
        })
        .unwrap();
    let emit = tokio::spawn(async move { source.emit("topic", Value::Null, DEADLINE).await });
    tokio::time::timeout(DEADLINE, entered.notified())
        .await
        .unwrap();
    kernel
        .unload("second", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(emit.await.unwrap().unwrap(), Flow::Continue);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn revocation_precedes_slow_stop_hook() {
    let mut kernel = Kernel::default();
    let entered = Arc::new(Notify::new());
    let signal = entered.clone();
    let release = Arc::new(Notify::new());
    let gate = release.clone();
    let mut p = provider("p", "p", Registration::New { replaceable: true });
    p.stop = callback(move |ctx| {
        let signal = signal.clone();
        let gate = gate.clone();
        async move {
            assert!(matches!(ctx.spawn(async {}), Err(Error::ScopeClosed(_))));
            signal.notify_one();
            gate.notified().await;
            Ok(())
        }
    });
    install(&mut kernel, p);
    kernel.start("p").await.unwrap();
    let old = kernel.context("p").unwrap();
    let unload = kernel.unload("p", UnloadMode::RefuseDependents);
    tokio::pin!(unload);
    tokio::select! {
        _ = entered.notified() => (),
        result = &mut unload => panic!("unexpected: {result:?}"),
    }
    assert!(matches!(
        old.call(&key(), Value::Null, DEADLINE).await,
        Err(Error::ScopeClosed(_))
    ));
    release.notify_one();
    unload.await.unwrap();
}

#[tokio::test]
async fn unrelated_invalid_registration_cannot_block_live_scope_cleanup() {
    let mut kernel = Kernel::default();
    install(
        &mut kernel,
        provider("base", "base", Registration::New { replaceable: true }),
    );
    kernel.start("base").await.unwrap();
    let mut bad = noop("bad");
    dependency(&mut bad, "absent");
    install(&mut kernel, bad);
    kernel
        .unload("base", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert_eq!(kernel.resources().service_implementations, 0);
}

#[tokio::test]
async fn isolated_plugin_cannot_call_or_replace_private_host_service_even_with_grants() {
    let mut kernel = Kernel::default();
    let mut manifest = Manifest::new("host");
    manifest.permissions = service_permissions();
    install(
        &mut kernel,
        plugin(
            manifest,
            callback(|ctx| async move { ctx.provide_private(key(), Arc::new(Echo("private"))) }),
        ),
    );
    let mut manifest = Manifest::new("guest");
    manifest.permissions = service_permissions();
    let grants = manifest.permissions.clone();
    kernel
        .install_isolated(
            Arc::new(plugin(manifest, callback(|_| async { Ok(()) }))),
            grants,
        )
        .unwrap();
    kernel.start_all().await.unwrap();
    let ctx = kernel.context("guest").unwrap();
    assert!(matches!(
        ctx.call(&key(), json!(null), DEADLINE).await,
        Err(Error::PermissionDenied { .. })
    ));
    assert!(
        ctx.provide(key(), Arc::new(Echo("stolen")), Registration::Replace)
            .is_err()
    );
    assert!(
        ctx.provide_private(ServiceKey::new("fake", 1), Arc::new(Echo("fake")))
            .is_err()
    );
    kernel.shutdown().await.unwrap();
}

#[tokio::test]
async fn notification_observer_failure_does_not_suppress_other_observers() {
    let mut kernel = Kernel::default();
    let count = Arc::new(AtomicUsize::new(0));
    for (id, fail) in [("bad", true), ("good", false)] {
        let mut manifest = Manifest::new(id);
        manifest
            .permissions
            .insert(Capability::EventSubscribe("notice".into()));
        let count = count.clone();
        install(
            &mut kernel,
            plugin(
                manifest,
                callback(move |ctx| {
                    let count = count.clone();
                    async move {
                        ctx.on("notice", 0, move |_| {
                            let count = count.clone();
                            async move {
                                if fail {
                                    Err(Error::PluginFailure {
                                        plugin: "bad".into(),
                                        message: "observer error".into(),
                                    })
                                } else {
                                    count.fetch_add(1, Ordering::SeqCst);
                                    Ok(Flow::Continue)
                                }
                            }
                        })
                    }
                }),
            ),
        );
    }
    let mut manifest = Manifest::new("publisher");
    manifest
        .permissions
        .insert(Capability::EventPublish("notice".into()));
    install(
        &mut kernel,
        plugin(manifest, callback(|_| async { Ok(()) })),
    );
    kernel.start_all().await.unwrap();
    let errors = kernel
        .context("publisher")
        .unwrap()
        .notify("notice", json!({}), DEADLINE)
        .await
        .unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    kernel.shutdown().await.unwrap();
}

#[tokio::test]
async fn contribution_registration_quota_and_generation_cleanup() {
    let mut kernel = Kernel::default();
    let mut manifest = Manifest::new("views");
    manifest
        .permissions
        .insert(Capability::Contribution("panel".into()));
    install(
        &mut kernel,
        plugin(manifest, callback(|_| async { Ok(()) })),
    );
    kernel.start_all().await.unwrap();
    let ctx = kernel.context("views").unwrap();
    for index in 0..24 {
        ctx.contribute(virt::Contribution {
            id: format!("views.panel{index}"),
            kind: virt::ContributionKind::Panel,
            title: "Panel".into(),
            schema_version: 1,
            spec: json!({"type":"text","text":"hello"}),
        })
        .unwrap();
    }
    for _ in 0..1000 {
        ctx.defer(|| {}).unwrap();
    }
    assert!(ctx.defer(|| {}).is_err());
    assert_eq!(kernel.contributions().len(), 24);
    kernel
        .unload("views", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert!(kernel.contributions().is_empty());
    assert!(ctx.defer(|| {}).is_err());
}
