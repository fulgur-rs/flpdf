# ObjectHandle In-Place Mutation API Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or
> superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** Add the ObjectHandle-native in-place mutation surface that
flpdf-5v4a, flpdf-egzr.3.2.5, and flpdf-egzr.3.2.6 need before their own
internals can migrate: `get_key`/`replace_key`/`remove_key`/`shallow_copy`/
`merge_resources` on `ObjectHandle`, plus `Pdf::make_indirect_object_handle`
and a stream-data replacement primitive. Zero consumer changes — purely
additive API, mirroring `flpdf-egzr.3.2.1`'s own shape.

**Architecture:** `ObjectHandle` already has the right foundation
(`Rc<RefCell<..>>` shared identity, documented as mirroring qpdf's
`shared_ptr<QPDFValue>`). The read-only accessors added in 3.2.1
(`as_dictionary`, `as_array`, ...) go through a private `with_value`
closure helper that only ever takes `&ObjectValue`. This plan adds a
sibling `with_value_mut` taking `&mut ObjectValue`, and builds every new
mutation method on top of it — mutating the *live* `RefCell` contents in
place, never materializing a snapshot and writing it back. This is not a
stylistic choice: it is the exact mechanism qpdf's own
`QPDFObjectHandle::replaceKey`/`removeKey` use (dereference into the same
shared value, mutate directly), verified against
`QPDFAcroFormDocumentHelper::adjustAppearanceStream`
(`libqpdf/QPDFAcroFormDocumentHelper.cc:615-680`), the exact function
flpdf-5v4a will later port.

**Tech Stack:** Rust 2021 workspace; pinned qpdf 11.9.0 source
(`libqpdf/QPDFObjectHandle.cc`, `libqpdf/QPDF.cc`,
`libqpdf/QPDFAcroFormDocumentHelper.cc`) as the behavioral oracle;
existing `cargo test`, Clippy, `cargo llvm-cov`, `scripts/patch-coverage.sh`.

---

## Status and Prior Context

- Parent: `flpdf-egzr.3.2.12` (bd issue — read its DESCRIPTION/DESIGN/
  ACCEPTANCE CRITERIA fields in full before starting; they carry the exact
  qpdf citations this plan builds from and are the authoritative source if
  this plan and the issue ever disagree).
- Depends on `flpdf-egzr.3.2.1` (closed, merged to `main` as PR #603,
  merge commit `4265c8ba`). `main` at that commit is this plan's base.
- Worktree: `/home/ubuntu/flpdf/.worktrees/flpdf-egzr-3-2-12-objecthandle-mutation`
  on branch `feat/flpdf-egzr-3-2-12-objecthandle-mutation`, branched from
  `main`.
- Blocks `flpdf-5v4a`, `flpdf-egzr.3.2.5`, `flpdf-egzr.3.2.6` — all three
  are re-checked against this plan's actual landed API shape before they
  start (this plan may adjust a signature during TDD; if so, update the bd
  issue, not just the code).
