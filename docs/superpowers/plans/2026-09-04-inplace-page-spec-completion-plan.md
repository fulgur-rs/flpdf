# InPlace page-spec completion ownership Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make both `PageSpecJobOutput::InPlace` consumers use one qpdf-shaped page-selection completion boundary.

**Architecture:** Add `QPDFJob::complete_in_place_page_selection` beside `handle_page_specs` in `job/page_specs.rs`. It owns navigation remapping, the existing qpdf-backed structural-reference drops, subset pruning, and AcroForm pruning in one order. The core lifecycle and CLI call it exactly once, then retain their distinct rotation parsing and document/output stages. No callback, sentinel, compatibility bridge, or full CLI-to-QPDFJob migration is introduced.

**Tech Stack:** Rust workspace, `flpdf`/`flpdf-cli`, pinned qpdf 11.9.0 source, qpdf differential tests, separate `flpdf-qtest` harness, Cargo, rustdoc, Clippy, llvm-cov.

---

### Task 1: Record the design and RED tests

**Files:**
- Create: `docs/superpowers/specs/2026-09-04-inplace-page-spec-completion-design.md`
- Modify: `crates/flpdf/tests/page_job_route_cutover_tests.rs`
- Modify: `crates/flpdf/tests/page_extract_thread_bead_p_tests.rs`

- [x] **Step 1: Write the qpdf-derived design**

Record qpdf's single `QPDFJob::createQPDF` order: page specs, rotations,
underlay/overlay, and transformations (`libqpdf/QPDFJob.cc:428-535`). Record
that `handlePageSpecs` owns page-subset mutation (`:2360-2632`) and that the
shared flpdf helper owns page completion only. CLI output/writer and QPDFJob
transformation/inspection stages remain their respective owners.

- [x] **Step 2: Add the source route-lock test**

Inspect both `run_page_extraction_after_plan` and `run_document_erased` and
require `complete_in_place_page_selection(` in each. Reject direct calls to
`remap_outline_and_dests`, `QPDFJob::prune_after_subset`, and
`QPDFJob::prune_acroform_after_subset` in either consumer. Assert that the
shared helper appears before the consumer's rotation call.

- [x] **Step 3: Add the QPDFJob behavioral regression**

Write the existing three-page article-thread fixture, run a JSON job selecting
pages `1,3` with `removeUnreferencedResources: yes`, reopen its output, and
assert that bead `12 0 R` remains a live dictionary with `/P` removed. The
pre-helper QPDFJob InPlace route must fail this assertion by nulling bead 12.

- [x] **Step 4: Run RED and commit**

Run:

```bash
cargo test -p flpdf --test page_job_route_cutover_tests
cargo test -p flpdf --test page_extract_thread_bead_p_tests qpdf_job_in_place_page_selection_drops_dangling_bead_p
```

Expected: route-lock fails on the missing helper and the behavioral test fails
because bead 12 is not a dictionary. These must be assertion failures, not
compilation or fixture failures. Commit the design and tests with:
`git commit -m "test: lock in-place page completion boundary"`.

### Task 2: Add and consume the shared completion boundary

**Files:**
- Modify: `crates/flpdf/src/job/page_specs.rs`
- Modify: `crates/flpdf/src/job/lifecycle.rs`
- Modify: `crates/flpdf-cli/src/main.rs`

- [ ] **Step 1: Add the job-owned helper**

Add this operation beside `QPDFJob::handle_page_specs` in `page_specs.rs`:

```rust
pub fn complete_in_place_page_selection<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    prune_mode: RemoveUnreferencedResources,
) -> Result<()> {
    super::outline_dest_remap::remap_outline_and_dests(pdf, result)?;
    let objr_obj_targets = crate::drop_struct_elem_dangling_pg(pdf, result)?;
    crate::drop_thread_bead_dangling_p(pdf, result)?;
    crate::drop_objr_obj_annot_dangling_p(pdf, result, &objr_obj_targets)?;
    super::page_subset::prune_after_subset(pdf, prune_mode)?;
    super::acroform_field_prune::prune_acroform_after_subset(pdf, result)
}
```

Document the qpdf source boundaries and the fact that route-specific output
stages are not part of this helper. Leave the existing public prune wrappers
for other consumers unchanged.

- [ ] **Step 2: Replace the QPDFJob InPlace arm**

Replace the direct remap/subset/AcroForm calls in `run_document_erased` with:

```rust
QPDFJob::complete_in_place_page_selection(pdf, &result, prune_mode)?;
self.apply_configured_rotations(pdf, configuration)?;
self.run_document_stages(pdf, configuration)
```

Do not change the `Merged` arm or later QPDFJob transformation/inspection
ordering.

- [ ] **Step 3: Replace the CLI InPlace arm**

In `run_page_extraction_after_plan`, replace direct page completion calls with:

```rust
QPDFJob::complete_in_place_page_selection(pdf, &result, prune_mode)?;
apply_rotate_specs(pdf, &page_ops.rotate, &result.new_kids)?;
```

Move the existing CLI rotation call below the helper. Preserve image,
overlay, split, writer, and warning-completion code after it. Remove only
imports that are proven unused.

