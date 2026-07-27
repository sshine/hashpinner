//! Deciding what each `uses:` reference deserves, and rewriting the ones that need it.
//!
//! Everything here is driven by a [`Resolver`], so the whole policy is exercisable
//! offline. Nothing in this module opens a file or a socket.
//!
//! Two invariants shape the code:
//!
//! - **Nothing short-circuits.** A reference that cannot be resolved produces a
//!   failing entry and the loop moves on, because the tool's job is to fix as much
//!   as it can in one pass and report the rest.
//! - **An unchanged line is not rewritten.** Edits are emitted only when something
//!   is actually wrong, never as a side effect of re-rendering, so running `--pin`
//!   over a correct file returns it byte for byte.

use std::ops::Range;

use crate::pattern::{Pattern, any_matches};
use crate::resolver::{Reachability, Remote, Resolver, TagInfo};
use crate::scan::{Occurrence, Quoting, Scan, scan};
use crate::uses::{GitRef, UsesRef};
use crate::version::{Version, latest_release, most_specific};
use crate::{Error, Result};

/// Which forge a file belongs to, which decides what a bare `owner/repo` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forge {
    /// A file under `.github/`.
    GitHub,
    /// A file under `.forgejo/` or `.gitea/`.
    Forgejo,
}

/// What the caller asked for.
#[derive(Debug, Clone)]
pub struct Options {
    /// Fail on anything that does not meet the criteria, rather than fixing it.
    pub check: bool,
    /// Pin unpinned references, and repair comments while doing so.
    pub pin: bool,
    /// Move pinned references onto the newest release.
    pub bump: bool,
    /// Verify that pins exist, belong to the repository, and are described truthfully.
    pub deep: bool,
    /// Slugs exempt from failing `--check` when unpinned.
    pub allow: Vec<Pattern>,
    /// Where a bare `owner/repo` points on a Forgejo instance.
    pub forgejo_host: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            check: false,
            pin: false,
            bump: false,
            deep: false,
            allow: vec![Pattern::new("actions/*")],
            // Forgejo's own default for DEFAULT_ACTIONS_URL. Note this is not GitHub:
            // a bare `actions/checkout` means a different repository, with different
            // commit ids, depending on which directory the workflow lives in.
            forgejo_host: "https://data.forgejo.org".to_string(),
        }
    }
}

impl Options {
    /// Whether any mutation is wanted.
    ///
    /// `--check` turns every mode read-only, including when combined with `--bump`:
    /// a gate that edited the files it was gating would be a trap in CI.
    fn writes(&self) -> bool {
        (self.pin || self.bump) && !self.check
    }
}

/// How serious a note is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Informational, shown by `--list`.
    Info,
    /// Worth knowing, but not a failure.
    Warn,
    /// Sets the exit code to 1.
    Fail,
}

/// One thing worth saying about a reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// How serious it is.
    pub level: Level,
    /// Phrased for a user reading a terminal.
    pub message: String,
}

impl Note {
    fn info(message: impl Into<String>) -> Self {
        Self {
            level: Level::Info,
            message: message.into(),
        }
    }
    fn warn(message: impl Into<String>) -> Self {
        Self {
            level: Level::Warn,
            message: message.into(),
        }
    }
    fn fail(message: impl Into<String>) -> Self {
        Self {
            level: Level::Fail,
            message: message.into(),
        }
    }
}

/// What became of one `uses:` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 1-indexed line it was found on.
    pub line: usize,
    /// The reference as written.
    pub value: String,
    /// The part after the `@`, or a description for forms that have no ref.
    pub git_ref: String,
    /// The trailing comment as found, which --list shows verbatim.
    pub comment: Option<String>,
    /// Everything worth reporting.
    pub notes: Vec<Note>,
}

impl Entry {
    /// The most serious note attached.
    pub fn level(&self) -> Level {
        self.notes
            .iter()
            .map(|n| n.level)
            .max()
            .unwrap_or(Level::Info)
    }
}

/// A byte range to replace, and what to put there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Byte range in the original source.
    pub span: Range<usize>,
    /// Replacement text.
    pub replacement: String,
}

/// The result of processing one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// One per `uses:` reference, in document order.
    pub entries: Vec<Entry>,
    /// The rewritten file, present only when something actually changed.
    pub rewritten: Option<String>,
}

impl Outcome {
    /// Whether anything failed.
    pub fn failed(&self) -> bool {
        self.entries.iter().any(|e| e.level() == Level::Fail)
    }
}

