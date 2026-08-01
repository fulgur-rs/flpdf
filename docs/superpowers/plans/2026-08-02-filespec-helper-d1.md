# Filespec Helper D1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the qpdf 11.9.0 Filespec and EmbeddedFile helper surface without moving JSON conversion work out of its D2 issue.

**Architecture:** `FileSpec` resolves and mutates Filespec dictionaries; `EmbeddedFileStream` resolves and mutates EmbeddedFile streams. `FileSpecBuilder` composes those two boundaries rather than retaining independent PDF-dictionary construction logic.

**Tech Stack:** Rust workspace, `flpdf` object model, qpdf 11.9.0 source at `scripts/fetch-qpdf-source.sh --print-path`.

## Global Constraints

- Mirror qpdf 11.9.0 `QPDFFileSpecObjectHelper.cc` and `QPDFEFStreamObjectHelper.cc` observable behavior.
- Preferred name-key order is `UF`, `F`, `Unix`, `DOS`, `Mac`.
- Do not alter `json_inspect.rs`; D2 belongs to `flpdf-q2fo`.
- Use RED then GREEN for every production API addition.
- Run `cargo fmt -- --check` and focused `filespec_helper_tests` after each task.

---

### Task 1: Complete Filespec read operations

**Files:**
- Modify: `crates/flpdf/src/filespec_helper.rs`
- Test: `crates/flpdf/tests/filespec_helper_tests.rs`

**Interfaces:**
- Produces methods on `FileSpec` for qpdf preferred filename lookup, all recognized filename keys, requested `/EF` lookup, and raw `/EF` dictionary access.
- Consumes `Pdf::resolve_borrowed`, `resolve_ref_chain`, `Dictionary`, and `ObjectRef`.

```rust
pub fn preferred_filename(&mut self) -> Result<Option<Vec<u8>>>;
pub fn filenames(&mut self) -> Result<BTreeMap<Vec<u8>, Vec<u8>>>;
pub fn embedded_file_for_key(&mut self, key: &str) -> Result<Option<Object>>;
pub fn embedded_file_entries(&mut self) -> Result<Option<Dictionary>>;
```

- [ ] **Step 1: Write failing tests**

Add tests that build `/UF`, `/F`, `/Unix`, `/DOS`, and `/Mac` values and assert `UF` wins, only string-valued entries appear in the filename map, a named `/EF` lookup returns its exact reference, and `UF` non-stream falls through to a lower-priority stream.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p flpdf --test filespec_helper_tests -- preferred_filename`

Expected: compilation failure because the new `FileSpec` operation is absent.

- [ ] **Step 3: Implement minimal read helpers**

Centralize `const NAME_KEYS: [&str; 5] = ["UF", "F", "Unix", "DOS", "Mac"]`. Resolve each candidate through the existing reference-chain helper. For the empty preferred EF request, return only a terminal `Object::Stream`; for a named request, return that `/EF` entry without treating it as a stream requirement.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p flpdf --test filespec_helper_tests -- preferred_filename`

Expected: PASS.

- [ ] **Step 5: Run the focused suite**

Run: `cargo fmt -- --check && cargo test -p flpdf --test filespec_helper_tests`

Expected: all existing and new Filespec helper tests pass.

### Task 2: Complete EmbeddedFile metadata and mutation operations

**Files:**
- Modify: `crates/flpdf/src/filespec_helper.rs`
- Test: `crates/flpdf/tests/filespec_helper_tests.rs`

**Interfaces:**
- Produces `EmbeddedFileStream` metadata aliases plus creation-date, modification-date, and subtype mutation methods.
- Consumes the stream's `/Params` dictionary and `Pdf::set_object`.

```rust
pub fn set_creation_date(&mut self, value: impl AsRef<[u8]>) -> Result<()>;
pub fn set_modification_date(&mut self, value: impl AsRef<[u8]>) -> Result<()>;
pub fn set_subtype(&mut self, value: impl AsRef<[u8]>) -> Result<()>;
```

- [ ] **Step 1: Write failing tests**

Add tests that create a Filespec-backed EF stream, set both dates and subtype, then resolve the stream to assert `/Params /CreationDate`, `/Params /ModDate`, and `/Subtype` contain the expected raw PDF values. Add a missing metadata test that observes `None`.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p flpdf --test filespec_helper_tests -- setter`

Expected: compilation failure because the new mutation methods are absent.

- [ ] **Step 3: Implement minimal metadata operations**

Resolve the indirect EF stream, create `/Params` only when a setter needs it, write date values as `Object::String`, and write subtype as logical `Object::Name` bytes. Preserve existing raw-byte getters and add qpdf-role names only as thin aliases.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p flpdf --test filespec_helper_tests -- setter`

