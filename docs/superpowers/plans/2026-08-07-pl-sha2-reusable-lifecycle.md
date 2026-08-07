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

Add a loop over `[256, 384, 512]` that hashes `abc`, finishes, writes `def`,
finishes again, and compares the result to `digest_of(bits, b"def")`.

```rust
#[test]
fn write_after_finish_starts_a_fresh_cycle_with_the_same_bit_size() {
    for bits in [256, 384, 512] {
        let mut sha2 = PlSha2::new("sha2", None, bits).unwrap();
        sha2.write(b"abc").unwrap();
        sha2.finish().unwrap();
        sha2.write(b"def").unwrap();
        sha2.finish().unwrap();

        assert_eq!(sha2.get_hex_digest().unwrap(), digest_of(bits, b"def"));
    }
}
```

- [ ] **Step 2: Replace the repeated-finish rejection test**

For every supported size, finish `abc` twice, compare the second result to
`digest_of(bits, b"")`, and assert the downstream sink observed two finishes.

```rust
#[test]
fn repeated_finish_starts_an_empty_cycle_and_forwards_each_time() {
    for bits in [256, 384, 512] {
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

        assert_eq!(second_digest, digest_of(bits, b""));
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
