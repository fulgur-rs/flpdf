# flpdf-s5i2: Remove the `!reconstructed` gate on the page-tree repair pass

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make flpdf's linearize page-tree repair (duplicate-leaf clone, interior/leaf `/Type` override, root `/Pages` correction) run unconditionally, matching qpdf 11.9.0's `getAllPagesInternal` (which has no reconstruction gate), so reconstructed-xref inputs become byte-identical to `qpdf --linearize --deterministic-id`.

**Architecture:** `push_inherited_attributes_to_pages` (crates/flpdf/src/linearization/inherited_attrs.rs) currently wraps the whole repair pass in `if !reconstructed { ... }`. That gate was modeled on qpdf **12.4.0** (unreleased), which does gate cloning on `!reconstructed_xref` (QPDF_pages.cc:205). The oracle is qpdf **11.9.0** (regenerate.sh pins it; `/usr/bin/qpdf` is 11.9.0), whose `getAllPagesInternal` (QPDF_pages.cc:77-138) has NO reconstruction gate and repairs unconditionally. Removing the gate closes the divergence. flpdf-zy9t's drop+flatten premise was 12.4.0-only and was closed wrong-version.

**Tech Stack:** Rust; qpdf 11.9.0 oracle; `qpdf-zlib-compat` feature for byte-identical goldens (hint stream is Flate-compressed); `scripts/patch-coverage.sh` gate.

**Empirical grounding (already verified, pre-plan):** With the gate removed, flpdf's linearize output is byte-identical to the qpdf 11.9.0 golden for BOTH a minimal empty-`/Contents` reconstructed fixture (1317 B) AND the committed `shared-page-two-parents.pdf` damaged to force reconstruction (1439 B, real content stream). qpdf 11.9.0 `--check` on both emits "creating a new page object as a copy" — it clones, it does not drop+flatten. See beads flpdf-s5i2 `design` field for full detail.

---

### Task 1: Remove the gate (clean) and delete the throwaway harness

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs` (the `if !reconstructed { ... }` block, was lines 66-129)
- Delete: `crates/flpdf/tests/zzz_throwaway_s5i2.rs`

**Step 1: Delete the throwaway test file**

```bash
rm crates/flpdf/tests/zzz_throwaway_s5i2.rs
```

**Step 2: Restore the gate to its original committed form first (undo the throwaway `{` hack)**, then apply the clean removal. The original committed block is:

```rust
    let reconstructed = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|d| d.message.contains("reconstruct cross-reference"));
    if !reconstructed {
        // (6) Correct a catalog whose `/Pages` points into the tree ...
        ... repair (6) loop ...
        let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
        ... repair_page_tree(...)?;
    }
```

Replace the `let reconstructed = ...; if !reconstructed {` wrapper and its matching `}` so the body runs unconditionally, **de-indented one level**. Keep the repair (6) loop and the `repair_page_tree` call verbatim (only the indentation changes).

**Step 3: Rewrite the module comment (was lines 54-70).** Drop the "11.9.0's `getAllPagesInternal` has no xref-reconstruction gate ... flpdf instead skips the whole repair pass ... a deliberate flpdf-specific divergence ... Tracked as flpdf-s5i2." sentence. Replace with a statement that flpdf now mirrors 11.9.0's unconditional repair. Keep it spec-grounded (cite QPDF_pages.cc:77-138, no reconstruction gate). Do NOT leave a beads ID in the comment (this is `//` non-doc, but keep it clean). Example replacement for the trailing sentences:

```rust
    // qpdf 11.9.0's `getAllPagesInternal` performs these repairs unconditionally
    // (there is no xref-reconstruction gate anywhere in QPDF_pages.cc:77-138), so
    // flpdf runs them for every input, reconstructed or not.
```

**Step 4: Verify it compiles and existing tests still pass (no regression on non-reconstructed goldens).**

```bash
cargo build -p flpdf
cargo test -p flpdf --lib linearization::inherited_attrs
```
Expected: PASS (existing non-reconstructed unit tests unaffected).

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git rm crates/flpdf/tests/zzz_throwaway_s5i2.rs 2>/dev/null || true
git commit -m "fix(linearize): repair page tree unconditionally, matching qpdf 11.9.0 getAllPagesInternal (no reconstruction gate) (flpdf-s5i2)"
```

---

### Task 2: Invert the reconstructed-clone unit test + revise stale guard comments

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs` (test `reconstructed_xref_input_does_not_clone`, was ~2316-2337; its doc comment ~2291-2296; ~9 guard comments elsewhere referencing "behind the !reconstructed gate")

