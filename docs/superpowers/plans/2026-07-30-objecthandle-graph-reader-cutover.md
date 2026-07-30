# ObjectHandle Graph and Reader Cutover Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended, given task size) or superpowers:executing-plans to implement this
> plan task-by-task.

**Goal:** Introduce the public core `ObjectHandle` graph and cut the production
parser/cache/reader route onto it, while every existing consumer of the legacy
`Object`/`Pdf::resolve`/`Pdf::resolve_borrowed` surface keeps compiling and
behaving identically through a narrow, named, bounded materialization bridge.

**Architecture:** A new crate-internal canonical object graph
(`ObjectHandle` wrapping `Rc<RefCell<..>>` slots, keyed by `ObjectRef` for
indirect identity) becomes the reader's real source of truth for identity,
lazy resolution, and parsed offset. The existing `Object`/`Dictionary`/`Stream`
public types and the existing `Pdf::resolve` / `Pdf::resolve_borrowed` /
`Pdf::set_object` / `Pdf::delete_object` / `Pdf::trailer` signatures are not
touched; instead they become thin materialization wrappers that convert
to/from the new handle graph. This is the "narrow transitional bridge" named
in the design and removed whole-cloth in `flpdf-egzr.3.2`.

**Tech Stack:** Rust 2021 workspace; `Rc<RefCell<..>>` for shared canonical
handle identity (Rust-idiomatic substitute for qpdf's `std::shared_ptr<QPDFValue>`
— internal-structure-only, does not touch output bytes, permitted under
CLAUDE.md category (B)); pinned qpdf 11.9.0 source
(`include/qpdf/QPDFObjectHandle.hh`, `libqpdf/qpdf/QPDFValue.hh`,
`libqpdf/QPDFParser.cc`, `libqpdf/QPDF.cc`) as the behavioral oracle; existing
`cargo test`, Clippy, `cargo llvm-cov`, `scripts/patch-coverage.sh`.

---

## Status and Prior Context

- Design (approved 2026-07-30):
  `docs/superpowers/specs/2026-07-30-xref-parsed-offset-object-handle-design.md`
  — read this file in full before starting; this plan does not repeat its
  qpdf source citations, it only implements against them.
