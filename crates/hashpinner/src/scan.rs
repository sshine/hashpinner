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
//!    be caught separately or it vanishes.
//!
//! [`Marker::index`]: saphyr_parser::Marker::index

use std::ops::Range;

use saphyr_parser::{Event, Parser, ScalarStyle};

use crate::{Error, Result};

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
    pub span: Range<usize>,
    /// The trailing comment, if the line has one.
    pub comment: Option<Comment>,
    /// 1-indexed line, for diagnostics.
    pub line: usize,
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
    },
    Sequence,
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
    let char_to_byte = char_to_byte_table(src);
    let mut out = Scan::default();
    let mut frames: Vec<Frame> = Vec::new();
    let mut path: Vec<Seg> = Vec::new();

    for event in Parser::new_from_str(src) {
        let (event, span) = event.map_err(|e| Error::Yaml(e.to_string()))?;
        let span = byte_span(&char_to_byte, src, span);

        match event {
            Event::MappingStart(..) => {
                path.push(entering_segment(&frames));
                frames.push(Frame::Mapping {
                    expect_key: true,
                    current_key: None,
                });
            }

            Event::SequenceStart(..) => {
                path.push(entering_segment(&frames));
                frames.push(Frame::Sequence);
            }

            Event::MappingEnd | Event::SequenceEnd => {
                frames.pop();
                path.pop();
                finish_value(&mut frames);
            }

            Event::Scalar(value, style, ..) => {
                if let Some(Frame::Mapping {
                    expect_key: expect_key @ true,
                    current_key,
                }) = frames.last_mut()
                {
                    *expect_key = false;
                    *current_key = Some(value.into_owned());
                    continue;
                }

                if let Some(slot) = pinnable_slot(&frames, &path) {
                    match quoting_of(style) {
                        Some(quoting) => out.occurrences.push(Occurrence {
                            slot,
                            value: value.into_owned(),
                            quoting,
                            comment: trailing_comment(src, span.end),
                            line: line_of(src, span.start),
                            span,
                        }),
                        None => out.unsupported.push(Unsupported {
                            reason: "block scalar values are not rewritten".to_string(),
                            line: line_of(src, span.start),
                        }),
                    }
                }

                finish_value(&mut frames);
            }

            Event::Alias(..) => {
                if pinnable_slot(&frames, &path).is_some() {
                    out.unsupported.push(Unsupported {
                        reason: "YAML alias; pin the anchor it refers to instead".to_string(),
                        line: line_of(src, span.start),
                    });
                }
                finish_value(&mut frames);
            }

            _ => {}
        }
    }

    Ok(out)
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
    fn alias_is_reported_not_silently_dropped() {
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
        assert_eq!(s.occurrences.len(), 1, "the anchored value is still a use");
        assert_eq!(s.occurrences[0].value, "actions/checkout@v4");
        assert_eq!(s.unsupported.len(), 1);
        assert!(s.unsupported[0].reason.contains("alias"));
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
