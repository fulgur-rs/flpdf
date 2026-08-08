# QPDFParser Live Description Stamping Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Make every non-null direct value produced by the live file-object parser carry qpdf 11.9.0's per-parse description template and owning document context, including nested arrays, dictionaries, and scalars.

**Architecture:** `HandleResolver` supplies one optional description template for each parse call. `LiveFileParser` snapshots that template once and applies it together with the parser offset at the existing direct-handle construction seam; parsed `null` remains the undescribed shared-null equivalent. The canonical resolver builds the template from its input-source description and expected object reference, and preserves the top-level parser description when it transfers the parsed value into the canonical indirect handle. Explicit parsing supplies qpdf's `parsed object` template but remains contextless; ObjStm legacy materialization remains undescribed because it does not retain parser handles.

**Tech Stack:** Rust workspace, `cargo test`, `cargo fmt`, qpdf 11.9.0 pinned source and `/usr/bin/qpdf` behavior, Beads.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are the semantic oracle.
- Stamp non-null parser-created values only; parsed `null` stays without a description or parsed offset.
- Preserve qpdf's set-once parsed-offset behavior and existing array/dictionary render shifts.
- Keep explicit parsing contextless and do not deepen `ObjectHandle::context()` containment traversal.
- Use RED→GREEN TDD and run the relevant focused tests before broader verification.

---

### Task 1: Add RED coverage for parser descriptions

**Files:**

- Modify: `crates/flpdf/src/reader/resolver.rs` tests for the canonical top-level handle and deeply nested direct values.
- Modify: `crates/flpdf/src/object_handle.rs` tests for explicit-parse stamping and contextless warning behavior.

**Interfaces:**

- Consumes: existing `ObjectHandle::description`, `get_parsed_offset`, `parse_explicit_object_handle`, and canonical `ResolverHandle` test helpers.
- Produces: failing tests that require the exact qpdf template `input.pdf, object 1 0 at offset $PO`, the dictionary/array offset shifts, scalar offsets, and contextless explicit parse descriptions.

- [x] **Step 1: Write the canonical-resolver RED test.**

Use a named resolver over `\n1 0 obj\n<< /L1 << /L2 << /Value 7 >> >> >>\nendobj\n` with the xref offset set to `1`, resolve object `1 0` through `get_object_handle`, and assert the canonical root description is `input.pdf, object 1 0 at offset 11`. Assert the L1 dictionary, L2 dictionary, and scalar descriptions use the same template with offsets `18`, `25`, and `33`; assert both nested dictionaries and the scalar still emit through the owning resolver.

- [x] **Step 2: Write the explicit-parse RED test.**

Parse `<< /Value 7 >>` through `ObjectHandle::parse`, assert the direct child description is qpdf's contextless template `parsed object,  at offset 10`, and assert `object_warning` returns `parsed object,  at offset 10: contextless explicit parse` without routing through a document sink, matching `QPDFExc::createWhat` with a non-empty object description and empty filename.

- [x] **Step 3: Run only the new tests and verify the failure is the missing description.**

Run:

```bash
cargo test -p flpdf reader::resolver::tests::canonical_live_parser_stamps_root_and_nested_descriptions -- --exact
cargo test -p flpdf object_handle::parse_tests::parse_without_context_stamps_the_qpdf_parsed_object_description -- --exact
```

Expected result: the tests fail because parser-created handles currently have empty descriptions, while existing context and parsed-offset tests remain unrelated and passing.

- [x] **Step 4: Commit the RED tests.**

```bash
git add crates/flpdf/src/reader/resolver.rs crates/flpdf/src/object_handle.rs
git commit -m "test: pin qpdf parser object descriptions"
```

### Task 2: Implement one parse-call template and preserve the root metadata

**Files:**

- Modify: `crates/flpdf/src/parser.rs` in `HandleResolver`, `DetachedHandles`, `parse_live_file_object_with_context`, `LiveFileParser`, and `direct_at`.
- Modify: `crates/flpdf/src/reader/resolver.rs` in `ChildHandles`, the object-description builder, and the resolution transfer path.
- Modify: `crates/flpdf/src/object_handle.rs` only if a private metadata-preserving transfer helper is needed for the canonical root.

**Interfaces:**

- Consumes: the RED tests from Task 1 and the existing resolver-bearing direct-handle primitive from `flpdf-25kg.3.33`.
- Produces: `HandleResolver::description_template() -> Option<String>`, parser stamping at one construction seam, and a resolver path that transfers the root description into the canonical indirect slot without changing public APIs.

