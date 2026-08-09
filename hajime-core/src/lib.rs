//! Shared foundation for every Hajime service.
//!
//! One library, so that authentication, capability policy, run history and
//! secret handling exist once rather than once per pillar. The pillars never
//! import each other; they import this and talk over HTTP.
//!
//! Nothing here knows what a workflow is, what a model is, or what WhatsApp
//! is. The moment something domain-specific appears in this crate, it belongs
//! in the pillar that owns it.

pub mod auth;
pub mod config;
pub mod history;
pub mod lock;
pub mod policy;
pub mod secrets;
pub mod tools;

pub use auth::Auth;
pub use policy::Policy;
pub use secrets::{Secret, SecretError};
pub use tools::{Effect, Gateway, ToolProvider, ToolSpec};