/// The version and date a pin comment claims.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PinComment {
    /// The version named, or `None` for the literal `no tag`.
    version: Option<String>,
}

impl PinComment {
    /// Read `v6.0.1, 2025-12-02` or `no tag, 2025-12-02`.
    ///
    /// Comments that are not in this shape are treated as absent rather than wrong:
    /// people write all sorts of things next to a pin, and clobbering an unrelated
    /// note would be worse than leaving a stale one.
    fn parse(text: &str) -> Option<Self> {
        let head = text.split(',').next().unwrap_or(text).trim();
        if head == "no tag" {
            return Some(Self { version: None });
        }
        Version::parse(head).map(|v| Self {
            version: Some(v.raw().to_string()),
        })
    }
}

/// Process one file's contents.
///
/// `forge` decides what a bare `owner/repo` means and must reflect the directory the
/// file was found in, not a guess.
pub fn process(
    src: &str,
    forge: Forge,
    resolver: &dyn Resolver,
    opts: &Options,
) -> Result<Outcome> {
    let Scan {
        occurrences,
        unsupported,
    } = scan(src)?;

    let mut entries = Vec::new();
    let mut edits = Vec::new();

    for u in unsupported {
        entries.push(Entry {
            line: u.line,
            value: String::new(),
            git_ref: String::new(),
            comment: None,
            notes: vec![Note::warn(u.reason)],
        });
    }

    for occ in &occurrences {
        let (entry, edit) = process_one(occ, forge, resolver, opts);
        entries.push(entry);
        if let Some(edit) = edit {
            edits.push(edit);
        }
    }

    entries.sort_by_key(|e| e.line);

    let rewritten = if edits.is_empty() {
        None
    } else {
        Some(apply(src, &mut edits))
    };

    Ok(Outcome { entries, rewritten })
}

/// Decide the fate of one reference. Never returns an error: a failure here is a
/// note on the entry, so the caller's loop keeps going.
fn process_one(
    occ: &Occurrence,
    forge: Forge,
    resolver: &dyn Resolver,
    opts: &Options,
) -> (Entry, Option<Edit>) {
    let mut entry = Entry {
        line: occ.line,
        value: occ.value.clone(),
        git_ref: String::new(),
        comment: occ.comment.as_ref().map(|c| c.text.clone()),
        notes: Vec::new(),
    };

    let parsed = match UsesRef::parse(&occ.value) {
        Ok(p) => p,
        Err(e) => {
            entry.notes.push(Note::fail(e.to_string()));
            return (entry, None);
        }
    };

    match parsed {
        UsesRef::Local(l) => {
            entry.git_ref = "local".to_string();
            entry
                .notes
                .push(Note::info(format!("local action {}", l.path)));
            (entry, None)
        }

        UsesRef::Docker(d) => {
            entry.git_ref = match &d.reference {
                crate::uses::ImageRef::Digest(x) => x.clone(),
                crate::uses::ImageRef::Tag(t) => t.clone(),
                crate::uses::ImageRef::Untagged => "latest (implicit)".to_string(),
            };
            if d.reference.is_mutable() {
                entry.notes.push(Note::fail(
                    "mutable image reference; pin it as image@sha256:...".to_string(),
                ));
                if opts.writes() {
                    entry.notes.push(Note::warn(
                        "not fixed: resolving registry digests is out of scope".to_string(),
                    ));
                }
            }
            (entry, None)
        }

        UsesRef::Remote(r) => {
            entry.git_ref = r.git_ref.as_str().to_string();
            let remote = Remote {
                host: r.host.clone().unwrap_or_else(|| default_host(forge, opts)),
                owner: r.owner.clone(),
                repo: r.repo.clone(),
            };
            let edit = process_remote(occ, &r.git_ref, &remote, resolver, opts, &mut entry);
            (entry, edit)
        }
    }
}

/// Where a bare `owner/repo` points, given the directory its file lives in.
fn default_host(forge: Forge, opts: &Options) -> String {
    match forge {
        Forge::GitHub => "https://github.com".to_string(),
        Forge::Forgejo => opts.forgejo_host.clone(),
    }
}

