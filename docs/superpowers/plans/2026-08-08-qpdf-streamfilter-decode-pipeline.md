# QPDFStreamFilter::getDecodePipeline Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Give `StreamFilter` a streaming stage-construction surface equivalent to qpdf's
`QPDFStreamFilter::getDecodePipeline`, so a caller can drive decoding incrementally
through a downstream `Pipeline` instead of handing over a complete payload.

**Architecture:** A new `decode_pipeline` trait method returns the constructed codec
chain as `Box<dyn Pipeline + 'a>` borrowing the caller's sink. Predictor-bearing
Flate/LZW needs its `next` to be either a borrow or an owned predictor stage, so
`Flate::new` and `LzwDecoder::new` accept `impl Into<PipelineRef<'a>>`. The existing
whole-buffer `pipe_decode_recovering` route stays until `flpdf-3yn9.6` retires it.
`SF_Crypt` joins the registry, which requires Crypt stages to retain their whole
`/DecodeParms` key set so the filter can reject unknown keys the way qpdf does.

**Tech Stack:** Rust 2021, workspace crate `flpdf`. qpdf 11.9.0 pinned at
`/home/ubuntu/.cache/flpdf/qpdf-11.9.0` (resolve with
`scripts/fetch-qpdf-source.sh --print-path`).

**Beads issue:** `flpdf-qynx.5.5` — its `design` field holds the full design rationale
and the rejected alternatives. Read it before starting: `bd show flpdf-qynx.5.5`.

**Worktree:** `/home/ubuntu/flpdf/.worktrees/flpdf-qynx.5.5-decode-pipeline`
on branch `feature/flpdf-qynx.5.5-decode-pipeline`. All paths below are relative to it.

---

## Background you need

`QPDFStreamFilter` (`include/qpdf/QPDFStreamFilter.hh:35-61`) is a *stage factory*,
not a decoder. `getDecodePipeline(Pipeline* next)` builds a decode stage that writes
into `next` and returns it; it decodes nothing itself. `QPDF_Stream::pipeStreamData`
(`libqpdf/QPDF_Stream.cc:556-568`) walks the filter list in reverse, threading each
returned stage into the next call, then writes the source bytes into the outermost
stage.

flpdf today has `StreamFilter::pipe_decode_recovering`
(`crates/flpdf/src/stream_filter.rs:903-908`), which takes a complete `&[u8]` and
returns a complete `Vec<u8>`. The individual codecs underneath
(`crates/flpdf/src/pipeline/{flate,lzw,ascii85,ascii_hex,run_length,png_filter}.rs`)
are already incremental `Pipeline` stages that wrap a `next`. This plan adds the
missing factory boundary on top of them.

**Two things that are deliberately NOT in scope** (acceptance criterion 9):

1. Rewriting `pipe_decode_recovering`/`pipe_codec` onto the new method. That is a
   consumer cutover owned by `flpdf-3yn9.6`. It is also what would force a Rust
   equivalent of qpdf's caller-side `dynamic_cast<Pl_Flate*>` warn-callback wiring
   (`QPDF_Stream.cc:562-566`), which does not belong in `getDecodePipeline`.
2. `Pl_DCT` and `Pl_TIFFPredictor`. `decode_predictor_geometry`
   (`stream_filter.rs:1245-1262`) already returns `Err(Unsupported)` for
   `/Predictor 2` as a declared deviation. Leave it exactly as it is.

---

## Task 1: `PipelineRef`

**Files:**
- Modify: `crates/flpdf/src/pipeline.rs` (add type near the `Pipeline` trait at `:128-132`)

**Step 1: Write the failing tests**

Add to `pipeline.rs`'s `mod tests`:

```rust
#[test]
fn pipeline_ref_borrowed_delegates_write_and_finish() {
    let mut sink = RecordingSink::default();
    {
        let mut r = PipelineRef::from(&mut sink);
        r.write(b"ab").unwrap();
        r.finish().unwrap();
    }
    assert_eq!(sink.data, b"ab");
    assert_eq!(sink.finishes, 1);
}

#[test]
fn pipeline_ref_owned_delegates_write_and_finish() {
    let mut sink = RecordingSink::default();
    {
        let boxed: Box<dyn Pipeline + '_> = Box::new(Count::new("count", &mut sink));
        let mut r = PipelineRef::from(boxed);
        r.write(b"ab").unwrap();
        r.finish().unwrap();
    }
    assert_eq!(sink.data, b"ab");
    assert_eq!(sink.finishes, 1);
}

#[test]
fn pipeline_ref_accepts_an_unsized_borrow() {
    // The blanket `From<&mut P>` impl carries an implicit `P: Sized`; this is
    // the shape `stream_filter::pipe_codec` already holds.
    let mut sink = RecordingSink::default();
    let dyn_next: &mut dyn Pipeline = &mut sink;
    let mut r = PipelineRef::from(dyn_next);
    r.write(b"z").unwrap();
    r.finish().unwrap();
}

#[test]
fn pipeline_ref_reports_the_inner_identifier() {
    let mut sink = RecordingSink::default();
    let r = PipelineRef::from(&mut sink);
    assert_eq!(r.identifier(), "recording");
}
```

`RecordingSink` here needs `data: Vec<u8>`, `finishes: usize`, and
`identifier() == "recording"`. `pipeline.rs`'s `mod tests` already has a `FaultSink`
at `:139`; add `RecordingSink` beside it following the same local-struct convention
used in `pipeline/flate.rs:493` and `pipeline/rc4.rs:153`.

Use whatever concrete stage is convenient for the owned case; `pipeline::count::Count`
is a pass-through and imports cleanly.

**Step 2: Run tests to verify they fail**

Run: `cargo test -p flpdf --lib pipeline::tests::pipeline_ref -- --nocapture`
Expected: FAIL, `cannot find type PipelineRef in this scope`.

**Step 3: Implement**

In `crates/flpdf/src/pipeline.rs`, after the `Pipeline` trait:

```rust
/// A `next` slot that is either borrowed from the caller or owned by the stage.
///
/// qpdf threads a bare `Pipeline*` through
/// `QPDFStreamFilter::getDecodePipeline` (`QPDFStreamFilter.hh:46-49`) and keeps
/// every stage it constructs alive in the filter instance
/// (`SF_FlateLzwDecode.cc:88-108`). Rust cannot hand back a stage that borrows
/// another stage the same object owns, so a multi-stage chain instead owns its
/// inner stage here and the whole chain is returned to the caller. Construction
/// order, stage count, and output bytes are unchanged; only the owner moves.
/// CLAUDE.md deviation class (B).
pub(crate) enum PipelineRef<'a> {
    Borrowed(&'a mut dyn Pipeline),
    Owned(Box<dyn Pipeline + 'a>),
}

impl<'a> Pipeline for PipelineRef<'a> {
    fn identifier(&self) -> &str {
        match self {
            Self::Borrowed(next) => next.identifier(),
            Self::Owned(next) => next.identifier(),
        }
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        match self {
            Self::Borrowed(next) => next.write(data),
            Self::Owned(next) => next.write(data),
        }
    }

    fn finish(&mut self) -> PipelineResult<()> {
        match self {
            Self::Borrowed(next) => next.finish(),
            Self::Owned(next) => next.finish(),
        }
    }
}

impl<'a, P: Pipeline> From<&'a mut P> for PipelineRef<'a> {
    fn from(next: &'a mut P) -> Self {
        Self::Borrowed(next)
    }
}

/// Required alongside the blanket impl above, which carries an implicit
/// `P: Sized`. `dyn Pipeline` is `!Sized`, so the two do not overlap.
impl<'a> From<&'a mut dyn Pipeline> for PipelineRef<'a> {
    fn from(next: &'a mut dyn Pipeline) -> Self {
        Self::Borrowed(next)
    }
}

impl<'a> From<Box<dyn Pipeline + 'a>> for PipelineRef<'a> {
    fn from(next: Box<dyn Pipeline + 'a>) -> Self {
        Self::Owned(next)
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p flpdf --lib pipeline::tests::pipeline_ref`
Expected: PASS, 4 tests.

**Step 5: Commit**

```bash
git add crates/flpdf/src/pipeline.rs
git commit -m "feat(pipeline): add PipelineRef for borrowed-or-owned next slots"
```

