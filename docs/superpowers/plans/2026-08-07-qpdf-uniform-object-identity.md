# qpdf Uniform Object Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `ObjectHandle`'s split direct/indirect allocations with one shared object slot and add an internal, infallible direct-to-indirect promotion primitive that preserves identity, payload, children, stream storage, parsed offset, resolution state, and qpdf-compatible lifecycle behavior.

**Architecture:** Every handle becomes `Rc<RefCell<ObjectSlot>>`. Indirectness is shared metadata (`Option<ObjectRef>`) on that slot, while `ObjectState` holds unresolved/resolved/missing/destroyed value state. Promotion updates the active Pdf/ObjGen/resolver metadata in place and returns a clone of the same `Rc`. Reverse containment stores only weak immediate-parent slots, so a parent's promotion or re-promotion is visible without rewriting copied root records. The public `Pdf::make_indirect_object_handle` allocator remains unchanged for the downstream consumer-migration issue.

**Tech Stack:** Rust 2021; `Rc`, `Weak`, `RefCell`, and `BTreeSet`; pinned qpdf 11.9.0 source and a compiled C++ oracle probe; Cargo unit/integration/doc tests; Clippy; strict rustdoc; qpdf module-doc validation; `cargo llvm-cov`; `scripts/patch-coverage.sh`.

## Global Constraints

- Work only in `/home/ubuntu/flpdf/.worktrees/flpdf-25kg.3.26-uniform-object` on `feature/flpdf-25kg.3.26-uniform-object`.
- Read `docs/superpowers/specs/2026-08-07-qpdf-uniform-object-identity-design.md` and `bd show flpdf-25kg.3.26` before implementation. If they differ, pinned qpdf 11.9.0 source and observed behavior win; record any resulting design change in the Bead before proceeding.
- Production allowlist: `crates/flpdf/src/object_handle.rs` only. `crates/flpdf/src/reader.rs` may change only inside its `#[cfg(test)]` tests to strengthen `Pdf::drop` coverage. Post-review scope correction: `crates/flpdf/src/reader/resolver.rs` may change only inside `#[cfg(test)] mod tests` for the post-disconnect owner-drop expectation update; no production code there may change. Oracle additions are limited to `tests/oracle/qpdf_objecthandle_uniform_identity_probe.cc` and `scripts/qpdf-objecthandle-uniform-identity-probe.sh`. This plan and the approved spec are the only documentation additions.
- Do not change `Pdf::make_indirect_object_handle`, `Pdf::next_available_object_ref`, `ResolverCore::object_cache`, dirty tracking, canonical enumeration, writer scheduling, or public API behavior. Those are owned by `flpdf-25kg.3.24` and `flpdf-25kg.3.6`.
- Do not introduce double-promotion or cross-document errors. Re-promotion is last-write-wins for active `ObjectRef`, Pdf identity, and resolver.
- Do not add a redirect variant, sentinel ObjGen, compatibility bridge, raw `Object` materialization, panic path, or payload/container/stream clone to promotion.
- Use RED to GREEN. Run the stated failing test before production edits, inspect that it fails for the expected semantic reason, then make the smallest implementation change that turns it green.
- Keep `RefCell` borrows shorter than any resolver callback or containment traversal. Snapshot metadata, drop the borrow, and only then call outward.
- Preserve `ObjectState::Missing` separately from `Resolved(ObjectValue::Null)` and `Destroyed`.
- Keep additive `pdf_unique_ids` provenance separate from active indirect metadata.
- A weak incoming containment edge created while a child was direct remains
  dormant while that child is indirect; root queries ignore it at the
  indirect boundary. If the forward occurrence is removed while the child is
  indirect, detach that dormant edge. If it remains, disconnect can make the
  child direct again and the still-current forward membership becomes visible
  without rewiring.
- One commit per implementation task. Never commit a RED state.
- Base all qpdf citations on the clean tree printed by `scripts/fetch-qpdf-source.sh --print-path`; never edit that tree.

---

## Task 1: Reconfirm the baseline and add the pinned-qpdf lifecycle probe

**Files:**

- Create: `tests/oracle/qpdf_objecthandle_uniform_identity_probe.cc`
- Create: `scripts/qpdf-objecthandle-uniform-identity-probe.sh`

- [ ] **Step 1: Reconfirm the issue, branch, and baseline**

Run:

```bash
bd show flpdf-25kg.3.26
git status --short --branch
git merge-base --is-ancestor origin/main HEAD
cargo test -p flpdf --lib object_handle::identity_tests::
```

Expected: the Bead is `IN_PROGRESS`; the branch is `feature/flpdf-25kg.3.26-uniform-object`; only the approved spec/plan history differs from `origin/main`; the focused baseline passes.

- [ ] **Step 2: Re-read the exact qpdf authority**

Run:

```bash
qpdf_source=$(scripts/fetch-qpdf-source.sh --print-path)
git -C "$qpdf_source" rev-parse HEAD
git -C "$qpdf_source" status --short
sed -n '304,309p;1338,1350p;1629,1633p' "$qpdf_source/include/qpdf/QPDFObjectHandle.hh"
sed -n '224,227p' "$qpdf_source/libqpdf/QPDFObjectHandle.cc"
sed -n '19,29p;60,68p;117,150p;176,180p' "$qpdf_source/libqpdf/qpdf/QPDFObject_private.hh"
sed -n '7,16p' "$qpdf_source/libqpdf/QPDFObject.cc"
sed -n '215,235p;1835,1839p;1882,1897p' "$qpdf_source/libqpdf/QPDF.cc"
```

Expected: commit `3b97c9bd266b7c32ea36d3536e22dab77412886d`, clean source, and the cited shared-object/promotion/destruction paths.

- [ ] **Step 3: Write the focused C++ oracle probe**

Create `tests/oracle/qpdf_objecthandle_uniform_identity_probe.cc` with one executable that:

