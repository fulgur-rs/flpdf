# resolve_ref_chain 横展開（holder-chain 堅牢化）Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 構造的 one-hop resolve サイト（dict/サブツリーを落としうる箇所）を `crate::ref_chain::resolve_ref_chain` で terminal 正規化し、二重間接（`ref→ref→値`）でも値を取りこぼさないようにする。

**Architecture:** 各サイトで「dict フィールド値/配列要素から取り出した ref を一度だけ解決→型判定」している箇所を `resolve_ref_chain(pdf, &Object::Reference(r))?` に置換し、返り値の terminal `Object`（`.0`）を型判定に、必要なら terminal `ObjectRef`（`.1`）を dedup/書換キーに使う。テストは各サイトの公開操作に対し、二重間接で対象を格納した PDF を `tests/common::build_pdf` で構築し、値が拾われることを assert する。

**Tech Stack:** Rust, flpdf crate。`resolve_ref_chain`（`crates/flpdf/src/ref_chain.rs:25`、`pub(crate)`、戻り値 `(Object terminal, Option<ObjectRef> last_ref)`、`MAX_REF_CHAIN_DEPTH=64`）。テストは `crates/flpdf/tests/*.rs`（`common::build_pdf` 利用）。

**Issue:** flpdf-3x23（design フィールドに full design）。

---

## 共通レシピ（全タスクで参照）

### レシピA: read-then-typecheck（多数）

現状（例）:
```rust
} else if let Some(reference) = resources.as_ref_id() {
    pdf.resolve_borrowed(reference)?.as_dict().cloned()
}
```
置換後:
```rust
} else if let Some(reference) = resources.as_ref_id() {
    let (terminal, _) = crate::ref_chain::resolve_ref_chain(pdf, &Object::Reference(reference))?;
    terminal.into_dict() // 所有 terminal から move で Option<Dictionary> を取り出す（追加クローンなし）
}
```

**重要（二重クローン回避 / レビューパターン#1）**: `resolve_ref_chain` は terminal を**所有 `Object` で返し、その時点でクローン済み**。そこからさらに `.as_dict().cloned()` すると dict を二度クローンする。所有 terminal は **move で取り出す**こと:
- `Object::into_dict(self) -> Option<Dictionary>`（`object.rs:257`）、`into_array(self) -> Option<Vec<Object>>`（`:273`）、`into_name(self) -> Option<Vec<u8>>`（`:281`）を使う。
- `match terminal { Object::Stream(s) => ..., _ => ... }` のように move-match でも可。
- `.as_dict().cloned()` は使わない（二度クローンになる）。

`use` 行: 各ファイル先頭に `use crate::ref_chain::resolve_ref_chain;` を1つ追加（既存ファイルで未 import の場合）。`name_number_tree.rs:16` が手本。

### レシピB: dedup-or-rewrite by ref（少数）

visited セットや `set_object` で **ref をキー/書換対象**にする箇所は、必ず `.1`（terminal_ref）を使う。`name_number_tree.rs:289` が手本:
```rust
let (terminal, terminal_ref) = resolve_ref_chain(pdf, &Object::Reference(r))?;
// dedup は terminal_ref で
if !visited.insert(terminal_ref.unwrap_or(r)) { ... }
// 書換も terminal_ref で
pdf.set_object(terminal_ref.unwrap_or(r), updated);
```
中間 ref（`r`）を visited/`set_object` に使うとバグが別の形で再発する。

**カバレッジ注意（必読）**: 本サイト群では `resolve_ref_chain` に**常に `Object::Reference(r)` を渡す**ため、`terminal_ref` は **構造的に常に `Some`**（`None` は start が非 Reference のときだけ）。よって必ず `terminal_ref.unwrap_or(r)`（eager・単一式）を使うこと。`match terminal_ref { Some(t)=>t, None=>r }` や `unwrap_or_else(|| r)` に展開しない — `None` アーム/クロージャは**到達不能な dead code** で、変更行 100% カバレッジゲートが落ちる。

### レシピT: holder-chain テスト（各サイト1件）

