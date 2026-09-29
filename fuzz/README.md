# flpdf fuzzing

A [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) harness for
flpdf. The core guarantee under test: **arbitrary byte input never panics,
aborts, or hangs**, and document traversal always terminates.

This is a standalone crate (its own `[workspace]` table) and lives at the repo
root so it is never bundled into the published `flpdf` crate. It requires a
**nightly** toolchain; stable `cargo build/test/clippy --workspace` never touches
it.

## Targets

- **`roundtrip`** — whole-document harness mirroring qpdf's `qpdf_fuzzer`:
  `QPDFJob::check` (repair-enabled open + validate), then `Pdf::open_mem` →
  `PdfWriter` (one fresh full rewrite).
  libFuzzer lends its input only for the duration of the closure, and `Pdf<R>`
  requires `R: 'static`, so the target copies the input into one `Arc<[u8]>`
  and shares it across both opens — one copy per iteration, not two.
- **`xref`** — xref/trailer safety harness that sends each input through the
  canonical reader twice: `Pdf::open_mem_with_options` with `repair: false`
  (strict) and with `repair: true` (qpdf-style recovery). Both passes set
  `suppress_warnings: true` so recovery diagnostics stay off the default
  logger's stderr. The input is shared as one `Arc<[u8]>` because `Pdf::open`
  requires `R: 'static`. Parse errors are expected; a panic, abort, sanitizer
  failure, or timeout is the defect under test. Its focused `/Prev` seeds
  include conflicting entries across generations, a 128-revision acyclic chain,
  and a 128-revision cycle.
- **`filter_pipeline`** — constructs document-owned streams and drives
  `ObjectHandle::pipe_stream_data` with one to seventeen filter stages, raw
  payload bytes, and aligned `/DecodeParms`. It covers qpdf-registered filters,
  qpdf-unfilterable CCITTFax/JPX/JBIG2 labels, predictor geometry, and malformed
  input without exposing an internal fuzz-only API. The harness allows one
  RunLength stage per chain to limit compounded expansion. Its sink stops one
  iteration after 1 MiB of decoded output or 4,096 downstream writes; product
  stream behavior and qpdf's unbounded-chain support are unchanged.
- **`primitive_parser`** — calls the public `ObjectHandle::parse` API on
  arbitrary bytes, then extracts up to 64 indirect-object bodies from each
  small PDF input and parses those independently. Its seeds are copied from
  `tests/fixtures/test_driver/`, so valid names, strings, arrays, dictionaries,
  numbers, and malformed objects reach the standalone parser after the PDF
  header.

The `filter_pipeline` seed format is: first byte selects one to seventeen
requested stages; the next bytes select filter IDs in this order: Flate,
ASCIIHex, ASCII85, RunLength, LZW, CCITTFax, DCT, JPX, JBIG2. The fuzz harness
omits repeated RunLength stages after the first. One parameter selector follows
per requested stage, then the remaining bytes become raw stream data. Selector
`0xff` exercises PNG row-width wrap-to-error; selector `0xfe` on LZW exercises
the TIFF predictor's overflow preflight.

The `xref` target follows qpdf 11.9.0's fuzzing boundary rather than
reimplementing qpdf output checks: qpdf lists its whole-document and focused
fuzzers in `fuzz/CMakeLists.txt:4-14`, and defines the arbitrary-input safety
contract in `fuzz/qpdf_fuzzer.cc:184-209`. The strict/repair split is the
canonical reader's `PdfOpenOptions::repair` flag; the standalone
`load_xref_and_trailer*` loaders this target used to call were removed with the
second document owner they constructed, matching qpdf, where one `QPDF` owns
the xref table and object cache (`include/qpdf/QPDF.hh:1465,1467`). There is
therefore no byte-level differential assertion in this harness; qpdf's
`qpdf_fuzzer` is the safety oracle, while qpdf `--check` and the flpdf reader
are probed independently.
The repair boundary follows qpdf's `QPDF::reconstruct_xref` recovery and
terminal missing-trailer path (`libqpdf/QPDF.cc:516-623`).
For xref streams, qpdf 11.9.0 rejects each `/W` value greater than
`sizeof(qpdf_offset_t)` before summing the entry width
(`libqpdf/QPDF.cc:986-1003`); `qpdf_offset_t` is `long long`
(`include/qpdf/Types.h:31`). flpdf applies the same fixed-width guard before
decoding stream bytes, with a qpdf-source-derived regression test for `/W [9 0 0]`.

### Recorded differential smoke probe

The same valid and empty inputs were checked through the qpdf CLI and flpdf's
CLI wrapper on 2026-08-15. Both accept the valid fixture (exit 0); both reject
the empty input (exit 2). qpdf reports its damaged-file reconstruction attempt
before the missing-trailer error, while flpdf reports the missing header
directly. The diagnostic text is intentionally not compared byte-for-byte:
this target checks the shared safety property, not qpdf's CLI diagnostic
surface.

```text
/usr/bin/qpdf --check tests/fixtures/minimal.pdf       -> exit 0
cargo run --quiet --bin flpdf -- --check tests/fixtures/minimal.pdf -> exit 0
/usr/bin/qpdf --check /dev/null                        -> exit 2
cargo run --quiet --bin flpdf -- --check /dev/null     -> exit 2
```

### Recorded xref fuzz run

On 2026-08-15, the xref target was rebuilt with the pinned nightly and run for
the full 300-second budget with AddressSanitizer enabled. The run started with
10,010 files in the local gitignored xref corpus and 2 committed roundtrip
seeds, then exited 0 without a crash, timeout, sanitizer failure, or artifact.
Every iteration called both the strict and repair entry points shown above.

