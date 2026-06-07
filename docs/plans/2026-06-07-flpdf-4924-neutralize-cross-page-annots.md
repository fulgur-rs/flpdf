# Neutralize Cross-Page Annotation Destinations (flpdf-4924) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

> **Note (superseded during implementation):** Tasks 1–6 below were the initial
> plan. During implementation the neutralization strategy was changed to the
> uniform qpdf-aligned rule "**drop the destination, keep the action**": a
> cross-page `/GoTo` has only its `/D` removed (the `/A` / action / chain is
> retained), and coverage was expanded to annotation `/AA`, `/A` `/Next` chains
> (single, array, indirect array), and the page's own `/AA`. The historical
> "remove the whole `/A`" / `assert annot.get("A").is_none()` snippets below
> reflect the original plan, **not** the shipped behavior. The source of truth is
> the beads `flpdf-4924` design (v2), the `page_extract.rs` module doc, and the
> tests in `page_extract_tests.rs`.

**Goal:** In single-page `extract_page`, neutralize Link annotations whose
explicit `/Dest` (or `/A /GoTo /D`) targets a page absent from the one-page
output, so the leaked sibling page stub + ancestor `/Pages` node get swept.

**Architecture:** Post-copy fixup in `page_extract::extract_page`, inserted after
leaf materialization and before the existing `sweep_unreachable_objects` call.
Operate on the TARGET side: a destination whose resolved page ref `!=
copied_page_ref` is "absent" → drop the dead destination (remove the `/Dest` key,
or the `/D` key of the offending `/GoTo` action — the action is kept). The
existing sweep then prunes the now-unreferenced sibling stub. Destination parsing
reuses `outline_dest_remap::dest_page_ref_resolved` (made `pub(crate)`), which
returns `None` for named/string/URI/GoToR dests — so those are left intact with
zero new name-tree code. Behavior matches qpdf (keep annotation, drop dead
destination).

**Tech Stack:** Rust, flpdf crate. Existing helpers: `dest_page_ref_resolved`,
`resolve_ref_chain` (outline_dest_remap.rs), `Dictionary::{get,remove,insert}`,
`Object::{as_ref_id,as_name,as_dict}`, `sweep_unreachable_objects`.

---

## Task 1: Expose dest-parsing helpers as `pub(crate)`

**Files:**
- Modify: `crates/flpdf/src/outline_dest_remap.rs` (fn `dest_page_ref_resolved` ~line 996, fn `resolve_ref_chain` ~line 819)

**Step 1: Change visibility**

In `outline_dest_remap.rs`, change:
- `fn dest_page_ref_resolved<R: Read + Seek>(` → `pub(crate) fn dest_page_ref_resolved<R: Read + Seek>(`
- `fn resolve_ref_chain<R: Read + Seek>(` → `pub(crate) fn resolve_ref_chain<R: Read + Seek>(`

Leave their doc comments; both already have explanatory `///` / `//`. Add to
`dest_page_ref_resolved`'s doc one sentence clarifying the `pub(crate)` contract
if missing: it returns `None` for named/string/external destinations (no in-doc
page ref). These are non-published (`pub(crate)`) so doc-review English rule still
applies but they are not on the docs.rs surface.

**Step 2: Verify it compiles**

Run: `cargo build -p flpdf`
Expected: PASS (no callers yet; just visibility widened).

**Step 3: Commit**

```bash
git add crates/flpdf/src/outline_dest_remap.rs
git commit -m "refactor(extract): expose dest_page_ref_resolved/resolve_ref_chain as pub(crate) [flpdf-4924]"
```

---

## Task 2: Failing test — flip the known-limitation pin

**Files:**
- Modify: `crates/flpdf/tests/page_extract_tests.rs` (fn `cross_page_link_leaks_sibling_known_limitation` ~line 471)

**Step 1: Rewrite the test to assert the FIXED behavior**

Rename `cross_page_link_leaks_sibling_known_limitation` →
`cross_page_link_neutralized_no_sibling_leak`. Replace its two leak assertions
(`== 2`) with the fixed expectations, and assert the annotation is retained but
its `/Dest` removed. Keep the existing core-guarantee block (single page,
content + `/Resources /Font /F1` intact) unchanged. New body:

