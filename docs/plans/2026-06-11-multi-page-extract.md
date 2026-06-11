# Multi-Page Extract Implementation Plan (flpdf-5h5.4)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `extract_pages(source, &[usize])` — qpdf `--pages` 互換（任意順序・重複可）の複数ページ抽出。共有リソースはちょうど1部だけコピーされる。

**Architecture:** 既存 `page_extract.rs` を拡張。unique 選択ページの closure の**和集合**を1回の `copy_objects` でコピー（単一 renumber map = 共有リソース dedup）。重複出現は materialize 済みページ辞書の shallow clone（`page_tree_rebuild` と同方式、qpdf 11.9.0 parity）。neutralization は `keep: ObjectRef` を `keep: &BTreeSet<ObjectRef>` に一般化。`extract_page` は `extract_pages(source, &[i])` への委譲となり既存テストが回帰ガード。

**Tech Stack:** Rust, flpdf 内部 API（`page_object_closure`, `copy_objects`, `rewrite_refs`, `sweep_unreachable_objects`）。

**Design:** beads issue `flpdf-5h5.4` の design フィールド（`bd show flpdf-5h5.4`）。

**作業ディレクトリ:** `/home/ubuntu/flpdf/.worktrees/flpdf-5h5-4-multi-extract`（branch `feature/flpdf-5h5.4-multi-page-extract`）

**コーディング規約:** `.claude/rules/pdf-rust-review-patterns.md`（不要 clone 禁止・間接参照 resolve・符号なしキャスト検証・走査境界）と `.claude/rules/pdf-rust-doc-review-patterns.md`（公開 doc に issue ID 禁止・英語のみ・# Errors 必須）を必ず守る。

---

## Tasks

### Task 1: neutralization helpers の keep を集合に一般化（純リファクタ）

**Files:**
- Modify: `crates/flpdf/src/page_extract.rs`

**Step 1: 署名変更**

以下の関数の `keep: ObjectRef` を `keep: &BTreeSet<ObjectRef>` に変更:
`neutralize_absent_dests`, `neutralize_bead_ring`, `neutralize_annot_if_absent`,
`neutralize_aa_if_absent`, `neutralize_action_chain`, `neutralize_action_array`,
`dest_targets_absent_page`, `sd_targets_absent_page`, `p_targets_absent_page`。

判定箇所の変更（3箇所、いずれも同型）:

```rust
// dest_targets_absent_page
Ok(match dest_page_ref_resolved(target, dest)? {
    Some(page_ref) => !keep.contains(&page_ref),
    None => false,
})

// sd_targets_absent_page
Ok(match pg_ref {
    Some(r) => !keep.contains(&r) && is_page_dict(&pg_concrete),
    None => false,
})

// p_targets_absent_page
Ok(match p_ref {
    Some(r) => !keep.contains(&r) && is_page_dict(&concrete),
    None => false,
})
```

`neutralize_absent_dests` / `neutralize_bead_ring` は「対象ページ」(`page_ref`) と「生存集合」(`keep`) の両方が要るので **引数を2つ持つ**形にする:
`fn neutralize_absent_dests(target, page_ref: ObjectRef, keep: &BTreeSet<ObjectRef>)`。
`extract_page` 内の呼び出しは暫定で
`let keep = BTreeSet::from([copied_page_ref]); neutralize_absent_dests(&mut target, copied_page_ref, &keep)?;`。

doc コメントの「`keep` (the page)」表現は「any page in `keep`」に更新（英語のみ、issue ID 書かない）。

**Step 2: テストが全部通ることを確認（挙動不変）**

Run: `cargo test -p flpdf --test page_extract_tests --test page_extract_outline_nullout_tests --test page_extract_structtree_pg_tests --test page_extract_thread_bead_p_tests`
Expected: 全 PASS（リファクタのみ、挙動変化なし）

**Step 3: Commit**

```bash
git add crates/flpdf/src/page_extract.rs
git commit -m "refactor(flpdf): generalize extract neutralization keep to a page set"
```

---

### Task 2: extract_pages コア（union closure・順序保持）+ extract_page 委譲