```text
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu xref \
  fuzz/corpus/xref fuzz/seeds/roundtrip \
  -- -max_total_time=300 -timeout=10 -rss_limit_mb=2048 -verbosity=0
-> exit 0; 300-second budget completed
```

The focused `/Prev` seeds are owned by `flpdf-9hc.19.6`. The broader
xref/recovery seed corpus remains the responsibility of `flpdf-9hc.19.7`.

### Recorded `/Prev` fuzz run

On 2026-09-29, the pinned-nightly xref target ran for the full 300-second
budget with the three committed `/Prev` seeds and an empty local corpus. It
exited 0 without a crash, abort, sanitizer failure, timeout, or artifact.

```text
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu xref \
  fuzz/corpus/xref fuzz/seeds/prev_chain \
  -- -max_total_time=300 -timeout=10 -rss_limit_mb=2048 -verbosity=0
-> exit 0; 300-second budget completed
```

### Recorded `/Prev` chain probe

On 2026-09-29, qpdf 11.9.0 and flpdf both accepted the 128-revision acyclic
chain and returned the same loop warning for the 128-revision cycle. qpdf's
`QPDF::read_xref` walks `/Prev` iteratively and rejects a repeated xref offset;
it does not impose a maximum length on acyclic chains. The flpdf strict-reader
regression test checks that the cycle returns the corresponding structured
parse error.

```text
qpdf --check fuzz/seeds/prev_chain/deep-128-generations.pdf -> exit 0
flpdf --check fuzz/seeds/prev_chain/deep-128-generations.pdf -> exit 0
qpdf --check fuzz/seeds/prev_chain/deep-128-cycle.pdf -> exit 3, loop detected
flpdf --check fuzz/seeds/prev_chain/deep-128-cycle.pdf -> exit 3, loop detected
```

## Run locally

```bash
# One-time: install the runner.
cargo install cargo-fuzz

# Fuzz the whole-document target (Ctrl-C to stop). `-timeout` flags a
# non-terminating input as a hang; without it libFuzzer's default is 1200s.
#
# `--target x86_64-unknown-linux-gnu` is pinned because cargo-fuzz defaults its
# build target to the triple it was itself built for; a musl-built cargo-fuzz
# (e.g. from `cargo binstall`) would otherwise build for musl, whose static
# libc is incompatible with -Zsanitizer=address.
cargo +nightly fuzz run --target x86_64-unknown-linux-gnu roundtrip \
  fuzz/corpus/roundtrip fuzz/seeds/roundtrip \
  -- -timeout=10 -rss_limit_mb=2048

# Fuzz strict and repair xref/trailer loading with focused /Prev and general
# document seeds.
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu xref \
  fuzz/corpus/xref fuzz/seeds/prev_chain fuzz/seeds/roundtrip \
  -- -timeout=10 -rss_limit_mb=2048

# Fuzz the public stream-decode route. Each seed selects a filter or chain;
# remaining bytes become raw stream payload and DecodeParms selectors. The
# fuzz-only sink bounds each stream to 1 MiB of decoded output or 4,096 writes.
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu filter_pipeline \
  fuzz/corpus/filter_pipeline fuzz/seeds/filter_pipeline \
  -- -timeout=10 -rss_limit_mb=2048

# Fuzz primitive objects and object bodies extracted from test-driver fixtures.
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu primitive_parser \
  fuzz/corpus/primitive_parser fuzz/seeds/primitive_parser \
  -- -timeout=10 -rss_limit_mb=2048

# Exercise the focused /Prev corpus for the five-minute acceptance budget.
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu xref \
  fuzz/corpus/xref fuzz/seeds/prev_chain \
  -- -max_total_time=300 -timeout=10 -rss_limit_mb=2048 -verbosity=0

# Exercise the primitive parser seed corpus for the five-minute acceptance budget.
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu primitive_parser \
  fuzz/corpus/primitive_parser fuzz/seeds/primitive_parser \
  -- -max_total_time=300 -timeout=10 -rss_limit_mb=2048 -verbosity=0

# Reproduce a crash artifact.
cargo +nightly fuzz run --target x86_64-unknown-linux-gnu roundtrip \
  fuzz/artifacts/roundtrip/crash-<hash>

# Reproduce an xref crash artifact.
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu xref \
  fuzz/artifacts/xref/crash-<hash>

# Reproduce a filter-pipeline crash artifact.
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu filter_pipeline \
  fuzz/artifacts/filter_pipeline/crash-<hash>
```

The first positional dir for each target (for example,
`fuzz/corpus/roundtrip` or `fuzz/corpus/xref`, both gitignored) is the writable
corpus; the following `fuzz/seeds/...` dir is committed, read-only seed input.

## When the fuzzer finds a crash

1. Minimize it with the target that found it, for example:
   `cargo +nightly-2026-05-24 fuzz tmin roundtrip fuzz/artifacts/roundtrip/crash-<hash>`
   or
   `cargo +nightly-2026-05-24 fuzz tmin xref fuzz/artifacts/xref/crash-<hash>`.
2. Copy the minimized bytes into `tests/fixtures/fuzz_regressions/` with a
   descriptive name (e.g. `deep-nested-array.pdf`).
3. `crates/flpdf/tests/fuzz_regression_tests.rs` replays the whole directory
   through both fuzz pipelines on **stable** (`cargo test -p flpdf`), so the
   fix is gated without a nightly/libFuzzer dependency.
4. Fix the defect; confirm `cargo test -p flpdf --test fuzz_regression_tests`
   passes.

## CI

CI runs short (60s) `roundtrip`, `xref`, `filter_pipeline`, and `primitive_parser`
fuzz sessions on every PR with `-timeout=10`, so a panic, abort, OOM, or hang
fails the build. The xref session uses the focused `/Prev` seeds plus general
document seeds. See the `fuzz` job in `.github/workflows/ci.yml`.
