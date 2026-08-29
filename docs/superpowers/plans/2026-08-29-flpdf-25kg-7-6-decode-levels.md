# qtest decode-levels Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox ("- [ ]") syntax for tracking.

**Goal:** Make qpdf 11.9.0 decode-levels.test pass all 14 cases through flpdf and the bounded Rust qpdf-ctest adapter.

**Architecture:** Keep decode-level state in the existing canonical WriterConfiguration/WriterSettings path. Add the CLI value and explicit-set bit needed to replay qpdf's fixed setter order, extend the writer gate for lossy DCT at all, and remove only the Rust-only DCT diagnostic prefix. Implement qpdf-ctest test20 as a process-level Rust adapter over Pdf/PdfWriter; do not add C ABI or edit vendored qtest files.

**Tech Stack:** Rust workspace (flpdf, flpdf-cli, flpdf-qtest-tools), clap, qtest-driver, qpdf 11.9.0 pinned source and /usr/bin/qpdf oracle, Python parity-manifest validator.

---

### Task 1: Verify the isolated baseline

**Files:**
- Modify: none
- Test: crates/flpdf-cli/tests/cli_decode_levels.rs (create in Task 2)
- Test: crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs

- [ ] **Step 1: Confirm the worktree and Beads state**

Run:

~~~
git status --porcelain=v1 --branch
git log -1 --oneline
bd show flpdf-25kg.7.6
bd show flpdf-25kg.2.1.1
~~~

Expected: clean feature/flpdf-25kg-7-6-decode-levels, the committed design spec, target issue IN_PROGRESS, child issue OPEN, and the target depending on the child.

- [ ] **Step 2: Run focused pre-change tests**

Run:

~~~
cargo test -p flpdf --lib stream_filter::tests::dct_stage_preserves_codec_error_and_does_not_finish_downstream
cargo test -p flpdf-cli --test cli_show_stream
cargo test -p flpdf-qtest-tools --test qpdf_ctest_cli
~~~

Expected: existing tests pass; the missing --decode-level and test20 behavior remain untested by these baselines.

- [ ] **Step 3: Reproduce the qtest baseline**

Run decode-levels.test from a disposable qtest data copy and disposable live directory with TESTS=decode-levels, FLPDF_PROGNAME=qpdf, FLPDF_QPDF_COMPAT=1, and the current worktree release binaries. Retain the same invocation's harness.log and qtest-results.xml under /tmp.

Expected: 14 failures: five option-parse failures, their dependent missing-file failures, unsupported qpdf-ctest test20, and two DCT warning-prefix diffs.

### Task 2: Write RED tests for CLI decode-level behavior and DCT gating

**Files:**
- Create: crates/flpdf-cli/tests/cli_decode_levels.rs
- Modify: crates/flpdf/src/writer.rs tests near filter_chain_is_decodable
- Modify: crates/flpdf-cli/tests/cli_show_stream.rs

- [ ] **Step 1: Add a real CLI level matrix test**

Create one integration test that invokes the real flpdf binary on tests/fixtures/test_driver/stream_dct.pdf with --compress-streams=n --decode-level={none,generalized,specialized,all} --static-id. For each output, open it through flpdf::Pdf, inspect the fixture's DCT stream, assert the output file exists, and assert /DCTDecode remains for the first three levels and is absent at all.

- [ ] **Step 2: Run the CLI test and observe RED**

Run:

~~~
cargo test -p flpdf-cli --test cli_decode_levels
~~~

Expected: clap reports unexpected argument '--decode-level' found.

- [ ] **Step 3: Add a source-near DCT gate test**

Add dct_decode_level_gate beside the existing writer gate tests. For a stream with /Filter /DCTDecode and CompressStreams::No, assert false at DecodeLevel::None, Generalized, and Specialized, and true at All.

- [ ] **Step 4: Run the gate test and observe RED**

Run:

~~~
cargo test -p flpdf --lib writer::tests::dct_decode_level_gate
~~~

Expected: only the All assertion fails because the current writer rejects DCT at every level.

- [ ] **Step 5: Add the qpdf diagnostic assertion**

