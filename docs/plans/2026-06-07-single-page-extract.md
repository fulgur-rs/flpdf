# Single-Page Extract Primitive Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `extract_page(source, page_index)` — build a new minimal in-memory PDF document containing one source page plus its full object closure, copied cross-document.

**Architecture:** Mirrors qpdf's `emptyPDF()` + `QPDFPageDocumentHelper::addPage()` pattern (document object built first, written by a separate writer). Compose existing primitives: `page_object_closure` (5h5.1) → `copy_objects` (5h5.2) into a freshly-built minimal target (Catalog + empty Pages). Source is left pristine (X approach). Inherited attributes (`/Resources`, `/MediaBox`, `/CropBox`, `/Rotate`) are materialized onto the copied leaf using the SAME helpers as `rebuild_page_tree`, with their internal refs remapped through the copier's source→target map. A fresh single-level `/Pages` root is built (the copied ancestor `/Pages` chain has `/Kids` Null-ed by the copier, so it cannot be reused).

**Tech Stack:** Rust, flpdf crate. Existing APIs: `pages::page_refs`, `pages::resolve_inherited_resources_with_max_depth`, `page_rotate::resolve_inherited_rotate_with_max_depth`, `page_tree_rebuild::resolve_inherited_raw` (to be made `pub(crate)`), `page_closure::page_object_closure`, `object_copy::copy_objects`, `Pdf::open_mem_owned`, `Pdf::{resolve_borrowed, set_object, root_ref}`, `Dictionary::{get, insert, values_mut}`.

---

## Key design facts (read before starting)

