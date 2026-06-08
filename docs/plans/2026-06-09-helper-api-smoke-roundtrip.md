# Helper API smoke + round-trip Tests Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a capstone integration test `crates/flpdf/tests/helper_api_tests.rs` that exercises all five document helpers as a public API surface and proves helper-vs-raw round-trip parity byte-for-byte.

**Architecture:** Two layers. (1) Smoke — each helper's read API is cross-checked against an independent manual raw-`Object` extraction on the same in-memory PDF. (2) Round-trip — each mutating helper is applied to one `Pdf`, the same semantic edit is applied to a second `Pdf` via raw `Object` manipulation, and both are serialised with `WriteOptions { full_rewrite: true, static_id: true }` and asserted byte-equal. `full_rewrite` makes the writer renumber Catalog-first (qpdf order) and drop unreachable objects, so divergent internal numbering never causes spurious mismatch.

**Tech Stack:** Rust, `flpdf` crate, `std::io::Cursor`, in-memory PDF byte builders (same style as `crates/flpdf/tests/page_document_helper_tests.rs`).

**Design source:** beads `flpdf-9hc.18.10` `design` field.

## Verified facts (de-risk before execution — already checked in source)

- **`Dictionary` is backed by `BTreeMap`** (`object.rs`): iteration/serialisation
  is lexicographic key order, NOT insertion order. → Manual-path dicts do **not**
  need to match the helper's `insert` call order; only the same keys+values.
- **`full_rewrite` renumber converges** (`rewrite_renumber.rs`): new object
  numbers are assigned in Catalog-seeded BFS discovery order, where each object's
  children are visited in lexicographic dict-key order / array order. Old object
  numbers are never consulted. → Two isomorphic graphs with DIFFERENT internal
  numbers serialise byte-identically. This is the keystone the round-trip layer
  rests on; **Task 1 verifies it empirically before any builder work.**
- **Doc acceptance ("documented as recommended public API") is already satisfied
  by dependency 18.9** (commit 55bcca2: cookbook `examples/*.rs` +
  `lib.rs` cross-references). This task is **tests-only**; add no doc work.

---

## Conventions for every task

- This is **test code**: the deliverable IS the test. "Failing first" is inverted —
  write the test, run it; because the helpers already exist it should **PASS**.
  A FAIL means either a real parity bug (investigate which path is correct,
  do NOT paper over it) or a builder/API mistake.
- All PDFs are built in-memory; never touch the filesystem.
- No beads issue IDs in `///`/`//!` comments (doc-review rule); comments English.
- Run `cargo fmt` before every commit (`cargo fmt --check` is a CI gate).
- Reference templates to copy idioms from:
  - `crates/flpdf/tests/page_document_helper_tests.rs` (builder + `PageRange`/`RotateMode` use)
  - `crates/flpdf/tests/acroform_document_helper_tests.rs`
  - `crates/flpdf/tests/outline_document_helper_tests.rs`
  - `crates/flpdf/tests/embedded_files_tests.rs` / `filespec_helper_tests.rs`
  - NOTE: `PageLabelDocumentHelper` has **no** existing integration test — this
    file is its first; read `crates/flpdf/src/page_label_document_helper.rs`
    for `LabelRange` / `LabelStyle` constructors.

## Key API reference (verified in source)

- Open: `Pdf::open(Cursor::new(bytes))`; mutate: `pdf.set_object(ObjectRef, Object)`,
  read: `pdf.resolve(ObjectRef) -> Result<Object>`, `pdf.root_ref()`, `pdf.trailer()`.
