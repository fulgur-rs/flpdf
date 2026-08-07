# qpdf Live Direct Containment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace flpdf's additive flattened direct-owner roots with qpdf-current-membership-derived reverse edges so detached direct children cannot dirty or incrementally emit former owners.

**Architecture:** qpdf's dictionary/array/stream forward children remain the source of truth. `DirectSlot` stores one weak immediate-parent edge per current forward occurrence plus separate additive Pdf identity provenance; root lookup walks only those reverse edges with cycle detection. Every constructor and mutation updates immediate edges after releasing `RefCell` borrows.

**Tech Stack:** Rust workspace; `Rc`, `Weak`, `RefCell`, `BTreeSet`; `ObjectHandle`; `Pdf`; pinned qpdf 11.9.0; Cargo tests and llvm-cov.

## Global Constraints

- Pinned qpdf 11.9.0 commit `3b97c9bd266b7c32ea36d3536e22dab77412886d` is the semantic oracle.
- Current qpdf-style forward container membership is authoritative; reverse edges are derived incremental-writer bookkeeping only.
- Pdf identity provenance and live containment roots remain separate.
- Do not add a sentinel, panic, document-wide resolve scan, qpdf-incompatible error branch, or flattened descendant path-count model.
- Preserve indirect boundaries, current foreign-Pdf filtering, and existing direct-cycle policy.
- Do not implement direct-null replacement, exact `checkOwnership` diagnostics, StreamDataProvider, filter pipeline, or Filespec migration.
- Every production change starts with a focused failing test and follows RED to GREEN.
- Work only in `.worktrees/flpdf-25kg.3.22-live-containment`; preserve `main`.

---

### Task 1: Pin the stale-owner failures with RED tests

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs` (`identity_tests` and `mutation_tests`)
- Modify: `crates/flpdf/src/reader.rs` (`tests`)

**Interfaces:**
- Consumes: existing `ObjectHandle::{containing_object_refs,replace_key,remove_key,replace_array_item,replace_array_items,replace_direct_value}` and `Pdf::{mark_object_handle_dirty,is_dirty,clear_dirty}`.
- Produces: an acceptance matrix that fails specifically because removed forward edges leave flattened roots behind.

- [ ] **Step 1: Add dictionary, sharing, array, and direct-value RED tests**

Add these tests to `object_handle::mutation_tests`:

```rust
#[test]
fn dictionary_detach_removes_only_the_removed_live_path() {
    let owner_ref = ObjectRef::new(7, 0);
    let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
    let child = ObjectHandle::dictionary(vec![]);
    owner.set_resolved(ObjectValue::Dictionary(
        [
            (b"A".to_vec(), child.clone()),
            (b"B".to_vec(), child.clone()),
        ]
        .into_iter()
        .collect(),
    ));

    owner.remove_key(b"A");
    assert_eq!(child.containing_object_refs(), vec![owner_ref]);
    owner.remove_key(b"B");
    assert!(child.containing_object_refs().is_empty());
}

#[test]
fn replacing_a_nested_dictionary_path_detaches_the_old_subtree() {
    let owner_ref = ObjectRef::new(7, 0);
    let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
    let leaf = ObjectHandle::integer(1);
    let nested = ObjectHandle::dictionary(vec![(b"Leaf".to_vec(), leaf.clone())]);
    owner.set_resolved(ObjectValue::Dictionary(
        [(b"Nested".to_vec(), nested.clone())]
            .into_iter()
            .collect(),
    ));

    owner.replace_key(b"Nested", ObjectHandle::dictionary(vec![]));

    assert!(nested.containing_object_refs().is_empty());
    assert!(leaf.containing_object_refs().is_empty());
}

#[test]
fn shared_subtree_loses_only_the_detached_indirect_root() {
    let first_ref = ObjectRef::new(7, 0);
    let second_ref = ObjectRef::new(9, 0);
    let first = ObjectHandle::new_indirect_unresolved(first_ref, -1);
    let second = ObjectHandle::new_indirect_unresolved(second_ref, -1);
    let shared = ObjectHandle::dictionary(vec![]);
    first.set_resolved(ObjectValue::Dictionary(
        [(b"Shared".to_vec(), shared.clone())]
            .into_iter()
            .collect(),
    ));
    second.set_resolved(ObjectValue::Dictionary(
        [(b"Shared".to_vec(), shared.clone())]
            .into_iter()
            .collect(),
    ));

    first.remove_key(b"Shared");

    assert_eq!(shared.containing_object_refs(), vec![second_ref]);
}

