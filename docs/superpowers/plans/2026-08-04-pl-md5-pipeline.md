# Pl_MD5 Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add qpdf 11.9.0-compatible `PlMd5`, migrate embedded-file checksum generation through it, and preserve all existing PDF output and public APIs.

**Architecture:** A crate-private `pipeline::md5::PlMd5<'a>` wraps RustCrypto's incremental `Md5` state and a borrowed downstream `Pipeline`, reproducing qpdf's enable, persist, reuse, digest, forwarding, and failure-order contract. `filespec_helper::md5_checksum` assembles the exact production shape `PlMd5 -> private discard`, then converts the qpdf-shaped lowercase hexadecimal digest back to the existing 16-byte checksum value.

**Tech Stack:** Rust workspace (`flpdf`), existing public `Pipeline` trait, RustCrypto `md-5` 0.10, `hex` 0.4, pinned qpdf 11.9.0 source, Cargo unit/integration tests, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

## Global Constraints

- Pinned qpdf 11.9.0 at `scripts/fetch-qpdf-source.sh --print-path` is the semantic oracle.
- Preserve qpdf responsibility boundaries; do not add a compatibility adapter, legacy fallback, driver-only route, or generic digest abstraction.
- Keep the MD5 primitive in RustCrypto; do not reimplement MD5 or add a crypto-provider abstraction.
- Keep `PlMd5` and its module crate-private; do not expand the public flpdf pipeline API.
- Migrate embedded-file `/Params /CheckSum`; do not modify the deterministic writer `/ID` route.
- Do not implement `Pl_SHA2`, `Pl_AES_PDF`, or the separate qpdf `Pl_Discard` component in this issue.
- Preserve `md5_checksum(&[u8]) -> Vec<u8>` and every existing embedded-file PDF byte/graph behavior.
- Follow RED -> GREEN TDD for the new component. Do not add test-only production instrumentation to detect the consumer route.
- Every expected digest in tests must be a hand-checked literal, not a value computed by `md5_checksum` or `PlMd5` itself.
- Finish with fresh 100% changed-line coverage, full-feature workspace clippy, crate/workspace tests, a clean worktree, and successful git and Beads pushes.

---

## File map

| File | Responsibility |
| --- | --- |
| `crates/flpdf/src/pipeline/md5.rs` | qpdf-shaped `PlMd5` state machine and focused behavior tests |
| `crates/flpdf/src/pipeline.rs` | crate-private module registration only |
| `crates/flpdf/src/filespec_helper.rs` | private discard sink and EmbeddedFile checksum consumer cutover |
| `docs/qpdf-correspondence.md` | truthful `Pl_MD5` implementation and consumer ledger entry |
| `docs/qpdf-module-doc-index.md` | generated module-correspondence index including `pipeline/md5.rs` |

No new public exports, dependencies, fixtures, or writer changes are required.

---

### Task 1: Implement the complete `PlMd5` component contract

**Files:**
- Create: `crates/flpdf/src/pipeline/md5.rs`
- Modify: `crates/flpdf/src/pipeline.rs:1-35`
- Test: `crates/flpdf/src/pipeline/md5.rs`

**Interfaces:**
- Consumes: `crate::pipeline::{Pipeline, PipelineError, PipelineResult}` and RustCrypto `md5::{Digest, Md5}`.
- Produces: `pub(crate) struct PlMd5<'a>` with:
  - `pub(crate) fn new(identifier: impl Into<String>, next: &'a mut dyn Pipeline) -> Self`
  - `pub(crate) fn enable(&mut self, enabled: bool)`
  - `pub(crate) fn persist_across_finish(&mut self, persist: bool)`
  - `pub(crate) fn get_hex_digest(&mut self) -> PipelineResult<String>`
  - `impl Pipeline for PlMd5<'_>`
- Later consumer: `filespec_helper::md5_checksum` constructs `PlMd5::new("EF md5", &mut discard)`.

- [ ] **Step 1: Add the module and failing contract tests without the production type**

Add `pub(crate) mod md5;` in alphabetic order in `pipeline.rs`. Create `pipeline/md5.rs` with the oracle module comment and the tests below, but deliberately do not define `PlMd5` yet:

```rust
//! qpdf correspondence: libqpdf/Pl_MD5.cc:5-65 and libqpdf/qpdf/Pl_MD5.hh:4-33 — unchanged forwarding, enable/persist state, reusable finish lifecycle, and hexadecimal digest retrieval.

#[cfg(test)]
mod tests {
    use super::PlMd5;
    use crate::pipeline::test_support::{shared_trace, RecordingSink, TraceCall};
    use crate::pipeline::{Pipeline, PipelineError};

    #[test]
    fn forwards_original_chunks_and_reports_the_known_digest() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        let digest = {
            let mut md5 = PlMd5::new("md5", &mut sink);
            md5.write(b"ab").unwrap();
            md5.write(b"c").unwrap();
            md5.finish().unwrap();
            md5.get_hex_digest().unwrap()
        };

        assert_eq!(digest, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(trace.borrow().output, b"abc");
        assert_eq!(
            trace.borrow().calls,
            vec![
                TraceCall::Write {
                    data: b"ab".to_vec(),
                    failed: false,
                },
                TraceCall::Write {
                    data: b"c".to_vec(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn successful_finish_makes_the_next_write_start_a_new_digest() {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        md5.write(b"a").unwrap();
        md5.finish().unwrap();
        md5.write(b"b").unwrap();

        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "92eb5ffee6ae2fec3ad71c777531578f"
        );
    }

    #[test]
    fn persistent_mode_accumulates_across_finish_boundaries() {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        md5.persist_across_finish(true);
        md5.write(b"a").unwrap();
        md5.finish().unwrap();
        md5.write(b"b").unwrap();
        md5.finish().unwrap();
        md5.write(b"c").unwrap();

        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
    }

    #[test]
    fn repeated_digest_is_stable_and_a_later_write_resets() {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        md5.write(b"abc").unwrap();
        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        md5.write(b"a").unwrap();
        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "0cc175b9c0f1b6a831c399e269772661"
        );
    }

    #[test]
    fn disabled_mode_forwards_but_rejects_digest_without_losing_progress() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        md5.write(b"a").unwrap();
        md5.enable(false);
        md5.write(b"b").unwrap();
        assert!(matches!(
            md5.get_hex_digest().unwrap_err(),
            PipelineError::Logic(_)
        ));
        assert_eq!(
            md5.get_hex_digest().unwrap_err().to_string(),
            "digest requested for a disabled MD5 Pipeline"
        );
        md5.enable(true);
        md5.write(b"c").unwrap();

        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "e2075474294983e013ee4dd2201c7a73"
        );
        assert_eq!(trace.borrow().output, b"abc");
    }

    #[test]
    fn downstream_write_failure_still_leaves_the_chunk_in_the_digest() {
        let mut sink = RecordingSink::new(&[1], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        assert!(matches!(md5.write(b"abc").unwrap_err(), PipelineError::Runtime(_)));

        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
    }

    #[test]
    fn downstream_finish_failure_keeps_the_digest_in_progress() {
        let mut sink = RecordingSink::new(&[], &[1]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        md5.write(b"a").unwrap();
        assert!(matches!(md5.finish().unwrap_err(), PipelineError::Runtime(_)));
        md5.write(b"b").unwrap();

        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "187ef4436122d1cc2f40dc2b92f0eba0"
        );
    }

    #[test]
    fn no_data_and_an_empty_write_both_report_the_empty_digest() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        md5.write(b"").unwrap();
        md5.finish().unwrap();
        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        assert_eq!(
            trace.borrow().calls,
            vec![
                TraceCall::Write {
                    data: Vec::new(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }
}
```

These tests name concrete breaks: altered downstream bytes/chunks, missing reset, incorrect persistence, destructive digest retrieval, disabled bytes entering the hash, a failed write being omitted, a failed finish resetting early, or an empty write being skipped downstream.

- [ ] **Step 2: Run the focused test target and verify RED**

```bash
cargo test -p flpdf pipeline::md5::tests -- --nocapture
```

Expected: compilation fails because `pipeline::md5::PlMd5` does not exist. Any syntax/import failure elsewhere is the wrong RED and must be fixed before proceeding.

- [ ] **Step 3: Add the minimal source-derived implementation above the tests**

