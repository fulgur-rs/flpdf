# Signed-PDF external validation — future CI extension

This document describes how cryptographic validation of signed-PDF preservation
could be added to CI as a future extension. It accompanies the structural
preservation matrix in
[`crates/flpdf/tests/signed_preservation_matrix.rs`](../crates/flpdf/tests/signed_preservation_matrix.rs)
(issue `flpdf-9hc.22.8`).

## Why the matrix is structural, not cryptographic

flpdf's signed-PDF contract is enforced *structurally*:

- **Detection** reads `/AcroForm /Fields … /FT /Sig … /V … /ByteRange`.
- **Refusal** rejects destructive full rewrites of signed documents.
- **`--remove-restrictions`** explicitly strips `/SigFlags` bits and signature
  `/V` values when the user opts into invalidation.
- **Incremental update** appends a new generation *after* the original bytes,
  leaving every region covered by a signature's `/ByteRange` **bit-identical**.

The last point is the cryptographically meaningful invariant: a PKCS#7
signature signs exactly the bytes its `/ByteRange` enumerates. If those bytes
are preserved byte-for-byte, the signature remains valid; if any change, it
breaks. The matrix asserts this byte-identity directly, so it verifies the
preservation property **without** needing a real cryptographic signature.

The committed fixtures are therefore **synthetic**: their `/Contents` is a
`<00>` placeholder rather than a real PKCS#7 blob. This keeps the test suite
deterministic, dependency-free, and immune to certificate expiry (see below).

## What real cryptographic validation would add

A future CI extension could additionally confirm that an *externally produced,
genuinely signed* PDF survives flpdf's incremental update path with its
signature still reported valid by an independent verifier. This guards against
a hypothetical bug where the byte-range bookkeeping is correct on synthetic
fixtures but mishandles some real-world signing layout.

### Tooling

| Tool | Role | Notes |
|------|------|-------|
| **pyhanko** (Python) or **endesive** (Python) | **Create** signed fixtures | `pip install pyhanko` (+ `pyhanko[pkcs11]` not needed). Generates a real PKCS#7-signed PDF from a key/cert pair. |
| **OpenSSL** | Generate a self-signed test cert/key | Already available in CI images. |
| **pdfsig** (poppler-utils) | **Verify** signatures | `apt-get install poppler-utils`. Reports signature validity, signer, and `/ByteRange` coverage. |
| **qpdf** | Structural cross-check only | **Cannot create or cryptographically verify signatures** — it is a structure/transform tool. Already installed in CI. |

> **Important:** neither `qpdf` nor `pdfsig` can *produce* a signature. The
> issue text's phrase "qpdf-signed" is a misnomer; signing requires a signer
> library such as pyhanko/endesive.

### Sketch of a future CI job

```bash
# 1. One-time (or fixture-build step): create a self-signed cert and a real
#    signed PDF. Commit the resulting *.signed.pdf as a binary fixture, OR
#    regenerate it in CI to avoid committing certs/keys.
openssl req -x509 -newkey rsa:2048 -keyout key.pem -out cert.pem \
  -days 3650 -nodes -subj "/CN=flpdf-test"
pyhanko sign addsig --field Sig1 \
  pkcs12  # or: pemder --key key.pem --cert cert.pem \
  unsigned.pdf signed.pdf

# 2. Run flpdf incremental update on the signed PDF (must NOT full-rewrite).
flpdf rewrite signed.pdf out.pdf        # incremental path, no --remove-restrictions

# 3. Verify the signature still validates after flpdf's incremental update.
pdfsig out.pdf
#   Expect: "Signature #1: ... - Signature is Valid."
#   and the same /ByteRange digest as the input.
```

### Caveat: certificate expiry

`pdfsig`'s *validity* verdict depends on the signing certificate's validity
window and trust chain. A committed signed fixture will eventually report
"certificate has expired" or "untrusted", making a naive `pdfsig … | grep
Valid` assertion **fail with age** rather than because flpdf regressed. A
future CI job should either:

- regenerate the signed fixture in-job (fresh cert each run), or
- assert on **`/ByteRange` digest preservation** (signature *integrity*) rather
  than full chain *validity* — i.e. compare the message digest over the signed
  ranges before and after flpdf's incremental update, which is exactly the
  property the structural matrix already covers.

This is the primary reason the in-tree matrix stays structural: it tests the
invariant flpdf actually controls (byte preservation) without coupling CI
stability to certificate lifetimes or trust stores.

## Prerequisites checklist for enabling the extension

- [ ] Add `poppler-utils` (`pdfsig`) to the CI tool install step.
- [ ] Add a fixture-build step using pyhanko/endesive (Python) + OpenSSL.
- [ ] Decide: commit binary signed fixtures vs. regenerate per run (prefer
      regenerate to dodge expiry).
- [ ] Gate the cryptographic checks so a missing tool skips rather than fails
      local `cargo test` runs (the structural matrix has no external deps).
