//! # photosuite-automation
//!
//! The policy behind the desktop app's loopback control channel (`docs/control-protocol.md`):
//!
//! * [`security`]: the control token, request size and rate limits.
//! * [`budgets`]: response and rendered-PNG size budgets, and the reply writer.
//! * [`workspace`]: capability-scoped file access ([`AuthorizedWorkspace`]) and the checks that
//!   keep agent-supplied commands from reaching ambient filesystem paths.
//!
//! L6, no UI-toolkit dependencies.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod budgets;
pub mod security;
pub mod workspace;

pub use workspace::AuthorizedWorkspace;

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error("{0}")]
    BadRequest(String),
    #[error("I/O: {0}")]
    Io(String),
    #[error(transparent)]
    Engine(#[from] photosuite_engine::EngineError),
    #[error(transparent)]
    Format(#[from] photosuite_format::FormatError),
    #[error("{0}")]
    Other(String),
}
