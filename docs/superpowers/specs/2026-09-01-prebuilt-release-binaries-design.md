# Prebuilt Release Binaries Design

**Issue:** `flpdf-ift.7`

## Context

The release automation now lives in `.github/workflows/release-plz.yml`.
Its `release` job publishes the lockstep crates and creates the canonical
`vX.Y.Z` tag and GitHub Release through a GitHub App token. The repository does
not yet attach CLI binaries to that Release.

The existing release-plz workflow creates the Release before the tag-triggered
binary workflow can run. Keeping the Release as a draft until all artifacts are
available prevents users from seeing a partially populated Release. The fulgur
repository already uses this tag-triggered pattern with the same six targets.

## Goals

- Build the `flpdf` CLI for all six required targets from the release tag.
- Attach one archive per target to the existing GitHub Release.
- Publish the Release only after all six builds and uploads succeed.
- Keep ordinary pushes to `main` on the existing release-plz path.
- Keep the implementation in the current `.github/workflows/release-plz.yml`.

## Non-goals

- Changing the Rust CLI or library build itself.
- Publishing package-manager-specific binaries.
- Removing `.github/release.yml`, which is a separate GitHub release-notes
  configuration file rather than the workflow being extended here.
- Creating repository tag-protection rules automatically; the required
  protection is documented for manual repository configuration.

## Design

### Release lifecycle

1. A push to `main` runs the existing `check-releases`, `release-pr`, and
   release-plz `release` jobs.
2. The canonical `flpdf` package sets `git_release_draft = true`, so the
   release-plz `release` job creates `vX.Y.Z` as a draft while still publishing
   the crates and creating the tag.
3. The same workflow also accepts `v*` tag pushes. The tag run executes only
   the new binary jobs; the existing main-push jobs remain skipped because
   `check-releases` rejects non-`main` refs.
4. `setup` validates the tag's SemVer suffix and exposes it as a job output.
5. `build-binaries` runs a six-entry matrix, archives the target binary, and
   uploads the archive as a per-target artifact.
6. `release-binaries` waits for every matrix entry, downloads all archives,
   uploads them to the existing `vX.Y.Z` Release with `gh release upload`, and
   clears the draft flag only after the upload succeeds.

The release-plz `release` job already uses a GitHub App token. That is required
so the tag push can trigger the tag path; a `GITHUB_TOKEN`-authored event would
be suppressed by GitHub's workflow loop-prevention rule.

### Target matrix and archive names

The matrix mirrors fulgur's proven build configuration:

| Target | Runner | Linker/tooling | Archive |
| --- | --- | --- | --- |
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | mold | `.tar.gz` |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` | musl-tools | `.tar.gz` |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | mold | `.tar.gz` |
| `aarch64-apple-darwin` | `macos-latest` | Rust target | `.tar.gz` |
| `x86_64-apple-darwin` | `macos-latest` | Rust target | `.tar.gz` |
| `x86_64-pc-windows-msvc` | `windows-latest` | `lld-link` | `.zip` |

Each build runs:

```text
cargo build --release --bin flpdf -p flpdf-cli --target <target>
```

The archive names are:

```text
flpdf-v<version>-<target>.tar.gz
flpdf-v<version>-x86_64-pc-windows-msvc.zip
```

The archive contains the target's `flpdf` executable. Artifact names are
`flpdf-<target>` and are merged into one download directory by the final job.

### Permissions and reruns

The workflow keeps the top-level `contents: read` permission. Build jobs only
need read access. `release-binaries` grants `contents: write`, creates the same
scoped GitHub App token used by the existing release job, and uses that token
for `gh release upload` and `gh release edit`.

Uploads use `--clobber` so rerunning a failed or interrupted tag workflow can
replace an archive without requiring manual asset cleanup. The final job keeps
the Release draft if any build or upload fails.

### Documentation and repository setup

`docs/RELEASE_SETUP.md` will replace its deferred-binaries section with the
implemented tag-triggered flow. It will document that the `flpdf` Release is
draft until the binary job completes and that a tag-protection ruleset must
restrict `v*` tag creation to the release App, preventing a direct tag push
from bypassing the approved release path.

## Acceptance mapping

- The six-entry matrix and `build-binaries` job are in
  `.github/workflows/release-plz.yml`.
- Each matrix entry produces and uploads a `.tar.gz` or `.zip` archive.
- The final `release-binaries` job uploads all archives to the matching GitHub
  Release and removes the draft flag only after successful upload.
- All archive names begin with `flpdf-v<version>-<target>`.

## Validation

Because this change is GitHub Actions configuration rather than Rust runtime
code, validation uses YAML parsing and structural assertions for the workflow,
plus the existing repository checks needed to ensure the release configuration
still parses and the workspace remains buildable. The target-specific builds
are executed by GitHub Actions on their respective runners.
