//! The seam between the policy engine and the network.
//!
//! Everything that needs to know what a tag points at goes through [`Resolver`], so
//! the whole of [`crate::rewrite`] can be exercised offline against [`FakeResolver`].
//! This follows the same discipline as passing a clock into a function rather than
//! reading one inside it: the untestable part is a parameter, not a dependency.

use std::collections::HashMap;

use crate::Result;

/// A repository to ask about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Remote {
    /// Base URL of the forge, such as `https://github.com`.
    pub host: String,
    /// The owning user or organisation.
    pub owner: String,
    /// The repository name.
    pub repo: String,
}

impl Remote {
    /// The clone URL.
    pub fn url(&self) -> String {
        format!(
            "{}/{}/{}",
            self.host.trim_end_matches('/'),
            self.owner,
            self.repo
        )
    }

    /// `owner/repo`, the form allowlist patterns are matched against.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// One tag in a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagInfo {
    /// The tag name, without `refs/tags/`.
    pub name: String,
    /// The commit it ultimately points at, with annotated tags already peeled.
    pub commit: String,
    /// The tagger date for annotated tags, the committer date otherwise, `YYYY-MM-DD`.
    pub date: String,
}

/// Whether a commit is genuinely part of a repository.
///
/// The distinction matters because GitHub's fork network shares an object store: a
/// commit pushed to any public fork can be fetched from the upstream URL even though
/// it was never merged. Existence therefore proves nothing; reachability does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reachability {
    /// Reachable from a tag, the normal case for a released action.
    FromTag(String),
    /// Reachable from a branch but not from any tag. Legitimate but unreleased.
    FromBranch,
    /// Present in no ref's history. On GitHub this is the fork-injection signature.
    Unreachable,
    /// Could not be established, so no conclusion may be drawn either way.
    Unverifiable,
}

/// What a deep check learned about one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// Whether the commit belongs to the repository.
    pub reachability: Reachability,
    /// Committer date as `YYYY-MM-DD`, when the commit could be read.
    pub date: Option<String>,
}

/// Answers questions about a remote repository's tags and commits.
pub trait Resolver {
    /// Every tag in the repository.
    fn tags(&self, remote: &Remote) -> Result<Vec<TagInfo>>;

    /// The commit a named tag or branch currently points at.
    ///
    /// Separate from [`tags`] because `uses: owner/repo@main` is legal and common,
    /// and a branch name will never appear in a tag listing.
    ///
    /// [`tags`]: Resolver::tags
    fn resolve_ref(&self, remote: &Remote, name: &str) -> Result<Option<String>>;

    /// Establish whether a commit belongs to the repository, and when it was made.
    ///
    /// Only called under `--deep`; it costs a full commit graph where [`tags`] costs
    /// a shallow ref listing.
    ///
    /// [`tags`]: Resolver::tags
    fn describe(&self, remote: &Remote, sha: &str) -> Result<CommitInfo>;
}

/// A resolver with canned answers, for tests.
///
/// Public because the interesting behaviour of this crate is what it does with a
/// resolver's answers, and that is worth testing from outside as well as inside.
///
/// Everything is keyed by full clone URL rather than by `owner/repo`, because the
/// two forges disagree about what a bare slug means: a fake keyed by slug alone
/// would quietly answer Forgejo lookups with GitHub's data and hide exactly the bug
/// that distinction exists to prevent.
#[derive(Debug, Default, Clone)]
pub struct FakeResolver {
    tags: HashMap<String, Vec<TagInfo>>,
    commits: HashMap<String, CommitInfo>,
    branches: HashMap<String, String>,
}

/// Where [`FakeResolver`]'s slug-taking builders put things.
const DEFAULT_FAKE_HOST: &str = "https://github.com";

impl FakeResolver {
    /// A resolver that knows nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a tag on a github.com repository, given as `owner/repo`.
    pub fn with_tag(self, slug: &str, name: &str, commit: &str, date: &str) -> Self {
        self.with_tag_at(DEFAULT_FAKE_HOST, slug, name, commit, date)
    }

