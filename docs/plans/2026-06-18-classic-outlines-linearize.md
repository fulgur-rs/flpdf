# Classic-path Outline Routing for Linearization

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix the classic (plain, no-ObjStm) `--linearize` path to route outline objects to the correct PDF section — second-half (part9) for no-UseOutlines, first-half before /E (part6) for UseOutlines — producing byte-identical output to `qpdf --linearize --deterministic-id`.

**Architecture:**

Two new `LinearizationPlan` fields split outline objects from `part4_rest`:

- `part9_outline_objects: Vec<ObjectRef>` — populated when `!UseOutlines`; gets second-half object numbers (inserted between `pages_tree` and `info/param_dict` in `renumber.rs`), emitted after /E via `part4_objects()`.
- `part6_outline_objects: Vec<ObjectRef>` — populated when `UseOutlines`; gets first-half numbers (same slot as current `part4_rest_remaining`), emitted **before** /E (new writer loop between Part 3 and /E mark).

For no-UseOutlines, `page_hints[0].object_count` is unchanged.
For UseOutlines, `page_hints[0].object_count` gains `part6_outline_objects.len()`.

Root-cause confirmed by diagnostic:
- In qpdf no-UseOutlines: outlines are objects 4-84 (second half, second-half numbers); Part 1 xref `xref\n85 85`.
- In flpdf (broken): outlines get first-half numbers 89-169 but are physically after /E; Part 1 xref `xref\n4 166` (too large).
- In qpdf UseOutlines: outlines are at objects 89+ (first-half), physically before /E=17794; page0 nobjects=163.
- In flpdf UseOutlines (broken): outlines at 89+ but physically after /E=11259; page0 nobjects=82.

**Tech Stack:** Rust, `crates/flpdf/src/linearization/{plan,renumber,writer,hint_page}.rs`

---

## Diagnostic commands (already run, goldens do not yet exist)

```bash
# Generate qpdf classic goldens (run from repo root)
FIXTURE=tests/fixtures/compat
qpdf --linearize --deterministic-id "$FIXTURE/objstm-lin-outlines-80-80.pdf" \
  /tmp/qpdf_outlines_classic.pdf
qpdf --linearize --deterministic-id "$FIXTURE/objstm-lin-useoutlines-80-80.pdf" \
  /tmp/qpdf_useoutlines_classic.pdf
```

---

## Task 1 — Add `part9_outline_objects` and `part6_outline_objects` fields to `LinearizationPlan`

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs:357-451` (struct fields)

**Step 1: Read struct fields section**

```bash
grep -n "part4_rest\|outline_first_page_members\|part4_other_pages" \
  crates/flpdf/src/linearization/plan.rs | head -30
```

**Step 2: Add two new fields after `outline_first_page_members`**

In the `LinearizationPlan` struct, add after the `outline_first_page_members` field:

```rust
/// Outline objects (reachable from `/Catalog /Outlines`) for classic
/// (non-ObjStm) linearization when `/PageMode` is NOT `/UseOutlines`.
/// These are assigned second-half object numbers (between `pages_tree` and
/// `info/param_dict`) and emitted after /E, matching qpdf's `lc_outlines`
/// (part9) placement.  Empty when `UseOutlines` is active or when there are
/// no outlines.
pub(crate) part9_outline_objects: Vec<ObjectRef>,

/// Outline objects for classic (non-ObjStm) linearization when `/PageMode
/// /UseOutlines` is set.  These get first-half numbers (same slot as
/// `part4_rest` remaining) and are emitted **before** /E (between Part 3
/// and the /E boundary), matching qpdf's `lc_outlines` (part6) placement.
/// Empty when `UseOutlines` is not set or when there are no outlines.
pub(crate) part6_outline_objects: Vec<ObjectRef>,
```

**Step 3: Verify fields added correctly**

```bash
cargo check -p flpdf 2>&1 | head -30
```

Expected: compilation errors about missing field initializations in `from_pdf()`. That's expected — fix in next task.

**Step 4: Commit**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "feat(flpdf): add part9/part6 outline fields to LinearizationPlan"
```

---