```rust
use super::{Pipeline, PipelineError, PipelineResult};
use md5::{Digest, Md5};

const MAX_UPDATE_BYTES: usize = 1 << 30;

pub(crate) struct PlMd5<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    in_progress: bool,
    md5: Md5,
    enabled: bool,
    persist_across_finish: bool,
}

#[allow(dead_code)]
impl<'a> PlMd5<'a> {
    pub(crate) fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            next,
            in_progress: false,
            md5: Md5::new(),
            enabled: true,
            persist_across_finish: false,
        }
    }

    pub(crate) fn enable(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub(crate) fn persist_across_finish(&mut self, persist: bool) {
        self.persist_across_finish = persist;
    }

    pub(crate) fn get_hex_digest(&mut self) -> PipelineResult<String> {
        if !self.enabled {
            return Err(PipelineError::logic(
                "digest requested for a disabled MD5 Pipeline",
            ));
        }
        self.in_progress = false;
        Ok(hex::encode(self.md5.clone().finalize()))
    }
}

impl Pipeline for PlMd5<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if self.enabled {
            if !self.in_progress {
                self.md5 = Md5::new();
                self.in_progress = true;
            }
            for chunk in data.chunks(MAX_UPDATE_BYTES) {
                self.md5.update(chunk);
            }
        }
        self.next.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.next.finish()?;
        if !self.persist_across_finish {
            self.in_progress = false;
        }
        Ok(())
    }
}
```

Do not reorder digest update after downstream `write`, and do not clear `in_progress` before downstream `finish`; those would contradict `Pl_MD5.cc:14-43` and break the failure-order tests.

- [ ] **Step 4: Run focused tests and verify GREEN**

```bash
cargo test -p flpdf pipeline::md5::tests -- --nocapture
```

Expected: 8 tests pass, 0 fail. Read the count rather than relying on the exit code alone.

- [ ] **Step 5: Format, rerun the focused target, and inspect the diff**

```bash
cargo fmt --all
cargo test -p flpdf pipeline::md5::tests
git diff --check
git diff -- crates/flpdf/src/pipeline.rs crates/flpdf/src/pipeline/md5.rs
```

Expected: formatting/checks pass; the diff contains only module registration, the source-cited stage, and its behavior tests.

- [ ] **Step 6: Commit the completed component**

```bash
git add crates/flpdf/src/pipeline.rs crates/flpdf/src/pipeline/md5.rs
git commit -m "feat: add qpdf-shaped Pl_MD5 pipeline"
```

Expected: one commit containing both the implementation and the tests that were observed RED then GREEN.

---

### Task 2: Migrate EmbeddedFile checksum generation

**Files:**
- Modify: `crates/flpdf/src/filespec_helper.rs:82-92,852-860,1994-2025`
- Test: `crates/flpdf/src/filespec_helper.rs:1994-2025`
- Test: `crates/flpdf/tests/filespec_helper_tests.rs:1484-1503`

**Interfaces:**
- Consumes: `crate::pipeline::md5::PlMd5`, `Pipeline`, and `PipelineResult` from Task 1.
- Produces: unchanged `pub fn md5_checksum(data: &[u8]) -> Vec<u8>` implemented through `PlMd5 -> ChecksumDiscard`.
- Preserves: `/Params /CheckSum` as 16 raw bytes over the uncompressed payload and every existing caller signature.

- [ ] **Step 1: Replace the tautological non-empty checksum expectation with a literal**

In `filespec_helper::tests::add_attachment_from_path_checksum_and_size`, replace the expectation computed through `md5_checksum(raw)` with this hand-checked byte literal:

```rust
assert_eq!(
    checksum,
    vec![
        0xcf, 0x5e, 0x73, 0xd1, 0x4d, 0xf5, 0xca, 0xd1, 0x94, 0xb0, 0x9e, 0xe5, 0x79, 0xf2,
        0x54, 0x9d,
    ],
    "/Params /CheckSum must be the MD5 of raw bytes"
);
```

This expected value is MD5(`deterministic checksum test data`) and is independent of both the new stage and the public helper.

- [ ] **Step 2: Run the strengthened characterization test before refactoring**

```bash
cargo test -p flpdf filespec_helper::tests::add_attachment_from_path_checksum_and_size -- --exact
```

Expected: PASS on the old direct-RustCrypto route. This task is an output-preserving consumer refactor, so there is no honest public-boundary RED; do not add route counters, source-grep tests, or test-only production hooks. Task 1 already supplied the required RED for the new component.

- [ ] **Step 3: Replace the direct MD5 import and add the private infallible sink**

Replace:

```rust
use md5::{Digest, Md5};
```

with:

```rust
use crate::pipeline::md5::PlMd5;
use crate::pipeline::{Pipeline, PipelineResult};
```

Immediately above `md5_checksum`, add:

```rust
struct ChecksumDiscard;

impl Pipeline for ChecksumDiscard {
    fn identifier(&self) -> &str {
        "embedded file checksum discard"
    }

    fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}
```

This is a consumer-local terminal sink, not a claim that qpdf's reusable `Pl_Discard` component has been implemented.

