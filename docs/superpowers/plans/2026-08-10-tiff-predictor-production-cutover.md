# TIFF Predictor Production Cutover Implementation Plan

> **For agentic workers:** Execute this plan task-by-task with RED→GREEN TDD and qpdf 11.9.0 as the behavioral oracle.

**Goal:** Port qpdf 11.9.0 `Pl_TIFFPredictor` and connect Predictor 2 to flpdf's Flate/LZW decode and Flate encode paths without adding a qpdf-nonexistent LZW encoder.

**Architecture:** Add one incremental `Pipeline` stage that owns qpdf's row buffering, signed sample differencing, bit packing, and finish padding. Reuse the existing `FlateLzwStreamFilter` parameter state and pipeline ownership model, selecting TIFF or PNG predictor behavior from the predictor number while preserving codec-then-predictor construction order.

**Tech Stack:** Rust workspace, `Pipeline`/`PipelineRef`, `Flate`, `LzwDecoder`, qpdf 11.9.0 source and predictor fixtures, focused unit/differential tests.

---

### Task 1: Add the incremental TIFF predictor stage

**Files:**
- Create: `crates/flpdf/src/pipeline/tiff_predictor.rs`
- Modify: `crates/flpdf/src/pipeline.rs`
- Test: `crates/flpdf/src/pipeline/tiff_predictor.rs` unit tests

- [x] Write failing tests for qpdf's constructor validation, arbitrary chunk boundaries, one-row reset, 8-bit encode/decode, packed 1/2/4/16-bit samples, and zero-padded partial rows.
- [x] Run `cargo test -p flpdf tiff_predictor -- --nocapture` and confirm the new tests fail because the stage is absent.
- [x] Implement `TiffPredictor` with qpdf's `bytes_per_row` calculation, `Pipeline` forwarding, per-row previous samples, `BitStream`/`BitWriter` for non-8-bit rows, byte arithmetic for 8-bit rows, and finish-time zero padding.
- [x] Run the focused predictor tests and confirm all pass.

### Task 2: Connect Predictor 2 to stream filter production paths

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs`
- Modify: `crates/flpdf/src/filters.rs`
- Test: `crates/flpdf/src/stream_filter.rs` and `crates/flpdf/src/filters.rs` unit tests

- [x] Add RED tests proving Flate and LZW decode use codec-then-TIFF-predictor ordering, preserve invalid-geometry construction errors, and no longer return the unsupported Predictor 2 error.
- [x] Run the focused tests and confirm the expected failures.
- [x] Add predictor-kind selection and construct `TiffPredictor` in preflight, decode-pipeline, and whole-buffer recovery paths.
- [x] Route Flate encode through `TiffPredictor` for Predictor 2 while retaining the explicit no-LZW-encoder error.
- [x] Run focused stream/filter tests and the existing PNG predictor suite.

### Task 3: Pin qpdf behavior and update correspondence documentation

**Files:**
- Modify: `crates/flpdf/src/pipeline/tiff_predictor.rs` tests and qpdf differential helpers as needed
- Modify: `crates/flpdf/src/stream_filter.rs`/`filters.rs` tests for exact diagnostics and timing
- Modify: `docs/qpdf-correspondence.md`

- [x] Add or update tests for BitsPerComponent 1/2/4/8/16, multiple colors, invalid columns/colors/bits, downstream write/finish errors, and the pinned qpdf predictor fixtures.
- [x] Run the qpdf predictor fixture checks and compare exact output bytes and errors.
- [x] Change the `Pl_TIFFPredictor` correspondence row from missing to the implemented responsibility, recording any intentional Rust ownership substitution.
- [x] Run focused tests again after the documentation change.

### Task 4: Verify and publish

**Files:**
- Modify only the implementation, tests, correspondence row, and this plan.

- [x] Run `cargo fmt --all -- --check`.
- [x] Run the focused flpdf tests, `cargo test -p flpdf`, workspace all-feature clippy, and workspace `cargo test`.
- [x] Run fresh changed-line coverage and confirm 100% executable-line coverage for the patch.
- [x] Inspect `git diff` and `git status`, stage only scoped files, commit, and push the feature branch.
- [x] Open the PR with qpdf source references and validation results, then mark it ready for review.