/// The pin/bump/check decision for a git-hosted action.
fn process_remote(
    occ: &Occurrence,
    git_ref: &GitRef,
    remote: &Remote,
    resolver: &dyn Resolver,
    opts: &Options,
    entry: &mut Entry,
) -> Option<Edit> {
    let allowed = any_matches(&opts.allow, &remote.slug());

    if git_ref.is_short_sha() {
        entry.notes.push(Note::warn(
            "abbreviated commit id; Forgejo rejects these outright".to_string(),
        ));
    }

    // Nothing below needs the network when only listing, or when checking something
    // already known to be unpinned.
    let needs_tags = opts.pin || opts.bump || opts.deep;
    if !needs_tags {
        if !git_ref.is_pinned() {
            entry.notes.push(if opts.check && !allowed {
                Note::fail("not pinned to a commit".to_string())
            } else if opts.check {
                Note::info("not pinned, but allowlisted".to_string())
            } else {
                Note::info("not pinned".to_string())
            });
        }
        return None;
    }

    let tags = match resolver.tags(remote) {
        Ok(t) => t,
        Err(e) => {
            entry
                .notes
                .push(Note::fail(format!("{}: {e}", remote.slug())));
            return None;
        }
    };

    // Where should this reference end up?
    let target = resolve_target(git_ref, &tags, remote, resolver, opts, entry)?;

    if opts.deep {
        deep_check(&target.sha, remote, resolver, entry);
    }

    let desired_comment = comment_for(&target.sha, &tags);
    let moved = git_ref.as_str() != target.sha;

    if moved {
        let from = git_ref.as_str().to_string();
        if opts.check {
            entry.notes.push(Note::fail(format!(
                "stale: {} is not the latest ({})",
                short(&from),
                desired_comment.as_deref().unwrap_or("unknown")
            )));
            return None;
        }
        entry.notes.push(Note::info(format!(
            "{} -> {}",
            short(&from),
            desired_comment.as_deref().unwrap_or(&short(&target.sha))
        )));
        if let Some(warning) = target.major_warning {
            entry.notes.push(Note::warn(warning));
        }
        return Some(render_edit_with_sha(
            occ,
            &target.sha,
            desired_comment.as_deref(),
        ));
    }

    comment_repair(occ, desired_comment.as_deref(), opts, entry)
}

/// Where a reference should point after this run.
struct Target {
    sha: String,
    major_warning: Option<String>,
}

/// Work out the commit a reference should end up at, or record why it cannot.
fn resolve_target(
    git_ref: &GitRef,
    tags: &[TagInfo],
    remote: &Remote,
    resolver: &dyn Resolver,
    opts: &Options,
    entry: &mut Entry,
) -> Option<Target> {
    let current_sha = match git_ref {
        GitRef::Sha(sha) => Some(sha.clone()),
        GitRef::Named(name) => {
            if !opts.pin {
                entry.notes.push(if opts.check {
                    Note::fail("not pinned to a commit".to_string())
                } else {
                    Note::info("not pinned".to_string())
                });
                return None;
            }
            match resolver.resolve_ref(remote, name) {
                Ok(Some(sha)) => Some(sha),
                Ok(None) => {
                    entry.notes.push(Note::fail(format!(
                        "no tag or branch named {name:?} in {}",
                        remote.slug()
                    )));
                    return None;
                }
                Err(e) => {
                    entry
                        .notes
                        .push(Note::fail(format!("{}: {e}", remote.slug())));
                    return None;
                }
            }
        }
    }?;

    if !opts.bump {
        return Some(Target {
            sha: current_sha,
            major_warning: None,
        });
    }

    let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
    let Some(latest) = latest_release(names) else {
        entry
            .notes
            .push(Note::warn("no version tags to bump to".to_string()));
        return Some(Target {
            sha: current_sha,
            major_warning: None,
        });
    };

    let Some(target) = tags.iter().find(|t| t.name == latest.raw()) else {
        return Some(Target {
            sha: current_sha,
            major_warning: None,
        });
    };

    // A major crossing is legal and requested, but it changes an action's inputs
    // often enough that it should never pass by unremarked.
    let major_warning = tag_names_at(&current_sha, tags)
        .and_then(|names| most_specific(names.iter().map(String::as_str)))
        .filter(|current| current.major() != latest.major())
        .map(|current| {
            format!(
                "major version change {} -> {}; review the action's inputs",
                current.raw(),
                latest.raw()
            )
        });

    Some(Target {
        sha: target.commit.clone(),
        major_warning,
    })
}

