# flpdf-sjgv: in_open_document Precedence in from_plan + AcroForm Widget Fixture

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix `LinearizationPlan::from_pdf` to model qpdf's `in_open_document > in_first_page` precedence, so AcroForm widgets reachable from both `/AcroForm /Fields` (open-document) and page 0 `/Annots` (first-page) are excluded from part2/part3 and left in Part 4 for ObjStm container routing.

**Architecture:** Call `open_document_set(pdf)?` at the top of `from_pdf`; skip open-document objects during Step 5's first-page closure partition loop. This automatically propagates to `page0_private`, `page_hints[0].object_count`, `shared_hints`, and `canonical_shared_hints` without touching any hint builder code. Add a fixture (W=5 widgets, S=10 shared fonts) with a structural test and a byte-identical compat test.

**Tech Stack:** Rust (flpdf crate), Python (fixture generator), qpdf 11.9.0 (oracle)

---

### Task 1: Create the fixture generator

**Files:**
- Create: `docs/plans/tools/gen_acroform_widget_page0.py`

**Step 1: Write the generator**

```python
import sys

# >cap fixture exercising qpdf in_open_document PRECEDENCE over in_first_page.
#
# An AcroForm with W widget annotations appears in BOTH:
#   - /AcroForm /Fields (catalog key => closure_from_seeds => in_open_document)
#   - page 0 /Annots (first-page closure => in_first_page)
# qpdf precedence: in_open_document > in_first_page => widget goes to
# lc_open_document => part4 (FIRST half, before /O).
# flpdf-sjgv: without the fix, from_pdf Step 5 places widgets in part2
# (first-page exclusives), inflating page_hints[0].object_count and diverging
# hint tables vs qpdf's output.
#
# S shared fonts: page 0 and page 1 share them => Part 3 (first-half shared).
# DFS order (BTreeMap key order for Catalog: /AcroForm before /Pages before /Type):
#   Catalog, AcroForm, Widgets(W), Pages, Page0, SharedFonts(S), Contents0, Page1, Contents1

W = int(sys.argv[1]) if len(sys.argv) > 1 else 5
S = int(sys.argv[2]) if len(sys.argv) > 2 else 10

catalog, pages, page0, page1, acroform = 1, 2, 3, 4, 5
w0 = 6
w_nums = list(range(w0, w0 + W))
s0 = w0 + W
s_nums = list(range(s0, s0 + S))
c0 = s0 + S
c1 = c0 + 1

objs = {}
objs[catalog] = b"<< /Type /Catalog /AcroForm %d 0 R /Pages %d 0 R >>" % (acroform, pages)
objs[pages] = b"<< /Type /Pages /Count 2 /Kids [ %d 0 R %d 0 R ] >>" % (page0, page1)

fields = b" ".join(b"%d 0 R" % n for n in w_nums)
objs[acroform] = b"<< /Fields [ %s ] >>" % fields

for i, n in enumerate(w_nums):
    objs[n] = b"<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /FT /Tx /T (F%d) /V () >>" % (i + 1)

shared_res = b" ".join(b"/SF%d %d 0 R" % (i + 1, n) for i, n in enumerate(s_nums))
res = b"<< /Font << %s >> >>" % shared_res
for i, n in enumerate(s_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /SF%d >>" % (i + 1)

annots = b" ".join(b"%d 0 R" % n for n in w_nums)
objs[page0] = (b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792]"
               b" /Resources %s /Annots [ %s ] /Contents %d 0 R >>"
               % (pages, res, annots, c0))
objs[page1] = (b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792]"
               b" /Resources %s /Contents %d 0 R >>"
               % (pages, res, c1))

for cnum, label in ((c0, b"Page0"), (c1, b"Page1")):
    stream = b"BT /SF1 12 Tf 72 720 Td (%s) Tj ET" % label
    objs[cnum] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(stream), stream)

out = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
offsets = {}
for num in sorted(objs):
    offsets[num] = len(out)
    out += b"%d 0 obj\n" % num + objs[num] + b"\nendobj\n"
xref_start = len(out)
total = max(objs) + 1
out += b"xref\n0 %d\n0000000000 65535 f \n" % total
for num in range(1, total):
    out += b"%010d 00000 n \n" % offsets[num]
out += b"trailer\n<< /Size %d /Root %d 0 R >>\n" % (total, catalog)
out += b"startxref\n%d\n%%%%EOF\n" % xref_start
sys.stdout.buffer.write(out)
```

