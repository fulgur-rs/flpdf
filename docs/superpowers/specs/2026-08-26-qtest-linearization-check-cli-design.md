# qtest `linearization.test` check CLI completion

## Goal

Make the remaining qpdf inspection failures in `linearization.test` pass at
the qpdf responsibility boundary: generic `--check` must use qpdf's fixed
completion text, and top-level `--no-warn --check` must accept the option,
retain warning status 3, and suppress warning delivery.

This slice is tracked by `flpdf-25kg.5.5.1`. The independent writer golden
failures are tracked separately by `flpdf-25kg.6.19`.

## Evidence and semantic authority

The current `flpdf/main` plus `flpdf-qtest/main` run is 303/309. Rows 16 and
17 differ only in the final noun: qpdf says `errors that qpdf cannot detect`,
while flpdf says `errors that flpdf cannot detect`. Row 309 rejects
`qpdf --no-warn --check lin3.pdf` with usage status 2. A live qpdf 11.9.0
probe returns status 3, emits the normal four-line check block, and emits no
warning lines.

Pinned qpdf source is authoritative:

- `libqpdf/QPDFJob.cc:800-801` emits the literal `errors that qpdf cannot detect`.
- `include/qpdf/QPDFJob.hh:589` defaults the job message prefix to `qpdf`.
- `libqpdf/QPDFJob_config.cc:407-410` implements `noWarn()` as
  `suppress_warnings = true`.
- `libqpdf/QPDFJob.cc:651-665` installs warning suppression on the document.
- `libqpdf/QPDFJob.cc:493-504` keeps warning state for exit status while
  suppressing completion warning text.

The qtest `shim/qpdf` already provides the explicit `FLPDF_PROGNAME` boundary
used to present the Rust binary as `qpdf`; the shim must set that value when it
delegates the `qpdf` command. Vendored qtest sources and expected outputs stay
unchanged.

## Design

The CLI owns top-level option selection, `QPDFJob` owns warning/completion
state, and `PdfOpenOptions`/`Pdf` own document warning delivery. The top-level
`--check` route passes its parsed `no_warn` state into the same `QPDFJob`
instance that opens and checks the document. The check consumer continues to
accumulate warning state, but manually emitted warnings honor the document's
suppression state. The clean-note text is a direct translation of qpdf's fixed
source string, not an executable-name substitution.

The qtest shim exports `FLPDF_PROGNAME=qpdf` only for its delegated qpdf
process. Native flpdf invocations retain the existing `flpdf` diagnostic
prefix, while qtest observes the qpdf command boundary.

## Testing

The RED evidence is the existing qtest failure plus focused Rust assertions:

- a CLI check with `--no-warn` returns status 3 and contains no `WARNING:`;
- the clean check note contains qpdf's fixed wording;
- the qtest shim forwards `FLPDF_PROGNAME=qpdf` to its target.

After implementation, focused Rust tests, qtest shim tests, and an isolated
`linearization.test` run must show rows 16, 17, and 309 passing. No vendored
fixture, golden file, or manifest attribution is changed in this slice.

## Scope boundary

This slice does not change linearization planning, object-stream membership,
or `lin-special` golden output. Those changes belong to `flpdf-25kg.6.19` and
will be stacked only after this CLI slice is independently green.
