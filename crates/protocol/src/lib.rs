//! Versioned P0 host contracts. Not a native dynamic-library ABI.
use std::collections::BTreeSet;

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const API_VERSION: &str = "0.1.0";
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ServiceKey {
    pub name: String,
    pub major: u32,
}

impl ServiceKey {
    pub fn new(name: impl Into<String>, major: u32) -> Self {
        Self {
            name: name.into(),
            major,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "target", rename_all = "snake_case")]
pub enum Capability {
    ServiceProvide(ServiceKey),
    ServiceCall(ServiceKey),
    ServiceReplace(ServiceKey),
    EventSubscribe(String),
    EventPublish(String),
    Contribution(String),
    ConfigRead,
    ConfigWrite,
    FilesystemRead(String),
    FilesystemWrite(String),
    NetworkOrigin(String),
    ProcessCommand(String),
    AgentControl { provider: String, root: String },
}

pub type Permissions = BTreeSet<Capability>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub id: String,
    pub version: VersionReq,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub version: Version,
    pub api: VersionReq,
    pub dependencies: Vec<Dependency>,
    pub permissions: Permissions,
}

impl Manifest {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            version: Version::new(0, 1, 0),
            api: VersionReq::parse("=0.1.0").expect("constant version"),
            dependencies: Vec::new(),
            permissions: Permissions::new(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            return Err(Error::InvalidManifest("invalid plugin id".into()));
        }
        if !self
            .api
            .matches(&Version::parse(API_VERSION).expect("constant version"))
        {
            return Err(Error::ApiMismatch(self.id.clone()));
        }
        let mut ids = BTreeSet::new();
        for dep in &self.dependencies {
            if !ids.insert(&dep.id) {
                return Err(Error::InvalidManifest(format!(
                    "duplicate dependency {}",
                    dep.id
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    #[error("plugin resource limit reached: {0}")]
    ResourceLimit(String),
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    #[error("plugin API incompatible: {0}")]
    ApiMismatch(String),
    #[error("plugin already registered: {0}")]
    DuplicatePlugin(String),
    #[error("plugin not found: {0}")]
    PluginNotFound(String),
    #[error("missing dependency {dependency} required by {plugin}")]
    MissingDependency { plugin: String, dependency: String },
    #[error("incompatible dependency {dependency} required by {plugin}")]
    DependencyVersion { plugin: String, dependency: String },
    #[error("dependency cycle involving {0:?}")]
    DependencyCycle(Vec<String>),
    #[error("plugin {plugin} still required by {dependents:?}")]
    DependentsActive {
        plugin: String,
        dependents: Vec<String>,
    },
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("scope closed: {0}")]
    ScopeClosed(String),
    #[error("permission denied to {plugin}: {capability:?}")]
    PermissionDenied {
        plugin: String,
        capability: Capability,
    },
    #[error("service conflict: {0:?}")]
    ServiceConflict(ServiceKey),
    #[error("service unavailable: {0:?}")]
    ServiceUnavailable(ServiceKey),
    #[error("service cannot be replaced: {0:?}")]
    NotReplaceable(ServiceKey),
    #[error("plugin callback panicked: {0}")]
    PluginPanic(String),
    #[error("operation timed out: {0}")]
    Timeout(String),
    #[error("plugin {plugin} failed: {message}")]
    PluginFailure { plugin: String, message: String },
}