## Task 2 — Populate new fields in `from_pdf()` and fix `page_hints[0].object_count`

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs` — `from_pdf()` (around line 478)

**Step 1: Locate outline membership computation**

```bash
grep -n "outlines_in_first_page\|outlines_set\|outline_first_page_members" \
  crates/flpdf/src/linearization/plan.rs | head -20
```

The relevant section is around lines 777-782 where `outline_first_page_members` is populated.

**Step 2: Read the `from_pdf()` return / struct initializer**

Find where the `LinearizationPlan { ... }` struct is constructed and returned. Look for `outline_first_page_members:` in that initializer.

**Step 3: Compute outline sets and populate new fields**

In `from_pdf()`, compute the outline set for BOTH cases. The existing `outlines_in_first_page_predicate` check already gates UseOutlines. Extend the code to also extract from `part4_rest`:

```rust
// Compute outline routing for the classic linearize path.
// outlines_in_first_page_predicate = true means /PageMode /UseOutlines.
let outlines_in_first_page = outlines_in_first_page_predicate(pdf)?;
let outline_set: BTreeSet<ObjectRef> = if outlines_in_first_page || has_any_outlines(pdf)? {
    outlines_set(pdf)?
} else {
    BTreeSet::new()
};
let outline_first_page_members = if outlines_in_first_page {
    outline_set.clone()
} else {
    BTreeSet::new()
};
```

Wait — the existing code already computes `outline_first_page_members`. Adjust the existing code to ALSO extract outline objects from `part4_rest`.

The actual approach: **after `part4_rest` is finalized** (all objects assigned), extract outline objects:

```rust
// Classic-path outline routing: extract outline objects from part4_rest.
// These are objects reachable only from /Catalog /Outlines (not from any page).
let all_outlines: BTreeSet<ObjectRef> = outlines_set(pdf)?;

let (part6_outline_objects, part9_outline_objects): (Vec<ObjectRef>, Vec<ObjectRef>) =
    if outlines_in_first_page_predicate(pdf)? {
        // UseOutlines: outlines go to first half (before /E) — part6.
        let part6: Vec<ObjectRef> = part4_rest
            .iter()
            .filter(|r| all_outlines.contains(r))
            .copied()
            .collect();
        (part6, vec![])
    } else {
        // No UseOutlines: outlines go to second half (after /E) — part9.
        let part9: Vec<ObjectRef> = part4_rest
            .iter()
            .filter(|r| all_outlines.contains(r))
            .copied()
            .collect();
        (vec![], part9)
    };

// Remove extracted outlines from part4_rest.
let outline_extract_set: BTreeSet<ObjectRef> = part6_outline_objects
    .iter()
    .chain(&part9_outline_objects)
    .copied()
    .collect();
part4_rest.retain(|r| !outline_extract_set.contains(r));
```

**Step 4: Update `outline_first_page_members` computation**

The existing `outline_first_page_members` can now use `all_outlines` directly:

```rust
let outline_first_page_members = if outlines_in_first_page_predicate(pdf)? {
    all_outlines
} else {
    BTreeSet::new()
};
```

**Step 5: Update `page_hints[0].object_count` for UseOutlines**

Find line ~639 where `page_hints[0].object_count` is set. Change it to:

```rust
page_hints[0].object_count =
    (page0_private.len() + part3_objects.len()) as u32;
```

This must be AFTER `part6_outline_objects` is computed (UseOutlines adds `part6_outline_objects.len()`). Add the outline count:

```rust
// For UseOutlines, outlines are emitted before /E and count toward page 0.
let outline_page0_count = part6_outline_objects.len() as u32;
page_hints[0].object_count =
    (page0_private.len() + part3_objects.len()) as u32 + outline_page0_count;
```

Note: restructure `from_pdf()` so that `part6_outline_objects` / `part9_outline_objects` are computed **before** `page_hints[0].object_count` is set, OR compute the count in two passes.

**Step 6: Update struct initializer**

In the `LinearizationPlan { ... }` constructor at end of `from_pdf()`, add:

```rust
part6_outline_objects,
part9_outline_objects,
```

**Step 7: Verify it compiles**

```bash
cargo check -p flpdf 2>&1 | grep -E "^error" | head -20
```

**Step 8: Commit**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "feat(flpdf): populate part9/part6 outline objects in from_pdf, fix page0 count"
```

