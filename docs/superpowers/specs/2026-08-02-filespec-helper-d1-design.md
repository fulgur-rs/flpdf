# Filespec helper D1 design

**Issue:** flpdf-d9sq  
**Oracle:** qpdf 11.9.0 `QPDFFileSpecObjectHelper` and `QPDFEFStreamObjectHelper`

## Goal

Make `filespec_helper.rs` a complete Rust counterpart of the two qpdf helpers' public surface. The helper boundary, not the pre-existing convenience API, determines ownership and observable behavior.

## Responsibility boundary

- `FileSpec` owns a `/Filespec` dictionary reference: name selection, `/Desc`, `/EF`, and mutation of those dictionary entries.
- `EmbeddedFileStream` owns an `/EmbeddedFile` stream reference: `/Params`, `/Subtype`, payload creation, and mutation of stream metadata.
- `FileSpecBuilder` remains a convenience composition of the two helpers. It does not become a second implementation of Filespec or EmbeddedFile behavior.
- `json_inspect.rs` remains unchanged here. Its duplicate attachment conversion is D2 work owned by `flpdf-q2fo` and must later consume these helpers.

## qpdf compatibility rules

- The ordered name keys are exactly `UF`, `F`, `Unix`, `DOS`, `Mac` in every preferred lookup.
- `getEmbeddedFileStream("")` skips a non-stream candidate and continues. A non-empty requested key returns that `/EF` value without preference scanning.
- qpdf's `getDescription`, `getFilename`, `getFilenames`, date getters, and subtype getter expose its UTF-8 value view. The qpdf-shaped Rust methods return UTF-8 `String` values using `pdf_string::utf8_value`; existing raw-byte accessors remain lower-level views only and do not reimplement lookup logic.
- A missing or incorrectly typed qpdf scalar accessor has qpdf's empty/zero result. qpdf-shaped Rust methods represent missing string data as `None` and missing size as `0`.
- `getSubtype()` returns the logical PDF name bytes without a leading slash.
- EF creation writes `/Type /EmbeddedFile`, and computes `/Params /Size` plus binary MD5 `/CheckSum` over decoded payload bytes.
- Filespec creation writes indirect `/Type /Filespec`, calls filename setup, and points both `/EF /F` and `/EF /UF` at the supplied EF stream. Description and Unicode filename setters store `pdf_string::new_unicode_string` output, matching qpdf `newUnicodeString` rather than always forcing UTF-16BE.

## Public API shape

Keep existing snake-case Rust methods as compatibility aliases where their semantics already match qpdf. Add qpdf-complete operations with explicit object references and mutation:

- `FileSpec`: preferred filename, all recognized filenames, a requested EF entry, raw `/EF` dictionary access, description and filename setters, and factories from an EF stream or a filesystem path.
- `EmbeddedFileStream`: creation from bytes and a filesystem path, date and subtype setters, and metadata getters whose names expose qpdf's creation/modification/size/subtype/checksum roles. The byte and path variants are the Rust equivalents of qpdf's buffer/string/provider overloads.

Factories return an indirect `ObjectRef` that a caller may wrap immediately. Setter methods mutate their referenced object and return `Result<()>`; fluent qpdf chaining is not used because Rust's `Pdf` borrow rules make an explicit mutable helper operation clearer and keeps the document ownership boundary honest.

## Tests

Integration tests build synthetic PDFs and prove each ordered/failure path before the corresponding production method is introduced. Factory tests assert the resulting dictionaries and raw metadata, including `/F` and `/UF` sharing the same EF reference. Existing attachment-list behavior remains covered by its module tests. The focused gate is `cargo test -p flpdf --test filespec_helper_tests`.
