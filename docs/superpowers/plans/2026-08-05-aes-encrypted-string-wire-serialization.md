# AES Encrypted String Wire Serialization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Emit encrypted PDF strings at qpdf 11.9.0's writer boundary so AES ciphertext and the five binary `/Encrypt` entries always use hexadecimal syntax in compact, QDF, and linearized output while RC4 and plaintext retain the normal string heuristic.

**Architecture:** Add crate-private fallible object serializers that delegate only scalar string tokens to a callback, then consume `WriterEncryptionState` from a focused writer-owned encrypted-string emitter. Full and linearized writers call the emitter with the emitted object number; a separate shared helper writes the plaintext `/Encrypt` dictionary and forces only `/O`, `/U`, `/OE`, `/UE`, and `/Perms` to hex.

**Tech Stack:** Rust 2021, `crate::object`, `crate::writer`, Standard Security Handler primitives, qpdf 11.9.0 as source and behavioral oracle, Cargo tests, LCOV changed-line coverage.

## Global Constraints

- The oracle is pinned qpdf 11.9.0 at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`; cite `QPDF_String.cc:72-105` and `QPDFWriter.cc:785-803,842-847,1567-1599,1761-1796,2244-2255`.
- Use the emitted object number with generation zero, set the key only during top-level object emission, and clear it after success or error.
- Object-stream members receive no individual data key; their container stream remains the only encryption boundary.
- AES encrypted strings are hexadecimal in compact, QDF, and linearized output; RC4 and plaintext strings keep the existing `write_string_value` heuristic.
- The `/Encrypt` object receives no data key; only direct string values for `/O`, `/U`, `/OE`, `/UE`, and `/Perms` are forced hexadecimal for all ciphers and copy-encryption.
- Do not add an encrypted-string object tag, sentinel key, panic-based cipher conversion, pre-serialization object-tree mutation, or a larger linearization iteration limit.
- Preserve the existing stream-payload encryption and cleartext-metadata responsibilities for `flpdf-3yn9.12`.
- Every production change begins with an observed failing regression test, and every task ends in a focused green test and a commit.

---

## File Structure

- `crates/flpdf/src/object.rs`: crate-private compact/QDF/container serializers with a fallible scalar-string callback; public plaintext serializers remain unchanged.
- `crates/flpdf/src/writer/encrypted_strings.rs`: `WriterEncryptionState` adapter, scalar encryption and representation, plus shared `/Encrypt` dictionary emission.
- `crates/flpdf/src/writer/encryption_state.rs`: retain the merged state primitive; tests here remain the lifecycle oracle.
- `crates/flpdf/src/writer.rs`: record actual `/V` and `/R`, remove tree mutation and QDF rejection, and wire full-rewrite object and `/Encrypt` emission.
- `crates/flpdf/src/linearization/writer.rs`: wire the same emitter and `/Encrypt` helper into the linearized route.
- `crates/flpdf/tests/encrypt_writer_smoke.rs`: public full-rewrite compact/QDF compatibility regressions.
- `docs/qpdf-module-doc-index.md`: regenerated correspondence index after adding the writer submodule.

### Task 1: Fallible scalar-string callback serializers

**Files:**
- Modify: `crates/flpdf/src/object.rs`

**Interfaces:**
- Produces: `Object::try_write_pdf_with_string_writer<F>(&self, out: &mut Vec<u8>, write_string: &mut F) -> crate::Result<()> where F: FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>`
- Produces: `Object::try_write_pdf_qdf_with_string_writer<F>(&self, out: &mut Vec<u8>, indent: usize, write_string: &mut F) -> crate::Result<()>` with the same bound
- Produces: private `Dictionary::try_write_pdf_with_string_writer<F>` and `Dictionary::try_write_pdf_qdf_with_string_writer<F>` recursive helpers with the same callback and result contract
- Produces: `Dictionary::try_write_pdf_stream_with_string_writer<F>(&self, out: &mut Vec<u8>, refiltered: bool, write_string: &mut F) -> crate::Result<()>` with the same bound
- Produces: `Dictionary::try_write_pdf_stream_qdf_with_string_writer<F>(&self, out: &mut Vec<u8>, indent: usize, write_string: &mut F) -> crate::Result<()>` with the same bound

- [x] **Step 1: Add nested compact/QDF and error-propagation tests**

Add unit tests beside the existing object serialization tests. Use a dictionary containing `Array[String("first"), Dictionary{"Nested": String("second")}]`; the callback writes `<{hex}>` and records both plaintext slices. Assert compact and QDF layout retain their existing spacing while both nested strings pass through the callback. Add a callback returning `Error::Internal("string writer failed")` and assert the exact error is returned.

```rust
let mut seen = Vec::new();
let mut string_writer = |out: &mut Vec<u8>, value: &[u8]| {
    seen.push(value.to_vec());
    crate::tokenizer::write_hex_string(out, value);
    Ok(())
};
object.try_write_pdf_with_string_writer(&mut out, &mut string_writer)?;
assert_eq!(seen, [b"first".to_vec(), b"second".to_vec()]);
```

- [x] **Step 2: Run the focused tests and observe RED**

Run: `cargo test -p flpdf object::tests::callback_string_writer -- --nocapture`

Expected: compilation fails because `try_write_pdf_with_string_writer` and its QDF/stream-dictionary companions do not exist.

- [x] **Step 3: Implement the compact and QDF recursive serializers**

Copy the established container traversal and formatting exactly, changing only the `Object::String` arm to `write_string(out, value)?`. In QDF scalar handling, dispatch `Object::String` to the callback and retain `self.write_pdf(out)` for every other scalar. Stream dictionary variants must preserve `/Length`-last, refiltered `/Filter` placement, indentation, and key order.

```rust
pub(crate) fn try_write_pdf_with_string_writer<F>(
    &self,
    out: &mut Vec<u8>,
    write_string: &mut F,
) -> crate::Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>,
{
    match self {
        Object::String(value) => write_string(out, value),
        Object::Array(values) => {
            out.push(b'[');
            for value in values {
                out.push(b' ');
                value.try_write_pdf_with_string_writer(out, write_string)?;
            }
            out.extend_from_slice(b" ]");
            Ok(())
        }
        Object::Dictionary(dict) => dict.try_write_pdf_with_string_writer(out, write_string),
        Object::Stream(stream) => {
            stream.dict.try_write_pdf_with_string_writer(out, write_string)?;
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&stream.data);
            out.extend_from_slice(b"\nendstream");
            Ok(())
        }
        _ => {
            self.write_pdf(out);
            Ok(())
        }
    }
}
```

Add the private dictionary recursion used by these four public crate-private entry points. Remove the branch-only `write_pdf_with_forced_hex_strings`, `write_pdf_stream_with_forced_hex_strings`, and boolean string-mode recursion after all references move in later tasks; until then, leave them in place so this commit is independently buildable.

- [x] **Step 4: Run callback tests and the object test module**

Run: `cargo test -p flpdf object::tests::callback_string_writer -- --nocapture`

Run: `cargo test -p flpdf object::tests -- --nocapture`

Expected: all selected tests pass; existing plaintext serialization bytes are unchanged.

- [x] **Step 5: Commit the serializer boundary**

```bash
git add crates/flpdf/src/object.rs
git commit -m "refactor: add string emission callbacks"
```

### Task 2: Writer-owned encrypted-string emitter

**Files:**
- Create: `crates/flpdf/src/writer/encrypted_strings.rs`
- Modify: `crates/flpdf/src/writer.rs`

**Interfaces:**
- Consumes: all four callback serializers from Task 1
- Consumes: `WriterEncryptionState::new(bool, Vec<u8>, bool, i32, i32)` and `with_object_data_key`
- Produces: `EncryptedStringEmitter::from_context(ctx: &EncryptionContext) -> Self`
- Produces: `EncryptedStringEmitter::write_object(&mut self, out: &mut Vec<u8>, emitted_ref: ObjectRef, object_stream_index: Option<u32>, object: &Object, qdf: bool) -> crate::Result<()>`
- Produces: `EncryptedStringEmitter::write_stream_dict(&mut self, out: &mut Vec<u8>, emitted_ref: ObjectRef, object_stream_index: Option<u32>, dict: &Dictionary, qdf: bool, refiltered: bool) -> crate::Result<()>`
- Produces: `serialize_encrypted_string(out: &mut Vec<u8>, ciphertext: &[u8], use_aes: bool)`

- [x] **Step 1: Add deterministic representation and lifecycle tests**

In the new module's test section, assert printable ciphertext `b"printable"` becomes `<7072696e7461626c65>` for AES and `(printable)` for RC4. Construct AES-128, RC4, and AES-256 contexts with fixed keys and assert emitted object strings decrypt/round-trip through the existing security primitives. Assert an `Object::String` clone is unchanged after emission, an ObjStm-member call emits plaintext syntax, and an injected callback error leaves `current_data_key()` empty through a test-only inspection method.

```rust
let mut aes = Vec::new();
serialize_encrypted_string(&mut aes, b"printable", true);
assert_eq!(aes, b"<7072696e7461626c65>");

