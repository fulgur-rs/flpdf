# QPDFJob lifecycle integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Align flpdf's QPDFJob create/write/run and completion boundaries with qpdf 11.9.0.

**Architecture:** Keep `JobDocument` as the public create-stage document, move configured document transformations before it is returned, and make the write stage select output or report-only inspection. Record/drain warnings once, with `get_exit_code` as a pure status query.

**Tech Stack:** Rust workspace, `QPDFJob` in `crates/flpdf/src/job/lifecycle.rs`, qpdf 11.9.0 oracle, Rust unit/integration tests, CLI compatibility tests.

**Spec:** `docs/superpowers/specs/2026-09-08-flpdf-3yn9-48-5-job-lifecycle-design.md`

## Global Constraints

- qpdf 11.9.0 source and behavior are authoritative: `libqpdf/QPDFJob.cc:428-520,535-564,1645-1693`.
- `run` must compose create then write; it must not own a third lifecycle implementation.
- Completion emits at most one warning summary per job; `get_exit_code` performs no logging or warning drain.
- Keep `JobExitCode` values aligned with qpdf exit constants and preserve encryption-status special cases.
- Do not touch qtest exception work or add a compatibility bridge.
- Run `cargo fmt --all -- --check`, all-features clippy, strict rustdoc, workspace tests, qpdf checkers, and fresh patch coverage before PR.

---

### Task 1: Fix the failing lifecycle contract tests

**Files:**
- Modify: `crates/flpdf/tests/job_lifecycle_tests.rs` near the existing direct `create_qpdf`/`write_qpdf` tests.
- Modify: `crates/flpdf/src/job/lifecycle.rs` test module only for test fixtures if an existing helper is private to that module.

**Interfaces:**
- Consumes: `QPDFJob::create_qpdf`, `QPDFJob::write_qpdf`, `QPDFJob::run`, `JobExitCode`, and the existing logger/test fixtures.
- Produces: executable assertions for the qpdf two-stage and one-summary contracts that fail against the current implementation.

- [ ] **Step 1: Write RED tests for direct create/write transformation state and pure exit status.** Add tests that (a) configure a rotation or JSON update, call `create_qpdf`, inspect the returned document before writing, (b) mutate the returned document, call `write_qpdf`, and assert the mutation is emitted, (c) run combined inspection flags and count exactly one completion summary, and (d) call `get_exit_code` twice while asserting no additional logger output.

- [ ] **Step 2: Run only the new tests against the baseline.**

Run:

```bash
cargo test -p flpdf --test job_lifecycle_tests create_qpdf_returns --exact
cargo test -p flpdf --test job_lifecycle_tests combined_inspection --exact
cargo test -p flpdf --test job_lifecycle_tests get_exit_code --exact
```

Expected: the new lifecycle assertions fail because `create_qpdf` leaves configured stages for `run_document_erased`, `write_qpdf` rejects inspection without output, and no pure `get_exit_code` method exists.

### Task 2: Separate create-stage preparation from operation dispatch

**Files:**
- Modify: `crates/flpdf/src/job/lifecycle.rs` around `create_qpdf`, `run_document_erased`, `run_document_stages`, and `write_qpdf`.
- Test: `crates/flpdf/tests/job_lifecycle_tests.rs` and the lifecycle unit tests in `crates/flpdf/src/job/lifecycle.rs`.

**Interfaces:**
- Consumes: existing `finish_created_document`, `handle_page_specs`, `apply_configured_rotations`, `apply_overlay_specs`, `run_configured_inspection`, `write_configured_json`, `split_pages`, and `write_qpdf` internals.
- Produces: `fn prepare_document(&mut self, primary: JobDocument, configuration: &JobConfiguration) -> Result<JobDocument>` and `pub fn write_qpdf<R>(&mut self, pdf: &mut Pdf<R>) -> Result<()>`, with the write dispatcher callable by both `run` and direct two-stage consumers without duplicating transformation order.

- [ ] **Step 1: Move the page-spec and transformation work into the create-stage path.** Preserve qpdf order: update JSON, page selection, rotations, underlay/overlay, then transformations. Keep secondary source owners alive until the merged document no longer references them; use the existing `PageSpecJobOutput` ownership contract rather than a sentinel or a second document representation.

- [ ] **Step 2: Change the write-stage dispatcher to select inspection, split, JSON, or ordinary output.** Use the qpdf `createsOutput` rule (`output_file.is_some() || replace_input`) and the existing split configuration. Report-only inspection methods must run before the single completion call.

