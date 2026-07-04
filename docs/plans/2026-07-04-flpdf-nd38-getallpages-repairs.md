# flpdf-nd38: remaining getAllPagesInternal page-tree repairs Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or
> superpowers:subagent-driven-development) to implement this plan task-by-task.

**Goal:** Make flpdf's linearized output byte-identical to qpdf **11.9.0** on
malformed page trees that qpdf's `getAllPagesInternal` repairs, by mirroring the four
repairs 11.9.0 performs (`QPDF_pages.cc:77-138` + `getAllPages` `:50-67`): (2) `/Type`
override, (3) `/MediaBox` default, (1) direct-kid→indirect, (6) root→`/Pages`
`/Parent`-chain correction.

**Architecture:** Extend the existing `resolve_duplicate_page_leaves` pass
(`crates/flpdf/src/linearization/inherited_attrs.rs`, flpdf-52md's clone arm) into a
faithful mirror of 11.9.0 `getAllPagesInternal`, doing the per-kid repairs in qpdf's
exact order (see below). Add (6) as a pre-step in `push_inherited_attributes_to_pages`
before the pass. Keep the whole pass behind the existing `!reconstructed` guard —
reconstructed-input page-tree handling is deferred entirely to a future edge harness.

**Tech Stack:** Rust, flpdf crate, qpdf **11.9.0** oracle (`/usr/bin/qpdf`;
`regenerate.sh` pins `REQUIRED_QPDF_VERSION=11.9.0`), `qpdf-zlib-compat` feature for
byte goldens, `cargo llvm-cov` / `scripts/patch-coverage.sh` patch coverage.

**Oracle note (critical):** The line numbers in the beads issue text and in
flpdf-52md's comments (`QPDF_pages.cc:104-242`, `:202-213`, `:205`, `:63-81`) are qpdf
**12.4.0**'s layout (unreleased dev/main), NOT 11.9.0's. 11.9.0's `getAllPagesInternal`
is `QPDF_pages.cc:77-138`. 11.9.0 has **no** `/Resources` default, **no** `/Annots`
repair, and **no** `reconstructed_xref` gate anywhere (`grep reconstruct
QPDF_pages.cc` == 0 hits) — those are all 12.4.0-only. This plan targets 11.9.0 only.

---

## 11.9.0 per-kid order (the whole target, byte-identity critical)

```
(6, pre-step)  root->/Pages /Parent-chain correction   getAllPages 50-67   [no mint]
getAllPagesInternal(cur_node, media_box):
  (2i)  cur_node not isDictionaryOfType(/Pages) -> replaceKey /Type /Pages   89-92
        if !media_box: media_box = cur_node./MediaBox isRectangle            93-96
        for kid in /Kids:
          if kid hasKey /Kids: recurse (interior)                            101-102
          else (leaf):
  (3)       if !media_box && !kid./MediaBox isRectangle:
                kid./MediaBox = [0 0 612 792]                                104-112
  (1)       if !kid.isIndirect: kid = makeIndirectObject(kid); kids[i]=kid   113-118  [MINT]
              elif !seen.add(kid): clone (flpdf-52md, existing)              119-130  [MINT]
  (2l)      if kid not isDictionaryOfType(/Page): replaceKey /Type /Page     131-134
            all_pages.push(kid)
```

**Invariants that must hold across the stack:**
- Only (1) and clone MINT; both draw from the SAME running allocator (`next_clone`,
  first-free = max+1, incremented per mint) in DFS traversal order. This is flpdf-52md's
  proven byte-identical rail (`shared_page_two_parents*` goldens).
- (3) runs BEFORE (1) on the same kid → a minted indirect leaf carries the defaulted
  `/MediaBox`.
- (1) is LEAF-ONLY in 11.9.0. A direct **interior** node (has `/Kids`) is recursed
  in place, NOT made indirect.
- (2) also aligns `push_internal`'s dispatch: `push_internal` branches on
  `/Type==Pages` while `getAllPagesInternal` branches on `hasKey(/Kids)`. Forcing
  `/Type` to match structure (repair runs first, push second) makes them agree, so
  `push_internal` needs no change of its own.
- Well-formed tree ⇒ every repair is a no-op (no `replaceKey`, no mint) ⇒ existing
  goldens unaffected ⇒ zero regression.

---

## Shared conventions (every PR/repair follows this recipe)

Each repair is ONE stacked PR (gh-stack). Stack order:
`(2) /Type` → `(3) MediaBox` → `(1) direct→indirect` → `(6) root correction`.
Each PR = the flpdf-52md 6-task shape:

1. **Fixture + golden** — generate a minimal classic `%PDF-1.4` fixture in
   `tests/fixtures/compat/<stem>.pdf` that triggers ONLY this repair (everything else
   well-formed) with a **python generator** in `docs/plans/tools/gen_<stem>.py`
   (model on the existing `gen_*.py`; the generator writes objects then computes xref
   byte offsets programmatically — **do NOT hand-compute offsets**, a one-byte error
   silently changes the file). Add a `regenerate.sh` block (near the
   `shared-page-two-parents` block, `tests/golden/regenerate.sh:1073`). Generate the
   golden with **qpdf 11.9.0** `--linearize --deterministic-id --warning-exit-0` into
   `tests/golden/references/<stem>/linearize.pdf`.
   **Two mandatory fixture checks (advisor):**
   (a) `qpdf --check <fixture>` prints exactly the expected repair warning (and NO
   reconstruction/`WARNING: ... file is damaged` message).
   (b) The fixture is NOT reconstructed by flpdf: a quick `Pdf::open` +
   `pdf.repair_diagnostics().entries().is_empty()` assertion (add it as the first
   line of this repair's byte or unit test). RATIONALE: the whole pass sits behind the
   `!reconstructed` gate — a fixture that accidentally trips xref reconstruction makes
   flpdf skip the repair, so a correct implementation would leave the RED byte test
   red and send you debugging the wrong thing. Commit.
2. **Byte-golden test (RED)** — add `#[test] fn <stem>_byte_identical_to_qpdf()` calling
   `assert_linearize_byte_identical("<stem>.pdf", "<stem>")` in
   `crates/flpdf/tests/cmp_linearize_tests.rs` (already in the CI byte list at
   `.github/workflows/ci.yml:157` — **no ci.yml change**). Run with `--features
   qpdf-zlib-compat` → verify it FAILS. Commit.
3. **Unit tests (RED)** — add tests to `inherited_attrs.rs`'s `#[cfg(test)] mod tests`
   using the file's hand-built-PDF idiom. Run default build → verify FAIL. Commit.
4. **Implement (GREEN)** — extend the pass. Run unit tests + byte test → PASS. Commit.
5. **Regression + coverage** — full `cmp_linearize_tests`, `cmp_linearize_objstm_tests`,
   `linearize_classic_tests`, `-p flpdf --lib` all PASS; `cargo fmt --all` (CI Quality
   gate = `cargo fmt --check`); `cargo clippy -p flpdf --all-targets`; then (after
   commit) `scripts/patch-coverage.sh --base <parent-branch>` → flpdf changed lines
   100% (do NOT pass `qpdf-zlib-compat` to llvm-cov — compat baseline is miniz-fixed).
   Commit any fmt/coverage follow-ups.
6. **PR** — rebase on the parent branch, re-run the byte test, `gh pr create` (REST if
   GraphQL 401) with `--base <parent-branch>`. Body: the 11.9.0 repair, the fixture's
   malformation, the exact qpdf warning, and the scope note (11.9.0-only; edge/12.x
   deferred).

**Key file locations:**
- Pass + entry: `crates/flpdf/src/linearization/inherited_attrs.rs`
  (`push_inherited_attributes_to_pages` ~42; `resolve_duplicate_page_leaves` ~251;
  `next_object_ref` ~326; `MAX_DEPTH` ~33; existing `!reconstructed` gate ~61-66).
- Push call sites (both re-run the pass; must stay idempotent): `plan.rs`, `writer.rs`
  (search `push_inherited_attributes_to_pages`).
- Byte test + helper: `cmp_linearize_tests.rs` (`assert_linearize_byte_identical` ~90).
- Golden generator: `tests/golden/regenerate.sh` (`FIX`/`REF` at 13-14; append near
  1073). Fixture dir `tests/fixtures/compat/`; golden `tests/golden/references/<stem>/`.

---

## PR 1 — Repair (2): `/Type` override (interior→/Pages, leaf→/Page)

**Rename:** In this PR rename `resolve_duplicate_page_leaves` → `repair_page_tree`
(and update the call site + doc comment) to reflect its expanded role as the
11.9.0 `getAllPagesInternal` mirror. Keep the existing clone arm behavior unchanged.

### Task 1: Fixture + qpdf golden

**Files:**
- Create: `tests/fixtures/compat/mistyped-page-tree.pdf`
- Modify: `tests/golden/regenerate.sh`
- Create: `tests/golden/references/mistyped-page-tree/linearize.pdf`

**Step 1: Generate the fixture** with `docs/plans/tools/gen_mistyped_page_tree.py`
(model on an existing `gen_*.py`; write objects then compute xref offsets in code — do
NOT hand-compute). A well-formed 1-page classic PDF EXCEPT: an interior `/Pages` node
whose `/Type` is wrong (carries an inheritable `/Rotate` so the override demonstrably
matters to the push), and the leaf whose `/Type` is wrong. All kids indirect; leaf has
`/MediaBox`+`/Resources`; no `/Annots`; no direct `/Outlines`. Object layout:

```
1 Catalog     << /Type /Catalog /Pages 2 0 R >>
2 root /Pages  << /Type /Pages /Kids [3 0 R] /Count 1 >>
3 interior     << /Type /Foo   /Parent 2 0 R /Kids [4 0 R] /Count 1 /Rotate 90 >>   % /Type wrong (should be /Pages)
4 leaf         << /Type /Bar   /Parent 3 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 5 0 R >>  % /Type wrong (should be /Page)
5 content      << /Length N >> stream ... endstream
```

The generator emits `%PDF-1.4`, binary comment, objects, `xref` (offsets computed in
code), `trailer`, `startxref`, `%%EOF`. Verify BOTH:
- `qpdf --check tests/fixtures/compat/mistyped-page-tree.pdf` prints the two
  `/Type key should be /Pages but is not; overriding` / `...should be /Page...`
  warnings and NO reconstruction/damage warning.
- flpdf does NOT reconstruct: `Pdf::open` then
  `assert!(pdf.repair_diagnostics().entries().is_empty())` (fold into Task 3's first
  unit test). If it reconstructs, the `!reconstructed` gate skips the repair and the
  byte test can never go green — fix the fixture, not the code.

**Step 2: Add the regenerate.sh block** (near line 1073):

```bash
# --- mistyped-page-tree: interior node /Type != /Pages and leaf /Type != /Page;
# qpdf 11.9.0 getAllPagesInternal overrides both (QPDF_pages.cc:89-92, 131-134)
# (flpdf-nd38 repair 2). ---
mkdir -p "$REF/mistyped-page-tree"
qpdf --linearize --deterministic-id --warning-exit-0 \
    "$FIX/mistyped-page-tree.pdf" "$REF/mistyped-page-tree/linearize.pdf"
echo "mistyped-page-tree/linearize.pdf"
```

**Step 3: Generate the golden** with qpdf 11.9.0 (run the Step-2 command). Confirm the
golden's obj for node 3 is `/Type /Pages` and the leaf is `/Type /Page`, and `/Rotate
90` is pushed to the leaf (interior node stripped of `/Rotate`).

**Step 4: Commit.**
```bash
git add tests/fixtures/compat/mistyped-page-tree.pdf \
        tests/golden/references/mistyped-page-tree/linearize.pdf \
        tests/golden/regenerate.sh
git commit -m "test(linearize): add mistyped /Type page-tree fixture + qpdf golden (flpdf-nd38)"
```

### Task 2: Byte-golden test (RED)

**Files:** Modify `crates/flpdf/tests/cmp_linearize_tests.rs`.

**Step 1:** Append near `shared_page_two_parents_*`:
```rust
/// An interior /Pages node whose /Type is not /Pages and a leaf whose /Type is not
/// /Page. qpdf 11.9.0's getAllPagesInternal overrides both /Type keys
/// (QPDF_pages.cc:89-92, 131-134); the corrected interior node then has its inherited
/// /Rotate pushed down to the leaf (flpdf-nd38 repair 2).
#[test]
fn mistyped_page_tree_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("mistyped-page-tree.pdf", "mistyped-page-tree");
}
```

**Step 2:** `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests mistyped_page_tree -- --nocapture` → Expected FAIL (flpdf emits the wrong `/Type` and — because `push_internal` bails on the non-`/Pages` interior node — never pushes `/Rotate`, so multiple bytes diverge).

**Step 3: Commit** the RED test.

### Task 3: Unit tests (RED)

**Files:** Modify `inherited_attrs.rs` `#[cfg(test)] mod tests`.

**Step 1:** Add tests (hand-built-PDF idiom):
1. `interior_type_not_pages_is_overridden`: root→interior(`/Type /Foo`,`/Kids`,`/Rotate 90`)→leaf. After `push_inherited_attributes_to_pages`: interior node's `/Type` == `Pages`; `/Rotate` pushed to leaf; interior `/Rotate` stripped.
2. `leaf_type_not_page_is_overridden`: leaf with `/Type /Bar` ⇒ after pass, leaf `/Type` == `Page`.
3. `correct_types_unchanged_no_mint`: well-formed tree ⇒ no `/Type` mutation, object count unchanged (no-op).
4. `type_missing_is_set`: interior node with NO `/Type` key ⇒ set to `Pages`; leaf with no `/Type` ⇒ set to `Page`.

**Step 2:** `cargo test -p flpdf --lib linearization::inherited_attrs` → Expected FAIL.

**Step 3: Commit** the RED unit tests.

### Task 4: Implement (GREEN)

**Files:** Modify `inherited_attrs.rs`.

**Step 1: Rename** `resolve_duplicate_page_leaves` → `repair_page_tree`; update the
call site (~74) and doc comment (describe the full 11.9.0 `getAllPagesInternal` mirror,
cite `QPDF_pages.cc:77-138`). **While here, fix the stale 12.4.0 line refs** in this
file's comments — `QPDF_pages.cc:202-213` (clone), `:315-317` (cache), `:355-360`
(makeIndirectObject), `:298-410`, `:205` — they are 12.4.0's layout and are
known-wrong-version. Re-anchor to 11.9.0: clone at `:119-130`, `cache()`/getAllPages at
`:39-75`, push (in a DIFFERENT file) at `QPDF_optimization.cc:127-156`. Do NOT leave a
mix of versions in the comments.

**Step 2: Add (2i)** at the top of `repair_page_tree`, right after resolving the node
dict: if the node's `/Type` name is not `Pages`, set `dict.insert("Type",
Object::Name(b"Pages".to_vec()))` and mark the dict dirty. (This runs for the root
pages node and every recursed interior node.)

**Step 3: Add (2l)** in the leaf branch. After the existing first-seen/clone decision,
resolve the FINAL kid (original ref or the freshly minted clone), and if its `/Type`
name is not `Page`, set it to `Page` and `set_object` it back. Apply to BOTH the
first-occurrence leaf and a cloned leaf (11.9.0 overrides `/Type` after the clone
decision, line 131-134). NOTE: currently the pass only mutates leaves when cloning —
now EVERY leaf's `/Type` is checked; a first-occurrence leaf that needs no override
stays untouched (no `set_object`) to preserve the no-op property.

**Step 4:** Use the existing `is_dictionary_of_type`-style check or a small local
helper `type_name_is(dict, b"Pages")` (resolve `/Type` to a `Name`; missing/other ⇒
false ⇒ override). Mirror qpdf's `isDictionaryOfType` (dict AND `/Type` == the name).

**Step 5:** `cargo test -p flpdf --lib linearization::inherited_attrs` → PASS. Then
`cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests mistyped_page_tree` → PASS.

**Step 6: Commit** `fix(linearize): override page-tree /Type keys (qpdf getAllPagesInternal parity, flpdf-nd38)`.

### Task 5: Regression + coverage
Per Shared conventions step 5. `--base main` for patch-coverage (PR1 is the stack base).

### Task 6: PR
`gh pr create --base main`. Body: repair (2), the 11.9.0 override + push-dispatch
alignment, scope note.

---

## PR 2 — Repair (3): `/MediaBox` default `[0 0 612 792]`

Stacked on PR1 (`--base fix/nd38-type-override`). Adds a `media_box: bool` param to
`repair_page_tree` threaded exactly like qpdf (`93-96`: `media_box ||=
cur_node./MediaBox isRectangle`; passed into recursion). In the leaf branch, BEFORE the
direct/clone handling, `if !media_box && !is_rectangle(leaf./MediaBox)` set
`/MediaBox = [0 0 612 792]` (direct 4-integer array).

- **Helper:** `is_rectangle(pdf, value) -> bool`: resolve the value (follow refs, as
  qpdf `getKey` does), true iff `Object::Array` of exactly 4 numeric (`Integer`/`Real`)
  elements.
- **Fixture** `missing-mediabox-leaf.pdf`: leaf with NO `/MediaBox` and NO ancestor
  `/MediaBox` anywhere ⇒ qpdf mints `[0 0 612 792]` on the leaf. `qpdf --check` warning:
  `... MediaBox is undefined; setting to letter / ANSI A`.
- **Second byte fixture (or unit test) `inherited-mediabox-leaf.pdf`:** ancestor SUPPLIES
  `/MediaBox` ⇒ leaf gets NO default (inheritance wins). Verifies the `media_box` guard.
- **Unit tests:** default-mints-when-absent; ancestor-mediabox-suppresses-default;
  invalid-mediabox (wrong length/type) → default; valid-rectangle → untouched.
- Coverage `--base fix/nd38-type-override`.

## PR 3 — Repair (1): direct leaf kid → `makeIndirectObject`

Stacked on PR2. In the leaf branch, mirror `113-118`: a `/Kids` entry that is a DIRECT
dictionary (not a `Reference`) → mint via the shared `next_clone` allocator, `set_object`
the dict, rewrite the `/Kids` entry to the new ref. Mutually exclusive with clone
(`if direct … elif dup … `).

**Scope the fixture to a direct LEAF only (advisor).** A direct *interior* node (direct
dict WITH `/Kids`) would need a non-ref recursion path the current ref-keyed pass does
not have, is exotic, and has NO qpdf golden to byte-verify against. Do NOT build that
machinery. Either handle direct-interior recurse-in-place only if it falls out trivially
AND you can golden it, or explicitly note it as a known limitation in the PR body and
the pass doc comment. The issue's repair (1) and qpdf's warning (`kid N is direct;
converting to indirect`) are about the leaf case — that is what PR3 delivers and verifies.

- **Order:** (3) MediaBox default applies to the direct dict BEFORE minting, so the
  minted object carries the defaulted MediaBox (covered by an existing/added unit test).
- **Fixture** `direct-leaf-kid.pdf`: `/Pages /Kids [<< /Type /Page /MediaBox … /Contents
  R >>]` with the leaf inline (direct). `qpdf --check` warning: `kid 0 (from 0) is
  direct; converting to indirect`.
- **Unit tests:** direct-leaf-minted + `/Kids` rewritten to new ref; minted object
  number = shared allocator order; direct-leaf-lacking-mediabox → minted object carries
  `[0 0 612 792]` (order (3)-before-(1)). Direct-interior handling: only if trivially
  covered AND goldenable — otherwise documented as a limitation, no test.
- Coverage `--base fix/nd38-mediabox` (parent).

## PR 4 — Repair (6): root→`/Pages` `/Parent`-chain correction

### Task 0 (BLOCKING, do FIRST): verify root→`/Pages` ref propagation (advisor)

52md proves that mutations to page LEAVES inside `push_inherited_attributes_to_pages`
reach linearize output. It does NOT prove it for repair (6), which rewrites the
CATALOG's `/Pages` — a root-level ref other stages may read independently and early.
Before implementing (6): grep every read of root→`/Pages` across the linearize pipeline
(`plan.rs`, `writer.rs`, `renumber.rs`, `part1.rs`, `hint_page.rs`, `hint_shared.rs`,
`hint_stream.rs`, `check.rs`) and confirm each occurs AFTER
`push_inherited_attributes_to_pages` OR re-resolves through the catalog each time. If
any consumer captured the OLD `/Pages` ref before the pass, (6) cannot be byte-identical
no matter how faithfully it mirrors `getAllPages:50-67` — the correction must be applied
where all consumers observe it (or the earliest consumer). This grep discriminates PR4
success; do not build (6) until it passes.

### Task 1+: implement

Stacked on PR3. Add a PRE-STEP at the top of `push_inherited_attributes_to_pages`
(before the `!reconstructed` gate / `repair_page_tree` call), mirroring
`getAllPages:50-67`:

```
pages = root./Pages (resolved ref)
seen = {}
changed = false
loop while resolved(pages) is a Dictionary AND has a /Parent:
    if !seen.insert(pages_ref): break            // loop guard
    changed = true
    pages_ref = the /Parent ref
