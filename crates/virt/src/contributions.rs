use crate::Context;
use serde_json::Value;
use std::sync::Arc;
use virt_protocol::{Capability, Error, Result};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContributionKind {
    Panel,
    Sidebar,
    ChatRenderer,
    ToolRenderer,
    Command,
    Menu,
    Keybinding,
    Status,
    Settings,
    Editor,
    WorkspaceView,
    AgentProvider,
    Workflow,
}
impl ContributionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Panel => "panel",
            Self::Sidebar => "sidebar",
            Self::ChatRenderer => "chat_renderer",
            Self::ToolRenderer => "tool_renderer",
            Self::Command => "command",
            Self::Menu => "menu",
            Self::Keybinding => "keybinding",
            Self::Status => "status",
            Self::Settings => "settings",
            Self::Editor => "editor",
            Self::WorkspaceView => "workspace_view",
            Self::AgentProvider => "agent_provider",
            Self::Workflow => "workflow",
        }
    }
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contribution {
    pub id: String,
    pub kind: ContributionKind,
    pub title: String,
    pub schema_version: u32,
    pub spec: Value,
}
#[derive(Clone)]
pub(crate) struct Entry {
    pub owner: String,
    pub value: Contribution,
}
impl Context {
    pub fn contribute(&self, value: Contribution) -> Result<()> {
        self.require(Capability::Contribution(value.kind.as_str().into()))?;
        if !value.id.starts_with(&format!("{}.", self.owner))
            || value.schema_version != 1
            || value.id.len() > 200
            || value.title.len() > 200
            || serde_json::to_vec(&value.spec).map_or(true, |bytes| bytes.len() > 1024 * 1024)
        {
            return Err(Error::InvalidManifest(
                "invalid contribution identity/version/size".into(),
            ));
        }
        self.scope.register(|| {
            let mut registry = self.host.contributions.lock().expect("contributions");
            let owned: Vec<_> = registry
                .values()
                .filter(|entry| entry.owner == self.owner)
                .collect();
            let size = serde_json::to_vec(&value.spec)
                .map_err(|error| Error::InvalidManifest(error.to_string()))?
                .len();
            if owned.len() >= 64
                || owned
                    .iter()
                    .map(|entry| {
                        serde_json::to_vec(&entry.value.spec).map_or(0, |bytes| bytes.len())
                    })
                    .sum::<usize>()
                    + size
                    > 4 * 1024 * 1024
            {
                return Err(Error::InvalidManifest(
                    "contribution count/byte quota exceeded".into(),
                ));
            }
            if registry.contains_key(&value.id) {
                return Err(Error::InvalidManifest("contribution ID conflict".into()));
            }
            let id = value.id.clone();
            registry.insert(
                id.clone(),
                Entry {
                    owner: self.owner.clone(),
                    value,
                },
            );
            let host = Arc::downgrade(&self.host);
            Ok((
                (),
                Box::new(move || {
                    if let Some(host) = host.upgrade() {
                        host.contributions
                            .lock()
                            .expect("contributions")
                            .remove(&id);
                    }
                }) as Box<dyn FnOnce() + Send>,
            ))
        })
    }
}
impl crate::Kernel {
    pub fn contributions(&self) -> Vec<(String, Contribution)> {
        self.host
            .contributions
            .lock()
            .expect("contributions")
            .values()
            .map(|entry| (entry.owner.clone(), entry.value.clone()))
            .collect()
    }
}
