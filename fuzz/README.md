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
  `check_reader` (repair-enabled open + validate), then `Pdf::open_mem` →
  `PdfWriter` (one fresh full rewrite).
  libFuzzer lends its input only for the duration of the closure, and `Pdf<R>`
  requires `R: 'static`, so the target copies the input into one `Arc<[u8]>`
  and shares it across both opens — one copy per iteration, not two.
- **`xref`** — xref/trailer safety harness that sends each input through both
  `load_xref_and_trailer` (strict) and
  `load_xref_and_trailer_with_repair(..., true)` (qpdf-style recovery), using a
  fresh cursor for each call. Parse errors are expected; a panic, abort,
  sanitizer failure, or timeout is the defect under test.

The `xref` target follows qpdf 11.9.0's fuzzing boundary rather than
reimplementing qpdf output checks: qpdf lists its whole-document and focused
fuzzers in `fuzz/CMakeLists.txt:4-14`, and defines the arbitrary-input safety
contract in `fuzz/qpdf_fuzzer.cc:184-209`. The flpdf entry points and their
strict/repair responsibility are documented in
`crates/flpdf/src/xref.rs:657-708`. There is therefore no byte-level
differential assertion in this harness; qpdf's `qpdf_fuzzer` is the safety
oracle, while qpdf `--check` and the flpdf loaders are probed independently.
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

The dedicated committed xref/recovery seed corpus remains the responsibility
of `flpdf-9hc.19.7`; this target records the safety run without adding that
seed-ownership scope here.

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

# Fuzz strict and repair xref/trailer loading. Until the dedicated xref seed
# corpus is added, reuse the committed PDF seeds already used by roundtrip.
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu xref \
  fuzz/corpus/xref fuzz/seeds/roundtrip \
  -- -timeout=10 -rss_limit_mb=2048

# Reproduce a crash artifact.
cargo +nightly fuzz run --target x86_64-unknown-linux-gnu roundtrip \
  fuzz/artifacts/roundtrip/crash-<hash>

# Reproduce an xref crash artifact.
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu xref \
  fuzz/artifacts/xref/crash-<hash>
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

CI runs a short (60s) fuzz session on every PR with `-timeout=10`, so a panic,
abort, OOM, or hang fails the build. See the `fuzz` job in
`.github/workflows/ci.yml`.