**Files:**
- Modify: `crates/flpdf/src/page_extract.rs`
- Modify: `crates/flpdf/src/lib.rs`（`pub use page_extract::{extract_page, extract_pages};`）
- Test: `crates/flpdf/tests/page_extract_tests.rs`

**Step 1: failing test を書く**

`page_extract_tests.rs` に追加。既存ヘルパー `build_pdf` / `pages_dict` を使う。

```rust
/// Three-page document; pages 3 and 4 SHARE font 7; page 5 has its own font 8.
fn three_page_shared_font_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F2 8 0 R >> >> >>"),
            (6, "<< /Length 15 >>\nstream\nBT /F1 12 Tf ET\nendstream"),
            (7, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (8, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
        ],
        1,
    )
}

/// Count objects in `doc` whose dict matches a /Type /Font with `base`.
fn count_font_objects(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, base: &[u8]) -> usize {
    doc.object_refs()
        .iter()
        .filter(|&&r| {
            doc.resolve_borrowed(r)
                .ok()
                .and_then(|o| o.as_dict().cloned())
                .is_some_and(|d| {
                    matches!(d.get("Type"), Some(Object::Name(n)) if n == b"Font")
                        && matches!(d.get("BaseFont"), Some(Object::Name(n)) if n == base)
                })
        })
        .count()
}

#[test]
fn extract_pages_copies_shared_resource_once() {
    let bytes = three_page_shared_font_pdf();
    let mut src = Pdf::open_mem_owned(bytes).unwrap();
    let mut doc = flpdf::extract_pages(&mut src, &[0, 1]).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2, "two extracted pages");
    let root = pages_dict(&mut doc);
    assert!(matches!(root.get("Count"), Some(Object::Integer(2))));
    // The shared Helvetica font exists EXACTLY once; Courier (page 3) absent.
    assert_eq!(count_font_objects(&mut doc, b"Helvetica"), 1);
    assert_eq!(count_font_objects(&mut doc, b"Courier"), 0);
}

#[test]
fn extract_pages_object_count_sublinear_vs_per_page_extracts() {
    let bytes = three_page_shared_font_pdf();
    let mut src = Pdf::open_mem_owned(bytes.clone()).unwrap();
    let combined = flpdf::extract_pages(&mut src, &[0, 1]).unwrap().object_refs().len();
    let mut s1 = Pdf::open_mem_owned(bytes.clone()).unwrap();
    let one = extract_page(&mut s1, 0).unwrap().object_refs().len();
    let mut s2 = Pdf::open_mem_owned(bytes).unwrap();
    let two = extract_page(&mut s2, 1).unwrap().object_refs().len();
    assert!(
        combined < one + two,
        "shared font must be copied once: {combined} >= {one} + {two}"
    );
}

#[test]
fn extract_pages_preserves_selection_order() {
    let bytes = three_page_shared_font_pdf();
    let mut src = Pdf::open_mem_owned(bytes).unwrap();
    let mut doc = flpdf::extract_pages(&mut src, &[2, 0]).unwrap();
    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2);
    // First output page is source page 3 (Courier), second is page 1 (Helvetica).
    let leaf0 = doc.resolve_borrowed(refs[0]).unwrap().as_dict().cloned().unwrap();
    let leaf1 = doc.resolve_borrowed(refs[1]).unwrap().as_dict().cloned().unwrap();
    let font_name = |leaf: &flpdf::Dictionary| -> Vec<u8> {
        // /Resources /Font の最初のエントリの BaseFont を doc から解決して返す
        // (実装時: get("Resources") → /Font dict → 値の ref を resolve → BaseFont)
        ...
    };
    assert_eq!(font_name(&leaf0), b"Courier".to_vec());
    assert_eq!(font_name(&leaf1), b"Helvetica".to_vec());
}
```

（`font_name` ヘルパーは実装時に具体化。リーフの /Resources はインラインなので
`leaf.get("Resources")` → `/Font` dict → 値の `Object::Reference` を
`doc.resolve_borrowed` → `BaseFont` 名を返す。）

**Step 2: fail を確認**

Run: `cargo test -p flpdf --test page_extract_tests extract_pages 2>&1 | tail -5`
Expected: COMPILE FAIL（`extract_pages` 未定義）

