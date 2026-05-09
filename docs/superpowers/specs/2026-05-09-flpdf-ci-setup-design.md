# CI Setup Design

## Goal

Add automated CI for the Rust workspace so every change to the main branch and pull requests are validated by formatting, linting, and tests before review or merge.

## Scope

- Repository: Rust workspace in `Cargo.toml` with crates `flpdf` and `flpdf-cli`.
- Triggers: `push` to `main`, all `pull_request` events.
- Validation targets: workspace formatting, clippy lint, and full workspace tests.

## Non-Goals

- Deployment or release automation.
- Fuzzing and heavyweight compatibility matrix tasks.
- Running docs generation in CI.

## CI Design Options

1) **Recommended (adopted): Single workflow with two jobs**
   - `quality` job on `ubuntu-latest` runs `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
   - `test` job on Linux and Windows runs `cargo test --workspace --all-targets --all-features`.
   - Pros: fast signal, good platform coverage, explicit quality gate before tests.
   - Cons: does not cover macOS.

2) **Single-file workflow matrix only tests on Linux**
   - One matrix job for OSes, run fmt/clippy/test in each.
   - Pros: simpler; easier to reason about one job.
   - Cons: duplicated work and longer runs; lower signal-to-noise.

3) **Comprehensive workflow with macOS + nightly formatting checks**
   - Adds macOS runner and scheduled/cron runs.
   - Pros: broader environment coverage.
   - Cons: longer wall-clock time and higher CI usage.

## Implementation Plan

- Add `.github/workflows/ci.yml`.
- Use `dtolnay/rust-toolchain` (pinned to SHA in CI) with `rustfmt` and `clippy` components.
- Enable dependency caching via `Swatinem/rust-cache` (pinned to SHA in CI) for faster runs.
- Keep strict, deterministic checks for review-friendly failures.

## Error and Quality Handling

- Any step failure fails the workflow and blocks PR merge feedback.
- `clippy` runs with `-D warnings` to prevent warning regressions.
- `fmt` uses `--check` to avoid automatic formatting diffs in CI.

## Validation Commands in CI

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-targets --all-features`

## Review Criteria

- Workflow file exists under `.github/workflows/ci.yml`.
- Push/pull_request events execute quality and test jobs.
- CI runs on fresh Linux and Windows where supported dependencies allow.
- Local command equivalents pass with repository status clean.
