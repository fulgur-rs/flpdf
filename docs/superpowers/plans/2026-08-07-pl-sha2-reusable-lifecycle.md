# Pl_SHA2 Reusable Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `PlSha2` reuse the selected SHA-256/384/512 digest after every `finish()`, matching qpdf 11.9.0.

**Architecture:** Preserve the selected size in the existing `Sha2Digest` enum variant and finalize with RustCrypto `finalize_reset()`. Keep qpdf's downstream-first finish ordering and existing Rust safety translations.

**Tech Stack:** Rust 2024, RustCrypto `sha2` 0.10, the existing `Pipeline` trait, Cargo tests.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are authoritative.
- Use RED -> GREEN TDD before changing production code.
- Do not add a crypto-provider abstraction or perform the R5/R6 consumer cutover.
- Preserve downstream write/finish error ordering and defined invalid-state errors.
- Finish with focused tests, fmt, denied-warning clippy, workspace tests, and fresh patch coverage 100%.

---

### Task 1: Pin reusable digest cycles with failing tests

**Files:**
- Modify/Test: `crates/flpdf/src/pipeline/sha2.rs`

**Interfaces:**
- Consumes: `PlSha2::new`, `Pipeline::write`, `Pipeline::finish`, `PlSha2::get_hex_digest`
- Produces: lifecycle regression coverage for all three supported digest sizes

- [ ] **Step 1: Replace the write-after-finish rejection test**

Add a table of OpenSSL-derived literal digests. Hash `abc`, finish, write
`def`, finish again, and compare the result to the independent literal for the
selected size.

```rust
#[test]
fn write_after_finish_starts_a_fresh_cycle_with_the_same_bit_size() {
    let cases = [
        (256, "cb8379ac2098aa165029e3938a51da0bcecfc008fd6795f401178647f96c5b34"),
        (384, "180c325cccb299e76ec6c03a5b5a7755af8ef499906dbf531f18d0ca509e4871b0805cac0f122b962d54badc6119f3cf"),
        (512, "40a855bf0a93c1019d75dd5b59cd8157608811dd75c5977e07f3bc4be0cad98b22dde4db9ddb429fc2ad3cf9ca379fedf6c1dc4d4bb8829f10c2f0ee04a66663"),
    ];
    for (bits, expected) in cases {
        let mut sha2 = PlSha2::new("sha2", None, bits).unwrap();
        sha2.write(b"abc").unwrap();
        sha2.finish().unwrap();
        sha2.write(b"def").unwrap();
        sha2.finish().unwrap();

        assert_eq!(sha2.get_hex_digest().unwrap(), expected);
    }
}
```

- [ ] **Step 2: Replace the repeated-finish rejection test**

For every supported size, finish `abc` twice, compare the second result to an
independently derived empty-input literal, and assert the downstream sink
observed two finishes.

```rust
#[test]
fn repeated_finish_starts_an_empty_cycle_and_forwards_each_time() {
    let cases = [
        (256, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        (384, "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b"),
        (512, "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"),
    ];
    for (bits, expected) in cases {
        let mut sink = RecordingSink::default();
        let second_digest;
        {
            let mut sha2 =
                PlSha2::new("sha2", Some(&mut sink as &mut dyn Pipeline), bits).unwrap();
            sha2.write(b"abc").unwrap();
            sha2.finish().unwrap();
            sha2.finish().unwrap();
            second_digest = sha2.get_hex_digest().unwrap();
        }

        assert_eq!(second_digest, expected);
        assert_eq!(sink.finishes, 2);
    }
}
```

- [ ] **Step 3: Run the focused tests and verify RED**

Run: `cargo test -p flpdf pipeline::sha2`

Expected: both new lifecycle tests fail because the current implementation
returns `PipelineError::Logic` on the second cycle.

### Task 2: Reset the selected digest after finalize

**Files:**
- Modify: `crates/flpdf/src/pipeline/sha2.rs`
- Modify: `docs/qpdf-correspondence.md`

**Interfaces:**
- Consumes: `sha2::Digest::finalize_reset`
- Produces: `Sha2Digest::finalize_and_reset(&mut self) -> Vec<u8>` and reusable `PlSha2::finish`

- [ ] **Step 1: Implement the minimal lifecycle change**

Replace the consuming helper with:

```rust
fn finalize_and_reset(&mut self) -> Vec<u8> {
    match self {
        Self::Bits256(hasher) => hasher.finalize_reset().to_vec(),
        Self::Bits384(hasher) => hasher.finalize_reset().to_vec(),
        Self::Bits512(hasher) => hasher.finalize_reset().to_vec(),
    }
}
```

Change `finish()` to borrow the selected digest mutably, finalize/reset it,
store the result, and return `Ok(())`. Keep downstream `finish()` before this
state transition and retain the never-selected-bits error.

- [ ] **Step 2: Run the focused tests and verify GREEN**

Run: `cargo test -p flpdf pipeline::sha2`

Expected: all lifecycle and existing SHA2 tests pass.

- [ ] **Step 3: Correct parity documentation**

Replace the stale explanation that native close leaves a finalized context
with the `sha2.c`/`sha2big.c` reinitialization evidence. Classify the reusable
lifecycle as qpdf-equivalent while retaining the two memory-safety deviations.

- [ ] **Step 4: Commit the implementation slice**

```bash
git add crates/flpdf/src/pipeline/sha2.rs docs/qpdf-correspondence.md
git commit -m "fix(pipeline): match reusable Pl_SHA2 lifecycle"
```

### Task 3: Verify, persist, and publish

**Files:**
- Verify: all changed files

**Interfaces:**
- Consumes: completed implementation and repository quality gates
- Produces: pushed branch and closed/persisted Beads issue

- [ ] **Step 1: Run formatting and static checks**

Run `cargo fmt --all -- --check`, then
`cargo clippy --workspace --all-targets --all-features -- -D warnings`.

- [ ] **Step 2: Run workspace tests**

Run: `cargo test`

- [ ] **Step 3: Verify changed-line coverage**

Run the repository's LLVM coverage command against `origin/main` and require
100% patch coverage for the changed Rust lines.

- [ ] **Step 4: Push and close Beads after readback**

Push the feature branch, close `flpdf-1c7z` with verification evidence, run
`bd dolt push`, and read back the issue and remote branch state.
