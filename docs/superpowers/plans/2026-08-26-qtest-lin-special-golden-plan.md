# qtest lin-special golden completion Implementation Plan

> For agentic workers: use the executing-plans or subagent-driven-development workflow. Steps use checkbox syntax for tracking.

**Goal:** Make qtest linearization.test rows 23, 29, and 35 byte-identical to qpdf 11.9.0.

**Architecture:** Snapshot qpdf's Generate ObjStm eligibility before optimization can mint inherited attributes, carry it with the optimization user map, and use it for the linearization even split. Reorder the physical part9 list so the Pages tree is emitted before the remaining lc_other objects; keep the existing RenumberMap and writer on that one ordered plan.

**Tech Stack:** Rust workspace linearization planner/writer, qpdf 11.9.0 source, qpdf CLI probes, flpdf-qtest qtest-driver and paired golden artifacts.

---

### Task 1: Add and run the RED regression tests

**Files:**
- Modify: crates/flpdf/src/linearization/plan.rs

- [ ] Step 1: Test Pages-first physical plan order.

Construct a LinearizationPlan with pages_tree_ref = Some(3 0 R) and
part4_rest = [2 0 R, 3 0 R]. Assert part4_objects() returns [3 0 R, 2 0 R].
The current implementation returns source order, so this must fail before the
fix.

- [ ] Step 2: Test post-optimization inherited arrays.

Build a two-page classic PDF whose Pages node owns a direct /MediaBox array.
Create a Generate plan, obtain objstm_membership_linearized, resolve both page
/MediaBox references, and assert neither reference appears in any generated
ObjStm batch. The current post-optimization eligibility walk puts the minted
arrays into a batch, so this must fail before the fix.

- [ ] Step 3: Run both tests and record the expected RED.

Run:

    cargo test -p flpdf --lib linearization::plan::tests::part4_objects_places_pages_tree_before_other_objects -- --exact
    cargo test -p flpdf --lib linearization::plan::tests::generated_objstm_membership_excludes_post_optimization_inherited_arrays -- --exact

Expected: both tests fail for the two independent qpdf gaps. Do not change
production code before this failure evidence exists.

### Task 2: Snapshot Generate eligibility at the qpdf setup boundary

**Files:**
- Modify: crates/flpdf/src/optimization.rs
- Modify: crates/flpdf/src/linearization/plan.rs
- Modify: crates/flpdf/src/linearization/writer.rs

- [ ] Step 1: Add an optional pre-optimization eligibility snapshot.

Extend the existing Optimization snapshot with an optional set or ordered list
of Generate-eligible object refs. Keep Optimization::default() empty so
hand-built unit plans retain their current behavior.

- [ ] Step 2: Capture the set before inherited-attribute optimization.

In LinearizationPlan::from_pdf_with_writer_options, when the effective mode is
Generate, call the existing qpdf-shaped compressible-object traversal before
Optimization::optimize can mint indirect inherited attributes. Store the result
on the returned Optimization value; Preserve and Disable do not need this
snapshot.

- [ ] Step 3: Use the snapshot for the global even split.

Add an internal membership helper that accepts an optional eligibility override.
The production Generate route passes the pre-optimization snapshot, filters to
renumber-assigned refs, performs qpdf's fixed even split, and only then erases
page dictionaries and the Catalog. Existing unit-only callers without a
snapshot continue to call the current traversal wrapper.

- [ ] Step 4: Run the focused RED test GREEN.

Run:

    cargo test -p flpdf --lib linearization::plan::tests::generated_objstm_membership_excludes_post_optimization_inherited_arrays -- --exact
    cargo test -p flpdf --lib linearization::plan::tests::linearized_membership_even_splits_then_erases_page_dicts_and_root

Expected: the inherited arrays stay plain and the existing global split test
continues to pass.

### Task 3: Enforce qpdf Pages-first part9 order

**Files:**
- Modify: crates/flpdf/src/linearization/plan.rs
- Test: crates/flpdf/src/linearization/plan.rs

- [ ] Step 1: Reorder the plan's physical part4 view.

When pages_tree_ref is present in part4_rest, remove that exact ref from its
current position and insert it at index zero. Preserve the order of all
remaining part4 sub-partitions and keep outline extraction after this qpdf
part9-head normalization.

- [ ] Step 2: Keep renumber and emission aligned.

Verify RenumberMap::from_plan and the writer both consume the normalized
Pages-first list; do not add a second writer-only sort or change object IDs at
serialization time.

- [ ] Step 3: Run the Pages-first test GREEN.

Run:

    cargo test -p flpdf --lib linearization::plan::tests::part4_objects_places_pages_tree_before_other_objects -- --exact

### Task 4: Verify exact qpdf output

**Files:**
- Test: qtest paired artifacts; no vendored source changes

- [ ] Step 1: Run focused Rust tests.

Run:

    cargo fmt --all -- --check
    cargo test -p flpdf --lib linearization::plan::tests
    cargo test -p flpdf --test writer_tests
    cargo test -p flpdf --test linearize_classic_tests
    cargo test -p flpdf --test linearize_objstm_generate_tests
    cargo clippy --workspace --all-targets --all-features -- -D warnings

- [ ] Step 2: Run qtest with the CLI and shim PR branches.

Build the required release binaries, copy vendor/qpdf-qtest into a temporary
directory, put the qtest shim directory first in PATH, and run
TESTS=linearization qtest-driver with both FLPDF_CLI_BIN and
FLPDF_TEST_COMPARE_BIN set. Keep harness.log and qtest-results.xml from the
same invocation.

- [ ] Step 3: Require the exact completion evidence.

The qtest XML must report total-cases=309, passes=309, and failures=0. Rows 23,
29, and 35 must compare exactly to the three vendored qpdf golden files. Run
qpdf --check-linearization over each generated output as an additional
independent check.

- [ ] Step 4: Inspect, commit, and push the stacked writer slice.

Run:

    git diff --check
    git status --short --branch
    git diff --stat origin/main...HEAD

Commit only the planner/writer regression and implementation changes, then
push the .6.19 branch with its base recorded as the CLI PR branch.