`tests/*.rs`（既存の対応ファイル）に追加。`common::build_pdf(&[(num,"body")], root)` を使い、対象の値を **carrier `(N,"M 0 R")` + terminal `(M, "<本来の値>")`** の2ホップで格納する。既存の手本: `acroform_document_helper_tests.rs:410`（`(6,"7 0 R")`,`(7,"[4 0 R]")`）、`page_merge_tests.rs:4329`（`(10,"11 0 R")`）。

TDD 順序:
1. テストを書く（terminal が拾われること = 非空/値一致を assert）。
2. **fix 前に実行して fail を確認**（one-hop が中間 ref を型不一致で落とすため、結果が空/None になり assert 失敗）。これが各サイトが**真の候補である証拠**。
3. レシピA/B を適用。
4. テスト pass を確認。

**ゲート（重要）**: もしテストが fix 前に **fail しない**（= 二重間接でも結果が変わらない / その経路が観測不能）なら、そのサイトは真の候補ではない。**no-op 変更を強制せず**、サイトを skip し、その旨を PR 説明と本 plan のチェックに記録する（design の「分類こそ作業」方針）。

---

## Task 1: fonts.rs（レシピA × 3サイト）

**Files:**
- Modify: `crates/flpdf/src/fonts.rs`（`collect_page_fonts`: /Resources 値の resolve、/Font 値の resolve、font エントリ値の resolve。現状 `resolve_borrowed(reference)?.as_dict().cloned()` ×2 と `Object::Reference(font_ref) => resolve_borrowed(*font_ref)?`）
- Test: `crates/flpdf/tests/fonts_tests.rs`

**対象サイト（関数 `collect_page_fonts` 内、行は変動するので関数＋スニペットで特定）:**
1. `/Resources` 値が `as_ref_id()` のとき `resolve_borrowed(reference)?.as_dict().cloned()` → レシピA。
2. `/Font`（Resources 内）値が `as_ref_id()` のとき同上 → レシピA。
3. fonts_dict iter の `Object::Reference(font_ref) => resolve_borrowed(*font_ref)?`（後続で Dictionary/Stream に match）→ レシピA（terminal を後続 match に渡す）。

**Steps:**
1. **Step 1（テスト）**: `tests/fonts_tests.rs` に3テスト追加 — (a) ページ `/Resources` を `(R,"R2 0 R")→(R2, "<< /Font ... >>")` の2ホップにし font が収集されること、(b) `/Font` を2ホップに、(c) 個別 font エントリを2ホップに。既存の font 収集 public API（`fonts_tests.rs` 先頭の呼び出しを踏襲）で `assert!(!fonts.is_empty())` 等。
2. **Step 2**: `cargo test -p flpdf --test fonts_tests <new_test_names>` → **FAIL**（one-hop が dict を落とし font 空）を確認。
3. **Step 3**: レシピA を3サイトに適用。`use` 追加。二重クローン回避（move 取り出し）。
4. **Step 4**: `cargo test -p flpdf --test fonts_tests` → PASS。`cargo clippy -p flpdf` 警告なし。
5. **Step 5（commit）**:
   ```bash
   git add crates/flpdf/src/fonts.rs crates/flpdf/tests/fonts_tests.rs
   git commit -m "fix(flpdf): follow holder chains in fonts.rs font collection (flpdf-3x23)"
   ```
6. **Step 6（パイロット検証 — fan-out 前に必ず実行）**: `scripts/patch-coverage.sh --base main` を回し、fonts.rs 変更行が 100% カバーされることを確認する。これは Task 1 が以下を end-to-end で検証するパイロット: `into_dict()` move イディオム、`build_pdf` 2ホップテスト形、red-gate が実際に赤くなる、**カバレッジゲートが実変更行で通る**。ここで失敗したらレシピを修正してから Task 2 以降に進む（借用チェックの問題が出るのも通常ここだけ — `resolve_ref_chain` は所有を返し `resolve_borrowed` の借用を解放するため、むしろ改善することが多い）。

---

