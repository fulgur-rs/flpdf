# qpdf-compatible CLI ArgParser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace flpdf-cli's scattered qpdf argv preprocessing with one CLI-owned parser boundary that preserves qpdf 11.9.0 option-table and value-terminated-segment behavior.

**Architecture:** Add crates/flpdf-cli/src/qpdf_args.rs with a QpdfArgParser that owns grammar state, canonical option spelling, bare-option equals handling, and raw named-segment extraction. It returns canonical residual argv plus feature-neutral raw segments; existing clap, overlay, attachment, reader, and writer consumers remain responsible for feature semantics.

**Tech Stack:** Rust 2021, clap 4 derive/command metadata, assert_cmd CLI tests, qpdf 11.9.0 source and executable oracle.

**Spec:** docs/superpowers/specs/2026-08-24-flpdf-qpdf-arg-parser-design.md

## Global Constraints

- qpdf 11.9.0 source and observed behavior are authoritative.
- Work only in .worktrees/flpdf-9hc-23-1-qpdf-arg-parser on branch feature/flpdf-9hc-23-1-qpdf-arg-parser.
- No core crates/flpdf reader/writer API changes in this parser slice.
- Parser code owns argv grammar only; feature consumers own PDF semantics and output parity.
- Every production change follows RED, verify failure, minimal GREEN, verify passing.
- Preserve qpdf's distinction between a named segment terminator -- and the top-level option terminator --.
- Preserve existing qpdf-shaped behavior for -o, -h, numeric operands, dash-prefixed segment operands, and global -- passthrough.
- Do not add qpdf completion, @argfile, or feature semantics without a current flpdf consumer.

---

### Task 1: Define the parser result and grammar tables

Files:
- Create: crates/flpdf-cli/src/qpdf_args.rs
- Modify: crates/flpdf-cli/src/main.rs near the crate module/import declarations
- Test: crates/flpdf-cli/src/qpdf_args.rs unit tests

Interfaces:
- ParsedQpdfArgs contains residual_args: Vec<String> and ordered raw named segments.
- A raw named segment contains its canonical option name and captured tokens; it does not parse page ranges, encryption, or attachment metadata.
- QpdfArgParser::from_command(command).parse(args) returns CliResult<ParsedQpdfArgs>.

- [ ] Step 1: Write the failing parser interface tests.

Use a minimal clap command and assert the wished-for API:

    #[test]
    fn parser_returns_canonical_residual_args_and_raw_segments() {
        let command = clap::Command::new("flpdf")
            .arg(clap::Arg::new("qdf").long("qdf"));
        let parsed = QpdfArgParser::from_command(command)
            .parse(vec!["flpdf".into(), "-qdf".into(), "input.pdf".into()])
            .expect("qpdf argv should parse");
        assert_eq!(parsed.residual_args, ["flpdf", "--qdf", "input.pdf"]);
        assert!(parsed.named_segments.is_empty());
    }

Add a second test that captures a raw overlay segment without running PageRange::parse or any PDF operation.

- [ ] Step 2: Run the focused test and verify the expected RED failure.

    cargo test -p flpdf-cli qpdf_args --quiet

Expected: compilation failure because qpdf_args and its parser result do not exist.

- [ ] Step 3: Implement the minimal parser result and explicit grammar types.

    pub(crate) struct ParsedQpdfArgs {
        pub(crate) residual_args: Vec<String>,
        pub(crate) named_segments: Vec<QpdfNamedSegment>,
    }

    pub(crate) struct QpdfNamedSegment {
        pub(crate) option: String,
        pub(crate) tokens: Vec<String>,
    }

    pub(crate) struct QpdfArgParser {
        known_long_options: HashSet<String>,
        bare_long_options: HashSet<String>,
    }

from_command may seed ordinary option names and aliases from clap during the staged migration, but parser-owned compatibility names must include ignore-xref-streams, object-streams, and stream-data. Keep the named segment table for encrypt, pages, add-attachment, copy-attachments-from, overlay, and underlay inside this module.

- [ ] Step 4: Run the focused tests and verify GREEN.

    cargo test -p flpdf-cli qpdf_args --quiet

Expected: the interface and result-shape tests pass with no production consumer changes.

- [ ] Step 5: Commit the parser interface slice.

    git add crates/flpdf-cli/src/qpdf_args.rs crates/flpdf-cli/src/main.rs
    git commit -m "refactor(cli): add qpdf argv parser boundary"

### Task 2: Move canonical spelling and option-state scanning into the parser

Files:
- Modify: crates/flpdf-cli/src/qpdf_args.rs
- Test: crates/flpdf-cli/src/qpdf_args.rs
- Reference/remove after migration: qpdf grammar helpers in crates/flpdf-cli/src/main.rs

Interfaces:
- QpdfArgParser::parse(Vec<String>) -> CliResult<ParsedQpdfArgs> owns the raw argv state machine.
- Existing semantic parsers continue receiving canonical residual tokens and raw segment tokens.

- [ ] Step 1: Write RED tests for qpdf spelling and terminator behavior.

Cover these individual behaviors: -qdf and --qdf equivalence; bare --check=ignored normalization; preservation of --object-streams=generate; named-segment -- resuming top-level parsing; top-level -- passthrough; segment-local -to=1 normalization; and preservation of -1, -0.5, and -oPATH.

- [ ] Step 2: Run the tests and verify each RED failure.

    cargo test -p flpdf-cli qpdf_args --quiet

Expected: the new assertions fail because the parser state machine is not implemented.

- [ ] Step 3: Implement the parser state machine from qpdf's call order.

