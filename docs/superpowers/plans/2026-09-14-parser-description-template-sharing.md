# Parser Description Template Sharing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make qpdf parser description template ownership parse-call-shared so the template payload does not scale with the number of parsed values.

**Architecture:** Preserve the existing ObjectDescription enum and qpdf warning/rendering behavior, but store the template variant as Rc<Vec<u8>>. The parser/resolver boundary creates one Rc per parse call and passes clones of that owner through HandleResolver; programmatic callers keep independent template owners. JSON and child descriptions remain boxed and unchanged.

**Tech Stack:** Rust workspace, Rc, canonical ObjectHandle parser/resolver, Rust unit/integration tests, cargo llvm-cov, heaptrack, and scripts/perf-matrix.py against qpdf 11.9.0.

**Spec:** docs/superpowers/specs/2026-09-14-parser-description-template-sharing-design.md

## Global Constraints

- qpdf 11.9.0 source at commit 3b97c9bd266b7c32ea36d3536e22dab77412886d is the semantic oracle.
- qpdf's one QPDFParser::description shared pointer per parse call remains the ownership model (libqpdf/qpdf/QPDFParser.hh:14-27,78; libqpdf/QPDFParser.cc:394-444).
- Preserve $PO, $OG, $VD, non-UTF-8 bytes, parsed-offset set-once behavior, warning context, JSON/Child descriptions, aliasing, replacement, disconnect, streams, encryption, and output bytes.
- Do not modify StateOwners, containment, source extents, xref maps, writer streaming, linearization, legacy Object, or materialization routes.
- A test must fail for the intended missing sharing behavior before production implementation is written.
- Use cargo fmt --all -- --check, workspace all-features tests, all-features clippy with repository allowances, strict private rustdoc, qpdf checkers, fresh patch coverage, and the same qpdf/flpdf performance matrix before PR creation.

---

### Task 1: Add the RED shared-owner regression test

**Files:**
- Modify: crates/flpdf/src/object_handle.rs:20034-20128 in the existing description unit-test module.

**Interfaces:**
- The test will use the existing ObjectHandle::parse_with_description, try_get_array_item, and try_get_key APIs.
- It will call a test-only description_template_owner observation method that does not exist yet; the initial run must fail because the current Vec<u8> implementation has no shared-owner API.

- [ ] **Step 1: Write the failing test**

Add this test beside the existing description-template tests:

~~~rust
#[test]
fn parser_values_share_one_description_template_owner() {
    let parsed = ObjectHandle::parse_with_description(
        b"[1 [2] << /K 3 >>]",
        "shared template",
    )
    .unwrap();
    let scalar = parsed.try_get_array_item(0).unwrap();
    let nested_scalar = parsed
        .try_get_array_item(1)
        .unwrap()
        .try_get_array_item(0)
        .unwrap();
    let dictionary_value = parsed
        .try_get_array_item(2)
        .unwrap()
        .try_get_key(b"/K")
        .unwrap();

    let first = scalar.description_template_owner().unwrap();
    let second = nested_scalar.description_template_owner().unwrap();
    let third = dictionary_value.description_template_owner().unwrap();
    assert!(std::rc::Rc::ptr_eq(&first, &second));
    assert!(std::rc::Rc::ptr_eq(&first, &third));
    assert_eq!(scalar.description(), b"parsed object, shared template at offset 1");
    assert_eq!(nested_scalar.description(), b"parsed object, shared template at offset 4");
    assert_eq!(dictionary_value.description(), b"parsed object, shared template at offset 13");
}
~~~

- [ ] **Step 2: Run the RED test**

Run:

~~~bash
cargo test -p flpdf --lib parser_values_share_one_description_template_owner --quiet
~~~

Expected result: compilation fails with a missing description_template_owner
method. This confirms the test is probing the new ownership contract rather
than passing against the existing copied Vec<u8> representation.

- [ ] **Step 3: Commit the RED test**

~~~bash
git add crates/flpdf/src/object_handle.rs
git commit -m "test(object-handle): require shared parser descriptions"
~~~

### Task 2: Make the value description template a shared owner

**Files:**
- Modify: crates/flpdf/src/object_handle.rs:1114-1126,1421-1505,3107-3250.
- Test: crates/flpdf/src/object_handle.rs:20034-20145.

