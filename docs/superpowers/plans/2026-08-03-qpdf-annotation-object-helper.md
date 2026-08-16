# QPDFAnnotationObjectHelper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** qpdf 11.9.0 `QPDFAnnotationObjectHelper` の全公開責務を ObjectHandle-native helper に集約し、`page_annotation_enum` を削除する。

**Architecture:** `AnnotationObjectHelper` は `ObjectHandle` を値で保持する。ページの `/Annots` 列挙は `PageObjectHelper` に残し、flatten と CLI は ObjectHandle-native helper を直接使う。対象範囲に raw `Object`、`Pdf::resolve_borrowed`、互換 wrapper を残さない。

**Tech Stack:** Rust、flpdf ObjectHandle、pinned qpdf 11.9.0、cargo test/clippy、qpdf probe。

## Global Constraints

- Oracle は `include/qpdf/QPDFAnnotationObjectHelper.hh` と `libqpdf/QPDFAnnotationObjectHelper.cc`（qpdf 11.9.0）。
- qpdf public API を snake_case 化する。raw `Object`/`Dictionary` を歩く旧実装は残さない。
- **2026-08-16 訂正**: 当初「`ObjectRef + &mut Pdf` constructor を残さない／`AnnotationObjectHelper` は `ObjectHandle` を値で保持し `Pdf` 参照を持たない」としていたが誤り。
  qpdf の `QPDFObjectHandle` は所有 `QPDF*` を通して transparent に dereference するため、`getAppearanceStream` 等の
  「解決済みの型で分岐する」ロジックは resolve 能力を要求する。`ObjectHandle::get_key`/`as_stream_dict` 等は
  「resolve は一切しない」契約（`object_handle.rs` 各所の doc）なので、`Pdf` 参照を持たない value-only helper では
  `get_appearance_stream` を実装できない（矛盾）。Tier A1 の姉妹実装 `FormFieldObjectHelper`（`form_field_object_helper.rs`,
  closed issue `flpdf-ceun`）が実際に採用している形— `{ field_ref: ObjectRef, field: ObjectHandle, pdf: &'a mut Pdf<R> }` を
  保持し、各アクセサが hop ごとに `self.pdf.resolve_object_handle(&x)?` を呼ぶ — を `AnnotationObjectHelper` にも採用する。
  よって旧 `new(annot_ref: ObjectRef, pdf: &mut Pdf<R>)` constructor シグネチャ自体は維持し、**内部実装だけ** raw
  `Object`/`resolve_borrowed` から `ObjectHandle`/`resolve_object_handle` に置き換える。Task 1 の2件の RED テスト
  （`AnnotationObjectHelper::new(handle)` 単一引数、`get_rect()` が `Option<PageBox>` を返す想定）はこの矛盾する
  API 形状を specify していたため誤り — Task 2 で `new(annot_ref, pdf)` の2引数形状と非 `Option` の `get_rect`
  （後述）に修正する。
- 併せて Step 4 の検証コマンド（`rg -n 'Object::|resolve_borrowed|Pdf<' ...` で `Pdf<` もゼロ件を期待）から
  `Pdf<` を除外する。`Pdf<R>` フィールドは意図的に残る。`Object::`/`resolve_borrowed` の不在のみを検証する。
- **qpdf の fail-soft 規約への訂正**: 旧 flpdf 実装は「欠落/型不一致は `None`、不正値は `Err`」という独自の
  strict 方針だったが、qpdf の該当アクセサ（`getSubtype`/`getRect`/`getFlags`/`getAppearanceState`）は
  型不一致・欠落を **例外を投げず既定値で返す**（`libqpdf/QPDFObjectHandle.cc:817-836` の `getArrayAsRectangle`
  は非配列・要素数不一致・非数値要素のいずれも `{}`（0,0,0,0）を返す。`getName`/`getIntValueAsInt` も同様に
  type warning + 既定値）。Task 2 でこの fail-soft 契約に合わせて `get_rect` は非 `Option` の値を返し、
  `annotation_helper_error_tests.rs` の `rect_*_errors`/`appearance_reference_not_dict_errors` 系は
  「既定値を返す」期待に書き換える。`getArrayAsRectangle` は llx/lly/urx/ury を `min`/`max` で正規化する
  （逆順の矩形を許容）— 旧 `parse_rect_array` にはこの正規化が無かったので追加する。
- `QPDFFormFieldObjectHelper` の継承属性は Tier A1 に残す。
- `ObjectHandle::materialize` を annotation helper に導入しない（値ツリー全体の所有変換であり、hop ごとの
  resolve とは別責務）。
