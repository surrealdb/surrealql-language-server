# Publishing

How a release reaches its users, and what each copy promises.

Publishing a GitHub release runs `publish.yml` on its tag, and that one run
publishes every copy of it:

| Where | What it is for |
|---|---|
| GitHub release | The editor extensions (`surrealql-language-server-<platform>` assets), `builtins.json`, the npm `.tgz` |
| crates.io, npm | `cargo install`, the browser WASM package |
| `download.surrealdb.com/cli/lsp/` | The `surreal` launcher: `surreal install lsp`, then `surreal lsp stdio` |

Each target is compiled once, and that build yields both the GitHub release
asset and the launcher's archive, so the two copies cannot drift. Nothing is
published until `check.yml`, the same formatting, tests and packaging check
every push gets, has passed on the tagged commit.

The extensions keep finding the server exactly as they always have; moving
them to the launcher is a separate decision.

## The executables

The crate builds one program under two names:

- **`surreal-lsp`**, the executable the launcher runs as `surreal lsp`, and the
  one published under `cli/lsp/`.
- **`surrealql-language-server`**, the same program under the name the editor
  extensions look for on `PATH` and download from GitHub releases. It stays
  until every extension has moved.

Neither says anything about the other. Serving over stdio writes the protocol
on stdout and nothing at all on stderr, under either name and however it was
started; `tests/stdio.rs` pins that byte for byte, and the release smoke test
checks it again on every target.

---

## Cutting a release

1. Bump `version` in `Cargo.toml`, run `cargo build` so `Cargo.lock` follows,
   and merge.
2. Publish a GitHub release whose tag is `v<version>`, as the README's release
   checklist describes. Tick *Set as latest release* when this version should
   become what `surreal install lsp` installs; for a backport, leave it off.
3. `publish.yml` runs on the tag. A reviewer of the `release` environment
   approves its *Publish to download.surrealdb.com* job once the builds are
   green; everything else proceeds without approval.
4. `notify-consumers.yml` tells the extensions once the four GitHub assets are
   on the release.

The run refuses to publish anything if the tag is not `v<version>`, where
`<version>` is what `Cargo.toml` says at that tag, so every copy of a release
is built from the commit it names.

### Which pointers move

There are no inputs. The pointers under `cli/lsp/` follow the release:

| Pointer | Moved when |
|---|---|
| `latest.txt` | Stable, and GitHub marks this release as the latest |
| `v<major>.txt` | Stable, and newer than the version it names now (or it does not exist) |
| `beta.txt` / `alpha.txt` | A pre-release, on the version's channel |

`latest.txt` follows GitHub's own flag because writing it *is* offering the
release to every launcher: a backport published without *Set as latest
release* leaves new installs on the newer line.

The channel is derived from the version: no `-` is stable, an `alpha` first
pre-release identifier is alpha, and anything else with a `-` is beta.

---

## What gets written

The database's layout, under the prefix the launcher's registry gives the
`lsp` component, as every component of the `surreal` CLI is published:

```
download.surrealdb.com/cli/lsp/
  latest.txt                -> v0.7.0      mutable    no-store
  v0.txt                    -> v0.7.0      mutable    no-store
  beta.txt / alpha.txt      -> v0.8.0-beta.1, only for a pre-release
  v0.7.0/
    surreal-lsp-v0.7.0.linux-amd64.tgz     immutable  INTELLIGENT_TIERING
    surreal-lsp-v0.7.0.linux-amd64.txt
    surreal-lsp-v0.7.0.linux-arm64.tgz   + .txt
    surreal-lsp-v0.7.0.darwin-amd64.tgz  + .txt
    surreal-lsp-v0.7.0.darwin-arm64.tgz  + .txt
    surreal-lsp-v0.7.0.windows-amd64.exe + .txt
```

- Each `.tgz` holds exactly one member, `surreal-lsp`, at the archive root,
  mode 0755. Windows publishes the bare `.exe`.
- Each `.txt` is the asset's SHA-256, 64 lowercase hex characters. Its name
  replaces the asset's extension rather than appending to it.
- Nothing else is written beside a version. The launcher reads every component
  with one reader, so a file one component has and another does not is a
  special case in it.