#[test]
fn array_and_direct_value_replacement_detach_old_children() {
    let owner_ref = ObjectRef::new(7, 0);
    let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
    let first = ObjectHandle::integer(1);
    let second = ObjectHandle::integer(2);
    let array = ObjectHandle::array(vec![first.clone(), first.clone()]);
    owner.set_resolved(ObjectValue::Dictionary(
        [(b"Array".to_vec(), array.clone())]
            .into_iter()
            .collect(),
    ));

    assert!(array.replace_array_item(0, second.clone()));
    assert_eq!(first.containing_object_refs(), vec![owner_ref]);
    assert_eq!(second.containing_object_refs(), vec![owner_ref]);
    assert!(array.replace_array_items(vec![]));
    assert!(first.containing_object_refs().is_empty());
    assert!(second.containing_object_refs().is_empty());

    let replacement = ObjectHandle::integer(3);
    array.replace_direct_value(ObjectValue::Array(vec![replacement.clone()]));
    assert_eq!(replacement.containing_object_refs(), vec![owner_ref]);
    array.replace_direct_value(ObjectValue::Array(vec![]));
    assert!(replacement.containing_object_refs().is_empty());
}
```

- [ ] **Step 2: Add the Pdf dirty/output RED test**

Add this test beside the existing `mark_object_handle_dirty_*` tests in `reader.rs`:

```rust
#[test]
fn detached_direct_child_neither_dirties_nor_emits_its_former_owner() {
    let owner_ref = ObjectRef::new(1, 0);
    let bytes = classic_pdf_with_bodies(
        &[b"1 0 obj\n<< /Type /Catalog /Child << /Value 1 >> >>\nendobj\n"],
        owner_ref,
    );
    let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("open fixture");
    let owner = pdf.get_object_handle(owner_ref);
    pdf.resolve_object_handle(&owner).unwrap();
    let child = owner.get_key(b"Child");

    owner.remove_key(b"Child");
    pdf.clear_dirty(owner_ref);
    child.replace_key(b"Value", ObjectHandle::integer(2));
    pdf.mark_object_handle_dirty(&child).unwrap();

    assert!(!pdf.is_dirty(owner_ref));
    let mut out = Vec::new();
    write_pdf(&mut pdf, &mut out).expect("incremental write");
    assert_eq!(out, bytes);
}
```

- [ ] **Step 3: Verify RED**

Run:

```bash
cargo test -p flpdf dictionary_detach_removes_only_the_removed_live_path --lib
cargo test -p flpdf replacing_a_nested_dictionary_path_detaches_the_old_subtree --lib
cargo test -p flpdf shared_subtree_loses_only_the_detached_indirect_root --lib
cargo test -p flpdf array_and_direct_value_replacement_detach_old_children --lib
cargo test -p flpdf detached_direct_child_neither_dirties_nor_emits_its_former_owner --lib
```

Expected: each command fails on an assertion that a detached child still reports or dirties `7 0 R`/`1 0 R`; compilation and fixture opening succeed.

- [ ] **Step 4: Commit the RED tests**

```bash
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/reader.rs
git commit -m "test(object-handle): expose stale containment owners"
```

---

### Task 2: Replace flattened roots with immediate live parent edges

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:343-358`
- Modify: `crates/flpdf/src/object_handle.rs:549-685`
- Modify: `crates/flpdf/src/object_handle.rs:1361-1426`

**Interfaces:**
- Produces: `ContainmentParent`, separate `pdf_unique_ids`, immediate attach/detach helpers, and cycle-safe `containment_roots`.
- Preserves: `containing_object_refs`, `containing_object_refs_for_pdf`, and `belongs_to_pdf` call signatures used by `Pdf`.

- [ ] **Step 1: Introduce the live edge data model**

Replace the flattened field with:

```rust
#[derive(Debug)]
struct DirectSlot {
    value: ObjectValue,
    parsed_offset: i64,
    pdf_unique_ids: std::collections::BTreeSet<u64>,
    containment_parents: Vec<ContainmentParent>,
}

#[derive(Debug, Clone)]
enum ContainmentParent {
    Root(ContainmentOwner),
    Direct(Weak<RefCell<DirectSlot>>),
}
```

Keep `ContainmentOwner` unchanged. Initialize both new collections in
`new_direct`.

- [ ] **Step 2: Add exact edge comparison and immediate-child helpers**

Implement helpers with these signatures:

```rust
fn same_containment_parent(left: &ContainmentParent, right: &ContainmentParent) -> bool;
fn direct_children(value: &ObjectValue) -> Vec<ObjectHandle>;
fn containment_parent(&self) -> ContainmentParent;
fn attach_child_to_parent(child: &ObjectHandle, parent: &ContainmentParent);
fn detach_child_from_parent(child: &ObjectHandle, parent: &ContainmentParent);
fn attach_value_children(&self, value: &ObjectValue);
fn detach_value_children(&self, value: &ObjectValue);
```

`same_containment_parent` compares roots by value and direct parents with
`Weak::ptr_eq`. Attach only direct children and push one entry per occurrence.
Detach finds and removes exactly one matching entry. `direct_children` returns
array items, dictionary values, or the stream dictionary and returns an empty
vector for scalars and indirect references. On attach, propagate a root's
`pdf_unique_id`, or every `pdf_unique_id` already recorded by an upgraded
direct parent, through the new child's current direct descendants. Import and
use `BTreeSet` for identity and visited collections.

- [ ] **Step 3: Register edges during direct construction**

Change `new_direct` to create the `Rc` first and then attach current children:

```rust
let handle = Self(Repr::Direct(Rc::new(RefCell::new(DirectSlot {
    value,
    parsed_offset,
    pdf_unique_ids: Default::default(),
    containment_parents: Vec::new(),
}))));
handle.with_value(|value| {
    if let Some(value) = value {
        handle.attach_value_children(value);
    }
});
handle
```

Adjust `into_direct_value` so an exclusively owned direct wrapper detaches its
immediate child edges before `Rc::try_unwrap`; do not detach when the strong
count is greater than one and the method returns `None`.

- [ ] **Step 4: Separate Pdf identity propagation from root lookup**

Replace recursive root stamping with:

```rust
fn associate_pdf_identity(&self, pdf_unique_id: u64, visited: &mut BTreeSet<usize>);
fn containment_roots(&self) -> BTreeSet<ContainmentOwner>;
```

`associate_pdf_identity` records the id on each current direct descendant,
stops at indirect handles, and uses direct `Rc` address identity to terminate
cycles. `containment_roots` walks only `containment_parents`, upgrades weak
direct parents, uses the same visited strategy, and collects roots.

Implement the existing queries as:

```rust
pub(crate) fn containing_object_refs_for_pdf(&self, pdf_unique_id: u64) -> Vec<ObjectRef> {
    self.containment_roots()
        .into_iter()
        .filter(|owner| owner.pdf_unique_id == Some(pdf_unique_id))
        .map(|owner| owner.object_ref)
        .collect()
}

pub(crate) fn belongs_to_pdf(&self, pdf_unique_id: u64) -> bool {
    match &self.0 {
        Repr::Indirect(slot) => slot.borrow().pdf_unique_id == Some(pdf_unique_id),
        Repr::Direct(slot) => {
            let ids = &slot.borrow().pdf_unique_ids;
            ids.is_empty() || ids.contains(&pdf_unique_id)
        }
    }
}
```

- [ ] **Step 5: Attach/detach root edges on indirect state transitions**

For `set_resolved`, clone the old resolved value if present, detach its
immediate children from `ContainmentParent::Root(owner)`, replace the state,
attach the new immediate children, and propagate `owner.pdf_unique_id` when it
is `Some`. Apply the same old-child detachment before `set_missing` and
`disconnect` discard a resolved value.

- [ ] **Step 6: Make dictionary mutation update one immediate edge**

Change `replace_key` and `remove_key` to return the removed child from the
`with_value_mut` closure, release the borrow, detach it once, then attach the
new direct child once. Preserve all existing no-op and self-cycle behavior.

