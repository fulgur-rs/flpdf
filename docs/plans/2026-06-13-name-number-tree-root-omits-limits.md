# name/number tree root omits /Limits — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `build_name_tree` / `build_number_tree` emit a **root node without `/Limits`** (single-node root → `/Names`|`/Nums` only; multi-node root → `/Kids` only), matching ISO 32000-2 §7.9.6/7.9.7 (`/Limits` is intermediate/leaf only) and qpdf 11.9.0. Leaves keep `/Limits`.

**Architecture:** Two builders in `crates/flpdf/src/name_number_tree.rs` currently add `/Limits` to the root in both the single-leaf-is-root branch (via `build_leaf_dict`/`build_num_leaf_dict`) and the multi-node branch (explicit insert). Remove `/Limits` from the root in both, for both trees, by factoring the pair-array construction so the root reuses it *without* `/Limits`. The reader (`walk_tree`) never reads `/Limits`, so reading is unaffected. The only callers are PageLabels (`build_number_tree`) and EmbeddedFiles (`build_name_tree`); named-destinations (`/Dests`) do **not** go through these builders, so merge/extract `/Dests` tests are out of scope.

**Tech Stack:** Rust, `crates/flpdf/src/name_number_tree.rs`, `crates/flpdf/src/embedded_files.rs` (doc only), `crates/flpdf/tests/helper_api_tests.rs` (byte-identity round-trip), `crates/flpdf/tests/embedded_files_tests.rs`. qpdf 11.9.0 oracle.

**Ground truth (measured qpdf 11.9.0):** single-attachment EmbeddedFiles tree root = `<< /Names [...] >>` (no `/Limits`); 40-attachment root = `<< /Kids [...] >>` (no `/Limits`), each leaf `<< /Limits [..] /Names [..] >>`. ISO 32000-2 §7.9.6/7.9.7: `/Limits` on intermediate + leaf nodes only, never the root.

