# Publishing

How a release reaches its users, and what each copy promises.

A release is published twice, from the same tag:

| Where | Built by | What it is for |
|---|---|---|
| GitHub release, crates.io, npm | `ci.yml`, on the `v<version>` tag | The editor extensions (`surrealql-language-server-<platform>` assets), `cargo install`, the browser WASM package, `builtins.json` |
| `download.surrealdb.com/cli/lsp/` | `publish-lsp.yml`, dispatched by hand | The `surreal` launcher: `surreal install lsp`, then `surreal lsp` |

The first is unchanged by the second. The extensions keep finding the server
exactly as they do today; moving them to the launcher is a separate decision.

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
2. Release `v<version>` as the README's release checklist describes. `ci.yml`
   builds the GitHub assets, the crate and the npm package from the tag, and
   `notify-consumers.yml` tells the extensions.
3. Dispatch **Publish LSP** on the tag `v<version>` with **`publish: false`**.
   This is a full rehearsal: all five targets build, each binary is checked for
   its `--version` line and serves one silent session, the Linux builds start
   inside `debian:12`, every asset is re-verified against its sidecar, and the
   exact list of keys a real publish would write is printed. No credential
   reaches that run.
4. Read the key list against the layout below.
5. Dispatch again on the same tag with **`publish: true`**, and `latest: true`
   when this version should become what `surreal install lsp` installs. A
   reviewer of the `release` environment approves the publish job.

```bash
gh workflow run publish-lsp.yml --ref v0.7.0 -f publish=false -f latest=false
gh workflow run publish-lsp.yml --ref v0.7.0 -f publish=true -f latest=true
```

A real publish refuses to run from anything but the tag `v<version>`, where
`<version>` is what `Cargo.toml` says at that tag, so the launcher's copy is
always built from the commit the GitHub release came from.

### Inputs

| Input | Default | What it does |
|---|---|---|
| `publish` | `false` | Upload anything at all. `false` is the rehearsal. |
| `latest` | `false` | Move `latest.txt` to this version. Ignored for a pre-release. |

`latest` is a separate decision from `publish`: writing the stable pointer *is*
offering the release to every launcher, so a build can be published and sat on
before it becomes what a new install gets.

There is no `version` input. The version is `Cargo.toml`'s, which is also what
the binary reports, and the launcher refuses an install whose `--version` names
a different release than the pointer did.

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
layout here, and a copy of the launcher repository's. CI runs it on every push.

**Order matters:** assets, then sidecars, then pointers. A client that reads a
pointer must find the files it names already in place. Every mutable key gets
`Cache-Control: no-store`; the versioned keys are immutable and get none.

### Pointer gates

| Pointer | Written when |
|---|---|
| `latest.txt` | `publish && !pre-release && latest` |
| `v<major>.txt` | `publish && !pre-release` |
| `beta.txt` / `alpha.txt` | `publish && pre-release`, the version's channel |

The channel is derived from the version: no `-` is stable, an `alpha` first
pre-release identifier is alpha, and anything else with a `-` is beta.

### Republish protection

A version is published at most once. Before anything builds, the workflow asks
for the version's five `.txt` sidecars and refuses if any answers 200 (`403`
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

`darwin-amd64` is new: the GitHub release has never had an Intel macOS build.

`ubuntu-22.04` sets the glibc floor at 2.35, and the Linux builds are started
inside `debian:12` (2.36) to prove it. Windows links the C runtime statically,
so the bare `.exe` starts on a machine without the VC++ redistributable.

The grammar revision comes from `scripts/setup-grammar.sh`. `ci.yml` pins the
same revision separately; bump both together.

---

## Security and prerequisites

The repository is public, so the bucket's credentials live in a GitHub
**environment**, not in repository or organisation secrets, and only the one
job that uploads names that environment. That job runs no compiler: the builds
run `build.rs` for the whole dependency graph, which is arbitrary code from
crates.io, and hold no credential. A rehearsal passes no credential at all.

Before the first publish, a repository admin has to set up the following.
None of it can be checked by a rehearsal.

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

A rehearsal cannot detect a policy that is too narrow; the first real publish
will, part-way through its uploads, and leaves the version needing its partial
keys deleted (see republish protection above).

Moving to OIDC, with the workflow assuming a role instead of holding a key,
removes the long-lived secret altogether and is worth doing. It is left for the
house's release pipelines to move together, as `surrealctl`'s own publishing
notes record.
