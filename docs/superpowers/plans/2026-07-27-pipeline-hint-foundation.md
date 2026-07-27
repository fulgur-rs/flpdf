# Pipeline and Hint-Stream Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add complete qpdf-shaped `Pipeline`, `Pl_Buffer`, `Pl_Count`, `Pl_Flate`, `BitStream`, and `BitWriter` components, then make linearization hint writing and reading use them as their only production routes.

**Architecture:** A crate-private borrowed pipeline chain replaces the hint encoder's owned builder and one-shot zlib call. Separate root modules implement qpdf's MSB-first bit reader and pipeline-backed bit writer; short-lived writers at byte-aligned hint-section boundaries allow Rust to inspect the reusable count stage without shared mutable ownership.

**Tech Stack:** Rust 2021; `flate2` low-level streaming API; `thiserror`; qpdf 11.9.0 pinned source and live binary oracle; Cargo tests, Clippy, strict rustdoc, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

## Global Constraints

- Resolve the read-only oracle with `scripts/fetch-qpdf-source.sh --print-path`; do not edit or re-clone it.
- `Pipeline`, all initial stages, `BitStream`, and `BitWriter` are `pub(crate)`.
- Mirror qpdf responsibilities, lifecycle, input domain, error timing, and bytes; do not reproduce C++ inheritance, raw pointers, `dynamic_cast`, or the global writer pipeline stack.
- The caller must call `finish`; `Drop` must not finish a stage.
- Store downstream stages as borrowed `&mut dyn Pipeline`; do not use `Rc<RefCell<_>>`, downcast, or a boxed ownership graph.
- `Pipeline`, `Pl_Buffer`, `Pl_Count`, `Pl_Flate`, `BitStream`, and `BitWriter` must each satisfy D1; the production dogfood uses only `Pl_Flate` deflate mode, but inflate, callback, compression-level, output-buffer, reuse, and finish behavior remain required.
- Delete `HintStreamBuilder`, `linearization/show.rs::BitReader`, the direct hint `ZlibEncoder`, their re-export, and all production/test callsites before completion. Do not leave aliases or wrappers.
- Keep `HintStreamBytes` and its four existing public fields unchanged.
- Existing raw and `qpdf-zlib-compat` compressed hint bytes, `/S`, `/O`, `--show-linearization`, and `--check-linearization` are byte/behavior gates.
- Every production step follows RED→GREEN→REFACTOR and every changed executable line must have fresh 100% patch coverage against the immediate parent branch.
- Do not modify work owned by `flpdf-qxba.7` or `flpdf-80b6`.

## Delivery Boundary

This plan produces one reviewable foundations PR. Internal commits may temporarily contain both
old and new helpers, but the PR is not complete until the old routes are deleted and the grep
audit is clean.

**Branch:** `feature/flpdf-qxba-phase2-pipeline`
**PR base / patch-coverage base:** `origin/main`

---

### Task 0: Create the approved Beads hierarchy and dependency graph

**Files:**
- Tracking only: Beads database under `.beads`
- Design: `docs/superpowers/specs/2026-07-27-qpdf-phase-2-foundations-design.md`
- Plans: the four `2026-07-27-*-foundation/cutover/users/completion.md` documents

**Interfaces:**
- Produces a bounded Phase 2 Foundations Epic under `flpdf-qxba`.
- Produces four ordered child issues, one for each implementation plan.
- Produces a separate full-Pipeline Epic whose inventory entry task is blocked by the Pipeline
  foundation child. Beads does not permit a task to block an Epic directly.
- Produces an explicit future RC4 child whose acceptance criterion is qpdf component-contract
  parity, not normal PDF key-size coverage.

**Created 2026-07-27:** foundations `flpdf-qxba.9` with children
`flpdf-qxba.9.1`–`.9.4`; full Pipeline `flpdf-qynx` with children
`flpdf-qynx.1`–`.4`.

- [x] **Step 1: Refresh Beads and reject duplicate titles**

Run:

```bash
bd prime
bd list --json | rg -n \
  "Phase 2 Foundations|Pipeline contract.*hint|XRefEntry complete|Optimization object-user|Optimization inherited|full Pipeline completion|stateful RC4"
```

Expected: no existing issue already represents one of the proposed exact scopes. If a matching
issue exists, use its ID in the later dependency commands and do not create a duplicate.

- [x] **Step 2: Create the bounded foundations Epic and four children**

Run:

```bash
foundation_id="$(bd create \
  --parent flpdf-qxba \
  --type epic \
  --priority 1 \
  --title "Phase 2 Foundations: Pipeline基盤と埋没責務のqpdf境界化" \
  --description "Pipeline契約の実dogfood、XRefEntry完全cutover、QPDF_optimization責務の2段階cutoverを行う。各sliceは全callsite移行と旧実装削除までをscopeとし、全Pl_*完成は別Epicとする。" \
  --design "docs/superpowers/specs/2026-07-27-qpdf-phase-2-foundations-design.md" \
  --silent)"

pipeline_id="$(bd create \
  --parent "$foundation_id" \
  --type task \
  --priority 1 \
  --title "Pipeline契約 + BitStream/BitWriter + hint/show完全cutover" \
  --description "Pipeline、Pl_Buffer、Pl_Count、Pl_Flate、BitStream、BitWriterをqpdf 11.9.0責務で完成させる。hint write/show readを新経路へ移し、HintStreamBuilder、private BitReader、direct hint ZlibEncoderを削除する。" \
  --design "docs/superpowers/plans/2026-07-27-pipeline-hint-foundation.md" \
  --silent)"

xref_id="$(bd create \
  --parent "$foundation_id" \
  --type task \
  --priority 1 \
  --title "XRefEntry complete cutover（XrefOffset削除）" \
  --description "xref_entry.rsへFree/Uncompressed/Compressed値責務を分離し、reader/cache/writer/ObjStm/linearization/testの全consumerを移行する。XrefOffset aliasやwrapperは残さない。" \
  --design "docs/superpowers/plans/2026-07-27-xref-entry-cutover.md" \
  --silent)"

optimization_users_id="$(bd create \
  --parent "$foundation_id" \
  --type task \
  --priority 1 \
  --title "Optimization object-user map complete cutover" \
  --description "qpdf ObjUserと双方向map、updateObjectMaps traversalをoptimization.rsへ移す。linearizationのpage/thumb/root/trailer分類consumerを全移行し、旧bespoke traversalを削除する。" \
  --design "docs/superpowers/plans/2026-07-27-optimization-object-users.md" \
  --silent)"

optimization_complete_id="$(bd create \
  --parent "$foundation_id" \
  --type task \
  --priority 1 \
  --title "Optimization inherited attrs + compressed-user completion" \
  --description "page repairをpagesへ、pushInheritedAttributesToPageをoptimizationへ配置し、optimize順序とfilterCompressedObjectsを完成させる。linearization/inherited_attrs.rsとmember-union重複を削除してD1/D2を閉じる。" \
  --design "docs/superpowers/plans/2026-07-27-optimization-inherited-completion.md" \
  --silent)"
```

Expected: five non-empty IDs. Do not pass `--id` together with `--parent`.

- [x] **Step 3: Create the ordered foundations dependency chain**

Run:

```bash
bd dep "$pipeline_id" --blocks "$xref_id"
bd dep "$xref_id" --blocks "$optimization_users_id"
bd dep "$optimization_users_id" --blocks "$optimization_complete_id"
bd dep cycles
```

Expected: no dependency cycles.

- [x] **Step 4: Create the separate full-Pipeline Epic and scoped future children**

Run:

```bash
full_pipeline_id="$(bd create \
  --type epic \
  --priority 1 \
  --title "qpdf full Pipeline / Pl_* completion and consumer cutover" \
  --description "Foundation後にqpdf 11.9.0の全Pipeline/Pl_*カテゴリを棚卸しし、stageごとの実装とproduction consumerのvertical cutoverを行う。対象カテゴリはstring/file/concatenate/null/debug、stream filters、QPDFStreamFilter、LZW/PNG/TIFF/DCT、AES/RC4、MD5/SHA adapters、writer/ObjStm/xref/JSON/inspection、QPDFLogger、ResourceFinder/Replacer。各sliceで旧routeを削除する。" \
  --design "docs/superpowers/specs/2026-07-27-qpdf-phase-2-foundations-design.md" \
  --silent)"

pipeline_inventory_id="$(bd create \
  --parent "$full_pipeline_id" \
  --type task \
  --priority 1 \
  --title "qpdf Pipeline/Pl_* public responsibility and consumer inventory" \
  --description "qpdf 11.9.0 headersの全public API、flpdf既存one-shot/streaming実装、全consumerを列挙し、vertical cutover単位と依存順を確定する。全stage先行追加は禁止する。" \
  --silent)"

rc4_id="$(bd create \
  --parent "$full_pipeline_id" \
  --type task \
  --priority 1 \
  --title "stateful RC4 + PlRc4 qpdf full-contract parity" \
  --description "現行one-shot KSA/PRGAをstateful rc4.rsへ抽出し、one-shot wrapperとPlRc4を同じ状態機械へ通す。runtime key_len、-1 NUL mode、1/5/16/256/>256 keys、multi-chunk、in-place/out-of-place、65536境界をqpdf比較する。rc4 crateは1-256 compile-time key size制約で契約不一致のためprimitiveには使わない。" \
  --silent)"

resource_id="$(bd create \
  --parent "$full_pipeline_id" \
  --type task \
  --priority 2 \
  --title "TokenFilter-based ResourceFinder / ResourceReplacer cutover" \
  --description "flpdf-qxba.7のTokenizer/ContentNormalizer境界とPipeline基盤を使い、qpdf ResourceFinder/Replacerを実装する。既存resource走査consumerを移行し旧routeを削除する。" \
  --silent)"

logger_id="$(bd create \
  --parent "$full_pipeline_id" \
  --type task \
  --priority 2 \
  --title "QPDFLogger pipeline sinks and CLI output cutover" \
  --description "info/warn/error/saveのpipeline sink契約を用意し、libraryとflpdf-cliのdirect println/eprintln経路をscopeごとに移行・削除する。" \
  --silent)"
```

- [x] **Step 5: Add full-Pipeline dependencies and verify readback**

Run:

```bash
bd dep "$pipeline_id" --blocks "$pipeline_inventory_id"
bd dep "$pipeline_inventory_id" --blocks "$rc4_id"
bd dep "$pipeline_inventory_id" --blocks "$resource_id"
bd dep "$pipeline_inventory_id" --blocks "$logger_id"
bd dep flpdf-qxba.7 --blocks "$resource_id"
bd dep cycles
bd show "$foundation_id"
bd show "$full_pipeline_id"
```

Expected: no cycles; four bounded foundation children; separate full-Pipeline Epic; RC4 decision
and ResourceFinder's dual dependency visible in readback.

- [x] **Step 6: Persist Beads**

Run:

```bash
bd dolt push
```

Expected: `Push complete.`

---

### Task 1: Define Pipeline lifecycle and error contract

**Files:**
- Create: `crates/flpdf/src/pipeline.rs`
- Modify: `crates/flpdf/src/lib.rs:104-175`
- Modify: `crates/flpdf/src/error.rs:1-68`
- Test: `crates/flpdf/src/pipeline.rs`
- Test: `crates/flpdf/src/error.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/Pipeline.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/Pipeline.cc`

