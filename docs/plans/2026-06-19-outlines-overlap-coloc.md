# Outline ∩ Page Overlap + Container Co-location Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add fixtures for the two UNVERIFIED scenarios in flpdf-vvjr.4 (outline object overlapping a page-reachable object; even-split container co-locating an outline object with a multi-page-shared object) and achieve byte-parity with qpdf 11.9.0.

**Architecture:** Verify-by-oracle flow: build fixture → run qpdf 11.9.0 → diff against flpdf output → fix any divergence. No code is written before the oracle reveals whether a divergence exists. Two fixtures are needed: (A) a PDF where a shared font is reachable from both `/Outlines` and a page; (B) a PDF where the even-split places an outline object and a multi-page-shared object in the same ObjStm container. Both are exercised by the existing `qpdf-zlib-compat`–gated byte-parity test infrastructure.

**Tech Stack:** Rust (flpdf), Python (fixture generators), qpdf 11.9.0 CLI, `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`, `tests/golden/regenerate.sh`, `ci.yml`

---

## Background

Working directory: `.worktrees/flpdf-vvjr4` (branch `flpdf-vvjr4`).

Existing tests and structure to mirror:
- Fixture PDF: `tests/fixtures/compat/<stem>.pdf`
- Golden output: `tests/golden/references/<stem>/linearize-objstm.pdf`
- Byte-parity test: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs` (`assert_strict` / `assert_structural`)
- CI gate: `ci.yml` lines ~154 (byte tests, `qpdf-zlib-compat` feature)
- Generation script: `tests/golden/regenerate.sh`

Key files to understand:
- `crates/flpdf/src/linearization/plan.rs` — `outlines_set`, `route_objstm_containers`, `part8_container_nums`, `canonical_shared_hints`, `from_pdf`
- `docs/plans/tools/gen_outlines_gap.py` — existing fixture generator (S shared fonts, K outline items)

---

## Task 1: Fixture (A) — outline ∩ page overlap

**Scenario:** A shared font `F1` is referenced from BOTH an outline item (via a non-standard `/Extra` key) AND both pages' `/Resources`. qpdf's strict precedence (`in_outlines > in_first_page`) categorizes `F1` as `in_outlines`, removing it from the first-page section. flpdf may still put `F1` in `part3_objects` (first-page-shared). This task creates the fixture and oracle.

**Files:**
- Create: `docs/plans/tools/gen_outlines_shared_page.py`
- Create: `tests/fixtures/compat/objstm-lin-outlines-shared-page-80-80.pdf`
- Create: `tests/golden/references/objstm-lin-outlines-shared-page-80-80/linearize-objstm.pdf`

### Step 1: Write the fixture generator

Create `docs/plans/tools/gen_outlines_shared_page.py`:

```python
import sys

# >cap fixture exercising the outline∩page object-overlap scenario (flpdf-vvjr.4 part A).
#
# Same structure as gen_outlines_gap.py (S shared fonts between two pages, K outline
# items reachable only from /Outlines) but outline item 1 gains an extra key:
#   /Extra <shared_nums[0]> 0 R
# making shared_nums[0] (font F1) reachable from BOTH /Outlines (via closure) AND the
# two pages' /Resources.  qpdf categorizes F1 as in_outlines (higher precedence than
# in_first_page), removing it from the first-page shared set; flpdf must do the same.
#
# DFS order (getCompressibleObjGens):
#   Catalog, Outlines, item0_with_extra, F1, items(K-1), Pages, Page0, F2..FS, Page1
S = int(sys.argv[1]) if len(sys.argv) > 1 else 80
K = int(sys.argv[2]) if len(sys.argv) > 2 else 80
use_outlines = len(sys.argv) > 3 and sys.argv[3] == "--use-outlines"

catalog, pages, page0, page1, outlines = 1, 2, 3, 4, 5
o0 = 6
item_nums = list(range(o0, o0 + K))
shared0 = o0 + K
shared_nums = list(range(shared0, shared0 + S))
c0 = shared0 + S
c1 = c0 + 1

