# /Rotate Flattening Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Bake each page's effective `/Rotate` into the page content via a single
prepended `cm` matrix, transform every present page box and annotation `/Rect`
with the SAME matrix, then set `/Rotate=0` — so visual rendering is unchanged.

**Architecture:** One matrix `M` is the single source of truth. Content is wrapped
`q\n{M} cm\n … \nQ\n`; boxes and `/Rect` rects are transformed by mapping their 4
corners through `M` and taking the bounding box. `/Rotate` is materialized to `0`
on each leaf page (never inherited). Implemented as a new public function in
`crates/flpdf/src/page_rotate.rs`, mirroring `apply_rotate_to_pages`.

**Tech Stack:** Rust, flpdf crate (`Pdf<R>`, `Object`, `ObjectRef`, `Stream`,
`Dictionary`, `PageObjectHelper`, `pages::page_content_bytes`).

**Scope boundary (the "bytes-compat held for review" caveat):** Annotation
`/QuadPoints` and appearance `/AP` `/Matrix` are NOT rotated; only `/Rect` is.
So annotation appearance orientation may change. Document, do not silently ship.

**Byte-parity:** Not required. The new content stream is built from the page's
decoded content, so exact byte-identity with the original (or qpdf) is not a goal.
qpdf exposes `flattenRotation` only via its C++ API (no CLI), so there is no qpdf
oracle; tests assert invariants instead. No image renderer exists in the repo.

---

## Reference facts (already verified against the codebase)

- New-object allocation pattern (no pdf-level helper exists): `let n =
  pdf.object_refs().iter().map(|r| r.number).max().unwrap_or(0) + 1;
  let new = ObjectRef::new(n, 0); pdf.set_object(new, obj);`. Allocate → set →
  re-query for the next one (set_object updates `object_refs()`).
- `resolve_inherited_rotate(pdf, page_ref)` + `normalize_rotate(deg)` already exist
  in this file. Reuse them.
- `pages::page_content_bytes(pdf, page_ref)` returns the page's concatenated,
  filter-decoded content bytes (`Vec<u8>`), empty when no `/Contents`.
- `Stream::new(dict, data)`; `Object::Stream(Stream)`. The writer sets `/Length`,
  but set `/Length` explicitly on the new stream dict anyway.
- `PageBox { llx, lly, urx, ury: f64 }` lives in `page_object_helper.rs` and is
  re-exported from the crate root (confirm the import path during Task 1).
- Verified matrices for an origin-0 MediaBox `[0 0 W H]` (encode as tests):
  - 90:  `[0 -1 1 0 0 W]`  → box → `[0 0 H W]` (dims swap), old top → new right (CW)
  - 180: `[-1 0 0 -1 W H]` → dims unchanged
  - 270: `[0 1 -1 0 H 0]`  → box → `[0 0 H W]` (dims swap), old top → new left
  - General (non-zero origin): `M = T(-llx,-lly) ∘ Rorigin ∘ T(+llx,+lly)`.

---

## Task 1: Matrix primitives (pure functions)

**Files:**
- Modify: `crates/flpdf/src/page_rotate.rs` (add a private matrix section + tests)

**Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` (and add `use` for `PageBox` — check whether it
is `crate::PageBox` or `crate::page_object_helper::PageBox`):

```rust
#[test]
fn rotation_matrix_origin0_constants() {
    let mb = PageBox::new(0.0, 0.0, 200.0, 300.0); // W=200 H=300
    assert_eq!(rotation_matrix(90,  mb), [0.0, -1.0, 1.0, 0.0, 0.0, 200.0]);
    assert_eq!(rotation_matrix(180, mb), [-1.0, 0.0, 0.0, -1.0, 200.0, 300.0]);
    assert_eq!(rotation_matrix(270, mb), [0.0, 1.0, -1.0, 0.0, 300.0, 0.0]);
}

#[test]
fn apply_matrix_maps_points() {
    // 90deg: (x,y) -> (y, W - x), W=200
    let m = [0.0, -1.0, 1.0, 0.0, 0.0, 200.0];
    assert_eq!(apply_matrix(m, 0.0, 0.0), (0.0, 200.0));
    assert_eq!(apply_matrix(m, 200.0, 0.0), (0.0, 0.0));
    assert_eq!(apply_matrix(m, 0.0, 300.0), (300.0, 200.0));
}