**Interfaces:**
- Produces:

```rust
pub(crate) type PipelineResult<T> = std::result::Result<T, PipelineError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PipelineError {
    #[error("{0}")]
    Logic(String),
    #[error("{0}")]
    Runtime(String),
}

pub(crate) trait Pipeline {
    fn identifier(&self) -> &str;
    fn write(&mut self, data: &[u8]) -> PipelineResult<()>;
    fn finish(&mut self) -> PipelineResult<()>;
}
```

- Adds public boundary:

```rust
Internal(String),
System(String),
```

- Adds `impl From<PipelineError> for crate::Error`; qpdf logic/runtime exceptions map to
  `Internal`/`System`, matching qpdf's C adapter.

- [ ] **Step 1: Write failing lifecycle and public mapping tests**

Add to `pipeline.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct FaultSink {
        id: &'static str,
        writes: usize,
        finishes: usize,
    }

    impl Pipeline for FaultSink {
        fn identifier(&self) -> &str {
            self.id
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            self.writes += 1;
            Err(PipelineError::logic(format!("{}: write failed", self.id)))
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }

    #[test]
    fn pipeline_error_retains_qpdf_exception_category_and_message() {
        let err = PipelineError::runtime("flate: inflate: data: incorrect header check");
        assert!(matches!(err, PipelineError::Runtime(_)));
        assert_eq!(
            err.to_string(),
            "flate: inflate: data: incorrect header check"
        );
    }
}
```

Add to `error.rs`:

```rust
#[test]
fn pipeline_runtime_error_maps_to_qpdf_system_category() {
    let public: Error =
        crate::pipeline::PipelineError::runtime("inflate: inflate: data: corrupt stream").into();
    assert!(matches!(
        public,
        Error::System(ref message) if message == "inflate: inflate: data: corrupt stream"
    ));
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p flpdf pipeline::tests::pipeline_error_retains_qpdf_exception_category_and_message -- --exact
cargo test -p flpdf error::tests::pipeline_runtime_error_maps_to_qpdf_system_category -- --exact
```

Expected: compile failure because `pipeline`, `PipelineError`, `Error::Internal`, and
`Error::System` do not exist.

- [ ] **Step 3: Implement the minimal contract**

Create `pipeline.rs` with the exact interfaces above and `logic`/`runtime` constructors.
Add:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/Pipeline.cc.
```

Declare only the trait and errors so this commit compiles. Task 2 adds `pub(crate) mod buffer;`
and `pub(crate) mod count;`; Task 3 adds `pub(crate) mod flate;`. Add
`pub(crate) mod pipeline;` to `lib.rs`, the public error variant, and:

```rust
impl From<crate::pipeline::PipelineError> for Error {
    fn from(error: crate::pipeline::PipelineError) -> Self {
        match error {
            crate::pipeline::PipelineError::Logic(message) => Self::Internal(message),
            crate::pipeline::PipelineError::Runtime(message) => Self::System(message),
        }
    }
}
```

Expose a crate-private `message()` accessor; do not make `PipelineError` public. Construct the
complete qpdf `what()`-equivalent message at each error site, and propagate downstream or callback
errors unchanged.

- [ ] **Step 4: Run focused and error tests**

Run:

```bash
cargo test -p flpdf pipeline::tests --lib
cargo test -p flpdf error::tests --lib
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/pipeline.rs crates/flpdf/src/lib.rs crates/flpdf/src/error.rs
git commit -m "feat: add pipeline lifecycle contract"
```

---

### Task 2: Complete Pl_Buffer and Pl_Count

**Files:**
- Create: `crates/flpdf/src/pipeline/buffer.rs`
- Create: `crates/flpdf/src/pipeline/count.rs`
- Modify: `crates/flpdf/src/pipeline.rs`
- Test: `crates/flpdf/src/pipeline/buffer.rs`
- Test: `crates/flpdf/src/pipeline/count.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/Pl_Buffer.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/Pl_Buffer.cc`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/Pl_Count.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/Pl_Count.cc`

**Interfaces:**
- Produces:

```rust
pub(crate) struct Buffer<'a> {
    identifier: String,
    next: Option<&'a mut dyn Pipeline>,
    data: Vec<u8>,
    ready: bool,
}

impl<'a> Buffer<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: Option<&'a mut dyn Pipeline>) -> Self;
    pub(crate) fn take_buffer(&mut self) -> PipelineResult<Vec<u8>>;
}

pub(crate) struct Count<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    count: u64,
    last_byte: u8,
}

impl<'a> Count<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: &'a mut dyn Pipeline) -> Self;
    pub(crate) fn count(&self) -> u64;
    pub(crate) fn last_byte(&self) -> u8;
}
```

- `Buffer` starts ready, any `write` including an empty write makes it not ready, optionally forwards that exact slice, and `finish` makes it ready before finishing downstream.
- `take_buffer` requires ready state, moves out the bytes, and leaves an empty reusable buffer.
- `Count` ignores and does not forward empty writes, accumulates using checked `u64`, retains the last non-empty byte, and remains reusable after `finish`.

- [ ] **Step 1: Write failing Buffer tests**