**Step 3: 実装**

`page_extract.rs` の `extract_page` 本体を `extract_pages` に一般化:

```rust
/// Inherited attributes resolved from the source page tree, captured before
/// copying severs the /Parent chain.
struct InheritedAttrs {
    resources: Option<Dictionary>,
    rotate: i32,            // resolve_inherited_rotate_with_max_depth の戻り型に合わせる
    mediabox: Option<Object>,
    cropbox: Option<Object>,
}

pub fn extract_pages<R: Read + Seek>(
    source: &mut Pdf<R>,
    page_indices: &[usize],
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    if page_indices.is_empty() {
        return Err(Error::Unsupported("empty page selection".to_string()));
    }
    let all_pages = page_refs(source)?;
    let mut selected: Vec<ObjectRef> = Vec::with_capacity(page_indices.len());
    for &idx in page_indices {
        selected.push(*all_pages.get(idx).ok_or_else(|| {
            Error::Unsupported(format!(
                "page index {idx} out of range (document has {} pages)",
                all_pages.len()
            ))
        })?);
    }
    // Unique source pages, first-occurrence order (deterministic output).
    let mut unique: Vec<ObjectRef> = Vec::new();
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    for &r in &selected {
        if seen.insert(r) {
            unique.push(r);
        }
    }

    // Inherited attrs per unique page, from the SOURCE, before copying.
    let depth = DEFAULT_MAX_PAGE_TREE_DEPTH;
    let mut inherited: Vec<InheritedAttrs> = Vec::with_capacity(unique.len());
    for &src in &unique {
        inherited.push(InheritedAttrs {
            resources: resolve_inherited_resources_with_max_depth(source, src, depth)?,
            rotate: resolve_inherited_rotate_with_max_depth(source, src, depth)?,
            mediabox: resolve_inherited_raw(source, src, "MediaBox", depth)?,
            cropbox: resolve_inherited_raw(source, src, "CropBox", depth)?,
        });
    }

    // UNION closure over unique pages -> ONE copy_objects call -> a single
    // renumber map, so an object shared between pages is copied exactly once.
    let mut closure: BTreeSet<ObjectRef> = BTreeSet::new();
    for &src in &unique {
        closure.extend(page_object_closure(source, src)?);
    }
    let mut target = Pdf::open_mem_owned(minimal_target_bytes())?;
    let map = copy_objects(source, &mut target, &closure)?;
    let pages_root_ref = target_pages_root(&mut target)?;

    // Materialize inherited attrs + reparent, per unique copied leaf.
    // （既存 extract_page の leaf 処理ループ化。has_own ガード4種 + Parent 挿入。
    //  inherited[i] の resources/mediabox/cropbox は所有値なのでムーブし、
    //  rewrite_refs で map 経由の remap を適用。）
    let mut copied_unique: Vec<ObjectRef> = Vec::with_capacity(unique.len());
    for (i, &src) in unique.iter().enumerate() { ... }

    // keep = all copied selected pages. Duplicate clones (below) are never
    // the target of a remapped destination (copy_objects maps each source
    // page to its FIRST copy), so the unique copies suffice.
    let keep: BTreeSet<ObjectRef> = copied_unique.iter().copied().collect();
    for &copied in &copied_unique {
        neutralize_absent_dests(&mut target, copied, &keep)?;
    }

    // /Kids in SELECTION order; 2nd+ occurrence of a page = shallow clone of
    // the post-materialization, post-neutralization dict under a fresh number
    // (sub-objects stay shared — qpdf 11.9.0 duplicate behaviour).
    let mut next_num: u32 = target.object_refs().iter().map(|r| r.number).max().unwrap_or(0);
    let mut used: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut kids: Vec<ObjectRef> = Vec::with_capacity(selected.len());
    for &src in &selected {
        let first = map[&src];   // map に必ずある（closure に unique 全頁が入る）
        let kid = if used.insert(first) {
            first
        } else {
            next_num = next_num.checked_add(1).ok_or_else(|| {
                Error::Unsupported(
                    "page extract: object-number overflow allocating duplicate page".to_string(),
                )
            })?;
            let clone_ref = ObjectRef::new(next_num, 0);
            let dict = resolve_dict(&mut target, first, "copied page is not a dictionary")?;
            target.set_object(clone_ref, Object::Dictionary(dict));
            clone_ref
        };
        kids.push(kid);
    }

    // Fresh single-level /Pages root.
    let mut root = resolve_dict(&mut target, pages_root_ref, "target /Pages is not a dictionary")?;
    root.insert("Kids", Object::Array(kids.iter().map(|&r| Object::Reference(r)).collect()));
    root.insert("Count", Object::Integer(kids.len() as i64));
    target.set_object(pages_root_ref, Object::Dictionary(root));

    sweep_unreachable_objects(&mut target)?;
    Ok(target)
}

pub fn extract_page<R: Read + Seek>(
    source: &mut Pdf<R>,
    page_index: usize,
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    extract_pages(source, &[page_index])
}
```

