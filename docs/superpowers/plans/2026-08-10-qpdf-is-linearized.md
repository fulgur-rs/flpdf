# qpdf `isLinearized` implementation plan

## 1. Establish the canonical RED contract

- Replace the old object-1 fixture helper with a hand-built PDF whose first
  object has a non-1 number and a complete `/Linearized` dictionary in the
  first 1024 bytes.
- Add tests for the qpdf numeric-floor rule, `/L` equality, absent and
  non-integer `/L`, malformed first digit candidates, non-dictionary and
  unresolved candidates, and ignored `/N`/`/O`/`/H`/`/T`/`/P` values.
- Rename the old test so no test depends on `linearized_hint_ref`.
- Run the focused test and confirm it fails against the current
  object-`(1, 0)` implementation before writing production code.

## 2. Implement the qpdf-shaped detector

- Add the smallest resolver/reader seam needed to read the first 1024 bytes
  through live seek/read operations and the existing tokenizer.
- Locate the first `integer integer obj <<` candidate exactly as qpdf does.
- Resolve the candidate at generation `0`, downgrade candidate-resolution
  failures to false, and apply only `/Linearized` and integer `/L` checks.
- Preserve the existing source-error responsibility outside malformed candidate
  handling.

## 3. Cut over and remove the bridge

- Change `check.rs` to call the canonical predicate.
- Delete `Pdf::linearized_hint_ref` and its old wrapper/docs.
- Update the CLI and writer-related tests that still mention or call the old
  API; do not add a compatibility alias.
- Leave deep check/show responsibilities to their existing component boundary.

## 4. Verify and review

- Run the focused reader/check tests and qpdf fixture probes.
- Run `cargo fmt --all -- --check`.
- Run `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- Run the relevant crate tests and then `cargo test` for the workspace.
- Run strict rustdoc and the repository patch-coverage gate.
- Inspect the diff for remaining old-route callers, run `bd dep cycles`, read
  back the Bead state, and persist Beads/Git only after all checks pass.
