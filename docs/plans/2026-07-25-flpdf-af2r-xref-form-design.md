# flpdf-af2r: qpdf-compatible plain xref form selection

## Problem

For a full rewrite with `ObjectStreamMode::Disable`, qpdf 11.9.0 writes a
classic xref table even when the source PDF uses an xref stream. flpdf instead
inherits the source form through `Pdf::last_xref_form()` and writes a new
`/Type /XRef` stream.

qpdf chooses the standard output form from the final object-stream membership:
it writes an xref stream only when at least one output object is stored in an
ObjStm container. The input xref form is not part of this decision.

## Considered approaches

1. Derive the form from `PlainWritePlan`'s final placements. This matches
   qpdf's responsibility boundary and also fixes Preserve on an xref-stream
   source that has no surviving ObjStm members.
2. Special-case `ObjectStreamMode::Disable`. This fixes the reported command
   but leaves the same incorrect source-form fallback in other modes.
3. Preserve the source form and add a later writer override. This duplicates
   planning knowledge in the serializer and allows plan validation to observe
   a form that will not be emitted.

Approach 1 is selected.

## Design

`PlainWritePlan::build` already computes `has_object_stream` from the final
`PlannedIndirectObject` placements. Set `TrailerPlan.form` to
`XrefForm::Stream` exactly when that value is true; otherwise select
`XrefForm::Table`.

The existing pre-routing normalization continues to suppress Preserve and
Generate under a forced PDF version below 1.5. No serializer, object-placement,
stream-compression, or incremental-writer behavior changes.

`TrailerPlan::structural_filtered` remains derived from
`effective_stream_policy`. Live qpdf 11.9.0 comparison confirms that
`--stream-data=preserve` and `--stream-data=uncompress` produce unfiltered xref
streams, while `--stream-data=compress` produces a filtered xref stream.

## Testing

- Add a qpdf 11.9.0 `--static-id --object-streams=disable` golden for
  `null-visible-matrix-objstm.pdf`.
- Add a byte-identical regression test that fails on the current source-form
  fallback and passes after the form selection change.
- Update plan-level tests whose old expectations encode the incorrect inherited
  xref-stream behavior.
- Keep the existing Preserve and Generate tests that require xref streams when
  ObjStm containers are present.
- Run focused tests, workspace formatting/clippy/tests, and committed-HEAD patch
  coverage with a 100% changed-line gate.

## Non-goals

- Incremental xref form selection.
- Linearized or encrypted writer paths.
- Structural stream compression policy changes.
- Refactoring xref serialization.
