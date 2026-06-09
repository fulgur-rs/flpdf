# link-annot / OpenAction destination null-out 実装計画 (flpdf-9hc.20.33)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `--pages` でページを subset したとき、生存ページの link annotation (`/Dest`・`/A /GoTo /D`) および catalog `/OpenAction` のみから参照される削除ページを、qpdf 11.9.0 と同じく `null` object に置換する（destination 参照は verbatim 保持）。

**Architecture:** `outline_dest_remap.rs::remap_outline_and_dests_with_max_depth` に Step 4 を追加する。annotation は outline item と同一構造（`/Dest`・`/A /GoTo /D` を持つ dict）なので、既存の `null_removed_item_targets` + `remap_item_dest` をそのまま annotation に適用する。`/OpenAction`（dest 配列／GoTo action dict／indirect いずれの形態でも）は既存 `remap_or_null_dest` 1 発で処理する。新規ロジックは「生存ページの `/Annots` を列挙して各 annot に適用する walk」と「catalog `/OpenAction` の取得・書き戻し」のみ。

**Tech Stack:** Rust, flpdf crate, qpdf 11.9.0 (`/usr/bin/qpdf`, parity truth source)。

**前提確認 (実装前に 1 度だけ):** `RebuildResult.new_kids: Vec<ObjectRef>` が「rebuild 後の新 page tree の kids = 生存ページの新 ref 列」であること（`page_tree_rebuild.rs:89-93, 246` で確認済み）。annotation 内の `/Dest` は rebuild では touch されず元 page ref を指したままで、`remap_item_dest`(survive→new) / `null_removed_item_targets`(removed→null) が old ref ベースの `surviving: BTreeMap<old, new>` で処理する前提。

---

## 背景: qpdf 11.9.0 観測結果（design フィールドにも記録済み）

`qpdf in.pdf --pages . 1,3 -- out.pdf` で 3p の page2 を削除、page2 を 1 経路のみから参照させて `--qdf` 検査:

- link annot `/Dest`             → page を **null 化**、`/Dest [N 0 R /Fit]` 保持
- link annot `/A /GoTo /D`        → page を **null 化**、`/A /D [N 0 R /Fit]` 保持
- catalog `/OpenAction /GoTo /D`  → page を **null 化**、`/OpenAction /D` 保持
- thread-bead `/P` / struct `/Pg` / 任意キー → 参照ごと **drop**、page は GC（**本 issue スコープ外** → flpdf-9hc.20.34 / .35）

→ null 化は GoTo destination セマンティクス特有。本 issue は destination null-out ファミリーの未対応分（annot・OpenAction）を既存 walk に追加する。

---

## Task 1: link annotation `/Dest` の null-out（unit test → 実装）

**Files:**
- Modify: `crates/flpdf/src/outline_dest_remap.rs`（Step 4 追加・新規 fn 2 個・module doc 追記）
- Test: `crates/flpdf/src/outline_dest_remap.rs`（`#[cfg(test)] mod tests`）

**Step 1: テスト用フィクスチャ helper を追加**

既存テストモジュールの PDF ビルダー（`build_outline_pdf` など、`outline_dest_remap.rs` 末尾の `mod tests`）の流儀に合わせ、3 ページ + 生存 page1 に link annotation を持つ最小 PDF を build する helper を追加する。annotation の `/Dest [<page2ref> /Fit]` が削除対象 page2 を指す。outline / named-dest は **置かない**（このケースを既存 walk が拾わないことを保証するため）。

```rust
// in mod tests
/// 3-page PDF; page1 carries a link annotation whose /Dest targets `dest_page`
/// (1-based). No outline, no named dests — isolates the annotation path.
fn build_annot_dest_pdf(dest_page: usize) -> Vec<u8> {
    // catalog(1) -> pages(2) -> page1(3, /Annots [6 0 R]) page2(4) page3(5)
    // annot(6): << /Type /Annot /Subtype /Link /Rect [0 0 50 50]
    //              /Dest [ <3+dest_page-1> 0 R /Fit ] >>
    // ...オフセットを正しく計算して xref を組む（既存 helper のスタイルに倣う）
}
```