let mut rc4 = Vec::new();
serialize_encrypted_string(&mut rc4, b"printable", false);
assert_eq!(rc4, b"(printable)");
```

- [x] **Step 2: Run the new module test filter and observe RED**

Run: `cargo test -p flpdf writer::encrypted_strings::tests -- --nocapture`

Expected: compilation fails because the module and emitter do not exist.

- [x] **Step 3: Record exact encryption revision in `EncryptionContext`**

Add `encryption_v: i32` and `encryption_r: i32`. Return `(dict, key, cipher, v, r)` from every `EncryptMethod` match arm: V4 AES `(4,4)`, V5 R6 `(5,6)`, V5 R5 `(5,5)`, V1 RC4-40 `(1,2)`, V2 RC4-128 `(2,3)`, V4 RC4-128 `(4,4)`. The currently supported copied V4 AES source records `(4,4)`.

```rust
pub(crate) struct EncryptionContext {
    pub(crate) encrypt_dict: Dictionary,
    pub(crate) file_key: Vec<u8>,
    pub(crate) cipher: WriteCipher,
    pub(crate) encryption_v: i32,
    pub(crate) encryption_r: i32,
    // existing fields unchanged
}
```

- [x] **Step 4: Implement emission-time scalar encryption**

Declare `#[path = "writer/encrypted_strings.rs"] pub(crate) mod encrypted_strings;` in `writer.rs`. The emitter owns a `WriterEncryptionState`, copied `WriteCipher`, and `static_aes_iv`. Inside `with_object_data_key`, clone the scalar plaintext, choose the current data key, generate a fresh IV only for AES, call `encrypt_cipher_bytes`, and serialize based on AES versus RC4.

