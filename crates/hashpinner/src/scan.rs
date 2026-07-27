//! Locating every `uses:` value in a workflow or action file, with byte spans.
//!
//! A real YAML parse is used rather than a line regex, because a `run: |` block that
//! generates a workflow contains `uses:` lines that must not be rewritten, and no
//! regex can tell those from the real thing. The parse is only used to *find*
//! things: edits are spliced into the original text by byte offset, so comments,
//! quoting, indentation and anchors survive untouched.
//!
//! Three properties of [`saphyr_parser`] shape this module, all verified by the
//! tests below rather than taken from its documentation:
//!
//! 1. [`Marker::index`] is a **character** offset, even though its own doc comment
//!    says bytes. Slicing with it directly corrupts any file containing non-ASCII,
//!    and does so silently, so a char-to-byte table is mandatory.
//! 2. Block scalar spans run past the value into the next line's indentation.
//! 3. `uses: *alias` arrives as [`Event::Alias`] and never as a scalar, so it has to
//!    be resolved separately or it vanishes.
//!
//! # Anchors, aliases and merge keys
//!
//! These are not exotic. GitHub gained anchor support in September 2025, so they are
//! becoming more common rather than less, and both forms hide a `uses:` from anything
//! that only pattern-matches the document structure:
//!
//! ```yaml
//! x-shared: &co actions/checkout@v4
//! jobs:
//!   a:
//!     steps:
//!       - uses: *co          # an alias
//!       - <<: *step-defaults # a merge key, whose mapping may carry its own uses
//! ```
//!
//! Neither reference is a scalar at a `uses:` position, and the anchor they point at
//! sits under a key no pinner would look at. The stream is therefore expanded before
//! it is walked: aliases are replaced by a replay of the node they name, and a `<<`
//! key splices that node's contents into the mapping around it.
//!
//! An expanded value is reported where it was *written* but rewritten where it was
//! *defined*, since the anchor is the only place an edit can go. Two aliases onto one
//! anchor therefore produce one edit, which is why [`crate::rewrite`] must tolerate
//! duplicates.
//!
//! [`Marker::index`]: saphyr_parser::Marker::index

use std::collections::HashMap;
use std::ops::Range;

use saphyr_parser::{Event, Parser, ScalarStyle};

use crate::{Error, Result};

/// Ceiling on events after expansion.
///
/// Alias replay is what makes a YAML bomb possible: a chain of anchors each naming
/// the previous one several times expands exponentially. GitHub caps the same thing
/// at 50000 nodes; this is looser, and only has to stop the process from dying.
const MAX_EXPANDED_EVENTS: usize = 200_000;

/// Where in the document a `uses:` key was found.
///
/// Only these positions are pinnable. A `uses` key anywhere else — most commonly as
/// an input under `with:` — is somebody else's data and is left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// `jobs.<id>.steps[*].uses` in a workflow.
    Step,
    /// `jobs.<id>.uses`, a reusable workflow.
    ReusableWorkflow,
    /// `runs.steps[*].uses` in a composite `action.yml`.
    CompositeStep,
}

/// How the value was written, which determines how a replacement must be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quoting {
    /// Bare.
    Plain,
    /// Wrapped in `'`.
    Single,
    /// Wrapped in `"`.
    Double,
}

/// A trailing `# ...` comment on the same line as a `uses:` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Byte span covering the whitespace before `#` through to end of line.
    pub span: Range<usize>,
    /// The comment text with its `#` and surrounding whitespace stripped.
    pub text: String,
}

/// How a value reached the `uses:` key it was found under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Written at the position it was found.
    Inline,
    /// Reached through a YAML alias or merge key, from an anchor defined elsewhere.
    ///
    /// The occurrence's span points at that anchor, because an edit has nowhere else
    /// to go: the alias site holds no value of its own.
    Anchored {
        /// 1-indexed line the anchor is defined on.
        line: usize,
    },
}

