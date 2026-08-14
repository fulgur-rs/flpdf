# ObjectHandle Encryption Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an unused, qpdf-shaped ObjectHandle emission-time encryption surface that later writer consumers can use without materializing an Object tree or mutating the source document.

**Architecture:** Extend the existing ObjectHandle unparse walkers with additive callback-specific recursive walkers while retaining the current plain wrappers byte-for-byte. Add handle-aware entry points to `EncryptedStringEmitter` and a handle view of `EncryptionContext`'s `/Encrypt` snapshot; no current writer call site changes. Keep stream payload encryption as a separate pipeline operation, with the metadata exemption selected by the existing stream-dictionary options.

**Tech Stack:** Rust workspace, `ObjectHandle`/`ObjectValue`, qpdf 11.9.0 source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`, `cargo test`, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 source and observed output are authoritative; cite `QPDFWriter.cc:842-847,1490-1504,1528-1599,1761-1796,2244-2256`.
- Encryption is emission-time: do not add a mutating `ObjectHandle -> Object` bridge and do not encrypt the source graph in place.
- Existing writer consumers and output paths remain unchanged in this additive slice; the diff gate must exclude writer call-site migrations.
- `QPDFWriter` sets a per-object data key for top-level objects, does not set one for object-stream members, leaves `/Encrypt` plaintext, and exempts cleartext metadata from stream encryption.
- AES strings are serialized as hexadecimal; RC4 strings retain ordinary PDF string escaping; IV generation is performed only for AES.
- Use RED→GREEN TDD and require fresh 100% changed executable-line coverage against `origin/main`.

---

### Task 1: Pin the callback contract with failing ObjectHandle tests

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs` tests near the existing unparse tests.

**Interfaces:**
- Consumes: existing `ObjectHandle::unparse_object`, `unparse_object_qdf`, `unparse_stream_body`, and `unparse_stream_body_qdf` output.
- Produces: the required behavior for additive callback entry points:
  `unparse_object_with_string_writer`, `unparse_object_qdf_with_string_writer`,
  `unparse_stream_body_with_string_writer`, and
  `unparse_stream_body_qdf_with_string_writer`.

- [x] **Step 1: Write failing tests** for a callback that replaces direct string serialization in a nested dictionary, array, and stream dictionary while preserving indirect child references.
- [x] **Step 2: Add failing QDF and stream-body cases** covering nonzero indentation, `/Length` placement, `/Filter` refilter ordering, and a `/Sig` `/Contents` string. The signature value must bypass the encryption callback and retain qpdf's cleartext `f_hex_string | f_no_encryption` behavior (`QPDFWriter.cc:1490-1504,1567-1599`).
- [x] **Step 3: Run the focused tests**:

```bash
cargo test -p flpdf --lib object_handle::unparse_object_tests::unparse_encrypted_string_writer -- --nocapture
```

The initial RED run failed because the four callback entry points did not exist; the implementation is now GREEN.

