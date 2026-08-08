# Hybrid Xref Stream Type Review Fix Design

## Scope

Address PR #684's `/XRefStm` review finding: a classic trailer must not accept
an ordinary stream as a hybrid xref stream merely because it carries
xref-shaped keys and data. This change is limited to xref-stream identity
validation. It does not include the separate review suggestion to optimize
hybrid-entry merging.

## Oracle decision

Classification: oracle match.

qpdf 11.9.0's `QPDF::read_xrefStream` accepts the target only when
`xref_obj.isStreamOfType("/XRef")` succeeds (`libqpdf/QPDF.cc:949-967`).
The existing flpdf `parse_xref_stream` accepts every `Object::Stream`, so both
the startxref and `/XRefStm` paths are too permissive.

A minimal PDF with a classic xref table and an `/XRefStm` pointing at an
otherwise parseable stream that lacks `/Type /XRef` made
`qpdf --check` report `xref not found` at the stream offset and enter recovery.

## Design

Keep the responsibility at `parse_xref_stream`, immediately after the object
is confirmed to be a stream and before its dictionary is treated as an xref
trailer. Require the stream dictionary's `Type` key to be the name `XRef`.
Return the existing `xref not found` parse error when it is absent or differs.

Centralizing this check matches qpdf's `read_xrefStream` boundary and covers
both direct xref-stream start offsets and hybrid `/XRefStm` targets. Adding a
check only in `merge_xref_stream_from_classic_trailer` is excluded because it
would leave direct streams semantically divergent.

## TDD and verification

1. Add a hybrid-PDF regression test whose target stream has valid `/Size`,
   `/W`, `/Index`, and data but no `/Type /XRef`; assert that flpdf returns
   `xref not found`.
2. Run that test first and confirm it fails because the current implementation
   accepts the stream.
3. Add the smallest dictionary-name validation in `parse_xref_stream`.
4. Re-run the focused xref suite and the real-PDF qpdf probe, then run fmt,
   denied-warning clippy, workspace tests, module-doc checks, and fresh
   changed-line coverage before pushing the PR branch.

## GitHub handling

After verification and push, reply to the original Thread 1 with the oracle
classification, qpdf source location, probe result, and verification. Leave
the thread unresolved unless explicitly requested. Thread 2 remains outside
this scope.
