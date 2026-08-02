# QPDFObjectHandle Dereference Primitive Design

## Status

Approved direction on 2026-08-02. Implementation Bead: `flpdf-25kg.3.3`.

This design supersedes only the lazy-resolution and transitional-bridge parts
of `2026-07-30-xref-parsed-offset-object-handle-design.md`. Its xref and
parsed-offset facts remain authoritative.

## Goal

Translate qpdf 11.9.0's `QPDFObject` / `QPDFObjectHandle::dereference`
responsibility boundary directly into Rust. A handle accessor must resolve its
own canonical indirect slot, in place, before inspecting its value. It must
not ask a caller to chase a reference chain or materialize a detached terminal
copy.

## Oracle facts

- `QPDFObjectHandle` owns only `std::shared_ptr<QPDFObject>`
  (`include/qpdf/QPDFObjectHandle.hh:1338-1400`).
- `QPDFObjectHandle::dereference` calls `obj->resolve()` on that same object
  (`libqpdf/QPDFObjectHandle.cc:2376-2383`), and type/dictionary accessors use
  it before reading the value (for example `asDictionary` at
  `libqpdf/QPDFObjectHandle.cc:264-268`).
- An unresolved `QPDFObject` stores its owning `QPDF*` and `QPDFObjGen`;
  `doResolve` calls `QPDF::Resolver::resolve(qpdf, og)`
  (`libqpdf/qpdf/QPDFObject_private.hh:139-167`,
  `libqpdf/QPDFObject.cc:7-11`). `Resolver` is private specifically so only
  `QPDFObject` can invoke `QPDF::resolve` (`include/qpdf/QPDF.hh:770-781`).
- `QPDF::updateCache` replaces the value of the already-cached object rather
  than replacing its identity (`libqpdf/QPDF.cc:1844-1864`).
- During destruction, qpdf disconnects then destroys cached indirect objects,
  breaking cyclic handle graphs (`libqpdf/QPDF.cc:211-236`).

There is no qpdf counterpart to flpdf's `ref_chain`, terminal-handle clone, or
an indirect object whose stored value redirects to another indirect object.
`QPDF::replaceObject` rejects an indirect replacement; qpdf's canonical slot
is updated in place instead.

## Rust model

`ObjectHandle` remains a cloneable `Rc<RefCell<_>>` value, but its indirect
slot becomes the direct equivalent of qpdf's `QPDFObject`:

```text
ObjectHandle
  -> canonical indirect slot { ObjectRef, parsed offset, state, resolver link }
       state = Unresolved | Resolved(ObjectValue) | Missing | Destroyed
       resolver link = Weak<dyn DocumentResolver>
```

The resolver link is private and non-owning. `Pdf` owns the resolver, source,
xref state, cache, diagnostics, and canonical-slot registry. The handle never
owns `Pdf`, never becomes generic over `R`, and cannot resolve a foreign or
dropped document.

To express qpdf's object-to-document call safely, the reader/cache state moves
behind a document-owned `Rc<RefCell<PdfCore<R>>>`. A sealed
`DocumentResolver` implementation holds a `Weak` link to that core. It is
created after the core and registered into each canonical indirect slot.
`ObjectHandle::try_dereference` upgrades the link and asks the resolver to
resolve that slot's `ObjectRef`; the resolver updates that *same* slot. Borrows
are released before resolver entry, preventing `RefCell` re-entrancy.

This is a Rust representation change, not a new semantic layer: it is the
safe equivalent of qpdf's non-owning `QPDF*` stored by `QPDFObject`.

## Public and crate-private API

New qpdf-shaped accessors are fallible because qpdf reports resolution errors
with C++ exceptions:

```rust
impl ObjectHandle {
    pub fn try_dereference(&self) -> Result<()>;
    pub fn try_is_null(&self) -> Result<bool>;
    pub fn try_as_dictionary(&self) -> Result<Option<DictionaryHandle>>;
    pub fn try_get_key(&self, key: &[u8]) -> Result<ObjectHandle>;
    pub fn try_has_key(&self, key: &[u8]) -> Result<bool>;
}
```

The exact typed views are selected by the RED tests, but each `try_*` accessor
must call the one dereference primitive before it examines the slot. A direct
handle is a no-op. A missing indirect object becomes the canonical null slot;
a destroyed object cannot reconnect to a document.

Existing non-fallible accessors and `Pdf::resolve_object_handle*` are not
modified into wrappers. They are left as legacy routes while complete qpdf
components are migrated to the new API, then removed at zero call sites.

## Explicit prohibitions

The new primitive, its resolver, and its tests must not call or construct:

- `ref_chain::{resolve_ref_chain, terminal_ref_of_chain}`;
- `Pdf::resolve_borrowed`, `Pdf::resolve_object_handle`, or
  `resolve_object_handle_to_terminal*`;
- `ObjectHandle::materialize`, raw `Object`, or a terminal clone; or
- an adapter that exposes a legacy route under a qpdf-shaped name.

`set_object`-driven bare-reference redirects are legacy-only semantics. They
are not represented in a canonical slot created by the new resolver and are
removed with the final legacy route.

## Delivery and deletion order

1. Add the canonical-slot/resolver primitive with direct unit tests and a
   pinned-qpdf probe covering lazy accessor resolution, repeated-handle
   identity, missing entries, resolver errors, and document drop.
2. Make the reader register canonical slots through that primitive and prove
   source resolution updates the slot in place.
3. Migrate exactly one complete qpdf component to the new `try_*` API. The
   first consumer is page-tree repair because qpdf resolves its `/Kids` holder
   through `QPDFObjectHandle` accessors.
4. Delete that component's old route rather than retaining an adapter.
5. Repeat by qpdf component until the old resolver, raw `Object`, redirect
   semantics, and `ref_chain` have zero users; then delete them.

The page-tree repair change is therefore a consumer of this primitive, not a
parallel holder-chain implementation in PR #616.

## Verification

Each delivery step starts RED. Rust tests cover both the direct primitive and
one public reader fixture. The qpdf probe is compiled against the pinned 11.9.0
source and demonstrates that `isDictionary`, `hasKey`, and `getKey` trigger
the same lazy resolution while `isIndirect` preserves holder identity.

Before publication: focused `object_handle` and reader tests, the affected
component suite, `cargo fmt --all -- --check`, workspace all-features clippy,
workspace tests, and fresh changed-line coverage. The plan records exact
commands before implementation begins.