- Roadmap: `docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`
- This plan implements **only** `flpdf-egzr.3.1` ("ObjectHandle graph and
  reader cutover"), the first of four stacked layers. `flpdf-egzr.3.2`
  (consumer cutover + bridge removal), `flpdf-egzr.3.3` (public xref table +
  full parsed-offset coordinate parity incl. ObjStm-relative/recovered
  tables), and `flpdf-egzr.3.4` (qtest helper binaries) are explicitly **out
  of scope** here.
- Base branch/commit for this work: `design/flpdf-egzr-3-object-handle`
  (`f39f85a2`) — **not** `main`. `bd update flpdf-egzr.3.1 --claim` has
  already been run.
- Worktree: `/home/ubuntu/flpdf/.worktrees/flpdf-egzr-3-1-objecthandle-cutover`
  on branch `feat/flpdf-egzr-3-1-objecthandle-cutover`.
- A clean baseline (full `cargo test --workspace`, plus every
  `qpdf-zlib-compat`-gated byte-identical test enumerated in
  `.github/workflows/ci.yml`) has already been verified green at `f39f85a2`
  in this session. A fresh executor (new subagent/session) must redo this
  (Task 1) since it has no memory of that run.

## Why the bridge must not touch consumers

A full-crate survey (this session) found that `writer.rs`, `pages.rs`,
`page_object_helper.rs`, `json_inspect.rs`, `resources.rs`, `outline.rs`,
`outline_document_helper.rs`, `linearization/*.rs`, and `object_copy.rs` all
pattern-match **directly** on `Object::Variant(..)` (2 to 613 match sites per
file) rather than going only through accessors — plus `outline.rs`,
`nntree.rs`, `encrypt_setup.rs`, `xref.rs`, `acroform_document_helper.rs`,
`annotation_helper.rs`, and `page_object_helper.rs` have **public function
signatures** that return `Object`/`Dictionary`/`Stream` by value. `resolve_borrowed`
alone has 350 call sites across 47 files. An "opaque handle behind accessors
only" bridge would not be transparent to any of this — it would force
`flpdf-egzr.3.2`-shaped edits into this layer. The bridge implemented here
therefore changes **nothing** about the public `Object` enum, `Dictionary`,
`Stream`, or any existing public `Pdf` method signature. It only changes what
backs them internally.

## Global Constraints

- **Zero edits outside this allowlist:**
  `crates/flpdf/src/object_handle.rs` (new), `crates/flpdf/src/object.rs`
  (additive only — no existing signature changes), `crates/flpdf/src/cache.rs`,
  `crates/flpdf/src/reader.rs`, `crates/flpdf/src/parser.rs`,
  `crates/flpdf/src/lib.rs` (module registration/re-export lines only),
  plus new test files. If implementing a task appears to require editing
  `writer.rs`, `pages.rs`, `page_object_helper.rs`, `json_inspect.rs`,
  `resources.rs`, `outline.rs`, `outline_document_helper.rs`,
  `linearization/**`, `object_copy.rs`, `nntree.rs`, `encrypt_setup.rs`,
  `xref.rs`, `acroform_document_helper.rs`, `annotation_helper.rs`,
  `embedded_files.rs`, `signatures.rs`, `filespec_helper.rs`, `appearance.rs`,
  any `overlay*`/`page_*` file, `struct_tree_pg.rs`, `name_number_tree.rs`,
  `ref_chain.rs`, `rewrite_renumber.rs`, `xref_entry.rs`, or anything under
  `crates/flpdf-cli/` / `crates/flpdf-qtest-tools/` — **stop**, the bridge
  design has leaked, and the task needs to be reworked, not the consumer.
- **No existing public signature changes.** `Object`, `Dictionary`, `Stream`,
  `XrefEntry`, `Pdf::resolve`, `Pdf::resolve_borrowed`, `Pdf::set_object`,
  `Pdf::delete_object`, `Pdf::trailer`, `Pdf::object_refs`,
  `Pdf::live_object_refs`, `Pdf::root_ref`, `Pdf::is_encrypted` are unchanged.
- **New public names must not collide** with existing ones. Where the design
  calls for a name that already exists with a different signature
  (`trailer() -> ObjectHandle` collides with the existing
  `trailer() -> &Dictionary` at `reader.rs:861`), use a temporary distinct
  name and record it in the table in "Naming bridge" below. `flpdf-egzr.3.2`
  renames these once the legacy method is deleted.
- **`get_xref_table()` is out of scope for this layer** — it is
  `flpdf-egzr.3.3`'s deliverable. Do not add it here.
- **Content-stream parsing is out of scope permanently**, not just for this
  layer. `Parser::object()` / `dictionary()` / `array()` /
  `parse_content_object()` (content-stream mode, `Object::Operator` /
  `Object::InlineImage`) are not part of qpdf's `QPDFObjectHandle` file-graph
  identity/offset contract and must not be touched by this plan at all.
- **Byte-identical output must not change.** This layer touches no writer or
  serialization code. Re-run the full `qpdf-zlib-compat` byte-identical
  suite (list in Task 1) after every task as a fast regression tripwire.
- Follow `.claude/rules/pdf-rust-review-patterns.md` (avoid needless
  `.clone()`, resolve every indirect reference, validate before unsigned
  casts, bound graph traversal) and
  `.claude/rules/pdf-rust-doc-review-patterns.md` (no beads IDs / internal
  jargon in `///`/`//!` doc comments; English only) for every new public
  item.
- Coverage gate before PR: `scripts/patch-coverage.sh --base
  design/flpdf-egzr-3-object-handle` — **read the script's current flags
  first**; do not assume `--features` wiring from memory.
- Commit after every task. Small, reviewable diffs.

## Naming bridge (temporary — resolved in flpdf-egzr.3.2)

| Design name | Collides with | Temporary name used in this layer |
|---|---|---|
| `Pdf::resolve(&ObjectHandle) -> Result<()>` | `Pdf::resolve(ObjectRef) -> Result<Object>` (`reader.rs:1227`) | `Pdf::resolve_object_handle(&ObjectHandle) -> Result<()>` |
| `Pdf::trailer() -> ObjectHandle` | `Pdf::trailer() -> &Dictionary` (`reader.rs:861`) | `Pdf::trailer_handle() -> ObjectHandle` |
| `Pdf::get_object_handle(ObjectRef) -> ObjectHandle` | none | `Pdf::get_object_handle` (no collision, use design name) |
| `Pdf::get_all_objects() -> Result<Vec<ObjectHandle>>` | none (existing `object_refs`/`live_object_refs` return `Vec<ObjectRef>`, different return type, no name reuse needed but avoid confusion) | `Pdf::get_all_object_handles() -> Result<Vec<ObjectHandle>>` |
| `Pdf::get_xref_table()` | n/a — **out of scope this layer**, see Global Constraints | (not added) |

## File Structure

- Create: `crates/flpdf/src/object_handle.rs` — `ObjectHandle`, crate-private
  `ObjectValue`, identity/parsed-offset state, public direct-handle factories.
- Create: `crates/flpdf/tests/object_handle_parity_tests.rs` — cross-cutting
  identity/resolution/parity integration tests (cycles, ObjStm, dangling,
  decrypt, materialization round-trip).
- Modify: `crates/flpdf/src/lib.rs` — register `mod object_handle;` and
  `pub use object_handle::ObjectHandle;`.
- Modify: `crates/flpdf/src/reader.rs` — add the canonical handle registry
  and a *new*, additive `resolve_object_handle` that dual-writes alongside
  the untouched legacy engine (Task 6); later make `resolve` /
  `resolve_borrowed` / `set_object` / `delete_object` thin bridge wrappers
  around it (Task 8); add `get_object_handle`, `get_all_object_handles`,
  `trailer_handle`.
- `crates/flpdf/src/cache.rs` (`ObjectCache`/`CacheEntry`) is **not**
  modified. `resolve_to_cache` and its private callees keep depending on it
  exactly as today, as the untouched legacy engine's own internal
  bookkeeping — see Task 8's explicit note on why this is not shrunk or
  repurposed.
- Modify: `crates/flpdf/src/parser.rs` — add a file-object-body handle
  construction path with parsed-offset capture (Task 7), without touching
  `object()`/`dictionary()`/`array()`/`parse_content_object()`.
- Untouched (verified by `git diff --stat` in Task 10): every file listed in
  Global Constraints' allowlist exclusion.

---

### Task 1: Confirm clean baseline

**Files:** none (verification only)

**Step 1: Build with the byte-identical feature**

Run: `cargo build --workspace --features qpdf-zlib-compat`
Expected: builds clean, no warnings treated as errors.

**Step 2: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all `test result: ok` lines, 0 failed, across every binary.

**Step 3: Run every CI-enumerated byte-identical test**

Run (from `.github/workflows/ci.yml`'s `qpdf-zlib-compat` block):
```bash
cargo test -p flpdf --features qpdf-zlib-compat --test zlib_compat_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_null_visibility_tests
cargo test -p flpdf --features qpdf-zlib-compat --test deterministic_id_qpdf_parity_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay
cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_baseline_static_id -- --nocapture
cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_matrix_baseline -- --nocapture
cargo test -p flpdf-qtest-tools --features qpdf-zlib-compat --test e2e
```
Expected: every one passes with 0 failed. If anything fails here, stop — this
is a pre-existing problem unrelated to this plan and must be resolved or
reported before any code change.

**Step 4: No commit** (verification only, nothing changed).

---

### Task 2: `ObjectValue` and `ObjectHandle` scaffold with identity semantics

**Files:**
- Create: `crates/flpdf/src/object_handle.rs`
- Modify: `crates/flpdf/src/lib.rs` (add `mod object_handle;` and
  `pub use object_handle::ObjectHandle;` near the existing `pub use` block,
  e.g. next to the existing `pub use` of `Object`/`ObjectRef`)

**Step 1: Write the failing tests**

```rust
// crates/flpdf/src/object_handle.rs
#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn direct_handle_clone_shares_identity_not_a_deep_copy() {
        let handle = ObjectHandle::integer(42);
        let clone = handle.clone();
        assert!(handle.ptr_eq(&clone));
    }

    #[test]
    fn two_direct_handles_with_equal_value_are_distinct_identity() {
        let a = ObjectHandle::integer(42);
        let b = ObjectHandle::integer(42);
        assert!(!a.ptr_eq(&b));
    }

    #[test]
    fn direct_handle_reports_direct_not_indirect() {
        let handle = ObjectHandle::integer(1);
        assert!(handle.is_direct());
        assert!(!handle.is_indirect());
        assert_eq!(handle.object_ref(), None);
    }

    #[test]
    fn indirect_handle_retains_object_ref_before_resolution() {
        let object_ref = ObjectRef::new(5, 0);
        let handle = ObjectHandle::new_indirect_unresolved(object_ref, 0);
        assert!(handle.is_indirect());
        assert!(!handle.is_direct());
        assert_eq!(handle.object_ref(), Some(object_ref));
    }

    #[test]
    fn cloning_an_indirect_handle_shares_the_same_slot() {
        let object_ref = ObjectRef::new(5, 0);
        let handle = ObjectHandle::new_indirect_unresolved(object_ref, 0);
        let clone = handle.clone();
        assert!(handle.ptr_eq(&clone));
    }
}
```

`ptr_eq`, `new_indirect_unresolved` are `pub(crate)` test/internal-only
constructors — they exist so this layer's own tests can assert identity
without needing a live `Pdf`; they are not part of the AC1-required public
surface (`get_object_handle`, etc. come from `Pdf` in Task 5).

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib object_handle::identity_tests`
Expected: FAIL — `ObjectHandle` does not exist yet.

**Step 3: Write the minimal implementation**

```rust
//! The core object-handle graph: shared, cloneable identity for direct and
//! indirect PDF objects, with qpdf-compatible parsed-offset tracking.
//!
//! qpdf correspondence: `QPDFObjectHandle` (`include/qpdf/QPDFObjectHandle.hh`)
//! and its backing `QPDFValue` (`libqpdf/qpdf/QPDFValue.hh`).

use crate::ObjectRef;
use std::cell::RefCell;
use std::rc::Rc;

/// The no-offset sentinel qpdf uses for values that were not parsed from a
/// source position (`QPDFValue`'s parsed offset starts at `-1` and is set
/// only while still negative; see
/// `libqpdf/qpdf/QPDFValue.hh:90-100,149-152`).
pub(crate) const NO_PARSED_OFFSET: i64 = -1;

/// A shared, cloneable handle to a PDF object.
///
/// Cloning a handle is O(1) and does not deep-copy the underlying value;
/// every clone of an indirect handle shares the same canonical identity and
/// resolution state.
#[derive(Clone)]
pub struct ObjectHandle(Repr);

#[derive(Clone)]
enum Repr {
    Direct(Rc<RefCell<DirectSlot>>),
    Indirect(Rc<RefCell<IndirectSlot>>),
}

struct DirectSlot {
    #[allow(dead_code)] // populated starting Task 3
    value: Option<()>, // placeholder until ObjectValue lands in Task 3
    parsed_offset: i64,
}

pub(crate) enum IndirectState {
    Unresolved,
    // Resolved/Missing/etc. variants land in Task 6 alongside the real
    // resolution engine cutover.
}

struct IndirectSlot {
    object_ref: ObjectRef,
    #[allow(dead_code)]
    state: IndirectState,
    parsed_offset: i64,
}

impl ObjectHandle {
    /// True if this handle wraps a value constructed directly, without an
    /// indirect object number/generation.
    pub fn is_direct(&self) -> bool {
        matches!(self.0, Repr::Direct(_))
    }

    /// True if this handle refers to an indirect object.
    pub fn is_indirect(&self) -> bool {
        matches!(self.0, Repr::Indirect(_))
    }

    /// The object number/generation for an indirect handle, or `None` for a
    /// direct one.
    pub fn object_ref(&self) -> Option<ObjectRef> {
        match &self.0 {
            Repr::Indirect(slot) => Some(slot.borrow().object_ref),
            Repr::Direct(_) => None,
        }
    }

    /// True if `self` and `other` share the same underlying storage — the
    /// same canonical object, not merely an equal value.
    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Direct(a), Repr::Direct(b)) => Rc::ptr_eq(a, b),
            (Repr::Indirect(a), Repr::Indirect(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    pub(crate) fn new_indirect_unresolved(object_ref: ObjectRef, offset: i64) -> Self {
        let _ = offset; // real Unresolved{offset} state lands in Task 6
        Self(Repr::Indirect(Rc::new(RefCell::new(IndirectSlot {
            object_ref,
            state: IndirectState::Unresolved,
            parsed_offset: NO_PARSED_OFFSET,
        }))))
    }

    fn new_direct(parsed_offset: i64) -> Self {
        Self(Repr::Direct(Rc::new(RefCell::new(DirectSlot {
            value: None,
            parsed_offset,
        }))))
    }

    // Minimal factory to satisfy this task's tests; the full factory set
    // (null/boolean/real/name/string/array/dictionary/stream) is Task 3.
    pub(crate) fn integer(_value: i64) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }
}
```

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib object_handle::identity_tests`
Expected: PASS, 5 passed.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/lib.rs
git commit -m "feat(object_handle): scaffold ObjectHandle identity"
```

---

### Task 3: Parsed offset — sentinel, storage, and the full contract matrix

Implements the design's Parsed-Offset Contract table
(`docs/superpowers/specs/2026-07-30-xref-parsed-offset-object-handle-design.md`,
"Parsed-Offset Contract" section) for handles constructed directly (not yet
through the parser — Task 7 wires the parser).

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod parsed_offset_tests {
    use super::*;

    #[test]
    fn public_factory_direct_handles_default_to_no_offset_sentinel() {
        for handle in [
            ObjectHandle::null(),
            ObjectHandle::boolean(true),
            ObjectHandle::integer(1),
            ObjectHandle::real(1.5),
            ObjectHandle::name(b"Foo".to_vec()),
            ObjectHandle::string(b"bar".to_vec()),
            ObjectHandle::array(Vec::new()),
            ObjectHandle::dictionary(Vec::new()),
        ] {
            assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        }
    }

    #[test]
    fn new_indirect_unresolved_starts_at_no_offset_sentinel() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn set_parsed_offset_is_retained_once_set() {
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(100);
        assert_eq!(handle.get_parsed_offset(), 100);
    }

    #[test]
    fn first_nonnegative_offset_is_retained_a_second_set_is_ignored() {
        // "The first nonnegative offset assigned to a value is retained.
        // Resolution, cache access, unparse, and writer planning do not
        // recompute or replace it." (design, Parsed-Offset Contract)
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(100);
        handle.set_parsed_offset_if_unset(200);
        assert_eq!(handle.get_parsed_offset(), 100);
    }
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib object_handle::parsed_offset_tests`
Expected: FAIL — `get_parsed_offset`, `set_parsed_offset_if_unset`, `null`,
`boolean`, `real`, `name`, `string`, `array`, `dictionary` don't exist yet.

**Step 3: Write the minimal implementation**

Add to `object_handle.rs`:

```rust
impl ObjectHandle {
    /// The qpdf-compatible signed parsed offset. `-1` means the value was
    /// not parsed from a source position (`QPDFObjectHandle::getParsedOffset`,
    /// `include/qpdf/QPDFObjectHandle.hh:415-419`).
    pub fn get_parsed_offset(&self) -> i64 {
        match &self.0 {
            Repr::Direct(slot) => slot.borrow().parsed_offset,
            Repr::Indirect(slot) => slot.borrow().parsed_offset,
        }
    }

    /// Record `offset` as the parsed offset, but only if none has been set
    /// yet (matches qpdf: "set only while still negative",
    /// `libqpdf/qpdf/QPDFValue.hh:149-152`). Called by the parser (Task 7);
    /// exposed here so identity/offset tests do not need a live parser.
    pub(crate) fn set_parsed_offset_if_unset(&self, offset: i64) {
        let mut set = |current: &mut i64| {
            if *current < 0 {
                *current = offset;
            }
        };
        match &self.0 {
            Repr::Direct(slot) => set(&mut slot.borrow_mut().parsed_offset),
            Repr::Indirect(slot) => set(&mut slot.borrow_mut().parsed_offset),
        }
    }

    pub fn null() -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }
    pub fn boolean(_value: bool) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }
    pub fn real(_value: f64) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }
    pub fn name(_value: Vec<u8>) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }
    pub fn string(_value: Vec<u8>) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }
    pub fn array(_children: Vec<ObjectHandle>) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }
    pub fn dictionary(_entries: Vec<(Vec<u8>, ObjectHandle)>) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }
}
```

Note: these factories still discard their payload (`DirectSlot.value` stays
`None`) — `ObjectValue` itself (the real payload enum: Null/Boolean/Integer/
Array-of-handles/Dictionary-of-handles/Stream) is Task 4's job. Keeping
payload storage out of this task keeps the parsed-offset contract test
isolated from the value-representation task, per bite-sized-task granularity.

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib object_handle`
Expected: PASS, all tests in the module (Task 2's + Task 3's) green.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): parsed-offset sentinel and set-once contract"
```

---

### Task 4: `ObjectValue` payload — the real value representation

Replaces the `DirectSlot.value: Option<()>` placeholder with the actual
qpdf-shaped value enum, and gives every factory its real payload. Array and
dictionary children are `ObjectHandle`s (design: "Arrays and dictionaries
contain child `ObjectHandle` values rather than raw recursive values").

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod object_value_tests {
    use super::*;

    #[test]
    fn integer_handle_round_trips_its_value() {
        let handle = ObjectHandle::integer(42);
        assert_eq!(handle.as_integer(), Some(42));
    }

    #[test]
    fn array_handle_holds_child_handles_not_raw_values() {
        let child = ObjectHandle::integer(7);
        let array = ObjectHandle::array(vec![child.clone()]);
        let children = array.as_array().expect("array");
        assert_eq!(children.len(), 1);
        assert!(children[0].ptr_eq(&child));
    }

    #[test]
    fn dictionary_handle_preserves_insertion_of_child_handles() {
        let value = ObjectHandle::name(b"Type".to_vec());
        let dict = ObjectHandle::dictionary(vec![(b"Key".to_vec(), value.clone())]);
        let entries = dict.as_dictionary().expect("dictionary");
        assert!(entries.get(b"Key".as_slice()).unwrap().ptr_eq(&value));
    }

    #[test]
    fn null_handle_is_null() {
        assert!(ObjectHandle::null().is_null());
        assert!(!ObjectHandle::integer(0).is_null());
    }

    #[test]
    fn real_literal_handle_preserves_the_non_canonical_source_literal() {
        // Object::RealLiteral exists so a non-canonical source spelling
        // (e.g. ".4") survives unparse byte-identically. The handle payload
        // must carry the same two fields, or byte-identical output breaks
        // the moment a real-literal round-trips through this layer.
        let handle = ObjectHandle::real_literal(0.4, b".4".to_vec());
        assert_eq!(handle.as_real_literal(), Some((0.4, b".4".to_vec())));
    }
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib object_handle::object_value_tests`
Expected: FAIL — `as_integer`/`as_array`/`as_dictionary`/`is_null` don't exist,
and every factory currently discards its argument.

**Step 3: Write the minimal implementation**

```rust
pub(crate) enum ObjectValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    RealLiteral { value: f64, literal: Vec<u8> },
    Name(Vec<u8>),
    String(Vec<u8>),
    Array(Vec<ObjectHandle>),
    Dictionary(std::collections::BTreeMap<Vec<u8>, ObjectHandle>),
    Stream { dict: ObjectHandle, data: Vec<u8> },
}

struct DirectSlot {
    value: ObjectValue,
    parsed_offset: i64,
}
```

Update `new_direct` to take an `ObjectValue`, and every factory to build the
real payload:

```rust
impl ObjectHandle {
    fn new_direct(value: ObjectValue, parsed_offset: i64) -> Self {
        Self(Repr::Direct(Rc::new(RefCell::new(DirectSlot {
            value,
            parsed_offset,
        }))))
    }

    pub fn null() -> Self {
        Self::new_direct(ObjectValue::Null, NO_PARSED_OFFSET)
    }
    pub fn boolean(value: bool) -> Self {
        Self::new_direct(ObjectValue::Boolean(value), NO_PARSED_OFFSET)
    }
    pub fn integer(value: i64) -> Self {
        Self::new_direct(ObjectValue::Integer(value), NO_PARSED_OFFSET)
    }
    pub fn real(value: f64) -> Self {
        Self::new_direct(ObjectValue::Real(value), NO_PARSED_OFFSET)
    }
    pub fn name(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::Name(value), NO_PARSED_OFFSET)
    }
    pub fn string(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::String(value), NO_PARSED_OFFSET)
    }
    pub fn array(children: Vec<ObjectHandle>) -> Self {
        Self::new_direct(ObjectValue::Array(children), NO_PARSED_OFFSET)
    }
    pub fn dictionary(entries: Vec<(Vec<u8>, ObjectHandle)>) -> Self {
        Self::new_direct(
            ObjectValue::Dictionary(entries.into_iter().collect()),
            NO_PARSED_OFFSET,
        )
    }
    /// Preserves a non-canonical source literal (e.g. `.4`) for byte-identical
    /// unparse, mirroring `Object::RealLiteral`.
    pub fn real_literal(value: f64, literal: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::RealLiteral { value, literal }, NO_PARSED_OFFSET)
    }

    pub fn as_real_literal(&self) -> Option<(f64, Vec<u8>)> {
        self.with_value(|value| match value {
            Some(ObjectValue::RealLiteral { value, literal }) => Some((*value, literal.clone())),
            _ => None,
        })
    }

    pub fn is_null(&self) -> bool {
        self.with_value(|value| matches!(value, Some(ObjectValue::Null) | None))
    }

    pub fn as_integer(&self) -> Option<i64> {
        self.with_value(|value| match value {
            Some(ObjectValue::Integer(n)) => Some(*n),
            _ => None,
        })
    }

    pub fn as_array(&self) -> Option<Vec<ObjectHandle>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Array(children)) => Some(children.clone()),
            _ => None,
        })
    }

    pub fn as_dictionary(&self) -> Option<std::collections::BTreeMap<Vec<u8>, ObjectHandle>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => Some(entries.clone()),
            _ => None,
        })
    }

    // `None` for an unresolved indirect handle — value access on an
    // unresolved handle must not perform hidden I/O (design, `Pdf` section).
    // Real `Some(..)` for a resolved indirect handle lands in Task 6.
    fn with_value<T>(&self, f: impl FnOnce(Option<&ObjectValue>) -> T) -> T {
        match &self.0 {
            Repr::Direct(slot) => f(Some(&slot.borrow().value)),
            Repr::Indirect(_) => f(None),
        }
    }
}
```

Note: `as_array`/`as_dictionary` cloning `Vec<ObjectHandle>` /
`BTreeMap<..., ObjectHandle>` here clones a collection of cheap `Rc` clones,
not a deep value copy — consistent with
`.claude/rules/pdf-rust-review-patterns.md` §1 (unnecessary `.clone()` on
deep trees is the anti-pattern; cloning a handle is O(1) by construction).

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib object_handle`
Expected: PASS, all tests in the module green.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): real ObjectValue payload for direct handles"
```

---

### Task 5: Canonical indirect-handle registry on `Pdf`

Gives `Pdf` the "canonical indirect-handle cache" the design requires:
repeated `get_object_handle` calls for the same `ObjectRef` return the same
shared handle. This task only wires identity/registration; lazy resolution
into a real value is Task 6.

**Files:**
- Modify: `crates/flpdf/src/reader.rs`

**Step 1: Write the failing test**

Add near the existing reader tests (or a new `#[cfg(test)] mod
object_handle_registry_tests` in `reader.rs`):

```rust
#[test]
fn get_object_handle_returns_the_same_canonical_handle_for_repeated_calls() {
    let mut pdf = open_minimal_test_pdf(); // reuse whatever existing helper
                                            // opens a minimal fixture in
                                            // this file's other tests
    let object_ref = ObjectRef::new(1, 0);
    let first = pdf.get_object_handle(object_ref);
    let second = pdf.get_object_handle(object_ref);
    assert!(first.ptr_eq(&second));
}

