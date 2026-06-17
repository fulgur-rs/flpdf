# outlines_in_first_page (flpdf-vvjr.1) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Route `/PageMode /UseOutlines` outline objects into part6 (first-page section) and fold the outline ObjStm container into page-0 nobjects, achieving byte-identical output to qpdf 11.9.0 for the `UseOutlines` fixture.

**Architecture:** Three touch-points in `plan.rs` (predicate + routing + plan field) and one in `hint_page.rs` (page-0 count fold). A new fixture generator variant + qpdf golden captures the expected output; existing `objstm-lin-outlines-80-80` regression test guards the part9 path.

**Tech Stack:** Rust, `crates/flpdf/src/linearization/plan.rs`, `crates/flpdf/src/linearization/hint_page.rs`, `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`, `tests/fixtures/compat/`, `tests/golden/`, `tests/golden/regenerate.sh`, `docs/plans/tools/gen_outlines_gap.py`

---

## For Claude: Key Context

### Empirical ground truth (from issue design)
- Fixture: `gen_outlines_gap.py 80 80` + catalog `/PageMode /UseOutlines`
- qpdf output: `L=6687, /E=5220, hint dict /S 45 /O 76, page0 nobjects=4, page1 nobjects=2`
- flpdf current (rm09): `L=6667, /E=3542, /S 45 /O 75, page0 nobjects=3` — outline container goes to second half

### What needs to change
1. **Predicate** `outlines_in_first_page`: catalog has `/PageMode /UseOutlines` AND `/Outlines`
2. **Routing** in `route_objstm_containers` (`plan.rs:1805`): check `outlines_set` FIRST (higher precedence than `in_open_document`); route to `FirstPage` if predicate, else `Rest`
3. **Plan field** `outline_first_page_members` in `LinearizationPlan`: computed in `from_pdf` when predicate=true; used by `page0_object_count_with_objstm`
4. **page-0 nobjects fold** in `hint_page.rs::page0_object_count_with_objstm` (~line 255): chain `plan.outline_first_page_members` so the outline container counts toward page-0

### What does NOT change
- `build_outline_hint_table` / `hint_stream.rs` — still emits Outlines table + `/O` (both part6 and part9)
- The offset formula in `build_outline_hint_table` — already correct for part6 too
- The `objstm-lin-outlines-80-80` (no `/PageMode`) behavior — must remain byte-identical (regression guard)

### qpdf precedence order (QPDF_linearization.cc:1118-1122)
`in_outlines > in_open_document > in_first_page > other_page_categories`

---

## Task 1: Create fixture generator for `/PageMode /UseOutlines`

**Files:**
- Modify: `docs/plans/tools/gen_outlines_gap.py`

**Step 1: Add optional `--use-outlines` flag to gen_outlines_gap.py**

```python
# Near the top, after the existing S/K arg parsing:
use_outlines = len(sys.argv) > 3 and sys.argv[3] == "--use-outlines"
```

Then modify the catalog line to conditionally include `/PageMode`:

```python
if use_outlines:
    objs[catalog] = b"<< /Type /Catalog /PageMode /UseOutlines /Outlines %d 0 R /Pages %d 0 R >>" % (outlines, pages)
else:
    objs[catalog] = b"<< /Type /Catalog /Outlines %d 0 R /Pages %d 0 R >>" % (outlines, pages)
```

**Step 2: Verify the generator produces valid PDF**

Run:
```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-vvjr1
python3 docs/plans/tools/gen_outlines_gap.py 80 80 --use-outlines | qpdf --check /dev/stdin
```

Expected: `No errors found.`

