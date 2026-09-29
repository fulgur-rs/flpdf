# Fuzzing flpdf

This guide covers the contributor workflow for running flpdf's cargo-fuzz
targets, triaging findings, and adding a target. The target-specific harnesses,
seed formats, and recorded runs are documented in [`fuzz/README.md`](../fuzz/README.md).

## Prerequisites

- Install Rust with `rustup` and the `cargo` command.
- Install [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz):
  `cargo install cargo-fuzz`.
- Use a nightly toolchain. CI pins `nightly-2026-05-24` and installs
  `rust-src`; install the same toolchain locally with
  `rustup toolchain install nightly-2026-05-24 --component rust-src`.
- Run from the repository root. The standalone fuzz crate is under `fuzz/`;
  stable workspace commands do not build it.

Fuzz commands below explicitly select `x86_64-unknown-linux-gnu`. Keep this
target: cargo-fuzz otherwise defaults to the target triple of the cargo-fuzz
binary, which can be musl when installed with `cargo binstall`; that target
does not provide the sanitizer runtime used by this harness.

## Run a target locally

Seed the local, gitignored corpora from fixtures and committed seeds:

```bash
tools/seed-corpus.sh
```

The script is safe to rerun: it adds hash-named fixture inputs and preserves
libFuzzer discoveries. Run one target with the same 60-second budget and
per-input limits used by CI:

```bash
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu roundtrip \
  fuzz/corpus/roundtrip \
  -- -max_total_time=60 -timeout=10 -rss_limit_mb=2048
```

Replace `roundtrip` and its corpus path with another target and
`fuzz/corpus/<target>`. The `objstm` CI command also sets
`-max_len=4096`. To run until interrupted, omit `-max_total_time`; keep
`-timeout=10` and `-rss_limit_mb=2048` so one input cannot hang or consume
unbounded memory. Target-specific commands and longer acceptance budgets are
in [`fuzz/README.md`](../fuzz/README.md).

The CI job runs each target for 60 seconds and has a 15-minute job timeout.
It runs on Linux with AddressSanitizer. A crash, abort, sanitizer finding, OOM,
or input exceeding the timeout makes the command fail and writes a reproducer
under `fuzz/artifacts/<target>/`.

## Reproduce and inspect a finding

Use the target and artifact path printed by libFuzzer. Replaying a single
artifact runs it directly instead of starting a corpus session:

```bash
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu roundtrip \
  fuzz/artifacts/roundtrip/crash-<hash>
```

Artifacts may be named `crash-*`, `timeout-*`, or `oom-*`. The output includes
the failure type, stack trace, and artifact path. Minimize a reproducer with the
same target before investigating:

```bash
cargo +nightly-2026-05-24 fuzz tmin roundtrip \
  fuzz/artifacts/roundtrip/crash-<hash>
```

Use the minimized artifact to identify the failing production path, then keep
the smallest input that reliably reproduces the problem. The writable corpora
and artifacts are gitignored; do not commit an entire local corpus.

## File a fuzz finding

Create or update a Beads bug issue with enough detail for another contributor
to reproduce it:

- target name and flpdf commit;
- the exact command and toolchain used;
- whether the failure was a crash, timeout, sanitizer error, or OOM;
- the artifact path and minimized reproducer, or a committed regression
  fixture path.

If the finding is a product defect, add the minimized bytes under
`tests/fixtures/fuzz_regressions/` and add a stable regression test before or
with the fix. `cargo test -p flpdf --test fuzz_regression_tests` replays those
fixtures without nightly or libFuzzer. Keep only the regression fixture or a
deliberately selected seed in Git; leave generated corpus entries and crash
artifacts in their ignored directories. Remove sensitive document data before
adding an input to the repository or an issue.

## Add a fuzz target

Use this checklist so a new target is built, seeded, exercised in CI, and
documented consistently with the existing harnesses:

1. Add `fuzz/fuzz_targets/<target>.rs` and a matching `[[bin]]` entry in
   `fuzz/Cargo.toml`. Drive an existing public or canonical API; parse errors
   should be ordinary input outcomes, not harness panics. Bound work and output
   per input where a valid input could cause large traversal or expansion.
2. Add small, useful source seeds under `fuzz/seeds/<target>/`, or document
   which existing fixture set seeds the target. Register the target in the
   `targets` array and add its seed-copy rule in `tools/seed-corpus.sh`. Keep
   source seeds deterministic and the seeding script idempotent; never check
   in `fuzz/corpus/<target>`.
3. Add a step for the target to the `fuzz` job in
   `.github/workflows/ci.yml`. Use the pinned nightly, explicit
   `x86_64-unknown-linux-gnu` target, `-max_total_time=60`, `-timeout=10`, and
   `-rss_limit_mb=2048`; add a justified `-max_len` or other bound when needed.
   Keep the job's 15-minute timeout sufficient for the build and every target.
4. Document the harness contract, input format, and seed sources in
   `fuzz/README.md`. Update this guide only if the contributor workflow or
   shared limits change.
5. Run `tools/seed-corpus.sh`, then run the new target against its generated
   corpus with the same command and limits as CI. Confirm a second seeding run
   is safe and leaves existing discoveries intact. Add stable regression tests
   for any minimized bug found during development.

The parent workspace intentionally excludes `fuzz/`; a successful
`cargo test --workspace` does not build or run this target. The explicit
`cargo fuzz run` above is required locally, and the `Fuzz (short)` Actions job
is the CI gate for all registered targets.
