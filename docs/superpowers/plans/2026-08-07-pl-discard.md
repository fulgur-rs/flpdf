# Public Discard Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the public `flpdf::pipeline::Discard` terminal stage and make the EmbeddedFile checksum path consume it without changing checksum bytes.

**Architecture:** A focused `pipeline/discard.rs` module owns qpdf 11.9.0 `Pl_Discard` behavior and is publicly re-exported from `pipeline.rs`. The existing Filespec checksum helper keeps its `PlMd5` flow but replaces its private `ChecksumDiscard` implementation with the canonical public terminal.

**Tech Stack:** Rust workspace, existing `Pipeline`/`PipelineResult` API, Cargo integration tests, pinned qpdf 11.9.0 source, generated qpdf module-doc index.

## Global Constraints

- The public API name is exactly `flpdf::pipeline::Discard`; do not add a `PlDiscard` alias.
- Match `include/qpdf/Pl_Discard.hh:22-38` and `libqpdf/Pl_Discard.cc:5-22`: identifier `discard`, no successor, no-op write/finish, reusable after finish.
- Keep `md5_checksum`, `PlMd5`, and current EmbeddedFile metadata behavior unchanged except for replacing the terminal implementation.
- Leave provider/path input, `Pl_Count` finalization, warning behavior, and direct helper removal to `flpdf-25kg.4.4`.
- Add no state, sentinel, panic, buffering, allocation, compatibility wrapper, or generic null-sink abstraction.
- Use RED to GREEN TDD and finish with fresh 100% changed executable-line coverage.

---

### Task 1: Add the public Discard component

**Files:**
- Create: `crates/flpdf/src/pipeline/discard.rs`
- Modify: `crates/flpdf/src/pipeline.rs:6-54`
- Modify/Test: `crates/flpdf/tests/pipeline_public_api.rs:1-4`

**Interfaces:**
- Consumes: `flpdf::pipeline::Pipeline` with `identifier`, `write`, and `finish`; `PipelineResult<()>`.
- Produces: public unit type `flpdf::pipeline::Discard` implementing `Pipeline`.

- [ ] **Step 1: Write the failing public API tests**

Add `Discard` to the existing import and add these tests near the other public component tests:

```rust
use flpdf::pipeline::{
    Base64Action, Discard, Pipeline, PipelineError, PipelineResult, PlBase64, PlConcatenate,
    PlOStream, PlStdioFile, PlString,
};

#[test]
fn discard_is_a_public_pipeline_with_the_qpdf_identifier() {
    let discard = Discard;
    let pipeline: &dyn Pipeline = &discard;

    assert_eq!(pipeline.identifier(), "discard");
}

#[test]
fn discard_accepts_empty_and_nonempty_writes_across_finish_boundaries() {
    let mut discard = Discard;
    let pipeline: &mut dyn Pipeline = &mut discard;

    pipeline.write(b"").unwrap();
    pipeline.write(b"discarded bytes").unwrap();
    pipeline.finish().unwrap();
    pipeline.finish().unwrap();
    pipeline.write(b"after finish").unwrap();
    pipeline.finish().unwrap();
}
```

- [ ] **Step 2: Run the focused test target and verify RED**

Run:

```bash
cargo test -p flpdf --test pipeline_public_api discard
```

Expected: compilation fails with unresolved import `flpdf::pipeline::Discard`; the failure is caused by the missing public component.

- [ ] **Step 3: Implement the minimal public component**

Create `crates/flpdf/src/pipeline/discard.rs`:

```rust
//! qpdf correspondence: include/qpdf/Pl_Discard.hh:22-38 and libqpdf/Pl_Discard.cc:5-22 — terminal identifier, no-op writes and finishes, and reuse after finish.

use super::{Pipeline, PipelineResult};

pub struct Discard;

impl Pipeline for Discard {
    fn identifier(&self) -> &str {
        "discard"
    }

    fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}
```

Wire the private module and public re-export in `pipeline.rs`:

```rust
mod discard;
pub use discard::Discard;
```

- [ ] **Step 4: Run formatting and verify GREEN**

Run:

```bash
cargo fmt --all
cargo test -p flpdf --test pipeline_public_api discard
```

Expected: both Discard tests pass with no warnings.

- [ ] **Step 5: Commit the component**

```bash
git add crates/flpdf/src/pipeline.rs crates/flpdf/src/pipeline/discard.rs crates/flpdf/tests/pipeline_public_api.rs
git commit -m "feat(pipeline): add public Discard terminal"
```

---

### Task 2: Cut the Filespec checksum consumer over to Discard

**Files:**
- Modify: `crates/flpdf/src/filespec_helper.rs:83-89,851-879`
- Modify/Test: `crates/flpdf/tests/filespec_helper_tests.rs:1514-1528`

**Interfaces:**
- Consumes: Task 1's public `crate::pipeline::Discard` and the existing internal `PlMd5`.
- Produces: the unchanged public `md5_checksum(&[u8]) -> Vec<u8>` with no consumer-local terminal implementation.

- [ ] **Step 1: Write the failing sole-route guard**

Add this test beside `md5_checksum_length_and_known_value`:

```rust
#[test]
fn embedded_file_checksum_uses_the_canonical_discard_terminal() {
    let source = include_str!("../src/filespec_helper.rs");

    assert!(
        !source.contains("struct ChecksumDiscard"),
        "consumer-local checksum discard remains"
    );
    assert!(
        source.contains("let mut discard = Discard;"),
        "EmbeddedFile checksum does not use pipeline::Discard"
    );
}
```

- [ ] **Step 2: Run the guard and verify RED**

