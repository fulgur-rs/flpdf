# CI Suite Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce CI execution minutes by removing a redundant 3000-iteration linearization stress loop and duplicate default-feature test invocations while retaining the full workspace suite on every OS.

**Architecture:** Keep the encrypted hint-table end-to-end regression as one real random-IV write with deterministic offset assertions, and retain the fixed-IV emitter test for both framing outcomes. Replace the test matrix's serial focused/default Cargo commands with one explicit `cargo test --workspace` command; leave the Linux amd64 qpdf-zlib-compat commands unchanged and protect the workspace command with a workflow contract test.

**Tech Stack:** Rust unit/integration tests, GitHub Actions YAML, `yaml-rust2` workflow-contract parser, Cargo workspace, Beads, qpdf 11.9.0 compatibility tests.

---

## Files and responsibilities

- Modify `crates/flpdf/src/linearization/writer.rs`: reduce the expensive encrypted hint-table test to one end-to-end case and correct stale convergence-loop documentation.
- Modify `crates/flpdf-cli/tests/ci_workflow_contract.rs`: add a contract helper that targets the named `test` job and a regression asserting that this job runs the default workspace suite.
- Modify `.github/workflows/ci.yml`: replace only the repeated default-feature test commands in the four-OS matrix job with `cargo test --workspace`; preserve the Linux amd64 qpdf-zlib-compat block byte-for-byte.
- Create `docs/superpowers/plans/2026-08-09-ci-suite-runtime.md`: this implementation plan.
- Do not modify production linearization, encryption, qpdf version, coverage, fuzz, or qpdf-zlib-compat command behavior.

### Task 1: Create and claim the Beads work item

**Files:** None.

- [ ] **Step 1: Refresh Beads context.**

  Run:

  ```bash
  bd prime
  ```

- [ ] **Step 2: Create the implementation issue.**

  Run:

  ```bash
  BEAD_ID="$(bd create "Reduce CI suite execution minutes without dropping OS coverage" \
    --type task \
    --priority 2 \
    --description "Reduce CI runtime by removing the obsolete 3000-iteration linearization hint-table stress loop and duplicate default-feature Cargo test invocations while retaining the full workspace suite on all four test-matrix OSes." \
    --acceptance "The encrypted hint-table regression runs once with deterministic offset assertions; cargo test --workspace is the sole default-feature matrix test command; Linux amd64 qpdf-zlib-compat commands remain present; focused, workspace, contract, fmt, clippy, and rustdoc checks pass." \
    --spec-id docs/superpowers/specs/2026-08-09-ci-suite-runtime-design.md \
    --silent)"
  test -n "$BEAD_ID"
  printf 'Beads issue: %s\n' "$BEAD_ID"
  ```

  Keep the printed ID in `BEAD_ID` for the remaining steps.

- [ ] **Step 3: Claim and read back the issue.**

  Run:

  ```bash
  bd update "$BEAD_ID" --claim
  bd show "$BEAD_ID"
  ```

  Expected: the issue is assigned to the current actor and its design/acceptance text matches the approved spec. Do not edit source files until the claim succeeds.

### Task 2: Add the full-suite CI contract regression (RED)

**Files:**
- Modify: `crates/flpdf-cli/tests/ci_workflow_contract.rs` after `workflow_contains_test_command`.

- [ ] **Step 1: Add a helper that inspects only the matrix `test` job.**

  Add this exact helper:

  ```rust
  fn test_job_contains_test_command(workflow: &str, command: &str) -> ContractResult<bool> {
      let workflow = parse_workflow(workflow)?;
      let jobs = mapping_get(&workflow, "jobs")
          .ok_or_else(|| "ci workflow must define jobs".to_owned())?;
      let jobs = require_mapping(jobs, "workflow.jobs")?;
      let test_job = mapping_get(jobs, "test")
          .ok_or_else(|| "ci workflow must define the test job".to_owned())?;
      job_contains_test_command(test_job, command)
  }
  ```

- [ ] **Step 2: Add the failing contract test.**

  Add this exact test after the helper:

  ```rust
  #[test]
  fn test_matrix_runs_default_workspace_suite() {
      assert!(
          test_job_contains_test_command(CI_WORKFLOW, "cargo test --workspace")
              .expect("ci workflow must be valid and define the test job"),
          "the four-OS test matrix must run the complete default workspace suite"
      );
  }
  ```

