# ASCIIHex Encoder Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove flpdf's qpdf-nonexistent whole-buffer ASCIIHex encoder and make the legacy explicit-Crypt compatibility path follow qpdf's decrypt-before-filter pipeline ordering, while preserving the qpdf-shaped streaming decoders.

**Architecture:** `pipeline::ascii_hex::AsciiHexDecoder` remains the canonical decode route wired through `stream_filter`. `filters::encode_stream_data` rejects `/ASCIIHexDecode` explicitly because qpdf 11.9.0 has `Pl_ASCIIHexDecoder`/`SF_ASCIIHexDecode` but no ASCIIHex encoder. The legacy raw-`Object` explicit-Crypt compatibility path mirrors qpdf `QPDF::decryptStream`: decrypt the raw source payload once before any remaining `/Filter` stage, remove only `/Crypt`, and never decode/re-encode a filter prefix. Tests that need encoded PDF bytes build them in test-only fixture helpers or use literal bytes.

**Tech Stack:** Rust workspace, Cargo tests, pinned qpdf 11.9.0 source, generated qpdf module documentation.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are the semantic oracle.
- Keep the change separate from the ASCII85 PR and do not change ASCII85 behavior.
- Do not add a compatibility adapter or preserve the old encoder for backward compatibility.
- Production code must follow RED -> GREEN TDD; test-only fixture encoders are allowed for constructing pre-encoded PDFs.
- For explicit `/Crypt`, preserve recovered stream framing for Identity, but subtract recovered framing before a real RC4/AES stage just as the canonical resolver pipe does.

---

### Task 1: Pin the write-side rejection and qpdf Crypt ordering

**Files:**
- Modify: `crates/flpdf/src/filters.rs` in the encode-side unit tests
- Modify: `crates/flpdf/src/reader.rs` in the legacy explicit-Crypt tests

**Interfaces:**
- Consumes: existing `encode_stream_data` and `ascii_hex_dict` test helpers.
- Produces: a regression test requiring an explicit `Error::Unsupported` for `/ASCIIHexDecode` encoding.
- Produces: a regression test proving actual RC4 ciphertext is decrypted before an `ASCIIHexDecode` stage even when `/Crypt` is the later filter slot.

- [x] **Step 1: Write the failing test**

Add a test next to the ASCIIHex integration tests:

```rust
#[test]
fn encode_stream_data_ascii_hex_is_explicitly_unsupported() {
    let error = encode_stream_data(&ascii_hex_dict(), b"payload").unwrap_err();

    assert_eq!(
        error.to_string(),
        "unsupported PDF feature: ASCIIHexEncode is not supported: qpdf provides an ASCIIHex decoder but no encoder"
    );
}
```

- [x] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p flpdf --lib filters::tests::encode_stream_data_ascii_hex_is_explicitly_unsupported -- --exact
```

Expected: FAIL because the current root `ascii_hex::encode` path returns bytes instead of `Error::Unsupported`.

- [x] **Step 3: Commit the RED test**

```bash
git add crates/flpdf/src/filters.rs
git commit -m "test: require ASCIIHex encode rejection"
```

### Task 2: Remove the production encoder route

**Files:**
- Modify: `crates/flpdf/src/filters.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Delete: `crates/flpdf/src/ascii_hex.rs`

**Interfaces:**
- Consumes: Task 1's failing contract and qpdf `Pl_ASCIIHexDecoder.cc`/`SF_ASCIIHexDecode.hh` evidence.
- Produces: decode-only ASCIIHex behavior and an explicit unsupported encode error.
- Produces: qpdf-ordered explicit-Crypt materialization without an ASCII85/ASCIIHex/Flate prefix re-encoder.

- [x] **Step 1: Replace the encoder branch with the minimal error**

In `apply_single_filter_encode`, replace the `ASCIIHexDecode` branch with:

```rust
if filter_name == b"ASCIIHexDecode" {
    return Err(
        "ASCIIHexEncode is not supported: qpdf provides an ASCIIHex decoder but no encoder"
            .to_string(),
    );
}
```

Remove the now-unused `use crate::ascii_hex;` import.

- [x] **Step 2: Run the focused test and verify GREEN**

Run the Task 1 command and expect one passing test.

- [x] **Step 3: Remove the obsolete module**

Delete `crates/flpdf/src/ascii_hex.rs` and remove `pub(crate) mod ascii_hex;` from `lib.rs`. Keep `pub(crate) mod ascii_hex;` in `pipeline.rs` unchanged.

- [x] **Step 4: Run the focused library tests**

```bash
cargo test -p flpdf --lib filters::tests::encode_stream_data_ascii_hex_is_explicitly_unsupported -- --exact
cargo test -p flpdf --lib pipeline::ascii_hex::tests
```