/// Verify a pin exists, belongs to the repository, and is described truthfully.
fn deep_check(sha: &str, remote: &Remote, resolver: &dyn Resolver, entry: &mut Entry) {
    match resolver.describe(remote, sha) {
        Ok(info) => match info.reachability {
            Reachability::FromTag(_) => {}
            Reachability::FromBranch => entry.notes.push(Note::info(
                "commit is on a branch but carries no release tag".to_string(),
            )),
            Reachability::Unreachable => entry.notes.push(Note::fail(format!(
                "{} is not reachable from any ref in {}; on GitHub this is what a \
                 commit injected through a fork looks like",
                short(sha),
                remote.slug()
            ))),
            Reachability::Unverifiable => entry.notes.push(Note::warn(format!(
                "could not verify {} against {}",
                short(sha),
                remote.slug()
            ))),
        },
        Err(e) => entry
            .notes
            .push(Note::warn(format!("could not verify {}: {e}", short(sha)))),
    }
}

/// Compare the existing comment against the truth, and repair or complain.
///
/// Verifying a comment is a `--deep` concern, because a comment that misdescribes
/// its pin defeats human review in a way no syntactic check can see. Repairing one
/// is not: `--pin` regenerates the comment whenever it knows better, on the grounds
/// that the commit is authoritative and the comment is derived from it.
fn comment_repair(
    occ: &Occurrence,
    desired: Option<&str>,
    opts: &Options,
    entry: &mut Entry,
) -> Option<Edit> {
    let Some(desired) = desired else {
        // No tag points at this commit, so there is nothing to compare against and
        // guessing would be worse than leaving the line alone.
        if !opts.deep {
            entry.notes.push(Note::info(
                "commit carries no tag; --deep establishes whether that is expected".to_string(),
            ));
        }
        return None;
    };

    let desired_version = PinComment::parse(desired).and_then(|c| c.version);
    let claimed = occ
        .comment
        .as_ref()
        .and_then(|c| PinComment::parse(&c.text))
        .and_then(|c| c.version);

    // The date is deliberately not compared: it may legitimately be the release date
    // rather than the commit date, and those differ. Not comparing it is also what
    // keeps a no-op run byte-identical.
    if claimed == desired_version {
        return None;
    }

    let claimed_text = match (&claimed, &occ.comment) {
        (Some(v), _) => v.clone(),
        (None, Some(c)) => format!("{:?}", c.text),
        (None, None) => "nothing".to_string(),
    };

    if opts.writes() {
        entry
            .notes
            .push(Note::info(format!("comment {claimed_text} -> {desired}")));
        return Some(render_edit(occ, &occ.value, Some(desired)));
    }

    if opts.deep {
        let message = format!("comment says {claimed_text}, but this commit is {desired}");
        entry.notes.push(if opts.check {
            Note::fail(message)
        } else {
            Note::warn(message)
        });
    }
    None
}

/// Build the replacement text for a line.
fn render_edit(occ: &Occurrence, value: &str, comment: Option<&str>) -> Edit {
    let quoted = match occ.quoting {
        Quoting::Plain => value.to_string(),
        Quoting::Single => format!("'{value}'"),
        Quoting::Double => format!("\"{value}\""),
    };
    let replacement = match comment {
        Some(c) => format!("{quoted} # {c}"),
        None => quoted,
    };
    Edit {
        span: occ.edit_span(),
        replacement,
    }
}

/// Rebuild the value with a new commit, preserving quoting.
fn render_edit_with_sha(occ: &Occurrence, sha: &str, comment: Option<&str>) -> Edit {
    let head = occ
        .value
        .split_once('@')
        .map_or(occ.value.clone(), |(path, _)| path.to_string());
    render_edit(occ, &format!("{head}@{sha}"), comment)
}

/// The comment a commit deserves: its most specific tag and that tag's date.
fn comment_for(sha: &str, tags: &[TagInfo]) -> Option<String> {
    let at_sha: Vec<&TagInfo> = tags.iter().filter(|t| t.commit == sha).collect();
    if at_sha.is_empty() {
        return None;
    }
    let names: Vec<&str> = at_sha.iter().map(|t| t.name.as_str()).collect();
    let best = most_specific(names)?;
    let date = at_sha
        .iter()
        .find(|t| t.name == best.raw())
        .map(|t| t.date.as_str())?;
    Some(format!("{}, {}", best.raw(), date))
}

/// Tag names pointing at a commit, if any.
fn tag_names_at(sha: &str, tags: &[TagInfo]) -> Option<Vec<String>> {
    let names: Vec<String> = tags
        .iter()
        .filter(|t| t.commit == sha)
        .map(|t| t.name.clone())
        .collect();
    (!names.is_empty()).then_some(names)
}

