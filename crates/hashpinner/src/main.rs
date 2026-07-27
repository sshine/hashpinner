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
//! - **Local actions** (`./path`) never fail: they live in this repository and are
//!   covered by the same review as the rest of it. hashpinner scans their
//!   `action.yml` for third-party references, which is what makes that safe.
//! - **YAML aliases** (`uses: *anchor`) are reported and left alone; pin the anchor.
//!
//! One hazard sits outside a pinner's remit and is worth knowing anyway: a workflow
//! triggered by `pull_request_target` that checks out the pull request's head and
//! *then* invokes a local action is running attacker-controlled code with secrets.
//! No amount of pinning helps there.

mod cli;

use std::process::ExitCode;

use clap::Parser;
use hashpinner::git::GitResolver;
use hashpinner::pattern::Pattern;
use hashpinner::rewrite::{self, Options};
use hashpinner::{Error, Result};
use owo_colors::OwoColorize;

use cli::args::{Args, Format};
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

    let mut reports = Vec::new();
    for target in &targets {
        let path = target.path.display().to_string();

        // A file that cannot be read or parsed is reported and skipped, never fatal:
        // one malformed workflow must not stop the others from being fixed.
        let outcome = match rewrite::process_path(&target.path, target.forge, &resolver, &opts) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("{}: {path}: {e}", "error".red().bold());
                reports.push(FileReport {
                    path,
                    outcome: rewrite::Outcome {
                        entries: Vec::new(),
                        rewritten: None,
                    },
                    written: false,
                });
                continue;
            }
        };

        let written = match (&outcome.rewritten, args.writes()) {
            (Some(new), true) => {
                std::fs::write(&target.path, new).map_err(Error::Io)?;
                true
            }
            _ => false,
        };

        reports.push(FileReport {
            path,
            outcome,
            written,
        });
    }

    Ok(match args.format {
        Format::Text => report::text(&reports, &warnings, args.quiet),
        Format::Json => report::json(&reports, &warnings),
    })
}
