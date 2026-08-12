# qpdf newStream Owned Factory Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans (recommended). Steps use checkbox syntax for tracking.

**Goal:** Add a qpdf-faithful Pdf-owned empty stream factory and buffer convenience without cloning the stream object or replacing qpdf's no-data state with an empty buffer.

**Architecture:** Construct one resolver-associated ObjectValue::Stream, set its parsed offset to zero, and promote that same ObjectHandle through the existing canonical make_indirect_from_object_handle primitive. The buffer convenience delegates to that factory and the existing replace_stream_data boundary; the legacy cloning allocator remains untouched for its later consumer migration.

**Tech Stack:** Rust 2021, Rc<Vec<u8>>, ObjectHandle/resolver, pinned qpdf 11.9.0 source, Cargo tests, qpdf compatibility writer tests, and repository coverage gates.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are the semantic oracle.
- Never use Pdf::make_indirect_object_handle or ObjectHandle::stream to implement the new factory.
- Never use Some(Rc::new(Vec::new())) as the original no-data state; use stream_data: None and parsed offset 0.
- Preserve the existing resolver canonical allocation and shared-state semantics; do not change legacy consumers in this issue.
- No provider, filterable, pipeline, retry, Filespec, or direct-stream API expansion.
- Use RED-to-GREEN TDD and run changed-line coverage before the PR.

---

### Task 1: Commit the approved design

**Files:**

- Create: docs/superpowers/specs/2026-08-12-qpdf-new-stream-design.md
- Create: docs/superpowers/plans/2026-08-12-qpdf-new-stream.md

**Interfaces:**

- Consumes: flpdf-25kg.3.21, pinned qpdf source, and the existing canonical allocation primitive.
- Produces: the design and plan that constrain implementation.

- [x] Step 1: Record qpdf authority and chosen flpdf path

The spec records QPDF.hh:319-340, QPDF.cc:1912-1931, QPDF_Stream.cc:109-137, the no-data pipe branch, the shared-state promotion route, and the legacy scope exclusion.

- [ ] Step 2: Commit the design files

    git add docs/superpowers/specs/2026-08-12-qpdf-new-stream-design.md docs/superpowers/plans/2026-08-12-qpdf-new-stream.md
    git commit -m "docs: design qpdf new stream factory"

### Task 2: Add failing factory and boundary tests

**Files:**

- Modify: crates/flpdf/src/reader.rs in the existing unit-test module near canonical allocation tests.

**Interfaces:**

- Consumes: Pdf::open, ObjectHandle stream accessors, ObjectRef, minimal_pdf_bytes, and PdfWriter.
- Produces: failing tests for Pdf::new_stream() and Pdf::new_stream_with_data(Rc<Vec<u8>>).

- [ ] Step 1: Write the empty factory identity test

Open minimal_pdf_bytes(), call pdf.new_stream(), and assert that the result is indirect, uses ObjectRef::new(4, 0), has parsed offset 0, has as_stream_data() == None, has an empty stream dictionary, and is the canonical resolver handle. Clone the dictionary, insert /Marker, and observe the mutation through the stream handle.

- [ ] Step 2: Write the no-data failure test

Call get_raw_stream_data() before replacement and assert the exact Error::Internal message pipeStreamData called for stream with no data.

- [ ] Step 3: Write allocation tests

Call new_stream() twice and assert distinct generation-zero references and distinct identities. Register ObjectRef::new(i32::MAX as u32, 0) through the canonical resolver, call new_stream(), and assert the exact qpdf boundary message max object id is too high to create new objects.

- [ ] Step 4: Write the buffer boundary test

Pass one shared Rc<Vec<u8>> to new_stream_with_data, assert Rc::ptr_eq through as_stream_data(), and assert the exact positive /Length. Repeat with an empty buffer and assert that /Length is absent.

- [ ] Step 5: Run the focused tests and verify RED

    cargo test -p flpdf --lib reader::tests::new_stream

Expected: failure because the two methods do not exist. Do not add production code until this missing-API failure is observed.

### Task 3: Implement the qpdf-shaped factory

**Files:**

- Modify: crates/flpdf/src/reader.rs in impl<R: Read + Seek> Pdf<R> beside the canonical allocation primitive.

**Interfaces:**

- Consumes: ResolverHandle::direct_object_handle, ObjectValue::Stream, ObjectHandle::dictionary, set_parsed_offset_if_unset, make_indirect_from_object_handle, and replace_stream_data.
- Produces: pub fn new_stream(&self) -> Result<ObjectHandle> and pub fn new_stream_with_data(&self, data: Rc<Vec<u8>>) -> Result<ObjectHandle>.

- [ ] Step 1: Implement the empty factory

Construct a resolver-associated ObjectValue::Stream with an empty dictionary, stream_data: None, and stream_length: 0. Set parsed offset 0, then call self.make_indirect_from_object_handle(stream). Add qpdf citations to the method documentation. Do not call the legacy allocator, clone a value, insert /Length, or create an empty replacement buffer.

- [ ] Step 2: Implement the buffer convenience

Call self.new_stream()?, then replace_stream_data(data, None, None), and return the same handle. This preserves the exact Rc and delegates the zero/nonzero /Length boundary.

- [ ] Step 3: Run the focused tests and verify GREEN

    cargo test -p flpdf --lib reader::tests::new_stream
    cargo test -p flpdf --lib reader::tests::make_indirect_from_object_handle

Expected: all new and existing canonical allocation tests pass.

### Task 4: Verify lifecycle and writer reachability

**Files:**

- Modify: crates/flpdf/src/reader.rs in tests only.

**Interfaces:**

- Consumes: the new factory, root canonical handles, and PdfWriter.
- Produces: lifecycle and reachable-output regression coverage.

- [ ] Step 1: Test owner drop

Clone the returned stream, drop the owning Pdf, and assert the surviving handle follows the existing qpdf-shaped teardown state. Do not add a new drop representation.

- [ ] Step 2: Test reachable full rewrite

Make a data-bearing new stream reachable from the catalog/root through the existing canonical mutation path, write a full rewrite, reopen it, and assert the reachable stream's payload and /Length. Do not require an unattached object to be emitted by the default writer.

- [ ] Step 3: Run focused writer tests

    cargo test -p flpdf --lib reader::tests::new_stream
    cargo test -p flpdf --test writer_tests

If this exposes a pre-existing writer consumer gap, record a dependent Beads issue instead of adding a legacy bridge here.

### Task 5: Verify and publish the stacked PR

**Files:**

- Verify all changed files; no unrelated production scope is allowed.

- [ ] Step 1: Run quality gates

    cargo fmt --all -- --check
    RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test -p flpdf
    cargo test

- [ ] Step 2: Run changed-line coverage

Run the repository's configured cargo llvm-cov and scripts/patch-coverage.sh commands against the branch's actual parent. Confirm every changed executable line is covered.

- [ ] Step 3: Inspect scope and Beads

    git diff --check
    git status --short
    bd dep cycles
    bd show flpdf-25kg.3.21

Confirm no legacy allocator migration, provider implementation, empty-buffer sentinel, or unrelated files entered the diff.

- [ ] Step 4: Commit, push, and open the next stack layer

    git add crates/flpdf/src/reader.rs docs/superpowers/specs/2026-08-12-qpdf-new-stream-design.md docs/superpowers/plans/2026-08-12-qpdf-new-stream.md
    git commit -m "feat(reader): add qpdf new stream factory"
    git push -u origin feature/flpdf-25kg-3-21-new-stream

Create the PR against the current flpdf-25kg.3.6.3.1 stack branch. Keep the next PR based on this PR and stop if the repository reaches five open PRs.
