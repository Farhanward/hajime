//! System management for Hajime.
//!
//! The service table, ZFS snapshots and the boot self-check. Kept in one crate
//! because `hajimectl` and the console must agree on what a service is; two
//! lists would drift and the drift would only show up during an incident.

pub mod jail;
pub mod readiness;
pub mod service;
pub mod snapshot;