```rust
#[test]
fn cross_page_link_neutralized_no_sibling_leak() {
    // flpdf-4924: an explicit cross-page /Dest is neutralized (dest removed,
    // annotation kept). The sibling /Page stub + its ancestor /Pages node then
    // become unreachable and are swept. qpdf-aligned.
    let src = cross_page_link_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();

    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "sibling page must be pruned after neutralizing its inbound /Dest"
    );
    assert_eq!(
        count_type(&mut out, b"Pages"),
        1,
        "ancestor /Pages must be pruned once the sibling stub is gone"
    );

    // Annotation is RETAINED but its /Dest is removed (neutralized).
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(leaf_refs.len(), 1);
    let leaf = out.resolve_borrowed(leaf_refs[0]).unwrap().as_dict().cloned().unwrap();
    let annots = match leaf.get("Annots") {
        Some(Object::Array(a)) => a.clone(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    assert_eq!(annots.len(), 1, "annotation must be retained, not dropped");
    let annot_ref = annots[0].as_ref_id().expect("annot is an indirect ref");
    let annot = out.resolve_borrowed(annot_ref).unwrap().as_dict().cloned().unwrap();
    assert!(annot.get("Dest").is_none(), "/Dest must be neutralized (removed)");
    assert_eq!(
        annot.get("Subtype").and_then(|o| o.as_name()),
        Some(&b"Link"[..]),
        "annotation subtype preserved"
    );

    // CORE GUARANTEE: extracted leaf content + resources intact.
    let contents_ref = match leaf.get("Contents") {
        Some(Object::Reference(r)) => *r,
        other => panic!("expected /Contents ref, got {other:?}"),
    };
    let stream = match out.resolve(contents_ref).unwrap() {
        Object::Stream(s) => s,
        other => panic!("expected content stream, got {other:?}"),
    };
    assert_eq!(stream.data, b"BT /F1 12 Tf ET", "leaf content stream intact");
    let res = leaf.get("Resources").and_then(|o| o.as_dict()).expect("/Resources present");
    assert!(
        res.get("Font").and_then(|o| o.as_dict()).and_then(|f| f.get("F1")).is_some(),
        "leaf /Resources /Font /F1 intact"
    );
}
```

NOTE: `as_ref_id` is already in scope via `flpdf::Object`. If the test file does
not import a needed item, add it to the `use flpdf::{...}` line at the top.

**Step 2: Run test to verify it FAILS**

Run: `cargo test -p flpdf --test page_extract_tests cross_page_link_neutralized_no_sibling_leak`
Expected: FAIL — currently Page count == 2 (sibling leaks), assertion fails.

**Step 3: Commit**

```bash
git add crates/flpdf/tests/page_extract_tests.rs
git commit -m "test(extract): pin neutralize-on-extract behavior (failing) [flpdf-4924]"
```

---

## Task 3: Implement the neutralization pass

**Files:**
- Modify: `crates/flpdf/src/page_extract.rs`

**Step 1: Add imports + call the new pass before sweep**

At the top `use` block of `page_extract.rs`, add:

```rust
use crate::outline_dest_remap::{dest_page_ref_resolved, resolve_ref_chain};
```

In `extract_page`, immediately BEFORE the `sweep_unreachable_objects(&mut target)?;`
line (current line ~137), insert:

```rust
    // Neutralize annotations on the extracted leaf whose destination targets a
    // page absent from this single-page output. Without this, an explicit
    // cross-page /Dest keeps the copied sibling-page stub (and its ancestor
    // /Pages) reachable, so the sweep below cannot prune them. qpdf-aligned:
    // the annotation is retained, only the dead destination is removed.
    neutralize_absent_dests(&mut target, copied_page_ref)?;
```

**Step 2: Implement the helper functions**

Add at module scope (after `extract_page`, before `minimal_target_bytes`):