/// Abbreviate a commit id for a message, leaving anything else alone.
fn short(git_ref: &str) -> String {
    if git_ref.len() == 40 && git_ref.chars().all(|c| c.is_ascii_hexdigit()) {
        git_ref[..7].to_string()
    } else {
        git_ref.to_string()
    }
}

/// Splice edits into the source in one forward pass.
fn apply(src: &str, edits: &mut [Edit]) -> String {
    edits.sort_by_key(|e| e.span.start);
    let mut out = String::with_capacity(src.len());
    let mut cursor = 0;
    for edit in edits.iter() {
        if edit.span.start < cursor {
            continue;
        }
        out.push_str(&src[cursor..edit.span.start]);
        out.push_str(&edit.replacement);
        cursor = edit.span.end;
    }
    out.push_str(&src[cursor..]);
    out
}

/// Read a file and process it.
pub fn process_path(
    path: &std::path::Path,
    forge: Forge,
    resolver: &dyn Resolver,
    opts: &Options,
) -> Result<Outcome> {
    let src = std::fs::read_to_string(path).map_err(Error::Io)?;
    process(&src, forge, resolver, opts)
}

#[cfg(test)]
mod tests_support {
    pub use super::tests::{CO, CO_OLD, resolver, wf};
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::FakeResolver;

    pub const CO: &str = "8e8c483db84b4bee98b60c0593521ed34d9990e8";
    pub const CO_OLD: &str = "1111111111111111111111111111111111111111";
    const EVIL: &str = "dead00000000000000000000000000000000beef";
    /// A commit no tag points at, for the untagged-pin cases.
    const UNTAGGED: &str = "2222222222222222222222222222222222222222";

    /// `actions/checkout` with v6.0.1 current and v6.0.0 superseded.
    pub fn resolver() -> FakeResolver {
        FakeResolver::new()
            .with_tag("actions/checkout", "v6", CO, "2025-12-02")
            .with_tag("actions/checkout", "v6.0.1", CO, "2025-12-02")
            .with_tag("actions/checkout", "v6.0.0", CO_OLD, "2025-06-01")
            .with_branch("actions/checkout", "main", CO)
    }

    pub fn wf(uses: &str) -> String {
        format!("jobs:\n  b:\n    steps:\n      - uses: {uses}\n")
    }

    fn run(src: &str, opts: &Options) -> Outcome {
        process(src, Forge::GitHub, &resolver(), opts).expect("processes")
    }

    fn opts(f: impl FnOnce(&mut Options)) -> Options {
        let mut o = Options {
            allow: Vec::new(),
            ..Default::default()
        };
        f(&mut o);
        o
    }

    fn messages(o: &Outcome) -> Vec<String> {
        o.entries
            .iter()
            .flat_map(|e| e.notes.iter().map(|n| n.message.clone()))
            .collect()
    }

    // --- pinning ---------------------------------------------------------

    #[test]
    fn pin_resolves_a_tag_and_writes_the_comment() {
        let out = run(&wf("actions/checkout@v6"), &opts(|o| o.pin = true));
        assert_eq!(
            out.rewritten.as_deref(),
            Some(wf(&format!("actions/checkout@{CO} # v6.0.1, 2025-12-02")).as_str())
        );
    }

    #[test]
    fn pin_picks_the_most_specific_tag_for_the_comment() {
        // The commit carries both v6 and v6.0.1; the comment must name the latter.
        let out = run(&wf("actions/checkout@v6"), &opts(|o| o.pin = true));
        assert!(out.rewritten.expect("rewritten").contains("# v6.0.1,"));
    }

    #[test]
    fn pin_resolves_a_branch() {
        let out = run(&wf("actions/checkout@main"), &opts(|o| o.pin = true));
        assert!(out.rewritten.expect("rewritten").contains(CO));
    }

    #[test]
    fn pin_preserves_quoting() {
        let out = run(&wf("'actions/checkout@v6'"), &opts(|o| o.pin = true));
        let got = out.rewritten.expect("rewritten");
        assert!(got.contains(&format!("'actions/checkout@{CO}'")), "{got}");
    }