### Task 2: Implement the ObjectHandle string-emission hook

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`.

**Interfaces:**
- Consumes: the existing compact, QDF, stream-body, and signature-dictionary walkers.
- Produces: four `pub(crate)` callback entry points accepting
  `F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>` and the same bytes/errors as the existing plain wrappers when the callback writes ordinary strings.

- [x] **Step 1: Add private callback-specific walkers** for the compact, QDF, stream-body, and signature-dictionary paths; retain the plain walkers and avoid changing their output.
- [x] **Step 2: Preserve indirect identity** by keeping the existing `object_ref()` child short circuit; only direct `ObjectValue::String` values reach the callback.
- [x] **Step 3: Make `/Sig` `/Contents` honor qpdf precedence**: both plain and callback emission keep the existing forced-hex path, and an active encryption callback is bypassed because qpdf supplies `f_hex_string | f_no_encryption` for this value.
- [x] **Step 4: Keep the old public(crate) wrappers as plain-emission adapters** and add the four callback wrappers without changing any consumer call site.
- [x] **Step 5: Run the RED tests from Task 1** and the existing unparse test group; all pass.

### Task 3: Add the handle-aware encrypted emitter and Encrypt dictionary view

**Files:**
- Modify: `crates/flpdf/src/writer/encrypted_strings.rs`.
- Modify: `crates/flpdf/src/object_handle.rs` only for conversion helpers needed to represent the existing encryption snapshot as direct handles.

**Interfaces:**
- Consumes: `EncryptionContext`, `WriterEncryptionState`, `WriteCipher`, the qpdf callback entry points, and existing legacy `write_encryption_dictionary` behavior.
- Produces: additive APIs:
  `EncryptedStringEmitter::write_handle_object`,
  `EncryptedStringEmitter::write_handle_stream_dict`,
  `EncryptionContext::encrypt_dict_handle`, and
  `write_encryption_dictionary_handle`.

- [x] **Step 1: Write failing tests** for RC4, V=4 AES-128, and V=5 AES-256 handle-object emission. Decrypt the emitted string bytes with the existing reader cipher helpers and assert the source ObjectHandle's unparse output/state is unchanged.
- [x] **Step 2: Write failing tests** for object-stream members (no per-member key), the `/Encrypt` object (plaintext), and cleartext metadata stream dictionaries (`encrypt_strings=false`).
- [x] **Step 3: Write failing tests** for V=1/V=2/V=4/V=5 encryption-context handle conversion, including `/O`, `/U`, `/OE`, `/UE`, `/Perms`, nested `/CF`, `/EncryptMetadata`, and `--copy-encryption-from` donor contexts. Compare handle-dictionary serialization with the existing qpdf-specific legacy dictionary writer.
- [x] **Step 4: Implement `write_handle_object`** using the same `with_object_data_key` lifecycle as the existing Object emitter; call the callback serializer and never mutate/materialize the source graph.
- [x] **Step 5: Implement `write_handle_stream_dict`** with the existing `StreamDictOptions` metadata switch; leave payload bytes to the existing stream pipeline API.
- [x] **Step 6: Implement `encrypt_dict_handle`** as a direct canonical handle snapshot of the context dictionary and implement handle `/Encrypt` dictionary emission with hexadecimal encoding only for the five direct binary keys (`/O`, `/U`, `/OE`, `/UE`, `/Perms`).
- [x] **Step 7: Run focused tests**:

```bash
cargo test -p flpdf --lib writer::encrypted_strings -- --nocapture
cargo test -p flpdf --lib object_handle::unparse_object_tests::unparse_encrypted_string_writer -- --nocapture
```

All handle-emitter and context-matrix tests pass, and no existing output golden changes.

### Task 4: Oracle documentation and regression gates

**Files:**
- Modify: `docs/qpdf-correspondence.md`.
- Modify: the Beads issue notes for `flpdf-egzr.3.2.15`.

- [x] **Step 1: Document** the exact qpdf source lines, the callback-to-`unparseObject` mapping, the `setDataKey`/object-stream lifecycle, the `/Encrypt` plaintext boundary, the metadata exemption, and the intentional separation of stream-payload encryption.
- [x] **Step 2: Run formatting, lint, private docs, focused tests, workspace tests, and the qpdf-zlib coverage command.
- [x] **Step 3: Run the changed-file and diff gates**:

```bash
git diff --name-only origin/main...HEAD
git diff --check origin/main...HEAD
```

- Only the ObjectHandle/encryption helper/docs files are listed; no writer consumer call site is changed.
- [x] **Step 4: Record the exact test/coverage evidence in Beads, run `bd dep cycles`, and push Beads only after readback confirms the issue state.**
- [x] **Step 5: Commit the implementation as one reviewable additive PR change.**

### Task 5: PR and integration gates

- [ ] Push the isolated branch and open the PR against `main`.
- [ ] Review the diff against qpdf 11.9.0 source and classify every comment by qpdf parity before changing code.
- [ ] Wait for every CI check, including Coverage, Fuzz, all platform tests, Codecov patch, and release gates; keep the PR draft until all are green, then mark it ready.
- [ ] Merge the PR, close `flpdf-egzr.3.2.15` only after the merge readback, run `bd dep cycles`, and finish with `bd dolt push`.