/// One `uses:` value found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// Where in the document structure it sits.
    pub slot: Slot,
    /// The value with any quotes removed.
    pub value: String,
    /// How it was quoted.
    pub quoting: Quoting,
    /// Byte span of the scalar, including its quotes if any.
    ///
    /// For an anchored occurrence this is the anchor's own span, not the alias site.
    pub span: Range<usize>,
    /// The trailing comment, if the line has one.
    pub comment: Option<Comment>,
    /// 1-indexed line the reference was written on.
    pub line: usize,
    /// Whether the value was written here or reached through an anchor.
    pub origin: Origin,
}

impl Occurrence {
    /// The byte span an edit must replace: the value plus any trailing comment.
    pub fn edit_span(&self) -> Range<usize> {
        let end = self.comment.as_ref().map_or(self.span.end, |c| c.span.end);
        self.span.start..end
    }
}

/// A `uses:` key whose value could not be turned into an [`Occurrence`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// Why it was skipped, phrased for a user.
    pub reason: String,
    /// 1-indexed line, for diagnostics.
    pub line: usize,
}

/// Everything of interest in one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Values that can be inspected and rewritten.
    pub occurrences: Vec<Occurrence>,
    /// Values recognised as `uses:` but not representable, reported and left alone.
    pub unsupported: Vec<Unsupported>,
}

/// One level of document nesting.
#[derive(Debug)]
enum Frame {
    Mapping {
        expect_key: bool,
        current_key: Option<String>,
        /// A mapping spliced in by `<<`, whose keys belong to its parent and which
        /// therefore occupies no segment of the path.
        merged: bool,
    },
    Sequence,
}

/// One event from the document, with its span already in bytes.
#[derive(Debug, Clone)]
struct Step<'a> {
    event: Event<'a>,
    span: Range<usize>,
    /// Byte offset of the alias this event was replayed for, if it was replayed at
    /// all. `None` for events that are where the author put them.
    via: Option<usize>,
}

/// A step in the path from the document root to the current node.
#[derive(Debug, PartialEq, Eq)]
enum Seg {
    Key(String),
    Index,
}

/// Find every pinnable `uses:` value in `src`.
///
/// Fails only when the document does not parse. A file hashpinner cannot read is
/// never a file it will edit.
pub fn scan(src: &str) -> Result<Scan> {
    let mut out = Scan::default();
    let mut frames: Vec<Frame> = Vec::new();
    let mut path: Vec<Seg> = Vec::new();

    for step in expand(src)? {
        let Step { event, span, via } = step;

        match event {
            Event::MappingStart(..) => {
                // A mapping introduced by `<<` is transparent: its keys are keys of
                // the mapping around it, so it must not occupy a path segment or the
                // merged-in `uses:` would sit one level too deep to be recognised.
                let merged = current_key_is_merge(&frames);
                if merged {
                    take_current_key(&mut frames);
                } else {
                    path.push(entering_segment(&frames));
                }
                frames.push(Frame::Mapping {
                    expect_key: true,
                    current_key: None,
                    merged,
                });
            }

            Event::SequenceStart(..) => {
                path.push(entering_segment(&frames));
                frames.push(Frame::Sequence);
            }

            Event::MappingEnd | Event::SequenceEnd => {
                let was_merged = matches!(frames.last(), Some(Frame::Mapping { merged: true, .. }));
                frames.pop();
                if !was_merged {
                    path.pop();
                }
                finish_value(&mut frames);
            }

            Event::Scalar(value, style, ..) => {
                if let Some(Frame::Mapping {
                    expect_key: expect_key @ true,
                    current_key,
                    ..
                }) = frames.last_mut()
                {
                    *expect_key = false;
                    *current_key = Some(value.into_owned());
                    continue;
                }

                if let Some(slot) = pinnable_slot(&frames, &path) {
                    // The span is where the value is, which for a replayed event is
                    // the anchor; `via` is where the author asked for it.
                    let defined_at = line_of(src, span.start);
                    let (line, origin) = match via {
                        Some(offset) => {
                            (line_of(src, offset), Origin::Anchored { line: defined_at })
                        }
                        None => (defined_at, Origin::Inline),
                    };

                    match quoting_of(style) {
                        Some(quoting) => out.occurrences.push(Occurrence {
                            slot,
                            value: value.into_owned(),
                            quoting,
                            comment: trailing_comment(src, span.end),
                            line,
                            span,
                            origin,
                        }),
                        None => out.unsupported.push(Unsupported {
                            reason: "block scalar values are not rewritten".to_string(),
                            line,
                        }),
                    }
                }

                finish_value(&mut frames);
            }

            // Expansion resolves every alias it can, so one surviving here named an
            // anchor that does not exist — which no forge will run either.
            Event::Alias(..) => {
                if pinnable_slot(&frames, &path).is_some() {
                    out.unsupported.push(Unsupported {
                        reason: "alias refers to an anchor that is not defined".to_string(),
                        line: line_of(src, span.start),
                    });
                }
                finish_value(&mut frames);
            }

            _ => {}
        }
    }

    out.occurrences.sort_by_key(|o| o.line);
    Ok(out)
}

