#![no_std]
extern crate alloc;
use alloc::string::String;
wit_bindgen::generate!({path:"../../crates/plugin-runtime/wit",world:"plugin"});
#[global_allocator]
static ALLOC: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    core::arch::wasm32::unreachable()
}
struct Hello;
impl Guest for Hello {
    fn activate() -> Result<(), String> {
        virt::plugin::host::service_register("hello.greeting", 1, true, false)?;
        virt::plugin::host::event_subscribe("virtade.agent.codex.event.v1", 0)?;
        virt::plugin::host::contribute(
            r#"{"id":"hello.panel","kind":"panel","title":"Hello from WASM","schema_version":1,"spec":{"type":"iframe","text":"A third-party component registered this panel without changing virtade."}}"#,
        )?;
        #[cfg(feature = "test-provider")]
        {
            virt::plugin::host::service_register("hello.agent", 1, false, false)?;
            virt::plugin::host::contribute(
                r#"{"id":"hello.provider","kind":"agent_provider","title":"Contract test provider","schema_version":1,"spec":{"api_version":1,"service":{"name":"hello.agent","major":1}}}"#,
            )?;
        }
        virt::plugin::host::log("hello activated");
        Ok(())
    }
    fn deactivate() -> Result<(), String> {
        Ok(())
    }
    fn service(name: String, request: String) -> Result<String, String> {
        #[cfg(feature = "test-provider")]
        if name == "hello.agent" {
            return provider_fixture(&request);
        }
        if name != "hello.greeting" {
            return Err(String::from("unknown service"));
        }
        #[cfg(feature = "test-probes")]
        match request.as_str() {
            "\"spin\"" => loop {
                core::hint::spin_loop();
            },
            "\"trap\"" => core::arch::wasm32::unreachable(),
            "\"allocate\"" => {
                let mut memory = alloc::vec::Vec::<u8>::with_capacity(64 * 1024 * 1024);
                memory.resize(64 * 1024 * 1024, 0);
                core::hint::black_box(memory);
            }
            "\"config-probe\"" => {
                virt::plugin::host::config_set("probe", "true")?;
            }
            _ => (),
        }
        // Return the caller's JSON unchanged; no native pointers or Rust ABI are shared.
        Ok(request)
    }
    fn event(_: String, _: String) -> Result<bool, String> {
        virt::plugin::host::log("received Agent event");
        Ok(false)
    }
}
export!(Hello);

// Only compiled for the explicit contract-test artifact; never the distributable hello plugin.
#[cfg(feature = "test-provider")]
fn provider_fixture(request: &str) -> Result<String, String> {
    use alloc::string::ToString;
    use core::sync::atomic::{AtomicBool, Ordering};
    use serde_json::{Value, json};
    static PENDING: AtomicBool = AtomicBool::new(false);
    let request: Value = serde_json::from_str(request).map_err(|error| error.to_string())?;
    let result = match request["method"].as_str() {
        Some("provider/connect") => {
            json!({"connection_id":"test-connection","info":{"id":"untrusted-claimed-id","name":"fixture","native_version":"synthetic-contract-test","capabilities":{}}})
        }
        Some("session/create") => {
            json!({"native_id":"fixture-session","cwd":request["params"]["cwd"],"reserved":false})
        }
        Some("task/submit") => {
            PENDING.store(true, Ordering::SeqCst);
            json!({"native_id":"fixture-task","session_id":"fixture-session"})
        }
        Some("events/poll") if PENDING.swap(false, Ordering::SeqCst) => {
            json!([{"schema_version":1,"provider":"codex","native_version":"forged","sequence":1,"timestamp_ms":1,"session_id":"fixture-session","task_id":"fixture-task","kind":"task_state","native_method":"test/result","payload":{"fixture":true},"approval_id":null,"outcome":"completed"}])
        }
        Some("events/poll") => json!([]),
        Some("provider/disconnect" | "task/cancel") => Value::Null,
        _ => return Err("Unknown contract-test method".into()),
    };
    Ok(result.to_string())
}
