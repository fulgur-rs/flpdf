# flpdf qpdf-style Core Design

## Goal

Build a Pure Rust PDF library that can eventually cover the major capabilities provided by qpdf, with a new Rust-native API rather than a lopdf-compatible API. The first release focuses on safely reading an existing PDF, preserving its structure at the object-tree level, and writing it back as a normalized PDF.

The initial CLI targets a small qpdf-compatible subset:

- `flpdf input.pdf output.pdf`
- `flpdf --check input.pdf`

The long-term goal includes PDF structure manipulation, security, stream filters, normalization, repair, validation, page operations, and CLI compatibility. The first implementation deliberately keeps the scope smaller while establishing the architecture needed for those features.

## Non-Goals For The Initial Release

- Full qpdf CLI compatibility.
- Incremental update writing.
- Digital signature preservation.
- Linearization.
- Complete PDF 2.0 feature coverage.
- A full PDF conformance validator.
- lopdf API compatibility.

## Workspace Layout

The project will be a Rust workspace with separate library and CLI crates.

- `crates/flpdf`: core PDF library.
- `crates/flpdf-cli`: command-line interface.
- `tests/fixtures`: small PDF fixtures and regression inputs.
- `docs/superpowers/specs`: design specifications.

The CLI depends on the library. The library must not depend on the CLI.

## Architecture

The core design follows qpdf's model rather than lopdf's in-memory `Document` model. Input sources must be seekable. The reader loads enough structure to understand the document's cross-reference data and trailers, but indirect objects are resolved lazily on access.

The library hides bookkeeping details such as byte offsets, xref formats, object streams, stream filters, and encryption state. It does not hide the PDF object hierarchy. Users are expected to work with PDF-level objects such as dictionaries, arrays, streams, names, strings, and indirect references.

Core public types:

- `Pdf`: top-level representation of one PDF document.
- `PdfReader<R: Read + Seek>`: owns or coordinates input, xref data, trailers, parsing, and object resolution.
- `PdfWriter<W: Write>`: writes a PDF document.
- `Object`: enum for PDF primitive and composite values.
- `ObjectRef`: object number plus generation.
- `ObjectHandle`: direct or indirect object access handle.
- `ObjectCache`: cache of unresolved, resolved, missing, reserved, and deleted objects.
- `Diagnostic`: structured warning or error.
- `CheckReport`: diagnostics and summary data for `--check`.

The initial compatibility baseline is PDF 1.7. PDF 2.0 should not be structurally blocked, but it is not an initial compatibility target.

## Reading Model

Input APIs accept `Read + Seek` sources. Opening a PDF performs only the initial structural reads:

- Validate and record the PDF header.
- Locate `startxref` near the end of the file.
- Read xref table or xref stream.
- Follow trailer `/Prev` chains.
- Initialize encryption metadata if present, while initial encrypted document support may return an explicit unsupported error.

The reader does not parse every indirect object during open. Indirect objects are represented as `ObjectRef` values and resolved through the cache when accessed.

Cache entry states:

- `Unresolved { source }`: xref data identifies a byte offset or object stream location.
- `Resolved(Object)`: object has been parsed.
- `Missing`: referenced object does not exist and resolves to PDF `null`.
- `Reserved`: reservation entry for future object creation, imports, and mutually referential objects.
- `Deleted`: free xref entry or explicitly deleted object.

When resolving an object stored in an object stream, the reader loads and decodes that object stream, parses the contained objects, populates cache entries for them, and then discards temporary decode buffers. Stream data itself remains lazy: callers can request raw stream bytes or decoded stream bytes, and filters run only when needed.

## Strict And Recovery Modes

Reading supports two modes.

- `Strict`: report specification violations as errors where practical. Used as the basis for `--check`.
- `Recovery`: collect diagnostics and continue when safe. Used for normal content-preserving transformations.

The policy is strict when writing and liberal when reading. Recovery must never silently invent successful output without recording diagnostics for meaningful repairs.

## Writing Model

The initial writer performs complete rewriting only. It does not preserve original byte offsets, original object numbers, original xref representation, incremental update history, or unreachable objects.

Rewrite flow:

- Write a PDF header.
- Start from trailer roots such as `/Root`, and include `/Info` and `/Encrypt` when applicable.
- Traverse reachable indirect objects.
- Build a renumber table from old object references to new generation-zero references.
- Write each reachable object with its new number.
- Normalize unresolved missing references to `null`.
- For streams, update `/Length`, `/Filter`, and `/DecodeParms` according to writer policy.
- Write xref data.
- Write trailer, `startxref`, and `%%EOF`.

Initial output uses a conservative xref table and plain indirect objects rather than xref streams or object streams. The writer API reserves space for later modes:

- `Rewrite`: complete rewrite, implemented first.
- `Incremental`: append-only update, future work.

Signature preservation and history-preserving edits belong to the future `Incremental` mode.

## CLI Design

The first CLI supports:

- `flpdf input.pdf output.pdf`: read in recovery mode and rewrite the file.
- `flpdf --check input.pdf`: inspect and report structural issues.

`flpdf input.pdf output.pdf` prints recoverable warnings to stderr and exits with code `0` if output succeeds. Unrecoverable parse, xref, unsupported encryption, or write errors exit non-zero.

`flpdf --check input.pdf` uses strict checks first and may optionally attempt recovery to report whether the file can still be processed. The check command focuses on actionable diagnostics for PDF developers. It is not intended to be a complete conformance validator.

The CLI is only responsible for argument parsing, display, and exit code decisions. Diagnostics and check data are produced by the library so fulgur can use the same behavior through library APIs.

## Module Structure

The `flpdf` crate will be split into focused modules.

- `object`: `Object`, `Dictionary`, `Array`, `Name`, `String`, `Stream`, `ObjectRef`.
- `parser`: tokenizer, primitive parser, object parser, xref parser, stream boundary handling.
- `reader`: `PdfReader`, header/xref/trailer loading, object resolution.
- `cache`: `ObjectCache` and cache entry states.
- `writer`: rewrite writer, renumbering, xref output.
- `filters`: stream filter implementations, initially including uncompressed streams and Flate where required for common PDF 1.7 structures.
- `diagnostics`: structured warning and error reporting.
- `security`: encryption/decryption extension point, initially explicit unsupported paths where needed.
- `check`: `CheckReport` construction and structural validation.

## Dependency Policy

The library remains Pure Rust. C and C++ FFI are excluded. Existing Rust crates are allowed for low-level algorithms such as compression, hashing, cryptography, command-line parsing, diagnostics, and testing.

Likely dependency categories:

- Compression: Flate support.
- Hashing and crypto primitives: later security support.
- CLI parsing: CLI crate only.
- Error handling and diagnostics: library-safe, lightweight crates.

Dependencies should not force the core library to include CLI-only or platform-specific behavior.

## Testing Strategy

Testing starts with small deterministic fixtures and grows toward compatibility and fuzz testing.

Required initial test groups:

- Parser unit tests for primitive objects, arrays, dictionaries, streams, and indirect objects.
- Xref tests for xref tables, xref streams, and trailer `/Prev` chains.
- Reader tests for lazy object resolution, missing references resolving to `null`, and object stream expansion.
- Writer tests for read/write round trips, renumbering, stream `/Length` updates, and xref output.
- CLI tests for `input output` and `--check` exit codes and stderr/stdout behavior.
- Recovery fixtures for malformed xref data, extra EOF markers, missing objects, and damaged trailers.

Required quality commands once implementation exists:

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features`
- `cargo test --all-targets --all-features`

Fuzzing should be added after the first parser and reader implementation is stable enough to produce meaningful crashes.

## Initial Success Criteria

The first milestone is successful when:

- A minimal workspace builds.
- `flpdf --check input.pdf` can report success or clear structural diagnostics for fixture PDFs.
- `flpdf input.pdf output.pdf` can rewrite simple PDF 1.7 files with xref table or xref stream input.
- Object streams can be resolved lazily for fixture PDFs that use them.
- The reader uses `Read + Seek` and lazy indirect object resolution from the beginning.
- Missing indirect references are handled as PDF `null` where appropriate.
- Diagnostics are available through library APIs and CLI output.

## Future Work

After the first milestone, expand in this order unless project needs change:

- xref stream and object stream completeness.
- Stream filter coverage.
- Recovery improvements.
- Encryption and decryption.
- Page tree helpers and page operations.
- qpdf CLI compatibility expansion.
- Incremental update writing.
- Linearization.
- PDF 2.0-specific behavior.
