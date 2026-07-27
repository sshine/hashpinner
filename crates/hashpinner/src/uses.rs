//! Parsing the value of a `uses:` key.
//!
//! Four forms exist across the two forges:
//!
//! ```text
//! owner/repo[/subpath]@ref                        both
//! https://host/owner/repo[/subpath]@ref           Forgejo only
//! docker://image[:tag|@sha256:digest]             both
//! ./path/to/dir                                   both
//! ```
//!
//! The absolute-URL form is a Forgejo extension; GitHub has no equivalent. It exists
//! because a bare `owner/repo` is resolved against an instance-level default that
//! differs per forge, so it is the only spelling that means one thing everywhere.
//!
//! This module is pure. Deciding which host a bare reference belongs to is the
//! caller's job, because the answer depends on where the file lives on disk.

use crate::{Error, Result};

/// A parsed `uses:` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsesRef {
    /// An action in a git repository, the only form hashpinner can pin.
    Remote(RemoteRef),
    /// A container image. Pinnable by digest, but not by anything git knows.
    Docker(DockerRef),
    /// An action inside this repository, covered by the same review as the rest of it.
    Local(LocalRef),
}

/// An action in a git repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRef {
    /// The host, when written absolutely. `None` means the forge's default applies.
    pub host: Option<String>,
    /// The owning user or organisation.
    pub owner: String,
    /// The repository name.
    pub repo: String,
    /// A path within the repository, for actions not at its root.
    pub subpath: Option<String>,
    /// Whatever followed the `@`.
    pub git_ref: GitRef,
}

/// The part after the `@`.
///
/// Tags and branches are textually indistinguishable, so no attempt is made to tell
/// them apart; only a full-length hex string is treated as a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRef {
    /// A 40-character hex commit id.
    Sha(String),
    /// Anything else: a tag, a branch, or an abbreviated commit id.
    Named(String),
}

/// A container image reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerRef {
    /// The image name, including any registry and port.
    pub image: String,
    /// How the image is identified.
    pub reference: ImageRef,
}

/// How a container image is identified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageRef {
    /// An immutable `sha256:` digest.
    Digest(String),
    /// A mutable tag.
    Tag(String),
    /// No tag at all, which means a mutable implicit `latest`.
    Untagged,
}

/// An action inside this repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRef {
    /// The path as written, relative to the repository root.
    pub path: String,
}

impl GitRef {
    /// Classify the text after the `@`.
    pub fn parse(text: &str) -> Self {
        if text.len() == 40 && text.chars().all(|c| c.is_ascii_hexdigit()) {
            Self::Sha(text.to_ascii_lowercase())
        } else {
            Self::Named(text.to_string())
        }
    }

    /// Whether this is a full-length commit id.
    pub fn is_pinned(&self) -> bool {
        matches!(self, Self::Sha(_))
    }

    /// Whether this looks like an abbreviated commit id rather than a tag.
    ///
    /// Worth reporting separately: Forgejo rejects short refs outright, so a
    /// workflow that works on GitHub can fail on a mirror for this reason alone.
    pub fn is_short_sha(&self) -> bool {
        match self {
            Self::Sha(_) => false,
            Self::Named(t) => {
                (7..40).contains(&t.len()) && t.chars().all(|c| c.is_ascii_hexdigit())
            }
        }
    }

    /// The text as written.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Sha(s) | Self::Named(s) => s,
        }
    }
}

impl ImageRef {
    /// Whether the image can change under the same reference.
    pub fn is_mutable(&self) -> bool {
        !matches!(self, Self::Digest(_))
    }
}

impl RemoteRef {
    /// `owner/repo`, the form allowlist patterns are matched against.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

impl UsesRef {
    /// Parse a `uses:` value.
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();

        if value.is_empty() {
            return Err(Error::Parse("empty uses: value".to_string()));
        }

        if value.starts_with("./") || value.starts_with("../") {
            return Ok(Self::Local(LocalRef {
                path: value.to_string(),
            }));
        }

        if let Some(rest) = value.strip_prefix("docker://") {
            return parse_docker(rest).map(Self::Docker);
        }

