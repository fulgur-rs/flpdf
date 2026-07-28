# Top-level Normalize Content Consumer Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make qpdf-shaped top-level `--normalize-content=y|n` use the existing `ContentNormalizer` production path, including qpdf 11.9.0's QDF default and explicit-`n` override.

**Architecture:** The top-level parser retains qpdf's three states (`unset`, explicit `y`, explicit `n`) with `Option<CliYesNo>`. A single resolver applies the qpdf Writer rule—QDF enables normalization only when the option was not explicitly set—and passes the resulting policy to the shared `run_rewrite` consumer. `run_rewrite` remains the sole CLI owner of page traversal and delegates stream bytes to the existing `normalize_content_stream`; its linearized route performs the same mutation before constructing both the plan and write graphs.

**Tech Stack:** Rust 2021, Clap, existing `flpdf::content_normalizer`, qpdf 11.9.0 source and `/usr/bin/qpdf` oracle, Cargo integration tests, qtest, Clippy, and `cargo llvm-cov`.

## Global Constraints

- Treat qpdf 11.9.0 `QPDFJob::normalize_set` and `QPDFWriter::normalize_content_set` as the state/precedence oracle.
- Reuse `CliYesNo`, `run_rewrite`, `apply_normalize_content`, and `normalize_content_stream`; do not add another normalizer or parser-only compatibility option.
- Preserve `unset` separately from explicit `n`: `--qdf` defaults to normalization, while `--qdf --normalize-content=n` disables it.
- Apply normalization before a linearization plan is computed, and apply it identically to the independent write graph.
- Reject unsupported dispatch combinations rather than silently dropping the requested transformation.
- Keep `--stream-data`, `--decode-level`, and specialized filter work out of scope.
- Follow RED→GREEN→REFACTOR and finish with fresh 100% changed executable-line coverage against `origin/main`.

## Task 1: Specify top-level state and QDF precedence

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_tests.rs`
- Modify: `crates/flpdf-cli/src/main.rs`
- Read-only oracle: `libqpdf/QPDFJob.cc:2850-2870`
- Read-only oracle: `include/qpdf/QPDFJob.hh:638-639`
- Read-only oracle: `libqpdf/QPDFWriter.cc:192-195,2078-2083`

- [x] Add integration tests using a repository-authored one-page PDF whose content bytes visibly differ after normalization. Assert top-level explicit `y` transforms, explicit `n` preserves, QDF unset behaves like `y`, and QDF explicit `n` overrides that default.
- [x] Run the focused tests and verify RED because top-level Clap rejects `--normalize-content`.
- [x] Add `normalize_content: Option<CliYesNo>` to `Cli` and a small resolver that maps `(option, qdf)` to the effective boolean without collapsing explicitness early.
- [x] Replace both top-level hard-coded `false` arguments with the resolved policy and keep inspection/page-operation conflicts explicit.
- [x] Run focused tests to GREEN, refactor only shared policy duplication, and re-run.

## Task 2: Preserve normalization through linearization

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_tests.rs`
- Modify: `crates/flpdf-cli/src/main.rs`

- [x] Add a RED integration test proving top-level `--linearize --normalize-content=y` changes content while producing a valid linearized PDF.
- [x] In `run_rewrite`'s linearize branch, normalize the planning graph before `LinearizationPlan::from_pdf_with_object_stream_mode` and normalize the independently opened write graph before `write_linearized`.
- [x] Preserve warning order/deduplication and exit code 3 for lexical recovery, matching the non-linearized route.
- [x] Run the focused tests to GREEN and refactor the duplicated page traversal into one helper if the tests expose useful duplication.

## Task 3: Verify qtest gain and delivery gates

- [ ] Run `cargo fmt -- --check`.
- [ ] Run `cargo test -p flpdf-cli --test cli_tests`.
- [ ] Run qtest `basic-parsing` and confirm subtests 64 and 65 pass without regressions.
- [ ] Run `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [ ] Run `cargo test`.
- [ ] Produce fresh LLVM coverage and run `scripts/patch-coverage.sh origin/main`; require 100%.
- [ ] Commit and push the feature branch, then push Beads state. Leave the Bead open until merged-main evidence exists.
