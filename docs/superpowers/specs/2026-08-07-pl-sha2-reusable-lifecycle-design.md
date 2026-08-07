# Pl_SHA2 Reusable Lifecycle Design

## Goal

Make `pipeline::sha2::PlSha2` lifecycle-equivalent to qpdf 11.9.0's
reusable `Pl_SHA2` stage while preserving the existing Rust safety errors for
states where qpdf dereferences an uninitialized crypto provider.

## Oracle

- `include/qpdf/Pipeline.hh:31-33` allows reusable pipeline stages.
- `libqpdf/qpdf/Pl_SHA2.hh:4-11` preserves the selected 256, 384, or 512 bit
  size and permits `write()` after `finish()`.
- `libqpdf/Pl_SHA2.cc:17-46` updates the digest before forwarding writes and
  forwards downstream `finish()` before finalizing its own digest.
- `libqpdf/sha2.c:670-673` and `libqpdf/sha2big.c:209-228` reinitialize the
  native context with the same digest size after every finalize.

## Design

Keep the selected digest size encoded by the existing `Sha2Digest` enum
variant. Replace the consuming `finalize(self)` helper with a mutable
`finalize_and_reset(&mut self)` helper that calls RustCrypto's
`Digest::finalize_reset`. This yields the completed digest and immediately
returns the same enum variant to a fresh empty cycle, matching qpdf's native
backend without a separate bit sentinel or buffered copy of the input.

`PlSha2::finish` continues to call the downstream pipeline first. If that call
fails, the local digest remains in progress and is not finalized. On success,
it finalizes and resets the selected hasher, stores the resulting digest, and
clears `in_progress`. A repeated `finish()` therefore finalizes the fresh empty
cycle and forwards downstream again. A later `write()` updates the fresh cycle
without requiring `reset_bits()`.

`reset_bits()` retains its current responsibility: reject reset while in
progress, validate 256/384/512, replace the current hasher, and clear the last
digest. The defined Rust errors for never selecting a bit size and requesting
a digest before one has been computed remain unchanged.

## Testing

- For 256, 384, and 512 bits, prove that `finish(input); write(next); finish()`
  produces the same digest as a fresh pipeline hashing `next` alone.
- For all three sizes, prove that a second `finish()` produces the empty-input
  digest and calls the downstream `finish()` twice.
- Preserve the existing vectors, chunking, empty-write, passthrough,
  downstream-error ordering, reset, and invalid-state tests.
- Correct the module documentation and `docs/qpdf-correspondence.md` entry to
  cite the native reinitialization behavior.

## Scope

This issue does not expose `PlSha2` publicly, add a crypto-provider
abstraction, or move the R5/R6 production consumer. That cutover remains in
`flpdf-qynx.9`.