#[test]
fn get_object_handle_is_indirect_with_the_requested_ref() {
    let mut pdf = open_minimal_test_pdf();
    let object_ref = ObjectRef::new(1, 0);
    let handle = pdf.get_object_handle(object_ref);
    assert!(handle.is_indirect());
    assert_eq!(handle.object_ref(), Some(object_ref));
}
```

(Use whichever existing fixture-opening helper this test file already has —
grep `reader.rs`'s existing `#[cfg(test)]` module for the pattern before
writing a new one, to avoid duplicating fixture setup.)

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib reader::object_handle_registry_tests`
Expected: FAIL — `get_object_handle` doesn't exist on `Pdf` yet.

**Step 3: Write the minimal implementation**

Add a new field to `Pdf` (near the existing `cache: ObjectCache` field,
`reader.rs:52-110`):

```rust
handle_registry: std::collections::BTreeMap<ObjectRef, ObjectHandle>,
```

Initialize it empty wherever `cache: ObjectCache::from_offsets(..)` is
currently initialized (same constructor site(s) — grep `ObjectCache::from_offsets`
in `reader.rs` for the exact call site(s) before editing).

```rust
/// Returns the canonical handle for `object_ref`, creating and registering
/// an unresolved one on first request. Repeated calls for the same
/// `object_ref` return the same shared handle
/// (`QPDF::getObject`-equivalent identity — indirect object identity is
/// cached and stable across calls; see design "Pdf" section).
///
/// Does not perform file I/O or force body parsing.
pub fn get_object_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
    self.handle_registry
        .entry(object_ref)
        .or_insert_with(|| ObjectHandle::new_indirect_unresolved(object_ref, NO_PARSED_OFFSET))
        .clone()
}
```

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib reader::object_handle_registry_tests`
Expected: PASS.

**Step 5: Run the full test suite as a regression check**

Run: `cargo test --workspace`
Expected: unchanged pass count plus the 2 new tests — this task is purely
additive so nothing existing should move.

**Step 6: Commit**

