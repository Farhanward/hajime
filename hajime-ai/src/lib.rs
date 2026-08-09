//! Model gateway.
//!
//! Speaks the OpenAI chat-completions API, because that is what every client
//! already understands, and hosts the tools the model may reach. Inference runs
//! in a separate process; this crate never loads a model.
//!
//! The Ollama-shaped routes exist only so the services written against the old
//! server keep working during the migration.

pub mod api;
pub mod backend;

// The tool contract and its gateway are shared infrastructure, so they live in
// hajime-core. Re-exported here because a tool provider crate would otherwise
// have to depend on this one, which would make the graph circular.
pub use hajime_core::tools;
