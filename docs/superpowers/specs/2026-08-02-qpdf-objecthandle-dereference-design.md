# QPDFObjectHandle Dereference Primitive Design

## Status

Approved direction on 2026-08-02. Responsibility boundary corrected with the
user's approval after testing compressed-object provenance against pinned qpdf
11.9.0. Implementation Bead: `flpdf-25kg.3.3`.

This design supersedes only the lazy-resolution and transitional-bridge parts
of `2026-07-30-xref-parsed-offset-object-handle-design.md`. Its verified xref
facts remain authoritative except for its claim that compressed objects have
no parsed offset; the live qpdf result below disproves that claim.

## Goal

Translate qpdf 11.9.0's `QPDFObject` / `QPDFObjectHandle::dereference`
primitive directly into Rust. A fallible handle accessor asks its owning
document resolver to update that same canonical indirect slot before the
accessor inspects the value. The primitive does not chase a reference chain or
materialize a detached terminal copy.

This slice defines the object-to-document callback boundary. It does not
implement `QPDF::Resolver`, `Pdf::get_object`, stream decoding, object-stream
resolution, mutation write-back, or a consumer cutover.

## Oracle facts

- `QPDFObjectHandle` owns only `std::shared_ptr<QPDFObject>`
  (`include/qpdf/QPDFObjectHandle.hh:1338-1400`).
- `QPDFObjectHandle::dereference` calls `obj->resolve()` on that same object
  (`libqpdf/QPDFObjectHandle.cc:2376-2383`), and type/dictionary accessors use
  it before reading the value.
- An unresolved `QPDFObject` stores its owning `QPDF*` and `QPDFObjGen`;
  `doResolve` calls `QPDF::Resolver::resolve(qpdf, og)`
  (`libqpdf/qpdf/QPDFObject_private.hh:139-167`,
  `libqpdf/QPDFObject.cc:7-11`). `Resolver` is private so only `QPDFObject`
  can invoke `QPDF::resolve` (`include/qpdf/QPDF.hh:770-781`).
- `QPDFObjectHandle::isSameObjectAs` compares the underlying object pointers,
  not structural values (`libqpdf/QPDFObjectHandle.cc:224-227`).
- `QPDF::updateCache` replaces the value of the already-cached object rather
  than replacing its identity (`libqpdf/QPDF.cc:1844-1864`).
- During destruction, qpdf disconnects and then destroys cached indirect
  objects, breaking cyclic handle graphs (`libqpdf/QPDF.cc:211-236`).

There is no qpdf counterpart to flpdf's `ref_chain`, a terminal-handle clone,
or a canonical indirect slot whose stored value redirects to another indirect
object. `QPDF::replaceObject` rejects an indirect replacement.

## Resolver boundary discovered during implementation

A generated-object-stream differential produced these pinned qpdf results:

```text
root-parsed-offset  9
pages-parsed-offset 43
```

Both objects were compressed members of the same decoded `/ObjStm`. In qpdf,
`QPDF::resolveObjectsInStream` decodes the stream, reads every header entry,
parses every member still owned by that object stream, and updates all matching
canonical cache slots. The parsed offsets are positions in the decoded object
stream, not `-1` and not file offsets (`libqpdf/QPDF.cc:1756-1837`).

Therefore a production `DocumentResolver` that supports only uncompressed
direct objects is not a valid incremental implementation of qpdf's resolver.
It would also need QPDF_Stream/QPDFStreamFilter, encryption, warnings,
recursion-loop handling, full ObjStm cache population, and correct provenance.
The attempted partial resolver was removed instead of being retained behind
`Unsupported` branches.

## Rust primitive

`ObjectHandle` remains a cloneable `Rc<RefCell<_>>`. Its indirect slot is the
Rust equivalent of qpdf's `QPDFObject` state boundary:

```text
ObjectHandle
  -> canonical indirect slot
       object_ref: ObjectRef
       parsed_offset: i64
       state: NotYetResolved | Resolved(ObjectValue) | Missing | Destroyed
       resolver: optional Weak<dyn DocumentResolver>
```

The resolver link is private, weak, and non-owning. `try_dereference` copies
the reference and resolver link out of the slot before calling the resolver,
so no `RefCell` borrow crosses resolver entry. The resolver receives the same
handle and must update that slot in place.

The optional resolver is required temporarily because the existing legacy
constructor still creates unattached handles. New production code must not use
that constructor as a qpdf-native resolver substitute; it is marked for later
cutover with the other legacy routes.

## API

The primitive adds qpdf-shaped fallible accessors because qpdf reports
resolution failures with exceptions:

```rust
impl ObjectHandle {
    pub fn try_dereference(&self) -> Result<()>;
    pub fn try_is_null(&self) -> Result<bool>;
    pub fn try_as_dictionary(&self) -> Result<Option<BTreeMap<Vec<u8>, ObjectHandle>>>;
    pub fn try_get_key(&self, key: &[u8]) -> Result<ObjectHandle>;
    pub fn try_has_key(&self, key: &[u8]) -> Result<bool>;
    pub fn is_same_object_as(&self, other: &ObjectHandle) -> bool;
}
```

`try_has_key` resolves both the dictionary holder and a present child, because
qpdf dictionary visibility treats a present value that resolves to null as
absent. Direct handles are already resolved. Missing objects present as null.
An unresolved handle whose weak resolver has expired returns an error and does
not reconnect to another document.

## Explicit prohibitions

This primitive and its tests must not call or construct:

- `ref_chain::{resolve_ref_chain, terminal_ref_of_chain}`;
- `Pdf::resolve`, `resolve_borrowed`, `resolve_object_handle`, or terminal
  resolver helpers;
- `ObjectHandle::materialize`, raw `Object`, or a terminal clone;
- a partial `Pdf::get_object` implementation; or
- an adapter exposing a legacy resolver under a qpdf-shaped name.

Existing non-fallible accessors and legacy resolver routes remain unchanged.
Deletion targets carry the searchable marker
`qpdf-cutover-delete(flpdf-25kg.3.3)`. `#[deprecated]` is added only after a
complete replacement exists and is immediately usable by callers.

## Dependent delivery order

1. `flpdf-25kg.3.3`: canonical slot and dereference callback primitive.
2. `flpdf-25kg.3.4`: ObjectHandle-native QPDF_Stream/QPDFStreamFilter decode
   primitives, implemented directly rather than through raw `Object`.
3. `flpdf-25kg.3.5`: complete QPDF resolver/cache, including streams,
   encryption, xref streams, and all-member ObjStm resolution. Only this slice
   publishes `Pdf::get_object`.
4. `flpdf-25kg.3.6`: canonical replacement, mutation, dirty tracking, and
   writer write-back.
5. `flpdf-25kg.3.7`: complete QPDF_pages repair cutover followed by deletion of
   its dead legacy route.

## Verification

Unit tests use sealed recording, missing, failing, and dropped resolvers. They
prove one resolver invocation updates every clone of the same canonical slot,
errors propagate unchanged through every fallible accessor, present-null key
visibility matches qpdf, and unrelated handles do not share identity.

The live C++ probe is compiled and linked only against pinned qpdf 11.9.0. It
proves that dictionary/type access triggers resolution while `isIndirect()`
continues to report the holder's indirect identity. Its parsed-offset output
also records the resolver boundary that forced the dependent-slice correction.