    #[test]
    fn pin_reports_an_unknown_ref_without_giving_up() {
        let src = "jobs:\n  b:\n    steps:\n      - uses: actions/checkout@nope\n      - uses: actions/checkout@v6\n";
        let out = run(src, &opts(|o| o.pin = true));
        assert!(
            messages(&out)
                .iter()
                .any(|m| m.contains("no tag or branch"))
        );
        assert!(out.failed());
        // The second reference is still pinned despite the first one failing.
        assert!(out.rewritten.expect("rewritten").contains(CO));
    }

    // --- the round-trip guarantee ---------------------------------------

    #[test]
    fn pinning_an_already_correct_file_changes_nothing() {
        let src = wf(&format!("actions/checkout@{CO} # v6.0.1, 2025-12-02"));
        let out = run(&src, &opts(|o| o.pin = true));
        assert_eq!(out.rewritten, None, "a correct file must not be rewritten");
    }

    #[test]
    fn a_differing_date_is_left_alone() {
        // The date may be the release date rather than the commit date, so it is
        // never the thing that triggers a rewrite.
        let src = wf(&format!("actions/checkout@{CO} # v6.0.1, 2020-01-01"));
        assert_eq!(run(&src, &opts(|o| o.pin = true)).rewritten, None);
    }

    #[test]
    fn round_trip_holds_with_non_ascii_above_the_pin() {
        let src = format!(
            "name: na\u{ef}ve \u{2728}\njobs:\n  b:\n    steps:\n      - name: h\u{e9}llo \u{2728}\n        uses: actions/checkout@{CO} # v6.0.1, 2025-12-02\n"
        );
        assert_eq!(run(&src, &opts(|o| o.pin = true)).rewritten, None);
    }

    // --- comment repair --------------------------------------------------

    #[test]
    fn pin_repairs_a_lying_comment() {
        let src = wf(&format!("actions/checkout@{CO} # v1.0.0, 2020-01-01"));
        let out = run(&src, &opts(|o| o.pin = true));
        assert!(
            out.rewritten
                .expect("rewritten")
                .contains("# v6.0.1, 2025-12-02")
        );
    }

    #[test]
    fn pin_adds_a_missing_comment() {
        let src = wf(&format!("actions/checkout@{CO}"));
        let out = run(&src, &opts(|o| o.pin = true));
        assert!(
            out.rewritten
                .expect("rewritten")
                .contains("# v6.0.1, 2025-12-02")
        );
    }

    #[test]
    fn deep_check_fails_on_a_lying_comment() {
        let src = wf(&format!("actions/checkout@{CO} # v1.0.0, 2020-01-01"));
        let out = run(
            &src,
            &opts(|o| {
                o.check = true;
                o.deep = true
            }),
        );
        assert!(out.failed());
        assert!(
            messages(&out)
                .iter()
                .any(|m| m.contains("comment says v1.0.0"))
        );
    }

    #[test]
    fn check_without_deep_ignores_the_comment() {
        let src = wf(&format!("actions/checkout@{CO} # v1.0.0, 2020-01-01"));
        assert!(!run(&src, &opts(|o| o.check = true)).failed());
    }

    // --- checking --------------------------------------------------------

    #[test]
    fn check_fails_on_an_unpinned_reference() {
        let out = run(&wf("actions/checkout@v6"), &opts(|o| o.check = true));
        assert!(out.failed());
        assert!(messages(&out).iter().any(|m| m == "not pinned to a commit"));
    }

    #[test]
    fn check_passes_on_a_pinned_reference() {
        let src = wf(&format!("actions/checkout@{CO} # v6.0.1, 2025-12-02"));
        assert!(!run(&src, &opts(|o| o.check = true)).failed());
    }

    #[test]
    fn allowlist_relaxes_check_only() {
        let src = wf("actions/checkout@v6");
        let allowed = opts(|o| {
            o.check = true;
            o.allow = vec![Pattern::new("actions/*")];
        });
        assert!(
            !process(&src, Forge::GitHub, &resolver(), &allowed)
                .expect("processes")
                .failed()
        );

        // ...but --pin still pins it.
        let pinning = opts(|o| {
            o.pin = true;
            o.allow = vec![Pattern::new("actions/*")];
        });
        assert!(
            process(&src, Forge::GitHub, &resolver(), &pinning)
                .expect("processes")
                .rewritten
                .is_some()
        );
    }

    // --- bumping ---------------------------------------------------------

