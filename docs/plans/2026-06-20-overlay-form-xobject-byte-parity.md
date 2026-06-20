# Overlay/Underlay Form XObject Byte-Parity (flpdf-9hc.16.10) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Close four byte-identity gaps vs qpdf 11.9.0 in overlay/underlay: conditional `/Matrix` emission, `/UserUnit` scale, content-stream newline coalescing, and destination-page inverse-transform placement (`/Fx0` and sources).

**Architecture:** Faithfully port qpdf logic (oracle: `libqpdf/QPDFPageObjectHelper.cc`, `QPDFJob.cc`, `QPDFObjectHandle.cc`, `QPDFMatrix.cc`, all read & verified). Order **B → A → C → run existing byte_gate → D** so the 10 existing `qpdf-zlib-compat` overlay goldens act as the regression net before any new fixture lands. Existing fixtures are all single-stream with explicit `/Rotate 0` and non-rotated dest pages, so B/A/C must leave every existing golden byte-identical.

**Tech Stack:** Rust (`crates/flpdf`), qpdf 11.9.0 oracle (`/tmp/qpdf-1190`, binary `/usr/bin/qpdf`), `cargo test --features qpdf-zlib-compat`, `scripts/patch-coverage.sh`.

**Key qpdf facts (verified from source):**
- `getFormXObjectForPage`: emits `/Matrix` iff `!(getAttribute("/Rotate",false).isNull() && getAttribute("/UserUnit",false).isNull())`. `/Rotate` inherits via `/Parent` (qpdf inherits only MediaBox/CropBox/Resources/Rotate); `/UserUnit` is leaf-only.
- `getMatrixForTransformations`: `scale = UserUnit if isNumber else 1.0`; `rotate = raw getIntValueAsInt()` (NOT normalized) switched on 90/180/270/default; width/height from `getTrimBox(false)`. `invert` ⇒ `scale=1/scale` (guard `scale==0` returns identity), `rotate=360-rotate`.
- `pipeContentStreams`: insert `\n` between streams ONLY when previous decoded stream's last byte `!= '\n'` (fresh `LastChar{0}` per stream ⇒ an empty stream forces a newline). No trailing newline.
- `placeFormXObject` header default `invert_transformations = true`. `doUnderOverlayForPage`: sources ⇒ `(invert=true, allow_shrink=true, allow_expand=false)`, rect=dest TrimBox; `/Fx0` ⇒ `(invert=true, allow_shrink=false, allow_expand=false)`, rect=dest MediaBox. Both fold in `tmatrix = dest_page.getMatrixForTransformations(true)`.
- `QPDFMatrix::concat(other)` exact: `ap=a*oa+c*ob; bp=b*oa+d*ob; cp=a*oc+c*od; dp=b*oc+d*od; ep=a*oe+c*of+e; fp=b*oe+d*of+f`. `unparse` = `double_to_string(fix_rounding(x),5)`, `fix_rounding` zeroes `(-0.00001, 0.00001)`. Final `cm = translate(tx,ty)·scale(s)·tmatrix`.

---

## Task B: Content-stream newline coalescing (`pages.rs`)

**Files:**
- Modify: `crates/flpdf/src/pages.rs` (`page_content_bytes` ~L137-146, doc ~L66/137)
- Test: `crates/flpdf/src/pages.rs` (`#[cfg(test)] mod tests`, update ~L967 & ~L1799, add empty-stream case)

**Step 1 — Update the two unit tests + add empty-stream test (failing).** Rename/rewrite the `..._with_space_separator` tests to expect a single `\n` between two non-newline-terminated streams; add a test where stream 1 already ends in `\n` (no separator added) and a test where stream 1 is empty (forces a `\n`).

**Step 2 — Run, expect FAIL.** `cargo test -p flpdf --lib pages::tests::page_content_bytes`

**Step 3 — Implement qpdf coalescing.** Replace the `if i > 0 { result.push(b' ') }` block with:
```rust
let mut result: Vec<u8> = Vec::new();
let mut need_newline = false;
for stream in streams {
    let decoded = decode_stream_data(&stream.dict, &stream.data)?;
    if need_newline {
        result.push(b'\n');
    }
    // qpdf LastChar resets per stream (init 0); an empty stream forces a newline.
    let last = decoded.last().copied().unwrap_or(0);
    need_newline = last != b'\n';
    result.extend_from_slice(&decoded);
}
Ok(result)
```
Update the module/function doc to describe the qpdf `pipeContentStreams` rule (not "single ASCII space"). Add a `//` note that `coalesce_page_contents` has the same latent unconditional-`\n` divergence (out of scope; follow-up issue).

**Step 4 — Run, expect PASS.** Same filter.

**Step 5 — Commit.** `git add -A && git commit -m "fix(overlay): page_content_bytes uses qpdf newline coalescing (flpdf-9hc.16.10 B)"`