---

## Task 3 — Update `part4_objects()` to include `part9_outline_objects`

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs:858-865` (`part4_objects()`)

**Step 1: Read current implementation**

```bash
sed -n '855,870p' crates/flpdf/src/linearization/plan.rs
```

**Step 2: Chain `part9_outline_objects`**

The `part9_outline_objects` need to be emitted after /E via the writer's `plan.part4_objects()` call. Add them to the chain (BEFORE `part4_rest` so that when sorted by new number they interleave correctly):

```rust
pub fn part4_objects(&self) -> Vec<ObjectRef> {
    self.part4_other_pages_private
        .iter()
        .chain(&self.part4_other_pages_shared)
        .chain(&self.part9_outline_objects)
        .chain(&self.part4_rest)
        .copied()
        .collect()
}
```

Note: The writer sorts `part4_emits` by new object number, so order in this Vec only affects tie-breaking. With correct renumber (part9_outlines get numbers < param_dict), they'll sort into the right position naturally.

**Step 3: Check `parts_are_disjoint()` assertion**

```bash
grep -n "parts_are_disjoint\|part6_outline\|part9_outline" \
  crates/flpdf/src/linearization/plan.rs | head -20
```

Ensure `parts_are_disjoint()` includes the new fields in its check, so duplicates are caught. Add `part6_outline_objects` and `part9_outline_objects` to the disjoint check:

```rust
fn parts_are_disjoint(&self) -> bool {
    let all: Vec<ObjectRef> = self
        .part2_objects
        .iter()
        .chain(&self.part3_objects)
        .chain(&self.part4_other_pages_private)
        .chain(&self.part4_other_pages_shared)
        .chain(&self.part6_outline_objects)  // NEW
        .chain(&self.part9_outline_objects)  // NEW
        .chain(&self.part4_rest)
        .copied()
        .collect();
    let set: BTreeSet<_> = all.iter().collect();
    set.len() == all.len()
}
```

**Step 4: Run existing tests to catch regressions**

```bash
cargo test -p flpdf --lib -- linearization 2>&1 | tail -30
```

Expected: all existing unit tests pass.

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "feat(flpdf): add part9_outline_objects to part4_objects() chain"
```

---

## Task 4 — Update `renumber.rs` to assign second-half numbers to `part9_outline_objects`

**Files:**
- Modify: `crates/flpdf/src/linearization/renumber.rs:165-276`

**Step 1: Read the current slot assignment**

```bash
sed -n '213,270p' crates/flpdf/src/linearization/renumber.rs
```

**Step 2: Update capacity hint**

In `from_plan()` capacity calculation, add the new fields:

```rust
let total_parts = plan.part2_objects.len()
    + plan.part3_objects.len()
    + plan.part4_other_pages_private.len()
    + plan.part4_other_pages_shared.len()
    + plan.part6_outline_objects.len()  // NEW
    + plan.part9_outline_objects.len()  // NEW
    + plan.part4_rest.len();
```

**Step 3: Add `part9_outline_objects` in second-half slot assignment**

After step 3 (`pages_tree`) and before step 4 (`info`), insert:

```rust
// 3b. part9 outline objects (classic path, no UseOutlines).
// These are the objects reachable from /Catalog /Outlines when /PageMode is
// NOT /UseOutlines.  qpdf places them in lc_outlines (part9) between the
// pages tree and the info dict, giving them consecutive second-half numbers.
for &original in &plan.part9_outline_objects {
    push_real(original, &mut by_new_number, &mut by_original);
}
```

**Step 4: Add `part6_outline_objects` in first-half slot assignment**

After step 9 (`Part 3`) and before step 10 (`part4_rest_remaining`), insert:

```rust
// 9b. Part-6 outline objects (classic path, UseOutlines).
// When /PageMode /UseOutlines, outline objects go into the first-half section
// (before /E), between Part 3 and the remaining part4_rest objects.
// They get first-half numbers in plan order.
for &original in &plan.part6_outline_objects {
    push_real(original, &mut by_new_number, &mut by_original);
}
```

