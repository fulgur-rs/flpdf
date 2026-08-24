# flpdf CLI qpdf-compatible ArgParser Design

## Goal

Create a CLI-owned qpdf-compatible argument grammar boundary for flpdf. The
initial consumer is the `flpdf-w5ny` `basic-parsing` option slice, but the
boundary must be reusable for later qpdf-compatible CLI options.

The parser owns argv grammar and dispatch shape. Existing reader, writer, and
job consumers continue to own feature semantics and output behavior.

## Oracle and current gap

The semantic oracle is pinned qpdf 11.9.0:

- `libqpdf/qpdf/QPDFArgParser.hh` defines the option-table model, including
  main, help, and named subparser tables.
- `libqpdf/QPDFArgParser.cc:433-555` implements qpdf's argument scan: both
  `-option` and `--option`, attached `=value`, positional dispatch, choice
  validation, and `--` table termination.
- `libqpdf/QPDFJob_argv.cc:38-125` registers qpdf's main and named option
  tables from generated definitions.

The current flpdf CLI splits the same responsibility across
`rewrite_qpdf_single_dash`, `normalize_qpdf_bare_equals`,
`QPDF_BARE_LONG_OPTIONS`, `QpdfArgSegment`, `extract_overlay_groups`, and
`extract_attachment_groups` in `crates/flpdf-cli/src/main.rs`. This permits
an option to be listed as qpdf-compatible without being registered in the
actual top-level parser.

## Design

### Ownership boundary

Add a CLI-only `arg_parser` module with an `ArgParser` type. It owns:

- canonical recognition of long and single-dash long spellings;
- attached `=value` handling for bare and value-bearing options;
- the main option table and value-terminated segment state;
- segment-local sub-option recognition;
- `--` behavior inside a named segment versus at top level;
- preservation of positional operands and qpdf-style grammar errors.

The parser does not open PDFs, resolve page ranges, build encryption settings,
or choose reader/writer behavior. Those remain in the existing feature
consumers.

During the migration, clap may remain the typed feature-value validator. The
qpdf parser is the source of truth for token state and segment boundaries;
clap is only a temporary downstream consumer of the canonical residual argv.
This avoids a second feature implementation while removing grammar logic from
`main.rs`.

### Interface

The module exposes a parser result containing:

- canonical residual argv for the existing clap dispatch;
- raw named segments, preserving declaration order and their terminators;
- enough segment identity for `main.rs` to pass overlay and attachment tokens
  to their existing semantic parsers.

The raw segment representation is intentionally feature-neutral. Overlay
range validation and attachment metadata validation remain outside the parser.

### Migration

1. Move the grammar helpers and their unit tests into `arg_parser.rs`.
2. Replace the three preprocessing calls in `main` with one parser invocation.
3. Keep existing overlay and attachment semantic parsing, but consume the
   parser's raw segments.
4. Add regression tests for the qpdf forms needed by `flpdf-w5ny`, including
   top-level options that are currently present only on `rewrite`.
5. Remove the old helper constants and duplicate state-machine paths after
   all callers use the module.

The initial slice does not implement qpdf completion, `@argfile` expansion,
help generation, or feature semantics that have no current flpdf consumer.
Those are separate responsibilities and must not be approximated by parser
special cases.

## Error and compatibility rules

- Unknown feature values remain the responsibility of the existing typed
  consumer until that consumer migrates into the parser configuration layer.
- Unterminated named segments and invalid grammar boundaries fail before any
  PDF operation starts.
- A top-level `--` stops option recognition and preserves every following token
  verbatim.
- A named segment's `--` closes that segment and resumes top-level parsing.
- Single-dash long forms normalize to the same canonical option as double-dash
  forms; real short options and numeric operands remain unchanged.
- No core `flpdf` reader or writer API changes are required for this parser
  slice.

## Verification

The parser module will have focused unit tests for:

- bare, required-value, optional-value, and choice-shaped options;
- `-foo`, `--foo`, and `--foo=value` equivalence;
- top-level and named-segment `--` behavior;
- segment-local sub-options and opaque positional values;
- unknown options, missing terminators, and misplaced operands.

CLI integration tests will verify that the canonical argv reaches the existing
consumer paths and that qpdf-shaped invocations do not regress existing
`cli_tests` behavior. qpdf 11.9.0 probes remain the authority for ambiguous
grammar and diagnostics.
