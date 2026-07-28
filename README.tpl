# {{crate}}

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
that the version in the comment is correct.

## Installation

```
nix run github:sshine/hashpinner
nix profile install github:sshine/hashpinner
cargo install hashpinner
```

You can download prebuilt [static binaries for x86_64 and aarch64 Linux][releases].

[releases]: https://github.com/sshine/hashpinner/releases

`git` must be on `PATH`; hashpinner drives it to resolve tags.

The Nix package wraps the binary so this is already taken care of.

## CLI

{{readme}}

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
