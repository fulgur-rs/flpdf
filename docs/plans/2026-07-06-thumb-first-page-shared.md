# Thumb First-Page-Shared Classification Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** linearize が「first-page closure のオブジェクトが同時に別ページの `/Thumb`
ターゲット」を qpdf と同じく `lc_first_page_shared`（part3）に分類するようにする。

**Architecture:** `LinearizationPlan::from_pdf` に `thumb_shared_set`（各ページの
`/Thumb` 閉包 − そのページ自身の ou_page 閉包の和集合）を計算し、Step 5 の
part2/part3 gate に `thumb_shared_set` メンバーシップを追加する。qpdf の
`ou_thumb` user（QPDF_optimization.cc:317-324）と分類条件
`thumbs==0`（QPDF_linearization.cc:1124-1127）を模倣する。classic・generate 両モードは
同じ `from_pdf` の part2/part3 を消費するため、この 1 箇所で両方が直る（POC 実測確認済み）。

**Tech Stack:** Rust, flpdf linearization crate, qpdf 11.9.0 oracle,
`qpdf-zlib-compat` feature（byte-identical 検証）, python3 fixture 生成。

**背景（実測エビデンス）:** design（beads flpdf-hn1g.16）参照。cross-page（X=page0
リソース＋page1 の /Thumb）→ shared、same-page self-thumb（page0 が X を自リソース＋
自 /Thumb、他ページ thumb 無し）→ private を qpdf 11.9.0 で実測確認済み。

---

## Task 1: 分類の unit テスト（cross-page → part3）

**Files:**
- Modify test module: `crates/flpdf/src/linearization/plan.rs`（`#[cfg(test)] mod tests`）

新規の in-memory fixture builder + テストを追加する。既存の fixture builder
（`mod tests` 内、`fn` 群）のスタイルに合わせる。2 ページ: page0 が画像 X を
`/Resources /XObject` で参照、page1 が `/Thumb → X`。X の番号は content0 より小さく。

**Step 1: 失敗するテストを書く**

`mod tests` に追加（オブジェクト番号は builder に合わせて調整。X < content0 を維持）:

```rust
/// 2-page PDF: page0 uses image X (obj 5) as an /XObject resource; page1's
/// /Thumb points at the SAME X. qpdf gives X both ou_page(0) and ou_thumb(1),
/// so X is lc_first_page_shared (QPDF_linearization.cc:1124-1127) -> part3.
/// X (obj 5) is numbered below content0 (obj 6) so private/shared changes order.
fn thumb_first_page_shared_pdf() -> Vec<u8> {
    let mut b = Vec::new();
    // 1=Catalog 2=Pages 3=page0 4=page1 5=X(image) 6=content0 7=content1
    // (build with the module's byte-fixture idiom; keep X=5 < content0=6)
    // ... (mirror gen_thumbnail_firstpage_shared.py layout) ...
    b
}

#[test]
fn thumb_target_that_is_first_page_object_is_part3_shared() {
    let bytes = thumb_first_page_shared_pdf();
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open");
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
    let x = ObjectRef::new(5, 0);
    assert!(
        plan.part3_objects.contains(&x),
        "X (first-page object that is also page1's /Thumb) must be part3 (lc_first_page_shared); \
         part2={:?} part3={:?}",
        plan.part2_objects, plan.part3_objects
    );
    assert!(!plan.part2_objects.contains(&x), "X must NOT be in part2 (private)");
}
```

**Step 2: テストが失敗することを確認**

Run: `cargo test -p flpdf --lib thumb_target_that_is_first_page_object_is_part3_shared`
Expected: FAIL（現状 X は part2 に入る）。

**Step 3: コミット（テストのみ、red）**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "test(linearize): failing test — thumb target that is first-page object must be part3 (flpdf-hn1g.16)"
```

---

## Task 2: same-page self-thumb の unit テスト（→ part2 に残る）

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs`（`mod tests`）

