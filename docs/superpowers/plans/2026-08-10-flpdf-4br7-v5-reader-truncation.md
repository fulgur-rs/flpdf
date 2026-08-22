# flpdf-4br7 implementation plan

> Execute this plan in `/home/ubuntu/flpdf/.worktrees/flpdf-4br7-v5-password-truncation`.
> Keep `/home/ubuntu/flpdf` on `main` untouched.

## Goal

Make V=5 password handling match qpdf 11.9.0 while keeping `reader.rs` a
delegating facade: writer-side hash primitives consume raw password bytes;
security-layer authentication truncates supplied V=5 passwords to 127 bytes.

## Files in scope

- `crates/flpdf/src/encryption/password.rs`
  - remove the encryption-specific truncation from generic normalization and
    update its focused unit test/docs.
- `crates/flpdf/src/encryption/standard.rs`
  - remove truncation from R=5/R=6 hash primitives;
  - add a private 127-byte prefix helper;
  - apply it at all four V=5 user/owner authentication entry points;
  - update algorithm and parameter documentation;
  - replace stale truncated-hash expectations and add direct boundary tests.
- `crates/flpdf/tests/qpdf_v5_password_parity.rs`
  - add qpdf-gated end-to-end checks using the public writer/reader APIs and
    `qpdf --check`.
- `docs/superpowers/specs/2026-08-10-flpdf-4br7-v5-reader-truncation-design.md`
  - design record already reviewed and approved.

Do not modify `crates/flpdf/src/reader.rs` or `crates/flpdf/src/writer.rs`.

## Execution steps

### 1. RED: pin the corrected contracts

1. In `encryption/password.rs`, change the existing long-password expectation to
   assert that V=5 normalization preserves all bytes. This should fail before
   the production normalization change.
2. In `encryption/standard.rs`, change the R=5 and R=6 long-password hash tests
   to expected full-password oracle values, and add suffix-sensitivity
   assertions where useful. These should fail while the hash helpers still
   clamp at 127 bytes.
3. Add direct R=5 and R=6 user/owner authentication tests using deterministic
   `/U`, `/O`, `/UE`, and `/OE` inputs. Build those inputs with the existing
   fixed-secret helpers and verify that a supplied password consisting of a
   matching 127-byte prefix plus a suffix authenticates at the security
   boundary. These tests should fail before the check entry points own the
   truncation after the raw hash change.
4. Add the qpdf-gated integration test before changing production code. Cover
   both `EncryptParams::v5_r5` and `EncryptParams::v5_r6`:
   - write a minimal PDF with a 127-byte user password and a distinct owner
     password;
   - confirm qpdf accepts a reader password with the same 127-byte prefix plus
     a suffix;
   - write a second PDF with a 128-byte password and confirm qpdf rejects both
     the full input and its 127-byte prefix;
   - open the corresponding outputs through flpdf with the qpdf-observed
     passwords, allowing weak crypto for R=5.

Run the narrow tests and record the expected failures. Do not implement until
the RED signal is confirmed:

```bash
cargo test -p flpdf encryption::password::tests::r5_preserves_password_bytes
cargo test -p flpdf encryption::standard::tests::r5_salted_hash_ --lib
cargo test -p flpdf --test qpdf_v5_password_parity
```

### 2. GREEN: move the responsibility boundary

1. Remove the 127-byte slice from `r5_salted_hash`; retain qpdf's streaming
   write order: password, salt, extra.
2. Remove the pre-slice in `r6_password_hash`; let Algorithm 2.B's repeated
   input use the complete password passed by the caller.
3. Add a private helper in `encryption/standard.rs` that returns the first 127
   bytes without allocation.
4. At the start of each of `check_user_password_r5`,
   `check_owner_password_r5`, `check_user_password_r6`, and
   `check_owner_password_r6`, pass the helper's prefix to the corresponding
   decrypt routine. Keep the lower-level decrypt/hash functions raw.
5. Remove `truncate_to` and its V=5 truncation branch from
   `normalize_password`; retain mode resolution, hex decoding, UTF-8
   validation, raw-byte pass-through, and legacy behavior. Update comments so
   they distinguish normalization from qpdf's encryption check boundary.
6. Update `V5R6EncryptParams` and hash/compute docs that currently claim the
   hash helper truncates passwords.

Run the focused tests until all are green:

```bash
cargo test -p flpdf encryption::password::tests --lib
cargo test -p flpdf encryption::standard::tests --lib
cargo test -p flpdf --test qpdf_v5_password_parity
```

### 3. Refactor and parity verification

1. Check `git diff --check` and `cargo fmt --all -- --check`.
2. Run the focused reader/security tests:

```bash
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test encrypt_writer_smoke
cargo test -p flpdf --lib
```

3. Run workspace quality gates:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
```

4. Run the repository's fresh patch-coverage command for the final diff. If
   the qpdf-gated test is skipped because qpdf is unavailable, report that
   explicitly; do not replace it with an unverified claim.

### 4. Beads and handoff

1. Review the final diff to confirm only the planned security/test/spec files
   changed and `reader.rs`/`writer.rs` are untouched.
2. Read back `bd show flpdf-4br7`, run `bd dep cycles`, and add only evidence
   supported by the completed tests to the issue notes.
3. Run `bd close flpdf-4br7` only after all acceptance criteria pass.
4. Push Beads and git according to the repository session-close instructions;
   verify the exact success output before reporting completion.

## Failure handling

- If any baseline or focused test fails for an unrelated pre-existing reason,
  stop and report the exact failure before modifying production code.
- If qpdf output disagrees with the proposed boundary, re-check the pinned
  qpdf source and probe before changing the design.
- Do not add a compatibility adapter to `reader.rs` or a writer-only hash
  variant to make a test pass.