/// Read the document and replace every alias with a replay of the node it names.
///
/// Anchors are scoped to their document, so the table is cleared at each document
/// boundary: the YAML 1.2 spec makes an alias to an anchor from a previous document
/// an error, and resolving one anyway would invent a reference no forge would honour.
fn expand(src: &str) -> Result<Vec<Step<'_>>> {
    let char_to_byte = char_to_byte_table(src);

    let mut raw: Vec<Step<'_>> = Vec::new();
    for event in Parser::new_from_str(src) {
        let (event, span) = event.map_err(|e| Error::Yaml(e.to_string()))?;
        raw.push(Step {
            span: byte_span(&char_to_byte, src, span),
            event,
            via: None,
        });
    }

    // Where each anchor's node begins and ends in `raw`, so it can be replayed.
    let mut anchors: HashMap<usize, Range<usize>> = HashMap::new();
    for (index, step) in raw.iter().enumerate() {
        match &step.event {
            Event::DocumentStart(..) => anchors.clear(),
            Event::Scalar(_, _, id, _) if *id > 0 => {
                anchors.insert(*id, index..index + 1);
            }
            Event::MappingStart(id, _) | Event::SequenceStart(id, _) if *id > 0 => {
                anchors.insert(*id, index..node_end(&raw, index));
            }
            _ => {}
        }
    }

    let mut out = Vec::with_capacity(raw.len());
    replay(&raw, 0..raw.len(), &anchors, None, 0, &mut out)?;
    Ok(out)
}

