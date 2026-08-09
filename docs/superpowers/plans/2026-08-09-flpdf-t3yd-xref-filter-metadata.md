# Xref Bootstrap Filter Metadata Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make bootstrap xref-stream decoding resolve indirect `/Filter` and `/DecodeParms` metadata through `XrefReadContext`, matching qpdf 11.9.0 without connecting the post-bootstrap resolver.

**Architecture:** Keep `parse_xref_stream` as the canonical xref-stream entrypoint. Extend the existing `stream_filter` Object-shape reader with an optional object resolver so it can resolve only the values qpdf's filter path inspects; keep the existing direct reader behavior unchanged through an identity resolver. Add a filter decode entrypoint in `filters.rs` that uses the shared `decode_prepared_specs` engine, and pass `XrefReadContext::resolve_value` from `xref.rs`.

**Tech Stack:** Rust workspace, `flate2` synthetic PDF fixtures, qpdf 11.9.0 source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`, existing `xref_tests` and shared filter pipeline.

---

### Task 1: Add canonical xref-stream regressions

**Files:**
- Modify: `crates/flpdf/tests/xref_tests.rs`

- [x] **Step 1: Add the indirect `/Filter` regression fixture.**

Build a hybrid PDF in memory so the classic xref table registers the metadata holders before the `/XRefStm` stream is read. Store `/Filter` in object `10 0`, its array item in `20 0`, and `/FlateDecode` in object `20 0`. Compress one `[type, offset, generation]` row and assert that `load_xref_and_trailer` records the xref-stream entry instead of treating the unresolved `/Filter` reference as an invalid filter shape.

- [x] **Step 2: Add the indirect `/DecodeParms` regression fixture.**

Use a direct `/FlateDecode` and an indirect `/DecodeParms` dictionary whose `/Predictor`, `/Columns`, `/Colors`, and `/BitsPerComponent` values are themselves indirect objects. Encode the xref row with a PNG predictor prefix and assert that the decoded row is accepted and the expected live entry is present. This proves both the parameter-container and parameter-value dereference paths.

- [x] **Step 3: Add only fixture helpers needed by both tests.**

Reuse the existing xref row builder and add a small `flate_encode` helper using `flate2::write::ZlibEncoder` and `Compression::default()`. Keep all object offsets explicit and emit a complete classic xref table for every referenced holder.

- [x] **Step 4: Format the test file without changing production code.**

Run:

```bash
cargo fmt --all
```

Expected: exit 0; only `crates/flpdf/tests/xref_tests.rs` and the new plan file are changed in this worktree.

### Task 2: Verify the regressions are genuinely RED

**Files:**
- Test: `crates/flpdf/tests/xref_tests.rs`

- [x] **Step 1: Run the indirect-filter test.**

Run:

```bash
cargo test -p flpdf --test xref_tests indirect_xref_stream_filter_metadata
```

Expected: FAIL because the current `parse_xref_stream` passes the raw stream dictionary to `filters::decode_stream_data`, producing `stream filter type is not name or array`.

- [x] **Step 2: Run the indirect-DecodeParms test.**

Run:

```bash
cargo test -p flpdf --test xref_tests indirect_xref_stream_decode_parms
```

Expected: FAIL because the current Object-shape reader does not dereference the indirect `/DecodeParms` container or its predictor values, so the predicted row is parsed with the wrong decoded shape.

- [x] **Step 3: Record the exact failure causes before implementation.**

Do not alter the assertions to accept the existing error. If either test fails for fixture construction rather than unresolved metadata, correct only the fixture and repeat the command until the failure identifies the missing qpdf behavior.

### Task 3: Add resolver-aware filter-spec decoding

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs`
- Modify: `crates/flpdf/src/filters.rs`

- [x] **Step 1: Preserve the existing direct Object reader through an identity resolver.**

Refactor `decode_filter_specs_from_object` to delegate to a new resolver-aware helper. The direct entrypoint must pass a resolver that clones its input, so existing callers retain their current behavior and existing Object/Handle equivalence tests remain valid.

- [x] **Step 2: Resolve `/Filter` in qpdf order.**

In the resolver-aware helper, resolve the top-level `/Filter` before testing null/name/array. For an array, resolve each item before the existing name validation and chain-length checks. Do not resolve array children when the top-level value is not an array; this preserves qpdf's accessor order.

- [x] **Step 3: Resolve `/DecodeParms` in qpdf order.**

