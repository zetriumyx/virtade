//! Synthetic Provider ABI fixture compiled to a real isolated WebAssembly component.
use serde_json::json;
use std::{path::PathBuf, time::Duration};
use virt_protocol::{Capability, Manifest, ServiceKey};
use virtade_desktop_host::{AppHost, DesktopRequest};
#[tokio::test]
async fn third_party_provider_is_discovered_without_core_registration_and_cannot_forge_identity() {
    let temp = tempfile::TempDir::new().unwrap();
    let source = temp.path().join("package");
    std::fs::create_dir(&source).unwrap();
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/wasm-hello");
    let mut manifest: Manifest =
        serde_json::from_slice(&std::fs::read(example.join("manifest.json")).unwrap()).unwrap();
    manifest.permissions.extend([
        Capability::ServiceProvide(ServiceKey::new("hello.agent", 1)),
        Capability::ServiceCall(ServiceKey::new("hello.agent", 1)),
        Capability::Contribution("agent_provider".into()),
    ]);
    std::fs::write(
        source.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::copy(
        example.join("provider-test.component.wasm"),
        source.join("component.wasm"),
    )
    .unwrap();
    let host = AppHost::open(temp.path().join("state.db")).await.unwrap();
    host.plugins
        .install(source, manifest.permissions)
        .await
        .unwrap();
    host.plugins.enable("hello").await.unwrap();
    assert!(
        host.snapshot()
            .await
            .unwrap()
            .contributions
            .iter()
            .any(|(_, view)| view.id == "hello.provider")
    );
    let workspace = host
        .request(
            1,
            DesktopRequest::OpenWorkspace {
                root: temp.path().into(),
            },
        )
        .await
        .unwrap();
    let session = host
        .request(
            1,
            DesktopRequest::OpenSession {
                workspace_id: workspace["id"].as_str().unwrap().into(),
                provider: "hello.provider".into(),
                resume: None,
            },
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap().to_owned();
    host.request(
        1,
        DesktopRequest::Submit {
            session_id: id.clone(),
            request_key: "test".into(),
            text: "contract fixture".into(),
        },
    )
    .await
    .unwrap();
    let history = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let history = host
                .request(
                    1,
                    DesktopRequest::History {
                        session_id: id.clone(),
                        after: 0,
                        limit: 100,
                    },
                )
                .await
                .unwrap();
            if history["tasks"][0]["state"] == "completed" {
                break history;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        history["events"][0]["event"]["provider"],
        json!("hello.provider")
    );
    assert_eq!(
        history["events"][0]["event"]["native_version"],
        "synthetic-contract-test"
    );
    host.shutdown().await.unwrap();
}