objs = {}
if use_outlines:
    objs[catalog] = b"<< /Type /Catalog /PageMode /UseOutlines /Outlines %d 0 R /Pages %d 0 R >>" % (outlines, pages)
else:
    objs[catalog] = b"<< /Type /Catalog /Outlines %d 0 R /Pages %d 0 R >>" % (outlines, pages)
objs[pages] = b"<< /Type /Pages /Count 2 /Kids [ %d 0 R %d 0 R ] >>" % (page0, page1)
objs[outlines] = b"<< /Type /Outlines /First %d 0 R /Last %d 0 R /Count %d >>" % (
    item_nums[0],
    item_nums[-1],
    K,
)
for i, n in enumerate(item_nums):
    entry = b"<< /Title (Item%d) /Parent %d 0 R" % (i + 1, outlines)
    if i > 0:
        entry += b" /Prev %d 0 R" % item_nums[i - 1]
    if i < K - 1:
        entry += b" /Next %d 0 R" % item_nums[i + 1]
    # Item 0 has an extra key pointing to F1 (shared_nums[0]).
    # This makes F1 reachable from /Outlines, creating the overlap with pages.
    if i == 0 and S > 0:
        entry += b" /Extra %d 0 R" % shared_nums[0]
    entry += b" >>"
    objs[n] = entry

