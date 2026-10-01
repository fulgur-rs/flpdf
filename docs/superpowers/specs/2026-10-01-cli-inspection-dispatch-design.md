# CLI Inspection Dispatch Design

## Context

`flpdf-3yn9.48.180` tracks the remaining CLI inspection routes that bypass
`QPDFJob::run()`. The original standalone top-level inspection paths have
already been removed. Live `main` still routes the flpdf-native
`check-linearization`, `pages`, and `show-encryption` subcommands through
public inspection wrappers that open a document and complete the Job
separately.

The qpdf 11.9.0 CLI has one execution sequence in `qpdf/qpdf.cc`: construct a
`QPDFJob`, call `initializeFromArgv`, call `run`, then return
`getExitCode`. `QPDFJob::run` owns `createQPDF` followed by `writeQPDF`
(`libqpdf/QPDFJob.cc:513-520`); `writeQPDF` selects inspection/output and
performs the shared warning completion (`:483-511`); the inspection branches
run in one fixed order (`:1645-1693`).

## Goal

Keep flpdf's native subcommand syntax while dispatching its remaining
inspection commands through the same `QPDFJob::run()` lifecycle used by the
equivalent qpdf flag routes.

## Non-goals

- Do not change CLI command names, flags, positional arguments, or usage text.
- Do not change public library APIs or the behavior of the inspection wrapper
  methods for library consumers.
- Do not fold page selection, extraction, rewrite, attachment mutation, or
  other multi-Job CLI pipelines into one new abstraction.
- Do not remove the post-`write_qpdf` status query: qpdf itself calls
  `getExitCode` after `run`, and flpdf's `get_exit_code` is a pure query.
- Do not remove warning-state handoff between page-selection and writer Jobs;
  that state bridges distinct Rust Job instances and represents qpdf's
  same-Job warning fold.

## Design

Keep the existing command parser and dispatch arms. For the native
`check-linearization`, `pages`, and `show-encryption` handlers:

1. Build one `QPDFJob` with the existing CLI logger, warning, password, and
   input settings.
2. Set the existing `QPDFJobConfig` inspection flags corresponding to the
   command: `check_linearization`, `show_npages`, `show_pages`, or
   `show_encryption`.
3. Pass the input through the Job's configured open path rather than opening a
   `Pdf` before creating the Job.
4. Let `QPDFJob::run()` own the create, inspection, warning-completion, and
   exit-status sequence.

The `check` subcommand already uses `run()` and remains unchanged. The
`pages` subcommand selects exactly one report based on `--show-npages`; the
default remains `show_pages`. The `show-encryption` subcommand continues to
omit `--show-encryption-key`, which is a separate flpdf subcommand. Existing
qpdf-compatible top-level inspection flags remain unchanged.

The wrong-password `show-encryption` path must keep qpdf's partial-document
behavior: report encryption details, preserve the current warning delivery,
skip subsequent transformations when authentication failed, and return the
same exit status. `QPDFJob::create_qpdf` already models that path for the
configured `show_encryption` flag (`libqpdf/QPDFJob.cc:432-448` is the qpdf
oracle). Repair and password options must reach the Job open boundary before
parsing the input.

## Acceptance criteria

- The listed native inspection handlers no longer call the public
  `show_npages`, `show_pages`, `check_linearization`, `show_encryption`, or
  `complete_report` wrappers directly from the CLI production path.
- They enter through `QPDFJob::run()` with the corresponding existing config
  flags and input policy.
- Differential tests compare each native subcommand with its equivalent qpdf
  top-level flag route. They assert exit code, stdout, stderr, warning order,
  and warning summary for normal, repaired/warning, and encrypted inputs;
  the wrong-password encryption report receives explicit coverage.
- Existing CLI argument and job-route contract tests continue to pass.
- Update the E-7/E-19/E-21 route-matrix rows only where a fresh caller audit
  supports the revised classification; this slice does not claim that
  unrelated multi-Job operations have become a single qpdf Job.

## Verification

Run the focused qpdf differential cases for `check-linearization`, both
`pages` report variants, and `show-encryption`, then run:

- `cargo fmt --all -- --check`
- `cargo test -p flpdf-cli`
- workspace all-feature Clippy with warnings denied
- strict workspace rustdoc with private items
- the CI patch-coverage gate and route-matrix checks

Finally, inspect the exact PR head and require every check on that head to pass
before marking the PR Ready. Do not merge or close `flpdf-3yn9.48.180` until
the implementation is present on live `main` and the parent acceptance
criteria are verified.