```rust
#[test]
fn buffer_requires_finish_then_takes_and_resets() {
    let mut buffer = Buffer::new("buffer", None);
    buffer.write(b"ab").unwrap();
    assert_eq!(
        buffer.take_buffer().unwrap_err().to_string(),
        "Pl_Buffer::getBuffer() called when not ready"
    );
    buffer.finish().unwrap();
    assert_eq!(buffer.take_buffer().unwrap(), b"ab");
    assert_eq!(buffer.take_buffer().unwrap(), b"");
}

#[test]
fn buffer_retains_and_passes_through_exact_chunks() {
    let mut sink = RecordingSink::default();
    let retained;
    {
        let mut buffer = Buffer::new("tee", Some(&mut sink));
        buffer.write(b"ab").unwrap();
        buffer.write(b"").unwrap();
        buffer.write(b"cd").unwrap();
        buffer.finish().unwrap();
        retained = buffer.take_buffer().unwrap();
    }
    assert_eq!(retained, b"abcd");
    assert_eq!(sink.chunks, vec![b"ab".to_vec(), Vec::new(), b"cd".to_vec()]);
    assert_eq!(sink.finishes, 1);
}
```

Define `RecordingSink` inside the test module; it records every slice and finish call.

- [ ] **Step 2: Run Buffer tests and verify RED**

Run:

```bash
cargo test -p flpdf pipeline::buffer::tests --lib
```

Expected: compile failure because `pipeline/buffer.rs` and `Buffer` are absent.

- [ ] **Step 3: Implement Buffer**

Implement the exact qpdf state transitions:

```rust
pub(crate) fn take_buffer(&mut self) -> PipelineResult<Vec<u8>> {
    if !self.ready {
        return Err(PipelineError::logic(
            "Pl_Buffer::getBuffer() called when not ready",
        ));
    }
    Ok(std::mem::take(&mut self.data))
}
```

`write` must append and set `ready = false` before forwarding. `finish` must set
`ready = true` before invoking downstream so buffer readiness does not depend on downstream
success.

- [ ] **Step 4: Write failing Count tests**

```rust
#[test]
fn count_ignores_empty_writes_and_is_reusable_after_finish() {
    let mut sink = RecordingSink::default();
    {
        let mut count = Count::new("count", &mut sink);
        count.write(b"abc").unwrap();
        count.write(b"").unwrap();
        assert_eq!(count.count(), 3);
        assert_eq!(count.last_byte(), b'c');
        count.finish().unwrap();
        count.write(b"d").unwrap();
        assert_eq!(count.count(), 4);
        assert_eq!(count.last_byte(), b'd');
    }
    assert_eq!(sink.chunks, vec![b"abc".to_vec(), b"d".to_vec()]);
}

#[test]
fn empty_count_reports_qpdf_defaults() {
    let mut sink = RecordingSink::default();
    let count = Count::new("count", &mut sink);
    assert_eq!(count.count(), 0);
    assert_eq!(count.last_byte(), 0);
}
```

- [ ] **Step 5: Run Count tests and verify RED**

Run:

```bash
cargo test -p flpdf pipeline::count::tests --lib
```

Expected: compile failure because `Count` is absent.

- [ ] **Step 6: Implement Count and run both modules**

Use `checked_add(data.len() as u64)` and return a state error on overflow. Run:

```bash
cargo test -p flpdf pipeline::buffer::tests --lib
cargo test -p flpdf pipeline::count::tests --lib
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf/src/pipeline.rs crates/flpdf/src/pipeline/buffer.rs crates/flpdf/src/pipeline/count.rs
git commit -m "feat: add buffer and count pipeline stages"
```

---

### Task 3: Complete Pl_Flate streaming contract

**Files:**
- Create: `crates/flpdf/src/pipeline/flate.rs`
- Modify: `crates/flpdf/src/pipeline.rs`
- Test: `crates/flpdf/src/pipeline/flate.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/Pl_Flate.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/Pl_Flate.cc`

**Interfaces:**
- Consumes: `Pipeline`, `PipelineError`, borrowed downstream.
- Produces:

```rust
pub(crate) const DEFAULT_OUT_BUFFER_SIZE: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlateAction {
    Inflate,
    Deflate,
}

enum FlateCodec {
    Inflate(flate2::Decompress),
    Deflate(flate2::Compress),
}

pub(crate) struct Flate<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    codec: Option<FlateCodec>,
    output: Vec<u8>,
    warn_callback:
        Option<Box<dyn FnMut(&str, i32) -> PipelineResult<()> + 'a>>,
}

impl<'a> Flate<'a> {
    pub(crate) fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        action: FlateAction,
        out_buffer_size: usize,
    ) -> PipelineResult<Self>;
    pub(crate) fn set_compression_level(level: i32) -> PipelineResult<()>;
    pub(crate) fn set_warn_callback(
        &mut self,
        callback: impl FnMut(&str, i32) -> PipelineResult<()> + 'a,
    );
}
```

- Compression level is process-wide like qpdf and stored in `AtomicI32`; valid values are `-1`
  and `1..=9`.
- Zero output-buffer size is a state error.
- Deflate uses zlib wrapping and qpdf default level.
- Inflate uses sync-flush semantics and treats zlib's exact `"incorrect data check"` case like
  qpdf; `Z_BUF_ERROR` invokes the callback and preserves valid output.
- `finish` finalizes local codec state, makes write-after-finish fail, and always attempts one
  downstream finish. The first error wins.

- [ ] **Step 1: Write failing deflate chunk and finish tests**

```rust
#[test]
fn deflate_is_invariant_to_input_chunking_and_finishes_zlib_stream() {
    let input = b"abcabcabcabcabcabc";
    let one = deflate_chunks(&[input.as_slice()], 65_536).unwrap();
    let many = deflate_chunks(&[b"a", b"bcabc", b"abcabcabcabc"], 3).unwrap();
    assert_eq!(one, many);

    let mut decoder = flate2::read::ZlibDecoder::new(one.as_slice());
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
    assert_eq!(decoded, input);
}

#[test]
fn write_after_finish_matches_qpdf_logic_error() {
    let mut sink = Buffer::new("sink", None);
    let mut flate = Flate::new("flate", &mut sink, FlateAction::Deflate, 8).unwrap();
    flate.finish().unwrap();
    let err = flate.write(b"x").unwrap_err();
    assert_eq!(
        err.to_string(),
        "flate: Pl_Flate: write() called after finish() called"
    );
}
```

