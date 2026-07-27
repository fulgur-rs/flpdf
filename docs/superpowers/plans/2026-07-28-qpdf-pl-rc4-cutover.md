# qpdf PlRc4 Production Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement qpdf 11.9.0 `Pl_RC4`, route the reader and writer RC4 stream payload paths through it, and delete the superseded stream-decryption helper.

**Architecture:** `pipeline/rc4.rs::PlRc4` owns one retained `security::rc4::Rc4`, a reusable bounded output buffer, the downstream `Pipeline`, and qpdf's finished state. String encryption/decryption continues to use `Rc4` directly; only stream payload consumers in `reader.rs` and `writer.rs` assemble `PlRc4 -> Buffer`, while AES remains on the existing one-shot path until `flpdf-qynx.10`.

**Tech Stack:** Rust 2021 workspace; existing crate-private `Pipeline`, `Buffer`, and stateful `Rc4`; qpdf 11.9.0 at commit `3b97c9bd266b7c32ea36d3536e22dab77412886d`; C++17 oracle probe; Cargo tests, Clippy, strict rustdoc, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are the oracle.
- The default `PlRc4` output buffer is exactly 65,536 bytes.
- One `Rc4` state is retained across every `write` call and every internal output-buffer chunk.
- `finish` drops the output buffer before propagating downstream; every repeated `finish` propagates again.
- A `write` after any `finish` attempt returns `<identifier>: Pl_RC4: write() called after finish() called`.
- Empty writes emit no downstream chunk and do not advance RC4 state.
- Reader and writer RC4 stream payloads use `PlRc4`; string objects keep the direct `Rc4` path.
- AES stream and string behavior is unchanged and remains for `flpdf-qynx.10`.
- The unused `filters.rs::decode_stream_data_with_decryption` route and its private tests are deleted.
- No compatibility wrapper or fallback RC4 stream route remains.
- Changed executable lines must have fresh 100% patch coverage against `origin/main`.

---

### Task 1: Add the PlRc4 pipeline component

**Files:**
- Create: `crates/flpdf/src/pipeline/rc4.rs`
- Modify: `crates/flpdf/src/pipeline.rs`

**Interfaces:**
- Consumes: `crate::security::rc4::Rc4`, `Pipeline`, `PipelineError`, and `PipelineResult`
- Produces: `PlRc4::new(identifier, next, key)`, `PlRc4::from_c_str(identifier, next, key)`, `PlRc4::with_buffer_size(identifier, next, key, out_buffer_size)`, and `DEFAULT_OUT_BUFFER_SIZE`

- [ ] **Step 1: Add the module and failing contract tests**

Add `pub(crate) mod rc4;` to `pipeline.rs`. Create `pipeline/rc4.rs` with tests that reference the not-yet-defined `PlRc4` API. The tests use a real recording sink and assert:

```rust
assert_eq!(DEFAULT_OUT_BUFFER_SIZE, 65_536);
assert_eq!(
    encrypt_chunks(&[b"Plain", b"text"], DEFAULT_OUT_BUFFER_SIZE),
    hex::decode("bbf316e8d940af0ad3").unwrap()
);
assert_eq!(sink.chunk_lengths, vec![65_536, 1]);
assert_eq!(sink.finishes, 2);
assert_eq!(
    stage.write(b"x").unwrap_err().to_string(),
    "rc4: Pl_RC4: write() called after finish() called"
);
```

Include separate tests for:

- one write versus multiple writes over identical bytes;
- default-buffer boundaries at 65,535, 65,536, and 65,537 bytes;
- custom output buffer boundaries;
- empty write with no downstream chunk and no state advancement;
- explicit key versus `CStr` key;
- in-place core output versus out-of-place pipeline output;
- repeated finish propagation;
- downstream write and finish errors remaining unchanged;
- zero buffer-size rejection without entering a non-progressing loop.

- [ ] **Step 2: Run the tests to verify RED**

Run:

```bash
cargo test -p flpdf --lib pipeline::rc4::tests --no-fail-fast
```

Expected: compilation fails because `PlRc4` and `DEFAULT_OUT_BUFFER_SIZE` do not exist.

