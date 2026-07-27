// These docs are spliced into README.md as its CLI section; the sections around
// it are hand-written in README.tpl at the repository root.

//! The `hashpinner` command: list, check, pin and bump Actions references.
//!
//! ## Modes
//!
//! One mode, optionally combined with `--deep`. The default is `--list`.
//!
//! ```text
//! hashpinner                       list every reference and what it points at
//! hashpinner --check               fail if anything is unpinned      (offline)
//! hashpinner --check --bump        ...and fail if any pin is stale
//! hashpinner --check --deep        ...and verify pins and comments
//! hashpinner --pin                 pin the unpinned, repair comments
//! hashpinner --bump                move pins onto their latest release
//! hashpinner --pin --bump          both
//! ```
//!
//! `--check` never writes. `--pin` and `--bump` do, unless `--dry-run` is given.
//!
//! With no path, hashpinner scans whichever of `.forgejo/workflows`,
//! `.gitea/workflows` and `.github/workflows` exist, plus a root `action.yml`.
//! Otherwise it scans the files and directories named.
//!
//! From there it follows every `uses: ./path` to the file it names and scans that
//! too, repeating until nothing new turns up. This reaches files no directory walk
//! would: a local action may live at any path, and on Forgejo so may a local
//! reusable workflow. Relative paths resolve against the repository root, which is
//! the working directory, and one that climbs out of it with `..` fails.
//!
//! ## What each level costs
//!
//! The three levels nest, and each is worth what it costs:
//!
//! | | network | catches |
//! |---|---|---|
//! | `--check` | none | unpinned refs, mutable `docker://` tags |
//! | `--check --bump` | tags, shallow | stale pins |
//! | `--check --deep` | full commit graph | nonexistent pins, fork-injected pins, lying comments |
//!
//! `--deep` checks reachability rather than existence, because on GitHub a fork
//! shares its object store with the upstream repository: a commit pushed to any
//! public fork can be fetched from the upstream URL even though it was never merged.
//! Existence therefore proves nothing. A commit reachable from no ref at all is what
//! a fork-injected pin looks like, and `--deep` fails on it.
//!
//! `--deep` also compares each comment against the tag the commit really carries.
//! Reviewers read `# v6.0.1`, not the hex beside it, so a pin whose comment
//! misdescribes it passes every syntactic check and sails through review.
//!
//! ## The allowlist
//!
//! `--allow` marks actions that need not be pinned, defaulting to `actions/*`.
//! It relaxes `--check` only: `--pin` still pins an allowlisted action and `--bump`
//! still bumps it. `--no-allow` empties it, so every unpinned reference fails.
//!
//! ```text
//! hashpinner --check --no-allow          strict: everything must be pinned
//! hashpinner --check --allow 'actions/*' --allow 'nix-community/*'
//! ```
//!
//! ## Forgejo
//!
//! A bare `owner/repo` does not mean the same thing on both forges. Under
//! `.github/` it is github.com; under `.forgejo/` it resolves against the instance's
//! `DEFAULT_ACTIONS_URL`, which Forgejo defaults to `https://data.forgejo.org` — a
//! different repository, with different commit ids. hashpinner takes the host from
//! the directory the file is in; `--forgejo-host` overrides it.
//!
//! One consequence is worth stating plainly: a repository mirrored to both forges
//! cannot share a pinned workflow file, because the correct commit differs.
//!
//! Forgejo also reads only the *first* of `.forgejo/workflows`, `.gitea/workflows`
//! and `.github/workflows` that exists, silently ignoring the others. hashpinner
//! scans all of them and warns when more than one is present.
//!
//! ## What is not pinned
//!
//! - **`docker://` references** are pinnable by digest but not by anything git
//!   knows. A mutable tag fails `--check`; `image@sha256:...` passes. Neither is
//!   ever rewritten.
//! - **Local actions** (`./path`) are never pinned: they live in this repository
//!   and are covered by the same review as the rest of it. What makes that safe is
//!   that hashpinner follows them and pins what it finds inside, so a `./path` is
//!   not a way to launder an unpinned third-party action past `--check`. A local
//!   reference that resolves to nothing, or out of the repository, fails.
//!
//! ## Anchors, aliases and merge keys
//!
//! A `uses:` does not have to be written where it is used:
//!
//! ```yaml
//! x-shared: &co actions/checkout@v4
//! jobs:
//!   a:
//!     steps:
//!       - uses: *co
//!       - <<: *step-defaults
//! ```
//!
//! Both forms are resolved, offline, against the document they appear in. This is
//! not a corner: GitHub gained anchor support in September 2025, so they are getting
//! more common, and Forgejo has always accepted merge keys. Before they were
//! resolved, an anchor under a key like `x-shared:` was a way past `--check`
//! entirely, because nothing looked at it.
//!
//! A value reached this way is reported on the line that asked for it and rewritten
//! on the line that defines it, since the anchor is the only place an edit can go.
//! Several aliases onto one anchor therefore produce one edit, not several.
//!
//! Anchors do not cross a document boundary, and neither does the resolution; an
//! alias naming an anchor from an earlier document is reported, not resolved.
//!
//! One hazard sits outside a pinner's remit and is worth knowing anyway: a workflow
//! triggered by `pull_request_target` that checks out the pull request's head and
//! *then* invokes a local action is running attacker-controlled code with secrets.
//! No amount of pinning helps there.