**Step 1: 失敗しない（現状 pass）が回帰ガードになるテストを書く**

page0 が X を `/Resources /XObject` **と** 自身の `/Thumb` 両方に使い、他ページは
thumb しない fixture。qpdf 実測: X は private（thumbs=0、ou_page が visited 共有で勝つ）。

```rust
/// page0 uses image X (obj 5) as BOTH an /XObject resource AND its own /Thumb;
/// no other page references X. qpdf: within page0's traversal the /Resources walk
/// reaches X (ou_page 0) before /Thumb, sharing `visited`, so X gets NO ou_thumb
/// -> thumbs=0 -> lc_first_page_private. X (obj 5 < content0 6) stays in part2.
fn self_thumb_first_page_private_pdf() -> Vec<u8> { /* ... */ Vec::new() }

#[test]
fn self_thumb_first_page_object_stays_part2_private() {
    let bytes = self_thumb_first_page_private_pdf();
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open");
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
    let x = ObjectRef::new(5, 0);
    assert!(plan.part2_objects.contains(&x), "self-thumb X must stay part2 (private)");
    assert!(!plan.part3_objects.contains(&x), "self-thumb X must NOT be part3");
}
```

**Step 2: 現状で走らせる**

Run: `cargo test -p flpdf --lib self_thumb_first_page_object_stays_part2_private`
Expected: 現状 PASS（現状は全 thumb を無視するので X は part2）。これは Task 3 の
subtraction が過剰に shared 化しないことのガード。

**Step 3: コミット**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "test(linearize): guard — self-thumb first-page object stays part2 private (flpdf-hn1g.16)"
```

---

## Task 3: 修正の実装（thumb_shared_set + gate）

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs`
  - Step 4（per-page closure ループ、`other_page_closures.push(closure); }` の直後、
    現状 line ~937）の後に `thumb_shared_set` を計算。
  - Step 5 の part3 gate（現状 line ~983-985）に `|| thumb_shared_set.contains(obj_ref)`。

**Step 1: thumb_shared_set 計算を挿入**

`other_page_closures` ループ直後、Step 5 コメントの前:

```rust
// ----------------------------------------------------------------
// Step 4b: thumb-set for the first-page private/shared split.
// ----------------------------------------------------------------
// qpdf gives a page's /Thumb target the separate `ou_thumb` user
// (QPDF_optimization.cc:317-324) sharing that page's ou_page `visited`. A
// first-page object that is also some page's /Thumb therefore has thumbs>0
// and is lc_first_page_shared, not lc_first_page_private
// (QPDF_linearization.cc:1124-1127). compute_closure skips /Thumb (each page's
// closure excludes its thumbnail targets), so neither shared_page_indices nor
// document_other_set captures this; recover the set here. Object-stream-mode
// independent, like open_document/outlines/others above.
let thumb_page_tree = page_tree_node_refs(pdf)?;
let mut thumb_refs: Vec<(usize, ObjectRef)> = Vec::new();
for (page_idx, &page_ref) in page_refs.iter().enumerate() {
    if let Object::Dictionary(d) = pdf.resolve_borrowed(page_ref)? {
        if let Some(Object::Reference(r)) = d.get("Thumb") {
            thumb_refs.push((page_idx, *r));
        }
    }
}
let mut thumb_shared_set: BTreeSet<ObjectRef> = BTreeSet::new();
for (page_idx, thumb_ref) in thumb_refs {
    let closure = closure_from_seeds(pdf, vec![(thumb_ref, false)], &thumb_page_tree)?;
    // Subtract the same page's ou_page closure: qpdf traverses /Thumb AFTER the
    // page's other ref-bearing keys (alphabetical getKeys order) with a shared
    // `visited`, so an object already reached by that page's ou_page walk never
    // also receives ou_thumb from the same page (verified: page0 self-thumb of
    // its own resource stays lc_first_page_private in qpdf 11.9.0).
    let own_set: BTreeSet<ObjectRef> = if page_idx == 0 {
        first_page_closure.iter().copied().collect()
    } else {
        other_page_closures[page_idx - 1].iter().copied().collect()
    };
    for r in closure {
        if !own_set.contains(&r) {
            thumb_shared_set.insert(r);
        }
    }
}
```

