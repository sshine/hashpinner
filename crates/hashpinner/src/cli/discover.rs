//! Finding the files to scan, and deciding which forge each one belongs to.

use std::path::{Path, PathBuf};

use hashpinner::rewrite::Forge;
use hashpinner::{Error, Result};

/// The three workflow directories, in the order Forgejo searches them.
const WORKFLOW_DIRS: [&str; 3] = [
    ".forgejo/workflows",
    ".gitea/workflows",
    ".github/workflows",
];

/// A file to process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Path to the file.
    pub path: PathBuf,
    /// Which forge's resolution rules apply to its bare references.
    pub forge: Forge,
}

/// Expand the paths given on the command line, or fall back to the defaults.
///
/// Returns the targets along with any warnings worth showing the user.
pub fn discover(paths: &[PathBuf]) -> Result<(Vec<Target>, Vec<String>)> {
    let mut warnings = Vec::new();

    let roots: Vec<PathBuf> = if paths.is_empty() {
        let present: Vec<&str> = WORKFLOW_DIRS
            .iter()
            .copied()
            .filter(|d| Path::new(d).is_dir())
            .collect();

        // Forgejo stops at the first of these that exists, so a repository with more
        // than one has directories that are silently dead on that forge. Nothing
        // else reports this, and it is invisible until a workflow mysteriously
        // never runs.
        if present.len() > 1 {
            warnings.push(format!(
                "{} both exist; Forgejo reads only {} and ignores the rest",
                present.join(" and "),
                present[0]
            ));
        }

        let mut roots: Vec<PathBuf> = present.iter().map(PathBuf::from).collect();
        for action in ["action.yml", "action.yaml"] {
            if Path::new(action).is_file() {
                roots.push(PathBuf::from(action));
            }
        }
        if roots.is_empty() {
            return Err(Error::Other(
                "no workflow directories found; pass a path explicitly".to_string(),
            ));
        }
        roots
    } else {
        paths.to_vec()
    };

    let mut targets = Vec::new();
    for root in roots {
        if root.is_dir() {
            collect_dir(&root, &mut targets)?;
        } else if root.is_file() {
            targets.push(Target {
                forge: forge_of(&root),
                path: root,
            });
        } else {
            return Err(Error::Other(format!("no such file: {}", root.display())));
        }
    }

    targets.sort_by(|a, b| a.path.cmp(&b.path));
    targets.dedup();
    Ok((targets, warnings))
}

/// Collect YAML files directly inside a directory, recursing into subdirectories.
fn collect_dir(dir: &Path, out: &mut Vec<Target>) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|e| e.path())
        .collect();
    entries.sort();

    for path in entries {
        if path.is_dir() {
            collect_dir(&path, out)?;
        } else if is_yaml(&path) {
            out.push(Target {
                forge: forge_of(&path),
                path,
            });
        }
    }
    Ok(())
}

/// Whether a path looks like a YAML file.
fn is_yaml(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yml" | "yaml")
    )
}

/// Which forge's rules apply, based on the directory the file sits in.
///
/// This decides what a bare `owner/repo` means, and the two forges disagree: under
/// `.github/` it is github.com, under `.forgejo/` it is that instance's configured
/// default. Guessing wrong resolves to a different repository with different commits.
fn forge_of(path: &Path) -> Forge {
    let text = path.to_string_lossy();
    if text.contains(".forgejo/") || text.contains(".gitea/") {
        Forge::Forgejo
    } else {
        Forge::GitHub
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_follows_the_directory() {
        assert_eq!(
            forge_of(Path::new(".forgejo/workflows/ci.yml")),
            Forge::Forgejo
        );
        assert_eq!(
            forge_of(Path::new(".gitea/workflows/ci.yml")),
            Forge::Forgejo
        );
        assert_eq!(
            forge_of(Path::new(".github/workflows/ci.yml")),
            Forge::GitHub
        );
    }

    #[test]
    fn a_bare_action_file_defaults_to_github() {
        assert_eq!(forge_of(Path::new("action.yml")), Forge::GitHub);
    }

    #[test]
    fn recognises_both_yaml_extensions() {
        assert!(is_yaml(Path::new("a.yml")));
        assert!(is_yaml(Path::new("a.yaml")));
        assert!(!is_yaml(Path::new("a.txt")));
        assert!(!is_yaml(Path::new("README")));
    }

    #[test]
    fn an_explicit_missing_path_is_an_error() {
        let err = discover(&[PathBuf::from("definitely/not/here.yml")]);
        assert!(err.is_err());
    }
}
