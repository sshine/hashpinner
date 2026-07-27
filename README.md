# hashpinner

Check, pin and bump SHA-pinned GitHub/Forgejo Actions references.

An unpinned reference such as `uses: actions/checkout@v4` names a mutable tag, so
whoever controls the upstream repository controls what runs in your CI, with your
secrets. Pinning to a commit fixes that, but a bare 40-character hex string is
unreviewable, so the commit is annotated with what it actually is:

```
uses: actions/checkout@8e8c483db84b4bee98b60c0593521ed34d9990e8 # v6.0.1, 2025-12-02
```

hashpinner maintains that form, from the command line or as a CI action: it lists
references, checks them, pins the unpinned ones, bumps the pinned ones, and verifies
that the comments are telling the truth.

## Installation

```
nix run github:sshine/hashpinner
nix profile install github:sshine/hashpinner
cargo install hashpinner
```

Prebuilt static binaries for x86_64 and aarch64 Linux are attached to each
[release](https://github.com/sshine/hashpinner/releases).

`git` must be on `PATH`; hashpinner drives it to resolve tags. The Nix package wraps
the binary so this is already taken care of.

## CLI

The `hashpinner` command: list, check, pin and bump Actions references.

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

From there it follows every `uses: ./path` to the file it names and scans that
too, repeating until nothing new turns up. This reaches files no directory walk
would: a local action may live at any path, and on Forgejo so may a local
reusable workflow. Relative paths resolve against the repository root, which is
the working directory, and one that climbs out of it with `..` fails.

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
- **Local actions** (`./path`) are never pinned: they live in this repository
  and are covered by the same review as the rest of it. What makes that safe is
  that hashpinner follows them and pins what it finds inside, so a `./path` is
  not a way to launder an unpinned third-party action past `--check`. A local
  reference that resolves to nothing, or out of the repository, fails.

### Anchors, aliases and merge keys

A `uses:` does not have to be written where it is used:

```yaml
x-shared: &co actions/checkout@v4
jobs:
  a:
    steps:
      - uses: *co
      - <<: *step-defaults
```

Both forms are resolved, offline, against the document they appear in. This is
not a corner: GitHub gained anchor support in September 2025, so they are getting
more common, and Forgejo has always accepted merge keys. Before they were
resolved, an anchor under a key like `x-shared:` was a way past `--check`
entirely, because nothing looked at it.

A value reached this way is reported on the line that asked for it and rewritten
on the line that defines it, since the anchor is the only place an edit can go.
Several aliases onto one anchor therefore produce one edit, not several.

Anchors do not cross a document boundary, and neither does the resolution; an
alias naming an anchor from an earlier document is reported, not resolved.

One hazard sits outside a pinner's remit and is worth knowing anyway: a workflow
triggered by `pull_request_target` that checks out the pull request's head and
*then* invokes a local action is running attacker-controlled code with secrets.
No amount of pinning helps there.

## The CI Action

The same checks, as a composite action that runs unchanged on GitHub-hosted runners
and on Forgejo runners with either a `docker` or a `host` label:

```yaml
- uses: sshine/hashpinner@<sha>                      # GitHub
- uses: https://github.com/sshine/hashpinner@<sha>   # Forgejo, absolute URL
  with:
    version: v0.1.0
    mode: check
    deep: "true"
```

| input | default | meaning |
|---|---|---|
| `version` | *required* | Release tag to download and run. |
| `mode` | `check` | One of `list`, `check`, `pin`, `bump`. |
| `deep` | `false` | Add `--deep`. |
| `path` | *workflow dirs* | Files or directories to scan. |
| `allow` | `actions/*` | Allowlist patterns, whitespace-separated. Empty means strict. |
| `base-url` | GitHub releases | Where to fetch the release asset from. |

`version` is required and takes an explicit tag because there is no "latest" URL
that works on both forges: GitHub serves `/releases/latest/download/<asset>` and
Forgejo 404s on it. The action downloads the static musl binary for the runner's
architecture and verifies it against the `.sha256` sidecar before running it.

Pin the action itself, of course. hashpinner will do it for you.

### Self-hosting

To run this from your own Forgejo, mirror the repository and reference it by
absolute URL, which Forgejo accepts and GitHub does not. Release binaries are still
fetched from the GitHub release; on an instance with no route to github.com, mirror
the assets and point `base-url` at them:

```yaml
- uses: https://git.example.com/sshine/hashpinner@<sha>
  with:
    version: v0.1.0
    base-url: https://artifacts.example.com/hashpinner
```

The asset layout under `base-url` is `<base-url>/<version>/<asset>`, matching what
both forges serve.