---

## Task A: Conditional `/Matrix` + `/UserUnit` scale (`page_form_xobject.rs`)

**Files:**
- Modify: `crates/flpdf/src/page_form_xobject.rs` (`page_to_form_xobject` ~L78-99, replace `matrix_for_rotation` ~L365-408)
- Test: same file, `#[cfg(test)] mod tests`

**Step 1 — Failing tests.** Add: page with NO `/Rotate`/`/UserUnit` ⇒ `/Matrix` absent (key set is BBox/Resources/Subtype/Type); page with explicit `/Rotate 0` ⇒ `/Matrix [1 0 0 1 0 0]` present (guard existing behavior); inherited `/Rotate` from `/Pages` ⇒ `/Matrix` present; `/UserUnit 2` with no `/Rotate` ⇒ `/Matrix [2 0 0 2 0 0]`; `/UserUnit 2 /Rotate 90` on 612×792 ⇒ `/Matrix [0 -2 2 0 0 1224]`; present-but-non-integer `/Rotate` (e.g. `/Rotate /X`) ⇒ `/Matrix` present, identity; present-but-non-numeric `/UserUnit` ⇒ `/Matrix` present, scale 1.0.

**Step 2 — Run, expect FAIL.** `cargo test -p flpdf --lib page_form_xobject::tests`

**Step 3 — Implement.**
- Add `pub(crate) struct PageTransform { pub rotate_present: bool, pub rotate: i32, pub uu_present: bool, pub scale: f64 }`.
- `pub(crate) fn read_page_transform<R>(pdf, page_ref) -> Result<PageTransform>`: walk `/Parent` (depth/cycle guarded like `inherited_box_array`) for `/Rotate` presence+raw int (resolve refs; integer ⇒ value, non-integer-non-null ⇒ present, value 0); read leaf `/UserUnit` (resolve ref; `Integer|Real` ⇒ present+value, other non-null ⇒ present+1.0).
- `pub(crate) fn transformation_matrix(t: &PageTransform, width: f64, height: f64, invert: bool) -> [f64;6]` mirroring `getMatrixForTransformations` exactly (identity when `!(rotate_present||uu_present)`; invert branch with `scale==0` ⇒ identity, `rotate=360-rotate`; switch 90/180/270/default; emit as the existing `Object::Real` later).
- In `page_to_form_xobject`: replace the `rotate()` + unconditional insert with: `let t = read_page_transform(pdf, page_ref)?;` then `if t.rotate_present || t.uu_present { dict.insert("Matrix", Object::Array(matrix_objects(transformation_matrix(&t, bbox_w, bbox_h, false)))); }`. Keep BBox/Resources/Group/stream logic unchanged. Remove now-dead `matrix_for_rotation` / `PageObjectHelper::rotate()` use.

**Step 4 — Run, expect PASS.** Same filter.

**Step 5 — Commit.** `git add -A && git commit -m "fix(overlay): getFormXObjectForPage conditional /Matrix + /UserUnit scale (flpdf-9hc.16.10 A)"`

---

## Task C: Inverse-transform placement for `/Fx0` and sources (`overlay.rs`)

**Files:**
- Modify: `crates/flpdf/src/overlay.rs` (`apply_overlays_to_page` ~L156-201; replace `place_form_xobject` ~L81-122 and `xobject_placement_box` ~L606-645; extend `fmt_number` with `fix_rounding`)
- Test: same file, `#[cfg(test)] mod tests`

**Step 1 — Failing tests.** Unit tests on the new matrix helpers: `qpdf_concat` against a hand-computed product; `transform_rectangle` for a 90° matrix; `matrix_unparse` applies `fix_rounding` (`0.000004` ⇒ `"0"`); `matrix_for_form_xobject_placement` for the non-rotated/identity case reproduces `[scale 0 0 scale tx ty]` (TrimBox==MediaBox ⇒ `1 0 0 1 0 0`); rotated-dest `/Fx0` case (612×792 `/Rotate 90`) produces a matrix with nonzero b/c; `allow_shrink=false` clamps a <1 scale to 1; degenerate bbox ⇒ `None`.

**Step 2 — Run, expect FAIL.** `cargo test -p flpdf --lib overlay::tests`

