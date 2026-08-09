//! Hajime workflow engine.
//!
//! A native replacement for the subset of n8n this deployment actually uses.
//! Workflows are loaded from n8n's own export format, so the same file runs on
//! either engine and a migration stays reversible.

pub mod engine;
pub mod expression;
pub mod model;
pub mod nodes;
pub mod scheduler;
pub mod server;
pub mod store;

// Shared foundation lives in hajime-core. Re-exported so existing
// `crate::auth` / `crate::policy` / `crate::history` paths keep working.
pub use hajime_core::{auth, history, lock, policy, secrets};