## Task 2: embedded_files.rs（レシピA/B × 5サイト）

**Files:**
- Modify: `crates/flpdf/src/embedded_files.rs`
- Test: `crates/flpdf/tests/embedded_files_tests.rs`（無ければ src `#[cfg(test)]` mod、ただし `common` 不可のため公開 API を叩く integration を優先）

**対象サイト:**
1. `remove_ref_from_af_in_dict`: `/AF` 値 `Object::Reference(r) => match resolve_borrowed(r)? { Object::Array(arr) => (Some(r), arr.clone()) }` → **レシピB**（`array_ref` を後で `set_object` に使うため `.1`=terminal_ref を `Some(...)` に入れる）。
2. `list_embedded_files_with_max_depth`: `/Names` 値 `Object::Reference(r) => match resolve_borrowed(r)? { Object::Dictionary(d) => d.clone() }` → レシピA。
3. `collect_embedded_file_pairs_raw`: `/Names` 値同パターン → レシピA。
4. `rebuild_embedded_files_tree`（names_dict_opt 取得）: `/Names` 値 `Object::Reference(r) => match { Object::Dictionary(d) => Some((Some(r), d.clone())) }` → **レシピB**（`Some(r)` を names_ref として後段 `set_object` に使用 → terminal_ref）。
5. `rebuild_embedded_files_tree`（`(names_ref, mut names_dict)` 取得）: `Object::Reference(r) => match { Object::Dictionary(d) => (r, d.clone()) }` → **レシピB**（`names_ref` は `set_object(names_ref, ...)` 対象 → terminal_ref）。

**Steps（TDD、各サイト1テスト）:**
1. テスト: `/AF` 配列、`/Names` dict を2ホップ carrier で格納した PDF を作り、(2)(3) は `list_embedded_files` が列挙する、(1)(4)(5) は AF 除去/tree 再構築の公開操作が二重間接の terminal を正しく書き換えることを assert。
   - **レシピB の正しい assertion（効果ベース）**: 書換は terminal_ref（内側オブジェクト）に着地し、carrier `r` は引き続き terminal を指す正当な中間ホップなので **orphan にはならない**。「carrier が orphan GC される」を assert してはならない（fix 後も pass しない）。代わりに **操作の効果**を assert する: 例 (1) なら「除去後に resolved `/AF` 配列が除去 ref を含まない」、(4)(5) なら「二重間接の `/Names` 経由で tree が再構築され列挙/参照が成立する」。
2. fix 前 FAIL 確認。
3. レシピ適用（1/4/5=B、2/3=A）。
4. PASS + clippy。
5. commit: `fix(flpdf): follow holder chains in embedded_files.rs /Names and /AF handling (flpdf-3x23)`

---

## Task 3: filespec_helper.rs（レシピA × 6サイト）

**Files:**
- Modify: `crates/flpdf/src/filespec_helper.rs`
- Test: `crates/flpdf/tests/filespec_helper_tests.rs`

**対象サイト（全 read-then-typecheck）:**
1. `/Params` 値 `Object::Reference(r) => match resolve_borrowed(r)? { Object::Dictionary(d) => Some(d.clone()) }`。
2. `resolve_dict` 周辺の `/EF` 値 `Object::Reference(r) => match { Object::Dictionary(d) => d.clone() }`（line ~381）。
3. `/EF` 候補 stream: `for ef_ref in candidates { if let Object::Stream(stream) = resolve_borrowed(ef_ref)? }`（candidates は `ef_dict.get(k).and_then(as_ref_id)`）→ レシピA（terminal を Stream match）。
4. `embed_pairs` 系（line ~1165）: filespec 値 `Object::Reference(r) => match resolve_borrowed(r) { Ok(Object::Dictionary(d)) => d.clone() }`。
5. `/EF` 値（line ~1178）同パターン。
6. `/EF` エントリ stream（line ~1194）: `if let Some(Object::Reference(r))=ef_dict.get(k) { if let Ok(Object::Stream(s))=resolve_borrowed(r) }`。

