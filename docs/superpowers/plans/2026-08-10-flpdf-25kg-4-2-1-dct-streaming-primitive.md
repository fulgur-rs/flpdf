
# flpdf-25kg.4.2.1 DCT streaming primitive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the qpdf 11.9.0 Pl_DCT decode stage and SF_DCTDecode factory with published libjpeg-turbo-rs = "=0.8.0" as the default backend and an explicit system-libjpeg compatibility feature.

**Architecture:** Keep StreamFilter::decode_pipeline as the canonical stage-factory route and leave pipe_decode_recovering/pipe_codec, writer passthrough, and decode-level consumers untouched. pipeline/dct.rs buffers encoded bytes until finish, then emits qpdf-default JPEG scanlines to the caller's downstream Pipeline; backend selection is compile-time, with qpdf-libjpeg-compat selecting a small C shim rather than exposing libjpeg structs to Rust.

**Tech Stack:** Rust 2021, Cargo workspace, libjpeg-turbo-rs 0.8.0, optional system libjpeg through a cc-compiled C shim, pinned qpdf 11.9.0, Rust unit tests, qpdf differential probes, cargo llvm-cov, and scripts/patch-coverage.sh.

---

## Files and responsibilities

- Create crates/flpdf/src/pipeline/dct.rs: crate-private PlDct<'a> stage and JPEG backend adapters.
- Modify crates/flpdf/src/pipeline.rs: register the crate-private DCT pipeline module without changing the public Pipeline contract.
- Modify crates/flpdf/src/stream_filter.rs: add DctStreamFilter, register /DCTDecode, preserve /DCT normalization, and add canonical factory/stage tests.
- Modify crates/flpdf/Cargo.toml: add the exact Rust JPEG dependency, C-shim build dependency, and qpdf-libjpeg-compat feature.
- Modify crates/flpdf-cli/Cargo.toml and crates/flpdf-qtest-tools/Cargo.toml: forward qpdf-libjpeg-compat to flpdf.
- Create crates/flpdf/build.rs: compile the C shim only when CARGO_FEATURE_QPDF_LIBJPEG_COMPAT is set.
- Create crates/flpdf/src/jpeg_compat.h and crates/flpdf/src/jpeg_compat.c: expose only a callback-based 8-bit scanline decoder; keep jpeg_decompress_struct and setjmp handling in C.
- Modify docs/qpdf-correspondence.md: replace the current Pl_DCT missing row with the source-to-Rust mapping, backend decision, and canonical/bridge coexistence note.

The existing untracked route-cutover spec/plan remain outside this worktree change set.

## Task 1: Add backend dependencies and feature names without changing decode behavior

**Files:**
- Modify: Cargo.toml workspace dependencies
- Modify: crates/flpdf/Cargo.toml
- Modify: crates/flpdf-cli/Cargo.toml
- Modify: crates/flpdf-qtest-tools/Cargo.toml

- [ ] **Step 1: Add the exact dependency and compatibility feature declarations**

Add these workspace dependencies:

~~~toml
libjpeg-turbo-rs = "=0.8.0"
cc = "1"
~~~

In crates/flpdf/Cargo.toml add libjpeg-turbo-rs.workspace = true, a build-dependencies section containing cc.workspace = true, and:

~~~toml
qpdf-libjpeg-compat = []
~~~

Preserve the existing default, qtest-driver, and qpdf-zlib-compat features. Add this forwarding feature to the CLI and qtest-tools crates:

~~~toml
qpdf-libjpeg-compat = ["flpdf/qpdf-libjpeg-compat"]
~~~

Do not enable the feature by default and do not enable libjpeg-turbo-rs/full-c-parity; that upstream feature is test-only and is not the selected C fallback.

- [ ] **Step 2: Resolve and check the dependency graph**

Run:

~~~bash
cargo check -p flpdf
cargo check -p flpdf-cli
cargo check -p flpdf-qtest-tools
~~~

Expected: all three checks pass, with libjpeg-turbo-rs 0.8.0 in Cargo.lock and no DCT behavior changed.

- [ ] **Step 3: Commit the dependency-only change on the feature branch**

~~~bash
git add Cargo.toml Cargo.lock crates/flpdf/Cargo.toml crates/flpdf-cli/Cargo.toml crates/flpdf-qtest-tools/Cargo.toml
git commit -m "build: add DCT backend dependencies"
~~~

## Task 2: Write the canonical RED tests

**Files:**
- Test/modify: crates/flpdf/src/stream_filter.rs existing unit-test module

- [ ] **Step 1: Add a real JPEG test helper and a recording downstream**

Use the selected published crate only to create deterministic test input; the qpdf differential remains the byte-parity authority. Add helpers with these shapes:

~~~rust
fn test_jpeg() -> Vec<u8> {
    let pixels = [
        0u8, 32, 64, 96, 128, 160, 192, 224, 255, 240, 120, 8,
    ];
    libjpeg_turbo_rs::compress(
        &pixels,
        2,
        2,
        libjpeg_turbo_rs::PixelFormat::Rgb,
        75,
        libjpeg_turbo_rs::Subsampling::S444,
    )
    .expect("test JPEG must encode")
}

#[derive(Default)]
struct DctSink {
    writes: Vec<Vec<u8>>,
    finishes: usize,
    fail_write: bool,
    fail_finish: bool,
}
~~~

Implement Pipeline for DctSink. write stores each chunk unless fail_write is true; finish increments finishes unless fail_finish is true. Return PipelineError::runtime with stable messages "dct test write failure" and "dct test finish failure".

- [ ] **Step 2: Add tests for the factory contract and lifecycle**

Add tests named dct_factory_is_registered_and_classified,
dct_factory_accepts_only_absent_decode_params,
dct_stage_decodes_chunked_input_one_scanline_per_write,
dct_stage_empty_and_repeated_finish_forward_finish,
dct_stage_preserves_codec_error_and_does_not_finish_downstream,
dct_stage_preserves_downstream_write_error, and
dct_stage_preserves_downstream_finish_error.

The bodies must exercise stream_filter_for(b"DCTDecode"), call decode_pipeline, write the JPEG in multiple chunks, call finish, and assert bytes, chunk count, finish count, and error text. The stage test must not call pipe_decode_recovering or filters::decode_stream_data; those are bridge tests and cannot be the acceptance authority for this primitive.

- [ ] **Step 3: Run the focused tests and verify the RED state**

Run:

~~~bash
cargo test -p flpdf --lib stream_filter::tests::dct -- --nocapture
~~~

Expected: the tests compile using the new dependency and fail at the missing DCTDecode registry entry because stream_filter_for returns None. Do not change production code to make the tests pass in this step.

## Task 3: Implement the Rust Pl_DCT stage and SF_DCTDecode factory

**Files:**
- Create: crates/flpdf/src/pipeline/dct.rs
- Modify: crates/flpdf/src/pipeline.rs
- Modify: crates/flpdf/src/stream_filter.rs

- [ ] **Step 1: Add the qpdf-shaped buffered stage**

Implement this structure in pipeline/dct.rs:

~~~rust
pub(crate) struct PlDct<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    buffer: Buffer<'static>,
}

impl<'a> PlDct<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: &'a mut dyn Pipeline) -> Self;
}

impl Pipeline for PlDct<'_> {
    fn identifier(&self) -> &str;
    fn write(&mut self, data: &[u8]) -> PipelineResult<()>;
    fn finish(&mut self) -> PipelineResult<()>;
}
~~~

Construct Buffer with no downstream. write delegates only to Buffer::write. finish calls Buffer::finish, takes the owned bytes, calls next.finish when the buffer is empty, and otherwise routes the bytes to the selected backend. The taken buffer makes a second finish follow qpdf's empty-buffer path. Do not call downstream during compressed-input writes.

- [ ] **Step 2: Implement the default Rust scanline backend**

Use libjpeg_turbo_rs::ScanlineDecoder::new(data) without setting output format or decoder options. Reject header().precision != 8 with PipelineError::runtime before pixel output. Derive the default row length as width times 1 for grayscale, 3 for RGB/YCbCr, or 4 for CMYK/YCCK using checked arithmetic. For every row, call read_scanline, then exactly one next.write(row). After all rows, call next.finish.

Map JpegError to PipelineError::Runtime and return downstream PipelineError values unchanged. Keep the adapter in this module so the stage's qpdf responsibility is not split across stream_filter.rs.

- [ ] **Step 3: Register the module and factory**

Add pub(crate) mod dct to pipeline.rs, import PlDct into stream_filter.rs, and implement:

~~~rust
struct DctStreamFilter;

impl StreamFilter for DctStreamFilter {
    fn decode_pipeline<'a>(
        &mut self,
        next: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>> {
        Ok(Some(Box::new(PlDct::new("DCT decode", next))))
    }

    fn is_specialized_compression(&self) -> bool { true }
    fn is_lossy_compression(&self) -> bool { true }
}
~~~

Add b"DCTDecode" => Some(Box::new(DctStreamFilter)) to stream_filter_for. Leave normalize_filter_name, filters.rs, pipe_decode_recovering, and pipe_codec otherwise unchanged.

- [ ] **Step 4: Run the RED tests to GREEN**

Run:

~~~bash
cargo test -p flpdf --lib stream_filter::tests::dct -- --nocapture
~~~