The local `deflate_chunks` helper must build `Buffer <- Flate`, write each supplied chunk, finish
the stage, drop it, and take the buffer.

- [ ] **Step 2: Run deflate tests and verify RED**

Run:

```bash
cargo test -p flpdf pipeline::flate::tests::deflate_is_invariant_to_input_chunking_and_finishes_zlib_stream -- --exact
cargo test -p flpdf pipeline::flate::tests::write_after_finish_matches_qpdf_logic_error -- --exact
```

Expected: compile failure because `Flate` is absent.

- [ ] **Step 3: Implement incremental deflate**

Use `flate2::Compress`, `FlushCompress::None` for writes, and
`FlushCompress::Finish` for finish. Repeatedly drain into a fixed-size output buffer until the
codec reports no additional output; forward each non-empty output slice immediately. Never
buffer the whole logical stream inside `Flate`.

- [ ] **Step 4: Write failing inflate, callback, level, and first-error tests**

```rust
#[test]
fn inflate_is_invariant_to_input_and_output_boundaries() {
    let encoded = deflate_chunks(&[b"payload payload payload"], 7).unwrap();
    let decoded = inflate_chunks(encoded.chunks(2), 3).unwrap();
    assert_eq!(decoded, b"payload payload payload");
}

#[test]
fn compression_level_accepts_qpdf_domain_only() {
    Flate::set_compression_level(-1).unwrap();
    Flate::set_compression_level(1).unwrap();
    Flate::set_compression_level(9).unwrap();
    assert!(Flate::set_compression_level(0).is_err());
    assert!(Flate::set_compression_level(10).is_err());
    Flate::set_compression_level(-1).unwrap();
}

#[test]
fn codec_finish_error_still_finishes_downstream_and_keeps_first_error() {
    let mut sink = FinishFaultSink::default();
    let mut flate = Flate::new("inflate", &mut sink, FlateAction::Inflate, 8).unwrap();
    flate.write(b"not zlib").unwrap_or(());
    let err = flate.finish().unwrap_err();
    assert!(matches!(err, PipelineError::Runtime(_)));
    assert_eq!(
        err.to_string(),
        "inflate: inflate: data: incorrect header check"
    );
    assert_eq!(sink.finishes, 1);
}
```

Add oracle vectors for `Z_BUF_ERROR` warning and `"incorrect data check"` by compiling/running a
small C++ oracle test against the pinned qpdf build tooling already used by repository tests; keep
the resulting bytes/messages as flpdf-authored constants, not copied qpdf fixtures.

- [ ] **Step 5: Implement full inflate and cleanup behavior**

Use `flate2::Decompress` with `FlushDecompress::Sync` on write and
`FlushDecompress::Finish` on finish. Construct qpdf's complete runtime-error message at codec
failure sites, and propagate callback failures unchanged. On successful local finalization,
terminalize the codec before finishing downstream. On local failure, preserve the codec state,
best-effort finish downstream, and return the local failure as a runtime error:

```rust
match self.finish_codec() {
    Ok(()) => {
        self.finished = true;
        self.codec = None;
        self.next.finish()
    }
    Err(first) => {
        let _ = self.next.finish();
        Err(PipelineError::runtime(first.to_string()))
    }
}
```

This retains qpdf's failed-FDICT behavior: repeated `finish` and later `write` repeat the same
runtime error, and every failed `finish` still calls downstream `finish`.

- [ ] **Step 6: Run all Flate tests and oracle comparisons**

Run:

```bash
cargo test -p flpdf pipeline::flate::tests --lib
cargo test -p flpdf --features qpdf-zlib-compat pipeline::flate::tests --lib
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf/src/pipeline.rs crates/flpdf/src/pipeline/flate.rs
git commit -m "feat: add qpdf-compatible flate pipeline"
```

---

### Task 4: Add standalone BitStream

**Files:**
- Create: `crates/flpdf/src/bit_stream.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Test: `crates/flpdf/src/bit_stream.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/qpdf/BitStream.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/BitStream.cc`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/qpdf/bits_functions.hh`

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BitStreamError {
    #[error("overflow reading bit stream: wanted = {wanted}; available = {available}")]
    Exhausted { wanted: usize, available: usize },
    #[error("read_bits: too many bits requested")]
    TooWide,
    #[error("overflow skipping to next byte in bitstream")]
    AlignmentOverflow,
}

pub(crate) struct BitStream<'a> {
    data: &'a [u8],
    byte_position: usize,
    bit_offset: usize,
    bits_available: usize,
}

impl<'a> BitStream<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self;
    pub(crate) fn reset(&mut self);
    pub(crate) fn get_bits(&mut self, bits: usize) -> Result<u64, BitStreamError>;
    pub(crate) fn get_bits_signed(&mut self, bits: usize) -> Result<i64, BitStreamError>;
    pub(crate) fn get_bits_i32(&mut self, bits: usize) -> Result<i32, BitStreamError>;
    pub(crate) fn skip_to_next_byte(&mut self) -> Result<(), BitStreamError>;
}
```

- qpdf 11.9.0 permits zero-width unsigned reads and rejects widths above 32.
- Signed reads are tested only for valid non-zero widths and reproduce qpdf's observed boundary
  behavior exactly.

- [ ] **Step 1: Write failing qpdf bit-reader tests**

```rust
#[test]
fn reads_msb_first_across_bytes_and_resets() {
    let mut bits = BitStream::new(&[0b1011_0110, 0b1100_0011]);
    assert_eq!(bits.get_bits(3).unwrap(), 0b101);
    assert_eq!(bits.get_bits(5).unwrap(), 0b1_0110);
    assert_eq!(bits.get_bits(4).unwrap(), 0b1100);
    bits.reset();
    assert_eq!(bits.get_bits(8).unwrap(), 0b1011_0110);
}

