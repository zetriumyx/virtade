//! Protocol-only subprocess fixture. Never invokes a model.
use std::io::{BufRead, Write};
fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut lines = stdin.lock().lines();
    while let Some(line) = lines.next() {
        let Ok(line) = line else {
            break;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(id) = value.get("id") {
            let _ = writeln!(
                stdout,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","method":"fixture/started","params":{"method":value["method"],"pid":std::process::id()}})
            );
            let _ = stdout.flush();
            std::thread::sleep(std::time::Duration::from_millis(30));
            let result = if value["method"] == "fixture/permission" {
                writeln!(stdout,"{}",serde_json::json!({"jsonrpc":"2.0","id":"server-permission","method":"fixture/permission","params":{}})).unwrap();
                stdout.flush().unwrap();
                let response: serde_json::Value =
                    serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
                assert_eq!(response["id"], "server-permission");
                assert!(response.get("result").is_some());
                serde_json::json!({"permission_reply":response["result"]})
            } else {
                value["params"].clone()
            };
            let _ = writeln!(
                stdout,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})
            );
            let _ = stdout.flush();
        }
    }
}
