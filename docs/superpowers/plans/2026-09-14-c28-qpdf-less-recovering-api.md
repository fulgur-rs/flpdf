# C28 qpdf-less recovering filter API removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the qpdf-less public recovering stream API, its limits/events implementation, and the now-unused `filters` module while preserving canonical ObjectHandle stream processing.

**Architecture:** `ObjectHandle::prepare_stream_filter_plan` and `pipe_stream_data` remain the ordinary filter owners. The old whole-buffer/recovering implementation has no in-tree production consumer after `.48.93`, so its public API, hardening limits, and dedicated tests are deleted rather than narrowed into a compatibility facade.

**Tech Stack:** Rust workspace, Cargo tests, qpdf 11.9.0 source/probes, route matrix scripts, and Beads.

---

### Task 1: Add RED assertions for the public/API cleanup

**Files:**
- Modify: `crates/flpdf/tests/filters_route_cutover_tests.rs`

- [ ] **Step 1: Add a source/API absence contract**

Append this test:

```rust
#[test]
fn qpdf_less_recovering_filter_api_is_removed() {
    let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!crate_root.join("src/filters.rs").exists());
    assert!(!crate_root.join("tests/tiff_predictor_memory_tests.rs").exists());
    let lib = std::fs::read_to_string(crate_root.join("src/lib.rs")).unwrap();
    assert!(!lib.contains("pub mod filters;"));
    for (relative, forbidden) in [
        ("src/stream_filter.rs", "pipe_decode_recovering"),
        ("src/stream_filter.rs", "FilterDecodeOutcome"),
        ("src/stream_filter.rs", "FilterDecodePhase"),
        ("src/stream_filter.rs", "decode_filter_specs_from_handle"),
        ("src/stream_filter.rs", "set_tiff_memory_limit"),
        ("src/pipeline/dct.rs", "DecodeLimits"),
        ("src/pipeline/dct.rs", "with_max_output"),
        ("src/pipeline/tiff_predictor.rs", "new_with_memory_limit"),
        ("src/pipeline/tiff_predictor.rs", "max_memory"),
    ] {
        let source = std::fs::read_to_string(crate_root.join(relative)).unwrap();
        assert!(!source.contains(forbidden), "{relative} retains C28: {forbidden}");
    }
}
```

- [ ] **Step 2: Run the route contract test and verify RED**

Run:

```bash
cargo test -p flpdf --test filters_route_cutover_tests -- --nocapture
```

Expected: the new test fails because `filters.rs`, the TIFF hardening test, and recovery-only symbols still exist. Do not delete production code before observing this failure.

### Task 2: Remove the qpdf-less public module and hardening API

**Files:**
- Delete: `crates/flpdf/src/filters.rs`
- Delete: `crates/flpdf/tests/tiff_predictor_memory_tests.rs`
- Modify: `crates/flpdf/src/lib.rs:103`
- Modify: `crates/flpdf/tests/qpdf_route_hygiene_tests.rs:29-90`
- Modify: `crates/flpdf/src/object_handle.rs:3416,9934`

- [ ] **Step 1: Delete the module and its dedicated tests**

Remove `pub mod filters;` from `lib.rs`, delete the entire legacy `filters.rs`, and delete the standalone TIFF hardening integration test. Do not add `mod filters;` or a replacement public facade.

- [ ] **Step 2: Update route hygiene tests for an absent module**

Replace each `read_source("filters.rs")` block with:

```rust
assert!(
    !source_root().join("filters.rs").exists(),
    "qpdf-less filters.rs compatibility module remains"
);
```

Keep the existing xref and ObjStm canonical-owner assertions.

- [ ] **Step 3: Remove stale ObjectHandle documentation references**

Change comments naming `stream_filter::decode_filter_specs_from_handle` to describe the canonical `ObjectHandle::prepare_stream_filter_plan` caller without linking to a deleted symbol.

