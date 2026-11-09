//! Public workflow service: capability-scoped Agent control, never an approval or settings channel.
use crate::{AppHost, DesktopRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Weak},
};
use virt::{
    Caller, Context, Contribution, ContributionKind, Plugin, Registration, Service, async_trait,
};
use virt_protocol::{Capability, Error, Manifest, Result, ServiceKey};

pub(crate) fn key() -> ServiceKey {
    ServiceKey::new("virtade.agents", 1)
}
pub(crate) struct AgentServices {
    pub host: Weak<AppHost>,
}
#[async_trait]
impl Plugin for AgentServices {
    fn manifest(&self) -> Manifest {
        let mut manifest = Manifest::new("virtade-agent-services");
        manifest
            .permissions
            .insert(Capability::ServiceProvide(key()));
        manifest
            .permissions
            .insert(Capability::Contribution("agent_provider".into()));
        manifest
    }
    async fn start(&self, context: Context) -> Result<()> {
        context.provide(
            key(),
            Arc::new(AgentService {
                host: self.host.clone(),
            }),
            Registration::New { replaceable: false },
        )?;
        for (provider, title) in [
            ("codex", "OpenAI Codex"),
            ("claude", "Claude Code"),
            ("dsh", "DeepSeek Harness"),
        ] {
            context.contribute(Contribution {
                id: format!("virtade-agent-services.{provider}"),
                kind: ContributionKind::AgentProvider,
                title: title.into(),
                schema_version: 1,
                spec: json!({"provider":provider,"type":"native"}),
            })?;
        }
        Ok(())
    }
}
struct AgentService {
    host: Weak<AppHost>,
}
#[derive(Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Request {
    Providers,
    OpenSession {
        provider: String,
        root: PathBuf,
        resume: Option<String>,
    },
    Submit {
        session_id: String,
        request_key: String,
        text: String,
    },
    Cancel {
        session_id: String,
    },
    History {
        session_id: String,
        after: u64,
        limit: u32,
    },
}
fn fail(error: impl std::fmt::Display) -> Error {
    Error::PluginFailure {
        plugin: "virtade-agent-services".into(),
        message: error.to_string(),
    }
}
fn authorize(caller: &Caller, provider: &str, root: &std::path::Path) -> Result<()> {
    if caller.trusted {
        return Ok(());
    }
    let cap = Capability::AgentControl {
        provider: provider.into(),
        root: root
            .to_str()
            .ok_or_else(|| fail("Unsupported path encoding"))?
            .into(),
    };
    if caller.grants.contains(&cap) {
        Ok(())
    } else {
        Err(Error::PermissionDenied {
            plugin: caller.owner.clone(),
            capability: cap,
        })
    }
}
#[async_trait]
impl Service for AgentService {
    async fn call(&self, _: Value) -> Result<Value> {
        Err(fail("This service requires a kernel-bound caller identity"))
    }
    async fn call_scoped(&self, request: Value, caller: Caller) -> Result<Value> {
        let host = self
            .host
            .upgrade()
            .ok_or_else(|| fail("Application is closed"))?;
        let request: Request = serde_json::from_value(request).map_err(fail)?;
        match request {
            Request::Providers => Ok(
                json!([{"id":"codex","name":"OpenAI Codex"},{"id":"claude","name":"Claude Code"},{"id":"dsh","name":"DeepSeek Harness"}]),
            ),
            Request::OpenSession {
                provider,
                root,
                resume,
            } => {
                authorize(&caller, &provider, &root)?;
                let canonical = root.canonicalize().map_err(fail)?;
                if canonical != root {
                    return Err(fail("Agent scope must use a canonical workspace root"));
                }
                let workspace = host
                    .store
                    .workspace(root, "Plugin workflow".into())
                    .await
                    .map_err(fail)?;
                host.request(
                    1,
                    DesktopRequest::OpenSession {
                        workspace_id: workspace.id,
                        provider,
                        resume,
                    },
                )
                .await
                .map_err(fail)
            }
            command => {
                let session_id = match &command {
                    Request::Submit { session_id, .. }
                    | Request::Cancel { session_id }
                    | Request::History { session_id, .. } => session_id,
                    _ => unreachable!(),
                };
                let session = host.store.session(session_id.clone()).await.map_err(fail)?;
                let workspace = host
                    .store
                    .workspaces()
                    .await
                    .map_err(fail)?
                    .into_iter()
                    .find(|workspace| workspace.id == session.workspace_id)
                    .ok_or_else(|| fail("Workspace missing"))?;
                authorize(&caller, &session.provider, &workspace.root)?;
                let request = match command {
                    Request::Submit {
                        session_id,
                        request_key,
                        text,
                    } => DesktopRequest::Submit {
                        session_id,
                        request_key,
                        text,
                    },
                    Request::Cancel { session_id } => DesktopRequest::Cancel { session_id },
                    Request::History {
                        session_id,
                        after,
                        limit,
                    } => DesktopRequest::History {
                        session_id,
                        after,
                        limit,
                    },
                    _ => unreachable!(),
                };
                host.request(1, request).await.map_err(fail)
            }
        }
    }
}