- [ ] **Step 3: Make `run` call the two public stages.** Replace the direct `run_document_erased` operation path with `create_qpdf` followed by the write-stage dispatcher, preserving creation error reporting and encryption-status early returns.

- [ ] **Step 4: Run the Task 1 tests and focused lifecycle tests.**

Run:

```bash
cargo test -p flpdf --test job_lifecycle_tests --quiet
cargo test -p flpdf --lib job::lifecycle --quiet
```

Expected: direct create/write and combined inspection tests pass, with no duplicate summary or lost post-create mutation.

### Task 3: Add qpdf-shaped completion and exit-code query

**Files:**
- Modify: `crates/flpdf/src/job/lifecycle.rs` around `JobExitCode`, `complete`, `has_warnings`, and encryption status.
- Modify: `crates/flpdf-cli/src/main.rs` only at callers that currently compute status by calling `complete` directly.
- Test: `crates/flpdf/tests/job_lifecycle_tests.rs`.

**Interfaces:**
- Consumes: `warnings`, `suppress_warnings`, `warnings_exit_zero`, encryption status state, and `Pdf::any_warnings`/`get_warnings` from `.48.26`.
- Produces: `pub fn get_exit_code(&self) -> JobExitCode`, a private completion helper `fn complete(&self, creates_output: bool) -> Result<()>`, and callers that observe status without re-emitting the summary.

- [ ] **Step 1: Add RED assertions for warning/no-warn/warnings-exit-0 and encryption status.** Cover status queries before and after completion, repeated queries, suppression retaining warning state, and the qpdf encrypted/password status values.

- [ ] **Step 2: Implement the pure status query and make completion only perform summary/drain work.** Keep logger writes out of `get_exit_code`; do not add a second warning collection or a new sentinel status.

- [ ] **Step 3: Run the status tests and existing CLI lifecycle tests.**

Run:

```bash
cargo test -p flpdf --test job_lifecycle_tests warning --quiet
cargo test -p flpdf-cli --test cli_tests --quiet
```

Expected: all status combinations pass and each combined operation emits one summary.

### Task 4: Reconcile route documentation and regression coverage

**Files:**
- Modify: `docs/qpdf-route-matrix/e-job-cli-capi.md` rows E-1, E-3, E-5, E-6, E-7, and E-19 plus P-1/P-3/P-4 notes.
- Modify: `docs/qpdf-correspondence.md` QPDFJob lifecycle correspondence row.
- Test: `crates/flpdf/tests/job_lifecycle_tests.rs` and relevant CLI integration tests.

**Interfaces:**
- Consumes: measured caller counts and the final private/public lifecycle boundaries.
- Produces: source-cited current route classification, direct two-stage oracle coverage, combined inspection output/status coverage, and no stale claim that the old no-output error is qpdf behavior.

- [ ] **Step 1: Extend the existing qpdf differential coverage with the named fixtures `tests/fixtures/minimal.pdf`, `tests/fixtures/test_driver/repairable_input.pdf`, and `tests/fixtures/encrypted/v4-aes-128-r4.pdf` for ordinary output, warning inspection, and encryption status. Compare stdout, stderr, exit status, and output files with the pinned qpdf 11.9.0 binary; add the combined inspection flags `--check --show-npages --show-xref` to the repairable-input case.

- [ ] **Step 2: Recompute route callers from the final source.** Record exact production/test counts and leave unrelated CLI direct-writer and qtest exception routes in their existing issues.

- [ ] **Step 3: Run route and documentation checkers.**

Run:

```bash
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
python3 scripts/check-qpdf-route-matrix.py --check
```

Expected: all checkers exit 0 and citations point to the current source lines.

### Task 5: Full verification and handoff

**Files:**
- Verify: all changed Rust/docs files and the generated test artifacts are absent from Git status.

- [ ] **Step 1: Run formatting, focused tests, full library, and workspace tests.**

```bash
cargo fmt --all -- --check
cargo test -p flpdf --lib
cargo test --workspace --no-fail-fast --quiet
```

- [ ] **Step 2: Run strict quality gates.**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
```

- [ ] **Step 3: Run fresh patch coverage against the latest base.**

```bash
scripts/patch-coverage.sh --base origin/main
```

Expected: `flpdf` changed lines have `uncovered 0` and `PASS (100%)`.

- [ ] **Step 4: Commit, rebase latest `origin/main`, push a Draft PR, wait for every CI check, and mark the PR Ready only after all checks pass.** Update Beads with the commit/PR/check evidence, run `bd dep cycles`, run `bd dolt push`, and confirm `Push complete.` before the Ready transition.