**Interfaces:**
- Change ObjectDescription::Template(Vec<u8>) to Template(Rc<Vec<u8>>), keeping Json and Child unchanged.
- Change ObjectHandle::description_template to return Option<Rc<Vec<u8>>>.
- Add ObjectHandle::set_shared_description(&self, template: Rc<Vec<u8>>, offset: i64) for parser-owned transfer.
- Keep set_description(&self, description: impl AsRef<[u8]>, offset: i64) as the independent-owner wrapper.
- Add #[cfg(test)] fn description_template_owner(&self) -> Option<Rc<Vec<u8>>> used only by the RED/GREEN identity test.

- [ ] **Step 1: Implement the minimal value-layer change**

Use the following shape:

~~~rust
pub(crate) enum ObjectDescription {
    Template(Rc<Vec<u8>>),
    Json(Box<JsonDescription>),
    Child(Box<ChildDescription>),
}

pub(crate) fn set_shared_description(&self, template: Rc<Vec<u8>>, offset: i64) {
    let shared = self.0.borrow().shared.clone();
    let mut shared = shared.borrow_mut();
    shared.description = Some(ObjectDescription::Template(template));
    if shared.parsed_offset < 0 {
        shared.parsed_offset = offset;
    }
}

pub(crate) fn set_description(&self, description: impl AsRef<[u8]>, offset: i64) {
    self.set_shared_description(Rc::new(description.as_ref().to_vec()), offset);
}
~~~

Update expand_description_template to accept &[u8], update the match arm
to pass tmpl.as_slice(), return Rc::clone(template) from
description_template, and wrap set_object_description input in one Rc.
Do not alter marker replacement or parsed-offset logic.

- [ ] **Step 2: Run the focused GREEN test**

Run:

~~~bash
cargo test -p flpdf --lib parser_values_share_one_description_template_owner --quiet
~~~

Expected result: the test still reports compile errors from parser/resolver
callers that return Option<Vec<u8>>; those callers are the next task and no
production caller should be bypassed with a conversion back to copied bytes.

- [ ] **Step 3: Commit the value-layer change**

~~~bash
git add crates/flpdf/src/object_handle.rs
git commit -m "perf(object-handle): share description template owners"
~~~

### Task 3: Thread the shared owner through every parser boundary

**Files:**
- Modify: crates/flpdf/src/parser.rs:17-64,170-186,1992-2027.
- Modify: crates/flpdf/src/reader/resolver.rs:1285-1338,2491-2678,3600-3780,3876-3975,4983-5005.
- Test: parser and resolver description tests in crates/flpdf/src/object_handle.rs and crates/flpdf/src/reader/resolver.rs.

**Interfaces:**
- HandleResolver::description_template returns Option<Rc<Vec<u8>>>.
- HandleResolver::direct_handle_at calls set_shared_description.
- DetachedHandles, OffsetHandleResolver, and ChildHandles forward/clone the Rc without cloning its bytes.
- read_stream accepts Rc<Vec<u8>> and calls set_shared_description when reattaching the stream dictionary description.

- [ ] **Step 1: Update the parser trait and detached/offset adapters**

Make the parser contract explicit:

~~~rust
fn direct_handle_at(&mut self, value: ObjectValue, offset: i64) -> ObjectHandle {
    let handle = self.direct_handle(value);
    if let Some(description) = self.description_template() {
        handle.set_shared_description(description, offset);
    } else {
        handle.set_parsed_offset_if_unset(offset);
    }
    handle
}

fn description_template(&self) -> Option<Rc<Vec<u8>>>;
~~~

parse_explicit_object_handle_with_description creates one
Rc::new(format!(...).into_bytes()); DetachedHandles returns an Rc clone;
OffsetHandleResolver forwards that clone.

- [ ] **Step 2: Update live resolver parser ownership**

Change ChildHandles.description_template to Rc<Vec<u8>> and return
Some(Rc::clone(&self.description_template)). At every constructor, build one
Rc for the parse invocation and clone it into the adapter. In the object-stream
member loop, keep the Rc in a local so the resolved member handle can receive
the same owner after parsing. In read_object_at_offset_with_description, use
the parser's Rc directly and fall back to Rc::new(parsed.value.description())
only for the existing no-template branch.

Change read_stream from dict_description: Vec<u8> to
dict_description: Rc<Vec<u8>> and use dict.set_shared_description at its
existing reattachment point. This avoids introducing a one-stream byte copy
at the top-level boundary.

- [ ] **Step 3: Run parser/resolver and description tests**

Run:

