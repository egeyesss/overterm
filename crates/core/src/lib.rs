//! OverTerm core: PTY session management and agent-state detection.
//!
//! This crate must stay free of UI dependencies. Anything platform- or
//! window-specific belongs in the app crate behind its platform module.

pub mod choreo;
pub mod detect;
pub mod session;

pub use choreo::{ChoreoConfig, ChoreoEvent, Cues, WindowAction, WindowMode};
pub use detect::{AgentState, Detector, Signal, StateChange};
pub use session::{PtySession, SessionOutput, SpawnConfig};