/// The index one past the node starting at `start`, by matching nesting.
fn node_end(raw: &[Step<'_>], start: usize) -> usize {
    let mut depth = 0usize;
    for (offset, step) in raw[start..].iter().enumerate() {
        match step.event {
            Event::MappingStart(..) | Event::SequenceStart(..) => depth += 1,
            Event::MappingEnd | Event::SequenceEnd => {
                depth -= 1;
                if depth == 0 {
                    return start + offset + 1;
                }
            }
            _ => {}
        }
    }
    raw.len()
}

/// Copy `range` into `out`, expanding aliases as they are met.
///
/// `via` carries the line of the alias currently being expanded, so a value replayed
/// from an anchor still knows where it was asked for.
fn replay<'a>(
    raw: &[Step<'a>],
    range: Range<usize>,
    anchors: &HashMap<usize, Range<usize>>,
    via: Option<usize>,
    depth: usize,
    out: &mut Vec<Step<'a>>,
) -> Result<()> {
    // An anchor may name itself, directly or around a loop, and YAML does not forbid
    // it. Depth is the only thing that distinguishes that from legitimate nesting.
    if depth > 64 {
        return Err(Error::Yaml("alias expansion nested too deeply".to_string()));
    }

    let mut index = range.start;
    while index < range.end {
        if out.len() > MAX_EXPANDED_EVENTS {
            return Err(Error::Yaml(
                "alias expansion produced too many nodes".to_string(),
            ));
        }

        let step = &raw[index];
        match step.event {
            Event::Alias(id) => match anchors.get(&id) {
                Some(target) => {
                    let site = via.unwrap_or(step.span.start);
                    replay(raw, target.clone(), anchors, Some(site), depth + 1, out)?;
                }
                // Left in place, so the walk can report it rather than lose it.
                None => out.push(step.clone()),
            },
            _ => out.push(Step {
                event: step.event.clone(),
                span: step.span.clone(),
                via: via.or(step.via),
            }),
        }
        index += 1;
    }
    Ok(())
}

/// Whether the key now awaiting its value is the merge key.
fn current_key_is_merge(frames: &[Frame]) -> bool {
    matches!(
        frames.last(),
        Some(Frame::Mapping { current_key, .. }) if current_key.as_deref() == Some("<<")
    )
}

/// Forget the key awaiting a value, used when its value is spliced into the mapping.
fn take_current_key(frames: &mut [Frame]) {
    if let Some(Frame::Mapping { current_key, .. }) = frames.last_mut() {
        *current_key = None;
    }
}

/// Which path segment a container occupies in its parent.
fn entering_segment(frames: &[Frame]) -> Seg {
    match frames.last() {
        Some(Frame::Mapping { current_key, .. }) => {
            Seg::Key(current_key.clone().unwrap_or_default())
        }
        Some(Frame::Sequence) => Seg::Index,
        None => Seg::Key(String::new()),
    }
}

/// Mark the innermost mapping as ready for its next key.
fn finish_value(frames: &mut [Frame]) {
    if let Some(Frame::Mapping { expect_key, .. }) = frames.last_mut() {
        *expect_key = true;
    }
}

/// Decide whether the value now being read belongs to a pinnable `uses:` key.
fn pinnable_slot(frames: &[Frame], path: &[Seg]) -> Option<Slot> {
    let Some(Frame::Mapping { current_key, .. }) = frames.last() else {
        return None;
    };
    // A merged mapping holds no path segment of its own, so a `uses:` inside one is
    // matched against the path of the mapping it was spliced into, which is right.
    if current_key.as_deref() != Some("uses") {
        return None;
    }

    // `path` excludes the document root, whose segment is an empty key.
    let segs: Vec<&Seg> = path.iter().skip(1).collect();
    match segs.as_slice() {
        // jobs.<id>.steps[*].uses
        [Seg::Key(a), Seg::Key(_), Seg::Key(c), Seg::Index] if a == "jobs" && c == "steps" => {
            Some(Slot::Step)
        }
        // jobs.<id>.uses
        [Seg::Key(a), Seg::Key(_)] if a == "jobs" => Some(Slot::ReusableWorkflow),
        // runs.steps[*].uses, in a composite action.yml
        [Seg::Key(a), Seg::Key(b), Seg::Index] if a == "runs" && b == "steps" => {
            Some(Slot::CompositeStep)
        }
        _ => None,
    }
}

/// Map a scalar style onto a rewritable quoting, or `None` if it is a block scalar.
fn quoting_of(style: ScalarStyle) -> Option<Quoting> {
    match style {
        ScalarStyle::Plain => Some(Quoting::Plain),
        ScalarStyle::SingleQuoted => Some(Quoting::Single),
        ScalarStyle::DoubleQuoted => Some(Quoting::Double),
        ScalarStyle::Literal | ScalarStyle::Folded => None,
    }
}

/// A lookup from character index to byte index, plus a final entry for the end.
fn char_to_byte_table(src: &str) -> Vec<usize> {
    let mut table: Vec<usize> = src.char_indices().map(|(b, _)| b).collect();
    table.push(src.len());
    table
}

/// Convert a parser span into a byte range, trimming the overshoot that block
/// scalars and some plain scalars leave on the end.
fn byte_span(table: &[usize], src: &str, span: saphyr_parser::Span) -> Range<usize> {
    let start = table.get(span.start.index()).copied().unwrap_or(src.len());
    let mut end = table.get(span.end.index()).copied().unwrap_or(src.len());

    let bytes = src.as_bytes();
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }

    start..end
}