**Step 3: Commit**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-vvjr1
git add docs/plans/tools/gen_outlines_gap.py
git commit -m "test(flpdf): add --use-outlines flag to gen_outlines_gap.py fixture generator"
```

---

## Task 2: Generate fixture PDF and qpdf golden

**Files:**
- Create: `tests/fixtures/compat/objstm-lin-useoutlines-80-80.pdf`
- Create: `tests/golden/references/objstm-lin-useoutlines-80-80/linearize-objstm.pdf`
- Modify: `tests/golden/regenerate.sh`

**Step 1: Generate fixture PDF**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-vvjr1
python3 docs/plans/tools/gen_outlines_gap.py 80 80 --use-outlines \
    > tests/fixtures/compat/objstm-lin-useoutlines-80-80.pdf
```

**Step 2: Generate qpdf golden**

```bash
mkdir -p tests/golden/references/objstm-lin-useoutlines-80-80
qpdf --linearize --object-streams=generate --deterministic-id --warning-exit-0 \
    tests/fixtures/compat/objstm-lin-useoutlines-80-80.pdf \
    tests/golden/references/objstm-lin-useoutlines-80-80/linearize-objstm.pdf
qpdf --check tests/golden/references/objstm-lin-useoutlines-80-80/linearize-objstm.pdf
```

Expected: `No errors found.`

**Step 3: Verify golden matches empirical data from design**

```bash
qpdf --show-linearization \
    tests/golden/references/objstm-lin-useoutlines-80-80/linearize-objstm.pdf 2>&1 | head -20
```

Expected to see: `page0 nobjects=4`, `/E=5220`, hint dict `/S 45 /O 76`.

**Step 4: Update regenerate.sh to include the new fixture**

In `tests/golden/regenerate.sh`, find the `G6HB2_FIX` block (~line 221) and add:

```bash
[objstm-lin-useoutlines-80-80]="gen_outlines_gap.py 80 80 --use-outlines"
```

Also add `objstm-lin-useoutlines-80-80` to the golden loop (~line 382):

```bash
for stem in objstm-lin-sharedfonts-100 objstm-lin-mixed-60-70 \
            objstm-lin-threepage-2-120 objstm-lin-disc-2-250-2 \
            objstm-lin-openaction-80-80 objstm-lin-outlines-80-80 \
            objstm-lin-useoutlines-80-80; do
```

**Step 5: Commit**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-vvjr1
git add tests/fixtures/compat/objstm-lin-useoutlines-80-80.pdf \
        tests/golden/references/objstm-lin-useoutlines-80-80/linearize-objstm.pdf \
        tests/golden/regenerate.sh
git commit -m "test(flpdf): add UseOutlines linearized ObjStm fixture and qpdf golden"
```

---

## Task 3: Add `outlines_in_first_page_predicate` and plan field

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs`

**Step 1: Add `outline_first_page_members` field to `LinearizationPlan` struct**

Locate the `all_referenced_pages` field (~line 442) and add AFTER it:

```rust
/// Outline objects routed to the first-page section (part6) when the catalog
/// specifies `/PageMode /UseOutlines`. Empty when the predicate is false.
///
/// Used by `page0_object_count_with_objstm` to include the outline ObjStm
/// container in the page-0 object count (qpdf `entries.at(0).nobjects`
/// includes all part6 objects, including outlines placed there).
pub(crate) outline_first_page_members: Vec<ObjectRef>,
```

**Step 2: Write a failing unit test for the predicate (TDD)**

Locate the unit test block (~line 1863) and add AFTER the existing `outlines_set_empty_for_non_dictionary_catalog` test (~line 4288):