```rust
/// Remove `/Dest` and/or `/A /GoTo` from any annotation on `page_ref` whose
/// destination targets a page other than `page_ref` (i.e. a page absent from
/// the single-page output). Named / string / `/URI` / `/GoToR` destinations
/// carry no in-document page reference and are left untouched.
fn neutralize_absent_dests(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    page_ref: ObjectRef,
) -> Result<()> {
    let page_obj = target.resolve_borrowed(page_ref)?;
    let Some(page_dict) = page_obj.as_dict() else {
        return Ok(());
    };
    // /Annots may be an inline array or an indirect reference to one.
    let annot_refs: Vec<ObjectRef> = match page_dict.get("Annots").cloned() {
        Some(Object::Array(arr)) => arr.iter().filter_map(Object::as_ref_id).collect(),
        Some(Object::Reference(r)) => match target.resolve_borrowed(r)? {
            Object::Array(arr) => arr.iter().filter_map(Object::as_ref_id).collect(),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };

    for annot_ref in annot_refs {
        neutralize_annot_if_absent(target, annot_ref, page_ref)?;
    }
    Ok(())
}

/// Inspect one annotation; strip `/Dest` and/or the `/A` GoTo action when its
/// destination resolves to a page other than `keep`.
fn neutralize_annot_if_absent(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    annot_ref: ObjectRef,
    keep: ObjectRef,
) -> Result<()> {
    let Some(mut annot) = target.resolve_borrowed(annot_ref)?.as_dict().cloned() else {
        return Ok(());
    };
    let mut changed = false;

    // /Dest — explicit array, dict, or an indirect reference to either.
    if let Some(dest) = annot.get("Dest").cloned() {
        if dest_targets_absent_page(target, &dest, keep)? {
            annot.remove("Dest");
            changed = true;
        }
    }

    // /A — only a /GoTo action carries an in-document page /D. Follow the /A
    // ref chain to the action dict; if it is a GoTo whose /D is absent, drop
    // the whole /A key (an actionless annotation is the neutralized form).
    if let Some(a_val) = annot.get("A").cloned() {
        let (action_obj, _) = resolve_ref_chain(target, &a_val)?;
        if let Some(action) = action_obj.as_dict() {
            let is_goto = matches!(action.get("S"), Some(Object::Name(n)) if n == b"GoTo");
            if is_goto {
                if let Some(d_val) = action.get("D").cloned() {
                    if dest_targets_absent_page(target, &d_val, keep)? {
                        annot.remove("A");
                        changed = true;
                    }
                }
            }
        }
    }

    if changed {
        target.set_object(annot_ref, Object::Dictionary(annot));
    }
    Ok(())
}

/// `true` when `dest` resolves to an explicit page reference other than `keep`.
/// Named / string / external destinations (no resolvable in-doc page ref) and
/// self-links (`== keep`) return `false` — they are not neutralized.
fn dest_targets_absent_page(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    dest: &Object,
    keep: ObjectRef,
) -> Result<bool> {
    Ok(match dest_page_ref_resolved(target, dest)? {
        Some(page_ref) => page_ref != keep,
        None => false,
    })
}
```

NOTE on imports: `Cursor`, `Object`, `ObjectRef`, `Pdf`, `Result` are already
imported at the top of `page_extract.rs`. `resolve_ref_chain` /
`dest_page_ref_resolved` come from the Task 1 `use`. `as_ref_id` / `as_dict` are
inherent `Object` methods.

**Step 3: Run the Task 2 test to verify it PASSES**

Run: `cargo test -p flpdf --test page_extract_tests cross_page_link_neutralized_no_sibling_leak`
Expected: PASS.

**Step 4: Run the full page_extract suite**