1. Stop at a top-level -- and copy remaining tokens verbatim.
2. Recognize -foo and --foo as the same long option and extract attached =value only after the option name.
3. Keep real short options and numeric operands unchanged.
4. Track named segments after encrypt, pages, add-attachment, copy-attachments-from, overlay, and underlay.
5. Inside a named segment, normalize only that table's sub-options and treat non-option tokens as opaque positional values.
6. Treat a named segment's -- as its end and resume the main table.
7. Discard attached values only for bare options; preserve values for choices and required/optional parameters.

Keep grammar errors in the parser. Do not call feature-specific validators from this module.

- [ ] Step 4: Run the focused tests and verify GREEN.

    cargo test -p flpdf-cli qpdf_args --quiet

- [ ] Step 5: Commit the grammar state machine.

    git add crates/flpdf-cli/src/qpdf_args.rs
    git commit -m "feat(cli): match qpdf argv grammar"

### Task 3: Move raw overlay and attachment extraction behind the parser

Files:
- Modify: crates/flpdf-cli/src/qpdf_args.rs
- Modify: crates/flpdf-cli/src/main.rs
- Test: crates/flpdf-cli/src/qpdf_args.rs
- Test: existing main.rs unit tests while migrating callers

Interfaces:
- main calls one parser entry point before Cli::parse_from.
- main maps raw QpdfNamedSegment values to existing parse_overlay_segment, parse_add_attachment_segment, and parse_copy_attachments_segment functions.
- No overlay, attachment, or encryption semantic validation moves into qpdf_args.rs.

- [ ] Step 1: Write RED migration tests for ordering and opacity.

Cover repeated overlay/underlay order, all add-attachment groups with only the first dispatch segment retained in residual argv, overlay tokens inside encrypt/pages/copy-attachment segments remaining opaque, equals-form segment starters discarding the bare option's attached value, and unterminated segments producing grammar errors.

- [ ] Step 2: Run the tests and verify RED.

    cargo test -p flpdf-cli qpdf_args --quiet

Expected: migration assertions fail against the old scattered helpers.

- [ ] Step 3: Migrate main to QpdfArgParser.

Replace the sequence calling rewrite_qpdf_single_dash, normalize_qpdf_bare_equals, extract_overlay_groups, and extract_attachment_groups with one parser invocation. Convert raw named segments into the existing semantic OverlaySpec and attachment argument types in main. Delete old grammar constants, segment-state enum, and duplicate extraction state machines only after all callers use qpdf_args.

- [ ] Step 4: Run parser and existing CLI tests and verify GREEN.

    cargo test -p flpdf-cli qpdf_args --quiet
    cargo test -p flpdf-cli --test cli_tests --quiet

Expected: parser tests and the baseline 199 non-ignored CLI tests pass.

- [ ] Step 5: Commit the consumer migration.

    git add crates/flpdf-cli/src/qpdf_args.rs crates/flpdf-cli/src/main.rs
    git commit -m "refactor(cli): route segment parsing through qpdf args"

### Task 4: Add end-to-end qpdf-shaped CLI regression coverage

Files:
- Modify: crates/flpdf-cli/tests/cli_tests.rs
- Modify: crates/flpdf-cli/src/qpdf_args.rs only for parser corrections identified by tests
- Reference: crates/flpdf-cli/tests/cli_object_streams.rs, cli_stream_data.rs, and cli_optimization_matrix.rs

Interfaces:
- Tests exercise the actual flpdf binary, not only parser internals.
- Existing writer/reader behavior remains the oracle for the later flpdf-w5ny consumer slice.

- [ ] Step 1: Write RED integration tests.

Add binary tests for equivalent -qdf and --qdf forms, named segments not consuming following top-level options, and dash-prefixed segment operands remaining operands.

- [ ] Step 2: Run each test and verify the expected RED result.

    cargo test -p flpdf-cli --test cli_tests qpdf_ --quiet

Expected: argument-shape failures before migration, not PDF writer failures.

- [ ] Step 3: Implement only minimal integration fixes.

Adjust parser registration or residual-argv construction only where RED tests identify a grammar mismatch. Do not add top-level reader or writer semantics here; those belong to flpdf-w5ny and flpdf-9hc.17.7.

- [ ] Step 4: Run the complete CLI regression suite.

    cargo test -p flpdf-cli --test cli_tests --quiet
    cargo test -p flpdf-cli --test cli_object_streams --quiet
    cargo test -p flpdf-cli --test cli_stream_data --quiet
    cargo test -p flpdf-cli --test cli_optimization_matrix --quiet

Expected: zero failures; qpdf-dependent tests may retain repository skip behavior when qpdf is unavailable.

- [ ] Step 5: Commit the regression coverage.

    git add crates/flpdf-cli/tests/cli_tests.rs crates/flpdf-cli/src/qpdf_args.rs
    git commit -m "test(cli): cover qpdf argument grammar"

### Task 5: Run quality gates and hand off the implementation branch

Files:
- Verify all changed files; no generated fixtures, target files, or unrelated worktree files.

- [ ] Step 1: Run formatting and focused checks.

    cargo fmt --all -- --check
    cargo test -p flpdf-cli --test cli_tests --quiet

- [ ] Step 2: Run workspace quality gates.

    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace

- [ ] Step 3: Inspect final diff and repository state.

    git diff --check origin/main...HEAD
    git status --short --branch
    git log --oneline --decorate -5

Confirm no generated fixtures, target files, or unrelated worktree files are included.

- [ ] Step 4: Push the verified branch and persist tracker state.

    bd dolt push
    git push

- [ ] Step 5: Report branch, commits, tests, and qpdf skips.

Do not close the parser issue; implementation completion only makes the dependent flpdf-w5ny consumer eligible to proceed.
