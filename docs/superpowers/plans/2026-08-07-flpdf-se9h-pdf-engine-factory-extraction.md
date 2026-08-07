# Pdf Engine Factory Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move every `Pdf` document-construction factory and its construction helper from `reader.rs` into the existing `engine.rs` module without changing public APIs, errors, document state, or output bytes.

**Architecture:** `Pdf<R>` remains the single document type in `pdf.rs`. `engine.rs` owns the `emptyPDF`/`processFile`/`processMemoryFile`-equivalent construction surface and the private state needed only while constructing a `Pdf`; `reader.rs` keeps resolution, recovery diagnostics, encryption/authentication implementation, and all resolve entry points. The move is a qpdf correspondence/container refactor, not a semantic change.

**Tech Stack:** Rust workspace, Cargo, Beads (`bd`), qpdf 11.9.0 correspondence docs, `cargo llvm-cov`, Git worktrees.

## Global Constraints

- Implement Bead `flpdf-se9h`; run `bd prime`, read the issue back, and claim it before source edits.
- Perform implementation in an isolated worktree created through `superpowers:using-git-worktrees`; preserve `/home/ubuntu/flpdf` main and its unrelated untracked files.
- Move only `open`, `open_with_repair`, `open_best_effort`, `open_with_options`, `open_mem`, `open_mem_with_options`, `open_mem_owned`, `open_mem_owned_with_options`, private `open_with_repair_mode`, `NEXT_PDF_ID`, and `MAX_RESOLUTION_FALLBACKS` into `engine.rs`; `Pdf::empty` is already there.
- Do not move `resolve_object_handle*`, `repair_diagnostics`, `push_warning`, encryption accessors, `authenticate_if_encrypted`, resolver primitives, or cache operations.
- Preserve every public signature, rustdoc contract, error variant/order, xref/recovery path, authentication call order, unique-ID allocation, buffer ownership property, and emitted byte.
- Keep qpdf 11.9.0 `QPDF.cc` `emptyPDF`/`processFile`/`processMemoryFile` as the responsibility oracle; do not introduce wrappers or a second construction path.
- Treat this as CLAUDE.md deviation class (B): record the container split in `engine.rs` and `docs/qpdf-correspondence.md`.
- Do not edit `AGENTS.md`, `.beads/issues.jsonl`, or unrelated files.
- Require 100% changed-line coverage for `crates/flpdf/src` against `origin/main`.

---

## File Map

- Modify `crates/flpdf/src/engine.rs`: own all nine factories, the shared construction helper, the two construction-only constants, and updated module-level qpdf correspondence.
- Modify `crates/flpdf/src/reader.rs`: remove moved definitions/imports; expose only the existing authentication implementation through a `pub(crate)` method so the sibling engine module can call it.
- Modify `docs/qpdf-correspondence.md`: attribute `emptyPDF`/`processFile`/`processMemoryFile` and the factory orchestration to `engine.rs`, while retaining remaining QPDF.cc responsibilities under `reader.rs` and the existing modules.
- Regenerate `docs/qpdf-module-doc-index.md`: reflect the updated `engine.rs` and `reader.rs` module annotations.
- Test existing `crates/flpdf/src/engine.rs` unit tests: canonical `Pdf::empty` behavior.
- Test existing `crates/flpdf/src/reader.rs` unit tests: `open_mem*`, buffer sharing, Cursor equivalence, repair options, encrypted opening, and resolver behavior remain unchanged.
- Test existing integration/CLI byte gates; do not add a source-inspection test that pins implementation text instead of behavior.

---

### Task 1: Relocate the complete Pdf factory dependency closure

