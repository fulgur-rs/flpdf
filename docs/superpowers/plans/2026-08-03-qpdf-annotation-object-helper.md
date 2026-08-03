# QPDFAnnotationObjectHelper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** qpdf 11.9.0 `QPDFAnnotationObjectHelper` の全公開責務を ObjectHandle-native helper に集約し、`page_annotation_enum` を削除する。

**Architecture:** `AnnotationObjectHelper` は `ObjectHandle` を値で保持する。ページの `/Annots` 列挙は `PageObjectHelper` に残し、flatten と CLI は ObjectHandle-native helper を直接使う。対象範囲に raw `Object`、`Pdf::resolve_borrowed`、互換 wrapper を残さない。

**Tech Stack:** Rust、flpdf ObjectHandle、pinned qpdf 11.9.0、cargo test/clippy、qpdf probe。

## Global Constraints

- Oracle は `include/qpdf/QPDFAnnotationObjectHelper.hh` と `libqpdf/QPDFAnnotationObjectHelper.cc`（qpdf 11.9.0）。
- qpdf public API を snake_case 化する。旧 `ObjectRef + &mut Pdf` constructor と raw-object accessor は残さない。
- `QPDFFormFieldObjectHelper` の継承属性は Tier A1 に残す。
- `ObjectHandle::materialize` を annotation helper に導入しない。

---

### Task 1: ObjectHandle annotation read API を RED で固定する

**Files:**
- Modify: `crates/flpdf/tests/annotation_helper_tests.rs`
- Modify: `crates/flpdf/tests/annotation_helper_error_tests.rs`
- Reference: `include/qpdf/QPDFAnnotationObjectHelper.hh:31-91`
- Reference: `libqpdf/QPDFAnnotationObjectHelper.cc:11-76`

**Interfaces:**
- Produces: `AnnotationObjectHelper::new(ObjectHandle)`, `get_subtype`, `get_rect`, `get_appearance_dictionary`, `get_appearance_state`, `get_flags`, `get_appearance_stream`.
- Consumes later: `Pdf::resolve_object_handle(ObjectRef)`.

- [ ] **Step 1: Write the failing tests**

Migrate annotation-only fixtures to resolve object handles and express the desired public API.

```rust
let handle = pdf.resolve_object_handle(ObjectRef::new(4, 0))?;
let annotation = AnnotationObjectHelper::new(handle);
assert_eq!(annotation.get_subtype(), b"Highlight");
assert_eq!(annotation.get_flags(), 0);
assert!(annotation.get_appearance_dictionary().is_null());
```

Add tests for direct/indirect `/Rect`, absent/non-integer `/F`, `/AS` name/non-name, direct appearance stream, state dictionary selected by explicit state, state dictionary selected by `/AS`, and missing/non-stream state returning null.

- [ ] **Step 2: Run RED tests**

Run: `cargo test -p flpdf --test annotation_helper_tests -- annotation_handle`

Expected: compile failure because the ObjectHandle constructor and `get_*` methods do not yet exist.

- [ ] **Step 3: Add fallback/error tests**

Specify qpdf’s branch behavior: `/AP/which` that is already a stream ignores `/AS`; a non-dictionary `/AP`, absent `/AP`, and absent selected state yield a null handle instead of the old raw-dictionary error API.

- [ ] **Step 4: Run RED error tests**

Run: `cargo test -p flpdf --test annotation_helper_error_tests -- appearance_stream`

Expected: compile failure for the new API.

- [ ] **Step 5: Commit the test-only change**

```bash
git add crates/flpdf/tests/annotation_helper_tests.rs crates/flpdf/tests/annotation_helper_error_tests.rs
git commit -m "test: specify ObjectHandle annotation helper API"
```

### Task 2: Implement the qpdf annotation helper boundary

**Files:**
- Modify: `crates/flpdf/src/annotation_helper.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `crates/flpdf/tests/annotation_helper_tests.rs`
- Modify: `crates/flpdf/tests/annotation_helper_error_tests.rs`

**Interfaces:**
- Consumes: Task 1 tests and `ObjectHandle`.
- Produces: an ObjectHandle-only annotation helper; `FormFieldObjectHelper` remains in its Tier A1 home.

- [ ] **Step 1: Implement the minimal value helper**

Replace the annotation portion with:

```rust
pub struct AnnotationObjectHelper {
    object: ObjectHandle,
}

impl AnnotationObjectHelper {
    pub fn new(object: ObjectHandle) -> Self { Self { object } }
    pub fn object_handle(&self) -> &ObjectHandle { &self.object }
    pub fn get_appearance_dictionary(&self) -> ObjectHandle {
        self.object.get_key(b"AP")
    }
    pub fn get_flags(&self) -> i64 {
        self.object.get_key(b"F").as_integer().unwrap_or(0)
    }
}
```

Implement `get_subtype` and `get_appearance_state` with `as_name().unwrap_or_default()`. Implement `get_rect` from four ObjectHandle numeric values. Implement `get_appearance_stream` exactly as qpdf: take `/AP/which`, return it immediately when it is a stream, otherwise select explicit state or `/AS` only from a state dictionary, then return `ObjectHandle::null()`.

- [ ] **Step 2: Run GREEN tests**

Run: `cargo test -p flpdf --test annotation_helper_tests && cargo test -p flpdf --test annotation_helper_error_tests`

Expected: all migrated and new tests pass.

- [ ] **Step 3: Separate form-field responsibility**

Move any remaining `FormFieldObjectHelper` code/re-export to its Tier A1 module. Do not preserve raw-`Pdf` annotation access as a bridge.

- [ ] **Step 4: Verify the boundary**

Run:

```bash
cargo test -p flpdf --test annotation_helper_tests
rg -n 'Object::|resolve_borrowed|Pdf<' crates/flpdf/src/annotation_helper.rs
```

Expected: tests pass and the grep has no annotation-helper legacy references.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/annotation_helper.rs crates/flpdf/src/lib.rs crates/flpdf/tests/annotation_helper_tests.rs crates/flpdf/tests/annotation_helper_error_tests.rs
git commit -m "feat: add ObjectHandle annotation helper"
```