shared_res = b" ".join(b"/S%d %d 0 R" % (i + 1, n) for i, n in enumerate(shared_nums))
res = b"<< /Font << %s >> >>" % shared_res
objs[page0] = b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>" % (pages, res, c0)
objs[page1] = b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>" % (pages, res, c1)
for i, n in enumerate(shared_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /S%d /Mark %d >>" % (i + 1, n)
for cnum, label in ((c0, b"Page0"), (c1, b"Page1")):
    stream = b"BT /S1 12 Tf 72 720 Td (%s) Tj ET" % label
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

### Step 2: Generate the fixture PDF

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-vvjr4
python3 docs/plans/tools/gen_outlines_shared_page.py 80 80 \
    > tests/fixtures/compat/objstm-lin-outlines-shared-page-80-80.pdf
```

### Step 3: Generate qpdf golden and verify qpdf accepts it

```bash
mkdir -p tests/golden/references/objstm-lin-outlines-shared-page-80-80
qpdf --linearize --object-streams=generate --deterministic-id --warning-exit-0 \
    tests/fixtures/compat/objstm-lin-outlines-shared-page-80-80.pdf \
    tests/golden/references/objstm-lin-outlines-shared-page-80-80/linearize-objstm.pdf
qpdf --check tests/golden/references/objstm-lin-outlines-shared-page-80-80/linearize-objstm.pdf
echo "exit $?"
```

Expected: exit 0 (clean linearization).

### Step 4: Inspect qpdf output to verify scenario A is triggered

```bash
# Check that F1 (shared font 1) is in the SECOND half (part 9 / outline section)
# NOT in the first-page shared section
qpdf --show-linearization \
    tests/golden/references/objstm-lin-outlines-shared-page-80-80/linearize-objstm.pdf \
    2>/dev/null
```

Also check the QDF to confirm F1's container:
```bash
qpdf --qdf --object-streams=preserve \
    tests/golden/references/objstm-lin-outlines-shared-page-80-80/linearize-objstm.pdf - \
    2>/dev/null | grep -n "^%% Object stream.*F1\|ObjStm\|/N " | head -30
```

**Expected result:** The shared-object hint table should list ONE FEWER shared entry compared to the plain `outlines-80-80` golden (since F1 is now in the outline section, not the first-page shared section). If qpdf still lists F1 as a shared object (i.e., the scenario doesn't actually change the shared hints), record that and proceed.

### Step 5: Commit the fixture generator and fixture

```bash
git add docs/plans/tools/gen_outlines_shared_page.py \
        tests/fixtures/compat/objstm-lin-outlines-shared-page-80-80.pdf
git commit -m "test(vvjr.4): add objstm-lin-outlines-shared-page-80-80 fixture (outline∩page overlap)"
```

---

## Task 2: Fixture (B) — container co-location

**Scenario:** With S=200 shared fonts and K=20 outline items, the even-split (100-object batches) places the outline objects and some shared fonts in the same ObjStm container. `route_objstm_containers` then routes the whole container to `Rest` (outline precedence, no `/UseOutlines`). The bug hypothesis: `canonical_shared_hints`/`part8_container_nums` may still classify this container as a Part-8 shared entry.

**Files:**
- Create: `tests/fixtures/compat/objstm-lin-outlines-coloc-200-20.pdf`
- Create: `tests/golden/references/objstm-lin-outlines-coloc-200-20/linearize-objstm.pdf`

### Step 1: Generate the co-location fixture

Use the existing `gen_outlines_gap.py` with S=200, K=20 (no changes to the generator):

```bash
python3 docs/plans/tools/gen_outlines_gap.py 200 20 \
    > tests/fixtures/compat/objstm-lin-outlines-coloc-200-20.pdf
```

### Step 2: Verify co-location occurs in the SOURCE PDF DFS order

Inspect the raw DFS traversal to confirm outline items and shared fonts are adjacent:

```bash
python3 - <<'EOF'
import sys
data = open('tests/fixtures/compat/objstm-lin-outlines-coloc-200-20.pdf', 'rb').read()
import re
objs = {}
for m in re.finditer(rb'(\d+) 0 obj\n(.*?)\nendobj', data, re.DOTALL):
    num = int(m.group(1))
    body = m.group(2)[:80].decode(errors='replace')
    objs[num] = body
# Show first few and last few objects to understand layout
for num in sorted(objs)[:10]:
    print(f"obj {num}: {objs[num][:60]}")
EOF
```

### Step 3: Generate qpdf golden and verify

```bash
mkdir -p tests/golden/references/objstm-lin-outlines-coloc-200-20
qpdf --linearize --object-streams=generate --deterministic-id --warning-exit-0 \
    tests/fixtures/compat/objstm-lin-outlines-coloc-200-20.pdf \
    tests/golden/references/objstm-lin-outlines-coloc-200-20/linearize-objstm.pdf
qpdf --check tests/golden/references/objstm-lin-outlines-coloc-200-20/linearize-objstm.pdf
echo "exit $?"
```

### Step 4: Inspect qpdf output to verify co-location occurs

```bash
# Show the ObjStm containers and verify one contains BOTH outline items AND shared fonts
qpdf --qdf --object-streams=preserve \
    tests/golden/references/objstm-lin-outlines-coloc-200-20/linearize-objstm.pdf - \
    2>/dev/null | grep "^%% Object stream" | sort -t: -k2 -n | head -40
```

**Expected:** At least one ObjStm container has members whose original IDs span both the outline section (items obj 6-25) AND the shared fonts section (obj 26-225). If NO container mixes them (i.e., the even split just happens to separate them), record this and adjust K and S values until co-location is confirmed.

**If co-location is NOT achieved with K=20, S=200:**
Adjust: compute new K and S such that `ceil((1+1+K+1+1+S)/ceil((1+1+K+1+1+S)/100))` puts at least one shared font in the same batch as outline items. Try K=5, S=100 or K=1, S=100.

The formula: for n compressible objects, n_per = ceil(n / ceil(n/100)). Items fill positions [1 (outlines root) + K items + 1 (catalog) + 1 (pages) + 1 (page0)] = K+4 positions. If K+4 < n_per, co-location occurs.

### Step 5: Inspect shared-object hint table for Part-8 classification

```bash
qpdf --show-linearization \
    tests/golden/references/objstm-lin-outlines-coloc-200-20/linearize-objstm.pdf \
    2>/dev/null | grep -A5 "Shared Objects"
```

**Expected:** The number of shared objects should match 2 or 3 ObjStm containers (for the fonts NOT in the outline-collocated container). The outline-collocated container should NOT appear in the shared hints.

### Step 6: Commit fixture

```bash
git add tests/fixtures/compat/objstm-lin-outlines-coloc-200-20.pdf
git commit -m "test(vvjr.4): add objstm-lin-outlines-coloc-200-20 fixture (even-split co-location)"
```

---

## Task 3: Run flpdf against goldens — identify divergences

**Files:**
- Modify: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

### Step 1: Add structural tests (no qpdf-zlib-compat needed)

Append to `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`:

```rust
// outlines-shared-page-80-80 (flpdf-vvjr.4 part A): an outline item's /Extra key
// makes F1 reachable from /Outlines; F1 is also shared by both pages.  qpdf
// categorizes F1 as in_outlines (precedence over in_first_page), so F1 leaves the
// first-page section.  flpdf must do the same.
#[test]
fn outlines_shared_page_objstm_structurally_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-outlines-shared-page-80-80.pdf",
        "objstm-lin-outlines-shared-page-80-80",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn outlines_shared_page_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-outlines-shared-page-80-80.pdf",
        "objstm-lin-outlines-shared-page-80-80",
    );
}