**Files:**
- Modify: `crates/flpdf/src/engine.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Test: `crates/flpdf/src/engine.rs`
- Test: `crates/flpdf/src/reader.rs`

**Interfaces:**
- Consumes: `Pdf<R>` from `crate::Pdf`; `PdfOpenOptions` from `crate::reader`; `ResolverHandle` from `crate::reader::resolver`; `load_xref_state_with_repair`; `ObjectCache::from_offsets`; `ResolverHandle::new_shared`; existing `Pdf::authenticate_if_encrypted`.
- Produces: unchanged public signatures `Pdf::<R>::open*`, `Pdf<Cursor<Arc<[u8]>>>::open_mem*`, `Pdf<Cursor<Vec<u8>>>::open_mem_owned*`, and `Pdf<Cursor<Vec<u8>>>::empty` from the `engine` module's impl blocks.

- [ ] **Step 1: Recover and claim the Bead, then create the isolated worktree**

Run in `/home/ubuntu/flpdf`:

```bash
bd prime
bd show flpdf-se9h
bd update flpdf-se9h --claim
git worktree add /home/ubuntu/flpdf/.worktrees/flpdf-se9h-engine-factories -b refactor/flpdf-se9h-engine-factories main
```

Expected: `flpdf-se9h` is `in_progress`; the linked worktree is on `refactor/flpdf-se9h-engine-factories` at current `main`. If the worktree skill selects another ignored worktree root, use its reported absolute path consistently instead.

- [ ] **Step 2: Establish the behavior-preserving baseline**

Run in the new worktree:

```bash
cargo test -p flpdf --lib open_mem
cargo test -p flpdf --lib engine::tests
cargo test -p flpdf --test reader_tests
cargo test -p flpdf-cli --test cli_tests
```

Expected: all commands PASS before the move. Stop and diagnose any baseline failure rather than attributing it to this refactor.

- [ ] **Step 3: Move the public factory impls and construction helper unchanged**

Move these exact definitions, including their rustdoc, from `reader.rs` to `engine.rs`:

```rust
impl<R: Read + Seek> Pdf<R> {
    pub fn open(reader: R) -> Result<Self>;
    pub fn open_with_repair(reader: R) -> Result<Self>;
    pub fn open_best_effort(reader: R) -> Result<Self>;
    pub fn open_with_options(reader: R, options: PdfOpenOptions) -> Result<Self>;
    fn open_with_repair_mode(reader: R, options: PdfOpenOptions) -> Result<Self>;
}

impl Pdf<Cursor<Arc<[u8]>>> {
    pub fn open_mem(bytes: Arc<[u8]>) -> crate::Result<Self>;
    pub fn open_mem_with_options(
        bytes: Arc<[u8]>,
        options: PdfOpenOptions,
    ) -> crate::Result<Self>;
}

impl Pdf<Cursor<Vec<u8>>> {
    pub fn open_mem_owned(bytes: Vec<u8>) -> crate::Result<Self>;
    pub fn open_mem_owned_with_options(
        bytes: Vec<u8>,
        options: PdfOpenOptions,
    ) -> crate::Result<Self>;
}
```

Do not rewrite the bodies. Move these private values with `open_with_repair_mode`:

```rust
static NEXT_PDF_ID: AtomicU64 = AtomicU64::new(1);
const MAX_RESOLUTION_FALLBACKS: u32 = 64;
```

Leave `MAX_OBJECT_STREAM_CHAIN_DEPTH`, `READER_STACK_RED_ZONE`, and `READER_STACK_GROWTH_SIZE` in `reader.rs`; they belong to the unresolved reader/resolve slice.

- [ ] **Step 4: Run a compile check to expose the cross-module boundary (RED)**

Run:

```bash
cargo check -p flpdf
```

Expected: FAIL until `engine.rs` imports the factory dependency closure and `authenticate_if_encrypted` is callable from the sibling module. The expected privacy failure is an `E0624`-class error for the private authentication method; unresolved-import errors may accompany it. No runtime or semantic failure is expected.

- [ ] **Step 5: Add the minimal imports and visibility seam (GREEN implementation)**

Use this ownership shape in `engine.rs`:

```rust
use crate::cache::ObjectCache;
use crate::error::EncryptedError;
use crate::reader::resolver::ResolverHandle;
use crate::reader::PdfOpenOptions;
use crate::xref::load_xref_state_with_repair;
use crate::{Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Seek};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
```

Change only the visibility of the existing authentication method in `reader.rs`:

```rust
pub(crate) fn authenticate_if_encrypted(
    &mut self,
    options: &PdfOpenOptions,
) -> Result<()> {
    // existing body unchanged
}
```

Remove `load_xref_state_with_repair`, `ObjectCache`, `ResolverHandle`, `AtomicU64`, and `Ordering` from `reader.rs` imports after the moved helper is gone; retain `CacheEntry`. Move the test-only `Cursor` and `Arc` names into the existing test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, SeekFrom};
    use std::sync::Arc;
    // existing tests unchanged
}
```

Keep production `Read` and `Seek` imports in `reader.rs`. Do not widen `EncryptionState` members or move authentication helpers.

- [ ] **Step 6: Verify the focused factory paths are GREEN**

Run:

```bash
cargo fmt --all
cargo check -p flpdf
cargo test -p flpdf --lib open_mem
cargo test -p flpdf --lib engine::tests
cargo test -p flpdf --test reader_tests
cargo test -p flpdf-cli --test cli_tests
```

Expected: all commands PASS. The existing Arc strong-count tests prove that `open_mem*` still retain the caller's allocation; Cursor-equivalence tests prove the wrappers still use the generic open path; repair tests prove option forwarding.

- [ ] **Step 7: Verify source ownership and review the source-only diff**

Run:

```bash
rg -n 'static NEXT_PDF_ID|const MAX_RESOLUTION_FALLBACKS|pub fn open\(|pub fn open_with_repair\(|pub fn open_best_effort\(|pub fn open_with_options\(|fn open_with_repair_mode|pub fn open_mem\(|pub fn open_mem_with_options\(|pub fn open_mem_owned\(|pub fn open_mem_owned_with_options\(' crates/flpdf/src/engine.rs crates/flpdf/src/reader.rs
git diff -- crates/flpdf/src/engine.rs crates/flpdf/src/reader.rs
```

Expected: every listed definition appears in `engine.rs`, none appears in `reader.rs`, and the only non-move logic diff is `authenticate_if_encrypted` becoming `pub(crate)` plus import cleanup.

- [ ] **Step 8: Commit the source relocation**

```bash
git add crates/flpdf/src/engine.rs crates/flpdf/src/reader.rs
git commit -m "refactor(engine): extract Pdf factory orchestration"
```

---

### Task 2: Retarget qpdf correspondence and generated module documentation

**Files:**
- Modify: `crates/flpdf/src/engine.rs:1`
- Modify: `crates/flpdf/src/reader.rs:1`
- Modify: `docs/qpdf-correspondence.md:134`
- Modify: `docs/qpdf-correspondence.md:222`
- Regenerate: `docs/qpdf-module-doc-index.md`

**Interfaces:**
- Consumes: final source ownership from Task 1 and pinned qpdf 11.9.0 names `QPDF::emptyPDF`, `QPDF::processFile`, and `QPDF::processMemoryFile`.
- Produces: module docs and correspondence rows that identify `engine.rs` as the construction owner without removing the QPDF.cc responsibilities that remain in `reader.rs` and its supporting modules.

- [ ] **Step 1: Make the generated module-doc check fail for the stale annotation (RED)**

After changing `engine.rs`'s module annotation to the new ownership but before regenerating the index, run:

```bash
python3 scripts/qpdf-module-docs.py --check
```

Expected: FAIL because `docs/qpdf-module-doc-index.md` still says `processFile`/`processMemoryFile` remain in `reader.rs`.

- [ ] **Step 2: Record the exact module responsibilities**

Set the source annotations to these meanings:

```rust
// engine.rs
//! qpdf correspondence: QPDF.cc document-construction entry points (`emptyPDF()`, `processFile()`, and `processMemoryFile()`) and their shared construction orchestration.

// reader.rs
//! qpdf correspondence: QPDF.cc object resolution, recovery, diagnostics, and authentication responsibilities.
```

Retain the existing deviation-class comment next to `Pdf::empty`, and add one concise module-level/source-near sentence stating that Rust splits QPDF.cc construction into `engine.rs` while retaining the single `Pdf<R>` type.

- [ ] **Step 3: Update the durable correspondence table**

In the `QPDF.cc` row at `docs/qpdf-correspondence.md:134`:

- add `engine.rs` with `Pdf::empty`, all eight other public factories, `open_with_repair_mode`, `NEXT_PDF_ID`, and `MAX_RESOLUTION_FALLBACKS`;
- identify them as the `emptyPDF`/`processFile`/`processMemoryFile` construction path;
- retain `pdf.rs`, `reader.rs`, `reader/resolver.rs`, `reader/file_object.rs`, `xref.rs`, `object_copy.rs`, `cache.rs`, `writer/object_streams.rs`, `signatures.rs`, `page_closure.rs`, and `ref_chain.rs` because their QPDF.cc responsibilities remain;
- refresh only changed file line counts with `wc -l`; do not rewrite unrelated correspondence claims.

In the `QPDFPageDocumentHelper.cc` row at `docs/qpdf-correspondence.md:222`, replace the old `engine.rs` line range and “first step” wording with the final construction ownership: `Pdf::empty` delegates to `open_mem_owned`, and both now live in `engine.rs` as the `emptyPDF`/`processMemoryFile`-equivalent path.

- [ ] **Step 4: Regenerate and verify module documentation (GREEN)**

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
python3 scripts/qpdf-module-docs.py --check
cargo fmt --all -- --check
```

Expected: all commands PASS; the generated index describes the new engine and reader ownership exactly once each.

- [ ] **Step 5: Review and commit the documentation**

```bash
git diff --check
git diff -- crates/flpdf/src/engine.rs crates/flpdf/src/reader.rs docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md
git add crates/flpdf/src/engine.rs crates/flpdf/src/reader.rs docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md
git commit -m "docs: record Pdf engine factory ownership"
```

Expected: the second commit contains correspondence changes and any source-annotation-only adjustment, with no unrelated generated-index churn.

---

### Task 3: Run full parity, quality, and publication gates

**Files:**
- Verify: all files changed in Tasks 1-2
- Update tracker: Bead `flpdf-se9h`

**Interfaces:**
- Consumes: the two committed implementation/documentation changes.
- Produces: a pushed branch and PR-ready evidence; the Bead remains `in_progress` until the PR is merged and verified on `main`.

- [ ] **Step 1: Run formatting, lint, library, CLI, and strict-doc gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf
cargo test -p flpdf-cli --test cli_tests
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
python3 scripts/qpdf-module-docs.py --check
```

