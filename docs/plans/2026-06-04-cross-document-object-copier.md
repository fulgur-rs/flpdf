# Cross-document Object Copier Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `copy_objects()` — copy a pre-closed set of source `ObjectRef`s deep into a target `Pdf` with fresh object numbers, returning a source→target renumber map; cycles handled, out-of-set refs nulled.

**Architecture:** New free-function module `crates/flpdf/src/object_copy.rs`. The function takes a `BTreeSet<ObjectRef>` (the curated closure from `page_object_closure`, flpdf-5h5.1) as both work-list and boundary — it does NOT re-traverse to discover objects. Target numbers are pre-allocated for the whole set in sorted order (so cycles are trivially safe), then each source object is resolved, its embedded references rewritten (in-set → mapped, out-of-set → `Object::Null`), and stored into the target via `set_object`. Stream byte payloads are carried verbatim.

**Tech Stack:** Rust, flpdf crate (`Pdf`, `Object`, `ObjectRef`, `Dictionary`, `Stream`, `Result`).

---

## Issue: flpdf-5h5.2 (design saved in beads design field)

Acceptance criteria:
- Copied subgraph in target resolves identically to source view.
- Cycles preserved.
- Copying the same source twice without sharing produces independent target copies.

---

### Task 1: Module skeleton + number allocation + pre-allocated map

**Files:**
- Create: `crates/flpdf/src/object_copy.rs`
- Modify: `crates/flpdf/src/lib.rs` (add `pub mod object_copy;` and `pub use object_copy::copy_objects;`)
- Test: `crates/flpdf/tests/object_copy_tests.rs`

**Step 1: Write the failing test** (simple chain A→B→C, fresh numbers + identical resolve)

In `tests/object_copy_tests.rs`, build a source PDF whose page-area has a small
ref chain and a target PDF (`build_target` — a minimal catalog/pages PDF). Use a
helper that returns the source refs to copy as a `BTreeSet`. Assert:
- returned map has one entry per input ref;
- every target number is greater than the target's original max object number;
- resolving each mapped target ref yields a structurally-equal object to the
  source (with references rewritten to the mapped numbers).

```rust
use flpdf::{copy_objects, Object, ObjectRef, Pdf};
use std::collections::BTreeSet;

#[test]
fn copies_chain_with_fresh_numbers() {
    let src = build_chain_pdf();       // 4 0 obj -> 5 0 R -> 6 0 R (in page area)
    let tgt = build_target_pdf();      // catalog(1)/pages(2)/page(3)
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut target = Pdf::open_mem(&tgt).unwrap();

    let refs: BTreeSet<ObjectRef> = [
        ObjectRef::new(4, 0), ObjectRef::new(5, 0), ObjectRef::new(6, 0),
    ].into_iter().collect();

    let map = copy_objects(&mut source, &mut target, &refs).unwrap();

    assert_eq!(map.len(), 3);
    let tgt_max = 3; // target's original max object number
    for (_src, t) in &map { assert!(t.number > tgt_max); }

    // A's copied dict now references map[5 0 R]; resolve and check.
    let a = target.resolve(map[&ObjectRef::new(4, 0)]).unwrap();
    let expected_b = Object::Reference(map[&ObjectRef::new(5, 0)]);
    assert!(object_contains_ref(&a, &expected_b));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --test object_copy_tests copies_chain_with_fresh_numbers`
Expected: FAIL (`copy_objects` not found).

**Step 3: Write minimal implementation**

```rust
//! Cross-document deep object copier (renumber + cycle handling).
use crate::{Object, ObjectRef, Pdf, Result};
use crate::object::{Dictionary, Stream};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// Copy the pre-closed object set `refs` from `source` into `target`, assigning
/// fresh target object numbers. Returns the source→target renumber map.
///
/// `refs` is treated as both work-list and boundary: references inside copied
/// objects that point outside `refs` are replaced with `Object::Null`.
pub fn copy_objects<RS: Read + Seek, RT: Read + Seek>(
    source: &mut Pdf<RS>,
    target: &mut Pdf<RT>,
    refs: &BTreeSet<ObjectRef>,
) -> Result<BTreeMap<ObjectRef, ObjectRef>> {
    // 1. Next free target number.
    let mut next = target
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        + 1;

    // 2. Pre-allocate target numbers for the whole set (sorted → deterministic).
    let mut map: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();
    for &src_ref in refs {
        map.insert(src_ref, ObjectRef::new(next, 0));
        next += 1;
    }

    // 3. Resolve, rewrite, store.
    for &src_ref in refs {
        let obj = source.resolve(src_ref)?;
        let rewritten = rewrite_refs(&obj, &map);
        target.set_object(map[&src_ref], rewritten);
    }

    Ok(map)
}

/// Deep-rewrite every `Object::Reference` in `obj`: in-map → mapped ref,
/// out-of-map → `Object::Null`. Stream bytes are carried verbatim.
fn rewrite_refs(obj: &Object, map: &BTreeMap<ObjectRef, ObjectRef>) -> Object {
    match obj {
        Object::Reference(r) => match map.get(r) {
            Some(&t) => Object::Reference(t),
            None => Object::Null,
        },
        Object::Array(items) => {
            Object::Array(items.iter().map(|i| rewrite_refs(i, map)).collect())
        }
        Object::Dictionary(dict) => Object::Dictionary(rewrite_dict(dict, map)),
        Object::Stream(stream) => Object::Stream(Stream::new(
            rewrite_dict(&stream.dict, map),
            stream.data.clone(),
        )),
        // Scalars unchanged.
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => obj.clone(),
    }
}

fn rewrite_dict(dict: &Dictionary, map: &BTreeMap<ObjectRef, ObjectRef>) -> Dictionary {
    let mut out = Dictionary::new();
    for (key, value) in dict.iter() {
        out.insert(key, rewrite_refs(value, map));
    }
    out
}
```

