//! Styrman core: the normalized event model, the adapter trait, the derived
//! session status, and the local IPC wire types shared by `styrd` and `styr`.

pub mod adapter;
pub mod command;
pub mod event;
pub mod ids;
pub mod ipc;
pub mod paths;
pub mod phone;
pub mod status;
pub mod tailscale;

pub use command::Command;
pub use event::{Event, EventKind, EventSource};
pub use ids::{AgentKind, NativeSessionRef, RequestId, SessionId};
pub use status::{SessionStatus, StatusTracker};

/// Crate version, reported by the daemon and CLI.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