**Step 2: gate に追加**

```rust
} else if shared_page_indices.contains_key(obj_ref)
    || document_other_set.contains(obj_ref)
    || thumb_shared_set.contains(obj_ref)
{
```

（併せて直上のコメント `// lc_first_page_shared: in_first_page AND (other_pages>0 ||
others>0).` に thumbs を追記: `... || others>0 || thumbs>0).` と `thumb_shared_set`
supplies thumbs の 1 行を足す。）

**Step 3: 両 unit テストが pass することを確認**

Run: `cargo test -p flpdf --lib self_thumb_first_page_object_stays_part2_private thumb_target_that_is_first_page_object_is_part3_shared`
Expected: 両方 PASS。

**Step 4: 既存の linearize unit テストが無回帰か確認**

Run: `cargo test -p flpdf --lib linearization`
Expected: 全 PASS（thumb を持たない/非 first-page thumb の既存 plan テストが不変）。

**Step 5: コミット**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "fix(linearize): classify first-page /Thumb target as lc_first_page_shared (flpdf-hn1g.16)"
```

---

## Task 4: fixture 生成器 + golden（byte-identical 用）

**Files:**
- Create: `docs/plans/tools/gen_thumbnail_firstpage_shared.py`
- Modify: `tests/golden/regenerate.sh`（G6HB2_FIX map に stem 追加、golden 生成ループ 2 箇所に stem 追加）
- Generated (commit): `tests/fixtures/compat/objstm-lin-thumb-firstpage-shared.pdf`,
  `tests/golden/references/objstm-lin-thumb-firstpage-shared/{linearize.pdf,linearize-objstm.pdf}`

**Step 1: 生成器を作成**（scratchpad の gen_thumb_firstpage.py を基に。stem に合う出力）

`docs/plans/tools/gen_thumbnail_firstpage_shared.py`：2 ページ、page0 が画像 X
（obj 5）を `/Resources /XObject /Im0` で参照、page1 が `/Thumb → X`、content0=6,
content1=7。（scratchpad の検証済みスクリプトをそのまま採用。）

**Step 2: regenerate.sh に登録**

- `G6HB2_FIX` 連想配列に:
  `[objstm-lin-thumb-firstpage-shared]="gen_thumbnail_firstpage_shared.py"`
- classic golden ループ（`linearize.pdf` を作る for ループ）に stem 追加。
- objstm golden ループ（`linearize-objstm.pdf` を作る for ループ、
  `objstm-lin-thumbnail-private-shared` の隣）に stem 追加。

**Step 3: fixture + golden を生成**

Run: `bash tests/golden/regenerate.sh`（qpdf 11.9.0 必須）
Expected: `tests/fixtures/compat/objstm-lin-thumb-firstpage-shared.pdf` と
`tests/golden/references/objstm-lin-thumb-firstpage-shared/{linearize.pdf,linearize-objstm.pdf}` が生成。
`qpdf --check` が両 golden で pass。

**Step 4: 生成物を確認**（X が shared 位置＝content0 の後にあること）

Run: `python3 -c "..."` で golden の first-page section オブジェクト順を確認、
`page0, content0, X` になっていること（design の実測と一致）。

**Step 5: コミット**

```bash
git add docs/plans/tools/gen_thumbnail_firstpage_shared.py tests/golden/regenerate.sh \
        tests/fixtures/compat/objstm-lin-thumb-firstpage-shared.pdf \
        tests/golden/references/objstm-lin-thumb-firstpage-shared/