- `extract_page` returns an **owned** `Pdf<Cursor<Vec<u8>>>` (from `open_mem_owned`). Caller writes it with `WriteOptions { full_rewrite: true, .. }`.
- **Page index is 0-based** (matches `page_refs()` Vec index / qpdf `getAllPages()`). Out of range → `Err`.
- **`/Info` is omitted** (freshly minimal). The task text permits this; qpdf `addPage` also does not carry `/Info`. flpdf has no trailer-mutation API, so copying `/Info` is a separate follow-up issue (filed at the end).
- The closure walker follows `/Parent` upward (collecting ancestor inherited resources) but skips `/Pages` `/Kids`. Therefore copied ancestor `/Pages` nodes have out-of-set `/Kids` → the copier rewrites those to `Null`. **Do not reuse the copied ancestor as the root.** Build a fresh single-level root and repoint the leaf `/Parent` to it. The copied ancestors become unreferenced and are dropped by the caller's `full_rewrite`, satisfying "no unrelated objects".
- Materialized attribute values come from the SOURCE and contain SOURCE refs. `/Resources` is an inline dict whose font/XObject refs are in the closure; `/MediaBox`/`/CropBox` may be returned by `resolve_inherited_raw` as an `Object::Reference` (qpdf shares the array indirectly). **Remap every materialized value through the copier map** (a tiny recursive walk). `/Rotate` is an integer with no refs.
- "Own attribute wins": only materialize an inherited attribute when the copied leaf lacks its own (mirror `rebuild_page_tree`'s `leaf_has_own`). `/Rotate` is always materialized unless the leaf has its own (mirror rebuild).

---

## Task 1: Expose `resolve_inherited_raw` as `pub(crate)`

**Files:**
- Modify: `crates/flpdf/src/page_tree_rebuild.rs` (the `fn resolve_inherited_raw` declaration, ~line 111)

**Step 1: Change visibility**

Change:
```rust
fn resolve_inherited_raw<R: Read + Seek>(
```
to:
```rust
pub(crate) fn resolve_inherited_raw<R: Read + Seek>(
```

**Step 2: Verify it still builds**

Run: `cargo build -p flpdf`
Expected: builds clean (no new warnings; the fn is now used by another module in later tasks, but `pub(crate)` on an as-yet-unused-cross-module item is fine).

**Step 3: Commit**

```bash
git add crates/flpdf/src/page_tree_rebuild.rs
git commit -m "refactor(page-tree): expose resolve_inherited_raw as pub(crate) [flpdf-5h5.3]"
```

---

## Task 2: Module scaffold + minimal target + first behavior (single-page extract)

**Files:**
- Create: `crates/flpdf/src/page_extract.rs`
- Modify: `crates/flpdf/src/lib.rs` (add `mod page_extract;` + `pub use`)
- Test: `crates/flpdf/tests/page_extract_tests.rs`

**Step 1: Write the failing test**

Create `crates/flpdf/tests/page_extract_tests.rs`:
```rust
//! Integration tests for [`flpdf::extract_page`].

use flpdf::{extract_page, pages, Object, ObjectRef, Pdf};
use std::collections::BTreeMap;

/// Build a PDF from `(number, body)` object definitions plus a `/Root` number.
/// `body` is the literal text between `N 0 obj` and `endobj`.
fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
    for (n, body) in objects {
        offsets.insert(*n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    let size = max + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..=max {
        match offsets.get(&n) {
            Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// Two-page document; each page carries its own /MediaBox and /Resources.
fn two_page_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >> /Contents 5 0 R >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Length 19 >>\nstream\nBT /F1 12 Tf ET\nendstream"),
            (6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        1,
    )
}

/// Resolve the catalog's /Pages dict from a freshly-extracted document.
fn pages_dict(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> flpdf::Dictionary {
    let catalog_ref = doc.root_ref().unwrap();
    let catalog = doc.resolve_borrowed(catalog_ref).unwrap().as_dict().cloned().unwrap();
    let pages_ref = catalog.get("Pages").and_then(|o| match o {
        Object::Reference(r) => Some(*r),
        _ => None,
    }).unwrap();
    doc.resolve_borrowed(pages_ref).unwrap().as_dict().cloned().unwrap()
}

#[test]
fn extracts_single_page_with_count_one() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();

    // Exactly one page in the extracted document.
    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(page_refs.len(), 1, "extracted doc must have exactly one page");

    // /Pages root: /Count 1, /Kids has one element.
    let root = pages_dict(&mut out);
    assert_eq!(root.get("Count"), Some(&Object::Integer(1)));
    match root.get("Kids") {
        Some(Object::Array(kids)) => assert_eq!(kids.len(), 1),
        other => panic!("expected /Kids array, got {other:?}"),
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --test page_extract_tests`
Expected: FAIL — `extract_page` not found (compile error).

**Step 3: Write minimal implementation**

Create `crates/flpdf/src/page_extract.rs`:
```rust
//! Single-page extraction into a fresh minimal document.
//!
//! [`extract_page`] builds a brand-new minimal [`Pdf`] containing exactly one
//! page from `source` plus its transitive object closure, copied across
//! documents. This mirrors qpdf's `emptyPDF()` + `QPDFPageDocumentHelper::
//! addPage()` pattern: the document object is constructed and populated here,
//! then written by a separate writer (`write_pdf` / `write_pdf_with_options`).
//!
//! `source` is left unmodified. Inherited page attributes (`/Resources`,
//! `/MediaBox`, `/CropBox`, `/Rotate`) are materialized onto the extracted page
//! exactly as [`crate::page_tree_rebuild`] does, so the page renders
//! identically in isolation.
//!
//! Part of the page extraction & merge primitives epic (flpdf-5h5). Composes
//! [`page_object_closure`](crate::page_closure::page_object_closure) and
//! [`copy_objects`](crate::object_copy::copy_objects).

use crate::object_copy::copy_objects;
use crate::page_closure::page_object_closure;
use crate::page_rotate::resolve_inherited_rotate_with_max_depth;
use crate::page_tree_rebuild::resolve_inherited_raw;
use crate::pages::{
    page_refs, resolve_inherited_resources_with_max_depth, DEFAULT_MAX_PAGE_TREE_DEPTH,
};
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek};

