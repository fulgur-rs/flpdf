# Merge inline `/OpenAction` `/GoTo /SD` drop-to-`/D` Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or subagent-driven-development) to implement this plan task-by-task.

**Goal:** In `page_merge`, make a primary catalog's inline `/OpenAction` of `/S /GoTo` with `/SD` (a structure destination) **drop** `/SD` unconditionally and fall back to `/D`, instead of leaving the unmapped StructElem ref to be nulled by `remap_refs_in_object` (`flpdf-ahkf`).

**Architecture:** The catalog is never copied, so an inline `/OpenAction`'s destinations are reconstructed by `remap_inline_action_depth` (page_merge.rs). It already detaches `/D` and `/Next` for dest-aware/recursive handling, but leaves `/SD` in the dict where the bulk `remap_refs_in_object` nulls its (uncopied) StructElem ref. The fix: detach `/SD` too and drop it. merge never copies the structure tree, so `/SD` can never resolve in the output; `/SD` takes precedence over `/D` for structure-aware viewers (ISO 32000-2 §12.3.2.3), so a dangling/null `/SD` would actively suppress the working `/D`. Dropping degrades the action to its explicit `/D`.

**Tech Stack:** Rust, `cargo test -p flpdf`, integration tests in `crates/flpdf/tests/page_merge_tests.rs`.

**Scope:** Inline (on-catalog) `/OpenAction` only. The indirect `/OpenAction` and per-page annotation `/A /SD` paths copy the StructElem via `page_object_closure` (a different copy domain); their behavior is governed by that shared primitive and is out of scope (see the existing "page_object_closure is out of scope" note at page_merge.rs ~641-645). Confirm the indirect path's actual behavior before closing the issue (verification task at the end).

---

### Task 1: Failing tests for inline `/OpenAction /GoTo /SD`

**Files:**
- Test: `crates/flpdf/tests/page_merge_tests.rs` (append near the other inline-`/OpenAction` tests, after `merge_opaque_openaction_d_operand_is_copied_and_remapped`)

Reuse existing helpers in that file: `build_pdf`, `merge_documents`, `MergeInput`, `pages::page_refs`, `catalog_dict`, `dest_array_first`.

**Step 1: Write the failing tests**

```rust
// An inline-on-catalog /OpenAction of /S /GoTo carrying BOTH a /SD structure
// destination and a /D explicit destination: merge never copies the structure
// tree, so /SD (pointing at an uncopied StructElem) cannot resolve and — per
// ISO 32000-2 §12.3.2.3, /SD takes precedence over /D for structure-aware
// viewers — would suppress the working /D if left in place (nulled or dangling).
// merge drops /SD and keeps /D, which is remapped to the copied page. (flpdf-ahkf)
#[test]
fn merge_inline_openaction_goto_sd_dropped_falls_back_to_d() {
    let src = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R \
                 /OpenAction << /S /GoTo /SD [8 0 R /Fit] /D [3 0 R /Fit] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"), // page0 kept, /D target
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"), // page1 removed, /SD target
            (8, "<< /Type /StructElem /S /Sect /Pg 4 0 R >>"),
        ],
        1,
    );
    let mut a = Pdf::open_mem_owned(src).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();
    let refs = pages::page_refs(&mut doc).unwrap();

    let cat = catalog_dict(&mut doc);
    let oa = match cat.get("OpenAction") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected /OpenAction action dict, got {other:?}"),
    };
    assert!(
        oa.get("SD").is_none(),
        "/SD must be dropped (structure tree not copied), got {:?}",
        oa.get("SD")
    );
    let d = match oa.get("D") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected /D array fallback, got {other:?}"),
    };
    let (d_ref, d_null) = dest_array_first(&mut doc, &d);
    assert!(!d_null, "/D fallback must remain live");
    assert_eq!(d_ref, refs[0], "/D remaps to copied page0");

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok(), "merged doc round-trips");
}

// An inline /OpenAction /S /GoTo with ONLY /SD (no /D): dropping /SD leaves the
// GoTo with no destination (a benign no-op action), retained like extract's
// neutralize keeps the action and drops only the destination key. (flpdf-ahkf)
#[test]
fn merge_inline_openaction_goto_sd_only_yields_no_dest() {
    let src = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R \
                 /OpenAction << /S /GoTo /SD [8 0 R /Fit] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"), // page0 kept
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"), // /SD target page
            (8, "<< /Type /StructElem /S /Sect /Pg 4 0 R >>"),
        ],
        1,
    );
    let mut a = Pdf::open_mem_owned(src).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let cat = catalog_dict(&mut doc);
    let oa = match cat.get("OpenAction") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected /OpenAction action dict, got {other:?}"),
    };
    assert!(oa.get("SD").is_none(), "/SD dropped");
    assert!(oa.get("D").is_none(), "no /D fallback present");
    assert_eq!(
        oa.get("S").and_then(Object::as_name),
        Some(&b"GoTo"[..]),
        "the GoTo action itself is retained"
    );

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok(), "merged doc round-trips");
}

// /SD is dropped UNCONDITIONALLY — even when its StructElem /Pg targets a KEPT
// page. Unlike extract (which keeps a kept-page /SD because it retains the
// structure tree), merge never copies the structure tree, so the StructElem ref
// is uncopied regardless of whether the target page survives. This pins the
// unconditional-drop discipline (guards against a copy of extract's
// drop-only-when-absent condition). (flpdf-ahkf)
#[test]
fn merge_inline_openaction_goto_sd_dropped_even_when_target_kept() {
    let src = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R \
                 /OpenAction << /S /GoTo /SD [8 0 R /Fit] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"), // page0 kept
            (8, "<< /Type /StructElem /S /Sect /Pg 3 0 R >>"),              // /Pg → KEPT page0
        ],
        1,
    );
    let mut a = Pdf::open_mem_owned(src).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let cat = catalog_dict(&mut doc);
    let oa = match cat.get("OpenAction") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected /OpenAction action dict, got {other:?}"),
    };
    assert!(
        oa.get("SD").is_none(),
        "/SD dropped even when its target page is kept (structure tree never copied)"
    );
}
```

