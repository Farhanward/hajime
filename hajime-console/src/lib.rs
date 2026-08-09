//! The system console.
//!
//! One page that answers: what is broken, what ran, what the model did, and
//! what you can roll back to. It owns no data; it asks the services and
//! assembles their answers.
//!
//! Rendered on the server. A dashboard that needs JavaScript to tell you the
//! system is down is a dashboard that tells you nothing when it matters.

pub mod collect;
pub mod i18n;
pub mod render;

/// Who the system says it is.
///
/// Included from hajime-brand/out/brand.rs, which is printed from brand.toml.
/// The console renders these strings from compiled code rather than reading a
/// file at run time, so without this include it would need its own copy of the
/// repository URL and the author's name -- and a second copy is the one that
/// stays wrong after the first is corrected. The CI job regenerates and diffs,
/// so the copy cannot drift silently.
pub mod brand {
    include!("../../hajime-brand/out/brand.rs");
}
