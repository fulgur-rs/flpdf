# Skip /Length Edges in Renumber/Reachability Walk — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make the object-numbering / reachability walks skip a stream's indirect
`/Length` edge (matching qpdf's `removeKey("/Length")` before child enqueue), and
remove the now-subsumed pre-GC orphan-holder machinery — fixing flpdf-orv9 (a
holder kept alive via a non-`/Length` ref from a GC'd object) and the latent
ordering divergence (a holder reached via both `/Length` and non-`/Length`).

**Architecture:** qpdf unparses a stream by `removeKey("/Length")`
(QPDFWriter.cc:1442) *before* enqueueing children, so with `direct_stream_lengths`
(qpdf default == flpdf non-qdf) the `/Length` edge contributes to neither numbering
nor reachability. flpdf instead follows every stream-dict key in `collect_refs` and
patches it post-hoc with `orphaned_indirect_length_holders` (a pre-GC full-xref
scan that has the bug). We replace the patch with a faithful structural skip,
gated by a `skip_length` flag = `!options.qdf` (qdf keeps the indirect `/Length`
holder, so it must still follow the edge — writer.rs:2989-3003 maps it).

**Tech Stack:** Rust, qpdf 11.9.0 oracle, `cargo test`, `qpdf-zlib-compat` feature
for byte-identity verification, `scripts/patch-coverage.sh` gate.

**Note on TDD shape:** This is a signature-changing refactor; the three walk
functions and their tests must change together to keep the crate compiling. Each
task below leaves a compiling, green state. The *proof* of the fix is the new
regression test in Task 4 (plain unreachable `U` → holder dropped), which cannot
exist before the new API does — so the new API lands first, then is proven.

---

## Background / current call graph (do not skip)

Single shared `collect_refs` (rewrite_renumber.rs:460) follows ALL stream keys.
Three walks consume it:

| Walk | non-test caller | qdf? | desired skip_length |
|------|-----------------|------|---------------------|
| `CatalogFirstRenumber::build_excluding` | writer.rs:2702 (plain rewrite) | yes/no | `!options.qdf` |
| `GenerateRenumber::build_excluding` | writer.rs:3709 (generate ObjStm) | never (qdf forces ObjStm off, writer.rs:2820) | `true` |
| `reachable_object_set` | plan.rs:565 (linearize) | never (linearize directizes /Length, flpdf-q1j2) | `true` |

`orphaned_indirect_length_holders` (object_streams.rs:400) + its only helper
`collect_non_length_refs` (object_streams.rs ~330) compute the `excluded` set fed
to all three. Both are removed.

End-to-end safety nets that MUST stay green (behavior unchanged — holder still
dropped): `crates/flpdf/tests/orphan_indirect_length_holder_tests.rs`,
`crates/flpdf/tests/kept_indirect_length_holder_tests.rs`.

---

## Task 1: Add `skip_length` to `collect_refs`

**Files:**
- Modify: `crates/flpdf/src/rewrite_renumber.rs:460-488` (`collect_refs`)

**Step 1: Change the signature and the `Stream` arm**

Replace `collect_refs(obj, depth, f)` with `collect_refs(obj, depth, skip_length, f)`.
In the `Object::Stream(stream)` arm, when `skip_length`, skip the `Length` key
(mirrors `collect_non_length_refs`):

```rust
fn collect_refs(
    obj: &Object,
    depth: usize,
    skip_length: bool,
    f: &mut impl FnMut(ObjectRef),
) -> crate::Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(
            "plain rewrite: inline object nesting exceeds MAX_INLINE_DEPTH during \
             reference collection"
                .to_string(),
        ));
    }
    match obj {
        Object::Reference(r) => f(*r),
        Object::Array(elements) => {
            for element in elements {
                collect_refs(element, depth + 1, skip_length, f)?;
            }
        }
        Object::Dictionary(dict) => {
            for (_key, value) in dict.iter() {
                collect_refs(value, depth + 1, skip_length, f)?;
            }
        }
        Object::Stream(stream) => {
            // qpdf removes /Length before enqueueing a stream's children
            // (QPDFWriter.cc:1442); with direct stream lengths the indirect
            // /Length edge is dead in the output. `skip_length` carries that
            // `direct_stream_lengths` state (false only in qdf mode, which keeps
            // the indirect holder).
            for (key, value) in stream.dict.iter() {
                if skip_length && key == b"Length" {
                    continue;
                }
                collect_refs(value, depth + 1, skip_length, f)?;
            }
        }
        _ => {}
    }
    Ok(())
}
```

**Step 2:** Update the doc comment on `collect_refs` to mention the `/Length`
skip and the qpdf correspondence (no beads ID in doc per
`.claude/rules/pdf-rust-doc-review-patterns.md` — this is a non-pub fn so the rule
is advisory, but keep it clean).

**Step 3:** Don't compile yet (callers still pass 3 args). Proceed to Task 2 —
they land together. (If you want an intermediate checkpoint, temporarily update
the in-file callers; otherwise continue.)

---

## Task 2: Collapse `build_excluding` → `build(skip_length)` for all three walks

**Files:**
- Modify: `crates/flpdf/src/rewrite_renumber.rs`
  - `CatalogFirstRenumber::build` / `build_excluding` (lines 90-143)
  - `enqueue` (lines 426-446)
  - `reachable_object_set` (lines 169-204)
  - `GenerateRenumber::build` / `build_excluding` (lines 279-374)
  - `enqueue_gen` (lines 382-420)

**Step 1: `CatalogFirstRenumber`** — delete the `build` wrapper and the `excluded`
param; rename `build_excluding` to `build(pdf, skip_length: bool)`. Drop the
`excluded` arg from `enqueue` and its `excluded.contains` guard. Pass `skip_length`
into the `collect_refs` call:

```rust
pub(crate) fn build<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    skip_length: bool,
) -> crate::Result<Self> {
    // ... unchanged seed collection ...
    for seed in seeds {
        enqueue(seed, &mut old_to_new, &mut order, &mut queue);
    }
    while let Some(cur) = queue.pop_front() {
        let obj = pdf.resolve_borrowed(cur)?;
        collect_refs(obj, 0, skip_length, &mut |r| {
            enqueue(r, &mut old_to_new, &mut order, &mut queue);
        })?;
    }
    Ok(Self { old_to_new, order })
}
```

`enqueue` loses the `excluded` parameter (and its early-return guard).

**Step 2: `reachable_object_set`** — replace `excluded: &BTreeSet<ObjectRef>`
with `skip_length: bool`; drop the two `excluded.contains` guards; pass
`skip_length` into `collect_refs`.

**Step 3: `GenerateRenumber`** — same as Step 1: delete `build` wrapper, rename
`build_excluding` → `build(pdf, groups, skip_length)`, drop `excluded` from
`enqueue_gen` (and its guard), pass `skip_length` to `collect_refs`.

**Step 4:** Update doc comments that referenced the orphan/`excluded` mechanism to
describe the `/Length` skip instead (CatalogFirstRenumber, GenerateRenumber,
reachable_object_set). Remove the `flpdf-sqkq` references from non-pub docs.

**Step 5: Do not build yet** — call sites (Task 3) still pass the old signatures.

---

## Task 3: Update the three production call sites + remove orphan machinery

**Files:**
- Modify: `crates/flpdf/src/writer.rs` (plain 2679-2702, generate 3702-3709)
- Modify: `crates/flpdf/src/linearization/plan.rs` (37 import, 548-565)
- Modify: `crates/flpdf/src/writer/object_streams.rs` (remove
  `orphaned_indirect_length_holders` ~378-438 and `collect_non_length_refs`
  ~320-376)
- Modify: `crates/flpdf/src/linearization/writer.rs:450` (comment only)

**Step 1: plain rewrite (writer.rs ~2679-2702)** — delete the
`orphan_length_holders` block (the `if !options.qdf { orphaned_... } else { empty }`)
and call:

```rust
use crate::rewrite_renumber::CatalogFirstRenumber;
// qpdf removes a stream's /Length before enqueueing children, so the indirect
// /Length edge never affects numbering with direct stream lengths. qdf keeps the
// indirect holder, so it still follows the edge.
let renumber = CatalogFirstRenumber::build(pdf, !options.qdf)?;
```

Keep the existing explanatory comment, rewritten to describe the skip (qpdf
removeKey) rather than the orphan set.

**Step 2: generate (writer.rs ~3702-3709)** — delete the `orphan_length_holders`
line and call:

```rust
// Generate mode always writes a direct /Length (qdf forces ObjStm off), so the
// indirect /Length edge is always dead — skip it during numbering.
let renumber = GenerateRenumber::build(pdf, &groups, true)?;
```

**Step 3: linearize (plan.rs ~548-565)** — delete the `orphan_length_holders`
line and pass `true`:

```rust
// The linearized writer always emits a direct /Length (even for kept holders,
// flpdf-q1j2), so the indirect /Length edge is dead — skip it so an object
// reachable only through it is GC'd, matching qpdf.
let reachable = crate::rewrite_renumber::reachable_object_set(pdf, true)?;
```

Remove `orphaned_indirect_length_holders` from the `use` on plan.rs:37.

**Step 4: remove the machinery** — delete `orphaned_indirect_length_holders` and
`collect_non_length_refs` from object_streams.rs. Fix the `use` lists / any
`is_source_structural_container` import that becomes unused (check with the
compiler).

**Step 5: comment cleanup** — linearization/writer.rs:450 and any other comment
that names `orphaned_indirect_length_holders`: reword to "the dropped holder" /
"the /Length skip".

**Step 6: Build the crate**

Run: `cargo build -p flpdf`
Expected: compiles (tests not yet updated — `cargo build` excludes test cfg for
unit tests in the same crate, so also run `cargo test -p flpdf --no-run` and
expect test-module compile errors to fix in Task 4–5).

---

## Task 4: Adapt unit tests in rewrite_renumber.rs + add regression tests

**Files:**
- Modify: `crates/flpdf/src/rewrite_renumber.rs` tests (lines ~593-737, 872-895)

**Step 1: Mechanical signature updates**
- `CatalogFirstRenumber::build(&mut pdf)` → `build(&mut pdf, true)` (lines 593,
  605, 619, 634 — these fixtures have direct `/Length`, so `true` is correct and
  results are unchanged).
- `collect_refs(obj, 0, &mut f)` test callers (872, 884) → add `true`.

**Step 2: Convert the `build_excluding`/`excluded` drop tests to the skip API**

`build_excluding_drops_orphan_length_holder_and_renumbers_contiguously` → drop the
`excluded` set; assert the holder is dropped *by the skip alone*:

```rust
#[test]
fn build_drops_orphan_length_holder_via_length_skip_and_renumbers_contiguously() {
    let bytes =
        include_bytes!("../../../tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");
    let mut pdf = Pdf::open_mem(bytes).expect("open");
    // obj 7 is reachable ONLY via obj 6's indirect /Length; skipping that edge
    // drops it with no excluded set.
    let map = CatalogFirstRenumber::build(&mut pdf, true).expect("build");
    assert_eq!(map.len(), 6);
    assert!(map.new_for_original(ObjectRef::new(7, 0)).is_none());
    let mut news: Vec<u32> = map.pairs().map(|(new, _)| new.number).collect();
    news.sort_unstable();
    assert_eq!(news, vec![1, 2, 3, 4, 5, 6]);
}
```

Convert `reachable_object_set_skips_excluded_orphan_length_holder` similarly:
`reachable_object_set(&mut pdf, true)` (no excluded) → obj 7 absent, obj 4 present.

Convert `reachable_object_set_drops_source_linearization_artifacts` and
`reachable_object_set_includes_trailer_encrypt_dict`: replace `&BTreeSet::new()`
with `true`.

Convert `generate_build_excluding_drops_orphan_length_holder`:
`GenerateRenumber::build(&mut pdf, &[], true)` → obj 7 dropped, obj 4 present, 6
pairs.

**Step 3: NEW regression test — the flpdf-orv9 bug (plain unreachable U)**

Add to rewrite_renumber.rs tests. Build a raw PDF inline (pattern: the deleted
`orphaned_indirect_length_holders_keeps_holder_referenced_in_direct_trailer_value`
test). Layout: live page /Contents stream S1 (obj 4) with `/Length 6 0 R`; holder
obj 6 = `16`; an UNREACHABLE plain dict obj 7 `<< /Held 6 0 R >>` that is NOT in
the page tree and NOT in the trailer. Assert obj 6 is dropped and obj 7 is dropped:

```rust
#[test]
fn build_drops_length_holder_referenced_only_from_unreachable_object() {
    // flpdf-orv9: obj 7 is unreachable from /Root and references the /Length
    // holder (obj 6) via a non-/Length edge. The pre-GC orphan scan wrongly kept
    // obj 6 alive; the /Length-edge skip drops it (qpdf GCs obj 7 and directizes
    // /Length).
    let pdf_bytes = build_raw_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Count 1 /Kids [ 3 0 R ] >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>"),
        (4, b"<< /Length 6 0 R >>\nstream\nBT ET\nendstream"),
        // obj 5 intentionally absent to keep numbering simple; or include a 2nd
        // direct-length stream. Use contiguous numbers — see helper.
        (6, b"16"),
        (7, b"<< /Held 6 0 R >>"),
    ]);
    let mut pdf = Pdf::open_mem(&pdf_bytes).expect("open");
    let map = CatalogFirstRenumber::build(&mut pdf, true).expect("build");
    assert!(map.new_for_original(ObjectRef::new(6, 0)).is_none(),
        "holder reached only via /Length + unreachable U must be dropped");
    assert!(map.new_for_original(ObjectRef::new(7, 0)).is_none(),
        "the unreachable referrer U must itself be GC'd");
    // Same for the linearize universe walk.
    let mut pdf2 = Pdf::open_mem(&pdf_bytes).expect("open");
    let reachable = reachable_object_set(&mut pdf2, true).expect("walk");
    assert!(!reachable.contains(&ObjectRef::new(6, 0)));
    assert!(!reachable.contains(&ObjectRef::new(7, 0)));
}
```

Add a small `build_raw_pdf(bodies: &[(u32, &[u8])]) -> Vec<u8>` helper in the test
module (port the xref/trailer construction from the deleted object_streams test;
trailer = `<< /Size N /Root 1 0 R >>`). Handle non-contiguous object numbers by
sizing `offsets` to `max(num)+1`.

**Step 4: NEW regression test — qdf keeps the holder (skip_length = false)**

```rust
#[test]
fn build_keeps_length_holder_when_not_skipping_length() {
    // qdf mode keeps the indirect /Length holder (qpdf !direct_stream_lengths);
    // skip_length=false follows the edge so the holder is numbered.
    let bytes =
        include_bytes!("../../../tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");
    let mut pdf = Pdf::open_mem(bytes).expect("open");
    let map = CatalogFirstRenumber::build(&mut pdf, false).expect("build");
    assert!(map.new_for_original(ObjectRef::new(7, 0)).is_some(),
        "with skip_length=false the /Length holder stays numbered");
    assert_eq!(map.len(), 7);
}
```

**Step 5:** `cargo test -p flpdf --lib rewrite_renumber`
Expected: all pass.

---

## Task 5: Remove dead tests in object_streams.rs + fix remaining callers

**Files:**
- Modify: `crates/flpdf/src/writer/object_streams.rs` (tests ~1820-1990, helper
  `pdf_with_indirect_length_holder`, caller line 982)

**Step 1:** Delete the four `orphaned_indirect_length_holders_*` tests
(1837-1961) and the `collect_non_length_refs_*` nesting tests (1963+). Delete the
`pdf_with_indirect_length_holder` helper and `ref0` if they become unused (compiler
will flag).

**Step 2:** object_streams.rs:982 `GenerateRenumber::build(&mut pdf, &groups)` →
`build(&mut pdf, &groups, true)`.

**Step 3:** `cargo test -p flpdf --lib`
Expected: all pass (≈1818 baseline minus deleted tests plus new ones).

**Step 4: Commit** the whole refactor:

```bash
git add crates/flpdf/src/rewrite_renumber.rs crates/flpdf/src/writer.rs \
        crates/flpdf/src/writer/object_streams.rs \
        crates/flpdf/src/linearization/plan.rs crates/flpdf/src/linearization/writer.rs
git commit -m "fix(writer): skip /Length edges in renumber walk, drop pre-GC orphan scan (flpdf-orv9)"
```

---

## Task 6: End-to-end + byte-identity verification

**Step 1: behavioral safety nets**

Run: `cargo test -p flpdf --test orphan_indirect_length_holder_tests --test kept_indirect_length_holder_tests`
Expected: PASS (holder still dropped in plain/generate/preserve; kept-holder
linearize directization unchanged).

**Step 2: full default-feature test suite**

Run: `cargo test -p flpdf`
Expected: PASS.

**Step 3: byte-identity vs qpdf (the gate that matters)**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests --test cmp_linearize_objstm_tests`
plus any `cmp_diff_zero_tests` / `cmp_generate_objstm_tests` present.
Expected: PASS (plain rewrite + generate + linearize all byte-identical).
Per memory `llvm-cov-no-qpdf-zlib-compat`: run byte tests WITH the feature, but
coverage WITHOUT it.

**Step 4: qdf idempotence** (skip_length=false path unchanged) — run any
`*qdf*` byte/idempotence tests:
Run: `cargo test -p flpdf qdf`
Expected: PASS.

**Step 5: workspace clippy + fmt**

Run: `cargo clippy -p flpdf --all-targets && cargo fmt --check`
Expected: clean (per memory `flpdf-ci-quality-fmt-check`).

---

## Task 7: Patch-coverage gate + finalize

**Step 1:** Commit any test additions, then:

Run: `scripts/patch-coverage.sh --base main`
Expected: changed `flpdf` lines 100% covered. If the new error/skip arms show
uncovered lines, add targeted tests (do NOT use `cov:ignore` unless truly
untestable).

**Step 2: qualitative check** — confirm the new regression test actually asserts
the dropped-holder behavior (not just line execution), and the qdf-keeps test
guards the `skip_length=false` arm.

**Step 3:** Update beads:
```bash
bd update flpdf-orv9 --notes "Implemented via /Length-edge skip in collect_refs (skip_length=!qdf); removed orphaned_indirect_length_holders + collect_non_length_refs. Verified byte-identity (cmp_linearize*, qdf idempotence)."
```

**Step 4:** Use superpowers:finishing-a-development-branch.

---

## Risks / watch-items
- **qdf regression:** the ONLY path with `skip_length=false`. If qdf byte tests
  fail, the wiring of `!options.qdf` is wrong. (Generate/linearize are never qdf.)
- **Numbering order:** for a holder reached via BOTH /Length and non-/Length, the
  skip moves its number to the non-/Length position — this is the *intended* qpdf
  match. If a byte golden shifts, it is exposing this case; verify against qpdf
  output, do NOT revert.
- **Unused-import / dead-helper churn:** lean on the compiler after Task 3/5.