```rust
pub(crate) struct EncryptedStringEmitter {
    state: WriterEncryptionState,
    cipher: WriteCipher,
    static_aes_iv: bool,
}

fn encrypt_string(
    cipher: WriteCipher,
    static_aes_iv: bool,
    data_key: &[u8],
    plaintext: &[u8],
) -> crate::Result<Vec<u8>> {
    let mut bytes = plaintext.to_vec();
    let mut iv = if static_aes_iv {
        crate::pipeline::aes::static_initialization_vector()
    } else {
        [0; 16]
    };
    if crate::writer::cipher_needs_aes_iv(cipher) && !static_aes_iv {
        getrandom::getrandom(&mut iv).map_err(|error| {
            crate::Error::Unsupported(format!(
                "OS CSPRNG (getrandom) unavailable for AES IV generation: {error}"
            ))
        })?;
    }
    match cipher {
        WriteCipher::PerObject(ObjectKeyAlg::Rc4) => {
            encrypt_cipher_bytes(
                &mut bytes,
                StringEncryptCipher::Rc4 { key: data_key },
                &iv,
            )?;
        }
        WriteCipher::PerObject(ObjectKeyAlg::Aes) => {
            let key: &[u8; 16] = data_key.try_into().map_err(|_| {
                crate::Error::Unsupported(
                    "V=4 AES-128 data key is not 16 bytes".to_string(),
                )
            })?;
            encrypt_cipher_bytes(
                &mut bytes,
                StringEncryptCipher::Aes128 { key },
                &iv,
            )?;
        }
        WriteCipher::FileKeyAes256 => {
            let key: &[u8; 32] = data_key.try_into().map_err(|_| {
                crate::Error::Unsupported(
                    "V=5 AES-256 data key is not 32 bytes".to_string(),
                )
            })?;
            encrypt_cipher_bytes(
                &mut bytes,
                StringEncryptCipher::Aes256 { key },
                &iv,
            )?;
        }
    }
    Ok(bytes)
}
```

`Aes128` conversion returns `Error::Unsupported("V=4 AES-128 data key is not 16 bytes")`; `Aes256` conversion returns `Error::Unsupported("V=5 AES-256 data key is not 32 bytes")`. If `current_data_key()` is `None`, the callback writes the plaintext with `write_string_value`; this is the ObjStm-member route, not a silent encryption fallback for top-level objects.

- [x] **Step 5: Run emitter, encryption-state, and security tests**

Run: `cargo test -p flpdf writer::encrypted_strings::tests -- --nocapture`

