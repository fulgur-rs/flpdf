# PR #591 Driver Parity Follow-up Design

## Goal

Address two valid PR #591 review findings without weakening the library's
default hardening:

1. preserve non-UTF-8 executable and PDF path data through the test-driver
   filesystem and diagnostic paths;
2. let the qpdf-compatibility driver decode a supported filter chain longer
   than the library's default 16-stage limit.

Keep `/DCTDecode` in existing follow-up `flpdf-n9t0.9`. Track TIFF
`/Predictor 2` as a separate component follow-up.

## Oracle facts

- qpdf 11.9.0 `qpdf/test_driver.cc:3572-3589` receives `char* argv[]`, derives
  `whoami` directly from `argv[0]`, and passes `argv[2]` unchanged to file
  processing. On Unix, those argv and filesystem path bytes need not be UTF-8.
- Test 1 requests `qpdf_dl_all` both for filterability and actual output
  (`qpdf/test_driver.cc:268-272`).
- `QPDF_Stream::pipeStreamData` constructs every parsed filter in reverse order
  without a chain-count check (`libqpdf/QPDF_Stream.cc:529-568`).
- qpdf implements TIFF Predictor 2 as a distinct `Pl_TIFFPredictor` stage
  (`libqpdf/SF_FlateLzwDecode.cc:75-100`). flpdf currently declares its absence
  explicitly, so it is not a small driver-only routing fix.

Pinned qpdf 11.9.0 source and measured merged output/status remain authoritative.

## Decision 1: OS-native arguments and byte-oriented diagnostics

Change the Rust test-driver entry boundary from `env::args`/`Vec<String>` to
`env::args_os`/`Vec<OsString>`. `driver::run` consumes OS-native arguments.

The PDF filename has two representations with separate responsibilities:

- `&OsStr`/`&Path` is the filesystem authority and is passed to
  `std::fs::read`;
- a borrowed/owned byte representation is the diagnostic authority.

On Unix, diagnostics use `OsStrExt::as_bytes` exactly, including invalid UTF-8.
On Windows, file opening and the CRT diagnostic probe use the native wide path;
diagnostic bytes use the existing UTF-8-compatible representation for valid
Unicode and a documented lossy fallback only for unpaired wide values. This
does not regress ordinary Windows paths and removes the Unix panic reported by
the review.

Program-name extraction operates on diagnostic bytes, stripping the final
`/` or `\` component and `.exe` without converting the whole argument to
`String`.

Warning and error writers emit a fixed ASCII prefix/suffix around raw filename
bytes. They must not rebuild the complete line with `format!`, which would
force UTF-8. Diagnostic messages originating from flpdf remain UTF-8 strings.

Only the test number is interpreted as text. Its bytes are parsed by the
existing integer grammar without path conversion; invalid non-ASCII bytes
therefore cannot panic.

## Decision 2: Configurable filter-chain limit with a hardened default

Extend `DecodeLimits` with:

```rust
pub max_filter_chain: Option<usize>
```

`DecodeLimits::default()` sets `max_filter_chain` to `Some(16)` and
`max_output` to `None`. Thus existing strict and recovering library entry
points retain the intentional 16-stage hardening.

Expose a recovering-with-limits entry point so callers that need ordered
`StreamDecodeEvent` output can supply the same limits object as strict decode.
Both the raw `/Filter` array pre-check and the normalized filter-spec count
check consume `limits.max_filter_chain`; `None` means unlimited.

The test driver passes:

```rust
DecodeLimits {
    max_output: None,
    max_filter_chain: None,
}
```

This reproduces qpdf test 1's `qpdf_dl_all` chain behavior while keeping the
ordinary library boundary capped. The driver must not decode the chain in
chunks: doing so would change `/DecodeParms`, event ordering, pending predictor
data, and error precedence.

## Deferred components

### DCTDecode

Existing Bead `flpdf-n9t0.9` remains the owner. This change does not claim DCT
support or reply to its review thread as fixed.

### TIFF Predictor 2

Create `flpdf-n9t0.10` under `flpdf-n9t0`, depending on `flpdf-n9t0.2`.
Its scope is a qpdf-faithful TIFF predictor component shared by Flate and LZW,
not a catch-all driver exception.

Acceptance must cover:

- 8-bit and packed 1/2/4/16-bit samples;
- multiple colors/samples per pixel;
- row reset and partial-row zero padding at finish;
- invalid columns/colors/bits geometry;
- Flate and LZW composition;
- pinned qpdf test-driver merged output/status;
- 100% changed executable-line coverage.

## Error handling

- A non-UTF-8 path must never panic before `driver::run`.
- Existing stdout-before-stderr flush ordering remains unchanged.
- Open failures include the original path bytes followed by the existing CRT or
  Rust error bytes.
- Ordinary library decode continues returning the existing
  `filter chain length 17 exceeds maximum of 16` error.
- The qpdf-driver route removes only that chain-count restriction. Unsupported
  codecs, malformed parameters, codec errors, output limits, warning order, and
  partial-data behavior remain unchanged.

## Acceptance criteria

### Non-UTF-8 paths

- A Unix integration test invokes `flpdf-test-driver` with a valid PDF whose
  filename contains invalid UTF-8 and verifies normal test output/status.
- A Unix failure-path test verifies the raw invalid filename bytes appear in
  the open error and the process exits 2 without a panic.
- A unit test supplies a non-UTF-8 argv0 and verifies byte-exact basename usage
  in the usage line.
- Existing Linux, macOS, and Windows CLI/golden tests remain unchanged in
  meaning and pass.

### Filter-chain length

- The default library API still rejects a 17-stage supported chain.
- Recovering decode with `max_filter_chain: None` decodes that same chain and
  preserves ordered events.
- A deterministic flpdf-authored PDF with 17 `ASCIIHexDecode` stages is added
  to the generator and qpdf differential inventory.
- Its `.out` is generated only from pinned qpdf 11.9.0.
- The differential inventory increases from 37 to 38 fixtures.
- The Rust driver matches qpdf merged output and exit status exactly.

### Quality gates

- `cargo fmt --all -- --check`
- focused driver, filter, golden, and differential tests
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- strict private rustdoc and qpdf module-doc checks
- fresh `scripts/patch-coverage.sh --base origin/main`, with 100% for flpdf and
  report-only changed executable lines and no new coverage exclusions
- all GitHub checks green after push

## GitHub handling

After verified code is pushed:

- reply in the non-UTF-8 thread with the OS-native path tests;
- reply in the overlong-chain thread with the driver-specific unlimited
  `DecodeLimits` evidence;
- do not claim DCT or TIFF Predictor 2 is fixed;
- keep all four threads unresolved unless the user separately authorizes
  resolution.