mod cli;

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use hashpinner::git::GitResolver;
use hashpinner::local;
use hashpinner::pattern::Pattern;
use hashpinner::rewrite::{self, Entry, Finding, Forge, Level, Note, Options, Outcome};
use hashpinner::{Error, Result};
use owo_colors::OwoColorize;

use cli::args::{Args, Format};
use cli::discover::Target;
use cli::report::{self, FileReport};

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(false) => ExitCode::SUCCESS,
        // Exit 1 means "the files are not how you asked them to be", which is what a
        // CI gate wants; exit 2 is reserved for hashpinner itself being unable to run.
        Ok(true) => ExitCode::from(1),
        Err(e) => {
            eprintln!("{}: {e}", "error".red().bold());
            ExitCode::from(2)
        }
    }
}

/// Do the work, returning whether anything failed validation.
fn run(args: &Args) -> Result<bool> {
    let (targets, warnings) = cli::discover::discover(&args.paths)?;

    let opts = Options {
        check: args.check,
        pin: args.pin,
        bump: args.bump,
        deep: args.deep,
        allow: args
            .allow_patterns()
            .iter()
            .map(|p| Pattern::new(p))
            .collect(),
        forgejo_host: args.forgejo_host.clone(),
    };

    let resolver = GitResolver::with_default_cache()?.offline(args.offline);
    let root = std::env::current_dir().map_err(Error::Io)?;

    // Local references are followed as they are found, so the set of files to scan
    // grows during the walk: an action reached only from another action is exactly
    // the one a directory listing would miss.
    let mut walk = Walk::new(&targets);
    let mut reports: Vec<FileReport> = Vec::new();

    while let Some(target) = walk.queue.pop_front() {
        if walk.done.contains(&target.path) {
            continue;
        }
        walk.done.insert(target.path.clone());
        let index = reports.len();
        walk.report_of.insert(target.path.clone(), index);

        let path = target.path.display().to_string();

        // A file that cannot be read or parsed is reported and skipped, never fatal:
        // one malformed workflow must not stop the others from being fixed.
        let mut outcome = match rewrite::process_path(&target.path, target.forge, &resolver, &opts)
        {
            Ok(o) => o,
            Err(e) => {
                eprintln!("{}: {path}: {e}", "error".red().bold());
                reports.push(FileReport {
                    path,
                    forge: target.forge,
                    outcome: Outcome::default(),
                    written: false,
                });
                continue;
            }
        };

        follow_locals(&root, &target, &mut outcome, &mut walk);

        let written = match (&outcome.rewritten, args.writes()) {
            (Some(new), true) => {
                std::fs::write(&target.path, new).map_err(Error::Io)?;
                true
            }
            _ => false,
        };

        reports.push(FileReport {
            path,
            forge: target.forge,
            outcome,
            written,
        });
    }

    walk.report_conflicts(&mut reports);
    reports.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(match args.format {
        Format::Text => report::text(&reports, &warnings, args.quiet),
        Format::Json => report::json(&reports, &warnings),
    })
}

