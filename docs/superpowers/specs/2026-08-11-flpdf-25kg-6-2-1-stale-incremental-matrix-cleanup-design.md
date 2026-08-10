# Stale incremental matrix cleanup design

## Context

PR #710 (`Add incremental output semantic matrix`) was merged after the
qpdf-style `PdfWriter` full-rewrite cutover in PR #716. The merge added
`crates/flpdf-cli/tests/incremental_matrix_tests.rs`, but that test still
imports `flpdf::WriteOptions` and calls `flpdf::write_pdf_with_options`, both
of which were intentionally removed by the cutover. As a result, the merged
`main` branch cannot compile the workspace test suite.

This follow-up is a stale-consumer cleanup. It is not a request to restore the
old incremental PDF output route.

## Decision

Remove the stale incremental matrix test and its obsolete plan document:

- `crates/flpdf-cli/tests/incremental_matrix_tests.rs`
- `docs/superpowers/plans/2026-08-10-flpdf-25kg-6-2-incremental-matrix.md`

Keep `PdfWriter` as the only PDF document-output writer. Do not add aliases,
adapters, compatibility exports, or a replacement incremental writer. The
existing `PdfWriter` contract tests and CLI full-rewrite tests remain the
authoritative output coverage.

## Oracle and responsibility boundary

The parent cutover follows qpdf 11.9.0's `QPDFWriter` responsibility boundary:

- `include/qpdf/QPDFWriter.hh` defines the writer as a fresh document-output
  writer.
- `libqpdf/QPDFWriter.cc:88-109` initializes fresh output state.
- `libqpdf/QPDFWriter.cc:2008-2025` removes `/Prev` from the rewritten
  trailer.

Therefore the old matrix's source-prefix, appended-revision, `/Prev`, and
incremental xref assertions describe a removed producer route. Existing
reader support for `/Prev`, and incremental serialization used by JSON or
Pipeline APIs, are separate responsibilities and are not changed by this
follow-up.

## Scope

### In scope

- Delete the stale integration test that references removed writer APIs.
- Delete the plan that specifies the removed incremental-output implementation.
- Preserve the current full-rewrite tests and all production writer code.
- Document the post-merge ordering conflict in the pull request.

### Out of scope

- Reintroducing `WriteOptions` or `write_pdf_with_options`.
- Reintroducing PDF incremental output, source-prefix preservation, or
  append-only signature behavior.
- Rewriting the old 968-line matrix against `PdfWriter`; its assertions are
  for the removed incremental producer.
- Changing reader `/Prev` parsing or JSON/Pipeline incremental delivery.

## Verification

Run the following from the follow-up worktree:

```text
cargo fmt --all -- --check
cargo test --workspace
```

Also confirm by inspection that the diff contains only the two stale-file
deletions and no production API or writer-route restoration. The workspace
test must compile and pass, while the existing `PdfWriter` contract and CLI
full-rewrite coverage remain part of the executed suite.

## PR handoff

The PR should link Bead `flpdf-25kg.6.2.1`, explain that PR #710 landed after
PR #716, and state that the fix removes obsolete consumers rather than
restoring an API that is outside the qpdf 11.9.0 writer boundary.
