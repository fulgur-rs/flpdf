# qpdf xref bootstrap registration view implementation plan

> **For agentic workers:** Execute this plan task-by-task with RED→GREEN TDD.

**Goal:** Remove per-context cumulative xref registration clones from
`XrefReadContext` while preserving qpdf 11.9.0 active/previous and
reconstruction lookup semantics.

**Architecture:** Keep `XrefRegistration` as the sole cumulative owner. Give
`XrefReadContext` a borrowed lookup source: one map for active/previous
sections, and an exact-key-priority overlay for reconstruction. Scope the
context around xref-stream dictionary decoding so registration mutation occurs
after the borrow ends. Keep owned snapshots only at `LoadedXref` boundaries.

**Tech Stack:** Rust, `cargo test`, qpdf 11.9.0 source and installed qpdf
11.9.0 as the semantic oracle.

## Constraints

- Do not change `ResolverCore` or post-bootstrap `Pdf::resolve`.
- Do not change qpdf semantics, add a sentinel, or introduce an adapter.
- Preserve reconstruction line-scan precedence, including free tombstones.
- Preserve cache, recursion, diagnostics, repair-trigger, and final snapshot
  behavior.
- Preserve the user's unrelated untracked files on `main`.

## Task 1: Establish RED ownership/view coverage

**Files:**

- Modify: `crates/flpdf/src/xref.rs`

**Step 1: Add a focused failing test**

Add an internal test that constructs an active bootstrap context and asserts
that its entry source is a borrowed registration view, not an owned map. Add a
second focused assertion for reconstruction lookup precedence if the view API
exposes that distinction.

**Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p flpdf xref::tests::active_context_uses_borrowed_registration_view
```

Expected result: the test fails because the current context owns a cloned
`BTreeMap`/does not expose the borrowed source variant.

## Task 2: Implement the qpdf-shaped lookup view

**Files:**

- Modify: `crates/flpdf/src/xref.rs`

**Step 1: Introduce the private borrowed source representation**

Define a lifetime-bearing source enum and a lookup helper. Active and previous
contexts borrow `registration.entries`; reconstruction checks the line-scan
map first and then registration only for an absent exact key. Filter Free
entries at lookup so a line-scan Free entry remains a tombstone and all free
or missing references resolve to null.

**Step 2: Replace context construction clones**

Make `XrefReadContext` store the source view instead of an owned
`BTreeMap`. Remove the `snapshot()` calls from active/previous context
construction and the reconstruction map clone. Keep `snapshot()` for owned
`LoadedXref` state.

**Step 3: Run the focused test and verify GREEN**

Run the focused test from Task 1. It must pass, and the source must contain no
`registration.snapshot()` call inside `XrefReadContext::new`.

## Task 3: Split xref-stream context lifetime from registration mutation

**Files:**

- Modify: `crates/flpdf/src/xref.rs`
- Modify: `crates/flpdf/src/xref.rs` tests if behavior coverage needs a narrow
  regression case

**Step 1: Refactor `parse_xref_stream` into an inner context scope**

Read and resolve the xref stream object, `/Type`, `/Size`, `/W`, `/Index`, and
decoded entries while the borrowed context is alive. Return the decoded
entries, trailer, object metadata, and diagnostics/trigger state from the
scope. Insert entries into `XrefRegistration` only after the context is
dropped, preserving existing error and diagnostic ordering.

**Step 2: Run focused bootstrap and reconstruction tests**

Run:

```bash
cargo test -p flpdf --lib xref::tests
cargo test -p flpdf --test xref_tests
```

Expected result: all existing bootstrap, hybrid, `/Prev`, candidate
reconstruction, cache, cycle, free-entry, and diagnostic tests pass.

**Step 3: Add or tighten one behavior regression if needed**

Only if the refactor exposes a gap, add a real fixture-level test for the
affected qpdf behavior. Do not add tests that assert unrelated implementation
details.

## Task 4: Run qpdf parity and quality verification

**Files:**

- No additional files unless a regression test from Task 3 is required.

**Step 1: Format and focused quality checks**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test xref_tests
cargo test -p flpdf
```

**Step 2: Run qpdf compatibility coverage**

Run the relevant compatibility test suite and the documented CLI smoke check
when qpdf is installed:

```bash
cargo test -p flpdf-cli --test compat_matrix_tests
cargo run --bin flpdf -- --check tests/fixtures/minimal.pdf
```

Record skips or environment failures separately from code failures.

**Step 3: Inspect the final diff**

Verify only the intended source, tests, and the new spec/plan are changed in
the feature worktree. Confirm the main worktree's unrelated untracked files
remain untouched.

## Task 5: Commit and handoff readiness

**Step 1: Commit implementation**

Use a focused commit message after all verification passes:

```bash
git add crates/flpdf/src/xref.rs docs/superpowers/plans/2026-08-09-xref-bootstrap-registration-view.md
git commit -m "fix: borrow cumulative xref registration during bootstrap"
```

Include any intentionally added regression test in the same commit.

**Step 2: Read back Beads and branch state**

Run `bd show flpdf-hj45`, `git status --short --branch`, and the final test
commands. Do not close the issue until the implementation is reviewed and
merged.
