# Overlay Box-Geometry Normalization (flpdf-lkk7) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Normalize all box geometry reads the way qpdf's `getArrayAsRectangle` does
(`llx=min(x0,x2)`, `urx=max(x0,x2)`, …) so overlay/underlay output for pages with a
swapped/reversed box (llx>urx or lly>ury) is byte-identical to qpdf 11.9.0.

**Architecture:** Three in-place edits at the points where raw box coordinates flow
into placement math: (A) `rectangle_dimensions` → Form `/Matrix` dims; (B) the dest
inverse `tmatrix` dims in `apply_overlays_to_page`; (C) the placement `rect`
(`trim_rect`/`media_rect`). The Form `/BBox` and the output page boxes stay RAW
(qpdf `shallowCopy`s them). The `fo_bbox` fed to placement is left RAW on purpose:
`transform_bbox` already takes the min/max of the transformed corners, so a swapped
input yields the identical axis-aligned box (mirrors qpdf `transformRectangle`).

**Tech Stack:** Rust (`crates/flpdf`), qpdf 11.9.0 oracle, feature-gated byte gates
(`qpdf-zlib-compat`), Python fixture generators in `tests/golden/regenerate.sh`.

---

## Verified qpdf 11.9.0 references (do not re-derive)

- `libqpdf/QPDFObjectHandle.cc::getArrayAsRectangle` — min/max normalize; `{}` unless
  exactly 4 numeric elements.
- `libqpdf/QPDFPageObjectHelper.cc`:
  - `getMatrixForTransformations` (`rect=getTrimBox().getArrayAsRectangle()`) → dims for
    the `/Rotate`+`/UserUnit` Form `/Matrix` AND for the inverse `tmatrix`.
  - `getMatrixForFormXObjectPlacement` — `rect` param arrives normalized from
    `doUnderOverlayForPage` (`getTrimBox()/getMediaBox().getArrayAsRectangle()`); fo
    bbox read via `getArrayAsRectangle` (no-op given `transformRectangle`).
  - `getFormXObjectForPage` — `/BBox = getTrimBox(false).shallowCopy()` (RAW).

---

## Task 1: `normalize_rectangle` helper + placement rect normalization (Edit C)

**Files:**
- Modify: `crates/flpdf/src/overlay.rs` (add helper near `transform_bbox`; edit
  `apply_overlays_to_page` lines ~295-296)
- Test: `crates/flpdf/src/overlay.rs` `#[cfg(test)] mod tests`

**Step 1: Write the failing unit test** (in `mod tests`)

```rust
#[test]
fn normalize_rectangle_orders_swapped_corners() {
    // Reversed box [612 792 0 0] -> [0 0 612 792]; already-ordered box unchanged.
    assert_eq!(normalize_rectangle([612.0, 792.0, 0.0, 0.0]), [0.0, 0.0, 612.0, 792.0]);
    assert_eq!(normalize_rectangle([0.0, 0.0, 612.0, 792.0]), [0.0, 0.0, 612.0, 792.0]);
}

#[test]
fn place_swapped_rect_uses_normalized_width() {
    // A reversed destination rect must place exactly like its normalized form:
    // rect_w/rect_h take |urx-llx| (qpdf getArrayAsRectangle), r_cx/r_cy invariant.
    let fo_bbox = [0.0, 0.0, 100.0, 100.0];
    let id = IDENTITY_MATRIX;
    let swapped = matrix_for_form_xobject_placement(fo_bbox, id, [200.0, 200.0, 0.0, 0.0], id, true, true);
    let normalized = matrix_for_form_xobject_placement(fo_bbox, id, [0.0, 0.0, 200.0, 200.0], id, true, true);
    assert_eq!(swapped, normalized);
}
```

**Step 2: Run to verify failure** — `cargo test -p flpdf normalize_rectangle place_swapped_rect`
Expected: `normalize_rectangle` fails to compile (fn missing); `place_swapped_rect` fails
(swapped rect produces a different/negative-scale matrix).

**Step 3: Implement**

Add helper (doc references `getArrayAsRectangle`):

```rust
/// Normalize a rectangle's corners the way qpdf's
/// `QPDFObjectHandle::getArrayAsRectangle` does: `llx = min(x0, x2)`,
/// `lly = min(x1, x3)`, `urx = max(x0, x2)`, `ury = max(x1, x3)`. qpdf reads all
/// box geometry through this accessor, so a page with a reversed box (llx > urx or
/// lly > ury) still yields non-negative width/height and places identically.
fn normalize_rectangle([x0, x1, x2, x3]: [f64; 4]) -> [f64; 4] {
    [x0.min(x2), x1.min(x3), x0.max(x2), x1.max(x3)]
}
```

Edit the placement-rect construction in `apply_overlays_to_page`:

```rust
let trim_rect = normalize_rectangle(page_box_array(&trim_box));
let media_rect = normalize_rectangle(page_box_array(&media_box));
```

**Step 4: Run** — `cargo test -p flpdf overlay::` → PASS. Commit.

---

## Task 2: Inverse-transform `tmatrix` dims normalization (Edit B)