- [x] **Step 5: Commit the production cutover**

```bash
git add crates/flpdf/src/filters.rs crates/flpdf/src/lib.rs
git add -u crates/flpdf/src/ascii_hex.rs
git commit -m "refactor: remove ASCIIHex write encoder"
```

The focused reader regression and both filter-slot integration fixtures were also run. The after-Flate fixture encrypts the already-Flate-encoded bytes; qpdf decrypts before applying the declared filter order.

### Task 3: Migrate decode fixtures without restoring a production bridge

**Files:**
- Modify: `crates/flpdf/src/filters.rs` test module
- Modify: `crates/flpdf/tests/multi_filter_chain_tests.rs`
- Modify: `crates/flpdf/tests/stream_decode_recovery_public_api.rs`
- Modify: `crates/flpdf-cli/tests/cli_multi_filter_chain.rs`

**Interfaces:**
- Consumes: canonical `decode_stream_data` and the existing Flate/ASCII85 encoders where those remain qpdf-supported in this separate slice.
- Produces: pre-encoded ASCIIHex fixtures that test only decoder behavior and preserve recovery/CLI coverage.

- [x] **Step 1: Add test-only ASCIIHex fixture helpers**

Use helpers returning uppercase hex bytes, with or without a trailing `>` as each test requires. Keep them inside test modules or fixture support; do not add a library encoder.

- [x] **Step 2: Replace internal unit-test encoder calls**

Change ASCIIHex decode round-trip tests to pass fixture bytes directly. Change multi-stage and equivalence-corpus setup to use test-only fixtures. Preserve tests for whitespace, EOD, odd nibbles, errors, output limits, and filter ordering.

- [x] **Step 3: Convert integration round-trip tests to decode pre-encoded chains**

For `/Filter [/FlateDecode /ASCIIHexDecode]`, ASCIIHex-encode the raw test payload in the test helper, then pass that result through the existing single-filter Flate encoder. Assert only the decode result. Change test names/comments from “both encode and decode directions” to decoder fixture wording.

- [x] **Step 4: Build the CLI ASCIIHex PDF through the prefiltered fixture helper**

Keep the generic builder for ASCII85. For the ASCIIHex CLI test, construct `Flate(ASCIIHex(raw))` in test code and call `build_pdf_with_prefiltered_stream` with `[/FlateDecode /ASCIIHexDecode]`.

- [x] **Step 5: Run focused tests**

```bash
cargo test -p flpdf --test multi_filter_chain_tests
cargo test -p flpdf --test stream_decode_recovery_public_api
cargo test -p flpdf-cli --test cli_multi_filter_chain
```

- [x] **Step 6: Commit the fixture migration**

```bash
git add crates/flpdf/src/filters.rs crates/flpdf/tests/multi_filter_chain_tests.rs crates/flpdf/tests/stream_decode_recovery_public_api.rs crates/flpdf-cli/tests/cli_multi_filter_chain.rs
git commit -m "test: use pre-encoded ASCIIHex decoder fixtures"
```

### Task 4: Refresh qpdf correspondence and verify the complete slice

**Files:**
- Modify: `docs/qpdf-module-doc-index.md` via `python3 scripts/qpdf-module-docs.py --write`
- Modify: `crates/flpdf/src/filters.rs` public encode error documentation if needed

**Interfaces:**
- Consumes: the final source route and qpdf 11.9.0 correspondence.
- Produces: documentation that lists only the pipeline ASCIIHex correspondence and accurately describes the unsupported encode behavior.

- [x] **Step 1: Update generated correspondence**

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
```

Confirm the deleted root encoder row is gone and the pipeline row remains.

- [x] **Step 2: Run all verification gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo test
git diff --check
rg -n "crate::ascii_hex|ascii_hex::encode" crates/flpdf/src crates/flpdf/tests crates/flpdf-cli/tests
rg -n "pub\(crate\) mod ascii_hex;" crates/flpdf/src/pipeline.rs
```

Expected: the first `rg` has no root encoder hits; only the canonical pipeline module declaration remains, all quality commands exit 0, and no test failures occur.

- [x] **Step 3: Inspect scope and commit documentation**

```bash
git status --short
git diff --stat origin/main...HEAD
git add docs/qpdf-module-doc-index.md crates/flpdf/src/filters.rs
git commit -m "docs: record ASCIIHex decoder-only correspondence"
```

- [x] **Step 4: Push the branch and open a separate draft PR**

```bash
git push -u origin agent/asciihex-decoder-cutover
```

Open a draft PR against `main` describing the removed encoder, preserved decoder, qpdf source evidence, fixture migration, and verification commands.