- `ObjectRef::new(number: u32, generation: u16)`; `Object::Reference(ObjectRef)`.
- `Dictionary`: `.get(k)`, `.get_ref(k)`, `.insert(k, Object)`, `.remove(k)`.
- `Object`: `as_dict()/as_dict_mut()/as_array()/as_name()/as_integer()/as_ref_id()` etc.
- Serialise canonical bytes:
  ```rust
  use flpdf::{write_pdf_with_options, WriteOptions};
  fn write_canonical<R: std::io::Read + std::io::Seek>(pdf: &mut flpdf::Pdf<R>) -> Vec<u8> {
      let opts = WriteOptions { full_rewrite: true, static_id: true, ..Default::default() };
      let mut buf = Vec::new();
      write_pdf_with_options(pdf, &mut buf, &opts).expect("write_canonical");
      buf
  }
  ```
- Helpers: `pdf.acroform()`, `pdf.outline()`, `pdf.page_labels()`,
  `PageDocumentHelper::new(&mut pdf)` (or `flpdf::PageDocumentHelper`);
  attachments via free fns `flpdf::insert_embedded_file/delete_embedded_file/
  list_embedded_files` and `flpdf::list_attachment_info`
  (confirm exact export names at use site via `crates/flpdf/src/lib.rs`).

---

### Task 1: Scaffold + canonical-write harness + KEYSTONE convergence test

**Files:**
- Create: `crates/flpdf/tests/helper_api_tests.rs`

> **Why a keystone test first:** the entire round-trip layer assumes that
> `full_rewrite` renumbering makes two isomorphic graphs with *different internal
> object numbers* serialise byte-identically. Source analysis says yes, but prove
> it empirically NOW — before building five fixtures. If this fails, STOP and
> reconsider (fall back to semantic comparison); do not proceed to Tasks 2–4.

**Step 1: Write the module header, imports, the `write_canonical` helper, and a
flat-N-page builder** (copy `build_n_page_pdf` from `page_document_helper_tests.rs`
verbatim — including its xref/trailer emission). Add a `//!` module doc:
```rust
//! Capstone integration tests for the flpdf document-helper public API.
//!
//! Layer 1 (smoke): each helper read API is cross-checked against an
//! independent manual raw-`Object` extraction. Layer 2 (round-trip): each
//! mutating helper produces byte-identical output to the equivalent direct
//! `Object` manipulation, serialised with `full_rewrite + static_id`.
```

**Step 2: Add one trivial smoke test to prove the scaffold compiles & links:**
```rust
#[test]
fn page_helper_pages_matches_manual_kids() {
    let bytes = build_n_page_pdf(3);
    let mut pdf = flpdf::Pdf::open(std::io::Cursor::new(bytes)).unwrap();

    // Helper path.
    let mut helper = flpdf::PageDocumentHelper::new(&mut pdf);
    let helper_pages = helper.pages().unwrap();

    // Manual path: resolve /Root -> /Pages -> /Kids refs.
    let root = pdf.root_ref().unwrap();
    let cat = pdf.resolve(root).unwrap();
    let pages_ref = cat.as_dict().unwrap().get_ref("Pages").unwrap();
    let pages = pdf.resolve(pages_ref).unwrap();
    let manual: Vec<_> = pages.as_dict().unwrap().get("Kids").unwrap()
        .as_array().unwrap().iter()
        .map(|o| o.as_ref_id().unwrap()).collect();

    assert_eq!(helper_pages, manual);
}
```
(Adjust `PageDocumentHelper` construction / borrow order to satisfy the borrow
checker — the helper borrows `&mut pdf`; drop it before the manual path, e.g.
scope the helper in a block.)