**Step 2: 失敗するテストを書く**

```rust
#[test]
fn annot_dest_to_removed_page_is_nulled() {
    // page2 (obj 4) を削除し page1,page3 を残す subset を rebuild し、
    // remap_outline_and_dests を呼ぶ。
    let bytes = build_annot_dest_pdf(2);
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
    let page_refs = pdf.page_refs().unwrap();           // [p1, p2, p3]
    let selected = vec![page_refs[0], page_refs[2]];     // keep p1, p3
    let result = crate::page_tree_rebuild::rebuild_page_tree(&mut pdf, &selected).unwrap();
    remap_outline_and_dests(&mut pdf, &result).unwrap();

    // 削除された page2 (obj 4) が null になっている
    assert!(matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null));
    // annot(6) の /Dest は verbatim 保持（[4 0 R /Fit] のまま）
    let annot = pdf.resolve(ObjectRef::new(6, 0)).unwrap();
    let dest = annot.as_dict().unwrap().get("Dest").unwrap();
    // first element はまだ 4 0 R を指す
    // ...
}
```

（ObjectRef の正確な API・page_refs/open_mem_owned の正確なシグネチャは既存テストを参照して合わせる。）

**Step 3: テストを実行して失敗を確認**

Run: `cargo test -p flpdf annot_dest_to_removed_page_is_nulled -- --nocapture`
Expected: FAIL（現状 page2 は full Page dict のまま＝`Object::Null` でない）

**Step 4: Step 4（実装）を `remap_outline_and_dests_with_max_depth` に追加**

`remap_outline_and_dests_with_max_depth` の `Ok(())` 直前（Step 3 の後）に追加:

```rust
    // --- Step 4: Link-annotation and /OpenAction destinations -------------
    // qpdf nulls a removed page reached only via a surviving page's link
    // annotation (/Dest or /A /GoTo /D) or the catalog /OpenAction, keeping the
    // destination reference verbatim — the same null-out family as outlines and
    // named destinations. (A removed page reached only via a thread-bead /P or a
    // struct element /Pg is a different, drop-and-GC family handled separately.)
    remap_annot_dests(pdf, result, &surviving)?;
    remap_open_action_dest(pdf, catalog_ref, &surviving)?;

    Ok(())
}
```

新規 fn（`remap_outline_and_dests_with_max_depth` の後ろ、helper 群の近くに配置）:

```rust
/// Null/remap link-annotation destinations on every surviving page (qpdf
/// `--pages` parity). An annotation is structurally identical to an outline
/// item for destination purposes (`/Dest` and `/A /GoTo /D`), so the same
/// [`null_removed_item_targets`] + [`remap_item_dest`] pair applies: a removed
/// target page is replaced with `null` (its `/Dest`/`/D` reference kept
/// verbatim), a surviving target is remapped to its new ref. Both helpers are
/// idempotent, so a page or annotation reached more than once is safe.
fn remap_annot_dests<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    for &page_ref in &result.new_kids {
        // /Annots may be an inline array or an indirect reference to one.
        let annots_val = {
            let page_obj = pdf.resolve_borrowed(page_ref)?;
            let Some(page) = page_obj.as_dict() else {
                continue;
            };
            page.get("Annots").cloned()
        };
        let annot_refs: Vec<ObjectRef> = match annots_val {
            Some(Object::Array(arr)) => arr.iter().filter_map(Object::as_ref_id).collect(),
            Some(Object::Reference(r)) => match pdf.resolve_borrowed(r)? {
                Object::Array(arr) => arr.iter().filter_map(Object::as_ref_id).collect(),
                _ => Vec::new(),
            },
            // Inline-dict annotations have no object to rewrite; skip (rare).
            _ => Vec::new(),
        };
        for annot_ref in annot_refs {
            null_removed_item_targets(pdf, annot_ref, surviving)?;
            remap_item_dest(pdf, annot_ref, surviving)?;
        }
    }
    Ok(())
}

/// Null/remap the catalog `/OpenAction` destination (qpdf `--pages` parity).
/// `/OpenAction` is either a destination array `[page /Fit ...]` or a GoTo
/// action dict `<< /S /GoTo /D [page /Fit] >>` (possibly indirect); both forms
/// expose the page ref through [`remap_or_null_dest`] (array first element or
/// dict `/D`). A surviving target is remapped, a removed target is nulled.
fn remap_open_action_dest<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    catalog_ref: ObjectRef,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    let oa = {
        let catalog_obj = pdf.resolve_borrowed(catalog_ref)?;
        let Some(catalog) = catalog_obj.as_dict() else {
            return Ok(());
        };
        catalog.get("OpenAction").cloned()
    };
    let Some(oa) = oa else {
        return Ok(());
    };
    // For an indirect /OpenAction, remap_or_null_dest rewrites the referenced
    // object in place and returns it unchanged; re-storing the same value is a
    // no-op. For a direct value this applies the remap/null result.
    let updated = remap_or_null_dest(pdf, oa, surviving)?;
    let catalog_obj = pdf.resolve_borrowed(catalog_ref)?;
    if let Some(mut catalog) = catalog_obj.as_dict().cloned() {
        catalog.insert("OpenAction", updated);
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));
    }
    Ok(())
}
```

