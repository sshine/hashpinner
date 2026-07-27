//! Check, pin and bump SHA-pinned GitHub/Forgejo Actions references.
//!
//! An unpinned reference such as `uses: actions/checkout@v4` names a mutable tag,
//! so whoever controls the upstream repository controls what runs in your CI, with
//! your secrets. Pinning to a commit fixes that, and annotating the commit with the
//! tag it carries keeps the result reviewable:
//!
//! ```text
//! uses: actions/checkout@8e8c483db84b4bee98b60c0593521ed34d9990e8 # v6.0.1, 2025-12-02
//! ```
//!
//! This crate ships the `hashpinner` binary, which maintains that form. The library
//! side is the same machinery, for anything that wants it directly:
//!
//! - **[`scan`]** — finds every `uses:` value in a file, with the byte span an
//!   edit must replace.
//! - **[`uses`]** — parses the value of a `uses:` key into a [`uses::UsesRef`].
//! - **[`local`]** — resolves a `uses: ./path` to the file it names, so that the
//!   contents of a local action are scanned rather than trusted.
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
pub mod local;
pub mod pattern;
pub mod resolver;
pub mod rewrite;
pub mod scan;
pub mod uses;
pub mod version;

pub use error::{Error, Result};
