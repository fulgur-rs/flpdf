# ObjectHandle Type Introspection Result Design

## Goal

Align `ObjectHandle::type_code` and `ObjectHandle::type_name` with qpdf 11.9.0's
type-introspection responsibility: an initialized handle resolves its own
indirect object before reporting the underlying type, and resolution failures
are propagated through `Result`.

The public method names remain `type_code` and `type_name` in this issue. The
later qpdf-derived naming cutover is outside `flpdf-25kg.8`.

## Oracle and responsibility boundary

Pinned qpdf 11.9.0 provides the following contract:

- `QPDFObjectHandle::getTypeCode()` and `getTypeName()` call
  `dereference()` before inspecting the underlying object
  (`libqpdf/QPDFObjectHandle.cc:240-250`).
- `dereference()` is a no-op for an already resolved value and calls the
  object's `resolve()` otherwise (`libqpdf/QPDFObjectHandle.cc:2376-2382`).
- `QPDFObject::resolve()` checks `isUnresolved()` before doing I/O and turns a
  failed resolution into the null object (`libqpdf/qpdf/QPDFObject_private.hh:155-166`,
  `libqpdf/QPDF.cc:1700-1750`).

Therefore resolution belongs to `ObjectHandle`, not to the value-type match.
`ObjectHandle::try_dereference` already owns this responsibility and returns
the crate's `Result`, so no prerequisite primitive is needed.

## Chosen approach

The implementation will make `ObjectHandle::type_code` the single fallible
classification boundary:

1. Read `ObjectState::Reserved` and `ObjectState::Destroyed` first and return
   qpdf's existing ordinals `1` and `14` without invoking a resolver. These
   states represent qpdf's internal sentinels and are intentionally preserved
   in this refactor stage.
2. Call `self.try_dereference()?` for every other state. Direct values and
   already-resolved indirect values take the existing no-op path; unresolved
   document handles now resolve before classification; resolver errors escape.
3. Match the resolved `ObjectValue` exactly as today and return
   `Result<u8>`. The flpdf-only `ObjectValue::Reference` bridge continues to
   report `13` in this issue; extracting value-owned classification is the
   responsibility of `flpdf-25kg.9`.
4. Make `type_name` call the fallible `type_code` and return
   `Result<&'static str>`, preserving all existing qpdf type strings.
5. Update each consumer to propagate the new result. Where an error message is
   built inside an `ok_or_else` closure, resolve the type name before entering
   the closure so the resolver error is not hidden or replaced.

## Alternatives considered

### Resolve at every caller

This would keep `type_code` pure, but duplicate qpdf's handle-layer
responsibility and allow future callers to observe the old unresolved sentinel.
It is rejected.

### Add a second resolving accessor and retain the old method

This would preserve the current no-I/O behavior as an implicit compatibility
contract, even though qpdf's public `getTypeCode` resolves. It would also leave
callers choosing between two subtly different type APIs. It is rejected for
the pre-v1 qpdf-parity policy.

### Resolve in `type_code` and keep `type_name` infallible

This would make the two qpdf counterparts disagree about error propagation and
would force `type_name` to conceal a failed resolution. It is rejected.

## Testing contract

The focused tests will prove:

- an unresolved indirect handle with a live resolver is resolved by
  `type_code` and reports the resolved qpdf ordinal;
- `type_name` reports the corresponding name through the same route;
- a resolver error from either accessor is returned unchanged;
- Reserved and Destroyed handles still return `1`/`14` and their names;
- direct, already-resolved, missing/null, and `ObjectValue::Reference` cases
  retain their existing mappings;
- all existing library tests and the workspace formatting/build checks remain
  green after consumer propagation.

No qpdf behavior, fixture, or public method rename beyond the fallible return
type is in scope.
