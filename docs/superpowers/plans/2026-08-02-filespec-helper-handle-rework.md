# Filespec Helper Handle Rework Implementation Plan

> **For agentic workers:** Execute inline in this session. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Translate qpdf 11.9.0 Filespec and EmbeddedFile helpers onto `ObjectHandle` so all helper reads dereference values and metadata mutation never copies attachment payloads.

**Architecture:** `FileSpec` and `EmbeddedFileStream` keep their live qpdf-equivalent handle, resolving it through `Pdf` before each operation. `Pdf::mark_object_dirty` also discards any materialized object memo, so a direct handle mutation is visible to both `resolve` and the incremental writer.

**Tech Stack:** Rust workspace, `ObjectHandle`, `Pdf`, qpdf 11.9.0 helper source.

## Global Constraints

- qpdf 11.9.0 is the behavioral oracle.
- Do not retain compatibility-only `ObjectRef` or `Option` aliases.
- Every production change starts with a focused failing integration test.
- Patch coverage must remain 100 percent against `origin/main`.

---

### Task 1: Prove indirect metadata behavior

**Files:**
- Modify: `crates/flpdf/tests/filespec_helper_tests.rs`

- [ ] Add one test whose `/Subtype`, `/CreationDate`, `/ModDate`, `/Size`, and `/CheckSum` are all indirect (including a two-hop holder), and assert qpdf-shaped getters return their terminal values.
- [ ] Run `cargo test -p flpdf --test filespec_helper_tests -- indirect_metadata` and confirm the current helper fails because it only dereferences `/Params`.

### Task 2: Move helpers to live handles

**Files:**
- Modify: `crates/flpdf/src/filespec_helper.rs`
- Modify: `crates/flpdf/src/attachment_list.rs`
- Modify: `crates/flpdf/tests/filespec_helper_tests.rs`

- [ ] Replace copied Filespec dictionaries and EmbeddedFile streams with resolved `ObjectHandle` access.
- [ ] Make qpdf getter methods return empty byte vectors/zero or a null `ObjectHandle`, matching qpdf's public helper defaults.
- [ ] Make setters mutate the stream or terminal `/Params` dictionary handle and mark only its owning indirect object dirty.
- [ ] Run the focused test after each behavior is implemented and then run `cargo test -p flpdf --test filespec_helper_tests`.

### Task 3: Persist handle mutations and verify output

**Files:**
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/tests/filespec_helper_tests.rs`

- [ ] Add a test that resolves a stream, applies consecutive metadata setters, writes it, and reopens it; the dates and subtype must survive without replacing the stream payload.
- [ ] Run that test and confirm the pre-change memo path returns stale data or requires copied stream replacement.
- [ ] Change `Pdf::mark_object_dirty` to invalidate the materialized memo for that reference before marking it dirty.
- [ ] Run `cargo fmt -- --check`, `cargo test -p flpdf --test filespec_helper_tests`, `cargo test -p flpdf --test embedded_files_tests`, and changed-line coverage.