```bash
git add crates/flpdf/src/reader.rs
git commit -m "feat(reader): canonical indirect ObjectHandle registry on Pdf"
```

---

### Task 6: Lazy resolution engine — dual-write alongside the untouched legacy cache

This is the core of "the production reader path cuts over in this layer"
(AC2) and is the largest task in this plan. **It is a strictly additive,
dual-write step: the existing `resolve_to_cache` / `read_object_at` /
`resolve_compressed_entry` / `resolve_pending_stream_length` /
`decrypt_resolved_object` machinery (`reader.rs:1269-1510`) is not rewritten
and not touched.** `Pdf::resolve` / `Pdf::resolve_borrowed` keep calling it
exactly as today, so the entire existing test suite (~2841+ tests) stays
green throughout this task — that is the whole point of doing it this way
instead of rewriting the engine in place. This task only adds a *new*,
independent, so-far-unused-by-production-consumers method,
`Pdf::resolve_object_handle`, that piggy-backs on the untouched engine's
output to populate the new handle graph.

**Do not pre-decide every internal data-structure detail before starting.**
The design explicitly defers this ("Exact Rust data structures and internal
synchronization remain implementation details to be settled by TDD as long
as they preserve the approved public behavior and ownership"). What follows
is the **contract this task must satisfy**, expressed as tests, plus the
**existing behaviors it must reproduce exactly** (cited by file:line so they
can be diffed against, not re-derived from scratch) — reproduced "for free"
here because this task calls the untouched engine rather than reimplementing
it.

**Behaviors this task relies on unchanged (read these sites first, do not edit them):**

1. Cyclic `/Length` handling via the existing `Reserved` cache-entry guard
   (`reader.rs:1338-1377`, `resolve_pending_stream_length`;
   `cache.rs:65-80`). Untouched.
2. Compressed (ObjStm) member resolution
   (`reader.rs:1480-1510+`, `resolve_compressed_entry`). Untouched.
3. Bounded read-then-fallback-to-EOF policy
   (`reader.rs:1256-1322`, `resolution_fallbacks_remaining`). Untouched.
4. Decryption hook (`decrypt_resolved_object`, `reader.rs:1435,1500`).
   Untouched.
5. Missing/freed/broken-compressed references resolve to `Null`, not an
   error (`reader.rs:1244-1245,1253`). Untouched.

**Parsed offsets are intentionally not populated by this task.** The handle
graph this task builds gets its `ObjectValue` by converting
(`lift`-ing — see Task 8) the `Object` that the untouched legacy engine
already produced, so every offset stays at the `-1` sentinel here. This is
not the forbidden "reparse to reconstruct provenance" pattern (design,
"Parser" section) — it is simply a feature (real parsed offsets) not yet
built. Task 7 replaces *only* the source of the resolved `ObjectValue` for
plain file objects (native construction with real offsets, during the one
parse pass, per design lines 145-147) while leaving this task's dual-write
wiring and every behavior above completely intact.

**Step 1: Write the failing tests (contract, not implementation)**

Add to `crates/flpdf/tests/object_handle_parity_tests.rs` (new file):

```rust
// Cross-cutting resolution parity: every test here asserts *observable*
// behavior (what resolving a handle yields), not internal representation.

#[test]
fn resolving_an_indirect_handle_yields_its_parsed_value() {
    let mut pdf = open_fixture("minimal.pdf");
    let object_ref = pdf.root_ref().expect("root");
    let handle = pdf.get_object_handle(object_ref);
    pdf.resolve_object_handle(&handle).unwrap();
    assert!(handle.as_dictionary().is_some());
}

#[test]
fn dangling_indirect_handle_resolves_to_null_not_error() {
    let mut pdf = open_fixture("minimal.pdf");
    let dangling_ref = ObjectRef::new(999_999, 0); // not in this fixture's xref
    let handle = pdf.get_object_handle(dangling_ref);
    pdf.resolve_object_handle(&handle).unwrap();
    assert!(handle.is_null());
    assert_eq!(handle.get_parsed_offset(), -1);
}

#[test]
fn cyclic_length_holder_resolves_without_infinite_recursion() {
    // Use (or add) a fixture with a stream whose /Length is an indirect
    // reference back to itself or to a mutually-referential holder —
    // mirror whatever existing fixture reader.rs's own cyclic-/Length
    // test already uses (grep reader.rs test module for "cyclic" first).
    let mut pdf = open_fixture("cyclic-length.pdf");
    let stream_ref = /* whichever ref the fixture defines */;
    let handle = pdf.get_object_handle(stream_ref);
    pdf.resolve_object_handle(&handle).unwrap(); // must return, not hang
}

#[test]
fn compressed_objstm_member_resolves_through_the_handle_graph() {
    let mut pdf = open_fixture("objstm-source.pdf"); // reuse an existing
                                                      // ObjStm-bearing fixture
    let member_ref = /* a known compressed member's ObjectRef */;
    let handle = pdf.get_object_handle(member_ref);
    pdf.resolve_object_handle(&handle).unwrap();
    assert!(!handle.is_null());
}

#[test]
fn repeated_object_ref_occurrences_share_the_resolved_value() {
    let mut pdf = open_fixture("minimal.pdf");
    let object_ref = pdf.root_ref().expect("root");
    let first = pdf.get_object_handle(object_ref);
    pdf.resolve_object_handle(&first).unwrap();
    let second = pdf.get_object_handle(object_ref);
    assert!(first.ptr_eq(&second));
    assert!(second.as_dictionary().is_some()); // already resolved via `first`
}
```

Pick real existing fixture paths (`crates/flpdf/tests/fixtures/` or wherever
this crate's tests currently source PDFs — check an existing `reader.rs` or
`tests/reader_tests.rs` test for the exact helper/path convention) rather
than inventing new ones; only add a new fixture if no existing one exercises
cyclic `/Length` or ObjStm members (there almost certainly is one already,
since `reader.rs:1338-1377` and `resolve_compressed_entry` are already
tested against the legacy `Object`/`resolve_borrowed` path — reuse that same
fixture and construction as its own parity check).

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --test object_handle_parity_tests`
Expected: FAIL — `resolve_object_handle` doesn't exist yet, fixtures may need
adding.

**Step 3: Implement**

Add a minimal `IndirectState` — deliberately *not* a mirror of every
`CacheEntry` variant, because this task never drives byte-level resolution
itself; it only records what the untouched legacy engine already decided:

```rust
// object_handle.rs
pub(crate) enum IndirectState {
    NotYetResolved,
    Resolved(ObjectValue),
    Missing, // ref absent from source_xref_entries — the dangling case
}
```

Add crate-internal setters (`pub(crate)` only, not public API):
`set_resolved(&self, value: ObjectValue)`, `set_missing(&self)`, and a
`pub(crate) fn is_resolved(&self) -> bool` reader, operating on the handle's
own `RefCell`.

In `reader.rs`, add:

```rust
/// Resolves `handle` in place if it is an unresolved indirect handle.
/// Direct handles and already-resolved indirect handles are a no-op.
///
/// Temporary bridge implementation: delegates to the existing *private*
/// `resolve_to_cache` engine (unchanged) and converts its result — this
/// task does not reimplement decryption, ObjStm decoding, or the cyclic
/// `/Length` guard, it reuses them as-is. Parsed offsets are not populated
/// here; see Task 7.
///
/// Deliberately calls `resolve_to_cache` (private), **not** the public
/// `resolve_borrowed` — Task 8 repoints `resolve_borrowed` to call *this*
/// method, so routing through the public method here would recurse.
pub fn resolve_object_handle(&mut self, handle: &ObjectHandle) -> Result<()> {
    let Some(object_ref) = handle.object_ref() else {
        return Ok(()); // direct handle, already has a value
    };
    if handle.is_resolved() {
        return Ok(());
    }
    self.resolve_to_cache(object_ref)?; // untouched private engine
    match self.cache.entry(object_ref) {
        Some(CacheEntry::Resolved(object)) => {
            let object = object.clone();
            let value = self.lift(&object);
            handle.set_resolved(value);
        }
        _ => handle.set_missing(), // Missing/Deleted/Reserved-still (should
                                    // not happen post-resolve_to_cache)/None
    }
    Ok(())
}
```

Matching on `self.cache.entry(object_ref)` after `resolve_to_cache` (rather
than re-deriving presence from the xref table separately) naturally
distinguishes a *genuinely* null resolved object (`CacheEntry::Resolved(Object::Null)`
— present in the file, correctly lifted to `ObjectValue::Null`) from an
actually missing/freed/dangling ref (`CacheEntry::Missing`/`Deleted`/`None`
— correctly `set_missing()`). Write a test for this distinction explicitly
(a fixture where some indirect object's real value is the literal PDF
`null`, vs. a ref to an object number absent from the xref table) since both
produce `ObjectHandle::is_null() == true` from the outside and are easy to
conflate if the implementation is refactored later.

Add a minimal `Pdf::lift(&mut self, object: &Object) -> ObjectValue` in
`reader.rs`, scoped to what this task's own tests need (a full,
production-grade version — including the `Object::Reference` →
`get_object_handle` identity-preserving case and the `Stream`
dict/data split — is Task 8's job, which extends this exact function
rather than introducing a second one):

```rust
fn lift(&mut self, object: &Object) -> ObjectValue {
    match object {
        Object::Null => ObjectValue::Null,
        Object::Boolean(b) => ObjectValue::Boolean(*b),
        Object::Integer(n) => ObjectValue::Integer(*n),
        Object::Real(r) => ObjectValue::Real(*r),
        Object::RealLiteral { value, literal } => ObjectValue::RealLiteral {
            value: *value,
            literal: literal.clone(),
        },
        Object::Name(name) => ObjectValue::Name(name.clone()),
        Object::String(s) => ObjectValue::String(s.clone()),
        Object::Array(items) => {
            ObjectValue::Array(items.iter().map(|item| self.lift_to_handle(item)).collect())
        }
        Object::Dictionary(dict) => ObjectValue::Dictionary(
            dict.iter()
                .map(|(k, v)| (k.to_vec(), self.lift_to_handle(v)))
                .collect(),
        ),
        // Stream dict/data split and Object::Operator/InlineImage (content-
        // stream-only, never reachable from a file-object resolve) are out
        // of this task's own test scope; Task 8 completes them.
        _ => ObjectValue::Null,
    }
}

fn lift_to_handle(&mut self, object: &Object) -> ObjectHandle {
    match object {
        Object::Reference(object_ref) => self.get_object_handle(*object_ref),
        direct => {
            let value = self.lift(direct);
            ObjectHandle::from_value(value) // new tiny constructor: Direct
                                             // handle, offset -1 — add next
                                             // to `new_direct` in object_handle.rs
        }
    }
}
```

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --test object_handle_parity_tests`
Expected: PASS for every test in this task's list (Step 1) — the `Stream`
dict/data split noted as out of scope above is not exercised by any of
them.

**Step 5: Run the full existing suite as a regression check**

Run: `cargo test --workspace`
Expected: **unchanged pass count, zero moved tests** — this task adds a new
method and calls the untouched legacy engine from it; it does not modify
`resolve`, `resolve_borrowed`, `set_object`, `delete_object`, `cache.rs`, or
any of the resolution internals listed above. If anything outside
`object_handle.rs`/the new test file changed behavior, something leaked
beyond this task's intended additive scope — revert and redo.

**Step 6: Commit**

```bash
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/reader.rs crates/flpdf/tests/object_handle_parity_tests.rs
git commit -m "feat(reader): dual-write ObjectHandle resolution alongside the legacy engine"
```

---

### Task 7: File-object parser integration with offset capture

Wires the parser to build `ObjectHandle`/`ObjectValue` (with parsed offsets)
for the file-object graph, and to request canonical indirect handles from
`Pdf` for `N G R` instead of producing a bare `Object::Reference`.

**Explicit scope boundary:** `Parser::object()` / `dictionary()` / `array()`
/ `integer_or_ref()` / `parse_content_object()` (`parser.rs:238-482`) power
**both** file-object-body parsing and content-stream parsing today, and share
qpdf-recovery-sensitive logic (`top_level_no_reference`, bad-token recovery
counters, `MAX_PARSE_DEPTH`). **Do not fork or duplicate this logic.**
Content-stream parsing must keep using exactly what exists today, untouched,
forever (see Global Constraints).

**This task has one required shape, not a choice — but "the same recursive
walk" does not mean "one literal function."** The design's real constraint
is about *provenance*, not Rust code-sharing mechanics: "The parser builds
the handle graph directly and assigns parsed offsets during node
construction. **It does not return a parallel metadata tree**, and `Pdf`
does not reparse an object later solely to reconstruct provenance" (design,
"Parser" section, lines 145-147). Parsing to `Object` first and then
converting/annotating with offsets in a second pass — via a parallel offset
table or a `Parser::position()` re-scan — **is** exactly the forbidden
parallel-metadata-tree/reparse pattern, and remains forbidden. What is
**not** forbidden: `content_dictionary()`/`finish_content_dictionary()`
already exist as container-construction functions entirely separate from
`dictionary()`, used only for `ParserMode::Content` — the split between
content-mode and object-mode container construction predates this task.
Given that, a new object-mode-only container-construction path (e.g.
`dictionary_handle()`/`array_handle()`/`object_inner_handle()`) that
assigns offsets during its own single parse pass is compliant, **provided**
every byte-identical-load-bearing leaf decision is factored into one
shared function called by both the legacy `Object`-producing path and the
new handle-producing path — never reimplemented a second time with
similar-looking code. Concretely, these three decisions must each live in
exactly one function, called from both paths:

- `real_object`'s literal-preservation branch (`value.to_string().as_bytes()
  == token.raw` → `Real` vs `RealLiteral`, `parser.rs:495`).
- `integer_or_ref`'s three-token backtracking / `unread_token` ordering
  (`parser.rs:479-480`).
- `top_level_no_reference && depth == 1` gating.

Duplicating the *container shells* (the loop that walks tokens and builds
either an `Object::Array`/`Object::Dictionary` or an `ObjectHandle` array/
dictionary) is acceptable — those shells differ inherently by output type
anyway. Duplicating any *token-level decision* is not: if the handle path
recomputes the real-literal comparison or the backtracking order itself
instead of calling the same extracted helper, a future edit to one and not
the other silently moves output bytes, which is exactly the failure mode
this plan exists to prevent. `top_level_no_reference`, bad-token recovery,
and `MAX_PARSE_DEPTH` remain byte-for-byte the same shared code either way.
Content-stream parsing (`ParserMode::Content`) keeps using
`content_dictionary()`/`parse_content_object()` exactly as today, completely
untouched; only the file-object-body path gains a handle-producing route.
`Object::Reference(object_ref)` at `integer_or_ref` (`parser.rs:459-482`)
becomes, on the handle path only, a call to the same canonical-handle
lookup this plan already added (`Pdf::get_object_handle`), not a bare
`ObjectRef` value — the legacy `Object`-producing path keeps returning a
bare `Object::Reference` exactly as today (Task 8 is where legacy callers
change, not this task).

Read `parser.rs:192-509` in full (further than the excerpt already reviewed
this session) and run the existing `parser.rs` test module first, to see
exactly how deeply recovery/bad-token behavior is entangled with the
leaf/container construction sites before touching them.

**Step 1: Extract the shared leaf decisions, then add a parallel
object-mode-only handle-construction path**

First extract the three decisions listed above into standalone functions
(or methods taking only the token/value they need, no `Object`- or
`ObjectHandle`-shaped state) if they are not already isolated enough to
call from two call sites without duplication. Then add the new
handle-producing container/leaf construction (parallel to `object_inner`/
`dictionary`/`array`/`integer_or_ref`, object-mode only — never invoked from
`ParserMode::Content`) that builds `ObjectHandle`s via the extracted
helpers, with the offset read from `token.start`/`self.position()` at the
exact same call site that constructs the value, never a second pass.

Task 6's `resolve_object_handle` currently resolves every indirect handle
by delegating to the legacy engine and calling `Pdf::lift` (offset `-1`
always). This task changes that delegation **only for the plain
uncompressed-file-object case** (`IndirectState` sourced from
`XrefEntry::Uncompressed`): instead of calling `resolve_borrowed` + `lift`,
it calls this task's new native parse-to-handle entry point directly on the
object's source bytes, so the resulting handle (and every direct child
in its tree) carries a real parsed offset from construction. The
`Compressed` (ObjStm-member) case is explicitly **not** required to gain
real per-member offset coordinates in this task — full ObjStm-relative/
recovered/hybrid-xref offset coordinate correctness is `flpdf-egzr.3.3`'s
scope (see Non-goals); leaving ObjStm members on the Task 6 `lift`-based
path (offset `-1`) here is a recorded, intentional scope boundary, not a
silent gap. Since `IndirectState`/the handle itself carries no xref-class
tag, deciding which route to take requires consulting the xref table
(`source_xref_entries()` or equivalent) for the object's `XrefEntry` variant
*before* choosing native-parse vs. legacy-`lift`; get this branch right and
add a test that a compressed (ObjStm) member handle still resolves via the
Task 6 `lift` path with offset `-1`, not through native parsing against a
file-relative offset that would be wrong for it (see Step 2's coverage of
this exact scenario, mirroring the design's Parsed-Offset Contract table).

**Step 2: Write the failing tests**

Cover the design's full Parsed-Offset Contract table with one asymmetric
fixture per row (design, "Test Strategy > Parsed offsets and xref"):

```rust
// crates/flpdf/tests/object_handle_parity_tests.rs (extend Task 6's file)

#[test]
fn scalar_parsed_offset_is_the_token_start_not_leading_whitespace() {
    // fixture: "   42" as an indirect object body — offset must be 3, not 0.
}

#[test]
fn array_parsed_offset_is_the_bracket_not_the_first_child() {
    // fixture: "[  1 2 3]" — array's own offset is `[`'s position; the
    // first child integer's offset is its own token start, independently.
}

#[test]
fn dictionary_parsed_offset_is_the_double_angle_bracket() {
    // fixture: "<<  /A 1>>" — offset is `<<`'s position.
}

#[test]
fn parsed_null_offset_is_always_the_sentinel() {
    // "the parser constructs QPDF_Null without assigning a description or
    // offset" (design, Fixed qpdf Facts) — even though `null` has a token
    // position, its handle's parsed offset must stay -1.
}

#[test]
fn stream_handle_and_its_dictionary_handle_have_distinct_offsets() {
    // stream object's own offset = encoded stream-data start;
    // its dictionary's offset = the `<<` start. They must differ and both
    // must be correct.
}

#[test]
fn indirect_reference_child_is_the_canonical_handle_not_a_fresh_value() {
    // parse "1 0 obj << /Kid 5 0 R >> endobj" against a Pdf that already
    // has object 5 registered (or registers it during parse); the
    // dictionary's "Kid" entry handle must `ptr_eq` `pdf.get_object_handle(ObjectRef::new(5, 0))`.
}

#[test]
fn compressed_object_stream_member_keeps_the_sentinel_offset_via_legacy_lift() {
    // an ObjStm-member indirect object (XrefEntry::Compressed) must still
    // resolve through Task 6's lift path (offset -1), not be native-parsed
    // against a file-relative offset that would be meaningless for it.
}

#[test]
fn real_literal_round_trips_through_native_parsing() {
    // a file-object body containing ".4" resolved via the native handle
    // path: `as_real_literal()` must return `(0.4, b".4")`, exercising the
    // shared real_object literal-preservation decision through the new
    // path specifically (Task 4 only ever exercised this via a hand-built
    // direct handle, never through actual parsing).
}
```

**Step 2b: Cross-path parity tests — the drift tripwire**

The existing `parser::` test suite only exercises the legacy `Object`-
producing path; it will stay green even if the new handle-producing path
silently diverges from it on malformed input, since nothing there calls the
new path. Add tests that feed the same malformed/edge-case object-mode
bodies through *both* `Pdf::resolve` (legacy path) and
`resolve_object_handle` (new native path) and assert identical error
messages/offsets for at least: an unterminated dictionary (`"expected byte
47"`-class error from `dictionary()`), an unterminated array (`"unexpected
EOF in array"`-class error), and nesting past `MAX_PARSE_DEPTH` (`"object
nesting too deep"`-class error). If any pair diverges, that is a bug in
this task's implementation, not a pre-existing difference to document away.