- [ ] **Step 7: Verify the dictionary GREEN subset**

Run:

```bash
cargo test -p flpdf dictionary_detach_removes_only_the_removed_live_path --lib
cargo test -p flpdf replacing_a_nested_dictionary_path_detaches_the_old_subtree --lib
cargo test -p flpdf shared_subtree_loses_only_the_detached_indirect_root --lib
cargo test -p flpdf resolving_an_indirect_dictionary_records_its_direct_child_owner --lib
cargo test -p flpdf associating_direct_owners_stops_at_an_indirect_child --lib
```

Expected: all pass.

- [ ] **Step 8: Commit the live edge foundation**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "refactor(object-handle): derive owners from live edges"
```

---

### Task 3: Cover array and whole-value replacement boundaries

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:1313-1359`
- Modify: `crates/flpdf/src/object_handle.rs:1998-2018`

**Interfaces:**
- Consumes: Task 2 immediate attach/detach helpers.
- Produces: live edge updates for `replace_array_item`, `replace_array_items`, and `replace_direct_value`.

- [ ] **Step 1: Update single array item replacement**

Inside the mutation closure, replace the item with `value.clone()` and return
the old item. After the borrow ends, detach the old item once and attach the new
item once. Preserve the current `false` results for invalid index, non-array,
and direct self-cycle.

- [ ] **Step 2: Update whole-array replacement**

Move the old `Vec<ObjectHandle>` out with `std::mem::replace`, then after the
borrow ends detach every old occurrence and attach every new occurrence. Do not
deduplicate identical children: two identical array positions are two qpdf
forward edges.

- [ ] **Step 3: Update direct-value replacement**

Use `std::mem::replace` on `DirectSlot::value`, release the borrow, detach all
immediate children of the old value, and attach all immediate children of the
new value. Leave `parsed_offset`, `pdf_unique_ids`, and the direct slot's own
parent edges unchanged.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p flpdf array_and_direct_value_replacement_detach_old_children --lib
cargo test -p flpdf replace_array_item_preserves_identity_and_rejects_invalid_slots --lib
cargo test -p flpdf replacing_a_contained_direct_value_propagates_its_owner_to_new_children --lib
cargo test -p flpdf object_handle::mutation_tests --lib
```

Expected: all pass.

- [ ] **Step 5: Commit the mutation coverage**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "fix(object-handle): detach replaced direct children"
```

---

### Task 4: Pin identity, cycle, and incremental-output preservation

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs` (`identity_tests` and `mutation_tests`)
- Modify: `crates/flpdf/src/reader.rs` (`tests` and comments only unless a test exposes a defect)
- Modify: `docs/qpdf-correspondence.md:121-124,441-444`

**Interfaces:**
- Consumes: live root lookup and separate Pdf identity from Tasks 2-3.
- Produces: explicit preservation tests and correspondence documentation for the derived reverse index.

- [ ] **Step 1: Add identity and direct-cycle tests before any corrective code**

Add the identity test to `identity_tests`, where `RecordingResolver` is
available:

```rust
#[test]
fn detached_child_preserves_pdf_identity_without_a_live_root() {
    let owner_ref = ObjectRef::new(7, 0);
    let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
    let owner = ObjectHandle::new_indirect_for_pdf_with_resolver(
        owner_ref,
        NO_PARSED_OFFSET,
        41,
        Rc::downgrade(&resolver),
    );
    let child = ObjectHandle::dictionary(vec![]);
    owner.set_resolved(ObjectValue::Dictionary(
        [(b"Child".to_vec(), child.clone())]
            .into_iter()
            .collect(),
    ));
    owner.remove_key(b"Child");

    assert!(child.belongs_to_pdf(41));
    assert!(!child.belongs_to_pdf(42));
    assert!(child.containing_object_refs_for_pdf(41).is_empty());
}
```

Add the cycle test to `mutation_tests`:

```rust

