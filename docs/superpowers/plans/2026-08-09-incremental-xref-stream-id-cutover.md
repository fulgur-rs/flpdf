# Incremental xref-stream ID cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make incremental xref-stream output expose the single qpdf-selected `/ID` through the reader-visible xref-stream dictionary and remove its obsolete trailing classic trailer.

**Architecture:** Select the ordinary/static ID array once in `write_pdf_incremental`. The stream-form branch passes that value to the xref-stream dictionary and writes `startxref`/`%%EOF` directly; the table-form branch keeps the classic xref table and receives the same selected value in its required trailer. The existing reader-visible integration test is the RED gate for both behaviors.

**Tech Stack:** Rust workspace, `cargo test`, flpdf writer/xref APIs, qpdf 11.9.0 source and `/usr/bin/qpdf` as behavioral oracles.

## Global Constraints

- qpdf 11.9.0 source and observed output are the semantic oracle.
- xref-stream output must not append a second classic `trailer` dictionary.
- classic xref-table support for table-form input remains in scope.
- one ordinary/static `/ID` array is selected per incremental write and reused by every emitted xref/trailer form.
- full-rewrite, QDF, linearization, encryption, and deterministic-ID policies are unchanged.
- The original source prefix must remain byte-identical on the incremental path.
- Use RED→GREEN TDD and run relevant tests after every code edit.

---

### Task 1: Lock the reader-visible stream contract with a regression test

**Files:**
- Modify: `crates/flpdf/tests/writer_tests.rs:418-466`

**Interfaces:**
- Consumes: `Pdf::open`, `Pdf::resolve`, `Pdf::set_object`, `write_pdf_with_options`, and `load_xref_and_trailer`.
- Produces: a failing test named `incremental_xref_stream_static_id_is_reader_visible` that later implementation must satisfy.

- [x] **Step 1: Write the failing test**

Use the existing `three-page-objstm.pdf` xref-stream fixture, re-emit the
reachable catalog through `set_object` so the incremental writer emits an
update, set `WriteOptions::static_id = true`, and assert from the reopened
`LoadedXref::trailer` that `/ID[1]` is qpdf's literal pi constant and `/ID[0]`
is the source permanent identifier. Add a raw-output assertion that the
stream-form update contains no `\ntrailer\n` section.

The expected constant is the hand-derived qpdf 11.9.0 value:

```rust
let pi: [u8; 16] = [
    0x31, 0x41, 0x59, 0x26, 0x53, 0x58, 0x97, 0x93,
    0x23, 0x84, 0x62, 0x64, 0x33, 0x83, 0x27, 0x95,
];
assert_eq!(id[1], Object::String(pi.to_vec()));
assert!(!output.windows(b"\ntrailer\n".len()).any(|w| w == b"\ntrailer\n"));
```