`scripts/package.sh` is the only implementation of the archive and sidecar
layout here, and a copy of the launcher repository's. `check.yml` runs it on
every push.

**Order matters:** assets, then sidecars, then pointers. A client that reads a
pointer must find the files it names already in place. Every mutable key gets
`Cache-Control: no-store`; the versioned keys are immutable and get none.

### Republish protection

A version is published at most once. Before uploading anything, the publish
job asks for the version's five `.txt` sidecars and refuses if any answers 200 (`403`
and `404` both mean absent; anything else fails the run rather than guess). The
sidecars are uploaded after every asset, so one that answers is a download
somebody can already complete.

After an interrupted publish the version cannot be retried until its partial
`cli/lsp/v<version>/` keys are deleted by hand, or the version is bumped. No
pointer will have moved, because pointers are written last.

### Propagation

After the pointers, `propagate-lsp` copies `cli/lsp/` to each of the eleven
regional buckets, `download.<region>.surrealdb.com`, in parallel, and fails
unless a majority succeeds. Unlike the house `propagate-binaries`, it syncs only
`cli/lsp/` and never deletes, so the credential it needs is the narrow one
below.

---

## Build matrix

| Target | Runner | Asset |
|---|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` | `linux-amd64` |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` | `linux-arm64` |
| `aarch64-apple-darwin` | `macos-15` | `darwin-arm64` |
| `x86_64-apple-darwin` | `macos-15` (cross) | `darwin-amd64` |
| `x86_64-pc-windows-msvc` | `windows-2022` | `windows-amd64` |

Every leg but `darwin-amd64` also uploads `surrealql-language-server` as the
GitHub release asset for its platform (`linux-amd64`, `linux-arm64`,
`macos-arm64`, `windows-amd64.exe`). The GitHub release has never had an Intel
macOS build; only the launcher gets one.

`ubuntu-22.04` sets the glibc floor at 2.35, and the Linux builds are started
inside `debian:12` (2.36) to prove it. Windows links the C runtime statically,
so the bare `.exe` starts on a machine without the VC++ redistributable.

The grammar revision comes from `scripts/setup-grammar.sh`, which every
workflow calls.

---

## Security and prerequisites

The repository is public, so the bucket's credentials live in a GitHub
**environment**, not in repository or organisation secrets, and only the one
job that uploads names that environment. That job runs no compiler: the builds
run `build.rs` for the whole dependency graph, which is arbitrary code from
crates.io, and hold no credential.

Before the first publish, a repository admin has to set up the following.
None of it can be checked short of a real release.

1. **An environment named `release`**, exactly. A misspelt environment in a
   workflow is created empty rather than refused.
   - *Required reviewers*: the people who may release. Turn on *Prevent
     self-review* if the team is large enough for it.
   - *Deployment branches and tags*: **selected tags only, `v*`**. The workflow
     already refuses to publish from anything but the release's tag; this makes
     the environment refuse its secrets to any other ref as well.
   - Do not allow administrators to bypass the protection rules.
2. **Two environment secrets in `release`**, `AMAZON_ACCESS_KEY` and
   `AMAZON_SECRET_KEY`: the key pair of an IAM user (or role) dedicated to this
   repository. Not the bucket-wide key the other release pipelines use: a key
   kept in a public repository's settings should be able to write
   `cli/lsp/*` and nothing else.
3. **That identity's IAM policy**, allowing, and only on the `cli/lsp/` prefix:
   - `s3:PutObject` on `download.surrealdb.com/cli/lsp/*`;
   - `s3:GetObject` on `download.surrealdb.com/cli/lsp/*` and `s3:ListBucket`
     on `download.surrealdb.com` conditioned on the `cli/lsp/` prefix, for the
     sync's source;
   - `s3:PutObject` on `download.<region>.surrealdb.com/cli/lsp/*` and
     `s3:ListBucket` on each regional bucket conditioned on the same prefix,
     for each of the eleven regions in `propagate-lsp`.

Nothing short of a release detects a policy that is too narrow; the first one
will, part-way through its uploads, and leaves the version needing its partial
keys deleted (see republish protection above).

Moving to OIDC, with the workflow assuming a role instead of holding a key,
removes the long-lived secret altogether and is worth doing. It is left for the
house's release pipelines to move together, as `surrealctl`'s own publishing
notes record.
