# LZW and PNG Predictor Pipeline Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete `flpdf-qynx.5.3` by mirroring qpdf 11.9.0 `Pl_LZWDecoder` and `Pl_PNGFilter` as crate-private `Pipeline` stages, replacing `FlateStreamFilter` with an `SF_FlateLzwDecode`-shaped adapter that owns `/DecodeParms`, routing every production LZW-decode, PNG-decode, and PNG-encode consumer through them, and deleting the four superseded helpers.

**Design:** [`docs/superpowers/specs/2026-07-28-qpdf-lzw-png-cutover-design.md`](../specs/2026-07-28-qpdf-lzw-png-cutover-design.md)

**Architecture:** `pipeline/lzw.rs` and `pipeline/png_filter.rs` own LZW decoding and PNG predictor transformation in both directions. `stream_filter.rs::FlateLzwStreamFilter` owns `/Predictor`, `/Columns`, `/Colors`, `/BitsPerComponent`, and `/EarlyChange` parsing and builds the qpdf decode chain. `filters.rs` keeps only filter-chain orchestration and the non-predictor encoders. `writer/serialize.rs` keeps only xref row construction and Flate compression.

**Tech Stack:** Rust 2021 workspace; existing `Pipeline` trait, `OutputBuffer`, `Buffer`, and `RecordingSink` test support; qpdf 11.9.0 (`3b97c9bd`) as source and live oracle; `qpdf-zlib-compat` byte gates; Beads; Git worktrees; Cargo tests, Clippy, strict rustdoc, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 is the behavior oracle. Resolve its read-only source with `scripts/fetch-qpdf-source.sh --print-path`; never clone or edit another qpdf tree.
- Work in the worktree `.worktrees/flpdf-qynx-5-3-lzw-png` on branch `feature/flpdf-qynx-5-3-lzw-png`, based on `origin/main` at `460c9f20`.
- Reproduce qpdf's algorithm, data structures, and processing order. Do not substitute a wide bit accumulator for the three-byte rotating LZW buffer, and do not replace per-byte or per-row downstream write boundaries with batched writes.
- Adopt qpdf's exact diagnostic strings. No flpdf-authored wording survives on a path that qpdf also diagnoses.
- TIFF `/Predictor 2` stays out of scope and remains an explicit declared deviation raised at pipeline-construction time.
- Do not introduce qpdf's "non-filterable means pass through encoded" semantics; the existing `Error::Unsupported` mapping is unchanged.
- The writer cutover must be byte-neutral. `deterministic_id_xref_stream_tests`, `cmp_linearize_objstm_tests`, and every `compat_baseline_*` byte test must pass **without re-blessing**.
- The committed branch must have 100% patch coverage for changed executable lines against `origin/main`.
- Public doc comments in `crates/*/src/` stay English, carry no beads IDs, and cite qpdf or ISO 32000 rather than internal tracking.

## File Map

| Path | Change |
| --- | --- |
| `crates/flpdf/src/pipeline/lzw.rs` | new — `LzwDecoder` |
| `crates/flpdf/src/pipeline/png_filter.rs` | new — `PngFilter`, `PngFilterAction` |
| `crates/flpdf/src/pipeline.rs` | declare both modules |
| `crates/flpdf/src/pipeline/lzw_png_oracle.rs` | new — live differential harness |
| `tests/oracle/qpdf_lzw_png_probe.cc` | new — qpdf-side probe |
| `scripts/qpdf-lzw-png-diff.sh` | new — differential runner |
| `crates/flpdf/src/stream_filter.rs` | `FlateStreamFilter` → `FlateLzwStreamFilter`; register `LZWDecode` |
| `crates/flpdf/src/filters.rs` | delete four helpers; rebuild encode predictor path |
| `crates/flpdf/src/writer/serialize.rs` | `png_up_predict` → `PngFilter::Encode` |
| `crates/flpdf/src/check.rs` | limit and predictor rustdoc |
| `crates/flpdf/tests/qdf_tests.rs` | LZW tests move to the new route |
| `docs/qpdf-correspondence.md`, `docs/qpdf-module-doc-index.md` | correspondence and index |
| `.github/workflows/ci.yml` | only if a new gated byte test is added |

## Task 1 — Oracle probe first