Expected: every command exits 0.

- [ ] **Step 2: Run the qpdf-zlib-compatible byte-stability gates**

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests
cargo test -p flpdf --features qpdf-zlib-compat --test deterministic_id_qpdf_parity_tests
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical
```

Expected: every command PASS, demonstrating that construction relocation did not alter deterministic IDs, writer bytes, linearization bytes, or CLI bytes.

- [ ] **Step 3: Run authoritative changed-line coverage**

Run from a clean committed worktree and retain the evidence:

```bash
set -o pipefail
scripts/patch-coverage.sh --base origin/main 2>&1 | tee /tmp/flpdf-se9h-patch-coverage.log
```

Expected: `crates/flpdf/src` changed lines report 100% coverage. Do not use `--allow-dirty` and do not reuse an LCOV file from another commit.

- [ ] **Step 4: Perform final scope and history review**

```bash
git status --short --branch
git diff --check origin/main...HEAD
git diff --stat origin/main...HEAD
git log --oneline --decorate origin/main..HEAD
git diff origin/main...HEAD -- crates/flpdf/src/engine.rs crates/flpdf/src/reader.rs docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md
```

Expected: only the approved source/docs/plan files are present; no resolve entrypoint moved; public signatures and bodies are unchanged apart from module location and the one visibility seam.

- [ ] **Step 5: Record evidence in Beads and persist it**

Build the evidence note from the committed history and the authoritative coverage log:

```bash
FLPDF_SE9H_COMMITS="$(git log --format='%h %s' origin/main..HEAD | paste -sd ';' -)"
FLPDF_SE9H_COVERAGE="$(tail -n 8 /tmp/flpdf-se9h-patch-coverage.log | tr '\n' ' ')"
bd update flpdf-se9h --notes "Implementation complete on refactor/flpdf-se9h-engine-factories. Commits: ${FLPDF_SE9H_COMMITS}. Pdf factory dependency closure moved to engine.rs with public API and bodies unchanged; authenticate_if_encrypted visibility only widened to pub(crate). Passed: cargo fmt --all -- --check; workspace all-target/all-feature clippy -D warnings; cargo test -p flpdf; flpdf-cli cli_tests; cmp_linearize_tests; deterministic_id_qpdf_parity_tests; cli_byte_identical; strict rustdoc; qpdf module-doc check. Coverage: ${FLPDF_SE9H_COVERAGE}"
bd show flpdf-se9h
bd dolt push
```

Expected: the readback contains the actual commit subjects and measured coverage output. Do not close the Bead before merge verification.

- [ ] **Step 6: Push the branch and open the PR**

```bash
git push -u origin refactor/flpdf-se9h-engine-factories
FLPDF_SE9H_COMMITS="$(git log --format='%h %s' origin/main..HEAD | paste -sd ';' -)"
FLPDF_SE9H_COVERAGE="$(tail -n 8 /tmp/flpdf-se9h-patch-coverage.log | tr '\n' ' ')"
gh pr create --base main --head refactor/flpdf-se9h-engine-factories --title "refactor(engine): extract Pdf factory orchestration" --body "## Summary
- move Pdf::open*, Pdf::open_mem*, and their shared construction helper into engine.rs beside Pdf::empty
- keep resolver and authentication implementations in reader.rs with one crate-private call seam
- retarget qpdf correspondence and generated module documentation

Bead: flpdf-se9h

Resolve entrypoints are intentionally excluded from this PR.

## Verification
- cargo fmt --all -- --check
- cargo clippy --workspace --all-targets --all-features -- -D warnings
- cargo test -p flpdf
- cargo test -p flpdf-cli --test cli_tests
- qpdf-zlib-compat cmp_linearize_tests, deterministic_id_qpdf_parity_tests, and cli_byte_identical
- strict workspace rustdoc
- qpdf module-doc check

Commits: ${FLPDF_SE9H_COMMITS}

Changed-line coverage: ${FLPDF_SE9H_COVERAGE}"
```

The PR body must summarize the responsibility-only move, list `flpdf-se9h`, state that resolve entrypoints are excluded, and include the exact verification/coverage evidence. Do not merge the PR; hand it to the user after checks are green.