**Step 2: Smoke-check the generator**

```bash
python3 docs/plans/tools/gen_acroform_widget_page0.py 5 10 | qpdf --check - --warning-exit-0
```

Expected: `No syntax or stream encoding errors found.`

**Step 3: Commit**

```bash
git add docs/plans/tools/gen_acroform_widget_page0.py
git commit -m "test(fixture): add gen_acroform_widget_page0.py for in_open_document precedence fixture (flpdf-sjgv)"
```

---

### Task 2: Generate the compat fixture PDF and golden

**Files:**
- Create: `tests/fixtures/compat/objstm-lin-acroform-widget-page0-5-10.pdf`
- Create: `tests/golden/references/objstm-lin-acroform-widget-page0-5-10/linearize-objstm.pdf`
- Modify: `tests/golden/regenerate.sh`

**Step 1: Generate the compat fixture**

```bash
python3 docs/plans/tools/gen_acroform_widget_page0.py 5 10 \
  > tests/fixtures/compat/objstm-lin-acroform-widget-page0-5-10.pdf
qpdf --check tests/fixtures/compat/objstm-lin-acroform-widget-page0-5-10.pdf --warning-exit-0
```

**Step 2: Generate the golden (qpdf oracle)**

```bash
mkdir -p tests/golden/references/objstm-lin-acroform-widget-page0-5-10
qpdf --linearize --object-streams=generate --deterministic-id --warning-exit-0 \
  tests/fixtures/compat/objstm-lin-acroform-widget-page0-5-10.pdf \
  tests/golden/references/objstm-lin-acroform-widget-page0-5-10/linearize-objstm.pdf
qpdf --check tests/golden/references/objstm-lin-acroform-widget-page0-5-10/linearize-objstm.pdf
```

Expected: `No syntax or stream encoding errors found.`

**Step 3: Add to regenerate.sh**

Add to the `G6HB2_FIX` array (after `openaction-80-80` entry):

```bash
    [objstm-lin-acroform-widget-page0-5-10]="gen_acroform_widget_page0.py 5 10"
```

Add to the golden generation loop (after `objstm-lin-openaction-80-80`):

```
objstm-lin-acroform-widget-page0-5-10 \
```

Also add `"$REF/objstm-lin-acroform-widget-page0-5-10" \` to the `mkdir -p` block
(search for `"$REF/objstm-lin-outlines-80-80"` and add the new entry adjacent to it).

**Step 4: Commit**

```bash
git add tests/fixtures/compat/objstm-lin-acroform-widget-page0-5-10.pdf \
        tests/golden/references/objstm-lin-acroform-widget-page0-5-10/linearize-objstm.pdf \
        tests/golden/regenerate.sh