注意（review-patterns 順守）:
- `map[&src]` は panic しうるので `map.get(&src).ok_or(Error::Missing(...))` を使う（既存 extract_page と同じ文言 "extracted page missing from copy map"）。
- `inherited` の各値は `remove`/ムーブで使い回し、clone しない（Vec から `into_iter` で消費するか `std::mem::take`）。
- `next_num` の `checked_add` 必須（rebuild と同じガード）。
- doc コメント: 1行要約 + `# Errors` 必須、英語のみ、issue ID 禁止。重複ページ・bead /P first-occurrence の挙動は qpdf 11.9.0 の観測として書く（`thread_bead_p.rs` モジュール doc の文体を踏襲）。

lib.rs: `pub use page_extract::extract_page;` を `pub use page_extract::{extract_page, extract_pages};` に変更。

**Step 4: テスト確認**

Run: `cargo test -p flpdf --test page_extract_tests`
Expected: 新規3テスト + 既存全テスト PASS（委譲で挙動同一）

Run: `cargo test -p flpdf`（unit + 他の page_extract_* 統合テスト）
Expected: 全 PASS

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_extract.rs crates/flpdf/src/lib.rs crates/flpdf/tests/page_extract_tests.rs
git commit -m "feat(flpdf): extract_pages multi-page extract with shared-resource dedup (flpdf-5h5.4)"
```

---

### Task 3: 重複選択（shallow clone）

**Files:**
- Test: `crates/flpdf/tests/page_extract_tests.rs`
- Modify（必要時）: `crates/flpdf/src/page_extract.rs`

**Step 1: failing test**

```rust
#[test]
fn extract_pages_duplicate_selection_clones_page_dict_shares_contents() {
    let bytes = three_page_shared_font_pdf();
    let mut src = Pdf::open_mem_owned(bytes).unwrap();
    let mut doc = flpdf::extract_pages(&mut src, &[0, 0]).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2, "duplicate selection yields two enumerated pages");
    assert_ne!(refs[0], refs[1], "two DISTINCT page dict objects");
    let root = pages_dict(&mut doc);
    assert!(matches!(root.get("Count"), Some(Object::Integer(2))));

    // /Contents is the SAME indirect ref on both copies (shared stream).
    let c0 = doc.resolve_borrowed(refs[0]).unwrap().as_dict().cloned().unwrap()
        .get("Contents").cloned();
    let c1 = doc.resolve_borrowed(refs[1]).unwrap().as_dict().cloned().unwrap()
        .get("Contents").cloned();
    assert!(matches!((&c0, &c1), (Some(Object::Reference(a)), Some(Object::Reference(b))) if a == b),
        "duplicate copies must share the same /Contents ref: {c0:?} vs {c1:?}");
    // Shared font still copied once.
    assert_eq!(count_font_objects(&mut doc, b"Helvetica"), 1);
}
```

**Step 2: 実行** — Task 2 の実装で既に通るはず。通れば pin として確定、通らなければ修正。
Run: `cargo test -p flpdf --test page_extract_tests duplicate`

**Step 3: Commit**

```bash
git add crates/flpdf/tests/page_extract_tests.rs
git commit -m "test(flpdf): pin extract_pages duplicate-selection shallow-clone behaviour"
```

---

### Task 4: 選択ページ間リンク保持 / 非選択ページ宛 neutralize

**Files:**
- Test: `crates/flpdf/tests/page_extract_tests.rs`

**Step 1: failing tests**

リンク注釈付き 3 ページ fixture（page 3 に page 4 宛と page 5 宛の 2 つのリンク）:

```rust
/// Page 3 carries two link annotations: one to page 4 (/Dest [4 0 R /Fit]),
/// one to page 5 (/Dest [5 0 R /Fit]).
fn three_page_linked_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [6 0 R 7 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (6, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [4 0 R /Fit] >>"),
            (7, "<< /Type /Annot /Subtype /Link /Rect [20 0 30 10] /Dest [5 0 R /Fit] >>"),
        ],
        1,
    )
}