// outlines-coloc-200-20 (flpdf-vvjr.4 part B): even-split places outline items and
// shared fonts in the same ObjStm container; outline precedence routes the container
// to Rest (part 9).  The shared fonts in that container must NOT appear in the shared-
// object hint table as a Part-8 entry.
#[test]
fn outlines_coloc_objstm_structurally_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-outlines-coloc-200-20.pdf",
        "objstm-lin-outlines-coloc-200-20",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn outlines_coloc_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-outlines-coloc-200-20.pdf",
        "objstm-lin-outlines-coloc-200-20",
    );
}
```

### Step 2: Run structural tests (no feature flag needed)

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-vvjr4
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    outlines_shared_page_objstm_structurally_identical_to_qpdf -- --nocapture 2>&1 | tail -30
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    outlines_coloc_objstm_structurally_identical_to_qpdf -- --nocapture 2>&1 | tail -30
```

If these PASS: the structural layout matches qpdf. Proceed to byte tests.
If these FAIL: record the diff and investigate (likely a bug in `from_pdf` or container routing).

### Step 3: Run byte-parity tests under qpdf-zlib-compat

```bash
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    --features qpdf-zlib-compat \
    outlines_shared_page_objstm_byte_identical_to_qpdf -- --nocapture 2>&1 | tail -30
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    --features qpdf-zlib-compat \
    outlines_coloc_objstm_byte_identical_to_qpdf -- --nocapture 2>&1 | tail -30
```

**Record the results:** PASS or FAIL with diff location. Do NOT write fixes until you have this evidence.

### Step 4: Commit the new test stubs

```bash
git add crates/flpdf/tests/cmp_linearize_objstm_tests.rs
git commit -m "test(vvjr.4): add structural+byte tests for shared-page-80-80 and coloc-200-20"
```

---

## Task 4: Fix divergence for fixture (A) — remove outlines_set objects from page closures

**Condition:** Only implement this task if the structural or byte test for fixture (A) FAILS.

