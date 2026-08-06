# qpdf Live Direct Containment Design

**Issue:** `flpdf-25kg.3.22`

**Goal:** Keep flpdf's incremental dirty-owner lookup synchronized with the
current direct-child membership of dictionaries, arrays, and stream/direct
values, matching qpdf 11.9.0 after replacement and removal without resolving
unrelated objects.

## qpdf authority

qpdf's source graph is authoritative:

- `QPDFObjectHandle::isSameObjectAs` compares the shared object payload
  (`libqpdf/QPDFObjectHandle.cc:223-227`).
- `QPDF_Dictionary::replaceKey` and `removeKey` update the dictionary's current
  map directly (`libqpdf/QPDF_Dictionary.cc:135-153`).
- `QPDF_Array::setAt` and `setFromVector` replace the current child pointers
  directly (`libqpdf/QPDF_Array.cc:219-243`), reached through
  `QPDFObjectHandle::setArrayItem` and `setArrayFromVector`
  (`libqpdf/QPDFObjectHandle.cc:871-893`).
- `QPDFWriter::enqueueObject` traverses only the current array and dictionary
  children (`libqpdf/QPDFWriter.cc:1126-1137`). A detached child is not a
  member of its former container.
- `QPDFObjectHandle::checkOwnership` compares owning `QPDF*` identities before
  dictionary insertion (`libqpdf/QPDFObjectHandle.cc:1199-1209,2356-2364`).
  That document identity is not the same concept as current containment.

qpdf does not maintain a reverse owner graph because it rewrites from the
current forward graph. flpdf's default writer is incremental and must map an
in-place direct-child mutation back to the indirect objects that currently
contain it. A reverse index is therefore permitted only as derived bookkeeping:
every stored reverse edge must correspond one-for-one to a current qpdf-style
forward container edge.

## Current divergence

`DirectSlot::containing_object_refs` is a flattened, additive set of indirect
roots. `set_resolved`, `replace_key`, `replace_array_item`,
`replace_array_items`, and `replace_direct_value` recursively add roots to new
descendants. `remove_key` and the replacement paths never remove roots from old
descendants.

Consequences:

- a retained detached child can still dirty and incrementally emit its former
  owner;
- the set cannot distinguish two keys leading to the same child from one key;
- nested paths and multiple indirect roots cannot be detached independently;
- recursively propagating flattened roots is an flpdf representation with no
  qpdf counterpart.

## Chosen architecture

### Current membership edges

Replace the flattened root set with immediate reverse edges. Each direct slot
stores one `ContainmentParent` entry per current forward occurrence:

```rust
enum ContainmentParent {
    Root(ContainmentOwner),
    Direct(Weak<RefCell<DirectSlot>>),
}
```

`Root` represents an immediate child of a canonical indirect object's resolved
value. `Direct` represents an immediate child of a direct array, dictionary, or
stream value. Duplicate keys/items intentionally create duplicate parent
entries. Removing one occurrence removes exactly one matching reverse edge.

The direct-parent variant is weak: forward container values already hold their
children strongly, while reverse edges must not create a new ownership cycle.
An expired weak parent contributes no live root and may be pruned opportunistically.

### Document identity remains separate

Current containment and Pdf identity must not be collapsed. A detached parsed
direct child still originated in its Pdf, just as qpdf's direct `QPDFValue`
retains its owning `QPDF*`. Each `DirectSlot` therefore keeps the existing
additive Pdf identity provenance separately from its live parent edges.

Attaching a root propagates its `pdf_unique_id` through current direct
descendants, stopping at indirect boundaries, as the existing implementation
does. When a new child is later attached to a contained direct parent, it
inherits that parent's already-recorded Pdf identities through its current
direct descendants. Detaching an edge never erases this identity provenance.
Consequently:

- `belongs_to_pdf` continues to reject a retained foreign direct child after
  it has been detached;
- an identity-free programmatically constructed direct value remains accepted;
- `containing_object_refs_for_pdf` returns only currently reachable roots and
  never substitutes identity provenance for membership.

Exact `QPDFObjectHandle::checkOwnership` warning/exception behavior remains out
of scope, as required by the issue.

