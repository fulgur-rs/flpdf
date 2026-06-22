# flpdf-lubb: open-document half-partition in preserve/disable linearization

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (or
> superpowers:executing-plans) to implement this plan task-by-task. Work in the worktree
> `.worktrees/flpdf-lubb-od-partition`. byte-identical-to-qpdf is the project's top
> directive — verify with the `qpdf-zlib-compat` feature against committed goldens.

**Goal:** Make `--linearize` preserve/disable-mode output byte-identical to
`qpdf 11.9.0 --linearize --deterministic-id` for inputs carrying an open-document
closure (`/OpenAction`, `/AcroForm`, `/PageMode`, `/Threads`, `/ViewerPreferences`,
trailer `/Encrypt`), by routing those objects to part4 (first half, before `/O`) in
ALL object-stream modes — not just generate.

**Architecture:** The bug is entirely in `LinearizationPlan::from_pdf`
(`crates/flpdf/src/linearization/plan.rs`). qpdf's `calculateLinearizationData`
classifies open-document objects into part4 (first half) independent of object-stream
mode (`part4 = [lc_root] ++ lc_open_document`, ascending source objgen;
QPDF_linearization.cc:1118-1182). flpdf gates that peeling on `use_generate_objstm`
(plan.rs:593/699/811/886/910) based on a **misread oracle** (the comment at
plan.rs:692-698 miscounted: the "12 first-page objects" in acroform-widget-page0-5-10
are page-dict + content + 10 Fonts; the AcroForm dict + widgets are in part4). The
renumber map (`from_plan` step 6b) and writer (pre-`/O` plain emission, writer.rs:1809)
already place `part4_open_document_plain` correctly in ALL modes — only `from_pdf`'s
gating must change. No renumber/writer logic changes (doc comments only).

**Tech Stack:** Rust; `cargo test -p flpdf`; byte goldens under
`tests/golden/references/<stem>/`; `qpdf 11.9.0` on PATH; `qpdf-zlib-compat` feature
for deflate-byte parity; `scripts/patch-coverage.sh` gate (100% changed lines on flpdf).

**Verified root-cause data (od-indirect-length, classic, disable):**
- qpdf:  2nd half = {Pages tree}; part4 (1st half) = catalog, OpenAction action, JS stream → param dict = obj 2.
- flpdf: 2nd half = {Pages tree, OpenAction action, JS stream} → param dict = obj 4.
- After fix flpdf must match qpdf: 1=Pages, 2=param, 3=catalog, 4=action, 5=JS, 6=hint, 7=page, 8=content.

These fixtures contain NO source ObjStm (grep `/ObjStm` = 0); `qpdf --linearize`
default == `--object-streams=disable` for them. flpdf `from_pdf(.., false)` targets
the same classic output, so the existing `assert_classic_byte_identical` harness applies.

---

### Task 1: RED — disable/preserve byte goldens + failing tests

**Files:**
- Modify: `tests/golden/regenerate.sh` (add a loop generating `linearize-classic.pdf`
  for the OD fixtures; mirror the existing outlines loop at regenerate.sh:653-659).
- Create (via qpdf): `tests/golden/references/<stem>/linearize-classic.pdf` for stems:
  `objstm-lin-od-indirect-length`, `objstm-lin-openaction-80-80`,
  `objstm-lin-acroform-widget-page1-page2`, `objstm-lin-acroform-widget-page1-only`,
  `objstm-lin-acroform-widget-page0-5-10`, `objstm-lin-acroform-widget-ap-stream-page0`,
  `objstm-lin-useoutline-od-shared-stream`.
- Modify: `crates/flpdf/tests/cmp_linearize_tests.rs` (add `#[test]` per stem calling
  `assert_classic_byte_identical(fixture, stem)`; this is the existing harness using
  `flpdf_linearized` = `from_pdf(.., false)` + golden `linearize-classic.pdf`, gated on
  `#![cfg(feature = "qpdf-zlib-compat")]`).

