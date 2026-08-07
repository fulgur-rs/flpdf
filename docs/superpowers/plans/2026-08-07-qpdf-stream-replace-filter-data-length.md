# QPDF Stream replaceFilterData Length Boundary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make stream buffer replacement remove `/Length` for an empty buffer and set the exact length for a non-empty buffer, matching qpdf 11.9.0.

**Architecture:** Add one private `ObjectHandle::replace_filter_data` helper corresponding to `QPDF_Stream::replaceFilterData`. Keep the public buffer API and shared `Rc<Vec<u8>>` ownership unchanged, and route its filter, decode-parameter, and length dictionary mutations through the helper so the provider follow-up can reuse the same boundary.

**Tech Stack:** Rust workspace, qpdf 11.9.0 source oracle, Cargo tests, cargo-llvm-cov.

## Global Constraints

- Pinned qpdf 11.9.0 `QPDF_Stream.cc:640-684` and `QPDFObjectHandle.cc:1344-1362` are authoritative.
- Length zero removes `/Length`; nonzero length writes the exact integer.
- Preserve the caller's shared `Rc<Vec<u8>>` allocation without copying.
- Preserve optional `/Filter` and `/DecodeParms` behavior and the current non-stream no-op surface.
- Do not implement provider storage/execution, `QPDF::newStream`, pipeline/writer/Filespec migration, or direct-stream API expansion.

---

### Task 1: Port the shared replaceFilterData boundary

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Modify: `docs/qpdf-correspondence.md`
- Test: `crates/flpdf/src/object_handle.rs` (`stream_payload_sharing_tests` and `mutation_tests`)

**Interfaces:**
- Consumes: `ObjectHandle::as_stream_dict`, `ObjectHandle::replace_key`, `ObjectHandle::remove_key`, and the existing `Rc<Vec<u8>>` stream payload slot.
- Produces: private `ObjectHandle::replace_filter_data(filter: Option<ObjectHandle>, decode_parms: Option<ObjectHandle>, length: usize)` and qpdf-compatible `ObjectHandle::replace_stream_data` length behavior.

- [x] **Step 1: Write failing zero-length tests**

  Change `an_empty_payload_is_shared_like_any_other` to assert `!dict.has_key(b"Length")`. Add tests that start with existing and missing `/Length`, exercise repeated empty/non-empty replacement, and mutate a document-owned indirect stream.

- [x] **Step 2: Run focused tests and verify RED**

  Run:

  ```bash
  cargo test -p flpdf --lib stream_payload_sharing_tests::an_empty_payload_is_shared_like_any_other
  cargo test -p flpdf --lib object_handle::mutation_tests::replace_stream_data
  ```

  Expected: empty-buffer assertions fail because current code writes `/Length 0`; existing non-empty assertions remain green.

- [x] **Step 3: Implement the minimal shared boundary**

  Add the private helper with the qpdf branch:

  ```rust
  fn replace_filter_data(
      &self,
      filter: Option<ObjectHandle>,
      decode_parms: Option<ObjectHandle>,
      length: usize,
  ) {
      let Some(dict) = self.as_stream_dict() else {
          return;
      };
      if let Some(filter) = filter {
          dict.replace_key(b"Filter", filter);
      }
      if let Some(decode_parms) = decode_parms {
          dict.replace_key(b"DecodeParms", decode_parms);
      }
      if length == 0 {
          dict.remove_key(b"Length");
      } else {
          dict.replace_key(
              b"Length",
              ObjectHandle::integer(i64::try_from(length).unwrap_or(i64::MAX)),
          );
      }
  }
  ```

  In `replace_stream_data`, retain the shared payload installation, then delegate dictionary mutation and the non-stream guard to this helper with `data.len()`.

- [x] **Step 4: Verify GREEN and update correspondence docs**

  Run the two focused commands from Step 2 and the full `replace_stream_data` unit-test filter. Update the method rustdoc and the `QPDF_Stream::stream_data` correspondence annotation to record the shared zero/nonzero boundary.

- [ ] **Step 5: Run repository quality gates**

  Run:

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test
  ```

  Then run the relevant qpdf byte-identical corpus under `qpdf-zlib-compat` and produce fresh workspace LCOV plus `scripts/patch-coverage.sh --base main` evidence with zero uncovered changed executable lines.

- [ ] **Step 6: Review, commit, and publish**

  Inspect `git diff --check` and the exact `main...HEAD` diff, commit only the plan, implementation, tests, and correspondence update, push the feature branch, persist Beads with `bd dolt push`, and report without closing the issue until integration is confirmed.