```cpp
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFObjectHandle.hh>

#include <iostream>
#include <stdexcept>
#include <string>

namespace
{
void require(bool condition, std::string const& message)
{
    if (!condition) {
        throw std::runtime_error(message);
    }
}
}

int main()
{
    try {
        auto direct = QPDFObjectHandle::newDictionary();
        direct.replaceKey("/Value", QPDFObjectHandle::newInteger(1));
        auto original_clone = direct;

        QPDF first;
        first.emptyPDF();
        auto first_indirect = first.makeIndirectObject(direct);
        require(direct.isSameObjectAs(original_clone), "direct clone identity changed");
        require(direct.isSameObjectAs(first_indirect), "promotion cloned QPDFObject");
        require(direct.isIndirect() && original_clone.isIndirect(), "promotion metadata was not shared");
        require(direct.getObjGen() == first_indirect.getObjGen(), "promoted ObjGen differs");

        original_clone.replaceKey("/Value", QPDFObjectHandle::newInteger(2));
        require(first_indirect.getKey("/Value").getIntValue() == 2, "direct-to-indirect mutation was not visible");
        first_indirect.replaceKey("/Value", QPDFObjectHandle::newInteger(3));
        require(direct.getKey("/Value").getIntValue() == 3, "indirect-to-direct mutation was not visible");

        auto repeated = first.makeIndirectObject(direct);
        require(repeated.isSameObjectAs(direct), "repeat promotion changed identity");
        require(direct.getObjGen() == repeated.getObjGen(), "repeat promotion did not install latest ObjGen");

        {
            QPDF second;
            second.emptyPDF();
            auto cross_document = second.makeIndirectObject(direct);
            require(cross_document.isSameObjectAs(direct), "cross-document promotion changed identity");
            require(direct.getObjGen() == cross_document.getObjGen(), "cross-document metadata was not latest");
        }

        require(!direct.isIndirect(), "latest owner drop retained ObjGen");
        require(
            std::string(direct.getTypeName()) == "destroyed",
            "latest owner drop retained a live value");
        std::cout << "qpdf uniform object identity probe: ok\n";
        return 0;
    } catch (std::exception const& e) {
        std::cerr << e.what() << '\n';
        return 1;
    }
}
```

Do not add output-only assertions: each required property must be checked in the executable and produce a non-zero exit on mismatch.

- [ ] **Step 4: Write the pinned build/run wrapper**

Create `scripts/qpdf-objecthandle-uniform-identity-probe.sh` by following the safety and pinning shape of `scripts/qpdf-objecthandle-dereference-diff.sh`:

- require zero arguments;
- resolve the repo root and pinned source via `scripts/fetch-qpdf-source.sh --print-path`;
- require the exact pinned commit and a clean tracked source tree both before and after the probe;
- create a private `mktemp -d -t flpdf-qpdf-objecthandle-uniform-XXXXXXXX` build directory and remove only that validated prefix in an `EXIT` trap;
- build pinned `libqpdf` with CMake;
- compile the new probe with `-std=c++17`, `-DPOINTERHOLDER_TRANSITION=4`, pinned include paths, and the pinned built library;
- use `ldd` plus `realpath` to prove the executable resolves `libqpdf.so` from that build directory;
- run the executable with `LD_LIBRARY_PATH` set to the pinned build directory.

- [ ] **Step 5: Run the probe**

Run:

```bash
chmod +x scripts/qpdf-objecthandle-uniform-identity-probe.sh
scripts/qpdf-objecthandle-uniform-identity-probe.sh
```

Expected: `qpdf uniform object identity probe: ok` and exit 0.

- [ ] **Step 6: Commit the oracle**

```bash
git add scripts/qpdf-objecthandle-uniform-identity-probe.sh tests/oracle/qpdf_objecthandle_uniform_identity_probe.cc
git commit -m "test(qpdf): probe uniform object identity lifecycle"
```

---

## Task 2: Replace split storage and add same-allocation promotion

This is one atomic implementation task. Separating the type replacement from accessor migration would leave the crate uncompilable, while implementing promotion before the replacement would encode the forbidden redirect/copy design.

**Files:**

- Modify: `crates/flpdf/src/object_handle.rs`

- [ ] **Step 1: Add the RED contract module**

Add `#[cfg(test)] mod uniform_identity_tests` beside `identity_tests`. Reuse a minimal resolver that never resolves unless a test explicitly asks it to:

```rust
struct NoopResolver;

impl DocumentResolver for NoopResolver {
    fn resolve_indirect(
        &self,
        _object_ref: ObjectRef,
        _handle: &ObjectHandle,
    ) -> crate::Result<()> {
        Ok(())
    }
}

fn resolver() -> Rc<dyn DocumentResolver> {
    Rc::new(NoopResolver)
}

struct ReenteringResolver {
    calls: Rc<RefCell<Vec<ObjectRef>>>,
}

impl DocumentResolver for ReenteringResolver {
    fn resolve_indirect(
        &self,
        object_ref: ObjectRef,
        handle: &ObjectHandle,
    ) -> crate::Result<()> {
        self.calls.borrow_mut().push(object_ref);
        assert_eq!(handle.object_ref(), Some(object_ref));
        handle.set_resolved(ObjectValue::Dictionary(Default::default()));
        handle.replace_key(b"Resolved", ObjectHandle::boolean(true));
        Ok(())
    }
}
```

Add these tests before production code:

```rust
#[test]
fn promotion_preserves_one_shared_object_identity_and_offset() {
    let resolver = resolver();
    let original = ObjectHandle::dictionary(vec![(
        b"Value".to_vec(),
        ObjectHandle::integer(1),
    )]);
    let outstanding_clone = original.clone();
    original.set_parsed_offset_if_unset(37);

    let promoted = original.promote_to_indirect(
        ObjectRef::new(17, 2),
        41,
        Rc::downgrade(&resolver),
    );

    assert!(original.is_same_object_as(&outstanding_clone));
    assert!(original.is_same_object_as(&promoted));
    assert!(original.is_indirect());
    assert!(outstanding_clone.is_indirect());
    assert_eq!(promoted.object_ref(), Some(ObjectRef::new(17, 2)));
    assert_eq!(outstanding_clone.get_parsed_offset(), 37);

    original.replace_key(b"Value", ObjectHandle::integer(2));
    assert_eq!(promoted.get_key(b"Value").as_integer(), Some(2));
    promoted.replace_key(b"Value", ObjectHandle::integer(3));
    assert_eq!(outstanding_clone.get_key(b"Value").as_integer(), Some(3));
}

#[test]
fn promotion_does_not_clone_container_or_stream_storage() {
    let resolver = resolver();
    let array_child = ObjectHandle::dictionary(vec![]);
    let stream_dict = ObjectHandle::dictionary(vec![]);
    let stream_data = Rc::new(b"shared stream data".to_vec());
    let stream = ObjectHandle::from_value(ObjectValue::Stream {
        stream_dict: stream_dict.clone(),
        stream_data: Some(stream_data.clone()),
        stream_length: stream_data.len(),
    });
    let root = ObjectHandle::array(vec![array_child.clone(), stream.clone()]);

    let promoted = root.promote_to_indirect(
        ObjectRef::new(19, 0),
        51,
        Rc::downgrade(&resolver),
    );

    let children = promoted.as_array().expect("promoted array");
    assert!(children[0].is_same_object_as(&array_child));
    assert!(children[1].is_same_object_as(&stream));
    let promoted_dict = children[1].as_stream_dict().expect("stream dictionary");
    assert!(promoted_dict.is_same_object_as(&stream_dict));
    children[1].with_value(|value| {
        let Some(ObjectValue::Stream { stream_data: Some(actual), .. }) = value else {
            panic!("promoted child must retain stream data");
        };
        assert!(Rc::ptr_eq(actual, &stream_data));
    });
}

#[test]
fn resolution_state_is_shared_by_every_alias() {
    let unresolved = ObjectHandle::new_indirect_unresolved(ObjectRef::new(23, 0), -1);
    let alias = unresolved.clone();
    unresolved.set_resolved(ObjectValue::Integer(7));
    assert!(alias.is_same_object_as(&unresolved));
    assert!(alias.is_resolved());
    assert_eq!(alias.as_integer(), Some(7));
}

#[test]
fn re_promotion_uses_latest_resolver() {
    let first_calls = Rc::new(RefCell::new(Vec::new()));
    let first: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
        calls: first_calls.clone(),
    });
    let handle = ObjectHandle::new_indirect_for_pdf_with_resolver(
        ObjectRef::new(59, 0),
        NO_PARSED_OFFSET,
        101,
        Rc::downgrade(&first),
    );
    let latest_calls = Rc::new(RefCell::new(Vec::new()));
    let latest: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
        calls: latest_calls.clone(),
    });
    let alias = handle.promote_to_indirect(
        ObjectRef::new(61, 7),
        102,
        Rc::downgrade(&latest),
    );
    drop(first);

    alias.try_dereference().expect("latest resolver resolves");

    assert!(handle.is_same_object_as(&alias));
    assert_eq!(*first_calls.borrow(), Vec::<ObjectRef>::new());
    assert_eq!(*latest_calls.borrow(), vec![ObjectRef::new(61, 7)]);
    assert_eq!(handle.object_ref(), Some(ObjectRef::new(61, 7)));
    assert_eq!(handle.get_key(b"Resolved").as_boolean(), Some(true));
}

#[test]
fn resolver_reentry_uses_latest_metadata_without_borrow_panic() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let latest: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
        calls: calls.clone(),
    });
    let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(63, 0), -1);
    handle.promote_to_indirect(
        ObjectRef::new(67, 3),
        103,
        Rc::downgrade(&latest),
    );

    handle.try_dereference().expect("reentrant resolver");

    assert_eq!(*calls.borrow(), vec![ObjectRef::new(67, 3)]);
    assert_eq!(handle.get_key(b"Resolved").as_boolean(), Some(true));
}

#[test]
fn dropped_latest_resolver_reports_latest_object_and_stays_unresolved() {
    let first = resolver();
    let latest_calls = Rc::new(RefCell::new(Vec::new()));
    let latest: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
        calls: latest_calls.clone(),
    });
    let handle = ObjectHandle::new_indirect_for_pdf_with_resolver(
        ObjectRef::new(69, 0),
        NO_PARSED_OFFSET,
        104,
        Rc::downgrade(&first),
    );
    handle.promote_to_indirect(
        ObjectRef::new(71, 5),
        105,
        Rc::downgrade(&latest),
    );
    drop(latest);

    let error = handle.try_dereference().expect_err("latest owner was dropped");

    assert_eq!(error.to_string(), "object 71 5 belongs to a dropped PDF");
    assert!(latest_calls.borrow().is_empty());
    assert!(!handle.is_resolved());
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf --lib object_handle::uniform_identity_tests::
```

Expected: compile failure because `promote_to_indirect` does not exist. A failure caused by an incorrect test API must be fixed before production code begins.

- [ ] **Step 3: Introduce the uniform slot types**

Replace `ObjectHandle(Repr)`, `Repr`, `DirectSlot`, `IndirectSlot`, and `IndirectState` with:

```rust
#[derive(Clone)]
pub struct ObjectHandle(Rc<RefCell<ObjectSlot>>);

struct ObjectSlot {
    state: ObjectState,
    object_ref: Option<ObjectRef>,
    active_pdf_unique_id: Option<u64>,
    resolver: Option<Weak<dyn DocumentResolver>>,
    parsed_offset: i64,
    pdf_unique_ids: BTreeSet<u64>,
    containment_parents: Vec<ContainmentParent>,
}

#[derive(Debug)]
pub(crate) enum ObjectState {
    NotYetResolved,
    Resolved(ObjectValue),
    Missing,
    Destroyed,
}
```

For this atomic storage migration, retain `ContainmentOwner` and adapt the
existing `ContainmentParent` to point at the uniform slot:

```rust
#[derive(Clone)]
enum ContainmentParent {
    Root(ContainmentOwner),
    Slot(Weak<RefCell<ObjectSlot>>),
}
```

This is the existing containment representation carried through one
compilable commit, not a new compatibility route. Task 3 removes the enum and
its copied root variant under its own RED tests. In this task, convert old
`Direct` parent matches to `Slot`, reconstruct upgraded parents with
`ObjectHandle(parent)`, and preserve current root-snapshot behavior. Do not
derive `Debug` on `ObjectSlot`; update the hand-written `ObjectHandle::fmt` to
snapshot `object_ref`, state name, and parsed offset without recursively
formatting resolved indirect values.

- [ ] **Step 4: Convert constructors without changing their contracts**

Use these exact initial states:

```rust
fn new_indirect_unresolved_with_identity(
    object_ref: ObjectRef,
    offset: i64,
    pdf_unique_id: Option<u64>,
    resolver: Option<Weak<dyn DocumentResolver>>,
) -> Self {
    let _ = offset;
    Self(Rc::new(RefCell::new(ObjectSlot {
        state: ObjectState::NotYetResolved,
        object_ref: Some(object_ref),
        active_pdf_unique_id: pdf_unique_id,
        resolver,
        parsed_offset: NO_PARSED_OFFSET,
        pdf_unique_ids: BTreeSet::new(),
        containment_parents: Vec::new(),
    })))
}

fn new_direct(value: ObjectValue, parsed_offset: i64) -> Self {
    let handle = Self(Rc::new(RefCell::new(ObjectSlot {
        state: ObjectState::Resolved(value),
        object_ref: None,
        active_pdf_unique_id: None,
        resolver: None,
        parsed_offset,
        pdf_unique_ids: BTreeSet::new(),
        containment_parents: Vec::new(),
    })));
    handle.with_value(|value| {
        if let Some(value) = value {
            handle.attach_value_children(value);
        }
    });
    handle
}
```

- [ ] **Step 5: Convert identity, classification, and state accessors**