git commit -m "test(linearize): add thumb-firstpage-shared fixture + qpdf goldens (flpdf-hn1g.16)"
```

---

## Task 5: byte-identical テスト（classic + generate）+ ci.yml 登録

**Files:**
- Modify: `crates/flpdf/tests/cmp_linearize_tests.rs`（classic, qpdf-zlib-compat gated）
- Modify: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`（generate）
- Modify: `.github/workflows/ci.yml`（bytes-identical テスト列挙に新テスト名を追加）

**Step 1: classic byte テストを追加**（既存 `assert_linearize_byte_identical` ヘルパー利用）

```rust
#[test]
fn thumb_firstpage_shared_classic_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "objstm-lin-thumb-firstpage-shared.pdf",
        "objstm-lin-thumb-firstpage-shared",
    );
}
```

**Step 2: generate byte テスト + structural を追加**（既存 thumbnail テストのパターンに合わせる）

`cmp_linearize_objstm_tests.rs` の `objstm-lin-thumbnail-private-shared` の隣に、
`_structurally_byte_identical_to_qpdf`（非 gated）と `_byte_identical_to_qpdf`
（qpdf-zlib-compat gated）の 2 本を追加。

**Step 3: 走らせて pass 確認**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests --test cmp_linearize_objstm_tests thumb_firstpage`
Expected: 追加した byte テストが全 PASS（修正後 flpdf 出力が qpdf golden と 1 バイト一致）。

**Step 4: 無回帰確認（既存 thumbnail 含む全 linearize byte テスト）**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests --test cmp_linearize_objstm_tests`
Expected: 全 PASS。

**Step 5: ci.yml に登録**

`.github/workflows/ci.yml` の bytes-identical テストを明示列挙する箇所に、追加した
テスト関数名（gated なもの）を足す（列挙しないと CI で走らない — memory
flpdf-ci-bytes-identical-explicit-test-list）。

**Step 6: コミット**

```bash
git add crates/flpdf/tests/cmp_linearize_tests.rs crates/flpdf/tests/cmp_linearize_objstm_tests.rs .github/workflows/ci.yml
git commit -m "test(linearize): byte-identical thumb-firstpage-shared (classic+generate) + CI register (flpdf-hn1g.16)"
```

---

## Task 6: 品質ゲート（patch-coverage + fmt + clippy）

**Files:** なし（検証のみ）

**Step 1: fmt**

Run: `cargo fmt` → `cargo fmt --check`
Expected: 差分なし（push 前ゲート — memory flpdf-ci-quality-fmt-check）。

**Step 2: clippy**

Run: `cargo clippy -p flpdf --all-targets`
Expected: warning なし。

**Step 3: patch-coverage（変更行 100%）**

作業を commit 済みであることを確認してから:
Run: `scripts/patch-coverage.sh --base main`
Expected: flpdf 変更行 100%。未カバーがあればテスト追加（subtraction の両分岐・
None(/Thumb 無しページ) 分岐が cross-page/self-thumb テストでカバーされるはず）。
llvm-cov は `qpdf-zlib-compat` なしで回す（memory llvm-cov-no-qpdf-zlib-compat）。

**Step 4: 質的チェック** — エラーアーム・境界・self-thumb エッジのアサーションが
実質的か確認。

**Step 5: 最終コミット（もしテスト追加が発生した場合）**

```bash
git add -A && git commit -m "test(linearize): cover thumb_shared_set branches (flpdf-hn1g.16)"
```

---

## 完了基準（design の acceptance と一致）

1. 新 fixture で qpdf 11.9.0 の `--linearize --deterministic-id` と
   `--linearize --object-streams=generate --deterministic-id` 出力が
   flpdf と byte-identical（qpdf-zlib-compat）。
2. cross-page → part3、self-thumb → part2（unit テスト）。
3. 既存 golden 全て無回帰。
4. 変更行 100% カバー、新 byte テストを ci.yml に登録。
