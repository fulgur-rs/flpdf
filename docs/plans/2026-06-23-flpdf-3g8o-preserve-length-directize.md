# flpdf-3g8o: preserve-mode /Length direct-ization + orphan holder drop — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `--stream-data=preserve` full rewrites direct-ize every stream's
`/Length` and garbage-collect the now-orphaned indirect `/Length` holder, byte-identical to `qpdf --static-id --stream-data=preserve`.

**Architecture:** The classic rewrite path (`write_pdf_full_rewrite`) gates the
orphan-holder drop on `effective_stream_policy(options).is_some()`, which is
`None` in preserve mode, so the holder is kept. The generate path already
computes orphans unconditionally and is byte-correct, and
`renumber_refs_in_place` already direct-izes a stream `/Length` whose holder is
absent from the renumber map. The fix is therefore a one-line gate change to
`!options.qdf`: excluding the holder from the map makes the existing
direct-ization fire and the existing `build_excluding` drop the holder.

**Tech Stack:** Rust, qpdf 11.9.0 oracle, byte-golden tests gated on the
`qpdf-zlib-compat` feature, `scripts/patch-coverage.sh` gate.

**Beads issue:** flpdf-3g8o (design + acceptance live in the issue).

**Worktree:** `.worktrees/flpdf-3g8o` on branch
`flpdf-3g8o-preserve-length-directize`. All work happens here.

---

## Task 1: Core fix — gate orphan drop on `!qdf` + fix stale comment

**Files:**
- Modify: `crates/flpdf/src/writer.rs:2668-2675`

**Step 1: Update the existing structural test to assert the corrected behavior (failing test first)**

In `crates/flpdf/tests/orphan_indirect_length_holder_tests.rs`, replace
`stream_data_preserve_keeps_indirect_length_and_holder` (lines ~111-129) with:

```rust
#[test]
fn stream_data_preserve_drops_orphan_holder_and_directizes_length() {
    // `--stream-data=preserve` keeps stream bytes verbatim, but qpdf still
    // normalizes every stream's /Length to a direct integer and garbage-collects
    // the now-orphaned indirect holder. flpdf must match (flpdf-3g8o): the
    // orphan-drop gate fires for every non-qdf mode, not only when streams are
    // recompressed.
    let mut opts = base_opts();
    opts.stream_data = Some(StreamDataMode::Preserve);
    let out = rewrite(&opts);

    assert_eq!(
        object_count(&out),
        6,
        "preserve mode must drop the orphaned indirect /Length holder"
    );
    assert!(
        matches!(js_stream_length(&out), Object::Integer(_)),
        "preserve mode must direct-ize the JS stream's /Length once the holder is dropped"
    );
}
```

**Step 2: Run the test to verify it fails**

Run: `cargo test -p flpdf --test orphan_indirect_length_holder_tests stream_data_preserve_drops_orphan_holder_and_directizes_length`
Expected: FAIL — `left: 7, right: 6` (holder still present pre-fix).

**Step 3: Apply the one-line gate change + rewrite the comment**

In `crates/flpdf/src/writer.rs`, replace the comment block + gate (lines ~2668-2675):

```rust
    // flpdf-sqkq / flpdf-3g8o: drop indirect `/Length` holders that orphan once
    // each stream's `/Length` is normalized to a direct integer, matching qpdf's
    // reachability GC. Every non-qdf mode emits a direct `/Length` (the compress
    // path via `apply_stream_compress_policy`, preserve/encrypt via
    // `renumber_refs_in_place` direct-izing a dropped holder's placeholder), so
    // the orphan drop applies to all of them. Only qdf is excluded: it
    // externalizes `/Length` into its own holder objects, so that lifecycle is
    // left untouched. Normal PDFs have no such orphans, so the set is empty and
    // renumbering is unaffected.
    let orphan_length_holders = if !options.qdf {
        object_streams::orphaned_indirect_length_holders(pdf)?
    } else {
        BTreeSet::new()
    };
```

**Step 4: Run the structural test to verify it passes**