---

## Task 2: Generalize the Flate and LZW `next` parameter

**Files:**
- Modify: `crates/flpdf/src/pipeline/flate.rs:87-92`
- Modify: `crates/flpdf/src/pipeline/lzw.rs:44-48`

Both currently take `next: &'a mut dyn Pipeline` and store it in a
`next: &'a mut dyn Pipeline` field. Change the parameter to
`next: impl Into<PipelineRef<'a>>`, store `PipelineRef<'a>`, and call
`.into()` in the constructor. Every `self.next.write(..)`/`self.next.finish(..)`
keeps working because `PipelineRef` implements `Pipeline`.

Do **not** touch `png_filter.rs`, `ascii85.rs`, `ascii_hex.rs`, or `run_length.rs`.
They only ever wrap a caller-supplied `next`.

**Step 1: Make the change**

In each file, adjust the struct field type and the constructor signature. Import
`PipelineRef` from `crate::pipeline`.

**Step 2: Verify existing call sites still compile unchanged**

Run: `cargo check -p flpdf --all-targets`
Expected: clean. No call site edits should be needed — the blanket `From<&mut P>`
covers `&mut sink` and the `dyn` impl covers `pipe_codec`'s
`next: &mut dyn Pipeline` (`stream_filter.rs:1266`). If any site fails to infer,
fix that site rather than widening the impls further.

**Step 3: Verify no behavior changed**

Run: `cargo test -p flpdf --lib`
Expected: PASS, same count as the pre-change baseline (3630 passed at branch point).

**Step 4: Commit**

```bash
git add crates/flpdf/src/pipeline/flate.rs crates/flpdf/src/pipeline/lzw.rs
git commit -m "refactor(pipeline): let Flate and LZW take a borrowed or owned next"
```

---

## Task 3: The `decode_pipeline` trait method

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs` — trait at `:866-934`, impls at
  `:1200-1237` (Flate/LZW), `:1301-1342` (ASCII85 / ASCIIHex / RunLength),
  `:1344-1391` (three `#[cfg(test)]` filters)

**Step 1: Write the failing test**

In `stream_filter.rs`'s `mod tests`:

```rust
#[test]
fn ascii_hex_decode_pipeline_decodes_through_the_caller_sink() {
    let mut sink = OutputBuffer::new(None);
    {
        let mut filter = AsciiHexStreamFilter;
        let mut stage = filter.decode_pipeline(&mut sink).unwrap().unwrap();
        stage.write(b"616263>").unwrap();
        stage.finish().unwrap();
    }
    assert_eq!(sink.data, b"abc");
}
```

**Step 2: Run it to verify it fails**

Run: `cargo test -p flpdf --lib stream_filter::tests::ascii_hex_decode_pipeline`
Expected: FAIL, no method named `decode_pipeline`.

**Step 3: Add the trait method**

No default body — qpdf's `getDecodePipeline` is pure virtual
(`QPDFStreamFilter.hh:49`). Place it directly after `preflight_decode_pipeline`:

```rust
    /// Port of `QPDFStreamFilter::getDecodePipeline`
    /// (`include/qpdf/QPDFStreamFilter.hh:46-49`): build this filter's decode
    /// stage around `next` and return it without decoding anything.
    ///
    /// `Result` carries the construction failures qpdf raises from the stage
    /// constructors themselves; `None` is qpdf's `nullptr`, which only
    /// `SF_Crypt` returns (`QPDF_Stream.cc:52-56`).
    ///
    /// qpdf keeps each constructed stage in the filter instance and hands the
    /// caller a non-owning pointer. The stage is returned by value here
    /// instead — see [`crate::pipeline::PipelineRef`] for why, and
    /// `QPDF_Stream.cc:556-568` for the caller-side loop this feeds.
    ///
    /// The Flate warn callback is deliberately absent: qpdf installs it at the
    /// `pipeStreamData` caller (`QPDF_Stream.cc:562-566`), not here.
    fn decode_pipeline<'a>(
        &mut self,
        next: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>>;
```

**Step 4: Implement for every filter**

