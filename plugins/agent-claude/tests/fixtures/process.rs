//! Synthetic private bridge peer; never invokes a model.
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
fn send(value: Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}
fn main() {
    let mut task = 0;
    for line in io::stdin().lock().lines() {
        let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = value["id"].clone();
        match value["method"].as_str().unwrap_or("") {
            "initialize" => send(
                json!({"id":id,"result":{"protocol_version":1,"sdk_version":"fixture","capabilities":["session.reserve","session.resume","task.stream","task.interrupt","tool.approval.once"]}}),
            ),
            "session/create" => {
                send(json!({"id":id,"result":{"session_id":"claude-1","reserved":true}}))
            }
            "session/resume" if value["params"]["session_id"] == "claude-1" => {
                send(json!({"id":id,"result":{"session_id":"claude-1","reserved":false}}))
            }
            "session/resume" => {
                send(json!({"id":id,"error":{"code":404,"message":"missing native session"}}))
            }
            "task/submit" => {
                task += 1;
                if value["params"]["text"] == "crash" {
                    std::process::exit(7);
                }
                if value["params"]["text"] == "timeout" {
                    continue;
                }
                let task_id = format!("task-{task}");
                send(json!({"id":id,"result":{"session_id":"claude-1","task_id":task_id}}));
                send(
                    json!({"method":"sdk/message","params":{"session_id":"claude-1","task_id":task_id,"message":{"type":"system","subtype":"init","session_id":"claude-1","extra":"preserved"}}}),
                );
                send(
                    json!({"method":"sdk/message","params":{"session_id":"claude-1","task_id":task_id,"message":{"type":"stream_event","event":{"type":"content_block_delta","delta":{"text":"fixture"}}}}}),
                );
                send(
                    json!({"id":format!("native-approval-{task}"),"method":"tool/approval","params":{"session_id":"claude-1","task_id":task_id,"tool":"Write","input":{"file_path":"fixture"}}}),
                );
            }
            "task/cancel" => {
                send(json!({"id":id,"result":{"cancellation_requested":true}}));
                send(
                    json!({"method":"task/ended","params":{"session_id":"claude-1","task_id":value["params"]["task_id"],"outcome":"unknown","native_result":null,"cancellation_requested":true}}),
                );
            }
            "" => {
                send(json!({"method":"fixture/decision","params":{"original":value}}));
                send(
                    json!({"method":"task/ended","params":{"session_id":"claude-1","task_id":format!("task-{task}"),"outcome":"native_result","native_result":{"subtype":"success","is_error":false},"cancellation_requested":false}}),
                );
            }
            _ => send(
                json!({"id":id,"error":{"code":-32601,"message":"unsupported fixture method"}}),
            ),
        }
    }
}
