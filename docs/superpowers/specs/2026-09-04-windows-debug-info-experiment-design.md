# Windows debug-info build experiment

**Issue:** `flpdf-w2mi`

## Goal

Measure whether Cargo dev-profile debug-info generation is the dominant cost of
the Windows test-binary build, without changing the required CI workflow or
its shared cache.

## Current baseline

The required Windows CI job splits compilation from test execution:
`cargo build --workspace --all-targets` compiles the test binaries, followed by
`cargo test --workspace`. The first recorded run measured approximately 272
seconds for the build and 51 seconds for the test phase. The workspace has no
explicit profile section, so the default dev profile emits full MSVC debug
information and PDB data.

Cargo's test profile inherits the dev profile. A separate `[profile.test]`
experiment recompiles the dependency graph and would invalidate the existing
build/test split, so this experiment changes only `CARGO_PROFILE_DEV_DEBUG`.

## Approaches considered

1. Add a manual `workflow_dispatch` experiment workflow with three Windows
   matrix cells. This is the recommended approach: it uses the same hosted
   runner and Cargo command as required CI, but never runs on every PR and
   cannot add a required check.
2. Add three variants to the required `ci.yml` matrix. This would provide
   measurements automatically but would multiply required CI cost and alter
   the established four-platform contract.
3. Measure locally or in a shell-only benchmark. This is cheaper but cannot
   answer a Windows MSVC/PDB question and does not reproduce the hosted runner
   cache boundary.

## Workflow design

Create `.github/workflows/build-experiment.yml` with `workflow_dispatch` and a
`push` trigger restricted to `feature/flpdf-w2mi-build-experiment`, so the
published experiment runs once for this branch without becoming a required
check on other branches. Use one `windows-latest` job matrix with these values:

| Variant | `CARGO_PROFILE_DEV_DEBUG` |
|---|---|
| `full` | `true` |
| `line-tables-only` | `line-tables-only` |
| `none` | `0` |

Each job checks out the same revision, installs Rust 1.97.1, and uses the
pinned `Swatinem/rust-cache` action with a variant-specific shared key and
`save-if: false`. The variant-specific key prevents an existing target cache
from mixing profile outputs, while `save-if: false` guarantees that an
experimental target directory is never published into a shared cache.

The measured sequence is:

1. Run `cargo build --workspace --all-targets` once as a dependency and
   workspace warm-up.
2. Run `cargo clean` with `--package` repeated for the four workspace packages:
   `flpdf`, `flpdf-cli`, `flpdf-libjpeg-compat`, and `flpdf-qtest-tools`.
3. Measure a second `cargo build --workspace --all-targets` with Bash's
   `SECONDS` counter. This is the comparable clean-workspace rebuild cost.
4. Run `cargo test --workspace --no-run` without timing it. Its log must contain
   no `Compiling` line for the four cleaned packages, proving the measured
   all-target build produced the test artifacts used by the existing split.
5. Write the variant, profile value, warm-up seconds, rebuild seconds, and
   test-no-run reuse result to `$GITHUB_STEP_SUMMARY`.

`set -euo pipefail` applies to every shell block. The workflow does not call
the full test suite, change `ci.yml`, alter Cargo profiles, modify source code,
upload artifacts, or save a cache.

## Acceptance and interpretation

The workflow is successful only if all three matrix cells complete both build
commands and the no-run reuse check. The timing table is the measurement
artifact; no threshold or optimization is declared by this issue. A later
change to `[profile.dev]` or the required CI workflow needs a separate design
and must use the measured results rather than assuming the fastest variant is
safe for debugging or CI diagnostics.

## Non-goals

- Do not change `.github/workflows/ci.yml`.
- Do not add `[profile.dev]` or `[profile.test]` to `Cargo.toml`.
- Do not change the required four-OS matrix, cache keys, test commands, or
  branch protection.
- Do not commit generated timing output or experimental runner artifacts.
