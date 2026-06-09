# Outline / Named-Dest qpdf null-out Parity Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or subagent-driven-development) to implement this plan task-by-task. Work in worktree `.worktrees/flpdf-9hc-20-32-outline-nullout`.

**Goal:** Make `--pages` subset extraction match qpdf 11.9.0's outline/named-destination handling: never drop nav entries, remap surviving-page dests, and emit removed-but-referenced pages as `null` (drop removed pages referenced by nothing).

**Architecture:** Rewrite `crates/flpdf/src/outline_dest_remap.rs` from prune-during-walk (DROP) to walk-and-null-out. Visit every named dest / legacy dest / outline item; remap surviving-page targets to their new ref; for a removed-page target, leave the entry untouched and set the target page object to `Object::Null`. Never stitch siblings, recompute `/Count`/`/Limits`, drop items, or remove `/Outlines`/`/Names` from the catalog. The existing `subset_prune` mark-and-sweep keeps a nulled page alive iff a surviving dest still references it (qpdf rule: referenced→null, unreferenced→absent); the full-rewrite writer emits a live `Object::Null` as `N 0 obj null`.

**Tech Stack:** Rust, `cargo test -p flpdf`. qpdf 11.9.0 (`/usr/bin/qpdf`) is the behavioural oracle (dumps captured in the issue design field, regenerable via `qpdf --qdf --static-id in.pdf --pages in.pdf 1,3 -- out.pdf`).

## qpdf null-out rules (oracle — confirmed empirically)

1. Outline items & named dests are **never dropped**. `/Count`, `/Limits`, `/Prev`/`/Next`/`/First`/`/Last` are **unchanged**.
2. Surviving-page dests are **remapped** to the new page ref.
3. A removed page **still referenced** by a surviving dest is emitted as `N 0 obj null` (keeps an object number); the dest keeps pointing at it.
4. A removed page **referenced by nothing** is **absent** (fully dropped by GC).
5. `/Limits` is **not** recomputed even when an entry now points at a null page.

## Key codebase facts

- Pipeline (CLI `--pages`, `main.rs:3091-3103`): `rebuild_page_tree` → `prune_acroform_after_subset` → `remap_outline_and_dests` → `subset_prune::prune_after_subset` (GC) → `write_pdf` (full rewrite, renumbers reachable objects).
- `rebuild_page_tree` reuses the **source ref** for a surviving page's leaf (`page_tree_rebuild.rs:311`), so `ref_map[old][0] == old` for non-duplicate selections (surviving dests often need no remap). Removed page objects are **left live** (not deleted) until the later sweep — so they can be nulled in `remap_outline_and_dests`.
- `subset_prune::sweep_unreachable_objects` deletes every live object NOT reachable from `/Root`. A nulled page referenced by a kept dest stays reachable → survives as `null`. A nulled/whole page referenced by nothing → swept.
- `write_pdf_full_rewrite` (`writer.rs:2521-2555`) skips only `deleted_object_refs()` + object 0; a live `Object::Null` is emitted as a body object `null`.
- Existing entry points to KEEP (signatures unchanged): `remap_outline_and_dests`, `remap_outline_and_dests_with_max_depth`. Reusable helpers: `remap_dest_value`, `remap_item_dest`, `dest_page_ref_resolved`, `resolve_ref_chain`.
- Helpers to DELETE (DROP machinery): `prune_name_tree`, `prune_name_tree_node_dict`, `prune_name_pairs`, `prune_legacy_dests`, `prune_dests_dict` (replace with keep-all variants), `collect_siblings` drop/stitch path, `drop_subtree`, `stitch_siblings`, `count_visible_descendants`/`count_visible_in_chain`, `compute_limits`, `merge_node_limits`, `item_survives`/`dest_survives`/`surviving_names` classification.

---

### Task 1: Capture the core qpdf-parity behaviour as a failing test (named dests)

**Files:**
- Test: `crates/flpdf/src/outline_dest_remap.rs` (tests module)

**Step 1: Write the failing test** — using the existing `build_outline_pdf()` fixture, keep pages 1,3 (obj 3,5); pages 2,4 (obj 4,6) removed.

