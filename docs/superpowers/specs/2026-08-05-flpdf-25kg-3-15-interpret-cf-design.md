# ObjectHandle-native crypt-filter selector design

**Issue:** `flpdf-25kg.3.15`

**Goal:** Add the qpdf-shaped `ObjectHandle` entry point for crypt-filter
selection while keeping value selection shared with the existing materialized
`Object` path.

## Scope

This slice adds one crate-internal primitive in `crates/flpdf/src/reader.rs`:
an `ObjectHandle`-native selector that lazily resolves a crypt-filter value and
returns the corresponding `EncryptionMode` from the active
`EncryptionState`.

The existing materialized selector remains available to current initialization
and legacy callers. Both entry points delegate to one value-selection core, so
the lookup order has a single implementation. No production consumer is
migrated in this issue; `flpdf-25kg.3.12` owns the `decryptStream` cutover.

## Oracle and responsibility boundary

Pinned qpdf 11.9.0 is the semantic authority.

- `include/qpdf/QPDF.hh:1122-1127` declares
  `QPDF::interpretCF(std::shared_ptr<EncryptionParameters>, QPDFObjectHandle)`.
- `libqpdf/QPDF_encryption.cc:700-711` checks whether the handle is a name,
  looks in `EncryptionParameters::crypt_filters` first, then recognizes the
  built-in `/Identity`, and otherwise returns `e_unknown`.
- `libqpdf/QPDF_encryption.cc:712-715` maps every non-name value to `e_none`,
  qpdf's identity/no-encryption mode.
- qpdf's `QPDFObjectHandle::isName` and `getName` automatically dereference
  indirect handles. In flpdf, `ObjectHandle::try_as_name` owns this lazy
  dereference and returns resolver failures rather than converting them to a
  value-selection result.

The new entry point corresponds only to `QPDF::interpretCF`. It does not own
stream decryption, warnings, key derivation, cipher construction, or pipeline
insertion.

## Considered approaches

### A. Shared value core with two shape adapters — selected

A private core receives an optional name byte slice plus the crypt-filter map.
The existing materialized `Object` selector extracts a name without changing
its current caller contract. The new `ObjectHandle` selector calls
`try_as_name`, propagates any error, and passes the resolved name to the same
core.

This keeps qpdf's branch order in one place, preserves the legacy boundary,
and makes lazy-resolution ownership explicit.

### B. Add an `EncryptionState` method — rejected

A method would work, but it couples handle resolution to the state type and is
less direct than qpdf's selector-shaped free function. There is no state
mutation or invariant that requires a method receiver.

### C. Convert the handle to a materialized `Object` — rejected

Materialization would bypass the live-handle seam this issue exists to add. It
would also obscure resolver lifetime failures and make a future consumer rely
on a legacy bridge.

### D. Cut over initialization or `decryptStream` now — rejected

That expands this primitive issue into consumer migration and pipeline work.
Those responsibilities and their warning/stage-order tests belong to the
dependent `flpdf-25kg.3.12` issue.

## Detailed design

`reader.rs` gains a private value-selection core with inputs equivalent to:

```rust
fn interpret_cf_name(
    crypt_filters: &BTreeMap<Vec<u8>, EncryptionMode>,
    filter: Option<&[u8]>,
) -> EncryptionMode
```

The exact helper name may follow the surrounding naming style, but its data
contract is fixed:

1. If `filter` is present and is a key in `crypt_filters`, return the mapped
   mode. This lookup must run before the built-in identity check so an explicit
   `Identity` entry shadows the built-in value exactly as in qpdf.
2. If the name is `Identity`, return `EncryptionMode::Identity`.
3. For every other name, return `EncryptionMode::Unknown`.
4. If no name is present, return `EncryptionMode::Identity`.

The existing `interpret_cf(..., Option<&Object>)` becomes a thin adapter: it
extracts the materialized name and calls the core. Its callers and return type
do not change.

The new qpdf-shaped entry point accepts `&EncryptionState` and
`&ObjectHandle`, calls `ObjectHandle::try_as_name()?`, and wraps the core result
in the crate's `Result`. Its visibility is limited to `crate::reader`, which is
enough for the future resolver/stream consumer without exposing an API outside
the responsibility-owning module.

No sentinel name, fallback object, trait, new enum, cache, or duplicate map
lookup is introduced.

## Error behavior

- Direct name, direct non-name, and null handles produce a value immediately.
- Indirect handles are lazily dereferenced through `try_as_name`.
- A resolvable indirect name follows the same map/Identity/unknown rules as a
  direct name.
- A dropped owning document produces the existing `ObjectHandle` error.
- A live resolver that fails while resolving an indirect name returns that
  resolver error unchanged.
- Resolution errors are never converted to `Identity`, `Unknown`, or another
  selection sentinel.
- Once resolution succeeds, a resolved non-name uses qpdf's normal
  `Identity` result and is not an error.

## Test design

Implementation follows RED to GREEN. Tests are added beside the existing
`reader.rs` crypt-filter selector tests before production code.

The ObjectHandle-native tests cover:

- direct known name;
- indirect known name through a live resolver;
- an explicit `Identity` entry in `crypt_filters`, proving it shadows the
  built-in value;
- built-in `Identity` when the map has no matching entry;
- unknown name;
- direct non-name and null;
- a dropped-document indirect handle;
- an independent live-resolver failure for an indirect name.

At least one table or paired assertion passes the same logical values through
the legacy `Object` adapter and the new `ObjectHandle` adapter to prove that
the shared value core preserves current materialized behavior. Resolver-error
tests assert `Err` separately so equivalence cannot hide fallback behavior.

Documentation updates `docs/qpdf-correspondence.md` with the new
ObjectHandle-native contract and the qpdf declaration/implementation line
mapping.

Verification records:

- the focused `reader` unit tests in RED and GREEN states;
- `cargo fmt --all -- --check`;
- focused and workspace tests;
- a fresh LCOV result showing 100% changed executable-line coverage;
- a final diff check confirming no consumer cutover or unrelated production
  change.

## Non-goals

- Modifying `QPDF::decryptStream` correspondence or any stream consumer.
- Adding warnings or changing warning ownership.
- Deriving encryption keys or constructing AES/RC4 stages.
- Changing legacy resolve-time decryption.
- Migrating initialization to `ObjectHandle`.
- Removing the materialized `Object` selector.
- Exposing the new selector as a public library API.
