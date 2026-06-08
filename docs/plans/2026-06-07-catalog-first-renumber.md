# Catalog-First Renumber for Plain Rewrite (byte-identical to qpdf) — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `flpdf rewrite --static-id` (plain, non-linearized) assign output object
numbers in qpdf's exact Catalog-first BFS order, so the three compat fixtures
(`one/two/three-page.pdf`) become byte-identical to `qpdf --static-id` output (after
`/ID` elision), closing flpdf-9hc.32.

**Architecture:** Add a deterministic *numbering walk* that reproduces qpdf's
`enqueueObjectsStandard` order: seed from the trimmed trailer (`/Root` first, then the
remaining trailer indirect refs in sorted-key order, dropping `/ID` `/Encrypt` `/Prev` —
this is what places `/Info` at object 2), then breadth-first enqueue every referenced
object on first encounter, descending into each object via `Dictionary::iter()`
(already lexicographic — `Dictionary` is a `BTreeMap`, matching qpdf's `std::map`) and
arrays in element order. New numbers are the visitation order `1..=N`. Thread the
resulting map through `write_pdf_full_rewrite`: build a `Vec<(new_ref, old_ref)>` sorted
by new number, use it in place of `object_refs`, rewrite each emitted object's internal
references via the map, and remap the trailer `/Root` / `/Info`. The existing
`sort_by_key((number, generation))` then emits in qpdf order for free.

**Tech Stack:** Rust, `crates/flpdf` (library), `crates/flpdf-cli` (golden harness),
`qpdf` 11.9.0 oracle, `BLESS=1` golden re-bless.

---

## Ground truth (qpdf object order — captured empirically, this is the test oracle)

Captured via `qpdf --static-id --object-streams=disable <fixture> out.pdf` then
`qpdf --show-object=N`:

| fixture     | new# → object |
|-------------|----------------|
| one-page    | 1=Catalog, 2=Info, 3=Pages, 4=Page, 5=Contents(stream), 6=Resources/Font dict `<< /F1 7 0 R >>`, 7=Font |
| two-page    | 1=Catalog, 2=Info, 3=Pages, 4=Page1, 5=Page2, 6=Contents1, 7=Font dict, 8=Contents2, 9=Font |
| three-page  | 1=Catalog, 2=Info, 3=Pages, 4=Page1, 5=Page2, 6=Page3, 7=Contents1, 8=Font dict, 9=Contents2, 10=Contents3, 11=Font |

This proves: **BFS** (both pages numbered before any page content), **sorted dict keys**
(`/Contents` < `/Resources`, so Contents precedes the Font dictionary),
**first-encounter-wins** for shared objects (the single Font dictionary is numbered on
Page 1 and reused by Pages 2 and 3), **`/Info` seeded at 2** (not reachable from the
Catalog — it comes from the trailer seed).

Confirmed against qpdf source (`QPDFWriter::enqueueObjectsStandard` / `enqueue`,
deepwiki): `/Root` enqueued first; remaining `trimmed_trailer()` refs next (trailer
trimmed of `/ID` `/Encrypt` `/Prev`); breadth-first queue; `std::map` key order;
`o.renumber == 0` first-encounter assignment; **unreferenced objects are dropped** by
default (no `--preserve-unreferenced`).

---

## Key facts the implementer must not re-derive

- `Dictionary` (`object.rs:551`) is `BTreeMap<Vec<u8>, Object>`; `iter()` (`object.rs:588`)
  yields keys in lexicographic byte order. **The numbering walk gets qpdf's key order for
  free** — do NOT add a second sort, but do NOT rely on insertion order either.
- `write_pdf_full_rewrite` (`writer.rs:2151`) is the plain + `--qdf` full-rewrite writer.
  Its signature is `fn write_pdf_full_rewrite<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, ...)`
  — `pdf` is already `&mut`. Incremental update is a different path (`writer.rs:539`
  dispatches). Linearization has its own writer (`linearization/writer.rs`) — **do not touch it**.
- `Pdf<R: Read + Seek>` (`reader.rs:40`). `resolve(&mut self, ObjectRef) -> Result<Object>`
  (`reader.rs:901`) takes **`&mut self`** (lazy load), so the numbering walk and
  `CatalogFirstRenumber::build` must take `&mut Pdf<R>`. `root_ref(&self) -> Option<ObjectRef>`
  (`reader.rs:853`) and `trailer(&self) -> &Dictionary` (`reader.rs:738`) are `&self`.
  `Pdf::open(reader)` takes a reader, not a path; `Pdf::open_mem(&[u8])` (`reader.rs:1224`)
  is the in-memory convenience.
- Existing reusable pattern: `linearization/writer.rs:377` `renumber_object(&Object,
  &RenumberMap)` recursively rewrites refs. We mirror it for the plain map (different map
  type), erroring on any ref absent from the map (dangling-ref signal).
- Object-0 / deleted skip already in place (`writer.rs:2413-2425`, flpdf-9hc.31, merged).
- Per-object encryption keys derive from the object number (`encrypt_strings_in_object_for_writer`,
  `encrypt_stream_payload_for_writer`, called at `writer.rs:2448-2452`, `2523-2537` with
  `*object_ref`). After renumber these MUST receive the **new** ref or encrypted output is
  unreadable. The 3 acceptance fixtures are unencrypted, but this is a merge-gate.
- Trailer is `pdf.trailer().clone()` (`writer.rs:2724`); `/Root` is set explicitly to
  `root_ref` (`2727`), `/Info` survives from the clone as the ORIGINAL ref → must be remapped.
  `root_ref` comes from `pdf.root_ref()` (`writer.rs:2166`).
- `object_count = max emitted number + 1` (`writer.rs:2702`); classic table fills gaps with
  `f` rows (`2715-2720`). Contiguous `1..=N` new numbers ⇒ no gaps ⇒ matches qpdf.

---

## Decisions (explicit, per design "full mimicry")

1. **Unreferenced objects dropped.** The BFS numbers only reachable objects; unreachable
   ones are not emitted. Matches qpdf default; is a behavior change from flpdf's
   "preserve all live objects". Verify no existing test asserts unreferenced preservation
   before blessing (Task 6).
2. **QDF re-blesses too** (shares the loop). qpdf renumbers in `--qdf` as well, so this
   moves qdf toward parity. **Byte-identity to `qpdf --qdf` is NOT the acceptance gate** —
   keep `--qdf` output valid and re-blessed; do not rabbit-hole.
3. **Scope of renumber:** applies to `write_pdf_full_rewrite` only (plain + qdf + objstm +
   encrypt all share it). Acceptance fixtures exercise only plain.

---

## Task 1: New module skeleton + numbering-walk struct (no writer wiring yet)

**Files:**
- Create: `crates/flpdf/src/rewrite_renumber.rs`
- Modify: `crates/flpdf/src/lib.rs` (add `mod rewrite_renumber;` near the other `mod`
  declarations — confirm visibility; `pub(crate)` is enough, it is not a public API)

**Step 1:** Declare the module. Add `mod rewrite_renumber;` (or `pub(crate) mod`) to
`lib.rs`. Build to confirm it compiles empty.
Run: `cargo build -p flpdf` → Expected: PASS.

**Step 2:** Write the struct + builder *signature only* (no algorithm yet), so tests can
reference it:

```rust
//! Catalog-first object renumbering for the plain full-rewrite writer.
//!
//! Reproduces qpdf's `QPDFWriter::enqueueObjectsStandard` numbering order so that
//! non-linearized `rewrite` output is byte-identical to `qpdf --static-id`.
use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::HashMap;
use std::io::{Read, Seek};

/// Bijective map from original object refs to new (Catalog-first) numbers.
pub(crate) struct CatalogFirstRenumber {
    /// original ref → new ref (generation forced to 0).
    old_to_new: HashMap<ObjectRef, ObjectRef>,
    /// emission order: index i holds the original ref assigned new number i+1.
    order: Vec<ObjectRef>,
}

impl CatalogFirstRenumber {
    pub(crate) fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        self.old_to_new.get(&original).copied()
    }
    /// Number of objects that received a new number (== highest new number).
    pub(crate) fn len(&self) -> usize { self.order.len() }
    /// `(new_ref, old_ref)` pairs in ascending new-number order.
    pub(crate) fn pairs(&self) -> impl Iterator<Item = (ObjectRef, ObjectRef)> + '_ {
        self.order.iter().enumerate()
            .map(|(i, &old)| (ObjectRef::new(i as u32 + 1, 0), old))
    }
}
```

**Step 3:** Commit.

```bash
git add crates/flpdf/src/rewrite_renumber.rs crates/flpdf/src/lib.rs
git commit -m "feat(writer): scaffold CatalogFirstRenumber module [flpdf-9hc.32]"
```

---

## Task 2: TDD the numbering walk against the captured oracle (THE make-or-break task)

This task proves the order is correct *in isolation*, before any writer edit. Do not
proceed to Task 3 until all three fixtures pass.

**Files:**
- Modify: `crates/flpdf/src/rewrite_renumber.rs` (add `build` + `#[cfg(test)]` tests)

**Step 1: Write the failing test** (in `rewrite_renumber.rs`). Helper loads a fixture into
memory, builds the map, and asserts the `/Type` (or "stream"/"dict" tag) at each new
number matches the oracle table. Fixtures live at workspace-root `tests/fixtures/compat/`;
from the `crates/flpdf` crate the committed pattern (see `filespec_helper.rs:1850`) is
`concat!(env!("CARGO_MANIFEST_DIR"), "/../..", "/tests/fixtures/compat/<f>.pdf")`. Simplest
for a unit test is `include_bytes!` (compile-time, no runtime path) + `Pdf::open_mem`.
Note: `build` and `resolve` take `&mut Pdf`.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pdf;

    // Returns a tag for each new number 1..=N: "/Catalog","/Pages","/Page","/Font",
    // "stream", or "dict" (no /Type, e.g. /Info or a resources sub-dict).
    fn tags_in_order(bytes: &[u8]) -> Vec<String> {
        let mut pdf = Pdf::open_mem(bytes).unwrap();
        let map = CatalogFirstRenumber::build(&mut pdf).unwrap();
        let order: Vec<ObjectRef> = map.pairs().map(|(_new, old)| old).collect();
        order.into_iter().map(|old| {
            let obj = pdf.resolve(old).unwrap(); // &mut pdf; map already built/owned
            classify(&obj)
        }).collect()
    }

    const ONE: &[u8] = include_bytes!("../../../tests/fixtures/compat/one-page.pdf");
    // TWO, THREE similarly.

    #[test]
    fn one_page_matches_qpdf_order() {
        assert_eq!(
            tags_in_order(ONE),
            ["/Catalog", "dict", "/Pages", "/Page", "stream", "dict", "/Font"]
        );
        // obj 2 is /Info (plain dict, no /Type) → "dict"; obj 6 is the
        // resources /Font dict (no /Type) → "dict".
    }
    // two_page_matches_qpdf_order, three_page_matches_qpdf_order similarly.
}
```

Run: `cargo test -p flpdf rewrite_renumber -- --nocapture`
Expected: FAIL (`build` unimplemented / `todo!()`).

**Step 2: Implement `build`** — the numbering walk:

```rust
impl CatalogFirstRenumber {
    // `&mut Pdf<R>` because `resolve` is `&mut self` (lazy load). `root_ref`/`trailer`
    // are `&self` — call them BEFORE the resolve loop (or clone the needed bits) to
    // avoid overlapping borrows.
    pub(crate) fn build<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Self> {
        use std::collections::VecDeque;
        let mut old_to_new: HashMap<ObjectRef, ObjectRef> = HashMap::new();
        let mut order: Vec<ObjectRef> = Vec::new();
        let mut queue: VecDeque<ObjectRef> = VecDeque::new();

        // enqueue(): first-encounter assignment (qpdf `o.renumber == 0`).
        // Implemented inline because it mutates three locals.
        let mut enqueue = |r: ObjectRef,
                           map: &mut HashMap<ObjectRef, ObjectRef>,
                           ord: &mut Vec<ObjectRef>,
                           q: &mut VecDeque<ObjectRef>| {
            // Normalize: qpdf keys on object number; flpdf refs carry generation.
            if map.contains_key(&r) { return; }
            let new_num = ord.len() as u32 + 1;
            map.insert(r, ObjectRef::new(new_num, 0));
            ord.push(r);
            q.push_back(r);
        };

        // Seed: /Root first, then remaining trailer indirect refs in sorted-key
        // order, dropping /ID /Encrypt /Prev (trimmed_trailer). Dictionary::iter()
        // is already lexicographic, so iterating the trailer dict yields sorted keys.
        let root = pdf.root_ref().ok_or_else(|| /* Error::Unsupported "no /Root" */)?;
        enqueue(root, &mut old_to_new, &mut order, &mut queue);
        for (key, value) in pdf.trailer().iter() {
            if matches!(key, b"ID" | b"Encrypt" | b"Prev" | b"Root" | b"Size") { continue; }
            if let Object::Reference(r) = value {
                enqueue(*r, &mut old_to_new, &mut order, &mut queue);
            }
        }

        // BFS: dequeue, resolve, enqueue referenced objects in content order.
        while let Some(cur) = queue.pop_front() {
            let obj = pdf.resolve(cur)?; // owned Object (do NOT clone)
            collect_refs(&obj, &mut |r| {
                enqueue(r, &mut old_to_new, &mut order, &mut queue)
            });
        }

        Ok(Self { old_to_new, order })
    }
}

