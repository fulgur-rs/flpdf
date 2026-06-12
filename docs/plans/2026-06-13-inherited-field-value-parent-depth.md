# inherited_field_value の /Parent 深さ上限 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** signatures.rs と json_inspect.rs の `inherited_field_value()` の `/Parent`
チェーン走査に深さ上限を追加し、長い `/Parent` チェーン（数千世代）による
計算量 DoS を catchable な `Error` に変える。手本は
`annotation_helper::resolve_inherited_name`（loop + `seen` + 深さ上限）。

**Architecture:** 各関数の `while let Some(Object::Reference) = parent` ループに
`depth: usize` カウンタを追加。ループ先頭で上限チェック→超過なら `Err`、各
`/Parent` 上昇で `depth += 1`。循環は従来どおり `seen` で `Ok(None)`。**機構は
各ファイルのローカル慣例に合わせる**（cross-function 統一はしない）:
- `signatures.rs`: `if depth > DEFAULT_MAX_SIGNATURE_FIELD_DEPTH`（同ファイル
  516/596/640 行と同形）、`Err(Error::Unsupported(..))`。定数はモジュールスコープ既存。
- `json_inspect.rs`: 関数ローカル `use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;`
  （同ファイル 914/1122 行の idiom）、`if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH`
  （同ファイル 1046 行 `depth >= max_depth` と同形）、`Err(ConvertError::PdfError(..))`。

**Tech Stack:** Rust, cargo test, cargo llvm-cov（patch-coverage ゲート）。

**スコープ（issue 明記の2関数のみ。peer 棚卸しはしない）:** 他の `/Parent` 上昇
ループへの横展開はしない。Task 0 で1度だけ sanity grep し、glaring なものが無ければ
2関数に限定。

**挙動変化（PR 説明に1行）:** >上限 深の**非循環** `/Parent` チェーンが
従来の解決成功から catchable `Error` に変わる（pathological のみ。狙い=hardening）。
各 guard は所属モジュールの既存 field-tree-depth 慣例に一致
（signatures `>`/SIGNATURE_FIELD_DEPTH、json_inspect `>=`/PAGE_TREE_DEPTH; 共に上限100）。

**テストフィクスチャ（手本）:** `name_number_tree::empty_pdf`（最小 PDF を
`Pdf::open`、ノードは `pdf.set_object(ref, dict)` で渡す）。`Pdf::set_object` は
`reader.rs` の `pub fn`。

---

### Task 0: スコープ確認（sanity grep のみ、コード変更なし）

Run: `grep -rnE 'get\("Parent"\)|"Parent"' crates/flpdf/src/*.rs | grep -iE "while|loop|recurs" ` 等で
他に深さ無制限の `/Parent` 上昇ループが無いか1度だけ確認する。手本
`annotation_helper::resolve_inherited_name`（上限あり）、signatures の
`walk_signature_*`（上限あり）、対象2関数（上限なし）を把握。**glaring な無制限
ループが無ければ 2関数に限定**して Task 1 へ。発見時はこのセッションで報告のみ
（別 issue 候補）。コミット不要。

---

### Task 1: `signatures.rs::inherited_field_value` に深さ上限

**Files:**
- Modify: `crates/flpdf/src/signatures.rs`（関数 `inherited_field_value`、`grep -n "fn inherited_field_value" crates/flpdf/src/signatures.rs` で位置特定。概ね 853 付近）
- Test: `crates/flpdf/src/signatures.rs` 末尾に `#[cfg(test)] mod tests`（**存在しないので新規追加**）

**Step 1: 失敗するテストを書く**