#[test]
fn transform_box_swaps_dims_for_90_270() {
    let mb = PageBox::new(0.0, 0.0, 200.0, 300.0);
    let b90 = transform_box(rotation_matrix(90, mb), mb);
    assert_eq!((b90.llx, b90.lly, b90.urx, b90.ury), (0.0, 0.0, 300.0, 200.0));
    let b270 = transform_box(rotation_matrix(270, mb), mb);
    assert_eq!((b270.llx, b270.lly, b270.urx, b270.ury), (0.0, 0.0, 300.0, 200.0));
}

#[test]
fn transform_box_keeps_dims_for_180() {
    let mb = PageBox::new(0.0, 0.0, 200.0, 300.0);
    let b = transform_box(rotation_matrix(180, mb), mb);
    assert_eq!((b.llx, b.lly, b.urx, b.ury), (0.0, 0.0, 200.0, 300.0));
}

#[test]
fn rotation_matrix_preserves_lower_left_for_nonzero_origin() {
    // The transformed MediaBox lower-left must land back at the original (llx,lly).
    let mb = PageBox::new(10.0, 20.0, 210.0, 320.0); // W=200 H=300
    for r in [90, 180, 270] {
        let m = rotation_matrix(r, mb);
        let b = transform_box(m, mb);
        assert_eq!((b.llx, b.lly), (10.0, 20.0), "rotate {r} lower-left moved");
        let (w, h) = (b.urx - b.llx, b.ury - b.lly);
        if r == 180 { assert_eq!((w, h), (200.0, 300.0)); }
        else { assert_eq!((w, h), (300.0, 200.0)); }
    }
}
```

**Step 2: Run, verify they fail to compile / fail**

Run: `cargo test -p flpdf page_rotate::tests::rotation_matrix -- --nocapture`
Expected: FAIL — `rotation_matrix`/`apply_matrix`/`transform_box` not found.

**Step 3: Implement (private, above the tests module)**

```rust
// ---------------------------------------------------------------------------
// Affine matrix primitives for /Rotate flattening (flpdf-9hc.9.9)
// ---------------------------------------------------------------------------
// Row-vector convention: a point [x y 1] maps to
//   (a*x + c*y + e, b*x + d*y + f)  for matrix [a b c d e f].
type Mat = [f64; 6];

/// Apply matrix `m` to point (x, y).
fn apply_matrix(m: Mat, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// Compose A then B for a row vector: result == (p * A) * B.
fn mat_mul(a: Mat, b: Mat) -> Mat {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

fn translate(tx: f64, ty: f64) -> Mat { [1.0, 0.0, 0.0, 1.0, tx, ty] }

/// Origin-0 rotation matrix for a box of width `w`, height `h`. `r` must be one
/// of {90,180,270}; any other value yields identity (callers normalize first and
/// skip r==0).
fn rotate_origin(r: i32, w: f64, h: f64) -> Mat {
    match r {
        90 => [0.0, -1.0, 1.0, 0.0, 0.0, w],
        180 => [-1.0, 0.0, 0.0, -1.0, w, h],
        270 => [0.0, 1.0, -1.0, 0.0, h, 0.0],
        _ => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    }
}

/// Build the flatten matrix `M` for normalized rotation `r` and the page's
/// MediaBox: translate the box lower-left to origin, rotate, translate back so
/// the transformed box's lower-left returns to (llx, lly).
fn rotation_matrix(r: i32, mb: PageBox) -> Mat {
    let w = mb.urx - mb.llx;
    let h = mb.ury - mb.lly;
    mat_mul(
        mat_mul(translate(-mb.llx, -mb.lly), rotate_origin(r, w, h)),
        translate(mb.llx, mb.lly),
    )
}

/// Map the 4 corners of `b` through `m` and return their axis-aligned bbox.
fn transform_box(m: Mat, b: PageBox) -> PageBox {
    let corners = [
        apply_matrix(m, b.llx, b.lly),
        apply_matrix(m, b.urx, b.lly),
        apply_matrix(m, b.urx, b.ury),
        apply_matrix(m, b.llx, b.ury),
    ];
    let llx = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
    let lly = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
    let urx = corners.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max);
    let ury = corners.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max);
    PageBox::new(llx, lly, urx, ury)
}
```

**Step 4: Run tests, verify pass**

Run: `cargo test -p flpdf page_rotate::tests -- --nocapture`
Expected: PASS (new matrix tests green; existing tests still green).

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_rotate.rs
git commit -m "feat(rotate): matrix primitives for /Rotate flattening (flpdf-9hc.9.9)"
```

