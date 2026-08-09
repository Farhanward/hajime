//! A small model of this system, trained on this system.
//!
//! Not a language model. It cannot write an essay and was never meant to. Its
//! whole subject is Hajime: what runs, what depends on what, what fits in the
//! memory left, and what a given request would actually do.
//!
//! Three parts, and the split is the design:
//!
//! - [`world`] holds the structure, as a CAD package holds an assembly:
//!   entities, the constraints between them, and a solver that says what a
//!   change does before anyone makes it.
//! - [`classify`] maps a sentence to an intent. It is trained from scratch, on
//!   a corpus [`corpus`] generates out of `world`, so the model's language
//!   follows the system rather than a file someone has to remember to update.
//! - [`plan`] turns an intent and the names found in the text into something
//!   executable, and refuses when the world model says it would break.
//!
//! The division of labour between the learned part and the exact part is the
//! point. Phrasing is fuzzy, so phrasing is learned. Service names are not
//! fuzzy: they come from the table, they are matched exactly, and the model has
//! no way to name one that does not exist. A wrong guess about intent produces
//! a question; there is no path that produces a command against an invented
//! service.

pub mod classify;
pub mod corpus;
pub mod execute;
pub mod features;
pub mod plan;
pub mod repair;
pub mod slots;
pub mod world;

pub use world::World;