Expected: all DCT factory, scanline, empty/repeated-finish, malformed-input, and downstream-failure tests pass. If an assertion fails, compare call order with libqpdf/Pl_DCT.cc:83-141,298-326; do not weaken the test to match an implementation detail.

- [ ] **Step 5: Run existing filter and pipeline tests**

~~~bash
cargo test -p flpdf --lib pipeline::tests
cargo test -p flpdf --lib filters::tests
cargo test -p flpdf --test compress_streams_tests
~~~

Expected: all pass, including existing DCT writer passthrough tests. A passthrough failure means decode registration leaked into writer policy and must be corrected at the responsibility boundary.

- [ ] **Step 6: Commit the canonical Rust implementation**

~~~bash
git add crates/flpdf/src/pipeline.rs crates/flpdf/src/pipeline/dct.rs crates/flpdf/src/stream_filter.rs
git commit -m "feat: add qpdf DCT decode pipeline"
~~~

## Task 4: Add and implement the explicit C-libjpeg compatibility backend

**Files:**
- Create: crates/flpdf/build.rs
- Create: crates/flpdf/src/jpeg_compat.h
- Create: crates/flpdf/src/jpeg_compat.c
- Modify: crates/flpdf/src/pipeline/dct.rs
- Modify: crates/flpdf/Cargo.toml

- [ ] **Step 1: Add a feature-gated compatibility test before the C shim**

Add a cfg(feature = "qpdf-libjpeg-compat") test that sends test_jpeg through the canonical DCT factory and compares emitted rows with the qpdf probe helper from Task 5. The test must compile only when the feature is enabled and must fail before the FFI symbol exists with the expected missing-backend/build failure.

- [ ] **Step 2: Define the C callback ABI**

Use this header contract:

~~~c
typedef int (*flpdf_jpeg_scanline_callback)(
    void *user,
    const unsigned char *row,
    size_t row_len);

int flpdf_jpeg_decode_scanlines(
    const unsigned char *data,
    size_t data_len,
    flpdf_jpeg_scanline_callback callback,
    void *user,
    char *error_message,
    size_t error_message_len);
~~~

The C implementation must use jpeg_std_error, a setjmp-based error_exit, jpeg_mem_src, jpeg_read_header, jpeg_calc_output_dimensions, jpeg_start_decompress, one jpeg_read_scanlines call per row, jpeg_finish_decompress, and jpeg_destroy_decompress. Do not set out_color_space, DCT method, upsampling, smoothing, or fast flags. Return distinct success, codec-error, and callback-error codes; copy libjpeg's formatted diagnostic only for codec errors.

- [ ] **Step 3: Compile the shim only for the compatibility feature**

build.rs must check CARGO_FEATURE_QPDF_LIBJPEG_COMPAT, compile src/jpeg_compat.c with cc::Build, print cargo:rustc-link-lib=jpeg, and emit cargo:rerun-if-changed for both C files. A default build must not require jpeglib.h or link libjpeg.

- [ ] **Step 4: Add the Rust FFI adapter and backend selection**

Declare the one C function in a private module under cfg(feature = "qpdf-libjpeg-compat"). The Rust callback stores the first downstream PipelineError in local state and returns callback-error to C. The adapter returns the stored downstream error unchanged and maps C codec errors to PipelineError::Runtime. Select this adapter from finish only under the feature, leaving libjpeg-turbo-rs as the default implementation.

- [ ] **Step 5: Run both backend test matrices**

~~~bash
cargo test -p flpdf --lib stream_filter::tests::dct -- --nocapture
cargo test -p flpdf --features qpdf-libjpeg-compat --lib stream_filter::tests::dct -- --nocapture
~~~

Expected: default tests use Rust 0.8.0; feature tests use system libjpeg and produce the same qpdf scanline bytes. If the feature build cannot find jpeglib.h or libjpeg, report the missing system prerequisite instead of adding a vendored fallback.

- [ ] **Step 6: Commit the explicit fallback**

~~~bash
git add crates/flpdf/build.rs crates/flpdf/src/jpeg_compat.h crates/flpdf/src/jpeg_compat.c crates/flpdf/src/pipeline/dct.rs crates/flpdf/Cargo.toml
git commit -m "feat: add explicit libjpeg DCT compatibility backend"
~~~

## Task 5: Add the pinned qpdf differential and correspondence record

**Files:**
- Modify: crates/flpdf/src/stream_filter.rs test module
- Modify: docs/qpdf-correspondence.md current Pl_DCT row

- [ ] **Step 1: Add a deterministic PDF probe helper**

Create a test-only helper that builds a minimal PDF containing object 3 0 as an image XObject with /Filter /DCTDecode, /Width 2, /Height 2, /ColorSpace /DeviceRGB, /BitsPerComponent 8, and the exact JPEG bytes from test_jpeg. Compute every xref offset from assembled bytes; do not use placeholder offsets. Write it through tempfile and verify /Root and startxref before invoking qpdf.