- [ ] **Step 3: Run the new test and verify RED.**

  Run:

  ```bash
  cargo test -p flpdf-cli --test ci_workflow_contract test_matrix_runs_default_workspace_suite
  ```

  Expected: FAIL because the current `test` job has focused commands, `cargo test -p flpdf`, and `cargo test`, but no `cargo test --workspace` command.

### Task 3: Replace duplicate default-feature CI invocations

**Files:**
- Modify: `.github/workflows/ci.yml:202-229`.
- Test: `crates/flpdf-cli/tests/ci_workflow_contract.rs`.

- [ ] **Step 1: Replace only the default-feature test step.**

  Replace the existing `Run tests in required order` step and its eight Cargo commands with:

  ```yaml
      - name: Run workspace test suite
        shell: bash
        run: |
          set -euo pipefail

          echo "[ci] cargo test --workspace"
          cargo test --workspace
  ```

  Do not change the matrix `include`, qpdf installation, quality dependency, coverage job, fuzz job, or any command in the subsequent `bytes-identical zlib compat (Linux amd64)` step.

- [ ] **Step 2: Run the contract test and verify GREEN.**

  Run:

  ```bash
  cargo test -p flpdf-cli --test ci_workflow_contract
  ```

  Expected: all workflow-contract tests pass, including `test_matrix_runs_default_workspace_suite` and `ci_runs_every_whole_file_qpdf_zlib_compat_test`.

- [ ] **Step 3: Inspect the diff for accidental qpdf-suite changes.**

  Run:

  ```bash
  git diff --check
  git diff -- .github/workflows/ci.yml crates/flpdf-cli/tests/ci_workflow_contract.rs
  ```

  Expected: the only workflow command removal is the repeated default-feature sequence, the new command is exactly `cargo test --workspace`, and every qpdf-zlib-compat command remains present.

- [ ] **Step 4: Commit the CI change.**

  Run:

  ```bash
  git add .github/workflows/ci.yml crates/flpdf-cli/tests/ci_workflow_contract.rs
  git commit -m "ci: run the workspace test suite once per OS"
  ```

  The commit must be created on `codex/ci-suite-runtime`, never on `main`.

