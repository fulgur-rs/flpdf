# qtest String Adapter Boundary Design

## Context

`flpdf-qtest-tools::character_encoding` implements the qpdf
`test_pdf_doc_encoding` and `test_pdf_unicode` helper binaries. The PDF string
operations it needs are domain operations in `flpdf`; a second qtest-tools
module that only delegates those calls would add an unnecessary layer. The
qpdf responsibility split is:

- qpdf's `libqpdf/QPDF_String.cc` owns PDF string semantics: PDFDocEncoding
  decoding, UTF-8/UTF-16 conversion, and string unparsing.
- qpdf's `qpdf/test_pdf_doc_encoding.cc` and `qpdf/test_pdf_unicode.cc` own
  qtest adapter behavior: byte-oriented input lines, argv validation, output
  formatting, and process results.

The Rust boundary must follow that split. The PDF string semantics belong to
`flpdf`; the qtest binary input/output contract belongs to
`flpdf-qtest-tools::character_encoding`.

## Goals

1. Keep one canonical implementation of PDFDocEncoding decoding, qpdf UTF-8
   normalization, Unicode-string construction, and forced binary serialization.
2. Keep qtest-specific input/output handling in `character_encoding.rs`
   without introducing a delegation-only adapter module.
3. Keep `flpdf`'s normal build independent of qtest-specific names and feature
   gates while exposing only a minimal domain-oriented PDF string interface to
   the helper crate.
4. Preserve qpdf 11.9.0 helper output bytes, stderr, exit statuses, and signal
   behavior.
5. Update correspondence and character-encoding documentation to describe the
   new ownership boundary.

## Non-goals

- Do not duplicate the PDFDocEncoding table or any conversion algorithm in
  `flpdf-qtest-tools`.
- Do not add a `qpdf_*`-prefixed Rust module or a qtest-only public module to
  `flpdf`.
- Do not change the qtest helper command-line or output contract.
- Do not refactor unrelated PDF string consumers beyond routing them through the
  canonical implementation.

## Design

### Core domain module

Create `crates/flpdf/src/pdf_string.rs`. It corresponds to the domain owned by
qpdf's `libqpdf/QPDF_String.cc`, without importing qpdf-specific naming into
the Rust module name. It provides the smallest domain-oriented interface
needed by existing core consumers and the helper crate:

```rust
pub fn utf8_value(stored: &[u8]) -> Vec<u8>;
pub fn new_unicode_string(utf8: &[u8]) -> Vec<u8>;
pub fn unparse_binary(stored: &[u8]) -> Vec<u8>;
```

The implementation moves the existing canonical qpdf-compatible operations
out of `json_inspect` and keeps their unit tests with the domain module. The
PDFDocEncoding table, malformed UTF-8 normalization, BOM handling, UTF-16BE
construction, and lowercase hexadecimal serialization remain in this module.

`pdf_string` is a normal domain module, not a feature-gated qtest API. This
allows `json_inspect`, `nntree`, outline handling, and qtest-tools to consume
the same implementation without making the core crate expose `qtest_string`.

### qtest helper boundary

`character_encoding.rs` calls the ordinary `flpdf::pdf_string` domain API
directly. It owns only qpdf helper behavior: byte-oriented input lines, argv
validation, output formatting, and process results. No qtest-tools string
adapter module is needed because there is no qtest-specific transformation
between the helper and the domain API.

Remove `crates/flpdf/src/qtest_string.rs` and its `lib.rs` module declaration.

### Core consumers

Update the existing `json_inspect`, `nntree`, and outline consumers to use
`crate::pdf_string`. Their behavior and tests remain unchanged; the routing
change must not introduce a second implementation.

### Documentation

Update the generated qpdf module index and the character-encoding design/plan
so that `pdf_string.rs` is documented as the `libqpdf/QPDF_String.cc`
correspondence and `character_encoding.rs` is documented as the qpdf
test-binary boundary. No document should claim that a separate qtest string
adapter module is required.

## Verification

The implementation must pass, in this order:

1. Focused core PDF-string tests and qtest-tools character-encoding tests.
2. `scripts/qpdf-character-encoding-diff.sh --check` against the pinned qpdf
   11.9.0 source/toolchain.
3. Workspace formatting, all-target/all-feature clippy, and workspace tests.
4. The changed-line coverage gate with fresh 100% executable-line coverage.

The qtest differential and CLI tests must assert unchanged stdout bytes,
stderr bytes, exit codes, and SIGABRT behavior for malformed input paths and
directory inputs. The final survey must record zero qtest allowlist
regressions.