    #[test]
    fn bump_moves_a_stale_pin_to_the_latest_release() {
        let src = wf(&format!("actions/checkout@{CO_OLD} # v6.0.0, 2025-06-01"));
        let out = run(&src, &opts(|o| o.bump = true));
        let got = out.rewritten.expect("rewritten");
        assert!(got.contains(CO), "{got}");
        assert!(got.contains("# v6.0.1, 2025-12-02"), "{got}");
    }

    #[test]
    fn bump_leaves_a_current_pin_alone() {
        let src = wf(&format!("actions/checkout@{CO} # v6.0.1, 2025-12-02"));
        assert_eq!(run(&src, &opts(|o| o.bump = true)).rewritten, None);
    }

    #[test]
    fn check_bump_is_a_staleness_gate() {
        let src = wf(&format!("actions/checkout@{CO_OLD} # v6.0.0, 2025-06-01"));
        let out = run(
            &src,
            &opts(|o| {
                o.check = true;
                o.bump = true
            }),
        );
        assert!(out.failed());
        assert!(messages(&out).iter().any(|m| m.contains("stale")));
        assert_eq!(out.rewritten, None, "--check must not write");
    }

    #[test]
    fn bump_warns_when_crossing_a_major() {
        let r = FakeResolver::new()
            .with_tag("a/b", "v1.0.0", CO_OLD, "2024-01-01")
            .with_tag("a/b", "v2.0.0", CO, "2025-01-01");
        let src = wf(&format!("a/b@{CO_OLD} # v1.0.0, 2024-01-01"));
        let out = process(&src, Forge::GitHub, &r, &opts(|o| o.bump = true)).expect("processes");
        assert!(
            messages(&out)
                .iter()
                .any(|m| m.contains("major version change v1.0.0 -> v2.0.0"))
        );
    }

    #[test]
    fn bump_ignores_prereleases() {
        let r = FakeResolver::new()
            .with_tag("a/b", "v1.0.0", CO_OLD, "2024-01-01")
            .with_tag("a/b", "v2.0.0-rc1", CO, "2025-01-01");
        let src = wf(&format!("a/b@{CO_OLD} # v1.0.0, 2024-01-01"));
        assert_eq!(
            process(&src, Forge::GitHub, &r, &opts(|o| o.bump = true))
                .expect("processes")
                .rewritten,
            None
        );
    }

    #[test]
    fn pin_and_bump_together_land_on_the_latest() {
        let out = run(
            &wf("actions/checkout@v6.0.0"),
            &opts(|o| {
                o.pin = true;
                o.bump = true
            }),
        );
        let got = out.rewritten.expect("rewritten");
        assert!(got.contains(CO), "{got}");
        assert!(got.contains("# v6.0.1,"), "{got}");
    }

    // --- deep validation -------------------------------------------------

    #[test]
    fn deep_fails_on_a_commit_reachable_from_no_ref() {
        let src = wf(&format!("actions/checkout@{EVIL}"));
        let out = run(
            &src,
            &opts(|o| {
                o.check = true;
                o.deep = true
            }),
        );
        assert!(out.failed());
        assert!(
            messages(&out)
                .iter()
                .any(|m| m.contains("not reachable from any ref"))
        );
    }

    #[test]
    fn deep_accepts_an_untagged_commit_on_a_branch() {
        // Pinning an unreleased fix on main is legitimate and must not fail.
        let r = resolver().with_commit(
            "actions/checkout",
            UNTAGGED,
            Reachability::FromBranch,
            Some("2025-06-01"),
        );
        let src = wf(&format!("actions/checkout@{UNTAGGED}"));
        let out = process(
            &src,
            Forge::GitHub,
            &r,
            &opts(|o| {
                o.check = true;
                o.deep = true
            }),
        )
        .expect("processes");
        assert!(!out.failed(), "{:?}", messages(&out));
        assert!(messages(&out).iter().any(|m| m.contains("no release tag")));
    }

    #[test]
    fn deep_does_not_fail_when_it_cannot_verify() {
        let r = resolver().with_commit(
            "actions/checkout",
            UNTAGGED,
            Reachability::Unverifiable,
            None,
        );
        let src = wf(&format!("actions/checkout@{UNTAGGED}"));
        let out = process(
            &src,
            Forge::GitHub,
            &r,
            &opts(|o| {
                o.check = true;
                o.deep = true
            }),
        )
        .expect("processes");
        assert!(!out.failed());
        assert!(
            messages(&out)
                .iter()
                .any(|m| m.contains("could not verify"))
        );
    }

    // --- non-git references ----------------------------------------------

