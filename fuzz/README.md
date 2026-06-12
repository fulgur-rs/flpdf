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
  `write_pdf` (incremental) and `write_pdf_with_options { full_rewrite }`.

## Run locally

```bash
# One-time: install the runner.
cargo install cargo-fuzz

# Fuzz the whole-document target (Ctrl-C to stop). `-timeout` flags a
# non-terminating input as a hang; without it libFuzzer's default is 1200s.
cargo +nightly fuzz run roundtrip fuzz/corpus/roundtrip fuzz/seeds/roundtrip \
  -- -timeout=10 -rss_limit_mb=2048

# Reproduce a crash artifact.
cargo +nightly fuzz run roundtrip fuzz/artifacts/roundtrip/crash-<hash>
```

The first positional dir (`fuzz/corpus/roundtrip`, gitignored) is the writable
corpus; `fuzz/seeds/roundtrip` (committed) is read-only seed input.

## When the fuzzer finds a crash

1. Minimize it: `cargo +nightly fuzz tmin roundtrip fuzz/artifacts/roundtrip/crash-<hash>`.
2. Copy the minimized bytes into `tests/fixtures/fuzz_regressions/` with a
   descriptive name (e.g. `deep-nested-array.pdf`).
3. `crates/flpdf/tests/fuzz_regression_tests.rs` replays the whole directory
   through the same pipeline on **stable** (`cargo test -p flpdf`), so the fix
   is gated without a nightly/libFuzzer dependency.
4. Fix the defect; confirm `cargo test -p flpdf --test fuzz_regression_tests`
   passes.

## CI

CI runs a short (60s) fuzz session on every PR with `-timeout=10`, so a panic,
abort, OOM, or hang fails the build. See the `fuzz` job in
`.github/workflows/ci.yml`.