**Step 3 (KEYSTONE): differing-object-number convergence test.** Prove the
round-trip premise with the cheapest possible fixture — no helper involved, pure
raw manipulation building the *same* page graph at *different* object numbers:
```rust
use flpdf::{write_pdf_with_options, Object, ObjectRef, Pdf, WriteOptions};

fn insert_page_at(pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>, new_num: u32) {
    // Create a detached page object at object number `new_num`.
    let page_ref = ObjectRef::new(new_num, 0);
    let pages_ref = {
        let root = pdf.root_ref().unwrap();
        let cat = pdf.resolve(root).unwrap();
        cat.as_dict().unwrap().get_ref("Pages").unwrap()
    };
    let mut page = flpdf::Dictionary::new();
    page.insert("Type", Object::Name(b"Page".to_vec()));
    page.insert("Parent", Object::Reference(pages_ref));
    page.insert("MediaBox", Object::Array(vec![
        Object::Integer(0), Object::Integer(0),
        Object::Integer(612), Object::Integer(792)]));
    pdf.set_object(page_ref, Object::Dictionary(page));
    // Splice into /Kids at index 1 and bump /Count.
    let mut pages = pdf.resolve(pages_ref).unwrap().as_dict().unwrap().clone();
    let kids = pages.get("Kids").unwrap().as_array().unwrap().to_vec();
    let mut new_kids = kids.clone();
    new_kids.insert(1, Object::Reference(page_ref));
    pages.insert("Kids", Object::Array(new_kids));
    pages.insert("Count", Object::Integer(kids.len() as i64 + 1));
    pdf.set_object(pages_ref, Object::Dictionary(pages));
}

#[test]
fn full_rewrite_converges_across_object_numbers() {
    let mut a = Pdf::open(std::io::Cursor::new(build_n_page_pdf(2))).unwrap();
    let mut b = Pdf::open(std::io::Cursor::new(build_n_page_pdf(2))).unwrap();
    insert_page_at(&mut a, 50);   // same graph, different internal number
    insert_page_at(&mut b, 80);
    assert_eq!(write_canonical(&mut a), write_canonical(&mut b),
        "full_rewrite renumber must converge regardless of internal object number");
}
```
Run `cargo test -p flpdf --test helper_api_tests full_rewrite_converges -- --nocapture`.
Expected: PASS. **If it FAILS, stop and report — the round-trip approach needs
rethinking (semantic comparison fallback).**

**Step 4:** Run the full file: `cargo test -p flpdf --test helper_api_tests`.
Expected: 2 passed.

**Step 5:** `cargo fmt` then commit.
```bash
git add crates/flpdf/tests/helper_api_tests.rs docs/plans/2026-06-09-helper-api-smoke-roundtrip.md
git commit -m "test(flpdf): scaffold capstone + page smoke + renumber-convergence keystone [flpdf-9hc.18.10]"
```

---

### Task 2: Smoke — AcroForm, Outline, PageLabel, Attachment read APIs

**Files:**
- Modify: `crates/flpdf/tests/helper_api_tests.rs`

For each helper add a dedicated in-memory builder fn + a smoke test that compares
the helper read API to a manual raw extraction.

**Step 1: AcroForm builder + test.** Build a PDF whose Catalog has
`/AcroForm << /Fields [F1 F2] >>`, two text fields:
- F1: `<< /FT /Tx /T (name) /V (Alice) /DA (/Helv 0 Tf 0 g) >>`
- F2: `/V` stored as an **indirect reference** to a separate string object
  (guards the resolve path). e.g. F2 `<< /FT /Tx /T (city) /V 99 0 R >>`,
  object `99 0 obj (Paris) endobj`.
Test: `pdf.acroform().field_infos()` — assert `full_name`s, `field_type` =
`Some(b"Tx")`, and that F2's `value` resolved to `Object::String(b"Paris")`
(NOT a `Reference`). Also assert `field_value(f2_ref)` returns the resolved
string.

**Step 2: Outline builder + test.** Build `/Outlines` with 2 top-level items,
the first having 2 children (`/First /Last /Next /Prev /Parent /Count`, titles
"A","A.1","A.2","B"). Test: collect `pdf.outline().walk(|n, d| ...)` into
`Vec<(String, usize)>` (title, depth); assert
`[("A",0),("A.1",1),("A.2",1),("B",0)]`. Assert `has_outlines()` is true.