**Step 1: Rewrite the test to assert the reconstructed input NOW clones.** Rename `reconstructed_xref_input_does_not_clone` -> `reconstructed_xref_input_clones_shared_leaf`. Keep the reconstruction-happened assertion. Change the post-push assertion from `== before_count` to `== before_count + 1` (one clone minted), and add structural assertions mirroring `duplicate_leaf_across_two_parents_is_cloned` (parent A keeps original leaf; parent B points at the clone; both `/Rotate 90` and `/Rotate 180` are preserved on separate objects). Rewrite the doc comment to state 11.9.0 clones unconditionally and flpdf now matches.

**Step 2: Revise stale guard comments.** For each test whose comment says "fixture must NOT trip xref reconstruction, else the repair pass is skipped behind the !reconstructed gate", the `assert!(...is_empty())` / `...!any(reconstruct)` assertion can stay as a harmless sanity check, but the "behind the !reconstructed gate" rationale is now false. Reword to a neutral "fixture must NOT trip xref reconstruction (it exercises the clean-parse repair path)". Do not delete the assertions.

**Step 3: Run the unit tests.**

```bash
cargo test -p flpdf --lib linearization::inherited_attrs
```
Expected: PASS, including `reconstructed_xref_input_clones_shared_leaf`.

**Step 4: Commit**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "test(linearize): reconstructed shared leaf now clones; drop stale gate rationale from guards (flpdf-s5i2)"
```

---

### Task 3: Add reconstructed mistyped-/Type + reconstructed-clean-no-op unit tests

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs` (new tests + a damaged-xref helper reusing `pdf_with_interior_type_not_pages`)

**Step 1: Add a reconstructed variant of the interior-`/Type`-override test.** Add a helper `pdf_interior_type_not_pages_damaged_xref()` (damage the startxref of `pdf_with_interior_type_not_pages()`, same technique as `pdf_shared_leaf_damaged_xref`). Add a test asserting: fixture reconstructs, and after push the interior node's `/Type /Foo` is overridden to `/Pages` (i.e. the repair runs even when reconstructed).

**Step 2: Add a reconstructed-but-clean no-op regression test.** A minimal well-formed single-page fixture with a damaged startxref (no duplicate leaf, correct `/Type`, correct root `/Pages`). Assert: fixture reconstructs; after push, `object_refs().len()` is unchanged (repair (6) + repair_page_tree are both no-ops) and the page's inherited attrs are pushed as normal. This pins the advisor's flag that removing the gate does not regress clean reconstructed inputs.

**Step 3: Run the unit tests.**

```bash
cargo test -p flpdf --lib linearization::inherited_attrs
```
Expected: PASS.

**Step 4: Commit**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "test(linearize): reconstructed interior /Type override + clean-input no-op regression (flpdf-s5i2)"
```

---

### Task 4: Oracle byte-identical golden test for a reconstructed shared-page input

**Files:**
- Create: `tests/fixtures/compat/shared-page-two-parents-reconstructed.pdf` (committed binary = `shared-page-two-parents.pdf` with startxref value repointed to `9`)
- Create: `tests/golden/references/shared-page-two-parents-reconstructed/linearize.pdf`
- Modify: `tests/golden/regenerate.sh` (golden-generation entry)
- Modify: `crates/flpdf/tests/cmp_linearize_tests.rs` (repair-based helper + test)

**Step 1: Create the committed fixture** by damaging the committed base fixture's startxref (deterministic; do NOT regenerate from scratch):

```bash
python3 - <<'PY'
base = open("tests/fixtures/compat/shared-page-two-parents.pdf","rb").read()
n = b"startxref\n"; p = base.index(n)+len(n); e = base.index(b"\n", p)
open("tests/fixtures/compat/shared-page-two-parents-reconstructed.pdf","wb").write(base[:p]+b"9"+base[e:])
PY
```

**Step 2: Add the golden-generation entry to `tests/golden/regenerate.sh`** (after the `shared-page-two-parents-pushmint` block, ~line 1134). `--warning-exit-0` absorbs the reconstruction warnings:

```bash
# --- shared-page-two-parents-reconstructed: the shared-leaf fixture with a
# damaged startxref, forcing qpdf-style xref reconstruction. qpdf 11.9.0's
# getAllPagesInternal has no reconstruction gate (QPDF_pages.cc:77-138), so it
# STILL clones the duplicate leaf; pins flpdf's gate-free repair (flpdf-s5i2).
mkdir -p "$REF/shared-page-two-parents-reconstructed"
qpdf --linearize --deterministic-id --warning-exit-0 \
    "$FIX/shared-page-two-parents-reconstructed.pdf" \
    "$REF/shared-page-two-parents-reconstructed/linearize.pdf"