Update the malformed-DCT CLI test to require qpdf's exact warning text error decoding stream data for object ...: Not a JPEG file: starts with ... and to reject the Rust-only DCT decode: prefix, while preserving qpdf's warning exit status and final summary.

- [ ] **Step 6: Run the diagnostic test and observe RED**

Run:

~~~
cargo test -p flpdf-cli --test cli_show_stream show_stream_dct
~~~

Expected: the current output still contains DCT decode:.

### Task 3: Implement canonical CLI and writer wiring

**Files:**
- Modify: crates/flpdf-cli/src/main.rs WriterOptions and writer_configuration
- Modify: crates/flpdf-cli/src/main.rs top-level Cli and RewriteCommand
- Modify: crates/flpdf-cli/src/main.rs all top-level/rewrite option builders
- Modify: crates/flpdf-cli/src/main.rs JSON option builders
- Test: crates/flpdf-cli/tests/cli_decode_levels.rs

- [ ] **Step 1: Add explicit state to WriterOptions**

Add decode_level: StreamDecodeLevel with default None and decode_level_set: bool with default false. Add CliDecodeLevel as a ValueEnum with exactly None, Generalized, Specialized, and All, plus conversions to writer and JSON decode-level enums.

- [ ] **Step 2: Add the flag to both CLI surfaces**

Add decode_level: Option<CliDecodeLevel> to top-level Cli and RewriteCommand. Use clap value validation; do not translate the flag into a different stream policy.

- [ ] **Step 3: Replay qpdf setter order**

In writer_configuration, call set_stream_data_mode, then set_compress_streams, then set_decode_level only when the explicit-set bit is true. Populate the value and bit in all top-level and rewrite writer option builders. This preserves QDF's implicit generalized default and makes explicit --qdf --decode-level=none override it.

- [ ] **Step 4: Wire JSON's decode level**

Replace the hard-coded JSON DecodeLevel::Generalized values with the same CLI value, defaulting to generalized when the flag is absent. This prevents an accepted qpdf option from becoming a silently ignored JSON option.

- [ ] **Step 5: Implement the DCT writer gate**

Normalize DCT to DCTDecode in filter_chain_is_decodable and allow it only at DecodeLevel::All. Preserve all-or-nothing chain behavior and raw fallback on decode failure.

- [ ] **Step 6: Run the focused GREEN checks**

Run:

~~~
cargo test -p flpdf --lib writer::tests::dct_decode_level_gate
cargo test -p flpdf-cli --test cli_decode_levels
~~~

Expected: both pass and only the all output removes /DCTDecode.

### Task 4: Align DCT diagnostic ownership with qpdf

**Files:**
- Modify: crates/flpdf/src/pipeline/dct.rs
- Modify: crates/flpdf/src/stream_filter.rs DCT tests
- Modify: crates/flpdf-cli/tests/cli_show_stream.rs

- [ ] **Step 1: Update source-near expected diagnostics**

Change malformed, truncated, precision, component, and compatibility-backend expectations to qpdf's underlying codec/runtime text without DCT decode:. Keep the pipeline identifier test and downstream error ownership assertions.

- [ ] **Step 2: Run the DCT tests and observe RED**

Run:

~~~
cargo test -p flpdf --lib stream_filter::tests::dct_
cargo test -p flpdf --lib pipeline::dct::tests
~~~

Expected: the updated expectations fail on the prefix-producing implementation.

- [ ] **Step 3: Remove only the stage prefix**

Change PlDct::runtime_error and codec-error branches in both JPEG backends to return the underlying qpdf diagnostic. Keep identifier for Pipeline::identifier() and leave ObjectHandle's error decoding stream data: mapping unchanged.

- [ ] **Step 4: Run the DCT GREEN checks**

Run:

~~~
cargo test -p flpdf --lib stream_filter::tests::dct_
cargo test -p flpdf --lib pipeline::dct::tests
cargo test -p flpdf-cli --test cli_show_stream show_stream_dct
~~~

### Task 5: Implement qpdf-ctest test20 as a Rust process adapter

**Files:**
- Modify: crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs
- Modify: crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs

- [ ] **Step 1: Write test20 integration coverage**

