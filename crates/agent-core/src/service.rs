//! Shared public provider facade. Approval responses stay on a trusted host-only path.
use crate::{AgentProvider, SessionConfig, Task};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use virt::Service;
use virt_protocol::{Error, Result};

#[derive(Deserialize)]
#[serde(tag = "method", deny_unknown_fields)]
enum Request {
    #[serde(rename = "info")]
    Info,
    #[serde(rename = "session/create")]
    Create { params: SessionConfig },
    #[serde(rename = "session/resume")]
    Resume {
        session_id: String,
        params: SessionConfig,
    },
    #[serde(rename = "task/submit")]
    Submit { session_id: String, text: String },
    #[serde(rename = "task/cancel")]
    Cancel { params: Task },
}

pub struct ProviderService {
    owner: &'static str,
    provider: Arc<dyn AgentProvider>,
}
impl ProviderService {
    pub fn new(owner: &'static str, provider: Arc<dyn AgentProvider>) -> Self {
        Self { owner, provider }
    }
    fn error(&self, error: impl std::fmt::Display) -> Error {
        Error::PluginFailure {
            plugin: self.owner.into(),
            message: error.to_string(),
        }
    }
}

#[async_trait]
impl Service for ProviderService {
    async fn call(&self, request: Value) -> Result<Value> {
        match serde_json::from_value(request).map_err(|error| self.error(error))? {
            Request::Info => Ok(json!(self.provider.info())),
            Request::Create { params } => Ok(json!(
                self.provider
                    .create_session(params)
                    .await
                    .map_err(|error| self.error(error))?
            )),
            Request::Resume { session_id, params } => Ok(json!(
                self.provider
                    .resume_session(&session_id, params)
                    .await
                    .map_err(|error| self.error(error))?
            )),
            Request::Submit { session_id, text } => Ok(json!(
                self.provider
                    .submit(&session_id, &text)
                    .await
                    .map_err(|error| self.error(error))?
            )),
            Request::Cancel { params } => {
                self.provider
                    .cancel(&params)
                    .await
                    .map_err(|error| self.error(error))?;
                Ok(Value::Null)
            }
        }
    }
}
