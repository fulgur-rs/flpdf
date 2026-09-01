# Prebuilt Release Binaries Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** Extend the release-plz automation so every canonical vX.Y.Z Release receives six platform archives before it is published.

**Architecture:** Keep ordinary main pushes on the existing release-plz path. Add a v* tag path to the same workflow: a setup job validates the tag, a six-entry matrix builds and archives flpdf, and a final release-binaries job uploads the archives to the draft Release and removes the draft flag. Configure release-plz to create the canonical Release as a draft and document the required tag protection.

**Tech Stack:** GitHub Actions YAML, release-plz TOML configuration, Rust stable toolchain, cargo build, tar, PowerShell Compress-Archive, actions/upload-artifact, actions/download-artifact, and GitHub CLI.

**Spec:** docs/superpowers/specs/2026-09-01-prebuilt-release-binaries-design.md

## Global Constraints

- Modify only release-plz.toml, .github/workflows/release-plz.yml, and docs/RELEASE_SETUP.md for the implementation.
- Preserve the current main push release detection and its App-token permissions.
- Use exactly these targets: x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu, aarch64-apple-darwin, x86_64-apple-darwin, and x86_64-pc-windows-msvc.
- Use ubuntu-latest plus mold for x86_64 GNU Linux, ubuntu-latest plus musl-tools for x86_64 musl Linux, ubuntu-24.04-arm plus mold for aarch64 GNU Linux, macos-latest for both Apple targets, and windows-latest plus lld-link for Windows.
- Archive names must begin with flpdf-v<version>-<target>; Unix targets use .tar.gz and Windows uses .zip.
- Build exactly cargo build --release --bin flpdf -p flpdf-cli --target <target> from the release tag.
- Use the existing pinned action SHAs where already present and the pinned artifact action SHAs used by the repository's CI/fulgur release workflow.
- Configuration files are the TDD exception approved in the design: validate syntax and required structure after each edit instead of adding Rust unit tests for YAML/TOML.

---

### Task 1: Configure draft Releases and tag execution

**Files:**
- Modify: release-plz.toml:25-32
- Modify: .github/workflows/release-plz.yml:21-28
- Test: TOML/YAML syntax checks

**Interfaces:**
- Consumes: the existing package block for flpdf and the existing on.push.branches trigger.
- Produces: a draft canonical v{{ version }} Release and a tag-triggered run for the binary jobs.

- [ ] **Step 1: Confirm the current anchors**

~~~bash
sed -n '50,72p' release-plz.toml
sed -n '14,30p' .github/workflows/release-plz.yml
~~~

Confirm that the flpdf package has git_release_enable = true and the workflow has a main branch filter.

- [ ] **Step 2: Make the canonical Release a draft**

In the flpdf package block, immediately after git_release_enable = true, add:

~~~toml
git_release_draft = true
~~~

Do not add it to flpdf-cli or the workspace block.

- [ ] **Step 3: Add the tag filter**

Change only the trigger portion to:

~~~yaml
on:
  push:
    branches:
      - main
    tags:
      - 'v*'
  workflow_dispatch:
~~~

Keep the existing workflow_dispatch trigger and all main-push jobs unchanged.

- [ ] **Step 4: Validate the edits**

~~~bash
python3 - <<'PY'
import tomllib
from pathlib import Path

config = tomllib.loads(Path("release-plz.toml").read_text())
flpdf = next(package for package in config["package"] if package["name"] == "flpdf")
assert flpdf["git_release_draft"] is True
PY
ruby -e 'require "yaml"; YAML.parse_file(".github/workflows/release-plz.yml")'
git diff --check
~~~

Expected: both parsers exit successfully and git diff --check emits no output.

- [ ] **Step 5: Commit the configuration boundary**

~~~bash
git add release-plz.toml .github/workflows/release-plz.yml
git commit -m "ci: keep release-plz GitHub releases as drafts"
~~~

### Task 2: Add tag setup and the six-target build matrix

**Files:**
- Modify: .github/workflows/release-plz.yml before check-releases
- Test: matrix structural assertions and YAML parsing

**Interfaces:**
- Consumes: github.ref_name, the v* push event, and the checked-out release tag.
- Produces: setup.outputs.version and one flpdf-<target> artifact per target.

- [ ] **Step 1: Add the tag-only setup job**

Insert before check-releases:

~~~yaml
  setup:
    name: Resolve release version
    if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')
    runs-on: ubuntu-latest
    outputs:
      version: ${{ steps.version.outputs.version }}
    steps:
      - name: Extract version from tag
        id: version
        run: |
          set -euo pipefail
          VERSION="${GITHUB_REF_NAME#v}"
          if ! printf '%s\n' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
            echo "::error::Invalid version from tag: $GITHUB_REF_NAME"
            exit 1
          fi
          echo "version=$VERSION" >> "$GITHUB_OUTPUT"
          echo "Releasing version: $VERSION"
