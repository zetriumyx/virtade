use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
fn send(value: Value) {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, &value).unwrap();
    writeln!(output).unwrap();
    output.flush().unwrap();
}
fn main() {
    let mut prompt = None;
    for line in io::stdin().lock().lines() {
        let frame: Value = serde_json::from_str(&line.unwrap()).unwrap();
        assert_eq!(frame["jsonrpc"], "2.0");
        let id = frame["id"].clone();
        match frame["method"].as_str() {
            Some("initialize") => send(
                json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":1,"agentInfo":{"name":"DSH fixture","version":"fixture"},"agentCapabilities":{"sessionCapabilities":{"resume":{}}}}}),
            ),
            Some("session/new" | "session/resume") => {
                send(json!({"jsonrpc":"2.0","id":id,"result":{"sessionId":"native-session"}}))
            }
            Some("session/prompt") => {
                prompt = Some(id);
                send(
                    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"native-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"fixture output"}}}}),
                );
                send(
                    json!({"jsonrpc":"2.0","id":"permission-1","method":"session/request_permission","params":{"sessionId":"native-session","toolCall":{"toolCallId":"tool-1"},"options":[{"optionId":"yes","kind":"allow_once","name":"Yes"},{"optionId":"no","kind":"reject_once","name":"No"}]}}),
                );
            }
            Some("session/cancel") => {
                if let Some(id) = prompt.take() {
                    send(json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"cancelled"}}));
                }
            }
            None if id == "permission-1" => {
                assert!(
                    frame["result"]["outcome"]["outcome"] == "selected"
                        || frame["result"]["outcome"]["outcome"] == "cancelled"
                );
                if let Some(id) = prompt.take() {
                    send(json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}}));
                }
            }
            _ => panic!("Unexpected frame {frame}"),
        }
    }
}
