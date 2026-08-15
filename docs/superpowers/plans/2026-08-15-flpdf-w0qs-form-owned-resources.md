# Form-Owned Resources Pruning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the extraction resource-pruning route so Form XObjects' own `/Font` and `/XObject` dictionaries are pruned with qpdf 11.9.0 semantics.

**Architecture:** Keep `resources.rs` as the production owner of the existing extraction route. During the existing bounded Form traversal, retain direct names for each own-resource Form separately from page usage, then shallow-copy and prune only that Form's `/Font` and `/XObject` dictionaries immediately after its own content is completely tokenised and before queued child Forms are visited, matching qpdf's parent-first helper order. Resource-less Forms continue to bubble unresolved names to the page, and malformed Forms remain conservatively unmodified.

**Tech Stack:** Rust workspace, `cargo test`, qpdf 11.9.0 pinned source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`, `/usr/bin/qpdf` live oracle, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are authoritative.
- qpdf `QPDFPageObjectHelper.cc:539-633,636-649` owns the `/Font` and `/XObject` Form/page pruning contract.
- Do not change the writer encryption path, ObjectHandle consumer cutovers, legacy bridges, or the separate `PageDocumentHelper` implementation already covered by `flpdf-3yn9.1.1`.
- Preserve the existing `Auto`/`Yes`/`No` gate, ever-seen traversal bound, inherited/resource-less Form behavior, and conservative failure behavior.
- Change only `crates/flpdf/src/resources.rs`, `crates/flpdf/tests/resource_pruning_tests.rs`, and this plan; do not modify active-worktree files.

### Task 1: Establish the qpdf-backed RED regression

**Files:**
- Modify: `crates/flpdf/tests/resource_pruning_tests.rs`

**Interfaces:**
- Consumes: `remove_unreferenced_resources`, the existing raw-PDF builders, and `Object::Stream` inspection helpers.
- Produces: failing regressions proving that an own-resource Form is pruned and that malformed own-resource content is retained.

- [x] **Step 1: Add the direct own-resource regression.** Build one page whose content invokes a Form with its own `/Resources /Font` containing used `F1` and unused `F2`; call `remove_unreferenced_resources(..., Yes)` and assert the Form keeps `F1`, removes `F2`, and the page keeps the invoked `/XObject` entry.
- [x] **Step 2: Add the Auto and shared-category regressions.** Verify an unshared Form is pruned under `Auto`, and a Form whose own `/Font` points at a page-shared indirect category gets a private shallow copy while the page's category remains independently owned.
- [x] **Step 3: Add the failure-path regression.** Give an own-resource Form undecodable or malformed content and assert both of its font entries remain unchanged; also cover qpdf's unresolved own Font-name guard.
- [x] **Step 4: Run the focused tests and confirm RED.** The new direct and Auto tests first failed because the Form still contained the unused resources; after implementation `resource_pruning_tests` passed.

### Task 2: Implement the minimal qpdf-shaped scope and write-back

**Files:**
- Modify: `crates/flpdf/src/resources.rs`

**Interfaces:**
- Consumes: the existing `CollectCtx`, `Scope`, `collect_from_stream`, `recurse_form_xobject`, `prune_font_and_xobject_dictionaries`, and `Pdf::set_object`.
- Produces: own-Form usage tracking and canonical stream write-back with no new public API.

- [x] **Step 1: Add explicit usage ownership.** Add a small `UsedTarget` enum (`Page` or `Form(ObjectRef)`) and a per-page `form_used` map to `CollectCtx`; record complete direct names into the page accumulator or the owning Form accumulator. Keep resource-less nested Forms targeting the page so qpdf's unresolved-name protection remains unchanged.
- [x] **Step 2: Prune only complete own-resource Forms.** After the Form's own content is complete and before visiting queued child Forms, take its recorded direct names, call the existing qpdf-shaped `/Font` and `/XObject` shallow-copy helper, replace the Form's `/Resources` with the copied dictionary, and write the updated `Stream` back through `Pdf::set_object`.
- [x] **Step 3: Preserve failure and scope boundaries.** Do not mutate a Form when decoding/tokenisation is incomplete or when an own Font/XObject name is unresolved; treat null/non-dictionary `/Resources` as resource-less; leave other resource categories and the existing page-level `apply_pruning` behavior untouched.
- [x] **Step 4: Record the oracle boundary in module documentation.** Cite `QPDFPageObjectHelper.cc:539-633` for helper behavior and `:636-649` for Form-before-page traversal, and state why the `PageDocumentHelper` route is not changed here.
- [x] **Step 5: Run the focused tests and confirm GREEN.** Run `cargo test -p flpdf --test resource_pruning_tests` and verify the new regressions plus all existing resource-pruning tests pass.

### Task 3: Differential and quality verification

**Files:**
- Modify: `crates/flpdf/tests/resource_pruning_tests.rs` only if a verified oracle edge case requires a test correction.

- [x] **Step 1: Run a real qpdf 11.9.0 probe** for the direct extraction route; indirect/shared, inherited/resource-less, unresolved, and malformed cases are covered by the differential regression suite. Record the command, exit status, stderr, and resulting `/Font`/`/XObject` observations below.
- [x] **Step 2: Run focused checks:** `cargo test -p flpdf --test resource_pruning_tests` (56), relevant `page_document_helper_tests` (64), `xref_tests` (109), and the qpdf-zlib compatibility differential (56).
- [x] **Step 3: Run repository gates:** `cargo fmt --all -- --check`, strict private rustdoc, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace` (all passed).
- [x] **Step 4: Run coverage and hygiene:** fresh `cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov` plus `scripts/patch-coverage.sh --base origin/main`, and `git diff --check`; patch coverage passed 29 changed executable lines, 0 uncovered (100%).
- [x] **Step 5: Perform an independent qpdf-parity review** against the pinned source and inspect every changed line for scope creep, shared-resource mutation, failure-path regressions, and accidental active-worktree overlap; the review's parent-order and shared-category findings were verified and fixed.

