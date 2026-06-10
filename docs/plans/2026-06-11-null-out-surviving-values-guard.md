# null-out surviving-values guard (flpdf-9hc.20.36) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Harden the qpdf `--pages` null-out pass so a destination already remapped to a surviving page's new ref is never mistaken for a removed-page target and nulled.

**Architecture:** Introduce a `Surviving` struct bundling the old→new page map (`ref_map` first-occurrences) with the set of all rebuilt page refs (`new_kids`). The two null sites decide "removed?" via a single `is_surviving_target()` predicate (page is a remap key OR an output ref) instead of a key-only `contains_key`. Proposal B from the issue; Proposal A (shared `visited`) is insufficient because the shared object is the GoTo *action* object, which neither `visited` set tracks.

**Tech Stack:** Rust, `crates/flpdf/src/outline_dest_remap.rs`, `cargo test -p flpdf --lib`, `scripts/patch-coverage.sh`.

---

### Task 1: RED — hazard reproduction test (shared action object, non-identity remap)

**Files:**
- Test: `crates/flpdf/src/outline_dest_remap.rs` (`mod tests`)

**Step 1: Write the failing test**

Build a PDF where outline item `20 0 R` and link annotation `60 0 R` (on surviving page `99 0 R`) both carry `/A 50 0 R`, with `50 0 obj << /S /GoTo /D [4 0 R /Fit] >>`. Hand-build a **non-identity** `RebuildResult { new_kids: [99 0 R], ref_map: {4 0 R → [99 0 R]} }` (single-input `rebuild_page_tree` only ever yields identity, so this must be constructed directly — fields are `pub`). Call `remap_outline_and_dests`. Assert page `99 0 R` is **not** `Object::Null` afterward.

**Step 2: Run — expect FAIL**

Run: `cargo test -p flpdf --lib outline_dest_remap::tests::shared_goto_action_does_not_null_surviving_remapped_page -- --exact`
Expected: FAIL — Step 3 remaps obj50 `/D` to `[99 0 R]`, then Step 4 re-resolves it, `99` is not a `ref_map` key, and the page is nulled.

**Step 3: Commit the red test** (optional, may fold into Task 2 commit)

---

### Task 2: GREEN — `Surviving` struct + values guard

**Files:**
- Modify: `crates/flpdf/src/outline_dest_remap.rs`

**Step 1:** Add `struct Surviving { map: BTreeMap<ObjectRef, ObjectRef>, new_refs: BTreeSet<ObjectRef> }` with `#[derive(Default)]`, a `from_rebuild(result: &RebuildResult) -> Self` constructor (`map` = ref_map first-occurrences, `new_refs` = `result.new_kids` collected), `remap(old) -> Option<ObjectRef>`, and `is_surviving_target(r) -> bool` (`map.contains_key || new_refs.contains`).

**Step 2:** Replace the entry-point `surviving` build (lines ~100-104) with `Surviving::from_rebuild(result)`.

**Step 3:** Change the 18 `surviving: &BTreeMap<ObjectRef, ObjectRef>` params to `surviving: &Surviving`. Update the two remap-pass uses: `surviving.contains_key(&page_ref)` (remap_or_null_dest, ~544) → `surviving.is_surviving_target(page_ref)`; `surviving.get(&old)` (remap_dest_value_depth, ~811) → `surviving.remap(old)`. Update the null-pass check (null_removed_dest_target, ~673) → `!surviving.is_surviving_target(page_ref)`.

**Step 4: Run — expect PASS**

Run: `cargo test -p flpdf --lib outline_dest_remap 2>&1 | tail`
Expected: Task 1 test now PASS; the existing 38 tests still PASS (fix the 4 internal-helper call sites that build `BTreeMap::new()` → `Surviving::default()`).

**Step 5: Commit**

---

### Task 3: GREEN — over-skip regression guard

**Files:**
- Test: `crates/flpdf/src/outline_dest_remap.rs` (`mod tests`)

**Step 1: Write the test**

A destination targeting a genuinely removed page (`[7 0 R]`, where `7` is neither a `ref_map` key nor in `new_kids`) must still be nulled after the pass. Asserts the predicate's removed-side still nulls.

**Step 2: Run — expect PASS** (proves `is_surviving_target` did not over-broaden).

**Step 3: Commit**

---

### Task 4: Doc + lint + coverage gate

**Files:**
- Modify: `crates/flpdf/src/outline_dest_remap.rs` (module doc + `remap_annot_dests` doc)

**Step 1:** Update public doc comments to state the invariant in present tense, English only, no beads IDs / internal-progress jargon: e.g. *"A destination already pointing at a surviving page's remapped new ref is left verbatim and never nulled."*

**Step 2:** `cargo fmt`; `cargo clippy -p flpdf --all-targets`.

**Step 3:** `cargo test -p flpdf --lib outline_dest_remap` (and full `cargo test -p flpdf --lib`).

**Step 4:** Commit, then run `scripts/patch-coverage.sh --base main` (flpdf changed lines must be 100%). Qualitative arm: both predicate sides tested (Task 1 surviving-value side, Task 3 removed side).

**Step 5:** Commit any coverage follow-ups.