- Research already done (see the bd issue's DESCRIPTION for full qpdf
  citations, condensed here):
  - `ObjectHandle`'s internal `Repr::Direct(Rc<RefCell<DirectSlot>>)` /
    `Repr::Indirect(Rc<RefCell<IndirectSlot>>)` (`object_handle.rs:69-72`)
    already gives every handle shared, mutable identity — no new storage
    layer needed, only new methods.
  - `ObjectValue::Dictionary` is `BTreeMap<Vec<u8>, ObjectHandle>`,
    `ObjectValue::Array` is `Vec<ObjectHandle>` (`object_handle.rs:105-106`)
    — children are handles, so a container mutation never touches a
    child's own subtree.
  - The existing private `with_value<T>(&self, f: impl FnOnce(Option<&ObjectValue>) -> T) -> T`
    (`object_handle.rs:703-712`) is read-only; every read accessor added in
    3.2.1 goes through it. This plan's `with_value_mut` is its mutable
    twin.
  - `ObjectValue` does not derive `Clone` today (only `Debug`). Task 5
    (`shallow_copy`) and the `make_indirect_object_handle` helper both need
    to clone a value's immediate contents (not deep-clone subtrees, since
    children are already `Rc`-shared handles) — add `#[derive(Clone)]` to
    `ObjectValue` rather than hand-writing a parallel clone function
    (`materialize_value` is NOT reusable here: it converts to `Object`, a
    different type).
  - `Pdf::set_object` (`reader.rs:1184`) is the existing "lift" mechanism:
    it resolves/creates the canonical handle for a ref via
    `self.get_object_handle(object_ref)`, converts the caller's `Object`
    into an `ObjectValue` (`lift_for_set_object`), and calls
    `handle.set_resolved(value)`. `make_indirect_object_handle` mirrors
    this shape but skips the `Object` round-trip entirely: it goes straight
    from a caller-supplied direct `ObjectHandle` to a freshly indirect one.
  - No existing "next available object number" helper on `Pdf` was found;
    `overlay_appearance_stream.rs`'s own `allocate_next_ref` computes
    `object_refs().max() + 1` locally. Re-derive this the same way inside
    `make_indirect_object_handle` rather than searching further — do not
    invent a new counter-based scheme (qpdf's own `nextObjGen()` is a
    counter, but porting that book-keeping is out of this slice's scope;
    a scan is correct and this method is not hot-path).

## Global Constraints

- **Allowlist:** `crates/flpdf/src/object_handle.rs` for Tasks 1-6;
  `crates/flpdf/src/reader.rs` for Task 7 (`Pdf::make_indirect_object_handle`
  lives on `Pdf`, which is defined there). No other file changes. This is
  additive-only — no existing consumer file is touched, matching
  `flpdf-egzr.3.2.1`'s own zero-consumer-diff shape.
- Every new `pub fn` needs a one-line doc comment (imperative, English —
  `.claude/rules/pdf-rust-doc-review-patterns.md` §3, §5) and a real qpdf
  citation (file + line in `/tmp/qpdf-11.9.0-source`, or wherever
  `scripts/fetch-qpdf-source.sh` materializes it for the executing
  session — re-run that script first if the cited path doesn't exist).
- No behavior change to any existing method or output byte. This task set
  is additive only.
- New test code lives in `object_handle.rs`'s existing `#[cfg(test)]`
  module tree (new `mod` blocks, e.g. `mutation_tests`), matching 3.2.1's
  own convention.
- Do not add `insertItem`/`eraseItem` (array mutation), `QPDF::replaceObject`'s
  wholesale-existing-object-replacement form, or a public
  `ObjectHandle::stream(dict, data)` constructor. None of flpdf-5v4a's real
  algorithm calls them (see the bd issue's DESIGN field). If a later slice
  needs one, add it then, grounded in what that slice's own qpdf source
  actually calls.

## File Structure

All changes: `crates/flpdf/src/object_handle.rs` (Tasks 1-6) and
`crates/flpdf/src/reader.rs` (Task 7), one commit per task.

---

### Task 1: Confirm clean baseline

**Step 1: Build and test**

Run: `cargo build --workspace --all-features && cargo test -p flpdf --lib`
Expected: clean build, all green. Record the pass count for later
comparison.

**Step 2: No commit** (verification only).

---

### Task 2: `with_value_mut` and `ObjectValue: Clone`

**Files:** Modify `crates/flpdf/src/object_handle.rs`.

**Step 1: Write the failing tests**

Add to a new `mod mutation_tests` at the end of the file (after the
existing test modules):

```rust
#[cfg(test)]
mod mutation_tests {
    use super::*;

    #[test]
    fn object_value_clone_preserves_scalar_content() {
        let value = ObjectValue::Integer(42);
        let cloned = value.clone();
        assert!(matches!(cloned, ObjectValue::Integer(42)));
    }

    #[test]
    fn object_value_clone_of_a_dictionary_shares_child_identity() {
        let child = ObjectHandle::integer(7);
        let dict = ObjectValue::Dictionary(
            [(b"K".to_vec(), child.clone())].into_iter().collect(),
        );
        let cloned = dict.clone();
        let ObjectValue::Dictionary(entries) = cloned else {
            panic!("expected dictionary");
        };
        assert!(entries.get(b"K".as_slice()).unwrap().ptr_eq(&child));
    }
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib mutation_tests -- --nocapture`
Expected: compile failure (`ObjectValue` does not implement `Clone`;
`ptr_eq` is `pub(crate)`, already visible in-crate so that part compiles).

**Step 3: Implement**

Add `#[derive(Clone)]` next to `ObjectValue`'s existing `#[derive(Debug)]`
(`object_handle.rs:81`).

Add the mutable sibling of `with_value` right after it
(`object_handle.rs:712`, after the existing `with_value` closes):

```rust
// Mutable twin of `with_value` above: `None` for an indirect handle not
// yet resolved (mutation on an unresolved handle must not perform hidden
// I/O, same rule as every read accessor), and for `Missing`/`Destroyed`
// (there is no live `ObjectValue::Null` slot to hand out a `&mut` into —
// those states only *present* as null, they do not store one).
fn with_value_mut<T>(&self, f: impl FnOnce(Option<&mut ObjectValue>) -> T) -> T {
    match &self.0 {
        Repr::Direct(slot) => f(Some(&mut slot.borrow_mut().value)),
        Repr::Indirect(slot) => match &mut slot.borrow_mut().state {
            IndirectState::Resolved(value) => f(Some(value)),
            IndirectState::NotYetResolved
            | IndirectState::Missing
            | IndirectState::Destroyed => f(None),
        },
    }
}
```

**Step 4: Run tests, verify pass**

Run: `cargo test -p flpdf --lib mutation_tests -- --nocapture`
Expected: 2 passed.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): derive Clone for ObjectValue, add with_value_mut"
```

---

### Task 3: `get_key`

**Step 1: Write the failing tests** (add to `mutation_tests`)

```rust
#[test]
fn get_key_returns_a_live_child_handle_without_snapshotting_the_dictionary() {
    let child = ObjectHandle::integer(1);
    let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), child.clone())]);
    let fetched = dict.get_key(b"A");
    assert!(fetched.ptr_eq(&child));
}

