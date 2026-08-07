# qpdf Uniform Object Identity Design

**Issue:** `flpdf-25kg.3.26`

**Goal:** Represent every direct and indirect `ObjectHandle` with one shared
object allocation so direct-to-indirect promotion adds qpdf metadata in place,
without cloning the value, container children, stream dictionary, or stream
payload.

## qpdf authority

Pinned qpdf 11.9.0 defines one identity boundary:

- every `QPDFObjectHandle` holds a `shared_ptr<QPDFObject>` and
  `isSameObjectAs` compares that pointer
  (`include/qpdf/QPDFObjectHandle.hh:304-309,1338-1350`;
  `libqpdf/QPDFObjectHandle.cc:224-227`);
- `QPDFObject` holds one `shared_ptr<QPDFValue>`, while the value owns the
  active `QPDF*`, `QPDFObjGen`, description, and parsed offset
  (`libqpdf/qpdf/QPDFObject_private.hh:19-29,60-68,117-150,176-180`;
  `libqpdf/qpdf/QPDFValue.hh:60-72,90-110,144-152`);
- `QPDFObject::doResolve` reads the active QPDF and ObjGen from that same value
  (`libqpdf/QPDFObject.cc:7-11`);
- `QPDF::makeIndirectFromQPDFObject` registers the exact same object pointer
  and `newIndirect` adds the document/ObjGen metadata in place
  (`libqpdf/QPDF.cc:1835-1839,1882-1897`); and
- `isIndirect()` derives from the shared value's ObjGen, so every outstanding
  clone becomes indirect when that object is promoted
  (`include/qpdf/QPDFObjectHandle.hh:1629-1633`).

Live probes linked to installed libqpdf 11.9.0 and compiled against the pinned
headers establish behavior not guarded explicitly in the source:

- the original direct handle, its clone, and the returned indirect handle are
  `isSameObjectAs`, and mutations through either side are mutually visible;
- promoting the same object again does not fail: all handles remain the same
  object and observe the latest ObjGen;
- promoting it into another `QPDF` also does not fail: the active QPDF/ObjGen
  metadata is overwritten on the same object; and
- when that later owning document is dropped, the surviving handles observe
  the common object as non-indirect and destroyed.

Therefore this port must not add a double-attachment error, a conflicting-
document error, a redirect object, or any other qpdf-incompatible guard.

## Current divergence

`ObjectHandle` currently stores a `Repr` enum whose variants own unrelated
allocations:

```rust
enum Repr {
    Direct(Rc<RefCell<DirectSlot>>),
    Indirect(Rc<RefCell<IndirectSlot>>),
}
```

This creates several coupled gaps:

- `is_same_object_as` can only match handles inside the same variant;
- `is_direct`, `is_indirect`, and `object_ref` inspect the handle's storage
  variant rather than shared object metadata;
- `Pdf::make_indirect_object_handle` calls `direct_value_clone`, creates a
  different indirect slot, and shallow-copies a stream dictionary;
- mutations and resolution after that clone are not visible through the
  original direct handle;
- direct containment uses weak `DirectSlot` parents while indirect roots use
  copied `ContainmentOwner` snapshots, so an in-place promotion cannot be
  represented without rewiring the reverse graph; and
- disconnect destroys an indirect state but retains its object reference,
  whereas qpdf clears ObjGen before replacing a non-null value with
  `QPDF_Destroyed`.

The public `Pdf::make_indirect_object_handle` consumer also rejects an
already-indirect handle, while qpdf rejects only an uninitialized handle. That
public API migration remains a non-goal owned by `flpdf-25kg.3.6`; this issue
provides the faithful internal object boundary it will consume.

## Chosen architecture

### One shared object slot

Replace `Repr`, `DirectSlot`, and `IndirectSlot` with one allocation:

```rust
pub struct ObjectHandle(Rc<RefCell<ObjectSlot>>);

struct ObjectSlot {
    state: ObjectState,
    object_ref: Option<ObjectRef>,
    active_pdf_unique_id: Option<u64>,
    resolver: Option<Weak<dyn DocumentResolver>>,
    parsed_offset: i64,
    pdf_unique_ids: BTreeSet<u64>,
    containment_parents: Vec<Weak<RefCell<ObjectSlot>>>,
}

enum ObjectState {
    NotYetResolved,
    Resolved(ObjectValue),
    Missing,
    Destroyed,
}
```