/// Extract page `page_index` (0-based) from `source` into a brand-new minimal
/// document.
///
/// Returns an owned in-memory [`Pdf`] whose catalog has a single-level
/// `/Pages` tree with one `/Kid`. Write it with
/// [`write_pdf_with_options`](crate::write_pdf_with_options) and
/// `WriteOptions { full_rewrite: true, .. }` so the (unreferenced) copied
/// ancestor `/Pages` nodes are dropped.
///
/// `source` is not modified.
///
/// # Errors
///
/// - [`Error::Unsupported`] if `page_index` is out of range.
/// - Propagates resolve/copy errors from the underlying primitives.
pub fn extract_page<R: Read + Seek>(
    source: &mut Pdf<R>,
    page_index: usize,
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    let all_pages = page_refs(source)?;
    let page_ref = *all_pages.get(page_index).ok_or_else(|| {
        Error::Unsupported(format!(
            "page index {page_index} out of range (document has {} pages)",
            all_pages.len()
        ))
    })?;

    // Resolve inherited attributes from the SOURCE before copying severs the
    // /Parent chain. Same four attributes / helpers as page_tree_rebuild.
    let depth = DEFAULT_MAX_PAGE_TREE_DEPTH;
    let inherited_resources = resolve_inherited_resources_with_max_depth(source, page_ref, depth)?;
    let inherited_rotate = resolve_inherited_rotate_with_max_depth(source, page_ref, depth)?;
    let inherited_mediabox = resolve_inherited_raw(source, page_ref, "MediaBox", depth)?;
    let inherited_cropbox = resolve_inherited_raw(source, page_ref, "CropBox", depth)?;

    // Transitive closure of the page, then deep-copy into a fresh minimal doc.
    let closure = page_object_closure(source, page_ref)?;
    let mut target = Pdf::open_mem_owned(minimal_target_bytes())?;
    let map = copy_objects(source, &mut target, &closure)?;

    let copied_page_ref = *map
        .get(&page_ref)
        .ok_or(Error::Missing("extracted page missing from copy map"))?;
    let pages_root_ref = target_pages_root(&mut target)?;

    // Materialize inherited attrs onto the copied leaf (remapping refs), then
    // repoint /Parent at the fresh root.
    let mut leaf = target
        .resolve_borrowed(copied_page_ref)?
        .as_dict()
        .cloned()
        .ok_or(Error::Missing("copied page is not a dictionary"))?;

    if !has_own(&leaf, "Resources") {
        if let Some(res) = inherited_resources {
            let mut value = Object::Dictionary(res);
            remap_refs(&mut value, &map);
            leaf.insert("Resources", value);
        }
    }
    if !has_own(&leaf, "MediaBox") {
        if let Some(mut mb) = inherited_mediabox {
            remap_refs(&mut mb, &map);
            leaf.insert("MediaBox", mb);
        }
    }
    if !has_own(&leaf, "CropBox") {
        if let Some(mut cb) = inherited_cropbox {
            remap_refs(&mut cb, &map);
            leaf.insert("CropBox", cb);
        }
    }
    if !has_own(&leaf, "Rotate") {
        leaf.insert("Rotate", Object::Integer(inherited_rotate as i64));
    }
    leaf.insert("Parent", Object::Reference(pages_root_ref));
    target.set_object(copied_page_ref, Object::Dictionary(leaf));

    // Build the fresh single-level /Pages root.
    let mut root = target
        .resolve_borrowed(pages_root_ref)?
        .as_dict()
        .cloned()
        .ok_or(Error::Missing("target /Pages is not a dictionary"))?;
    root.insert("Kids", Object::Array(vec![Object::Reference(copied_page_ref)]));
    root.insert("Count", Object::Integer(1));
    target.set_object(pages_root_ref, Object::Dictionary(root));

    Ok(target)
}

/// Minimal valid target: Catalog(1) + empty Pages(2). No placeholder page (so
/// there is no orphan to delete after copying).
fn minimal_target_bytes() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = out.len() as u64;
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!("xref\n0 3\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n")
            .as_bytes(),
    );
    out.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    out
}