#[test]
fn get_key_on_a_missing_key_returns_a_direct_null_handle() {
    let dict = ObjectHandle::dictionary(vec![]);
    assert!(dict.get_key(b"Missing").is_null());
}

#[test]
fn get_key_on_a_non_dictionary_handle_returns_a_direct_null_handle() {
    let scalar = ObjectHandle::integer(5);
    assert!(scalar.get_key(b"A").is_null());
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib mutation_tests -- --nocapture`
Expected: compile failure, `get_key` not found.

**Step 3: Implement**

Add after `as_dictionary` (`object_handle.rs:552`):

```rust
/// The value at `key` if this handle's value is a dictionary and `key` is
/// present, or a direct null handle otherwise (a missing key, or this
/// handle not being a dictionary at all) — mirrors
/// `QPDFObjectHandle::getKey`'s own "returns null for a missing key"
/// contract (`include/qpdf/QPDFObjectHandle.hh`,
/// `libqpdf/QPDFObjectHandle.cc`). Unlike [`Self::as_dictionary`], this
/// never snapshots the whole dictionary — it returns the one live child
/// handle directly, so a caller that only needs one key does not pay for
/// every sibling. Never performs resolution itself.
pub fn get_key(&self, key: &[u8]) -> ObjectHandle {
    self.with_value(|value| match value {
        Some(ObjectValue::Dictionary(entries)) => entries.get(key).cloned(),
        _ => None,
    })
    .unwrap_or_else(ObjectHandle::null)
}
```

(Confirm `ObjectHandle::null()` already exists as a public constructor —
it does, from 3.2.1's own accessor family; if the name differs, match
whatever the existing constructor is actually called.)

**Step 4: Run tests, verify pass**

Run: `cargo test -p flpdf --lib mutation_tests -- --nocapture`
Expected: 5 passed (2 from Task 2 + 3 new).

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): add get_key"
```

---

### Task 4: `replace_key` and `remove_key`

**Step 1: Write the failing tests**

```rust
#[test]
fn replace_key_mutates_the_live_dictionary_in_place() {
    let dict = ObjectHandle::dictionary(vec![]);
    let clone = dict.clone();
    dict.replace_key(b"A", ObjectHandle::integer(9));
    assert_eq!(clone.get_key(b"A").as_integer(), Some(9));
}

#[test]
fn replace_key_overwrites_an_existing_key() {
    let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
    dict.replace_key(b"A", ObjectHandle::integer(2));
    assert_eq!(dict.get_key(b"A").as_integer(), Some(2));
}

#[test]
fn replace_key_on_a_non_dictionary_handle_is_a_no_op() {
    let scalar = ObjectHandle::integer(1);
    scalar.replace_key(b"A", ObjectHandle::integer(2));
    assert_eq!(scalar.as_integer(), Some(1));
}

#[test]
fn remove_key_deletes_a_present_key() {
    let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
    dict.remove_key(b"A");
    assert!(dict.get_key(b"A").is_null());
}

#[test]
fn remove_key_on_a_missing_key_is_a_no_op() {
    let dict = ObjectHandle::dictionary(vec![]);
    dict.remove_key(b"Missing"); // must not panic
    assert!(dict.get_key(b"Missing").is_null());
}

#[test]
fn remove_key_on_a_non_dictionary_handle_is_a_no_op() {
    let scalar = ObjectHandle::integer(1);
    scalar.remove_key(b"A");
    assert_eq!(scalar.as_integer(), Some(1));
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib mutation_tests -- --nocapture`
Expected: compile failure.

**Step 3: Implement**

Add after `get_key`:

```rust
/// Insert or overwrite `key` in this handle's dictionary with `value`,
/// mutating the live value every other clone of this handle also
/// observes — mirrors `QPDFObjectHandle::replaceKey`
/// (`libqpdf/QPDFObjectHandle.cc:1200-1209`). A no-op on a non-dictionary
/// handle or an unresolved/missing/destroyed indirect handle, matching
/// qpdf's own `typeWarning`-and-ignore contract rather than panicking.
/// Never performs resolution itself.
pub fn replace_key(&self, key: &[u8], value: ObjectHandle) {
    self.with_value_mut(|v| {
        if let Some(ObjectValue::Dictionary(entries)) = v {
            entries.insert(key.to_vec(), value);
        }
    });
}

/// Remove `key` from this handle's dictionary if present, mutating the
/// live value every other clone of this handle also observes — mirrors
/// `QPDFObjectHandle::removeKey` (`libqpdf/QPDFObjectHandle.cc:1228-1238`).
/// A no-op if `key` is absent, this handle is not a dictionary, or the
/// indirect handle is unresolved/missing/destroyed. Never performs
/// resolution itself.
pub fn remove_key(&self, key: &[u8]) {
    self.with_value_mut(|v| {
        if let Some(ObjectValue::Dictionary(entries)) = v {
            entries.remove(key);
        }
    });
}
```

**Step 4: Run tests, verify pass**

Run: `cargo test -p flpdf --lib mutation_tests -- --nocapture`
Expected: 11 passed.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): add replace_key/remove_key"
```

---

### Task 5: `shallow_copy`

**Step 1: Write the failing tests**

```rust
#[test]
fn shallow_copy_is_always_direct_even_from_an_indirect_source() {
    let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
    indirect.set_resolved(ObjectValue::Dictionary(Default::default()));
    let copy = indirect.shallow_copy();
    assert!(copy.is_direct());
}

