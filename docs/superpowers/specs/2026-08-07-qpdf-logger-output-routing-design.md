# QPDFLogger and output routing design

**Issue:** `flpdf-qynx.4`

**Oracle:** qpdf 11.9.0 at pinned commit `3b97c9bd`

## Goal

Port qpdf's `QPDFLogger` responsibility as a public shared pipeline router, attach it to
document warnings, and migrate every supported qpdf-equivalent CLI info, warning, error, and
binary-save route away from direct `println!`, `eprintln!`, and stdout writes.

The result must preserve qpdf's output bytes, ordering, exit status, save-to-stdout collision
rules, and caller ownership of custom sinks.

## Scope

The implementation covers:

- `QPDFLogger::create` and the process-global default logger;
- shared `info`, `warn`, `error`, and optional `save` pipeline handles;
- standard-output, standard-error, and discard terminals;
- null reset, warning-following-error, `only_if_not_set`, and identical-sink no-op behavior;
- stdout-use tracking and `saveToStandardOutput` collision prevention;
- `setOutputStreams`-equivalent output/error reset;
- `Pdf` warning accumulation plus immediate logger delivery and warning suppression;
- supported CLI commands corresponding to qpdf `QPDFJob` info/warn/error/save consumers.

The following are out of scope:

- the aggregate `QPDFJob` lifecycle owned by `flpdf-25kg.5.2`;
- C ABI compatibility and `qpdflogger-c`;
- qpdf operations not otherwise supported by flpdf;
- flpdf-only inspection commands with no qpdf `QPDFJob` counterpart;
- writer-global output-pipeline redesign beyond the declared CLI save consumers;
- `Pl_Function`, which still has no independent production consumer.

## Oracle responsibility

`QPDFLogger` owns routing rather than message semantics. Its members contain one shared handle for
each built-in terminal and one shared handle for each current route. `warn` follows the current
`error` route only while its own route is null. The save route is independently optional.

The private `Pl_Track` in `QPDFLogger.cc:9-40` records whether the standard-output pipeline has
received any write. `QPDFLogger::setSave` checks that state before assigning stdout, reroutes info
from stdout to stderr, and enables binary stdout (`QPDFLogger.cc:189-209`). Resetting info while
save still points at stdout selects stderr; resetting it after save is cleared selects stdout
(`QPDFLogger.cc:160-171`).

`QPDF::warn` has two responsibilities in a fixed order: append the warning to the document's
warning collection, then, unless suppressed, write the formatted warning to the document logger
(`QPDF.cc:487-494`). The flpdf port retains both responsibilities rather than replacing the
existing diagnostic collection.

## Shared pipeline representation

Add a public cloneable `PipelineHandle` backed by an `Arc` around one mutex-protected boxed
`Pipeline + Send`. Clones preserve pointer identity, matching qpdf's copied
`std::shared_ptr<Pipeline>`. Identity checks are used only where qpdf compares shared pointers:
save no-op, standard-output selection, and info rerouting.

`PipelineHandle` remains inside the pipeline responsibility:

- raw `write` and `finish` return `PipelineResult`;
- the mutex is an ownership/synchronization mechanism, not a new error source;
- a poisoned mutex is recovered with its inner value rather than producing a qpdf-incompatible
  panic or error branch.

Refactor `PlOStream` to own its generic `W: Write` value. Passing `&mut W` continues to express an
externally owned writer, while moving `Stdout`, `Stderr`, or a custom owned writer into the stage
makes a `'static + Send` logger handle possible. The stage still never closes the underlying
destination and retains its existing sticky write/flush behavior.

## Logger public boundary

`QPDFLogger` is a cloneable handle whose clones share one logger state. It provides:

- `create()` and `default_logger()`;
- `info`, `warn`, and `error` byte/string delivery;
- `get_info`, `get_warn`, `get_error`, `get_save`, and `get_save_if_set`;
- `standard_output`, `standard_error`, and `discard`;
- `set_info`, `set_warn`, `set_error`, and `set_save` using `Option<PipelineHandle>` for qpdf null
  reset;
- `save_to_standard_output`;
- `set_output_streams` using optional pipeline handles.

The logger state owns the real stdout/stderr terminals and a tracked stdout wrapper. Destruction
finishes only the built-in stdout and stderr pipelines. Assigning a custom sink never transfers
finish ownership to the logger.

The process-global default is stored in `OnceLock<QPDFLogger>`. A private logger created for a CLI
invocation or document is independent of that singleton, while clones of either logger retain
identity.

## Error boundary

Logger contract errors do not use `PipelineError`. qpdf throws them from `QPDFLogger` itself, not
from a `Pipeline` subclass:

