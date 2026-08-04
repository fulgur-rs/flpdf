# Pl_MD5 pipeline design

**Issue:** `flpdf-fitw`

**Date:** 2026-08-04

**Oracle:** qpdf 11.9.0 (`v11.9.0`, commit `3b97c9bd`)

**Oracle source:** `scripts/fetch-qpdf-source.sh --print-path`

**Parent:** `flpdf-25kg.4` (stream, filter, and crypto convergence)

## Problem

flpdf uses the RustCrypto `md-5` crate for one-shot MD5 calculations, but it has no pipeline
stage corresponding to qpdf's `Pl_MD5`. As a result, a caller cannot calculate a digest over
bytes passing through a pipeline while forwarding those same bytes unchanged to the next stage.

The missing component is observable beyond the digest bytes. qpdf's `Pl_MD5` is reusable, may be
disabled as a pass-through, may retain one digest across downstream `finish()` boundaries, and
has specific state and error ordering around downstream failures. Implementing only a one-shot
wrapper would not complete that component contract.

The first production consumer is embedded-file checksum generation. qpdf
`QPDFEFStreamObjectHelper::newFromStream` pipes decoded payload bytes through
`Pl_Count -> Pl_MD5 -> Pl_Discard` and stores the binary form of the hexadecimal digest in
`/Params /CheckSum`. flpdf currently calls RustCrypto directly in `md5_checksum`.

## Goals

1. Add a crate-private `PlMd5` stage that implements the existing public `Pipeline` trait.
2. Match qpdf 11.9.0 `Pl_MD5` lifecycle, state transitions, error category, and byte forwarding.
3. Keep RustCrypto as the MD5 primitive instead of reimplementing MD5.
4. Migrate embedded-file `/Params /CheckSum` generation to the new pipeline stage.
5. Remove the direct RustCrypto MD5 route from `filespec_helper.rs`.
6. Update the qpdf correspondence ledger and retain source citations in module documentation.

## Non-goals

- Migrating the deterministic writer `/ID` path in this issue.
- Implementing `Pl_SHA2`, `Pl_AES_PDF`, `Pl_Discard`, or a generic digest-stage abstraction.
- Exposing `PlMd5` as a public flpdf API.
- Reimplementing the MD5 algorithm or qpdf's crypto-provider abstraction.
- Changing embedded-file PDF structure, compression, dates, filenames, or extraction behavior.

## Oracle evidence and responsibility boundary

The pinned qpdf source settles the component contract without a real-PDF probe:

- `libqpdf/qpdf/Pl_MD5.hh:4-8` defines unchanged forwarding and reuse after `finish()`.
- `libqpdf/Pl_MD5.cc:14-35` resets on the first enabled write of a digest, updates the digest
  before calling the downstream `write`, and remains a pass-through while disabled.
- `libqpdf/Pl_MD5.cc:38-43` calls downstream `finish()` first and resets digest progress only
  after that call succeeds and persistence is disabled.
- `libqpdf/Pl_MD5.cc:47-65` defines enablement, persistence, the disabled-digest logic error,
  and digest retrieval ending the current digest.
- `libtests/md5.cc:41-76` exercises ordinary reuse, persistent accumulation across finishes,
  partial digest retrieval, and repeated retrieval without intervening writes.
- `libqpdf/QPDFEFStreamObjectHelper.cc:131-147` owns the embedded-file size/checksum pipeline and
  converts `Pl_MD5::getHexDigest()` back into binary bytes for `/CheckSum`.

This issue owns the `Pl_MD5` adapter and the exact embedded-file consumer cutover. The underlying
MD5 primitive remains in RustCrypto. The writer remains outside this slice even though qpdf also
uses `Pl_MD5` there; the issue acceptance criteria do not require all consumers to migrate, and
the embedded-file route provides a complete qpdf-shaped production consumer.

## Component contract

`pipeline/md5.rs` will define a crate-private `PlMd5<'a>` holding an identifier, a borrowed
downstream `&'a mut dyn Pipeline`, an incremental RustCrypto MD5 state, and the three qpdf state
flags: digest in progress, enabled, and persistent across finish.

The observable rules are:

- Construction starts enabled, non-persistent, and without an in-progress digest.
- The first enabled `write`, including an empty write, starts a new digest.
- Every enabled write updates the digest before forwarding the identical slice downstream.
- A disabled write performs no digest work but still forwards the identical slice downstream.
- `finish()` first calls downstream `finish()`.
- After a successful downstream finish, non-persistent mode ends the current digest; persistent
  mode leaves it open so later writes continue it.
- If downstream finish fails, digest progress is unchanged because qpdf never reaches its state
  reset.
- `get_hex_digest()` returns 32 lowercase hexadecimal ASCII characters, ends the current digest,
  and may be called repeatedly with no intervening write to return the same value.
