# ObjectHandle-native stream encoding bridge design

**Issue:** `flpdf-egzr.3.2.14`

**Goal:** Add a crate-private stream-encoding entry point that reads `/Filter`
and `/DecodeParms` through `ObjectHandle` resolving accessors, then executes
the exact same encoding engine as the existing `Dictionary` entry point.

## Scope

This slice adds one primitive:

```rust
pub(crate) fn encode_stream_data_from_handle(
    stream_dict: &ObjectHandle,
    stream_data: &[u8],
) -> Result<Vec<u8>>
```

No writer, reader, page, CLI, or other consumer is migrated here. The function
may land with no production caller; `flpdf-egzr.3.2.5` owns writer migration.
The legacy public `encode_stream_data(&Dictionary, &[u8])` remains unchanged at
its API boundary and must produce identical bytes and errors.

The production source diff is confined to `crates/flpdf/src/filters.rs` and, if
a shape-reader test must live beside its implementation,
`crates/flpdf/src/stream_filter.rs`. Documentation is not a consumer change.

## Oracle and responsibility boundary

Pinned qpdf 11.9.0 is the semantic authority.

- `libqpdf/QPDF_Stream.cc:386-409` reads `/Filter` with
  `stream_dict.getKey`, accepts null, name, or an array of names, and rejects
  other shapes.
- `libqpdf/QPDF_Stream.cc:419-435` expands abbreviations and requires a known
  filter factory before processing decode parameters.
- `libqpdf/QPDF_Stream.cc:441-480` reads `/DecodeParms` with `getKey`, expands
  array/scalar forms, validates the length, and lets each filter consume its
  parameters.
- `libqpdf/QPDF_Stream.cc:529-568` constructs the stream pipeline in reverse
  order and adds `Pl_Flate` with `a_deflate` when compression is requested.
- `libqpdf/Pl_PNGFilter.cc:53-87,215-228` defines predictor encoding and its
  fixed Up-row output.
- `libqpdf/Pl_RunLength.cc:24-64,105-145` defines RunLength encoding packets
  and the final EOD byte.

The new API changes only the shape used to read filter specifications. It does
not change any codec semantics. ASCII85 and ASCIIHex encoding remain the
existing documented flpdf-specific writer facilities because qpdf 11.9.0 has
decoders but no matching encoder components.

`ObjectHandle::try_get_key` and the `try_*` child accessors are required. They
preserve qpdf's automatic dereference behavior for the dictionary holder,
indirect `/Filter`, indirect `/DecodeParms`, array items, and parameter values.
Materializing a handle into the legacy `Object` graph is outside this boundary.

## Considered approaches

### A. Two shape readers, one encode engine — selected

The legacy reader continues to call `decode_filter_specs_from_object`; the new
entry point calls `decode_filter_specs_from_handle`. Both produce
`Vec<FilterSpec>` and pass it to one private encode executor. This is the
smallest change, keeps all codec behavior in one copy, and mirrors the existing
decode-side Object/ObjectHandle seam.

### B. Materialize the handle and call the Dictionary API — rejected

Materialization recreates the bridge that the ObjectHandle cutover is removing.
It also collapses lazy resolution and provenance into a cloned raw `Object`, so
unresolved and indirect-child behavior would no longer match qpdf accessors.

### C. Duplicate the encoding loop for ObjectHandle — rejected

This avoids a private helper but creates two reverse-chain, predictor, Flate,
and error paths that can drift. There is no semantic difference below
`FilterSpec` that justifies duplication.

## Detailed design

`encode_stream_data` keeps reading the two optional `Object` values from its
`Dictionary`. Its private path reduces them with
`decode_filter_specs_from_object` and then calls a new private executor that
takes `Vec<FilterSpec>` plus the raw bytes.

`encode_stream_data_from_handle` performs these steps in order:

1. `stream_dict.try_get_key(b"Filter")`.
2. `stream_dict.try_get_key(b"DecodeParms")`.
3. `decode_filter_specs_from_handle(&filter, &decode_params, None)`; encoding
   remains uncapped, matching the existing Dictionary path.
4. Call the same private `Vec<FilterSpec>` executor as the Dictionary path.

The shared executor preserves the existing reverse iteration, predictor
application, Flate compressor, ASCII encoders, RunLength encoder, and error
mapping without modification.

No trait, adapter type, materialization helper, resolver fallback, or new
public abstraction is introduced. The existing
`decode_filter_specs_from_handle` is reused directly.

## Error behavior

- A missing or null `/Filter` returns the input bytes unchanged.
- Unknown filters, malformed filter shapes, parameter length mismatches,
  unsupported predictors, invalid geometry, and unsupported encoders return
  the same `Error::Unsupported` result as the Dictionary path.
- An unresolved indirect handle with a live resolver is resolved lazily.
- A still-unresolved handle whose resolver has been dropped returns the
  existing `Error::Internal("object N G belongs to a dropped PDF")`; it is not
  treated as a missing key.
- Errors are propagated unchanged. The bridge adds no fallback or diagnostic
  translation.

## Test design

All production changes follow RED to GREEN. Before adding the entry point, add
tests that fail because `encode_stream_data_from_handle` does not exist.

The equivalence table supplies the same logical dictionary through both
shapes and compares `Result<Vec<u8>>` for:

- missing `/Filter`;
- canonical and abbreviated Flate names;
- ASCII85 and ASCII85 abbreviation;
- ASCIIHex and ASCIIHex abbreviation;
- RunLength and RunLength abbreviation;
- a multi-filter array, proving reverse encoding order;
- Flate with PNG predictors 10 through 15 and explicit geometry;
- malformed/unsupported rows already rejected by the Dictionary encoder.

Independent assertions prevent equivalence from hiding shared defects:

- missing `/Filter` must equal the original bytes;
- RunLength encoding of `b"AA"` must equal `[0xff, b'A', 0x80]`;
- the multi-filter result must decode back to the original bytes.

Resolver-specific tests cover:

- indirect `/Filter` and indirect `/DecodeParms` values in a minimal parsed
  PDF, including a parameter value reached through its document resolver;
- an unresolved indirect stream dictionary with a live test resolver;
- an unresolved indirect holder whose resolver is dropped, asserting the
  exact `Error::Internal` text.

Verification records:

- focused unit-test commands and pass counts;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- `cargo test -p flpdf --lib` and `cargo test`;
- a fresh LCOV run and `scripts/patch-coverage.sh --base main --lcov ...`
  showing 100% changed executable-line coverage;
- the existing byte-comparison suite before and after the change, with no
  output regression;
- `git diff --name-only main...HEAD -- crates/flpdf/src`, whose output may
  contain only `filters.rs` and `stream_filter.rs`.

## Non-goals

- Migrating any existing consumer to the new function.
- Changing codec algorithms, filter ordering, aliases, predictors, limits, or
  diagnostics.
- Adding resolver plumbing owned by `flpdf-25kg.3.5`.
- Removing the Dictionary entry point or the legacy Object shape reader.
- Closing pre-existing qpdf parity gaps outside the handle-reading seam.