**Step 3-4: Implement per Step 1, run to green.**

**Step 5: Run the FULL existing parser test suite**

Run: `cargo test -p flpdf --lib parser::`
Expected: 100% unchanged pass — this is the highest-risk task in the plan
for silently breaking qpdf recovery parity; treat any behavior change here
(not just compile failure) as a blocking regression.

**Step 6: Run the full workspace suite + byte-identical suite (Task 1's list)**

Expected: unchanged.

**Step 7: Commit**

```bash
git add crates/flpdf/src/parser.rs crates/flpdf/src/object_handle.rs crates/flpdf/tests/object_handle_parity_tests.rs
git commit -m "feat(parser): build the ObjectHandle graph with parsed offsets for file objects"
```

---

### Task 8: Materialization bridge — `resolve`/`resolve_borrowed` cutover

Makes the legacy public API a thin view over the new engine, with **zero
signature changes**. This is the task that actually removes "the old raw
Reference, cloned-value reader resolution, and cache-value production
routes" as AC3 requires, replacing them with the named-and-bounded bridge.

**Hard precondition carried over from Task 7 (bd issue flpdf-jjxb, blocks
flpdf-egzr.3.2): verify string decryption before this task ships.** Task
7's native-parse path (the plain-uncompressed-object case) builds
`ObjectValue::String` directly from raw source bytes, bypassing the legacy
engine's decryption step — unlike the already-resolved legacy `Object` (via
`resolve_to_cache`) it runs alongside, whose `String` values *are*
decrypted. This was inert in Task 7 (no accessor read a handle's decrypted
string content yet), but this task is exactly where that risk becomes live:
once `resolve_borrowed`/`resolve` route through `materialize()`, ANY
encrypted PDF whose `/Info` dictionary, or any other string-bearing
dictionary, lives in an `XrefEntry::Uncompressed` object (very common —
`/Info` in particular is frequently not object-stream-compressed even in
otherwise-hybrid files) surfaces raw ciphertext bytes as plaintext through
the public API. This is an output-visible correctness bug the moment it
ships, not a deferred one.

**Required Step 0, before writing any other code for this task:** resolve
this concretely, don't re-derive the design from scratch — the reasoning
below already traces the two facts that decide it:

- `decrypt_resolved_object` (`reader.rs:1870`) — the legacy engine's own
  decryption entry point — takes an *already-built* `Object` tree (`mut
  object: Object`) and mutates it in place via `decrypt_object_strings`
  (`reader.rs:2148`) → `decrypt_strings_in_object`, which walks the whole
  tree recursively. It does not take individual string byte slices in
  isolation anywhere in the existing call graph.
- `resolve_to_cache` (`reader.rs:1746`) calls `decrypt_resolved_object`
  **before** `self.cache.set_resolved(object_ref, object)` — so the
  `object` available at `native_parse_uncompressed_value`'s call site in
  `resolve_object_handle` (passed in from the `CacheEntry::Resolved(object)`
  match) is *already fully decrypted*. It is the correct source of truth
  for every `String` value in that object's tree today; nothing needs to
  reach into `parser.rs` to fix this.
- **Do not attempt a lockstep walk between the native `ObjectValue` tree
  and the legacy `Object` tree** to copy over decrypted string bytes
  position-by-position. The two trees can have different shapes for a
  malformed object with a duplicate dictionary key (native's
  `BTreeMap<Vec<u8>, ObjectHandle>` construction and the legacy engine's own
  duplicate-key handling are not guaranteed to agree byte-for-byte on which
  duplicate wins) — a silent mismatch here is exactly the kind of
  correctness hazard this plan exists to prevent, so this approach is
  rejected, not merely discouraged.
- **First check whether this is even reachable today** before building
  anything: does any existing fixture (or a quick one you construct) route
  an *encrypted* PDF through an `XrefEntry::Uncompressed` object containing
  a `String`? Search `crates/flpdf/tests/` (`encrypt_writer_smoke.rs`,
  `reader_tests.rs`, `check_tests.rs`, and the `encrypted/` fixture
  directories) for existing coverage; if none exists, construct a minimal
  one (encrypted classic PDF, `/Info` dict as a plain uncompressed object
  with a `/Title` string) and drive it through `resolve_borrowed` before
  and after your Step 3 changes.