**Step 5: Update the slot-assignment comment block**

Update the comment at line ~213 to document the new steps:

```rust
// Second-half renumber order (qpdf slot assignment):
//
//  slot 1..    part7 (other pages' private) in plan order
//  slot N+1..  part8 (other pages' shared) in plan order
//  slot ..     pages_tree (if in part4_rest)
//  slot ..     part9_outline_objects (classic no-UseOutlines outlines)
//  slot ..     info (if in part4_rest)
//  slot ..     <param dict reserved>
//  slot ..     root_ref / Catalog (if in part4_rest)
//  slot ..     <hint stream reserved>
//
// First-half follows:
//  slot ..     Part 2 in plan order
//  slot ..     Part 3 in plan order
//  slot ..     part6_outline_objects (classic UseOutlines outlines)
//  slot ..     part4_rest remaining (pages_tree/info/root/outlines already handled)
```

**Step 6: Verify `param_dict_slot` is correct after the change**

After the fix for no-UseOutlines (2 page1 private + 1 pages_tree + 81 outlines = 84 objects before param slot), `param_dict_slot` becomes 85. Check by running a quick test.

**Step 7: Run tests**

```bash
cargo test -p flpdf --lib -- linearization 2>&1 | tail -30
```

**Step 8: Commit**

```bash
git add crates/flpdf/src/linearization/renumber.rs
git commit -m "feat(flpdf): assign second/first-half numbers to part9/part6 outline objects"
```

---

## Task 5 — Update `writer.rs` to emit `part6_outline_objects` before /E

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs` — `do_write_pass()` around line 1880-1891

**Step 1: Read the Part 3 → /E boundary section**

```bash
sed -n '1878,1895p' crates/flpdf/src/linearization/writer.rs
```

**Step 2: Add emission loop for `part6_outline_objects`**

Between the Part 3 ObjStm containers loop and the `/E` mark, add:

```rust
// Part 6 (Annex F): first-page outlines (classic path, UseOutlines).
// When /PageMode /UseOutlines, outline objects are in the first-page section
// (before /E) matching qpdf's lc_outlines (part6) behaviour.  On the ObjStm
// path these objects are already handled via their ObjStm containers emitted
// above; skip any that are ObjStm members so they are not written twice.
for original_ref in &plan.part6_outline_objects {
    if objstm_layout.member_to_container.contains_key(original_ref) {
        continue;
    }
    let Some(new_ref) = renumber.new_for_original(*original_ref) else {
        return Err(crate::Error::Unsupported(format!(
            "part6 outline object {} has no renumber entry",
            original_ref
        )));
    };
    let object = pdf.resolve_borrowed(*original_ref)?;
    let renumbered = renumber_object(object, 0, renumber)?;
    let offset = append_body_object(&mut bytes, new_ref, &renumbered, options);
    xref_offsets.insert(new_ref.number, offset);
}
```

**Step 3: Verify existing `compute_outline_hint_info()` handles classic path**

```bash
grep -n "cov:ignore.*outline\|plain.*uncompressed.*outline\|compute_outline_hint" \
  crates/flpdf/src/linearization/writer.rs | head -10
```

The classic branch has `// cov:ignore: plain (uncompressed) outline — deferred`. This needs to be implemented now.

**Step 4: Implement the classic branch of `compute_outline_hint_info()`**

Read lines ~1448-1510 of writer.rs:

```bash
sed -n '1448,1510p' crates/flpdf/src/linearization/writer.rs
```

The function needs to handle the classic path (no `member_to_container`). For the classic path:
- Outline objects each have their own new object number
- `first_object` = the new number of the outline dict (root of the outline tree)
- `nobjects` = count of all outline objects (should be `part9_outline_objects.len()` or `part6_outline_objects.len()`)
- Objects ARE consecutive by construction (renumber assigns consecutive slots)

Remove the `// cov:ignore` and implement:

```rust
// Classic (uncompressed) path: each outline object is its own plain indirect.
// They are numbered consecutively (renumber assigns contiguous slots between
// pages_tree and param_dict for part9, or after Part 3 for part6), so the
// first new number plus the object count covers the entire group.
let outline_objects = if !plan.part9_outline_objects.is_empty() {
    &plan.part9_outline_objects[..]
} else {
    &plan.part6_outline_objects[..]
};
if outline_objects.is_empty() {
    return Ok(None);
}
// Resolve outline dict ref to its new number.
let outline_ref = outline_objects.iter()
    .filter_map(|r| renumber.new_for_original(*r))
    .map(|nr| nr.number)
    .min()
    .expect("outline objects must have renumber entries");
return Ok(Some(OutlineHintInfo {
    first_object: outline_ref,
    nobjects: outline_objects.len() as u32,
}));
```

Wait — actually looking at the function more carefully, it already handles the ObjStm case and has a fallthrough for the classic path. Read the full function to understand its structure before editing.

**Step 5: Build and run tests**

```bash
cargo test -p flpdf --lib 2>&1 | tail -30
```

**Step 6: Commit**

```bash
git add crates/flpdf/src/linearization/writer.rs
git commit -m "feat(flpdf): emit part6 outlines before /E, implement classic outline hint"
```

---

## Task 6 — Generate goldens and add integration tests

**Files:**
- Create: `tests/golden/references/objstm-lin-outlines-80-80/linearize.pdf`
- Create: `tests/golden/references/objstm-lin-useoutlines-80-80/linearize.pdf`
- Modify: `tests/golden/regenerate.sh`
- Modify: `crates/flpdf/tests/cmp_linearize_tests.rs`

**Step 1: Verify flpdf output matches qpdf for Case A (no UseOutlines)**

```bash
cargo build -p flpdf-cli --release 2>&1 | tail -5

FIXTURE=tests/fixtures/compat/objstm-lin-outlines-80-80.pdf
./target/release/flpdf rewrite --linearize "$FIXTURE" /tmp/flpdf_out_a.pdf

qpdf --show-linearization /tmp/flpdf_out_a.pdf 2>&1 | grep -E "first_page_object|first_page_end|Outlines|first_object:|nobjects:|group_length:"
```

Expected after fix:
- `first_page_object: 88`
- `first_page_end: 9705`
- `Outlines Hint Table / first_object: 4 / nobjects: 81`

**Step 2: Verify Case B (UseOutlines)**

```bash
FIXTURE=tests/fixtures/compat/objstm-lin-useoutlines-80-80.pdf
./target/release/flpdf rewrite --linearize "$FIXTURE" /tmp/flpdf_out_b.pdf

qpdf --show-linearization /tmp/flpdf_out_b.pdf 2>&1 | grep -E "first_page_object|first_page_end|Outlines|nobjects|group_length"
```

Expected after fix:
- `first_page_object: 7`
- `first_page_end: 17794`
- Page 0 `nobjects: 163`

**Step 3: Generate qpdf goldens**

```bash
mkdir -p tests/golden/references/objstm-lin-outlines-80-80
mkdir -p tests/golden/references/objstm-lin-useoutlines-80-80

qpdf --linearize --deterministic-id \
  tests/fixtures/compat/objstm-lin-outlines-80-80.pdf \
  tests/golden/references/objstm-lin-outlines-80-80/linearize.pdf

qpdf --linearize --deterministic-id \
  tests/fixtures/compat/objstm-lin-useoutlines-80-80.pdf \
  tests/golden/references/objstm-lin-useoutlines-80-80/linearize.pdf
```

**Step 4: Update `regenerate.sh`**

In `tests/golden/regenerate.sh`, find the section where `linearize-objstm.pdf` is generated for the outline fixtures and add generation of `linearize.pdf` (classic path):

```bash
# Add after the objstm-lin-outlines-80-80 objstm linearize entry:
qpdf --linearize --deterministic-id \
  "$FIXTURES/objstm-lin-outlines-80-80.pdf" \
  "$OUTDIR/objstm-lin-outlines-80-80/linearize.pdf"

qpdf --linearize --deterministic-id \
  "$FIXTURES/objstm-lin-useoutlines-80-80.pdf" \
  "$OUTDIR/objstm-lin-useoutlines-80-80/linearize.pdf"
```