#[test]
fn zero_width_alignment_and_exhaustion_match_qpdf() {
    let mut bits = BitStream::new(&[0x80, 0x5a]);
    assert_eq!(bits.get_bits(0).unwrap(), 0);
    assert_eq!(bits.get_bits(1).unwrap(), 1);
    bits.skip_to_next_byte().unwrap();
    assert_eq!(bits.get_bits(8).unwrap(), 0x5a);
    assert!(matches!(
        bits.get_bits(1),
        Err(BitStreamError::Exhausted { wanted: 1, available: 0 })
    ));
    assert_eq!(BitStream::new(&[0; 5]).get_bits(33), Err(BitStreamError::TooWide));
}
```

Add signed cases generated against qpdf for 1, 2, 8, 16, and 32-bit positive, sign-bit, and
all-ones values.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p flpdf bit_stream::tests --lib
```

Expected: compile failure because `BitStream` does not exist.

- [ ] **Step 3: Implement BitStream and run tests**

Implement qpdf's `bit_offset = 7` model rather than adapting the old
`show.rs::BitReader`'s consumed-bit model. Check available bits first, then the 32-bit width
limit, matching qpdf error precedence.

Run:

```bash
cargo test -p flpdf bit_stream::tests --lib
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/flpdf/src/bit_stream.rs crates/flpdf/src/lib.rs
git commit -m "feat: add qpdf bit stream reader"
```

---

### Task 5: Add pipeline-backed BitWriter

**Files:**
- Create: `crates/flpdf/src/bit_writer.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Test: `crates/flpdf/src/bit_writer.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/qpdf/BitWriter.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/BitWriter.cc`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/qpdf/bits_functions.hh`

**Interfaces:**
- Consumes: `&mut dyn Pipeline`.
- Produces:

```rust
pub(crate) struct BitWriter<'a> {
    pipeline: &'a mut dyn Pipeline,
    byte: u8,
    bit_offset: usize,
}

impl<'a> BitWriter<'a> {
    pub(crate) fn new(pipeline: &'a mut dyn Pipeline) -> Self;
    pub(crate) fn write_bits(&mut self, value: u64, bits: usize) -> PipelineResult<()>;
    pub(crate) fn write_bits_signed(&mut self, value: i64, bits: usize) -> PipelineResult<()>;
    pub(crate) fn write_bits_i32(&mut self, value: i32, bits: usize) -> PipelineResult<()>;
    pub(crate) fn flush(&mut self) -> PipelineResult<()>;
}
```

- Zero-width writes are no-ops; widths above 32 are state errors.
- `flush` writes one zero-padded byte only when partial data exists.
- `BitWriter` never calls pipeline `finish`, including on drop.

- [ ] **Step 1: Write failing writer and round-trip tests**

```rust
#[test]
fn writes_msb_first_flushes_padding_and_does_not_finish_pipeline() {
    let mut sink = TestSink::default();
    {
        let mut writer = BitWriter::new(&mut sink);
        writer.write_bits(0b101, 3).unwrap();
        writer.write_bits(0b1_0110, 5).unwrap();
        writer.write_bits(0b1100, 4).unwrap();
        writer.flush().unwrap();
    }
    assert_eq!(sink.bytes, [0b1011_0110, 0b1100_0000]);
    assert_eq!(sink.finishes, 0);
}

#[test]
fn writer_reader_round_trip_uses_only_byte_contract() {
    let mut sink = TestSink::default();
    {
        let mut writer = BitWriter::new(&mut sink);
        writer.write_bits(0xdead_beef, 32).unwrap();
        writer.write_bits_signed(-2, 4).unwrap();
        writer.flush().unwrap();
    }
    let mut reader = BitStream::new(&sink.bytes);
    assert_eq!(reader.get_bits(32).unwrap(), 0xdead_beef);
    assert_eq!(reader.get_bits_signed(4).unwrap(), -2);
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p flpdf bit_writer::tests --lib
```

Expected: compile failure because `BitWriter` is absent.

- [ ] **Step 3: Implement qpdf bit packing**

Port the `bits_functions.hh` write loop using a pending byte and bit offset. Emit each completed
byte with `self.pipeline.write(&[self.byte])?`, then reset the byte and offset. For negative signed
values, compute `(1_u64 << bits).wrapping_add_signed(value)` only after validating
`1..=32`.

- [ ] **Step 4: Run writer, reader, and round-trip tests**

Run:

```bash
cargo test -p flpdf bit_writer::tests --lib
cargo test -p flpdf bit_stream::tests --lib
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/bit_writer.rs crates/flpdf/src/lib.rs
git commit -m "feat: add pipeline-backed bit writer"
```

---

### Task 6: Cut the hint encoder over to the new pipeline

**Files:**
- Modify: `crates/flpdf/src/linearization/hint_stream.rs:86-211,270-621,703-806`
- Modify: `crates/flpdf/src/linearization/mod.rs:35`
- Test: `crates/flpdf/src/linearization/hint_stream.rs`
- Test: `crates/flpdf/tests/linearize_objstm_generate_tests.rs`
- Test: `crates/flpdf-cli/tests/cli_linearize.rs`
- Test: `crates/flpdf-cli/tests/cli_linearize_qpdf.rs`
- Test: `crates/flpdf/tests/zlib_compat_tests.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDF_linearization.cc:1635-1800`

