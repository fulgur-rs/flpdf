# flpdf-p1t9 Bootstrap ObjStm Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Make the xref bootstrap resolver resolve type-2 entries through qpdf-compatible object streams, including indirect /Prev and /XRefStm values.

**Architecture:** Extend the existing XrefReadContext only. It will own the bootstrap-local object-stream resolution set, decode an ObjStm with the existing xref-context filter engine, parse all applicable members with the existing qpdf direct-object parser, and populate the existing bootstrap object cache. The later canonical resolver and legacy Pdf ObjStm route remain outside this change.

**Tech Stack:** Rust workspace, crates/flpdf/src/xref.rs, qpdf 11.9.0 source oracle, cargo test, cargo fmt, cargo clippy.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are authoritative.
- Keep ObjStm resolution bootstrap-local; do not call Pdf::resolve, ref_chain, the legacy ObjStm route, or the post-bootstrap canonical resolver.
- Preserve qpdf's resolved_object_streams once-only behavior and cache all applicable members while skipping xref-overridden members.
- Do not scan ObjStm contents during reconstruction recovery.
- Use RED to GREEN TDD and keep existing main unchanged by working in the dedicated worktree.

---

### Task 1: Lock the missing type-2 behavior with bootstrap unit tests

**Files:**
- Modify: crates/flpdf/src/xref.rs in the cfg(test) module near the existing bootstrap resolver tests.

**Interfaces:**
- Consumes: XrefReadContext, XrefRegistration, XrefEntry, Object, ObjectRef, and the existing synthetic PDF/object-stream test patterns.
- Produces: failing tests for one valid ObjStm with multiple members, overridden members, malformed ObjStm metadata, and indirect /Prev and /XRefStm references.

- [ ] Step 1: Write the failing tests

Add tests with these exact behaviors:

~~~rust
#[test]
fn bootstrap_context_resolves_type2_members_and_caches_all_applicable_members() {
    // Build one uncompressed ObjStm whose header names objects 2 and 4,
    // register both as type-2 entries, and assert that resolving either
    // member returns its direct object rather than Null.
}

#[test]
fn bootstrap_context_does_not_replace_an_overridden_objstm_member() {
    // Register object 2 as an uncompressed object while the ObjStm header also
    // names object 2; assert resolution reads the uncompressed object and the
    // ObjStm member is not cached over it.
}

#[test]
fn bootstrap_context_resolves_indirect_prev_through_objstm() {
    // Put the previous xref offset in a type-2 member, run the existing
    // previous-section merge path, and assert the previous section is read.
}

#[test]
fn bootstrap_context_resolves_indirect_xrefstm_through_objstm() {
    // Put the hybrid xref-stream offset in a type-2 member, run the existing
    // classic/hybrid merge path, and assert the hybrid stream is registered.
}

#[test]
fn bootstrap_context_turns_malformed_objstm_metadata_into_null_with_warning() {
    // Use a non-integer /N or /First, resolve a compressed member, and
    // assert qpdf-style warning/null fallback rather than a panic or bridge.
}
~~~

- [ ] Step 2: Run the focused tests to verify RED

Run:

~~~bash
cargo test -p flpdf --lib xref::tests::bootstrap_context_resolves_type2_members_and_caches_all_applicable_members -- --exact
cargo test -p flpdf --lib xref::tests::bootstrap_context_resolves_indirect_prev_through_objstm -- --exact
cargo test -p flpdf --lib xref::tests::bootstrap_context_resolves_indirect_xrefstm_through_objstm -- --exact
~~~

Expected: the new tests fail because XrefReadContext::resolve_reference currently maps every XrefEntry::Compressed to Object::Null; the existing missing/free test remains green.

---

### Task 2: Implement bootstrap-local qpdf resolveObjectsInStream

**Files:**
- Modify: crates/flpdf/src/xref.rs imports, shared bootstrap-cache state, XrefObjectCache, XrefReadContext, and resolve_reference.

