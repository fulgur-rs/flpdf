# qpdf Progress Reporting Implementation Plan

> For agentic workers: use superpowers:executing-plans or superpowers:subagent-driven-development to execute this plan task by task. Steps use checkbox syntax.

Goal: Implement the qpdf 11.9.0 --progress CLI consumer so progress-reporting.test reaches the qpdf-compatible output, routing, and PDF comparison.

Architecture: Reuse the canonical PdfWriter event accounting and QPDFJob::configure_writer_progress fallback. Add only the missing CLI option/configuration and output identity; do not duplicate a callback in flpdf-cli, modify vendored qtest code, or add a legacy bridge.

Tech Stack: Rust workspace, clap, assert_cmd, predicates, qpdf 11.9.0, flpdf-qtest, Beads/Dolt.

Spec: docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md.

## Global constraints

- Pinned qpdf 11.9.0 source and /usr/bin/qpdf 11.9.0 are semantic and output-routing oracles.
- qpdf ownership is QPDFJob configuration -> QPDFWriter reporter; preserve that boundary.
- Keep the existing flpdf-25kg.7 parent-child blocking edge unchanged.
- All flpdf source commits go to branch flpdf-25kg-7-10-progress in the dedicated worktree, never main.
- Keep qpdf-qtest vendor files pristine. Ledger and allowlist edits belong to a separate /tmp flpdf-qtest checkout.
- Treat the same-run harness.log and qtest-results.xml pair as the only qtest promotion evidence.
- Every production change follows RED -> GREEN TDD. Changed executable lines must pass patch coverage.

## Files

- crates/flpdf/src/job/lifecycle.rs: qpdf-shaped progress-requested state setter.
- crates/flpdf-cli/src/main.rs: top-level and rewrite parsing plus canonical writer registration.
- crates/flpdf-cli/tests/progress_reporting.rs: real subprocess routing regressions.
- docs/qpdf-correspondence.md: source-backed CLI consumer note.
- /tmp/flpdf-qtest-25kg-7-10-progress/allowlist.txt: three target acceptance names.
- /tmp/flpdf-qtest-25kg-7-10-progress/parity/qtest-11.9.0.jsonl: three target passing rows.

### Task 1: RED CLI tests

- [ ] Create crates/flpdf-cli/tests/progress_reporting.rs with a real one-page fixture and tempfile directories.
- [ ] Add a file-output test invoking --progress --deterministic-id INPUT OUTPUT. Assert exit 0, empty stderr, output existence, and stdout exactly:
~~~text
flpdf: OUTPUT: write progress: 0%
flpdf: OUTPUT: write progress: 99%
flpdf: OUTPUT: write progress: 100%
~~~
Replace OUTPUT only with the test's hand-derived temporary path.
- [ ] Add a stdout-output test invoking --progress --deterministic-id INPUT -. Assert exit 0, stdout starts with %PDF-1.7 followed by an LF, and stderr exactly:
~~~text
flpdf: standard output: write progress: 0%
flpdf: standard output: write progress: 99%
flpdf: standard output: write progress: 100%
~~~
- [ ] Run cargo test -p flpdf-cli --test progress_reporting. The current baseline must fail with clap's unexpected argument --progress error before implementation code is written.
- [ ] Commit only the RED test with git add crates/flpdf-cli/tests/progress_reporting.rs and git commit -m "test(cli): specify qpdf progress output routing".

### Task 2: GREEN canonical job and CLI route

- [ ] Add QPDFJob::set_progress(&mut self, value: bool) in crates/flpdf/src/job/lifecycle.rs. It must assign only self.configuration.progress and document correspondence to QPDFJob::Config::progress at libqpdf/QPDFJob_config.cc:478-481.
- [ ] Add a private configure_cli_progress helper in crates/flpdf-cli/src/main.rs. It creates a QPDFJob using cli_logger and progname, sets the output path and progress=true, then calls the existing QPDFJob::configure_writer_progress. Do not implement the message callback in main.rs.
- [ ] Add progress: bool with false default to WriterOptions.
- [ ] Add a bare --progress bool option to Cli and RewriteCommand. Document that it reports approximate write progress and mirrors qpdf.
- [ ] Call configure_cli_progress from write_with_pdf_writer after stdout/save routing is prepared and after writer configuration, before writer.write().
- [ ] Pass progress through top-level ordinary rewrite, top-level linearize, page-operation option construction, and Commands::Rewrite option construction. Preserve the final output path when the writer is memory-backed.
- [ ] Change write_qpdf_to_memory to accept the final output Path and attach the same reporter before writing. Pass the output from every existing call site.
- [ ] Add a native rewrite --progress test to progress_reporting.rs and assert the same three lines and a valid output file.
- [ ] Run cargo test -p flpdf-cli --test progress_reporting. All three tests must pass, with no test stderr and stdout-output remaining a valid PDF.
- [ ] Run cargo test -p flpdf --test job_lifecycle_tests and cargo test -p flpdf-cli --test cli_tests.
- [ ] Commit the production route and tests with git add crates/flpdf/src/job/lifecycle.rs crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/progress_reporting.rs and git commit -m "feat(cli): wire qpdf progress reporting".