---

## Task 2: Box array <-> PageBox + inherited-present-box lookup

**Files:**
- Modify: `crates/flpdf/src/page_rotate.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn pagebox_array_roundtrip() {
    let b = PageBox::new(10.0, 20.5, 210.0, 320.0);
    let obj = pagebox_to_object(b);
    assert_eq!(object_to_pagebox(&obj), Some(b));
}

#[test]
fn object_to_pagebox_accepts_ints_and_reals() {
    let obj = Object::Array(vec![
        Object::Integer(0), Object::Real(1.5),
        Object::Integer(200), Object::Integer(300),
    ]);
    assert_eq!(object_to_pagebox(&obj), Some(PageBox::new(0.0, 1.5, 200.0, 300.0)));
}

#[test]
fn object_to_pagebox_rejects_wrong_arity() {
    assert_eq!(object_to_pagebox(&Object::Array(vec![Object::Integer(0)])), None);
    assert_eq!(object_to_pagebox(&Object::Integer(0)), None);
}
```

(An end-to-end test for `inherited_present_box` arrives in Task 4 against a real
synthetic PDF; do not over-mock the parent walk here.)

**Step 2: Run, verify fail.** `cargo test -p flpdf page_rotate::tests::pagebox`
Expected: FAIL — functions not found.

**Step 3: Implement**

```rust
/// Parse a PDF rectangle array `[x1 y1 x2 y2]` (ints or reals) into a `PageBox`.
/// Normalizes so llx<=urx and lly<=ury. Returns None on wrong shape.
fn object_to_pagebox(obj: &Object) -> Option<PageBox> {
    let Object::Array(a) = obj else { return None };
    if a.len() != 4 { return None; }
    let mut v = [0.0_f64; 4];
    for (i, e) in a.iter().enumerate() {
        v[i] = match e {
            Object::Integer(n) => *n as f64,
            Object::Real(r) => *r,
            _ => return None,
        };
    }
    Some(PageBox::new(v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])))
}

/// Emit a `PageBox` as a 4-element PDF real array.
fn pagebox_to_object(b: PageBox) -> Object {
    Object::Array(vec![
        Object::Real(b.llx), Object::Real(b.lly),
        Object::Real(b.urx), Object::Real(b.ury),
    ])
}
```

Then a presence-aware inherited lookup (mirror the parent-walk in
`resolve_inherited_rotate_with_max_depth` — read it first for the exact `/Parent`
resolution + cycle/depth guard idiom, and reuse `DEFAULT_MAX_PAGE_TREE_DEPTH`):

```rust
/// Return the explicit value of box `key` from the leaf page or the nearest
/// ancestor `/Pages` node that carries it. None if absent at every level (so the
/// box is left untouched — no invented boxes). No defaulting between box types.
fn inherited_present_box<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    key: &str,
) -> Result<Option<PageBox>> {
    let mut current = page_ref;
    let mut seen = BTreeSet::new();
    for _ in 0..=DEFAULT_MAX_PAGE_TREE_DEPTH {
        if !seen.insert(current) { break; } // cycle guard
        let Object::Dictionary(dict) = pdf.resolve(current)? else { break; };
        if let Some(obj) = dict.get(key) {
            // resolve in case the rect is an indirect reference
            let resolved = match obj {
                Object::Reference(r) => pdf.resolve(*r)?,
                other => other.clone(),
            };
            if let Some(b) = object_to_pagebox(&resolved) { return Ok(Some(b)); }
        }
        match dict.get("Parent") {
            Some(Object::Reference(p)) => current = *p,
            _ => break,
        }
    }
    Ok(None)
}
```

