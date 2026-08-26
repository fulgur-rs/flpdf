# Inspect-Only Output Conflict Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax.

**Goal:** Make eight unguarded top-level inspect-only flags reject a second positional output path, matching qpdf 11.9.0.

**Architecture:** Keep validation at the existing clap Cli parser boundary. Add conflicts_with = "output" to each affected field and leave lifecycle, formatting, and output-producing routes unchanged. Test the real binary once per flag.

**Tech Stack:** Rust workspace, clap, assert_cmd, predicates, qpdf 11.9.0.

---

### Task 1: Add failing CLI regression tests

**Files:**
- Create: crates/flpdf-cli/tests/cli_inspect_output_conflicts.rs

- [x] **Step 1: Add one real-binary test per affected flag.**

Use a shared helper that creates a tempfile output path, invokes flpdf with
tests/fixtures/minimal.pdf plus that path, and asserts code 2, empty stdout,
stderr containing cannot be used with, and no output file. Add these eight
tests: check_rejects_output_file, show_object_rejects_output_file,
show_npages_rejects_output_file, show_pages_rejects_output_file,
show_xref_rejects_output_file, show_linearization_rejects_output_file,
list_attachments_rejects_output_file, and show_attachment_rejects_output_file.

    use assert_cmd::Command;
    use predicates::prelude::*;

    fn assert_rejects_output(flag_args: &[&str]) {
        let temp = tempfile::tempdir().expect("tempdir");
        let output = temp.path().join("must-not-be-created.pdf");
        let mut command = Command::cargo_bin("flpdf").expect("flpdf binary");
        command.args(flag_args).args([
            "../../tests/fixtures/minimal.pdf",
            output.to_str().expect("UTF-8 temporary path"),
        ]);
        command.assert()
            .failure()
            .code(2)
            .stdout(predicate::str::is_empty())
            .stderr(predicate::str::contains("cannot be used with"));
        assert!(!output.exists(), "inspection must not create output");
    }

- [x] **Step 2: Run the new tests and verify RED.**

Run:

    cargo test -p flpdf-cli --test cli_inspect_output_conflicts

Expected before production changes: all eight tests fail because the parser
accepts the output positional and dispatches the inspection route. The
show-attachment case may independently report its missing key. Fix only test
setup errors until the failure is specifically the absent output conflict.

- [x] **Step 3: Commit the RED tests.**

    git add crates/flpdf-cli/tests/cli_inspect_output_conflicts.rs
    git commit -m "test: reject output files for inspect-only flags"

### Task 2: Add the parser conflicts

**Files:**
- Modify: crates/flpdf-cli/src/main.rs, Cli fields check through show_attachment

- [x] **Step 1: Add the existing clap conflict declaration to all eight fields.**

The affected declarations must have these attributes:

    #[arg(long, conflicts_with = "output")]
    check: bool,
    #[arg(long, conflicts_with = "output")]
    show_object: Option<String>,
    #[arg(long, conflicts_with = "output")]
    show_npages: bool,
    #[arg(long, conflicts_with = "output")]
    show_pages: bool,
    #[arg(long, conflicts_with = "output")]
    show_xref: bool,
    #[arg(long, conflicts_with = "output")]
    show_linearization: bool,

    #[arg(long = "list-attachments", conflicts_with = "output",
          help = "List all embedded-file attachments (qpdf --list-attachments)")]
    list_attachments: bool,

    #[arg(long = "show-attachment", conflicts_with = "output",
          value_name = "KEY",
          help = "Extract the embedded file with the given key to stdout \
                  (qpdf --show-attachment)")]
    show_attachment: Option<String>,

Do not change check_linearization or show_encryption; both are already guarded.
Do not alter the shared output field or any dispatch function.

- [x] **Step 2: Run the focused GREEN tests.**

    cargo fmt --all
    cargo test -p flpdf-cli --test cli_inspect_output_conflicts
    cargo test -p flpdf-cli --test cli_tests
    cargo test -p flpdf-cli --test encrypt_cli_tests

Expected: all eight new tests pass and existing inspection/attachment tests
remain green.

- [x] **Step 3: Commit the implementation.**

    git add crates/flpdf-cli/src/main.rs
    git commit -m "fix: reject output files for inspect-only flags"

### Task 3: Run repository verification

**Files:**
- Verify: crates/flpdf-cli/src/main.rs
- Verify: crates/flpdf-cli/tests/cli_inspect_output_conflicts.rs

- [x] **Step 1: Run focused qpdf and CLI checks.**

    cargo test -p flpdf-cli --test cli_inspect_output_conflicts
    cargo test -p flpdf-cli --test cli_tests
    cargo test -p flpdf-cli --test compat_matrix_tests
    scripts/qpdf-tokenizer-diff.sh

- [x] **Step 2: Run static and workspace gates.**

    cargo fmt --all -- --check
    RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    python3 -m unittest scripts/tests/test_qpdf_module_docs.py
    python3 scripts/qpdf-module-docs.py --check
    python3 -m unittest scripts/tests/test_qpdf_deviation_markers.py
    python3 scripts/check-qpdf-deviation-markers.py --check
    cargo test --workspace --all-features

- [x] **Step 3: Require fresh changed-line coverage.**

    cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path /tmp/flpdf-eyo0-inspect-output-conflict.lcov
    scripts/patch-coverage.sh --base origin/main --lcov /tmp/flpdf-eyo0-inspect-output-conflict.lcov

Expected: the flpdf-cli report is informational and the overall patch gate
exits 0 with all gated flpdf executable changed lines covered.

### Task 4: Rebase, publish, and record

- [ ] **Step 1: Re-fetch and rebase before publication.**

    git fetch --prune origin
    git rebase origin/main
    git status --short --branch

If origin/main moved, rerun Task 3 after the rebase.

- [ ] **Step 2: Push the branch and create a Draft PR.**

    git push --set-upstream origin feature/flpdf-eyo0-inspect-output-conflict
    gh pr create --draft --base main --head feature/flpdf-eyo0-inspect-output-conflict --title "fix: reject output files for inspect-only flags" --body-file /tmp/flpdf-eyo0-pr.md

The PR body must include qpdf source/live evidence, exact local gates, and
flpdf-eyo0; it must not contain a merge-blocking directive.

- [ ] **Step 3: Wait for every CI check, then mark ready.**

Poll gh pr checks NUMBER until Quality, Coverage/patch, Fuzz, all OS tests,
CodeQL, labels, release gates, and Codecov pass. Query review APIs for
reviews, inline comments, and issue comments; validate any finding against
qpdf 11.9.0. Only then run gh pr ready NUMBER and read back OPEN,
isDraft:false, CLEAN, all statuses pass, and unmerged.

- [ ] **Step 4: Append final Beads evidence and persist it.**

Record implementation commits, PR URL/state, qpdf evidence, RED/GREEN tests,
all gates, and bd dep cycles. Keep flpdf-eyo0 open/in progress until the
integration session owns final closeout; run bd dolt push and confirm the
exact output Push complete. Do not merge the PR.
