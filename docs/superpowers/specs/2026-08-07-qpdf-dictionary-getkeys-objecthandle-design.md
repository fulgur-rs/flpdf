# qpdf Dictionary getKeys ObjectHandle Design

**Issue:** `flpdf-25kg.3.23`

**Goal:** Add an ObjectHandle-native, null-resolving dictionary key
enumeration primitive that matches qpdf 11.9.0 responsibility boundaries and
can later be consumed by stream-filter handling without embedding filter
knowledge in `ObjectHandle`.

## qpdf authority

Pinned qpdf 11.9.0 defines the behavior across two layers:

- `QPDFObjectHandle::getKeys()` lazily resolves the holder, delegates dictionary
  behavior to `QPDF_Dictionary`, and returns an empty set for a non-dictionary
  holder after qpdf's type-warning path
  (`libqpdf/QPDFObjectHandle.cc:265-268,997-1022` and
  `include/qpdf/QPDFObjectHandle.hh:762-780`); and
- `QPDF_Dictionary::getKeys()` visits every stored map entry, calls `isNull()`
  on its value, omits resolved null values, and inserts the remaining keys in a
  `std::set`, yielding deterministic sorted order
  (`libqpdf/QPDF_Dictionary.cc:117-127`; null resolution is
  `libqpdf/QPDFObjectHandle.cc:345-356`).

The primitive therefore belongs on `ObjectHandle`: holder resolution and
child null resolution are object operations. It must not be implemented as a
filter-name allowlist or as a reader-only dictionary traversal.

## Existing flpdf boundary

`ObjectHandle` already supplies the required lower-level operations:

- `try_as_dictionary()` resolves the holder and exposes a cloned dictionary
  snapshot;
- `try_is_null()` resolves a child through its canonical
  `DocumentResolver`; and
- dictionary handles retain their child handles, so cloning entries preserves
  child identity and resolver behavior.

The writer-private `visible_dict_entries()` has a different responsibility: it
produces serialization-oriented entries and is not the general object
primitive qpdf consumers require. This change will neither promote nor reuse
that helper.

## Chosen API

Add one crate-private method:

```rust
pub(crate) fn try_get_keys(&self) -> Result<BTreeSet<Vec<u8>>>;
```

Crate visibility is sufficient for the dependent stream-filter migration. No
public API is added by this issue.

## Behavior and data flow

`try_get_keys()` performs these operations in order:

1. Resolve the holder through `try_as_dictionary()`.
2. If the resolved holder is not a dictionary, return an empty `BTreeSet`.
3. Snapshot the dictionary's `(key, child_handle)` entries and release any
   container borrow.
4. Visit every child without inspecting the key name.
5. Call `try_is_null()` on each child. Omit the key when it resolves to null;
   otherwise insert the key into the result.
6. Return the `BTreeSet`, whose bytewise ordering provides qpdf's set ordering.

The snapshot-before-resolution rule is required even though
`try_as_dictionary()` currently returns cloned data: it states the durable
re-entrancy boundary. A resolver may enter object code while resolving a
child, so no borrow of the dictionary container may remain live across that
call.

## Null and error semantics

The existing canonical resolver determines what a child resolves to.
`try_get_keys()` adds no resolver fallback or error translation:

- direct null is omitted;
- an indirect reference whose terminal value is null is omitted;
- a missing or dangling indirect reference that the canonical resolver
  materializes as null is omitted;
- a reference loop handled by the canonical resolver's established
  loop-to-missing/null behavior is omitted;
- every non-null terminal value is retained; and
- holder-resolution and child-resolution errors propagate unchanged through
  `Result`.

In particular, the enumeration must not silently turn an `ErrorResolver`
failure into an absent key or empty set.

## Tests

Add focused `object_handle::identity_tests` coverage using existing resolver
fixtures and call recording:

1. a mixed dictionary excludes direct null, indirect null, and dangling/null
   terminals, retains non-null values, and returns keys in sorted byte order;
2. a resolver-bearing dictionary holder is lazily resolved before enumeration;
3. a non-dictionary holder returns an empty set;
4. a child resolver error is returned unchanged;
5. a holder resolver error is returned unchanged;
6. an unknown/non-filter key whose value is indirect is still resolved, proven
   through the resolver call log; and
7. the existing canonical resolver loop-to-missing test remains part of the
   focused regression set, tying cycle termination to the primitive used here.

The RED phase must add the new behavioral tests before the method exists and
observe the expected compile/test failure. The GREEN phase adds only the
minimal method needed to pass them.

## Documentation and verification

Update only the `ObjectHandle` entry in `docs/qpdf-correspondence.md` to cite
the qpdf dictionary-key enumeration responsibility. The downstream
stream-filter row and consumer code remain unchanged.

Verification after implementation:

- focused ObjectHandle tests, including the existing resolver-cycle test;
- `cargo fmt -- --check`;
- `cargo test` for the complete workspace; and
- fresh changed-line coverage showing 100% coverage for changed executable
  lines.

## Non-goals

This issue does not:

- migrate `stream_filter`, `filters`, `/Filter`, or `/DecodeParms` consumers;
- add any filter-specific key names or parameter rules to `ObjectHandle`;
- change the legacy `Object` reader path;
- change raw dictionary storage or enumeration;
- change `try_has_key`, `try_get_key`, `replace_key`, or
  `visible_dict_entries()`;
- introduce qpdf's public non-dictionary type-warning surface; or
- add a public flpdf API.

Consumer integration remains owned by `flpdf-h8mv`. This issue produces only
the qpdf-shaped primitive that dependency can call.