ファイル末尾に追加（`Error`/`Object`/`ObjectRef`/`Dictionary`/`Pdf`/`DEFAULT_MAX_SIGNATURE_FIELD_DEPTH`
は `use super::*;` で見える）:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // Minimal valid PDF; nodes are supplied via set_object refs (catalog unused).
    fn empty_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(Cursor::new(bytes)).expect("open")
    }

    // Register a /Parent chain obj(start)->obj(start+1)->...->obj(start+len-1).
    // The deepest node carries `key`; the starting dict (returned) only has /Parent.
    fn parent_chain(pdf: &mut Pdf<Cursor<Vec<u8>>>, start: u32, len: u32, key: &str) -> Dictionary {
        for i in 0..len {
            let num = start + i;
            let mut d = Dictionary::new();
            if i + 1 < len {
                d.insert("Parent", Object::Reference(ObjectRef::new(num + 1, 0)));
            } else {
                // deepest node holds the inheritable value
                d.insert(key, Object::Integer(42));
            }
            pdf.set_object(ObjectRef::new(num, 0), Object::Dictionary(d));
        }
        let mut start_dict = Dictionary::new();
        start_dict.insert("Parent", Object::Reference(ObjectRef::new(start, 0)));
        start_dict
    }

    #[test]
    fn inherited_field_value_errors_on_excessive_parent_depth() {
        let mut pdf = empty_pdf();
        // Chain longer than the limit so the guard trips before reaching the leaf.
        let start_dict = parent_chain(&mut pdf, 2, (DEFAULT_MAX_SIGNATURE_FIELD_DEPTH as u32) + 5, "V");
        let err = inherited_field_value(&mut pdf, &start_dict, "V");
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn inherited_field_value_resolves_within_limit() {
        let mut pdf = empty_pdf();
        // Short chain: the inherited value must be found, not errored.
        let start_dict = parent_chain(&mut pdf, 2, 4, "V");
        let got = inherited_field_value(&mut pdf, &start_dict, "V").unwrap();
        assert_eq!(got, Some(Object::Integer(42)));
    }
}
```

**Step 2: 失敗を確認**

Run: `cargo test -p flpdf signatures::tests::inherited 2>&1 | tail -20`
Expected: 深さ上限がまだ無いので `errors_on_excessive_parent_depth` が FAIL（Err でなく
Ok(Some) か、あるいは深いチェーンを最後まで辿って 42 を返す）。`resolves_within_limit` は
PASS。これが RED。

**Step 3: 実装**

`inherited_field_value` のループに `depth` を追加:

```rust
    let mut parent = field_dict.get("Parent").cloned();
    let mut seen = BTreeSet::new();
    let mut depth: usize = 0;
    while let Some(Object::Reference(parent_ref)) = parent {
        if depth > DEFAULT_MAX_SIGNATURE_FIELD_DEPTH {
            return Err(Error::Unsupported(format!(
                "signature field-tree depth limit {DEFAULT_MAX_SIGNATURE_FIELD_DEPTH} exceeded at {parent_ref}"
            )));
        }
        if !seen.insert(parent_ref) {
            break;
        }
        match pdf.resolve_borrowed(parent_ref)? {
            Object::Dictionary(parent_dict) => {
                if let Some(value) = parent_dict.get(key).cloned() {
                    return Ok(Some(value));
                }
                parent = parent_dict.get("Parent").cloned();
            }
            _ => break,
        }
        depth += 1;
    }
    Ok(None)
```
（`DEFAULT_MAX_SIGNATURE_FIELD_DEPTH` はモジュールスコープ既存・`Error` も import 済み。
カウント形は同ファイル 516/596/640 行の `depth > DEFAULT_MAX_SIGNATURE_FIELD_DEPTH` と同形。）

**Step 4: PASS 確認**

Run: `cargo test -p flpdf signatures 2>&1 | grep -E "test result|error\["`
Expected: 新2件 + 既存 signatures テスト全 PASS。

**Step 5: Commit**

```bash
git add crates/flpdf/src/signatures.rs
git commit -m "fix(flpdf): bound inherited_field_value /Parent depth in signatures (flpdf-hn1g.3)"
```

---

### Task 2: `json_inspect.rs::inherited_field_value` に深さ上限

**Files:**
- Modify: `crates/flpdf/src/json_inspect.rs`（関数 `inherited_field_value`、`grep -n "fn inherited_field_value" crates/flpdf/src/json_inspect.rs`、概ね 1390 付近）
- Test: 既存 `#[cfg(test)] mod tests`（2588 行付近）に追加

**Step 1: 失敗するテストを書く**

既存テストモジュールに追加。`empty_pdf()` / `parent_chain()` 相当のヘルパが
無ければ Task 1 と同じものを追加（あれば再利用）。エラー型は `ConvertError`、
値は `ConvertError::PdfError(_)` を期待:

```rust
    #[test]
    fn inherited_field_value_errors_on_excessive_parent_depth() {
        use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
        let mut pdf = empty_pdf();
        let start_dict = parent_chain(&mut pdf, 2, (DEFAULT_MAX_PAGE_TREE_DEPTH as u32) + 5, "V");
        let err = inherited_field_value(&mut pdf, &start_dict, "V");
        assert!(matches!(err, Err(ConvertError::PdfError(_))));
    }

    #[test]
    fn inherited_field_value_resolves_within_limit() {
        let mut pdf = empty_pdf();
        let start_dict = parent_chain(&mut pdf, 2, 4, "V");
        let got = inherited_field_value(&mut pdf, &start_dict, "V").unwrap();
        assert_eq!(got, Some(Object::Integer(42)));
    }
```
（`empty_pdf`/`parent_chain` は Task 1 と同一実装。`Pdf<Cursor<Vec<u8>>>` 型・
`set_object`・`ObjectRef`・`Dictionary`・`Object` は `use super::*;`/既存 import で参照。
json_inspect の test モジュールに無いものだけ補う。）

**Step 2: 失敗を確認**

Run: `cargo test -p flpdf json_inspect::tests::inherited 2>&1 | tail -20`
Expected: `errors_on_excessive_parent_depth` が RED、`resolves_within_limit` は PASS。

**Step 3: 実装**

`inherited_field_value` のループに `depth` を追加（関数先頭で `use` を file idiom に合わせる）:

```rust
fn inherited_field_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_dict: &Dictionary,
    key: &str,
) -> Result<Option<Object>, ConvertError> {
    use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
    if let Some(local) = field_dict.get(key).cloned() {
        return Ok(Some(local));
    }
    let mut parent = field_dict.get("Parent").cloned();
    let mut seen: std::collections::BTreeSet<crate::ObjectRef> = std::collections::BTreeSet::new();
    let mut depth: usize = 0;
    while let Some(Object::Reference(pr)) = parent {
        if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
            return Err(ConvertError::PdfError(format!(
                "AcroForm field-tree depth limit {DEFAULT_MAX_PAGE_TREE_DEPTH} exceeded"
            )));
        }
        if !seen.insert(pr) {
            break;
        }
        match pdf.resolve_borrowed(pr).map_err(ConvertError::from)? {
            Object::Dictionary(pd) => {
                if let Some(v) = pd.get(key).cloned() {
                    return Ok(Some(v));
                }
                parent = pd.get("Parent").cloned();
            }
            _ => break,
        }
        depth += 1;
    }
    Ok(None)
}
```
（関数ローカル `use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;` は同ファイル 914/1122 行の
idiom。`if depth >= ...` は同ファイル 1046 行 `depth >= max_depth` と同形。）

**Step 4: PASS 確認**

Run: `cargo test -p flpdf json_inspect 2>&1 | grep -E "test result|error\["`
Expected: 新2件 + 既存 json_inspect テスト全 PASS。

**Step 5: Commit**

```bash
git add crates/flpdf/src/json_inspect.rs
git commit -m "fix(flpdf): bound inherited_field_value /Parent depth in json_inspect (flpdf-hn1g.3)"
```

---

### Task 3: 全体ゲート

**Step 1: 全テスト** — Run: `cargo test -p flpdf 2>&1 | grep -E "test result:" | grep -v "0 failed" || echo ALL_GREEN`
Expected: 全 suite 0 failed（既存 signatures/json_inspect スイートが緑＝正常 PDF を誤って弾かない）。

**Step 2: fmt / clippy** — Run: `cargo fmt -p flpdf && cargo fmt -p flpdf --check && cargo clippy -p flpdf --all-targets 2>&1 | grep -E "warning:|error:" | head`
Expected: fmt 差分なし、clippy 警告ゼロ。

**Step 3: patch-coverage** — Run（commit 後）: `scripts/patch-coverage.sh --base main 2>&1 | tail -4`
Expected: flpdf 変更行 100% カバー。未カバーはテスト追加。

**Step 4: doc-grep** — Run: `grep -rnE '(///|//!).*flpdf-[0-9a-z.]+' crates/flpdf/src/signatures.rs crates/flpdf/src/json_inspect.rs`
Expected: 0 件。

---

## 完了基準
- 両 `inherited_field_value` に depth 上限。signatures=`>`/SIGNATURE_FIELD_DEPTH/Error::Unsupported、
  json_inspect=`>=`/PAGE_TREE_DEPTH/ConvertError::PdfError（各ファイルの近傍慣例と同形）。
- 各関数に「超過=Err・上限内=Ok(Some(継承値))」テスト。flpdf 変更行 100% カバー。
- `cargo test -p flpdf` 全緑、`cargo fmt --check` 差分なし、clippy 警告ゼロ。
- 循環時は従来どおり `Ok(None)`（挙動不変）。