- [ ] **Step 2: Compare canonical stage output with qpdf 11.9.0**

Run qpdf from the test using:

~~~text
qpdf --show-object=3 --filtered-stream-data <fixture.pdf>
~~~

Capture stdout, stderr, and exit status. Assert exit status 0, empty stderr, and exact equality between stdout and the bytes recorded from DctStreamFilter::decode_pipeline. Skip only when qpdf is absent, using the same explicit skip convention as existing live-oracle tests. Record in the test comment that qpdf 11.9.0 removes /DCTDecode at decode level all and writes libjpeg scanlines unchanged.

- [ ] **Step 3: Run the oracle test with both backends**

~~~bash
cargo test -p flpdf --lib stream_filter::tests::dct_qpdf -- --nocapture
cargo test -p flpdf --features qpdf-libjpeg-compat --lib stream_filter::tests::dct_qpdf -- --nocapture
~~~

Expected: both backend outputs equal pinned qpdf output. If only the C feature matches, record the Rust mismatch as DCT-attributable and keep the feature as strict compatibility route; do not add a runtime per-input switch. If neither matches, stop and return to qpdf source/probe before changing the implementation.

- [ ] **Step 4: Update the correspondence row**

Replace the Pl_DCT.cc row that currently says 無し/❌ 消費者あり with a mapping to pipeline/dct.rs and stream_filter.rs. Include qpdf locations Pl_DCT.hh:30-70, Pl_DCT.cc:83-141,298-326, SF_DCTDecode.hh:8-40; buffered finish, empty/repeated finish, scanline, and error mapping; the Rust backend decision and qpdf-libjpeg-compat feature; the stage-owner Rust value/borrow substitution as class (B); and the remaining whole-buffer bridge caller and later flpdf-3yn9.6 cutover.

- [ ] **Step 5: Run documentation and focused tests**

~~~bash
cargo fmt --all -- --check
cargo test -p flpdf --lib stream_filter::tests::dct -- --nocapture
git diff --check
~~~

Expected: formatting and focused tests pass, and the correspondence row contains no stale Pl_DCT missing marker.

- [ ] **Step 6: Commit the oracle and correspondence record**

~~~bash
git add crates/flpdf/src/stream_filter.rs docs/qpdf-correspondence.md
git commit -m "test: pin DCT output against qpdf"
~~~

## Task 6: Verify complete implementation and changed-line coverage

**Files:**
- No new source files; verify all files from Tasks 1-5.

- [ ] **Step 1: Run focused crate tests**

~~~bash
cargo test -p flpdf --lib pipeline::dct
cargo test -p flpdf --lib stream_filter::tests::dct
cargo test -p flpdf --test compress_streams_tests
~~~

Expected: all DCT tests and existing writer passthrough tests pass under the default Rust backend.

- [ ] **Step 2: Run workspace format, clippy, and tests**

~~~bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
~~~

Expected: all commands exit 0. The all-features command must compile the C compatibility path; it must not enable upstream full-c-parity behavior.

- [ ] **Step 3: Run qpdf compatibility consumers**

~~~bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_matrix_tests
cargo test -p flpdf-qtest-tools --features qpdf-zlib-compat --test e2e
~~~

Run the DCT qtest subset in the external qtest worktree if available. Attribute any mismatch to the changed DCT path, the existing zlib exception, or an unrelated consumer before selecting qpdf-libjpeg-compat.

- [ ] **Step 4: Run fresh patch coverage**

~~~bash
cargo llvm-cov --workspace --features qpdf-libjpeg-compat --ignore-run-fail --lcov --output-path target/dct-patch.lcov
scripts/patch-coverage.sh --base main --lcov target/dct-patch.lcov
~~~

Expected: every changed executable line is covered, with no cov:ignore marker unless the C error callback or missing system library path is demonstrably unobservable in the supported test environment and the reason is recorded beside the marker.

- [ ] **Step 5: Read back the final branch and Beads state**

Run:

~~~bash
git status --short
git log --oneline --decorate -8
bd show flpdf-25kg.4.2.1
bd dep cycles
~~~

Expected: all implementation commits are on feature/flpdf-25kg-4-2-1-dct-streaming, no implementation commit exists on main, the target remains open until verified work is ready to close, and no dependency cycle is introduced.

- [ ] **Step 6: Report completion without merging**

Provide the user the worktree path, commit list, focused and workspace test results, qpdf differential counts, backend selected for the passing matrix, remaining bridge callers, and any qtest mismatch classification. Do not merge, push, close the Bead, or remove the worktree until the user explicitly chooses the integration step.