#[test]
fn shallow_copy_mutation_does_not_affect_the_source() {
    let original = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
    let copy = original.shallow_copy();
    copy.replace_key(b"A", ObjectHandle::integer(2));
    assert_eq!(original.get_key(b"A").as_integer(), Some(1));
    assert_eq!(copy.get_key(b"A").as_integer(), Some(2));
}

#[test]
fn shallow_copy_of_a_non_container_clones_the_scalar_value() {
    let original = ObjectHandle::integer(5);
    let copy = original.shallow_copy();
    assert!(!copy.ptr_eq(&original));
    assert_eq!(copy.as_integer(), Some(5));
}
```

**Correction (2026-08-01, post-implementation):** the fourth test
originally planned here —
`shallow_copy_children_keep_shared_identity_with_the_source`, asserting
`copy.get_key(b"A").ptr_eq(&child)` for a *direct* child — assumed
`shallowCopy()` is a single-level-only copy that shares every child's
identity. Reading `libqpdf/QPDF_Dictionary.cc`/`libqpdf/QPDF_Array.cc`'s
`copy(shallow=false)` default (the method `QPDFObjectHandle::shallowCopy`
actually defers to) before implementing this task showed that is wrong:
qpdf recursively copies through every *direct* descendant, and shares
identity only across an *indirect* boundary. The test above was never
committed; the implemented and committed test suite instead covers this
correctly (`shallow_copy_of_a_direct_dictionary_child_produces_an_independent_copy`,
`shallow_copy_of_an_indirect_dictionary_child_keeps_shared_identity`, and
`shallow_copy_of_an_array_recurses_through_direct_elements` in
`crates/flpdf/src/object_handle.rs`'s `mutation_tests` module — a direct
child is *not* `ptr_eq` after the copy, while an indirect child *is*). The
implementation and its doc comment (`ObjectHandle::shallow_copy`) reflect
this corrected semantics; only this plan step's original draft did not
get updated to match. See the implementation's own doc comment for the
precise contract.

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib mutation_tests -- --nocapture`

