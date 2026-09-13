# Linearization Pass-1 Streaming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the canonical flpdf linearization writer stream pass 1 to discard or the requested file while retaining only qpdf-required layout metadata, digest state, and the final pass buffer.

**Architecture:** Reuse the existing counted `OutputSink` as the single pass-emission boundary. Add an optional in-place patch capability to its target for the final Vec-backed pass, while the pass-1 target is forward-only and writes to a discard sink or buffered file. Change linearization helpers to emit through `OutputSink`, make pass-1 xref records direct-output values rather than deferred patches, and return metadata without a pass-1 body.

**Tech Stack:** Rust workspace, `OutputSink`/`OutputTarget`, qpdf 11.9.0 source and live oracle, `cargo test`, qpdf differential CLI tests, patch coverage, and `scripts/perf-matrix.py`.

**Spec:** Beads issue `flpdf-ymuj.5` (`perf(linearization): avoid retaining the pass-1 body in memory`), audited against qpdf 11.9.0 `QPDFWriter.cc:2537-2904`.

## Global Constraints

- Preserve qpdf 11.9.0 two-pass ordering, fixed-width padding, hint metadata, deterministic IDs, encryption, ObjStm/xref-stream bytes, diagnostics, and sink error behavior.
- Pass 1 uses a forward-only discard/file target; no seekable temporary PDF is introduced and no complete pass-1 `Vec<u8>` is retained when no artifact is requested.
- Keep the final pass in the existing Vec-backed output because `LinearizedDocument::bytes` and final back-patching are public/internal contracts.
- Use canonical writer/ObjectHandle routes; do not add a qtest-only shim, legacy bridge, sentinel representation, or post-hoc output rewrite.
- Do not copy qtest fixtures into flpdf; use existing real fixtures and live qpdf probes.
- Run focused tests after every edit and finish with formatting, workspace tests, strict rustdoc/clippy, qpdf route/deviation checks, patch coverage, and identical-workload performance evidence.

---

### Task 1: Extend the counted output boundary for final patches and pass-1 targets

**Files:**
- Modify: `crates/flpdf/src/writer/output.rs:10-175`
- Modify: `crates/flpdf/src/linearization/writer.rs:1-85,3143-3191`
- Test: `crates/flpdf/src/writer/output.rs` unit tests and `crates/flpdf/src/linearization/writer.rs` unit tests

**Interfaces:**
- `OutputTarget` gains a default `patch_bytes(Range<usize>, &[u8]) -> Result<()>` operation; `Vec<u8>` validates equal-width in-bounds patches and other targets retain the unsupported default.
- `OutputSink` exposes `last_byte()` and `position_usize()`, delegates `patch_bytes`, and continues to update position/MD5 only for accepted forward writes.
- `Pass1OutputTarget::new(Option<&Path>) -> Result<Self>` owns either a discard destination or a buffered pass-1 file, implements `OutputTarget`, flushes with qpdf's `Pl_StdioFile` EBADF rule at document finish, and appends ignored-error debug comments after the body.

- [ ] **Step 1: Write failing tests for the new boundary.** Add one output test that writes bytes through an `OutputSink`, verifies the position and MD5 digest, patches a fixed-width Vec target without changing its length, and rejects a non-equal-width patch. Add one linearization test that constructs a discard `Pass1OutputTarget`, writes through `OutputSink`, and verifies that the target reports the accepted byte count while retaining no body collection.

```rust
#[test]
fn pass1_target_is_forward_only_and_counted() {
    let mut target = Pass1OutputTarget::discard();
    let mut sink = OutputSink::new(&mut target);
    sink.begin_digest();
    sink.write_bytes(b"pass-1").unwrap();
    assert_eq!(sink.position(), 6);
    assert_eq!(sink.take_digest().unwrap(), md5::Md5::digest(b"pass-1").into());
    assert!(target.patch_bytes(0..1, b"x").is_err());
}
```

- [ ] **Step 2: Run the focused tests and confirm RED.**

Run: `cargo test -p flpdf --lib writer::output::tests::pass1_target_is_forward_only_and_counted linearization::writer::tests -- --nocapture`

Expected: compilation/test failure because the patch operation, position accessor, and pass-1 target do not yet exist.

- [ ] **Step 3: Implement the output boundary.** Add the default target patch method, Vec bounds/width validation, sink delegation/accessors, and the discard/file `Pass1OutputTarget` with `BufWriter<File>` ownership. Use `Error::file_io("open", path, source)` during construction; write body chunks as all-or-error accepted chunks; make `finish_document` ignore ordinary flush errors and translate raw EBADF to the existing qpdf-shaped internal error. Keep `write_pass1_debug_comments` as the ignored-error comment writer and call it only for the file variant.

- [ ] **Step 4: Run the focused tests and confirm GREEN.**

Run: `cargo test -p flpdf --lib writer::output::tests linearization::writer::tests -- --nocapture`

Expected: all selected tests pass, with no change to the existing output-sink short-write/error tests.