**Step 2: Run to verify they fail**

Run: `cargo test -p flpdf --test page_merge_tests merge_inline_openaction_goto_sd`
Expected: FAIL — current code leaves `/SD` in the dict (nulled or `[null /Fit]`), so `oa.get("SD").is_none()` fails.

---

### Task 2: Drop `/SD` in `remap_inline_action_depth`

**Files:**
- Modify: `crates/flpdf/src/page_merge.rs` — `remap_inline_action_depth` Dictionary arm (the `let dest = out.remove("D"); let next = out.remove("Next");` block, ~line 1048-1049).

**Step 1: Add the `/SD` drop**

After `let next = out.remove("Next");`, add:

```rust
            // Drop `/SD` (a GoTo structure destination). It references a
            // StructElem in the primary's structure tree, which page-merge never
            // copies, so the ref is unmapped: left in place it would dangle (or
            // be nulled by remap_refs_in_object below) AND — `/SD` takes
            // precedence over `/D` for structure-aware viewers (ISO 32000-2
            // §12.3.2.3) — suppress the still-valid `/D` fallback. Removing it
            // degrades the action to its explicit `/D`. Unconditional: merge
            // copies no structure tree, so `/SD` is invalid whether or not its
            // target page is selected (cf. the per-page annotation / indirect
            // paths, where page_object_closure copies the StructElem). The fold
            // side already skips `/SD` (ACTION_DEST_KEYS), so this keeps remap
            // symmetric with the closure-fold.
            out.remove("SD");
```

(Detaching `/SD` before the `remap_refs_in_object` call below is what prevents the null-ification; not re-inserting it is the drop.)

**Step 2: Update the `remap_inline_action` doc comment**

In the `///` doc on `remap_inline_action` (~line 1008-1019), add one sentence (English, spec-grounded, no issue IDs — per `.claude/rules/pdf-rust-doc-review-patterns.md`):

> A `/GoTo`'s `/SD` (structure destination) is dropped: page-merge copies no structure tree, so its StructElem ref cannot resolve, and `/SD` would otherwise take precedence over the `/D` fallback (ISO 32000-2 §12.3.2.3).

**Step 3: Run the new tests**

Run: `cargo test -p flpdf --test page_merge_tests merge_inline_openaction_goto_sd`
Expected: PASS (3 tests).

**Step 4: Run the full merge + extract suites (no regressions)**

Run: `cargo test -p flpdf --test page_merge_tests --test page_extract_tests`
Expected: all pass (prior 65 merge tests + new 3, extract unchanged).

---

### Task 3: Quality gates + patch coverage

**Step 1: fmt + clippy + doctest**

Run:
```bash
cargo fmt -p flpdf
cargo clippy -p flpdf --all-targets -- -D warnings
cargo test -p flpdf --doc
```
Expected: clean (the doc edit is prose; no doctest added).

**Step 2: Commit** (coverage gate diffs HEAD, so commit first)

```bash
git add crates/flpdf/src/page_merge.rs crates/flpdf/tests/page_merge_tests.rs docs/plans/2026-06-13-merge-inline-openaction-sd-drop.md
git commit -m "fix(flpdf): drop /GoTo /SD in primary inline /OpenAction, fall back to /D (flpdf-ahkf)"
```

**Step 3: Patch coverage (flpdf changed lines must be 100%)**

Run: `scripts/patch-coverage.sh --base main`
Expected: exit 0. The only changed src line is `out.remove("SD");`, executed by all three new tests. (Doc-comment lines are non-executable.) If any changed line is uncovered, add a test or justify with `// cov:ignore: <reason>` and note it in the PR description.

---

### Task 4: Verify indirect `/OpenAction /SD` behavior (before close, scope confirmation)

Not a code change — a verification to confirm the scope split stated in the issue design holds.

**Step 1:** Trace/test what the **indirect** `/OpenAction` (`/OpenAction 9 0 R` → `<< /S /GoTo /SD [se /Fit] >>`) produces. `fold_doc_level_closure` folds it via `page_object_closure`, which follows `/SD` → StructElem (not a Page/Catalog boundary) → its `/Pg` (Page, stops) and `/P` parent (up the structure tree).

**Step 2:** Classify the outcome:
- If it yields a broken/dangling `/SD` too → note it; decide whether to widen scope or file a follow-up.
- If it over-copies (pulls StructElem / structure tree / sibling pages) → that is a *different* bug in the shared `page_object_closure` primitive (already flagged out-of-scope in page_merge.rs); file a follow-up beads issue and reference it.

**Step 3:** Record the finding in the PR description and (if a follow-up is warranted) `bd create` it as a child of the merge/page_object_closure epic before closing `flpdf-ahkf`.