Run: `cargo test -p flpdf writer::encryption_state -- --nocapture`

Run: `cargo test -p flpdf security::standard::tests::encrypt_strings -- --nocapture`

Expected: all selected tests pass, including exact V/R and ObjStm exclusion assertions.

- [x] **Step 6: Commit the emitter**

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/src/writer/encrypted_strings.rs
git commit -m "feat: emit encrypted strings from writer state"
```

### Task 3: Exact `/Encrypt` dictionary wire representation

**Files:**
- Modify: `crates/flpdf/src/writer/encrypted_strings.rs`
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/linearization/writer.rs`

**Interfaces:**
- Produces: `write_encryption_dictionary(out: &mut Vec<u8>, dict: &Dictionary)`
- Consumes: `object::write_name_escaped` and `tokenizer::{write_hex_string, write_string_value}`

- [x] **Step 1: Add exact-five-key tests**

Build a dictionary with printable direct strings at `O`, `U`, `OE`, `UE`, `Perms`, and `Custom`, plus a nested dictionary containing another `O`. Assert the first five direct values are `<hex>`, `/Custom` remains `(custom)`, and nested `/O` remains `(nested)`.

```rust
assert!(wire.windows(b"/O <7072696e7461626c65>".len())
    .any(|part| part == b"/O <7072696e7461626c65>"));
assert!(wire.windows(b"/Custom (custom)".len())
    .any(|part| part == b"/Custom (custom)"));
assert!(wire.windows(b"/Nested << /O (nested) >>".len())
    .any(|part| part == b"/Nested << /O (nested) >>"));
```

- [x] **Step 2: Run the `/Encrypt` helper test and observe RED**

Run: `cargo test -p flpdf writer::encrypted_strings::tests::encryption_dictionary -- --nocapture`

Expected: compilation fails because `write_encryption_dictionary` does not exist.

- [x] **Step 3: Implement and wire the helper**

Iterate the `BTreeMap`-ordered direct entries, write each name with `write_name_escaped`, and use `write_hex_string` only when both the key is in the exact five-key set and the value is `Object::String`. All other values call ordinary `Object::write_pdf`.

```rust
const HEX_ENCRYPT_KEYS: [&[u8]; 5] = [b"O", b"U", b"OE", b"UE", b"Perms"];

pub(crate) fn write_encryption_dictionary(out: &mut Vec<u8>, dict: &Dictionary) {
    out.extend_from_slice(b"<<");
    for (key, value) in dict.iter() {
        out.extend_from_slice(b" /");
        crate::object::write_name_escaped(out, key);
        out.push(b' ');
        match value {
            Object::String(bytes) if HEX_ENCRYPT_KEYS.contains(&key) => {
                crate::tokenizer::write_hex_string(out, bytes);
            }
            _ => value.write_pdf(out),
        }
    }
    out.extend_from_slice(b" >>");
}
```

Replace the full writer's cloned `Object::Dictionary(...).write_pdf` and the linearized writer's generic `/Encrypt` append path with this helper. Both routes write the indirect object header/footer themselves and never call `with_object_data_key` for this object.

- [x] **Step 4: Run unit and existing encryption dictionary tests**

Run: `cargo test -p flpdf writer::encrypted_strings::tests::encryption_dictionary -- --nocapture`

Run: `cargo test -p flpdf writer::tests::v5_r6_encrypt -- --nocapture`

Run: `cargo test -p flpdf linearization::writer::tests::linearize_with_encrypt_emits_encrypt_dict_at_reserved_object_number -- --nocapture`

Expected: all selected tests pass and the encryption dictionary remains plaintext.

- [x] **Step 5: Commit `/Encrypt` emission**

```bash
git add crates/flpdf/src/writer/encrypted_strings.rs crates/flpdf/src/writer.rs crates/flpdf/src/linearization/writer.rs
git commit -m "fix: serialize encrypt dictionary binary strings as hex"
```

