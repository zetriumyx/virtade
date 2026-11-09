use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::mpsc;
use virtade_agent_claude::{ClaudeConfig, ClaudeProvider};
use virtade_agent_codex::{CodexConfig, CodexProvider};
use virtade_agent_core::{AgentError, AgentEvent, AgentProvider, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderSettings {
    pub codex_executable: String,
    pub node_executable: String,
    pub claude_bridge: String,
    pub claude_executable: String,
    pub dsh_executable: String,
}
pub type ConnectedProvider = (Arc<dyn AgentProvider>, mpsc::Receiver<Result<AgentEvent>>);
#[async_trait]
pub trait Connector: Send + Sync {
    async fn connect(
        &self,
        name: &str,
        cwd: PathBuf,
        settings: ProviderSettings,
    ) -> Result<ConnectedProvider>;
}
pub struct NativeConnector;
#[async_trait]
impl Connector for NativeConnector {
    async fn connect(
        &self,
        name: &str,
        cwd: PathBuf,
        settings: ProviderSettings,
    ) -> Result<ConnectedProvider> {
        match name {
            "dsh" => {
                let program = if settings.dsh_executable.is_empty() {
                    "dsh".into()
                } else {
                    settings.dsh_executable
                };
                let mut process = virtade_agent_core::rpc::ProcessConfig::new(program, cwd);
                process.args = ["--profile", "acp"].into_iter().map(Into::into).collect();
                process.inherit_env.extend(
                    ["DEEPSEEK_API_KEY", "DSH_HOME"]
                        .into_iter()
                        .map(String::from),
                );
                let (provider, events) = virtade_agent_dsh::DshProvider::connect(process).await?;
                Ok((provider, events))
            }
            "codex" => {
                let mut config = CodexConfig::new(cwd);
                if !settings.codex_executable.is_empty() {
                    config.process.program = settings.codex_executable.into();
                }
                config.process.inherit_env.push("CODEX_HOME".into());
                let (provider, events) = CodexProvider::connect(config).await?;
                Ok((provider, events))
            }
            "claude" => {
                let bridge = if settings.claude_bridge.is_empty() {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../packages/claude-bridge/dist/main.js")
                } else {
                    settings.claude_bridge.into()
                };
                if !bridge.is_file() {
                    return Err(AgentError::InvalidState(
                        "Configure the built Claude SDK bridge in Settings".into(),
                    ));
                }
                let mut config = ClaudeConfig::new(cwd, bridge.canonicalize()?);
                if !settings.node_executable.is_empty() {
                    config.process.program = settings.node_executable.into();
                }
                if !settings.claude_executable.is_empty() {
                    config.process.env.insert(
                        "VIRTADE_CLAUDE_EXECUTABLE".into(),
                        settings.claude_executable.into(),
                    );
                }
                let (provider, events) = ClaudeProvider::connect(config).await?;
                Ok((provider, events))
            }
            _ => Err(AgentError::Unsupported(format!("provider {name}"))),
        }
    }
}