/// Resolve the target catalog's `/Pages` root ref.
fn target_pages_root(target: &mut Pdf<Cursor<Vec<u8>>>) -> Result<ObjectRef> {
    let catalog_ref = target.root_ref().ok_or(Error::Missing("/Root"))?;
    let catalog = target
        .resolve_borrowed(catalog_ref)?
        .as_dict()
        .cloned()
        .ok_or(Error::Missing("/Root is not a dictionary"))?;
    catalog
        .get("Pages")
        .and_then(|o| match o {
            Object::Reference(r) => Some(*r),
            _ => None,
        })
        .ok_or(Error::Missing("/Pages"))
}

/// `true` when `dict` carries `key` as something other than `null`
/// (ISO 32000-1 §7.3.9: explicit `null` == absent). Mirrors
/// `page_tree_rebuild::leaf_has_own`.
fn has_own(dict: &Dictionary, key: &str) -> bool {
    !matches!(dict.get(key), None | Some(Object::Null))
}

/// Rewrite every indirect reference inside `obj` through `map`. Refs not present
/// in `map` (out-of-closure) become `Object::Null`, matching `copy_objects`'
/// out-of-set policy. Used to fix up materialized inherited attribute values,
/// whose refs point into the SOURCE document.
fn remap_refs(obj: &mut Object, map: &BTreeMap<ObjectRef, ObjectRef>) {
    match obj {
        Object::Reference(r) => {
            *obj = match map.get(r) {
                Some(target) => Object::Reference(*target),
                None => Object::Null,
            };
        }
        Object::Array(items) => {
            for item in items.iter_mut() {
                remap_refs(item, map);
            }
        }
        Object::Dictionary(dict) => {
            for value in dict.values_mut() {
                remap_refs(value, map);
            }
        }
        Object::Stream(stream) => {
            for value in stream.dict.values_mut() {
                remap_refs(value, map);
            }
        }
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => {}
    }
}
```

Add to `crates/flpdf/src/lib.rs` (near the other `mod`/`pub use` lines, e.g. next to `page_closure` / `object_copy`):
```rust
mod page_extract;
pub use page_extract::extract_page;
```
(Verify `Dictionary` is already publicly exported from `lib.rs`; the test uses `flpdf::Dictionary`. If not, add `pub use object::Dictionary;` — check first with `grep -n "Dictionary" crates/flpdf/src/lib.rs`.)

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf --test page_extract_tests`
Expected: PASS (1 test).

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_extract.rs crates/flpdf/src/lib.rs crates/flpdf/tests/page_extract_tests.rs
git commit -m "feat(extract): single-page extract primitive into fresh minimal doc [flpdf-5h5.3]"
```

---

## Task 3: Inherited attributes are materialized onto the extracted leaf

**Files:**
- Test: `crates/flpdf/tests/page_extract_tests.rs` (add fixture + test)

**Step 1: Write the failing test**

Add a fixture where the parent `/Pages` node carries `/MediaBox`, `/Resources`, and `/Rotate`, and the leaf page inherits all three:
```rust
/// Parent /Pages carries /MediaBox, /Resources (font), and /Rotate; the leaf
/// page (obj 3) inherits all three.
fn inherited_attrs_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 400 500] /Rotate 90 /Resources << /Font << /F1 5 0 R >> >> >>"),
            (3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>"),
            (4, "<< /Length 19 >>\nstream\nBT /F1 12 Tf ET\nendstream"),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        1,
    )
}