The `Option<ObjectRef>` is the Rust representation of whether qpdf's ObjGen is
indirect; it is not a numeric sentinel. The following invariants hold:

- a newly constructed direct value has `object_ref == None` and
  `state == Resolved(value)`;
- a canonical unresolved object has `object_ref == Some(og)` and
  `state == NotYetResolved`;
- `Missing` remains distinct from a resolved literal null;
- `Destroyed` represents qpdf's destroyed value, independently of whether the
  object had previously been indirect; and
- every clone shares all state and metadata through the one outer `Rc`.

`is_same_object_as` becomes one unconditional `Rc::ptr_eq`. `is_direct`,
`is_indirect`, and `object_ref` read `object_ref`; value access reads
`ObjectState`. No accessor dispatches through a direct/indirect storage enum.

### Metadata and flpdf provenance

`object_ref`, `active_pdf_unique_id`, and `resolver` are one active metadata
set corresponding to qpdf's `QPDF*` plus ObjGen. They are updated together.
The weak resolver and numeric identity remain a Rust split of qpdf's single
raw `QPDF*`, as already documented by the canonical resolver primitive.

`pdf_unique_ids` remains separate additive provenance for direct values. It is
flpdf's approved incremental-writer bookkeeping and must not be collapsed into
the active metadata:

- promotion records the new active Pdf identity in the provenance set and
  propagates it through current direct descendants;
- re-promotion overwrites the active metadata but does not erase provenance;
- `belongs_to_pdf` preserves its current contract: an indirect object matches
  only its active Pdf, while a direct value uses additive provenance; and
- detaching or destroying an active root does not fabricate a new owner for a
  retained direct child.

No separate default-description string is stored. qpdf's default description
is derived from ObjGen, and flpdf's corresponding diagnostics can derive it
from the same `object_ref`. This issue does not add a second description
authority.

### Internal promotion primitive

Add one crate-private primitive, intended for the later canonical allocator:

```rust
pub(crate) fn promote_to_indirect(
    &self,
    object_ref: ObjectRef,
    pdf_unique_id: u64,
    resolver: Weak<dyn DocumentResolver>,
) -> Self;
```

The method mutates only the shared slot metadata, propagates the Pdf identity
through current direct descendants, and returns `self.clone()`. It does not:

- choose an ObjGen;
- register anything in `ResolverCore::object_cache`;
- materialize or clone `ObjectValue`;
- mark an object dirty;
- reject an already-indirect object or a different active Pdf; or
- resolve the object.

Repeated calls overwrite `object_ref`, `active_pdf_unique_id`, and `resolver`
together, matching qpdf's last-write behavior. `parsed_offset` and
`ObjectState` remain attached to the same allocation and are not reset by
promotion.

### Unified live containment edges

With one slot type, reverse containment no longer needs separate `Root` and
`Direct` variants. Every current direct-child occurrence stores one weak edge
to its immediate parent `ObjectSlot`.

Root discovery walks those weak parents:

1. If a live parent has `object_ref == Some(og)`, collect its current
   `active_pdf_unique_id` and `og`, then stop at that indirect boundary.
2. Otherwise continue through that direct parent's weak parents.
3. Deduplicate visited slots by `Rc` identity to terminate direct cycles.
4. Ignore expired weak parents.

This is the same current-forward-membership model completed by
`flpdf-25kg.3.22`, expressed against the new uniform allocation. Promotion and
re-promotion require no edge detachment or replacement: existing children
immediately observe the parent's latest metadata. Add/remove/replace still
attach and detach one weak edge per forward occurrence.

Indirect children are never attached as direct-containment edges. Root
discovery never resolves an object or scans a document cache.

## Resolution and borrow discipline

`try_dereference` preserves its existing qpdf-shaped call order:

1. borrow the slot only long enough to snapshot state, active ObjGen, and the
   weak resolver;
2. release the `RefCell` borrow;
3. upgrade and call the resolver;
4. observe the resolver's in-place update through the same slot.