```rust
#[test]
fn nullout_named_dests_kept_removed_pages_nulled() {
    let mut pdf = open(build_outline_pdf());
    let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)];
    let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
    remap_outline_and_dests(&mut pdf, &result).unwrap();

    // ALL four named dests are still present (qpdf rule 1 — none dropped).
    let leaf = dict_of(&mut pdf, ObjectRef::new(30, 0));
    let Some(Object::Array(names)) = leaf.get("Names") else { panic!("/Names array"); };
    let keys: Vec<&[u8]> = names.iter().step_by(2)
        .filter_map(|o| match o { Object::String(b)|Object::Name(b)=>Some(b.as_slice()), _=>None })
        .collect();
    assert_eq!(keys, vec![b"dest_named_p4".as_slice(), b"dest_p1", b"dest_p2", b"dest_p3"],
        "all named dests kept, order + /Limits unchanged");
    assert!(leaf.get("Limits").is_some(), "/Limits not removed");

    // Surviving dests remapped; removed-page dests point at a now-null page.
    let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
    let new_p3 = result.ref_map[&ObjectRef::new(5, 0)][0];
    let dest_of = |names: &[Object], k: &[u8]| -> Object {
        let i = names.iter().step_by(2).position(|o| matches!(o, Object::String(b)|Object::Name(b) if b==k)).unwrap();
        names[i*2+1].clone()
    };
    let arr_first = |o: &Object| -> ObjectRef { o.as_array().unwrap().first().unwrap().as_ref_id().unwrap() };
    assert_eq!(arr_first(&dest_of(names, b"dest_p1")), new_p1);
    assert_eq!(arr_first(&dest_of(names, b"dest_p3")), new_p3);
    // dest_p2 -> obj4 (page 2 removed); obj4 is now null but still referenced.
    assert_eq!(arr_first(&dest_of(names, b"dest_p2")), ObjectRef::new(4, 0));
    assert!(matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
        "removed page 2 (obj4) nulled");
    assert!(matches!(pdf.resolve(ObjectRef::new(6, 0)).unwrap(), Object::Null),
        "removed page 4 (obj6) nulled (referenced by dest_named_p4)");
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib outline_dest_remap::tests::nullout_named_dests_kept_removed_pages_nulled`
Expected: FAIL — current DROP impl removes dest_p2/dest_named_p4 and does not null obj4/obj6.

**Step 3:** (implementation lands in Task 2) — leave failing for now.

**Step 4: Commit the test (red)**

```bash
git add crates/flpdf/src/outline_dest_remap.rs
git commit -m "test(flpdf): qpdf null-out parity for named dests (red)"
```

---

### Task 2: Rewrite named-dest + legacy-dest handling to null-out

**Files:**
- Modify: `crates/flpdf/src/outline_dest_remap.rs`

**Step 1:** Replace the prune name-tree walkers with keep-all variants. New private fn `remap_name_tree_node` (and `_node_dict` for the direct-in-catalog root) that, per node:
- leaf `/Names`: for each `(name, dest)` pair, compute `dest_page_ref_resolved`; if `Some(p)` and `p ∈ surviving` → `remap_dest_value` (write back inline-changed value into the SAME slot, keep the pair); if `Some(p)` and `p ∉ surviving` → `null_page(pdf, p)` (set_object Null, idempotent), keep the pair verbatim; if `None` → keep verbatim. Rebuild the leaf `/Names` array with **all** pairs in original order; **do not touch `/Limits`**.
- intermediate `/Kids`: recurse into every kid (depth + `visited` cycle guard, `depth >= max_depth -> Err`), keep **all** kids; **do not touch `/Limits`**.

Add helper:
```rust
/// Replace a removed page object with `null` in place (qpdf null-out). Idempotent.
fn null_page<R: Read + Seek>(pdf: &mut Pdf<R>, page_ref: ObjectRef) {
    pdf.set_object(page_ref, Object::Null);
}
```

**Step 2:** Replace `prune_legacy_dests`/`prune_dests_dict` with keep-all equivalents (same null/remap logic over a `/Dests` dict; keep every key).

**Step 3:** In `remap_outline_and_dests_with_max_depth`, drop the `surviving_names`/empties bookkeeping and the catalog `/Names` removal branches: `/Names` and `/Dests` are always retained. Keep the indirect-vs-direct dispatch (write the rebuilt node back).

**Step 4: Run the Task-1 test**

Run: `cargo test -p flpdf --lib outline_dest_remap::tests::nullout_named_dests_kept_removed_pages_nulled`
Expected: PASS.

**Step 5: Commit**