**Step 3: Implement**

Add after `remove_key`. **Note:** the sketch below is the pre-correction
draft (same single-level-only assumption as the removed test above) and
was superseded before landing — see the implemented
`ObjectHandle::shallow_copy` (delegating to `shallow_copy_value`/
`shallow_copy_child`) and its doc comment in
`crates/flpdf/src/object_handle.rs` for the actual recursive
implementation.

```rust
/// A fresh, direct handle with a one-level-deep copy of this handle's
/// value — mirrors `QPDFObjectHandle::shallowCopy`
/// (`libqpdf/QPDFObjectHandle.cc`). Always direct regardless of whether
/// `self` is indirect. A dictionary or array's *entries* are copied into
/// the new container, but each entry keeps its own existing shared
/// identity (child `ObjectHandle`s are `Rc`-cloned, not deep-cloned) — so
/// mutating the copy's own top-level keys/items never affects `self`, but
/// mutating a value reached *through* a shared child does. A scalar value
/// is cloned outright (there is nothing to share). Never performs
/// resolution itself: shallow-copying an unresolved/missing/destroyed
/// indirect handle produces a direct null handle, matching every other
/// accessor's "no hidden I/O" rule.
pub fn shallow_copy(&self) -> ObjectHandle {
    self.with_value(|value| match value {
        Some(v) => ObjectHandle::from_value(v.clone()),
        None => ObjectHandle::null(),
    })
}
```

**Step 4: Run tests, verify pass**