**Step 3: PageLabel builder + test.** On a 5-page PDF add Catalog
`/PageLabels << /Nums [0 << /S /r >> 3 << /S /D /P (A-) >>] >>`. Test:
`pdf.page_labels().label_string_for_page(i)` for i=0..5 equals
`["i","ii","iii","A-1","A-2"]`. Assert `ranges().len() == 2`.

**Step 4: Attachment builder + test.** Build `/Names /EmbeddedFiles` name tree
with one entry key `(hello.txt)` -> Filespec -> EmbeddedFile stream with known
bytes and `/Params << /Size N >>`. Test: `list_attachment_info(&mut pdf)` (or
the crate's exact export) returns 1 entry with `key == b"hello.txt"` and
`size == Some(N)`; cross-check `list_embedded_files` key set.

**Step 5:** Run `cargo test -p flpdf --test helper_api_tests`. Expected: all pass.
If a builder's structure is rejected, fix the builder (consult the matching
per-helper test file for a known-good fixture shape).

**Step 6:** `cargo fmt` then commit.
```bash
git add crates/flpdf/tests/helper_api_tests.rs
git commit -m "test(flpdf): smoke tests for acroform/outline/label/attachment helpers [flpdf-9hc.18.10]"
```

---

### Task 3: Round-trip harness + page mutation parity

**Files:**
- Modify: `crates/flpdf/tests/helper_api_tests.rs`

**Step 1: Add the round-trip harness:**
```rust
fn roundtrip_eq(
    build: impl Fn() -> Vec<u8>,
    via_helper: impl FnOnce(&mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>),
    via_manual: impl FnOnce(&mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>),
) {
    let mut a = flpdf::Pdf::open(std::io::Cursor::new(build())).unwrap();
    let mut b = flpdf::Pdf::open(std::io::Cursor::new(build())).unwrap();
    via_helper(&mut a);
    via_manual(&mut b);
    assert_eq!(write_canonical(&mut a), write_canonical(&mut b),
        "helper path and manual path produced different canonical bytes");
}
```

**Step 2: page `remove` parity.** `build = || build_n_page_pdf(3)`.
- helper: `PageDocumentHelper::new(pdf).remove(1).unwrap();`
- manual: resolve `/Pages`, drop `/Kids[1]`, set `/Count` to 2, `set_object`.
Call `roundtrip_eq`. Run the test; expect pass.