### Task 3: qpdf differential and qtest acceptance

- [ ] Build the release binaries from the committed feature worktree:
~~~text
cargo build --release --bin flpdf --bin flpdf-test-compare --bin flpdf-test-driver --bin qpdfjob-ctest --bin qpdf-ctest --bin flpdf-test-pdf-doc-encoding --bin flpdf-test-pdf-unicode --bin flpdf-test-unicode-filenames --bin test_xref --bin test_parsedoffset --bin flpdf-test-large-file
~~~
- [ ] Create /tmp/flpdf-qtest-25kg-7-10-progress from the clean flpdf-qtest main checkout. Run the vendored progress-reporting.test against a disposable copied datadir and the release shim binaries with TESTS=progress-reporting and -stdout-tty=0. Keep qtest.log separate from harness.log.
- [ ] Confirm progress-reporting 1, 2, and 3 each pass; confirm the two generated PDFs compare equal and /usr/bin/qpdf --check accepts both.
- [ ] Change only the three progress-reporting rows in parity/qtest-11.9.0.jsonl from blocked to passing. Set rationale, owner, bead, and replacement_ref to null.
- [ ] Add the three exact suite names to allowlist.txt in sorted order. Do not promote merge-and-split rows, which remain owned by the Phase 4/5 page-operation work.
- [ ] Run the validators against the same artifacts:
~~~text
python3 scripts/verify-allowlist.py survey/latest/harness.log survey/latest/qtest-results.xml survey/latest/qtest-summary.md
python3 scripts/verify-parity-manifest.py survey/latest/harness.log survey/latest/qtest-results.xml parity/qtest-11.9.0.jsonl
~~~
- [ ] Require zero validation errors and three target passes before committing the qtest checkout with git -C /tmp/flpdf-qtest-25kg-7-10-progress add allowlist.txt parity/qtest-11.9.0.jsonl and git -C /tmp/flpdf-qtest-25kg-7-10-progress commit -m "data: promote progress-reporting qtest parity".

### Task 4: correspondence and tracker evidence

- [ ] Add a source-backed note to docs/qpdf-correspondence.md covering QPDFJob_config.cc:478-481, QPDFJob.cc:281-284 and 2926-2935, QPDFWriter.cc:2187-2193 and 2957-2987, the two output-routing probes, and reuse of QPDFJob::configure_writer_progress.
- [ ] Run python3 scripts/check-qpdf-deviation-markers.py --check. No deviation marker is authorized for this qpdf-compatible route.
- [ ] Append implementation evidence to flpdf-25kg.7.10 without overwriting the readiness audit: branch, commits, qtest commit, artifact pair, focused results, qpdf probe, and the remaining merge-and-split ownership.
- [ ] Read back bd show flpdf-25kg.7.10, bd dep tree flpdf-25kg.7.10, and bd dep cycles. The parent edge and no-cycle result must remain unchanged.

### Task 5: final verification and Draft PR

- [ ] Run cargo fmt --all -- --check.
- [ ] Run cargo test -p flpdf --test job_lifecycle_tests, cargo test -p flpdf-cli --test progress_reporting, cargo test -p flpdf-cli --test cli_tests, and cargo test --workspace. Verify nonzero test counts and zero failures.
- [ ] Run strict rustdoc:
~~~text
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
~~~
- [ ] Run cargo clippy --workspace --all-targets --all-features -- -D warnings.
- [ ] Generate fresh LCOV from this exact committed worktree and run scripts/patch-coverage.sh --base origin/main --lcov. Changed executable lines must be fully covered.
- [ ] Fetch and rebase onto the current remote main with git fetch --prune origin and git rebase origin/main. Rerun focused tests and patch coverage after the rebase.
- [ ] Push the feature branch with git push --set-upstream origin flpdf-25kg-7-10-progress and create a Draft PR against main. The PR body must cite qpdf source/probe evidence, verification commands, and the qtest commit, and must omit merge instructions.
- [ ] Set `pr_id=$(gh pr view --json number --jq .number)` after creating the PR, poll `gh pr checks "$pr_id"` until every required check including patch coverage is green, and only then run `gh pr ready "$pr_id"`. Do not merge this PR in this session.
- [ ] Append the PR URL, CI result, and final verification to Beads, close the completed implementation issue only after acceptance readback, run bd dep cycles, and run bd dolt push until it reports Push complete.