Add to `lib.rs`: `pub mod object_copy;` (with the other `pub mod`s) and
`pub use object_copy::copy_objects;` (with the other `pub use`s).

Add the `object_contains_ref` + PDF-builder test helpers to the test file
(pattern copied from `tests/page_closure_tests.rs`).

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf --test object_copy_tests copies_chain_with_fresh_numbers`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_copy.rs crates/flpdf/src/lib.rs crates/flpdf/tests/object_copy_tests.rs
git commit -m "feat(object_copy): add copy_objects cross-document deep copier"
```

---

### Task 2: Cycle handling (A↔B)

**Files:** Modify: `crates/flpdf/tests/object_copy_tests.rs`

**Step 1: Write the failing test**

Build a source with a 2-object cycle (A references B, B references A). Copy
`{A, B}`. Assert: terminates, both copied, A's copy references `map[B]` and B's
copy references `map[A]`.

**Step 2:** Run → expect PASS already (pre-allocation makes cycles safe). This
task is a *regression guard*: if it passes immediately, good — the design is
proven. If the test infra needs a cycle builder, add it.

Run: `cargo test -p flpdf --test object_copy_tests cycle`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/flpdf/tests/object_copy_tests.rs
git commit -m "test(object_copy): cover reference cycle preservation"
```

---

### Task 3: In-call shared-child dedup

**Files:** Modify: `crates/flpdf/tests/object_copy_tests.rs`

**Step 1: Write the failing test**

Source: E and F both reference G; set = `{E, F, G}`. Assert E's copy and F's copy
both reference the SAME `map[G]` (G copied once, not duplicated).

**Step 2:** Run → expect PASS (single map entry per ref guarantees dedup).

Run: `cargo test -p flpdf --test object_copy_tests shared_child`
Expected: PASS

**Step 3: Commit**

```bash
git commit -am "test(object_copy): cover in-call shared-child dedup"
```

---

### Task 4: Out-of-set reference → Null

**Files:** Modify: `crates/flpdf/tests/object_copy_tests.rs`

**Step 1: Write the failing test**

Source object H references object Z; set = `{H}` only (Z excluded). Copy. Assert
H's copy has `Object::Null` where the Z reference was.

**Step 2:** Run → expect PASS (`rewrite_refs` maps out-of-map refs to Null).

Run: `cargo test -p flpdf --test object_copy_tests out_of_set`
Expected: PASS

**Step 3: Commit**

```bash
git commit -am "test(object_copy): cover out-of-set reference nulling"
```

---

### Task 5: Independence across calls

**Files:** Modify: `crates/flpdf/tests/object_copy_tests.rs`

**Step 1: Write the failing test**

Copy the same source set into the same target TWICE. Assert the two maps use
disjoint target number ranges (no shared target ObjectRef), proving independent
copies.

**Step 2:** Run → expect PASS (each call re-reads target max + allocates fresh).

Run: `cargo test -p flpdf --test object_copy_tests independent`
Expected: PASS

**Step 3: Commit**

```bash
git commit -am "test(object_copy): cover independence across copy calls"
```

---

### Task 6: Stream payload copied

**Files:** Modify: `crates/flpdf/tests/object_copy_tests.rs`

**Step 1: Write the failing test**

Source has a stream object with known bytes. Copy it. Resolve the target copy and
assert `Stream.data` equals the source bytes and the dict was rewritten.

**Step 2:** Run → expect PASS.

Run: `cargo test -p flpdf --test object_copy_tests stream`
Expected: PASS

**Step 3: Commit**

```bash
git commit -am "test(object_copy): cover stream byte payload copy"
```

---

### Task 7: Quality gates

**Step 1:** `cargo fmt --all`
**Step 2:** `cargo clippy -p flpdf --all-targets -- -D warnings`
**Step 3:** `cargo test -p flpdf --test object_copy_tests`
**Step 4:** `cargo test -p flpdf` (full crate regression)
**Step 5:** Commit any fmt/clippy fixes.

```bash
git commit -am "style(object_copy): apply fmt/clippy"
```
