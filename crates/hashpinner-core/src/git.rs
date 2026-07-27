//! The [`Resolver`] that actually talks to a forge, by driving `git`.
//!
//! One bare repository is cached per remote under `$XDG_CACHE_HOME/hashpinner`, and
//! every question is answered from it. Shelling out is deliberate: `git` supports
//! partial clone (`--filter=tree:0` transfers commits with no trees or blobs) which
//! no Rust git library does yet, and `for-each-ref` yields ref name, commit, peeled
//! commit and date in a single command and a single format string.
//!
//! Two levels of fetch exist because they cost very different amounts:
//!
//! - **shallow** — `--depth=1` over tags and heads, enough to map names to commits.
//! - **full graph** — no depth limit, needed only by `--deep`, because deciding
//!   whether a commit is *reachable* requires history, and mere existence proves
//!   nothing on a forge whose forks share an object store.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::resolver::{CommitInfo, Reachability, Remote, Resolver, TagInfo};
use crate::{Error, Result};

/// Resolves against real repositories, caching each one on disk.
#[derive(Debug, Clone)]
pub struct GitResolver {
    root: PathBuf,
    offline: bool,
}

impl GitResolver {
    /// Cache repositories under `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            offline: false,
        }
    }

    /// Cache under `$XDG_CACHE_HOME/hashpinner`, falling back to `$HOME/.cache`.
    pub fn with_default_cache() -> Result<Self> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .ok_or_else(|| Error::Other("neither XDG_CACHE_HOME nor HOME is set".to_string()))?;
        Ok(Self::new(base.join("hashpinner")))
    }

    /// Answer only from what is already cached, never touching the network.
    pub fn offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    /// Where a remote's bare repository lives.
    fn repo_path(&self, remote: &Remote) -> PathBuf {
        self.root
            .join(host_dir(&remote.host))
            .join(&remote.owner)
            .join(format!("{}.git", remote.repo))
    }

    /// Create the bare repository if this is the first time we have seen the remote.
    fn ensure_repo(&self, remote: &Remote) -> Result<PathBuf> {
        let path = self.repo_path(remote);
        if path.join("HEAD").exists() {
            return Ok(path);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        run(
            Path::new("."),
            &["init", "--bare", "--quiet", to_str(&path)?],
        )?;
        Ok(path)
    }

    /// Fetch refs, preferring a partial clone and falling back if the server says no.
    ///
    /// `uploadpack.allowfilter` is off by default on some self-hosted Forgejo
    /// instances, so a refused filter has to be an inconvenience rather than an error.
    fn fetch(&self, repo: &Path, url: &str, deep: bool) -> Result<()> {
        let mut args: Vec<&str> = vec!["fetch", "--quiet", "--prune", "--filter=tree:0"];
        if !deep {
            args.push("--depth=1");
        }
        args.extend([
            url,
            "+refs/tags/*:refs/tags/*",
            "+refs/heads/*:refs/heads/*",
        ]);

        if run(repo, &args).is_ok() {
            return Ok(());
        }

        let retry: Vec<&str> = args
            .into_iter()
            .filter(|a| *a != "--filter=tree:0")
            .collect();
        run(repo, &retry).map(|_| ())
    }

    /// Bring the cache up to date for the kind of question about to be asked.
    fn sync(&self, remote: &Remote, deep: bool) -> Result<PathBuf> {
        let repo = self.ensure_repo(remote)?;
        if self.offline {
            return Ok(repo);
        }
        // A deep fetch supersedes a shallow one, so the marker records only that the
        // history is complete; anything less is re-fetched every run.
        let marker = repo.join("hashpinner-full-graph");
        if deep && marker.exists() {
            return Ok(repo);
        }
        self.fetch(&repo, &remote.url(), deep)?;
        if deep {
            std::fs::write(&marker, b"")?;
        }
        Ok(repo)
    }
}

impl Resolver for GitResolver {
    fn tags(&self, remote: &Remote) -> Result<Vec<TagInfo>> {
        let repo = self.sync(remote, false)?;
        let out = run(
            &repo,
            &[
                "for-each-ref",
                "--format=%(refname:short)%09%(objectname)%09%(*objectname)%09%(creatordate:short)",
                "refs/tags/",
            ],
        )?;
        Ok(parse_for_each_ref(&out))
    }

    fn resolve_ref(&self, remote: &Remote, name: &str) -> Result<Option<String>> {
        let repo = self.sync(remote, false)?;
        // Ask about both namespaces in one call; a tag wins over a branch of the
        // same name, matching how a forge resolves `uses: owner/repo@x`.
        for pattern in [format!("refs/tags/{name}"), format!("refs/heads/{name}")] {
            let out = run(
                &repo,
                &[
                    "for-each-ref",
                    "--format=%(objectname)%09%(*objectname)",
                    &pattern,
                ],
            )?;
            if let Some(line) = out.lines().next() {
                let mut cols = line.split('\t');
                let object = cols.next().unwrap_or_default();
                let peeled = cols.next().unwrap_or_default();
                let sha = if peeled.is_empty() { object } else { peeled };
                if !sha.is_empty() {
                    return Ok(Some(sha.to_string()));
                }
            }
        }
        Ok(None)
    }