(If `dict.get` returns `Object` by value vs reference, adjust `&resolved`/clone to
match the actual `Dictionary` API — check `apply_rotate_to_pages` for the idiom.)

**Step 4: Run, verify pass.** `cargo test -p flpdf page_rotate::tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_rotate.rs
git commit -m "feat(rotate): box array parse/emit + inherited-present-box lookup (flpdf-9hc.9.9)"
```

---

## Task 3: Content-stream wrapper builder

**Files:**
- Modify: `crates/flpdf/src/page_rotate.rs`

**Step 1: Write failing test**

```rust
#[test]
fn wrap_content_has_safe_seams_and_cm() {
    let m = [0.0, -1.0, 1.0, 0.0, 0.0, 200.0];
    let inner = b"BT /F1 12 Tf (hi) Tj ET";
    let out = wrap_content_with_matrix(m, inner);
    let s = String::from_utf8(out).unwrap();
    // starts with q + the cm, newline-separated; matrix formatted compactly
    assert!(s.starts_with("q\n0 -1 1 0 0 200 cm\n"), "prefix: {s:?}");
    // original content preserved verbatim in the middle
    assert!(s.contains("BT /F1 12 Tf (hi) Tj ET"), "{s:?}");
    // suffix Q is separated from the preceding token by whitespace (no ET+Q merge)
    assert!(s.ends_with("ET\nQ\n"), "suffix: {s:?}");
}

#[test]
fn format_number_drops_trailing_zeros() {
    assert_eq!(format_matrix_number(200.0), "200");
    assert_eq!(format_matrix_number(-1.0), "-1");
    assert_eq!(format_matrix_number(1.5), "1.5");
    assert_eq!(format_matrix_number(0.0), "0");
}
```

**Step 2: Run, verify fail.**
`cargo test -p flpdf page_rotate::tests::wrap_content` Expected: FAIL.

**Step 3: Implement**

```rust
/// Format an f64 matrix operand without a trailing `.0` and without scientific
/// notation, so `cm` operands stay compact and valid (e.g. 200.0 -> "200").
fn format_matrix_number(v: f64) -> String {
    if v == v.trunc() && v.is_finite() {
        format!("{}", v as i64)
    } else {
        // up to 6 fractional digits, strip trailing zeros
        let s = format!("{v:.6}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        s.to_string()
    }
}

/// Wrap `inner` content bytes as `q\n{a b c d e f} cm\n{inner}\nQ\n`.
/// Whitespace at both seams prevents token merges (e.g. `ET`+`Q` -> `ETQ`).
fn wrap_content_with_matrix(m: Mat, inner: &[u8]) -> Vec<u8> {
    let cm = format!(
        "q\n{} {} {} {} {} {} cm\n",
        format_matrix_number(m[0]), format_matrix_number(m[1]),
        format_matrix_number(m[2]), format_matrix_number(m[3]),
        format_matrix_number(m[4]), format_matrix_number(m[5]),
    );
    let mut out = Vec::with_capacity(cm.len() + inner.len() + 4);
    out.extend_from_slice(cm.as_bytes());
    out.extend_from_slice(inner);
    out.extend_from_slice(b"\nQ\n");
    out
}
```

**Step 4: Run, verify pass.** `cargo test -p flpdf page_rotate::tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_rotate.rs
git commit -m "feat(rotate): content wrapper builder with safe seams (flpdf-9hc.9.9)"
```

---

## Task 4: `flatten_rotation_on_pages` — content + boxes + /Rotate=0

**Files:**
- Modify: `crates/flpdf/src/page_rotate.rs`
- Modify: `crates/flpdf/src/lib.rs` (re-export the new fn next to `apply_rotate_to_pages`)

**Step 1: Write failing E2E test** (mirror the existing test helpers in this
module — read the bottom of `page_rotate.rs` for how synthetic PDFs are built with
`pages`, `write_pdf`, `Cursor`):

