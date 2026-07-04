# flpdf-zda0: other-page object with others>0 → part9 (lc_other), not part7

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make flpdf's classic (disable/preserve) linearization route a non-first-page
object that is reached by exactly one other page **and** by a document-level `others`
reference (a Catalog non-open-document key or a trailer non-`/Root`,-`/Encrypt` key)
to qpdf **part9 (`lc_other`)** instead of part7 (`lc_other_page_private`), matching
`qpdf --linearize` byte-for-byte.

**Architecture:** qpdf categorizes such an object with `other_pages==1 && others>0`,
which fails the part7 test (`other_pages==1 && others==0 && thumbs==0`) and the part8
test (`other_pages>1`), falling through to `lc_other` / part9
(QPDF_linearization.cc:1128-1138). flpdf's `per_page_private_objects` filter keys only
on `page_reach==1` with no `others` gate, so it wrongly captures the object into part7
(and inflates that page's `object_count` hint). Fix: exclude `document_other_set`
members (flpdf's exact model of qpdf's `others`, already used for the first-page split
in flpdf-8891) from `per_page_private_objects`. The excluded object then flows through
`part4_provisional` into the part8/part9 loop and lands in `part4_rest` (part9) via the
existing `reach>=2 ? part8 : part9` branch. `part4_rest` is emitted in object-number
order (it iterates the `all_refs.sort()`-ed `part4_provisional`), matching qpdf's
number-sorted `lc_other`.

**Tech Stack:** Rust (`crates/flpdf`), qpdf 11.9.0 oracle, `qpdf-zlib-compat` feature
for byte-identical goldens, `scripts/patch-coverage.sh` gate.

**Scope:** Classic path only. The generate-mode sibling (`route_objstm_containers`) is
tracked in **flpdf-pn7h** (needs its own generate-mode byte oracle). Out of scope:
thumbs modeling (flpdf-hn1g.16); the `Info` part9-head promotion ordering (avoided here
by a fixture with no `/Info`).

---

## Task 1: Add the fixture generator + golden to regenerate.sh, generate assets

**Files:**
- Modify: `tests/golden/regenerate.sh` (fixture-derivation phase + golden section)
- Create (generated): `tests/fixtures/compat/catalog-otherpage-other-two-page.pdf`
- Create (generated): `tests/golden/references/catalog-otherpage-other-two-page/linearize.pdf`

**Step 1: Add the fixture generator to regenerate.sh (Phase 1 fixtures block, near the
`catalog-firstpage-shared-two-page` generator ~line 288).**

```bash
if [[ ! -f "$FIX/catalog-otherpage-other-two-page.pdf" ]]; then
    echo "Generating catalog-otherpage-other-two-page.pdf ..."
    # flpdf-zda0: a non-first-page object (page-2 font, obj 7) that is BOTH
    # page-2-private (other_pages==1) AND referenced by a Catalog non-open-document
    # key (/Ref2 -> obj 7, others>0). qpdf categorizes it lc_other (part9), NOT
    # lc_other_page_private (part7), because part7 requires others==0
    # (QPDF_linearization.cc:1128). No /Info in the trailer keeps part9 unambiguous
    # (only the pages tree + obj 7 are lc_other). Page 1 keeps its own private font
    # (obj 6, first-page-private) for a non-degenerate first-page section.
    python3 - "$FIX/catalog-otherpage-other-two-page.pdf" <<'PY'
import sys
objs = {
    1: b"<< /Type /Catalog /Pages 2 0 R /Ref2 7 0 R >>",
    2: b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
    3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >> /Contents 8 0 R >>",
    4: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F2 7 0 R >> >> /Contents 9 0 R >>",
    6: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Name /F1 >>",
    7: b"<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman /Name /F2 >>",
}
c1 = b"BT /F1 12 Tf 72 720 Td (P1) Tj ET"
c2 = b"BT /F2 12 Tf 72 720 Td (P2) Tj ET"
objs[8] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(c1), c1)
objs[9] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(c2), c2)
order = [1, 2, 3, 4, 6, 7, 8, 9]
out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
offs = {}
for n in order:
    offs[n] = len(out)
    out += b"%d 0 obj\n" % n + objs[n] + b"\nendobj\n"
xo = len(out)
size = 10
out += b"xref\n0 %d\n" % size + b"0000000000 65535 f \n"
for n in range(1, size):
    out += (b"%010d 00000 n \n" % offs[n]) if n in offs else b"0000000000 65535 f \n"
out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (size, xo)
open(sys.argv[1], "wb").write(out)
PY
else
    echo "Skipping catalog-otherpage-other-two-page.pdf (already exists)"
fi
```

**Step 2: Add the golden generation (golden section, near the
`catalog-firstpage-shared-two-page` golden ~line 1165).**

```bash
# --- catalog-otherpage-other-two-page: a page-2-private object (font obj 7) also
# referenced by a Catalog non-OD key (/Ref2) is lc_other (part9), not
# lc_other_page_private (part7), because part7 requires others==0 (flpdf-zda0).
# CLASSIC ONLY (generate sibling: flpdf-pn7h). ---
mkdir -p "$REF/catalog-otherpage-other-two-page"
qpdf --linearize --deterministic-id --warning-exit-0 \
    "$FIX/catalog-otherpage-other-two-page.pdf" \
    "$REF/catalog-otherpage-other-two-page/linearize.pdf"
echo "catalog-otherpage-other-two-page/linearize.pdf"
```

**Step 3: Generate the assets.**

Run: `bash tests/golden/regenerate.sh`
Expected: prints `Generating catalog-otherpage-other-two-page.pdf ...` and
`catalog-otherpage-other-two-page/linearize.pdf`, exit 0, both files created.
(Requires qpdf 11.9.0 exactly — confirmed present.)

**Step 4: Sanity-check the oracle (obj 7 is NOT part7).**

Run: `qpdf --show-linearization tests/golden/references/catalog-otherpage-other-two-page/linearize.pdf | grep -A1 'Page 1'`
Expected: `Page 1:` then `nobjects: 2` (page object + content stream only; the font is
part9, so it is NOT counted in page 1's part7 nobjects).

**Step 5: Commit.**

```bash
git add tests/golden/regenerate.sh tests/fixtures/compat/catalog-otherpage-other-two-page.pdf tests/golden/references/catalog-otherpage-other-two-page/linearize.pdf
git commit -m "test(flpdf-zda0): add catalog-otherpage-other-two-page fixture + qpdf classic golden"
```

---

## Task 2: Add the byte-identity test and confirm it FAILS (RED)

**Files:**
- Modify: `crates/flpdf/tests/cmp_linearize_tests.rs`

**Step 1: Add the test (place beside the existing `catalog-firstpage-shared` classic
tests).**

```rust
/// flpdf-zda0: a non-first-page object reached by exactly one other page AND by a
/// document-level `others` reference (Catalog `/Ref2`) is qpdf `lc_other` (part9),
/// not `lc_other_page_private` (part7). Pins the classic byte layout: the demoted
/// object gets a part9 object number (after the pages tree) and page 1's part7
/// `object_count` hint excludes it.
#[test]
fn catalog_otherpage_other_two_page_classic_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "catalog-otherpage-other-two-page.pdf",
        "catalog-otherpage-other-two-page",
    );
}
```

**Step 2: Run it to verify it FAILS with the current (buggy) code.**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests catalog_otherpage_other_two_page -- --nocapture`
Expected: FAIL — `not byte-identical to qpdf ... first diff at byte N`. This is the RED
state proving the bug (flpdf routes the font to part7, shifting object numbers + the
page-1 hint).

**Step 3: Do NOT commit yet** (RED test committed together with the fix in Task 3).

---

## Task 3: Fix `per_page_private_objects` to exclude `document_other_set` (GREEN)

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs` (per_page_private filter ~1096-1106;
  stale comment ~1219-1222)

**Step 1: Add the `others` exclusion in the `per_page_private_objects` filter.**

In the closure filter (currently ending with `page_reach.get(r).copied() == Some(1)`),
add, immediately before the `page_reach` check:

```rust
                    // qpdf routes a non-first-page object to lc_other_page_private
                    // (part7) ONLY when others==0 (QPDF_linearization.cc:1128). An
                    // object also reached by a document-level `others` reference
                    // (Catalog non-open-document key / trailer non-/Root,-/Encrypt
                    // key) is lc_other (part9) even at other_pages==1. Exclude it
                    // from the per-page-private set so it is neither placed in part7
                    // nor counted in this page's part7 object_count hint; it flows
                    // through part4_provisional into the part8/part9 loop and lands
                    // in part4_rest (part9) (flpdf-zda0).
                    if document_other_set.contains(r) {
                        return false;
                    }
```

**Step 2: Update the now-inaccurate comment in the part8/part9 loop (~1219-1222).**

Replace the `else` comment that claims `reach == 1` "shouldn't happen" with one that
acknowledges the `others>0` case:

```rust
                // reach == 0 (trailer-/document-only), or reach == 1 with others>0
                // (excluded from per_page_private above, so lc_other not part7).
                // Both are qpdf part9.
```

**Step 3: Run the byte test to verify it PASSES (GREEN).**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests catalog_otherpage_other_two_page -- --nocapture`
Expected: PASS (byte-identical).

**Step 4: Run the full classic + objstm linearize suites for regressions.**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests --test cmp_linearize_objstm_tests --test linearize_classic_tests`
Expected: all pass (no existing fixture has this pattern, so no golden re-bless needed).

**Step 5: Commit.**

```bash
git add crates/flpdf/src/linearization/plan.rs crates/flpdf/tests/cmp_linearize_tests.rs
git commit -m "fix(flpdf-zda0): other-page object with others>0 is lc_other (part9), not part7"
```

---

## Task 4: Add a plan-level unit test (partition membership)

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs` (`#[cfg(test)] mod tests`)

**Step 1: Write a unit test** that builds a `LinearizationPlan` from an in-memory PDF
matching the fixture shape (page-2 font also referenced by a Catalog non-OD key) and
asserts the font ref is in `plan.part4_rest` and NOT in `plan.part4_other_pages_private`.
Model it on the existing plan-building unit tests in the same module (e.g. the
`route_objstm_containers` / `document_other_set_*` tests use `Pdf` over a `Cursor`).
Also assert the page-1 `page_hints[..].object_count` excludes the font (i.e. equals the
count without it), to lock the hint-table effect.

**Step 2: Run it.**

Run: `cargo test -p flpdf --lib linearization::plan 2>&1 | tail`
Expected: PASS. (This test does NOT need `qpdf-zlib-compat`; keep it feature-agnostic so
it runs in the default coverage build.)

**Step 3: Commit.**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "test(flpdf-zda0): unit-test part9 routing for others>0 other-page object"
```

---

## Task 5: Wire the byte test into CI + coverage gate

**Files:**
- Modify: `.github/workflows/ci.yml` (explicit `qpdf-zlib-compat` bytes-identical test list)

**Step 1: Add `catalog_otherpage_other_two_page_classic_is_byte_identical_to_qpdf` (or
the whole `cmp_linearize_tests` invocation, matching how sibling tests are listed) to the
gated bytes-identical job.** (memory: CI bytes-identical needs explicit test enumeration —
gated byte tests don't run unless added to ci.yml.)

Run: `rg -n 'cmp_linearize_tests|qpdf-zlib-compat|bytes-identical|byte-identical' .github/workflows/ci.yml`
to find the exact list and match its format.

**Step 2: Run the patch-coverage gate (commit first).**

Run: `scripts/patch-coverage.sh --base origin/main`
Expected: `flpdf` changed lines 100% covered (exit 0). If the fix's changed lines are not
100%, the byte test + unit test should already cover them; add coverage or a justified
`// cov:ignore` with a PR-note reason. Run WITHOUT `qpdf-zlib-compat` (memory:
llvm-cov/patch-coverage run without the compat feature).

**Step 3: Commit.**

```bash
git add .github/workflows/ci.yml
git commit -m "ci(flpdf-zda0): run catalog-otherpage-other-two-page byte test in gated suite"
```

---

## Task 6: Final verification + finish branch

**Step 1:** `cargo fmt --all` then `cargo fmt --all --check` (memory: CI quality gate is
`cargo fmt --check`).

**Step 2:** `cargo clippy -p flpdf --all-targets` — zero warnings.

**Step 3:** Re-run the byte suite + unit tests once more to confirm green.

**Step 4:** REQUIRED SUB-SKILL: `superpowers:verification-before-completion` — run the
verification commands and paste real output before claiming done.

**Step 5:** REQUIRED SUB-SKILL: `superpowers:finishing-a-development-branch` — push +
open PR (referencing flpdf-zda0; note flpdf-pn7h as the generate-path follow-up and the
deviation: classic path only).
