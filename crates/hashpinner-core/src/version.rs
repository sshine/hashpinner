//! Recognising and ordering version tags.
//!
//! Deliberately not a semver implementation. Action repositories tag both exact
//! releases (`v6.0.1`) and floating majors (`v7`, `v34`), and `semver` rejects the
//! latter outright, so ordering them together needs a scheme that treats the number
//! of components as data rather than as an error.
//!
//! Two different orderings are needed and they are not the same:
//!
//! - **precedence** ([`Ord`]) answers "which release is newer", used to find the
//!   latest tag in a repository.
//! - **specificity** ([`Version::specificity`]) answers "which name describes this
//!   commit best", used to choose among several tags pointing at one commit.
//!
//! A commit tagged `v3`, `v3.2` and `v3.2.1` has one precedence-maximal tag by the
//! first ordering and three equally recent ones; specificity is what picks `v3.2.1`.

use std::cmp::Ordering;

/// A parsed version tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// Numeric components, outermost first: `v1.2.3` is `[1, 2, 3]`.
    components: Vec<u64>,
    /// Prerelease suffix without its leading `-`, if any.
    prerelease: Option<String>,
    /// The tag exactly as it appeared, including any `v` prefix and build metadata.
    raw: String,
}

impl Version {
    /// Parse a tag name, returning `None` if it does not look like a version.
    ///
    /// Accepts an optional `v` prefix, one or more dot-separated numeric components,
    /// and an optional `-prerelease` and/or `+build` suffix. Build metadata is kept
    /// in [`Version::raw`] but ignored for both orderings, as semver specifies.
    pub fn parse(tag: &str) -> Option<Self> {
        let body = tag.strip_prefix('v').unwrap_or(tag);

        // Split the suffix off first: a `-` or `+` ends the numeric part, and
        // whichever comes first wins so that `1.0+b-notpre` is build metadata.
        let (numeric, suffix) = match body.find(['-', '+']) {
            Some(i) => (&body[..i], Some(&body[i..])),
            None => (body, None),
        };

        if numeric.is_empty() {
            return None;
        }

        let mut components = Vec::new();
        for part in numeric.split('.') {
            components.push(part.parse::<u64>().ok()?);
        }

        let prerelease = suffix
            .and_then(|s| s.strip_prefix('-'))
            .map(|s| s.split('+').next().unwrap_or(s).to_string());

        Some(Self {
            components,
            prerelease,
            raw: tag.to_string(),
        })
    }

    /// The tag exactly as it appeared.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// How precisely this tag names a commit: the number of numeric components.
    ///
    /// `v3` is 1, `v3.2.1` is 3. Higher is more specific.
    pub fn specificity(&self) -> usize {
        self.components.len()
    }

    /// The leading numeric component, used to detect major-version crossings.
    pub fn major(&self) -> u64 {
        self.components.first().copied().unwrap_or(0)
    }

    /// Whether this is a prerelease such as `v1.2.3-rc1`.
    pub fn is_prerelease(&self) -> bool {
        self.prerelease.is_some()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        // Missing components read as zero, so v3 and v3.0 compare equal and both
        // sort below v3.0.1. Length is deliberately not a tiebreak here; that is
        // what specificity is for.
        let width = self.components.len().max(other.components.len());
        for i in 0..width {
            let a = self.components.get(i).copied().unwrap_or(0);
            let b = other.components.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => continue,
                other => return other,
            }
        }

        match (&self.prerelease, &other.prerelease) {
            (None, None) => Ordering::Equal,
            // A release outranks its own prereleases.
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Pick the tag that best describes a commit: most specific, then highest.
///
/// Non-version tags are ignored entirely. Returns `None` when no candidate parses
/// as a version, which is the `# no tag` case.
pub fn most_specific<'a, I>(tags: I) -> Option<Version>
where
    I: IntoIterator<Item = &'a str>,
{
    tags.into_iter()
        .filter_map(Version::parse)
        .max_by(|a, b| a.specificity().cmp(&b.specificity()).then_with(|| a.cmp(b)))
}

/// Pick the newest release among candidates, ignoring prereleases.
///
/// Prereleases are excluded because bumping onto an `-rc1` is never what a routine
/// `--bump` should do. When every candidate is a prerelease the result is `None`.
pub fn latest_release<'a, I>(tags: I) -> Option<Version>
where
    I: IntoIterator<Item = &'a str>,
{
    tags.into_iter()
        .filter_map(Version::parse)
        .filter(|v| !v.is_prerelease())
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).expect("valid version")
    }

