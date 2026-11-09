use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tempfile::TempDir;
use virt::{Context, Kernel, Plugin, UnloadMode, async_trait};
use virt_protocol::{Capability, Manifest, Permissions, ServiceKey};
use virtade_plugin_runtime::{ComponentEngine, PluginManager, WasmPlugin};
use virtade_storage::Store;

fn source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/wasm-hello")
}
fn manifest() -> Manifest {
    serde_json::from_slice(&std::fs::read(source().join("manifest.json")).unwrap()).unwrap()
}
struct Caller;
#[async_trait]
impl Plugin for Caller {
    fn manifest(&self) -> Manifest {
        let mut manifest = Manifest::new("caller");
        manifest.permissions = Permissions::from([
            Capability::ServiceCall(ServiceKey::new("hello.greeting", 1)),
            Capability::EventPublish("virtade.agent.codex.event.v1".into()),
        ]);
        manifest
    }
    async fn start(&self, _: Context) -> virt_protocol::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn real_component_registers_service_agent_event_and_panel_then_unloads_cleanly() {
    let temp = TempDir::new().unwrap();
    let database = Store::open(temp.path().join("plugins.db")).await.unwrap();
    let engine = ComponentEngine::new().unwrap();
    let manifest = manifest();
    let wasm = Arc::new(WasmPlugin::new(
        manifest.clone(),
        source().join("component.wasm"),
        engine,
        database.clone(),
    ));
    let mut kernel = Kernel::default();
    kernel.install(wasm.clone(), manifest.permissions).unwrap();
    kernel
        .install(Arc::new(Caller), Caller.manifest().permissions)
        .unwrap();
    kernel.start_all().await.unwrap();
    let ctx = kernel.context("caller").unwrap();
    assert_eq!(
        ctx.call(
            &ServiceKey::new("hello.greeting", 1),
            json!({"roundtrip":42}),
            Duration::from_secs(3)
        )
        .await
        .unwrap(),
        json!({"roundtrip":42})
    );
    ctx.emit(
        "virtade.agent.codex.event.v1",
        json!({"native_event":"fixture"}),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert!(
        wasm.diagnostics()
            .iter()
            .any(|message| message.contains("received Agent event"))
    );
    assert_eq!(kernel.contributions().len(), 1);
    kernel
        .unload("hello", UnloadMode::RefuseDependents)
        .await
        .unwrap();
    assert!(kernel.contributions().is_empty());
    assert_eq!(kernel.resources().service_implementations, 0);
    assert_eq!(kernel.resources().listeners, 0);
    kernel.shutdown().await.unwrap();
    database.close().await.unwrap();
}

#[tokio::test]
async fn manifest_requests_do_not_grant_component_host_imports() {
    let temp = TempDir::new().unwrap();
    let database = Store::open(temp.path().join("plugins.db")).await.unwrap();
    let engine = ComponentEngine::new().unwrap();
    let mut kernel = Kernel::default();
    let plugin = Arc::new(WasmPlugin::new(
        manifest(),
        source().join("component.wasm"),
        engine,
        database.clone(),
    ));
    kernel.install(plugin, Permissions::new()).unwrap();
    assert!(kernel.start_all().await.is_err());
    assert_eq!(kernel.resources().service_implementations, 0);
    assert!(kernel.contributions().is_empty());
    database.close().await.unwrap();
}

#[tokio::test]
async fn guest_fuel_memory_trap_and_sensitive_import_fail_without_crashing_host() {
    for probe in ["spin", "trap", "allocate", "config-probe"] {
        let temp = TempDir::new().unwrap();
        let database = Store::open(temp.path().join("plugins.db")).await.unwrap();
        let engine = ComponentEngine::new().unwrap();
        let manifest = manifest();
        let mut kernel = Kernel::default();
        kernel
            .install(
                Arc::new(WasmPlugin::new(
                    manifest.clone(),
                    source().join("test-probes.component.wasm"),
                    engine,
                    database.clone(),
                )),
                manifest.permissions,
            )
            .unwrap();
        kernel
            .install(Arc::new(Caller), Caller.manifest().permissions)
            .unwrap();
        kernel.start_all().await.unwrap();
        let ctx = kernel.context("caller").unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            ctx.call(
                &ServiceKey::new("hello.greeting", 1),
                json!(probe),
                Duration::from_secs(2),
            ),
        )
        .await
        .unwrap();
        assert!(result.is_err(), "probe {probe} must fail");
        let _ = kernel.shutdown().await;
        assert!(kernel.contributions().is_empty());
        assert_eq!(kernel.resources().service_implementations, 0);
        database.close().await.unwrap();
    }
}

#[tokio::test]
async fn package_install_enable_disable_reenable_and_uninstall_revoke_old_ui_generation() {
    let temp = TempDir::new().unwrap();
    let database = Store::open(temp.path().join("plugins.db")).await.unwrap();
    let manager = PluginManager::open(temp.path().join("packages"), database.clone())
        .await
        .unwrap();
    let manifest = PluginManager::inspect(source()).await.unwrap();
    manager
        .install(source(), manifest.permissions)
        .await
        .unwrap();
    assert_eq!(manager.snapshot().await[0].state, "Registered");
    manager.enable("hello").await.unwrap();
    assert_eq!(manager.contributions().await.len(), 1);
    let generation = manager.snapshot().await[0].generation.clone().unwrap();
    manager.disable("hello").await.unwrap();
    assert!(manager.contributions().await.is_empty());
    assert!(
        manager
            .call(
                "hello",
                &generation,
                ServiceKey::new("hello.greeting", 1),
                json!(null)
            )
            .await
            .is_err()
    );
    manager.enable("hello").await.unwrap();
    assert_ne!(
        manager.snapshot().await[0].generation.as_deref(),
        Some(generation.as_str())
    );
    manager.uninstall("hello").await.unwrap();
    assert!(manager.snapshot().await.is_empty());
    manager.shutdown().await;
    database.close().await.unwrap();
}

#[tokio::test]
async fn tampered_package_is_quarantined_on_restart_and_can_be_uninstalled() {
    let temp = TempDir::new().unwrap();
    let database = Store::open(temp.path().join("plugins.db")).await.unwrap();
    let root = temp.path().join("packages");
    let manager = PluginManager::open(root.clone(), database.clone())
        .await
        .unwrap();
    let definition = PluginManager::inspect(source()).await.unwrap();
    manager
        .install(source(), definition.permissions)
        .await
        .unwrap();
    manager.enable("hello").await.unwrap();
    let installed = manager.snapshot().await[0].package.directory.clone();
    manager.shutdown().await;
    drop(manager);
    std::fs::write(installed.join("component.wasm"), b"tampered").unwrap();
    let manager = PluginManager::open(root, database.clone()).await.unwrap();
    assert!(manager.contributions().await.is_empty());
    let snapshot = manager.snapshot().await;
    assert!(!snapshot[0].diagnostics.is_empty());
    assert!(manager.enable("hello").await.is_err());
    manager.uninstall("hello").await.unwrap();
    assert!(source().join("component.wasm").exists());
    assert!(!installed.exists());
    manager.shutdown().await;
    database.close().await.unwrap();
}

#[tokio::test]
async fn ui_broker_denies_ungranted_files_process_network_and_configuration() {
    use virtade_plugin_runtime::{ResourceRequest, UiRequest};
    let temp = TempDir::new().unwrap();
    let database = Store::open(temp.path().join("plugins.db")).await.unwrap();
    let manager = PluginManager::open(temp.path().join("packages"), database.clone())
        .await
        .unwrap();
    let definition = PluginManager::inspect(source()).await.unwrap();
    manager
        .install(source(), definition.permissions)
        .await
        .unwrap();
    manager.enable("hello").await.unwrap();
    let generation = manager.snapshot().await[0].generation.clone().unwrap();
    let requests = vec![
        UiRequest::ConfigGet {
            key: "secret".into(),
        },
        UiRequest::ConfigSet {
            key: "secret".into(),
            value: json!("value"),
        },
        UiRequest::Resource {
            request: ResourceRequest::ReadFile {
                root: temp.path().to_string_lossy().into(),
                path: "plugins.db".into(),
            },
        },
        UiRequest::Resource {
            request: ResourceRequest::Run {
                program: "/bin/sh".into(),
                cwd: "/".into(),
                args: vec![],
            },
        },
        UiRequest::Resource {
            request: ResourceRequest::Fetch {
                url: "https://example.com/".into(),
            },
        },
        UiRequest::ServiceCall {
            key: ServiceKey::new("virtade.workbench.internal", 1),
            request: json!({}),
        },
    ];
    for request in requests {
        assert!(
            manager
                .ui_request("hello", &generation, request)
                .await
                .is_err()
        );
    }
    manager.shutdown().await;
    database.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn package_symlink_cannot_escape_source_directory() {
    let temp = TempDir::new().unwrap();
    std::fs::copy(
        source().join("manifest.json"),
        temp.path().join("manifest.json"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        source().join("component.wasm"),
        temp.path().join("component.wasm"),
    )
    .unwrap();
    assert!(PluginManager::inspect(temp.path().into()).await.is_err());
}

#[tokio::test]
async fn failed_install_persistence_does_not_leave_a_registered_plugin() {
    let temp = TempDir::new().unwrap();
    let database = Store::open(temp.path().join("plugins.db")).await.unwrap();
    let manager = PluginManager::open(temp.path().join("packages"), database.clone())
        .await
        .unwrap();
    let manifest = PluginManager::inspect(source()).await.unwrap();
    let before = manager.resource_counts().await;
    database.close().await.unwrap();
    assert!(
        manager
            .install(source(), manifest.permissions)
            .await
            .is_err()
    );
    assert!(manager.snapshot().await.is_empty());
    assert!(manager.contributions().await.is_empty());
    assert_eq!(manager.resource_counts().await, before);
    manager.shutdown().await;
}

#[tokio::test]
async fn persistent_rpc_is_permission_scoped_streams_responses_and_revokes_on_disable() {
    use virtade_plugin_runtime::{ResourceRequest, UiRequest};
    let temp = TempDir::new().unwrap();
    let database = Store::open(temp.path().join("rpc.db")).await.unwrap();
    let root = temp.path().canonicalize().unwrap();
    let package = root.join("source");
    std::fs::create_dir(&package).unwrap();
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_rpc-broker-fixture"))
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let mut definition = manifest();
    definition.permissions.extend([
        Capability::ProcessCommand(executable.clone()),
        Capability::FilesystemRead(root.to_string_lossy().into()),
    ]);
    std::fs::write(
        package.join("manifest.json"),
        serde_json::to_vec(&definition).unwrap(),
    )
    .unwrap();
    std::fs::copy(
        source().join("component.wasm"),
        package.join("component.wasm"),
    )
    .unwrap();
    let manager = PluginManager::open(root.join("packages"), database.clone())
        .await
        .unwrap();
    manager
        .install(package, definition.permissions)
        .await
        .unwrap();
    manager.enable("hello").await.unwrap();
    let generation = manager.snapshot().await[0].generation.clone().unwrap();
    let resource = |request| UiRequest::Resource { request };
    assert!(
        manager
            .ui_request(
                "hello",
                &generation,
                resource(ResourceRequest::RpcOpen {
                    program: executable.clone(),
                    cwd: root.parent().unwrap().to_string_lossy().into(),
                    args: vec![],
                    json_rpc_2: true
                })
            )
            .await
            .is_err()
    );
    let opened = manager
        .ui_request(
            "hello",
            &generation,
            resource(ResourceRequest::RpcOpen {
                program: executable.clone(),
                cwd: root.to_string_lossy().into(),
                args: vec![],
                json_rpc_2: true,
            }),
        )
        .await
        .unwrap();
    let id = opened["id"].as_str().unwrap().to_owned();
    let started = manager
        .ui_request(
            "hello",
            &generation,
            resource(ResourceRequest::RpcStart {
                id: id.clone(),
                method: "fixture/echo".into(),
                params: json!({"native":42}),
                timeout_ms: 2000,
            }),
        )
        .await
        .unwrap();
    let frames = tokio::time::timeout(Duration::from_secs(3), async {
        let mut after = 0;
        let mut received = Vec::new();
        loop {
            let result = manager
                .ui_request(
                    "hello",
                    &generation,
                    resource(ResourceRequest::RpcPoll {
                        id: id.clone(),
                        after,
                    }),
                )
                .await
                .unwrap();
            for event in result["events"].as_array().unwrap() {
                after = event["sequence"].as_u64().unwrap();
                received.push(event["frame"].clone());
            }
            if received
                .iter()
                .any(|frame| frame["operation_id"] == started["operation_id"])
            {
                break received;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(frames[0]["method"], "fixture/started");
    assert_eq!(frames.last().unwrap()["result"], json!({"native":42}));
    // Answer a native server request with a valid JSON null result, then observe its asynchronous response.
    let after = frames.len() as u64;
    let pending = manager
        .ui_request(
            "hello",
            &generation,
            resource(ResourceRequest::RpcStart {
                id: id.clone(),
                method: "fixture/permission".into(),
                params: json!({}),
                timeout_ms: 2000,
            }),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let events = manager
                .ui_request(
                    "hello",
                    &generation,
                    resource(ResourceRequest::RpcPoll {
                        id: id.clone(),
                        after,
                    }),
                )
                .await
                .unwrap();
            if events["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["frame"]["id"] == "server-permission")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    manager
        .ui_request(
            "hello",
            &generation,
            resource(ResourceRequest::RpcRespond {
                id: id.clone(),
                request_id: json!("server-permission"),
                result: serde_json::Value::Null,
                error: None,
            }),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let events = manager
                .ui_request(
                    "hello",
                    &generation,
                    resource(ResourceRequest::RpcPoll {
                        id: id.clone(),
                        after,
                    }),
                )
                .await
                .unwrap();
            if let Some(response) = events["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["frame"]["operation_id"] == pending["operation_id"])
            {
                assert_eq!(
                    response["frame"]["result"],
                    json!({"permission_reply":null})
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    manager.disable("hello").await.unwrap();
    #[cfg(target_os = "linux")]
    tokio::time::timeout(Duration::from_secs(6), async {
        let path = PathBuf::from(format!(
            "/proc/{}",
            frames[0]["params"]["pid"].as_u64().unwrap()
        ));
        while path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scoped RPC subprocess was not reaped");

    assert!(
        manager
            .ui_request(
                "hello",
                &generation,
                resource(ResourceRequest::RpcPoll {
                    id: id.clone(),
                    after: 0
                })
            )
            .await
            .is_err()
    );
    manager.enable("hello").await.unwrap();
    let next = manager.snapshot().await[0].generation.clone().unwrap();
    assert!(
        manager
            .ui_request(
                "hello",
                &next,
                resource(ResourceRequest::RpcPoll { id, after: 0 })
            )
            .await
            .is_err()
    );
    manager.shutdown().await;
    database.close().await.unwrap();
}
