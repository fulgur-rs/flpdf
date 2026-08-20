# QPDFJob --check Linearization Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make flpdf `--check` treat a valid linearized PDF like qpdf 11.9.0: report the linearization line, exit 0 when no other diagnostics exist, and print the clean two-line tail.

**Architecture:** Keep the already-portable `Pdf::is_linearized` detector and the existing CLI summary formatter. Remove only the check-library advisory that incorrectly turns a detected linearized document into a warning; parser, stream, repair, and linearization-check diagnostics remain owned by their existing paths.

**Tech Stack:** Rust workspace, flpdf check library, flpdf-cli integration tests, pinned qpdf 11.9.0 oracle.

**Spec:** `docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`

## Global Constraints

- qpdf 11.9.0 source and live output are authoritative.
- Do not reintroduce `linearized_hint_ref`, a second detector, or a CLI-only special case.
- Preserve qpdf exit semantics: clean check 0, warning-only check 3, error check 2.
- Use RED→GREEN tests and fresh changed-line patch coverage at 100%.
- Keep `flpdf-25kg.3.1` and the QPDFJob consumer stack open until the PR is integrated.

**Scope boundary:** The regression uses the repository's qpdf-validated
`linearized-one-page.pdf` fixture and covers the clean `--check` status path:
the informational line, exit 0, and the two-line tail. It does not claim to
complete qpdf's deeper hint-table cross-validation; that existing
`check-linearization` responsibility and its physical-offset follow-up remain
tracked by `flpdf-1quo`.

---

### Task 1: Lock the qpdf regression at the CLI boundary

**Files:**
- Modify: `crates/flpdf/tests/check_tests.rs`
- Modify: `crates/flpdf-cli/tests/cli_check_exitcodes.rs`

**Interfaces:**
- Consumes: existing `check_reader` report and `--check` CLI invocation.
- Produces: regression coverage proving linearization is informational, not a warning.

- [x] **Step 1: Write the failing assertions**

Use `tests/fixtures/compat/linearized-one-page.pdf`, assert the library report has no linearization warning, and assert the CLI emits `File is linearized`, the clean qpdf tail, and exit 0 without `operation succeeded with warnings`.

- [x] **Step 2: Run the focused tests to verify RED**

Run `cargo test -p flpdf --test check_tests` and `cargo test -p flpdf-cli --test cli_check_exitcodes`. The new/updated linearization assertions must fail because current main emits the advisory and exits 3.

### Task 2: Remove the incorrect advisory

**Files:**
- Modify: `crates/flpdf/src/check.rs`

**Interfaces:**
- Consumes: `Pdf::is_linearized()` and `CheckReport` diagnostics.
- Produces: an unchanged `CheckSummary.linearized` value with no synthetic warning.

- [x] **Step 1: Delete only the synthetic warning branch**

Keep the `linearized` result in `CheckSummary`; remove the `Diagnostic::warning` whose message says rewrite support does not recompute tables. Do not change parser repair diagnostics, content-stream decoding, or `check_linearization` APIs.

- [x] **Step 2: Run the focused tests to verify GREEN**

Run `cargo test -p flpdf --test check_tests` and `cargo test -p flpdf-cli --test cli_check_exitcodes`; all tests must pass, including clean, warning-only, and error exit cases.

### Task 3: Verify qpdf parity and hand off the PR

**Files:**
- Inspect: `crates/flpdf/src/check.rs`, `crates/flpdf-cli/src/main.rs`
- Inspect: `crates/flpdf/tests/check_tests.rs`, `crates/flpdf-cli/tests/cli_check_exitcodes.rs`

**Interfaces:**
- Consumes: the RED→GREEN regression and qpdf 11.9.0 probes.
- Produces: a rebased, CI-green, Ready PR linked to `flpdf-u1ro` and `flpdf-s5cw`.

- [x] **Step 1: Run the oracle differential**

Compare `qpdf --check tests/fixtures/compat/linearized-one-page.pdf` with `cargo run --quiet --bin flpdf -- --check tests/fixtures/compat/linearized-one-page.pdf`; require exit 0, identical stdout, and no flpdf warning summary.

- [ ] **Step 2: Run repository quality gates**

Run `cargo fmt --all -- --check`, focused tests, `cargo test --workspace`, all-features clippy, strict private-item rustdoc, qpdf module-doc checks, and `scripts/patch-coverage.sh --base origin/main`; changed executable lines must be fully covered.

- [ ] **Step 3: Commit, rebase, push, and create a non-draft PR**

Rebase onto latest `origin/main`, push the feature branch, create the PR with qpdf source/probe/test/coverage/Beads evidence, wait for all CI and `codecov/patch`, and leave it Ready without merging.
