# qpdf Stateful RC4 Core Design

**Issue:** `flpdf-qynx.2.1`<br>
**Date:** 2026-07-28<br>
**Oracle:** qpdf 11.9.0 (`v11.9.0`, commit `3b97c9bd`)<br>
**Oracle source:** `scripts/fetch-qpdf-source.sh --print-path`

## Problem

flpdf currently implements RC4 as a one-shot `security::primitives::rc4`
function. The function performs key scheduling and applies the complete
keystream in one call. That is sufficient for current PDF encryption
consumers, but it does not represent qpdf's component boundary:

- qpdf `RC4` / `RC4_native` retains `state`, `x`, and `y` across repeated
  `process` calls;
- explicit runtime key lengths are not restricted to PDF's usual 5–16-byte
  range;
- `key_len = -1` selects NUL-terminated key input;
- input and output may be separate or identical.

The external Rust `rc4` 0.1 crate uses compile-time key-size types and documents
a 1–256-byte key domain. It cannot express the full qpdf component contract.
The dependency is declared in the workspace and `flpdf` manifests but has no
production callsite; the current one-shot implementation is already
hand-written.

The existing Phase 2 foundation spec proposed retaining a one-shot
compatibility wrapper. The clarified completion rule for this issue is
stronger: migrate every production consumer to the new component API and
delete the old function, imports, and callsites.

## Goals

1. Mirror the qpdf 11.9.0 `RC4.cc` and `RC4_native.cc` state-machine contract
   in a dedicated `security/rc4.rs` module.
2. Route every production RC4 consumer through that component directly.
3. Delete `security::primitives::rc4`, its inline KSA/PRGA implementation, and
   all old imports and callsites.
4. Verify behavior against a probe compiled from the pinned qpdf 11.9.0
   source.
5. Remove the external `rc4` crate dependency when the final repository-wide
   callsite inventory confirms that it is unused.

## Non-goals

- `Pl_RC4` and Pipeline integration; those belong to `flpdf-qynx.2.2`.
- The `Pl_RC4` default 65,536-byte chunk boundary.
- A qpdf-style `QPDFCryptoProvider` abstraction.
- Changes to PDF key derivation, weak-crypto policy, encryption dictionary
  handling, or writer/reader behavior.
- Public exposure of RC4. The component remains `pub(crate)` under the
  crate-private `security` module.

## Component boundary

Create `crates/flpdf/src/security/rc4.rs` with this crate-private API:

```rust
pub(crate) struct Rc4 {
    state: [u8; 256],
    x: u8,
    y: u8,
}

impl Rc4 {
    pub(crate) fn new(key: &[u8]) -> Result<Self, PrimitiveError>;
    pub(crate) fn from_c_str(key: &CStr) -> Result<Self, PrimitiveError>;
    pub(crate) fn process(&mut self, input: &[u8]) -> Vec<u8>;
    pub(crate) fn process_in_place(&mut self, data: &mut [u8]);
}
```

`Rc4::new` uses the entire slice as the explicit qpdf `key_len`. It accepts
all non-empty lengths, including lengths greater than 256. As in
`RC4_native.cc`, key scheduling performs exactly 256 iterations; therefore
bytes after the first 256 do not affect the initialized state.

`Rc4::from_c_str` represents qpdf's `key_len = -1` mode safely. `CStr`
provides the first-NUL boundary without reproducing C pointer scanning.
An empty explicit key or empty C string returns
`PrimitiveError::InvalidLength`, preserving flpdf's current safe handling for
an input outside qpdf's valid operational domain.

`process` allocates an output vector and advances the same state as
`process_in_place`. Empty input returns an empty vector or leaves the supplied
slice unchanged without advancing the state.

