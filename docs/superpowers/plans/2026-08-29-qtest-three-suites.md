# qtest three-suite parity implementation plan

## Goal

Make these qpdf 11.9.0 suites pass without changing their expected output:

- `stream-data.test` (2 cases)
- `output-redirection.test` (2 cases)
- `name-number-trees.test` (6 cases)

The acceptance evidence is one isolated qtest run with all 10 cases passing,
backed by the same-run `harness.log` and `qtest-results.xml`. The qtest
manifest is updated only after that evidence, and only for these ten rows.

## Oracle and ownership

- `QPDF_linearization.cc:837-870` owns `showLinearizationData`: one
  read/check/dump try-catch, with warnings delivered before the info dump.
- `QPDFLogger.cc:218-246` owns `setOutputStreams` and the info/warn/error
  destination relationship.
- `QPDF_Stream.cc:344-359` and `QPDFObjectHandle.cc:1288-1292` own the
  `getStreamData` unfilterable exception, including filename and parsed offset.
- `NNTree.cc:8-27,114-154,584-687,902-920` owns name/number-tree warning and
  error construction, cursor timing, and invalid insertion behavior;
  `QPDF.cc:487-504` and `QPDFExc.cc:19-50` compose the final diagnostic text.

The existing canonical flpdf NNTree engine and logger/pipeline primitives are
prerequisites, not replacement algorithms. The qtest driver remains a thin
consumer of those boundaries. No qpdf fixture or golden is copied into flpdf.

## Current RED evidence

The 2026-08-29 baseline from flpdf `main` (`344067b4`) reports 5/10 passing:

- `stream-data 1` passes; `stream-data 2` omits the caught unfilterable
  `QPDFExc` line.
- `output-redirection 1` and `2` are driver stubs and emit only `test 12/13
  done`.
- `name-number-trees 3-6` pass; cases 1 and 2 reach the canonical tree code
  but differ in warning formatting/timing and the caught invalid-insert line.

The baseline artifacts are retained at the temporary run directory reported by
the audit. A fresh run is required after each implementation slice.

## Task 1: output-redirection live-document boundary

- [x] Add a RED regression for invoking the linearization show operation on an
   already-open `Pdf<R>` and routing its info and warning output through custom
   `PipelineHandle` sinks. Assert that the existing byte/path wrappers retain
   their current behavior.
- [x] Generalize or add the internal linearization show entry point so it obtains
   source bytes from the resolver-owned input of the same `Pdf`, instead of
   reopening a `Cursor<Vec<u8>>`. Keep qpdf's `readLinearizationData`,
   `checkLinearizationInternal`, and `dumpLinearizationDataInternal` order and
   single catch boundary.
- [x] In `run_test_12`, install qpdf-equivalent stdout/stderr destinations on the
   document logger, disable only the driver's initial warning suppression for
   this operation, invoke the live-document show operation, and preserve the
   warning-before-dump order when forwarding captured sink bytes to the driver
   writers.
- [x] In `run_test_13`, install separate in-memory info/error sinks, invoke the
   same operation, then print the exact qpdf `---output---` and `---error---`
   framing from those captured buffers.
- [x] Run the focused linearization/logger tests and the two output-redirection
   qtest cases before moving on.

## Task 2: stream-data exception boundary

- [x] Add RED coverage for a parsed stream whose requested decode level cannot
   filter its filter chain. The assertion must distinguish the qpdf exception
   detail, source filename, and parsed stream offset from a generic unsupported
   error.
- [x] Extend the canonical `ObjectHandle::get_stream_data` error boundary, or
   expose its qpdf-shaped result, so the `QPDF_Stream::getStreamData` false
   filtering outcome carries the qpdf-compatible exception context. Preserve
   successful generalized/all/raw stream behavior and the source pipe's
   existing warning/retry semantics.
- [x] Replace the discarded call in `run_test_68` with a real qpdf-style
   try/catch translation. Print the exact caught error and continue to the
   independent `DecodeLevel::All` and raw calls.
- [x] Run focused object-handle/stream tests and the two stream-data qtest cases.

## Task 3: name/number-tree qtest observation boundary

- [x] Add focused driver tests for the qpdf warning renderer: structural
   `Name/Number tree node` context, nested repair context, and a caught
   `insert` error. Add coverage for draining diagnostics immediately after
   each source operation that can emit one.
- [x] Update `test_42_49.rs` to drain diagnostics at the same points as qpdf's
   `begin`, `last`, cursor increment, and invalid insertion calls. Do not
   alter the shared NNTree traversal or add a second tree implementation.
- [x] Render structural diagnostics using the equivalent of
   `QPDFExc::createWhat`, including filename insertion at every nested context;
   render the real invalid-insert error at the catch boundary rather than
   dropping it or printing flpdf's generic parse wrapper.
- [x] Run focused qtest-tools tests and the two name-number-trees cases, then
   compare the complete output with `number-tree.out` and `name-tree.out`.

## Task 4: integration and qtest repository manifest

- [x] Rebuild the ten helper binaries from the final flpdf commit.
- [x] Run an isolated qtest copy containing exactly the three target stems. Keep
   `harness.log` separate from qtest's own `qtest.log`, and count testcase
   outcomes from `qtest-results.xml`.
- [x] Update only the ten target rows in
   `/home/ubuntu/flpdf-qtest/parity/qtest-11.9.0.jsonl` from `blocked` to
   `passing` after the same-run evidence proves ordinary PASS. Do not touch
   unrelated stale rows, allowlist entries, or vendored qpdf files.
- [x] Re-run the target suite after the manifest change and validate the
   manifest/summary tooling.

## Task 5: quality gates and handoff

Run, in the final implementation worktrees as applicable:

- [x] `cargo fmt --all -- --check`
- [x] focused flpdf and flpdf-qtest-tools tests
- [x] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [x] `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items`
- [x] `cargo test --workspace --all-features`
- [x] qpdf module/deviation checks and fresh changed-line coverage
- [x] final isolated target qtest run with paired artifacts

Final observed evidence:

- final target run at `/tmp/flpdf-qtest-latest-final.whQ13c`: 10 total, 10 pass, 0
  fail, driver status 0; the same-run `harness.log` and `qtest-results.xml`
  both record pass for every testcase
- `cargo test -p flpdf-qtest-tools --all-features`: 153 passed
- `cargo test --workspace --all-features`: exit 0
- strict all-target/all-feature clippy and private-item rustdoc: exit 0
- qpdf module documentation and deviation-marker checks: exit 0
- fresh patch coverage: `flpdf` changed 77, uncovered 0 (100%); the
  qtest-tools report-only remainder is 3 generated closing/branch lines

Before claiming completion, read back all three Beads issues, run
`bd dep cycles`, verify git status in every touched repository, and record the
exact qtest counts and artifact paths. Close Beads only when the implementation
and manifest evidence are complete.