#[test]
fn extract_pages_keeps_dest_between_selected_pages() {
    let mut src = Pdf::open_mem_owned(three_page_linked_pdf()).unwrap();
    let mut doc = flpdf::extract_pages(&mut src, &[0, 1]).unwrap();
    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2);
    // Annot 6's /Dest must survive, remapped to the copied page 2 (refs[1]).
    // Annot 7's /Dest (to non-selected page 5) must be dropped, and page 5's
    // stub pruned.
    let annots = ...; // refs[0] の /Annots を resolve し、各 annot dict を集める
    let dests: Vec<Option<Object>> = annots.iter().map(|a| a.get("Dest").cloned()).collect();
    // One annotation keeps a /Dest whose first element == Reference(refs[1]);
    // the other has NO /Dest.
    assert!(dests.iter().any(|d| matches!(d, Some(Object::Array(arr))
        if matches!(arr.first(), Some(Object::Reference(r)) if *r == refs[1]))));
    assert!(dests.iter().any(|d| d.is_none()));
    // The non-selected page's stub is gone: only 2 /Type /Page objects remain.
    let page_dicts = doc.object_refs().iter().filter(|&&r| {
        doc.resolve_borrowed(r).ok()
            .and_then(|o| o.as_dict().cloned())
            .is_some_and(|d| matches!(d.get("Type"), Some(Object::Name(n)) if n == b"Page"))
    }).count();
    assert_eq!(page_dicts, 2);
}
```

（borrow が要るので実装時に `doc.resolve_borrowed` の借用スコープを分ける。）

**Step 2: 実行** — Task 1+2 の keep 集合化で通るはず。
Run: `cargo test -p flpdf --test page_extract_tests dest_between`

**Step 3: Commit**

```bash
git add crates/flpdf/tests/page_extract_tests.rs
git commit -m "test(flpdf): pin extract_pages inter-page dest retention and out-of-set neutralization"
```

---

### Task 5: 重複ページの bead /P は first occurrence（qpdf 11.9.0 parity）

**Files:**
- Test: `crates/flpdf/tests/page_extract_thread_bead_p_tests.rs`（既存の bead fixture 流儀に合わせる）

**Step 1: failing test**

既存 `page_extract_thread_bead_p_tests.rs` の fixture を流用し、
`extract_pages(&mut src, &[0, 0])`（bead を持つページの重複選択）で:
- bead オブジェクトは1つだけ（重複されない）
- 両ページコピーの `/B` が**同一の** bead ref を指す
- bead の `/P` は **first occurrence**（`page_refs()[0]`）を指す

```rust
#[test]
fn duplicate_selection_shares_bead_and_p_points_at_first_occurrence() {
    // fixture: 既存テストの bead 付き単一/複数ページ PDF を流用
    let mut src = ...;
    let mut doc = flpdf::extract_pages(&mut src, &[0, 0]).unwrap();
    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2);
    let b0 = ...; // refs[0] の /B 配列の最初の bead ref
    let b1 = ...; // refs[1] の /B 配列の最初の bead ref
    assert_eq!(b0, b1, "duplicate copies share the single bead (qpdf 11.9.0)");
    let bead = ...; // bead dict を resolve
    assert!(matches!(bead.get("P"), Some(Object::Reference(r)) if *r == refs[0]),
        "shared bead /P must point at the FIRST occurrence");
}
```

**Step 2: 実行・確認** — shallow clone は /B ref を共有し、copy_objects が bead /P を
map[src]（= first occurrence）に remap しているので通るはず。通らなければ実装修正。

**Step 3: Commit**

```bash
git add crates/flpdf/tests/page_extract_thread_bead_p_tests.rs
git commit -m "test(flpdf): pin duplicate-selection bead sharing and first-occurrence /P (qpdf 11.9.0 parity)"
```

---

### Task 6: 継承属性（異なる親）+ エラーケース

**Files:**
- Test: `crates/flpdf/tests/page_extract_tests.rs`

**Step 1: failing tests**

```rust
#[test]
fn extract_pages_materializes_inherited_attrs_per_parent() {
    // 2つの中間 /Pages ノードがそれぞれ異なる /MediaBox /Rotate を持ち、
    // 各リーフは自前の属性を持たない fixture。
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 /MediaBox [0 0 100 200] /Rotate 90 >>"),
            (4, "<< /Type /Pages /Parent 2 0 R /Kids [6 0 R] /Count 1 /MediaBox [0 0 300 400] >>"),
            (5, "<< /Type /Page /Parent 3 0 R >>"),
            (6, "<< /Type /Page /Parent 4 0 R >>"),
        ],
        1,
    );
    let mut src = Pdf::open_mem_owned(bytes).unwrap();
    let mut doc = flpdf::extract_pages(&mut src, &[0, 1]).unwrap();
    let refs = pages::page_refs(&mut doc).unwrap();
    let leaf0 = ...; let leaf1 = ...;
    // leaf0: MediaBox [0 0 100 200], Rotate 90 / leaf1: [0 0 300 400], Rotate 0
    ...
}