`FlateLzwStreamFilter` — mirrors `SF_FlateLzwDecode::getDecodePipeline`
(`SF_FlateLzwDecode.cc:88-108`): predictor first, then the codec wrapping it.

```rust
    fn decode_pipeline<'a>(
        &mut self,
        next: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>> {
        let next: PipelineRef<'a> = match self.decode_predictor_geometry()? {
            Some((columns, colors, bits_per_component)) => {
                let predictor = PngFilter::new(
                    "png decode",
                    next,
                    PngFilterAction::Decode,
                    columns,
                    colors,
                    bits_per_component,
                )
                .map_err(map_pipeline_error)?;
                PipelineRef::Owned(Box::new(predictor))
            }
            None => PipelineRef::Borrowed(next),
        };
        let stage: Box<dyn Pipeline + 'a> = if self.lzw {
            Box::new(LzwDecoder::new("lzw decode", next, self.early_code_change))
        } else {
            Box::new(
                Flate::new(
                    "stream inflate",
                    next,
                    FlateAction::Inflate,
                    DEFAULT_OUT_BUFFER_SIZE,
                )
                .map_err(map_pipeline_error)?,
            )
        };
        Ok(Some(stage))
    }
```

Keep the identifier strings byte-identical to the ones `pipe_codec` already uses
(`"png decode"`, `"lzw decode"`, `"stream inflate"`), which are qpdf's.

The three single-stage filters:

```rust
// Ascii85StreamFilter
Ok(Some(Box::new(Ascii85Decoder::new("ascii85 decode", next))))
// AsciiHexStreamFilter
Ok(Some(Box::new(AsciiHexDecoder::new("asciiHex decode", next))))
// RunLengthStreamFilter
Ok(Some(Box::new(RunLength::new("runlength decode", next, RunLengthAction::Decode))))
```

The three `#[cfg(test)]` filters (`TestStreamFilter`, `BorrowedInputProbe`,
`PostPreflightFailure`) each need a body now that the method has no default. Give
each one behavior consistent with what it already does in
`pipe_decode_recovering`, and say in the report what you chose and why.

As implemented: `TestStreamFilter` and `BorrowedInputProbe` return `Ok(None)`,
because qpdf's caller treats a null return as "this filter contributes no stage,
keep writing to `next`" (`QPDF_Stream.cc:561-563`) — which is exactly the
identity pass-through both already perform. `PostPreflightFailure` returns
`Err(Error::Internal(..))` with its existing message; the name stays accurate
because the filter does not override `preflight_decode_pipeline`, so preflight
still succeeds and the failure follows it on both routes.

**Step 5: Run the test**

Run: `cargo test -p flpdf --lib stream_filter::tests::ascii_hex_decode_pipeline`
Expected: PASS.

**Step 6: Full library run**

Run: `cargo test -p flpdf --lib`
Expected: PASS, no regressions.

**Step 7: Commit**

```bash
git add crates/flpdf/src/stream_filter.rs
git commit -m "feat(stream-filter): add getDecodePipeline-equivalent stage factory"
```

---

