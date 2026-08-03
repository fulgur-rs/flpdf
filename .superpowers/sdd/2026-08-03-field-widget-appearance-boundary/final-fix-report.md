# Final Fix Report

## Oracle findings

- qpdf typed form-field access resolves the terminal object for inherited `/V`, `/DV`, `/Ff`, `/T`, `/TU`, and `/TM` values.
- qpdf JSON inspection keeps the selected raw `/V` and `/DV` handle, so an indirect value is emitted as an indirect-reference identity rather than its materialized value.
- qpdf's appearance pass routes every `/Btn` field through `setV(getValue())`, including widgets without an existing `/AP/N`; text and choice widgets still use explicit appearance generation.

## Changes

- Added terminal-chain-aware typed holder resolution while retaining raw handle identity for JSON and signature consumers.
- Preserved non-UTF-8 dictionary key bytes in `set_field_attribute`.
- Matched qpdf's button appearance routing and retained explicit `/Tx` and `/Ch` generation.
- Corrected the renderer test filter in the implementation plan.

## TDD evidence

- RED covered chained typed holders, raw indirect identity, non-UTF-8 keys, signature holder chains, and a checkbox without `/AP`.
- Focused GREEN suites passed: form helper 58/58, renderer 161/161, signatures 9/9, CLI AcroForm transforms 14/14, plus targeted JSON and CLI appearance tests.

## Final verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `scripts/patch-coverage.sh --base origin/main`: flpdf 950 changed lines, 0 uncovered; report 16 changed lines, 0 uncovered; 100%.

The branch was intentionally not rebased or pushed; synchronization is left to the parent workflow.
