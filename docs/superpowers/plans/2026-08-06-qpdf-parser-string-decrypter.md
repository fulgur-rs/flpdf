# QPDFParser StringDecrypter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Decrypt encrypted file-object string tokens at the qpdf-shaped parser boundary while preserving raw signature `/Contents` bytes and offsets.

**Architecture:** `parser.rs` owns an optional fallible `StringDecrypter` callback and invokes it only for literal/hex string tokens. `reader/resolver.rs` binds that callback to the current indirect object and shared encryption state; it remains the only production consumer. The parser frame owns signature raw-byte capture, so no post-parse Object walk is used for this path.

**Tech Stack:** Rust workspace, existing `ObjectHandle` graph, pinned qpdf 11.9.0 source, existing RC4/AES primitives, `cargo test`, qpdf CLI differentials.

## Global Constraints

- qpdf 11.9.0 source and observed output are authoritative.
- Preserve the live InputSource parser's single-pass token consumption and existing parser diagnostics.
- Invoke decryption once only for `TokenType::String`; unknown words and content-stream tokens never call the callback.
- Preserve raw bytes and the token-start parsed offset only for completed `/Type /Sig` dictionaries with `/ByteRange` and string `/Contents`.
- Do not add stream-payload decryption, a raw Object post-parse pass, a legacy bridge, a sentinel, or panic-based error handling.
- Keep `flpdf-25kg.3.17 -> flpdf-25kg.3.18` and `flpdf-25kg.3.5 -> flpdf-25kg.3.17` unchanged.

---

### Task 1: Make live file-object parsing decrypter-aware

**Files:**
- Modify: `crates/flpdf/src/parser.rs:14-700`
- Test: `crates/flpdf/src/parser.rs:728-1240`

**Interfaces:**
- Consumes: `LiveInput`, `HandleResolver`, `ObjectHandle`, `ObjectValue`, and `Result`.
- Produces: `pub(crate) trait StringDecrypter` and an internal live-parser entry point accepting `Option<&mut dyn StringDecrypter>`.
- Produces: `LiveFrame::Dictionary` raw `/Contents` state that `finish_dictionary` can restore.

- [x] **Step 1: Write failing parser tests for callback scope and error propagation**

Add a test-only recorder immediately beside `NullResolver`:

```rust
struct RecordingDecrypter {
    calls: Vec<Vec<u8>>,
    fail: bool,
}

impl StringDecrypter for RecordingDecrypter {
    fn decrypt_string(&mut self, bytes: &mut Vec<u8>) -> Result<()> {
        self.calls.push(bytes.clone());
        if self.fail {
            return Err(Error::Internal("decrypter failure".into()));
        }
        bytes.extend_from_slice(b"-plain");
        Ok(())
    }
}
```

Exercise `b"(top) [(array)] << /Nested (dict) >>"` through the decrypter-aware live entry point. Assert three calls, decrypted handle values, and no additional calls for `b"unknown-word"`. Add a failing-decrypter case that asserts the same `Error::Internal("decrypter failure")` reaches the caller.

- [x] **Step 2: Run the new parser tests and verify RED**

Run:

```bash
cargo test -p flpdf --lib parser::live_input_tests::live_file_parser_decrypter
```

Expected: compilation failure because the callback contract and decrypter-aware entry point do not yet exist.

- [x] **Step 3: Add the parser-owned callback contract and token-time invocation**

Define the contract next to `HandleResolver`:

```rust
pub(crate) trait StringDecrypter {
    fn decrypt_string(&mut self, bytes: &mut Vec<u8>) -> Result<()>;
}
```

Keep `parse_live_file_object(input, resolver)` as the no-decrypter wrapper.
Add a sibling internal entry point that receives
`Option<&mut dyn StringDecrypter>`, store it on `LiveFileParser`, and pass it
through `parse_live_file_object_with_context`. In both top-level and
`parse_remainder` string-token paths, clone the raw token bytes, invoke
`decrypt_string`, then create `ObjectValue::String` from the mutated bytes.
Leave the `TokenType::Word` branch unchanged.

- [x] **Step 4: Add failing signature `/Contents` parser tests**

Parse the following body with `RecordingDecrypter`:

```text
<< /Type /Sig /ByteRange [0 10 20 30] /Contents (cipher) /Reason (reason) >>
```

Assert `/Reason == b"reason-plain"`, `/Contents == b"cipher"`, and that
`/Contents` retains the original string token's parsed offset. Add a second
body without `/ByteRange` and assert `/Contents == b"cipher-plain"`.

- [x] **Step 5: Implement frame-local raw signature capture**

Extend `LiveFrame::Dictionary` with `contents: Option<(Vec<u8>, i64)>`.
When the pending key is `/Contents` and a decrypter exists, save the raw token
bytes and `token.start as i64` before invoking the callback. On dictionary
completion, check exactly these conditions in the completed map:

```rust
let is_signature = values.get(b"Type" as &[u8]).and_then(ObjectHandle::as_name)
    == Some(b"Sig".to_vec());
let has_byte_range = values.contains_key(b"ByteRange" as &[u8]);
let has_string_contents = values.get(b"Contents" as &[u8])
    .and_then(ObjectHandle::as_string)
    .is_some();
```

When all are true, replace only `/Contents` with
`ObjectHandle::string(raw_bytes)` and set its parsed offset to the captured
token start. Do not capture or restore when no decrypter was supplied.

- [x] **Step 6: Run focused parser tests and commit the parser unit**

Run:

```bash
cargo test -p flpdf --lib parser::live_input_tests
```

Expected: all existing live-input tests plus callback, signature, unknown-word,
and error tests pass.

Commit:

```bash
git add crates/flpdf/src/parser.rs
git commit -m "feat(parser): add qpdf string decrypter hook"
```

### Task 2: Bind the callback to the canonical resolver encryption state

**Files:**
- Modify: `crates/flpdf/src/reader/resolver.rs:87-1260`
- Modify: `crates/flpdf/src/reader.rs:270-360`
- Test: `crates/flpdf/src/reader/resolver.rs:1800-3450`

**Interfaces:**
- Consumes: `parser::StringDecrypter`, `EncryptionState::string_method`, `EncryptionState::with_object_cipher`, `decrypt_cipher_bytes`, and `ResolverHandle::push_warning`.
- Produces: an object-ref-bound resolver adapter that decrypts a parser token using the shared encryption cell.
- Produces: canonical `read_object_at_offset` wiring that supplies the adapter only for encrypted documents.

- [x] **Step 1: Write a failing canonical-resolver integration test for parsed encrypted strings**

Build an uncompressed encrypted dictionary fixture in `resolver.rs` and call
`handle.try_dereference()` rather than `Pdf::resolve_object_handle`. Include
`/Title (TopSecretTitle)` and nested
`/Metadata << /Label (NestedSecret) >>`; assert both exposed handles are
plaintext after resolution. Preserve the raw source bytes before opening and
assert they do not contain either plaintext token.

- [x] **Step 2: Run the integration test and verify RED**

Run:

```bash
cargo test -p flpdf --lib reader::resolver::tests::canonical_resolver_decrypts_strings_at_parse_time
```

Expected: compilation failure because the canonical resolver has no parser
decrypter adapter yet.

- [x] **Step 3: Extract one object-string cipher operation from EncryptionState**

Add a narrowly scoped helper in `reader.rs` that performs qpdf string method
selection, decrypts one mutable byte vector with the object-specific cipher,
and returns the existing unknown-filter-warning flag. Its shape is:

```rust
fn decrypt_object_string(
    &mut self,
    object_ref: ObjectRef,
    bytes: &mut Vec<u8>,
) -> Result<bool>
```

It calls `string_method`; on `Some(use_aes)`, it calls
`with_object_cipher(object_ref, use_aes, |cipher| decrypt_cipher_bytes(bytes, cipher))`.
It returns the warning flag without decrypting when qpdf selects Identity.

- [x] **Step 4: Implement and wire the resolver adapter**

In `reader/resolver.rs`, implement a private adapter that holds the current
`ObjectRef`, `Rc<RefCell<Option<EncryptionState>>>`, and resolver warning
sink. Its `StringDecrypter::decrypt_string` implementation borrows the shared
state mutably for exactly the helper call, propagates the returned error, then
emits the existing unknown-string-filter diagnostic after that borrow ends.

