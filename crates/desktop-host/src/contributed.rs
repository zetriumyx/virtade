//! Versioned JSON Agent Provider ABI over the existing capability-scoped plugin service bridge.
use crate::ConnectedProvider;
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle};
use virt_protocol::ServiceKey;
use virtade_agent_core::*;
use virtade_plugin_runtime::PluginManager;
struct Channel {
    manager: Arc<PluginManager>,
    owner: String,
    generation: String,
    key: ServiceKey,
    connection: String,
}
impl Channel {
    async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let value=self.manager.call(&self.owner,&self.generation,self.key.clone(),json!({"api_version":1,"connection_id":self.connection,"method":method,"params":params})).await.map_err(AgentError::Protocol)?;
        serde_json::from_value(value).map_err(|error| AgentError::Protocol(error.to_string()))
    }
}
struct ContributedProvider {
    info: ProviderInfo,
    channel: Arc<Channel>,
    pump: Mutex<Option<JoinHandle<()>>>,
}
pub(crate) async fn connect(
    manager: Arc<PluginManager>,
    provider: &str,
    cwd: std::path::PathBuf,
) -> Result<ConnectedProvider> {
    // Namespaced contribution IDs prevent a third party from impersonating a built-in provider.
    let (owner, definition) = manager
        .contributions()
        .await
        .into_iter()
        .find(|(_, view)| view.kind == virt::ContributionKind::AgentProvider && view.id == provider)
        .ok_or_else(|| AgentError::Unsupported(format!("provider {provider}")))?;
    if definition.spec["api_version"] != 1 {
        return Err(AgentError::Unsupported("Agent Provider ABI version".into()));
    }
    let key: ServiceKey = serde_json::from_value(definition.spec["service"].clone())
        .map_err(|error| AgentError::Protocol(error.to_string()))?;
    let package = manager
        .snapshot()
        .await
        .into_iter()
        .find(|plugin| plugin.package.manifest.id == owner)
        .ok_or_else(|| AgentError::Closed("Provider plugin is unavailable".into()))?;
    let generation = package
        .generation
        .ok_or_else(|| AgentError::Closed("Provider plugin is disabled".into()))?;
    let response = manager
        .call(
            &owner,
            &generation,
            key.clone(),
            json!({"api_version":1,"method":"provider/connect","params":{"cwd":cwd}}),
        )
        .await
        .map_err(AgentError::Protocol)?;
    let connection = response["connection_id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 200)
        .ok_or_else(|| {
            AgentError::Protocol("Provider connection_id is missing or oversized".into())
        })?
        .into();
    let mut info: ProviderInfo = serde_json::from_value(response["info"].clone())
        .map_err(|error| AgentError::Protocol(error.to_string()))?;
    info.id = provider.into();
    info.name = definition.title;
    let channel = Arc::new(Channel {
        manager,
        owner,
        generation,
        key,
        connection,
    });
    let (sender, receiver) = mpsc::channel(128);
    let poll = channel.clone();
    let provider_id = info.id.clone();
    let version = info.native_version.clone();
    let pump = tokio::spawn(async move {
        let mut cursor = 0;
        loop {
            tokio::select! {_=sender.closed()=>break,_=tokio::time::sleep(Duration::from_millis(100))=>{}}
            let events: Vec<AgentEvent> = match poll
                .call("events/poll", json!({"after":cursor,"limit":64}))
                .await
            {
                Ok(events) => events,
                Err(error) => {
                    let _ = sender.try_send(Err(error));
                    break;
                }
            };
            if events.len() > 64 {
                let _ = sender.try_send(Err(AgentError::Protocol(
                    "Provider event batch exceeds limits".into(),
                )));
                break;
            }
            for mut event in events {
                if event.schema_version != 1 || event.sequence <= cursor {
                    let _ = sender.try_send(Err(AgentError::Protocol(
                        "Invalid Provider event version or sequence".into(),
                    )));
                    return;
                }
                cursor = event.sequence;
                event.provider = provider_id.clone();
                event.native_version = version.clone();
                if sender.try_send(Ok(event)).is_err() {
                    return;
                }
            }
        }
    });
    Ok((
        Arc::new(ContributedProvider {
            info,
            channel,
            pump: Mutex::new(Some(pump)),
        }),
        receiver,
    ))
}
#[async_trait]
impl AgentProvider for ContributedProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }
    async fn create_session(&self, config: SessionConfig) -> Result<Session> {
        self.channel.call("session/create", json!(config)).await
    }
    async fn resume_session(&self, id: &str, config: SessionConfig) -> Result<Session> {
        self.channel
            .call("session/resume", json!({"session_id":id,"config":config}))
            .await
    }
    async fn submit(&self, id: &str, text: &str) -> Result<Task> {
        self.channel
            .call("task/submit", json!({"session_id":id,"text":text}))
            .await
    }
    async fn cancel(&self, task: &Task) -> Result<()> {
        self.channel.call("task/cancel", json!(task)).await
    }
    async fn respond_approval(&self, response: ApprovalResponse) -> Result<()> {
        self.channel.call("approval/respond", json!(response)).await
    }
    async fn shutdown(&self) -> Result<()> {
        let pump = self.pump.lock().expect("provider pump").take();
        if let Some(pump) = pump {
            pump.abort();
            let _ = pump.await;
        }
        self.channel.call("provider/disconnect", Value::Null).await
    }
}
impl Drop for ContributedProvider {
    fn drop(&mut self) {
        if let Ok(mut pump) = self.pump.lock()
            && let Some(pump) = pump.take()
        {
            pump.abort();
        }
    }
}