- [ ] Add `tests/oracle/qpdf_lzw_png_probe.cc` modeled on `qpdf_stream_codecs_probe.cc`, with a parameterized codec selector (`lzw:EARLY`, `png-decode:C,S,B`, `png-encode:C,S,B`) so constructor throws are comparable results.
- [ ] Add `scripts/qpdf-lzw-png-diff.sh` modeled on `scripts/qpdf-stream-codecs-diff.sh`, compiling `Pipeline.cc`, `Pl_LZWDecoder.cc`, and `Pl_PNGFilter.cc`.
- [ ] Run the probe by hand over the ambiguous cases and record the observed answers in the design doc: unknown row filter byte, truncated final row at `finish`, post-`finish` reuse in both directions, `new_idx == 4096`, the 32-bit `bytes_per_row` wrap, and the LZW width-transition boundaries for `EarlyChange` `0` and `1`.

## Task 2 — LZW component

- [ ] Implement `pipeline/lzw.rs` against the recorded probe answers: rotating three-byte buffer, one code per input byte, qpdf mask arithmetic, table growth, width transitions, `eod` latch, and the seven diagnostics.
- [ ] `finish` calls downstream `finish` and resets nothing.
- [ ] Unit tests for every transition and failure boundary listed in the design.

## Task 3 — PNG component

- [ ] Implement `pipeline/png_filter.rs`: fallible constructor with the four rejections and 32-bit wrapping row width; row accumulation and buffer swap; decode filters 0–4 with unknown bytes ignored; encode with the hard-coded Up filter and qpdf's exact write boundaries; `finish` partial-row emission and reset.
- [ ] Unit tests for every branch, geometry, split, and downstream failure listed in the design.

## Task 4 — Differential harness

- [ ] Add `pipeline/lzw_png_oracle.rs` with the ignored `qpdf_lzw_png_differential` test covering the full case list, comparing bytes, per-call chunks, finish counts, and exception category and text.
- [ ] Run `scripts/qpdf-lzw-png-diff.sh` green.

## Task 5 — StreamFilter adapter

- [ ] Replace `FlateStreamFilter` with `FlateLzwStreamFilter`, including `i32` clamping, qpdf's key loop, the `/EarlyChange` LZW-only rule, and the post-loop `predictor > 1 && columns == 0` check.
- [ ] Build the decode chain in `pipe_decode`, raising the `/Predictor 2` deviation and the `QIntC::to_uint` range errors before any codec write.
- [ ] Register `LZWDecode` in `stream_filter_for`; verify `Fl` and `LZW` abbreviations reach it.
- [ ] Adapter tests for every acceptance and rejection branch.

## Task 6 — filters.rs cutover and deletion

- [ ] Drop `predictor` from `PreparedDecodeFilter`; delete `apply_prepared_decode_params` and `extract_predictor_params`'s standalone role.
- [ ] Remove the `LZWDecode` arms from `apply_single_filter_decode` and `validate_legacy_decode_filter`.
- [ ] Rebuild `apply_encode_params` on `PngFilter::Encode`.
- [ ] Delete `lzw_decode`, `decode_png_predictor`, `encode_png_predictor`, `png_filter_byte`.
- [ ] Rewrite the tests that asserted removed helper behavior as qpdf parity tests, including `qdf_tests.rs::lzw_decode_*` and any minimum-sum encode assertion.
- [ ] Update `filters.rs` and `check.rs` rustdoc for the new limit boundary.

## Task 7 — Writer cutover

- [ ] Replace `png_up_predict` with a `PngFilter::Encode` collector; assert the exact-row-multiple invariant.
- [ ] Confirm `deterministic_id_xref_stream_tests`, `cmp_linearize_objstm_tests`, and `qpdf-zlib-compat` `compat_baseline_*` pass unblessed. Stop and diagnose on any byte movement.

## Task 8 — Documentation and inventory

- [ ] Update `docs/qpdf-correspondence.md`; regenerate `docs/qpdf-module-doc-index.md`.
- [ ] Run the deletion inventory searches from the design and confirm each is empty.

## Task 9 — Completion gates

- [ ] Focused, workspace, and CLI tests; `scripts/qpdf-lzw-png-diff.sh`.
- [ ] `cargo fmt -- --check`; workspace Clippy all-targets/all-features `-D warnings`; strict rustdoc.
- [ ] `qpdf-zlib-compat` byte tests; add any new gated test to `ci.yml`.
- [ ] Commit, then `scripts/patch-coverage.sh --base origin/main` at 100%.
- [ ] Qualitative check: error arms, boundaries, and empty/extreme inputs have real assertions.