git commit -m "test(fixture): add objstm-lin-acroform-widget-page0-5-10 compat fixture and qpdf golden (flpdf-sjgv)"
```

---

### Task 3: Write the failing structural test

**Files:**
- Modify: `crates/flpdf/tests/linearize_objstm_generate_tests.rs`

**Step 1: Add the test at the end of the file**

```rust
/// A fixture whose AcroForm widgets appear in BOTH /AcroForm /Fields (open-document)
/// AND page 0 /Annots (first-page closure). qpdf's in_open_document > in_first_page
/// precedence means widgets must NOT be in part2/part3 (first-page section) — they
/// should be left in Part 4 so route_objstm_containers puts their ObjStm container
/// in the OpenDocument slot (first half, before /O). Pins the from_pdf Step 5 fix
/// (flpdf-sjgv): before the fix, widgets land in part2 and inflate
/// page_hints[0].object_count beyond what qpdf computes.
///
/// Fixture layout (W=5, S=10):
///   obj 1: Catalog (/AcroForm 5, /Pages 2)
///   obj 2: Pages
///   obj 3: Page0 (/Annots [6..10], /Resources inline /Font -> 11..20, /Contents 21)
///   obj 4: Page1 (/Resources inline /Font -> 11..20, /Contents 22)
///   obj 5: AcroForm (/Fields [6..10])
///   obj 6-10: Widget annotations (in both /AcroForm /Fields AND page0 /Annots)
///   obj 11-20: Shared fonts (page0 and page1 both reference them → Part 3)
///   obj 21: Content stream for page0
///   obj 22: Content stream for page1
#[test]
fn acroform_widget_page0_peeled_from_first_page_section() {
    use std::collections::BTreeSet;
    use flpdf::ObjectRef;

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join("objstm-lin-acroform-widget-page0-5-10.pdf");

    let f = std::fs::File::open(&path)
        .unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(f)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

    // Widgets: objects 6..=10 (W=5). They are in open_document_set (via /AcroForm
    // /Fields) AND first_page_closure (via page0 /Annots). qpdf's in_open_document
    // precedence means they must be ABSENT from part2 and part3.
    let widget_refs: Vec<ObjectRef> = (6u32..=10)
        .map(|n| ObjectRef { number: n, generation: 0 })
        .collect();

    let part2_set: BTreeSet<_> = plan.part2_objects.iter().copied().collect();
    let part3_set: BTreeSet<_> = plan.part3_objects.iter().copied().collect();

    for r in &widget_refs {
        assert!(
            !part2_set.contains(r),
            "widget {r} must not be in part2 (in_open_document > in_first_page)"
        );
        assert!(
            !part3_set.contains(r),
            "widget {r} must not be in part3 (in_open_document > in_first_page)"
        );
    }

    // Widgets must end up in Part 4 (awaiting OpenDocument container routing).
    let part4: BTreeSet<_> = plan.part4_objects().into_iter().collect();
    for r in &widget_refs {
        assert!(
            part4.contains(r),
            "widget {r} must be in part4 (left for route_objstm_containers OpenDocument routing)"
        );
    }

    // page_hints[0].object_count must reflect only the true first-page section:
    //   part2 = {page0(3), c0(21)} = 2 objects
    //   part3 = {shared fonts 11..=20} = 10 objects
    //   total = 12
    // Without the fix widgets(5) are in part2, giving 7+10 = 17.
    assert_eq!(
        plan.page_hints[0].object_count, 12,
        "page 0 object_count must be 12 (page + content + 10 shared fonts, widgets peeled)"
    );
}
```

**Step 2: Run the test to confirm it fails**

```bash
cargo test -p flpdf --test linearize_objstm_generate_tests \
  acroform_widget_page0_peeled_from_first_page_section -- --nocapture 2>&1 | tail -20
```

Expected: FAIL — widgets found in part2, or object_count assertion 17 != 12.

---

### Task 4: Fix from_pdf in plan.rs

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs`

**Step 1: Add open_document_set call at the top of from_pdf (after the `all_refs` build, before Step 2)**

After line ~521 (after `all_refs` is built and filtered), insert:

```rust
        // ----------------------------------------------------------------
        // Step 1b: compute open-document set for qpdf precedence.
        // ----------------------------------------------------------------
        // qpdf's in_open_document category takes precedence over in_first_page:
        // objects reachable from catalog open-document keys (/OpenAction,
        // /AcroForm, /ViewerPreferences, /PageMode, /Threads, /Encrypt) are
        // placed in the open-document section (part4, first half, before /O),
        // even if they are also in the first-page closure. Computing this set
        // here ensures Step 5 can exclude them from part2/part3 without
        // requiring the hint builders or container router to compensate.
        let open_document_set = open_document_set(pdf)?;
```

**Step 2: Skip open-document objects in Step 5**

Find the loop at ~line 610:

```rust
        for obj_ref in &first_page_closure {
            if Some(*obj_ref) == first_page_ref {
                part2_objects.push(*obj_ref);
            } else if shared_page_indices.contains_key(obj_ref) {
                part3_objects.push(*obj_ref);
            } else {
                part2_objects.push(*obj_ref);
            }
        }
```

Replace with:

```rust
        for obj_ref in &first_page_closure {
            // qpdf: in_open_document > in_first_page. Objects reachable from
            // catalog open-document keys (/AcroForm, /OpenAction, etc.) are
            // placed in the open-document section (part4, first half) even if
            // they also appear in the first-page closure. Leave them in Part 4
            // so route_objstm_containers can assign their ObjStm container to
            // ContainerPart::OpenDocument.
            if open_document_set.contains(obj_ref) {
                continue;
            }
            if Some(*obj_ref) == first_page_ref {
                part2_objects.push(*obj_ref);
            } else if shared_page_indices.contains_key(obj_ref) {
                part3_objects.push(*obj_ref);
            } else {
                part2_objects.push(*obj_ref);
            }
        }
```

**Step 3: No other changes needed**

`page0_private`, `page_hints[0].object_count`, `shared_hints`, and `canonical_shared_hints` all derive from `part2_objects`/`part3_objects`, so they automatically produce correct values after the peeling.

