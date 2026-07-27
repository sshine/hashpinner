Check, pin and bump SHA-pinned GitHub/Forgejo Actions references.

An unpinned reference such as `uses: actions/checkout@v4` names a mutable tag, so
whoever controls the upstream repository controls what runs in your CI, with your
secrets. Pinning to a commit fixes that, but a bare 40-character hex string is
unreviewable, so the commit is annotated with what it actually is:

```
uses: actions/checkout@8e8c483db84b4bee98b60c0593521ed34d9990e8 # v6.0.1, 2025-12-02
```

`hashpinner` maintains that form: it lists references, checks them, pins the
unpinned ones, bumps the pinned ones, and verifies that the comments are telling
the truth.

### Installation

```
nix run github:nix-tools/hashpinner
cargo install hashpinner-cli
```

`git` must be on `PATH`; hashpinner drives it to resolve tags. The Nix package
wraps the binary so this is already taken care of.

### Modes

One mode, optionally combined with `--deep`. The default is `--list`.

```
hashpinner                       list every reference and what it points at
hashpinner --check               fail if anything is unpinned      (offline)
hashpinner --check --bump        ...and fail if any pin is stale
hashpinner --check --deep        ...and verify pins and comments
hashpinner --pin                 pin the unpinned, repair comments
hashpinner --bump                move pins onto their latest release
hashpinner --pin --bump          both
```

`--check` never writes. `--pin` and `--bump` do, unless `--dry-run` is given.

With no path, hashpinner scans whichever of `.forgejo/workflows`,
`.gitea/workflows` and `.github/workflows` exist, plus a root `action.yml`.
Otherwise it scans the files and directories named.

### What each level costs

The three levels nest, and each is worth what it costs:

| | network | catches |
|---|---|---|
| `--check` | none | unpinned refs, mutable `docker://` tags |
| `--check --bump` | tags, shallow | stale pins |
| `--check --deep` | full commit graph | nonexistent pins, fork-injected pins, lying comments |

`--deep` checks reachability rather than existence, because on GitHub a fork
shares its object store with the upstream repository: a commit pushed to any
public fork can be fetched from the upstream URL even though it was never merged.
Existence therefore proves nothing. A commit reachable from no ref at all is what
a fork-injected pin looks like, and `--deep` fails on it.

`--deep` also compares each comment against the tag the commit really carries.
Reviewers read `# v6.0.1`, not the hex beside it, so a pin whose comment
misdescribes it passes every syntactic check and sails through review.

### The allowlist

`--allow` marks actions that need not be pinned, defaulting to `actions/*`.
It relaxes `--check` only: `--pin` still pins an allowlisted action and `--bump`
still bumps it. `--no-allow` empties it, so every unpinned reference fails.

```
hashpinner --check --no-allow          strict: everything must be pinned
hashpinner --check --allow 'actions/*' --allow 'nix-community/*'
```

### Forgejo

A bare `owner/repo` does not mean the same thing on both forges. Under
`.github/` it is github.com; under `.forgejo/` it resolves against the instance's
`DEFAULT_ACTIONS_URL`, which Forgejo defaults to `https://data.forgejo.org` — a
different repository, with different commit ids. hashpinner takes the host from
the directory the file is in; `--forgejo-host` overrides it.

One consequence is worth stating plainly: a repository mirrored to both forges
cannot share a pinned workflow file, because the correct commit differs.

Forgejo also reads only the *first* of `.forgejo/workflows`, `.gitea/workflows`
and `.github/workflows` that exists, silently ignoring the others. hashpinner
scans all of them and warns when more than one is present.

### What is not pinned

- **`docker://` references** are pinnable by digest but not by anything git
  knows. A mutable tag fails `--check`; `image@sha256:...` passes. Neither is
  ever rewritten.
- **Local actions** (`./path`) never fail: they live in this repository and are
  covered by the same review as the rest of it. hashpinner scans their
  `action.yml` for third-party references, which is what makes that safe.
- **YAML aliases** (`uses: *anchor`) are reported and left alone; pin the anchor.

One hazard sits outside a pinner's remit and is worth knowing anyway: a workflow
triggered by `pull_request_target` that checks out the pull request's head and
*then* invokes a local action is running attacker-controlled code with secrets.
No amount of pinning helps there.

### Self-hosting

To run this from your own Forgejo, mirror the repository and reference it by
absolute URL, which Forgejo accepts and GitHub does not:

```
uses: https://git.example.com/nix-tools/hashpinner@<sha>   # Forgejo
uses: nix-tools/hashpinner@<sha>                           # GitHub
```

Release binaries are always fetched from the GitHub release. On an instance with
no route to github.com, set the action's `base-url` input to an internal mirror.