### Task 4: Full-rewrite compact and QDF production wiring

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/object.rs`
- Modify: `crates/flpdf/tests/encrypt_writer_smoke.rs`

**Interfaces:**
- Consumes: `EncryptedStringEmitter::{write_object, write_stream_dict}`
- Removes: `encrypt_strings_in_object_for_writer`, `EncryptionContext::encrypted_strings_require_hex`, forced-hex serializer methods, and `force_hex_strings` parameters

- [x] **Step 1: Add public compact/QDF regressions**

Create a tiny PDF with a nested printable string in an Info dictionary. Write AES-128 with `static_id = true` and `static_aes_iv = true` in compact and QDF modes. Assert the encrypted scalar token is hexadecimal, the plaintext marker is absent, qpdf `--password= --check` exits successfully, and flpdf reopens/authenticates the output. Add RC4-128 with a deterministic printable-ciphertext unit seam and assert normal literal-string syntax. Add a generated ObjStm case and assert member strings are encrypted only through the container, not through an individual member key.

```rust
let mut qdf = encrypted_options();
qdf.qdf = true;
qdf.static_id = true;
qdf.static_aes_iv = true;
let bytes = rewrite_fixture(&qdf)?;
assert!(!bytes.windows(plaintext.len()).any(|part| part == plaintext));
assert_encrypted_string_tokens_are_hex(&bytes);
```

- [x] **Step 2: Run compact/QDF regressions and observe RED**

Run: `cargo test -p flpdf --test encrypt_writer_smoke aes_encrypted_strings_use_hex_in_compact_and_qdf -- --nocapture`

Expected: QDF fails with the current unsupported-option error; compact exposes that encryption happened by mutating an object clone rather than through the new emitter.

- [x] **Step 3: Wire full-rewrite emission-time encryption**

Construct one mutable `EncryptedStringEmitter` from `encrypt_ctx` before the object loop. Resolve and renumber objects without string mutation. For non-stream objects, call `write_object(..., None, &object, options.qdf)`. For stream objects, keep stream payload encryption where it is, then call `write_stream_dict(..., None, &s.dict, options.qdf, refiltered)` from the existing compact/QDF framing helpers.

Change the stream-buffer helpers to accept `Option<&mut EncryptedStringEmitter>` plus the emitted ref rather than a force-hex boolean. The QDF holder's indirect `/Length` stays in the same dictionary and is emitted through the callback serializer. ObjStm member serialization passes `Some(member_index)` and therefore receives no individual key; ObjStm container payload encryption remains unchanged.

- [x] **Step 4: Remove obsolete branch behavior and QDF rejection**

Delete the `if encrypting && options.qdf` preflight rejection. Delete the pre-tree-mutation helper and all forced-hex boolean propagation. Retain `security::standard::encrypt_strings_in_object` because reader/security tests and any non-writer callers still own that lower-level primitive.

- [x] **Step 5: Run full-writer focused and crate suites**

Run: `cargo test -p flpdf --test encrypt_writer_smoke -- --nocapture`

Run: `cargo test -p flpdf writer::tests -- --nocapture`

Run: `cargo test -p flpdf --test writer_tests -- --nocapture`

Expected: compact, QDF, RC4, AES-128, AES-256, copy-encryption, ObjStm, and cleartext-metadata tests pass.

- [x] **Step 6: Commit full-rewrite cutover**

```bash
git add crates/flpdf/src/object.rs crates/flpdf/src/writer.rs crates/flpdf/tests/encrypt_writer_smoke.rs
git commit -m "fix: serialize encrypted strings during full rewrite"
```

### Task 5: Linearized production wiring and convergence regression

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs`

**Interfaces:**
- Consumes: `EncryptedStringEmitter::write_object`, `EncryptedStringEmitter::write_stream_dict`, and `write_encryption_dictionary`
- Changes: `append_object` and `append_body_object` take `Option<&mut EncryptedStringEmitter>` for scalar/dictionary emission while retaining `Option<&EncryptionContext>` for stream payload encryption

- [x] **Step 1: Add AES string-syntax and convergence tests**

Extend the existing linearize-encrypt fixture with nested printable strings and assert all emitted encrypted scalar strings use hex tokens. Add a loop of at least 64 writes with random IVs using the PR #650 outline/shared-hint fixture; every write must succeed, reopen, authenticate, and pass linearization checks without increasing the convergence iteration bound.

```rust
for _ in 0..64 {
    let output = write_linearized_encrypted_fixture(false)?;
    assert_linearization_checks_pass(&output);
    assert_encrypted_body_strings_are_hex(&output);
}
```

- [x] **Step 2: Run linearized regressions and observe RED**

Run: `cargo test -p flpdf linearization::writer::tests::linearized_aes_strings_use_hex -- --nocapture`

Run: `cargo test -p flpdf linearization::writer::tests::linearized_encrypted_outline_and_part8_shared_hint_tables_stay_consistent_across_many_random_iv_runs -- --nocapture`