- If reachable (expected — this is the likely outcome and you should plan
  for it): fix it by having `native_parse_uncompressed_value`'s caller (or
  `native_parse_uncompressed_value` itself) decrypt strings in the
  *materialized* `Object` — not the native `ObjectValue` — using the
  existing `decrypt_object_strings`/`decrypt_resolved_object` machinery
  unchanged, keyed by the same `object_ref` `resolve_object_handle` already
  has in scope. Concretely: this task already builds `materialize()`
  (`ObjectHandle -> Object`, this task's own Step 3) — for a handle that
  was populated via the native-parse route specifically (not every handle;
  Compressed-branch handles built via `lift(&object, 0)` already carry
  correctly-decrypted strings from `object` and must not be decrypted
  twice), decrypt the *materialized* `Object` before caching it in
  `legacy_materialized_memo`, mirroring exactly what `resolve_to_cache`
  already does for the legacy engine, with the same "skip decryption for
  the `/Encrypt` dictionary object itself" guard `decrypt_resolved_object`
  already has. This reuses 100% of the existing crypto call graph (RC4/
  AES-128/AES-256 dispatch, per-object key derivation, explicit crypt
  filters) with zero duplication — the only new code is deciding *when* to
  invoke it (native-parse-populated handles only) and threading whatever
  marker distinguishes "this handle was populated via native parse" (the
  parsed offset being non-sentinel is one candidate signal already
  available on every handle; consider whether that's sufficient and
  precise enough, or whether a small explicit flag is clearer — your call,
  document whichever you pick).
- If NOT reachable today (e.g. every fixture with encrypted strings happens
  to route them through Compressed/ObjStm objects only): the honest move is
  smaller — add a test that pins this specific claim (encrypted + `String`
  + `Uncompressed` is not exercised by any current fixture/test), keep
  `flpdf-jjxb` open against `flpdf-egzr.3.2` rather than closing it here,
  and do not build the decryption machinery above speculatively. State
  explicitly in your report which branch you took and why.

**Separately, before writing Step 3's `resolve_borrowed` rewrite, check
this borrow-lifetime question**: the sketch below has `set_object`/
`delete_object` `remove` the corresponding `legacy_materialized_memo` entry.
Grep the crate for any call site that holds a `resolve_borrowed(..)`-
returned `&Object` borrow **across** a `set_object`/`delete_object` call on
the same `Pdf`. If the borrow checker already forbids this today (likely,
since `resolve_borrowed` takes `&mut self` and so does `set_object`), there
is nothing to do; if some existing call site currently compiles by relying
on `resolve_borrowed`'s current borrow shape in a way this rewrite would
break across many of the 350 call sites, that is a compile-time discovery
you want made now, not treated as a surprise mid-implementation.

**Files:**
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/object_handle.rs`
- `crates/flpdf/src/cache.rs` is **not** modified in this task — see Step 3.

**Step 1: Write the failing tests**

```rust
// crates/flpdf/tests/object_handle_parity_tests.rs

#[test]
fn legacy_resolve_borrowed_still_returns_object_reference_for_indirect_children() {
    // The bridge must materialize an indirect array/dict *child* back to
    // Object::Reference(ObjectRef) — NOT recursively resolve it — so every
    // existing consumer match on `Object::Reference(..)` keeps working
    // exactly as today.
    let mut pdf = open_fixture("nested-indirect.pdf");
    let object_ref = pdf.root_ref().expect("root");
    let resolved = pdf.resolve_borrowed(object_ref).unwrap();
    let dict = resolved.as_dict().expect("dict");
    let kid = dict.get("Kid").expect("Kid key");
    assert!(matches!(kid, Object::Reference(_)));
}

#[test]
fn legacy_resolve_matches_legacy_resolve_borrowed_cloned() {
    let mut pdf = open_fixture("minimal.pdf");
    let object_ref = pdf.root_ref().expect("root");
    let owned = pdf.resolve(object_ref).unwrap();
    let borrowed = pdf.resolve_borrowed(object_ref).unwrap();
    assert_eq!(&owned, borrowed);
}

#[test]
fn legacy_resolve_borrowed_on_dangling_ref_is_null_object() {
    let mut pdf = open_fixture("minimal.pdf");
    let dangling = ObjectRef::new(999_999, 0);
    assert_eq!(pdf.resolve_borrowed(dangling).unwrap(), &Object::Null);
}

#[test]
fn materialize_then_set_object_round_trips_structurally() {
    let mut pdf = open_fixture("minimal.pdf");
    let object_ref = pdf.root_ref().expect("root");
    let resolved = pdf.resolve(object_ref).unwrap();
    pdf.set_object(object_ref, resolved.clone());
    assert_eq!(pdf.resolve(object_ref).unwrap(), resolved);
}
```

Plus, covering the three round-trip hazards a byte-identical-suite failure
would otherwise catch late and expensively:

```rust
#[test]
fn real_literal_survives_resolve_set_object_round_trip() {
    // Object::RealLiteral{value, literal} preserves a non-canonical source
    // spelling (e.g. ".4") for byte-identical unparse. If materialize/lift
    // ever drops `literal` and falls back to Object::Real, this must fail.
    let mut pdf = open_fixture("real-literal.pdf"); // an object whose value
                                                     // is literally `.4`
    let object_ref = /* that object's ref */;
    let resolved = pdf.resolve(object_ref).unwrap();
    assert!(matches!(&resolved, Object::RealLiteral { literal, .. } if literal == b".4"));
    pdf.set_object(object_ref, resolved.clone());
    assert_eq!(pdf.resolve(object_ref).unwrap(), resolved);
}

#[test]
fn stream_dictionary_parsed_offset_survives_resolve_set_object_round_trip() {
    // Stream is `{ dict: Dictionary, data: Vec<u8> }` by value; the handle
    // graph keeps the stream dictionary as a *separate handle* with its own
    // `<<`-start parsed offset (design requirement). materialize() flattens
    // that into a plain Dictionary; lift() must re-split it by *reusing the
    // existing canonical dictionary handle* rather than minting a fresh one
    // with a lost offset — this test is the tripwire for getting that wrong.
    let mut pdf = open_fixture("minimal.pdf"); // any fixture with a stream
    let stream_ref = /* that stream's object ref */;
    let handle = pdf.get_object_handle(stream_ref);
    pdf.resolve_object_handle(&handle).unwrap();
    let dict_offset_before = /* the stream's dict-handle parsed offset,
                                 via whatever accessor Task 7 exposed */;
    let resolved = pdf.resolve(stream_ref).unwrap();
    pdf.set_object(stream_ref, resolved);
    let dict_offset_after = /* same accessor, same handle/ref */;
    assert_eq!(dict_offset_before, dict_offset_after);
}

#[test]
fn transformed_stream_refs_and_recovered_stream_eols_still_populate_on_the_handle_path() {
    // reader.rs:1437-1446 sets these side-tables alongside CacheEntry::Resolved
    // in the untouched legacy engine. resolve_object_handle must not bypass
    // the code path that sets them for a stream that needs stream-payload
    // transformation or has a recovered EOL.
    let mut pdf = open_fixture("recovered-stream-eol.pdf"); // reuse whichever
                                                             // existing fixture
                                                             // reader.rs's own
                                                             // recovery tests use
    let stream_ref = /* that stream's ref */;
    let handle = pdf.get_object_handle(stream_ref);
    pdf.resolve_object_handle(&handle).unwrap();
    assert!(pdf.recovered_stream_eols_contains(stream_ref)); // whatever the
                                                              // existing
                                                              // crate-internal
                                                              // accessor for
                                                              // this is
}
```

Plus: re-run **every existing** `resolve_borrowed`/`resolve` test in the
crate unchanged (350 call sites across 47 files, per this session's survey)
— they are this task's real regression suite, since none of their source is
being edited.

**Step 2: Run to verify the new tests fail, old ones still pass**

Run: `cargo test --workspace`
Expected: the 7 new tests FAIL (bridge not wired yet); all ~2841+ existing
tests still PASS (nothing touched yet).

**Step 3: Implement**

- Add `pub(crate) fn materialize(&self) -> Object` on `ObjectHandle`
  (`object_handle.rs`): converts a *resolved* handle's `ObjectValue` into an
  `Object`, recursively, but for an `ObjectValue::Array`/`Dictionary` child
  that is itself an **indirect** `ObjectHandle`, emit `Object::Reference(child.object_ref().unwrap())`
  — do not recurse into it. `ObjectValue::Stream { dict, data }` flattens
  `dict` (itself an `ObjectHandle`, resolved and materialized) into a plain
  `Dictionary` for `Object::Stream { dict, data }`. `ObjectValue::RealLiteral`
  round-trips its `literal` bytes unchanged. This is the exact semantic the
  parser already had for `Object::Reference` before this plan (`parser.rs`'s
  `integer_or_ref`, `459-482`) and is what every consumer's direct
  `Object::Reference(..)` match already expects.
- Extend Task 6's `Pdf::lift` (do not add a second function) so it also
  handles `Object::Stream { dict, data }` — converting `dict` via
  `lift_to_handle` exactly like any other nested dictionary — and so
  `lift_to_handle`'s `Object::Reference(r)` arm keeps calling
  `self.get_object_handle(r)` (the canonical handle), preserving identity
  for `set_object`'s round trip.
- **Do not repurpose or shrink `cache.rs`'s `ObjectCache`/`CacheEntry`.**
  Task 6 established that `resolve_to_cache` and its private callees
  (`read_object_at`, `resolve_compressed_entry`, `resolve_pending_stream_length`,
  `decrypt_resolved_object`) are untouched and still depend on the full
  `CacheEntry` state machine (`Unresolved`/`Compressed`/`Reserved` included)
  to do the actual byte-level work — deleting or shrinking it would break
  the very engine `resolve_object_handle` calls. What AC3 requires removed
  is `ObjectCache`'s *architectural role as the public-facing production
  route* — that role now belongs to `ObjectHandle`/`get_object_handle`/
  `resolve_object_handle`. `ObjectCache` remains, fully intact, as a
  private implementation detail of the untouched legacy byte-parsing engine.
  Record this explicitly (it is a deliberate, bounded scope decision, not
  silent scope creep) rather than claiming a cache.rs simplification that
  the untouched Task 6 engine cannot actually tolerate.
- Add a new, separate field for the bridge's own materialized-`Object` memo
  (it must be distinct from `self.cache`, since after Task 7, `self.cache`
  is **not** guaranteed to be populated for a handle that was resolved via
  Task 7's native-parsing path rather than via `resolve_to_cache`):
  ```rust
  legacy_materialized_memo: std::collections::BTreeMap<ObjectRef, Object>,
  ```
- Rewrite `Pdf::resolve_borrowed`, checking the memo **before** materializing
  (memoize-if-absent, not unconditional insert — a fresh `materialize()` per
  call on 350 call sites, several in loops of dozens of iterations
  (`page_extract_tests` 38, `json_inspect` 31, `embedded_files` 27), would
  reproduce the exact quadratic-resolution shape this repo has already fixed
  once, see `reader.rs:1256-1268`'s doc comment on that history):
  ```rust
  pub fn resolve_borrowed(&mut self, object_ref: ObjectRef) -> Result<&Object> {
      let handle = self.get_object_handle(object_ref);
      self.resolve_object_handle(&handle)?;
      if !self.legacy_materialized_memo.contains_key(&object_ref) {
          let materialized = handle.materialize();
          self.legacy_materialized_memo.insert(object_ref, materialized);
      }
      Ok(self
          .legacy_materialized_memo
          .get(&object_ref)
          .unwrap_or(&NULL_OBJECT))
  }
  ```
  (Keep `resolve`'s existing one-line `self.resolve_borrowed(..)?.clone()`
  body unchanged; it already composes correctly on top of the rewritten
  `resolve_borrowed`.)
- Rewrite `Pdf::set_object`/`Pdf::delete_object` (`reader.rs:1000,1020`) to
  write through `lift()` into the canonical handle graph (updating the
  shared `IndirectSlot`, so every other outstanding clone of that handle
  observes the new value too), and **invalidate** (`remove`, not
  re-`insert` with a stale value) the corresponding
  `legacy_materialized_memo` entry so the next `resolve_borrowed` call
  re-materializes from the updated handle instead of serving the old memo.

**Step 4: Run to verify it passes**

Run: `cargo test --workspace`
Expected: **every** test passes — the 7 new ones and all ~2841+ pre-existing
ones. A pre-existing test failing here means the bridge is not behaviorally
transparent; do not proceed to Task 9 until this is 100% green.

**Step 5: Run the byte-identical suite (Task 1's list)**

Expected: unchanged, all green — this task must not move a single output
byte.

**Step 6: Commit**

```bash
git add crates/flpdf/src/reader.rs crates/flpdf/src/object_handle.rs crates/flpdf/tests/object_handle_parity_tests.rs
git commit -m "feat(reader): materialization bridge - resolve/resolve_borrowed cut onto ObjectHandle graph"
```

---

### Task 9: `get_all_object_handles` and `trailer_handle`

Adds the two remaining pieces of AC1's "cache, and lazy-resolution
contracts" surface that this layer owns (per the Naming Bridge table); pure
additions, no legacy behavior touched.

**Files:**
- Modify: `crates/flpdf/src/reader.rs`

**Step 1: Write the failing tests**

```rust
#[test]
fn get_all_object_handles_returns_indirect_handles_in_object_ref_order() {
    let mut pdf = open_fixture("three-page.pdf");
    let handles = pdf.get_all_object_handles().unwrap();
    assert!(handles.iter().all(ObjectHandle::is_indirect));
    let refs: Vec<_> = handles.iter().map(|h| h.object_ref().unwrap()).collect();
    let mut sorted = refs.clone();
    sorted.sort();
    assert_eq!(refs, sorted);
}

#[test]
fn trailer_handle_is_indirect_or_direct_matching_trailer_dictionary_contents() {
    let mut pdf = open_fixture("minimal.pdf");
    let handle = pdf.trailer_handle();
    let dict = handle.as_dictionary().expect("trailer is a dictionary handle");
    assert!(dict.contains_key(b"Root".as_slice()) || dict.contains_key(b"Size".as_slice()));
}
```

**Step 2-4: RED, implement, GREEN** as in prior tasks. `get_all_object_handles`
mirrors qpdf's `getAllObjects` ordering contract ("returns the indirect
handles in object-cache order", design "Fixed qpdf 11.9.0 Facts" citing
`libqpdf/QPDF.cc:1285-1294`) — for this layer, "object-cache order" means
`ObjectRef` order over everything currently registered in
`handle_registry` plus every ref in `source_xref_entries()` not yet
registered (call `get_object_handle` for each to force registration, mirror
`ObjectCache::from_offsets`'s full-table registration). The
qpdf-equivalent **dangling-reference preparation** step mentioned in the
design ("performs the qpdf-equivalent dangling-reference preparation") is
`flpdf-egzr.3.3`'s full scope (it ties into xref/recovery semantics this
layer explicitly does not own) — implement the ordering/registration
contract here and leave a one-line doc note (not a public one — internal
`//` comment only, per doc-review-patterns rule 2) that full dangling-prep
parity is `flpdf-egzr.3.3`'s deliverable.

`trailer_handle` converts the existing `self.trailer: Dictionary` field
(already always direct/in-memory, never itself an indirect object per PDF
spec) into an `ObjectHandle` via `lift`.

**Step 5: Commit**

```bash
git add crates/flpdf/src/reader.rs
git commit -m "feat(reader): get_all_object_handles and trailer_handle"
```

---

### Task 10: Zero-consumer-diff verification (mandatory gate)

**This task is a gate, not new functionality. Do not skip it.**

**Step 1: Diff everything, then assert the changed set is a subset of the allowlist**

Enumerating consumer files to exclude (as an earlier draft of this plan did)
is fragile — the full-crate survey that grounded this plan already found
public `Object`-returning signatures and direct enum matches in more files
than fit in any hand-written list (`page_merge.rs`, `page_rotate.rs`,
`acroform_field_prune.rs`, `outline_dest_remap.rs`,
`writer/object_streams.rs`, `pages/repair.rs`, and others). Invert the
check instead: list everything that changed, then assert every path is one
this plan was actually allowed to touch.

```bash
git diff --name-only design/flpdf-egzr-3-object-handle...HEAD -- crates/ > /tmp/objecthandle_changed_files.txt
comm -23 \
  <(sort /tmp/objecthandle_changed_files.txt) \
  <(printf '%s\n' \
      crates/flpdf/src/object_handle.rs \
      crates/flpdf/src/object.rs \
      crates/flpdf/src/reader.rs \
      crates/flpdf/src/parser.rs \
      crates/flpdf/src/lib.rs \
      crates/flpdf/tests/object_handle_parity_tests.rs \
    | sort)
```
Expected: **empty output** — every changed path under `crates/` is one of
the six above. Any other line printed is a leak: stop, do not proceed to
Task 11, and revisit whichever of Tasks 6-9 introduced it. (If a task
legitimately needed a new test fixture file, add its exact path to the
second `printf` list here and note why in the commit that added it — do not
silently widen this check.)

**Step 2: Full workspace build and test, no features**

Run: `cargo build --workspace && cargo test --workspace`
Expected: clean build, all green.

**Step 3: No commit** (verification only).

---

### Task 11: Full regression — clippy, fmt, byte-identical, doctest, deviation recording

**Step 0: Record the `Rc<RefCell<..>>` deviation (CLAUDE.md category (B), condition 3)**

This is a category-(B) internal-structure substitute (qpdf's
`std::shared_ptr<QPDFValue>` → Rust `Rc<RefCell<..>>`), permitted because it
does not affect output bytes (verified throughout this plan by the
byte-identical suite) and preserves qpdf's algorithm/ordering. Condition 3
requires recording it explicitly in two places:

1. Add a one-line deviation note to `crates/flpdf/src/object_handle.rs`'s
   module doc (`//!`), e.g.: `// Deviation: shared handle identity uses
   Rc<RefCell<..>> in place of qpdf's std::shared_ptr<QPDFValue> — internal
   structure only, does not affect output bytes (see
   docs/qpdf-correspondence.md).` (as a `//` comment, not a `///`/`//!` doc
   line, per `.claude/rules/pdf-rust-doc-review-patterns.md` — this is
   implementation history, not user-facing API documentation).
2. Update `docs/qpdf-correspondence.md`:
   - Row for `QPDFObjectHandle.cc` (currently line 58: `🔀 アクセサが各所に
     散在 (flpdf-mfir)`) — add `object_handle.rs` alongside `object.rs` as a
     target module, since object identity/lazy-resolution now lives there.
   - Row for `QPDFObject.cc` / `QPDFValue.cc` (currently line 60: `✅`) —
     same addition; change the note to reflect that this layer is a
     `flpdf-egzr.3.1`-owned in-progress cutover if the row's classification
     needs to move off `✅` (check the current state at edit time; do not
     silently leave it claiming full correspondence if the bridge changes
     the module boundary).
   - Add a new ⚪ row (or extend the existing one) documenting the
     `Rc<RefCell<..>>` vs `std::shared_ptr<QPDFValue>` substitution
     specifically, in the "逸脱候補（⚪）" section's format (module, line
     count, flpdf replacement, ⚪), matching the existing style at e.g. line
     73 (`InputSource` 系 → `Read + Seek`).