#[test]
fn current_root_lookup_terminates_on_a_direct_cycle() {
    let owner_ref = ObjectRef::new(7, 0);
    let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
    let first = ObjectHandle::dictionary(vec![]);
    let second = ObjectHandle::dictionary(vec![]);
    first.replace_key(b"Second", second.clone());
    second.replace_key(b"First", first.clone());
    owner.set_resolved(ObjectValue::Dictionary(
        [(b"First".to_vec(), first.clone())]
            .into_iter()
            .collect(),
    ));

    assert_eq!(second.containing_object_refs(), vec![owner_ref]);
}
```

Run both tests immediately. Expected: pass if Task 2 implemented the approved
identity/cycle design correctly; otherwise fail on the exact missing invariant,
which must be fixed before continuing.

- [ ] **Step 2: Verify Pdf dirty/output GREEN and existing rejection behavior**

Run:

```bash
cargo test -p flpdf detached_direct_child_neither_dirties_nor_emits_its_former_owner --lib
cargo test -p flpdf mark_object_handle_dirty --lib
cargo test -p flpdf mark_object_dirty_makes_a_replace_key_mutation_survive_a_default_incremental_write --lib
```

Expected: all pass; the first leaves the former owner clean, while existing
attached and foreign-Pdf cases retain their prior behavior.

- [ ] **Step 3: Update correspondence documentation**

In the `QPDFObjectHandle.cc` and `QPDFObject.cc/QPDFValue.cc` rows, record that
`object_handle.rs` now models current direct membership as one immediate weak
reverse edge per qpdf forward edge for incremental dirty lookup, with Pdf
identity stored separately. In the approved `Rc<RefCell<..>>` deviation row,
state that the reverse index is derived bookkeeping and does not alter shared
identity or emitted bytes.

- [ ] **Step 4: Run focused component tests**

```bash
cargo fmt --all -- --check
cargo test -p flpdf object_handle --lib
cargo test -p flpdf mark_object_handle_dirty --lib
cargo test -p flpdf --test object_handle_parity_tests
cargo test -p flpdf --test reader_tests
```

Expected: all pass with no warnings.

- [ ] **Step 5: Commit preservation tests and documentation**

```bash
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/reader.rs docs/qpdf-correspondence.md
git commit -m "test(object-handle): verify live owner boundaries"
```

---

### Task 5: Full verification, coverage, and delivery

**Files:**
- Modify: only files already changed by Tasks 1-4.

**Interfaces:**
- Produces: verified branch, persisted Bead closure, and pushed Git branch.

- [ ] **Step 1: Run formatting and lint gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: both exit 0 with no warnings.

- [ ] **Step 2: Run full functional and qpdf byte-parity suites**

```bash
cargo test -p flpdf
cargo test --workspace
cargo test --workspace --features qpdf-zlib-compat
```

Expected: all exit 0. The feature run executes the committed qpdf byte-identical corpus with the zlib-compatible backend.

- [ ] **Step 3: Commit any verification-only correction using TDD**

If a gate exposes a defect, first add or identify the focused failing test,
watch it fail, make the minimal correction, rerun the focused test, and commit
only that correction:

```bash
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/reader.rs docs/qpdf-correspondence.md
git commit -m "fix(object-handle): complete live containment coverage"
```

Skip this commit when no correction is needed.

- [ ] **Step 4: Run fresh changed-line coverage from a clean commit**

```bash
scripts/patch-coverage.sh --base origin/main
```

Expected: `flpdf changed N, uncovered 0 -> PASS (100%)`. Do not reuse an old
LCOV report and do not use `--allow-dirty` for the authoritative result.

- [ ] **Step 5: Verify final repository state**

```bash
git status --short --branch
git log --oneline origin/main..HEAD
git diff --check origin/main...HEAD
```

Expected: clean feature branch, only scoped commits, and no whitespace errors.

- [ ] **Step 6: Close and persist the Bead**

After all gates pass:

```bash
bd close flpdf-25kg.3.22 --reason "Implemented qpdf-current live direct containment with reversible edge tracking; focused/full/qpdf-byte tests and fresh 100% changed-line coverage pass"
bd show flpdf-25kg.3.22
bd dep cycles
bd dolt push
```

Expected: issue closed, `flpdf-25kg.3.20` no longer blocked by `.3.22`, no
cycles, and `Push complete.`

- [ ] **Step 7: Push the feature branch**

```bash
git push -u origin feature/flpdf-25kg.3.22-live-containment
```

Expected: remote branch update succeeds. Do not merge or delete the worktree;
the user owns the merge decision.