/// The state of the walk over reachable files.
struct Walk {
    /// Files still to process.
    queue: VecDeque<Target>,
    /// Files already processed, so a cycle terminates.
    done: HashSet<PathBuf>,
    /// The forge each file was first reached under.
    claimed: HashMap<PathBuf, Forge>,
    /// Files reached under both forges, which cannot be pinned at all.
    contested: BTreeSet<PathBuf>,
    /// Where each file's report ended up.
    report_of: HashMap<PathBuf, usize>,
}

impl Walk {
    fn new(targets: &[Target]) -> Self {
        let mut walk = Self {
            queue: VecDeque::new(),
            done: HashSet::new(),
            claimed: HashMap::new(),
            contested: BTreeSet::new(),
            report_of: HashMap::new(),
        };
        for target in targets {
            walk.enqueue(target.path.clone(), target.forge);
        }
        walk
    }

    /// Add a file to the walk, or record that a second referrer disagrees about its
    /// forge.
    ///
    /// The disagreement has to be caught here rather than when the file is popped:
    /// by then the file is already queued, and a second referrer would be dropped as
    /// a duplicate without its forge ever being compared.
    fn enqueue(&mut self, path: PathBuf, forge: Forge) {
        match self.claimed.get(&path) {
            Some(&first) if first != forge => {
                self.contested.insert(path);
            }
            Some(_) => {}
            None => {
                self.claimed.insert(path.clone(), forge);
                self.queue.push_back(Target { path, forge });
            }
        }
    }

    /// Attach a finding to every file that two forges disagreed about.
    ///
    /// Deferred to the end because the file is often queued before the referrer that
    /// contradicts it has been read, so its report does not exist yet.
    fn report_conflicts(&self, reports: &mut [FileReport]) {
        for path in &self.contested {
            let Some(&index) = self.report_of.get(path) else {
                continue;
            };
            reports[index].outcome.findings.push(Finding {
                level: Level::Fail,
                line: None,
                message: "reached from both a .github/ and a .forgejo/ workflow; a bare \
                          owner/repo means a different repository on each, so no single \
                          pin is correct for both"
                    .to_string(),
            });
        }
    }
}

/// Resolve each `uses: ./path` in `outcome`, queueing what it names.
///
/// The resolution verdict lands on the reference's own entry rather than on the
/// file, so that a `./path` pointing at nothing fails the line that wrote it.
fn follow_locals(root: &std::path::Path, target: &Target, outcome: &mut Outcome, walk: &mut Walk) {
    for local_use in std::mem::take(&mut outcome.locals) {
        let note = match local::resolve(root, &local_use.path, local_use.slot, target.forge) {
            Ok(found) => {
                let note = Note::info(format!("local action, scanning {}", found.display()));
                walk.enqueue(found, target.forge);
                note
            }
            Err(why) => Note::fail(format!("{}: {why}", local_use.path)),
        };

        if let Some(entry) = find_entry(&mut outcome.entries, local_use.line, &local_use.path) {
            entry.notes.push(note);
        }
    }
}

/// The entry a local reference came from, matched on where and what it was.
fn find_entry<'a>(entries: &'a mut [Entry], line: usize, value: &str) -> Option<&'a mut Entry> {
    entries
        .iter_mut()
        .find(|e| e.line == line && e.value == value)
}