- requesting an unset save pipeline without the nullable form;
- assigning standard output to save after stdout has already been used.

These methods return `crate::Result` and map qpdf `std::logic_error` to `Error::Internal`, following
`error.rs`. Calls from `QPDFLogger::info`, `warn`, or `error` convert downstream `PipelineError`
through the existing `From<PipelineError> for Error`, preserving Logic -> Internal and Runtime ->
System classification and the original message.

Only `PipelineHandle::write` and `PipelineHandle::finish` expose `PipelineResult`, because those
operations remain inside the pipeline boundary.

## Document warning flow

Extend `PdfOpenOptions` with:

- `logger: Option<QPDFLogger>`, where null selects the default logger;
- `suppress_warnings: bool`, defaulting to false like qpdf;
- `description: String`, corresponding to the qpdf input-source name.

`QPDFLogger` uses shared-pointer identity for `PartialEq`/`Eq`, allowing `PdfOpenOptions` to retain
its current derives.

The resolver stores the selected logger, suppression flag, and input description beside the
diagnostic collection. Every lazy warning:

1. appends a `Diagnostic` with its message and optional offset;
2. releases the resolver-core mutable borrow;
3. when not suppressed, formats `WARNING: <description>[ (offset N)]: <message>\n`;
4. writes that exact byte sequence through the logger's warn route.

The xref loader currently creates initial repair diagnostics before the resolver exists. After
the logger-aware resolver is constructed, those diagnostics are replayed once in their original
order. No other output occurs between xref loading and this replay, so the observable warning
order matches delivery during loading without threading a logger through a second parsing
implementation.

`Pdf` exposes logger get/set and warning-suppression get/set operations backed by the resolver so
later library operations observe replacements, matching qpdf's live document logger.

## CLI cutover

Each CLI invocation creates one private logger and shares it with every opened document and
output consumer. The CLI supplies the input path as `PdfOpenOptions::description`.

The warning integration and removal of the old `emit_warnings_since` output path land in the same
stack layer. This prevents an intermediate branch from printing each warning both immediately and
again from the accumulated diagnostic collection. The collection remains the source for warning
counts and exit status.

Commands that save binary bytes to stdout configure `save_to_standard_output(true)` before any
info output or document operation that can emit info. This covers supported JSON stdout, raw or
filtered stream stdout, attachment stdout, and PDF output to `-`. Human-readable output goes
through info, warning text through warn, and fatal command text through error.

Direct stdout/stderr calls are removed only for qpdf-equivalent routes in the declared scope.
flpdf-only inspection commands remain unchanged and are listed explicitly in the final inventory.

## Stacked delivery

The issue is delivered as three dependent branches and pull requests:

1. **Logger core:** `PipelineHandle`, owned/generic `PlOStream`, `PlTrack`, public `QPDFLogger`, and
   source-level logger tests.
2. **Document warnings:** logger-aware `PdfOpenOptions`/resolver, warning suppression, initial and
   lazy warning delivery, and the CLI warning cutover in the same layer.
3. **CLI info/save:** remaining qpdf-equivalent info/error/save routes, binary stdout collision
   protection, differential tests, correspondence documentation, and obsolete direct-route
   deletion.

Before layer 3 edits `docs/qpdf-correspondence.md`, synchronize with the active `flpdf-h8mv`
worktree's change or stop on unresolved overlap. Do not overwrite or duplicate that work.

## Acceptance and verification

Layer 1 ports the three scenarios in qpdf `libtests/logger.cc` and its stdout/stderr goldens:

- default info/warn/error destinations and unset-save error;
- discard/reset behavior and stdout-use collision;
- save-first info rerouting and restoration;
- warning following error until independently assigned;
- shared handle identity and caller-owned custom finish lifecycle.

Layer 2 tests:

- open-time repair warning capture with description and offset;
- lazy warning delivery in append order;
- retained `Diagnostics` snapshots and warning counts;
- suppression without loss from the collection;
- logger replacement on a live document;
- no duplicate CLI warning and unchanged warning exit status.

Layer 3 differentially tests qpdf 11.9.0 stdout, stderr, and exit status for:

- clean, warning, and error completion;
- JSON written to stdout;
- raw and filtered stream output;
- attachment output;
- info rerouting before binary save;
- custom/file output not marking standard output used.

Every production behavior is developed RED -> GREEN. Each stack layer must pass focused tests,
`cargo fmt --all -- --check`, denied-warning workspace clippy, workspace tests, applicable qpdf
differentials, and fresh 100% changed executable-line coverage before publication.