// Walk an object's content, invoking `f` on every Object::Reference in
// Dictionary::iter() (lexicographic) + array (element) order. Streams: walk the
// dict only. Scalars: nothing. NO depth recursion limit needed here because we
// only descend into *direct* (inline) children — indirect refs are enqueued, not
// recursed — so the structural depth is bounded by inline nesting, not the object
// graph. Still, guard inline nesting with a depth cap to satisfy the graph-walk
// rule in .claude/rules (DoS on pathological inline nesting).
fn collect_refs(obj: &Object, f: &mut impl FnMut(ObjectRef)) { /* ... */ }
```

**Important details to get right (each can break cmp-diff-0):**
- Iterate `Dictionary::iter()` directly — it is sorted. Iterate arrays in element order
  (do NOT sort arrays).
- Seed loop must skip `/Root` (already enqueued) and non-refs; the `trimmed_trailer`
  exclusions are `/ID` `/Encrypt` `/Prev`. `/Size` is an integer, harmless, skip anyway.
- `enqueue` keys on the full `ObjectRef`. If the same object is reached via refs with
  differing generation, qpdf keys on number only — but flpdf fixtures use generation 0
  throughout; if the suite later surfaces mixed generations, normalize the key to number.
  Note this in a comment, don't over-engineer now.
- Do NOT clone resolved objects (`.claude/rules` #1): `resolve` returns owned; walk by ref.

Run: `cargo test -p flpdf rewrite_renumber -- --nocapture`
Expected: PASS for all three fixtures.

**Step 3: Add the dangling-ref / unreferenced observation tests.** Assert `len()` equals
the qpdf object count for each fixture (7 / 9 / 11). Run → PASS.

**Step 4: Commit.**

```bash
git add crates/flpdf/src/rewrite_renumber.rs
git commit -m "feat(writer): Catalog-first numbering walk matching qpdf order [flpdf-9hc.32]"
```

---

## Task 3: Reference-rewrite helper for the plain map

**Files:**
- Modify: `crates/flpdf/src/rewrite_renumber.rs`

**Step 1: Write failing test** — a `renumber_refs(&Object, &map)` that rewrites every
`Object::Reference` to its new ref and errors on a ref absent from the map. Test on a
small hand-built dict/array/stream.

**Step 2: Implement** mirroring `linearization/writer.rs:377` `renumber_object`, but
against `CatalogFirstRenumber::new_for_original`, returning
`Error::Unsupported("plain rewrite: reference {r} absent from renumber map (dangling ref)")`
on miss. Recurse dict (`iter`)/array/stream-dict; clone scalars; **clone stream data only
once** (unavoidable — mirror the existing helper; the writer already owns the resolved
object, so consider taking `Object` by value and rewriting in place to avoid the data
clone — prefer in-place mutation per `.claude/rules` #1).

Prefer this in-place signature to avoid cloning stream payloads:

```rust
/// Rewrite all references inside `obj` to their new numbers, in place.
pub(crate) fn renumber_refs_in_place(
    obj: &mut Object, map: &CatalogFirstRenumber,
) -> Result<()> { /* match &mut, recurse; Reference => *r = map.new_for_original(*r).ok_or(..)? */ }
```

Run: `cargo test -p flpdf rewrite_renumber` → PASS. **Commit.**

---

## Task 4: Wire the map into `write_pdf_full_rewrite`

**Files:**
- Modify: `crates/flpdf/src/writer.rs` (`write_pdf_full_rewrite`, ~2151–2730)

Do this incrementally; build after each sub-step. Re-read the exact current lines before
editing (line numbers drift).

**Step 1:** Build the map at the top of the function, right after `root_ref` is obtained
(~`writer.rs:2166`) and before `object_refs`/`existing_max` (`2281-2284`):

```rust
let renumber = crate::rewrite_renumber::CatalogFirstRenumber::build(pdf)?;
```

**Step 2:** Replace the iteration source. Instead of
`object_refs.sort_by_key(|r| (r.number, r.generation))` driving the loop with original
numbers, build:

```rust
// (new_ref, old_ref) in ascending new-number order. Drives both the emit loop
// and existing_max. Object 0 / deleted are never reachable from /Root, so the
// existing skip guards become no-ops here, but keep them for defensive parity.
let renumbered: Vec<(ObjectRef, ObjectRef)> = renumber.pairs().collect();
```

Then in the main loop iterate `&renumbered` as `(new_ref, old_ref)`:
- resolve `old_ref` (`pdf.resolve(*old_ref)?`),
- `renumber_refs_in_place(&mut object, &renumber)?` BEFORE encryption/stream policy,
- emit `"{} {} obj\n", new_ref.number, new_ref.generation`,
- record `offsets.insert(new_ref.number, ...)`,
- pass `*new_ref` (not old) to the encryption helpers,
- duplicate-detection keys on `new_ref.number`.

**Step 3:** `existing_max` / aux allocations. Set `existing_max = renumber.len() as u32`
so ObjStm containers / `/Encrypt` / qdf length-holders allocate above the new max. (For
the acceptance fixtures `plan.batches` is empty, encrypt is None, qdf off — these are
inert, but must remain correct: a quick objstm/qdf/encrypt run must still produce valid
files in Task 5.)

**Step 4:** Trailer remap (`writer.rs:2724-2728` and the xref-stream branch's trailer
build, plus `build_xref_stream_dict` at `writer.rs:1292`/`1357` which sets `/Root`):
- `root_ref` written to the trailer must be the NEW root ref:
  `let new_root = renumber.new_for_original(root_ref).expect("root is always enqueued");`
  Use `new_root` everywhere the old `root_ref` fed the trailer/xref-stream `/Root`.
- After `let mut trailer = pdf.trailer().clone(); strip_incremental_trailer_keys(&mut trailer);`
  remap surviving indirect refs (notably `/Info`):
  `renumber_refs_in_place_dict(&mut trailer, &renumber)` — but `/Root` is overwritten
  explicitly, and `/Encrypt` is handled by `apply_encrypt_trailer_entries`. Simplest:
  remap only `/Info` (and any other indirect trailer value that exists in the map);
  leave `/Encrypt` to its own path. Guard: if a trailer ref is absent from the map
  (unreferenced-from-Root but present in trailer), decide — for `/Info` it is always
  enqueued by the seed, so this should not happen; surface as error if it does.

**Step 5:** Build. `cargo build -p flpdf` → PASS. Then `cargo test -p flpdf` → triage
failures (golden/struct tests may need re-bless in Task 5; unit tests should pass).

**Commit** once it builds and library unit tests (non-golden) pass.

---

## Task 5: Acceptance — cmp-diff-0 on the three fixtures + full suite

**Step 1:** Add/locate a direct cmp acceptance check. The compat harness
(`crates/flpdf-cli/tests/compat_baseline_static_id.rs`,
`compat_matrix_baseline.rs`) already compares flpdf vs qpdf with `--static-id`. Run:

```bash
cargo test -p flpdf-cli --test compat_baseline_static_id --test compat_matrix_baseline
```

Inspect the new verdicts: the plain `--static-id` column for one/two/three-page must move
from `byte-equal=diverge` to `match`.

**Step 2:** Manual cmp sanity (ground truth, independent of the harness):

```bash
# In the worktree. Produce flpdf plain --static-id output and qpdf output, elide /ID, cmp.
# (Use the CLI: `cargo run -p flpdf-cli -- rewrite --static-id in.pdf out.pdf`; confirm
#  the exact subcommand/flags from flpdf-cli --help.)
```

Expected: identical bytes after `/ID` elision for all three fixtures.

**Step 3:** Full regression — the shared writer affects everything:

```bash
cargo test -p flpdf
cargo test -p flpdf-cli
```

Triage EVERY failure. Golden diffs are expected (re-bless in Step 4); logic failures are
not.

**Step 4:** Re-bless goldens and **inspect every diff**:

```bash
BLESS=1 cargo test -p flpdf-cli --test compat_matrix_baseline
BLESS=1 cargo test -p flpdf-cli --test compat_baseline_static_id
git diff tests/golden/
```

Confirm each change is an intended renumber (object counts, offsets, verdict flips) — NOT
a dropped object, broken ref, or regressed verdict elsewhere. Pay attention to: qdf
columns (valid + re-blessed, not necessarily byte-equal), encrypt/objstm columns (must
stay valid — round-trip, don't just cmp).

**Step 5:** Commit goldens + writer wiring together if not already.

---

## Task 6: Decisions registry + issue bookkeeping

**Files:**
- Modify: `docs/qpdf-compat-decisions.md` (confirm exact path; design says this file)

**Step 1:** Record flpdf-9hc.32 as a **byte-identical** decision (NOT deferred/observable):
plain rewrite now mimics qpdf's Catalog-first renumber; one/two/three-page byte-identical
to `qpdf --static-id` after `/ID` elision. Note the two behavior changes: unreferenced
objects dropped; qdf re-blessed (qdf byte-identity not claimed).

**Step 2:** Verify the unreferenced-drop decision against the suite: confirm no test
asserted unreferenced-object preservation (Task 5 would have surfaced it). If one did,
file a follow-up issue and reconcile before closing.

**Step 3:** `cargo clippy -p flpdf -p flpdf-cli --all-targets` and `cargo fmt --check`
(match the repo's quality gate) → clean.

**Step 4 (handled by the orchestrator, not this plan):** finishing-a-development-branch,
push, PR, roborev/coderabbit, `bd close flpdf-9hc.32`.

---

## Risk notes for the executor

- **#1 risk is dict-key order in the numbering walk** — already de-risked: `Dictionary`
  is a `BTreeMap`, `iter()` is lexicographic. Use it directly; add NO second sort; rely on
  NO insertion order. Task 2 proves it before any writer edit.
- Encryption number consistency (#5) is correctness, not byte-identity — pass new refs to
  encrypt helpers; verify by round-tripping encrypted fixtures in Task 5, not cmp.
- Do not touch `linearization/`. Do not chase `--qdf` byte-identity.
- Follow `.claude/rules/pdf-rust-review-patterns.md`: no needless `.clone()` (rewrite refs
  in place; resolve returns owned), resolve indirect refs before type-matching, bound
  inline-nesting depth in `collect_refs`.