```rust
#[test]
fn outlines_in_first_page_predicate_true_when_use_outlines_and_outlines_present() {
    // Catalog with /PageMode /UseOutlines + /Outlines → predicate returns true.
    let mut pdf = Pdf::open(Cursor::new(
        outlines_pdf_bytes_with_page_mode(b"UseOutlines"),
    ))
    .unwrap();
    assert!(outlines_in_first_page_predicate(&mut pdf).unwrap());
}

#[test]
fn outlines_in_first_page_predicate_false_without_page_mode() {
    // Catalog with /Outlines but no /PageMode → predicate returns false.
    let mut pdf = Pdf::open(Cursor::new(outlines_pdf_bytes())).unwrap();
    assert!(!outlines_in_first_page_predicate(&mut pdf).unwrap());
}

#[test]
fn outlines_in_first_page_predicate_false_when_page_mode_not_use_outlines() {
    // /PageMode /FullScreen (not UseOutlines) → predicate returns false.
    let mut pdf = Pdf::open(Cursor::new(
        outlines_pdf_bytes_with_page_mode(b"FullScreen"),
    ))
    .unwrap();
    assert!(!outlines_in_first_page_predicate(&mut pdf).unwrap());
}

#[test]
fn outlines_in_first_page_predicate_false_when_no_outlines() {
    // /PageMode /UseOutlines but NO /Outlines → predicate returns false.
    let mut pdf = Pdf::open(Cursor::new(minimal_one_page_pdf())).unwrap();
    assert!(!outlines_in_first_page_predicate(&mut pdf).unwrap());
}
```

**Step 3: Add the `outlines_pdf_bytes_with_page_mode` test helper**

Locate `outlines_pdf_bytes()` test helper (~line 4228) and add a variant:

```rust
fn outlines_pdf_bytes_with_page_mode(mode: &[u8]) -> Vec<u8> {
    // Re-use outlines_pdf_bytes() body but inject /PageMode into the catalog.
    // Replace the catalog dict: add /PageMode <mode> before /Outlines.
    let base = outlines_pdf_bytes();
    // The catalog line is: "<< /Type /Catalog /Outlines ..."
    // We insert /PageMode /UseOutlines before /Outlines.
    let needle = b"<< /Type /Catalog /Outlines";
    let replacement = format!(
        "<< /Type /Catalog /PageMode /{} /Outlines",
        std::str::from_utf8(mode).unwrap()
    );
    let pos = base.windows(needle.len()).position(|w| w == needle).unwrap();
    let mut out = base[..pos].to_vec();
    out.extend_from_slice(replacement.as_bytes());
    out.extend_from_slice(&base[pos + needle.len()..]);
    out
}
```

**Step 4: Run the tests to verify they FAIL (since predicate not yet implemented)**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-vvjr1
cargo test -p flpdf --lib -- outlines_in_first_page 2>&1 | tail -20
```

Expected: compile error or test failures (predicate doesn't exist yet).

**Step 5: Implement `outlines_in_first_page_predicate`**

Add after the `outlines_set` function (~line 1775):

```rust
/// Returns `true` when the catalog specifies `/PageMode /UseOutlines` AND has
/// an `/Outlines` entry (QPDF_linearization.cc:1031-1043).
fn outlines_in_first_page_predicate<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<bool> {
    let Some(root) = pdf.root_ref() else {
        return Ok(false);
    };
    let Object::Dictionary(cat) = pdf.resolve(root)? else {
        return Ok(false);
    };
    if !cat.contains_key("Outlines") {
        return Ok(false);
    }
    match cat.get("PageMode") {
        Some(Object::Name(n)) => Ok(n == b"UseOutlines"),
        Some(Object::Reference(r)) => {
            Ok(matches!(pdf.resolve(*r)?, Object::Name(n) if n == b"UseOutlines"))
        }
        _ => Ok(false),
    }
}
```

**Step 6: Populate `outline_first_page_members` in `LinearizationPlan::from_pdf`**

Locate the `Ok(Self { ... })` block (~line 795). Before it, compute the field:

```rust
// Compute outline members for the first-page fold (outlines_in_first_page path).
// When the catalog has /PageMode /UseOutlines + /Outlines, qpdf routes outline
// containers to part6; page0_object_count_with_objstm needs these members to
// count their container.
let outline_first_page_members: Vec<ObjectRef> =
    if outlines_in_first_page_predicate(pdf)? {
        outlines_set(pdf)?.into_iter().collect()
    } else {
        Vec::new()
    };