    /// Declare a tag on a repository at a specific host.
    pub fn with_tag_at(
        mut self,
        host: &str,
        slug: &str,
        name: &str,
        commit: &str,
        date: &str,
    ) -> Self {
        let url = format!("{host}/{slug}");
        self.tags.entry(url.clone()).or_default().push(TagInfo {
            name: name.to_string(),
            commit: commit.to_string(),
            date: date.to_string(),
        });
        // A tagged commit is reachable from that tag unless something says otherwise.
        self.commits
            .entry(format!("{url}@{commit}"))
            .or_insert(CommitInfo {
                reachability: Reachability::FromTag(name.to_string()),
                date: Some(date.to_string()),
            });
        self
    }

    /// Declare a branch head on a github.com repository.
    pub fn with_branch(mut self, slug: &str, name: &str, commit: &str) -> Self {
        self.branches.insert(
            format!("{DEFAULT_FAKE_HOST}/{slug}@{name}"),
            commit.to_string(),
        );
        self
    }

    /// Declare what a deep check should conclude about a commit.
    pub fn with_commit(
        mut self,
        slug: &str,
        commit: &str,
        reachability: Reachability,
        date: Option<&str>,
    ) -> Self {
        self.commits.insert(
            format!("{DEFAULT_FAKE_HOST}/{slug}@{commit}"),
            CommitInfo {
                reachability,
                date: date.map(str::to_string),
            },
        );
        self
    }
}

impl Resolver for FakeResolver {
    fn tags(&self, remote: &Remote) -> Result<Vec<TagInfo>> {
        Ok(self.tags.get(&remote.url()).cloned().unwrap_or_default())
    }

    fn resolve_ref(&self, remote: &Remote, name: &str) -> Result<Option<String>> {
        let url = remote.url();
        let tagged = self
            .tags
            .get(&url)
            .and_then(|ts| ts.iter().find(|t| t.name == name))
            .map(|t| t.commit.clone());
        Ok(tagged.or_else(|| self.branches.get(&format!("{url}@{name}")).cloned()))
    }

    fn describe(&self, remote: &Remote, sha: &str) -> Result<CommitInfo> {
        Ok(self
            .commits
            .get(&format!("{}@{}", remote.url(), sha))
            .cloned()
            .unwrap_or(CommitInfo {
                reachability: Reachability::Unreachable,
                date: None,
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(slug: &str) -> Remote {
        let (owner, repo) = slug.split_once('/').expect("owner/repo");
        Remote {
            host: "https://github.com".to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
        }
    }

    #[test]
    fn url_joins_host_and_slug() {
        assert_eq!(
            remote("actions/checkout").url(),
            "https://github.com/actions/checkout"
        );
    }

    #[test]
    fn url_tolerates_a_trailing_slash_on_the_host() {
        let mut r = remote("a/b");
        r.host = "https://example.com/".to_string();
        assert_eq!(r.url(), "https://example.com/a/b");
    }

    #[test]
    fn fake_returns_declared_tags() {
        let f = FakeResolver::new().with_tag("actions/checkout", "v4", "aaa", "2024-01-01");
        let tags = f.tags(&remote("actions/checkout")).expect("tags");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "v4");
    }

    #[test]
    fn fake_is_empty_for_unknown_repositories() {
        let f = FakeResolver::new();
        assert!(f.tags(&remote("nobody/nothing")).expect("tags").is_empty());
    }

    #[test]
    fn tagging_a_commit_makes_it_reachable() {
        let f = FakeResolver::new().with_tag("a/b", "v1", "abc", "2024-01-01");
        let info = f.describe(&remote("a/b"), "abc").expect("describe");
        assert_eq!(info.reachability, Reachability::FromTag("v1".to_string()));
        assert_eq!(info.date.as_deref(), Some("2024-01-01"));
    }

    #[test]
    fn unknown_commits_are_unreachable() {
        let f = FakeResolver::new().with_tag("a/b", "v1", "abc", "2024-01-01");
        let info = f.describe(&remote("a/b"), "deadbeef").expect("describe");
        assert_eq!(info.reachability, Reachability::Unreachable);
    }
}
