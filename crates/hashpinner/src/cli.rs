//! The parts of this crate that exist only to serve the binary.
//!
//! Kept apart from the library modules, which sit directly under `src/`, so that
//! what is public API and what is merely command-line plumbing stays legible.

pub mod args;
pub mod discover;
pub mod report;
