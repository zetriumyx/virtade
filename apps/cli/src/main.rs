use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use uuid::Uuid;
use virtade_agent_claude::{ClaudeConfig, ClaudeProvider};
use virtade_agent_codex::{CodexConfig, CodexProvider};
use virtade_agent_core::{
    AgentError, AgentEvent, AgentProvider, ApprovalDecision, ApprovalResponse, TaskOutcome,
};
use virtade_storage::Store;
use virtade_workspace::WorkspaceSession;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn connect(
    name: &str,
    cwd: PathBuf,
) -> Result<(
    Arc<dyn AgentProvider>,
    mpsc::Receiver<virtade_agent_core::Result<AgentEvent>>,
)> {
    match name {
        "codex" => {
            let mut config = CodexConfig::new(cwd);
            if let Some(program) = std::env::var_os("VIRTADE_CODEX_EXECUTABLE") {
                config.process.program = program.into();
            }
            config.process.inherit_env.push("CODEX_HOME".into());
            let (provider, events) = CodexProvider::connect(config).await?;
            Ok((provider, events))
        }
        "claude" => {
            let script = std::env::var_os("VIRTADE_CLAUDE_BRIDGE")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../packages/claude-bridge/dist/main.js")
                });
            if !script.exists() {
                return Err(AgentError::InvalidState("run pnpm install --frozen-lockfile && pnpm build first, or set VIRTADE_CLAUDE_BRIDGE".into()).into());
            }
            let mut config = ClaudeConfig::new(cwd, script.canonicalize()?);
            if let Some(program) = std::env::var_os("VIRTADE_NODE_EXECUTABLE") {
                config.process.program = program.into();
            }
            let (provider, events) = ClaudeProvider::connect(config).await?;
            Ok((provider, events))
        }
        _ => Err(AgentError::Unsupported(format!("provider {name}")).into()),
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = execute().await {
        eprintln!("virtade: {error}");
        std::process::exit(1);
    }
}

async fn execute() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [command, name] if command == "doctor" => {
            let (provider, _events) = connect(name, std::env::current_dir()?).await?;
            println!(
                "{}",
                json!({"info":provider.info(), "model_task_tested":false})
            );
            provider.shutdown().await?;
            Ok(())
        }
        [command, name, cwd, db, prompt] if command == "run" => {
            run(name, cwd, db, prompt, None).await
        }
        [command, name, cwd, db, prompt, flag, session]
            if command == "run" && flag == "--resume" =>
        {
            run(name, cwd, db, prompt, Some(session.clone())).await
        }
        [command, db, cwd] if command == "sessions" => {
            let store = Store::open(db).await?;
            let result: Result<()> = async {
                let workspace = store.workspace(PathBuf::from(cwd), "Workspace".into()).await?;
                println!("{}", json!({"recovered_tasks":store.recovered_tasks(), "sessions":store.sessions(workspace.id).await?})); Ok(())
            }.await;
            store.close().await?;
            result
        }
        [command, db, session] if command == "tasks" || command == "history" => {
            let store = Store::open(db).await?;
            let result: Result<()> = async {
                if command == "tasks" {
                    println!("{}", json!({"tasks":store.tasks(session.clone()).await?}));
                } else {
                    println!(
                        "{}",
                        json!({"events":store.events(session.clone(),0,100).await?})
                    );
                }
                Ok(())
            }
            .await;
            store.close().await?;
            result
        }
        _ => {
            eprintln!(
                "Usage:\n  virtade doctor <codex|claude>\n  virtade run <codex|claude> <cwd> <database> <prompt> [--resume <local-session-id>]\n  virtade sessions <database> <cwd>\n  virtade tasks <database> <local-session-id>\n  virtade history <database> <local-session-id>\n\nApproval requests are denied by this CLI; use the desktop app for interactive approval."
            );
            Err(AgentError::InvalidState("invalid command".into()).into())
        }
    }
}

async fn run(name: &str, cwd: &str, db: &str, prompt: &str, resume: Option<String>) -> Result<()> {
    let cwd = PathBuf::from(cwd).canonicalize()?;
    let store = Store::open(db).await?;
    let (provider, mut events) = match connect(name, cwd.clone()).await {
        Ok(value) => value,
        Err(error) => {
            store.close().await?;
            return Err(error);
        }
    };
    let mut local_task = None;
    let result: Result<()> = async {
        let host = WorkspaceSession::open(store.clone(), provider.clone(), cwd, resume).await?;
        println!("{}",json!({"session":host.session,"recovered_tasks":store.recovered_tasks()}));
        let (stored, native) = host.submit(Uuid::new_v4().to_string(), prompt.into()).await?;
        local_task = Some(stored.id.clone());
        println!("{}",json!({"task":stored}));
        let mut expires = tokio::time::Instant::now()+Duration::from_secs(1800);
        loop {
            tokio::select! {
                event = events.recv() => {
                    let event = event.ok_or_else(|| AgentError::Closed("provider event stream ended".into()))??;
                    let cursor = if event.session_id.as_deref() == Some(&host.session.native_id) { Some(host.ingest(event.clone()).await?) } else { None };
                    println!("{}",json!({"event":event,"cursor":cursor}));
                    if let Some(id) = &event.approval_id {
                        provider.respond_approval(ApprovalResponse { approval_id:id.clone(),session_id:event.session_id.clone().unwrap_or_default(),task_id:event.task_id.clone().unwrap_or_default(),decision:ApprovalDecision::Deny }).await?;
                    }
                    if event.task_id.as_deref() == Some(&native.native_id) && let Some(outcome) = event.outcome {
                        return if outcome == TaskOutcome::Completed { Ok(()) } else { Err(AgentError::InvalidState(format!("native task outcome: {outcome:?}")).into()) };
                    }
                }
                signal = tokio::signal::ctrl_c() => { signal?; provider.cancel(&native).await?; expires=tokio::time::Instant::now()+Duration::from_secs(10); }
                _ = tokio::time::sleep_until(expires) => return Err(AgentError::Timeout("task outcome unknown; do not automatically resubmit".into()).into()),
            }
        }
    }.await;
    let shutdown = provider.shutdown().await;
    // Closing the process without a native terminal result never marks a task completed.
    if result.is_err()
        && let Some(id) = local_task
    {
        store.mark_unknown(id).await?;
    }
    let closed = store.close().await;
    result?;
    shutdown?;
    closed?;
    Ok(())
}