```

Then add to the struct initializer:

```rust
Ok(Self {
    ...
    all_referenced_pages,
    outline_first_page_members,
})
```

**Step 7: Initialize `outline_first_page_members` in the placeholder constructor**

Locate `LinearizationPlan` default/placeholder (~line 1117) and add:

```rust
outline_first_page_members: Vec::new(),
```

**Step 8: Run tests to verify predicate tests pass**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-vvjr1
cargo test -p flpdf --lib -- outlines_in_first_page 2>&1 | tail -20
```

Expected: all 4 tests PASS.

**Step 9: Verify regression — existing tests still pass**

```bash
cargo test -p flpdf --lib -- outlines_set 2>&1 | tail -10
```

**Step 10: Commit**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "feat(flpdf): add outlines_in_first_page_predicate and outline_first_page_members to LinearizationPlan"
```

---

## Task 4: Update `route_objstm_containers` — in_outlines routing

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs`

**Step 1: Write a failing test for UseOutlines container routing to FirstPage**

Locate the `linearized_routes_open_document_container_before_page_categories` test (~line 4296) and add AFTER it:

```rust
/// A container holding an outline member → FirstPage when
/// outlines_in_first_page=true (UseOutlines catalog).
#[test]
fn route_objstm_containers_outlines_first_page_routes_to_first_page() {
    let bytes = outlines_pdf_bytes_with_page_mode(b"UseOutlines");
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    // Build a synthetic single-container with just the outline dict
    // (the only indirect ref reachable from /Outlines in outlines_pdf_bytes).
    let outline_ref = ObjectRef::new(5, 0); // obj 5 = outline dict in outlines_pdf_bytes
    let synthetic = vec![vec![outline_ref]];
    let routes = route_objstm_containers(&mut pdf, &synthetic).unwrap();
    assert_eq!(routes, vec![ContainerPart::FirstPage],
        "outline container must route to FirstPage when /PageMode /UseOutlines");
}

/// Without /PageMode /UseOutlines, outline containers stay in Rest (part9).
#[test]
fn route_objstm_containers_outlines_no_use_outlines_routes_to_rest() {
    let mut pdf = Pdf::open(Cursor::new(outlines_pdf_bytes())).unwrap();
    let outline_ref = ObjectRef::new(5, 0);
    let synthetic = vec![vec![outline_ref]];
    let routes = route_objstm_containers(&mut pdf, &synthetic).unwrap();
    assert_eq!(routes, vec![ContainerPart::Rest],
        "outline container must route to Rest when no /PageMode /UseOutlines");
}
```

**Step 2: Run tests to confirm they FAIL**

```bash
cargo test -p flpdf --lib -- "route_objstm_containers_outlines" 2>&1 | tail -10
```

Expected: FAIL (outlines check not yet in route_objstm_containers).

**Step 3: Update `route_objstm_containers` to check in_outlines first**

Locate `route_objstm_containers` (~line 1805). At the top of the function body, add:

```rust
let outline_set = outlines_set(pdf)?;
let outlines_first_page = if outline_set.is_empty() {
    false
} else {
    outlines_in_first_page_predicate(pdf)?
};
```

Then in the per-container closure, add the in_outlines check as the FIRST check (before `open_doc_set`):

```rust
.map(|members| {
    // in_outlines takes precedence over in_open_document and in_first_page
    // (QPDF_linearization.cc:1118-1122).
    if !outline_set.is_empty() && members.iter().any(|m| outline_set.contains(m)) {
        return if outlines_first_page {
            ContainerPart::FirstPage
        } else {
            ContainerPart::Rest
        };
    }
    // in_open_document takes precedence over every page category.
    if members.iter().any(|m| open_doc_set.contains(m)) {
        return ContainerPart::OpenDocument;
    }
    // ...rest unchanged
```

**Step 4: Update the doc comment `# Deviation` on `route_objstm_containers`**