### Task 4: Commit, PR, CI, and Beads handoff

**Files:**
- Modify: Beads issue `flpdf-w0qs` notes only after verification.

- [ ] **Step 1: Read back the final diff and status; commit only the intended implementation, tests, and evidence documentation.**
- [ ] **Step 2: Push `feature/flpdf-w0qs-form-resources` and create a draft PR against `main`.**
- [ ] **Step 3: Read back PR metadata and wait for every CI check to pass; investigate failures with qpdf evidence before changing code.**
- [ ] **Step 4: Run `gh pr ready <number>` only after all CI checks succeed, then read back PR, CI, tests, qpdf citations, and changed files.**
- [ ] **Step 5: Append PR/commit/verification evidence to Beads, run `bd dep cycles`, run `bd dolt push`, and stop with the ready PR unless the user supplies merge authority.**

## Verification record (2026-08-15)

- Pinned source: `/home/ubuntu/.cache/flpdf/qpdf-11.9.0/libqpdf/QPDFPageObjectHelper.cc:318-343` discovers parent Forms before queued children; `:539-633` parses each Form in its own scope, shallow-copies only `/Font` and `/XObject`, rejects unresolved own Font/XObject names, and avoids mutation after parse failure; `:636-649` runs the Form pre-pass before the page helper without rolling back a successful parent action.
- Live probe input: `/tmp/flpdf-w0qs-qpdf-form-owned.pdf`; `qpdf --check` exited 0 with no warnings. Extraction probe: `qpdf --remove-unreferenced-resources=yes /tmp/flpdf-w0qs-qpdf-form-owned.pdf --pages . 1 -- /tmp/flpdf-w0qs-qpdf-form-owned-extracted.pdf`, followed by `qpdf --qdf --object-streams=disable ...`. The output retained Form `/Font/F1` and `/XObject/Used`, removed `/Font/F2` and `/XObject/Unused`, and retained the page `/XObject/Fm0`.
- Differential tests cover direct and Auto pruning, a true shared indirect `/Font` category shallow-copy, parent-before-child pruning when a child decode fails, resource-less/inherited scope, unresolved own names, malformed/decode failure retention, null/non-dictionary resources, and no-op mode.
- Fresh patch coverage: `scripts/patch-coverage.sh --base origin/main` passed `flpdf: changed 29, uncovered 0` and `report: changed 0, uncovered 0`.