**Interfaces:**
- Consumes: filters::decode_stream_data_from_xref_context, parser::parse_qpdf_direct_object, Stream, and the active/reconstruction XrefEntryLookup.
- Produces: XrefReadContext::resolve_object_stream(stream_number) -> Result<()>, type-2 dispatch, shared once-only ObjStm state, and qpdf-style warning/null fallback.

- [ ] Step 1: Add shared once-only ObjStm state

Extend the existing shared bootstrap cache from only BTreeMap<ObjectRef, Object> to a small state containing that object map plus BTreeSet<u32> of resolved object-stream numbers. Keep the public LoadedXrefState::bootstrap_cache ownership boundary unchanged, update cache merge to merge both members, and preserve source-cache precedence for object values.

- [ ] Step 2: Implement resolve_object_stream

Implement this qpdf order in XrefReadContext:

1. Insert the stream number into the shared resolved set and return if already present.
2. Resolve ObjectRef::new(stream_number, 0) through the same context and require Object::Stream.
3. Resolve /Type, warn if it is not /ObjStm but continue, then require integer non-negative /N and /First values.
4. Decode the raw stream bytes with decode_stream_data_from_xref_context, resolving filter/decode-parameter references through self.resolve_value.
5. Read exactly /N object-number/offset header pairs with Tokenizer, keeping the last offset for duplicate object numbers.
6. Recheck each header object against the current visible xref lookup and only parse entries whose effective entry is Compressed in this same stream.
7. Parse each applicable member from decoded[first + offset..] with parse_qpdf_direct_object, record parser diagnostics with the object-stream context, and insert the member into the bootstrap object cache.

- [ ] Step 3: Dispatch type-2 references and preserve cycle/null behavior

Replace the Compressed => Null branch with resolve_object_stream. Catch its errors in the same qpdf resolve catch-and-null boundary used for uncompressed reads, preserve already-cached members, and let the existing outer cache check retain a member inserted during ObjStm resolution. Keep the existing resolving-set cycle guard and do not add a fallback to any later resolver.

- [ ] Step 4: Run the focused tests to verify GREEN

Run:

~~~bash
cargo test -p flpdf --lib xref::tests::bootstrap_context_resolves_type2_members_and_caches_all_applicable_members -- --exact
cargo test -p flpdf --lib xref::tests::bootstrap_context_does_not_replace_an_overridden_objstm_member -- --exact
cargo test -p flpdf --lib xref::tests::bootstrap_context_resolves_indirect_prev_through_objstm -- --exact
cargo test -p flpdf --lib xref::tests::bootstrap_context_resolves_indirect_xrefstm_through_objstm -- --exact
cargo test -p flpdf --lib xref::tests::bootstrap_context_turns_malformed_objstm_metadata_into_null_with_warning -- --exact
~~~

Expected: all new tests and the existing bootstrap missing/free/cycle tests pass.

---

### Task 3: Verify qpdf boundary and repository quality gates

**Files:**
- Modify only: files listed in Tasks 1 and 2.

**Interfaces:**
- Consumes: the GREEN bootstrap implementation and its regression tests.
- Produces: evidence that xref stream parsing, /Prev, hybrid /XRefStm, recovery boundaries, and the workspace remain green.

- [ ] Step 1: Run focused xref tests

~~~bash
cargo test -p flpdf --lib xref::tests
cargo test -p flpdf --test xref_tests
~~~

- [ ] Step 2: Run formatting and lint checks

~~~bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
~~~

- [ ] Step 3: Run the workspace tests and strict documentation check

~~~bash
cargo test --workspace
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
~~~

- [ ] Step 4: Inspect the final diff and Beads state

~~~bash
git diff --check
git status --short --branch
bd show flpdf-p1t9
bd dep cycles
~~~

Confirm no reconstruction ObjStm scan, legacy/canonical resolver bridge, unrelated file, or untracked test artifact was introduced.