**Interfaces:**
- Consumes: `Buffer`, `Count`, `Flate`, `FlateAction::Deflate`, and `BitWriter`.
- Preserves:

```rust
pub fn encode_hint_stream(
    page_offset: &PageOffsetHintTable,
    shared_object: &SharedObjectHintTable,
    outline: Option<&OutlineHintTable>,
) -> crate::Result<HintStreamBytes>;
```

- Deletes `HintStreamBuilder`, its `Default`, its tests, and its `linearization::mod.rs`
  re-export.

- [ ] **Step 1: Add a failing production-route test**

Add a test that uses a fault-injecting pipeline through a test-only
`encode_hint_sections` helper and asserts the qpdf public error category and message. Also retain the
existing expected byte vectors for page/shared/outline tables unchanged:

```rust
#[test]
fn hint_encoding_maps_pipeline_logic_error_to_qpdf_internal_category() {
    let err = encode_hint_sections_for_test(&minimal_tables(), FailingSink::new("hint sink"))
        .unwrap_err();
    assert!(matches!(
        err,
        crate::Error::Internal(ref message) if message == "hint sink: write failed"
    ));
}
```

Do not add a permanent public injection hook. The test-only helper accepts the terminal pipeline;
the public function constructs real buffers and flate.

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p flpdf linearization::hint_stream::tests::hint_encoding_uses_pipeline_error_boundary -- --exact
```

Expected: compile failure because the encoder has no pipeline route.

- [ ] **Step 3: Change section helpers to BitWriter**

Change every helper from `&mut HintStreamBuilder` to `&mut BitWriter<'_>` and return
`PipelineResult<()>` (or `crate::Result<()>` when it also validates numeric widths):

```rust
fn encode_page_offset_entries(
    writer: &mut BitWriter<'_>,
    table: &PageOffsetHintTable,
) -> PipelineResult<()>;

fn encode_shared_object_entries(
    writer: &mut BitWriter<'_>,
    table: &SharedObjectHintTable,
) -> PipelineResult<()>;
```

Replace `align_to_byte()` with `flush()?` at each existing alignment point. Preserve every
existing field order and checked fixed-width conversion.

- [ ] **Step 4: Assemble the borrowed chain and count checkpoints**

Implement this ownership shape:

```rust
let mut compressed_sink = Buffer::new("compressed hint stream", None);
let uncompressed;
let shared_section_offset;
let outline_section_offset;
{
    let mut flate = Flate::new(
        "compress hint stream",
        &mut compressed_sink,
        FlateAction::Deflate,
        DEFAULT_OUT_BUFFER_SIZE,
    )?;
    let mut raw = Buffer::new("raw hint stream", Some(&mut flate));
    {
        let mut count = Count::new("count hint stream", &mut raw);
        {
            let mut writer = BitWriter::new(&mut count);
            encode_page_section(&mut writer, page_offset)?;
            writer.flush()?;
        }
        shared_section_offset = usize::try_from(count.count())
            .map_err(|_| crate::Error::Unsupported("hint /S offset does not fit usize".into()))?;
        {
            let mut writer = BitWriter::new(&mut count);
            encode_shared_section(&mut writer, shared_object)?;
            writer.flush()?;
        }
        outline_section_offset = if let Some(outline) = outline {
            let offset = usize::try_from(count.count()).map_err(|_| {
                crate::Error::Unsupported("hint /O offset does not fit usize".into())
            })?;
            let mut writer = BitWriter::new(&mut count);
            encode_outline_section(&mut writer, outline)?;
            writer.flush()?;
            Some(offset)
        } else {
            None
        };
        count.finish()?;
    }
    uncompressed = raw.take_buffer()?;
}
let compressed = compressed_sink.take_buffer()?;
```

This exact scoping is required: each writer must drop before `count.count()`, `Count` before
`raw.take_buffer()`, and `Flate`/raw before taking the compressed sink.

- [ ] **Step 5: Delete the legacy encoder and run focused tests**

Delete `HintStreamBuilder`, its tests and re-export, and imports of
`flate2::write::ZlibEncoder`, `Compression`, and `std::io::Write` from this file.

Run:

```bash
cargo test -p flpdf linearization::hint_stream::tests --lib
cargo test -p flpdf --test linearize_objstm_generate_tests
cargo test -p flpdf-cli --test cli_linearize
```

Expected: PASS with unchanged expected raw bytes and offsets.

- [ ] **Step 6: Run compressed-byte and qpdf gates**

Run:

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test zlib_compat_tests
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_linearize_qpdf
```

Expected: PASS; gated compressed bytes remain byte-identical.

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf/src/linearization/hint_stream.rs crates/flpdf/src/linearization/mod.rs crates/flpdf/tests/linearize_objstm_generate_tests.rs crates/flpdf/tests/zlib_compat_tests.rs crates/flpdf-cli/tests/cli_linearize.rs crates/flpdf-cli/tests/cli_linearize_qpdf.rs
git commit -m "refactor: route hint encoding through pipelines"
```

---

### Task 7: Cut show-linearization over to BitStream

**Files:**
- Modify: `crates/flpdf/src/linearization/show.rs:1-181,276-465,1000-1080,1450-1710`
- Test: `crates/flpdf/src/linearization/show.rs`
- Test: `crates/flpdf/tests/show_linearization_tests.rs`
- Test: `crates/flpdf-cli/tests/cli_linearize.rs`

**Interfaces:**
- Consumes: `crate::bit_stream::{BitStream, BitStreamError}`.
- Deletes private `BitReader` and every legacy builder/reader test callsite.
- Adds private conversion:

```rust
impl From<BitStreamError> for ShowLinearizationError {
    fn from(error: BitStreamError) -> Self {
        ShowLinearizationError::Malformed {
            message: error.to_string(),
        }
    }
}
```

- [ ] **Step 1: Add a failing error-conversion test**

```rust
#[test]
fn truncated_hint_uses_bit_stream_error_text() {
    let err = read_h_page_offset(&[0xff], 1).unwrap_err();
    assert!(err
        .to_string()
        .contains("overflow reading bit stream"));
}
```

Use the current private decoder entry with the smallest arguments needed to request more than
eight bits.

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p flpdf linearization::show::tests::truncated_hint_uses_bit_stream_error_text -- --exact
```

Expected: FAIL because the old `BitReader` emits its old hint-specific truncation message.

- [ ] **Step 3: Replace every reader callsite**

Replace:

```rust
let mut h = BitReader::new(buf);
h.get_bits_u32(width)?;
h.skip_to_next_byte();
```

with:

```rust
let mut bits = BitStream::new(buf);
let value = u32::try_from(bits.get_bits(width as usize)?)
    .map_err(|_| malformed!("hint stream field does not fit u32"))?;
bits.skip_to_next_byte()?;
```

Use `get_bits_i32` only where the qpdf reader uses its integer form. Keep the decoder's existing
public malformed-error wrapper and output formatting.

- [ ] **Step 4: Move generic bit tests and rewrite fixture construction**

Move the cross-byte, zero-width, alignment, too-wide, exhaustion, and signed tests to
`bit_stream.rs`/`bit_writer.rs`. For show-specific synthetic hint fixtures, build bytes through
`BitWriter -> Buffer`, call `finish`, and take the buffer. Do not recreate a test-only bit-packing
algorithm in `show.rs`.

- [ ] **Step 5: Delete BitReader and run show gates**

Run:

```bash
cargo test -p flpdf linearization::show::tests --lib
cargo test -p flpdf --test show_linearization_tests
cargo test -p flpdf-cli --test cli_linearize
```

Expected: PASS with unchanged qpdf-compatible output.

- [ ] **Step 6: Prove legacy deletion**

Run:

```bash
rg -n "HintStreamBuilder|struct BitReader|impl<'a> BitReader|ZlibEncoder" \
  crates/flpdf/src/linearization/hint_stream.rs \
  crates/flpdf/src/linearization/show.rs \
  crates/flpdf/src/linearization/mod.rs
```

Expected: no matches.

Run:

```bash
rg -n "HintStreamBuilder|struct BitReader" crates/flpdf/src crates/flpdf/tests crates/flpdf-cli
```

Expected: no matches.

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf/src/bit_stream.rs crates/flpdf/src/bit_writer.rs crates/flpdf/src/linearization/show.rs
git commit -m "refactor: route hint decoding through bit stream"
```

---

### Task 8: Correspondence, full gates, and immediate-parent coverage

**Files:**
- Modify if generated output changes: `docs/qpdf-correspondence.md`
- Modify if required by discovered module list: `scripts/qpdf-module-docs.py`
- Verify: all files changed by Tasks 1-7

**Interfaces:**
- Produces no new API; proves the foundations PR is complete.

- [ ] **Step 1: Run the duplicate and direct-route audit**

Run:

```bash
rg -n "flate2::write::ZlibEncoder|HintStreamBuilder|struct BitReader" \
  crates/flpdf/src/linearization crates/flpdf/tests crates/flpdf-cli
rg -n "pub struct HintStreamBuilder|pub use hint_stream::.*HintStreamBuilder" crates/flpdf/src
```

Expected: no matches in hint/show scope. Other non-hint `ZlibEncoder` consumers are outside this
PR and may remain.

- [ ] **Step 2: Regenerate/check qpdf correspondence**

Run:

```bash
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts.tests.test_qpdf_module_docs
```

If `--check` reports generated drift, run the script's documented generation mode, inspect the
diff, and commit only truthful mappings for `Pipeline`, `Pl_Buffer`, `Pl_Count`, `Pl_Flate`,
`BitStream`, and `BitWriter`.

- [ ] **Step 3: Run formatting, lint, rustdoc, and workspace tests**

Run:

```bash
cargo fmt -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings --document-private-items" cargo doc --workspace --all-features --no-deps
cargo test --workspace --all-features
git diff --check
```

Expected: all commands exit 0.

- [ ] **Step 4: Run affected byte/oracle suites**

Run:

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test zlib_compat_tests
cargo test -p flpdf --test show_linearization_tests
cargo test -p flpdf --test linearize_objstm_generate_tests
cargo test -p flpdf-cli --test cli_linearize
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_linearize_qpdf
```

Expected: all pass; qpdf-dependent tests skip only under their existing explicit skip rule when
the oracle binary is unavailable.

- [ ] **Step 5: Measure fresh patch coverage**

Use the branch's immediate parent commit:

```bash
base_ref="origin/main"
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --all-features --lcov --output-path /tmp/flpdf-pipeline-foundation.lcov
scripts/patch-coverage.sh "$base_ref" HEAD /tmp/flpdf-pipeline-foundation.lcov
```

Expected: 100% of changed executable lines. Add focused tests for every reported miss; do not add
coverage exclusions merely to satisfy the number.

- [ ] **Step 6: Commit verification-only correspondence changes**

If Tasks 2-5 changed tracked correspondence output:

```bash
git add docs/qpdf-correspondence.md
git commit -m "docs: record pipeline component correspondence"
```

If there is no tracked diff, do not create an empty commit.

- [ ] **Step 7: Final clean-state evidence**

Run:

```bash
git status --short --branch
git log --oneline --decorate -8
```

Expected: clean worktree, only intentional commits, and no legacy helper matches.