Expected: the new syntax test fails against generic/pre-mutated emission; the existing random-IV convergence regression remains the stability gate.

- [x] **Step 3: Wire the emitter through linearized helpers**

Create one emitter next to `EncryptionContext` and thread a mutable reference through every object emission pass. `append_object` calls `write_object(..., None, &object, false)`. `append_body_object` encrypts stream payloads with the existing context, then calls `write_stream_dict(..., None, &s.dict, false, refiltered)`. Keep hint-stream IV reuse and stream encryption unchanged.

The `/Encrypt` reserved object route calls `write_encryption_dictionary` directly and does not pass the context/emitter. No convergence constant changes are permitted.

- [x] **Step 4: Run linearization encryption and full linearization tests**

Run: `cargo test -p flpdf linearization::writer::tests::linearize_with_encrypt -- --nocapture`

Run: `cargo test -p flpdf linearization::writer::tests::linearized_encrypted -- --nocapture`

Run: `cargo test -p flpdf linearization::writer::tests -- --nocapture`

Expected: all selected tests pass repeatedly with both static and random IVs.

- [x] **Step 5: Commit the linearized cutover**

```bash
git add crates/flpdf/src/linearization/writer.rs
git commit -m "fix: serialize linearized encrypted strings from writer state"
```

### Task 6: Oracle probes, correspondence docs, coverage, and handoff gates

**Files:**
- Modify: `docs/qpdf-module-doc-index.md`
- Modify: `docs/superpowers/plans/2026-08-05-aes-encrypted-string-wire-serialization.md` only to mark completed checkboxes
- Modify through Beads tooling: issue `flpdf-a32l`

**Interfaces:**
- Validates: compact, QDF, and linearized AES paths; RC4; AES-256; ObjStm exclusion; `/Encrypt` self-skip; copy-encryption exact-five-key behavior

- [x] **Step 1: Probe qpdf 11.9.0 with deterministic fixtures**

Run qpdf 11.9.0 against a fixture containing nested strings for AES-128, RC4-40, AES-256, QDF AES-128, and linearized AES-128. Inspect raw object bytes and record that AES body strings and `/O /U /OE /UE /Perms` use `<...>`, RC4 body strings use the normal heuristic, and `/Encrypt` itself is not data-key encrypted.

Run: `scripts/fetch-qpdf-source.sh --print-path`

Run: `qpdf --version`

Expected: pinned source path resolves cleanly and binary reports qpdf 11.9.0.

- [x] **Step 2: Regenerate and verify module correspondence docs**

Run: `python3 scripts/qpdf-module-docs.py`

Run: `python3 scripts/qpdf-module-docs.py --check`

Expected: `writer/encrypted_strings.rs` appears as `QPDFWriter.cc:785-803` encryption-dictionary binary-key hex selection plus string-unparse, data-key lifecycle, and encryption-dictionary emission responsibility; the check exits zero.

- [x] **Step 3: Run formatting, clippy, and workspace tests**

Run: `cargo fmt -- --check`

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Run: `cargo test -p flpdf`

Run: `cargo test -p flpdf-cli`

Run: `cargo test`

Expected: every command exits zero with no warning denial.

- [x] **Step 4: Run fresh changed-line coverage**

Use the repository coverage command discovered from CI or contributor docs to regenerate LCOV after the final code commit. Run the clean-tree changed-line gate and inspect every uncovered executable line; add a regression test for reachable behavior and use `cov:ignore` only for a locally proven unreachable executable line.

Run: `rg -n "changed.*line|lcov|coverage" .github scripts Makefile.toml Cargo.toml`

Expected: the repository's changed executable-line gate reports 100% against the final branch diff and `git diff --check` is clean.

- [x] **Step 5: Commit generated docs and verification notes**

```bash
git add docs/qpdf-module-doc-index.md docs/superpowers/plans/2026-08-05-aes-encrypted-string-wire-serialization.md
git commit -m "docs: record encrypted string writer correspondence"
```

- [x] **Step 6: Persist Beads and push the implementation branch**

Add implementation, qpdf probe, focused-test, workspace-test, and coverage evidence to `flpdf-a32l`; do not close it until all acceptance criteria are demonstrated. Then persist and push.

Run: `bd show flpdf-a32l`

Run: `bd dolt push`

Run: `git status --short --branch`

Run: `git pull --rebase`

Run: `git push`

Expected: Beads persistence succeeds, the worktree is clean, and `fix/flpdf-a32l-aes-string-hex` is synchronized with its remote branch.