    #[test]
    fn parses_plain_three_part() {
        let x = v("v1.2.3");
        assert_eq!(x.components, vec![1, 2, 3]);
        assert_eq!(x.raw(), "v1.2.3");
        assert!(!x.is_prerelease());
    }

    #[test]
    fn parses_without_v_prefix() {
        assert_eq!(v("1.2.3").components, vec![1, 2, 3]);
    }

    #[test]
    fn parses_floating_major() {
        let x = v("v34");
        assert_eq!(x.components, vec![34]);
        assert_eq!(x.specificity(), 1);
    }

    #[test]
    fn parses_prerelease() {
        let x = v("v2.0.0-rc1");
        assert_eq!(x.components, vec![2, 0, 0]);
        assert_eq!(x.prerelease.as_deref(), Some("rc1"));
    }

    #[test]
    fn parses_build_metadata_as_not_prerelease() {
        let x = v("v1.0.0+20260727");
        assert!(!x.is_prerelease());
        assert_eq!(x.raw(), "v1.0.0+20260727");
    }

    #[test]
    fn rejects_non_versions() {
        for s in ["main", "latest", "", "v", "release-1", "v1.x", "1.2.3.beta"] {
            assert!(Version::parse(s).is_none(), "should reject {s:?}");
        }
    }

    #[test]
    fn orders_by_numeric_precedence() {
        assert!(v("v3.2.1") > v("v1.2.3"));
        assert!(v("v2.0.0") > v("v1.99.99"));
        assert!(v("v1.10.0") > v("v1.9.0"));
    }

    #[test]
    fn missing_components_read_as_zero() {
        assert_eq!(v("v3").cmp(&v("v3.0.0")), Ordering::Equal);
        assert!(v("v3.0.1") > v("v3"));
    }

    #[test]
    fn release_outranks_its_prerelease() {
        assert!(v("v2.0.0") > v("v2.0.0-rc1"));
        assert!(v("v2.0.0-rc2") > v("v2.0.0-rc1"));
    }

    #[test]
    fn most_specific_prefers_longest() {
        let picked = most_specific(["v3", "v3.2", "v3.2.1"]).expect("a version");
        assert_eq!(picked.raw(), "v3.2.1");
    }

    #[test]
    fn most_specific_breaks_ties_by_precedence() {
        let picked = most_specific(["v3.2.1", "v3.2.4"]).expect("a version");
        assert_eq!(picked.raw(), "v3.2.4");
    }

    #[test]
    fn most_specific_ignores_non_versions() {
        let picked = most_specific(["latest", "stable", "v7"]).expect("a version");
        assert_eq!(picked.raw(), "v7");
    }

    #[test]
    fn most_specific_of_nothing_is_none() {
        assert!(most_specific(["main", "latest"]).is_none());
    }

    #[test]
    fn latest_release_skips_prereleases() {
        let picked = latest_release(["v1.0.0", "v2.0.0-rc1"]).expect("a version");
        assert_eq!(picked.raw(), "v1.0.0");
    }

    #[test]
    fn latest_release_prefers_specific_over_floating() {
        // v7 and v7.0.0 tie on precedence; max() keeps the later-seen equal element,
        // so this asserts the pair is genuinely equal rather than arbitrary.
        assert_eq!(v("v7").cmp(&v("v7.0.0")), Ordering::Equal);
        let picked = latest_release(["v6.0.1", "v7"]).expect("a version");
        assert_eq!(picked.raw(), "v7");
    }
}