Find (~line 1792):
```
/// qpdf checks `in_outlines` and the thumbnail categories *before*
/// `in_first_page` too; those are not modeled here (they do not occur in the
/// supported corpus).
```

Replace with:
```
/// qpdf checks `in_outlines` before `in_open_document` and `in_first_page`
/// (QPDF_linearization.cc:1118-1122). This is modeled above: when the catalog
/// has `/PageMode /UseOutlines`, outline containers route to `FirstPage`;
/// otherwise to `Rest`. Thumbnail categories are not yet modeled.
```

**Step 5: Run the new routing tests**

```bash
cargo test -p flpdf --lib -- "route_objstm_containers_outlines" 2>&1 | tail -10
```

Expected: PASS.

**Step 6: Run all routing and outlines unit tests**

```bash
cargo test -p flpdf --lib -- "route_objstm_containers\|outlines_set\|outlines_in_first_page" 2>&1 | tail -15
```

Expected: all PASS.

**Step 7: Commit**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "feat(flpdf): route outline containers to FirstPage when /PageMode /UseOutlines (outlines_in_first_page)"
```

---

## Task 5: Fold outline containers into page-0 nobjects

**Files:**
- Modify: `crates/flpdf/src/linearization/hint_page.rs`

**Step 1: Write a failing test for the fold**

Locate the `page0_object_count_with_objstm` test block (~line 1114). Add a new test AFTER the existing one:

```rust
/// When outline objects are in a FirstPage container (outlines_in_first_page),
/// page0_object_count_with_objstm must count the outline container once.
#[test]
fn page0_object_count_includes_outline_container_when_first_page() {
    use std::io::Cursor;
    use crate::Pdf;

    // Build a minimal plan: 1 part2 object, 0 part3, 1 outline member in a container.
    let outline_ref = ObjectRef::new(10, 0);
    let container_num = 99u32;
    let mut plan = LinearizationPlan::placeholder();
    plan.part2_objects = vec![ObjectRef::new(5, 0)];
    plan.outline_first_page_members = vec![outline_ref];

    // member_to_container: outline_ref → container 99
    let mut m2c = std::collections::BTreeMap::new();
    m2c.insert(outline_ref, (container_num, 0u32));

    // Without the fold, count = 1 (part2 plain only).
    // With the fold, count = 2 (part2 plain + outline container).
    assert_eq!(page0_object_count_with_objstm(&plan, &m2c), 2,
        "outline container must be counted when outline_first_page_members is non-empty");
}
```

**Step 2: Run test to confirm it FAILS**

```bash
cargo test -p flpdf --lib -- "page0_object_count_includes_outline_container" 2>&1 | tail -10
```

Expected: FAIL (currently returns 1, not 2).

**Step 3: Update `page0_object_count_with_objstm`**

Locate `page0_object_count_with_objstm` (~line 255 in `hint_page.rs`):

```rust
fn page0_object_count_with_objstm(
    plan: &LinearizationPlan,
    member_to_container: &std::collections::BTreeMap<ObjectRef, (u32, u32)>,
) -> u32 {
    // Page 0's section is Part 2 (always plain) followed by Part 3 (plain or
    // folded into a first-half container). No container is excluded — page 0
    // owns its first-page (part6) containers.
    objstm_folded_count(
        plan.part2_objects.iter().chain(&plan.part3_objects),
        member_to_container,
        &std::collections::BTreeSet::new(),
    )
}
```

Change to:

```rust
fn page0_object_count_with_objstm(
    plan: &LinearizationPlan,
    member_to_container: &std::collections::BTreeMap<ObjectRef, (u32, u32)>,
) -> u32 {
    // Page 0's section is Part 2 (always plain) followed by Part 3 (plain or
    // folded into a first-half container), plus any outline objects routed to
    // the first-page section when /PageMode /UseOutlines is set. qpdf counts
    // all part6 objects in entries.at(0).nobjects (QPDF_linearization.cc:1222).
    objstm_folded_count(
        plan.part2_objects
            .iter()
            .chain(&plan.part3_objects)
            .chain(&plan.outline_first_page_members),
        member_to_container,
        &std::collections::BTreeSet::new(),
    )
}
```

**Step 4: Run the new fold test**

```bash
cargo test -p flpdf --lib -- "page0_object_count_includes_outline" 2>&1 | tail -10
```

Expected: PASS.

**Step 5: Run the existing page0 count test to confirm no regression**

```bash
cargo test -p flpdf --lib -- "page0_object_count" 2>&1 | tail -10
```

Expected: all PASS (existing test uses empty `outline_first_page_members`).

**Step 6: Commit**

```bash
git add crates/flpdf/src/linearization/hint_page.rs
git commit -m "feat(flpdf): fold first-page outline containers into page-0 object count"
```

---

## Task 6: Add byte-parity tests for UseOutlines

**Files:**
- Modify: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

**Step 1: Add default-feature coverage test**

Open `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`. Note: the file is `#![cfg(feature = "qpdf-zlib-compat")]`. For the default-feature coverage test, add a SEPARATE file or add an `#[cfg(not(feature = "qpdf-zlib-compat"))]` block. Check how the existing coverage tests are structured:

```bash
ls /home/ubuntu/flpdf/.worktrees/flpdf-vvjr1/crates/flpdf/tests/
```

Look for a `linearize_objstm_generate_tests.rs` file; it's likely the default-feature coverage test file.

**Step 2: Check `linearize_objstm_generate_tests.rs` structure**

```bash
grep -n "useoutlines\|outlines\|UseOutlines" \
    /home/ubuntu/flpdf/.worktrees/flpdf-vvjr1/crates/flpdf/tests/linearize_objstm_generate_tests.rs | head -20
```

**Step 3: Add coverage test (no qpdf-zlib-compat) in `linearize_objstm_generate_tests.rs`**

Add at the END of the file:

```rust
// useoutlines-80-80: /PageMode /UseOutlines causes outline containers to route
// to FirstPage (part6). Verifies route_objstm_containers FirstPage arm and
// page-0 nobjects fold without qpdf-zlib-compat.
#[test]
fn useoutlines_objstm_has_correct_page0_nobjects() {
    let output = flpdf_linearized_objstm("objstm-lin-useoutlines-80-80.pdf");
    // Verify the show-linearization page-0 object count matches qpdf's 4.
    // We check indirectly via the hint table bytes being non-trivially different
    // from the non-UseOutlines case (the test exercises the routing + fold code path).
    // The primary correctness gate is the byte-parity test in cmp_linearize_objstm_tests.
    assert!(!output.is_empty(), "linearized UseOutlines output must not be empty");
    // Must parse as valid linearized PDF (no convergence failure).
    use flpdf::Pdf;
    let mut pdf = Pdf::open(std::io::Cursor::new(&output)).unwrap();
    // The linearization dict must have /E > 0 (first-page section exists and /E is plausible).
    // Just open successfully without panic as a basic smoke test.
    let _ = pdf.object_refs();
}
```

Actually, better: use a structural assertion based on what we know about the fixture. Check that the output has a specific structure instead of just "not empty".

Actually since this is a coverage test (default features, no zlib-compat), the primary goal is to exercise the new code path. A simpler approach: just run flpdf on the fixture and verify it doesn't panic/error.

```rust
#[test]
fn useoutlines_objstm_linearize_succeeds_without_panic() {
    // Exercises route_objstm_containers FirstPage arm + page-0 fold.
    // Byte-parity is gated on qpdf-zlib-compat in cmp_linearize_objstm_tests.rs.
    let output = flpdf_linearized_objstm("objstm-lin-useoutlines-80-80.pdf");
    assert!(!output.is_empty());
}
```

**Step 4: Add qpdf-zlib-compat structural + strict tests in `cmp_linearize_objstm_tests.rs`**

Locate the `outlines_objstm_byte_identical_to_qpdf` test (~line 286) and add AFTER it:

```rust
// useoutlines-80-80 (flpdf-vvjr.1): /PageMode /UseOutlines causes outline
// objects (dict + 80 items) to route to part6 (first-page section) instead of
// part9. Their ObjStm container folds into page-0 nobjects (qpdf: 4, was 3).
// Two pages share fonts so a first-page (part6) container coexists.
// Regression: objstm-lin-outlines-80-80 (no /PageMode) must stay byte-identical.
#[test]
fn useoutlines_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-useoutlines-80-80.pdf",
        "objstm-lin-useoutlines-80-80",
    );
}

#[test]
fn useoutlines_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-useoutlines-80-80.pdf",
        "objstm-lin-useoutlines-80-80",
    );
}
```

**Step 5: Run all new tests under default features**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-vvjr1
cargo test -p flpdf --test linearize_objstm_generate_tests -- useoutlines 2>&1 | tail -10
```

Expected: PASS.

**Step 6: Run structural test under qpdf-zlib-compat**

```bash
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    --features qpdf-zlib-compat -- useoutlines_objstm_structurally 2>&1 | tail -15
```

Expected: PASS (structural = layout correct, ignoring /ID[1]).

**Step 7: Run strict test (full byte identity)**

```bash
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    --features qpdf-zlib-compat -- useoutlines_objstm_byte_identical 2>&1 | tail -15
```

Expected: PASS.

**Step 8: Run regression guard for outlines part9 path**

```bash
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    --features qpdf-zlib-compat -- outlines_objstm 2>&1 | tail -15
```

Expected: both `outlines_objstm_structurally_byte_identical_to_qpdf` and `outlines_objstm_byte_identical_to_qpdf` PASS.

**Step 9: Commit**

```bash
git add crates/flpdf/tests/cmp_linearize_objstm_tests.rs \
        crates/flpdf/tests/linearize_objstm_generate_tests.rs
git commit -m "test(flpdf): add UseOutlines byte-parity + coverage tests (flpdf-vvjr.1)"
```

---

## Task 7: Run all tests and quality gate

**Step 1: Full test suite (default features)**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-vvjr1
cargo test --workspace 2>&1 | tail -20
```

Expected: all PASS.

**Step 2: Full test suite with qpdf-zlib-compat**

```bash
cargo test --workspace --features flpdf/qpdf-zlib-compat 2>&1 | tail -20
```

Expected: all PASS (including new and existing byte-parity tests).

**Step 3: Cargo fmt**

```bash
cargo fmt --all
git add -p  # stage fmt changes if any
```

**Step 4: Patch coverage gate**

```bash
# Commit all changes first, then run
scripts/patch-coverage.sh --base main 2>&1 | tail -30
```

Expected: 100% coverage on changed lines in `crates/flpdf/`.

**Step 5: Final commit if fmt made changes**

```bash
git add crates/flpdf/src/linearization/plan.rs crates/flpdf/src/linearization/hint_page.rs
git commit -m "style(flpdf): cargo fmt"
```

---

## Summary of files changed

| File | Change |
|------|--------|
| `docs/plans/tools/gen_outlines_gap.py` | Add `--use-outlines` flag |
| `tests/golden/regenerate.sh` | Add `objstm-lin-useoutlines-80-80` to fixture map and golden loop |
| `tests/fixtures/compat/objstm-lin-useoutlines-80-80.pdf` | New fixture (binary) |
| `tests/golden/references/objstm-lin-useoutlines-80-80/linearize-objstm.pdf` | New golden (binary) |
| `crates/flpdf/src/linearization/plan.rs` | Add predicate, field, routing check |
| `crates/flpdf/src/linearization/hint_page.rs` | Fold outline members in page-0 count |
| `crates/flpdf/tests/cmp_linearize_objstm_tests.rs` | New structural + strict tests |
| `crates/flpdf/tests/linearize_objstm_generate_tests.rs` | New coverage test |