    fn describe(&self, remote: &Remote, sha: &str) -> Result<CommitInfo> {
        let repo = match self.sync(remote, true) {
            Ok(r) => r,
            Err(_) if self.offline => {
                return Ok(CommitInfo {
                    reachability: Reachability::Unverifiable,
                    date: None,
                });
            }
            Err(e) => return Err(e),
        };

        // Absent from the object store entirely: nothing more to establish.
        if run(&repo, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_err() {
            return Ok(CommitInfo {
                reachability: Reachability::Unreachable,
                date: None,
            });
        }

        let date = run(&repo, &["show", "-s", "--format=%cs", sha])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // `--contains` walks history, which is why this path needs the full graph.
        // Present-but-unreachable is the signature of a commit that only exists
        // because a fork pushed it into a shared object store.
        let containing = run(
            &repo,
            &[
                "for-each-ref",
                "--contains",
                sha,
                "--format=%(refname)",
                "refs/tags/",
                "refs/heads/",
            ],
        )?;

        let reachability = match containing
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("refs/tags/"))
        {
            Some(tag) => Reachability::FromTag(tag.trim_start_matches("refs/tags/").to_string()),
            None if containing.trim().is_empty() => Reachability::Unreachable,
            None => Reachability::FromBranch,
        };

        Ok(CommitInfo { reachability, date })
    }
}

/// Turn a URL into a filesystem-safe directory name.
fn host_dir(host: &str) -> String {
    host.trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .replace(['/', ':'], "_")
}

/// Parse `for-each-ref` output in this module's format.
///
/// `%(*objectname)` is the peeled commit and is empty for lightweight tags, so an
/// empty column means the tag already points straight at a commit.
fn parse_for_each_ref(out: &str) -> Vec<TagInfo> {
    out.lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let name = cols.next()?;
            let object = cols.next()?;
            let peeled = cols.next().unwrap_or_default();
            let date = cols.next().unwrap_or_default();
            if name.is_empty() || object.is_empty() {
                return None;
            }
            Some(TagInfo {
                name: name.to_string(),
                commit: if peeled.is_empty() { object } else { peeled }.to_string(),
                date: date.to_string(),
            })
        })
        .collect()
}

/// Run a git command in `cwd` and return its stdout.
fn run(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        // Never block waiting for a password: this runs in CI and in hooks. Helpers
        // configured by the user still work; only interactive prompting is off.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ADVICE", "0")
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                Error::Git("`git` was not found on PATH; hashpinner requires it".to_string())
            }
            _ => Error::Io(e),
        })?;

    if !output.status.success() {
        return Err(Error::Git(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Paths handed to git have to be valid UTF-8, which every cache path we build is.
fn to_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::Other(format!("path is not valid UTF-8: {}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_lightweight_tag() {
        let out = "v1.0.0\tabc123\t\t2024-01-01\n";
        let tags = parse_for_each_ref(out);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "v1.0.0");
        assert_eq!(tags[0].commit, "abc123");
        assert_eq!(tags[0].date, "2024-01-01");
    }

    #[test]
    fn peels_an_annotated_tag() {
        // The tag object is abc123; the commit it points at is def456.
        let out = "v2.0.0\tabc123\tdef456\t2025-02-02\n";
        let tags = parse_for_each_ref(out);
        assert_eq!(tags[0].commit, "def456");
    }

    #[test]
    fn parses_a_mixed_listing() {
        let out = "v1\taaa\t\t2024-01-01\nv2\tbbb\tccc\t2025-01-01\n";
        let tags = parse_for_each_ref(out);
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].commit, "aaa");
        assert_eq!(tags[1].commit, "ccc");
    }

    #[test]
    fn ignores_blank_and_partial_lines() {
        assert!(parse_for_each_ref("\n\n").is_empty());
        assert!(parse_for_each_ref("justaname\n").is_empty());
    }

    #[test]
    fn tag_names_may_contain_slashes() {
        let out = "release/v1.0\taaa\t\t2024-01-01\n";
        assert_eq!(parse_for_each_ref(out)[0].name, "release/v1.0");
    }

    #[test]
    fn host_becomes_a_safe_directory_name() {
        assert_eq!(host_dir("https://github.com"), "github.com");
        assert_eq!(host_dir("https://data.forgejo.org/"), "data.forgejo.org");
        assert_eq!(host_dir("http://git.shine.town"), "git.shine.town");
        assert_eq!(host_dir("https://localhost:3000"), "localhost_3000");
    }

    #[test]
    fn cache_paths_separate_forges_with_the_same_slug() {
        let r = GitResolver::new("/cache");
        let gh = Remote {
            host: "https://github.com".to_string(),
            owner: "actions".to_string(),
            repo: "checkout".to_string(),
        };
        let fj = Remote {
            host: "https://data.forgejo.org".to_string(),
            ..gh.clone()
        };
        assert_ne!(r.repo_path(&gh), r.repo_path(&fj));
        assert_eq!(
            r.repo_path(&gh),
            PathBuf::from("/cache/github.com/actions/checkout.git")
        );
    }
}
