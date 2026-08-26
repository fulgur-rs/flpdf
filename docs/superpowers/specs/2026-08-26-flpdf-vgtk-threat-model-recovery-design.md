# Design — flpdf-vgtk: recovery attack-surface documentation

## Goal

Synchronize `docs/threat-model.md` with the recovery behavior already present
in flpdf and qpdf 11.9.0. This is a documentation-only correction.

## Verified behavior

qpdf has one document-wide `attempt_recovery` permission, defaulting to `true`
(`include/qpdf/QPDF.hh:1458-1462`). `QPDF::setAttemptRecovery` stores the
permission (`libqpdf/QPDF.cc:334-336`), and parsing/recovered stream-length
handling consults it (`QPDF.cc:451-468,1391-1396`). qpdf exposes the explicit
opt-out as `--suppress-recovery` (`QPDFJob_config.cc:633-635`), which
`QPDFJob::setQPDFOptions` applies with `QPDF::setAttemptRecovery(false)`
(`QPDFJob.cc:650-659`). qpdf has no `--repair` option.

The live qpdf 11.9.0 probe on
`tests/fixtures/compat/null-length-framing-matrix.pdf` emitted recovery
warnings in the default `--check` path. With `--suppress-recovery`, it emitted
only the initial malformed stream-shape warnings; both checks returned qpdf's
warning exit status 3.

flpdf already matches this permission model: `PdfOpenOptions::default()` sets
`repair: true` (`crates/flpdf/src/reader.rs:97-114`), and
`RecoveryArgs::suppress_recovery` is wired through
`pdf_open_options_with_password_bytes` (`crates/flpdf-cli/src/main.rs:1613-1621,
5984-6000`). The retained flpdf `--repair` boolean does not disable or newly
enable anything because recovery is default-enabled; `--suppress-recovery` is
the explicit strict opt-out.

## Change boundary

- Update only `docs/threat-model.md`.
- Mark the document as reviewed on 2026-08-26.
- Replace the stale “strict” versus “repair — widest surface” opening rows with
  a default-recovery row covering all public open paths, plus a recovery-policy
  note naming `--suppress-recovery` and the inert `--repair` spelling.
- Update the introductory trust-boundary paragraph to include default recovery
  and explicit suppression.
- Do not change `PdfOpenOptions`, CLI parsing, warning behavior, or any legacy
  bridge.

## Acceptance criteria

1. `docs/threat-model.md` no longer classifies `Pdf::open`/`Pdf::open_mem` as
   strict-only or claims that flpdf lacks qpdf's suppression control.
2. The document names qpdf's single recovery permission and distinguishes the
   default-recovery and explicit-suppression paths.
3. No Rust production file changes are needed or made.
4. Existing recovery-related CLI tests, qpdf module-doc checks, deviation
   marker checks, formatting, strict rustdoc, all-features clippy, and workspace
   tests remain green.