```bash
git commit -am "feat(flpdf): null-out named/legacy dests instead of dropping (qpdf parity)"
```

---

### Task 3: Outline-tree items — keep all, remap, null removed pages

**Files:** `crates/flpdf/src/outline_dest_remap.rs`

**Step 1: Failing test** — keep pages 1,3; assert the full outline chain survives unchanged.

```rust
#[test]
fn nullout_outline_items_all_kept_count_and_links_unchanged() {
    let mut pdf = open(build_outline_pdf());
    let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3,0), ObjectRef::new(5,0)]).unwrap();
    remap_outline_and_dests(&mut pdf, &result).unwrap();

    let cat = dict_of(&mut pdf, ObjectRef::new(1,0));
    let root = dict_of(&mut pdf, cat.get_ref("Outlines").unwrap());
    assert_eq!(root.get("Count"), Some(&Object::Integer(5)), "root /Count unchanged");
    assert_eq!(root.get_ref("First"), Some(ObjectRef::new(20,0)));
    assert_eq!(root.get_ref("Last"), Some(ObjectRef::new(23,0)));
    // Item 21 (GoTo action -> removed page 2/obj4): KEPT, links intact, obj4 nulled.
    let i21 = dict_of(&mut pdf, ObjectRef::new(21,0));
    assert_eq!(i21.get_ref("Prev"), Some(ObjectRef::new(20,0)));
    assert_eq!(i21.get_ref("Next"), Some(ObjectRef::new(22,0)));
    assert!(matches!(pdf.resolve(ObjectRef::new(4,0)).unwrap(), Object::Null));
    // Item 22 (/Dest [5 0 R] surviving) keeps /Count 1 and its child 24.
    let i22 = dict_of(&mut pdf, ObjectRef::new(22,0));
    assert_eq!(i22.get("Count"), Some(&Object::Integer(1)));
    assert_eq!(i22.get_ref("First"), Some(ObjectRef::new(24,0)));
}
```

**Step 2: Run — verify FAIL.**

**Step 3: Implement** — replace `collect_siblings`/`drop_subtree`/`stitch_siblings`/count machinery with a single keep-all walk `remap_outline_tree`: traverse the sibling chain + children (depth + per-chain `visited` guards, `depth >= max_depth -> Err`); for each item call `remap_item_dest` (already remaps `/Dest` and `/A /GoTo /D`) and additionally null any removed target page reached via `dest_page_ref_resolved` on the item's `/Dest` / action `/D`. Do not modify `/Count`, `/First`, `/Last`, `/Prev`, `/Next`. In the entry point, remove the kept/dropped stitching block and the `/Outlines` catalog-removal branch (outline root always retained).

**Step 4: Run the test — PASS.**

**Step 5: Commit** `feat(flpdf): keep all outline items, null removed targets (qpdf parity)`

---

### Task 4: Unreferenced removed page is absent (end-to-end through GC + write)

**Files:** `crates/flpdf/src/outline_dest_remap.rs` (or a new `tests/` integration test using the full pipeline)

**Step 1: Failing test** — fixture with a page referenced by no dest/outline; after `remap` + `prune_after_subset` + `write_pdf`, re-open and assert the unreferenced removed page object is absent (resolves to `Null`/missing) while a referenced removed page survives as `null`. Use the `build_min_pdf` helper; mirror `/tmp/in2.pdf` (oracle: only the dest-referenced removed page becomes `null`, the unreferenced one is gone).

**Step 2: Run — FAIL or PASS?** If sweep already drops it, this may pass once Tasks 2-3 land; keep the test as a regression guard. Verify expected via the qpdf `out2` oracle in the issue design.

**Step 3:** No new impl expected (GC handles it); if a referenced-null is wrongly swept, investigate reachability seeds.

**Step 4: Run — PASS. Step 5: Commit.**

---

### Task 5: Rewrite the ~27 existing DROP regression tests to null-out

**Files:** `crates/flpdf/src/outline_dest_remap.rs` (tests module)