注意（review patterns）:
- `null_removed_item_targets` / `remap_item_dest` / `remap_or_null_dest` は同一モジュール内の既存 `fn`。直接呼べる。
- `/Annots`・`/Dest`・`/A`・`/D` の間接参照解決と深さ bound は既存ヘルパー内（`resolve_ref_chain` / `MAX_DEST_RESOLVE_DEPTH`）で完結。Step 4 側で新たな深さ制御は不要。
- `.clone()` は `/Annots` 値と annot ref 列挙の最小限に留める（page dict 全体を deep clone しない）。

**Step 5: テストを実行して成功を確認**

Run: `cargo test -p flpdf annot_dest_to_removed_page_is_nulled`
Expected: PASS

**Step 6: コミット**

```bash
git add crates/flpdf/src/outline_dest_remap.rs
git commit -m "feat(flpdf): null-out removed pages reached via link-annot /Dest (flpdf-9hc.20.33)"
```

---

## Task 2: link annotation `/A /GoTo /D` の null-out（test で確認）

Task 1 の Step 4 実装で `remap_item_dest` / `null_removed_item_targets` が `/A /GoTo /D` も処理するため、追加実装は不要。テストで保証する。

**Step 1: フィクスチャ helper（`/A` 形態）と失敗テストを追加**

```rust
fn build_annot_goto_pdf(dest_page: usize) -> Vec<u8> {
    // annot(6): << /Subtype /Link /Rect [...]
    //   /A << /Type /Action /S /GoTo /D [ <dest_page ref> /Fit ] >> >>
}

#[test]
fn annot_goto_action_to_removed_page_is_nulled() {
    // page2 削除後、page2 が null、annot の /A /D は保持。
}
```

**Step 2: 実行 → PASS を確認**（実装済みのため最初から通る想定。通らなければ実装を見直す）

Run: `cargo test -p flpdf annot_goto_action_to_removed_page_is_nulled`
Expected: PASS

**Step 3: コミット**

```bash
git add crates/flpdf/src/outline_dest_remap.rs
git commit -m "test(flpdf): cover link-annot /A /GoTo /D null-out (flpdf-9hc.20.33)"
```

---

## Task 3: catalog `/OpenAction` の null-out（test）

**Step 1: フィクスチャ helper（`/OpenAction`）と失敗テストを追加**

action dict 形態 (`/OpenAction << /S /GoTo /D [...] >>`) と dest 配列形態 (`/OpenAction [page /Fit]`) の両方をカバーする 2 テスト。

```rust
#[test]
fn open_action_goto_to_removed_page_is_nulled() { /* ... */ }
#[test]
fn open_action_dest_array_to_removed_page_is_nulled() { /* ... */ }
```

**Step 2: 実行**

