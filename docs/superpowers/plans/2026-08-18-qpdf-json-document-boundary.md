# qpdf JSON document boundary implementation plan

## Objective

Port the qpdf 11.9.0 `QPDF_json.cc:795-832` document boundary on top of the
existing canonical `JsonReactor` and ObjectHandle primitives. Complete creation
must parse the exact rootless `JSON_PDF` seed from `QPDF_json.cc:54-63`; update
must preserve objects omitted from the JSON input. The CLI remains the
downstream `.16` task.

## Oracle and responsibility

- qpdf source and `/usr/bin/qpdf` 11.9.0 are authoritative.
- `json/input.rs` owns the already-ported reactor, semantic warnings, deferred
  providers, canonical replacement, and page-flag delegation.
- The new `json/document.rs` owns create/update/import orchestration, source
  descriptions, parser-error wrapping, and final semantic-error aggregation.
- `document_json.rs` remains output-only.
- No `Pdf::empty()` shortcut, legacy `Pdf::set_object` caller, CLI wiring, or
  qpdf-qtest fixture vendoring.

## TDD tasks

1. Add public API tests for complete create, partial update, parser failures,
   semantic aggregation, and exact rootless bootstrap. Run the focused test and
   observe the compile/test failure before adding production code.
2. Add the source-backed document boundary with generic seekable input and
   file convenience methods. Route creation through `open_mem_owned` with the
   exact qpdf seed, then invoke the existing reactor incrementally.
3. Add parser-error and reactor-fatal/error aggregation tests, including input
   name preservation and qpdf warning attribution.
4. Add round-trip and qpdf differential fixtures for create/update, object
   descriptions, omitted objects, stream providers, and the two update page
   metadata flags.
5. Update module docs, `docs/qpdf-correspondence.md`, and the module index so
   `QPDF_json.cc:1-833` has an explicit input-side counterpart.

## Verification

- `cargo fmt --all -- --check`
- focused JSON document tests and existing JSON input tests
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- workspace tests and strict private rustdoc command from CI
- per-PR patch coverage against `feature/flpdf-3yn9-15-json-reactor`
- live qpdf 11.9.0 create/update/error probes before claiming parity