**Root cause:** `LinearizationPlan::from_pdf` builds `part2_objects` and `part3_objects` from page closures alone, without applying qpdf's `in_outlines > in_first_page` precedence. An object in BOTH `outlines_set` AND the first-page closure ends up in `part3_objects`, but qpdf places it in the outline section (part9/part6).

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs` (Steps 5–8 in `from_pdf`)

### Step 1: Understand the fix location

The fix is in `from_pdf`. After Step 7 (which builds `part4_provisional`) and around Step 8 (outline routing), `all_outline_refs` is computed via `outlines_set(pdf)`. We need to remove any object that is in `all_outline_refs` from `part2_objects`, `part3_objects`, `part4_other_pages_private`, and `part4_other_pages_shared` — because qpdf's `in_outlines` precedence means they belong in the outline section, not in page-based sections.

Add AFTER line `let all_outline_refs: BTreeSet<ObjectRef> = outlines_set(pdf)?;` (around line 808):

```rust
// qpdf precedence: in_outlines > in_first_page > other_page.
// Objects reachable from /Outlines must be removed from the page-based
// part2/part3/part4 sets and left for the outline extraction below.
// (QPDF_linearization.cc:1120-1126)
if !all_outline_refs.is_empty() {
    part2_objects.retain(|r| !all_outline_refs.contains(r));
    part3_objects.retain(|r| !all_outline_refs.contains(r));
    part4_other_pages_private.retain(|r| !all_outline_refs.contains(r));
    part4_other_pages_shared.retain(|r| !all_outline_refs.contains(r));
    part4_rest.retain(|r| !all_outline_refs.contains(r));
}
```

**Important**: Also update the page-0 hint count at line ~663 to NOT count outline objects. After the retain operations, `page_hints[0].object_count` should use the filtered counts. Review whether `page_hints[0].object_count` is recomputed later (it is, in step 8 for `outlines_in_first_page`). Since outline objects go into `part6_outline_objects` (UseOutlines) and those are added to the page-0 count in step 8, the non-UseOutlines case just sees fewer part2/part3 objects → correct.

Also update `shared_hints` construction: since `part2_objects` and `part3_objects` are now smaller, the `shared_hints` built from them will be correct. Objects moved out of part3 are no longer shared with the first page; they belong to the outline section.

**Note on `shared_page_indices`:** The `shared_page_indices` map (used to build `part3_objects`) may still have entries for the moved objects. This is OK: the `retain` on `part3_objects` removes the moved objects, and `shared_page_indices` is only used for the `part3_entries` construction in `shared_hints` — after the retain, moved objects no longer appear in `part3_objects`, so they won't be iterated.

### Step 2: Check whether `page_hints[0].object_count` needs adjustment

Search for where `page_hints[0].object_count` is assigned (around line 663):
```rust
page_hints[0].object_count = (page0_private.len() + part3_objects.len()) as u32;
```

`page0_private` is derived from `part2_objects`. After the retain, both are correctly smaller. The page-0 count is correct WITHOUT explicit adjustment (the removed outline objects are accounted for in `part6_outline_objects` which is added to `page_hints[0].object_count` in step 8 for UseOutlines only — the non-UseOutlines path leaves page-0 count unchanged for those objects, which is correct per qpdf).

### Step 3: Run the tests

```bash
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    outlines_shared_page_objstm_structurally_identical_to_qpdf -- --nocapture 2>&1
```

Also run the regression guard:
```bash
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    outlines_objstm -- --nocapture 2>&1
```

Iterate until both pass.

### Step 4: Add a unit test for the fix

In `crates/flpdf/tests/cmp_linearize_objstm_tests.rs` or in `plan.rs` inline tests, add:
```rust
// Verify that when an outline subtree object is also reachable from a page,
// from_pdf removes it from part3_objects (qpdf in_outlines > in_first_page precedence).
```
Exercise via the structural test above (the whole-document comparison is sufficient; no separate unit test required unless coverage demands it).

### Step 5: Commit

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "fix(vvjr.4-A): remove outlines_set objects from page-based parts (in_outlines precedence)"
```

---

## Task 5: Fix divergence for fixture (B) — exclude outline-routed containers from Part-8 hints

**Condition:** Only implement this task if the structural or byte test for fixture (B) FAILS.

**Root cause:** `part8_container_nums` classifies a container as Part-8 if it has a shared member (in `part4_other_pages_shared`) and no first-page member, regardless of whether the container was routed to `Rest` (outline precedence). `canonical_shared_hints` then incorrectly includes that container as a Part-8 shared hint entry, while qpdf (which routes it to Part-9) does not include it in the shared hints.

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs` (in `canonical_shared_hints` and/or `part8_container_nums`)

### Step 1: Determine what qpdf actually emits

Before writing the fix, examine the golden output:
```bash
qpdf --show-linearization \
    tests/golden/references/objstm-lin-outlines-coloc-200-20/linearize-objstm.pdf 2>/dev/null \
    | grep -A 20 "Shared Objects Hint Table"