- [ ] **Step 3: Implement the minimal qpdf-shaped stage**

Implement:

```rust
pub(crate) const DEFAULT_OUT_BUFFER_SIZE: usize = 65_536;

pub(crate) struct PlRc4<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    rc4: Rc4,
    outbuf: Option<Vec<u8>>,
}
```

`write` must:

1. reject `outbuf == None` with qpdf's exact logic error;
2. return immediately for empty input;
3. split input with `chunks(outbuf.len())`;
4. copy each chunk into the retained output buffer;
5. call `Rc4::process_in_place` on that slice;
6. call `next.write` once per processed output slice.

`finish` must set `outbuf = None` before calling `next.finish`, including when downstream finish fails. `from_c_str` delegates to `Rc4::from_c_str`. `with_buffer_size` rejects zero with `PipelineError::logic("Pl_RC4: output buffer size must be greater than zero")`.

- [ ] **Step 4: Run focused GREEN and formatting**

Run:

```bash
cargo fmt
cargo test -p flpdf --lib pipeline::rc4::tests --no-fail-fast
```

Expected: all PlRc4 contract tests pass.

- [ ] **Step 5: Commit the component**

```bash
git add crates/flpdf/src/pipeline.rs crates/flpdf/src/pipeline/rc4.rs
git commit -m "feat(rc4): add qpdf PlRc4 pipeline stage"
```

---

### Task 2: Add the pinned qpdf Pl_RC4 differential

**Files:**
- Create: `tests/oracle/qpdf_pl_rc4_shim/qpdf/RC4.hh`
- Modify: `tests/oracle/qpdf_rc4_probe.cc`
- Modify: `scripts/qpdf-rc4-diff.sh`
- Modify: `scripts/tests/qpdf-rc4-diff-contract.sh`
- Modify: `crates/flpdf/src/pipeline/rc4.rs`

**Interfaces:**
- Consumes: pinned `Pipeline.cc`, `Pl_RC4.cc`, `RC4_native.cc`, and qpdf internal headers
- Produces: `QPDF_PL_RC4_PROBE` ignored Rust differential and a hardened oracle runner

- [ ] **Step 1: Add a failing ignored-oracle boundary**

Add deterministic oracle cases in `pipeline/rc4.rs` for:

- empty input;
- 65,535, 65,536, 65,537, and 131,073-byte inputs;
- one write and split writes;
- explicit and NUL-terminated keys;
- default and custom output buffers;
- repeated finish;
- exact write-after-finish error text.

Add an ignored test whose name contains the shared
`qpdf_rc4_differential` selector:

```rust
#[test]
#[ignore = "live qpdf 11.9.0 Pl_RC4 oracle"]
fn qpdf_rc4_differential_pl_rc4_pipeline() {
    let probe = std::env::var_os("QPDF_PL_RC4_PROBE")
        .expect("set QPDF_PL_RC4_PROBE to the qpdf 11.9.0 probe");
    assert_qpdf_pl_rc4_oracle_matches(|case| {
        run_qpdf_pl_rc4_probe(Path::new(&probe), case)
    });
}
```

Ordinary tests must exercise the comparison loop and fake-probe success, failure, argument, and non-UTF-8 boundaries.

- [ ] **Step 2: Run the oracle boundary to verify RED**

Run:

```bash
cargo test -p flpdf --lib pipeline::rc4::tests::qpdf_rc4_differential_pl_rc4_pipeline -- --ignored --exact
```

Expected: failure stating that `QPDF_PL_RC4_PROBE` is unset.

- [ ] **Step 3: Add the C++ probe and shim**

Extend the existing RC4 probe with a `pipeline` mode. It must instantiate the
pinned `Pl_RC4` implementation over a recording `Pipeline` sink and print a
stable record containing ciphertext hex, downstream chunk lengths, finish
count, and write-after-finish message. Existing primitive probe modes remain
unchanged.

The test-only `qpdf/RC4.hh` shim must preserve the production wrapper contract exactly:

```cpp
class RC4
{
  public:
    RC4(unsigned char const* key_data, int key_len = -1) :
        impl(key_data, key_len)
    {
    }

    void process(unsigned char const* in_data, size_t len, unsigned char* out_data)
    {
        impl.process(in_data, len, out_data);
    }

  private:
    RC4_native impl;
};
```

This allows the actual pinned `Pl_RC4.cc` to compile with pinned `RC4_native.cc` without substituting the stage logic under test.

- [ ] **Step 4: Add and contract-test the runner**

Extend the already hardened `scripts/qpdf-rc4-diff.sh` rather than duplicating
its temporary-directory safety machinery. It must:

1. resolve only `scripts/fetch-qpdf-source.sh --print-path`;
2. verify commit `3b97c9bd266b7c32ea36d3536e22dab77412886d` and tracked cleanliness before and after each external action;
3. create a private external `mktemp` directory;
4. compile the probe with C++17, shim include first, then pinned includes, plus pinned `Pipeline.cc`, `Pl_RC4.cc`, and `RC4_native.cc`;
5. reject malformed probe arguments before Rust execution;
6. set both `QPDF_RC4_PROBE` and `QPDF_PL_RC4_PROBE`, then run the shared
   `qpdf_rc4_differential` ignored-test selector;
7. remove only its verified private temporary directory.

The existing shell contract test uses fake `git`, `mktemp`, `c++`, and `cargo`
tools to verify the expanded compile arguments, Cargo selector/environment,
source-state checks, unsafe temporary-path rejection, and cleanup containment.

- [ ] **Step 5: Run live oracle GREEN**

Run:

```bash
scripts/tests/qpdf-rc4-diff-contract.sh
scripts/qpdf-rc4-diff.sh
```

Expected: contract test and every live qpdf Pl_RC4 differential case pass.

- [ ] **Step 6: Commit the oracle**

```bash
git add crates/flpdf/src/pipeline/rc4.rs tests/oracle/qpdf_rc4_probe.cc tests/oracle/qpdf_pl_rc4_shim/qpdf/RC4.hh scripts/qpdf-rc4-diff.sh scripts/tests/qpdf-rc4-diff-contract.sh docs/superpowers/plans/2026-07-28-qpdf-pl-rc4-cutover.md
git commit -m "test(rc4): add qpdf PlRc4 differential"
```

---

### Task 3: Cut production stream consumers over and delete the old route

**Files:**
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/filters.rs`
- Modify: `crates/flpdf/src/security/standard.rs`

**Interfaces:**
- Consumes: `PlRc4`, `Buffer`, `Pipeline`, per-object RC4 keys, and existing AES helpers
- Produces: reader and writer RC4 stream payloads that traverse only `PlRc4`

- [ ] **Step 1: Establish the existing integration safety net**

Run:

```bash
cargo test -p flpdf --lib security::standard::tests --no-fail-fast
cargo test -p flpdf --lib writer::tests::rc4_methods_round_trip_string_and_stream_via_reader -- --exact
cargo test -p flpdf --test reader_tests rc4 --no-fail-fast
cargo test -p flpdf-cli --test encrypted_rewrite_tests --no-fail-fast
```

Expected: the existing string-and-stream integration tests pass before the refactor.

- [ ] **Step 2: Migrate reader RC4 stream decryption**

In `reader.rs::decrypt_stream_bytes`, replace the RC4 call to `decrypt_cipher_bytes` with:

```rust
let mut output = Buffer::new("RC4 stream decryption output", None);
{
    let mut rc4 = PlRc4::new("RC4 stream decryption", &mut output, &key)?;
    rc4.write(bytes)?;
    rc4.finish()?;
}
*bytes = output.take_buffer()?;
Ok(())
```

Keep `StringCipher::Rc4` in `decrypt_strings_in_object`; keep AES branches unchanged.

- [ ] **Step 3: Migrate writer RC4 stream encryption**

In `writer.rs::encrypt_stream_payload_for_writer`, replace the RC4 `StringEncryptCipher` branch with:

```rust
let mut output = Buffer::new("rc4 stream encryption output", None);
{
    let mut rc4 =
        PlRc4::new("rc4 stream encryption", &mut output, per_obj_key.as_slice())?;
    rc4.write(&stream.data)?;
    rc4.finish()?;
}
stream.data = output.take_buffer()?;
```

Keep the AES `encrypt_cipher_bytes` branches and all string-walker branches unchanged.

- [ ] **Step 4: Delete the superseded filter route**

Delete `filters.rs::decode_stream_data_with_decryption`, its `#[allow(dead_code)]`, its import of `decrypt_cipher_bytes`/`StringCipher`, and its four private tests. No production caller uses this helper; reader stream decryption is now the sole read-side crypt stage before normal filter decoding.