Run: `cargo test -p flpdf --test page_extract_tests`
Expected: all PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_extract.rs
git commit -m "fix(extract): neutralize cross-page annotation dests, prune sibling leak [flpdf-4924]"
```

---

## Task 4: Test matrix — self-link, named, /A GoTo, URI, indirect

**Files:**
- Modify: `crates/flpdf/tests/page_extract_tests.rs`

Add these tests. Each builds a one-extracted-page PDF and asserts (a) Page count
== 1 (no leak) and (b) the destination field is kept or removed as specified.
Reuse the existing `build_pdf` and `count_type` helpers. Page 0 is obj 3, the
sibling is obj 4.

**Step 1: self-link is preserved**

```rust
#[test]
fn self_page_link_is_preserved() {
    // /Dest targets the extracted page itself -> kept, no neutralization.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [3 0 R /Fit] >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out.resolve_borrowed(leaf_refs[0]).unwrap().as_dict().cloned().unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out.resolve_borrowed(annot_ref).unwrap().as_dict().cloned().unwrap();
    assert!(annot.get("Dest").is_some(), "self-link /Dest must be preserved");
}
```

**Step 2: named destination is preserved + no leak**

```rust
#[test]
fn named_dest_is_preserved_no_leak() {
    // A named destination (/Dest is a name) carries no in-doc page ref, so it
    // never pulled a sibling in; leave it untouched.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest /SomeNamedDest >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1, "named dest must not leak a sibling");
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out.resolve_borrowed(leaf_refs[0]).unwrap().as_dict().cloned().unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out.resolve_borrowed(annot_ref).unwrap().as_dict().cloned().unwrap();
    assert_eq!(
        annot.get("Dest").and_then(|o| o.as_name()),
        Some(&b"SomeNamedDest"[..]),
        "named /Dest preserved",
    );
}
```

**Step 3: /A GoTo to sibling is neutralized**

```rust
#[test]
fn action_goto_absent_page_is_neutralized() {
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /D [4 0 R /Fit] >> >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1, "GoTo sibling must be pruned");
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out.resolve_borrowed(leaf_refs[0]).unwrap().as_dict().cloned().unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out.resolve_borrowed(annot_ref).unwrap().as_dict().cloned().unwrap();
    // SHIPPED behavior (see superseded note at top): the /A action is RETAINED;
    // only its cross-page /D is dropped.
    let a = annot.get("A").and_then(|o| o.as_dict()).expect("/A retained");
    assert_eq!(a.get("S").and_then(|o| o.as_name()), Some(&b"GoTo"[..]));
    assert!(a.get("D").is_none(), "/A GoTo /D must be neutralized (removed)");
}
```

**Step 4: /A URI is preserved**

```rust
#[test]
fn action_uri_is_preserved() {
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /URI /URI (http://example.com) >> >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out.resolve_borrowed(leaf_refs[0]).unwrap().as_dict().cloned().unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out.resolve_borrowed(annot_ref).unwrap().as_dict().cloned().unwrap();
    assert!(annot.get("A").is_some(), "/A URI must be preserved");
}
```

**Step 5: indirect /Dest (array stored as a separate object) is neutralized**

```rust
#[test]
fn indirect_dest_absent_page_is_neutralized() {
    // /Dest is an indirect ref (8 0 R) to the [sibling /Fit] array.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest 8 0 R >>"),
            (8, "[4 0 R /Fit]"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1, "indirect-dest sibling must be pruned");
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out.resolve_borrowed(leaf_refs[0]).unwrap().as_dict().cloned().unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out.resolve_borrowed(annot_ref).unwrap().as_dict().cloned().unwrap();
    assert!(annot.get("Dest").is_none(), "indirect /Dest must be neutralized");
}
```

**Step 6: Run the full suite**

Run: `cargo test -p flpdf --test page_extract_tests`
Expected: all PASS.

**Step 7: Commit**

```bash
git add crates/flpdf/tests/page_extract_tests.rs
git commit -m "test(extract): cover self-link/named/GoTo/URI/indirect dest neutralization [flpdf-4924]"
```

---

## Task 5: Update module doc (remove Known limitation)

**Files:**
- Modify: `crates/flpdf/src/page_extract.rs` (module doc, lines ~18-24)

**Step 1: Replace the `# Known limitation` block**

Remove the existing `//! # Known limitation` paragraph (the one referencing
flpdf-4924) and replace with a present-tense behavior note. No beads ID, English,
spec/qpdf-grounded:

```rust
//! # Cross-page annotation destinations
//!
//! A Link annotation on the extracted page whose explicit `/Dest` (or `/A
//! /GoTo /D`) targets another page is neutralized: the annotation is retained
//! but its destination is removed, since that page is absent from the
//! single-page output. This matches qpdf's page-extraction behavior (keep the
//! annotation, drop the dead destination). Named, string, `/URI`, and remote
//! (`/GoToR`) destinations carry no in-document page reference and are left
//! intact.
```

**Step 2: Verify docs build with no broken intra-doc links**

Run: `cargo doc -p flpdf --no-deps 2>&1 | grep -i warning` (expect no
broken-intra-doc-link warnings introduced by this change).

**Step 3: Commit**

```bash
git add crates/flpdf/src/page_extract.rs
git commit -m "docs(extract): document qpdf-aligned cross-page dest neutralization [flpdf-4924]"
```

---

## Task 6: Quality gates + full verification

**Step 1: Full test suite**

Run: `cargo test -p flpdf`
Expected: all PASS (no regressions in outline_dest_remap, page_closure, etc.).

**Step 2: Clippy + fmt**

Run: `cargo clippy -p flpdf --all-targets -- -D warnings`
Run: `cargo fmt -p flpdf -- --check`
Expected: clean. Fix any findings (esp. the review-pattern rules: no needless
clone, resolve indirect refs — already followed via `dest_page_ref_resolved`).

**Step 3: Confirm the old test name is gone**

Run: `grep -rn cross_page_link_leaks_sibling_known_limitation crates/flpdf`
Expected: no matches (renamed in Task 2).

**Step 4: Commit any lint fixes, then done**

```bash
git add -A && git commit -m "chore(extract): clippy/fmt fixups [flpdf-4924]"  # only if needed
```

---

## Review-pattern compliance notes (per .claude/rules)

- **No needless clone:** annot dict is `.cloned()` once (we own + mutate it);
  destination values are `.cloned()` to detach the borrow before resolving
  (resolve needs `&mut target`). These are necessary, not double-allocations.
- **Resolve indirect refs:** all dest classification goes through
  `dest_page_ref_resolved`, which resolves `/Dest`, `/A`, `/D` indirection and
  the array/dict forms — no direct `Object::Name`/`Integer` matching of raw dests.
- **Bounded traversal:** `dest_page_ref_resolved` / `resolve_ref_chain` are
  depth-bounded by `MAX_DEST_RESOLVE_DEPTH`; cyclic/hostile dests terminate
  conservatively (treated as no resolvable page ref → not neutralized).
- **No unsigned-cast overflow:** no integer casts introduced.