- [ ] **Step 4: Route `md5_checksum` through `PlMd5`**

Replace the direct RustCrypto body with:

```rust
pub fn md5_checksum(data: &[u8]) -> Vec<u8> {
    let mut discard = ChecksumDiscard;
    let mut md5 = PlMd5::new("EF md5", &mut discard);
    md5.write(data)
        .expect("embedded-file MD5 discard write is infallible");
    md5.finish()
        .expect("embedded-file MD5 discard finish is infallible");
    let hex_digest = md5
        .get_hex_digest()
        .expect("embedded-file MD5 pipeline remains enabled");
    hex::decode(hex_digest).expect("PlMd5 always returns lowercase hexadecimal")
}
```

Do not add a fallback call to `Md5::digest`, return `Result`, expose `ChecksumDiscard`, or change any caller.

- [ ] **Step 5: Run the focused component and production consumer tests**

```bash
cargo test -p flpdf pipeline::md5::tests
cargo test -p flpdf filespec_helper::tests::add_attachment_from_path_checksum_and_size -- --exact
cargo test -p flpdf --test filespec_helper_tests md5_checksum_length_and_known_value -- --exact
cargo test -p flpdf --test filespec_helper_tests qpdf_factories_create_filespec_and_embedded_file_objects -- --exact
```

Expected: all four commands pass, with the non-empty production test comparing against a literal independent digest.

- [ ] **Step 6: Perform the consumer mutation check**

Temporarily change only `md5.write(data)` to `md5.write(b"")`, then run:

```bash
cargo test -p flpdf filespec_helper::tests::add_attachment_from_path_checksum_and_size -- --exact
```

Expected: FAIL showing actual empty-input digest bytes instead of the literal `cf5e73d14df5cad194b09ee579f2549d`. Restore `md5.write(data)` with `apply_patch`, rerun the same command, and require PASS. Do not commit the mutation.

- [ ] **Step 7: Prove the old filespec route is gone and the writer is untouched**

```bash
rg -n "use md5::|\bMd5\b" crates/flpdf/src/filespec_helper.rs
git diff -- crates/flpdf/src/writer.rs
git diff --check
```

Expected: the `rg` command has no matches, the writer diff is empty, and the diff check passes.

- [ ] **Step 8: Format and commit the consumer cutover**

```bash
cargo fmt --all
cargo test -p flpdf pipeline::md5::tests
cargo test -p flpdf --test filespec_helper_tests
git add crates/flpdf/src/filespec_helper.rs crates/flpdf/tests/filespec_helper_tests.rs
git commit -m "refactor(filespec): hash checksums through Pl_MD5"
```

If `crates/flpdf/tests/filespec_helper_tests.rs` remains unchanged, omit it from `git add`. Expected: the pipeline tests and all Filespec integration tests pass before the commit.

---

### Task 3: Record parity, run complete verification, and persist the branch

**Files:**
- Modify: `docs/qpdf-correspondence.md:184-210`
- Generate: `docs/qpdf-module-doc-index.md`
- Verify: all files changed since `origin/main`

**Interfaces:**
- Consumes: completed `PlMd5` and EmbeddedFile cutover from Tasks 1-2.
- Produces: truthful correspondence ledger, fresh verification evidence, closed/persisted Bead, and pushed git branch.

- [ ] **Step 1: Update the correspondence ledger**

Change the `Pl_MD5.cc` row from missing to implemented, naming both the component and first production consumer:

```markdown
| `Pl_MD5.cc` | 66 | `pipeline/md5.rs`（enable/persist/reuse、hex digest、forwarding/error order）+ `filespec_helper.rs`（EmbeddedFile `/Params /CheckSum` production consumer） | ✅ |
```

Leave the separate `Pl_Discard / Pl_Function / Pl_SHA2` row unchanged.

Regenerate the module-doc index after adding the `pipeline/md5.rs` correspondence annotation:

```bash
python3 scripts/qpdf-module-docs.py --write
```

- [ ] **Step 2: Re-resolve and recheck the pinned qpdf evidence**

```bash
qpdf_source="$(scripts/fetch-qpdf-source.sh --print-path)"
sed -n '1,90p' "$qpdf_source/libqpdf/Pl_MD5.cc"
sed -n '1,50p' "$qpdf_source/libqpdf/qpdf/Pl_MD5.hh"
sed -n '35,85p' "$qpdf_source/libtests/md5.cc"
sed -n '131,150p' "$qpdf_source/libqpdf/QPDFEFStreamObjectHelper.cc"
```

