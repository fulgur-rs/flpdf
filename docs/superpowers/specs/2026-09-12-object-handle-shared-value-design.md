# ObjectHandle shared-value ownership design

**Goal:** Port qpdf 11.9.0's `QPDFObject`/`QPDFValue` ownership boundary to
`flpdf` so one logical value is represented by one shared Rust value allocation,
without changing qpdf-visible identity, lazy resolution, warnings, containment,
teardown, or output bytes.

**Authority:** The pinned qpdf 11.9.0 source at
`/home/ubuntu/.cache/flpdf/qpdf-11.9.0` is authoritative. The current qpdf
commit is `3b97c9bd266b7c32ea36d3536e22dab77412886d`; the installed oracle is
`qpdf version 11.9.0`.

## Scope

This issue owns the canonical `ObjectHandle` value representation in
`crates/flpdf/src/object_handle.rs`:

- co-locating the value payload, active value identity, parsed offset,
  description, and common auxiliary state in one shared allocation;
- preserving the outer per-handle allocation so
  `ObjectHandle::is_same_object_as` continues to distinguish qpdf object
  identities;
- removing the separate `Rc` allocations that exist only because the value
  state is currently split;
- migrating constructors, assignment, swapping, detached replacement,
  resolution, stream state, warning descriptions, and teardown to the new
  representation;
- preserving the existing explicitly marked `containment_parents` deviation
  and its per-slot semantics. The shared `state_owners` index is retained
  inside the shared value state because removing its propagation would change
  those semantics. Its separate `Rc` allocation is removed, and the issue and
  correspondence documentation will record that the propagation loops remain
  as containment bookkeeping rather than qpdf value state.

The issue does not remove the raw `Object` representation, redesign stream-only
state placement, alter qpdf traversal order, or add a compatibility bridge.
Those are separate responsibilities tracked by existing follow-up issues.

## qpdf contract

qpdf's `QPDFObjectHandle` holds one `std::shared_ptr<QPDFObject>`
(`include/qpdf/QPDFObjectHandle.hh:1398-1401`). `QPDFObject` holds one
`std::shared_ptr<QPDFValue>` and delegates copy, unparse, JSON, type, and
description operations to that value
(`libqpdf/qpdf/QPDFObject_private.hh:19-75`).

`QPDFValue` is the shared value boundary. Its type code/name, object
description, owning `QPDF*`, `QPDFObjGen`, and parsed offset are value fields
(`libqpdf/qpdf/QPDFValue.hh:18-27,60-80,123-152`). Concrete array, dictionary,
stream, and scalar subclasses carry their payload in that same value
allocation family. Child containers retain forward object handles; they do not
maintain a reverse containment index
(`libqpdf/QPDF_Array.cc:33-48,103-119`;
`libqpdf/QPDF_Dictionary.cc:10-18,51-56`).

The value-pointer operations are observable contracts:

| qpdf operation | source contract | design consequence |
|---|---|---|
| `QPDFObject::assign` | `value = o->value` (`QPDFObject_private.hh:117-120`) | Assignment shares the complete value state; it must not copy payload children or propagate separate state cells. |
| `QPDFObject::swapWith` | swaps value pointers, then restores each value's ObjGen (`QPDFObject_private.hh:122-131`) | Swapping exchanges complete value state while preserving each handle's object identity projection. |
| `QPDFObject::setObjGen` | writes `value->qpdf` and `value->og` (`QPDFObject_private.hh:139-143`) | Owning-document and raw ObjGen state belongs to the shared value state. |
| `QPDFObject::disconnect` | disconnects the value, then clears qpdf/ObjGen (`QPDFObject_private.hh:145-151`) | Teardown must clear shared value identity without recursively disconnecting indirect children. |
| `QPDF::updateCache` | sets the incoming value identity, assigns it into an existing cache object, and updates cache span metadata (`QPDF.cc:1843-1857`) | Value identity/description state and cache-only span state must be classified separately. |

qpdf's value subclasses may recursively disconnect direct child values while
leaving indirect child objects to the document cache walk
(`libqpdf/QPDFObjectHandle.cc:229-237`; `QPDF_Array.cc:103-119`;
`QPDF_Dictionary.cc:51-56`; `QPDF_Stream.cc:167-171`).

## Responsibility mapping

| Current flpdf field | qpdf responsibility | new placement |
|---|---|---|
| `ObjectSlot::state` (`Rc<RefCell<ObjectValue>>`) | `QPDFValue` payload | `SharedValueState::value` |
| `ObjectSlot::identity` (`Rc<RefCell<ValueIdentity>>`) | `QPDFValue::qpdf` and `QPDFValue::og` | `SharedValueState::identity` |
| `parsed_offset` | `QPDFValue::parsed_offset` | `SharedValueState::parsed_offset` |
| `description` | `QPDFValue::object_description` | `SharedValueState::description` |
| `stream_token_filters`, `content_normalization_applied`, `mutation_generation` | value/stream-local mutable state used by the existing canonical writer | fields in the one shared value state; stream-only relocation remains `.3.4` scope |
| `state_owners` | no qpdf counterpart; bookkeeping required by flpdf's per-slot containment deviation | `SharedValueState::state_owners`, with the existing propagation loops retained but no separate `Rc` |
| `initialized` | whether the handle has an object allocation, distinct from qpdf's unresolved value | per-slot `ObjectSlot` field |
| `end_before_space`, `end_after_space` | `QPDF::ObjCache` source-span metadata, not `QPDFValue` | per-slot/cache field |
| `tree_pdf_unique_id` | flpdf helper provenance, not qpdf value identity | per-slot field unless current callers prove it is value-shared |
| `containment_parents` | explicitly documented flpdf reverse-edge deviation | per-slot field, with weak references to avoid cycles |