Run: `cargo test -p flpdf --test orphan_indirect_length_holder_tests`
Expected: PASS (3 tests).

**Step 5: Regression — full default-feature flpdf suite**

Run: `cargo test -p flpdf`
Expected: PASS (no failures; ~1815+ tests).

**Step 6: Commit**

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/tests/orphan_indirect_length_holder_tests.rs
git commit -m "fix(writer): drop orphan indirect /Length holder in preserve mode (flpdf-3g8o)"
```

---

## Task 2: Byte-identical goldens for preserve (plain + flate)

**Files:**
- Modify: `tests/golden/regenerate.sh`
- Create (generated): `tests/golden/references/objstm-lin-od-indirect-length/preserve.pdf`
- Create (generated): `tests/golden/references/objstm-lin-od-indirect-length-flate/preserve.pdf`
- Modify: `crates/flpdf/tests/cmp_diff_zero_tests.rs`

**Step 1: Add the preserve golden generation to regenerate.sh**

Find the block that emits the od-indirect-length references (around lines
588-669). Alongside the existing `static-id.pdf` generation for both stems, add a
`preserve.pdf` produced by:

```bash
qpdf --stream-data=preserve --static-id --warning-exit-0 \
    "$FIX/$stem.pdf" "$REF/$stem/preserve.pdf"