- [ ] **Step 5: Commit the boundary.**

```bash
git add crates/flpdf/src/writer/output.rs crates/flpdf/src/linearization/writer.rs
git commit -m "perf(linearization): add counted pass output targets"
```

### Task 2: Stream `do_write_pass` and eliminate pass-1 body ownership

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs:297-417,522-1017,1361-1753,1831-1893,2057-2787,3216-4335`
- Test: `crates/flpdf/tests/writer_linearization_route_tests.rs` and existing linearization writer/CLI tests

**Interfaces:**
- `LinearizedPassOutput` contains only xref offsets, hint offsets/lengths, `/E` and `/T` coordinates, patch ranges, ID ranges, and `second_xref_end`; it has no `bytes` field.
- `do_write_pass` accepts `&mut OutputSink<'_>` and emits all object, stream, trailer, and xref bytes through it. It returns `LinearizedPassOutput` metadata only.
- `write_first_page_xref_stream` and `write_part1_xref_and_trailer` receive the pass-1 flag and emit qpdf's pass-1 xref bytes directly; final mode reserves fixed-width regions and uses `OutputSink::patch_bytes` after downstream offsets are known.
- `compute_deterministic_id_from_digest` derives the same two-level ID from the incremental pass-1 MD5 without reading a body buffer.

- [ ] **Step 1: Write the route-contract and metadata tests first.** Extend the linearization route contract to require that `LinearizedPassOutput` has no `bytes: Vec<u8>` field, `do_write_pass` takes `OutputSink`, and the pass-1 branch does not reference `pass1_output.bytes`. Add a regression test that runs a deterministic linearized write and compares final bytes and offsets with the existing baseline path, while checking the pass-1 target receives the expected `% hint_offset`, `% hint_length`, `% second_xref_offset`, and `% second_xref_end` comments.

- [ ] **Step 2: Run the route and focused regression tests and confirm RED.**

Run: `cargo test -p flpdf --test writer_linearization_route_tests` and `cargo test -p flpdf linearization::writer::tests --lib`

Expected: the new source-contract assertion fails on the current `bytes` field/reference; existing behavior tests remain the baseline.

- [ ] **Step 3: Convert linearization emission helpers to `OutputSink`.** Replace direct `Vec` extension/resize operations in the pass helpers with `write_bytes`, `last_byte`, and a bounded repeated-padding helper. Replace nested `with_buffer_sink` calls with direct writes to the supplied `OutputSink`, preserving canonical ObjectHandle and encryption emitters. Route fixed final patches through `OutputSink::patch_bytes`; retain only small xref/padding regions and hint payload buffers required by qpdf.

- [ ] **Step 4: Make pass-1 xref output forward-only.** In the classic path, write zero-format first-page xref records directly in pass 1 and reserve/patch them only in the final pass. In the ObjStm path, build the pass-1 xref stream from the param-dict and first-xref offsets already known at the first-page boundary, write its fixed padded region immediately, and reserve/patch only the final region. Record the main xref end position before `startxref` instead of scanning pass-1 bytes.

- [ ] **Step 5: Replace pass-1 Vec construction and digesting.** Create `Pass1OutputTarget` and `OutputSink` before the pass-1 call, begin MD5 only when deterministic IDs are enabled, call `do_write_pass`, finish the sink, take the digest, and drop the sink before appending debug comments. Compute the deterministic ID from that digest; build hints from metadata; create a fresh Vec-backed sink for pass 2; retain final bytes and existing final deterministic-ID patching unchanged.

- [ ] **Step 6: Run focused GREEN tests and qpdf differentials.**

Run: `cargo test -p flpdf --test writer_linearization_route_tests`; `cargo test -p flpdf linearization::writer::tests --lib`; `cargo test -p flpdf-cli --test cli_logger_routing qpdf_differential_matches_small_and_large_pass1_dev_full_boundaries -- --nocapture`; `cargo test -p flpdf-cli --test cli_tests top_level_linearize_accepts_compress_streams_and_pass1 -- --nocapture`.

Expected: route assertions, existing pass-1/error tests, deterministic-ID tests, and `/dev/full` qpdf/flpdf status/error comparisons pass.

- [ ] **Step 7: Commit the implementation.**

```bash
git add crates/flpdf/src/linearization/writer.rs crates/flpdf/tests/writer_linearization_route_tests.rs
git commit -m "perf(linearization): stream pass one without retaining body"
```

### Task 3: Preserve documentation and add exact artifact/output coverage

**Files:**
- Modify: `docs/qpdf-correspondence.md` near the linearized writer ownership rows
- Modify: `crates/flpdf-cli/tests/cli_logger_routing.rs:225-310`
- Modify: `crates/flpdf-cli/tests/cli_tests.rs:2293-2330` only when an assertion needs strengthening