- [ ] **Step 4: Run GREEN focused checks**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test page_job_route_cutover_tests
cargo test -p flpdf --test page_extract_thread_bead_p_tests
cargo test -p flpdf --test job_lifecycle_tests json_job_run_in_place_page_subset_remaps_outline_dests
```

Expected: route-lock passes, all article-thread tests pass, bead 12 remains a
dictionary without `/P`, and the outline InPlace regression remains green.

### Task 3: Route tests through the boundary and update correspondence

**Files:**
- Modify: `crates/flpdf/tests/page_extract_thread_bead_p_tests.rs`
- Modify: `docs/qpdf-correspondence.md`
- Modify: `docs/superpowers/specs/2026-09-04-inplace-page-spec-completion-design.md`

- [ ] **Step 1: Use the helper in the direct fixture harness**

Change `run_subset_bytes` to call
`QPDFJob::complete_in_place_page_selection` once after `rebuild_page_tree`.
Remove only its direct individual remap/drop/prune calls and unused imports.
Keep all existing structural assertions.

- [ ] **Step 2: Update qpdf correspondence**

Extend the QPDFJob row near `docs/qpdf-correspondence.md:263` with the shared
helper and citations `QPDFJob.cc:428-535,2137-2210,2360-2632`. State that CLI
writer/output stages remain outside the helper. Add no deviation marker and
do not call the CLI output owner qpdf's `handleTransformations`.

- [ ] **Step 3: Run scope/document checks**

Run:

```bash
rg -n "complete_in_place_page_selection|remap_outline_and_dests|prune_after_subset|prune_acroform_after_subset" crates/flpdf/src/job/lifecycle.rs crates/flpdf/src/job/page_specs.rs crates/flpdf-cli/src/main.rs
git diff --check
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
```

Expected: both production consumers call the helper; direct completion calls
are confined to the helper; documentation checkers pass; no writer, NameTree,
fixture, bridge, or deviation-marker file is changed.

### Task 4: Run complete local and qtest gates

**Files:** Verify the complete worktree; do not add qtest fixtures.

- [ ] **Step 1: Build and run qtest**

Run in the worktree:

```bash
cargo build --release --workspace --features qpdf-zlib-compat
```

Then from `/home/ubuntu/flpdf-qtest` run:

```bash
FLPDF_DIR=/home/ubuntu/flpdf/.worktrees/flpdf-4wq4 QTEST_FULL=1 ./scripts/run.sh
```

Inspect the same-run `survey/latest/harness.log` and
`survey/latest/qtest-results.xml`. Require allowlist regressions 0, missing
cases 0, parity validation errors 0, and the relevant page/type-check cases
passing. Record any raw non-allowlisted informational failures separately.

- [ ] **Step 2: Run focused, crate, CLI, and workspace tests**

Run:

```bash
cargo test -p flpdf --lib page_specs
cargo test -p flpdf --test page_job_route_cutover_tests
cargo test -p flpdf --test page_extract_thread_bead_p_tests
cargo test -p flpdf --test job_lifecycle_tests
cargo test -p flpdf-cli --test page_ops_qpdf_matrix
cargo test -p flpdf-cli --test cli_tests
cargo test -p flpdf
cargo test --workspace
```

Every command must exit 0 with no newly ignored test.

- [ ] **Step 3: Run quality and coverage gates**

Run:

```bash
cargo fmt --all -- --check
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
git diff --check
```

Require all changed executable lines under `crates/flpdf/src` and the CLI
report to be covered. Do not use `cov:ignore` for reachable helper behavior.

### Task 5: Review, publish, and persist without merging

**Files:** Verify git, GitHub, and Beads state; preserve unrelated worktrees.

- [ ] **Step 1: Request independent review**

Review `origin/main..HEAD` against pinned qpdf 11.9.0. Check helper order,
structural cleanup ownership, rotation-after-helper behavior, QPDFJob/CLI
scope, and tests. Fix Critical/Important findings with RED tests and rerun
affected gates.

- [ ] **Step 2: Rebase and run fresh checks**

Run:

```bash
git fetch origin main
git rebase origin/main
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Confirm the branch is clean and patch coverage uses current-head LCOV.

- [ ] **Step 3: Push and create Draft PR**

Push `feature/flpdf-4wq4-inplace-completion` and create a Draft PR against
`main`. Include qpdf source/probe evidence, helper boundary, same-run qtest
artifact pair, local gates, and patch coverage. Do not merge or alter qtest
fixtures.

- [ ] **Step 4: Wait for CI and mark Ready**

Monitor Analyze, Quality, Coverage, codecov/patch, Fuzz, Release, label,
release approval, and every platform test. Freshly verify all checks and PR
base/head/body/state, then run `gh pr ready <actual-number>`. Leave it open.

- [ ] **Step 5: Append Beads evidence and push state**

Read back `flpdf-4wq4`, append commit/PR, review, RED/GREEN, qtest artifact
pair, local/CI gates, and patch coverage. Run `bd dep cycles`, `bd dolt push`,
and `git push`; require `No dependency cycles detected`, `Push complete.`, and
a successful git push. Keep the issue `in_progress` for integration.