The module header is:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/RC4.cc and libqpdf/RC4_native.cc.
```

The implementation uses qpdf's KSA and PRGA ordering directly. It does not use
the external Rust `rc4` crate and does not introduce a provider trait.

## Consumer cutover

Before editing production code, enumerate:

1. every RC4 definition and import;
2. every production callsite;
3. every test-only callsite;
4. workspace and crate manifest dependencies.

The current production path is concentrated in
`crates/flpdf/src/security/standard.rs`, which imports
`security::primitives::rc4` and invokes it for password validation, encryption
dictionary construction, string encryption/decryption, and stream
encryption/decryption.

Each one-shot consumer changes from:

```rust
rc4(key, data)?;
```

to:

```rust
let mut cipher = Rc4::new(key)?;
cipher.process_in_place(data);
```

Repeated PDF Algorithm 3/6/7 passes continue to construct a fresh `Rc4` for
each distinct key, matching the existing algorithm and qpdf behavior. They do
not reuse state between different passes. Stateful reuse is exercised by the
component tests and becomes the primitive used by the later `PlRc4` adapter.

Test helpers in `filters.rs` and `security/standard.rs`, plus the integration
helper in `crates/flpdf/tests/reader_tests.rs`, are updated to the new component
API where they directly generate RC4 ciphertext. Production reader and writer
callsites were unchanged. Public `EncryptParams::rc4` constructors and RC4 enum
variants are configuration APIs, not cipher implementations, and remain
unchanged.

After migration, repository searches must find none of the following:

- a `security::primitives::rc4` definition or import;
- calls to the deleted `rc4(key, data)` function;
- an external `rc4` crate use;
- inline duplicate KSA/PRGA outside `security/rc4.rs`.

No compatibility wrapper remains.

## Error handling and security boundary

The component performs no policy decisions. Existing reader and CLI layers
continue to enforce weak-crypto opt-in rules before RC4-backed document
processing. Existing object-key derivation continues to validate PDF key
lengths before constructing `Rc4`.

`Rc4::new` returns `PrimitiveError::InvalidLength` only for an empty key.
`process` and `process_in_place` cannot fail after successful construction.
The existing `From<PrimitiveError> for Error` conversion remains the
higher-level bridge.

RC4 is retained only for compatibility with legacy PDFs. Documentation keeps
the existing weak-cipher warning and does not present the crate-private type as
a recommended cryptographic API.

## Oracle and tests

### Unit tests

Tests in `security/rc4.rs` cover:

- RFC 6229 known-answer vectors;
- explicit key lengths 1, 5, 16, 256, and greater than 256;
- equivalence of one `process` call and multiple calls over the same bytes;
- equivalence of `process` and `process_in_place`;
- first-NUL behavior through `from_c_str`;
- empty input without state advancement;
- empty-key rejection without processing data.

The key-length-greater-than-256 test also proves qpdf's exact 256-iteration
KSA consequence: changing only bytes after index 255 does not change output.

### qpdf 11.9.0 differential

Add `tests/oracle/qpdf_rc4_probe.cc`. The probe accepts deterministic
hex-encoded key/input cases plus explicit or NUL-terminated key mode and emits
hex output for one-shot, split-call, and in-place variants. It exercises
qpdf's `RC4_native` component directly.

Add `scripts/qpdf-rc4-diff.sh`. The script:

1. resolves the pinned source with `scripts/fetch-qpdf-source.sh --print-path`;
2. verifies the pinned tracked source is clean and at commit `3b97c9bd`;
3. compiles the probe and qpdf `RC4_native.cc` into a private `mktemp`
   directory, never into the source tree;
4. runs the ignored Rust differential test with the probe path in
   `QPDF_RC4_PROBE`;
5. verifies the pinned source remains clean;
6. removes the temporary directory on exit.

The Rust differential test enumerates all approved key, state, input/output,
and empty-input cases and compares exact output bytes.

### Consumer regressions

Run the existing security, reader, writer, filter, and CLI RC4 tests after
cutover. These prove that the component replacement preserves PDF behavior
while unit and oracle tests prove the newly exposed stateful contract.

## Dependency and documentation updates

Remove `rc4 = "0.1"` from workspace dependencies and `rc4.workspace = true`
from `crates/flpdf/Cargo.toml`, then update `Cargo.lock` mechanically.

Update `security/primitives.rs` documentation so it covers AES, MD5, and SHA2,
not RC4. Update `docs/qpdf-correspondence.md` by separating RC4 from the
Rust-crypto substitution rows and marking `security/rc4.rs` as the completed
`RC4.cc` / `RC4_native.cc` mirror. Regenerate
`docs/qpdf-module-doc-index.md` through `scripts/qpdf-module-docs.py`.

The final documentation must not mark `Pl_RC4` complete; that remains tracked
by `flpdf-qynx.2.2`.

## Completion gates

- focused RED-to-GREEN RC4 unit tests;
- `scripts/qpdf-rc4-diff.sh`;
- focused security, reader, writer, filter, and CLI RC4 regressions;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- strict private-item rustdoc;
- `cargo test`;
- qpdf module-correspondence generator and contract tests;
- `git diff --check`;
- fresh changed-line coverage of 100%.

The issue is complete only when the new component owns the sole RC4
state-machine implementation, every consumer has migrated, the old route and
dependency are absent, all gates pass, and the Beads/Git state is pushed.