**Interfaces:**
- Documentation states qpdf's pass-1 ownership (`Pl_StdioFile`/discard, `Pl_MD5`, counters, hint buffer) and flpdf's equivalent (`OutputSink` + `Pass1OutputTarget` + metadata), including the final-pass-only Vec/back-patch boundary.
- Artifact tests compare the final output byte-for-byte with the existing qpdf differential helper, inspect the pass-1 comments, and keep qpdf's documented pass-1 invalid-PDF status separate from final-PDF validity.

- [ ] **Step 1: Add a live qpdf artifact comparison test.** Use the existing one-page fixture and qpdf command shape `--linearize --static-id --compress-streams=n --linearize-pass1=...`, run flpdf and qpdf in separate temporary directories, compare final output bytes, compare pass-1 body bytes before the four qpdf debug comments, and assert both comment fields are present with the same numeric values.

- [ ] **Step 2: Run the artifact test RED/GREEN around the implementation.**

Run: `cargo test -p flpdf-cli --test cli_logger_routing qpdf_differential_matches_linearize_pass1_artifact -- --nocapture`

Expected: the test passes after Task 2 and continues to prove explicit pass-1 file compatibility without requiring pass-1 to be a valid PDF.

- [ ] **Step 3: Update the correspondence row.** Document the no-seekable-temp conclusion and identify the small retained data (`file_size`/xref positions, hint buffer, MD5 state) separately from the final returned `LinearizedDocument::bytes`; cite qpdf `QPDFWriter.cc:2656-2900`, `QPDFWriter.hh:688-690`, and the option contract lines.

- [ ] **Step 4: Run documentation and route checks.**

Run: `cargo fmt --all -- --check`; `python3 scripts/check-qpdf-deviation-markers.py --check`; `python3 scripts/qpdf-module-docs.py --check`; `python3 scripts/check-qpdf-route-matrix.py --check`.

- [ ] **Step 5: Commit documentation and artifact coverage.**

```bash
git add docs/qpdf-correspondence.md crates/flpdf-cli/tests/cli_logger_routing.rs crates/flpdf-cli/tests/cli_tests.rs
git commit -m "test(linearization): pin streamed pass-one artifact parity"
```

### Task 4: Verify memory/time effect and hand off with all gates

**Files:**
- No source changes unless a verification failure identifies a scoped regression in the files above.
- Evidence: Beads notes for `flpdf-ymuj.5`, `/tmp/flpdf-ymuj5-*` benchmark artifacts, and CI checks for the PR head.

**Interfaces:**
- Performance evidence reports identical qpdf/flpdf inputs, operation, warmups/runs, wall time, max RSS, output validation, and whether `--linearize-pass1` is enabled.
- Completion requires local correctness gates, patch coverage, CI green, a Ready PR, and Beads readback/push; the issue remains in progress until its PR is merged by a later requested lifecycle action.

- [ ] **Step 1: Run the complete local verification sequence.**

```bash
cargo fmt --all -- --check
cargo test -p flpdf --all-features
cargo test -p flpdf-cli --all-features
cargo test --workspace --all-features
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/patch-coverage.sh --base origin/main
```

Expected: every command succeeds and the changed-line report has zero uncovered lines.

- [ ] **Step 2: Run the required compatibility and performance evidence.** Run the existing qpdf-zlib-compat linearization/CLI tests and an identical-workload matrix such as `python3 scripts/perf-matrix.py --sizes 1000,5000 --stream-mib 8,32 --operations check,rewrite --runs 5 --warmups 1 --skip-qtest`, adding the linearize/pass1 case through the repository's supported matrix options. Record output equality, wall-time ratio, max RSS ratio, and allocation evidence; do not claim the parent RSS gate is met unless each primary case is at most qpdf x1.10.

- [ ] **Step 3: Request review, create a Draft PR, and inspect the exact head SHA.** Use the repository review workflow with the qpdf source mapping, focused tests, full gates, patch-coverage result, and honest performance comparison. Push `refactor/flpdf-ymuj-5`, create a Draft PR, and verify base/head/merge state before waiting on every required check.

- [ ] **Step 4: Mark Ready only after all required CI checks are green.** Re-query checks and PR state at the latest head, then run `gh pr ready <number>`; do not merge or close the Beads issue in this session.

- [ ] **Step 5: Append implementation, verification, PR, and Ready evidence to `flpdf-ymuj.5`, read it back, run `bd dep cycles`, and execute `bd dolt push`.** Then re-query `bd ready` and continue to the next ready leaf as requested.

## Self-review

- Spec coverage: pass-1 body ownership, discard/file behavior, deterministic digesting, pass-1 comments, xref-stream/classic paths, final bytes/offsets, encryption/ObjStm, error behavior, route hygiene, and RSS/time evidence each have a task.
- Placeholder scan: no TBD/TODO/placeholder implementation steps are used; fixed-width PDF placeholders named in the plan are existing qpdf layout fields, not deferred work.
- Type consistency: Task 1 supplies `OutputTarget::patch_bytes`, `OutputSink` accessors, and `Pass1OutputTarget`; Task 2 consumes them and defines metadata-only `LinearizedPassOutput`; Tasks 3-4 consume the resulting artifact and verification contracts.
