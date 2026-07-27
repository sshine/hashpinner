//! Allowlist patterns, matched against an action's `owner/repo`.
//!
//! Only `*` is special, and it never crosses a `/`. That is enough for every
//! pattern this tool is asked about — `actions/*`, `*/checkout`, an exact slug —
//! and keeps the allowlist free of the surprises a full glob dialect brings.

/// A compiled allowlist pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    segments: Vec<String>,
    source: String,
}

impl Pattern {
    /// Compile a pattern such as `actions/*`.
    pub fn new(source: &str) -> Self {
        Self {
            segments: source.split('/').map(str::to_string).collect(),
            source: source.to_string(),
        }
    }

    /// The pattern as written.
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Whether this pattern matches a slug such as `actions/checkout`.
    pub fn matches(&self, slug: &str) -> bool {
        let parts: Vec<&str> = slug.split('/').collect();
        if parts.len() != self.segments.len() {
            return false;
        }
        self.segments
            .iter()
            .zip(parts)
            .all(|(pat, text)| segment_matches(pat, text))
    }
}

/// Match one `/`-free segment, where `*` stands for any run of characters.
fn segment_matches(pattern: &str, text: &str) -> bool {
    let mut chunks = pattern.split('*');

    let Some(first) = chunks.next() else {
        return true;
    };
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };

    // Everything after the first `*` may float, except the final chunk which is
    // anchored to the end.
    let chunks: Vec<&str> = chunks.collect();
    let Some((last, middle)) = chunks.split_last() else {
        // No `*` at all: the prefix had to consume the whole text.
        return rest.is_empty();
    };

    for chunk in middle {
        match rest.find(chunk) {
            Some(i) => rest = &rest[i + chunk.len()..],
            None => return false,
        }
    }

    rest.len() >= last.len() && rest.ends_with(last)
}

/// Whether any pattern in the list matches.
pub fn any_matches(patterns: &[Pattern], slug: &str) -> bool {
    patterns.iter().any(|p| p.matches(slug))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, slug: &str) -> bool {
        Pattern::new(pattern).matches(slug)
    }

    #[test]
    fn exact_slug() {
        assert!(m("actions/checkout", "actions/checkout"));
        assert!(!m("actions/checkout", "actions/cache"));
    }

    #[test]
    fn star_matches_a_whole_segment() {
        assert!(m("actions/*", "actions/checkout"));
        assert!(m("*/checkout", "actions/checkout"));
        assert!(m("*/*", "anyone/anything"));
    }

    #[test]
    fn star_does_not_cross_a_slash() {
        assert!(!m("actions/*", "actions/sub/deep"));
        assert!(!m("*", "actions/checkout"));
    }

    #[test]
    fn star_within_a_segment() {
        assert!(m("actions/set*", "actions/setup-go"));
        assert!(m("actions/*-go", "actions/setup-go"));
        assert!(m("actions/set*go", "actions/setup-go"));
        assert!(!m("actions/set*go", "actions/setup-node"));
    }

    #[test]
    fn overlapping_chunks_do_not_double_count() {
        // The prefix and suffix must not be allowed to consume the same characters.
        assert!(!m("a*bc", "abc "));
        assert!(m("a*c", "abc"));
        assert!(!m("ab*ab", "ab"));
    }

    #[test]
    fn segment_count_must_agree() {
        assert!(!m("actions/checkout", "actions"));
        assert!(!m("actions", "actions/checkout"));
    }

    #[test]
    fn any_matches_is_a_disjunction() {
        let ps = [Pattern::new("actions/*"), Pattern::new("nix-tools/*")];
        assert!(any_matches(&ps, "nix-tools/hashpinner"));
        assert!(!any_matches(&ps, "softprops/action-gh-release"));
        assert!(!any_matches(&[], "actions/checkout"));
    }
}