**Step 5: Add tests in `cmp_linearize_tests.rs`**

Add four tests (structural + strict for each fixture):

```rust
// --------------------------------------------------------------------------
// Classic-linearize outline routing: no-UseOutlines (part9) and UseOutlines
// (part6).  These test the classic (plain, non-ObjStm) linearize path with
// outline objects, which were previously routed to the wrong PDF section.
// --------------------------------------------------------------------------

#[test]
fn outlines_linearized_structurally_byte_identical_to_qpdf() {
    assert_linearize_structurally_byte_identical(
        "objstm-lin-outlines-80-80.pdf",
        "objstm-lin-outlines-80-80",
    );
}

#[test]
fn outlines_linearized_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "objstm-lin-outlines-80-80.pdf",
        "objstm-lin-outlines-80-80",
    );
}

#[test]
fn useoutlines_linearized_structurally_byte_identical_to_qpdf() {
    assert_linearize_structurally_byte_identical(
        "objstm-lin-useoutlines-80-80.pdf",
        "objstm-lin-useoutlines-80-80",
    );
}

#[test]
fn useoutlines_linearized_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "objstm-lin-useoutlines-80-80.pdf",
        "objstm-lin-useoutlines-80-80",
    );
}
```

**Step 6: Run new tests (requires `qpdf-zlib-compat` feature)**

```bash
cargo test -p flpdf --features qpdf-zlib-compat \
  -- outlines_linearized useoutlines_linearized 2>&1 | tail -20
```

Expected: all 4 tests pass.

**Step 7: Run regression tests (ObjStm outline tests must still pass)**

```bash
cargo test -p flpdf --features qpdf-zlib-compat \
  -- outlines_objstm useoutlines_objstm 2>&1 | tail -20
```

Expected: still pass.

**Step 8: Commit**

```bash
git add tests/golden/references/objstm-lin-outlines-80-80/linearize.pdf
git add tests/golden/references/objstm-lin-useoutlines-80-80/linearize.pdf
git add tests/golden/regenerate.sh
git add crates/flpdf/tests/cmp_linearize_tests.rs
git commit -m "test(flpdf): add classic-linearize outline golden + byte-identity tests"
```

---

## Task 7 — Patch coverage gate

**Step 1: Commit all changes**

```bash
git status  # confirm clean or only the new files
```

**Step 2: Run patch coverage**

```bash
scripts/patch-coverage.sh --base main 2>&1 | tail -30
```

Expected: 100% coverage on changed lines in `crates/flpdf/src/`.

**Step 3: If uncovered lines, add tests or `cov:ignore`**

For any error arm that genuinely cannot be exercised (e.g., the `part6 object has no renumber entry` guard), add:

```rust
// cov:ignore: planner/renumber inconsistency cannot occur by construction
```

Document each `cov:ignore` reason in the PR description.

**Step 4: Run full test suite**

```bash
cargo test -p flpdf 2>&1 | tail -20
cargo test -p flpdf --features qpdf-zlib-compat 2>&1 | tail -20
```

**Step 5: cargo fmt check**

```bash
cargo fmt --check 2>&1
```

If failures, run `cargo fmt` and commit.

---

## Acceptance criteria

- [ ] `qpdf --show-linearization` on flpdf's output for `objstm-lin-outlines-80-80.pdf` (classic path) shows `first_page_object: 88`, `first_page_end: 9705`, outline `first_object: 4`, `nobjects: 81`.
- [ ] `qpdf --show-linearization` on flpdf's output for `objstm-lin-useoutlines-80-80.pdf` (classic path) shows page0 `nobjects: 163`, `first_page_end: 17794`.
- [ ] `outlines_linearized_is_byte_identical_to_qpdf` test passes.
- [ ] `useoutlines_linearized_is_byte_identical_to_qpdf` test passes.
- [ ] Existing ObjStm outline tests (`outlines_objstm_byte_identical_to_qpdf`, `useoutlines_objstm_byte_identical_to_qpdf`) still pass.
- [ ] Existing non-outline classic tests (`one_page_linearized_is_byte_identical_to_qpdf` etc.) still pass.
- [ ] `scripts/patch-coverage.sh --base main` exits 0.