- [x] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p flpdf --test writer_tests incremental_xref_stream_static_id_is_reader_visible -- --exact
```

Expected: FAIL because the reader-visible xref-stream `/ID[1]` is the source
or independently generated value rather than the static pi value. The current
implementation also emits a trailing classic trailer, so the no-trailer
assertion must remain part of the same regression contract.

- [ ] **Step 3: Commit the regression test**

```bash
git add crates/flpdf/tests/writer_tests.rs
git commit -m "test: cover incremental xref-stream ID cutover"
```

### Task 2: Cut the stream branch over to one selected ID and no classic trailer

**Files:**
- Modify: `crates/flpdf/src/writer.rs:1087-1118`
- Modify: `crates/flpdf/src/writer.rs:1754-1903`
- Modify: `crates/flpdf/src/writer.rs:1904-1938`
- Modify: `crates/flpdf/src/writer.rs:5968-5979`

**Interfaces:**
- Consumes: `generate_id_array`, `WriteOptions::static_id`, source trailer, and the existing xref offsets.
- Produces: `write_incremental_xref_stream(..., selected_id: &Object, ...)` and `write_incremental_trailer(..., selected_id: &Object, ...)`; only the table branch calls the latter.

- [ ] **Step 1: Select the ID once before the xref-form match**

Immediately before the `let xref_offset = match pdf.last_xref_form()` in
`write_pdf_incremental`, add:

```rust
let selected_id = generate_id_array(pdf.trailer().get("ID"), options.static_id);
```

Pass `&selected_id` to both format-specific writers. Do not call
`generate_id_array` again later in the same incremental write.

- [ ] **Step 2: Make the stream dictionary own the selected ID**

Add `selected_id: &Object` to `write_incremental_xref_stream` and overwrite
the cloned source value after `strip_incremental_trailer_keys`:

```rust
let mut stream_dict = trailer.clone();
strip_incremental_trailer_keys(&mut stream_dict);
stream_dict.insert("ID", selected_id.clone());
```

Keep the existing qpdf-compact ID serializer for the stream dictionary.

- [ ] **Step 3: Remove the stream branch's trailing classic trailer**

Change the xref-form match so the table branch writes its xref table and
classic trailer, while the stream branch writes its xref stream and then
finishes the file directly:

```rust
let xref_offset = match pdf.last_xref_form() {
    XrefForm::Table => {
        let xref_offset = write_incremental_xref(&mut bytes, &final_offsets)?;
        write_incremental_trailer(
            &mut bytes,
            pdf,
            &selected_id,
            &root_ref,
            object_count,
            pdf.previous_xref_offset(),
            xref_offset,
        )?;
        xref_offset
    }
    XrefForm::Stream => {
        let xref_object_number = next_xref_stream_object_number(&final_xref_offsets)?;
        object_count = object_count.max(xref_object_number as usize + 1);
        let xref_offset = write_incremental_xref_stream(
            &mut bytes,
            pdf.trailer(),
            &selected_id,
            &final_xref_offsets,
            &root_ref,
            xref_object_number,
            object_count,
            pdf.previous_xref_offset(),
        )?;
        bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
        xref_offset
    }
};
```

Remove the common post-match `write_incremental_trailer` call. Update that
function to accept `selected_id: &Object`, insert a clone of it, and remove
its `options` parameter. It remains the classic-table-only trailer writer.

- [ ] **Step 4: Update the direct helper test's new argument**

Pass the test trailer's existing `/ID` value as `selected_id` in
`write_incremental_xref_stream_emits_qpdf_compact_id_shape`; retain its compact
serialization assertions.

- [ ] **Step 5: Run the focused tests to verify GREEN**

Run:

```bash
cargo fmt --all
cargo test -p flpdf --test writer_tests incremental_xref_stream_static_id_is_reader_visible -- --exact
cargo test -p flpdf --lib writer::tests::write_incremental_xref_stream_emits_qpdf_compact_id_shape
```

Expected: all commands pass; the reopened output's `startxref` resolves to the
xref stream carrying the static pi ID, and no trailing classic trailer is
present.

- [ ] **Step 6: Commit the implementation**

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/tests/writer_tests.rs
git commit -m "fix: cut incremental xref streams over to selected ID"
```

### Task 3: Verify compatibility boundaries and finish the branch

**Files:**
- Verify: `crates/flpdf/src/writer.rs`
- Verify: `crates/flpdf/tests/xref_tests.rs`
- Verify: `crates/flpdf/tests/check_tests.rs`
- Verify: `crates/flpdf/tests/reader_tests.rs`
- Verify: `crates/flpdf/tests/writer_tests.rs`

**Interfaces:**
- Consumes: the completed stream cutover and the existing table-form path.
- Produces: test evidence that the stream route is qpdf-shaped and the table route remains compatible.

- [ ] **Step 1: Run the writer and xref test suites**

```bash
cargo test -p flpdf --test writer_tests
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test check_tests
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --lib
```

Expected: every test passes, including existing classic-table incremental
tests and the new stream regression.

- [ ] **Step 2: Run formatting and patch hygiene checks**

```bash
cargo fmt --all -- --check
git diff --check
```

Expected: both commands exit successfully with no formatting or whitespace
diagnostics.

- [ ] **Step 3: Validate representative outputs with qpdf and flpdf**

Use the existing compatibility fixture and generated test output to confirm
both readers see the xref-stream update and its `/ID`; run the repository's
qpdf compatibility test when qpdf is installed:

```bash
cargo test -p flpdf-cli --test compat_matrix_tests
cargo run --bin flpdf -- --check tests/fixtures/compat/three-page-objstm.pdf
qpdf --check tests/fixtures/compat/three-page-objstm.pdf
```

Expected: the compatibility test passes (or reports only its documented skip
when qpdf is unavailable), and both check commands accept the fixture.

- [ ] **Step 4: Review the final diff and confirm the intended file set**

```bash
git status --short
git diff --check
git diff --stat
```

Confirm the diff contains only the intended writer change and focused
regression coverage; the already-committed spec and plan must not be modified
by the implementation. If the status is clean apart from those intended
commits, no additional commit is needed.