## Task 4: Failure-path and streaming tests

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs` `mod tests`

Prefer the shared `crate::pipeline::test_support::RecordingSink`
(`pipeline/test_support.rs:38`) over hand-rolling sinks. It injects write and
finish failures by attempt index — **1-based**, because `write_attempts`
increments before the `contains` check — and its `trace()` returns a `Trace`
carrying both `output: Vec<u8>` and `calls: Vec<TraceCall>`, so write/finish
ordering is assertable, not just counts. Task 1 switched to it for exactly this
reason. Reach for a local sink only where the shared one genuinely does not fit,
and say why.

### The hole this task must close

After Task 3, the *only* committed caller of `decode_pipeline` is the single
AsciiHex test. A reviewer replaced the whole body of
`FlateLzwStreamFilter::decode_pipeline` with `if true { return Ok(None); }` and
**all 3638 tests still passed**. That body is the only multi-stage path, the only
`PipelineRef::Owned` construction site, and the only body that can return `Err`.

So a Task 4 that adds tests for the three single-stage filters reads as complete
while leaving the real hole open. **The acceptance bar for this task is that the
same mutation fails.** Run it yourself before claiming done: mutate the body,
confirm a test fails, revert.

Concretely, `FlateLzwStreamFilter::decode_pipeline` needs, at minimum, a test
that a PNG-predictor + Flate chain and a PNG-predictor + LZW chain each carry
bytes through both stages to the sink correctly. Task 3's implementer reported
verifying this with throwaway tests that were then reverted — treat that as
**claimed but unverified**; it is precisely what this task must commit.

Write these tests, one commit per bullet group is fine:

**a. Construction failure precedes any write.**

```rust
#[test]
fn predictor_construction_failure_precedes_every_write() {
    let mut sink = RecordingSink::default();
    let mut filter = FlateLzwStreamFilter::new(false);
    // /Colors 0 is what PngFilter::new rejects as invalid samples_per_pixel.
    assert!(filter.set_decode_params(&params(&[("Predictor", 12), ("Colors", 0)])));
    // `.err().unwrap()`, not `.unwrap_err()`: the latter needs `T: Debug`, and
    // here `T` is `Option<Box<dyn Pipeline + 'a>>`, which has no `Debug`. Do not
    // "fix" that by giving `Pipeline` a `Debug` supertrait — qpdf's `Pipeline`
    // has no counterpart, and it would propagate to every stage.
    let err = filter.decode_pipeline(&mut sink).err().unwrap();
    assert!(err.to_string().contains("samples_per_pixel"));
    assert_eq!(sink.writes, 0);
    assert_eq!(sink.finishes, 0);
}
```

Use whatever `DecodeParams` construction helper the surrounding tests already use
(there is a `decode_params_from_object`-based helper near `:3377`); do not invent a
second one.

**b. Chunked writes with exactly one finish, per codec.**

One test per codec (Flate, LZW, ASCII85, ASCIIHex, RunLength) plus one for
predictor-bearing Flate. Each splits an encoded payload across at least three
`write` calls, finishes once, and asserts the exact expected bytes plus
`sink.finishes == 1`. These literal expectations are the absolute anchor that makes
the differential test in (f) meaningful — do not replace them with a comparison.

**c. Downstream write failure propagates.**

```rust
#[test]
fn downstream_write_failure_propagates_out_of_the_stage() {
    let mut sink = WriteFaultSink;
    let mut filter = AsciiHexStreamFilter;
    let mut stage = filter.decode_pipeline(&mut sink).unwrap().unwrap();
    let err = stage.write(b"616263>").unwrap_err();
    assert!(err.to_string().contains("write fault"));
}
```

**d. Downstream finish failure propagates** — same shape against `FinishFaultSink`,
asserting the error surfaces from `stage.finish()`.

**e. Base `set_decode_params` defaults and compression classification.**

```rust
#[test]
fn single_stage_filters_accept_absent_and_reject_present_decode_params() {
    for mut filter in [
        Box::new(Ascii85StreamFilter) as Box<dyn StreamFilter>,
        Box::new(AsciiHexStreamFilter),
        Box::new(RunLengthStreamFilter),
    ] {
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        assert!(!filter.set_decode_params(&DecodeParams::Present(Vec::new())));
    }
}

#[test]
fn only_run_length_reports_specialized_compression() {
    assert!(RunLengthStreamFilter.is_specialized_compression());
    assert!(!Ascii85StreamFilter.is_specialized_compression());
    assert!(!AsciiHexStreamFilter.is_specialized_compression());
    assert!(!FlateLzwStreamFilter::new(false).is_specialized_compression());
    assert!(!RunLengthStreamFilter.is_lossy_compression());
}
```

`RunLengthStreamFilter::is_specialized_compression` already returns `true`
(`stream_filter.rs:1341-1343`); this pins it against
`SF_RunLengthDecode.hh`'s override. Do not change the implementation.

**f. Stage lifetime and route agreement.**

```rust
#[test]
fn the_stage_may_outlive_construction_and_be_dropped_before_the_sink_is_read() {
    let mut sink = OutputBuffer::new(None);
    {
        let mut filter = FlateLzwStreamFilter::new(false);
        assert!(filter.set_decode_params(&params(&[("Predictor", 12), ("Columns", 4)])));
        let mut stage = filter.decode_pipeline(&mut sink).unwrap().unwrap();
        for chunk in encoded.chunks(3) {
            stage.write(chunk).unwrap();
        }
        stage.finish().unwrap();
    }
    assert_eq!(sink.data, expected);
}

