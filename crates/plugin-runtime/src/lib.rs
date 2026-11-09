//! Wasmtime component host. Guests receive only versioned WIT imports, never native Context/IPC.
mod broker;
mod packages;
mod runtime;
mod transport;
pub use broker::ResourceRequest;
pub use packages::*;
pub use runtime::*;