**Step 3 — Implement.**
- Add a private QPDFMatrix-equivalent on `[f64;6]`: `IDENTITY`, `qpdf_concat(this, other)`, `qpdf_scale(m, s)`, `qpdf_translate(m, tx, ty)`, `transform_rectangle(m, [f64;4]) -> [f64;4]`, `matrix_unparse(m) -> String` (6× `fmt_number(fix_rounding(x))`, space-joined). Add `fn fix_rounding(d: f64) -> f64 { if d > -0.00001 && d < 0.00001 { 0.0 } else { d } }`. Do NOT reuse `page_rotate::mat_mul` (different convention).
- `fn matrix_for_form_xobject_placement(fo_bbox: [f64;4], fo_matrix: [f64;6], rect: [f64;4], tmatrix: [f64;6], allow_shrink: bool, allow_expand: bool) -> Option<[f64;6]>` mirroring `getMatrixForFormXObjectPlacement`: `w = concat(concat(I, tmatrix), fo_matrix)`; `T = transform_rectangle(w, fo_bbox)`; degenerate ⇒ `None`; `scale = min(rect_w/T_w, rect_h/T_h)` clamped by allow_expand/allow_shrink; `w2 = concat(concat(scale(I,scale), tmatrix), fo_matrix)`; `T2`; `tx/ty` to centre; `cm = concat(scale(translate(I,tx,ty), scale), tmatrix)`; return `Some(cm)`.
- `fn place_form_xobject(fo_bbox, fo_matrix, rect, tmatrix, allow_shrink, allow_expand, name) -> String`: compute cm (None ⇒ empty string, matching qpdf's empty-cm-skips path — but for our fixtures cm is always Some); return `format!("q\n{} cm\n/{} Do\nQ\n", matrix_unparse(cm), name)`.
- Helper `fn fo_bbox_and_matrix<R>(pdf, xref) -> Result<([f64;4], [f64;6])>` reading the fo's `/BBox` (resolve indirect, ≥4 elems) and `/Matrix` (via `matrix_or_identity`).
- In `apply_overlays_to_page`: after reading `media_box`/`trim_box`, compute `let dest_t = read_page_transform(dest, dest_page_ref)?;` and `let tmatrix = transformation_matrix(&dest_t, trim_w, trim_h, true);` (trim dims from `trim_box`). For each underlay/overlay source: `(bb, fm) = fo_bbox_and_matrix(dest, xref)?; content.push_str(&place_form_xobject(bb, fm, trim_rect, tmatrix, true, false, name));`. For `/Fx0`: same but `media_rect`, `allow_shrink=false`. Remove `xobject_placement_box`.

**Step 4 — Run, expect PASS.** Same filter.

**Step 5 — REGRESSION NET (critical).** `cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate` — all 10 existing goldens MUST stay green. If any moves, stop and diff.

**Step 6 — Commit.** `git add -A && git commit -m "fix(overlay): faithful getMatrixForFormXObjectPlacement with dest inverse-transform (flpdf-9hc.16.10 C)"`

---

## Task D: Widen byte-gate fixtures + goldens + tests + ci.yml

**Files:**
- Modify: `tests/golden/regenerate.sh` (new fixtures + golden recipes)
- Create: `tests/fixtures/compat/userunit-one-page.pdf` (hand-crafted), rotated-dest fixture
- Create: `tests/golden/references/overlay/<new goldens>.pdf`
- Modify: `crates/flpdf/src/overlay.rs` `mod byte_gate` (new tests + matrix table comment)
- Modify: `.github/workflows/ci.yml` (add new gated test names — memory `flpdf-ci-bytes-identical-explicit-test-list`)

**Step 1 — Fixtures.** (a) `#1+#2`: reuse `multi-contents-one-page.pdf` (no `/Rotate`, `/Contents` array) as overlay source onto `three-page`. (b) `#3`: `qpdf --rotate=+90:1 three-page.pdf rotated-three-page.pdf` (rotated dest) or overlay onto `one-page-r90`. (c) `#4`: hand-craft `userunit-one-page.pdf` (minimal PDF with `/UserUnit 2`) — qpdf CLI has no `/UserUnit` flag; wire its generation into `regenerate.sh`.

**Step 2 — Goldens.** Add `qpdf 11.9.0 --static-id` overlay recipes for each new case to `regenerate.sh`; run it; commit the produced golden PDFs.

**Step 3 — byte_gate tests.** Add one `#[test]` per new case using `assert_byte_identical`, plus update the matrix-table comment. `cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate` ⇒ all PASS (old + new).

**Step 4 — ci.yml.** Add the new test function names to the explicit gated-byte-test list.

**Step 5 — Commit.** `git add -A && git commit -m "test(overlay): widen byte-gate to Matrix-omission/multi-stream/rotated-dest/UserUnit (flpdf-9hc.16.10 D)"`

---

## Completion gates
- `cargo test -p flpdf --lib` (default, Pure-Rust) green.
- `cargo test -p flpdf --features qpdf-zlib-compat --lib` green (old + new goldens).
- `cargo fmt --check`, `cargo clippy`.
- `scripts/patch-coverage.sh` ⇒ 100% on changed `flpdf` lines (unit tests cover non-golden arms: non-integer `/Rotate`, non-numeric `/UserUnit`, empty-stream newline, `scale==0` invert guard).
