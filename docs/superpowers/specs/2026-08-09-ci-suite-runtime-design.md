# CI Suite Runtime Design

## Status

Draft for review.

## Goal

Reduce GitHub Actions execution minutes caused by redundant or probabilistic
test work while preserving the complete default test suite on every OS in the
test matrix.

The change must keep the Linux-only qpdf-zlib-compat byte-parity suite and its
workflow contract intact.

## Current evidence

The slowest individual test is
`linearized_encrypted_outline_and_part8_shared_hint_tables_stay_consistent_across_many_random_iv_runs`
in `crates/flpdf/src/linearization/writer.rs`.

It performs 3000 complete linearized AES-128 writes. Each iteration checks the
linearization structure, decrypts and decodes the hint stream, and compares the
decoded Outlines and Part-8 Shared Objects offsets with physical object
offsets. A warm local run of this one test takes about 20 seconds.

The test was introduced for the previous hint-stream convergence loop, where
fresh IVs during repeated probe/final writes could change the encrypted object
length. The current writer instead builds one complete encrypted hint object
and splices the same bytes into the final pass. The 3000 iterations are now a
probabilistic boundary observation rather than 3000 independent layout cases;
the test does not require the boundary to be observed.

The following existing coverage remains valuable:

- `identical_plaintext_different_iv_can_change_hint_stream_object_length`
  deterministically exercises both sides of the ciphertext-last-byte framing
  boundary with fixed IVs.
- The end-to-end linearization test uniquely checks the decoded Outlines and
  Part-8 Shared Objects hint offsets against the actual shipped bytes. The
  ordinary linearization checker does not validate those internal tables.
- Existing library encryption tests cover ciphertext, decryption, xref
  plaintext, and V=5 AES round trips.
- Existing CLI/qpdf tests cover linearized AES-128 byte parity for one-, two-,
  and three-page inputs on both top-level and `rewrite` surfaces when the
  qpdf-zlib-compat feature is enabled.

## Design

### 1. Keep one end-to-end layout check with deterministic assertions

Replace the 3000-iteration random loop with one invocation of the existing
end-to-end fixture and assertions:

1. Produce an AES-128 encrypted linearized document with a genuinely random
   per-invocation IV.
2. Run the linearization checker and encrypted-body assertions.
3. Decode the hint stream.
4. Compare the Shared Objects and Outlines first-object offsets with the real
   offsets in the emitted bytes.

Remove the trial-count constant, the output/length sets, the informational
probability comments, and the assertion whose only purpose is to prove that
multiple independently randomized outputs differ. The latter is already
covered by the encryption randomness tests, while the fixed-IV emitter test
covers the framing branch directly.

Rename the test so its name describes the invariant it checks rather than a
nonexistent repeated-run requirement. Also remove or correct stale
documentation references to the old convergence-loop test.

This preserves both required properties without making correctness depend on
whether a random run happens to hit a one-byte framing boundary.

### 2. Run the default workspace suite once per OS

In the matrix test job, replace the sequence of focused default-feature test
commands followed by `cargo test -p flpdf` and `cargo test` with one explicit
workspace command:

```text
cargo test --workspace
```

This does not remove any default-feature test target. It removes repeated
execution of the same test binaries while leaving the four-OS matrix intact.
The qpdf installation and all other quality, coverage, and fuzz jobs remain
unchanged.

The Linux amd64 qpdf-zlib-compat block remains a separate explicit list. In
particular, the whole-file feature-gated integration tests must continue to be
invoked with their exact commands so
`ci_runs_every_whole_file_qpdf_zlib_compat_test` remains satisfied.

### 3. Preserve a CI contract for the full suite

Add a small workflow-contract assertion that the matrix test job contains the
gating `cargo test --workspace` command. This protects the requirement that a
future CI cleanup does not accidentally replace the per-OS full suite with
only focused tests. The existing contract for the qpdf-zlib-compat manual
list remains unchanged.

## Guarantees and non-goals

The design guarantees:

- all default workspace tests still run on Linux amd64, Linux arm64, macOS
  arm64, and Windows amd64;
- the internal encrypted hint-table offset regression remains covered;
- both ciphertext framing outcomes remain covered deterministically;
- qpdf-zlib-compat byte-parity coverage and its exact command contract remain
  unchanged;
- no production linearization or encryption behavior changes.

The design does not:

- move tests to a single OS, scheduled workflow, or nightly-only job;
- remove qpdf parity tests;
- change the qpdf oracle version or feature configuration;
- optimize the Rust target cache or qpdf installation steps;
- introduce a test-only production API for injecting encryption IVs.

## Verification contract

Before handoff, run:

- the focused linearization writer test for the renamed end-to-end check;
- `cargo test -p flpdf-cli --test ci_workflow_contract`;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items`;
- `cargo test --workspace`;
- the qpdf-zlib-compat manual tests required by the workflow when the local
  qpdf environment supports them;

The final CI run must show the four matrix OS jobs passing, with the default
workspace suite present on each OS and the Linux qpdf-zlib-compat block still
passing.
