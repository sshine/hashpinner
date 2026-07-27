//! Following a `uses: ./path` reference to the file it names.
//!
//! Local references are the reason `--check` can wave `./path` through without
//! looking: the target lives in this repository and is covered by the same review.
//! That argument only holds if the target is actually scanned, so it has to be
//! found first, and it is not always somewhere a directory walk would look.
//!
//! Two forge facts decide the rules here, and neither is guessable:
//!
//! 1. A relative path resolves against the repository root — **not** against the
//!    directory of the file containing it, even when that file is an `action.yml`
//!    referring to a sibling action. See [actions/runner#1348], still open.
//! 2. Where the target may live differs by forge. GitHub requires a reusable
//!    workflow to sit directly in `.github/workflows`; Forgejo accepts any path
//!    ending in `.yml` or `.yaml`. A composite action may live anywhere on both.
//!
//! [actions/runner#1348]: https://github.com/actions/runner/issues/1348

use std::path::{Path, PathBuf};

use crate::rewrite::Forge;
use crate::scan::Slot;

/// The filenames a local action directory may use, in the order GitHub prefers.
const ACTION_FILES: [&str; 2] = ["action.yml", "action.yaml"];

/// Why a local reference could not be followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unresolved {
    /// The path climbs out of the repository with `..`.
    Escapes,
    /// Nothing is there.
    Missing,
    /// A directory is there, but it holds no action manifest.
    NoManifest,
    /// The forge would not accept the target at this path.
    Rejected(String),
}

impl std::fmt::Display for Unresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Escapes => write!(f, "path leaves the repository"),
            Self::Missing => write!(f, "nothing at that path"),
            Self::NoManifest => write!(f, "directory holds no action.yml or action.yaml"),
            Self::Rejected(why) => write!(f, "{why}"),
        }
    }
}

/// Resolve `path` as written in a `uses:` value against the repository at `root`.
///
/// `slot` decides what is being looked for: a step names a directory holding an
/// action manifest, while `jobs.<id>.uses` names a workflow file directly.
pub fn resolve(
    root: &Path,
    path: &str,
    slot: Slot,
    forge: Forge,
) -> std::result::Result<PathBuf, Unresolved> {
    let rel = normalise(path).ok_or(Unresolved::Escapes)?;

    match slot {
        Slot::ReusableWorkflow => {
            reusable_workflow_is_allowed(&rel, forge)?;
            let full = root.join(&rel);
            if full.is_file() {
                Ok(rel)
            } else {
                Err(Unresolved::Missing)
            }
        }

        Slot::Step | Slot::CompositeStep => {
            let dir = root.join(&rel);
            if !dir.is_dir() {
                return Err(Unresolved::Missing);
            }
            ACTION_FILES
                .iter()
                .map(|name| rel.join(name))
                .find(|candidate| root.join(candidate).is_file())
                .ok_or(Unresolved::NoManifest)
        }
    }
}

/// Collapse `.` and `..` lexically, refusing anything that climbs past the root.
///
/// Lexical rather than [`std::fs::canonicalize`] on purpose: the question is which
/// path the forge will resolve, and the forge does not follow symlinks out of the
/// checkout to answer it.
fn normalise(path: &str) -> Option<PathBuf> {
    let mut out: Vec<&str> = Vec::new();

    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                // Nothing left to pop means the path has climbed above the checkout,
                // where a forge would find nothing and an attacker might.
                out.pop()?;
            }
            other => out.push(other),
        }
    }

    if out.is_empty() {
        return None;
    }
    Some(out.iter().collect())
}

/// Whether this forge will look for a reusable workflow at this path.
fn reusable_workflow_is_allowed(rel: &Path, forge: Forge) -> std::result::Result<(), Unresolved> {
    let is_yaml = matches!(
        rel.extension().and_then(|e| e.to_str()),
        Some("yml" | "yaml")
    );
    if !is_yaml {
        return Err(Unresolved::Rejected(
            "a reusable workflow must be a .yml or .yaml file".to_string(),
        ));
    }

    // GitHub reads reusable workflows only from the top level of `.github/workflows`;
    // Forgejo fetches any path in the repository. A file that works on one forge and
    // is invisible on the other is exactly the kind of divergence worth naming.
    if forge == Forge::GitHub && rel.parent() != Some(Path::new(".github/workflows")) {
        return Err(Unresolved::Rejected(
            "GitHub reads reusable workflows only from .github/workflows, not subdirectories"
                .to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_segments_collapse() {
        assert_eq!(normalise("./a/b"), Some(PathBuf::from("a/b")));
        assert_eq!(normalise("./a/../b"), Some(PathBuf::from("b")));
        assert_eq!(normalise("a/b/../../c"), Some(PathBuf::from("c")));
    }

    /// The path that matters: `..` inside the repository is fine, `..` past its root
    /// is not, and a substring check for ".." would confuse the two.
    #[test]
    fn climbing_past_the_root_is_refused() {
        assert_eq!(normalise("../shared"), None);
        assert_eq!(normalise("./a/../../shared"), None);
        assert_eq!(normalise("./a/../b/../.."), None);
    }

    #[test]
    fn an_empty_path_resolves_to_nothing() {
        assert_eq!(normalise("."), None);
        assert_eq!(normalise("./"), None);
    }

    #[test]
    fn github_confines_reusable_workflows_to_the_workflows_directory() {
        let ok = Path::new(".github/workflows/ci.yml");
        assert!(reusable_workflow_is_allowed(ok, Forge::GitHub).is_ok());

        let nested = Path::new(".github/workflows/sub/ci.yml");
        assert!(reusable_workflow_is_allowed(nested, Forge::GitHub).is_err());
        // The same file is legal on Forgejo, which fetches any path.
        assert!(reusable_workflow_is_allowed(nested, Forge::Forgejo).is_ok());
    }

    #[test]
    fn forgejo_accepts_a_workflow_anywhere() {
        let anywhere = Path::new("ci/shared.yml");
        assert!(reusable_workflow_is_allowed(anywhere, Forge::Forgejo).is_ok());
        assert!(reusable_workflow_is_allowed(anywhere, Forge::GitHub).is_err());
    }

    #[test]
    fn a_reusable_workflow_must_be_yaml() {
        let err = reusable_workflow_is_allowed(Path::new("ci/shared"), Forge::Forgejo);
        assert!(matches!(err, Err(Unresolved::Rejected(_))));
    }
}
