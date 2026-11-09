//! Platform-independent desktop command host. Native IPC is a small adapter around this boundary.
mod agents;
mod host;
mod provider;
mod workbench;
pub use host::*;
pub use provider::*;
pub use workbench::WorkbenchRequest;

mod contributed;
