# Filespec Helper Boundaries Design

## Goal

Split the current `filespec_helper.rs` implementation at the two qpdf 11.9.0
helper boundaries without changing attachment bytes, provider behavior, or
ObjectHandle identity semantics. The high-level file-I/O facade belongs to the
QPDFJob attachment boundary rather than to either low-level object helper.

## Oracle and boundaries

The pinned qpdf sources are:

- `QPDFFileSpecObjectHelper.cc:10-131` and
  `QPDFFileSpecObjectHelper.hh:27-103` for `/Filespec` validation,
  filename/description selection, `/EF` lookup, construction, and setters.
- `QPDFEFStreamObjectHelper.cc:8-149` and
  `QPDFEFStreamObjectHelper.hh:27-109` for embedded-stream metadata,
  provider factories, and `newFromStream` size/MD5 finalization.

The Rust module tree will mirror those responsibilities:

| Rust owner | Responsibility |
|---|---|
| `filespec_helper/filespec.rs` | `FileSpec`, `FileSpecBuilder`, filename selection, `/EF` selection, Filespec construction and mutation |
| `filespec_helper/embedded_file_stream.rs` | `EmbeddedFileStream`, provider-backed stream construction, `/Params`, MIME type, size and checksum |
| `filespec_helper/shared.rs` | low-level encoding/date/checksum and qpdf-style file-open helpers shared by the two owners and existing parser/job consumers |
| `job/attachments.rs` | path-based add/extract/write orchestration and ASCII filename fallback, alongside the existing `QPDFJob` attachment lifecycle |

`filespec_helper/mod.rs` will only declare those modules and re-export the
low-level public helper types/functions. It will not contain the old
high-level attachment add/extract implementation. The crate-root public
attachment functions will be re-exported from `job`, preserving the useful
crate-level entry points while removing the obsolete `filespec_helper::*`
facade. No compatibility wrapper or duplicate implementation is added.

## Data flow and invariants

`FileSpecBuilder` and `QPDFJob::add_attachments` continue to create streams
through `EmbeddedFileStream`'s live `ObjectHandle` provider path. `FileSpec`
continues to retain the Filespec handle and resolve terminal values through the
owning `Pdf`. The split is physical/module ownership only: qpdf's
`UF,F,Unix,DOS,Mac` priority, direct-vs-indirect `/EF` behavior, warning
timing, provider retries, `/Params /Size`, and binary MD5 bytes remain unchanged.

## Verification

The RED contract test will assert that the old monolithic
`src/filespec_helper.rs` path is gone, both qpdf owner modules exist, the
low-level symbols are owned by their corresponding module, and the high-level
file-I/O functions are owned by `job/attachments.rs`. Existing Filespec,
embedded-file, attachment job, JSON, and CLI suites remain the behavioral
regression tests. The pinned qpdf live fixture probe uses
`tests/fixtures/compat/attachment-two-page.pdf` and compares the attachments
JSON before and after the split.
