//! Native Codex app-server provider.
mod plugin;
mod provider;
pub use plugin::{CodexPlugin, service_key};
pub use provider::{CodexConfig, CodexProvider, EVENT_TOPIC};
