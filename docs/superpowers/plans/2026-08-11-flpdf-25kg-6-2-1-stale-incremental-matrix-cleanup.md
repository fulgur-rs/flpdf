# Stale incremental matrix cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore the merged workspace test suite by removing the incremental-output test and plan that target APIs intentionally deleted by the `PdfWriter` full-rewrite cutover.

**Architecture:** Keep `PdfWriter` as the sole PDF document-output path. Remove the two stale consumer artifacts without adding a compatibility bridge or changing production code; the existing `PdfWriter` contract and CLI full-rewrite suites remain the output coverage.

**Tech Stack:** Rust workspace, Cargo tests, Git worktree, Beads, qpdf 11.9.0 source/behavior boundary.

## Global Constraints

- Do not restore `flpdf::WriteOptions`, `flpdf::write_pdf_with_options`, PDF incremental output, source-prefix preservation, or append-only signature behavior.
- Preserve reader `/Prev` parsing and JSON/Pipeline incremental serialization; this follow-up only removes stale output consumers.
- Modify only the two stale tracked paths plus this plan and the already-reviewed design document; do not edit production writer code.
- Run `cargo fmt --all -- --check` and `cargo test --workspace` before claiming the fix is complete.
- Stage files explicitly; do not use `git add -A`, which could capture unrelated generated or untracked files.

---

### Task 1: Remove the stale incremental-output consumers

**Files:**
- Delete: `crates/flpdf-cli/tests/incremental_matrix_tests.rs`
- Delete: `docs/superpowers/plans/2026-08-10-flpdf-25kg-6-2-incremental-matrix.md`
- Reference: `docs/superpowers/specs/2026-08-11-flpdf-25kg-6-2-1-stale-incremental-matrix-cleanup-design.md`

**Interfaces:**
- Consumes: the current `PdfWriter` full-rewrite boundary and Bead `flpdf-25kg.6.2.1`.
- Produces: a source tree in which Cargo no longer discovers the stale integration test and no obsolete incremental-output plan remains.

- [ ] **Step 1: Confirm the failing consumer before removal**

Run:

```bash
cargo test --workspace
```

Expected: compilation fails only in `crates/flpdf-cli/tests/incremental_matrix_tests.rs` because `flpdf::WriteOptions` and `flpdf::write_pdf_with_options` are not exported by the current `PdfWriter` implementation.

- [ ] **Step 2: Delete the two obsolete artifacts**

Use `apply_patch` to delete exactly these files:

```text
crates/flpdf-cli/tests/incremental_matrix_tests.rs
docs/superpowers/plans/2026-08-10-flpdf-25kg-6-2-incremental-matrix.md
```

Do not modify `crates/flpdf/src/`, `crates/flpdf-cli/src/`, or any current full-rewrite test file.

- [ ] **Step 3: Verify no stale test or plan reference remains**

Run:

```bash
test ! -e crates/flpdf-cli/tests/incremental_matrix_tests.rs
test ! -e docs/superpowers/plans/2026-08-10-flpdf-25kg-6-2-incremental-matrix.md
rg -n "incremental_matrix_tests|flpdf-25kg-6-2-incremental-matrix" crates docs || true
git diff --check
git status --short
```

Expected: both `test` commands succeed; `rg` produces no matches for the deleted paths; `git diff --check` succeeds; status lists only the two deletions plus this implementation plan and the design commit's tracked files as applicable.

- [ ] **Step 4: Commit the focused cleanup**

Run:

```bash
git add crates/flpdf-cli/tests/incremental_matrix_tests.rs docs/superpowers/plans/2026-08-10-flpdf-25kg-6-2-incremental-matrix.md docs/superpowers/plans/2026-08-11-flpdf-25kg-6-2-1-stale-incremental-matrix-cleanup.md
git commit -m "test: remove stale incremental matrix"
```

Expected: the commit contains only the two stale deletions and this implementation plan; no production API or writer implementation changes are present.

### Task 2: Run the replacement verification gates and prepare the PR

**Files:**
- Test: current `crates/flpdf/tests/pdf_writer_contract_tests.rs`
- Test: current `crates/flpdf-cli/tests/cli_full_rewrite.rs`
- Metadata: Bead `flpdf-25kg.6.2.1`

**Interfaces:**
- Consumes: the focused cleanup commit from Task 1.
- Produces: verified `main`-compatible branch, pushed branch, and a new pull request linked to the Bead.

- [ ] **Step 1: Run formatting and focused full-rewrite coverage**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test pdf_writer_contract_tests
cargo test -p flpdf-cli --test cli_full_rewrite
```

Expected: all commands exit successfully. These suites confirm that deleting the stale matrix did not remove or bypass the current writer coverage.

- [ ] **Step 2: Run the complete workspace suite**

Run:

```bash
cargo test --workspace
```

Expected: the workspace compiles and every test passes; the deleted integration test is not compiled or run.

- [ ] **Step 3: Inspect the final diff and update Beads**

Run:

```bash
git diff --stat main...HEAD
git diff --name-status main...HEAD
git status --short --branch
bd show flpdf-25kg.6.2.1 --json
```

Expected: the branch diff contains only the design/plan documentation and the two stale-file deletions; the worktree has no unintended files; the Bead remains claimed and records the verified scope.

- [ ] **Step 4: Push and create the new pull request**

Run:

```bash
bd close flpdf-25kg.6.2.1 --reason="Removed stale incremental matrix and obsolete plan after PdfWriter cutover; workspace tests pass."
bd dolt push
git push -u origin fix/flpdf-25kg-6-2-1-stale-incremental-matrix
```

Create a pull request against `main` titled `Remove stale incremental matrix after PdfWriter cutover`. The body must link `flpdf-25kg.6.2.1`, cite the post-merge ordering conflict between PR #710 and PR #716, list the two deleted files, and include the successful formatting, focused, and workspace test commands.