**Step 1: Format check**

Run: `cargo fmt --check`
Expected: no diff. If it fails, run `cargo fmt` and re-verify, then fold the
formatting fix into the current task's commit (do not create a separate
"fmt" commit per CLAUDE.md's frequent-but-meaningful commit norm).

**Step 2: Clippy, all features, warnings denied**

Run: `cargo clippy --workspace --all-features -- -D warnings`
Expected: clean.

**Step 3: Doctest**

Run: `cargo test --workspace --doc`
Expected: clean. Every new public item (`ObjectHandle` and its public
methods) needs a one-line summary doc comment per
`.claude/rules/pdf-rust-doc-review-patterns.md` §3 — English only, no beads
IDs, no internal jargon (§1, §5).

**Step 4: Every byte-identical test from Task 1, re-run**

Run the full block from Task 1, Step 3.
Expected: all green, byte-for-byte unchanged from the Task 1 baseline.

**Step 5: Full workspace test, all features**

Run: `cargo test --workspace --all-features`
Expected: all green.

**Step 6: Commit any fixes surfaced by this task**

```bash
git add -A
git commit -m "chore: fmt/clippy/doctest cleanup for ObjectHandle cutover"
```
(Only if Steps 1-3 required changes; skip this commit if everything was
already clean.)

---

### Task 12: Coverage gate and qualitative review

**Step 1: Ensure all work is committed**

Run: `git status --short`
Expected: clean (patch-coverage errors on a dirty tree by design).

**Step 2: Read the current coverage script before trusting remembered flags**

Run: `sed -n '1,60p' scripts/patch-coverage.sh` (or open it) — confirm the
current `--base`/`--lcov`/`--allow-dirty` flag names before invoking it;
prior-session memory of this script's flags is not authoritative.

**Step 3: Run the coverage gate against the design branch**

Run: `scripts/patch-coverage.sh --base design/flpdf-egzr-3-object-handle`
(add whatever `--lcov`/feature flags the script currently documents).
Expected: exit 0, 100% coverage on every changed line in `crates/flpdf`.
Any uncovered line: add a test, or (only if genuinely untestable) annotate
`// cov:ignore: <reason>` and record the reason in the eventual PR
description (CLAUDE.md Test Coverage §2).

**Step 4: Qualitative check (CLAUDE.md Test Coverage §4)**

Manually confirm — beyond the 100% line number — that real assertions exist
for: unresolved-handle access (no hidden I/O), dangling/missing/freed
indirect refs, the cyclic-`/Length` guard, ObjStm member resolution, and the
materialization round-trip (`Object::Reference` preserved at indirect
boundaries, not recursively resolved). These are exactly the AC4 categories
this issue lists ("direct/indirect identity, repeated ObjectRef identity,
unresolved access, reference chains, dangling/free/missing objects, cycles,
streams, ObjStm, and failure paths") — check each has a real test, not just
a covered line.

**Step 5: No commit** (verification only, unless Step 3 required test
additions — then commit those with a normal `test:` message).

---

### Task 13: Update beads and prepare for PR

**Step 1: Record differential/verification commands on the issue**

`bd update flpdf-egzr.3.1 --notes "..."` — record the exact commands run in
Tasks 1, 11, and 12 (AC5: "Record exact focused Rust and pinned-qpdf
differential commands with expected output and exit behavior"; note that
AC5's *pinned-qpdf* differential commands are N/A for this layer since no
CLI-facing helper exists yet — that is `flpdf-egzr.3.4` — record this
explicitly as a scoped exception rather than silently skipping AC5).

**Step 2: Full-survey snapshot (AC7)**

If this repository has an existing "full survey" / allowlist-regression
script or convention (check for one before inventing a new mechanism — grep
for "allowlist" and "full-survey" or "full_survey" across `scripts/` and
`docs/`), run it before/after and record zero regressions. If no such
mechanism exists yet at this layer, record that explicitly rather than
fabricating one.

**Step 3: Push and open the PR against the design branch**

```bash
git push -u origin feat/flpdf-egzr-3-1-objecthandle-cutover
gh pr create --base design/flpdf-egzr-3-object-handle \
  --title "feat(flpdf): ObjectHandle graph and reader cutover (flpdf-egzr.3.1)" \
  --body "$(cat <<'EOF'
## Summary
- Introduces the core ObjectHandle graph (identity, lazy resolution, parsed
  offset) and cuts the production parser/cache/reader route onto it.
- Adds a named, bounded materialization bridge so every existing consumer of
  Object/Pdf::resolve/Pdf::resolve_borrowed keeps compiling and behaving
  identically; zero consumer files touched (verified: `git diff --stat`
  against this PR's base is empty for the consumer allowlist).
- Design: docs/superpowers/specs/2026-07-30-xref-parsed-offset-object-handle-design.md
- Plan: docs/superpowers/plans/2026-07-30-objecthandle-graph-reader-cutover.md
- Deviation note (CLAUDE.md 逸脱の2分類 (B)): Rc<RefCell<..>> canonical handle
  identity replaces qpdf's std::shared_ptr<QPDFValue> as an internal-only
  substitute; output bytes are unaffected (full qpdf-zlib-compat
  byte-identical suite re-verified green, see PR checks).
- Naming bridge (temporary, removed in flpdf-egzr.3.2): resolve_object_handle,
  trailer_handle, get_all_object_handles — see plan's Naming Bridge table.

## Test plan
- [ ] cargo test --workspace (all features)
- [ ] Full qpdf-zlib-compat byte-identical suite (list in plan Task 1) - unchanged
- [ ] cargo clippy --workspace --all-features -- -D warnings
- [ ] cargo fmt --check
- [ ] scripts/patch-coverage.sh --base design/flpdf-egzr-3-object-handle - 100%
- [ ] git diff --stat against consumer-file allowlist - empty
EOF
)"
```

**Step 4: Session completion**

Follow CLAUDE.md's Session Completion protocol in full
(`git pull --rebase`, `bd dolt push`, `git push`, `git status` clean) once
the PR is opened.

---

## Non-goals (explicitly out of scope for this plan)

- Editing any consumer file (see Global Constraints allowlist).
- `Pdf::get_xref_table()` — `flpdf-egzr.3.3`.
- Full ObjStm-relative / recovered-table / hybrid-xref parsed-offset
  coordinate correctness beyond what Task 7's contract tests exercise —
  `flpdf-egzr.3.3`.
- Removing the public raw `Object` enum, `resolve_borrowed`, or
  clone-based resolution paths — `flpdf-egzr.3.2` (this plan keeps them,
  it only changes what backs them).
- `flpdf-qtest-tools::driver::Handle` removal — `flpdf-egzr.3.2`.
- The `test_xref`/`test_parsedoffset` Rust helper binaries — `flpdf-egzr.3.4`.
- Content-stream object parsing changes of any kind — out of scope
  permanently, not deferred.