### Task 3: Move qpdf appearance-content construction under the helper

**Files:**
- Modify: `crates/flpdf/src/annotation_helper.rs`
- Modify: `crates/flpdf/src/page_annotation_flatten.rs`
- Modify: `crates/flpdf/tests/page_document_helper_tests.rs`
- Reference: `libqpdf/QPDFAnnotationObjectHelper.cc:78-193`

**Interfaces:**
- Consumes: Task 2 `get_appearance_stream`, `get_rect`, and `get_flags`.
- Produces: `get_page_content_for_appearance(name, rotate, required_flags, forbidden_flags)`.

- [ ] **Step 1: Write a failing helper-owned flatten test**

Use the existing annotation fixture builder to assert that `/AP/N` selection, required/forbidden flag gates, and NoRotate handling go through the helper. Include the existing non-UTF-8 `/AS` selection regression.

- [ ] **Step 2: Run RED test**

Run: `cargo test -p flpdf --test page_document_helper_tests -- annotation`

Expected: failure because the helper has no content-generation method or flatten still owns duplicate behavior.

- [ ] **Step 3: Migrate the matrix/content path**

Move qpdf’s `/BBox` + `/Matrix` + `/Rect` calculation into `get_page_content_for_appearance`. Preserve flag gates, NoRotate 90/180/270 transforms, zero-size rejection, `/Subtype /Form` mutation, and the exact `q\n... cm\n/name Do\nQ\n` output. Replace the private flatten resolver with this helper call.

- [ ] **Step 4: Run GREEN test**

Run: `cargo test -p flpdf --test page_document_helper_tests -- annotation`

Expected: all targeted flatten tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/annotation_helper.rs crates/flpdf/src/page_annotation_flatten.rs crates/flpdf/tests/page_document_helper_tests.rs
git commit -m "refactor: centralize annotation appearance content"
```

### Task 4: Absorb page annotation enumeration and migrate consumers

**Files:**
- Modify: `crates/flpdf/src/page_annotation_flatten.rs`
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Delete: `crates/flpdf/src/page_annotation_enum.rs`
- Modify: `crates/flpdf/tests/page_document_helper_tests.rs`
- Modify: `crates/flpdf-cli/tests/cli_tests.rs`

**Interfaces:**
- Consumes: Task 2 helper and `PageObjectHelper::get_annotations`.
- Produces: direct consumer loops without `EnumeratedAnnotation` or exported enumerator functions.

- [ ] **Step 1: Write failing consumer tests**

Move old enumeration cases to consumer tests: no annotations, ordered mixed annotations, merged widget/field, separated direct `/Parent`, `/T`-only terminal field, and indirect `/Rect`. Preserve CLI signature candidate selection at its public boundary.

- [ ] **Step 2: Run RED tests**

Run:

```bash
cargo test -p flpdf --test page_document_helper_tests -- annotation
cargo test -p flpdf-cli --test cli_tests -- signature
```

Expected: failures until consumers own the handle-native loop.

- [ ] **Step 3: Replace and delete**

Consumers obtain refs with `PageObjectHelper::get_annotations`, resolve handles via `Pdf::resolve_object_handle`, and wrap each in `AnnotationObjectHelper`. Preserve widget linkage: direct non-null `/FT` or `/T` means self; otherwise use the direct `/Parent` reference. Delete the module declaration and public re-exports.

- [ ] **Step 4: Run GREEN tests and source check**

Run:

```bash
cargo test -p flpdf --test page_document_helper_tests -- annotation
cargo test -p flpdf-cli --test cli_tests -- signature
rg -n 'page_annotation_enum|enumerate_page_annotations|enumerate_document_annotations|EnumeratedAnnotation' crates
```

Expected: tests pass and there are no production references to the deleted API.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/page_annotation_flatten.rs crates/flpdf-cli/src/main.rs crates/flpdf/src/lib.rs
git add crates/flpdf/tests/page_document_helper_tests.rs crates/flpdf-cli/tests/cli_tests.rs
git rm crates/flpdf/src/page_annotation_enum.rs
git commit -m "refactor: absorb page annotation enumeration"
```

### Task 5: Oracle probe and verification

**Files:**
- Modify only if a qpdf-verified failure requires it.

- [ ] **Step 1: Run a pinned-qpdf selection probe**

Build a smallest PDF covering direct normal stream, normal state dictionary, and missing state. Record the qpdf 11.9.0 source lines and actual result for each `/AP` selection branch.

- [ ] **Step 2: Run quality gates**

Run:

```bash
cargo fmt -- --check
cargo test -p flpdf --test annotation_helper_tests
cargo test -p flpdf --test annotation_helper_error_tests
cargo test -p flpdf --test page_document_helper_tests
cargo test -p flpdf-cli --test cli_tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: all commands exit 0 and changed executable-line coverage is 100%.

- [ ] **Step 3: Record closure evidence**

Only after the quality gates pass, record qpdf source/probe evidence, focused commands, coverage, and deleted-old-module evidence on `flpdf-9ng9`; then run `bd lint`, `bd close flpdf-9ng9`, and `bd dolt push`.
