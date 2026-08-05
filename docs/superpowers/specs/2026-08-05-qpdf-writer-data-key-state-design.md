# QPDFWriter Data-Key State Design

## Goal

Port qpdf 11.9.0's `QPDFWriter::setDataKey` responsibility as a writer-owned
current-data-key state that later string and stream emission primitives can
consume without mutating a legacy `Object` tree.

## Oracle boundary

The source of truth is:

- `include/qpdf/QPDFWriter.hh:641-663` for writer encryption fields;
- `libqpdf/QPDFWriter.cc:842-847` for `setDataKey`;
- `libqpdf/QPDFWriter.cc:1680-1715,1761-1796` for top-level versus object-stream
  member lifecycle; and
- `libqpdf/QPDF_encryption.cc:325-356` for V<5 Algorithm 3.1 and V>=5 direct-key
  behavior.

String encryption, stream encryption, metadata exemption, Encrypt dictionary
construction, production writer cutover, and linearization layout remain out
of scope.

## Considered approaches

1. Add a focused `writer::encryption_state` module that mirrors the qpdf writer
   fields and lifecycle. This is selected because it preserves the qpdf
   responsibility boundary and gives later consumers one shared current key.
2. Extend legacy `writer::EncryptionContext`. Rejected because that type also
   owns Encrypt dictionary, metadata, IV, and trailer concerns, which would
   keep the current-key primitive coupled to the legacy mutation route.
3. Add a stateless `derive_key` helper. Rejected because `per_object_key`
   already supplies derivation and a stateless helper cannot represent the
   set/use/clear lifecycle required by qpdf consumers.

## Architecture

Create `crates/flpdf/src/writer/encryption_state.rs` with
`WriterEncryptionState`. It stores qpdf's `encrypted`, `encryption_key`,
`encrypt_use_aes`, `encryption_v`, `encryption_r`, and `cur_data_key` meanings.
Absence of a current key is represented by `Option<Vec<u8>>`, not an empty-byte
sentinel.

`set_data_key(emitted_object_number)` always uses generation 0. For V>=5 it
copies the file key directly. For V<5 it delegates to the existing
`security::standard::per_object_key`, choosing its AES salt from
`encrypt_use_aes`. It does not invent key-length or V/R validation that qpdf's
`setDataKey` does not perform.

`with_object_data_key(emitted_object_number, object_stream_index, emit)` mirrors
`QPDFWriter::writeObject`:

- `None` means a top-level indirect object: set the key, invoke `emit`, then
  clear the key;
- `Some(index)` means an ObjStm member: invoke `emit` without setting a member
  key.

The callback receives `&mut WriterEncryptionState`, allowing later emission
primitives to inspect the shared current key without adding an encrypted-string
tag or materializing an `Object` tree.

## Error handling

The callback's `Result` is returned unchanged. Rust clears the current key after
both `Ok` and `Err`; qpdf's explicit clear is reached only after successful
unparse, but `QPDFWriter::write()` is single-use after failure. This is an
output-neutral Rust state-safety substitution and must be recorded both in the
module documentation and `docs/qpdf-correspondence.md`.

Invalid key lengths are retained by this primitive and left for the actual
RC4/AES consumer to reject, matching qpdf's responsibility boundary. The code
adds no panic, plaintext fallback, or local validation branch.

## Testing

Unit tests in the new module cover:

- RC4 Algorithm 3.1 from an emitted number and generation 0;
- AES-128 Algorithm 3.1 with `sAlT`;
- V5 AES-256 direct file key;
- disabled writer state while retaining qpdf's set/use/clear order;
- source identity differing from the emitted number;
- successful and failed callback cleanup;
- ObjStm member omission; and
- invalid direct-key length being deferred rather than rejected or panicked.

Focused tests are followed by formatting, workspace clippy/tests, module-doc
checks, and fresh changed executable-line coverage at 100%.