Run:

```bash
cargo test -p flpdf --test filespec_helper_tests embedded_file_checksum_uses_the_canonical_discard_terminal
```

Expected: the test fails with `consumer-local checksum discard remains` because `ChecksumDiscard` still exists.

- [ ] **Step 3: Replace the local terminal with Discard**

Change the pipeline import to:

```rust
use crate::pipeline::md5::PlMd5;
use crate::pipeline::{Discard, Pipeline};
```

Delete the complete `ChecksumDiscard` struct and `impl Pipeline` block. Change the first line of `md5_checksum` to:

```rust
let mut discard = Discard;
```

Keep the remaining `PlMd5` write, finish, digest, and `hex::decode` statements byte-for-byte unchanged.

- [ ] **Step 4: Verify GREEN and checksum stability**

Run:

```bash
cargo fmt --all
cargo test -p flpdf --test filespec_helper_tests embedded_file_checksum_uses_the_canonical_discard_terminal
cargo test -p flpdf --test filespec_helper_tests md5_checksum_length_and_known_value
cargo test -p flpdf --test filespec_helper_tests
```

Expected: the guard, known empty-MD5 vector, and all Filespec integration tests pass.

- [ ] **Step 5: Commit the production cutover**

```bash
git add crates/flpdf/src/filespec_helper.rs crates/flpdf/tests/filespec_helper_tests.rs
git commit -m "refactor(filespec): use canonical Discard terminal"
```

---

### Task 3: Record qpdf correspondence and regenerate module documentation

**Files:**
- Modify: `docs/qpdf-correspondence.md:185-208`
- Regenerate: `docs/qpdf-module-doc-index.md`

**Interfaces:**
- Consumes: `pipeline/discard.rs`'s qpdf correspondence module annotation.
- Produces: a separate completed `Pl_Discard.cc` row and an unchanged missing `Pl_Function.cc` row in the canonical ledger.

- [ ] **Step 1: Verify the generated module index is stale (RED)**

Run:

```bash
python3 scripts/qpdf-module-docs.py --check
```

Expected: non-zero exit identifying `docs/qpdf-module-doc-index.md` as stale because the new annotated module is absent.

- [ ] **Step 2: Split the combined correspondence row**

Replace the current combined `Pl_Discard / Pl_Function` row with:

```markdown
| `Pl_Discard.cc` | 23 | `pipeline/discard.rs`（public terminal identifier、no-op write/finish、finish 後の再利用）+ `filespec_helper.rs`（EmbeddedFile checksum terminal consumer） | ✅ |
| `Pl_Function.cc` | 62 | 専用 stage は未実装。使用箇所ごとの closure 実装 | ⚪ |
```

- [ ] **Step 3: Regenerate and verify module documentation (GREEN)**

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
python3 scripts/qpdf-module-docs.py --check
git diff --check
```

Expected: the generated index contains `crates/flpdf/src/pipeline/discard.rs`, the check exits 0, and the diff has no whitespace errors.

- [ ] **Step 4: Review and commit documentation**

Run:

```bash
git diff -- docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md
git add docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md
git commit -m "docs: record Discard pipeline parity"
```

Expected: only the split correspondence rows and one generated module-index entry change.

---

### Task 4: Run quality, coverage, tracker, and publication gates

**Files:**
- Verify: all files changed in Tasks 1-3 plus the approved spec and this plan.
- Update tracker: Bead `flpdf-qynx.8` remains `in_progress` until merge verification.

**Interfaces:**
- Consumes: the committed public component, production cutover, and documentation updates.
- Produces: a clean pushed branch with reproducible verification evidence and persisted Beads notes.

- [ ] **Step 1: Run focused and workspace quality gates**

Run each command independently:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test pipeline_public_api discard
cargo test -p flpdf --test filespec_helper_tests
cargo test --workspace
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
python3 scripts/qpdf-module-docs.py --check
```

Expected: every command exits 0 with no failures or denied warnings.

- [ ] **Step 2: Run fresh changed-line coverage from committed state**

Run:

```bash
scripts/patch-coverage.sh --base origin/main
```

Expected: changed executable lines under `crates/flpdf/src` report 100% coverage. Do not use `--allow-dirty` and do not reuse an LCOV file.

- [ ] **Step 3: Review final scope and history**

Run:

```bash
git status --short --branch
git diff --check origin/main...HEAD
git diff --stat origin/main...HEAD
git log --oneline --decorate origin/main..HEAD
git diff origin/main...HEAD -- crates/flpdf/src/pipeline.rs crates/flpdf/src/pipeline/discard.rs crates/flpdf/src/filespec_helper.rs crates/flpdf/tests/pipeline_public_api.rs crates/flpdf/tests/filespec_helper_tests.rs docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md
```

Expected: the worktree is clean; source changes are limited to Discard, its public export, and the Filespec terminal replacement; tests and documentation match the approved scope.

- [ ] **Step 4: Persist implementation evidence without closing the issue**

Append a dated `Implementation evidence` note to `flpdf-qynx.8` containing the exact qpdf source ranges, RED and GREEN commands, final gate exit results, changed-line coverage totals, branch name, and final commit. Then run:

```bash
bd show flpdf-qynx.8
bd dolt push
```

Expected: the issue remains `in_progress`, the evidence note reads back intact, and Dolt reports `Push complete.`

- [ ] **Step 5: Push the verified branch**

```bash
git push -u origin feature/flpdf-qynx.8-pl-discard
```

Expected: the remote branch is created or fast-forwarded successfully; do not close the Bead before merge verification.