At `read_object_at_offset`, construct the adapter after the indirect-object
header is validated and pass `Some(&mut adapter)` to the new parser entry only
when encryption parameters are present. Pass `None` for an unencrypted
document. Do not add an adapter to explicit parsing, ObjStm parsing, content
stream parsing, or `Pdf::resolve_object_handle`'s legacy native-reparse path.

- [x] **Step 5: Run focused encryption and resolver tests, then commit**

Run:

```bash
cargo test -p flpdf --lib reader::resolver::tests::canonical_resolver_decrypts_strings_at_parse_time
cargo test -p flpdf --lib reader::tests::decrypt_object_value_strings
cargo test -p flpdf --lib reader::resolver::tests
```

Expected: the parser-owned path decrypts canonical resolver handles once;
legacy walker regression tests retain their current behavior until `.3.5`
cuts consumers over.

Commit:

```bash
git add crates/flpdf/src/reader.rs crates/flpdf/src/reader/resolver.rs
git commit -m "feat(reader): decrypt strings during object parsing"
```

### Task 3: Lock observable qpdf parity and record the completed boundary

**Files:**
- Modify: `crates/flpdf/src/reader.rs:4400-4555`
- Modify: `crates/flpdf/src/parser.rs:728-1240`
- Modify: `docs/qpdf-correspondence.md:137`
- Test: `crates/flpdf/tests/reader_tests.rs`

**Interfaces:**
- Consumes: the parser callback from Task 1 and resolver adapter from Task 2.
- Produces: qpdf-differential tests for all supported string cipher families and signature `/Contents` preservation.

- [x] **Step 1: Write qpdf differential fixtures for the signature predicate**

Build a small indirect `/Type /Sig` object with `/ByteRange`, `/Contents`, and
one peer string. Encrypt it with the existing writer test helper. For qpdf,
run the pinned executable with the same password and inspect the object via
`--json-object=<object-number>`; for flpdf resolve the same object handle.
Assert that qpdf and flpdf both expose peer strings as plaintext and
`/Contents` as the original encrypted bytes. Repeat without `/ByteRange` and
assert both expose decrypted `/Contents`.

- [x] **Step 2: Add cipher-family and non-call coverage**

Use `tests/fixtures/encrypted/v2-rc4-128-r3.pdf`,
`tests/fixtures/encrypted/v4-aes-128-r4.pdf`, and
`tests/fixtures/encrypted/v5-aes-256-r6.pdf` with their documented passwords.
For each, compare the selected string-bearing object between qpdf and flpdf.
Keep parser-unit tests as the proof that unknown words and no-decrypter modes
make zero callback calls; add a content-stream regression that parses an
operator-like word without a callback.

- [x] **Step 3: Run differential tests and verify GREEN**

Run:

```bash
cargo test -p flpdf --test reader_tests encrypted
cargo test -p flpdf --lib parser::live_input_tests
cargo test -p flpdf --lib reader::resolver::tests
```

Expected: RC4, AES-128, and AES-256 fixtures agree with qpdf; signature
`/Contents` is restored only by the completed signature predicate.

- [x] **Step 4: Update the QPDFParser correspondence row**

Replace the `StringDecrypter ... 未接続` clause in
`docs/qpdf-correspondence.md`'s `QPDFParser.cc` row with the concrete live
parser callback, resolver adapter, `/Contents` sideband, and the remaining
explicit/content/ObjStm no-decrypter boundaries. Retain citations to the
pinned source.

- [x] **Step 5: Run quality gates and changed-line coverage**

Run:

```bash
cargo fmt -- --check
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --lib
cargo test --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path /tmp/flpdf-25kg-3-17.lcov
scripts/patch-coverage.sh --base origin/main --lcov /tmp/flpdf-25kg-3-17.lcov
```

Expected: formatter and all tests pass; changed executable lines have 100%
coverage before review.

- [x] **Step 6: Commit parity tests and documentation**

```bash
git add crates/flpdf/src/parser.rs crates/flpdf/src/reader.rs crates/flpdf/tests/reader_tests.rs docs/qpdf-correspondence.md
git commit -m "test: cover parser string decryption parity"
```