- `action()`（`/A` アクション辞書アクセサ）は qpdf 11.9.0 の `QPDFAnnotationObjectHelper.hh` に対応物が無い
  （メソッド一覧: `getSubtype`/`getRect`/`getAppearanceDictionary`/`getAppearanceState`/`getFlags`/
  `getAppearanceStream`/`getPageContentForAppearance` の7つのみ）。本体 crate 内に呼び出し元が無いことを確認済み
  （`rg -n '\.action\(\)' crates` は自身のテストのみ）。qpdf 忠実方針に従い削除する（`action_*` テストも削除）。

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

### Task 4 revision (2026-08-16)

`page_annotation_enum.rs::find_field_ref` (widget → owning field lookup) has
**no qpdf correspondence in `QPDFAnnotationObjectHelper`** — its header
comment states this responsibility lives in `QPDFAcroFormDocumentHelper`
(`getFieldForAnnotation`, backed by `analyze()`/`traverseField()`), which is
Tier B1 (`flpdf-2tfv`), not this issue. `find_field_ref` itself is a
one-hop heuristic (`/FT` or `/T` present → self, else direct `/Parent`),
not qpdf's real traversal.

A more faithful port of `analyze()`/`traverseField()` (with the orphan-widget
fallback pass) already exists, privately, in `signatures.rs::traverse_field` /
`page_widget_annotation_refs` / `collect_signature_form_field_refs`
(qpdf `libqpdf/QPDFAcroFormDocumentHelper.cc:204-286`). User decision
(2026-08-16): generalize that traversal into a shared primitive **now**, as
part of this issue, rather than deferring to `flpdf-2tfv`. This slice does
**not** absorb `overlay_annotations.rs`/`overlay_appearance_stream.rs` or
build the full `QPDFAcroFormDocumentHelper` surface — only
`annotation_to_field` (`analyze()`'s cache) and `field_for_annotation`
(`getFieldForAnnotation`).

Concrete shape:

1. In `acroform_document_helper.rs`, add `AcroFormDocumentHelper::
   annotation_to_field_map(&mut self) -> Result<BTreeMap<ObjectRef,
   ObjectRef>>` — the `analyze()` traversal (top-down from `/AcroForm/Fields`,
   `is_field`/`is_annotation` classification, cycle/depth guard, then the
   orphan-widget pass over every page). Add `field_for_annotation(&mut self,
   annot_ref) -> Result<Option<ObjectRef>>` mirroring `getFieldForAnnotation`
   (early `None` unless `annot_ref` resolves to a `/Subtype /Widget` dict,
   matching `QPDFObjectHandle::isDictionaryOfType("", "/Widget")`).
   **(B)-class deviation to record** (module doc + `docs/qpdf-correspondence.md`
   ⚪ row): qpdf caches `analyze()` on the helper instance
   (`Members::cache_valid`); `AcroFormDocumentHelper` holds no cached state
   (existing module convention — see `fields()`/`field_infos()`), so
   `annotation_to_field_map` recomputes the full traversal on every call.
   Algorithm and output order are unchanged; only the "container" (recomputed
   value vs. cached member) differs, and it does not change output bytes.
2. Rewrite `signatures.rs::collect_signature_form_field_refs` to call
   `annotation_to_field_map()` and take `.into_values().collect()`; delete its
   private `traverse_field`/`page_widget_annotation_refs`/`resolve_kids_array`
   (now redundant with the shared primitive).
3. Task 4's consumer migration (below) calls `field_for_annotation` in place
   of `find_field_ref`. Performance: do **not** call it once per widget across
   a page/document (qpdf's version is O(1) amortized only because
   `analyze()` is cached; a naive per-widget call here would rebuild the full
   traversal per widget — O(n²)). Build `annotation_to_field_map()` once per
   `enumerate_page_annotations` call and look up each widget in the returned
   map directly.

**Scope split (2026-08-16, post-implementation review):** step 3 above was
implemented and then reverted before landing. `page_annotation_enum.rs`'s
only production consumer of `.field_ref` is `flpdf-cli`'s
`generate_missing_appearances`, which *writes* `/V` through the resolved
field ref — swapping in `field_for_annotation`/`top_level_field` changes
which node that write targets for grouped fields (the new primitives walk to
the true top-level field, not the widget's direct parent), which is a
behavior change requiring `qpdf --generate-appearances` byte/structural
verification, not just a primitive substitution. Split out to
`flpdf-3yn9.22`, blocked on this issue. This issue keeps `find_field_ref` as
committed and ships only the `AnnotationObjectHelper` boundary (Task 1-2) and
the shared `annotation_to_field_map`/`field_for_annotation`/`top_level_field`
primitives plus `signatures.rs`'s delegation (steps 1-2 above) — both
verified qpdf-faithful and behavior-neutral for their existing callers.

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