/// Fetch the single extracted leaf page dict.
fn only_leaf(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> flpdf::Dictionary {
    let refs = pages::page_refs(doc).unwrap();
    assert_eq!(refs.len(), 1);
    doc.resolve_borrowed(refs[0]).unwrap().as_dict().cloned().unwrap()
}

#[test]
fn materializes_inherited_attributes() {
    let src = inherited_attrs_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);

    // /MediaBox and /Rotate materialized verbatim.
    assert_eq!(
        leaf.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0), Object::Integer(0),
            Object::Integer(400), Object::Integer(500),
        ]))
    );
    assert_eq!(leaf.get("Rotate"), Some(&Object::Integer(90)));

    // /Resources materialized; its font ref points INTO the extracted doc and
    // resolves to a Helvetica font (i.e. the ref was remapped, not dangling).
    let res = leaf.get("Resources").and_then(|o| o.as_dict()).expect("/Resources");
    let font_ref = res
        .get("Font").and_then(|o| o.as_dict())
        .and_then(|f| f.get("F1"))
        .and_then(|o| match o { Object::Reference(r) => Some(*r), _ => None })
        .expect("/Font /F1 ref");
    let font = out.resolve_borrowed(font_ref).unwrap().as_dict().cloned().unwrap();
    assert_eq!(font.get("Subtype"), Some(&Object::Name(b"Type1".to_vec())));
}
```

**Step 2: Run test to verify it (likely) passes**

Run: `cargo test -p flpdf --test page_extract_tests::materializes_inherited_attributes`
Expected: PASS. (Implementation from Task 2 already handles this. If it fails, that is a real signal — debug the remap/materialize path before continuing. In particular confirm the font ref resolves rather than being `Null`.)

**Step 3: Commit**

```bash
git add crates/flpdf/tests/page_extract_tests.rs
git commit -m "test(extract): cover inherited-attribute materialization + ref remap [flpdf-5h5.3]"
```

---

## Task 4: Own attribute wins over inherited

**Files:**
- Test: `crates/flpdf/tests/page_extract_tests.rs`

**Step 1: Write the test**

Reuse `two_page_pdf()`: page 1 (obj 3) has its OWN `/MediaBox [0 0 612 792]` while the parent has none; extract it and assert the own box is preserved (not overwritten). Add a second extraction of page 2 (obj 4, own `/MediaBox [0 0 200 300]`) to confirm per-page boxes:
```rust
#[test]
fn own_mediabox_is_preserved() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut p0 = extract_page(&mut source, 0).unwrap();
    let leaf0 = only_leaf(&mut p0);
    assert_eq!(
        leaf0.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0), Object::Integer(0),
            Object::Integer(612), Object::Integer(792),
        ]))
    );

    let mut p1 = extract_page(&mut source, 1).unwrap();
    let leaf1 = only_leaf(&mut p1);
    assert_eq!(
        leaf1.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0), Object::Integer(0),
            Object::Integer(200), Object::Integer(300),
        ]))
    );
}
```

**Step 2: Run**

Run: `cargo test -p flpdf --test page_extract_tests::own_mediabox_is_preserved`
Expected: PASS.

**Step 3: Commit**

```bash
git add crates/flpdf/tests/page_extract_tests.rs
git commit -m "test(extract): own attribute wins over inherited [flpdf-5h5.3]"
```

---

## Task 5: No unrelated objects after full_rewrite (shared resources)

**Files:**
- Test: `crates/flpdf/tests/page_extract_tests.rs`

**Step 1: Write the test**

Build a two-page doc where both pages share the SAME font object, plus page 2 has an extra image XObject that page 1 does NOT use. Extract page 1, write with `full_rewrite`, reopen, and assert the extracted doc contains the shared font but NOT page 2's exclusive object.
```rust
use flpdf::{write_pdf_with_options, WriteOptions};

/// obj 6 = shared font (both pages); obj 7 = image used ONLY by page 2.
fn shared_resource_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >> /Contents 5 0 R >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> /XObject << /Im1 7 0 R >> >> >>"),
            (5, "<< /Length 19 >>\nstream\nBT /F1 12 Tf ET\nendstream"),
            (6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (7, "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /Length 3 >>\nstream\n\x00\x00\x00\nendstream"),
        ],
        1,
    )
}

