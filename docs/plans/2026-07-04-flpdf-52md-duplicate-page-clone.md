# flpdf-52md: cache()-equivalent duplicate-page clone pass Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make flpdf's linearized output byte-identical to qpdf when a `/Page`
leaf is shared by more than one `/Pages` parent, by cloning each 2nd+ occurrence
into a distinct object (qpdf's `getAllPagesInternal` clone behavior).

**Architecture:** Add a `resolve_duplicate_page_leaves` pre-pass at the top of
`push_inherited_attributes_to_pages` (mirroring qpdf's `(void)cache();`). It walks
the `/Kids` tree depth-first; on a leaf seen more than once it mints a shallow-copy
clone (new object number, sub-refs shared, original `/Parent` kept) and rewrites
that `/Kids` entry. Gated off when xref was reconstructed (qpdf clones only when
`!reconstructed_xref`). No duplicates => complete no-op (zero regression risk).

**Tech Stack:** Rust, flpdf crate, qpdf 11.9.0 oracle, `qpdf-zlib-compat` feature
for byte goldens, `cargo llvm-cov` patch coverage.

**Empirical baseline (already observed):** For a 2-parent shared-leaf fixture
(A `/Rotate 90`, B `/Rotate 180`), qpdf emits leaf(`/Rotate 90`,`/Parent`→A) + a
distinct CLONE(`/Rotate 180`,`/Parent`→A,`/Contents` shared), root `/Count 2`
unchanged, `/N 2`. flpdf currently emits 1 page referenced twice, `/N 1`, and
loses B's `/Rotate 180`.

**Key file locations:**
- Implementation: `crates/flpdf/src/linearization/inherited_attrs.rs`
  (push entry at `push_inherited_attributes_to_pages` ~line 42; existing helper
  `next_object_ref` ~line 207; `MAX_DEPTH` ~line 33).
- Push call sites (both linearize-only, both re-run the pass; must be idempotent):
  `plan.rs:728`, `writer.rs:2461`.
- Byte-golden test: `crates/flpdf/tests/cmp_linearize_tests.rs` (already in the CI
  byte list at `.github/workflows/ci.yml:157` — NO ci.yml change needed).
- Golden generator: `tests/golden/regenerate.sh` (append a block near the
  one-page linearize block ~line 1069).
- Fixture dir: `tests/fixtures/compat/`. Golden dir:
  `tests/golden/references/<stem>/linearize.pdf`.
- Reconstructed detection: `pdf.repair_diagnostics().entries()` returns
  `&[Diagnostic]`; `Diagnostic.message: String`.

---

## Task 1: Commit the shared-leaf fixture + qpdf golden

**Files:**
- Create: `tests/fixtures/compat/shared-page-two-parents.pdf`
- Modify: `tests/golden/regenerate.sh` (add generation block)
- Create: `tests/golden/references/shared-page-two-parents/linearize.pdf`

**Step 1: Write the fixture bytes.** The fixture is a minimal well-formed classic
PDF; obj 5 (`/Page`) is a kid of BOTH obj 3 (`/Pages` A, `/Rotate 90`) and obj 4
(`/Pages` B, `/Rotate 180`). Everything else well-formed (all kids indirect,
`/Type` correct, leaf has `/MediaBox`+`/Resources`, no `/Annots`, no direct
`/Outlines`, valid hand-written xref ⇒ `reconstructed_xref=false`). Object layout:

```
1 Catalog        << /Type /Catalog /Pages 2 0 R >>
2 root /Pages     << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>
3 /Pages A        << /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 /Rotate 90 >>
4 /Pages B        << /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 /Rotate 180 >>
5 /Page (shared)  << /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 6 0 R >>
6 content stream  << /Length 34 >> stream ... endstream
```

Generate with the committed helper (already validated in scratchpad):
`python3 make_shared_2parent.py tests/fixtures/compat/shared-page-two-parents.pdf`
(or hand-write identical bytes). Verify `qpdf --check` prints exactly one
"appears more than once in the pages tree; creating a new page object as a copy"
warning.

**Step 2: Add the regenerate.sh block** (near line 1069), so the golden is
reproducible under qpdf 11.9.0:

```bash
# --- shared-page-two-parents: same /Page leaf under two /Pages parents with
# different inherited /Rotate; qpdf clones the 2nd occurrence (flpdf-52md). ---
mkdir -p "$REF/shared-page-two-parents"
qpdf --linearize --deterministic-id --warning-exit-0 \
    "$FIX/shared-page-two-parents.pdf" "$REF/shared-page-two-parents/linearize.pdf"
echo "shared-page-two-parents/linearize.pdf"
```

**Step 3: Generate the golden.** Run the exact command from Step 2 (qpdf 11.9.0).
Verify the golden raw bytes contain TWO page objects: leaf with `/Rotate 90`
`/Parent`→A and a clone with `/Rotate 180` `/Parent`→A (same original parent),
both sharing the `/Contents` stream, param dict `/N 2`, root `/Count 2`.

**Step 4: Commit.**

```bash
git add tests/fixtures/compat/shared-page-two-parents.pdf \
        tests/golden/references/shared-page-two-parents/linearize.pdf \
        tests/golden/regenerate.sh
git commit -m "test(linearize): add shared-/Page-two-parents fixture + qpdf golden (flpdf-52md)"
```

---

## Task 2: Byte-golden test (RED)

**Files:**
- Modify: `crates/flpdf/tests/cmp_linearize_tests.rs`

**Step 1: Write the failing test** (append near the other page tests):

```rust
/// A `/Page` leaf shared by two `/Pages` parents with different inherited
/// `/Rotate` values. qpdf's cache() clones the 2nd occurrence (flpdf-52md): the
/// original leaf inherits parent A's `/Rotate 90`, the clone inherits parent B's
/// `/Rotate 180`, the clone keeps the original leaf's `/Parent` (no flatten in
/// the clone arm), and both share the `/Contents` stream.
#[test]
fn shared_page_two_parents_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "shared-page-two-parents.pdf",
        "shared-page-two-parents",
    );
}
```

**Step 2: Run to verify it FAILS.**
`cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests shared_page_two_parents -- --nocapture`
Expected: FAIL — first_diff reports a divergence (flpdf emits `/N 1` + a single
page referenced twice; golden has `/N 2` + a clone).

**Step 3: Commit the RED test.**

```bash
git add crates/flpdf/tests/cmp_linearize_tests.rs
git commit -m "test(linearize): failing byte test for shared-/Page clone (flpdf-52md)"
```

---

## Task 3: Unit tests for the clone pass (RED)

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs` (`#[cfg(test)] mod tests`)

**Step 1: Write failing unit tests.** Follow the file's existing hand-built-PDF
fixture idiom (`%PDF` + objects + xref + trailer builders). Add tests:

1. `duplicate_leaf_across_two_parents_is_cloned`: root→A(`/Rotate 90`)+
   B(`/Rotate 180`), shared leaf. After `push_inherited_attributes_to_pages`:
   object count = before+1; A's kid keeps its ref with `/Rotate 90`; B's kid is a
   NEW ref with `/Rotate 180`; the clone's `/Parent` equals the original leaf's
   `/Parent` (A); both kids' `/Contents` share the same ref.
2. `leaf_listed_twice_in_one_parent_is_cloned`: single `/Pages` with
   `/Kids [L 0 R L 0 R]`. After push: `/Kids` = `[L, clone]`, clone minted, count
   = before+1.
3. `leaf_appearing_three_times_mints_two_clones`: `/Kids [L L L]` ⇒ count =
   before+2; entries `[L, c1, c2]` all distinct.
4. `no_duplicate_leaf_mints_nothing`: ordinary 2-page tree ⇒ object count
   unchanged (pass is a no-op).
5. `reconstructed_xref_input_does_not_clone`: build a damaged PDF that forces
   xref reconstruction on `Pdf::open` (so `repair_diagnostics()` contains the
   reconstruct message) with a shared leaf; after push, NO clone minted (object
   count unchanged), matching flpdf's current drop behavior. (Assert the fixture
   actually reconstructed via `pdf.repair_diagnostics()`.)
6. `clone_pass_is_idempotent`: run `push_inherited_attributes_to_pages` twice on
   the shared-leaf doc; the 2nd run mints nothing (object count stable after the
   first run).

**Step 2: Run to verify they FAIL/compile-error** (clone not implemented yet):
`cargo test -p flpdf --lib linearization::inherited_attrs`
Expected: the new tests FAIL (no clone happens; counts don't match).

**Step 3: Commit the RED unit tests.**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "test(linearize): failing unit tests for duplicate-leaf clone pass (flpdf-52md)"
```

---

## Task 4: Implement the clone pass + gate (GREEN)

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs`

**Step 1: Add the gate + pass call at the top of
`push_inherited_attributes_to_pages`**, after resolving `pages_ref`, before the
`push_internal` walk:

```rust
// qpdf's pushInheritedAttributesToPage calls (void)cache() first, which clones
// any /Page leaf reachable more than once so the push below pushes independent
// inherited attributes to each occurrence (QPDF_pages.cc:315-317, getAllPages-
// Internal seen-set clone at :202-213). qpdf only clones when the xref was NOT
// reconstructed (:205); reconstructed inputs take a drop+flatten arm that flpdf
// does not yet implement (tracked separately), so gate the clone off for them
// to avoid a new divergence.
let reconstructed = pdf
    .repair_diagnostics()
    .entries()
    .iter()
    .any(|d| d.message.contains("reconstruct cross-reference"));
if !reconstructed {
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    resolve_duplicate_page_leaves(pdf, pages_ref, &mut seen, &mut visited, 0)?;
}
```

**Step 2: Implement `resolve_duplicate_page_leaves`.** Depth-first, matching
`getAllPagesInternal` order (recurse iff the kid dict has `/Kids`; otherwise it is
a leaf). Mint clones during the walk (so object numbers follow traversal order);
defer the parent `/Kids` rewrite to after the loop.

```rust
/// qpdf-`cache()`-equivalent: clone any `/Page` leaf reachable more than once in
/// the `/Kids` tree so the subsequent inherited-attribute push treats each
/// occurrence as an independent object. Mirrors `getAllPagesInternal`'s seen-set
/// clone (QPDF_pages.cc:202-213). A well-formed tree (no shared leaf) is a no-op.
fn resolve_duplicate_page_leaves<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    seen: &mut BTreeSet<ObjectRef>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<()> {
    if depth >= MAX_DEPTH {
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {MAX_DEPTH} at {node_ref}"
        )));
    }
    if !visited.insert(node_ref) {
        return Ok(()); // loop guard (qpdf throws; flpdf tolerates, matches PageWalk)
    }
    let Object::Dictionary(dict) = pdf.resolve(node_ref)? else {
        return Ok(());
    };
    let Some(kids) = dict.get("Kids").and_then(Object::as_array).map(<[Object]>::to_vec) else {
        return Ok(());
    };

    let mut new_kids = kids.clone();
    let mut changed = false;
    for (i, kid) in kids.iter().enumerate() {
        let Object::Reference(kid_ref) = kid else { continue };
        let Object::Dictionary(kid_dict) = pdf.resolve_borrowed(*kid_ref)? else { continue };
        if kid_dict.get("Kids").is_some() {
            resolve_duplicate_page_leaves(pdf, *kid_ref, seen, visited, depth + 1)?;
        } else if !seen.insert(*kid_ref) {
            // Duplicate leaf: clone (shallow copy of the leaf dict; indirect
            // sub-objects such as /Contents stay shared). shallowCopy keeps the
            // original leaf's /Parent (qpdf does not fix it — flatten does not
            // run in the clone arm).
            let clone = kid_dict.clone();
            let new_ref = next_object_ref(pdf)?;
            pdf.set_object(new_ref, Object::Dictionary(clone));
            seen.insert(new_ref);
            new_kids[i] = Object::Reference(new_ref);
            changed = true;
        }
    }
    if changed {
        let Object::Dictionary(mut dict) = pdf.resolve(node_ref)? else {
            return Ok(()); // cov handled: node was a dict above
        };
        dict.insert(b"Kids", Object::Array(new_kids));
        pdf.set_object(node_ref, Object::Dictionary(dict));
    }
    Ok(())
}
```

(Confirm the exact `Dictionary` API: `insert(key, value)` and `Object::as_array`
signatures already used elsewhere in this file — mirror them. `next_object_ref`
re-reads `pdf.object_refs()` max each call, so sequential clones get sequential
numbers.)

**Step 3: Run unit tests → PASS.**
`cargo test -p flpdf --lib linearization::inherited_attrs`
Expected: all pass (including the 6 new tests).

**Step 4: Run the byte-golden test → PASS.**
`cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests shared_page_two_parents`
Expected: PASS (byte-identical).

**Step 5: Commit.**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "fix(linearize): clone /Page leaves shared across /Pages parents (qpdf cache() parity, flpdf-52md)"
```

---

## Task 5: Regression + coverage gate

**Step 1: Full linearize regression** (existing goldens must still pass — proves
the no-duplicate no-op):
`cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests`
`cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests`
`cargo test -p flpdf --test linearize_classic_tests`
Expected: all PASS.

**Step 2: Broader smoke** (nothing else regressed):
`cargo test -p flpdf --lib` and `cargo test -p flpdf --test linearize_objstm_generate_tests`.

**Step 3: fmt + clippy.**
`cargo fmt --all` (my memory: CI Quality gate = `cargo fmt --check`) and
`cargo clippy -p flpdf --all-targets`.

**Step 4: Patch coverage (flpdf changed lines = 100%).** Commit first, then:
`scripts/patch-coverage.sh --base main`
(Do NOT pass `qpdf-zlib-compat` to llvm-cov — compat baseline is miniz-fixed; my
memory `llvm-cov-no-qpdf-zlib-compat`.) If any changed line is uncovered, add a
unit test or a justified `// cov:ignore:` with a one-line reason in the PR body.
Note: the clone-mint lines are exercised by the default-build unit tests (Task 3),
so they are covered WITHOUT the feature.

**Step 5: Commit any fmt/coverage follow-ups.**

```bash
git add -A
git commit -m "chore(linearize): fmt + coverage for duplicate-page clone (flpdf-52md)"
```

---

## Task 6: PR

**Step 1:** Rebase on latest main if needed (my memory
`stacked-pr-rebase-revalidate-bytegates`). Re-run the byte test after any rebase.

**Step 2:** `gh pr create` (my memory `gh-pr-create-graphql-401-use-rest`), body
covering: the qpdf oracle divergence, the clone-arm scope, the reconstructed gate,
and the explicit OUT-of-scope follow-ups (drop+flatten arm; other
getAllPagesInternal repairs) — file those as bd issues and link them.

**Step 3:** Close flpdf-52md after merge.

---

## Out-of-scope follow-ups to FILE (bd issues) during Task 6

1. drop+flatten arm for reconstructed inputs (qpdf `flattenPagesTree`).
2. other `getAllPagesInternal` repairs: direct-kid→indirect, `/Type` override,
   `/MediaBox` default, `/Resources` repair, `/Annots` validation/dedup,
   root→`/Pages` `/Parent`-chain correction.