~~~

The explicit event/ref guard prevents a manual or main-push run from entering the tag path.

- [ ] **Step 2: Add the exact matrix**

Insert after setup:

~~~yaml
  build-binaries:
    name: Build ${{ matrix.target }}
    if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')
    needs: setup
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
            archive: tar.gz
            rustflags: "-C link-arg=-fuse-ld=mold"
            install-mold: true
          - target: x86_64-unknown-linux-musl
            os: ubuntu-latest
            archive: tar.gz
            rustflags: ""
            install-mold: false
          - target: aarch64-unknown-linux-gnu
            os: ubuntu-24.04-arm
            archive: tar.gz
            rustflags: "-C link-arg=-fuse-ld=mold"
            install-mold: true
          - target: aarch64-apple-darwin
            os: macos-latest
            archive: tar.gz
            rustflags: ""
            install-mold: false
          - target: x86_64-apple-darwin
            os: macos-latest
            archive: tar.gz
            rustflags: ""
            install-mold: false
          - target: x86_64-pc-windows-msvc
            os: windows-latest
            archive: zip
            rustflags: "-C linker=lld-link"
            install-mold: false
~~~

- [ ] **Step 3: Add checkout, toolchain, build, package, and artifact steps**

Use the existing checkout, toolchain, and cache pins. The build and package steps must include:

~~~yaml
      - name: Build
        run: cargo build --release --bin flpdf -p flpdf-cli --target ${{ matrix.target }}

      - name: Package (unix)
        if: matrix.archive == 'tar.gz'
        run: |
          cd target/${{ matrix.target }}/release
          tar czf ../../../flpdf-v${{ needs.setup.outputs.version }}-${{ matrix.target }}.tar.gz flpdf
          cd ../../..

      - name: Package (windows)
        if: matrix.archive == 'zip'
        shell: pwsh
        run: Compress-Archive -Path "target/${{ matrix.target }}/release/flpdf.exe" -DestinationPath "flpdf-v${{ needs.setup.outputs.version }}-${{ matrix.target }}.zip"

      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
        with:
          name: flpdf-${{ matrix.target }}
          path: flpdf-v${{ needs.setup.outputs.version }}-${{ matrix.target }}.*
~~~

Install mold when matrix.install-mold is true, set RUSTFLAGS from matrix.rustflags when non-empty, and install musl-tools only for the musl target.

- [ ] **Step 4: Verify the matrix and artifact contract**

~~~bash
python3 - <<'PY'
from pathlib import Path

workflow = Path(".github/workflows/release-plz.yml").read_text()
for target in (
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
):
    assert workflow.count(f"target: {target}") == 1
assert "build-binaries:" in workflow
assert "cargo build --release --bin flpdf -p flpdf-cli --target" in workflow
assert "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a" in workflow
PY
ruby -e 'require "yaml"; YAML.parse_file(".github/workflows/release-plz.yml")'
git diff --check
~~~

- [ ] **Step 5: Commit the matrix job**

~~~bash
git add .github/workflows/release-plz.yml
git commit -m "ci: build release binaries for six targets"
~~~

### Task 3: Upload all archives and publish the Release

**Files:**
- Modify: .github/workflows/release-plz.yml after build-binaries
- Test: dependency, permission, and command-order assertions

**Interfaces:**
- Consumes: setup.outputs.version and all flpdf-* artifacts.
- Produces: assets on the existing v<version> Release and a non-draft Release only after upload succeeds.

- [ ] **Step 1: Add the final job**

Insert after build-binaries:

