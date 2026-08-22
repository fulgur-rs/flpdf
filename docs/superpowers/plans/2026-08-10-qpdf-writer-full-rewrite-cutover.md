# qpdf PdfWriter Full-Rewrite Cutover Implementation Plan

> flpdf's local writer is named `PdfWriter`. `QPDFWriter` below is retained
> only where it names the qpdf 11.9.0 source-oracle class or its cited methods.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax so progress is trackable.

**Goal:** Make `PdfWriter` the only flpdf PDF document-output writer and match qpdf 11.9.0's fresh full-rewrite lifecycle and observable output. Remove the PDF incremental-output implementation, its public selector API, and signature-preserving writer policy while keeping incremental PDF reading and JSON/Pipeline incremental delivery.

**Architecture:** Add a public `PdfWriter<'pdf, R>` that owns a live `Pdf<R>` borrow, a qpdf-shaped output sink, and private `WriterSettings`. The writer exposes output setup, qpdf writer setters, `get_final_version`, `write`, `get_buffer`, `get_renumbered_obj_gen`, and `get_written_xref_table`. The existing plain/object-stream/encryption/linearization emitters become internal stages called by this object and return one internal result containing final version, renumbering, and written xref data. CLI and library mutation consumers configure this writer directly. No public function or field selects incremental PDF output.

**Tech Stack:** Rust workspace (`crates/flpdf`, `crates/flpdf-cli`), qpdf 11.9.0 source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`, qpdf differential/byte gates, existing PDF fixtures and golden references, Beads issue `flpdf-25kg.6.2`.

## Global Constraints

- Use the pinned qpdf source and live `/usr/bin/qpdf` 11.9.0 as the semantic and byte-output oracle. The relevant source anchors are `include/qpdf/QPDFWriter.hh:53-428`, `libqpdf/QPDFWriter.cc:88-109`, `:2008-2025`, `:2187-2203`, and `:2991-3044`.
- `PdfWriter::write` is the only flpdf PDF output route and mirrors qpdf's `QPDFWriter::write`. Every successful PDF output starts with a new header/body/xref and has no output-created `/Prev`; no code may copy `Pdf::source_bytes()` or append a revision.
- Do not retain `write_pdf`, `write_pdf_with_options`, `write_qdf`, `WriteOptions`, `WriteOptions::full_rewrite`, a compatibility alias, or an internal adapter solely to preserve the old public shape. Temporary migration helpers must be deleted before the final no-callers check.
- Keep reader parsing of classic/xref-stream `/Prev` chains, `source_xref_entries` needed by object-stream preservation, JSON incremental writers, and Pipeline stage lifecycle. “Incremental” test names for those non-PDF responsibilities are not deletion candidates.
- qpdf's `setStreamDataMode`, `setCompressStreams`, `setDecodeLevel`, QDF, content-normalization, version, ID, encryption, linearization, output, and result-query responsibilities must be represented by the new writer. The old flpdf-only `NewlineBeforeEndstream::No` middle mode and all append-only signature selection are removed.
- Preserve the existing qpdf-zlib-compat exception: default Pure-Rust DEFLATE may differ in compressed bytes, but decoded semantics must match; strict byte gates use `--features qpdf-zlib-compat`.
- Use RED→GREEN TDD for each route. Run the focused test immediately after each implementation slice, then run formatting, strict docs, clippy, focused parity tests, and workspace tests before integration.
- Preserve `/home/ubuntu/flpdf` `main` and unrelated worktrees. The old incremental matrix worktree/PR #710 is not an implementation basis; do not merge it.

---

## Task 1: Add qpdf writer RED gates before changing the route

**Files:**

- Create `crates/flpdf/tests/pdf_writer_contract_tests.rs`.
- Update `crates/flpdf/tests/cmp_diff_zero_tests.rs` and `crates/flpdf/tests/deterministic_id_qpdf_parity_tests.rs` with the new writer-construction helper after the public type exists.
- Update `crates/flpdf/tests/reader_tests.rs` and `crates/flpdf/tests/xref_tests.rs` only where a writer call is needed to construct an input; leave their `/Prev` parsing assertions intact.

**Contract to test:**

- `PdfWriter::new(&mut pdf)` followed by `set_output_memory`, `set_static_id(true)`, and `write` produces a valid fresh PDF whose trailer has no `/Prev`.
- A source with a real `/Prev` chain remains readable before writing, and the newly written output contains no `/Prev` while preserving the final document graph.
- `set_output_memory` followed by `write` permits one `get_buffer` call; missing output, a second output setup, a second `write`, and `get_buffer` before/after the wrong sink return explicit errors.
- `get_final_version` agrees with qpdf's header for the source fixture and the configured minimum/forced version; calling it prepares the writer before `write` without changing the final output.
- Default, `Disable`, `Preserve`, and `Generate` object-stream modes, QDF, `stream-data`, recompress, newline, static/deterministic IDs, direct encryption, preserved source encryption, donor-copy encryption, and linearization are wired through the object rather than an options struct.
- `get_renumbered_obj_gen` and `get_written_xref_table` return the mappings that the emitted output actually uses after `write`.
- A signed input is rewritten as a full file; the original signed byte range is not asserted as preserved. Read-only `signatures` inspection still sees a reachable signature unless the graph transformation removes it.

**Steps:**

- [ ] Write the contract tests against the concrete `PdfWriter` API described in Task 2, including a synthetic two-revision `/Prev` fixture and the existing `one-page`, `three-page-objstm`, QDF, encryption, and linearization fixtures.
- [ ] Add qpdf command helpers that require `qpdf --version` to report 11.9.0 for applicable byte/semantic gates; report infrastructure failure instead of silently skipping an applicable writer gate.
- [ ] Run `cargo test -p flpdf --test pdf_writer_contract_tests`; record the expected compile/API failures as the RED baseline.
- [ ] Run the existing qpdf parity tests once to record the pre-cutover baseline and identify every helper that still constructs `WriteOptions`.

## Task 2: Introduce the qpdf-shaped writer object and private settings

**Files:**

- Create `crates/flpdf/src/writer/settings.rs`.
- Create `crates/flpdf/src/writer/pdf_writer.rs`.
- Modify `crates/flpdf/src/writer.rs` module declarations and shared writer helpers.
- Modify `crates/flpdf/src/lib.rs` exports and crate-level writer documentation.
- Modify `crates/flpdf/src/pipeline.rs` only if the writer needs a private `Pipeline`→`Write` adapter; preserve its public trait and finish semantics.

**Concrete API:**

```rust
pub struct PdfWriter<'pdf, R: Read + Seek + 'static> { /* Pdf borrow, sink, settings, state */ }