    #[test]
    fn mutable_docker_tag_fails_check() {
        let out = run(&wf("docker://alpine:3.8"), &opts(|o| o.check = true));
        assert!(out.failed());
        assert!(
            messages(&out)
                .iter()
                .any(|m| m.contains("mutable image reference"))
        );
    }

    #[test]
    fn digest_pinned_docker_passes() {
        let out = run(&wf("docker://alpine@sha256:abc"), &opts(|o| o.check = true));
        assert!(!out.failed());
    }

    #[test]
    fn docker_is_never_rewritten() {
        let out = run(
            &wf("docker://alpine:3.8"),
            &opts(|o| {
                o.pin = true;
                o.bump = true
            }),
        );
        assert_eq!(out.rewritten, None);
        assert!(messages(&out).iter().any(|m| m.contains("out of scope")));
    }

    #[test]
    fn local_actions_never_fail() {
        let out = run(
            &wf("./.github/actions/build"),
            &opts(|o| {
                o.check = true;
                o.deep = true
            }),
        );
        assert!(!out.failed());
    }

    // --- host resolution -------------------------------------------------

    #[test]
    fn a_bare_slug_means_different_repositories_on_each_forge() {
        // The same text resolves against github.com under .github/ and against the
        // Forgejo default under .forgejo/, so a Forgejo run must not find the tag
        // that only the GitHub-side fake declares.
        let src = wf("actions/checkout@v6");
        let o = opts(|o| o.pin = true);
        assert!(
            process(&src, Forge::GitHub, &resolver(), &o)
                .expect("processes")
                .rewritten
                .is_some()
        );

        let forgejo = process(&src, Forge::Forgejo, &resolver(), &o).expect("processes");
        assert!(forgejo.rewritten.is_none());
    }

    #[test]
    fn an_absolute_url_overrides_the_forge_default() {
        let src = wf("https://github.com/actions/checkout@v6");
        let out =
            process(&src, Forge::Forgejo, &resolver(), &opts(|o| o.pin = true)).expect("processes");
        assert!(out.rewritten.expect("rewritten").contains(CO));
    }

    // --- listing ---------------------------------------------------------

    #[test]
    fn listing_touches_no_network_and_writes_nothing() {
        struct Exploding;
        impl Resolver for Exploding {
            fn tags(&self, _: &Remote) -> Result<Vec<TagInfo>> {
                panic!("--list must not resolve anything")
            }
            fn resolve_ref(&self, _: &Remote, _: &str) -> Result<Option<String>> {
                panic!("--list must not resolve anything")
            }
            fn describe(&self, _: &Remote, _: &str) -> Result<crate::resolver::CommitInfo> {
                panic!("--list must not resolve anything")
            }
        }
        let src = wf("actions/checkout@v6");
        let out = process(&src, Forge::GitHub, &Exploding, &Options::default()).expect("processes");
        assert_eq!(out.rewritten, None);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.entries[0].git_ref, "v6");
    }

    #[test]
    fn short_shas_are_flagged() {
        let out = run(&wf("actions/checkout@8e8c483"), &Options::default());
        assert!(
            messages(&out)
                .iter()
                .any(|m| m.contains("abbreviated commit id"))
        );
    }
}

#[cfg(test)]
mod check_never_writes {
    use super::tests_support::*;
    use super::*;

    /// A gate that edited the files it was gating would be a trap in CI, so this is
    /// asserted for every combination `--check` is allowed to appear in.
    #[test]
    fn no_combination_of_check_produces_an_edit() {
        let cases = [
            // A stale pin, which --bump would otherwise move.
            format!("actions/checkout@{CO_OLD} # v6.0.0, 2025-06-01"),
            // A lying comment, which --pin would otherwise repair.
            format!("actions/checkout@{CO} # v1.0.0, 2020-01-01"),
            // A missing comment.
            format!("actions/checkout@{CO}"),
            // An unpinned reference.
            "actions/checkout@v6".to_string(),
        ];

        for case in cases {
            for (bump, deep) in [(false, false), (true, false), (false, true), (true, true)] {
                let o = Options {
                    check: true,
                    bump,
                    deep,
                    pin: false,
                    allow: Vec::new(),
                    ..Default::default()
                };
                let out = process(&wf(&case), Forge::GitHub, &resolver(), &o).expect("processes");
                assert_eq!(
                    out.rewritten, None,
                    "--check --bump={bump} --deep={deep} rewrote {case:?}"
                );
            }
        }
    }
}