/// Count how many objects in `doc` are Type1 fonts / Image XObjects.
fn count_subtype(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, subtype: &[u8]) -> usize {
    let mut n = 0;
    for r in doc.live_object_refs() {
        if let Ok(obj) = doc.resolve(r) {
            let dict = match &obj {
                Object::Dictionary(d) => Some(d.clone()),
                Object::Stream(s) => Some(s.dict.clone()),
                _ => None,
            };
            if let Some(d) = dict {
                if d.get("Subtype").and_then(|o| o.as_name()) == Some(subtype) {
                    n += 1;
                }
            }
        }
    }
    n
}

#[test]
fn extracted_doc_has_no_unrelated_objects() {
    let src = shared_resource_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();

    // Round-trip through full_rewrite to drop unreferenced copied ancestors.
    let mut bytes = Vec::new();
    let opts = WriteOptions { full_rewrite: true, ..Default::default() };
    write_pdf_with_options(&mut out, &mut bytes, &opts).unwrap();
    let mut rt = Pdf::open_mem_owned(bytes).unwrap();

    // Page 1's shared font survives; page 2's exclusive image does NOT.
    assert_eq!(count_subtype(&mut rt, b"Type1"), 1, "shared font must be present");
    assert_eq!(count_subtype(&mut rt, b"Image"), 0, "page 2's image must not leak in");
    assert_eq!(pages::page_refs(&mut rt).unwrap().len(), 1);
}
```
(If `live_object_refs` / `resolve` signatures differ, adjust; confirm with `grep -n "pub fn live_object_refs\|pub fn resolve\b" crates/flpdf/src/reader.rs`. Also confirm `WriteOptions` field names with `grep -n "pub full_rewrite\|pub struct WriteOptions" crates/flpdf/src/writer.rs` and that `WriteOptions: Default`.)

**Step 2: Run**

Run: `cargo test -p flpdf --test page_extract_tests::extracted_doc_has_no_unrelated_objects`
Expected: PASS.

**Step 3: Commit**

```bash
git add crates/flpdf/tests/page_extract_tests.rs
git commit -m "test(extract): verify no unrelated objects after full_rewrite [flpdf-5h5.3]"
```

---

## Task 6: Render-equivalence proxy + out-of-range error

**Files:**
- Test: `crates/flpdf/tests/page_extract_tests.rs`

**Step 1: Write the tests**

```rust
#[test]
fn extracted_contents_match_source_page() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    // Source page 0 content bytes.
    let src_pages = pages::page_refs(&mut source).unwrap();
    let src_leaf = source.resolve_borrowed(src_pages[0]).unwrap().as_dict().cloned().unwrap();
    let src_contents_ref = match src_leaf.get("Contents") {
        Some(Object::Reference(r)) => *r,
        other => panic!("expected /Contents ref, got {other:?}"),
    };
    let src_stream = match source.resolve(src_contents_ref).unwrap() {
        Object::Stream(s) => s,
        other => panic!("expected stream, got {other:?}"),
    };

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);
    let out_contents_ref = match leaf.get("Contents") {
        Some(Object::Reference(r)) => *r,
        other => panic!("expected /Contents ref, got {other:?}"),
    };
    let out_stream = match out.resolve(out_contents_ref).unwrap() {
        Object::Stream(s) => s,
        other => panic!("expected stream, got {other:?}"),
    };

    assert_eq!(out_stream.data, src_stream.data, "content stream bytes must be identical");
}

#[test]
fn out_of_range_index_errors() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();
    assert!(extract_page(&mut source, 2).is_err(), "index 2 is out of range (2 pages)");
}
```

**Step 2: Run**

Run: `cargo test -p flpdf --test page_extract_tests`
Expected: all tests PASS.

**Step 3: Commit**

```bash
git add crates/flpdf/tests/page_extract_tests.rs
git commit -m "test(extract): render-equivalence proxy + out-of-range error [flpdf-5h5.3]"
```

---

## Task 7: Runnable example + rustdoc cross-reference

**Files:**
- Create: `crates/flpdf/examples/extract_page.rs`
- Modify: `crates/flpdf/src/page_extract.rs` (rustdoc `# Examples` block, optional)

