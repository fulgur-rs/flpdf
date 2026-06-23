# flpdf-79ef: CLI plain rewrite must not prune /Resources entries Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Stop the CLI `rewrite` (plain/overlay path) from pruning unused `/Resources`
entries, so `flpdf rewrite` keeps unreferenced image/font XObjects exactly like
`qpdf` (which only prunes during page-copy operations), fixing the dropped image
XObject on `kept-indirect-length.pdf`.

**Architecture:** `run_rewrite` (the non-`--pages` path) currently runs
`remove_unreferenced_resources(auto)` as Step 3, dropping `/Im0` because the page
content (`BT ET`) references no name. qpdf does NOT prune resource-dict entries on a
plain rewrite — verified: `qpdf --static-id` and `qpdf --static-id
--remove-unreferenced-resources=yes` both keep the unreferenced `/Im0` and `/F2`;
pruning only fires on page-copy ops (`--pages`). The library `write_pdf_with_options`
path never prunes and is byte-identical to the qpdf golden. Fix = delete the Step 3
prune call from `run_rewrite`; `run_page_extraction` (`--pages`) keeps its pruning.
This reverses the resource-pruning half of the closed flpdf-9hc.12.4/12.7 design
(which conflated unreferenced-OBJECT GC, kept, with resource-dict-ENTRY pruning).

**Tech Stack:** Rust, qpdf 11.9.0 oracle, cargo test, `assert_cmd` CLI tests.

**Invariant to protect:** `needs_mutation` (main.rs:3019-3025) keeps the
`remove_unref != No` term — it forces `full_rewrite=true` for a bare `flpdf rewrite`
(qpdf always full-rewrites) AND drives the documented signed-PDF incremental opt-out
(`--remove-unreferenced-resources=no` → incremental → signatures preserved,
`docs/signed-pdf.md`). Do NOT remove that term. Only the prune CALL goes.

---

### Task 1: Invert the optimization-matrix tests to assert plain rewrite KEEPS unused fonts

**Files:**
- Test: `crates/flpdf-cli/tests/cli_optimization_matrix.rs:356-412`

**Step 1: Rewrite cells 3a/3b to assert qpdf-parity (no pruning on plain rewrite)**

Change `remove_unref_resources_auto_prunes_unused_font` and
`remove_unref_resources_yes_prunes_unused_font` so that BOTH `/F1` (used) and `/F2`
(unused) must be RETAINED — matching qpdf, which keeps unreferenced resource entries
on a plain rewrite even with `=yes`. Rename the two tests to
`remove_unref_resources_auto_keeps_unused_font_like_qpdf` /
`..._yes_keeps_unused_font_like_qpdf`. Update the cell-comment block at lines 348-354
to state qpdf only prunes during page operations. Leave cell 3c
(`remove_unref_resources_no_retains_all_fonts`) unchanged. Add a header sentence
citing the qpdf observation (plain rewrite keeps unreferenced resources; pruning is
page-op-only).

**Step 2: Run to verify they FAIL against current (still-pruning) code**

Run: `cargo test -p flpdf-cli --test cli_optimization_matrix remove_unref`
Expected: the two inverted tests FAIL (current code prunes `/F2`).

**Step 3: Commit the (now-red) tests**

```bash
git add crates/flpdf-cli/tests/cli_optimization_matrix.rs
git commit -m "test(cli): plain rewrite keeps unreferenced /Resources like qpdf (flpdf-79ef)"
```

---

### Task 2: Add the kept-indirect-length CLI regression test (the actual bug)

**Files:**
- Test: `crates/flpdf-cli/tests/cli_optimization_matrix.rs` (new test, near the font cells)

**Step 1: Write the failing test**

Add a test `kept_indirect_length_plain_rewrite_keeps_image_xobject` that runs
`flpdf rewrite --static-id tests/fixtures/compat/kept-indirect-length.pdf OUT` and
asserts page 1's `/Resources/XObject` still contains `Im0` (reuse the
`extract_page_*_keys` pattern, adding an XObject-keys helper). This pins flpdf-79ef:
the DCTDecode image must survive a plain CLI rewrite. No byte comparison (miniz
deflate differs from qpdf), so no feature gate needed.

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf-cli --test cli_optimization_matrix kept_indirect_length`
Expected: FAIL — current code emits empty `/XObject`, `Im0` absent.

**Step 3: Commit**

```bash
git add crates/flpdf-cli/tests/cli_optimization_matrix.rs
git commit -m "test(cli): pin kept-indirect-length image XObject survives rewrite (flpdf-79ef)"
```

---

### Task 3: Delete the Step 3 prune call from run_rewrite (the fix)

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:3063-3066` (delete the prune call)
- Modify: `crates/flpdf-cli/src/main.rs:33` (remove now-orphaned import
  `resources::remove_unreferenced_resources,`)
- Modify: `crates/flpdf-cli/src/main.rs:~2989-3018` (comment block) and the
  step-3 line at `:3002` — drop the pruning rationale, keep the full-rewrite-default
  rationale, note pruning is page-op-only (qpdf observation).

**Step 1: Delete the call + orphaned import + correct the comments**

- Remove lines 3063-3066 (`// Step 3: remove unreferenced /Resources entries.` and
  the `if remove_unref != No { remove_unreferenced_resources(..)?; }`).
- Remove `resources::remove_unreferenced_resources,` from the `use flpdf::{...}` block.
  (`RemoveUnreferencedResources` enum + `CliRemoveUnreferencedResources` stay — still
  used by `prune_after_subset` on the `--pages` path.)