```rust
#[test]
fn flatten_90_swaps_mediabox_zeroes_rotate_and_wraps_content() {
    // Build a 1-page PDF with MediaBox [0 0 200 300], /Rotate 90, simple content.
    let mut pdf = build_single_page_pdf(/* mediabox */ (0.0,0.0,200.0,300.0),
                                        /* rotate */ 90,
                                        /* content */ b"BT (x) Tj ET");
    let page = pages::page_refs(&mut pdf).unwrap()[0];
    flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

    // /Rotate materialized to 0
    let Object::Dictionary(d) = pdf.resolve(page).unwrap() else { panic!() };
    assert_eq!(d.get("Rotate"), Some(&Object::Integer(0)));

    // MediaBox dims swapped to [0 0 300 200]
    let mb = object_to_pagebox(d.get("MediaBox").unwrap()).unwrap();
    assert_eq!((mb.urx - mb.llx, mb.ury - mb.lly), (300.0, 200.0));

    // content now begins with q ... cm and still contains the original op
    let content = pages::page_content_bytes(&mut pdf, page).unwrap();
    let s = String::from_utf8(content).unwrap();
    assert!(s.starts_with("q\n0 -1 1 0 0 200 cm\n"), "{s:?}");
    assert!(s.contains("BT (x) Tj ET"));

    // round-trips through the writer + reparse without error
    let mut buf = Vec::new();
    crate::writer::write_pdf(&mut pdf, &mut buf).unwrap();
    let mut pdf2 = Pdf::open(Cursor::new(buf)).unwrap();
    let p2 = pages::page_refs(&mut pdf2).unwrap()[0];
    let Object::Dictionary(d2) = pdf2.resolve(p2).unwrap() else { panic!() };
    assert_eq!(d2.get("Rotate"), Some(&Object::Integer(0)));
}

#[test]
fn flatten_is_noop_when_rotate_zero() {
    let mut pdf = build_single_page_pdf((0.0,0.0,200.0,300.0), 0, b"BT (x) Tj ET");
    let page = pages::page_refs(&mut pdf).unwrap()[0];
    let before = pages::page_content_bytes(&mut pdf, page).unwrap();
    flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();
    let after = pages::page_content_bytes(&mut pdf, page).unwrap();
    assert_eq!(before, after, "content must be untouched when rotate==0");
    let Object::Dictionary(d) = pdf.resolve(page).unwrap() else { panic!() };
    let mb = object_to_pagebox(d.get("MediaBox").unwrap()).unwrap();
    assert_eq!((mb.urx, mb.ury), (200.0, 300.0)); // unchanged
}

#[test]
fn flatten_materializes_inherited_rotate() {
    // /Rotate 270 on the /Pages root, none on the leaf -> leaf must end at 0.
    let mut pdf = build_single_page_pdf_inherited_rotate(270);
    let page = pages::page_refs(&mut pdf).unwrap()[0];
    flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();
    let Object::Dictionary(d) = pdf.resolve(page).unwrap() else { panic!() };
    assert_eq!(d.get("Rotate"), Some(&Object::Integer(0)));
}
```

Write `build_single_page_pdf(...)` / `build_single_page_pdf_inherited_rotate(...)`
test helpers in this module's test mod, following the construction style already
used by the existing `apply_rotate_to_pages` tests (allocate catalog/pages/page +
a content stream, `write_pdf` to bytes, `Pdf::open`).

**Step 2: Run, verify fail.** `cargo test -p flpdf page_rotate::tests::flatten`
Expected: FAIL — `flatten_rotation_on_pages` not found.

**Step 3: Implement**