Add a real-process test using tests/fixtures/test_driver/stream_dct.pdf that runs qpdf-ctest 20 input.pdf "" output.pdf, asserts status 0, exact stdout C test 20 done\n, empty stderr, output existence, and DCT preservation at the specialized level. Add a usage assertion for an unsupported test number or invalid arity.

- [ ] **Step 2: Run the adapter test and observe RED**

Run:

~~~
cargo test -p flpdf-qtest-tools --test qpdf_ctest_cli qpdf_ctest_20
~~~

Expected: the current test19-only usage error.

- [ ] **Step 3: Add run_test20 and dispatch**

Open with the existing password boundary, create PdfWriter, set output, then call set_static_id(true), set_static_aes_iv(true), set_compress_streams(false), and set_decode_level(DecodeLevel::Specialized) in that order. Write successfully before printing C test 20 done. Dispatch Some("20") and retain test19 unchanged. Do not add C ABI dependencies.

- [ ] **Step 4: Run the adapter package checks**

Run:

~~~
cargo test -p flpdf-qtest-tools --test qpdf_ctest_cli
cargo test -p flpdf-qtest-tools
~~~

### Task 6: Run qtest and update only proven evidence

**Files:**
- Modify in a separate /tmp qtest worktree: allowlist.txt
- Modify in a separate /tmp qtest worktree: parity/qtest-11.9.0.jsonl
- Artifacts: same-run survey/latest/harness.log and survey/latest/qtest-results.xml

- [ ] **Step 1: Create the qtest worktree safely**

Create a worktree under /tmp from qtest main, verify it is clean, and leave the existing checkout's untracked .claude/, .worktrees/, and survey/ untouched.

- [ ] **Step 2: Run focused decode-levels.test**

Run qtest-driver with TESTS=decode-levels and the flpdf worktree release binaries. Require XML total-cases="14", every testcase outcome pass, and the summary passes=14, failures=0.

- [ ] **Step 3: Run the full qtest corpus**

From the qtest worktree run:

~~~
FLPDF_DIR=/home/ubuntu/flpdf/.worktrees/flpdf-25kg-7-6-decode-levels \
QTEST_FULL=1 ./scripts/run.sh
~~~

Keep the paired artifacts from this invocation and do not mix them with an older survey.

- [ ] **Step 4: Promote the decode-levels manifest rows**

After the authoritative full run, update only the 14 decode-levels rows to passing with null rationale/owner/bead/replacement fields. Add the whole decode-levels suite to allowlist.txt only after all 14 ordinary PASS outcomes are present. Keep JSONL sorted and leave unrelated rows unchanged.

- [ ] **Step 5: Validate both qtest records**

Run:

~~~
python3 scripts/verify-parity-manifest.py \
  survey/latest/harness.log survey/latest/qtest-results.xml \
  parity/qtest-11.9.0.jsonl
python3 scripts/verify-allowlist.py \
  survey/latest/harness.log survey/latest/qtest-results.xml allowlist.txt
~~~

Expected: no parser/validation errors, no stale decode-levels blocked/failing rows, and the allowlist reports the complete suite as expected pass.

### Task 7: Full verification and handoff

**Files:**
- Modify: only reviewed implementation, spec/plan, and qtest evidence files

- [ ] **Step 1: Run Rust quality gates**

Run:

~~~
cargo fmt --all -- --check
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
python3 scripts/check-qpdf-deviation-markers.py --check
~~~

- [ ] **Step 2: Inspect the final diff**

Run:

~~~
git status --porcelain=v1 --branch
git diff --check
git diff --stat origin/main...HEAD
~~~

Confirm no vendored qtest file or unrelated worktree was changed and every production change has a pinned-qpdf source or differential-test justification.

- [ ] **Step 3: Read back and persist Beads**

Run:

~~~
bd show flpdf-25kg.7.6
bd show flpdf-25kg.2.1.1
bd dep cycles
bd dolt push
~~~

Expected: implemented evidence is recorded, no dependency cycles, and output contains Push complete.

- [ ] **Step 4: Commit and push reviewed branches**

Commit only reviewed files in each repository, re-query remote state, rebase only feature branches if needed, rerun focused checks after a rebase, and push both repositories without force-pushing or modifying main.
