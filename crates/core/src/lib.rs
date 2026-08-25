//! OverTerm core: PTY session management and agent-state detection.
//!
//! This crate must stay free of UI dependencies. Anything platform- or
//! window-specific belongs in the app crate behind its platform module.

pub mod session;

pub use session::{PtySession, SessionOutput, SpawnConfig};