#[test]
fn extract_pages_empty_selection_is_an_error() {
    let mut src = Pdf::open_mem_owned(three_page_shared_font_pdf()).unwrap();
    assert!(flpdf::extract_pages(&mut src, &[]).is_err());
}

#[test]
fn extract_pages_out_of_range_index_is_an_error() {
    let mut src = Pdf::open_mem_owned(three_page_shared_font_pdf()).unwrap();
    assert!(flpdf::extract_pages(&mut src, &[0, 3]).is_err());
}
```

**Step 2: 実行・確認**

Run: `cargo test -p flpdf --test page_extract_tests`
Expected: 全 PASS

**Step 3: Commit**

```bash
git add crates/flpdf/tests/page_extract_tests.rs
git commit -m "test(flpdf): extract_pages inherited-attr materialization and error arms"
```

---

### Task 7: doc 整備 + 品質ゲート

**Files:**
- Modify: `crates/flpdf/src/page_extract.rs`（モジュール doc / 関数 doc）
- Modify: `crates/flpdf/src/lib.rs`（必要なら crate doc の API 一覧に追記）

**Step 1: doc 更新**

- モジュール doc 冒頭を multi-page 前提に書き直す（`extract_page` 単独前提の文を更新）。
- `extract_pages` の rustdoc: 1行要約・`# Errors`・順序/重複セマンティクス・
  共有リソース dedup・選択集合内 dest 保持を記述。intra-doc リンク
  (`[`extract_page`]`, `[`copy_objects`]` 等) を使う。
- `.claude/rules/pdf-rust-doc-review-patterns.md` の grep 4種でゼロ件確認:

```bash
grep -rnE '(///|//!).*flpdf-[0-9a-z.]+' crates/flpdf/src/page_extract.rs
grep -rnP '(///|//!).*[ぁ-んァ-ヶ一-龠]' crates/flpdf/src/page_extract.rs
```

**Step 2: 品質ゲート**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --doc
```

Expected: すべてクリーン

**Step 3: カバレッジゲート（commit 後）**

```bash
git add -A && git commit -m "docs(flpdf): extract_pages module and API docs"
scripts/patch-coverage.sh
```

Expected: flpdf 変更行 100%（未カバー行があればテスト追加 or `// cov:ignore:` + 理由）

**Step 4: Commit（残差があれば）**

---

## 完了後

- `superpowers:verification-before-completion` → `superpowers:finishing-a-development-branch`
- PR 作成は REST（`gh api .../pulls`、GraphQL 401 回避）
- `bd close flpdf-5h5.4`、Session Completion プロトコル（git push まで）