**Step 1:** Go test-by-test. Convert DROP assertions to null-out:
- `dropped_pages_outline_items_removed_and_stitched`, `count_recomputed_correctly`, `string_dest_outline_item_dropped_when_named_dest_pruned`, `named_dests_pruned_and_remapped`, `parent_with_all_children_dropped_has_no_first_last`, `surviving_parent_with_all_children_dropped_has_no_count`, `all_items_dropped_outlines_removed_from_catalog`, `all_named_dests_pruned_removes_names_dests_from_catalog`, `indirect_named_dest_remapped_and_pruned`, `indirect_outline_item_dest_remapped_and_dropped`, `indirect_goto_action_remapped_and_dropped`, `*_direct_dictionary_*` → assert entries KEPT, removed targets resolve to `Null`, `/Count`/`/Limits`/links unchanged, catalog `/Outlines`+`/Names` retained.
- KEEP unchanged (still valid): hostile-PDF cycle/depth guards (`*_cycle_terminates`, `*_hits_depth_limit`) — but rename/retarget to the new walker fns; `count_visible_descendants_saturates_on_hostile_count` → DELETE (count recompute removed) unless a count-read path remains; `cyclic_indirect_dest_terminates_without_overflow` (dest resolution still guarded); `dict_form_*`/`indirect_*` remap-of-surviving assertions (KEEP the remap parts).
- DELETE tests asserting behaviour that no longer exists (stitching, count recompute, catalog removal).

**Step 2: Run the whole module** `cargo test -p flpdf --lib outline_dest_remap` — all green.

**Step 3: Commit** `test(flpdf): convert outline_dest_remap suite from DROP to null-out`

---

### Task 6: Hostile-PDF guards on the new walkers

**Files:** `crates/flpdf/src/outline_dest_remap.rs`

**Step 1:** Ensure `remap_name_tree_node` and `remap_outline_tree` carry depth (`depth >= max_depth -> Err`) + `visited` cycle guards (mirror the deleted walkers'). Add/retarget tests: `/Kids` cycle terminates, deep `/Kids` errors, `/Next` cycle terminates, deep `/First` errors.

**Step 2: Run — PASS. Step 3: Commit.**

---

### Task 7: Full-pipeline integration test + structural oracle check

**Files:** Create `crates/flpdf/tests/page_extract_outline_nullout_tests.rs`

**Step 1:** Build the 4-page outline+named-dest fixture (port `build_outline_pdf`), run the real CLI pipeline equivalent (`rebuild_page_tree` → `prune_acroform_after_subset` (if applicable) → `remap_outline_and_dests` → `prune_after_subset` → `write_pdf`), re-open the bytes, and assert the qpdf-parity structure end-to-end: `/Outlines` + `/Names` present, all 4 named dests present, surviving dests remapped, removed-page dests resolve to `Null`, `/Count` unchanged, an unreferenced removed page absent. (Not a byte-cmp vs qpdf — qpdf renumbers; assert structure.)

**Step 2: Run — PASS.** **Step 3: Commit.**

---

### Task 8: Module doc + interaction sweep

**Files:** `crates/flpdf/src/outline_dest_remap.rs` (module `//!` doc), check `acroform_field_prune.rs`, `subset_prune.rs`, `page_document_helper.rs`.

**Step 1:** Rewrite the module `//!` doc: remove the "flpdf chooses DROP semantics" section; document the qpdf null-out behaviour as the current contract (ground it in observed qpdf 11.9.0 behaviour per doc-review rule 1 — no beads IDs, English only). Update `# Errors` on the public fns if changed.

**Step 2:** Run the broader suites that touch extraction:
`cargo test -p flpdf --lib` and `cargo test -p flpdf --test page_extract_tests --test acroform_document_helper_tests --test page_document_helper_tests --test resource_pruning_tests`. Fix any fallout (acroform widget `/P` back-pointers, subset prune expectations).

**Step 3: Commit** `docs(flpdf): document qpdf null-out semantics for outline/dest remap`

---

### Task 9: Quality gate + workspace tests

**Step 1:** `cargo fmt --all` (CI quality gate is `cargo fmt --check`), `cargo clippy -p flpdf --all-targets`, `cargo test -p flpdf`.
**Step 2:** Review against `.claude/rules/pdf-rust-review-patterns.md` (no stray `.clone()`, resolve indirect refs, bounded walks).
**Step 3: Commit any fixups.**

---

## Out of scope (separate issues if they diverge)

- Single-page `extract_page` `neutralize_absent_dests` (drops dest keys) — a different path/behaviour; only `--pages` is in scope here.
- Whole-file byte-identity vs qpdf (object renumber order, etc.) — gated by the flpdf-9hc.20 writer work.