Expected: source still shows hash-before-forward, downstream-finish-before-reset, enable/persist, repeated digest, and `Pl_MD5 -> Pl_Discard` EmbeddedFile checksum conversion. If the fetch script reports a dirty pinned tree, stop rather than cite it.

- [ ] **Step 3: Run formatting and the full lint gate**

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all commands exit 0 with no warnings.

- [ ] **Step 4: Run focused, crate, and workspace tests**

```bash
cargo test -p flpdf pipeline::md5::tests
cargo test -p flpdf --test filespec_helper_tests
cargo test -p flpdf
cargo test
```

Expected: every command exits 0; read each final summary and confirm 0 failures. Ignored live-oracle tests may remain ignored exactly as on the clean baseline.

- [ ] **Step 5: Commit the ledger before fresh coverage**

Commit the correspondence ledger and this ordering correction before generating
coverage: `scripts/patch-coverage.sh` deliberately rejects a dirty worktree,
and coverage must compare the committed `HEAD` with `origin/main`.

```bash
git diff --check
git add docs/qpdf-correspondence.md docs/superpowers/plans/2026-08-04-pl-md5-pipeline.md
git commit -m "docs: record Pl_MD5 production parity"
```

Expected: the docs commit contains only the truthful ledger update and this
clean-tree coverage ordering correction.

- [ ] **Step 6: Generate fresh LCOV and enforce changed-line coverage**

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/flpdf-fitw.lcov
bash scripts/patch-coverage.sh --base origin/main \
  --lcov target/flpdf-fitw.lcov
```

Expected: `patch-coverage: OK` and 100% coverage on every changed executable line. If any line is uncovered, add only a behavior test that fails under a realistic mutation, observe that failure, restore the implementation, and rerun the focused and full gates; do not add a coverage-only assertion or default to `cov:ignore`.

- [ ] **Step 7: Audit the final diff and acceptance criteria**

```bash
python3 scripts/qpdf-module-docs.py --check
git diff --check origin/main...HEAD
git diff --stat origin/main...HEAD
git diff origin/main...HEAD -- \
  crates/flpdf/src/pipeline.rs \
  crates/flpdf/src/pipeline/md5.rs \
  crates/flpdf/src/filespec_helper.rs \
  crates/flpdf/tests/filespec_helper_tests.rs \
  docs/qpdf-correspondence.md \
  docs/qpdf-module-doc-index.md \
  docs/superpowers/specs/2026-08-04-pl-md5-pipeline-design.md \
  docs/superpowers/plans/2026-08-04-pl-md5-pipeline.md
git status --short --branch
```

Expected: only the planned files changed; writer code and public exports are untouched; worktree is clean; every design acceptance criterion maps to an implementation, test, source citation, or verification result.

- [ ] **Step 8: Record exact evidence in Beads**

After the preceding gates have produced exactly the required results, record them with the final
commit ID:

```bash
fitw_head="$(git rev-parse --short HEAD)"
bd update flpdf-fitw --append-notes "2026-08-04 implementation evidence: Classification oracle match. qpdf 11.9.0 evidence: Pl_MD5.cc:5-65, Pl_MD5.hh:4-33, libtests/md5.cc:41-76, QPDFEFStreamObjectHelper.cc:131-147. TDD: missing PlMd5 compile RED; 8/8 pipeline::md5 tests GREEN. Consumer mutation to empty input failed the independent non-empty checksum literal, then restored PASS. Verification: cargo fmt check PASS; full-feature workspace clippy PASS; pipeline, Filespec, flpdf crate, and workspace tests PASS with 0 failures; patch coverage 100%. Head: ${fitw_head}."
bd show flpdf-fitw
```

Expected: the issue shows those observed results and remains associated with the claimed owner.
If any preceding result differs, edit the note to state the actual result and do not proceed to
closure.

- [ ] **Step 9: Push git and Beads, then close the completed task**

```bash
git push -u origin feature/flpdf-fitw-pl-md5
bd close flpdf-fitw --reason="Pl_MD5 component and EmbeddedFile checksum consumer implemented with qpdf 11.9.0 lifecycle parity, full verification, and 100% changed-line coverage."
bd dolt push
bd show flpdf-fitw
git status --short --branch
```

Expected: git push succeeds without force, Dolt reports `Push complete`, `flpdf-fitw` is closed with the evidence retained, and the worktree remains clean. If either push fails, do not report completion; fix the persistence failure and retry safely.