~~~yaml
  release-binaries:
    name: Publish GitHub Release binaries
    if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')
    needs: [setup, build-binaries]
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c
        with:
          path: artifacts
          pattern: flpdf-*
          merge-multiple: true

      - name: Generate App token for release publish
        id: app-token
        uses: actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1
        with:
          app-id: ${{ secrets.RELEASE_APP_ID }}
          private-key: ${{ secrets.RELEASE_APP_PRIVATE_KEY }}
          permission-contents: write

      - name: Upload binaries and publish release
        env:
          GH_TOKEN: ${{ steps.app-token.outputs.token }}
        run: |
          set -euo pipefail
          VERSION="${{ needs.setup.outputs.version }}"
          REPO="${{ github.repository }}"
          TAG="v$VERSION"
          if ! gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
            gh release create "$TAG" --title "$TAG" --notes "Release $TAG" --draft --repo "$REPO"
          fi
          gh release upload "$TAG" artifacts/* --clobber --repo "$REPO"
          gh release edit "$TAG" --draft=false --repo "$REPO"
~~~

gh release edit --draft=false must remain after gh release upload. The fallback creates a draft only if release-plz did not create one.

- [ ] **Step 2: Verify the final job**

~~~bash
python3 - <<'PY'
from pathlib import Path

workflow = Path(".github/workflows/release-plz.yml").read_text()
release_job = workflow[workflow.index("  release-binaries:\n"):]
assert "needs: [setup, build-binaries]" in release_job
assert "contents: write" in release_job
assert "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c" in release_job
assert "gh release upload" in release_job
assert "gh release edit" in release_job
assert release_job.index("gh release upload") < release_job.index("gh release edit")
PY
ruby -e 'require "yaml"; YAML.parse_file(".github/workflows/release-plz.yml")'
git diff --check
~~~

- [ ] **Step 3: Commit the release upload job**

~~~bash
git add .github/workflows/release-plz.yml
git commit -m "ci: attach binaries before publishing releases"
~~~

### Task 4: Document the implemented binary release flow

**Files:**
- Modify: docs/RELEASE_SETUP.md:126-132
- Test: Markdown content assertions and diff checks

**Interfaces:**
- Consumes: the final workflow trigger/job names and git_release_draft.
- Produces: operator documentation for the draft lifecycle, target archives, and tag protection.

- [ ] **Step 1: Replace the deferred section**

Replace ## Deferred: prebuilt binaries with:

~~~markdown
## Prebuilt binaries

The canonical flpdf Release is created as a draft by release-plz and carries the
vX.Y.Z tag. The tag push starts the binary path in
.github/workflows/release-plz.yml:

1. build-binaries builds the CLI for six targets and creates one
   flpdf-vX.Y.Z-<target>.tar.gz archive per Unix target or one
   flpdf-vX.Y.Z-x86_64-pc-windows-msvc.zip archive for Windows.
2. release-binaries uploads all six archives to the existing draft Release.
3. The job removes the draft flag only after every upload succeeds.

Configure a repository tag-protection ruleset that restricts creation of v*
tags to the RELEASE_APP. Without that rule, a direct tag push could start the
binary path without the approved release-plz publish flow.
~~~

- [ ] **Step 2: Verify documentation references**

~~~bash
python3 - <<'PY'
from pathlib import Path

docs = Path("docs/RELEASE_SETUP.md").read_text()
assert "## Prebuilt binaries" in docs
assert "build-binaries" in docs
assert "release-binaries" in docs
assert "tag-protection ruleset" in docs
assert "## Deferred: prebuilt binaries" not in docs
PY
git diff --check
~~~

- [ ] **Step 3: Commit the documentation**

~~~bash
git add docs/RELEASE_SETUP.md
git commit -m "docs: document prebuilt release binaries"
~~~

### Task 5: Run the complete verification set

**Files:**
- Test: all three implementation files and the Rust workspace

**Interfaces:**
- Consumes: Tasks 1-4.
- Produces: evidence that configuration syntax, acceptance structure, and workspace health remain valid.

- [ ] **Step 1: Run combined TOML/YAML/structure/docs validation**

~~~bash
python3 - <<'PY'
import tomllib
from pathlib import Path

config = tomllib.loads(Path("release-plz.toml").read_text())
flpdf = next(package for package in config["package"] if package["name"] == "flpdf")
assert flpdf["git_tag_name"] == "v{{ version }}"
assert flpdf["git_release_draft"] is True

workflow = Path(".github/workflows/release-plz.yml").read_text()
assert "      - 'v*'" in workflow
assert workflow.count("target: ") == 6
assert workflow.count("archive: tar.gz") == 5
assert workflow.count("archive: zip") == 1
assert "build-binaries:" in workflow
assert "release-binaries:" in workflow
assert "gh release upload" in workflow
assert "gh release edit" in workflow

docs = Path("docs/RELEASE_SETUP.md").read_text()
assert "## Prebuilt binaries" in docs
assert "tag-protection ruleset" in docs
PY
ruby -e 'require "yaml"; YAML.parse_file(".github/workflows/release-plz.yml")'
git diff --check
~~~

- [ ] **Step 2: Run repository checks**

~~~bash
cargo fmt --all -- --check
cargo build --workspace
~~~

The six cross-target builds are GitHub-runner checks because the local runner does not provide all required OS/target environments.

- [ ] **Step 3: Inspect the final diff**

~~~bash
git diff --stat origin/main...HEAD
git diff --check origin/main...HEAD
git status --short --branch
~~~

Confirm that only the design/plan documents plus the three scoped implementation files are present on the feature branch. Do not stage the main checkout's pre-existing untracked files.

- [ ] **Step 4: Close and persist the issue after all checks pass**

~~~bash
bd close flpdf-ift.7 --reason="Added six-target prebuilt binary release flow to release-plz workflow"
bd dolt push
~~~

Read back bd show flpdf-ift.7 and require Push complete. before handoff.