#[test]
fn decode_pipeline_and_whole_buffer_route_agree() {
    // Flate, LZW, ASCII85, ASCIIHex, RunLength.
}
```

**Step: run and commit**

Run: `cargo test -p flpdf --lib stream_filter::tests`
Expected: PASS.

```bash
git add crates/flpdf/src/stream_filter.rs
git commit -m "test(stream-filter): cover decode_pipeline construction, streaming, and faults"
```

---

## Task 5: Let a Crypt stage keep its whole key set

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs:1028-1083` (retention constants and
  predicates), `:648-676` (`decode_params_from_object`), `:539-570`
  (`decode_params_from_consuming_handle` / `decode_params_from_entries`)
- Modify: `crates/flpdf/src/stream_filter.rs:1904` (bounded-bytes test)
- Modify: `docs/qpdf-correspondence.md` row 202

**Why:** `SF_Crypt::setDecodeParms` (`QPDF_Stream.cc:33-50`) rejects any key outside
`/Type` and `/Name`. `retains_decode_param_key` (`:1066-1068`) currently drops every
unretained key *before* `set_decode_params` runs, so `/DecodeParms << /Foo 1 >>`
would arrive as an empty entry set and be accepted. The retention rule — "keep what
the consumer reads" — is right; it just was not applied to a consumer that reads the
whole key set.

**Step 1: Write the failing test**

```rust
#[test]
fn a_crypt_stage_retains_every_key_so_unknown_ones_stay_visible() {
    let params = crypt_params(&[("Foo", Object::Integer(1)), ("Name", name(b"Identity"))]);
    assert_eq!(
        params.entries().iter().map(|(k, _)| k.as_slice()).collect::<Vec<_>>(),
        vec![b"Foo".as_slice(), b"Name".as_slice()]
    );
}

#[test]
fn a_crypt_stage_keeps_the_type_name_bytes() {
    let params = crypt_params(&[("Type", name(b"CryptFilterDecodeParms"))]);
    assert_eq!(
        params.entries(),
        [(b"Type".to_vec(), ParamValue::Name(b"CryptFilterDecodeParms".to_vec()))]
    );
}
```

**Step 2: Run to verify failure**

Run: `cargo test -p flpdf --lib stream_filter::tests::a_crypt_stage`
Expected: FAIL — `/Foo` is dropped, `/Type` is dropped.

**Step 3: Implement**

- `retains_decode_param_key` keeps every key when the stage is Crypt.
- The name-payload predicate admits `/Type` as well as `/Name` under Crypt. Rename
  `CRYPT_RETAINED_DECODE_PARAM_KEY` / `is_crypt_name_key` to reflect that they now
  cover two keys, and keep the "spelled once" property their docs describe.
- Apply the change in **both** shape readers. They are compared by
  `handle_reader_matches_object_reader_for_every_filter_shape`; a one-sided change
  fails it.

**Step 4: Re-scope the bounded-bytes test**

`retained_decode_parameter_bytes_do_not_grow_with_the_source_dictionary` (`:1904`)
asserts a property that no longer holds for Crypt. Restrict it to non-Crypt filters
and say why in the test doc comment. Do not delete it.

**Step 5: Correct the correspondence row**

`docs/qpdf-correspondence.md` row 202 currently states the owned snapshot affects
neither filterability nor which key carries name bytes. Both claims change: a Crypt
stage's retained key set now decides filterability, and `/Type` joins `/Name` as a
name-bytes key. Edit the row text; do not change its classification marker.

**Step 6: Run and commit**

Run: `cargo test -p flpdf --lib`

```bash
git add crates/flpdf/src/stream_filter.rs docs/qpdf-correspondence.md
git commit -m "fix(stream-filter): keep every /DecodeParms key on a Crypt stage"
```

---