- Digest retrieval while disabled returns `PipelineError::Logic` with qpdf's exact message:
  `digest requested for a disabled MD5 Pipeline`.
- A write after successful finish or digest retrieval starts a new digest unless persistence
  kept the digest open across finish.
- Downstream errors propagate without wrapping or category conversion.

The Rust type system supplies qpdf's required non-null downstream relationship through the
borrowed reference. No owned pipeline graph, compatibility adapter, or legacy route is added.

> **[provisional — settled by TDD, not by this document]**
>
> *(implementation-detail sketch)*
>
> The RustCrypto state may be reset on the first enabled write and cloned for non-consuming
> finalization. This can reproduce qpdf's repeated digest retrieval while ensuring the next
> write observes `in_progress == false` and resets the state. Input may be presented to
> RustCrypto in qpdf's at-most-`1 << 30` byte chunks, although this provider boundary is only
> observable for extremely large slices.
>
> **[/provisional]**

## Embedded-file consumer cutover

`filespec_helper::md5_checksum` remains a public, infallible helper returning the same 16 binary
bytes. Internally it will assemble `PlMd5 -> discard`, write the entire raw payload, finish the
stage, obtain the lowercase hexadecimal digest, and decode it back to 16 bytes. This mirrors
qpdf's conversion boundary and preserves the existing public API.

`filespec_helper.rs` defines a private checksum discard sink that cannot fail; this does not
claim or implement the separate qpdf `Pl_Discard` component. Any `PipelineResult` handling in
this infallible helper must be justified as an internal invariant; it must not widen the public
API or introduce an independent fallback hash route.

Because all embedded-file builders already call `md5_checksum`, changing that single route
migrates `/Params /CheckSum` production without altering their object-graph responsibilities.
The direct `md5::{Digest, Md5}` import is removed from `filespec_helper.rs`.

## Error handling

`PlMd5` itself introduces no runtime hash errors. It returns:

- `PipelineError::Logic` only for digest retrieval while disabled;
- the downstream error unchanged from `write()` or `finish()`.

Hash state changes that precede a downstream write failure remain visible to later digest
retrieval, matching qpdf's call order. Hash state reset after finish occurs only when downstream
finish succeeds. The production discard path is infallible, so embedded-file checksum generation
retains its current infallible API.

## Testing strategy

Development follows RED -> GREEN TDD. Focused `pipeline::md5` unit tests cover:

1. known MD5 output plus byte-for-byte downstream pass-through;
2. split writes matching a single write;
3. ordinary reuse after finish;
4. persistence across multiple finish boundaries;
5. disabled pass-through and exact disabled-digest logic error;
6. repeated digest retrieval and reset on a later write;
7. digest update before a downstream write failure;
8. unchanged progress after a downstream finish failure;
9. empty input and empty writes.

The production regression uses the existing embedded-file checksum fixtures and adds or tightens
an assertion only if needed to prove the migrated route's output. Repository verification runs:

```text
cargo test -p flpdf pipeline::md5::tests
cargo test -p flpdf --test filespec_helper_tests
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf
cargo test
```

Fresh LCOV and the repository patch-coverage gate must show 100% coverage for changed executable
lines. The pinned qpdf tree is resolved again before final source citations are reported. Since
the source and qpdf's own component test fully specify this non-PDF byte-stage behavior, no
real-PDF probe is required.

## Files

- Create `crates/flpdf/src/pipeline/md5.rs` for the qpdf-shaped stage and focused tests.
- Modify `crates/flpdf/src/pipeline.rs` to register the crate-private module.
- Modify `crates/flpdf/src/filespec_helper.rs` to migrate checksum generation.
- Modify `docs/qpdf-correspondence.md` to mark `Pl_MD5` implemented and name its production
  consumer.
- Modify `crates/flpdf/tests/filespec_helper_tests.rs` only if existing assertions do not fully
  cover the production checksum result.

## Acceptance criteria

1. `PlMd5` exists under `crates/flpdf/src/pipeline/` and implements `Pipeline`.
2. Its complete qpdf 11.9.0 enable, persist, reuse, digest, forwarding, and error-order contract
   is regression-tested.
3. Its module documentation cites pinned `Pl_MD5.cc` and `Pl_MD5.hh` source locations.
4. Embedded-file `/Params /CheckSum` generation uses `PlMd5` and no longer imports RustCrypto MD5
   directly in `filespec_helper.rs`.
5. Existing public checksum bytes and embedded-file behavior remain unchanged.
6. The deterministic writer `/ID` route remains unchanged and is explicitly outside this slice.
7. Focused tests, workspace formatting, full-feature workspace clippy, crate/workspace tests, and
   fresh 100% changed-line coverage pass.
8. `docs/qpdf-correspondence.md` records the implemented component and consumer truthfully.