**Reference before coding:** `.claude/rules/pdf-rust-review-patterns.md` (#1 no needless clone) and `.claude/rules/pdf-rust-doc-review-patterns.md` (public `///`/`//!`: English, no issue IDs, justify with spec not "validators").

**Blast radius (verified):**
- Fix: `name_number_tree.rs` `build_name_tree` (single 137-141 / multi 153-158) + `build_number_tree` (single 206-210 / multi 222-227).
- Unit tests pinning current behavior: `name_number_tree.rs:711` (single name root has /Limits), `:733-736` (multi: every node has /Limits), `:901` (single number root /Limits), `:923-925` (multi: every node has /Limits).
- Byte-identity round-trip tests (manual side hand-builds single-leaf root WITH /Limits — must drop it to match new helper output): `helper_api_tests.rs` `manual_set_pagelabels_leaf` (825-828) and `attachment_insert_embedded_file_matches_manual_name_tree` manual leaf (963-969), plus their doc comments (811-814, 842, 891, 938).
- Docs asserting wrong behavior: `name_number_tree.rs:112-116`; `embedded_files.rs:22, 40-44, 545, 548` ("Every node carries /Limits as required by validators", "single-leaf root (just /Names + /Limits)").
- **Unaffected (confirmed):** `embedded_files_tests.rs:505-555` (asserts root `/Kids` + no `/Names` + LEAF `/Limits` — all still true); `page_merge_tests.rs` / `page_extract_outline_nullout_tests.rs` (`/Dests` input-tree preservation, different code path).

---

## Task 1: Root omits /Limits in both builders + reconcile all coupled tests/docs

This is one cohesive semantic change: the fix necessarily updates the unit tests and the byte-identity round-trip tests that pin the old output, so they change atomically and the suite stays green.

**Files:**
- Modify: `crates/flpdf/src/name_number_tree.rs` (builders + unit tests + module/fn doc)
- Modify: `crates/flpdf/tests/helper_api_tests.rs` (manual sides of two byte-identity tests + their doc comments)

**Step 1: Update the pinning unit tests FIRST (TDD red).** In `name_number_tree.rs` tests:

- `build_name_tree_single_leaf_no_kids` (~711): change `assert!(d.get("Limits").is_some());` to:
  ```rust
  assert!(
      d.get("Limits").is_none(),
      "single-node root omits /Limits (ISO 32000-2 7.9.6; qpdf)"
  );
  ```
  (keep the `/Names is_some` and `/Kids is_none` asserts.)
- `build_name_tree_multi_leaf_root_kids_alloc_order` (~733-736): replace the "every node carries /Limits" loop with: root (`nodes[2]`) has **no** `/Limits`; both leaves (`nodes[0]`, `nodes[1]`) **have** `/Limits`:
  ```rust
  assert!(root_dict.get("Limits").is_none(), "multi-node root omits /Limits");
  for (_, leaf) in &nodes[..2] {
      assert!(leaf.as_dict().unwrap().get("Limits").is_some(), "leaf keeps /Limits");
  }
  ```
- `build_number_tree_single_leaf_no_kids` (~901): the test currently does `let lim = d.get("Limits").and_then(Object::as_array).expect("limits");` and checks `[0,20]`. Replace with `assert!(d.get("Limits").is_none(), ...)` and keep `/Nums is_some`. (Drop the `[0,20]` limit-value assertion — the root no longer has limits.)
- `build_number_tree_multi_leaf_root_kids_alloc_order` (~923-925): same shape as the name multi test — root no `/Limits`, leaves have integer `/Limits`.

Run: `cargo test -p flpdf --lib name_number_tree::tests` → these 4 FAIL (current builders still add root /Limits).

**Step 2: Implement the fix.** Factor the pair-array construction so the root can reuse it without `/Limits` (avoids building a throwaway `/Limits` array — cleaner than build-then-remove):

```rust
/// The `/Names` value: a flat `[key1 val1 key2 val2 ...]` array.
fn name_pairs(entries: &[(Vec<u8>, Object)]) -> Vec<Object> {
    let mut pairs = Vec::with_capacity(entries.len() * 2);
    for (key, val) in entries {
        pairs.push(Object::String(key.clone()));
        pairs.push(val.clone());
    }
    pairs
}
```
Rewrite `build_leaf_dict` to use it (still `/Limits` + `/Names`). Then the single-node branch of `build_name_tree` (137-141):
```rust
if entries.len() <= LEAF_MAX {
    let root_ref = alloc();
    // Root node omits /Limits: ISO 32000-2 7.9.6 restricts /Limits to
    // intermediate and leaf nodes. qpdf emits a single-node name-tree root as
    // /Names only.
    let mut root = Dictionary::new();
    root.insert("Names", Object::Array(name_pairs(entries)));
    nodes.push((root_ref, Object::Dictionary(root)));
    return (root_ref, nodes);
}
```
And the multi-node branch (153-158): delete the `root.insert("Limits", ...)` lines; build the root with `/Kids` only. Leaves still come from `build_leaf_dict` (with `/Limits`).

Do the analogous edit for `build_number_tree`: add `fn num_pairs(entries: &[(i64, Object)]) -> Vec<Object>` (Integer keys), single-node root = `/Nums` only, multi-node root = `/Kids` only, leaves via `build_num_leaf_dict` (with integer `/Limits`).

> Verify `Dictionary::new()` / `.insert()` are the real API (they are, used throughout this file). Do NOT remove `build_leaf_dict`/`build_num_leaf_dict` — still used for leaves.

Run: `cargo test -p flpdf --lib name_number_tree::tests` → PASS.

**Step 3: Update `name_number_tree.rs` module/fn docs** (112-116 and the `build_number_tree` analogue 184-189): describe single-node root as `/Names`|`/Nums` only and multi-node root as `/Kids` only (no `/Limits`); leaves carry `/Limits`. Cite ISO 32000-2 §7.9.6/7.9.7. English, no issue IDs.

**Step 4: Reconcile the byte-identity round-trip tests** in `helper_api_tests.rs` (the public PageLabels/EmbeddedFiles helpers now emit a single-leaf root WITHOUT /Limits, so the *manual* side must match or byte-identity fails):
- `manual_set_pagelabels_leaf` (824-830): remove the `leaf.insert("Limits", ...)` (825-828); the manual single-leaf root becomes `<< /Nums [...] >>` only. The `first`/`last` params are now unused — drop them from the fn signature and update the two call sites (`page_label_set_range_matches_manual_nums_rebuild` ~881, `page_label_remove_range_matches_manual_nums_shrink` ~907).
- `attachment_insert_embedded_file_matches_manual_name_tree` (962-976): remove the `leaf.insert("Limits", ...)` (963-969) so the manual single-leaf root is `<< /Names [...] >>` only.
- Update the doc comments that describe the emitted shape (811-814, 842, 891, 938) to drop `/Limits` from the single-leaf root description.

Run: `cargo test -p flpdf --test helper_api_tests` → PASS (byte-identity holds again; this is the end-to-end proof the fix propagates through the public helpers). Also run `cargo test -p flpdf --lib name_number_tree::tests` again.

**Step 5: fmt/clippy + commit.**
```bash
cargo fmt --all && cargo fmt --all --check
cargo clippy -p flpdf --all-targets -- -D warnings
git add crates/flpdf/src/name_number_tree.rs crates/flpdf/tests/helper_api_tests.rs
git commit -m "fix(flpdf): name/number tree root omits /Limits (ISO 32000-2 7.9.6/7.9.7, qpdf parity)"
```

---

## Task 2: Caller doc correction + explicit conformance/parity tests

**Files:**
- Modify: `crates/flpdf/src/embedded_files.rs` (doc only)
- Modify: `crates/flpdf/tests/embedded_files_tests.rs` (add a focused assertion)

**Step 1: Correct the `embedded_files.rs` module/helper doc.** Replace the "validator compat" rationale (22, 40-44, 545, 548) with the spec-grounded statement: leaves and intermediate nodes carry `/Limits`; the **root node omits it** (ISO 32000-2 §7.9.6; qpdf observed behavior). Remove the phrase "as required by validators" and "single-leaf root (just /Names + /Limits)". Keep it English, no issue IDs, justify with spec (doc rule #1).

**Step 2: Add an explicit conformance test** for the single-entry case (the issue's core), since the existing `embedded_files_tests.rs:505-555` only covers the multi-node leaf. Add a test that inserts ONE attachment and asserts the EmbeddedFiles name-tree **root** dict has `/Names` and **no** `/Limits` (and no `/Kids`). Model it on the existing multi-node test's resolution of `ef_root`; reuse helpers there. Pseudocode:
```rust
#[test]
fn single_attachment_embeddedfiles_root_has_no_limits() {
    // build a 1-page PDF, insert one embedded file, then resolve
    // catalog /Names /EmbeddedFiles and assert the root node shape.
    // ... (reuse the file's existing setup helpers)
    assert!(ef_root.get("Names").is_some(), "single-node root is /Names");
    assert!(ef_root.get("Limits").is_none(), "root omits /Limits (qpdf/ISO 32000-2 7.9.6)");
    assert!(ef_root.get("Kids").is_none(), "single node is not a /Kids root");
}
```
> If a PageLabels equivalent is cheap with existing helpers, add the number-tree analogue (single-range PageLabels root has `/Nums`, no `/Limits`). If it requires heavy scaffolding, the `build_number_tree_single_leaf_no_kids` unit test (Task 1) already pins the number-tree root; note that and skip.

Run: `cargo test -p flpdf --test embedded_files_tests` → PASS.

**Step 3: Commit.**
```bash
git add crates/flpdf/src/embedded_files.rs crates/flpdf/tests/embedded_files_tests.rs
git commit -m "test+doc(flpdf): EmbeddedFiles single-node root omits /Limits; correct validator-compat doc"
```

---

## Task 3: Quality gates + qpdf parity smoke

**Step 1: Full gates.**
```bash
cargo fmt --all --check
cargo clippy -p flpdf -p flpdf-cli --all-targets -- -D warnings
cargo test -p flpdf --doc
cargo test -p flpdf -p flpdf-cli
```
Expected: all clean/green. Doc-hygiene grep: `grep -rnE '(///|//!).*(flpdf-[0-9a-z.]+|[ぁ-んァ-ヶ一-龠])' crates/flpdf/src/name_number_tree.rs crates/flpdf/src/embedded_files.rs` → 0.

**Step 2: qpdf parity smoke (end-to-end).** Build the CLI, take a PDF flpdf writes with a single-entry EmbeddedFiles or PageLabels tree (e.g. via a small test program or an existing fixture), and confirm flpdf's emitted tree root has no `/Limits` — structurally matching `qpdf --add-attachment` output (root = `/Names` only). Document the comparison in the PR. (If wiring a CLI path is heavy, the Task 2 conformance test + the qpdf measurements already in the issue design suffice — note that.)

**Step 3: Patch coverage (commit first).**
```bash
git status   # clean
scripts/patch-coverage.sh --base main
```
Expected: flpdf changed lines uncovered = 0. Add tests for any uncovered changed line (e.g. the new `name_pairs`/`num_pairs` helpers must be exercised — they are, via the builders). No `cov:ignore` for reachable lines.

**Step 4: Qualitative check.** Confirm tests assert the *root omits /Limits* AND *leaves keep /Limits* for both single-node and multi-node, both name and number trees — not just line execution.

---

## Notes for the implementer
- **No needless clones (rule #1):** `name_pairs`/`num_pairs` clone keys/values because the entries are borrowed `&[(K, Object)]` and the dict needs owned objects — unavoidable, same as the existing `build_leaf_dict`. Do not add clones beyond that.
- **Reader untouched:** do not modify `walk_tree`/`read_*`; they never read `/Limits`, and input-tree `/Limits` preservation must not change.
- **Scope:** only `build_name_tree`/`build_number_tree` output changes. Do not touch `/Dests` handling or the merge/extract paths.
- **Doc rule:** justify the root-omits-/Limits behavior with ISO 32000-2 §7.9.6/7.9.7 and qpdf observed behavior — never "as required by validators".