Run: `cargo test -p flpdf --lib mutation_tests -- --nocapture`
Expected: 15 passed.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): add shallow_copy"
```

---

### Task 6: `merge_resources`

**Step 1: Read qpdf's actual algorithm first**

Run: `sed -n '1063,1150p' /tmp/qpdf-11.9.0-source/libqpdf/QPDFObjectHandle.cc`
(or wherever `scripts/fetch-qpdf-source.sh` materializes it). Read the
*whole* method, including the conflicts-map branch, before writing a line
of Rust — this is the single most fidelity-sensitive method in this
plan, and `adjust_appearance_stream`'s own doc comment
(`overlay_appearance_stream.rs:181-289`) already documents in detail how
its current manual reimplementation depends on this exact algorithm's
"reads live state, later entries can observe earlier renames" behavior.
Do not paraphrase from memory or from this plan doc — read the source
fresh.

**Step 2: Write the failing tests**

Cover at minimum:
- merging a key `other` has that `self` does not (installed directly,
  `shallowCopy`'d first if not already indirect... — confirm this exact
  rule against the source read in Step 1, do not assume);
- a key both have, no conflict tracking requested (`conflicts: None`):
  confirm the exact qpdf behavior for this arm from the source (it may
  differ from the has-a-conflicts-map arm — check before assuming);
- a key both have, conflict tracking requested: populate the conflicts
  map, confirm exact key/value shape against the source;
- an already-populated conflicts map from a caller (mirrors
  `adjust_appearance_stream`'s two-call pattern: once without tracking to
  force sub-dicts unshared, once with tracking for the real conflict
  pass) — if the source's algorithm makes this meaningful, test it;
  otherwise note in the commit message why it does not apply here.

Do not treat the above bullets as the full list — they are the cases this
plan's author identified before reading the source in Step 1 as
"probably important"; the actual source read may reveal more (or show
one of these is not actually a distinct case). Trust the source over this
list.

**Step 3: Run to verify it fails, Step 4: implement, Step 5: run to
verify pass, Step 6: commit** — same TDD shape as prior tasks. Signature
starting point (adjust if the source read in Step 1 shows this is wrong):

```rust
/// Merge `other`'s top-level entries into this handle's dictionary,
/// mirroring `QPDFObjectHandle::mergeResources`
/// (`libqpdf/QPDFObjectHandle.cc:1063-...`, cite the exact closing line
/// once read). `conflicts`, if given, records `rtype -> old_key -> new_key`
/// for every key both dictionaries already had under the same top-level
/// resource type. [Fill in the rest of this doc comment from the actual
/// source semantics read in Step 1 — do not leave this bracketed note in
/// the committed code.]
pub fn merge_resources(
    &self,
    other: &ObjectHandle,
    conflicts: Option<&mut std::collections::BTreeMap<Vec<u8>, std::collections::BTreeMap<Vec<u8>, Vec<u8>>>>,
) {
    todo!("implement per the qpdf source read in Task 6 Step 1")
}
```

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): add merge_resources"
```

---

### Task 7: `Pdf::make_indirect_object_handle` and stream-data replacement

**Files:** Modify `crates/flpdf/src/reader.rs`.

**Step 1: Write the failing tests** (in `reader.rs`'s existing test
module — find it via `grep -n '^mod tests' crates/flpdf/src/reader.rs`
and confirm the current boundary line before inserting, per this
project's own recurring "stale hardcoded line number" review finding —
do not assume a line number from this plan)

```rust
#[test]
fn make_indirect_object_handle_allocates_a_fresh_ref_and_preserves_the_value() {
    let mut pdf = /* open the smallest available test fixture, or build via
        whatever this test module's existing convention is for a fresh Pdf
        without opening a file, if one exists -- check for it before adding
        a new helper */;
    let direct = ObjectHandle::integer(42);
    let indirect = pdf.make_indirect_object_handle(direct).unwrap();
    assert!(indirect.is_indirect());
    assert_eq!(indirect.as_integer(), Some(42));
}

#[test]
fn make_indirect_object_handle_rejects_an_already_indirect_handle() {
    let mut pdf = /* same setup */;
    let ref_ = pdf.object_refs().into_iter().next().expect("fixture has an object");
    let already_indirect = pdf.get_object_handle(ref_);
    assert!(pdf.make_indirect_object_handle(already_indirect).is_err());
}

#[test]
fn make_indirect_object_handle_allocates_past_the_highest_existing_number() {
    let mut pdf = /* same setup */;
    let max_before = pdf.object_refs().iter().map(|r| r.number).max().unwrap_or(0);
    let indirect = pdf.make_indirect_object_handle(ObjectHandle::integer(1)).unwrap();
    assert!(indirect.object_ref().unwrap().number > max_before);
}
```

