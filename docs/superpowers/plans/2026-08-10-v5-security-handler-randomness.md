# Deterministic V5 Security-Handler Randomness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or superpowers:subagent-driven-development) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a test/helper-scoped per-write V5 R5/R6 randomness seam that consumes the same 68-byte order as qpdf 11.9.0 while retaining the production CSPRNG default.

**Architecture:** Keep `V5R6Secrets` as the internal one-to-one representation used by the existing R5/R6 dictionary builders. Add a hidden, feature-gated `V5Randomness` value to `WriteOptions`; `generate_v5r6_secrets` uses it only when supplied and otherwise calls `getrandom::fill` exactly as before. The seam is per-write and has no CLI seed option or process-global state.

**Tech Stack:** Rust 2024, `WriteOptions`, qpdf 11.9.0 `QUtil::initializeWithRandomBytes`, Cargo unit tests, and the existing `qpdf-zlib-compat` test feature.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are authoritative.
- The qpdf V5 draw order is file key 32 bytes, U salts 16 bytes, O salts 16 bytes, and `/Perms` tail 4 bytes.
- Production defaults remain CSPRNG-backed; no production seed option or global mutable provider is allowed.
- Use RED -> GREEN TDD before changing production code.
- Do not alter the existing V5 dictionary builders or the separate AES-IV seam.
- The qpdf C++ helper and encrypted writer comparison matrix remain in `flpdf-25kg.6.1`.

---

## Files and responsibilities

- Modify `crates/flpdf/src/writer.rs`: define the feature-gated test/helper value, expose it through `WriteOptions`, consume it at the existing V5 generation boundary, and add deterministic R5/R6 writer tests.
- Modify `crates/flpdf/src/lib.rs`: re-export the feature-gated hidden helper type for integration/helper consumers.
- No CLI files or qpdf source files change in this issue.

### Task 1: Pin the qpdf random-byte mapping with failing tests

**Files:**
- Modify: `crates/flpdf/src/writer.rs` in the `#[cfg(test)] mod tests` section near the existing V5 writer tests.

**Interfaces:**
- Produces the expected `V5Randomness::from_bytes([u8; 68])` mapping used by the later writer seam.

- [x] **Step 1: Write the failing mapping test**

Add a test that constructs bytes `0..68`, calls `V5Randomness::from_bytes`, and asserts the six fields match qpdf's 32+16+16+4 order.

- [x] **Step 2: Run the mapping test to verify RED**

Run:

```bash
cargo test -p flpdf --lib writer::tests::v5_randomness_from_qpdf_order
```

Expected: compilation fails because `V5Randomness` and its constructor do not exist.

### Task 2: Add the feature-gated per-write seam and deterministic writer coverage

**Files:**
- Modify: `crates/flpdf/src/writer.rs` near `WriteOptions`, `build_encryption_context`, and `generate_v5r6_secrets`.
- Modify: `crates/flpdf/src/lib.rs` in the public writer re-export list.

**Interfaces:**
- `V5Randomness` is compiled only under `cfg(test)` or the existing `qpdf-zlib-compat` feature and exposes `from_bytes([u8; 68])` plus the six qpdf-ordered fields.
- `WriteOptions::v5_randomness: Option<V5Randomness>` is compiled under the same gate and defaults to `None`.
- `generate_v5r6_secrets(&WriteOptions)` returns the fixed value when present and otherwise keeps the existing fallible CSPRNG path and error text.

- [x] **Step 1: Implement the minimum mapping and option field**

Define the hidden feature-gated value and field, preserving `WriteOptions`'s derived `Default`, `Debug`, and `Clone` implementations. Do not add a CLI option or process-global setter.

- [x] **Step 2: Run the mapping test to verify GREEN**

Run:

```bash
cargo test -p flpdf --lib writer::tests::v5_randomness_from_qpdf_order
```

Expected: PASS.

- [x] **Step 3: Write failing repeated-write tests for R5 and R6**

Add one test per revision that writes the existing string/stream fixture twice with `static_id`, `static_aes_iv`, `full_rewrite`, and the same `V5Randomness`, then asserts byte equality and reopens with the user password. Before the seam is wired, the test must fail because `WriteOptions` cannot yet carry the fixed value through encryption setup.

- [x] **Step 4: Run both deterministic tests to verify RED**

Run:

```bash
cargo test -p flpdf --lib writer::tests::v5_r6_fixed_randomness_is_byte_stable
cargo test -p flpdf --lib writer::tests::v5_r5_fixed_randomness_is_byte_stable
```

Expected: compilation or assertion failure caused by the missing V5 injection, not a fixture/setup error.

- [x] **Step 5: Thread the fixed value through the existing V5 boundary**

Change both R5 and R6 arms to call `generate_v5r6_secrets(options)`. Return the fixed fields when the feature-gated option is `Some`; otherwise retain one fallible `getrandom::fill` call and its actionable `Error::Unsupported` mapping.

- [x] **Step 6: Run the deterministic tests to verify GREEN**

Run:

```bash
cargo test -p flpdf --lib writer::tests::v5_r6_fixed_randomness_is_byte_stable
cargo test -p flpdf --lib writer::tests::v5_r5_fixed_randomness_is_byte_stable
```

Expected: both PASS, including password round-trip and exact byte equality.

### Task 3: Verify feature boundaries and regression safety

**Files:**
- No additional source files.

- [x] **Step 1: Run focused V5 and security tests**

```bash
cargo test -p flpdf --lib writer::tests::v5_r6_encrypt_round_trips_string_and_stream_via_reader
cargo test -p flpdf --lib writer::tests::v5_r5_encrypt_round_trips_string_and_stream_via_reader
cargo test -p flpdf --lib encryption::standard::tests::build_v5_r6_encrypt_dict_round_trips_user_owner_and_perms
```

- [x] **Step 2: Run the qpdf-zlib feature build and tests**

```bash
cargo test -p flpdf --features qpdf-zlib-compat --lib writer::tests::v5_r6_fixed_randomness_is_byte_stable
cargo test -p flpdf --features qpdf-zlib-compat --lib writer::tests::v5_r5_fixed_randomness_is_byte_stable
```

- [x] **Step 3: Run formatting, clippy, and workspace tests**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
```

- [x] **Step 4: Inspect the diff and preserve unrelated worktree state**

```bash
git status --short
git diff --check
git diff --stat
```

Expected: only the implementation plan and the two Rust source files are changed in this worktree; `main` and unrelated worktrees remain untouched.