### Root discovery

For a direct handle, root discovery walks only its reverse parent graph:

1. Start with the handle's `DirectSlot`.
2. For each `Root`, collect the `ContainmentOwner`.
3. For each live `Direct` parent, visit that parent.
4. Deduplicate visited direct slots by `Rc` identity to terminate direct cycles.
5. Stop at indirect roots; never dereference an indirect child or inspect the
   Pdf registry.

The result is a set of canonical indirect roots reachable through current
container membership. Multiple remaining paths collapse to one dirty root;
removing one path retains the root while any other path remains.

## Mutation boundary

One internal edge primitive owns attach/detach behavior. All forward mutations
must update their immediate reverse edges after releasing any `RefCell` borrow:

- direct array/dictionary/stream construction attaches each immediate direct
  child to the new direct parent;
- `set_resolved` detaches the previous root children, replaces the indirect
  state, then attaches the new immediate root children;
- `set_missing` and `disconnect` detach previous root children before removing
  the resolved value;
- `replace_key` detaches the replaced value once and attaches the new value
  once;
- `remove_key` detaches the removed value once;
- `replace_array_item` detaches the old item once and attaches the new item
  once;
- `replace_array_items` detaches every old occurrence and attaches every new
  occurrence;
- `replace_direct_value` detaches all immediate children of the old value and
  attaches all immediate children of the new value.

Indirect children are never attached as direct-containment edges. Existing
self-cycle rejection and other direct-cycle policy remain unchanged.

## Dirty tracking behavior

`Pdf::mark_object_handle_dirty` keeps its current public behavior:

- canonical indirect handles dirty themselves;
- direct handles are checked against preserved Pdf identity provenance;
- each live root for the current Pdf is marked dirty;
- an owned but detached child succeeds without dirtying any former root;
- no operation resolves unrelated lazy objects.

The incremental writer then emits only current owners. Mutating a retained
detached child cannot change output unless that child is reattached and its
current owner is marked.

## Error and cycle handling

- No sentinel, panic, document-wide scan, or qpdf-incompatible error branch is
  added.
- Borrow scopes end before attaching or detaching child edges, preventing
  `RefCell` re-entry during reciprocal direct graphs.
- Root discovery uses a visited set, so an existing multi-hop direct cycle
  terminates without changing the policy that allowed that cycle.
- A dead weak parent is treated as absent current membership.

## Testing

Every production change follows RED to GREEN TDD.

Focused `ObjectHandle` tests cover:

- sole dictionary edge replacement and removal;
- two dictionary keys sharing one child, removing one and then the last;
- nested direct containers;
- one direct subtree shared by two indirect roots, detaching one root only;
- single-item and whole-array replacement, including duplicate items;
- `replace_direct_value` and stream dictionary membership;
- indirect-boundary stopping;
- direct-cycle root discovery termination;
- Pdf identity preservation after detach.

Focused `Pdf` tests cover:

- a detached child neither dirties nor incrementally emits its former owner;
- a currently attached child dirties and emits the correct owner;
- foreign direct-handle rejection remains unchanged;
- unrelated lazy objects are not resolved.

Verification requires focused tests, `cargo test -p flpdf`, workspace tests,
format, all-target/all-feature clippy, fresh 100% changed executable-line
coverage, and the byte-identical corpus required by the Bead.

## Non-goals

- direct-null `QPDF_Dictionary::replaceKey` behavior (`flpdf-25kg.3.20`);
- exact `QPDFObjectHandle::checkOwnership` warning/exception surface;
- StreamDataProvider, filter pipeline, or Filespec migration;
- changing direct-container cycle policy;
- replacing the incremental writer with a full qpdf-style rewrite.

## Rejected alternatives

- **Flattened owner path counts:** requires recursively maintaining a derived
  root/path representation through sharing and cycles. qpdf stores current
  container children, not descendant root counts.
- **Document-wide owner scan:** resolves unrelated objects and recreates the
  failure fixed by `flpdf-3yn9.3`.
- **Keep stale roots and filter later:** a flattened stale root has no evidence
  from which current reachability can be recovered without a scan.