> **REVISION (during impl):** Edit B is an **output no-op** and is kept ONLY for
> qpdf structural fidelity (mandate: reproduce qpdf's computation, do not "improve").
> The box width/height feed only the tmatrix TRANSLATION column
> (`transformation_matrix` puts `width*scale`/`height*scale` in positions e/f, never
> a/b/c/d), and the placement centring (`tx = r_cx - t_cx`) absorbs that translation.
> Verified empirically: reverting Edit B leaves all 18 byte gates green; reverting
> Edit A (which IS serialized into the `/Matrix` array) fails the swapped+r90 gate.
> So **no byte-gate or `/Contents`-compare test can isolate Edit B** — unlike the
> issue's original premise. This is the inverse of the fo_bbox case: fo_bbox does an
> *equivalent* computation (transform_bbox already min/maxes), whereas dropping Edit B
> would make the intermediate tmatrix genuinely differ from qpdf's. Per the
> byte-identical mandate, KEEP it and document the no-op (do not delete as dead code).

**Files:**
- Modify: `crates/flpdf/src/overlay.rs` `apply_overlays_to_page` lines ~262-263

**Step 1: Implement** — normalize the tmatrix dims (covered by existing non-gated
`apply_*` tests for line coverage; no isolating behavioural test is possible). The
non-gated `apply_swapped_box_with_rotate_matches_normalized` test still passes (it is
discriminated by Edit A's `/Matrix` and Edit C's rect, not by Edit B):

```rust
let [n_llx, n_lly, n_urx, n_ury] = normalize_rectangle(page_box_array(&trim_box));
let trim_w = n_urx - n_llx;
let trim_h = n_ury - n_lly;
```

---

## Task 3: `rectangle_dimensions` normalization (Edit A)

**Files:**
- Modify: `crates/flpdf/src/page_form_xobject.rs:355-368`
- Test: `crates/flpdf/src/page_form_xobject.rs` `#[cfg(test)] mod tests`

**Step 1: Failing test**

```rust
#[test]
fn rectangle_dimensions_normalizes_swapped_box() {
    use crate::pdf_object::Object;
    let swapped = [Object::Integer(612), Object::Integer(792), Object::Integer(0), Object::Integer(0)];
    assert_eq!(rectangle_dimensions(&swapped), (612.0, 792.0));
}
```

**Step 2: Run** — `cargo test -p flpdf rectangle_dimensions_normalizes` → FAIL
(returns `(-612.0, -792.0)`).

**Step 3: Implement** — change the return to normalized magnitudes and update the doc
comment to reference `getArrayAsRectangle`:

```rust
// width/height are the normalized extents (qpdf reads box geometry through
// getArrayAsRectangle, so they are always non-negative even for a reversed box).
((urx - llx).abs(), (ury - lly).abs())
```

**Step 4: Run** → PASS. Also run existing `page_to_form_xobject_*` tests. Commit.

---

## Task 4: Swapped-box fixtures + qpdf goldens + byte gate (parity proof)

**Files:**
- Modify: `tests/golden/regenerate.sh` (new fixture(s) + golden recipe(s))
- Create: `tests/fixtures/compat/swapped-box-one-page.pdf` (generated)
- Create: `tests/golden/references/overlay/<...>.pdf` (generated)
- Modify: `crates/flpdf/src/overlay.rs` `mod byte_gate` (new `#[test]`) + matrix comment

**Step 1: Add a Python fixture generator** to `regenerate.sh` mirroring the
`userunit-one-page.pdf` block: a one-page doc whose `/MediaBox` is reversed
(`[612 792 0 0]`), normalized through `qpdf --static-id --force-version=1.3`.
**VERIFY** the reversed box survives the qpdf pass (`qpdf --json` / `--qdf` shows
`/MediaBox [612 792 0 0]`); if qpdf reorders it, build the fixture WITHOUT the qpdf
normalize pass.

For full coverage of Edits A+B, also add a `swapped-box-r90-one-page.pdf`
(reversed box + `/Rotate 90`) used as both dest and source so the source `/Matrix`
(A), the dest tmatrix (B), and the placement rects (C) are all exercised end-to-end.

**Step 2: Add the golden recipe(s)** in the overlay section of `regenerate.sh`:

```bash
qpdf --static-id "$FIX/swapped-box-one-page.pdf" --overlay "$FIX/one-page.pdf" -- \
    "$REF/overlay/swapped-box-overlay-one-page.pdf"
```
(plus the r90 combo recipe). Run `bash tests/golden/regenerate.sh`.

**Step 3: Add the byte-gate test(s)** to `mod byte_gate`, modeled on
`overlay_onto_rotated_dest_is_byte_identical`; add the new row(s) to the matrix
comment at the top of the gate.

**Step 4: Run the gate** (feature on):
`cargo test -p flpdf --features qpdf-zlib-compat byte_gate` → PASS.

**Step 5: Discriminating check** — temporarily revert each of Edits A/B/C and confirm
the relevant gate fails; restore. Commit fixtures + goldens + gate.

---

## Task 5: Verification & gates

- `cargo test -p flpdf` (no feature) — all green.
- `cargo test -p flpdf --features qpdf-zlib-compat` — all byte gates green (regression:
  normalization is identity for ordered boxes, so existing goldens stay byte-identical).
- `cargo fmt --all` then `cargo fmt --all --check` (memory: CI quality = fmt check).
- `cargo clippy -p flpdf` — no new warnings.
- `scripts/patch-coverage.sh --base main` — flpdf changed lines 100% (commit first; the
  byte-gate is feature-gated so its lines are NOT measured — the non-gated unit tests
  from Tasks 1-3 must cover the changed call sites/edits).
- Qualitative: error/boundary arms — swapped vs ordered both asserted; no untestable
  lines added (no `cov:ignore` expected).

---

## Out of scope (documented, not implemented)

- `getArrayAsRectangle`'s "exactly 4 elements else empty" semantics (flpdf accepts
  `>= 4`): pre-existing, orthogonal to swapped-box parity.