impl<'pdf, R: Read + Seek + 'static> PdfWriter<'pdf, R> {
    pub fn new(pdf: &'pdf mut Pdf<R>) -> Self;

    pub fn set_output_file(&mut self, path: impl AsRef<Path>) -> Result<()>;
    pub fn set_output_writer<W: Write + 'static>(&mut self, writer: W) -> Result<()>;
    pub fn set_output_memory(&mut self) -> Result<()>;
    pub fn set_output_pipeline<P: Pipeline + 'static>(&mut self, pipeline: P) -> Result<()>;
    pub fn get_buffer(&mut self) -> Result<Vec<u8>>;

    pub fn set_object_stream_mode(&mut self, mode: ObjectStreamMode);
    pub fn set_stream_data_mode(&mut self, mode: StreamDataMode);
    pub fn set_compress_streams(&mut self, value: bool);
    pub fn set_decode_level(&mut self, level: DecodeLevel);
    pub fn set_recompress_flate(&mut self, value: bool);
    pub fn set_content_normalization(&mut self, value: bool);
    pub fn set_qdf_mode(&mut self, value: bool);
    pub fn set_preserve_unreferenced_objects(&mut self, value: bool);
    pub fn set_newline_before_endstream(&mut self, value: bool);
    pub fn set_minimum_pdf_version(&mut self, version: impl Into<String>, extension_level: i64) -> Result<()>;
    pub fn force_pdf_version(&mut self, version: impl Into<String>, extension_level: i64) -> Result<()>;
    pub fn set_extra_header_text(&mut self, text: impl Into<String>);
    pub fn set_deterministic_id(&mut self, value: bool);
    pub fn set_static_id(&mut self, value: bool);
    pub fn set_static_aes_iv(&mut self, value: bool);
    pub fn set_suppress_original_object_ids(&mut self, value: bool);
    pub fn set_preserve_encryption(&mut self, value: bool);
    pub fn set_encryption_parameters(&mut self, params: EncryptParams);
    pub fn copy_encryption_parameters(&mut self, source: CopyEncryptionSource);
    pub fn set_linearization(&mut self, value: bool);
    pub fn set_linearization_pass1_filename(&mut self, path: impl Into<PathBuf>);
    pub fn set_pclm(&mut self, value: bool);
    pub fn register_progress_reporter(&mut self, reporter: Box<dyn FnMut(u8) + 'static>);

    pub fn get_final_version(&mut self) -> Result<String>;
    pub fn write(&mut self) -> Result<()>;
    pub fn get_renumbered_obj_gen(&self, source: ObjectRef) -> Result<Option<ObjectRef>>;
    pub fn get_written_xref_table(&self) -> Result<BTreeMap<ObjectRef, XrefEntry>>;
}
```

`DecodeLevel` has `None`, `Generalized`, `Specialized`, and `All`, matching qpdf's `qpdf_stream_decode_level_e`. `WriterSettings` is crate-private and contains these controls plus `preserve_encryption`, encryption sources, linearization state, PCLm state, output-only preparation state, and the final result. It contains no `full_rewrite` or incremental fields. `WriterOutput` owns a file/writer/pipeline adapter or memory buffer so output is configured once and finished once; `get_buffer` consumes the memory result exactly once.

**Steps:**

- [ ] Write unit tests in `pdf_writer.rs` for output-one-time state, write-once state, final-version preparation, setter rejection after preparation where qpdf forbids configuration changes, sink flush/finish, partial-output error propagation, progress `0..=100`, and the result-query precondition.
- [ ] Implement `WriterSettings::default` from qpdf's `QPDFWriter::Members` defaults: preserve object streams, compress enabled, generalized decode after setup, no QDF/normalization/linearization/PCLm, preserve encryption enabled, and no deterministic/static ID.
- [ ] Move the existing public option enums that still match qpdf into `settings.rs`; replace the flpdf-only `NewlineBeforeEndstream::No` variant with the boolean qpdf setter and update all framing helpers to receive the effective boolean.
- [ ] Implement output setup with `File::options().read(true).write(true).create(true).truncate(true)` for filename output, owned `Write` sinks for files/stdout wrappers, an owned Pipeline adapter that calls `finish`, and a memory sink whose bytes are transferred by `get_buffer`.
- [ ] Add `WriterPreparation`/`WriterResult` internal types. Preparation must trim output-sensitive trailer keys including `/Prev`, normalize `/Extensions` directness as qpdf does, apply qpdf setup precedence (linearization disables QDF; QDF/PCLm/normalization/decode disable preservation unless explicit encryption is selected; forced version below 1.5 disables object streams), and compute the final version/extension level.
- [ ] Make `PdfWriter::get_final_version` run preparation once and cache it; make `write` require an output sink, run preparation if needed, dispatch standard/PCLm or linearized emission exactly once, finish the sink, cache the final maps, and report progress completion.
- [ ] Export only `PdfWriter`, `DecodeLevel`, the qpdf-compatible setting enums, encryption parameter types, `ObjectRef`, and `XrefEntry` needed by the writer result. Remove the old writer free-function exports from `lib.rs` after consumer migration, not by adding aliases.
- [ ] Run `cargo test -p flpdf --test pdf_writer_contract_tests` and `cargo test -p flpdf --lib writer::pdf_writer`; the new contract tests must compile and fail only on not-yet-migrated emission behavior.

## Task 3: Make every full-rewrite emitter qpdf-faithful and return writer results

**Files:**

- Modify `crates/flpdf/src/writer.rs`.
- Modify `crates/flpdf/src/writer/plain/mod.rs`, `crates/flpdf/src/writer/plain/plan.rs`, `crates/flpdf/src/writer/plain/body.rs`, and `crates/flpdf/src/writer/plain/xref.rs`.
- Modify `crates/flpdf/src/writer/object_streams.rs`, `crates/flpdf/src/writer/serialize.rs`, `crates/flpdf/src/writer/encrypted_strings.rs`, and `crates/flpdf/src/writer/encryption_state.rs`.
- Modify `crates/flpdf/src/filters.rs` and `crates/flpdf/src/content_normalizer.rs` only where the new decode-level and content-normalization controls need the existing qpdf filter/normalizer primitives.
- Modify `crates/flpdf/src/encryption.rs` documentation and source-encryption construction helpers.

**Steps:**

- [ ] Rename the internal `WriteOptions` implementation type to `WriterSettings` and migrate every internal signature (`effective_pdf_version`, extension handling, stream policy, encryption context, object-stream planner, plain plan/body/xref, QDF emitter, and encryption emitters) without keeping a public alias.
- [ ] Split `emit_canonical_pdf_inner` into the `PdfWriter::prepare`/standard-emission stages. Keep the existing catalog snapshot/restore protection, but remove comments and branches whose only purpose was a later incremental append.
- [ ] Replace `strip_incremental_trailer_keys` with a common qpdf output-trailer trim helper used by standard, QDF, xref-stream, and linearized output. Assert that `/Prev`, `/XRefStm`, and source-only append keys never enter the final trailer.
- [ ] Implement qpdf's stream decision tree: `setStreamDataMode` maps to compression/decode settings; `setDecodeLevel` controls which filters are decoded; lone Flate is preserved unless recompress is enabled; unsupported image filters pass through verbatim; QDF forces generalized decode and human-readable framing; content normalization runs only when selected.
- [ ] Implement `set_preserve_unreferenced_objects`: the normal plan starts at the trimmed trailer, while the enabled plan enqueues every source object before the root in qpdf order. Keep page-operation resource pruning separate from writer reachability.
- [ ] Implement source-encryption preservation from the opened `Pdf` when preservation remains enabled, including the source `/Encrypt` dictionary, file key, permanent ID, metadata policy, object/stream keying, forced-version incompatibility handling, and the qpdf precedence of explicit encryption/copy-encryption over preservation. Add a private `Pdf` helper that constructs `CopyEncryptionSource` from authenticated source state without exposing reader internals.
- [ ] Replace the current `CompressStreams`-only version logic with qpdf's version/extension pairwise rules for minimum, forced, ObjStm/xref-stream, encryption, linearization, and source Catalog `/Extensions`.
- [ ] Make plain and specialized emitters return `WriterResult` containing old→new object mapping and every emitted `XrefEntry`; include encryption dictionaries, ObjStm type-2 entries, QDF length holders, and xref stream entries. Do not synthesize result data from the source xref table.
- [ ] Add the qpdf PCLm branch behind `set_pclm`: reproduce the `%PCLm 1.0` header, page/contents/strip enqueue order, synthetic `q /image Do Q` transform streams, no encryption/stream prefiltering, and standard xref/trailer emission for PCLm-structured inputs. Reject PCLm combined with linearization using a qpdf-shaped error.
- [ ] Add progress event accounting around preparation, object emission, linearization passes, sink finish, and final `100`; verify callback ordering and that errors do not report a false successful completion.
- [ ] Run focused tests: `cargo test -p flpdf --test writer_tests`, `cargo test -p flpdf --test object_streams_writer_tests`, `cargo test -p flpdf --test stream_data_tests`, `cargo test -p flpdf --test compress_streams_tests`, `cargo test -p flpdf --test newline_before_endstream_tests`, and `cargo test -p flpdf --test encrypt_writer_smoke`.

## Task 4: Put linearization under PdfWriter

**Files:**

- Modify `crates/flpdf/src/linearization/writer.rs`, `crates/flpdf/src/linearization/plan.rs`, `crates/flpdf/src/linearization/check.rs`, and `crates/flpdf/src/linearization/show.rs`.
- Modify `crates/flpdf/src/linearization/mod.rs` exports.
- Modify `crates/flpdf/src/writer/pdf_writer.rs` and `crates/flpdf/src/writer.rs`.
- Modify `crates/flpdf-cli/src/main.rs` to stop constructing a plan from one `Pdf` and writing a second independently opened `Pdf`.

**Steps:**

- [ ] Change internal linearization writer functions to consume `&WriterSettings`, return the same `WriterResult` metadata alongside `LinearizedDocument`, and preserve the existing pass-1 byte/back-patch implementation and qpdf-zlib gates.
- [ ] Move `LinearizationPlan::from_pdf_with_object_stream_mode` and `RenumberMap::from_plan` into `PdfWriter::write` when `set_linearization(true)` is active. Apply all CLI graph mutations to the one live `Pdf` before the plan is made.
- [ ] Route `set_linearization_pass1_filename` through `write_linearized_with_pass1_file`, back-patch the returned document, and send the final bytes through the configured `PdfWriter` sink.
- [ ] Populate writer result maps for linearized uncompressed and compressed entries from the final linearization layout, so result queries do not report standard-writer data for a linearized write.
- [ ] Keep `check_linearization` and `show_linearization` as reader/inspection APIs, but remove direct public writer entry points that bypass `PdfWriter`; migrate their unit/integration tests to the canonical object.
- [ ] Run `cargo test -p flpdf --test cmp_linearize_tests`, `cargo test -p flpdf --test cmp_linearize_objstm_tests`, `cargo test -p flpdf --test linearize_classic_tests`, and `cargo test -p flpdf --test linearize_objstm_generate_tests`.

## Task 5: Migrate library consumers and public documentation

**Files:**

- Modify `crates/flpdf/src/page_split.rs`, `crates/flpdf/src/page_extract.rs`, `crates/flpdf/src/page_merge.rs`, and `crates/flpdf/src/qdf_fix.rs` documentation/examples.
- Modify library test consumers in `crates/flpdf/src/acroform_field_prune.rs`, `crates/flpdf/src/form_field_object_helper/rendering.rs`, `crates/flpdf/src/outline_dest_remap.rs`, `crates/flpdf/src/overlay.rs`, `crates/flpdf/src/overlay_annotations.rs`, `crates/flpdf/src/page_annotation_flatten.rs`, `crates/flpdf/src/page_rotate.rs`, `crates/flpdf/src/page_tree_rebuild.rs`, `crates/flpdf/src/reader.rs`, `crates/flpdf/src/reader/resolver.rs`, and `crates/flpdf/src/subset_prune.rs`.
- Modify writer-adjacent test/helper files `crates/flpdf/src/writer/object_streams.rs`, `crates/flpdf/src/writer/plain/mod.rs`, `crates/flpdf/src/writer/plain/plan.rs`, `crates/flpdf/src/writer/plain/body.rs`, and `crates/flpdf/src/writer/encrypted_strings.rs`.
- Modify `crates/flpdf/src/object.rs` and `crates/flpdf/src/object_handle.rs` comments that describe default or required incremental PDF output.

**Steps:**

- [ ] Replace each test-only `write_pdf`/`write_pdf_with_options` call with a local `PdfWriter` helper that configures the exact qpdf settings required by that test; use memory output and `get_buffer` for byte assertions.
- [ ] Change `page_split::split_pages` to always rewrite each chunk through a new `PdfWriter`, carrying the deterministic-ID setting directly; remove the boolean that selected incremental chunks and update its docs to state that orphaned objects are handled by full-writer reachability.
- [ ] Change page extraction, page merge, attachment, annotation, rotation, outline, subset, and form-field tests to assert mutation survival in fresh output rather than appended generations/source-prefix preservation.
- [ ] Remove `write_qdf` documentation and route QDF examples through `PdfWriter::set_qdf_mode(true)`; keep `fix_qdf` as the byte-level repair function that operates on raw bytes.
- [ ] Update `crates/flpdf/src/lib.rs` examples and module docs so the only PDF write example constructs/configures/writes `PdfWriter`; keep JSON/Pipeline incremental wording scoped to those APIs.
- [ ] Run `rg -n "write_pdf_with_options|write_pdf\\(|write_qdf|WriteOptions" crates/flpdf/src` and migrate every remaining PDF writer hit. The only remaining matches may be explicit historical text in the cutover plan/spec, not source code or tests.
- [ ] Run `cargo test -p flpdf --lib` and the focused helper suites after the migration.

## Task 6: Migrate CLI output and remove flpdf-only route flags

**Files:**

- Modify `crates/flpdf-cli/src/main.rs`.
- Modify `crates/flpdf-cli/tests/cli_full_rewrite.rs` (retain the route-behavior name even though the CLI flag was removed).
- Modify `crates/flpdf-cli/tests/cli_byte_identical.rs`, `crates/flpdf-cli/tests/cli_byte_identical_overlay.rs`, `crates/flpdf-cli/tests/cli_linearize.rs`, `crates/flpdf-cli/tests/cli_linearize_objstm.rs`, `crates/flpdf-cli/tests/cli_multi_filter_chain.rs`, `crates/flpdf-cli/tests/cli_qdf.rs`, `crates/flpdf-cli/tests/cli_qdf_roundtrip_matrix.rs`, `crates/flpdf-cli/tests/cli_recompress_flate.rs`, `crates/flpdf-cli/tests/cli_tests.rs`, `crates/flpdf-cli/tests/compat_baseline_filter.rs`, `crates/flpdf-cli/tests/compat_baseline_metadata.rs`, `crates/flpdf-cli/tests/compat_matrix_tests.rs`, and `crates/flpdf-cli/tests/encrypt_cli_tests.rs`.

**Steps:**

- [ ] Replace the CLI's `WriteOptions` value with a CLI-local `WriterConfiguration` containing only qpdf settings. Implement one `configure_pdf_writer` function that applies every field through `PdfWriter` setters, including `min_extension_level` accumulated from overlay inputs.
- [ ] Remove `RewriteCommand::full_rewrite`, the `--full-rewrite` clap argument, its linearization conflict diagnostic, all comments/branches that promoted options to full rewrite, and all `full_rewrite` assignments. `rewrite`, top-level rewrite, page operations, QDF, and attachment operations all call the same writer lifecycle.
- [ ] Make `run_rewrite` configure and write one `PdfWriter` to either an owned `PipelineWriter` for `-` or an output file. For linearization, set the writer's linearization controls instead of opening `pdf2`, building a plan in the CLI, and writing raw `LinearizedDocument` bytes.
- [ ] Make page extraction and page-op split paths use memory-output `PdfWriter`, retrieve bytes once, and pass those bytes to `split_pages`; configure each generated chunk with qpdf settings and never select an incremental path.
- [ ] Remove `reject_encrypted_write`. Map `--decrypt`/`--remove-restrictions` to `set_preserve_encryption(false)`, map explicit `--encrypt` and donor-copy to the corresponding setters, and let the writer preserve authenticated source encryption by default where qpdf does.
- [ ] Keep `--qdf`, `--stream-data`, `--recompress-flate`, version, ID, newline, ObjStm, pass-1, and warning behavior, but make their diagnostics describe qpdf setup precedence rather than route promotion.
- [ ] Update CLI tests: remove `--full-rewrite` from successful invocations, delete the signed incremental-preservation test, add assertions that the default rewrite has no `/Prev` and invalidates signed byte ranges, and retain signature inspection/removal tests.
- [ ] Run `cargo test -p flpdf-cli --test cli_full_rewrite`, `cargo test -p flpdf-cli --test cli_byte_identical`, `cargo test -p flpdf-cli --test cli_linearize`, `cargo test -p flpdf-cli --test cli_qdf`, and `cargo test -p flpdf-cli --test encrypt_cli_tests`.

## Task 7: Delete PDF incremental output and its writer-only state

**Files:**

- Modify `crates/flpdf/src/writer.rs`.
- Modify `crates/flpdf/src/reader.rs`, `crates/flpdf/src/pdf.rs`, `crates/flpdf/src/engine.rs`, and `crates/flpdf/src/reader/resolver.rs` for fields/helpers used only by source-prefix copying and `/Prev` emission.
- Modify `crates/flpdf/src/signatures.rs` and `crates/flpdf/src/lib.rs`.
- Modify `crates/flpdf/tests/writer_tests.rs`, `crates/flpdf/tests/xref_tests.rs`, `crates/flpdf/tests/signature_rewrite_tests.rs`, `crates/flpdf/tests/sig_flags_tests.rs`, `crates/flpdf/tests/filespec_helper_tests.rs`, `crates/flpdf/tests/fuzz_regression_tests.rs`, and any file returned by the final no-callers search.

**Steps:**

- [ ] After Tasks 5–6 pass, run `rg -n "write_pdf_incremental|write_incremental_|IncrementalXref|ObjStmIncremental|source_bytes\\(|source_xref_offsets|previous_xref_offset|strip_incremental_trailer_keys" crates/flpdf/src crates/flpdf/tests crates/flpdf-cli/src crates/flpdf-cli/tests` and classify every hit as delete, reader preservation, JSON/Pipeline preservation, or test migration.
- [ ] Delete `write_pdf_incremental`, `ObjStmIncremental`, touched-object collection, source-prefix copying, incremental object/ObjStm/xref/trailer emitters, incremental ID logic, and their unit tests from `crates/flpdf/src/writer.rs`.
- [ ] Move the common output-trailer trim helper before the deleted block and keep only its full-rewrite callers. Remove `Pdf::source_bytes`, `Pdf::startxref`/`previous_xref_offset`, and resolver physical-input helpers only when the no-callers check proves they serve no reader/JSON/Pipeline path; retain `source_xref_entries` for ObjStm preserve and reader `/Prev` parsing.
- [ ] Remove `SignatureWriteMode`, `SignatureRewriteImpact`, `SignatureRewriteReason`, `signature_rewrite_impact`, and `would_rewrite_invalidate_signatures`. Keep `SignatureInfo`, `signatures`, `signatures_with_max_depth`, `/SigFlags` inspection/clearing, and explicit `disable_digital_signatures`/signature-value removal transformations.
- [ ] Rewrite signature tests around read-only inspection and full-rewrite invalidation/output reachability. Delete append-only policy assertions and any test that requires source-prefix or signed-byte-range preservation.
- [ ] Update `Pdf` dirty-state comments from “next incremental write” to generic mutation tracking used by live handles, JSON, helpers, and full-writer output preparation. Do not remove dirty tracking still used by those consumers.
- [ ] Run `rg -n "write_pdf_incremental|write_incremental_|WriteOptions|full_rewrite|SignatureWriteMode|would_rewrite_invalidate_signatures|source_bytes\\(" crates/flpdf/src crates/flpdf/tests crates/flpdf-cli/src crates/flpdf-cli/tests`; require zero production/test matches except intentional migration-plan/spec text and JSON/Pipeline incremental terminology that does not name the deleted PDF API.
- [ ] Run `cargo test -p flpdf --test reader_tests`, `cargo test -p flpdf --test xref_tests`, `cargo test -p flpdf --test signature_rewrite_tests`, `cargo test -p flpdf --test sig_flags_tests`, and `cargo test -p flpdf --test json_tests`.

## Task 8: Complete qpdf differential gates and repository verification

**Files:**

- Modify all qpdf writer parity tests listed by `rg -l "WriteOptions|write_pdf_with_options|write_pdf\\(|write_qdf" crates/flpdf/tests crates/flpdf-cli/tests` after the API migration.
- Add or update `tests/fixtures`/`tests/golden/references` only through the existing qpdf-golden generation workflow; do not copy qpdf-qtest licensed fixtures into this repository.
- Update `docs/qpdf-compat-decisions.md`, `docs/signed-pdf.md`, and any writer API docs that still claim incremental PDF output or signed-byte preservation.

**Steps:**

- [ ] Convert the existing plain/QDF/ObjStm/xref-stream/stream-data/ID/encryption/linearization parity matrices to configure `PdfWriter`, retaining their qpdf-zlib-compat byte gates and semantic gates in the default feature set.
- [ ] Add explicit standard-output assertions for no `/Prev`, fresh `%PDF-` header/body, final trailer `/ID`, no source-prefix bytes, qpdf `--check`, and qpdf `--check-linearization` where applicable.
- [ ] Add deterministic tuples for R2/R3/R4/R5/R6 encryption parameters, source-encryption preservation, donor-copy encryption, cleartext metadata, forced-version compatibility, and static AES IV. Compare decrypted semantics and deterministic bytes where qpdf permits them.
- [ ] Add the PCLm fixture/gate if the PCLm branch is selected, including `%PCLm 1.0`, page strip ordering, synthetic transform streams, and qpdf validation of the resulting output.
- [ ] Verify warning-only writes leave the partial/final output and return the qpdf-shaped warning exit status. Keep JSON and Pipeline chunk/finish/error-boundary tests unchanged except for writer API references.
- [ ] Run the required quality gates in this order:
  - [ ] `cargo fmt --all -- --check`
  - [ ] `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items`
  - [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - [ ] focused writer, reader, signature, JSON/Pipeline, and CLI tests from Tasks 3–7
  - [ ] `cargo test --workspace`
  - [ ] `cargo run --bin flpdf -- --check tests/fixtures/minimal.pdf`
  - [ ] `cargo run --bin flpdf -- tests/fixtures/minimal.pdf /tmp/flpdf-qpdf-writer-smoke.pdf`
- [ ] Record qpdf version, feature flags, focused test commands, workspace result, and any allowed zlib byte exception in the Beads notes and implementation PR.

## Task 9: Update Beads and retire the obsolete incremental matrix

**Files/state:** Beads issue `flpdf-25kg.6.2`, old PR #710, current implementation branch/PR.

**Steps:**

- [ ] Before implementation mutation, claim the issue with `bd update flpdf-25kg.6.2 --claim` and replace its title/description/design/acceptance with the committed qpdf-writer cutover spec and this plan; do not leave the old incremental acceptance text active.
- [ ] After the no-callers and verification gates pass, read back `bd show flpdf-25kg.6.2 --json`, run `bd dep cycles`, and verify the parent `flpdf-25kg.6` remains open with the new dependency meaning.
- [ ] Inspect PR #710 metadata by number (`gh pr view 710 --json number,title,state,headRefName,baseRefName`) and close/supersede it with an evidence-backed comment that qpdf 11.9.0 has no incremental writer and the implementation was replaced by the canonical full-writer cutover. Do not merge #710.
- [ ] Commit the implementation in focused commits, push the new branch/PR only after the quality gates pass, then run `bd dolt push` and read back the issue state. Report the exact commit/PR, test commands, qpdf pin, and cleanup state.

## Completion checklist

- [ ] `PdfWriter` is the only flpdf PDF document-output object and exposes the qpdf writer lifecycle/settings/results; qpdf's `QPDFWriter` is referenced only as the oracle class.
- [ ] No PDF incremental append implementation, public route selector, append-only signature policy, or source-prefix writer bookkeeping remains.
- [ ] Reader `/Prev` support, JSON/Pipeline incremental serialization, and read-only signature inspection remain covered.
- [ ] qpdf 11.9.0 semantic/structural/byte gates pass for every applicable writer tuple, with qpdf-zlib-compat explicitly distinguishing the permitted DEFLATE implementation difference.
- [ ] Formatting, strict docs, clippy, focused tests, workspace tests, smoke checks, Beads dependency checks, Beads push, and git/PR handoff are complete.
