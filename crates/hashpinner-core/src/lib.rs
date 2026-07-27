//! Core library for checking, pinning and bumping Actions references.
//!
//! Provides the building blocks used by the `hashpinner` CLI:
//!
//! - **[`scan`]** — finds every `uses:` value in a file, with the byte span an
//!   edit must replace.
//! - **[`uses`]** — parses the value of a `uses:` key into a [`uses::UsesRef`].
//! - **[`version`]** — recognises and orders version tags, which action
//!   repositories write at inconsistent precision (`v6.0.1` alongside `v7`).
//! - **[`resolver`]** — the [`resolver::Resolver`] trait, the single seam through
//!   which anything reaches the network, plus a fake for tests.
//! - **[`git`]** — the [`git::GitResolver`], which implements that trait by driving
//!   the `git` binary against a per-remote cache.
//! - **[`rewrite`]** — the policy: what each reference deserves, and the byte-span
//!   edits that follow.
//! - **[`pattern`]** — allowlist matching against `owner/repo`.
//! - **[`error`]** — the unified [`Error`] enum and [`Result`] alias.

pub mod error;
pub mod git;
pub mod pattern;
pub mod resolver;
pub mod rewrite;
pub mod scan;
pub mod uses;
pub mod version;

pub use error::{Error, Result};