### Task 4: Remove the obsolete 3000-iteration linearization loop

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs` around the encrypted hint-table test and its preceding fixed-IV framing test.

- [ ] **Step 1: Correct the stale fixed-IV test documentation.**

  In `identical_plaintext_different_iv_can_change_hint_stream_object_length`, remove the reference to the deleted `hint_stream_length_is_stable_across_a_forced_iv_change` test. Replace the proof paragraph with this text:

  ```rust
    /// This is the framing-boundary half of the proof. The end-to-end test
    /// below checks that a real encrypted hint stream still reconstructs its
    /// Shared Objects and Outlines offsets against the bytes shipped in the
    /// final linearized document.
  ```

- [ ] **Step 2: Replace the repeated-run test with one real write.**

  Rename the test to:

  ```rust
  fn linearized_encrypted_outline_and_part8_shared_hint_tables_are_consistent_with_random_iv()
  ```

  Replace the current function body with this single-case structure, retaining the existing fixture, decoder, and offset assertions:

  ```rust
      let src = outlines_and_part8_shared_pdf_bytes();
      let out = linearize_with(&src, |o| {
          // Empty user password lets the checker and hint-stream decoder
          // reopen the encrypted output without an explicit password.
          // static_aes_iv remains false so this covers the production random-IV path.
          o.encrypt = Some(crate::encryption::EncryptParams::v4_aes128(
              Vec::new(),
              b"owner".to_vec(),
          ));
      });

      crate::linearization::check_linearization_bytes(&out)
          .expect("encrypted linearized output must pass the linearization checker");
      assert_encrypted_body_strings_are_hex(&out);

      let dump = crate::linearization::show_linearization_bytes(&out, "test")
          .expect("hint stream must decode (decryption + bit-unpacking)");

      let first_shared_obj = parse_dump_field(&dump, "first_shared_obj") as u32;
      let first_shared_offset = parse_dump_field(&dump, "first_shared_offset") as usize;
      let real_shared_offset = find_object_offset(&out, first_shared_obj);
      assert_eq!(
          first_shared_offset, real_shared_offset,
          "Shared Objects Hint Table's first_shared_offset must match the real \
           physical offset of object {first_shared_obj} (dump:\n{dump})"
      );

      assert!(
          dump.contains("Outlines Hint Table"),
          "test premise: fixture's /Outlines must produce an Outlines Hint \
           Table section (dump:\n{dump})"
      );
      let first_object = parse_dump_field(&dump, "first_object") as u32;
      let first_object_offset = parse_dump_field(&dump, "first_object_offset") as usize;
      let real_object_offset = find_object_offset(&out, first_object);
      assert_eq!(
          first_object_offset, real_object_offset,
          "Outlines Hint Table's first_object_offset must match the real \
           physical offset of object {first_object} (dump:\n{dump})"
      );
  ```

  Update the test doc comment to say “one genuinely random per-invocation IV” and remove all `TRIALS`, `observed_hint_lengths`, `observed_outputs`, probability calculations, and informational boundary logging. Do not add an IV-injection API or change production code.

- [ ] **Step 3: Run the focused test and verify GREEN.**

  Run:

  ```bash
  cargo test -p flpdf --lib linearization::writer::tests::linearized_encrypted_outline_and_part8_shared_hint_tables_are_consistent_with_random_iv -- --exact
  ```

  Expected: one test passes. The run performs one full encrypted linearization and still checks both the Part-8 Shared Objects offset and Outlines offset.

- [ ] **Step 4: Confirm obsolete loop terminology is gone from the edited test area.**

  Run:

  ```bash
  rg -n "TRIALS|many_random_iv_runs|hint_stream_length_is_stable_across_a_forced_iv_change" crates/flpdf/src/linearization/writer.rs
  ```

  Expected: no matches.

- [ ] **Step 5: Commit the focused test change.**

  Run:

  ```bash
  git add crates/flpdf/src/linearization/writer.rs
  git commit -m "test(linearization): remove redundant random stress loop"
  ```

  The commit must be created on `codex/ci-suite-runtime`, never on `main`.

### Task 5: Run repository quality gates and compare the suite boundary

**Files:** None beyond the already committed changes.

- [ ] **Step 1: Check formatting and the focused tests.**

  Run:

  ```bash
  cargo fmt --all -- --check
  cargo test -p flpdf --lib linearization::writer::tests::linearized_encrypted_outline_and_part8_shared_hint_tables_are_consistent_with_random_iv -- --exact
  cargo test -p flpdf-cli --test ci_workflow_contract
  ```

  Expected: all commands exit 0; the linearization test reports one passed test.

- [ ] **Step 2: Run the complete default workspace suite once.**

  Run:

  ```bash
  cargo test --workspace
  ```

  Expected: the same workspace test targets that the matrix runs pass with zero failures. The qpdf-zlib-compat-only tests may report zero tests under default features, as they did in the baseline.

- [ ] **Step 3: Run the quality commands used by CI.**

  Run:

  ```bash
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
  ```

  Expected: both commands exit 0.

- [ ] **Step 4: Run the qpdf-zlib-compat block when the local qpdf oracle is available.**

  First run `qpdf --version` and require `qpdf version 11.9.0`. Then run the unchanged Linux block:

  ```bash
  cargo test -p flpdf --features qpdf-zlib-compat --test zlib_compat_tests
  cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
  cargo test -p flpdf --features qpdf-zlib-compat --test cmp_null_visibility_tests
  cargo test -p flpdf --features qpdf-zlib-compat --test deterministic_id_qpdf_parity_tests
  cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
  cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests
  cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
  cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate
  cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical
  cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay
  cargo test -p flpdf-cli --features qpdf-zlib-compat --test encrypt_cli_tests
  cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_baseline_static_id -- --nocapture
  cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_matrix_baseline -- --nocapture
  cargo test -p flpdf-qtest-tools --features qpdf-zlib-compat --test e2e
  ```

  Expected: every command exits 0 and the workflow contract still requires all whole-file feature-gated test files.

- [ ] **Step 5: Verify the final diff and branch isolation.**

  Run:

  ```bash
  git diff --check origin/main...HEAD
  git status --short --branch
  git log --oneline --decorate origin/main..HEAD
  git -C /home/ubuntu/flpdf status --short --branch
  ```

  Expected: the implementation worktree is clean on `codex/ci-suite-runtime`; the root worktree remains clean on `main...origin/main`; no commit is added to `main`.

### Task 6: Close Beads and publish only the feature branch

**Files:** None.

- [ ] **Step 1: Close the claimed issue after all gates pass.**

  Run:

  ```bash
  bd close "$BEAD_ID"
  ```

- [ ] **Step 2: Persist Beads state.**

  Run:

  ```bash
  bd dolt push
  ```

  Expected: Beads reports `Push complete.`

- [ ] **Step 3: Push only the feature branch.**

  Run:

  ```bash
  git push -u origin codex/ci-suite-runtime
  ```

  Never run `git push origin main`; the local and remote `main` branches must remain untouched by this work.