```rust
/// Box keys flattened, in a deterministic order.
const FLATTEN_BOX_KEYS: [&str; 5] =
    ["MediaBox", "CropBox", "BleedBox", "TrimBox", "ArtBox"];

/// Flatten the effective `/Rotate` of each leaf page into its content stream and
/// geometry. For each page: resolve the inherited+normalized rotation; if 0, skip.
/// Otherwise build matrix `M` from the rotation and the effective `/MediaBox`,
/// wrap the page content with `M`, transform every present box and annotation
/// `/Rect` with `M`, and materialize `/Rotate = 0` on the leaf.
///
/// CAVEAT (flpdf-9hc.9.9, held for review): annotation `/QuadPoints` and the
/// appearance `/AP` `/Matrix` are NOT transformed — only `/Rect`. Annotation
/// appearance orientation may therefore change. Byte-identity with the source is
/// not preserved (content is rebuilt from decoded bytes).
pub fn flatten_rotation_on_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    pages: &[ObjectRef],
) -> Result<()> {
    for &page_ref in pages {
        let rotate = normalize_rotate(resolve_inherited_rotate(pdf, page_ref)?);
        if rotate == 0 {
            continue; // nothing to flatten
        }

        // MediaBox is required for a sane matrix; without it we cannot flatten.
        let Some(mb) = inherited_present_box(pdf, page_ref, "MediaBox")? else {
            return Err(Error::Unsupported(format!(
                "page {page_ref} has no /MediaBox; cannot flatten /Rotate"
            )));
        };
        let m = rotation_matrix(rotate, mb);

        // 1. Content: wrap with `q M cm ... Q` (skip when the page has no content).
        let inner = pages::page_content_bytes(pdf, page_ref)?;
        let new_content = if inner.is_empty() {
            // Still emit the cm wrapper around empty content so future content
            // additions inherit the transform consistently; cheap and harmless.
            wrap_content_with_matrix(m, b"")
        } else {
            wrap_content_with_matrix(m, &inner)
        };

        // 2. Resolve the leaf page dict (guard: must be a /Type /Page).
        let Object::Dictionary(mut page_dict) = pdf.resolve(page_ref)? else {
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a dictionary, cannot flatten /Rotate"
            )));
        };
        if !matches!(page_dict.get("Type"),
                     Some(Object::Name(t)) if t.as_slice() == b"Page") {
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a leaf /Page, cannot flatten /Rotate"
            )));
        }

        // 3. Allocate a fresh content stream object and point /Contents at it.
        let mut sdict = Dictionary::new();
        sdict.insert("Length", Object::Integer(new_content.len() as i64));
        let stream_ref = next_object_ref(pdf)?;
        pdf.set_object(stream_ref, Object::Stream(Stream::new(sdict, new_content)));
        page_dict.insert("Contents", Object::Reference(stream_ref));

        // 4. Transform every present box; materialize on the leaf.
        for key in FLATTEN_BOX_KEYS {
            if let Some(b) = inherited_present_box(pdf, page_ref, key)? {
                page_dict.insert(key, pagebox_to_object(transform_box(m, b)));
            }
        }

        // 5. Materialize /Rotate = 0 on the leaf (never inherited).
        page_dict.insert("Rotate", Object::Integer(0));

        // 6. Annotations: transform /Rect (Task 5 fills this in).
        flatten_annotation_rects(pdf, &page_dict, m)?;

        pdf.set_object(page_ref, Object::Dictionary(page_dict));
    }
    Ok(())
}

/// Local copy of the new-object-ref allocation idiom used across the crate.
fn next_object_ref<R: Read + Seek>(pdf: &Pdf<R>) -> Result<ObjectRef> {
    let n = pdf.object_refs().iter().map(|r| r.number).max().unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".into()))?;
    Ok(ObjectRef::new(n, 0))
}
```

For this task, stub `flatten_annotation_rects` as a no-op returning `Ok(())` so the
build passes; Task 5 implements it. Add the needed `use` for `Stream`,
`Dictionary` (check whether they are already imported / available as
`crate::Stream`). Re-export in `lib.rs`.

> **Note on `inherited_present_box` + a just-inserted box:** within one page's
> loop, `inherited_present_box` re-resolves from `pdf` for each key. Since we have
> not yet written `page_dict` back (`set_object` is at the end), those reads see
> the ORIGINAL boxes — which is exactly what we want (transform each original box
> once). Do not move `set_object` earlier.

