//! Error types for the hashpinner crates.
//!
//! All public fallible APIs return [`Result<T>`], backed by the [`enum@Error`] enum
//! which uses [`thiserror`] for ergonomic `Display` and `Error` implementations.
//!
//! Note that a *failed check* is not an [`enum@Error`]. Checks produce outcomes that
//! accumulate in a report, because the tool is expected to keep going and fix what
//! it can; errors here are reserved for the tool itself being unable to proceed.

use thiserror::Error;

/// A convenient result type alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level error type for the project.
#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML error: {0}")]
    Yaml(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("git error: {0}")]
    Git(String),

    #[error("{0}")]
    Other(String),
}
