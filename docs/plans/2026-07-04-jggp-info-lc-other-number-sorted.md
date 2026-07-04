# Plan: flpdf-jggp — /Info number-sorted `lc_other`, not fixed part9-head slot

For Claude: execution plan for beads issue flpdf-jggp. Design is in the beads
`design` field (`bd show flpdf-jggp`). Scope: classic path only. qpdf oracle
verified against 11.9.0 source (QPDF_linearization.cc:1272-1337). TDD order below.

## Goal
qpdf places `/Info` as a number-sorted member of the part9 remaining `lc_other`
set, not at a fixed head slot after the pages tree. Make flpdf match byte-for-byte
when a lower-numbered `lc_other` sibling coexists with `/Info`.

## TDD steps

### 1. RED — feature-agnostic renumber unit test (the coverage gate)
File: `crates/flpdf/src/linearization/renumber.rs` (tests mod).
New test `info_is_number_sorted_lc_other_not_fixed_head_slot`:
- plan: `part2_objects=[obj2]`, `part4_rest=[font(7), pages(8), info(10)]`
  (number-sorted), `pages_tree_ref=Some(8)`, `info_ref=Some(10)`,
  `total_object_count` set.
- assert `pages→1`, `font→2`, `info→3` (font's new number < info's).
Run: must FAIL on current code (info fixed-slot → info=2, font=3).

### 2. GREEN — remove the `/Info` fixed promotion
File: `renumber.rs::from_plan`.
- Delete `promote(plan.info_ref, …)` (~line 247). Keep the `pages_tree` promotion.
- `/Info` now flows through the remaining-`part4_rest` loop (~260-268), skipped-if-
  already-placed guard already handles pages_tree/root.
Run step-1 test → GREEN. Run full `linearization::renumber` suite → all green
(the coincidentally-passing `qpdf_layout_…` test stays green; that's expected).

### 3. Doc coherence (same file)
- module-doc table line ~14 (`N+2 = Info dict fixed slot`): reframe `/Info` as a
  number-sorted `lc_other` in the remaining-`part4_rest` range.
- module-doc worked examples ~31-33: annotate that `1=Pages, 2=Info` holds only
  because `/Info` is the lowest remaining `lc_other` in those fixtures (number-sort,
  not a reserved slot).
- from_plan doc item "4. info dict" (~147) and slot-order comment block (~222-247):
  drop the "part9 head promotion" framing for `/Info`.

### 4. Fixture + qpdf golden
- `tests/fixtures/compat/catalog-otherpage-other-info-two-page.pdf`: copy the zda0
  fixture, ADD `10 0 obj << /Producer (flpdf) >>` Info dict, trailer
  `<< /Size 11 /Root 1 0 R /Info 10 0 R >>`, extend xref. Verify qpdf accepts it.
- Golden: generate with the repo's golden-gen path (same `qpdf --linearize`
  deterministic-id invocation as zda0's golden). Store at the harness's golden dir
  `catalog-otherpage-other-info-two-page/linearize.pdf`.

### 5. Byte-identical oracle test
File: `crates/flpdf/tests/cmp_linearize_tests.rs` (behind `qpdf-zlib-compat`).
`catalog_otherpage_other_info_two_page_classic_is_byte_identical_to_qpdf()` calling
`assert_linearize_byte_identical(fixture, golden_dir)`. Confirm: fails pre-fix
(revert-check), byte-identical post-fix.

### 6. CI registration
`.github/workflows/ci.yml`: add the new test fn to the explicit bytes-identical
test list (memory flpdf-ci-bytes-identical-explicit-test-list).

## Verification (Session Completion gate)
- `cargo test -p flpdf --lib linearization::renumber` — all green.
- `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests` —
  new byte test + ALL existing goldens green (incl. any classic-outline fixture;
  no re-bless).
- `cargo fmt --all --check` (memory flpdf-ci-quality-fmt-check).
- `cargo clippy` clean.
- `scripts/patch-coverage.sh` — flpdf changed lines 100% (run WITHOUT
  qpdf-zlib-compat; memory llvm-cov-no-qpdf-zlib-compat). Commit before running.