```

Count the number of shared objects. If it matches `ceil(180/n_per)` containers (the non-co-located shared fonts only), the co-located container is NOT in the shared section. That confirms the fix direction.

### Step 2: Understand the fix scope

In `canonical_shared_hints`, when processing a part4_other_pages_shared entry whose container also contains an outline member, the container should be treated as if its shared members are NOT in the shared section (they're physically in a Part-9 ObjStm). The fix: pass the outline object set to `canonical_shared_hints` and skip container folding if the container has any outline member.

Since `canonical_shared_hints` doesn't currently receive routing information, one approach is:
- Pass the `outlines_set_all: &BTreeSet<ObjectRef>` (all outline objects, not just first-page ones) to `canonical_shared_hints` and `part8_container_nums`.
- In the shared-hint folding loop, before adding a container to `container_pos`, check if the container has any outline member via `member_to_container` inspection. If it does, skip folding (output the plain object entry instead, or skip it per qpdf behavior).

**Alternative (simpler):** In `part8_container_nums`, add a parameter `outline_members: &BTreeSet<ObjectRef>` and exclude any container whose members intersect `outline_members`.

Pick the approach that matches qpdf's actual output — the golden tells you whether the mixed container appears AT ALL in the shared hints (as a plain entry or container entry), or is completely absent.

### Step 3: Implement the fix

Based on the golden inspection in Step 1, implement the minimal change. If qpdf omits the container entirely from shared hints:

In `part8_container_nums`, add exclusion:
```rust
pub(crate) fn part8_container_nums(
    &self,
    member_to_container: &BTreeMap<ObjectRef, (u32, u32)>,
    outline_all: &BTreeSet<ObjectRef>,  // <-- new param
) -> BTreeSet<u32> {
    // ... existing code ...
    // After building has_first_page_member, has_shared_member:
    let mut has_outline_member: BTreeSet<u32> = BTreeSet::new();
    for (member, &(cnum, _)) in member_to_container {
        if outline_all.contains(member) {
            has_outline_member.insert(cnum);
        }
    }
    all_containers
        .into_iter()
        .filter(|cnum| {
            !has_first_page_member.contains(cnum)
                && !has_outline_member.contains(cnum)  // <-- new exclusion
                && (has_shared_member.contains(cnum)
                    || container_pages.get(cnum).is_some_and(|p| p.len() >= 2))
        })
        .collect()
}
```

Similarly, in `canonical_shared_hints`, when folding shared_hints entries, if the entry's container has an outline member and the container should be treated as Part-9 (not Part-8), skip or decontainerize.

**Note:** The exact fix depends on what qpdf produces (golden inspection in Step 1). If the golden shows the mixed container IS in the shared hints as a plain entry, the fix is different (keep it but don't containerize). Implement based on the empirical evidence.

### Step 4: Update callers of `part8_container_nums`

Search for all callers: `canonical_shared_hints`. Pass the `outlines_set` (use `self.part9_outline_objects` and `self.part6_outline_objects` or recompute `all_outline_refs`).

### Step 5: Run tests

```bash
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    outlines_coloc_objstm_structurally_identical_to_qpdf -- --nocapture 2>&1
# Regression guards (run each filter separately):
cargo test -p flpdf --test cmp_linearize_objstm_tests outlines_objstm -- --nocapture 2>&1
cargo test -p flpdf --test cmp_linearize_objstm_tests outlines_multi_container -- --nocapture 2>&1
cargo test -p flpdf --test cmp_linearize_objstm_tests useoutlines_objstm -- --nocapture 2>&1
```

### Step 6: Commit

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "fix(vvjr.4-B): exclude outline-routed containers from Part-8 shared hints"
```

---

## Task 6: Add byte-parity tests under qpdf-zlib-compat

### Step 1: Run full byte tests