        for scheme in ["https://", "http://"] {
            if let Some(rest) = value.strip_prefix(scheme) {
                let (host, path) = rest.split_once('/').ok_or_else(|| {
                    Error::Parse(format!("{value:?} has a host but no owner/repo"))
                })?;
                let mut r = parse_path_spec(path, value)?;
                r.host = Some(format!("{scheme}{host}"));
                return Ok(Self::Remote(r));
            }
        }

        parse_path_spec(value, value).map(Self::Remote)
    }
}

/// Parse `owner/repo[/subpath]@ref`, with `whole` used only for error messages.
fn parse_path_spec(spec: &str, whole: &str) -> Result<RemoteRef> {
    // The first `@` is the separator: neither owner, repo nor subpath may contain
    // one, while a ref may (an email-shaped branch name is legal, if unusual).
    let (path, git_ref) = spec
        .split_once('@')
        .ok_or_else(|| Error::Parse(format!("{whole:?} has no @ref")))?;

    if git_ref.is_empty() {
        return Err(Error::Parse(format!("{whole:?} has an empty @ref")));
    }

    let mut segments = path.split('/');
    let owner = segments.next().filter(|s| !s.is_empty());
    let repo = segments.next().filter(|s| !s.is_empty());

    let (Some(owner), Some(repo)) = (owner, repo) else {
        return Err(Error::Parse(format!(
            "{whole:?} is not in owner/repo[/subpath]@ref form"
        )));
    };

    let subpath = {
        let rest = segments.collect::<Vec<_>>().join("/");
        if rest.is_empty() { None } else { Some(rest) }
    };

    Ok(RemoteRef {
        host: None,
        owner: owner.to_string(),
        repo: repo.to_string(),
        subpath,
        git_ref: GitRef::parse(git_ref),
    })
}

