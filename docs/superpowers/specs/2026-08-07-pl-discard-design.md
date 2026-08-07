# Public Discard Pipeline Design

## Goal

Port qpdf 11.9.0 `Pl_Discard` as the public
`flpdf::pipeline::Discard` terminal stage and replace the EmbeddedFile checksum
path's consumer-local discard sink without changing checksum output.

## Oracle and responsibility boundary

The authoritative behavior is defined by pinned qpdf 11.9.0:

- `include/qpdf/Pl_Discard.hh:22-38` declares an end-of-line Pipeline with no
  successor and explicitly permits reuse after `finish()`;
- `libqpdf/Pl_Discard.cc:5-22` fixes the identifier to `discard` and makes both
  `write()` and `finish()` no-ops;
- `libqpdf/QPDFEFStreamObjectHelper.cc:131-147` uses `Pl_Discard` as the
  terminal in `Pl_Count -> Pl_MD5 -> Pl_Discard`.

This slice owns only the `Pl_Discard` component and the existing checksum
path's terminal replacement. The complete EmbeddedFile `newFromStream`
finalization, provider/path inputs, `Pl_Count` integration, success-only
metadata publication, and removal of the direct `md5_checksum` helper belong
to `flpdf-25kg.4.4`.

## Public API and module layout

Create `crates/flpdf/src/pipeline/discard.rs` with a zero-sized public type:

```rust
pub struct Discard;
```

`Discard` implements the existing public `Pipeline` trait:

- `identifier()` always returns `"discard"`;
- `write(&mut self, _: &[u8])` returns `Ok(())` without retaining or forwarding
  bytes;
- `finish(&mut self)` returns `Ok(())` without changing state;
- any sequence of empty writes, non-empty writes, repeated finishes, and writes
  after finish remains valid.

Expose the component as `flpdf::pipeline::Discard` with a private module plus
a public re-export from `pipeline.rs`. No constructor is necessary because the
type is unit-like, matching the absence of configuration or downstream state.

## Production cutover

Delete `filespec_helper.rs::ChecksumDiscard` and construct `Discard` in
`md5_checksum`. Keep the existing `PlMd5` write, finish, hexadecimal digest,
and binary decode flow unchanged. This makes the current production checksum
path consume the canonical terminal while leaving the larger EmbeddedFile
provider migration for `flpdf-25kg.4.4`.

No compatibility alias named `PlDiscard`, generic null-sink abstraction, local
wrapper, or callback adapter is added.

## Error and lifecycle behavior

`Discard` has no error source and returns successful `PipelineResult` values
directly. It does not introduce state, panic paths, sentinel values, buffering,
allocation, downstream ownership, or implicit finalization. Reuse follows
qpdf's component-specific contract rather than a global Pipeline state machine.

## Test strategy

Use RED to GREEN TDD in the new module:

1. Start with a compile-failing test that imports the not-yet-existing public
   `flpdf::pipeline::Discard` surface.
2. Pin dynamic `Pipeline::identifier()` dispatch to `discard`.
3. Verify empty and non-empty writes produce successful results.
4. Verify repeated `finish()` calls and a later write remain successful.
5. Keep the existing independent `md5_checksum` known-value and EmbeddedFile
   `/CheckSum` tests green after replacing the local sink.

Verification expands from the focused Pipeline and Filespec tests to formatting,
workspace denied-warning clippy, workspace tests, and fresh changed-line
coverage of 100%.

## Non-goals

- Implementing or changing `Pl_Count` or `Pl_MD5`.
- Removing or changing the public `md5_checksum` helper.
- Adding provider-backed streams or `pipeStreamData`.
- Publishing `/Size` or `/CheckSum` through a new finalization path.
- Migrating QPDFLogger or writer-output MD5 consumers.