---

### Task 5: Run the structural test to confirm it passes

**Step 1:**

```bash
cargo test -p flpdf --test linearize_objstm_generate_tests \
  acroform_widget_page0_peeled_from_first_page_section -- --nocapture 2>&1 | tail -10
```

Expected: `test acroform_widget_page0_peeled_from_first_page_section ... ok`

**Step 2: Run full test suite to confirm no regressions**

```bash
cargo test --workspace --quiet 2>&1 | tail -15
```

Expected: all tests pass (same counts as before), 0 failures.

---

### Task 6: Write the byte-identical compat test

**Files:**
- Modify: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

**Step 1: Add two test functions after the openaction tests (around line 271)**

```rust
// acroform-widget-page0-5-10 (flpdf-sjgv): AcroForm widgets in both
// /AcroForm /Fields (in_open_document) and page 0 /Annots (in_first_page).
// qpdf's in_open_document > in_first_page precedence means widgets go to the
// open-document section (part4, first half, before /O). Without the fix,
// from_pdf Step 5 places them in part2, inflating page_hints[0].object_count
// and diverging hint tables. Exercises the from_pdf open_document_set peeling.
#[test]
fn acroform_widget_page0_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-acroform-widget-page0-5-10.pdf",
        "objstm-lin-acroform-widget-page0-5-10",
    );
}

#[test]
fn acroform_widget_page0_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-acroform-widget-page0-5-10.pdf",
        "objstm-lin-acroform-widget-page0-5-10",
    );
}
```

**Step 2: Run the compat tests (requires qpdf-zlib-compat feature)**

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests \
  acroform_widget_page0 -- --nocapture 2>&1 | tail -15
```

Expected: both tests pass — `ok`.

If `assert_strict` fails (ID[1] mismatch is ignored for now — mark `#[ignore]` like the pattern in the file):
Check existing `assert_strict` tests to see if they have `#[ignore]` annotations. If they do, add `#[ignore]` to `acroform_widget_page0_objstm_byte_identical_to_qpdf` too and note why in the comment.

---

### Task 7: Run patch coverage and full suite

**Step 1: Commit all code changes first**

```bash
git add crates/flpdf/src/linearization/plan.rs \
        crates/flpdf/tests/linearize_objstm_generate_tests.rs \
        crates/flpdf/tests/cmp_linearize_objstm_tests.rs
git commit -m "fix(linearize): peel open-document objects from first-page closure in from_pdf (flpdf-sjgv)

qpdf's in_open_document > in_first_page precedence: objects reachable from
catalog open-document keys (/AcroForm, /OpenAction, etc.) must be excluded
from part2/part3 even when they appear in the first-page closure, so their
ObjStm containers route to ContainerPart::OpenDocument (first half, before /O).

Fixes AcroForm widget annotations shared between /AcroForm /Fields and page 0
/Annots: without this change, such widgets land in part2, inflating
page_hints[0].object_count and causing hint table divergence vs qpdf 11.9.0."
```

**Step 2: Run patch coverage**

```bash
scripts/patch-coverage.sh --base main 2>&1 | tail -20
```

Expected: 100% coverage on changed lines in `crates/flpdf/`. If any lines are uncovered, add targeted tests or `// cov:ignore: <reason>`.

**Step 3: Run full workspace test suite**

```bash
cargo test --workspace --quiet 2>&1 | tail -15
```

Expected: all tests pass, 0 failures.

**Step 4: Run cargo fmt check**

```bash
cargo fmt --check 2>&1
```

Expected: no output (clean).

---

## Notes

- **open_document_set() call cost**: It's a BFS over the catalog's indirect refs. Called once per linearization. Already called inside `route_objstm_containers`; calling it again in `from_pdf` is safe (idempotent read, no write side effects on `pdf`). If profiling shows cost matters, cache on `LinearizationPlan` — but don't optimize prematurely.
- **AcroForm dict itself (obj 5)**: Not in `first_page_closure` (only reachable from Catalog, not from page 0), so it's not peeled — it falls into `part4_rest` naturally. Only widgets (6..10) are in the intersection.
- **Non-ObjStm direct objects**: This fix only handles the ObjStm path (physical placement is by container routing). Direct objects that are in both sets would still be physically misplaced. That's out of scope for flpdf-sjgv (parent epic is ObjStm-only).
