# ASCII85 decoder route cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the legacy production ASCII85 encoder, rename the qpdf-shaped streaming decoder module to `ascii85_decoder.rs`, and preserve all qpdf-compatible decode behavior.

**Architecture:** `pipeline::ascii85_decoder::Ascii85Decoder` remains the single production ASCII85 implementation and is constructed by `Ascii85StreamFilter`. `filters::encode_stream_data` keeps its API but rejects `/ASCII85Decode` encoding because qpdf has no ASCII85 encoder. Test-only fixture helpers produce encoded input for decode tests without becoming production routes.

**Tech Stack:** Rust 2021 workspace, `flpdf` and `flpdf-cli` crates, qpdf 11.9.0 source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`, Cargo unit/integration tests, and qpdf-backed compatibility tests.

## Global Constraints

- Preserve the decoder's current behavior and qpdf correspondence; this change is a route/module cleanup, not a decoder algorithm rewrite.
- Do not keep a compatibility alias for the deleted root encoder module or a production ASCII85 encoder.
- Keep ASCII85 fixture generation under test-only code or fixed fixture bytes.
- Existing unrelated worktree changes are out of scope.
- Use RED -> GREEN TDD: add and run a failing test before changing the production encode route, then run focused and workspace checks after each migration.
- Use the pinned qpdf source and current tests as the semantic oracle; do not infer new decoder behavior from the plan.

## Task 1: Pin the new encode boundary with a failing test

- [ ] Add a unit test in `crates/flpdf/src/filters.rs` named `encode_stream_data_ascii85_is_explicitly_unsupported`.
- [ ] Construct a dictionary with `/Filter /ASCII85Decode`, call `encode_stream_data`, and assert the exact error text:

  `unsupported PDF feature: ASCII85Encode is not supported: qpdf provides an ASCII85 decoder but no encoder`

- [ ] Run the focused test before changing production code:

  ```bash
  cargo test -p flpdf --lib filters::tests::encode_stream_data_ascii85_is_explicitly_unsupported -- --exact
  ```

- [ ] Confirm RED because the old encoder still returns `Ok`.
- [ ] Commit as `test(filters): pin unsupported ASCII85 encode boundary`.

## Task 2: Rename the canonical qpdf-shaped decoder module

- [ ] Rename `crates/flpdf/src/pipeline/ascii85.rs` to `crates/flpdf/src/pipeline/ascii85_decoder.rs` with `git mv`.
- [ ] Change the pipeline module declaration and all production imports to `ascii85_decoder`.
- [ ] Leave the decoder implementation and its tests behaviorally unchanged.
- [ ] Run:

  ```bash
  cargo test -p flpdf --lib pipeline::ascii85_decoder -- --nocapture
  ```

- [ ] Commit as `refactor(pipeline): name the ASCII85 decoder after qpdf`.

## Task 3: Remove the production encoder and migrate crate-unit fixtures

- [ ] Add a small `#[cfg(test)]` helper to `crates/flpdf/src/pipeline/test_support.rs` that emits ASCII85 fixture bytes, including `z` for a zero four-byte group and the `~>` terminator.
- [ ] Remove the root `crate::ascii85` import and module declaration.
- [ ] Replace the `ASCII85Decode` branch in `filters::encode_stream_data` with `Error::Unsupported` using the message pinned in Task 1.
- [ ] Delete `crates/flpdf/src/ascii85.rs`.
- [ ] Replace all production-source test calls to `ascii85::encode` with the test-only helper or fixed bytes, including the page chained-filter test.
- [ ] Run:

  ```bash
  cargo test -p flpdf --lib filters::tests::encode_stream_data_ascii85_is_explicitly_unsupported -- --exact
  cargo test -p flpdf --lib pipeline::ascii85_decoder -- --nocapture
  cargo test -p flpdf --lib filters::tests:: -- --nocapture
  cargo test -p flpdf --lib pages::tests::page_content_bytes_applies_chained_filters -- --exact
  ```

- [ ] Confirm the new boundary test is GREEN and the decoder behavior remains covered.
- [ ] Commit as `refactor(filters): remove legacy ASCII85 encoder route`.

## Task 4: Migrate integration and CLI fixtures off the production encoder

- [ ] Add test-only ASCII85 helper modules under `crates/flpdf/tests/common/ascii85.rs` and `crates/flpdf-cli/tests/support/ascii85.rs`, and include them only from the tests that need encoded fixture bytes.
- [ ] In `multi_filter_chain_tests.rs`, encode payloads with the existing Flate path and wrap the resulting bytes with the test helper for ASCII85 layers. Preserve filter order, predictor parameters, mixed-chain coverage, and scalar `/DecodeParms` coverage.
- [ ] In `writer_tests.rs`, keep Flate compression in the production helper and wrap it with the test-only ASCII85 helper.
- [ ] In `cli_multi_filter_chain.rs`, special-case only the `[/ASCII85Decode /FlateDecode]` fixture construction; retain the generic production encoder path for supported filters such as Flate and ASCIIHex. Replace the direct ASCII85 encoding of the known LZW payload with the test helper and update stale comments.
- [ ] Run:

  ```bash
  cargo test -p flpdf --test multi_filter_chain_tests
  cargo test -p flpdf --test writer_tests pdf_writer_decodes_and_reencodes_multi_filter_stream -- --exact
  cargo test -p flpdf-cli --test cli_multi_filter_chain
  ```

- [ ] Commit as `test: keep ASCII85 fixtures independent of production encoder`.

## Task 5: Update qpdf correspondence documentation and audit stale routes

- [ ] Update `docs/qpdf-correspondence.md` to point at `pipeline/ascii85_decoder.rs`.
- [ ] Update `docs/qpdf-module-doc-index.md` to rename the decoder row and remove the obsolete root encoder row.
- [ ] Run the stale-route audit:

  ```bash
  rg -n "crate::ascii85|ascii85::encode|crates/flpdf/src/ascii85\\.rs|pipeline/ascii85\\.rs" \
    crates/flpdf/src crates/flpdf/tests crates/flpdf-cli/tests docs/qpdf-correspondence.md \
    docs/qpdf-module-doc-index.md
  ```

- [ ] Confirm the audit has no hits in the scoped paths.
- [ ] Commit as `docs: point ASCII85 correspondence to decoder module`.

## Task 6: Run final verification and publish the worktree branch

- [ ] Run formatting and the focused tests again:

  ```bash
  cargo fmt -- --check
  cargo test -p flpdf --test multi_filter_chain_tests
  cargo test -p flpdf --test writer_tests
  cargo test -p flpdf-cli --test cli_multi_filter_chain
  ```

- [ ] Run package and workspace tests:

  ```bash
  cargo test -p flpdf
  cargo test -p flpdf-cli
  cargo test
  ```

- [ ] Run clippy and strict workspace rustdoc:

  ```bash
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
    cargo doc --workspace --no-deps --document-private-items
  ```

- [ ] Run `git diff --check` and a final production-route inventory confirming only the decoder remains under `src/pipeline` and no root ASCII85 encoder path exists.
- [ ] Push Beads state with `bd dolt push` and push the worktree branch with `git push -u origin feature/flpdf-ascii85-decoder-route`.
- [ ] Report the branch, commits, verification results, and any qpdf-dependent tests that were skipped because qpdf is unavailable.