## Task 6: `CryptStreamFilter`

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs` (new struct beside the other filters,
  plus the `stream_filter_for` match at `:1394-1409`)

**Step 0: Probe qpdf for the expected answers**

Before writing expectations, confirm each shape against the pinned build. Generate a
small PDF whose stream carries `/Filter /Crypt` with the `/DecodeParms` under test
and check whether qpdf treats it as filterable:

```bash
qpdf --show-object=<n> --filtered-stream-data <probe>.pdf; echo "exit=$?"
```

Shapes to probe: absent; `<< /Name /Identity >>`;
`<< /Type /CryptFilterDecodeParms /Name /Identity >>`; `<< /Type /Foo >>`;
`<< /Foo 1 >>`. Record the observed exit codes in the test doc comments — the tests
assert qpdf's answers, not ours.

**Step 1: Write the failing tests**

```rust
#[test]
fn crypt_accepts_only_type_and_name_keys() {
    let mut filter = CryptStreamFilter;
    assert!(filter.set_decode_params(&DecodeParams::Absent));
    assert!(filter.set_decode_params(&crypt_params(&[("Name", name(b"Identity"))])));
    assert!(filter.set_decode_params(&crypt_params(&[
        ("Type", name(b"CryptFilterDecodeParms")),
        ("Name", name(b"Identity")),
    ])));
    assert!(!filter.set_decode_params(&crypt_params(&[("Type", name(b"Foo"))])));
    assert!(!filter.set_decode_params(&crypt_params(&[("Foo", Object::Integer(1))])));
}

#[test]
fn crypt_builds_no_decode_stage() {
    let mut sink = RecordingSink::default();
    let mut filter = CryptStreamFilter;
    assert!(filter.decode_pipeline(&mut sink).unwrap().is_none());
    assert_eq!(sink.writes, 0);
}
```

**Step 2: Implement**

```rust
/// Port of the anonymous-namespace `SF_Crypt` in `libqpdf/QPDF_Stream.cc:27-58`.
struct CryptStreamFilter;

impl StreamFilter for CryptStreamFilter {
    fn set_decode_params(&mut self, decode_params: &DecodeParams) -> bool {
        // QPDF_Stream.cc:34-49 — every key must be /Type or /Name, and a
        // present /Type must satisfy isDictionaryOfType("/CryptFilterDecodeParms").
    }

    fn reads_decode_params(&self) -> bool {
        true
    }

    fn decode_pipeline<'a>(
        &mut self,
        _next: &'a mut dyn Pipeline,
    ) -> Result<Option<Box<dyn Pipeline + 'a>>> {
        // QPDF_Stream.cc:52-56 returns nullptr: piping is handled by decryptStream.
        Ok(None)
    }

    fn pipe_decode_recovering(..) -> .. {
        // Unreachable in production: filters::prepare_decode_filters peels a
        // Crypt spec off before the registry lookup. Return the same
        // "unsupported" error filters::reject_crypt_stage produces.
    }
}
```

Transcribe qpdf's loop structure directly, including that a present `/Type` is
checked on every iteration rather than once.

**Step 3: Register it**

Add `b"Crypt" => Some(Box::new(CryptStreamFilter)),` to `stream_filter_for`,
matching qpdf's `filter_factories` (`QPDF_Stream.cc:85-94`), which holds `/Crypt`.

**Verify this changes nothing existing**, and record the check in the commit message:
- `filter_reads_decode_params` (`:486-490`) returns `true` on `is_crypt_filter`
  before it consults the registry.
- `filters::prepare_decode_filters` (`filters.rs:690-699`) routes a Crypt spec to
  `PreparedStage::Crypt` with a `continue`, before the `stream_filter_for` lookup at
  `:702`.

Add a test asserting `stream_filter_for(b"Crypt").is_some()` so the registration
cannot silently disappear.

**Step 4: Run and commit**

Run: `cargo test -p flpdf --lib`

```bash
git add crates/flpdf/src/stream_filter.rs
git commit -m "feat(stream-filter): register SF_Crypt through the filter factory"
```

---

## Task 7: Documentation and the correspondence table

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs` module doc (`:1-40`)
- Modify: `crates/flpdf/src/pipeline.rs` module doc if needed
- Modify: `docs/qpdf-correspondence.md` rows 192, 196-198, 202