**除外確認（plan に明記、変更しない）**: `sanitize_imported_object`（line ~1020、`visited.insert(r)` 後に resolve→再帰）は既に再帰で chain 追従済みのため対象外。

**Steps:** 各サイトに2ホップ holder-chain テスト（filespec の `/EF`・`/Params`・EF stream を carrier 経由に）。fix 前 FAIL → レシピA → PASS → commit。
- commit: `fix(flpdf): follow holder chains in filespec_helper.rs /EF and /Params resolution (flpdf-3x23)`

---

## Task 4: resources.rs（レシピA × 3サイト）

**Files:**
- Modify: `crates/flpdf/src/resources.rs`
- Test: `crates/flpdf/tests/resource_pruning_tests.rs`

**対象サイト（全 read-then-typecheck）:**
1. `page_resources.get("XObject")` 値 `Object::Reference(cat_ref) => match resolve_borrowed(cat_ref)? { Object::Dictionary(xobj_dict) => ... }`（line ~757）。
2. `xobj_dict.get(name)` 値 `Object::Reference(xobj_ref) => resolve_borrowed(xobj_ref)?`（後続で Stream match、line ~779）。
3. Form XObject の `stream.dict.get("Resources")` 値 `Object::Reference(r) => match resolve_borrowed(r)? { Object::Dictionary(d) => Some(d.clone()) }`（line ~844）。

**除外確認（A=object-by-own-ref、変更しない）**: 332(`ResourcesLoc::Indirect`)、338、348、476、892、911、943、1045、1076 は own-ref。

**Steps:** resource pruning の公開操作（`resource_pruning_tests.rs` の手本）で、`/XObject` category・XObject stream・form `/Resources` を2ホップ carrier 経由にし、prune が正しく対象を解決すること（リソースが誤って drop/温存されない）を assert。fix 前 FAIL → レシピA → PASS → commit。
- commit: `fix(flpdf): follow holder chains in resources.rs /XObject and form /Resources (flpdf-3x23)`

---

## Task 5: pages.rs（レシピA × 4サイト）

**Files:**
- Modify: `crates/flpdf/src/pages.rs`
- Test: `crates/flpdf/tests/content_stream_tests.rs`（または `page_extract_tests.rs`、`/Contents` を扱う既存ファイル）

**対象サイト:**
1. `/Contents` 単一参照 `Object::Reference(r) => match resolve_borrowed(*r)? { Object::Stream(s) => ... }`（line ~162）。
2. `/Contents` 配列要素 `Object::Reference(r) => resolve_borrowed(*r)?`→Stream（line ~177）。
3. coalesce 経路の `/Contents` 配列要素同（line ~312）。
4. `/Resources` 継承解決 `Object::Reference(r) => match resolve_borrowed(r)? { Object::Dictionary(d) => Some(d.clone()) }`（line ~476、`/Resources` 値。**注**: これは `dict.get("Resources")` 値の解決で C。ただし周辺の `current` 走査は B のため、resolve 対象が値か own-ref か実コードで最終確認）。

**除外確認（A/B）**: 96, 261, 381, 570（own-ref catalog/page）、459, 610（tree-walker）は対象外。

**Steps:** `/Contents`（単一・配列）を2ホップ carrier 経由にして page content bytes 抽出が成立すること、`/Resources` 継承値を2ホップにして解決されることを assert。fix 前 FAIL → レシピA → PASS → commit。
- commit: `fix(flpdf): follow holder chains in pages.rs /Contents and inherited /Resources (flpdf-3x23)`

---

## Task 6: page_object_helper.rs（レシピA × 1サイト）

**Files:**
- Modify: `crates/flpdf/src/page_object_helper.rs`
- Test: `crates/flpdf/tests/annotation_helper_tests.rs`（または page_object_helper の annots を叩く既存ファイル）

**対象サイト:**
1. `/Annots` 値 `Object::Reference(r) => match resolve_borrowed(r)? { Object::Array(arr) => arr.clone() }`（`get_annotations`、line ~383）→ レシピA。