**Step 1: Write the example**

Create `crates/flpdf/examples/extract_page.rs` (model on existing `examples/extract_pages.rs` / `examples/merge_pdfs.rs` for arg parsing + writer setup):
```rust
//! Extract a single page (0-based) from a PDF into a new minimal PDF.
//!
//! Usage: cargo run --example extract_page -- <input.pdf> <page-index> <output.pdf>

use flpdf::{extract_page, write_pdf_with_options, Pdf, WriteOptions};
use std::fs::File;
use std::io::{BufReader, BufWriter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = args.next().ok_or("missing <input.pdf>")?;
    let index: usize = args.next().ok_or("missing <page-index>")?.parse()?;
    let output = args.next().ok_or("missing <output.pdf>")?;

    let mut source = Pdf::open(BufReader::new(File::open(&input)?))?;
    let mut extracted = extract_page(&mut source, index)?;

    let out = BufWriter::new(File::create(&output)?);
    let opts = WriteOptions { full_rewrite: true, ..Default::default() };
    write_pdf_with_options(&mut extracted, out, &opts)?;

    eprintln!("extracted page {index} from {input} -> {output}");
    Ok(())
}
```

**Step 2: Verify the example compiles**

Run: `cargo build -p flpdf --example extract_page`
Expected: builds clean.

**Step 3: Smoke-test against a fixture (if a test PDF exists)**

Run (adjust fixture path; find one with `ls crates/flpdf/tests/fixtures 2>/dev/null || find . -name '*.pdf' | head`):
`cargo run -p flpdf --example extract_page -- <some.pdf> 0 /tmp/extracted.pdf`
Expected: prints the success line, `/tmp/extracted.pdf` exists and reopens. (Skip if no fixture PDF is readily available; the integration tests already cover behavior.)

**Step 4: Commit**

```bash
git add crates/flpdf/examples/extract_page.rs crates/flpdf/src/page_extract.rs
git commit -m "docs(extract): runnable extract_page example [flpdf-5h5.3]"
```

---

## Task 8: Quality gates

**Step 1: Full crate test suite**

Run: `cargo test -p flpdf`
Expected: all green (no regressions).

**Step 2: Clippy**

Run: `cargo clippy -p flpdf --all-targets -- -D warnings`
Expected: no warnings. Fix any introduced by the new module.

**Step 3: Format**

Run: `cargo fmt -p flpdf` then `git diff --stat`
Expected: no/minor formatting changes; stage and amend the last commit or make a `style:` commit if anything changed.

**Step 4: Review against pdf-rust-review-patterns**

Re-read `.claude/rules/pdf-rust-review-patterns.md` and self-check the new code:
- No needless `.clone()` (the clone-modify-`set_object` of `leaf`/`root` is the established idiom — acceptable; the deep `resolve` clones in the test's `count_subtype` are test-only).
- Indirect refs resolved before type-matching (we operate on `resolve_borrowed` results).
- No unchecked unsigned casts of external integers (`inherited_rotate as i64` is i32→i64, widening, safe; `page_index` is a `usize` arg, bounds-checked via `.get`).
- Graph traversal is bounded (`remap_refs` walks only the small materialized attribute values, which are finite resolved objects with no cycles; the closure/copier already bound the main traversal).

**Step 5: Commit any fixes**

```bash
git add -A
git commit -m "chore(extract): clippy/fmt cleanup [flpdf-5h5.3]"
```

---

## Follow-up (file as a new beads issue at session end, do NOT implement here)

- **`/Info` copy on extract/merge.** `extract_page` ships a freshly-minimal trailer (no `/Info`). Copying the source document information dictionary needs a trailer-mutation API (none exists: `Pdf::trailer()` is read-only and `write_pdf_full_rewrite` preserves `/Info` verbatim from the source trailer). Scope: add a trailer/`/Info` setter (or a `WriteOptions.info` field) and wire it through extract + the upcoming merge primitive (5h5.6). This matches qpdf's separate document-level-data handling.