Record, in prose an outside reader can check:

1. The class (B) ownership substitution — stage returned by value rather than kept in
   the filter instance — with the qpdf lines it corresponds to. CLAUDE.md requires
   this in **both** the module doc and the correspondence table.
2. That `stream_filter_for` is a `match` where qpdf uses a `std::map`
   (`QPDF_Stream.cc:85-94`) — the same class (B) container substitution.
3. That the legacy whole-buffer route installs the Flate warn callback callee-side
   while qpdf installs it at the `pipeStreamData` caller
   (`QPDF_Stream.cc:562-566`), and that reconciling the placement belongs to the
   `pipeStreamData` port.
4. A note for that downstream port: qpdf's `dynamic_cast` sits *outside* the
   `if (decode_pipeline)` guard, so a null-returning filter such as Crypt leaves the
   previous iteration's stage in `pipeline` and can re-apply the callback to it.

Public doc rules apply (`.claude/rules/pdf-rust-doc-review-patterns.md`): no beads
IDs, no epic or follow-up jargon, English only on `///` and `//!`. Point 3 and 4
name the qpdf symbol and file, not an issue ID.

```bash
git add crates/flpdf/src/stream_filter.rs crates/flpdf/src/pipeline.rs docs/qpdf-correspondence.md
git commit -m "docs: record the decode-pipeline ownership and factory substitutions"
```

---

## Task 8: Gates

Run each, fix what fails, and do not skip a step because the previous one passed.

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --doc -p flpdf
```

Then patch coverage. Commit everything first — the gate refuses a dirty tree, and
`--allow-dirty` produces false greens:

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path /tmp/claude-1000/-home-ubuntu-flpdf/d4050246-d9d1-4d33-a7eb-62dc7be1bf30/scratchpad/lcov.info
scripts/patch-coverage.sh --base main \
  --lcov /tmp/claude-1000/-home-ubuntu-flpdf/d4050246-d9d1-4d33-a7eb-62dc7be1bf30/scratchpad/lcov.info
```

Every changed line in `flpdf` must be covered. If a line is genuinely unreachable,
mark it `// cov:ignore: <reason>` and note the reason for the PR description.

One line looks like a `cov:ignore` candidate and is not. The `Flate::new(..)
.map_err(map_pipeline_error)?` inside `FlateLzwStreamFilter::decode_pipeline` can
never take its `Err` branch: `Flate::new` fails only when the output buffer size is
zero or exceeds `u32::MAX`, and the argument is the constant
`DEFAULT_OUT_BUFFER_SIZE`. Keep the `?` anyway — qpdf's `Pl_Flate` constructor is
likewise fallible-but-never-failing at the default buffer size, and silently making
it infallible would be an unrecorded divergence. The line is executed on the `Ok`
path, so line coverage is satisfied without an ignore marker.

After the numbers, do the qualitative pass CLAUDE.md requires: confirm the error
arms, boundaries, and empty/extreme inputs of the new behavior have tests whose
assertions are substantive — not merely that the lines executed.

---

## Definition of done

Against `flpdf-qynx.5.5`'s acceptance criteria:

1. qpdf citations mapped in docs and test comments.
2. `decode_pipeline` accepts a downstream `Pipeline` and neither takes nor returns a
   complete payload.
3. `set_decode_params` still runs before construction (unchanged in
   `prepare_decode_filters`); predictor is built before the codec; specialized/lossy
   defaults pinned by test.
4. All five codecs route through their existing incremental stages — no second
   decoder implementation added.
5. `SF_Crypt` reachable through `stream_filter_for`, its allowed-key validation
   reproduced, and no decode stage returned.
6. Construction failure, chunked input, write and finish faults, exactly-once finish,
   and lifetime all covered without a panic or a whole-payload clone.
7. The ownership and container substitutions recorded in module docs **and**
   `docs/qpdf-correspondence.md`.
8. Oracle probe results recorded; workspace gates green; changed-line coverage 100%.
9. No filter-shape orchestration, decode levels, source dispatch, writer retry,
   DCT/TIFF, or consumer cutover in the diff.