/// Parse the part of a `docker://` value after the scheme.
fn parse_docker(rest: &str) -> Result<DockerRef> {
    if rest.is_empty() {
        return Err(Error::Parse("docker:// with no image".to_string()));
    }

    if let Some((image, digest)) = rest.split_once('@') {
        return Ok(DockerRef {
            image: image.to_string(),
            reference: ImageRef::Digest(digest.to_string()),
        });
    }

    // A colon only introduces a tag when it comes after the last slash; before it,
    // it is a registry port, as in `registry.local:5000/alpine`.
    let last_slash = rest.rfind('/').map_or(0, |i| i + 1);
    match rest[last_slash..].find(':') {
        Some(offset) => {
            let colon = last_slash + offset;
            Ok(DockerRef {
                image: rest[..colon].to_string(),
                reference: ImageRef::Tag(rest[colon + 1..].to_string()),
            })
        }
        None => Ok(DockerRef {
            image: rest.to_string(),
            reference: ImageRef::Untagged,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(s: &str) -> RemoteRef {
        match UsesRef::parse(s).expect("parses") {
            UsesRef::Remote(r) => r,
            other => panic!("expected a remote ref, got {other:?}"),
        }
    }

    fn docker(s: &str) -> DockerRef {
        match UsesRef::parse(s).expect("parses") {
            UsesRef::Docker(d) => d,
            other => panic!("expected a docker ref, got {other:?}"),
        }
    }

    #[test]
    fn bare_owner_repo() {
        let r = remote("actions/checkout@v4");
        assert_eq!(r.host, None);
        assert_eq!(r.owner, "actions");
        assert_eq!(r.repo, "checkout");
        assert_eq!(r.subpath, None);
        assert_eq!(r.git_ref, GitRef::Named("v4".to_string()));
    }

    #[test]
    fn full_sha_is_pinned() {
        let r = remote("actions/checkout@8e8c483db84b4bee98b60c0593521ed34d9990e8");
        assert!(r.git_ref.is_pinned());
        assert_eq!(
            r.git_ref.as_str(),
            "8e8c483db84b4bee98b60c0593521ed34d9990e8"
        );
    }

    #[test]
    fn uppercase_sha_is_normalised() {
        let r = remote("a/b@8E8C483DB84B4BEE98B60C0593521ED34D9990E8");
        assert_eq!(
            r.git_ref.as_str(),
            "8e8c483db84b4bee98b60c0593521ed34d9990e8"
        );
    }

    #[test]
    fn short_sha_is_named_but_flagged() {
        let r = remote("a/b@8e8c483");
        assert!(!r.git_ref.is_pinned());
        assert!(r.git_ref.is_short_sha());
    }

    #[test]
    fn tag_is_not_mistaken_for_a_short_sha() {
        assert!(!GitRef::parse("v4").is_short_sha());
        assert!(!GitRef::parse("main").is_short_sha());
        // Hex-shaped but too short to be an abbreviation git would accept.
        assert!(!GitRef::parse("abc").is_short_sha());
    }

    #[test]
    fn subpath_is_captured() {
        let r = remote("owner/repo/.github/workflows/ci.yml@v1");
        assert_eq!(r.repo, "repo");
        assert_eq!(r.subpath.as_deref(), Some(".github/workflows/ci.yml"));
    }

    #[test]
    fn absolute_url_captures_host() {
        let r = remote("https://code.forgejo.org/actions/checkout@v4");
        assert_eq!(r.host.as_deref(), Some("https://code.forgejo.org"));
        assert_eq!(r.owner, "actions");
        assert_eq!(r.repo, "checkout");
    }

    #[test]
    fn absolute_url_with_subpath() {
        let r = remote("https://git.shine.town/nix-tools/hashpinner/sub@v1");
        assert_eq!(r.host.as_deref(), Some("https://git.shine.town"));
        assert_eq!(r.subpath.as_deref(), Some("sub"));
    }

    #[test]
    fn ref_may_contain_slashes() {
        let r = remote("a/b@feature/some-branch");
        assert_eq!(r.git_ref.as_str(), "feature/some-branch");
    }

    #[test]
    fn local_paths() {
        assert_eq!(
            UsesRef::parse("./.github/actions/build").expect("parses"),
            UsesRef::Local(LocalRef {
                path: "./.github/actions/build".to_string()
            })
        );
        assert!(matches!(
            UsesRef::parse("../shared/action").expect("parses"),
            UsesRef::Local(_)
        ));
    }

    #[test]
    fn docker_tag_is_mutable() {
        let d = docker("docker://alpine:3.8");
        assert_eq!(d.image, "alpine");
        assert_eq!(d.reference, ImageRef::Tag("3.8".to_string()));
        assert!(d.reference.is_mutable());
    }

    #[test]
    fn docker_digest_is_immutable() {
        let d = docker(
            "docker://alpine@sha256:9c6f0724472873bb50a2ae67a9e7adcb57673a183cea8b06eb778dca859181b5",
        );
        assert_eq!(d.image, "alpine");
        assert!(!d.reference.is_mutable());
    }

    #[test]
    fn docker_untagged_is_mutable() {
        let d = docker("docker://alpine");
        assert_eq!(d.reference, ImageRef::Untagged);
        assert!(d.reference.is_mutable());
    }

    #[test]
    fn docker_registry_port_is_not_a_tag() {
        let d = docker("docker://registry.local:5000/team/img");
        assert_eq!(d.image, "registry.local:5000/team/img");
        assert_eq!(d.reference, ImageRef::Untagged);
    }

    #[test]
    fn docker_registry_port_with_tag() {
        let d = docker("docker://registry.local:5000/team/img:1.2");
        assert_eq!(d.image, "registry.local:5000/team/img");
        assert_eq!(d.reference, ImageRef::Tag("1.2".to_string()));
    }

    #[test]
    fn slug_is_owner_slash_repo() {
        assert_eq!(remote("actions/checkout@v4").slug(), "actions/checkout");
    }

    #[test]
    fn rejects_malformed() {
        for s in [
            "",
            "actions/checkout",
            "actions@v4",
            "actions/checkout@",
            "@v4",
            "/repo@v4",
            "docker://",
        ] {
            assert!(UsesRef::parse(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(remote("  actions/checkout@v4  ").owner, "actions");
    }
}
