# flpdf-ihb.4: Linearized PRESERVE >cap — match qpdf source-grouping (not greedy chunks) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make flpdf's `rewrite --linearize --object-streams=preserve` first-half ObjStm
packing byte-identical to qpdf 11.9.0 when the first-page-shared member set exceeds the
batch cap (>=100), by preserving the source ObjStm container grouping instead of greedily
re-chunking via `members.chunks(cap)`.

**Architecture:** qpdf's `preserveObjectStreams()` copies the source object→stream assignment
verbatim and only erases /Page dicts + /Catalog (the linearization-forbidden objects); it does
NOT even-split or re-chunk. flpdf's `objstm_batches_preserve` already reconstructs source
grouping, but `canonicalise_first_half_batch` then flattens all first-half batches, appends
/Pages+/Info, and re-chunks with `members.chunks(cap)` — destroying the grouping (200 members
→ 100+100 instead of qpdf's source-derived 66+66+68). Fix in two parts: **(A)** route
`pages_tree_ref`/`info_ref` into their *source container's* first-half bucket inside
`objstm_batches_preserve` (mirroring qpdf "keep the source member in its container"), and
**(B)** drop the flatten + `chunks(cap)` re-chunk from `canonicalise_first_half_batch` so the
per-container grouping survives.

**Tech Stack:** Rust; `crates/flpdf/src/linearization/plan.rs`; cmp golden tests gated on
`qpdf-zlib-compat`; goldens under `tests/golden/references/`; fixtures under
`tests/fixtures/compat/`; `tests/golden/regenerate.sh`; qpdf 11.9.0 oracle (local source at
`/tmp/qpdf-1190`).

**Oracle evidence (spike, in the beads issue design field):** source ObjStm containers
68/67/68; erase Catalog(obj2)+Page1(obj4) from c1, Page2(obj71) from c70 → 66/66/68 = qpdf
preserve output (EXACT). An even-split of 200→3 would be 67/67/66 (≠ 66/66/68), so reusing the
generate even-split is wrong. All 3 qpdf containers are first-half (offsets < /E). flpdf
pre-canonicalise part3_batches measured = [65,66,68] (fonts only; /Pages obj3 dropped to plain).

---

## Task 1: Add the ObjStm-bearing >cap preserve fixture + qpdf golden

**Files:**
- Modify: `tests/golden/regenerate.sh` (add a bearing fixture + a preserve golden)
- Create (via regenerate.sh): `tests/fixtures/compat/objstm-lin-cap-boundary-199-bearing.pdf`
- Create (via regenerate.sh): `tests/golden/references/objstm-lin-cap-boundary-199-bearing/linearize-objstm-preserve.pdf`

**Why a new fixture:** the existing `objstm-lin-cap-boundary-199.pdf` is the *raw*
`gen_shared_fonts.py 199` output with **no source ObjStms** (preserve on it yields an empty
plan). The preserve path needs an ObjStm-bearing input: `gen_shared_fonts.py 199` piped through
`qpdf --object-streams=generate` (3 source containers 68/67/68).

**Step 1: Add fixture derivation to regenerate.sh**

In Phase 1 (fixture derivation, near the `G6HB2_FIX` block ~line 244-252), after the bearing
fixture's base is available, derive the ObjStm-bearing variant deterministically:

```bash
if [[ ! -f "$FIX/objstm-lin-cap-boundary-199-bearing.pdf" ]]; then
    echo "Generating objstm-lin-cap-boundary-199-bearing.pdf ..."
    python3 "$ROOT/docs/plans/tools/gen_shared_fonts.py" 199 \
      | qpdf --object-streams=generate --deterministic-id --warning-exit-0 - \
        "$FIX/objstm-lin-cap-boundary-199-bearing.pdf"
else
    echo "Skipping objstm-lin-cap-boundary-199-bearing.pdf (already exists)"
fi
```

(Match the exact flag/quoting style used by the surrounding qpdf invocations; use
`--deterministic-id` so the committed fixture is byte-stable.)

**Step 2: Add the preserve golden to regenerate.sh (Phase 2)**

Mirror the existing generate golden loop (~line 397-410), but for preserve mode and the bearing
fixture, writing a distinctly-named golden so it never collides with the generate goldens:

```bash
mkdir -p "$REF/objstm-lin-cap-boundary-199-bearing"
qpdf --linearize --object-streams=preserve --deterministic-id --warning-exit-0 \
    "$FIX/objstm-lin-cap-boundary-199-bearing.pdf" \
    "$REF/objstm-lin-cap-boundary-199-bearing/linearize-objstm-preserve.pdf"
```

**Step 3: Run regenerate.sh and verify the golden distribution**

Run: `bash tests/golden/regenerate.sh`
Then verify the golden's ObjStm /N distribution is `66/66/68` and check-linearization clean:

```bash
qpdf --check-linearization tests/golden/references/objstm-lin-cap-boundary-199-bearing/linearize-objstm-preserve.pdf
```
Expected: `no linearization errors`, and the 3 ObjStm /N are 66, 66, 68 (sum 200).

**Step 4: Commit the fixture + golden + script change**

```bash
git add tests/golden/regenerate.sh tests/fixtures/compat/objstm-lin-cap-boundary-199-bearing.pdf \
        tests/golden/references/objstm-lin-cap-boundary-199-bearing/linearize-objstm-preserve.pdf
git commit -m "test(linearization): add ObjStm-bearing >cap preserve fixture + qpdf golden (flpdf-ihb.4)"
```

---

## Task 2: Add the failing preserve byte-parity test

**Files:**
- Modify: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

**Step 1: Add a preserve-mode helper next to `flpdf_linearized_objstm`**

The existing helper hard-codes `ObjectStreamMode::Generate`. Add a sibling that uses Preserve
and a preserve golden path (`linearize-objstm-preserve.pdf`):

```rust
fn flpdf_linearized_objstm_preserve(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let f1 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(f1)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();
    let renumber = RenumberMap::from_plan(&plan);
    let f2 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf2 = Pdf::open(std::io::BufReader::new(f2)).unwrap();
    let mut opts = WriteOptions::default();
    opts.object_streams = ObjectStreamMode::Preserve;
    opts.deterministic_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    doc.back_patch().unwrap();
    doc.bytes
}

fn golden_preserve(stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join("linearize-objstm-preserve.pdf");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}
```

**Step 2: Add the structural (mask_id1) test**

```rust
// flpdf-ihb.4: PRESERVE >cap must keep the source ObjStm grouping (qpdf
// preserveObjectStreams: source 68/67/68 minus erased Catalog/Page dicts =>
// 66/66/68), NOT greedy chunks(cap) (100+100).
#[test]
fn cap_boundary_199_bearing_preserve_structurally_byte_identical_to_qpdf() {
    let fixture = "objstm-lin-cap-boundary-199-bearing.pdf";
    let stem = "objstm-lin-cap-boundary-199-bearing";
    let actual = flpdf_linearized_objstm_preserve(fixture);
    let expected = golden_preserve(stem);
    report(fixture, &mask_id1(&actual), &mask_id1(&expected), "preserve structural");
}
```

**Step 3: Add the strict test (likely `#[ignore]` like the other strict cases)**

Mirror the existing strict pattern — full byte identity including `/ID[1]`. If the strict
generate tests are `#[ignore]`d pending pass-1 xref `/ID` reconstruction, mark this one the same
way and note why in a comment.

**Step 4: Run the structural test to verify it FAILS**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests cap_boundary_199_bearing_preserve_structurally -- --nocapture`
Expected: FAIL — flpdf emits 100+100 (greedy chunks(cap)); golden is 66+66+68. The `report`
panic shows the first diverging byte.

**Step 5: Commit the failing test**

```bash
git add crates/flpdf/tests/cmp_linearize_objstm_tests.rs
git commit -m "test(linearization): pin failing preserve >cap byte-parity (flpdf-ihb.4)"
```

---

## Task 3: Implement (A) — route /Pages + /Info into their source container's first-half bucket

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs` (`objstm_batches_preserve`, ~1788-1842)

**Step 1: In the per-container classification loop, keep `pages_tree_ref`/`info_ref` as
first-half members of their source container**

Currently a member that is neither in `part3_set` nor `part4_set` falls into the
"else: leave as plain indirect" arm. Add a branch BEFORE that else so that, when the member is
`self.pages_tree_ref` or `self.info_ref` (and eligible — the loop already checked
`is_eligible_for_objstm` and the part2/length exclusions), it is pushed into `p3_eligible`
(the first-half bucket for THIS source container), matching qpdf keeping the source member in
its container. Add a `//` comment citing qpdf `preserveObjectStreams` + the linearized
page/catalog erase (QPDFWriter.cc:1939, 2141-2161). Keep DRY: compute the
`pages_tree_ref`/`info_ref` match once.

Note: `pages_tree_ref`/`info_ref` live in `part4_rest` (renumber promotes them); batching them
into part3 keeps a valid RenumberMap slot (Invariant 9 in the existing unit test checks
`part3_set || part4_set`, which `part4_rest` satisfies). This is the same set/batch split the
current `canonicalise_first_half_batch` fold already relies on — verified clean.

**Step 2: Build and run the preserve unit tests**

Run: `cargo test -p flpdf objstm_batches_preserve`
Expected: still PASS — `objstm_batches_preserve_source_objstm_grouping_and_part_split`
Invariant 4 (Pages folded into part3) now holds via (A) instead of canonicalise; the cap-split
tests are unaffected (per-container `chunks(cap)` unchanged).

**Step 3: Commit**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "fix(linearization): preserve routes /Pages+/Info to source container first-half (flpdf-ihb.4)"
```

---

## Task 4: Implement (B) — stop flatten + chunks(cap) in canonicalise_first_half_batch

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs` (`canonicalise_first_half_batch` ~1539-1633;
  its call site/doc ~1442-1517)

**Step 1: Remove the grouping-destroying re-chunk**

`canonicalise_first_half_batch` currently: collects `first_half_extra` (/Info, /Pages-tree),
excludes Catalog/Pages/Info from Part 4, then flattens `part3_batches`, appends the extras,
sorts by number, and `members.chunks(cap)` → over-writes `part3_batches`. With (A) already
placing /Pages+/Info in the correct source-container batch, this whole flatten+append+chunk
step is both redundant and wrong (it greedily merges containers). Replace it so the per-source-
container `part3_batches` grouping is preserved:
- Keep the Part-4 exclusion of `/Catalog`/`/Pages`/`/Info` (Catalog drop already happens at the
  call site; /Pages+/Info no longer reach Part 4 after (A), so this becomes a safety no-op —
  retain or fold into a short guard, whichever keeps the code minimal and covered).
- Delete the `first_half_extra` collection, the flatten, the `sort_unstable_by_key`, and the
  `members.chunks(cap)` assignment.

If, after (A), `canonicalise_first_half_batch` no longer does anything beyond the Part-4
exclusion already covered at the call site, prefer **removing the function and its call**
entirely (and updating the surrounding doc-comment at ~1442-1517 + the `cov:ignore` block at
1503-1515 that guards its `?`). Decide by what leaves the smallest, fully-covered surface.
Update the Preserve-arm doc comment (1484-1499) to state the source-grouping behaviour
(no /Pages+/Info fold, no re-chunk) and cite qpdf preserveObjectStreams.

**Step 2: Run the structural preserve byte test — verify it now PASSES**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests cap_boundary_199_bearing_preserve_structurally`
Expected: PASS (flpdf now emits 66/66/68, byte-identical to the qpdf golden after id masking).

**Step 3: Run the full linearization regression set**

Run:
```bash
cargo test -p flpdf --test cmp_linearize_objstm_tests --test linearize_objstm_generate_tests
cargo test -p flpdf objstm_batches_preserve
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
```
Expected: all PASS. The generate goldens (66/68/66) are untouched (generate path doesn't call
canonicalise); the existing preserve unit tests pass via (A).

**Step 4: Commit**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "fix(linearization): preserve keeps source ObjStm grouping at >cap, drop greedy re-chunk (flpdf-ihb.4)"
```

---

## Task 5: Verify check-linearization + the ihb.3 invariant, then patch-coverage

**Step 1: End-to-end check-linearization on the real CLI path**

```bash
cargo build --release -p flpdf-cli
python3 docs/plans/tools/gen_shared_fonts.py 199 | qpdf --object-streams=generate --deterministic-id - /tmp/src.pdf
target/release/flpdf rewrite --linearize --object-streams=preserve --deterministic-id /tmp/src.pdf /tmp/f.pdf
qpdf --check-linearization /tmp/f.pdf      # expect: no linearization errors
# expect ObjStm /N = 66/66/68
```
Confirm the ihb.3 invariant still holds (page0 object count == shared-hint first_page_entries):
`flpdf --show-linearization /tmp/f.pdf` page nobjects + first_shared consistent with qpdf.

**Step 2: Quality gates**

Run:
```bash
cargo fmt --all
cargo clippy -p flpdf --all-targets
cargo test -p flpdf
```
Expected: clean (fmt no diff, no clippy warnings, tests green). `cargo fmt --check` must be
clean before push (CI quality gate).

**Step 3: patch-coverage (commit first)**

Run: `scripts/patch-coverage.sh --base main`
Expected: flpdf changed lines 100% covered. Add tests or justified `// cov:ignore: <reason>`
for any genuinely untestable line (record reason in the PR description). Run WITHOUT
`qpdf-zlib-compat` (compat baseline is miniz-fixed).

**Step 4: Commit any coverage-driven test additions**

```bash
git add -A && git commit -m "test(linearization): cover preserve source-grouping edge lines (flpdf-ihb.4)"
```

---

## Known gaps / out of scope (note in PR, file follow-ups if needed)
- **Mixed source container** (a single source ObjStm holding both first-page and other-page
  members): qpdf keeps the container whole and routes it as a unit; flpdf still splits per part.
  Not exercised by this fixture (all 3 containers are first-half). Defer.
- **/Info path**: the sf-199 fixture has no `/Info` (trailer /Info absent), so (A)'s info_ref
  branch is exercised structurally only if added. If patch-coverage flags the info_ref arm,
  either extend the fixture to carry an /Info that is a source ObjStm member, or add a focused
  unit test; do not `cov:ignore` a reachable behaviour.
- **>100-member source container** (preserve should NOT cap-split, but flpdf's per-container
  `chunks(cap)` would): pre-existing, not introduced here. Defer.