Implement the shared-metadata rules directly:

```rust
pub fn is_direct(&self) -> bool {
    self.0.borrow().object_ref.is_none()
}

pub fn is_indirect(&self) -> bool {
    self.0.borrow().object_ref.is_some()
}

pub fn object_ref(&self) -> Option<ObjectRef> {
    self.0.borrow().object_ref
}

pub fn is_same_object_as(&self, other: &Self) -> bool {
    Rc::ptr_eq(&self.0, &other.0)
}
```

Convert `strong_count`, `get_parsed_offset`, `set_parsed_offset_if_unset`, `reset_parsed_offset`, `is_resolved`, `is_null`, `with_value`, `with_value_mut`, `unparse`, stream-source access, and every remaining `Repr`/`IndirectState` match to `ObjectSlot`/`ObjectState`.

Use this value-access mapping everywhere:

- `Resolved(value)` -> `Some(value)`;
- `NotYetResolved` -> `None`;
- `Missing` and `Destroyed` -> null fallback only in existing infallible value accessors;
- `is_null()` -> true for `Resolved(Null)` and `Missing`, false for `NotYetResolved` and `Destroyed`.

Preserve existing direct-only behavior by checking `slot.object_ref.is_none()` before `into_direct_value`, `direct_value_clone`, and `replace_direct_value`. Preserve existing indirect-only behavior by checking `slot.object_ref.is_some()` before `set_resolved` and `set_missing`.

- [ ] **Step 6: Add the promotion primitive**

Add beside the constructors:

```rust
pub(crate) fn promote_to_indirect(
    &self,
    object_ref: ObjectRef,
    pdf_unique_id: u64,
    resolver: Weak<dyn DocumentResolver>,
) -> Self {
    let children = {
        let mut slot = self.0.borrow_mut();
        slot.object_ref = Some(object_ref);
        slot.active_pdf_unique_id = Some(pdf_unique_id);
        slot.resolver = Some(resolver);
        slot.pdf_unique_ids.insert(pdf_unique_id);
        match &slot.state {
            ObjectState::Resolved(value) => Self::direct_children(value),
            ObjectState::NotYetResolved | ObjectState::Missing | ObjectState::Destroyed => {
                Vec::new()
            }
        }
    };
    let mut visited = BTreeSet::new();
    visited.insert(Rc::as_ptr(&self.0) as usize);
    for child in children {
        child.associate_pdf_identity(pdf_unique_id, &mut visited);
    }
    self.clone()
}
```

The root's provenance is recorded in the same borrow as active metadata. The
metadata borrow must end before descendant propagation, because that traversal
may revisit the promoted slot through a direct cycle. Do not call the existing
`associate_pdf_identity` on `self` after setting `object_ref`: that helper
correctly stops at indirect boundaries and would therefore skip the promoted
root and all of its descendants, especially on re-promotion.

- [ ] **Step 7: Convert direct-self-cycle checks**

Replace variant matching in `is_same_direct_handle` with:

```rust
fn is_same_direct_handle(&self, other: &Self) -> bool {
    self.is_direct() && other.is_direct() && self.is_same_object_as(other)
}
```

This preserves rejection of a direct self-cycle while allowing a promoted/indirect object to contain a reference to itself.

- [ ] **Step 8: Remove all split-storage remnants**

Run:

```bash
rg -n 'Repr|DirectSlot|IndirectSlot|IndirectState' crates/flpdf/src/object_handle.rs
```

Expected: no `Repr`, `DirectSlot`, `IndirectSlot`, or `IndirectState` matches.
`ContainmentOwner` and the transitional `ContainmentParent` remain until Task
3.

- [ ] **Step 9: Run GREEN and the existing focused contracts**

Run:

```bash
cargo test -p flpdf --lib object_handle::uniform_identity_tests::
cargo test -p flpdf --lib object_handle::identity_tests::
cargo test -p flpdf --lib object_handle::mutation_tests::
cargo test -p flpdf --test object_handle_parity_tests
```

Expected: all pass. Do not weaken existing tests to accommodate the refactor.

- [ ] **Step 10: Commit the uniform slot**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): unify direct and indirect object identity"
```

---

## Task 3: Make containment roots observe promotion and active metadata

**Files:**

- Modify: `crates/flpdf/src/object_handle.rs`

- [ ] **Step 1: Add RED tests for promotion-aware containment**

Add to `uniform_identity_tests`:

```rust
#[test]
fn contained_children_observe_parent_promotion_without_edge_rewrite() {
    let resolver = resolver();
    let child = ObjectHandle::dictionary(vec![]);
    let parent = ObjectHandle::dictionary(vec![(b"Child".to_vec(), child.clone())]);
    assert!(child.containing_object_refs_for_pdf(61).is_empty());

    parent.promote_to_indirect(
        ObjectRef::new(29, 0),
        61,
        Rc::downgrade(&resolver),
    );

    assert_eq!(
        child.containing_object_refs_for_pdf(61),
        vec![ObjectRef::new(29, 0)]
    );
}

#[test]
fn re_promotion_updates_active_root_but_preserves_additive_provenance() {
    let first = resolver();
    let second = resolver();
    let child = ObjectHandle::dictionary(vec![]);
    let parent = ObjectHandle::dictionary(vec![(b"Child".to_vec(), child.clone())]);

    let first_alias = parent.promote_to_indirect(
        ObjectRef::new(31, 0),
        71,
        Rc::downgrade(&first),
    );
    let second_alias = parent.promote_to_indirect(
        ObjectRef::new(37, 4),
        72,
        Rc::downgrade(&second),
    );

    assert!(first_alias.is_same_object_as(&second_alias));
    assert_eq!(parent.object_ref(), Some(ObjectRef::new(37, 4)));
    assert!(!parent.belongs_to_pdf(71));
    assert!(parent.belongs_to_pdf(72));
    assert!(child.belongs_to_pdf(71));
    assert!(child.belongs_to_pdf(72));
    assert!(child.containing_object_refs_for_pdf(71).is_empty());
    assert_eq!(
        child.containing_object_refs_for_pdf(72),
        vec![ObjectRef::new(37, 4)]
    );
}

#[test]
fn promoted_child_is_an_indirect_boundary_not_a_direct_owner_path() {
    let resolver = resolver();
    let child = ObjectHandle::dictionary(vec![]);
    let outer = ObjectHandle::dictionary(vec![(b"Child".to_vec(), child.clone())]);
    outer.promote_to_indirect(
        ObjectRef::new(41, 0),
        81,
        Rc::downgrade(&resolver),
    );
    child.promote_to_indirect(
        ObjectRef::new(43, 0),
        81,
        Rc::downgrade(&resolver),
    );

    assert!(child.containing_object_refs_for_pdf(81).is_empty());
    let grandchild = ObjectHandle::integer(1);
    child.replace_key(b"Grandchild", grandchild.clone());
    assert_eq!(
        grandchild.containing_object_refs_for_pdf(81),
        vec![ObjectRef::new(43, 0)]
    );
}

