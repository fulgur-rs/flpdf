# qtest `test_renumber` Helper Design

**Goal:** Port the portable observable behavior of qpdf 11.9.0's
`qpdf/test_renumber.cc` into `flpdf-qtest-tools` and make all eight
`renumber-objects.test` invocations execute through the canonical flpdf writer
APIs.

**Authority:** The pinned qpdf 11.9.0 source at
`/home/ubuntu/.cache/flpdf/qpdf-11.9.0` and the live qpdf 11.9.0 helper are the
behavioral oracle. The helper is a qtest consumer; it must not introduce a
second writer or renumbering implementation in the flpdf core.

## Scope

The implementation owns:

- the Rust `test_renumber` executable and its usage/error contract;
- recursive comparison of input objects with their emitted objects;
- comparison of qpdf's written and reloaded xref snapshots;
- the eight option/input combinations in
  `qpdf/qtest/renumber-objects.test`;
- release-build, PATH-shim, and qtest manifest wiring in the separate
  `flpdf-qtest` repository.

It does not port C or C++ ABI symbols, vendor qpdf fixtures, or replace the
canonical `PdfWriter` renumbering algorithm.

## qpdf contract

`test_renumber.cc` parses one input and three options:
`--object-streams=preserve|disable|generate`, `--linearize`, and
`--preserve-unreferenced` (`qpdf/test_renumber.cc:14-22,168-205`). Invalid
arguments print the qpdf usage text and exit with status 2.

The helper opens the input, collects `getAllObjects()`, configures one
`QPDFWriter`, writes to memory, and reloads that memory as a second `QPDF`
(`qpdf/test_renumber.cc:206-224`). It then prints the source-to-output object
mapping, reports deleted mappings, recursively compares scalar/array/dictionary
values, and deliberately skips stream payload comparison while reporting that
stream objects are not compared (`qpdf/test_renumber.cc:24-117`).

Finally it compares the written and reloaded xref maps and prints the two
`complete` markers followed by `succeeded` (`qpdf/test_renumber.cc:119-166,
226-257`). The upstream helper contains self-comparisons for some xref values
at lines 147 and 153-154. The port preserves that observable helper contract;
it does not silently strengthen the qpdf test.

The qtest suite invokes the helper eight times: preserve, generate,
linearize, and preserve-unreferenced, each with `minimal.pdf` and
`digitally-signed.pdf` (`qpdf/qtest/renumber-objects.test:17-74`). A live
release qpdf helper run passed all eight cases with status 0 and one
`succeeded` marker per case. The current flpdf-qtest run fails all eight only
because `test_renumber` is not currently available on PATH.

## Responsibility mapping

| qpdf responsibility | flpdf counterpart | Design decision |
|---|---|---|
| `getAllObjects()` and object traversal | `Pdf::object_refs()`, `Pdf::get_object_handle()`, and public `ObjectHandle` type/value accessors | Use the canonical live handle graph; do not materialize a parallel raw-object snapshot. |
| writer option setters | `PdfWriter::set_object_stream_mode`, `set_linearization`, and `set_preserve_unreferenced_objects` | Configure the existing writer directly. |
| memory output | `PdfWriter::set_output_memory`, `write`, and `get_buffer` | Reload the exact bytes returned by the writer. |
| `getRenumberedObjGen` | `PdfWriter::get_renumbered_obj_gen` | Print one mapping for every source object reference. |
| `getWrittenXRefTable` | `PdfWriter::get_written_xref_table` | Compare the writer-owned snapshot with the reloaded document's xref table. |
| `processMemoryFile` and `getXRefTable` | `Pdf::open_mem_owned` and `Pdf::get_xref_table` | Keep reload and xref inspection in the helper boundary. |
| `QPDFXRefEntry` kinds | `XrefEntry::Free`, `Uncompressed`, and `Compressed` | Compare type and the same fields qpdf's helper observes, including its upstream self-comparison behavior. |

The existing `flpdf-egzr.3` prerequisite is closed and already supplies the
source xref/parsed-offset surface. No new core prerequisite or compatibility
bridge is required by this mapping.

## Implementation units

1. `crates/flpdf-qtest-tools/src/renumber.rs` will contain the shared helper
   logic and qpdf-shaped output formatting. Its public entry boundary is the
   binary's `run` function; recursive comparison and xref comparison remain
   private to the helper module.
2. `crates/flpdf-qtest-tools/src/bin/test_renumber.rs` will decode OS arguments,
   open the requested file, invoke the helper, print stdout/stderr through the
   existing qtest binary conventions, and return status 2 for usage or
   operation errors.
3. `crates/flpdf-qtest-tools/Cargo.toml` and `src/lib.rs` will register the
   binary/module without changing the core writer.
4. The isolated `flpdf-qtest` follow-up will add the binary environment
   variable, release build selection, executable shim, README/CI contract,
   and the eight manifest state transitions from helper-unavailable to
   passing. Its generated `harness.log` and `qtest-results.xml` remain ignored
   artifacts and are kept paired for verification.

## Error and comparison policy

Usage errors and file/open/write/reload errors are reported to stderr with
status 2, matching the qpdf helper's `catch (std::exception&)` boundary.
Successful comparisons print the same section markers and end in
`succeeded`. Recursive cycles are stopped using the source indirect
`ObjectRef`, matching qpdf's static visited `QPDFObjGen` set; direct values do
not enter that set. Streams are not decoded or compared because qpdf's helper
explicitly excludes their payloads.

The implementation will first exercise the existing public writer APIs in
tests. If a RED test demonstrates that a required value is not observable at
that public boundary, the missing qpdf responsibility will be isolated and
audited before any core change. A test-only raw-object bridge, sentinel, or
panic is not an acceptable fallback.

## Verification and acceptance

- Unit/integration tests cover valid option combinations, usage and bad
  options, missing/open failure, write/reload failure, scalar/array/dictionary
  comparisons, cycle termination, stream skipping, deleted mappings, and xref
  entry comparison.
- A focused qtest run reaches all eight `renumber-objects` cases with status 0
  and no failure/error testcase outcomes.
- A full qtest survey promotes exactly the eight matching manifest rows and
  has no allowlist regression.
- `cargo fmt --all -- --check`, strict private rustdoc, all-features Clippy,
  workspace tests, qpdf module/deviation checks, and fresh patch coverage pass.
- The helper's stdout/stderr/status and generated output are differentially
  checked against the pinned qpdf 11.9.0 helper for both fixtures and every
  option combination.