echo "shared-page-two-parents-reconstructed/linearize.pdf"
```

**Step 3: Generate the golden** (must run under qpdf 11.9.0):

```bash
bash tests/golden/regenerate.sh 2>&1 | grep -i reconstructed
```
Expected: prints `shared-page-two-parents-reconstructed/linearize.pdf`; golden file created (~1439 bytes).

**Step 4: Add a repair-based helper + test to `crates/flpdf/tests/cmp_linearize_tests.rs`.** The existing `flpdf_linearized` uses `Pdf::open` (strict), which fails on a reconstructed input. Add:

```rust
/// As `flpdf_linearized`, but opens with `open_with_repair` so a reconstructed-
/// xref fixture parses (both the plan handle and the write handle must repair).
fn flpdf_linearized_repair(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open_with_repair(std::io::BufReader::new(file)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    let renumber = RenumberMap::from_plan(&plan);

    let file2 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf2 = Pdf::open_with_repair(std::io::BufReader::new(file2)).unwrap();
    let mut opts = WriteOptions::default();
    opts.deterministic_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    doc.back_patch().unwrap();
    doc.bytes
}

fn assert_linearize_byte_identical_repair(fixture: &str, stem: &str) {
    let actual = flpdf_linearized_repair(fixture);
    let expected = golden(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf --linearize --deterministic-id golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(), expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

/// A shared `/Page` leaf fixture whose startxref is damaged, forcing xref
/// reconstruction on open. qpdf 11.9.0's getAllPagesInternal has no
/// reconstruction gate, so it still clones the duplicate leaf; this pins flpdf's
/// gate-free repair to the qpdf 11.9.0 golden (flpdf-s5i2).
#[test]
fn shared_page_two_parents_reconstructed_byte_identical_to_qpdf() {
    assert_linearize_byte_identical_repair(
        "shared-page-two-parents-reconstructed.pdf",
        "shared-page-two-parents-reconstructed",
    );
}
```

**Step 5: Run the byte test (the decisive oracle check).**

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests \
    shared_page_two_parents_reconstructed_byte_identical_to_qpdf -- --nocapture
```
Expected: PASS (byte-identical). CI already runs the whole `--test cmp_linearize_tests` target, so no `ci.yml` edit is needed.

**Step 6: Commit**

```bash
git add tests/fixtures/compat/shared-page-two-parents-reconstructed.pdf \
        tests/golden/references/shared-page-two-parents-reconstructed/linearize.pdf \
        tests/golden/regenerate.sh crates/flpdf/tests/cmp_linearize_tests.rs
git commit -m "test(linearize): qpdf-oracle byte-identical golden for reconstructed shared-page input (flpdf-s5i2)"
```

---

### Task 5: Quality gates + PR

**Step 1: Full build, format, clippy, tests.**

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy -p flpdf --all-targets -- -D warnings
cargo test -p flpdf
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests
```
Expected: all green.

**Step 2: Patch coverage (commit first; run WITHOUT qpdf-zlib-compat).** Per CLAUDE.md and memory `llvm-cov-no-qpdf-zlib-compat`, patch-coverage runs on the miniz default. The new SRC change is only the gate removal (de-indent) — all its lines are already covered by the (now unconditional) existing repair tests + the new reconstructed unit tests. The byte test is qpdf-zlib-compat-gated so it will NOT count toward the miniz coverage run; the reconstructed unit tests in `inherited_attrs.rs` (default build) must cover the repair lines for reconstructed inputs.

```bash
git status   # confirm all committed
scripts/patch-coverage.sh --base main
```
Expected: flpdf changed lines 100% (or add unit coverage / justified `// cov:ignore`).

**Step 3: Qualitative check (per CLAUDE.md gate step 4).** Confirm the new tests assert real behavior: the reconstructed clone test checks BOTH rotate values on separate objects; the byte test is a true byte comparison; the no-op test pins the clean-reconstructed regression.

**Step 4: Push branch + open PR** (per memory `gh-pr-create-graphql-401-use-rest`, `gh pr create` is correct). PR body: cite the 11.9.0 vs 12.4.0 version premise, that flpdf-zy9t was closed wrong-version, and the byte-identity evidence.

```bash
git push -u origin fix/s5i2-reconstructed-repair-gate
gh pr create --title "fix(linearize): repair page tree unconditionally to match qpdf 11.9.0 (flpdf-s5i2)" --body "<...>"
```

**Step 5: Update beads** — leave flpdf-s5i2 `in_progress` until the PR merges (per repo convention); add a note with the PR URL.
