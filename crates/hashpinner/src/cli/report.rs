//! Rendering results, in text for people and JSON for scripts.

use hashpinner::rewrite::{Entry, Finding, Forge, Level, Outcome};
use owo_colors::OwoColorize;

/// One file's results, kept alongside the path they came from.
pub struct FileReport {
    /// The file, as given on the command line or discovered.
    pub path: String,
    /// Which forge's rules were applied, which a second referrer may contradict.
    pub forge: Forge,
    /// What was found in it.
    pub outcome: Outcome,
    /// Whether the file was rewritten on disk.
    pub written: bool,
}

/// Render everything as text on stdout, and return whether anything failed.
pub fn text(reports: &[FileReport], warnings: &[String], quiet: bool) -> bool {
    for warning in warnings {
        eprintln!("{}: {warning}", "warning".yellow().bold());
    }

    let mut failed = false;
    for report in reports {
        let shown: Vec<&Entry> = report
            .outcome
            .entries
            .iter()
            .filter(|e| !quiet || e.level() == Level::Fail)
            .collect();
        let findings: Vec<&Finding> = report
            .outcome
            .findings
            .iter()
            .filter(|f| !quiet || f.level == Level::Fail)
            .collect();

        if shown.is_empty() && findings.is_empty() {
            continue;
        }

        println!("{}", report.path.bold());
        for finding in findings {
            if finding.level == Level::Fail {
                failed = true;
            }
            print_finding(finding);
        }
        for entry in shown {
            if entry.level() == Level::Fail {
                failed = true;
            }
            print_entry(entry);
        }
        if report.written {
            println!("  {}", "written".green());
        }
        println!();
    }

    // A failure in a file whose entries were all filtered out still counts.
    failed || reports.iter().any(|r| r.outcome.failed())
}

/// Something about the file rather than about one reference in it.
fn print_finding(finding: &Finding) {
    let location = match finding.line {
        Some(line) => format!("L{line}"),
        None => String::new(),
    };
    println!(
        "  {}  {:>4}  {}",
        marker(finding.level),
        location.dimmed(),
        finding.message
    );
}

/// One reference and everything to say about it.
fn print_entry(entry: &Entry) {
    println!(
        "  {}  {:>4}  {}",
        marker(entry.level()),
        format!("L{}", entry.line).dimmed(),
        describe(entry)
    );

    for note in &entry.notes {
        let bullet = match note.level {
            Level::Fail => "×".red().to_string(),
            Level::Warn => "!".yellow().to_string(),
            Level::Info => "·".dimmed().to_string(),
        };
        println!("           {bullet} {}", note.message);
    }
}

/// The severity marker, right-aligned in four columns.
///
/// Colour codes have no width but `{:>4}` counts them, so the padding is applied
/// before the colour.
fn marker(level: Level) -> String {
    let (label, width) = match level {
        Level::Fail => ("FAIL", 4),
        Level::Warn => ("warn", 4),
        Level::Info => ("ok", 2),
    };
    let pad = " ".repeat(4 - width);
    let coloured = match level {
        Level::Fail => label.red().bold().to_string(),
        Level::Warn => label.yellow().to_string(),
        Level::Info => label.green().to_string(),
    };
    format!("{pad}{coloured}")
}

/// The one-line summary: what the action is, what it points at, what it claims to be.
fn describe(entry: &Entry) -> String {
    if entry.value.is_empty() {
        return "(unrewritable)".dimmed().to_string();
    }

    let path = entry
        .value
        .split_once('@')
        .map_or(entry.value.as_str(), |(p, _)| p);

    let mut line = format!("{path}  {}", abbreviate(&entry.git_ref).bold());
    if let Some(comment) = &entry.comment {
        line.push_str(&format!("  {}", comment.dimmed()));
    }
    line
}

/// Show a commit id at review length, leaving tags and branches intact.
fn abbreviate(git_ref: &str) -> String {
    if git_ref.len() == 40 && git_ref.chars().all(|c| c.is_ascii_hexdigit()) {
        git_ref[..7].to_string()
    } else {
        git_ref.to_string()
    }
}

/// Render everything as one JSON object on stdout.
pub fn json(reports: &[FileReport], warnings: &[String]) -> bool {
    let files: Vec<serde_json::Value> = reports
        .iter()
        .map(|r| {
            serde_json::json!({
                "path": r.path,
                "forge": match r.forge {
                    Forge::GitHub => "github",
                    Forge::Forgejo => "forgejo",
                },
                "written": r.written,
                "findings": r.outcome.findings.iter().map(|f| serde_json::json!({
                    "line": f.line,
                    "level": level_name(f.level),
                    "message": f.message,
                })).collect::<Vec<_>>(),
                "entries": r.outcome.entries.iter().map(|e| serde_json::json!({
                    "line": e.line,
                    "value": e.value,
                    "ref": e.git_ref,
                    "comment": e.comment,
                    "level": level_name(e.level()),
                    "notes": e.notes.iter().map(|n| serde_json::json!({
                        "level": level_name(n.level),
                        "message": n.message,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    let failed = reports.iter().any(|r| r.outcome.failed());
    let doc = serde_json::json!({
        "failed": failed,
        "warnings": warnings,
        "files": files,
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
    failed
}

/// Stable lowercase names, so scripts do not depend on Debug formatting.
fn level_name(level: Level) -> &'static str {
    match level {
        Level::Info => "info",
        Level::Warn => "warn",
        Level::Fail => "fail",
    }
}
