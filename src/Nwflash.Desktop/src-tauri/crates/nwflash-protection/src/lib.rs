//! Synchronous, dependency-free-of-Tauri protection primitives.
//!
//! The crate deliberately accepts only normalized data at decision boundaries.
//! In particular, no dispatcher accepts a password or bearer token.

mod decision;
mod lease;

pub use decision::*;
pub use lease::*;