**Step 3: page `rotate` parity.** helper: `.rotate(&range, 90, RotateMode::Set)`
for page 0 (build `PageRange` per the page-helper test's idiom). manual: insert
`/Rotate 90` into page 0's dict and `set_object`. `roundtrip_eq`.

**Step 4: page `insert` parity** (proves full_rewrite renumber robustness).
- helper: create a detached page object at a free ref (e.g. `ObjectRef::new(50,0)`)
  via `set_object`, then `PageDocumentHelper::new(pdf).insert(1, that_ref)`.
- manual: independently create the same page dict (at a DIFFERENT free number,
  e.g. `ObjectRef::new(80,0)`), splice its ref into `/Kids` at index 1, bump
  `/Count`. The differing internal numbers MUST still yield byte-identical
  output thanks to Catalog-first renumber — this is the key assertion.
`roundtrip_eq`.

**Step 5:** Run `cargo test -p flpdf --test helper_api_tests`. Expect all pass.
If Step 4 mismatches, inspect both canonical outputs (write to /tmp and diff) to
confirm whether it is a genuine non-isomorphism vs a builder bug; report before
weakening the assertion.

**Step 6:** `cargo fmt` then commit.
```bash
git add crates/flpdf/tests/helper_api_tests.rs
git commit -m "test(flpdf): page helper round-trip byte parity [flpdf-9hc.18.10]"
```

---

### Task 4: Round-trip parity for form / label / attachment mutations

**Files:**
- Modify: `crates/flpdf/tests/helper_api_tests.rs`

Reuse the builders from Task 2 and the `roundtrip_eq` harness.

**Step 1: AcroForm `set_field_value` parity.**
- helper: `pdf.acroform().set_field_value(f1_ref, Object::String(b"Bob".to_vec()))`.
- manual: resolve f1 dict, `insert("V", Object::String(b"Bob".to_vec()))`, `set_object`.
`roundtrip_eq`.

**Step 2: AcroForm `set_default_appearance` parity.**
- helper: `pdf.acroform().set_default_appearance(b"/Helv 12 Tf 0 g".to_vec())`.
- manual: resolve `/AcroForm` dict, `insert("DA", Object::String(...))`, `set_object`.
`roundtrip_eq`.

**Step 3: PageLabel `set_range` + `remove_range` parity.**
- set_range: helper `pdf.page_labels().set_range(0, LabelRange…)`; manual edits
  `/PageLabels /Nums` array directly (build the same `/S`,`/P`,`/St` dict).
- remove_range: helper `remove_range(3)`; manual deletes the `3 << >>` pair.
Two `roundtrip_eq` calls.

**Step 4: Attachment `insert_embedded_file` + `delete_embedded_file`.**

> **Byte-identity is the goal, but the name tree is the highest-risk case.** The
> helper builds/prunes a `/Names /EmbeddedFiles` tree (sorting, possibly
> intermediate nodes / `/Limits`). A truly independent manual path is hard, and
> mirroring the helper's exact tree output makes the test circular. Decision
> rule: ATTEMPT byte-identity via `roundtrip_eq` where the manual path builds a
> single-level `/Names [(key) <filespec>]` tree (the simple shape for a 1-entry
> tree). **If it converges, keep byte-identity.** If it does NOT converge
> without reimplementing the helper's tree logic, FALL BACK to **semantic
> round-trip** for that case and add a `//` comment naming why (name-tree
> construction is helper-internal, byte-identity would be circular):
> ```rust
> // semantic round-trip: reopen via helper read API instead of byte-identity,
> // because an independent name-tree manual path would just reimplement the helper.
> let infos = flpdf::list_attachment_info(&mut pdf).unwrap();
> assert!(infos.iter().any(|i| i.key == b"new.txt"));   // insert
> // delete: assert the key is gone and (per delete docs) empty /Names pruned.
> ```
- insert: start from a no-attachment PDF; helper `insert_embedded_file(pdf,
  b"new.txt", filespec_ref)` (filespec object created via `set_object` first).
  Apply the decision rule above.
- delete: start from the 1-attachment builder; helper `delete_embedded_file(pdf,
  b"hello.txt")`. For the manual/byte-identity attempt, read the
  `delete_embedded_file` pruning contract in `embedded_files.rs`. Apply the
  decision rule; prefer semantic round-trip if byte convergence is circular.

**Step 5:** Run `cargo test -p flpdf --test helper_api_tests`. Expect all pass.
For any mismatch, diff canonical outputs and decide correctness before adjusting.

**Step 6:** `cargo fmt` then commit.
```bash
git add crates/flpdf/tests/helper_api_tests.rs
git commit -m "test(flpdf): form/label/attachment helper round-trip parity [flpdf-9hc.18.10]"
```

---

### Task 5: Full verification + quality gates

**Files:** none (verification only)

**Step 1:** `cargo fmt --check` — expect clean.
**Step 2:** `cargo test -p flpdf` — full crate suite, expect all pass (no
regression in existing per-helper tests).
**Step 3:** `cargo clippy -p flpdf --tests` — expect no new warnings on the new file.
**Step 4:** If all green, the work is ready for branch finishing
(`superpowers:finishing-a-development-branch`).

---

## Done criteria (maps to beads acceptance)

- `helper_api_tests.rs` exercises all 5 helpers (page/form/outline/label/attachment).
- Smoke layer cross-checks each read API against manual raw extraction
  (incl. an indirect-reference field value).
- Round-trip layer asserts byte-identical output (`full_rewrite + static_id`)
  for every mutating helper.
- `cargo fmt --check` and `cargo test -p flpdf` pass.