#[test]
fn dormant_parent_edge_tracks_removal_while_child_is_indirect() {
    let resolver = resolver();
    let child = ObjectHandle::dictionary(vec![]);
    let outer = ObjectHandle::dictionary(vec![(b"Child".to_vec(), child.clone())]);
    outer.promote_to_indirect(
        ObjectRef::new(73, 0),
        111,
        Rc::downgrade(&resolver),
    );
    child.promote_to_indirect(
        ObjectRef::new(79, 0),
        112,
        Rc::downgrade(&resolver),
    );

    assert!(child.containing_object_refs_for_pdf(111).is_empty());
    child.disconnect();
    assert_eq!(
        child.containing_object_refs_for_pdf(111),
        vec![ObjectRef::new(73, 0)]
    );

    child.promote_to_indirect(
        ObjectRef::new(83, 0),
        112,
        Rc::downgrade(&resolver),
    );
    outer.remove_key(b"Child");
    child.disconnect();
    assert!(child.containing_object_refs_for_pdf(111).is_empty());
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf --lib object_handle::uniform_identity_tests::
```

Expected: at least the parent-promotion root assertion fails if containment still snapshots a root or ignores uniform parent metadata.

- [ ] **Step 3: Store one weak immediate-parent slot per occurrence**

First change `ObjectSlot::containment_parents` to
`Vec<Weak<RefCell<ObjectSlot>>>` and delete `ContainmentParent`, including its
copied `Root` variant. Retain `ContainmentOwner` only as the value returned by
root queries. Then implement these rules:

- `containment_parent(&self)` becomes `Rc::downgrade(&self.0)`.
- `same_containment_parent` becomes `Weak::ptr_eq`.
- `containment_parent_is_live` becomes `Weak::strong_count(parent) != 0`.
- `attach_child_to_parent` first returns when `child.is_indirect()`. Otherwise it prunes expired edges, pushes one cloned weak parent, snapshots the parent's additive Pdf identities plus active Pdf identity, releases all borrows, and propagates each identity through the direct subtree. It does not remove an incoming edge created before promotion; that edge is dormant while the child is indirect.
- `detach_child_from_parent` removes exactly one pointer-equal edge occurrence regardless of the child's current direct/indirect state. This lets a parent remove a pre-promotion occurrence while the child is indirect, preventing that stale path from reappearing after disconnect. It must release the child borrow before any further traversal.
- `attach_value_children` and `detach_value_children` pass the weak slot returned by `containment_parent`.

Do not deduplicate stored occurrences: the same child in two dictionary/array positions needs two edges so removing one position does not detach the other.

- [ ] **Step 4: Walk live parent metadata dynamically**

Rewrite `containment_roots` as an iterative walk:

```rust
fn containment_roots(&self) -> BTreeSet<ContainmentOwner> {
    if self.is_indirect() {
        return BTreeSet::new();
    }
    let mut roots = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut pending = vec![self.clone()];
    while let Some(handle) = pending.pop() {
        let identity = Rc::as_ptr(&handle.0) as usize;
        if !visited.insert(identity) {
            continue;
        }
        let (object_ref, pdf_unique_id, parents) = {
            let mut slot = handle.0.borrow_mut();
            slot.containment_parents
                .retain(|parent| parent.strong_count() != 0);
            (
                slot.object_ref,
                slot.active_pdf_unique_id,
                slot.containment_parents.clone(),
            )
        };
        if let Some(object_ref) = object_ref {
            roots.insert(ContainmentOwner {
                pdf_unique_id,
                object_ref,
            });
            continue;
        }
        pending.extend(
            parents
                .into_iter()
                .filter_map(|parent| parent.upgrade())
                .map(ObjectHandle),
        );
    }
    roots
}
```

If `Rc::as_ptr` cannot be cast directly because of the unsized trait metadata elsewhere, use the concrete `ObjectSlot` pointer only; do not replace the visited set with recursion.

- [ ] **Step 5: Keep active identity and provenance separate**

Implement `belongs_to_pdf` as:

```rust
pub(crate) fn belongs_to_pdf(&self, pdf_unique_id: u64) -> bool {
    let slot = self.0.borrow();
    if slot.object_ref.is_some() {
        slot.active_pdf_unique_id == Some(pdf_unique_id)
    } else {
        slot.pdf_unique_ids.is_empty() || slot.pdf_unique_ids.contains(&pdf_unique_id)
    }
}
```

`associate_pdf_identity` must:

- visit only slots currently direct;
- identify slots by outer `Rc` pointer;
- insert into `pdf_unique_ids`;
- snapshot direct children while borrowed;
- release the borrow before pushing children;
- stop at indirect children and terminate on direct cycles.

- [ ] **Step 6: Run containment GREEN and regressions**

Run:

```bash
cargo test -p flpdf --lib object_handle::uniform_identity_tests::
cargo test -p flpdf --lib object_handle::mutation_tests::dictionary_detach
cargo test -p flpdf --lib object_handle::mutation_tests::shared_subtree
cargo test -p flpdf --lib object_handle::mutation_tests::current_root_lookup
cargo test -p flpdf --lib object_handle::mutation_tests::deep_containment_traversals
cargo test -p flpdf --lib object_handle::identity_tests::pdf_identity_propagation
```

Expected: all pass, including the subprocess-only 100,000-level traversal wrapper.

- [ ] **Step 7: Commit containment**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "refactor(object_handle): derive containment roots from shared slots"
```

---

## Task 4: Re-verify last-write metadata and resolver re-entry safety

**Files:** No planned edits. The RED tests and implementation landed in Task
2 so they could obey TDD; this task is a focused post-containment verification
checkpoint.

- [ ] **Step 1: Inspect the resolver contract introduced in Task 2**

Confirm `uniform_identity_tests` contains:

```rust
struct ReenteringResolver {
    calls: Rc<RefCell<Vec<ObjectRef>>>,
}

impl DocumentResolver for ReenteringResolver {
    fn resolve_indirect(
        &self,
        object_ref: ObjectRef,
        handle: &ObjectHandle,
    ) -> crate::Result<()> {
        self.calls.borrow_mut().push(object_ref);
        assert_eq!(handle.object_ref(), Some(object_ref));
        handle.set_resolved(ObjectValue::Dictionary(Default::default()));
        handle.replace_key(b"Resolved", ObjectHandle::boolean(true));
        Ok(())
    }
}
```

Confirm the exact last-write/re-entry tests from Task 2 remain unchanged:

```rust
#[test]
fn re_promotion_uses_latest_resolver() {
    let first_calls = Rc::new(RefCell::new(Vec::new()));
    let first: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
        calls: first_calls.clone(),
    });
    let handle = ObjectHandle::new_indirect_for_pdf_with_resolver(
        ObjectRef::new(59, 0),
        NO_PARSED_OFFSET,
        101,
        Rc::downgrade(&first),
    );

    let latest_calls = Rc::new(RefCell::new(Vec::new()));
    let latest: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
        calls: latest_calls.clone(),
    });
    let alias = handle.promote_to_indirect(
        ObjectRef::new(61, 7),
        102,
        Rc::downgrade(&latest),
    );
    drop(first);

    alias.try_dereference().expect("latest resolver resolves");

    assert!(handle.is_same_object_as(&alias));
    assert_eq!(*first_calls.borrow(), Vec::<ObjectRef>::new());
    assert_eq!(*latest_calls.borrow(), vec![ObjectRef::new(61, 7)]);
    assert_eq!(handle.object_ref(), Some(ObjectRef::new(61, 7)));
    assert_eq!(handle.get_key(b"Resolved").as_boolean(), Some(true));
}

#[test]
fn resolver_reentry_uses_latest_metadata_without_borrow_panic() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let latest: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
        calls: calls.clone(),
    });
    let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(63, 0), -1);
    handle.promote_to_indirect(
        ObjectRef::new(67, 3),
        103,
        Rc::downgrade(&latest),
    );

    handle.try_dereference().expect("reentrant resolver");

    assert_eq!(*calls.borrow(), vec![ObjectRef::new(67, 3)]);
    assert_eq!(handle.get_key(b"Resolved").as_boolean(), Some(true));
}

#[test]
fn dropped_latest_resolver_reports_latest_object_and_stays_unresolved() {
    let first = resolver();
    let latest_calls = Rc::new(RefCell::new(Vec::new()));
    let latest: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
        calls: latest_calls.clone(),
    });
    let handle = ObjectHandle::new_indirect_for_pdf_with_resolver(
        ObjectRef::new(69, 0),
        NO_PARSED_OFFSET,
        104,
        Rc::downgrade(&first),
    );
    handle.promote_to_indirect(
        ObjectRef::new(71, 5),
        105,
        Rc::downgrade(&latest),
    );
    drop(latest);

    let error = handle.try_dereference().expect_err("latest owner was dropped");

    assert_eq!(error.to_string(), "object 71 5 belongs to a dropped PDF");
    assert!(latest_calls.borrow().is_empty());
    assert!(!handle.is_resolved());
}
```

- [ ] **Step 2: Run the focused regression tests**

Run:

```bash
cargo test -p flpdf --lib object_handle::uniform_identity_tests::re_promotion_uses_latest_resolver
cargo test -p flpdf --lib object_handle::uniform_identity_tests::resolver_reentry
```

Expected: pass. If any test fails because Task 3 regressed resolver metadata or
borrow lifetime, stop and add one minimal RED reproducer for that regression
before changing production code.

- [ ] **Step 3: Verify snapshot-before-callback implementation**

Verify `try_dereference` has this one-short-borrow shape:

```rust
pub(crate) fn try_dereference(&self) -> Result<()> {
    let (object_ref, resolver) = {
        let slot = self.0.borrow();
        if !matches!(slot.state, ObjectState::NotYetResolved) {
            return Ok(());
        }
        let Some(object_ref) = slot.object_ref else {
            return Ok(());
        };
        (object_ref, slot.resolver.clone())
    };

    let Some(resolver) = resolver.and_then(|resolver| resolver.upgrade()) else {
        return Err(Error::Internal(format!(
            "object {} {} belongs to a dropped PDF",
            object_ref.number, object_ref.generation
        )));
    };
    resolver.resolve_indirect(object_ref, self)
}
```

Apply the same snapshot-before-call discipline to stream resolver access. Do not resolve inside `promote_to_indirect`.

- [ ] **Step 4: Run GREEN and resolver regressions**

Run:

```bash
cargo test -p flpdf --lib object_handle::uniform_identity_tests::
cargo test -p flpdf --lib object_handle::identity_tests::
cargo test -p flpdf --lib object_handle::mutation_tests::replace_key_allows_an_indirect_handle_to_reference_itself
cargo test -p flpdf --lib reader::tests::dropping_pdf_breaks_the_pages_parent_reference_cycle
```

Expected: all pass; no borrow panic; resolver call log contains exactly the latest ObjGen once.

- [ ] **Step 5: Commit only if this checkpoint found a regression**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "fix(object_handle): use latest promotion metadata during resolution"
```

If all focused tests pass and no code changes are required, record Task 4 as
complete with the verification evidence and no commit.

---

## Task 5: Match qpdf disconnect behavior for surviving aliases

**Files:**

- Modify: `crates/flpdf/src/object_handle.rs`
- Modify: `crates/flpdf/src/reader.rs` test module only

- [ ] **Step 1: Add RED disconnect tests**

Add to `uniform_identity_tests`:

```rust
#[test]
fn disconnect_clears_indirect_metadata_for_every_non_null_alias() {
    let resolver = resolver();
    let original = ObjectHandle::integer(9);
    original.set_parsed_offset_if_unset(44);
    let promoted = original.promote_to_indirect(
        ObjectRef::new(47, 0),
        91,
        Rc::downgrade(&resolver),
    );

    promoted.disconnect();

    assert!(original.is_same_object_as(&promoted));
    assert!(original.is_direct());
    assert_eq!(original.object_ref(), None);
    assert!(!original.is_null());
    assert_eq!(original.get_parsed_offset(), NO_PARSED_OFFSET);
}

#[test]
fn disconnect_preserves_literal_null_and_missing_as_null() {
    let resolver = resolver();
    let literal_null = ObjectHandle::null();
    literal_null.set_parsed_offset_if_unset(55);
    literal_null.promote_to_indirect(
        ObjectRef::new(49, 0),
        92,
        Rc::downgrade(&resolver),
    );
    literal_null.disconnect();
    assert!(literal_null.is_direct());
    assert!(literal_null.is_null());
    assert_eq!(literal_null.get_parsed_offset(), 55);

    let missing = ObjectHandle::new_indirect_unresolved(ObjectRef::new(51, 0), -1);
    missing.set_missing();
    missing.promote_to_indirect(
        ObjectRef::new(53, 0),
        93,
        Rc::downgrade(&resolver),
    );
    missing.disconnect();
    assert!(missing.is_direct());
    assert!(missing.is_null());
    assert_eq!(missing.get_parsed_offset(), NO_PARSED_OFFSET);
}
```

Strengthen `reader::tests::dropping_pdf_breaks_the_pages_parent_reference_cycle` after `drop(pdf)`:

```rust
assert!(pages.is_direct());
assert!(page.is_direct());
assert!(!pages.is_null());
assert!(!page.is_null());
```

Add adjacent Pdf-drop regressions for a surviving resolved literal-null handle and a surviving missing handle. Build them through existing test helpers and the canonical registry, keep a clone outside the Pdf lifetime, drop the Pdf, then assert both are direct and null.

Use these exact tests in `reader.rs`'s existing test module:

```rust
#[test]
fn dropping_pdf_preserves_a_surviving_literal_null_handle() {
    let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
    let object_ref = ObjectRef::new(90, 0);
    pdf.set_object(object_ref, Object::Null);
    let handle = pdf.get_object_handle(object_ref);
    assert!(handle.is_indirect());
    assert!(handle.is_null());

    drop(pdf);

    assert!(handle.is_direct());
    assert!(handle.is_null());
}

#[test]
fn dropping_pdf_preserves_a_surviving_missing_handle() {
    let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
    let handle = pdf.get_object_handle(ObjectRef::new(91, 0));
    handle.set_missing();
    assert!(handle.is_indirect());
    assert!(handle.is_null());

    drop(pdf);

    assert!(handle.is_direct());
    assert!(handle.is_null());
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf --lib object_handle::uniform_identity_tests::disconnect
cargo test -p flpdf --lib reader::tests::dropping_pdf_
```

Expected: current disconnect leaves `object_ref` present and destroys null/missing unconditionally, so the new assertions fail.

- [ ] **Step 3: Implement state-sensitive disconnect**

Use one slot mutation to snapshot/detach the old resolved value and clear all active metadata:

```rust
pub(crate) fn disconnect(&self) {
    let old_value = {
        let mut slot = self.0.borrow_mut();
        if slot.object_ref.is_none() {
            return;
        }
        slot.object_ref = None;
        slot.active_pdf_unique_id = None;
        slot.resolver = None;
        let old_state = std::mem::replace(&mut slot.state, ObjectState::Destroyed);
        match old_state {
            ObjectState::Resolved(ObjectValue::Null) => {
                slot.state = ObjectState::Resolved(ObjectValue::Null);
                None
            }
            ObjectState::Missing => {
                slot.state = ObjectState::Missing;
                None
            }
            ObjectState::Resolved(value) => {
                slot.parsed_offset = NO_PARSED_OFFSET;
                Some(value)
            }
            ObjectState::NotYetResolved => {
                slot.parsed_offset = NO_PARSED_OFFSET;
                None
            }
            ObjectState::Destroyed => {
                slot.parsed_offset = NO_PARSED_OFFSET;
                None
            }
        }
    };
    if let Some(old_value) = old_value {
        self.detach_value_children(&old_value);
    }
}
```

It must detach children after releasing the slot borrow. Preserve
`pdf_unique_ids` and `containment_parents`. The null and missing arms leave the
parsed offset untouched; missing already carries `NO_PARSED_OFFSET` by its
own contract.

- [ ] **Step 4: Verify cycles and detachment**

Run:

```bash
cargo test -p flpdf --lib object_handle::uniform_identity_tests::disconnect
cargo test -p flpdf --lib object_handle::parsed_offset_tests::disconnect
cargo test -p flpdf --lib object_handle::mutation_tests::stream_dictionary_membership_tracks_replacement_and_root_disconnect
cargo test -p flpdf --lib object_handle::mutation_tests::indirect_state_replacement_detaches_old_direct_children
cargo test -p flpdf --lib reader::tests::dropping_pdf_
```

Expected: all pass; non-null cycles release; null/missing survive as null; every alias becomes direct.

- [ ] **Step 5: Commit lifecycle behavior**

```bash
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/reader.rs
git commit -m "fix(object_handle): disconnect shared values like qpdf"
```

---

## Task 6: Complete source correspondence and eliminate obsolete assumptions

**Files:**

- Modify: `crates/flpdf/src/object_handle.rs`

- [ ] **Step 1: Update module and item documentation**

Document the final mapping with these source facts:

- `QPDFObjectHandle` holds `shared_ptr<QPDFObject>` and pointer equality defines sameness: `include/qpdf/QPDFObjectHandle.hh:304-309,1338-1350`; `libqpdf/QPDFObjectHandle.cc:224-227`.
- `QPDFObject`/`QPDFValue` own shared value plus QPDF/ObjGen/offset metadata: `libqpdf/qpdf/QPDFObject_private.hh:19-29,60-68,117-150,176-180`; `libqpdf/qpdf/QPDFValue.hh:60-72,90-110,144-152`.
- resolution reads current metadata from the same allocation: `libqpdf/qpdf/QPDFObject.cc:7-16`.
- promotion registers and mutates the same allocation: `libqpdf/QPDF.cc:1835-1839,1882-1897`.
- destructor clears indirect metadata and destroys non-null values: `libqpdf/QPDF.cc:215-235`.

Delete comments that describe split `Repr` storage or intentional clone divergence. Keep `direct_value_clone` documented solely as a legacy helper for the unchanged public allocator; explicitly say it is not the qpdf-native promotion primitive and is scheduled for consumer migration in `flpdf-25kg.3.6`.

- [ ] **Step 2: Search for stale implementation language**

Run:

```bash
rg -n 'Repr|DirectSlot|IndirectSlot|IndirectState|Direct and Indirect slots|distinct storage|own independent dict|promotion.*clone|clone.*promotion' crates/flpdf/src/object_handle.rs crates/flpdf/src/reader.rs
```

Expected: no production comment claims the new internal primitive clones or uses split storage. Existing `reader.rs` tests/comments may still describe the unchanged public `Pdf::make_indirect_object_handle`; retain those until `flpdf-25kg.3.6`.

- [ ] **Step 3: Run the documentation gates**

```bash
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo test --workspace --doc
```

Expected: all pass with no broken private links or malformed qpdf classification line.

- [ ] **Step 4: Commit documentation cleanup**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "docs(object_handle): map uniform slots to qpdf 11.9.0"
```

---

## Task 7: Run full regressions and byte-identity gates

**Files:** No planned edits. Fix only failures caused by this branch, following RED-to-GREEN in the owning task file.

- [ ] **Step 1: Format and lint**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: exit 0.

- [ ] **Step 2: Run focused crate tests**

```bash
cargo test -p flpdf --lib object_handle::
cargo test -p flpdf --test object_handle_parity_tests
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test writer_tests
```

Expected: all pass.

- [ ] **Step 3: Run full crate and workspace tests**

```bash
cargo test -p flpdf --all-features
cargo test --workspace --all-features
```

Expected: all pass with only the repository's documented ignored tests.

- [ ] **Step 4: Re-run the qpdf oracle probe**

```bash
scripts/qpdf-objecthandle-uniform-identity-probe.sh
```

Expected: `qpdf uniform object identity probe: ok`.

- [ ] **Step 5: Run the byte-identical corpus used by CI**

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
cargo test -p flpdf-cli --features qpdf-zlib-compat --test encrypt_cli_tests
cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_baseline_static_id -- --nocapture
cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_matrix_baseline -- --nocapture
cargo test -p flpdf-qtest-tools --features qpdf-zlib-compat --test e2e
```

Expected: all comparisons pass without regenerating or re-blessing any golden.

---

## Task 8: Prove fresh 100% changed executable-line coverage

**Files:** Tests may be added only to `object_handle.rs` or the existing `reader.rs` test module. Production `cov:ignore` is allowed only for a demonstrated invariant-impossible branch with an explanatory comment; never use it to hide an untested semantic path.

- [ ] **Step 1: Ensure coverage measures committed code**

```bash
git status --short
git diff --check origin/main...HEAD
```

Expected: clean worktree and no whitespace errors. If Task 7 required edits, commit them before coverage.

- [ ] **Step 2: Generate a fresh report with CI-equivalent features**

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: 100% of changed executable Rust lines covered.

- [ ] **Step 3: Perform qualitative branch review**

Confirm assertions—not mere execution—cover:

- direct clone, indirect clone, and promoted alias pointer identity;
- both directions of mutation visibility;
- shared child, stream dictionary, and stream data allocations;
- parsed-offset preservation during promotion;
- repeated and cross-document last-write metadata;
- latest resolver selection and dropped-latest-resolver error;
- additive provenance versus active Pdf identity;
- containment promotion, re-promotion, detach-one-occurrence, expired parent, deep chain, and direct cycle;
- unresolved, non-null resolved, literal-null, missing, and already-destroyed disconnect states;
- surviving handles after real `Pdf::drop` and reciprocal object-cycle release.

- [ ] **Step 4: Commit any coverage-only test additions**

```bash
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/reader.rs
git commit -m "test(object_handle): close uniform identity branch coverage"
```

Skip this commit if no additions were needed, then rerun Step 2 if a commit was created.

---

## Task 9: Independent review, Beads closure evidence, and publication

**Files:** No source edits planned. Any review fix returns to the owning TDD task and repeats all affected gates.

- [ ] **Step 1: Review the final diff against scope**

```bash
git diff --stat origin/main...HEAD
git diff --name-only origin/main...HEAD
git log --oneline --decorate origin/main..HEAD
rg -n 'promote_to_indirect' crates/flpdf/src
```

Expected files only:

- `crates/flpdf/src/object_handle.rs`
- `crates/flpdf/src/reader.rs` only if its test module changed
- `crates/flpdf/src/reader/resolver.rs` only for the `#[cfg(test)]` post-disconnect owner-drop expectation update (post-review scope correction)
- `tests/oracle/qpdf_objecthandle_uniform_identity_probe.cc`
- `scripts/qpdf-objecthandle-uniform-identity-probe.sh`
- the approved design spec
- this implementation plan

Expected production callers of `promote_to_indirect`: none in this issue. Only unit tests exercise it until `flpdf-25kg.3.24` consumes it.

- [ ] **Step 2: Verify the public allocator stayed unchanged**

```bash
git diff origin/main...HEAD -- crates/flpdf/src/reader.rs | sed -n '1,260p'
```

Expected: test-only changes. If `Pdf::make_indirect_object_handle` or `next_available_object_ref` changed, revert that scope by an ordinary patch—not `git reset`—and rerun affected tests.

- [ ] **Step 3: Request code review**

Invoke `superpowers:requesting-code-review` and provide the reviewer:

- Bead `flpdf-25kg.3.26` acceptance criteria;
- approved design spec;
- qpdf source citations and probe command;
- diff range `origin/main...HEAD`;
- explicit non-goals and public allocator exclusion;
- exact verification and coverage results.

Address validated findings with `superpowers:receiving-code-review`, source verification, RED-to-GREEN tests, and repeated relevant gates.

- [ ] **Step 4: Record exact evidence in Beads**

Append notes containing:

- final qpdf pin and source citations;
- oracle probe result;
- focused/full/byte-identical commands and pass counts;
- clippy, fmt, strict rustdoc, and module-doc results;
- changed executable-line numerator/denominator and 100% result;
- final commit SHA;
- confirmation that no public allocator/cache/writer consumer migrated.

Read the Bead back after mutation:

```bash
bd show flpdf-25kg.3.26
bd dolt push
```

- [ ] **Step 5: Push git and prepare the PR**

```bash
git status --short --branch
git pull --rebase
git push
```

State explicitly before pushing that the branch and Beads state will be published. Open or update a draft PR against `main`; do not merge unless the user asks.

- [ ] **Step 6: Close only after merged-main verification**

Do not close the Bead merely because the feature branch is green. After the user reports the exact PR merged, independently verify merge metadata, sync `main`, rerun the required merged-tree checks, append merge evidence, close `flpdf-25kg.3.26`, `bd dolt push`, and remove only the confirmed merged worktree/branch.

---

## Completion Checklist

- [ ] One `Rc<RefCell<ObjectSlot>>` backs every direct and indirect alias.
- [ ] `promote_to_indirect` is crate-private, infallible, same-allocation, and has no production consumer in this issue.
- [ ] `object_ref`, active Pdf identity, and resolver update together with last-write semantics.
- [ ] Promotion preserves `ObjectState`, parsed offset, children, stream dictionary, and stream data.
- [ ] Containment uses weak immediate-parent slots and dynamically reads current root metadata.
- [ ] Additive provenance survives re-promotion and detach independently of active ownership.
- [ ] Resolver calls occur without a live slot borrow.
- [ ] Disconnect clears active metadata for all aliases, destroys only non-null values, and breaks cycles.
- [ ] Public allocator/cache/dirty/writer behavior is unchanged.
- [ ] Pinned qpdf probe, focused tests, full workspace, byte corpus, fmt, clippy, docs, and 100% patch coverage all pass.
- [ ] Review findings are resolved, Beads evidence is pushed, git is pushed, and merge remains user-controlled.