Design the stream-data-replacement primitive's own tests during this
step too, once its exact signature is chosen in Step 3 (this plan
deliberately does not prescribe it — qpdf's `replaceStreamData` has
several overloads; pick the minimum one `adjust_appearance_stream`'s
future port will actually need: replacing data bytes plus `/Filter`
and `/DecodeParms` together, matching what
`overlay_appearance_stream.rs`'s current `stream.data = encoded;
stream.dict.insert("Filter", ...)` sequence already does by hand against
the legacy `Object` type).

**Step 2-6:** Same TDD shape. For the implementation, reuse
`self.get_object_handle`/`self.object_refs()` exactly as
`set_object`/`overlay_appearance_stream.rs::allocate_next_ref` already do
(cited in "Status and Prior Context" above) — do not invent a different
allocation strategy.

```bash
git add crates/flpdf/src/reader.rs
git commit -m "feat(reader): add Pdf::make_indirect_object_handle and stream-data replacement"
```

---

### Task 8: Zero-consumer-diff verification (mandatory gate)

**Step 1: Confirm the changed set matches the allowlist**

```bash
git diff --name-only main...HEAD -- crates/
```
Expected: exactly `crates/flpdf/src/object_handle.rs` and
`crates/flpdf/src/reader.rs`. Any other line is a leak.

**Step 2: Full workspace build and test**

Run: `cargo build --workspace --all-features && cargo test --workspace --all-features`
Expected: clean, all green — this also proves the new API doesn't
accidentally change any existing consumer's behavior, since none call it
yet.

**Step 3: No commit** (verification only).

---

### Task 9: Full regression — clippy, fmt, doctest

**Step 1:** `cargo fmt -- --check` — fold any fix into this task's commit.
**Step 2:** `cargo clippy --workspace --all-features -- -D warnings`.
**Step 3:** `cargo test --workspace --doc`.
**Step 4:** Commit any fixes surfaced (only if needed):
```bash
git add -A
git commit -m "chore: fmt/clippy/doctest cleanup for ObjectHandle mutation API"
```

Note: this slice does not touch stream filters or writer output, so the
qpdf-zlib-compat byte-identical suite is not expected to be relevant —
confirm this assumption by checking `git diff --name-only` (Task 8) shows
no overlap with anything that suite gates, rather than skipping the
suite on assumption alone. If in doubt, run it anyway; it costs little.

---

### Task 10: Coverage gate and qualitative review

**Step 1:** `git status --short` clean.
**Step 2:** Re-read `scripts/patch-coverage.sh`'s current flags before
invoking it (flag names may have changed since this plan was written).
**Step 3:** `scripts/patch-coverage.sh --base main` — expect 100%.
**Step 4:** Qualitative check (CLAUDE.md Test Coverage §4): beyond line
coverage, confirm real assertions exist for every "mirrors qpdf's
no-op/ignore contract" case (non-dictionary `replace_key`/`remove_key`,
missing-key `get_key`/`remove_key`), every shared-vs-fresh identity
distinction in `shallow_copy`, and `merge_resources`'s conflict-vs-no-
conflict branches — not just that the lines executed.
**Step 5:** No commit unless Step 3 required test additions.

---

### Task 11: Update beads and prepare for PR

**Step 1:** Record exact differential/verification commands on
`flpdf-egzr.3.2.12` via `bd update ... --append-notes`. Note any place
this plan's assumed signature (Task 6/7 especially) changed during TDD —
update the bd issue's DESIGN field too if so, since flpdf-5v4a/3.2.5/3.2.6
will read it, not this plan doc.
**Step 2:** Push and open the PR against `main`, base branch
`feat/flpdf-egzr-3-2-12-objecthandle-mutation` (stacks directly on
`main`, not on any other in-flight branch).
**Step 3:** Follow CLAUDE.md's Session Completion protocol in full.

## Summary

This plan adds exactly the `ObjectHandle` mutation surface flpdf-5v4a's
real qpdf source (`QPDFAcroFormDocumentHelper.cc:615-680`) calls, with
zero behavior change to any existing consumer. Every new method's doc
comment cites a specific qpdf source location, not a paraphrase.