```bash
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    --features qpdf-zlib-compat \
    outlines_shared_page_objstm_byte_identical_to_qpdf -- --nocapture 2>&1
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    --features qpdf-zlib-compat \
    outlines_coloc_objstm_byte_identical_to_qpdf -- --nocapture 2>&1
```

Expected: both PASS. If either FAILs, investigate further.

### Step 2: Run all regression guards under qpdf-zlib-compat

```bash
cargo test -p flpdf --test cmp_linearize_objstm_tests \
    --features qpdf-zlib-compat -- --nocapture 2>&1 | tail -10
```

All tests must pass.

---

## Task 7: Wire up to regenerate.sh and ci.yml

### Step 1: Update regenerate.sh

Add the two new stems to the linearized generate-mode block in `tests/golden/regenerate.sh`. Find the existing loop (around line 383) and add:

```bash
for stem in ... objstm-lin-outlines-shared-page-80-80 objstm-lin-outlines-coloc-200-20; do
```

Also add the generator entries to the fixture comments block (before the loop). Search for `[objstm-lin-useoutlines-80-80]="gen_outlines_gap.py 80 80 --use-outlines"` and add:

```bash
[objstm-lin-outlines-shared-page-80-80]="gen_outlines_shared_page.py 80 80"
[objstm-lin-outlines-coloc-200-20]="gen_outlines_gap.py 200 20"
```

### Step 2: Update ci.yml

Find the byte-identical test block in `ci.yml` (around line 154, the `qpdf-zlib-compat` block):

```yaml
- run: cargo test -p flpdf --test cmp_linearize_objstm_tests --features qpdf-zlib-compat
        outlines_shared_page_objstm_byte_identical_to_qpdf
        outlines_coloc_objstm_byte_identical_to_qpdf
```

Add the two new test names to the explicit test list. Exact format: match the existing lines. The CI block lists test names explicitly (per the memory: `flpdf-ci-bytes-identical-explicit-test-list` — byte tests must be explicitly listed in ci.yml).

### Step 3: Run patch-coverage check

```bash
# Ensure work is committed first
git status  # must be clean
scripts/patch-coverage.sh
```

Target: 100% coverage on changed `flpdf` lines. Add `// cov:ignore: <reason>` for any truly untestable branch (e.g., indirect /PageMode reference in a new code path).

### Step 4: Commit

```bash
git add tests/golden/regenerate.sh .github/workflows/ci.yml
git commit -m "ci(vvjr.4): wire outlines-shared-page-80-80 and outlines-coloc-200-20 to regenerate.sh and ci.yml"
```

---

## Task 8: Session completion

### Step 1: Build check

```bash
cargo check --workspace 2>&1 | tail -10
```

### Step 2: Run full test suite

```bash
cargo test --workspace -q 2>&1 | tail -20
```

All tests pass.

### Step 3: Run clippy

```bash
cargo clippy --workspace -- -D warnings 2>&1 | tail -20
```

No warnings.

### Step 4: Run cargo fmt

```bash
cargo fmt --check
# If dirty:
cargo fmt
git add -u && git commit -m "style: cargo fmt"
```

### Step 5: Push

```bash
git push -u origin flpdf-vvjr4
```

### Step 6: Close the beads issue

After PR is created and merged: `bd close flpdf-vvjr.4`.

---

## Quick Reference

| Fixture stem | Generator | Scenario |
|---|---|---|
| `objstm-lin-outlines-shared-page-80-80` | `gen_outlines_shared_page.py 80 80` | (A) F1 in outlines∩page |
| `objstm-lin-outlines-coloc-200-20` | `gen_outlines_gap.py 200 20` | (B) even-split co-location |

Key functions to modify if bugs found:
- Bug A: `LinearizationPlan::from_pdf` in `plan.rs` (Steps 5–7) — subtract `outlines_set` from page-based parts
- Bug B: `part8_container_nums` + `canonical_shared_hints` in `plan.rs` — exclude outline-routed containers