### Task 3: Remove recovery-only stream-filter and codec limits

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs`
- Modify: `crates/flpdf/src/pipeline/dct.rs`
- Modify: `crates/flpdf/src/pipeline/tiff_predictor.rs`

- [ ] **Step 1: Keep only the canonical StreamFilter contract**

Retain `set_decode_params`, `decode_pipeline_owned`, `set_warning_callback`, `is_specialized_compression`, and `is_lossy_compression`. Delete the unused `preflight_decode_pipeline`, `set_tiff_memory_limit`, `pipe_decode_recovering`, and types/functions used only by them: `FilterSpec`, `validate_filter_factories`, `validate_filter_chain_count`, `FilterDecodeOutcome`, `FilterDecodeError`, `FilterDecodePhase`, `OutputBuffer`, `StagePipelineError`, `write_and_finish`, `map_stage_error`, `filter_decode_phase`, and whole-buffer decode helpers.

- [ ] **Step 2: Preserve canonical predictor construction without a limit argument**

Canonical construction already uses `decode_pipeline_owned` with the downstream pipeline. Keep that ownership seam and remove the unused legacy preflight sink; no separate predictor preflight or limit argument is needed.

```rust
let next = make_predictor_pipeline(geometry, next, PredictorAction::Decode)?;
```

Remove `tiff_max_memory` from `FlateLzwStreamFilter` and all calls to `make_predictor_pipeline`. Remove recovery implementations from Ascii85, ASCIIHex, RunLength, DCT, and Crypt filters; retain their canonical pipeline/classification methods.

- [ ] **Step 3: Make TiffPredictor::new the single constructor**

Remove `TiffPredictor::new_with_memory_limit` and its optional limit check. Make `TiffPredictor::new` non-test-only and keep the existing qpdf geometry validation. Call it with the seven arguments shown by the existing `new` signature; keep only the test-only Encode action and qpdf-shaped predictor tests.

- [ ] **Step 4: Remove DCT max-output hardening only**

Delete `PlDct::max_output`, `with_max_output`, the `DECODE_OUTPUT_LIMIT_PREFIX` import, and the qpdf-deviation block that checks declared JPEG size. Leave JPEG parsing, scanline emission, compression, and error normalization unchanged.

- [ ] **Step 5: Remove C28-only stream_filter tests**

Delete filter-spec identity/array tests, the wide TIFF memory helper/tests, and the recovering memory-limit test. Keep canonical filter parameter, pipeline construction, DCT, LZW, warning, and predictor tests.

### Task 4: Re-anchor route evidence and manifest

**Files:**
- Modify: `docs/qpdf-route-matrix/c-stream-pipeline-encryption.md`
- Modify: `docs/qpdf-route-matrix/README.md`
- Modify: `docs/qpdf-route-matrix/tracked-symbols.txt`

- [ ] **Step 1: Mark C28 and C9 canonical/absent**

Set C28 to `canonical`, `absent`, `prod: 0 / test: 0`, recording `.48.96` removal of the qpdf-less public recovery API and limits. Set C9 to `canonical`, `absent`, `prod: 0 / test: 0`, with `ObjectHandle::prepare_stream_filter_plan` as canonical owner. Keep C43's prior cleanup record.

- [ ] **Step 2: Recompute same-run classifications**

Preserve all denominators and update expected counts to C canonical 39 / bridge 0 / mixed 3, A-E canonical 104 / bridge 0 / mixed 56, and checker logical canonical 130 / bridge 6 / mixed 123. Use the actual checker citation count in README after deleting the symbols and manifest entries.

Remove C9/C28 deleted symbols from `tracked-symbols.txt`; do not leave a manifest entry for a removed declaration.

### Task 5: Run all gates and commit

- [ ] **Step 1: Run focused and full tests**

```bash
cargo fmt --all -- --check
cargo test -p flpdf --lib stream_filter
cargo test -p flpdf --test filters_route_cutover_tests
cargo test -p flpdf --test embedded_files_tests
cargo test -p flpdf --test cmp_diff_zero_tests
cargo test -p flpdf
cargo test -p flpdf-qtest-tools
```

Expected: all non-ignored tests pass.

- [ ] **Step 2: Run qpdf and structural checks**

```bash
qpdf --show-object=6 --filtered-stream-data tests/fixtures/test_driver/stream_unfilterable.pdf
python3 scripts/qpdf-route-callers.py --symbol crates/flpdf/src/stream_filter.rs::decode_filter_specs_from_handle --expect-zero
python3 scripts/check-qpdf-route-matrix.py --check --qpdf-root /home/ubuntu/.cache/flpdf/qpdf-11.9.0
python3 scripts/check-qpdf-deviation-markers.py --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: qpdf exits 2 with its generic unfilterable diagnostic; the removed-symbol gate passes with an explicit absent note; route, deviation, and clippy checks exit 0.

- [ ] **Step 3: Inspect, commit, and close only the child issue**

```bash
git diff --check
git status --short
git diff --stat
git commit -am "refactor: remove qpdf-less recovering filter API"
bd close flpdf-3yn9.48.96 --reason="C28 qpdf-less public recovery API and limit implementation removed after canonical cutover and full verification."
bd show flpdf-3yn9.48.96 --short
```

Expected: no generated fixtures or target files are committed. Do not close the parent epic; remaining mixed routes and newly identified qpdf-less public APIs remain separate bounded work.
