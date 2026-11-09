use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

fn codex(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_virtade"))
        .args(args)
        .env(
            "VIRTADE_CODEX_EXECUTABLE",
            env!("CARGO_BIN_EXE_virtade-cli-fixture"),
        )
        .output()
        .unwrap()
}
fn frames(output: &Output) -> Vec<Value> {
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn cli_runs_persists_and_resumes_same_local_session_across_processes() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("virtade.db");
    let cwd = text(temp.path());
    let first = codex(&["run", "codex", cwd, text(&db), "fixture prompt"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first = frames(&first);
    let local = first[0]["session"]["id"].as_str().unwrap();
    assert!(first.iter().any(|frame| frame["cursor"].as_u64().is_some()));
    let second = codex(&[
        "run",
        "codex",
        cwd,
        text(&db),
        "follow up",
        "--resume",
        local,
    ]);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(frames(&second)[0]["session"]["id"], local);
    let tasks = codex(&["tasks", text(&db), local]);
    assert!(tasks.status.success());
    let tasks = frames(&tasks);
    let list = tasks[0]["tasks"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|task| task["state"] == "completed"));
    let history = codex(&["history", text(&db), local]);
    assert!(history.status.success());
    let history = frames(&history);
    let events = history[0]["events"].as_array().unwrap();
    assert!(events.len() >= 8);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["sequence"].as_u64(), Some(index as u64 + 1));
    }
}

#[test]
fn cli_claude_crash_leaves_unknown_and_refuses_new_submit() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("virtade.db");
    let cwd = text(temp.path());
    let command = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_virtade"))
            .args(args)
            .env(
                "VIRTADE_NODE_EXECUTABLE",
                env!("CARGO_BIN_EXE_virtade-cli-claude-fixture"),
            )
            .env(
                "VIRTADE_CLAUDE_BRIDGE",
                env!("CARGO_BIN_EXE_virtade-cli-claude-fixture"),
            )
            .output()
            .unwrap()
    };
    let first = command(&["run", "claude", cwd, text(&db), "crash"]);
    assert!(!first.status.success());
    let first = frames(&first);
    let local = first[0]["session"]["id"].as_str().unwrap();
    let tasks = command(&["tasks", text(&db), local]);
    assert!(tasks.status.success());
    assert_eq!(frames(&tasks)[0]["tasks"][0]["state"], "unknown");
    let retry = command(&[
        "run",
        "claude",
        cwd,
        text(&db),
        "no replay",
        "--resume",
        local,
    ]);
    assert!(!retry.status.success());
    assert!(String::from_utf8_lossy(&retry.stderr).contains("unresolved"));
}