Expected: PASS.

- [ ] **Step 5: Run the focused suite**

Run: `cargo fmt -- --check && cargo test -p flpdf --test filespec_helper_tests`

Expected: all Filespec helper tests pass.

### Task 3: Add qpdf-shaped factories and route the convenience builder through them

**Files:**
- Modify: `crates/flpdf/src/filespec_helper.rs`
- Test: `crates/flpdf/tests/filespec_helper_tests.rs`

**Interfaces:**
- Produces EF creation from decoded bytes and a filesystem path, and Filespec creation from a filename plus EF reference or source path.
- Produces Filespec description/filename mutation operations.
- Consumes `md5_checksum`, `encode_utf16be`, `format_pdf_date`, `ObjectRef`, and `Pdf::set_object`.

```rust
pub fn create<R: Read + Seek>(pdf: &mut Pdf<R>, data: impl AsRef<[u8]>) -> Result<ObjectRef>;
pub fn create_from_path<R: Read + Seek, P: AsRef<Path>>(pdf: &mut Pdf<R>, path: P) -> Result<ObjectRef>;
pub fn create<R: Read + Seek>(pdf: &mut Pdf<R>, filename: &str, ef_ref: ObjectRef) -> Result<ObjectRef>;
pub fn create_from_path<R: Read + Seek, P: AsRef<Path>>(pdf: &mut Pdf<R>, filename: &str, path: P) -> Result<ObjectRef>;
```

- [ ] **Step 1: Write failing tests**

Add a test that creates an EF stream from `b"payload"`, creates a Filespec from `"report.txt"` and that stream reference, then asserts `/Type`, `/Params /Size`, binary `/CheckSum`, `/F`, `/UF`, and equal `/EF /F` plus `/EF /UF` references. Add a temporary-file test for both path factories and a test for a distinct Unicode filename and compatibility filename.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p flpdf --test filespec_helper_tests -- create_filespec`

Expected: compilation failure because the factory operations are absent.

- [ ] **Step 3: Implement factories and builder delegation**

Allocate the next two object references deterministically. EF creation writes `/Type /EmbeddedFile` and `/Params` size/checksum from decoded data. Filespec creation writes `/Type /Filespec`, applies filename setup, then assigns the supplied stream reference under `/EF /F` and `/EF /UF`. Change `FileSpecBuilder::build` to call those helpers for its uncompressed path and retain only its feature-specific compression handling before helper construction.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p flpdf --test filespec_helper_tests -- create_filespec`

Expected: PASS.

- [ ] **Step 5: Run focused quality gates**

Run: `cargo fmt -- --check && cargo test -p flpdf --test filespec_helper_tests && cargo test -p flpdf --test embedded_files_tests`

Expected: all listed tests pass.

### Task 4: Verify public surface and repository integration

**Files:**
- Modify: `crates/flpdf/src/filespec_helper.rs` only if documentation corrections are needed
- Test: `crates/flpdf/tests/filespec_helper_tests.rs`

- [ ] **Step 1: Re-extract the qpdf public surface**

Run: `qpdf_tree=$(scripts/fetch-qpdf-source.sh --print-path); rg -n 'QPDF_DLL|createFileSpec|createEFStream|get[A-Z]|set[A-Z]' "$qpdf_tree/include/qpdf/QPDFFileSpecObjectHelper.hh" "$qpdf_tree/include/qpdf/QPDFEFStreamObjectHelper.hh"`

Expected: every getter, factory, and setter has a documented Rust counterpart or an intentional Rust-equivalent overload.

- [ ] **Step 2: Re-run consumer inventory**

Run: `rg -n 'FileSpec::|EmbeddedFileStream::|FileSpecBuilder::|filespec_dict_to_json' crates/flpdf/src crates/flpdf-cli/src --glob '*.rs'`

Expected: `attachment_list.rs`, `embedded_files.rs`, and CLI callers remain on the helper boundary; `json_inspect.rs` is explicitly unchanged for `flpdf-q2fo`.

- [ ] **Step 3: Run quality gates**

Run: `cargo fmt -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test -p flpdf --test filespec_helper_tests && cargo test -p flpdf --test embedded_files_tests`

Expected: all commands exit 0.

- [ ] **Step 4: Commit the implementation**

Run: `git add crates/flpdf/src/filespec_helper.rs crates/flpdf/tests/filespec_helper_tests.rs docs/superpowers/specs/2026-08-02-filespec-helper-d1-design.md docs/superpowers/plans/2026-08-02-filespec-helper-d1.md && git commit -m "feat(flpdf): complete filespec helper D1"`

Expected: one intentional commit containing the documented D1 surface and tests.