**除外確認**: 189, 365, 680（own-ref page）、621（tree-walker）、636/694（座標 box 配列 = leaf、design で除外）は対象外。

**Steps:** `/Annots` を2ホップ carrier 経由の配列にし、annotation 列挙が成立すること（空にならない）を assert。fix 前 FAIL → レシピA → PASS → commit。
- commit: `fix(flpdf): follow holder chain in page_object_helper.rs /Annots resolution (flpdf-3x23)`

---

## Task 7: page_label_document_helper.rs（レシピA × 1サイト）

**Files:**
- Modify: `crates/flpdf/src/page_label_document_helper.rs`
- Test: src 既存 `#[cfg(test)]` mod（line ~499、`common` 不可なので local fixture か、公開 API があれば tests/ へ）

**対象サイト:**
1. number-tree クロージャ内 label range: `Object::Reference(r) => resolve_borrowed(r)?.as_dict().cloned()`（line ~280）→ レシピA。

**除外確認**: 167（leaf scalar /S/P/St、design で除外）、247/457（own-ref catalog）は対象外。

**Steps:** `/PageLabels` の `/Nums` エントリ（label range dict）を2ホップ carrier 経由にし、label が解決されることを assert。fix 前 FAIL → レシピA → PASS → commit。
- commit: `fix(flpdf): follow holder chain in page_label range dict resolution (flpdf-3x23)`

---

## Task 8: appearance.rs（レシピA × 2〜3サイト）

**Files:**
- Modify: `crates/flpdf/src/appearance.rs`
- Test: appearance の公開操作を叩く既存 tests/ ファイル（appearance 生成系）

**対象サイト（構造的のみ。leaf scalar 22件中の大半は design で除外）:**
1. `/AP` 値 `Some(Object::Reference(r)) => match resolve(r)? { Object::Dictionary(d) => d, _ => Dictionary::new() }`（line ~157）→ レシピA。
2. `/AcroForm` 値 `Some(Object::Reference(r)) => resolve_borrowed(r)?.as_dict().cloned()`（line ~1152）→ レシピA。
3. line ~1186 `Some(Object::Reference(r)) => Ok(resolve(r)?.into_dict())` → **実コードで解決対象を確認**。dict（/MK・/DR 等の構造的）なら レシピA、leaf なら除外。

**除外確認（leaf/own-ref/tree-walker、変更しない）**: 144,486,572,778,1142,1255,1295(own-ref widget/root)、928,987,1032,1100,1231(inherited /Parent walk = B)、293,518,601,616,946,1050,1117,1165,1277,1307,1629,1701,1745,1759,1792,1837,1857,1869,1899(leaf scalar/座標配列、design 除外)。

**Steps:** `/AP`・`/AcroForm` を2ホップ carrier 経由にし、appearance 生成/AcroForm DA 解決が成立することを assert。1186 は対象判定後に処理。fix 前 FAIL → レシピA → PASS → commit。
- commit: `fix(flpdf): follow holder chains in appearance.rs /AP and /AcroForm (flpdf-3x23)`

---

## Task 9: 最終ゲート

**Steps:**
1. `cargo fmt --all` → `cargo fmt --all --check`（[[memory: flpdf-ci-quality-fmt-check]]）。
2. `cargo clippy --workspace --all-targets -- -D warnings`。
3. `cargo test -p flpdf`（全 green）。
4. **patch-coverage ゲート**: 全 commit 後に `scripts/patch-coverage.sh --base main`。flpdf 変更行 **100%** 必須。未カバー行はテスト追加 or `// cov:ignore: <理由>`。
5. **質的チェック**（CLAUDE.md step4）: 各 holder-chain テストが「fix 前に fail する実質的 assertion」であることを再確認（単なる行実行でない）。
6. PR 説明に: 対象/除外サイトの分類根拠、skip したサイト（fix 前 fail せず）と理由、`cov:ignore` があれば理由。

**最終確認**: skip 判定で実際に変更したサイト数が design の ~25 から減った場合、残りを follow-up issue 化するか PR 説明に明記。