Resolve the top-level parameter value before testing null/array. Resolve each array item before pairing it with a filter. For a filter that reads dictionary entries (`FlateDecode`, `LZWDecode`, and the existing `Crypt` staging path), resolve dictionary values as they are reduced; leave non-consuming filter parameter values as the existing bounded Object snapshot. Preserve null omission, parameter alignment, and existing filter-chain limits.

- [x] **Step 4: Add a filters entrypoint that shares the existing decode engine.**

Add a `pub(crate)` xref-bootstrap decode function in `filters.rs` that accepts a mutable `FnMut(&Object) -> Object`, reads the stream dictionary's two metadata keys, obtains resolver-aware `FilterSpec` values, runs `decode_prepared_specs`, and replays the same strict outcome/error path as `decode_stream_data`. Do not duplicate codec or pipeline logic.

### Task 4: Connect `XrefReadContext` at the canonical route

**Files:**
- Modify: `crates/flpdf/src/xref.rs`

- [x] **Step 1: Replace only the raw-dictionary filter call.**

In `parse_xref_stream`, after `/Type`, `/Size`, `/W`, and `/Index` have been validated, call the new resolver-aware decode function with `&mut |value| context.resolve_value(value)`. Keep `stream.data` unchanged and keep registration, trailer, reconstruction-trigger, and diagnostic boundaries unchanged.

- [x] **Step 2: Preserve diagnostics and context ownership.**

Let `XrefReadContext` retain all reference-read, cycle, missing/free, and malformed-object diagnostics. Keep the existing `context.append_diagnostics_to` calls on both success and failure paths. Do not add `Pdf::resolve`, `ObjectHandle`, `ref_chain`, or type-2 ObjStm resolution.

- [x] **Step 3: Run the focused GREEN tests.**

Run:

```bash
cargo test -p flpdf --test xref_tests indirect_xref_stream_filter_metadata
cargo test -p flpdf --test xref_tests indirect_xref_stream_decode_parms
cargo test -p flpdf --lib xref::tests::bootstrap_context
cargo test -p flpdf --test xref_tests
```

Expected: both new regressions pass, the existing bootstrap unit tests pass, and the xref integration suite reports zero failures.

### Task 5: Verify against qpdf and repository gates

**Files:**
- Read: `/home/ubuntu/.cache/flpdf/qpdf-11.9.0/libqpdf/QPDF_Stream.cc:386-482`
- Read: `/home/ubuntu/.cache/flpdf/qpdf-11.9.0/libqpdf/SF_FlateLzwDecode.cc:21-72`
- Read: `/home/ubuntu/.cache/flpdf/qpdf-11.9.0/libqpdf/QPDF.cc:972-1052`
- Read: `docs/qpdf-correspondence.md` filter row

- [x] **Step 1: Run the real hybrid indirect-metadata probe.**

Run qpdf 11.9.0 and flpdf `--check` against the same synthetic shape. Expected qpdf exit 0 with no syntax or stream-encoding error; flpdf must now exit 0 instead of `stream filter type is not name or array`.

- [x] **Step 2: Run formatting and lint gates.**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: both exit 0 with no warnings promoted to errors.

- [x] **Step 3: Run the full workspace verification.**

Run:

```bash
cargo test --workspace
```

Expected: zero failures; report the fresh counts from this run.

- [x] **Step 4: Review the final diff and changed-file coverage.**

Run:

```bash
git diff --check
git status --short --branch
git diff --stat
```

Expected changed files: the new plan, `crates/flpdf/src/xref.rs`, `crates/flpdf/src/filters.rs`, `crates/flpdf/src/stream_filter.rs`, and `crates/flpdf/tests/xref_tests.rs`; unrelated worktrees and the main worktree remain untouched.

- [x] **Step 5: Commit the implementation on the isolated branch.**

After all verification commands pass, commit only the plan and implementation/test files with:

```bash
git add docs/superpowers/plans/2026-08-09-flpdf-t3yd-xref-filter-metadata.md crates/flpdf/src/xref.rs crates/flpdf/src/filters.rs crates/flpdf/src/stream_filter.rs crates/flpdf/tests/xref_tests.rs
git commit -m "fix(xref): resolve indirect stream filter metadata"
```

Do not close the Beads issue until the branch is integrated. Push the isolated branch and persist Beads state at session close as instructed by the repository workflow.