- [x] **Step 1: Add the optional template seam with contextless defaults.**

Add a default `description_template` method to `HandleResolver` returning `None`. Make `DetachedHandles` carry an optional template: ObjStm materialization constructs it with `None`, while `parse_explicit_object_handle` constructs it with `Some("parsed object,  at offset $PO".to_owned())`. Existing test resolvers keep the default unless a test adapter overrides it.

- [x] **Step 2: Snapshot the template once per parser invocation.**

In `parse_live_file_object_with_context`, call `resolver.description_template()` before building `LiveFileParser`, store the returned `Option<String>` on the parser, and never rebuild it per token or per nesting level.

- [x] **Step 3: Stamp every non-null direct parser value.**

In `LiveFileParser::direct_at`, construct the handle through `resolver.direct_handle(value)` as today. If a template exists, call `handle.set_description(template.clone(), offset)`; otherwise call `set_parsed_offset_if_unset(offset)`. Leave the `TokenType::Null`, recovery nulls, and `indirect_handle` paths untouched. Keep `finish_array` and `finish_dictionary` offsets unchanged because their existing `start` values are qpdf's `frame.offset - 1` and `frame.offset - 2` pre-shifted coordinates, and `ObjectSlot::get_description` already applies the corresponding render shift.

- [x] **Step 4: Build the canonical resolver template from qpdf's input/object description.**

Add a `ResolverHandle` helper that snapshots `ResolverCore::description` and returns qpdf's unconditional constructor template:

```rust
format!(
    "{}, object {} {} at offset $PO",
    input_description, object_ref.number, object_ref.generation
)
```

Construct `ChildHandles` with that one string before invoking `parse_live_file_object_with_decrypter`; its `description_template` returns a clone of the stored string. This mirrors `QPDF::setLastObjectDescription` plus `QPDFParser`'s constructor without reusing the warning-only source description route for object warnings.

- [x] **Step 5: Preserve the top-level parser description during canonical resolution.**

Add a private resolver-only parse result path that returns the top-level rendered description in addition to `ObjectValue` and parsed offset. Keep the existing two-value `read_object_at_offset` wrapper for tests and non-canonical callers. In `resolve_indirect` and its xref-recovery retry, after `handle.set_resolved(value)` and `set_parsed_offset_if_unset(parsed_offset)`, apply the returned non-empty description to the same canonical handle with `set_description`. Do not transfer the parser dictionary description through `read_stream`; stream objects have qpdf's separate stream-description responsibility.

- [x] **Step 6: Run the RED tests GREEN, then refactor only if all remain green.**

Run the three exact test commands from Task 1. Then run:

```bash
cargo test -p flpdf --lib parser::live_input_tests
cargo test -p flpdf --lib reader::resolver::tests
```

- [ ] **Step 7: Commit the minimal implementation.**

```bash
git add crates/flpdf/src/parser.rs crates/flpdf/src/reader/resolver.rs crates/flpdf/src/object_handle.rs
git commit -m "feat: stamp qpdf parser object descriptions"
```

### Task 3: Complete qpdf-focused verification

**Files:**

- Inspect: `docs/qpdf-correspondence.md` and the pinned qpdf source citations.
- Modify: only source-near docs or tests if verification exposes a factual citation or regression gap.

**Interfaces:**

- Consumes: the implementation and focused tests from Task 2.
- Produces: verified qpdf parity evidence, clean formatting, and a Beads/Git handoff ready for review.

- [x] **Step 1: Run focused parser and object-description regressions.**

```bash
cargo test -p flpdf --lib parser::tests
cargo test -p flpdf --lib object_handle::parse_tests
cargo test -p flpdf --lib reader::resolver::tests
```

- [x] **Step 2: Run formatting and workspace quality gates.**

```bash
cargo fmt --all -- --check
cargo test -p flpdf
cargo test -p flpdf-cli --test cli_tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 3: Check changed-line coverage and inspect the final diff.**

Run `git diff --check`, `git status --short`, and the repository patch-coverage command against the branch base. Confirm every changed executable line is covered by the new or existing parser/resolver tests and that no containment traversal or unrelated API compatibility code was added.

- [ ] **Step 4: Persist Beads state and report the branch.**

Run `bd dolt push`, read back `bd show flpdf-ryt6`, and report the worktree path, branch, commits, tests, and any remaining integration/merge action. Do not close the issue until implementation and verification are complete.
