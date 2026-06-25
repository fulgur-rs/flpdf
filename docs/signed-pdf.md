# Signed-PDF policy & scope

A digital signature covers a byte range of the file (`/ByteRange`); any edit
that shifts those bytes invalidates the signature.

**Pre-v1.0 posture (qpdf-compatible).** flpdf's pre-v1.0 goal is byte-for-byte
qpdf compatibility, so flpdf matches qpdf's handling of signed PDFs: a full
rewrite of a signed PDF **proceeds** (it is not refused), leaving the signature
objects present-but-invalid — exactly as `qpdf in.pdf out.pdf` and
`qpdf in.pdf --pages in.pdf <range> -- out.pdf` do (both exit 0, with the
`/FT /Sig` field and its `/ByteRange` preserved, and no warning). flpdf does not
silently *remove* signature evidence either; the objects survive (a verifier
will report the signature as invalid/tampered, which is the honest signal).

> **Deferred improvement (>= v1.0).** A *preserve-by-default* protection that
> refuses (or warns about) operations that would invalidate a signature is a
> potential post-v1.0 improvement. It is intentionally **not** implemented
> pre-v1.0 because it diverges from qpdf. Tracked in `flpdf-hn1g.14`.

## Out of scope: signature *generation*

**flpdf does not create digital signatures.** It detects, preserves, and
(on request) strips them, but it never *signs* a PDF. This matches qpdf,
which also does not generate signatures. A signing capability is a possible
future roadmap item, tracked separately; it is intentionally excluded from
the scope described here.

## What flpdf does with signed PDFs

flpdf recognizes signed PDFs by walking the AcroForm field tree and
collecting any field whose (inherited) `/FT` is `/Sig` or that carries a
`/ByteRange` entry. Indirect references are resolved during this walk. The
`/AcroForm` `/SigFlags` bits `/SignaturesExist` (bit 1) and `/AppendOnly`
(bit 2) are read and surfaced. Note that `/AppendOnly` is currently
*informational only* — it is reported but does not change the
strip/preserve decision, and there is no enforcement layer that
rejects non-append modifications on its basis.

There are three outcomes, depending on the operation and flags:

### 1. Full rewrite — proceed (default, qpdf-compatible)

A **full rewrite** of a signed PDF proceeds. Renumbering and re-serializing
objects relocates the signed byte ranges, so the existing signature no longer
validates; the signature objects themselves are preserved (present-but-invalid),
matching qpdf. No diagnostic is printed and the command exits 0. The
[`signature_rewrite_impact`] query API still reports that a full rewrite *would*
invalidate signatures, for callers that want to make their own decision, but the
writer no longer refuses on that basis.

### 2. Strip (explicit opt-in)

If you genuinely want to discard the signatures and produce a modified
file, pass `--remove-restrictions`. This is the only opt-in flag — there is
no `--remove-signatures`. It is available both as a top-level alias and on
the `rewrite` subcommand:

```bash
flpdf rewrite --remove-restrictions input.pdf output.pdf
```

When signatures are removed, flpdf prints a warning so the loss is never
silent:

```text
flpdf: warning: removed signatures; signatures are now invalidated
```

`--remove-restrictions` is the qpdf `--remove-restrictions` equivalent: it
strips encryption and advisory permission restrictions, and clears signature
fields' `/V` and `/SigFlags`. It does **not** bypass authentication — an
auth-requiring input without a working `--password` is rejected exactly as a
plain `rewrite` would reject it.

### 3. Preserve (incremental update)

To modify a signed PDF *without* invalidating the existing signatures, use
the incremental-update path, which appends a new xref/trailer section and
leaves the original signed bytes (and their `/ByteRange`) untouched.

The incremental path is taken when a full rewrite is *not* forced. The full
rewrite is forced by the default `--remove-unreferenced-resources=auto`, so
to stay on the signature-preserving incremental path, disable it:

```bash
flpdf rewrite --remove-unreferenced-resources=no input.pdf output.pdf
```

(On a plain rewrite this flag does not actually remove any `/Resources`
entries — matching qpdf, which prunes resource entries only during page
operations. Its only plain-rewrite effect is this full-rewrite/incremental
toggle.)

This succeeds on a signed input without emitting the "removed signatures"
warning, because the signed byte ranges are preserved. This behaviour is
covered by the `incremental_rewrite_of_signed_pdf_succeeds_without_warning`
test in `crates/flpdf-cli/tests/cli_full_rewrite.rs`.

## Summary

| Operation                                                    | Signatures                  |
| ------------------------------------------------------------ | --------------------------- |
| `flpdf rewrite` (full rewrite, default)                      | **Preserved, invalidated**  |
| `flpdf rewrite --remove-restrictions`                        | Stripped (warned)           |
| `flpdf rewrite --remove-unreferenced-resources=no`           | **Preserved, valid**        |
| Signature generation                                         | Not supported               |

[`signature_rewrite_impact`]: https://docs.rs/flpdf/latest/flpdf/fn.signature_rewrite_impact.html
