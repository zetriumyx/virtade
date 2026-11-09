use crate::{CodexConfig, CodexProvider, EVENT_TOPIC};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};
use virt::{Context, Plugin, Registration};
use virt_protocol::{Capability, Error, Manifest, Result, ServiceKey};
use virtade_agent_core::{AgentProvider, ApprovalResponse, service::ProviderService};

pub fn service_key() -> ServiceKey {
    ServiceKey::new("virtade.agent.codex", 1)
}

pub struct CodexPlugin {
    config: CodexConfig,
    running: Mutex<Option<Arc<CodexProvider>>>,
}
impl CodexPlugin {
    pub fn new(config: CodexConfig) -> Self {
        Self {
            config,
            running: Mutex::new(None),
        }
    }
}

fn failure(error: impl std::fmt::Display) -> Error {
    Error::PluginFailure {
        plugin: "agent-codex".into(),
        message: error.to_string(),
    }
}

#[async_trait]
impl Plugin for CodexPlugin {
    fn manifest(&self) -> Manifest {
        let mut manifest = Manifest::new("agent-codex");
        manifest.permissions.extend([
            Capability::ServiceProvide(service_key()),
            Capability::EventPublish(EVENT_TOPIC.into()),
        ]);
        manifest
    }
    async fn start(&self, ctx: Context) -> Result<()> {
        let (provider, mut events) = CodexProvider::connect(self.config.clone())
            .await
            .map_err(failure)?;
        ctx.provide(
            service_key(),
            Arc::new(ProviderService::new("agent-codex", provider.clone())),
            Registration::New { replaceable: false },
        )?;
        let event_context = ctx.clone();
        ctx.spawn(async move {
            while let Some(event) = events.recv().await {
                let payload = match event {
                    Ok(event) => json!({"event": event}),
                    Err(error) => json!({"error": error.to_string()}),
                };
                // UI observers cannot approve requests by intercepting this event.
                if event_context
                    .emit(EVENT_TOPIC, payload, std::time::Duration::from_secs(2))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        })?;
        *self.running.lock().expect("provider lock") = Some(provider);
        Ok(())
    }
    async fn stop(&self, _ctx: Context) -> Result<()> {
        let provider = self.running.lock().expect("provider lock").take();
        if let Some(provider) = provider {
            provider.shutdown().await.map_err(failure)?;
        }
        Ok(())
    }
}

impl CodexPlugin {
    /// Trusted host approval path; not exposed as a kernel ServiceCall capability.
    pub async fn respond_approval(&self, response: ApprovalResponse) -> Result<()> {
        let provider = self
            .running
            .lock()
            .expect("provider lock")
            .clone()
            .ok_or_else(|| failure("provider inactive"))?;
        provider.respond_approval(response).await.map_err(failure)
    }
}