```

Match the exact loop/idiom already used for the static-id golden of these two
stems (do not invent a new structure — mirror the neighbouring lines).

**Step 2: Regenerate the goldens**

Run: `bash tests/golden/regenerate.sh` (or the targeted invocation the script
supports). Confirm the two `preserve.pdf` files appear and are 6-object outputs:
`qpdf --show-object=trailer .../preserve.pdf` shows `/Size 7`.

**Step 3: Add the preserve helper + sibling tests (failing first if golden absent)**

In `crates/flpdf/tests/cmp_diff_zero_tests.rs`, add a preserve-mode helper
mirroring `rewrite_qpdf_equivalent` but with `stream_data = Some(StreamDataMode::Preserve)`:

```rust
fn rewrite_preserve_qpdf_equivalent(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    opts.stream_data = Some(StreamDataMode::Preserve);
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn assert_cmp_diff_zero_named(actual: Vec<u8>, stem: &str, name: &str) {
    let expected = golden_named(stem, name);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{stem}/{name}: not byte-identical to qpdf golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(), expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

#[test]
fn od_indirect_length_preserve_drops_orphan_holder_byte_identical_to_qpdf() {
    let actual = rewrite_preserve_qpdf_equivalent("objstm-lin-od-indirect-length.pdf");
    assert_cmp_diff_zero_named(actual, "objstm-lin-od-indirect-length", "preserve.pdf");
}

#[test]
fn od_indirect_length_flate_preserve_drops_orphan_holder_byte_identical_to_qpdf() {
    let actual = rewrite_preserve_qpdf_equivalent("objstm-lin-od-indirect-length-flate.pdf");
    assert_cmp_diff_zero_named(actual, "objstm-lin-od-indirect-length-flate", "preserve.pdf");
}
```

Reuse the existing `StreamDataMode` import (add it if absent).

**Step 4: Run the new byte tests under the gated feature**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests od_indirect_length`
Expected: PASS for the two new `_preserve` tests and the two existing
`_drops_orphan_holder` tests.

**Step 5: Run the FULL gated byte suite (no other fixture may shift)**

Run: `cargo test -p flpdf --features qpdf-zlib-compat`
Expected: PASS — measure that no other fixture regresses (the gate now drops
orphans for all non-qdf modes; normal fixtures have empty orphan sets).

**Step 6: Commit**

```bash
git add tests/golden/regenerate.sh \
        tests/golden/references/objstm-lin-od-indirect-length/preserve.pdf \
        tests/golden/references/objstm-lin-od-indirect-length-flate/preserve.pdf \
        crates/flpdf/tests/cmp_diff_zero_tests.rs
git commit -m "test(writer): byte goldens for preserve-mode orphan holder drop (flpdf-3g8o)"
```

---

## Task 3: Encryption (preserve+encrypt) — empirical verify + structural regression test

**Files:**
- Modify: `crates/flpdf/tests/orphan_indirect_length_holder_tests.rs` (or a
  dedicated encryption test module if encryption helpers live elsewhere — check
  first with `grep -rln "encrypt" crates/flpdf/tests/`)

**Step 1: Confirm the correct flpdf `--encrypt` CLI syntax and qpdf oracle**

Run: `cargo run -p flpdf-cli -- rewrite --help | grep -A3 encrypt`
Run the qpdf oracle for object count:
`qpdf --allow-weak-crypto --static-id --encrypt "" "" 40 -- --stream-data=preserve <fixture> /tmp/q.pdf`
then `qpdf --decrypt /tmp/q.pdf /tmp/qd.pdf && qpdf --show-object=trailer /tmp/qd.pdf`.
Record qpdf's `/Size` (expected: holder dropped → same count as non-encrypted preserve).

**Step 2: Write the failing structural test**

Add a library-level test that builds the fixture with
`stream_data = Preserve` AND an encryption option (RC4-40 for determinism; mirror
how existing encryption tests construct `WriteOptions.encrypt`), re-opens with the
password, and asserts:
- the orphaned `/Length` holder is dropped (object count matches the
  non-encrypted preserve count = 6),
- the JS stream's `/Length` is a direct integer.

Look at existing encryption tests for the exact `WriteOptions.encrypt` builder
and reopen-with-password pattern; do NOT hand-roll the encryption setup.

**Step 3: Run the test**

Run: `cargo test -p flpdf <new_encryption_test_name>`
Expected: PASS (the Task 1 gate change already covers preserve+encrypt — this
test pins it as a regression guard for the Codex PR #401 NOTES case).

**Step 4: Regression**

Run: `cargo test -p flpdf`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/tests/<file>.rs
git commit -m "test(writer): preserve+encrypt drops orphan /Length holder (flpdf-3g8o)"
```

---

## Task 4: CI wiring + coverage gate

**Files:**
- Modify: `.github/workflows/ci.yml` (the explicit bytes-identical test list)
- Modify (if needed): test-exclusion comments only

**Step 1: Add the new byte tests to ci.yml's explicit list**

`grep -n "cmp_diff_zero\|qpdf-zlib-compat\|od_indirect_length" .github/workflows/ci.yml`
Add the two new `_preserve` test names to the explicit list that runs gated
byte tests (per the project rule: gated byte tests must be enumerated in ci.yml
or they never run in CI).

**Step 2: Patch coverage gate**

Commit all work first, then run:
`scripts/patch-coverage.sh --base main`
Expected: flpdf changed lines at 100% (the gate line is exercised by the
preserve byte/structural tests). Resolve any uncovered changed line by adding a
test (preferred) — the changed surface is tiny.

**Step 3: fmt + clippy**

Run: `cargo fmt --all` then `cargo fmt --all --check`
Run: `cargo clippy -p flpdf --all-targets`
Expected: clean (the project's CI Quality gate is `cargo fmt --check`).

**Step 4: Commit any CI/fmt changes**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run preserve-mode orphan-holder byte tests (flpdf-3g8o)"
```

---

## Task 5: Final verification + PR

**Step 1: Full gated suite one more time**

Run: `cargo test -p flpdf --features qpdf-zlib-compat`
Expected: PASS.

**Step 2: Verify acceptance criteria** (from beads flpdf-3g8o) are all met.

**Step 3: Push + PR** per CLAUDE.md Session Completion (gh pr create).

---

## Out of scope (explicit non-deviations)

- **linearize+preserve**: probed — already direct-izes `/Length` and drops the
  holder (8 objects matching qpdf). No change. A separate, pre-existing
  hint-stream compression divergence (flpdf FlateDecode vs qpdf raw) is unrelated
  to this issue.
- **qdf**: untouched; the gate keeps `!options.qdf`. qdf's `/Length`
  externalization lifecycle (flpdf-3g8o NOTES) remains a follow-up.
