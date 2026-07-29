# PR #591 Remaining Review Remediation Design

## Goal

Address the three selected unresolved review findings on PR #591:

1. retry full-object parsing when a bounded stream-offset parse is truncated;
2. preserve qpdf-ordered repair warnings when opening ultimately fails; and
3. avoid cloning the complete decoded payload for successful strict decodes.

The unresolved DCTDecode finding is explicitly excluded and will be tracked as
a follow-up Bead.

## Oracle

Pinned qpdf 11.9.0 remains authoritative.

- `libqpdf/QPDF.cc:QPDF::reconstruct_xref` emits `file is damaged`, the
  triggering error, and `Attempting to reconstruct cross-reference table`
  before a terminal reconstruction failure escapes.
- `qpdf/test_driver.cc:test_0_1` processes streams with `qpdf_dl_all`.
- `libqpdf/QPDF_Stream.cc` registers `/DCTDecode`, and `SF_DCTDecode` constructs
  `Pl_DCT`. This confirms that DCTDecode is a real parity gap, but its decoder
  implementation is outside this remediation.

All behavioral fixtures must be authored in flpdf and compared against the
pinned executable or an independently built pinned `test_driver`; qpdf-owned
qtest fixtures must not be copied into this repository.

## Selected Design

### 1. Bounded stream-offset fallback

`Pdf::source_stream_data_offset` will retain its current source-xref and parser
based lookup. It will gain the same bounded/full retry policy and the same
`resolution_fallbacks_remaining` budget used by ordinary object resolution:

1. read from the source object offset to the next recorded object offset;
2. parse the indirect-object syntax and return the stream payload offset on
   success;
3. if that bounded parse fails and a later recorded offset exists and budget
   remains, decrement the shared budget, read from the source offset to EOF,
   and retry;
4. if the full retry also fails, return the original bounded error.

The lookup must not use marker scanning. Text resembling `stream` inside
strings, names, comments, or payloads remains non-authoritative.

The regression fixture will contain a real stream followed by a false
uncompressed xref entry whose offset lands inside that stream. Ordinary
resolution and `source_stream_data_offset` must both succeed through their
bounded fallback. A mutation that removes the full retry must make the focused
test fail.

### 2. Failed-open diagnostics

The crate-wide `Error` gains an open-failure variant:

```rust
Error::OpenFailure {
    source: Box<Error>,
    diagnostics: Diagnostics,
}
```

Its `Display` output is the wrapped source error. It exposes read-only access to
the accumulated diagnostics and a way to inspect the wrapped source without
discarding warning state. Errors raised before recovery begins remain their
existing variants; only a failure after qpdf-style recovery has emitted
diagnostics is wrapped.

The xref recovery boundary will create the three diagnostics before attempting
the linear scan. If entry recovery or trailer recovery fails, it returns the
terminal error together with those already-emitted diagnostics. Successful
repair continues to store the same diagnostics on `Pdf`.

The test driver will:

1. detect diagnostics on an open failure;
2. emit them with the existing `write_warning` formatting and stdout-before-
   stderr flush ordering; and
3. emit the wrapped terminal error and exit 2.

This models qpdf's warning-then-exception lifecycle while keeping diagnostics
attached during `?` propagation. It intentionally changes the public error
surface; backward compatibility is subordinate to qpdf parity for this
pre-1.0 crate.

The differential fixture will have a valid PDF header, a broken xref/startxref,
and no recoverable indirect objects or trailer. The pinned test driver and Rust
driver must have identical merged output and exit status. A mutation that
returns the terminal error without diagnostics must fail the test.

### 3. Strict decode without duplicate payload retention

The shared decode engine gains an internal data-event recording mode:

- recovering entry points record `StreamDecodeEvent::Data` exactly as today;
- strict entry points suppress `Data` events while retaining the final decoded
  buffer;
- warning and error events remain recorded and replayed in exactly the same
  order for both modes.

No successful strict path may clone the complete final output merely to create
an event that the strict wrapper immediately discards. Recovering API behavior
and public event types stay unchanged.

An internal regression will run a non-empty successful strict decode through
the non-recording mode and assert that the result owns the decoded bytes but
contains no `Data` event. Existing recovering tests continue to require their
ordered `Data` events, making the mode distinction mutation-sensitive.

## Error and Resource Boundaries

- The stream-offset full retry consumes the existing global per-`Pdf` fallback
  budget; no new unbounded EOF scans are introduced.
- Failed-open diagnostics preserve their original order and offsets.
- Warning-output I/O failures still stop the driver with exit 2 before the
  terminal open error is written.
- Strict decoding changes allocation behavior only. Decoded bytes, error
  classification, warning callback ordering, and output-limit behavior are
  unchanged.

## Follow-up Scope

Create a separate child/follow-up Bead for DCTDecode parity. Its acceptance
criteria must require:

- pinned qpdf 11.9.0 source and live `test_driver 1` evidence for `/DCTDecode`
  and `/DCT`;
- a valid flpdf-authored JPEG fixture and byte-exact decoded output;
- evaluation of a pure-Rust JPEG dependency, color-component output semantics,
  DecodeParms `/ColorTransform`, malformed-JPEG diagnostics, and decode limits;
- no change to passthrough behavior in writer modes that intentionally preserve
  lossy/image streams unless qpdf source and output require it.

The follow-up does not block the three selected PR #591 fixes.

## Verification

Each selected finding follows RED → GREEN → REFACTOR independently.

Focused gates:

- reader fallback unit/integration test;
- failed-open driver unit and pinned differential fixture;
- strict/recovering decode allocation-contract tests.

Final gates:

- `cargo fmt --all -- --check`
- `cargo test -p flpdf-qtest-tools`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `bash tests/fixtures/test_driver/generate.sh --check`
- `bash scripts/qpdf-test-driver-diff.sh --check`
- `python3 scripts/qpdf-module-docs.py --check`
- `bash scripts/patch-coverage.sh --base origin/main`
- `git diff --check origin/main...HEAD`

Changed executable-line coverage must be 100%.

## GitHub State

After implementation is committed, pushed, and verified, post concise
technical replies to the three selected original inline threads. Do not resolve
threads unless the user explicitly requests resolution.