~~~bash
cargo test -p flpdf --lib parser_values_share_one_description_template_owner --quiet
cargo test -p flpdf --lib object_description_template --quiet
cargo test -p flpdf --lib reader::resolver::tests --quiet
~~~

Expected result: all shared-owner and existing description/resolve tests pass,
including explicit contextless parsing, ObjStm parsing, stream descriptions,
JSON/Child descriptions, and warning-context assertions.

- [ ] **Step 4: Commit the parser/resolver cutover**

~~~bash
git add crates/flpdf/src/parser.rs crates/flpdf/src/reader/resolver.rs crates/flpdf/src/object_handle.rs
git commit -m "perf(parser): share description templates across parsed values"
~~~

### Task 4: Add allocation-slope evidence and update correspondence docs

**Files:**
- Modify: crates/flpdf/tests/object_handle_shared_value_allocation_tests.rs.
- Modify: docs/qpdf-correspondence.md at the QPDFObject.cc / QPDFValue.cc row.
- Modify: crates/flpdf/src/object_handle.rs module documentation if the ownership mapping needs a source-near note.

**Interfaces:**
- Reuse the existing measuring-thread TLS allocator; do not add a second global allocator or a benchmark-only value representation.
- Use a real parser-created value graph so the allocation result includes SharedValueState and description ownership.

- [ ] **Step 1: Add the allocation slope test**

Add a test that constructs two parsed arrays with the same description and
different child counts inside measure_construction, retains both graphs until
the measurement is read, and asserts that the larger graph does not allocate a
description byte buffer for every child. Report allocations, allocated_bytes,
and live_bytes using report_measurement. Keep direct-scalar thresholds and
the existing wide constructor tests unchanged.

- [ ] **Step 2: Run the allocation tests**

~~~bash
cargo test -p flpdf --test object_handle_shared_value_allocation_tests -- --nocapture
~~~

Expected result: the new parser graph test passes and prints reproducible
allocation evidence; the existing direct scalar, wide array, dictionary,
stream, and alias tests remain green.

- [ ] **Step 3: Update the correspondence row**

Add a concise source-backed sentence to the QPDFObject.cc / QPDFValue.cc
row stating that qpdf's parser-owned Description is shared per parse call and
flpdf's Rc<Vec<u8>> is the ownership substitution; explicitly state that
JSON/Child variants remain distinct and that no observable warning/output
contract changes.

- [ ] **Step 4: Commit evidence/docs**

~~~bash
git add crates/flpdf/tests/object_handle_shared_value_allocation_tests.rs docs/qpdf-correspondence.md crates/flpdf/src/object_handle.rs
git commit -m "test(perf): measure shared parser description ownership"
~~~

### Task 5: Run full verification and the qpdf/flpdf memory matrix

**Files:**
- Read-only verification of the committed worktree.

- [ ] **Step 1: Run formatting and focused checks**

~~~bash
cargo fmt --all -- --check
cargo test -p flpdf --lib object_handle:: --quiet
cargo test -p flpdf --lib parser:: --quiet
cargo test -p flpdf --test object_handle_shared_value_allocation_tests -- --nocapture
~~~

- [ ] **Step 2: Run repository quality gates**

~~~bash
cargo test --workspace --all-features --quiet
cargo clippy --workspace --all-targets --all-features -- -D warnings -A clippy::chunks-exact-to-as-chunks -A clippy::useless-format
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
python3 scripts/check-qpdf-deviation-markers.py --check
python3 scripts/check-qpdf-route-matrix.py --check
scripts/patch-coverage.sh --base origin/main
~~~

Record every exit status and the fresh changed-line coverage result.

- [ ] **Step 3: Re-run the controlled performance matrix**

Use the established harness with identical qpdf/flpdf inputs, release builds,
the pages/content/objects 1000/5000 cases, stream-32 MiB uncompress case, five
steady samples, and heaptrack attribution. Invoke the non-executable harness
as python3 scripts/perf-matrix.py. Report qpdf/flpdf wall time, maximum RSS,
allocation count, allocated bytes, and peak live heap before/after. Validate
output bytes and exit status for every case; keep known zlib/output exceptions
separate from memory conclusions.

- [ ] **Step 4: Commit any final formatting-only correction, inspect status, and prepare the PR**

~~~bash
git status --short --branch
git diff --check origin/main...HEAD
git log --oneline --decorate -8
~~~

Create a Draft stacked PR with base main, reference flpdf-ymuj.7, attach
the fresh evidence, wait for all required CI checks, then mark it Ready. Do not
merge; the integration session owns merge.