Promotion performs no resolver call. Container mutation releases its slot
borrow before attaching or detaching weak child edges. These rules keep
resolver re-entry, reciprocal references, and containment traversal free from
nested `RefCell` borrows.

## Disconnect and owner-drop behavior

`Pdf::drop` continues to walk the canonical object cache and disconnect each
entry. Disconnect operates on the shared slot:

- detach the current resolved value's immediate child edges;
- clear `object_ref`, `active_pdf_unique_id`, and `resolver`, so every surviving
  clone becomes non-indirect as in qpdf;
- for a resolved literal null or `Missing`, preserve null rather than replacing
  it with `Destroyed`, matching qpdf's null exception;
- for unresolved or resolved non-null values, replace the common state with
  `Destroyed` and reset the parsed offset to `-1`; and
- preserve the existing additive direct provenance and weak-edge cleanup
  contracts required by flpdf's ownership checks.

The cycle-breaking property is unchanged: destroying a resolved non-null value
drops its strong child handles, while reverse containment remains weak.

## Error behavior

- Promotion is infallible because qpdf's metadata assignment is infallible.
- No double-promotion or foreign-document error is introduced.
- Resolver upgrade/call errors retain their existing `crate::Error` mapping.
- No sentinel value, redirect variant, panic, raw-`Object` bridge, or
  compatibility wrapper is added.

## Testing

Every production change follows RED to GREEN TDD.

### Pinned-qpdf probe

Add a focused script that builds and runs the identity/metadata/lifecycle
probe against pinned 11.9.0 headers and installed libqpdf 11.9.0. It asserts:

- original direct, outstanding clone, and promoted handle share identity;
- the original and clone become indirect with the promoted ObjGen;
- dictionary mutations are visible in both directions;
- repeated and cross-document promotion retain identity and use latest
  metadata; and
- dropping the latest owner makes surviving handles non-indirect and
  destroyed.

### Rust RED-to-GREEN coverage

Focused tests cover:

- one `Rc` identity for direct clones, indirect clones, and promotion;
- original-handle indirect classification and ObjGen after promotion;
- mutation and resolution visibility in both directions;
- no clone of array/dictionary children, stream dictionary, or shared stream
  data;
- parsed-offset preservation across promotion;
- repeated and cross-document promotion with latest active metadata;
- active Pdf identity versus additive provenance;
- immediate-parent containment before and after promotion and re-promotion;
- root detachment after replacement/removal;
- unresolved resolver re-entry without a held slot borrow;
- self/reciprocal indirect graphs and direct containment cycles;
- owner drop with surviving non-null, literal-null, and missing handles; and
- the existing parser/resolver/ObjectHandle contract suites.

Verification requires focused tests, `cargo test -p flpdf`, workspace tests,
`cargo fmt --all -- --check`, all-target/all-feature clippy, the strict rustdoc
gate, the qpdf module-doc gate, fresh 100% changed executable-line coverage,
and the issue's byte-identical corpus.

## Non-goals

- ObjGen selection or `QPDF::nextObjGen` (`flpdf-25kg.3.24`);
- canonical cache registration (`flpdf-25kg.3.24`);
- public `Pdf::make_indirect_object_handle` migration (`flpdf-25kg.3.6`);
- `replaceObject`, `removeObject`, or writer write-back (`flpdf-25kg.3.6`);
- dirty scheduling or writer cutover;
- StreamDataProvider, filter-pipeline, Filespec, or page-helper migration; and
- changing the policy for which direct container cycles callers may create.

## Rejected alternatives

- **Outer shared allocation plus direct/indirect handle views:** lets two
  handles disagree about whether the same object is indirect. qpdf stores
  indirectness on the shared value.
- **Direct-to-indirect redirect:** requires every accessor, resolver, drop, and
  containment operation to follow a qpdf-incompatible special case and creates
  new redirect-cycle states.
- **Rewire copied root edges during promotion:** snapshots metadata that qpdf
  mutates in place and requires another rewrite on every re-promotion. A weak
  immediate parent to the uniform slot observes current metadata naturally.
- **Reject repeated or cross-document promotion:** contradicted by pinned
  qpdf's source and live behavior.
- **Migrate the public allocator now:** mixes shared-object representation with
  ObjGen selection, cache registration, dirty scheduling, and consumer
  cutover, all owned by downstream issues.