/// Find a `# ...` comment between `from` and the end of that line.
fn trailing_comment(src: &str, from: usize) -> Option<Comment> {
    let rest = src.get(from..)?;
    let line_end = rest.find('\n').map_or(rest.len(), |i| i);
    let line = &rest[..line_end];

    let hash = line.find('#')?;
    // Only whitespace may separate the value from its comment; anything else means
    // the `#` belongs to the value or to something this tool should not touch.
    if !line[..hash].chars().all(char::is_whitespace) {
        return None;
    }

    Some(Comment {
        span: from..from + line_end,
        text: line[hash + 1..].trim().to_string(),
    })
}

/// 1-indexed line containing a byte offset.
fn line_of(src: &str, offset: usize) -> usize {
    src.get(..offset).map_or(1, |s| s.matches('\n').count() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every hazard this module exists to handle, in one document: non-ASCII ahead
    /// of the first edit, all three quoting styles, a `with:` decoy, a `run: |`
    /// heredoc containing a plausible `uses:` line, and a reusable workflow.
    const HOSTILE: &str = "\
name: na\u{ef}ve \u{2728}
on: push

jobs:
  build:
    steps:
      - name: h\u{e9}llo w\u{f6}rld \u{2728}
        uses: actions/checkout@v4  # v4.0.0, 2024-01-01
      - uses: 'actions/setup-go@v5'
      - uses: \"actions/cache@v3\"
      - uses: ./.github/actions/local
        with:
          uses: not-a-step-uses
      - name: heredoc
        run: |
          echo '- uses: evil/act@v1'
      - uses: docker://alpine:3.8
  call:
    uses: owner/repo/.github/workflows/ci.yml@v1
";

    fn scan_ok(src: &str) -> Scan {
        scan(src).expect("parses")
    }

    /// The whole point of the byte-span machinery: slicing must round-trip.
    fn sliced<'a>(src: &'a str, o: &Occurrence) -> &'a str {
        src.get(o.span.clone()).expect("span is on char boundaries")
    }

    #[test]
    fn finds_every_pinnable_use_and_no_others() {
        let s = scan_ok(HOSTILE);
        let values: Vec<&str> = s.occurrences.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(
            values,
            [
                "actions/checkout@v4",
                "actions/setup-go@v5",
                "actions/cache@v3",
                "./.github/actions/local",
                "docker://alpine:3.8",
                "owner/repo/.github/workflows/ci.yml@v1",
            ]
        );
    }

    #[test]
    fn heredoc_uses_is_not_a_use() {
        let s = scan_ok(HOSTILE);
        assert!(s.occurrences.iter().all(|o| !o.value.contains("evil")));
    }

    #[test]
    fn uses_under_with_is_not_a_use() {
        let s = scan_ok(HOSTILE);
        assert!(s.occurrences.iter().all(|o| o.value != "not-a-step-uses"));
    }

    /// The regression test for `Marker::index` being a character offset: with the
    /// emoji above the first edit, a byte/char mixup shifts every span by four.
    #[test]
    fn spans_survive_non_ascii() {
        let s = scan_ok(HOSTILE);
        for o in &s.occurrences {
            let expected = match o.quoting {
                Quoting::Plain => o.value.clone(),
                Quoting::Single => format!("'{}'", o.value),
                Quoting::Double => format!("\"{}\"", o.value),
            };
            assert_eq!(sliced(HOSTILE, o), expected, "span mismatch for {o:?}");
        }
    }

    #[test]
    fn quoting_is_recorded() {
        let s = scan_ok(HOSTILE);
        let q: Vec<Quoting> = s.occurrences.iter().map(|o| o.quoting).collect();
        assert_eq!(q[0], Quoting::Plain);
        assert_eq!(q[1], Quoting::Single);
        assert_eq!(q[2], Quoting::Double);
    }

    #[test]
    fn slots_are_classified() {
        let s = scan_ok(HOSTILE);
        assert_eq!(s.occurrences[0].slot, Slot::Step);
        assert_eq!(s.occurrences[5].slot, Slot::ReusableWorkflow);
    }

    #[test]
    fn trailing_comment_is_captured() {
        let s = scan_ok(HOSTILE);
        let c = s.occurrences[0].comment.as_ref().expect("has a comment");
        assert_eq!(c.text, "v4.0.0, 2024-01-01");
        assert_eq!(
            HOSTILE.get(c.span.clone()).expect("comment span"),
            "  # v4.0.0, 2024-01-01"
        );
    }

    #[test]
    fn absent_comment_is_none() {
        let s = scan_ok(HOSTILE);
        assert!(s.occurrences[1].comment.is_none());
    }

    #[test]
    fn edit_span_covers_value_and_comment() {
        let s = scan_ok(HOSTILE);
        assert_eq!(
            HOSTILE
                .get(s.occurrences[0].edit_span())
                .expect("edit span"),
            "actions/checkout@v4  # v4.0.0, 2024-01-01"
        );
        // With no comment, the edit stops at the value.
        assert_eq!(
            HOSTILE
                .get(s.occurrences[1].edit_span())
                .expect("edit span"),
            "'actions/setup-go@v5'"
        );
    }

    #[test]
    fn composite_action_steps_are_found() {
        let src = "\
name: thing
runs:
  using: composite
  steps:
    - uses: actions/checkout@v4
      with:
        uses: decoy
";
        let s = scan_ok(src);
        assert_eq!(s.occurrences.len(), 1);
        assert_eq!(s.occurrences[0].slot, Slot::CompositeStep);
    }

    #[test]
    fn an_alias_resolves_to_the_value_it_names() {
        let src = "\
jobs:
  a:
    steps:
      - uses: &co actions/checkout@v4
  b:
    steps:
      - uses: *co
";
        let s = scan_ok(src);
        assert!(s.unsupported.is_empty());
        assert_eq!(
            s.occurrences.len(),
            2,
            "both the anchor and the alias count"
        );

        let values: Vec<&str> = s.occurrences.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, ["actions/checkout@v4", "actions/checkout@v4"]);

        assert_eq!(s.occurrences[0].origin, Origin::Inline);
        assert_eq!(s.occurrences[0].line, 4);
        assert_eq!(s.occurrences[1].origin, Origin::Anchored { line: 4 });
        assert_eq!(s.occurrences[1].line, 7, "reported where it was written");
    }

    /// An edit can only land on the anchor, so both occurrences must address it.
    #[test]
    fn an_alias_is_rewritten_at_its_anchor() {
        let src = "\
jobs:
  a:
    steps:
      - uses: &co actions/checkout@v4
  b:
    steps:
      - uses: *co
";
        let s = scan_ok(src);
        assert_eq!(s.occurrences[0].span, s.occurrences[1].span);
        assert_eq!(sliced(src, &s.occurrences[1]), "actions/checkout@v4");
    }

    /// The bypass this resolution exists to close: with the anchor under a key no
    /// pinner inspects, the alias was the only sign a third-party action was in use,
    /// and it was reported as unrewritable rather than checked.
    #[test]
    fn an_anchor_outside_a_pinnable_slot_is_still_found() {
        let src = "\
x-shared: &co actions/checkout@v4
jobs:
  a:
    steps:
      - uses: *co
";
        let s = scan_ok(src);
        assert_eq!(s.occurrences.len(), 1);
        assert_eq!(s.occurrences[0].value, "actions/checkout@v4");
        assert_eq!(s.occurrences[0].origin, Origin::Anchored { line: 1 });
        assert_eq!(sliced(src, &s.occurrences[0]), "actions/checkout@v4");
    }

    /// A merge key splices a mapping's keys into the one around it, so a `uses:`
    /// inside the anchor lands in the step without ever being written there.
    #[test]
    fn a_merge_key_contributes_its_uses() {
        let src = "\
x-defaults: &d
  uses: actions/checkout@v4
jobs:
  a:
    steps:
      - <<: *d
        with:
          fetch-depth: 0
";
        let s = scan_ok(src);
        assert_eq!(s.occurrences.len(), 1);
        assert_eq!(s.occurrences[0].value, "actions/checkout@v4");
        assert_eq!(s.occurrences[0].origin, Origin::Anchored { line: 2 });
        assert_eq!(sliced(src, &s.occurrences[0]), "actions/checkout@v4");
    }

    /// The merged mapping must not shift the path, or `uses:` sits a level too deep.
    #[test]
    fn a_merge_key_does_not_disturb_the_surrounding_path() {
        let src = "\
x-defaults: &d
  uses: actions/checkout@v4
jobs:
  a:
    steps:
      - <<: *d
";
        let s = scan_ok(src);
        assert_eq!(s.occurrences[0].slot, Slot::Step);
    }

    /// Anchors do not cross a document boundary, so resolving one would invent a
    /// reference no forge would honour.
    #[test]
    fn an_anchor_does_not_reach_the_next_document() {
        let src = "\
jobs:
  a:
    steps:
      - uses: &co actions/checkout@v4
---
jobs:
  b:
    steps:
      - uses: *co
";
        let s = scan_ok(src);
        assert_eq!(s.occurrences.len(), 1);
        assert_eq!(s.unsupported.len(), 1);
        assert!(s.unsupported[0].reason.contains("not defined"));
    }

    #[test]
    fn an_alias_to_nothing_is_reported() {
        let src = "jobs:\n  a:\n    steps:\n      - uses: *nope\n";
        // saphyr rejects an undefined anchor outright; either way it must not be a
        // silent pass, which is the only property worth pinning down here.
        if let Ok(s) = scan(src) {
            assert!(!s.unsupported.is_empty());
        }
    }

    /// Aliases nested inside aliases must terminate rather than blow the stack.
    #[test]
    fn a_self_referential_anchor_does_not_hang() {
        let src = "jobs: &a\n  a:\n    steps:\n      - uses: *a\n";
        let _ = scan(src);
    }

    #[test]
    fn anchored_span_excludes_the_anchor_name() {
        let src = "jobs:\n  a:\n    steps:\n      - uses: &co actions/checkout@v4\n";
        let s = scan_ok(src);
        assert_eq!(sliced(src, &s.occurrences[0]), "actions/checkout@v4");
    }

    #[test]
    fn block_scalar_value_is_reported_not_rewritten() {
        let src = "jobs:\n  a:\n    steps:\n      - uses: >-\n          actions/checkout@v4\n";
        let s = scan_ok(src);
        assert!(s.occurrences.is_empty());
        assert_eq!(s.unsupported.len(), 1);
        assert!(s.unsupported[0].reason.contains("block scalar"));
    }

    #[test]
    fn hash_inside_the_value_is_not_a_comment() {
        let src = "jobs:\n  a:\n    steps:\n      - uses: 'a/b@v1#frag'\n";
        let s = scan_ok(src);
        assert!(s.occurrences[0].comment.is_none());
    }

    #[test]
    fn line_numbers_are_one_indexed() {
        let s = scan_ok(HOSTILE);
        assert_eq!(s.occurrences[0].line, 8);
    }

    #[test]
    fn malformed_yaml_is_an_error() {
        assert!(scan("jobs:\n  - [unclosed\n").is_err());
    }

    #[test]
    fn empty_document_is_fine() {
        assert_eq!(scan_ok(""), Scan::default());
    }
}
