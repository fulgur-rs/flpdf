# PR #672 Warning Failure Review Design

## Goal

Address the three inline review threads on PR #672 according to pinned qpdf
11.9.0 behavior: deliver recovery warnings even when document opening ultimately
fails, preserve logger failures across the check APIs, and reject the proposed
stream-finish change where it conflicts with qpdf.

## Oracle classification

### Terminal open failures: oracle match

qpdf installs its logger on the document before parsing. During xref recovery,
`QPDF::reconstruct_xref` calls `warn` for the damage marker, triggering error,
and reconstruction marker before later reconstruction work can fail
(`libqpdf/QPDF.cc:315-318, 516-530`). `QPDF::warn` first records the warning and
then writes it immediately to the configured warning pipeline
(`libqpdf/QPDF.cc:488-494`).

flpdf currently selects `PdfOpenOptions.logger` only after
`load_xref_state_with_repair` succeeds. An `Error::OpenFailure` therefore
returns its accumulated diagnostics without delivering them. This is an oracle
gap.

### Check API logger failures: oracle match

A custom qpdf warning pipeline may throw a runtime or logic error from
`QPDF::warn`; that infrastructure failure is not a malformed-PDF diagnostic.
flpdf maps those pipeline categories to `Error::System` and `Error::Internal`,
but the repair-enabled check path currently converts both to an
`Ok(CheckReport { valid: false, ... })`. The check APIs must propagate these
two categories while continuing to downgrade ordinary input/open failures.

### Stream cleanup after logger failure: oracle mismatch

`QPDF::pipeStreamData` reports a caught stream failure by calling
`qpdf_for_warning.warn(...)` inside its catch arms
(`libqpdf/QPDF.cc:2505-2530`). The common cleanup tail is later at lines
2531-2537. If the warning pipeline itself throws, exception unwinding exits the
function before that tail. flpdf's current early return therefore matches qpdf;
finishing the stream pipeline before returning the logger error would introduce
different behavior.

## Design

### Warning policy boundary

Select the effective logger before loading xref state. Keep warning formatting
and suppression in one resolver-owned primitive rather than duplicating it in
`engine.rs`.

`ResolverWarningOptions` will own the reusable operation that routes a
`Diagnostics` snapshot through the selected logger. Both paths use it:

- successful repair load: construct the resolver and replay the initial
  diagnostics exactly once;
- terminal repair failure: replay diagnostics attached to
  `Error::OpenFailure`, then return the original open error if delivery
  succeeds, or the logger error if delivery fails.

This ordering matches qpdf: already-raised warnings are observable before the
terminal error, and the first logger failure aborts processing.

### Check error boundary

In repair mode, `check_reader_inner_with_options` will propagate
`Error::Encrypted`, `Error::System`, and `Error::Internal`. Other open failures
remain an invalid `CheckReport` diagnostic. Public rustdoc will describe this
distinction.

No new public error variant is introduced. `Error::System` and
`Error::Internal` already represent qpdf's runtime/logic exception channels and
are sufficient for the check boundary.

### Stream cleanup thread

No production change will be made. The inline reply will cite
`QPDF.cc:2505-2537` and `QPDFLogger.cc:96-101`, explaining that warning-pipeline
failure unwinds before the common finish tail. The thread remains unresolved
unless the user separately requests resolution.

## Tests

- Add a real corrupt-PDF fixture builder whose recovery collects warnings and
  then fails for a missing trailer. With a recording logger, assert the warning
  bytes are delivered in order before `Error::OpenFailure` is returned.
- With the same terminal failure and a failing warning sink, assert the logger
  `Error::System` takes precedence.
- Call `check_reader_with_options` with a repair-triggering PDF and a failing
  logger; assert it returns `Error::System`, not `Ok(CheckReport)`.
- Run existing stream-pipeline tests as focused evidence for the third thread;
  do not add a test that encodes behavior qpdf does not have.

## Delivery

After RED to GREEN implementation, run focused logger/check/resolver tests,
formatting, denied-warning clippy, workspace tests, module-doc validation,
changed-line coverage, and `git diff --check`. Push only
`feature/flpdf-qynx.4-document-warnings`, wait for PR #672 CI, reply once in
each original inline thread with classification and evidence, and read back the
threads. Do not merge or resolve them.
