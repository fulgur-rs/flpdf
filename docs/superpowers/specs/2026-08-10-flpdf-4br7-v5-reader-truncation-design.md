# flpdf-4br7: V=5 password truncation at the security boundary

**Status:** Draft for review

## Context

flpdf currently truncates V=5 passwords inside the shared `r5_salted_hash`
and `r6_password_hash` primitives. Those primitives are used by both the
writer-side `/U`/`/UE`/`/O`/`/OE` construction and the reader-side password
checks. Consequently, the writer hashes only the first 127 bytes, while qpdf
hashes the complete writer password.

The implementation must also respect the ongoing policy of shrinking
`reader.rs`. The fix must not add another password-semantic branch to that
facade or make it the long-term owner of Standard security authentication.

## qpdf oracle facts

The pinned qpdf 11.9.0 source is authoritative:

- `libqpdf/QPDF_encryption.cc:171-174` defines `truncate_password_V5` as a
  127-byte prefix operation.
- `libqpdf/QPDF_encryption.cc:239-313` implements `hash_V5` by streaming the
  password, salt, and extra data without truncating the password.
- `libqpdf/QPDF_encryption.cc:520-529` and `:569-579` truncate before the user
  and owner password checks.
- `libqpdf/QPDF_encryption.cc:665-689` truncates before the reader's key
  recovery path.
- `libqpdf/QPDF_encryption.cc:601-635` and `:1180-1204` pass raw passwords to
  writer-side parameter construction.

Live qpdf 11.9.0 probes establish the observable consequence: a file written
with a 127-byte password opens with a longer input whose first 127 bytes match;
a file written with a 128-byte password does not open with either that full
input or its 127-byte prefix, because qpdf's reader truncates the supplied
input while the writer used all 128 bytes.

## Design

### Responsibility boundary

The qpdf-equivalent reader-side boundary in flpdf is the Standard security
handler, not `reader.rs`:

1. `r5_salted_hash` and `r6_password_hash` become raw hash primitives. They
   consume every byte supplied by their caller.
2. The four V=5 authentication entry points in
   `crates/flpdf/src/encryption/standard.rs` truncate their password argument to
   127 bytes before calling the decrypt/validation helpers:
   `check_user_password_r5`, `check_owner_password_r5`,
   `check_user_password_r6`, and `check_owner_password_r6`.
3. The password encoding step remains available to the reader path, but
   Unicode mode validates UTF-8 and preserves the supplied bytes. qpdf's
   reader-side implementation does not apply the SASLprep mentioned in its
   specification comment. V=5 truncation is removed from the generic
   normalization helper and remains beside the corresponding authentication
   checks.
4. Writer-side `compute_u_ue_r5`, `compute_o_oe_r5`, `compute_u_ue_r6`, and
   `compute_o_oe_r6` continue to pass their raw password bytes to the hash
   primitives. No writer-specific compatibility branch is introduced.

`reader.rs` and `writer.rs` are therefore not implementation targets for this
change. The security layer owns both the raw hash primitive and the
reader-only truncation boundary.

### Helper shape

Add one private, slice-preserving helper in the security module for the qpdf
operation, returning `&[u8]` and applying `min(127)` without allocating. Use
it explicitly at each V=5 check entry point so the call sites remain visible
and correspond to qpdf's user/owner checks. Do not put the clamp inside either
hash primitive or the shared decrypt helper, since those lower-level functions
must remain usable by writer construction with untruncated input.

### Password normalization

`normalize_password` continues to resolve `PasswordMode` and decode hex
bytes. Unicode mode validates UTF-8 and returns the original bytes, matching
qpdf 11.9.0; it no longer applies SASLprep. The helper also does not perform
the encryption-specific 127-byte truncation. The reader passes the validated
bytes to the security checks, which apply the qpdf truncation immediately
before authentication.

## Tests and acceptance criteria

Tests will be added or updated in the security module and a non-`writer.rs`
integration test:

1. R=5 and R=6 raw-hash tests prove that a password longer than 127 bytes
   changes the digest when its suffix changes, with expected values derived
   independently from the qpdf/algorithm oracle.
2. Direct user and owner authentication tests prove that a longer supplied
   password is truncated at the security check boundary for both revisions.
3. A qpdf-gated integration test writes/opens R=5 and R=6 cases with 127-byte
   and 128-byte passwords. It checks the qpdf-observed split: a 127-byte
   writer password accepts a matching longer reader input, while a 128-byte
   writer password is rejected by qpdf with both the full input and the
   127-byte prefix. The same cases are checked through flpdf where applicable.
4. Existing short-password vectors and encryption round trips remain green.
5. `reader.rs` and `writer.rs` remain unchanged by this issue.

Verification will include the focused security/integration tests, formatting,
workspace clippy with denied warnings, the relevant crate tests, and the
repository's patch-coverage gate.

## Alternatives rejected

- Keeping truncation inside `r5_salted_hash`/`r6_password_hash` preserves the
  current flpdf behavior but violates qpdf writer semantics.
- Adding a new truncation branch to `reader.rs` would reproduce the behavior
  in the wrong shrinking layer and leave direct security entry points
  semantically incomplete.
- Adding a writer-only raw-hash variant would preserve the mixed bridge rather
  than making the shared primitive match qpdf's `hash_V5` responsibility.

## Scope boundary

This issue changes only V=5 password truncation ownership and its tests. It
does not unify R=5/R=6 hashing, alter public APIs, or modify the qpdf source
mirror. The later reader-parity follow-up `flpdf-1f1y` removes the separate
SASLprep mismatch after live qpdf 11.9.0 verification.
