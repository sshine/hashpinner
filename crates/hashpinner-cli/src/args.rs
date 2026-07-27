//! CLI argument definitions using [`clap`].
//!
//! The [`Args`] struct derives [`Parser`] and describes every flag and option
//! accepted by the `hashpinner` binary. See the crate-level documentation for the
//! full help output.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

/// Check, pin and bump SHA-pinned GitHub/Forgejo Actions references.
///
/// Scans workflow and action files for `uses:` references and either lists them,
/// checks them, pins the unpinned ones, or bumps the pinned ones onto their latest
/// release. With no mode given it lists.
#[derive(Debug, Parser)]
#[command(name = "hashpinner", version, about)]
pub struct Args {
    /// List every reference and what it points at. The default.
    #[arg(short = 'l', long = "list", conflicts_with_all = ["check", "pin", "bump"])]
    pub list: bool,

    /// Exit 1 if anything fails validation, changing nothing.
    #[arg(short = 'c', long = "check", conflicts_with = "pin")]
    pub check: bool,

    /// Pin unpinned references to a commit, repairing their comments.
    #[arg(short = 'p', long = "pin")]
    pub pin: bool,

    /// Move pinned references onto the latest release.
    #[arg(short = 'b', long = "bump")]
    pub bump: bool,

    /// Also verify that pins exist, belong to the repository, and are described truthfully.
    #[arg(short = 'd', long = "deep")]
    pub deep: bool,

    /// Exempt matching actions from failing --check when unpinned (repeatable).
    ///
    /// Matched against `owner/repo`, where `*` stands for any run of characters
    /// within one path segment. Defaults to `actions/*`.
    #[arg(short = 'a', long = "allow", value_name = "GLOB")]
    pub allow: Vec<String>,

    /// Clear the allowlist, so every unpinned reference fails --check.
    #[arg(long = "no-allow", conflicts_with = "allow")]
    pub no_allow: bool,

    /// Where a bare owner/repo points in .forgejo/ and .gitea/ files.
    #[arg(long = "forgejo-host", value_name = "URL", default_value = DEFAULT_FORGEJO_HOST)]
    pub forgejo_host: String,

    /// Report what would change without writing anything.
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,

    /// Never fetch; answer from the cache and report anything unknown as unverified.
    #[arg(long = "offline")]
    pub offline: bool,

    /// Output format.
    #[arg(long = "format", value_enum, default_value_t = Format::Text)]
    pub format: Format,

    /// Only report failures.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// Files or directories to scan. Defaults to this repository's workflow directories.
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
}

/// Forgejo's own default for `DEFAULT_ACTIONS_URL`, which is deliberately not GitHub.
pub const DEFAULT_FORGEJO_HOST: &str = "https://data.forgejo.org";

/// How to render the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Human-readable, one line per reference.
    Text,
    /// One JSON object, for scripting.
    Json,
}

impl Args {
    /// Whether the run should write to disk.
    pub fn writes(&self) -> bool {
        (self.pin || self.bump) && !self.dry_run && !self.check
    }

    /// The allowlist patterns to use, honouring `--no-allow` and the default.
    pub fn allow_patterns(&self) -> Vec<String> {
        if self.no_allow {
            Vec::new()
        } else if self.allow.is_empty() {
            vec!["actions/*".to_string()]
        } else {
            self.allow.clone()
        }
    }
}