**Step 4: Run, verify pass.** `cargo test -p flpdf page_rotate::tests::flatten`
Expected: PASS (annotation test still pending).

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_rotate.rs crates/flpdf/src/lib.rs
git commit -m "feat(rotate): flatten_rotation_on_pages content+boxes+/Rotate=0 (flpdf-9hc.9.9)"
```

---

## Task 5: Annotation `/Rect` transform

**Files:**
- Modify: `crates/flpdf/src/page_rotate.rs`

**Step 1: Write failing test**

```rust
#[test]
fn flatten_transforms_annotation_rect() {
    // 1-page PDF, MediaBox [0 0 200 300], /Rotate 90, one annot with
    // /Rect [10 20 60 40].
    let mut pdf = build_single_page_pdf_with_annot(
        (0.0,0.0,200.0,300.0), 90, ((10.0,20.0,60.0,40.0)));
    let page = pages::page_refs(&mut pdf).unwrap()[0];
    let annot = first_annot_ref(&mut pdf, page);
    flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

    // 90deg map (x,y)->(y, 200 - x): corners (10,20),(60,40) ->
    // (20,190),(40,140) -> bbox [20 140 40 190].
    let Object::Dictionary(ad) = pdf.resolve(annot).unwrap() else { panic!() };
    let r = object_to_pagebox(ad.get("Rect").unwrap()).unwrap();
    assert_eq!((r.llx, r.lly, r.urx, r.ury), (20.0, 140.0, 40.0, 190.0));
}
```

**Step 2: Run, verify fail.** Expected: FAIL (rect unchanged / helper is a stub).

**Step 3: Implement** (replace the Task-4 stub)

```rust
/// Transform every annotation's `/Rect` on this page by `m`. Reads `/Annots`
/// from `page_dict`; each entry is an indirect reference to an annotation dict.
/// CAVEAT: /QuadPoints and /AP /Matrix are intentionally left untouched.
fn flatten_annotation_rects<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_dict: &Dictionary,
    m: Mat,
) -> Result<()> {
    let Some(Object::Array(annots)) = page_dict.get("Annots").cloned() else {
        return Ok(());
    };
    for entry in annots {
        let Object::Reference(annot_ref) = entry else { continue };
        let Object::Dictionary(mut ad) = pdf.resolve(annot_ref)? else { continue };
        if let Some(rect_obj) = ad.get("Rect") {
            let resolved = match rect_obj {
                Object::Reference(r) => pdf.resolve(*r)?,
                other => other.clone(),
            };
            if let Some(b) = object_to_pagebox(&resolved) {
                ad.insert("Rect", pagebox_to_object(transform_box(m, b)));
                pdf.set_object(annot_ref, Object::Dictionary(ad));
            }
        }
    }
    Ok(())
}
```

(Adjust `.get(...)` clone/borrow to match the actual `Dictionary` API, as in Task 2.)

**Step 4: Run, verify pass.** `cargo test -p flpdf page_rotate::tests::flatten`
Expected: PASS (all flatten tests).

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_rotate.rs
git commit -m "feat(rotate): transform annotation /Rect on flatten (flpdf-9hc.9.9)"
```

---

## Task 6: Module docs, caveat, and full verification

**Files:**
- Modify: `crates/flpdf/src/page_rotate.rs` (module-level doc comment)
- Modify: `CHANGELOG.md` (add an entry under unreleased)

**Step 1:** Extend the module `//!` header to document `flatten_rotation_on_pages`
and the **scope-boundary caveat** (Rect-only annotations; /QuadPoints & /AP /Matrix
untouched; content rebuilt from decoded bytes so byte-identity is not preserved;
no qpdf CLI oracle). Add a CHANGELOG entry referencing flpdf-9hc.9.9.

**Step 2: Full verification (no shortcuts)**

```bash
cargo test -p flpdf
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Expected: all green. Fix any clippy/fmt findings before continuing.

**Step 3: Commit**

```bash
git add crates/flpdf/src/page_rotate.rs CHANGELOG.md
git commit -m "docs(rotate): document /Rotate flattening + caveat (flpdf-9hc.9.9)"
```

---

## Out of scope (filed as follow-ups, not done here)

- CLI flag `--flatten-rotation` — that is **flpdf-9hc.9.10** (this fn is the
  library primitive it will call).
- Rotating `/QuadPoints` and appearance `/AP` `/Matrix` — the documented caveat.
- Image-diff visual regression — no renderer in the repo; invariants are asserted
  instead.