if changed: set root./Pages = pages_ref; set_object(root)
```

Use the corrected `pages_ref` for both `repair_page_tree` and `push_internal`. No mint.

- **Fixture** `root-pages-points-into-tree.pdf`: `/Root /Pages` points at a node that
  HAS a `/Parent` (e.g. root→intermediate `/Pages` whose `/Parent` is the true root, or
  root→first-page-leaf whose `/Parent` chain reaches the real root). qpdf walks `/Parent`
  up and rewrites `/Root /Pages`. `qpdf --check` warning: `document page tree root (root
  -> /Pages) doesn't point to the root of the page tree; attempting to correct`.
- **Unit tests:** root corrected to the true root ref; loop in the `/Parent` chain →
  break (no infinite loop, tolerated); already-correct root → no change (no-op).
- Coverage `--base fix/nd38-direct-indirect` (parent).

---

## Out of scope (do NOT implement here)

- (4) `/Resources`→empty-dict and (5) `/Annots` repair — **12.4.0-only**; absent from
  11.9.0's linearize path. Implementing them would DIVERGE from the 11.9.0 oracle.
- Reconstructed-input page-tree handling (whether 11.9.0's always-clone should replace
  flpdf-52md's `!reconstructed` gate; flpdf-zy9t's 12.4.0 drop+flatten). Deferred to a
  future edge/12.x harness — tracked in **flpdf-gs9q**. This plan leaves the
  `!reconstructed` gate untouched.
