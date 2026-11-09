//! Deliberately synthetic protocol peer. Never a model/agent simulator in product code.
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

fn write(out: &Arc<Mutex<io::Stdout>>, value: Value) {
    let mut out = out.lock().unwrap();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("rpc");
    if mode == "descendant" {
        let marker = args.get(2).unwrap();
        loop {
            std::fs::write(marker, format!("{:?}", std::time::SystemTime::now())).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let out = Arc::new(Mutex::new(io::stdout()));
    if mode == "invalid" {
        println!("not-json");
        std::thread::sleep(Duration::from_secs(10));
        return;
    }
    if mode == "oversized" {
        println!("{}", "x".repeat(8192));
        std::thread::sleep(Duration::from_secs(10));
        return;
    }
    if mode == "truncated" {
        print!("{{\"id\":1");
        return;
    }
    if mode == "descendant-parent" {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("descendant")
            .arg(args.get(2).unwrap())
            .spawn()
            .unwrap();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        write(&out, json!({"method":"ready","params":{}}));
    }
    if mode == "flood" {
        for i in 0..1000 {
            write(&out, json!({"method":"flood","params":{"i":i}}));
        }
    }
    if mode == "stderr" {
        let _ = io::stderr().write_all(&vec![b'e'; 100_000]);
    }
    let mut turn_count = 0;
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let frame: Value = serde_json::from_str(&line).unwrap();
        let method = frame["method"].as_str().unwrap_or("");
        let id = frame["id"].clone();
        match method {
            "initialize" => write(
                &out,
                json!({"id":id,"result":{"userAgent":"protocol-fixture/1"}}),
            ),
            "initialized" => (),
            "echo" => write(&out, json!({"id":id,"result":frame["params"]})),
            "environment" => write(
                &out,
                json!({"id":id,"result":{"allowed":std::env::var("FIXTURE_ALLOWED").ok(),"forbidden":std::env::var("FIXTURE_FORBIDDEN").ok(),"args":args}}),
            ),
            "delay" => {
                let out = out.clone();
                let ms = frame["params"]["ms"].as_u64().unwrap();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(ms));
                    write(&out, json!({"id":id,"result":ms}));
                });
            }
            "never" => (),
            "error" => write(
                &out,
                json!({"id":id,"error":{"code":-123,"message":"fixture error"}}),
            ),
            "crash" => std::process::exit(7),
            "thread/start" => write(
                &out,
                json!({"id":id,"result":{"thread":{"id":"session-1"}}}),
            ),
            "thread/resume" if frame["params"]["threadId"] == "session-1" => write(
                &out,
                json!({"id":id,"result":{"thread":{"id":"session-1"}}}),
            ),
            "thread/resume" => write(
                &out,
                json!({"id":id,"error":{"code":404,"message":"session not found"}}),
            ),
            "turn/start" => {
                turn_count += 1;
                let turn = format!("turn-{}-{turn_count}", std::process::id());
                if frame["params"]["input"][0]["text"] == "unknown" {
                    continue;
                }
                write(&out, json!({"id":id,"result":{"turn":{"id":turn}}}));
                write(
                    &out,
                    json!({"method":"turn/started","params":{"threadId":"session-1","turn":{"id":turn,"status":"inProgress"}}}),
                );
                write(
                    &out,
                    json!({"method":"item/agentMessage/delta","params":{"threadId":"session-1","turnId":turn,"delta":"fixture","native_extra":{"preserve":true}}}),
                );
                write(
                    &out,
                    json!({"id":format!("approval-{turn_count}"),"method":"item/commandExecution/requestApproval","params":{"threadId":"session-1","turnId":turn,"itemId":"command-1","command":"fixture command","startedAtMs":1}}),
                );
            }
            "turn/interrupt" => {
                write(&out, json!({"id":id,"result":{}}));
                write(
                    &out,
                    json!({"method":"turn/completed","params":{"threadId":"session-1","turn":{"id":frame["params"]["turnId"],"status":"interrupted"}}}),
                );
            }
            "" if id.is_string() => {
                write(
                    &out,
                    json!({"method":"fixture/approvalResult","params":{"original":frame}}),
                );
                write(
                    &out,
                    json!({"method":"turn/completed","params":{"threadId":"session-1","turn":{"id":format!("turn-{}-{turn_count}", std::process::id()),"status":"completed"}}}),
                );
            }
            _ => write(
                &out,
                json!({"id":id,"error":{"code":-32601,"message":"unknown fixture method"}}),
            ),
        }
    }
}