Rename the mixed `encrypt_cipher_bytes_round_trips_stream_payloads_for_all_aes_variants` test so it covers only the retained AES helper behavior. RC4 stream coverage comes from the real reader/writer integration tests and PlRc4 tests.

- [ ] **Step 5: Verify consumer GREEN and route deletion**

Run:

```bash
cargo fmt
cargo test -p flpdf --lib pipeline::rc4::tests --no-fail-fast
cargo test -p flpdf --lib security::standard::tests --no-fail-fast
cargo test -p flpdf --lib writer::tests::rc4_methods_round_trip_string_and_stream_via_reader -- --exact
cargo test -p flpdf --test reader_tests rc4 --no-fail-fast
cargo test -p flpdf-cli --test encrypted_rewrite_tests --no-fail-fast
rg -n "decode_stream_data_with_decryption" crates
rg -n "decrypt_cipher_bytes\\(bytes, StringCipher::Rc4|encrypt_cipher_bytes\\(&mut stream\\.data, cipher" crates/flpdf/src
```

Expected: tests pass; both searches return no matches. Direct `Rc4` remains only for string/password/core responsibilities, not reader/writer stream payloads.

- [ ] **Step 6: Commit the production cutover**

```bash
git add crates/flpdf/src/reader.rs crates/flpdf/src/writer.rs crates/flpdf/src/filters.rs crates/flpdf/src/security/standard.rs
git commit -m "refactor(rc4): cut stream consumers over to PlRc4"
```

---

### Task 4: Documentation and full quality gates

**Files:**
- Modify: `docs/qpdf-correspondence.md`
- Modify: `docs/qpdf-module-doc-index.md`
- Modify: `scripts/qpdf-module-docs.py` only if the generated correspondence rule requires it

**Interfaces:**
- Consumes: completed component and production-route inventory
- Produces: truthful `Pl_RC4` completion status and final verification evidence

- [ ] **Step 1: Update correspondence and regenerate generated docs**

Mark `Pl_RC4` complete in its own correspondence row and keep `Pl_AES_PDF` incomplete under `flpdf-qynx.10`. Record:

- `pipeline/rc4.rs` owns incremental chunking and lifecycle;
- `security/rc4.rs` remains the stateful primitive;
- `reader.rs` and `writer.rs` are the production stream consumers;
- string RC4 remains a direct primitive consumer by design.

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
python3 scripts/qpdf-module-docs.py --check
```

- [ ] **Step 2: Run focused oracle and route inventory**

Run:

```bash
scripts/tests/qpdf-rc4-diff-contract.sh
scripts/qpdf-rc4-diff.sh
rg -n "Pl_RC4|PlRc4|flpdf-qynx\\.2\\.2" docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md crates/flpdf/src
rg -n "decode_stream_data_with_decryption" crates
```

Expected: oracle passes; documentation identifies the completed component; the deleted route has no matches.

- [ ] **Step 3: Run all local quality gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
cargo test --workspace
```

Expected: every command exits zero.

- [ ] **Step 4: Run fresh 100% patch coverage**

Run:

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: every changed executable line is covered and patch coverage reports 100%.

- [ ] **Step 5: Commit final documentation and remediation**

```bash
git add docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md scripts/qpdf-module-docs.py
git commit -m "docs: record PlRc4 production cutover"
```

- [ ] **Step 6: Close and publish**

After verifying `git status`, append exact verification notes to `flpdf-qynx.2.2`, close it, push Beads, rebase if required, and push `feature/flpdf-qynx-2-2-plrc4`.
