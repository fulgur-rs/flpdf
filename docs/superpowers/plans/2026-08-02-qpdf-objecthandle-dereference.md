# QPDFObjectHandle Dereference Primitive Implementation Plan

> **Execution:** use TDD for behavioral changes and verify against pinned qpdf
> 11.9.0 before accepting review suggestions or semantic assumptions.

**Goal:** Land only the qpdf-faithful canonical-slot/dereference primitive.
Do not ship a partial document resolver or migrate a consumer before the
required stream, resolver, and write-back components exist.

## Task 1: Pin the qpdf accessor contract

**Files:**

- `tests/oracle/qpdf_objecthandle_dereference_probe.cc`
- `scripts/qpdf-objecthandle-dereference-diff.sh`
- `scripts/tests/qpdf-objecthandle-dereference-diff-contract.sh`

- [x] Add a C++ probe that observes indirect identity before and after
  dictionary/type access.
- [x] Resolve and integrity-check the pinned qpdf 11.9.0 source.
- [x] Build and link only the build-local pinned `libqpdf`.
- [x] Run the runner contract test.
- [x] Run the probe on `tests/fixtures/minimal.pdf`.
- [x] Generate an ObjStm form with qpdf and record decoded-stream-relative
  parsed offsets; use that result to bound the responsibility of this slice.

Commands:

```bash
bash scripts/tests/qpdf-objecthandle-dereference-diff-contract.sh
scripts/qpdf-objecthandle-dereference-diff.sh tests/fixtures/minimal.pdf
qpdf --object-streams=generate tests/fixtures/minimal.pdf /tmp/flpdf-objecthandle-objstm.pdf
scripts/qpdf-objecthandle-dereference-diff.sh /tmp/flpdf-objecthandle-objstm.pdf
```

## Task 2: Add resolver-bearing canonical slots

**Files:**

- `crates/flpdf/src/object_handle.rs`

- [x] Add RED tests using a recording resolver and a dropped resolver.
- [x] Add the crate-private `DocumentResolver` callback.
- [x] Store an optional weak resolver link in an indirect slot without
  changing the existing legacy constructor's behavior.
- [x] Implement `try_dereference`, releasing the slot borrow before resolver
  entry and updating the supplied same handle in place.
- [x] Implement fallible null and dictionary accessors.
- [x] Prove resolver errors propagate unchanged through every accessor.
- [x] Prove a present child resolving to null is absent under `try_has_key`.
- [x] Add qpdf's public identity predicate as `is_same_object_as`.

Focused command:

```bash
cargo test -p flpdf --lib object_handle::
```

## Task 3: Mark, but do not wrap, legacy deletion targets

**Files:**

- `crates/flpdf/src/object.rs`
- `crates/flpdf/src/object_handle.rs`
- `crates/flpdf/src/reader.rs`
- `crates/flpdf/src/ref_chain.rs`

- [x] Mark raw `Object`, `ObjectValue::Reference`, `ref_chain`, materialized
  memo, and legacy resolver/terminal-clone entry points with
  `qpdf-cutover-delete(flpdf-25kg.3.3)`.
- [x] Do not add `#[deprecated]` while no complete replacement exists.
- [x] Keep legacy implementations unchanged while they still have callers.

Gate:

```bash
rg -n 'qpdf-cutover-delete\(flpdf-25kg\.3\.3\)' crates/flpdf/src
```

## Task 4: Remove the invalid partial resolver integration

The first implementation attempt added `Pdf::get_object` backed by a resolver
that handled only uncompressed direct objects. The pinned-qpdf ObjStm probe
proved this was not the complete `QPDF::Resolver` responsibility: qpdf parses
all applicable members and records decoded-stream-relative offsets.

- [x] Remove `qpdf_resolver.rs`, `SharedInput`, `Pdf::get_object`, and their
  reader integration tests.
- [x] Retain the pure ObjectHandle primitive and qpdf identity API.
- [x] Create dependent Beads `flpdf-25kg.3.4` through `.3.7` for native stream
  decoding, complete resolution, write-back, and QPDF_pages cutover.
- [x] Move the consumer-audit dependency to the final component cutover.

## Task 5: Verify and publish the primitive slice

- [x] Run the focused primitive tests (128 passed).
- [x] Run the oracle runner and runner contract (minimal offsets 17/66;
  generated ObjStm offsets 9/43).
- [x] Run the full flpdf and workspace suites (zero failures).
- [x] Run format, clippy with warnings denied, and fresh changed-line coverage
  (156/156 executable lines, 100%).
- [x] Record exact results in `flpdf-25kg.3.3`.
- [x] Commit the responsibility correction.
- [x] Push Beads and git.
- [x] Open pull request #620 for the primitive slice.

Commands:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --lib object_handle::
bash scripts/tests/qpdf-objecthandle-dereference-diff-contract.sh
scripts/qpdf-objecthandle-dereference-diff.sh tests/fixtures/minimal.pdf
cargo test -p flpdf
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path /tmp/flpdf-25kg-3-3.lcov
scripts/patch-coverage.sh --base origin/main --lcov /tmp/flpdf-25kg-3-3.lcov
bd dolt push
git push
```