- In the 2989-3018 comment block: drop the numbered step "3. remove_unreferenced_resources"
  and rewrite the INTENTIONAL DEFAULT paragraph to: plain `flpdf rewrite IN OUT`
  full-rewrites by default (qpdf always full-rewrites) and applies default FlateDecode
  compression, matching plain `qpdf IN OUT`; it does NOT prune `/Resources` entries —
  qpdf only prunes during page operations (`--pages`/`--split-pages`), which flpdf
  performs in `run_page_extraction`. Add a one-line note on `needs_mutation` keeping
  `remove_unref != No` to preserve the full-rewrite default and the signed-PDF
  incremental opt-out.

**Step 2: Build (no unused-import error) and run the inverted + regression tests**

Run: `cargo build -p flpdf-cli`
Expected: clean (no `unused import` error).
Run: `cargo test -p flpdf-cli --test cli_optimization_matrix`
Expected: all PASS (3a/3b keep `/F2`, kept-indirect-length keeps `Im0`, 3c unchanged).

**Step 3: Verify the bare-rewrite full-rewrite default still holds**

Run: `cargo test -p flpdf-cli --test cli_tests rewrite_default_is_qpdf_equivalent_full_rewrite`
Expected: PASS (full rewrite + FlateDecode default unchanged).

**Step 4: Commit**

```bash
git add crates/flpdf-cli/src/main.rs
git commit -m "fix(cli): plain rewrite no longer prunes /Resources entries (flpdf-79ef)

qpdf only prunes unreferenced resource-dict entries during page-copy ops
(--pages), never on a plain rewrite (verified: --remove-unreferenced-resources=yes
still keeps them). Removing the run_rewrite Step 3 prune call makes CLI == library
== qpdf; the dropped DCTDecode image on kept-indirect-length.pdf is preserved.
Corrects the resource-pruning half of flpdf-9hc.12.4/12.7 (object GC vs
resource-entry pruning conflation)."
```

---

### Task 4: Verify signed-PDF doc + overlay still hold; fix doc only if it implies pruning

**Files:**
- Check/Modify: `docs/signed-pdf.md:76-100`
- Check: `crates/flpdf-cli/tests/cli_overlay.rs`, `cli_full_rewrite.rs`

**Step 1: Run signed-PDF + overlay tests**

Run: `cargo test -p flpdf-cli --test cli_full_rewrite`
Run: `cargo test -p flpdf-cli --test cli_overlay`
Expected: PASS. `incremental_rewrite_of_signed_pdf_succeeds_without_warning` relies on
`--remove-unreferenced-resources=no` → incremental (still true via `needs_mutation`).
Overlay only checks the overlay XObject is present (no byte/pruning dependency).

**Step 2: Re-read signed-pdf.md; correct only if it implies content pruning**

The doc says the full rewrite is "forced by the default
`--remove-unreferenced-resources=auto`" — still accurate (the auto default still forces
full_rewrite). If any sentence implies resources get *pruned*, reword to "forces a full
rewrite" without the pruning implication. If the doc is already accurate, leave it
unchanged and note that in the PR description.

**Step 3: Commit (only if doc changed)**

```bash
git add docs/signed-pdf.md
git commit -m "docs(signed-pdf): clarify auto default forces full rewrite, not resource pruning (flpdf-79ef)"
```

---

### Task 5: Full quality gate + beads correction note

**Step 1: Run the broader CLI + library suites touching resources/rewrite**

Run: `cargo test -p flpdf-cli`
Run: `cargo test -p flpdf --test resource_pruning_tests --test cmp_diff_zero_tests --test kept_indirect_length_holder_tests`
(the latter three also need `--features qpdf-zlib-compat` for the byte tests:
`cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests`)
Expected: all PASS. The library unit tests of `remove_unreferenced_resources` are
unaffected (the function is unchanged; only the CLI call site removed).

**Step 2: fmt + clippy**

Run: `cargo fmt --all` then `cargo fmt --all --check`; `cargo clippy -p flpdf-cli`
Expected: clean (per memory: fmt --check is the CI quality gate).

**Step 3: patch coverage gate**

Run: `scripts/patch-coverage.sh --base main` (after committing). flpdf changed lines
must be 100% (here the only `crates/flpdf/src` change is none — fix is CLI-only +
docs; CLI coverage is report-only). Confirm no flpdf src lines changed.

**Step 4: Record the flpdf-9hc.12.4/12.7 correction in beads**

`bd comment` / `bd update` note on flpdf-9hc.12.4 and flpdf-9hc.12.7 that the
plain-rewrite resource-entry pruning was a qpdf-divergence (conflated with object GC)
and is reverted in flpdf-79ef; page-op pruning (`--pages`) is retained and correct.

**Step 5: Final commit if any fmt/doc residue**

```bash
git add -A && git commit -m "chore(flpdf-79ef): fmt + finalize"
```

---

## Out of scope (note in PR; possible follow-up issue)
- Characterizing qpdf's exact page-op resource-removal rule (XObject vs font,
  shared/indirect dicts): `--pages` pruned an XObject in one probe but kept `/F2`
  in the font fixture — rule not fully characterized. Not needed for this fix; the
  `--pages` path behavior is unchanged here.
- Whether plain rewrite should warn/reject explicit `--remove-unreferenced-resources=yes`
  (qpdf silently no-ops; flpdf now also no-ops on the pruning axis).
