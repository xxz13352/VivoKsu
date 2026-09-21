//! Synchronous, dependency-free-of-Tauri protection primitives.
//!
//! The crate deliberately accepts only normalized data at decision boundaries.
//! In particular, no dispatcher accepts a password or bearer token.

mod decision;
mod lease;
mod local_artifact;
mod suspend_gate;
mod trace_redaction;
mod vmp;

pub use decision::*;
pub use lease::*;
pub use local_artifact::*;
pub use suspend_gate::*;
pub use trace_redaction::*;
pub use vmp::*;