The outer `Rc<RefCell<ObjectSlot>>` remains the Rust counterpart of qpdf's
`QPDFObject` handle allocation. A cloned `ObjectHandle` therefore remains
pointer-identical, while assignment can make two distinct outer slots share one
`SharedValueState`, matching qpdf's distinct `QPDFObject` pointers sharing one
`QPDFValue` pointer.

## Design

Introduce one crate-private shared state type, conceptually:

```text
ObjectHandle -> Rc<RefCell<ObjectSlot>>
                         |
                         +-> Rc<RefCell<SharedValueState>>
                                  |
                                  +-> ObjectValue + ValueIdentity +
                                      parsed/description/common aux state
```

`SharedValueState` owns the mutable `ObjectValue` and all state that must move
with qpdf's value pointer. It is constructed by one central constructor used
by every direct, indirect, reserved, and uninitialized path. Empty auxiliary
collections are represented as fields of this allocation, never as additional
per-value `Rc` cells.

`ObjectSlot` retains only handle/cache/provenance fields. Its outer allocation
is never shared by `assign_value_state`; only the `shared` pointer is replaced.
Detached replacement creates a fresh shared state for the departing slot, so
an alias that still points at the old qpdf value observes no mutation from the
replacement.

The existing weak reverse containment index is a known flpdf-only mechanism.
It is not used for writer scheduling or output. Because its public test
observers report per-slot containment roots, the propagation list is retained
inside the shared value allocation while `containment_parents` remains on each
slot. This preserves current-root behavior without inventing a qpdf
counterpart or paying for a separate state-owner allocation.

> **[provisional — settled by TDD, not by this document]**
>
> The exact borrow-safe order for `assign_value_state`,
> `swap_value_state_with`, `replace_detached_state`, and child attach/detach is
> an implementation detail. The implementation will first snapshot only the
> handles needed to release Rust borrows, perform the shared-pointer operation,
> then restore object identity projections and containment bookkeeping. Tests
> are authoritative for alias, mutation, and teardown behavior.
>
> **[/provisional]**

## Invariants

- `is_same_object_as` is outer-slot identity, never structural equality and
  never the address of the shared value state.
- A shared value state has exactly one active `ObjectValue`, identity,
  parsed-offset/description state, and common auxiliary state visible through
  all slots that share it.
- Assignment shares the complete shared state and does not shallow-copy child
  collections.
- Swapping exchanges shared state while preserving each outer slot's qpdf
  object generation projection and existing source-span contract.
- Detached replacement and document removal do not mutate an alias's shared
  value state.
- Lazy indirect resolution still calls the same `DocumentResolver` exactly
  where the old state did; unresolved, reserved, destroyed, and null values
  remain distinct.
- Direct child forward edges and the existing weak containment safety net do
  not create strong cycles; document teardown remains stack-safe.
- Warning descriptions and parsed offsets retain the same qpdf shifts and raw
  bytes.
- Public `ObjectHandle` APIs and writer output remain unchanged.

## Implementation units

1. Add `SharedValueState` and centralize construction of all `ObjectSlot`
   variants. Add private accessors so value/identity/metadata reads and writes
   borrow the shared state consistently.
2. Migrate `ObjectSlot` methods and every value mutation site, including
   resolver assignment/swap, stream replacement, array/dictionary mutation,
   description/offset methods, and teardown.
3. Remove redundant state-cell allocation by moving `state_owners` into
   `SharedValueState`; retain its propagation loops as the explicitly marked
   per-slot containment bookkeeping and prove their behavior with alias
   mutation tests.
4. Update qpdf correspondence documentation with the final ownership mapping
   and any retained explicitly marked deviation.

## Verification and acceptance

Acceptance is measured against qpdf 11.9.0 and the current `origin/main`:

- A direct scalar requires no more than three heap allocations in a counting
  allocator test; wide scalar, array, dictionary, stream, alias, and
  replacement cases report before/after live allocation sizes.
- A 1,000 / 2,000 / 5,000 / 10,000 / 20,000-page matrix reports wall time and
  maximum RSS for qpdf and flpdf, with the one-page baseline subtracted.
- Alias assignment, direct promotion, `swapObjects`, detached replacement,
  lazy resolver success/failure, warning descriptions, null/reserved/destroyed
  transitions, direct containment, deep teardown, and cycle guards have focused
  regression coverage.
- qpdf-zlib compatibility tests and flpdf-cli output/exit comparisons remain
  byte-identical wherever the existing suite claims byte parity.
- `cargo fmt --all -- --check`, all-features workspace tests, all-target
  all-features Clippy with `-D warnings`, strict private rustdoc, qpdf module
  documentation/deviation/route checks, and fresh patch coverage pass.

The implementation is complete only when the answer is recorded that
`state_owners` and its five propagation loops remain necessary for the existing
per-slot containment deviation but no longer require a separate allocation,
with measured evidence. No qtest exceptions work is included.