Run: `cargo test -p flpdf open_action`
Expected: PASS（Task 1 で `remap_open_action_dest` 実装済み）

**Step 3: コミット**

```bash
git commit -am "test(flpdf): cover catalog /OpenAction null-out (flpdf-9hc.20.33)"
```

---

## Task 4: 生存ページ destination の remap 回帰テスト

削除ではなく**生存**ページを指す annot `/Dest` が、新 page ref に正しく remap されること（acceptance #4、回帰防止）。

**Step 1: テスト追加**

```rust
#[test]
fn annot_dest_to_surviving_page_is_remapped() {
    // page2 を削除、annot の /Dest は page3 (survive) を指す。
    // remap 後、annot /Dest first element が page3 の新 ref になる。
}
```

**Step 2: 実行 → PASS**

Run: `cargo test -p flpdf annot_dest_to_surviving_page_is_remapped`

**Step 3: コミット**

```bash
git commit -am "test(flpdf): annot dest to surviving page is remapped (flpdf-9hc.20.33)"
```

---

## Task 5: qpdf 11.9.0 parity 統合テスト

`crates/flpdf-cli/tests/` または `crates/flpdf/tests/` に、qpdf とバイト/構造を突き合わせる parity テストを追加する。`page_ops_qpdf_matrix.rs` の `qpdf_available()` gate と `run_qpdf` / `--qdf` 正規化スタイルに倣う。

**Files:**
- Create: `crates/flpdf-cli/tests/cli_pages_annot_nullout_qpdf.rs`（または既存 matrix に追加）

**Step 1: テスト追加（qpdf gate）**

3 フィクスチャ（annot `/Dest`・annot `/A /GoTo /D`・catalog `/OpenAction`）を一時ファイルに書き、`flpdf --pages . 1,3` と `qpdf . --pages . 1,3` の両出力を `qpdf --qdf --object-streams=disable` で正規化し、削除ページが両者とも `null` object になること（および destination 参照が保持されること）を確認する。

**Step 2: 実行**

Run: `cargo test -p flpdf-cli --test cli_pages_annot_nullout_qpdf`
Expected: PASS（qpdf 不在環境では gate により早期 return）

**Step 3: コミット**

```bash
git add crates/flpdf-cli/tests/cli_pages_annot_nullout_qpdf.rs
git commit -m "test(flpdf): qpdf 11.9.0 parity for link-annot/OpenAction null-out (flpdf-9hc.20.33)"
```

---

## Task 6: module doc 更新・品質ゲート

**Step 1: `outline_dest_remap.rs` の module doc (`//!`) に annot/OpenAction を追記**

冒頭の挙動説明（現状 outline / `/Names /Dests` / legacy `/Dests` のみ列挙）に、link annotation `/Dest`・`/A /GoTo /D`・catalog `/OpenAction` も同じ null-out ファミリーである旨を 1〜2 文で追加する。**英語**で書く（公開 doc 面・doc review patterns 準拠）。issue ID や内部ジャーゴンを doc に書かない。qpdf 観測挙動を根拠として記述する。

**Step 2: 品質ゲート**

```bash
cargo fmt --all                 # CI Quality gate (fmt --check が回る) — 必ず実行
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test                      # 全 workspace（baseline からの回帰なし）
```

Expected: すべて成功、0 failed、fmt 差分なし。

**Step 3: コミット**

```bash
git commit -am "docs(flpdf): document link-annot/OpenAction null-out family (flpdf-9hc.20.33)"
```

---

## 完了基準（acceptance）

1. 削除ページが link annot `/Dest` のみから参照される場合、`--pages` 後に null object になり `/Dest` 参照が保持される（qpdf 一致）。
2. `/A /GoTo /D` 経由でも同様。
3. catalog `/OpenAction /GoTo /D` 経由でも同様。
4. survive ページを指す destination は新 ref に remap される（回帰なし）。
5. bead `/P` / struct `/Pg` は本スコープ外（flpdf-9hc.20.34 / .35 に記録済み）。
6. `cargo fmt --check` / `clippy -D warnings` / 全 test が通る。
