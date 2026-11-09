use std::sync::Arc;
use tauri::Manager;
use virtade_desktop_host::{AppHost, DesktopRequest, HostError};
struct IpcToken(String);

// Compiled only into the explicit test binary. Production builds have no report/exit IPC.
#[cfg(feature = "native-smoke")]
#[tauri::command]
fn native_smoke_report(
    window: tauri::WebviewWindow,
    token: tauri::State<'_, IpcToken>,
    raw: tauri::ipc::Request<'_>,
    result: serde_json::Value,
) -> Result<(), String> {
    if window.label() != "main"
        || raw
            .headers()
            .get("x-virtade-host")
            .and_then(|v| v.to_str().ok())
            != Some(token.0.as_str())
    {
        return Err("forbidden".into());
    }
    let destination = std::env::var_os("VIRTADE_SMOKE_REPORT").ok_or("missing report path")?;
    std::fs::write(
        destination,
        serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    window
        .app_handle()
        .exit(if result["ok"] == true { 0 } else { 1 });
    Ok(())
}

#[tauri::command]
async fn desktop_request(
    window: tauri::WebviewWindow,
    host: tauri::State<'_, Arc<AppHost>>,
    token: tauri::State<'_, IpcToken>,
    raw: tauri::ipc::Request<'_>,
    version: u32,
    request: DesktopRequest,
) -> Result<serde_json::Value, HostError> {
    if window.label() != "main"
        || raw
            .headers()
            .get("x-virtade-host")
            .and_then(|value| value.to_str().ok())
            != Some(token.0.as_str())
    {
        return Err(HostError::new(
            "forbidden",
            "This window cannot access workbench commands",
        ));
    }
    host.request(version, request).await
}

pub fn run() {
    let token = uuid::Uuid::new_v4().to_string();
    let development = cfg!(debug_assertions) && !cfg!(feature = "custom-protocol");
    let initialization = format!(
        "if(window.top===window&&((location.protocol==='tauri:'&&location.hostname==='localhost'&&!location.port)||(['http:','https:'].includes(location.protocol)&&location.hostname==='tauri.localhost'&&!location.port)||({development}&&location.protocol==='http:'&&['localhost','127.0.0.1'].includes(location.hostname)&&location.port==='1420'))){{Object.defineProperty(window,'__VIRTADE_HOST_TOKEN__',{{value:{},writable:false,configurable:false}});}}",
        serde_json::to_string(&token).expect("token")
    );
    let builder = tauri::Builder::default()
        .plugin(
            tauri::plugin::Builder::<tauri::Wry, ()>::new("navigation-boundary")
                .on_navigation(move |_, url| {
                    crate::security::app_origin(
                        url.scheme(),
                        url.host_str(),
                        url.port(),
                        development,
                    ) || (url.scheme() == "about" && matches!(url.path(), "blank" | "srcdoc"))
                })
                .build(),
        )
        .manage(IpcToken(token))
        .append_invoke_initialization_script(initialization)
        .setup(|app| {
            let directory = app.path().app_data_dir()?;
            std::fs::create_dir_all(&directory)?;
            let host = tauri::async_runtime::block_on(AppHost::open(directory.join("virtade.db")))?;
            app.manage(host);
            Ok(())
        });
    #[cfg(not(feature = "native-smoke"))]
    let builder = builder.invoke_handler(tauri::generate_handler![desktop_request]);
    #[cfg(feature = "native-smoke")]
    let builder = builder
        .invoke_handler(tauri::generate_handler![
            desktop_request,
            native_smoke_report
        ])
        .on_page_load(|window, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished
                && std::env::var_os("VIRTADE_SMOKE_REPORT").is_some()
            {
                let _ = window.eval(include_str!("native-smoke.js"));
            }
        });
    builder
        .build(tauri::generate_context!())
        .expect("build virtade")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let host = app.state::<Arc<AppHost>>().inner().clone();
                let _ = tauri::async_runtime::block_on(host.shutdown());
            }
        });
}