**Step 1:** Add the regenerate.sh loop:
```bash
# Open-document closure fixtures (flpdf-lubb): preserve/disable linearization must
# route /OpenAction,/AcroForm,... subtrees to part4 (first half), like qpdf.
for stem in objstm-lin-od-indirect-length objstm-lin-openaction-80-80 \
    objstm-lin-acroform-widget-page1-page2 objstm-lin-acroform-widget-page1-only \
    objstm-lin-acroform-widget-page0-5-10 objstm-lin-acroform-widget-ap-stream-page0 \
    objstm-lin-useoutline-od-shared-stream; do
    mkdir -p "$REF/$stem"
    qpdf --linearize --deterministic-id --warning-exit-0 \
        "$FIX/$stem.pdf" "$REF/$stem/linearize-classic.pdf"
    qpdf --check "$REF/$stem/linearize-classic.pdf" >/dev/null
    echo "$stem/linearize-classic.pdf"
done
```
Run it: `bash tests/golden/regenerate.sh` (or just the qpdf commands for the 7 stems).
Confirm each golden has 0 `/ObjStm` (classic) and `qpdf --check` passes.

**Step 2:** Add tests (one per stem), e.g.:
```rust
#[test]
fn od_indirect_length_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-od-indirect-length.pdf", "objstm-lin-od-indirect-length");
}
```

**Step 3:** Run RED:
`cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests`
Expected: the 7 new tests FAIL with a first-diff at the `/L` param-dict line (object
number / `/O` divergence); existing tests still pass.

**Step 4:** Commit (RED test + goldens):
`git add tests/golden crates/flpdf/tests/cmp_linearize_tests.rs && git commit -m "test(linearize): RED preserve/disable byte goldens for open-document closure (flpdf-lubb)"`

---

### Task 2: GREEN — route open-document objects to part4 in all modes

**Files:** Modify `crates/flpdf/src/linearization/plan.rs` (the `from_pdf` fn).

All five edits are interdependent (partial ungating breaks the membership invariant /
`debug_assert_eq!` at plan.rs:938); make them together, then run.

**Step 1 — compute open_document_set unconditionally (plan.rs ~593):**
```rust
// qpdf computes the open-document partition independent of object-stream mode
// (calculateLinearizationData; QPDF_linearization.cc:1118-1182), so disable/
// preserve must peel these objects to part4 (first half) exactly like generate.
let open_document_set = open_document_set(pdf)?;
// eligibility context is only consulted for the generate-mode ObjStm split below.
let elig_ctx = if use_generate_objstm {
    Some(eligibility_context(pdf)?)
} else {
    None
};
```
(Replaces the `if use_generate_objstm { .. } else { (BTreeSet::new(), None) }` block.)

**Step 2 — Step 5 peel, ungate (plan.rs ~699):**
`if use_generate_objstm && open_document_set.contains(obj_ref)` →
`if open_document_set.contains(obj_ref)`.

**Step 3 — per-page private exclusion, ungate (plan.rs ~811):**
`if use_generate_objstm && open_document_set.contains(r)` →
`if open_document_set.contains(r)`.

**Step 4 — in_first_page guard, ungate (plan.rs ~885-886):**
`let in_first_page = first_page_set.contains(&r) && !(use_generate_objstm && open_document_set.contains(&r));`
→ `let in_first_page = first_page_set.contains(&r) && !open_document_set.contains(&r);`

**Step 5 — OD routing block, handle disable (plan.rs ~910-924):**
```rust
if open_document_set.contains(&r) && !all_outline_refs.contains(&r) {
    if let Some(ctx) = elig_ctx.as_ref() {
        // generate mode: eligible OD objects pack into an ObjStm (part4_rest),
        // ineligible (streams) emit plain pre-/O.
        let obj = pdf.resolve_borrowed(r)?;
        if is_eligible_for_objstm(r, obj, ctx) {
            part4_rest.push(r);
        } else {
            part4_open_document_plain.push(r);
        }
    } else {
        // disable/preserve mode: no ObjStm; every OD object is a plain pre-/O
        // (part4) object, matching qpdf's part4 = [lc_root] ++ lc_open_document.
        part4_open_document_plain.push(r);
    }
    continue;
}
```

**Step 6 — match qpdf part4 order (after the part8/part9 loop, ~line 936):**
qpdf's `lc_open_document` is a `std::set<QPDFObjGen>` (ascending source number); the
catalog (`lc_root`) is placed separately by renumber's `root_ref` promote. Sort the
bucket to be order-stable regardless of `object_refs()` order:
```rust
part4_open_document_plain.sort_unstable_by_key(|r| r.number);
```
(No-op for generate if `object_refs()` is already ascending; safety net for disable.)

**Step 7:** Run GREEN:
`cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests --test cmp_linearize_objstm_tests`
Expected: all 7 new tests PASS; existing generate/classic tests still PASS.

**Step 8 — robustness check (broken-catalog tolerance):**
plan.rs:590-592 noted non-generate skipped the catalog traversal "to tolerate broken
catalog references". Run the FULL crate suite (Task 4 Step 1). If a disable-mode test
now errors from `open_document_set(pdf)?`, prefer matching qpdf (which also traverses);
only if a real fixture regresses, fall back gracefully (e.g. treat an open-document
resolve error as an empty set in non-generate mode) and document why.

**Step 9:** Commit:
`git add crates/flpdf/src/linearization/plan.rs && git commit -m "fix(linearize): route open-document closure to part4 in preserve/disable mode (flpdf-lubb)"`

---

### Task 3: Correct the misread-oracle comments

**Files:** `crates/flpdf/src/linearization/plan.rs`, `crates/flpdf/src/linearization/renumber.rs`.

**Step 1:** Rewrite plan.rs:692-698 to state the verified behavior: qpdf peels
open-document objects (OpenAction/AcroForm/… subtrees) to part4 (first half, before
`/O`) in ALL modes; in generate mode eligible ones additionally pack into an ObjStm.
Remove the false "qpdf keeps these as Part 2/3 first-page objects" claim and the
"12 objects" miscount. (These are `//` comments, not public doc — internal note rules
apply, not docs.rs rules.)

**Step 2:** Update plan.rs:800-813 and :878-884 comments to drop "In generate mode"
framing where the behavior now applies to all modes.

**Step 3:** Update renumber.rs step-6b doc (the "(generate mode only)" / "Ineligible
open-document plain objects (generate mode only)" lines ~277-285 and the module/header
mention) to "open-document part4 objects (plain in disable/preserve; ineligible-stream
subset in generate)". Keep it accurate; renumber/writer code is unchanged.

**Step 4:** `cargo build -p flpdf && cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests` (still green).

**Step 5:** Commit:
`git commit -am "docs(linearize): correct open-document part4 comments to verified qpdf behavior (flpdf-lubb)"`

---

### Task 4: Regression + coverage gate

**Step 1:** Full suite both feature configs:
- `cargo test -p flpdf` (default / miniz)
- `cargo test -p flpdf --features qpdf-zlib-compat`
- `cargo test -p flpdf-cli`
All green. Investigate any failure before proceeding (esp. generate-mode goldens and
any broken-catalog disable test per Task 2 Step 8).

**Step 2:** `cargo fmt --all` (CI quality gate is `cargo fmt --check`); `cargo clippy -p flpdf --all-targets` clean.

**Step 3:** Patch coverage (commit first; run without qpdf-zlib-compat):
`scripts/patch-coverage.sh --base flpdf-lubb-od-partition` (or default base). Changed
lines on `crates/flpdf` must be 100%. The new disable-mode arms in plan.rs are covered
by the Task-1 byte tests built under qpdf-zlib-compat — confirm patch-coverage (run
WITHOUT the feature per the project rule) still sees them covered via the non-feature
test paths; if an arm is only reachable under the feature, add a feature-independent
unit test on `LinearizationPlan::from_pdf(.., false)` asserting the bucket membership
(`part4_open_document_plain` contains the OD refs, second half excludes them) so the
line is covered without zlib-compat.

**Step 4:** Add a CI byte-test entry if required: per memory, qpdf-zlib-compat gated
byte tests must be listed explicitly in `ci.yml` (check whether the new test fns are
picked up by the existing `cmp_linearize_tests` invocation; that whole test target is
already run, so new `#[test]` fns inside it need no ci.yml change — verify).

**Step 5:** Commit any test/format/coverage additions.

---

### Out of scope (track separately)
- flpdf-oq7g: preserved-ObjStm *layout* (shared-id rank / container order) for inputs
  that genuinely carry ObjStm — different mechanism.
- flpdf-3g8o: `/Length` holder count direct-ization — different axis (counts equal here).
- flpdf-lz4a: multi-container within-part ordering (generate).
