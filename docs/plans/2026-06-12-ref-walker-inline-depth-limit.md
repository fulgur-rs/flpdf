# 構造 ref-walker への共有インライン深さ上限 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** インライン（直接）ネストが未上限の 8 つの構造 walker に、共有定数
`MAX_INLINE_DEPTH=256` による深さ上限を入れ、深いダイレクトネスト構造での
スタックオーバーフロー→abort（脅威モデル §2(b)）を catchable な `Error` に変える。

**Architecture:** 単一の `pub(crate) const MAX_INLINE_DEPTH: usize = 256` を
`crates/flpdf/src/object.rs` に新設。各 walker は `depth: usize` 引数を取り、
エントリで `depth >= MAX_INLINE_DEPTH` なら `Err(Error::Unsupported(..))`、下降時
`depth + 1`。間接参照は呼び出し側の反復（BFS/DFS + visited）で辿られるため、この
上限はインライン軸のみを bound する。汎用 visitor は採らず「共有定数 + 各 walker
ローカル depth 引数」で統一（既存 rewrite_renumber/acroform/signatures の前例どおり）。

**Tech Stack:** Rust, cargo test, cargo llvm-cov（patch-coverage ゲート）。

**境界規約（全 walker 共通）:** ルート呼び出しは `depth = 0`。エントリで
`if depth >= MAX_INLINE_DEPTH { return Err(crate::Error::Unsupported("<context>: inline object nesting exceeds MAX_INLINE_DEPTH".into())) }`。
各構造下降で `depth + 1` を渡す。256 段までのネストを許容し、257 段目で Err。

**テスト雛形（手本）:** `rewrite_renumber.rs` の tests に既存の
`nest_in_arrays(leaf, n)` / `collect_refs_errors_on_excessive_nesting`（`+5` で Err）/
`collect_refs_accepts_nesting_up_to_the_limit`（`-1` で Ok）。各 walker でこの3点
（超過=Err・限界=Ok・正常系回帰）を踏襲する。

**スコープ外（確認済みで触らない）:** `signatures::collect_known_signature_value`
（全構造下降で depth+1 済み）。`inherited_field_value` の /Parent 追跡は hn1g.3。

**注記（PR 説明に1行記す）:** in-scope walker では未上限→256 化により、`(256, 500]`
段の parser-valid だが pathological なオブジェクトが success→catchable `Error` に変わる
（`MAX_PARSE_DEPTH=500`）。これは狙い（hardening）であり回帰ではない。定数は
`MAX_PARSE_DEPTH` と独立に保つ。

---

### Task 1: 共有定数 `MAX_INLINE_DEPTH` を新設し rewrite_renumber を統一

**Files:**
- Modify: `crates/flpdf/src/object.rs`（imports 直後、`use std::str::FromStr;` の次行に挿入）
- Modify: `crates/flpdf/src/rewrite_renumber.rs:29,40`（local const 削除＋import）

**Step 1: `object.rs` に共有定数を追加**

`use std::str::FromStr;` の直後に挿入:

```rust
/// Maximum inline structural nesting depth any operation walks when descending
/// into a single resolved object's `Array` / `Dictionary` / `Stream`-dictionary
/// structure. Indirect references are followed iteratively (a caller-driven
/// BFS/DFS with a visited set), so this bounds only inline nesting within one
/// object and guards every post-parse structural walker against stack overflow
/// on adversarial input.
///
/// Exceeding it is a hard error, never a silent stop: a walker that returned
/// early would under-collect or under-rewrite references and corrupt its output
/// (garbage collection would delete still-reachable objects; renumbering would
/// emit mixed old/new object numbers). Returning [`crate::Error::Unsupported`]
/// preserves the no-panic/no-abort core guarantee even for parser-accepted but
/// pathological objects.
///
/// Independent of (and may be lower than) the parser's `MAX_PARSE_DEPTH`:
/// operations cap inline traversal more tightly than parsing. Real PDFs never
/// nest inline structures this deeply — deep hierarchies use indirect
/// references, which travel through the iterative queue rather than this
/// recursion.
pub(crate) const MAX_INLINE_DEPTH: usize = 256;
```

**Step 2: `rewrite_renumber.rs` の local const を削除し共有を import**

- `crates/flpdf/src/rewrite_renumber.rs:29` の `use crate::object::{Object, ObjectRef};` を
  `use crate::object::{Object, ObjectRef, MAX_INLINE_DEPTH};` に変更。
- `:40` の local const（doc コメント 33-39 行ごと）を削除:
  ```rust
  /// Maximum inline structural nesting depth ... Real PDFs never
  /// nest inline structures this deeply.
  const MAX_INLINE_DEPTH: usize = 256;
  ```

**Step 3: ビルド＋既存テストで回帰なしを確認**

Run: `cargo test -p flpdf rewrite_renumber 2>&1 | grep "test result"`
Expected: 既存の `*_excessive_nesting` / `*_up_to_the_limit` 含め全 PASS（値 256 不変・挙動不変）

**Step 4: Commit**

```bash
git add crates/flpdf/src/object.rs crates/flpdf/src/rewrite_renumber.rs
git commit -m "refactor(flpdf): hoist MAX_INLINE_DEPTH to shared object module (flpdf-hn1g.9)"
```

---

### Task 2: `page_closure::collect_refs_in_object`

**Files:**
- Modify: `crates/flpdf/src/page_closure.rs:8`（import）、`:106`（walker）、caller（`collect_refs_in_object(obj, &mut refs_found);`）
- Test: 同ファイル `#[cfg(test)] mod tests`（無ければ追加）

**Step 1: 失敗するテストを書く**

tests モジュールに追加:

```rust
fn nested_arrays(depth: usize) -> Object {
    let mut o = Object::Null;
    for _ in 0..depth {
        o = Object::Array(vec![o]);
    }
    o
}

#[test]
fn collect_refs_in_object_errors_on_excessive_nesting() {
    let mut out = Vec::new();
    let err = collect_refs_in_object(&nested_arrays(MAX_INLINE_DEPTH + 5), 0, &mut out);
    assert!(matches!(err, Err(crate::Error::Unsupported(_))));
}

#[test]
fn collect_refs_in_object_accepts_nesting_up_to_the_limit() {
    let mut out = Vec::new();
    // Bury one Reference just within the limit; it must be collected, not errored.
    let leaf = Object::Array(vec![Object::Reference(ObjectRef::new(7, 0))]);
    let mut o = leaf;
    for _ in 0..(MAX_INLINE_DEPTH - 2) {
        o = Object::Array(vec![o]);
    }
    collect_refs_in_object(&o, 0, &mut out).unwrap();
    assert_eq!(out, vec![ObjectRef::new(7, 0)]);
}
```

tests モジュール冒頭に `use super::*;` と、必要なら `use crate::object::MAX_INLINE_DEPTH;`。
（`super::*` で walker と `nested_arrays` が見えるが、`MAX_INLINE_DEPTH` は明示 import）

**Step 2: 失敗を確認**

Run: `cargo test -p flpdf page_closure 2>&1 | tail -20`
Expected: コンパイルエラー（引数個数不一致・戻り値型不一致）→ 実装前の RED

**Step 3: 実装**

`:8` import に `Error` と const を追加:
```rust
use crate::object::MAX_INLINE_DEPTH;
use crate::{Object, ObjectRef, Pdf, Result};
```
（`Error` は新コードで `crate::Error::Unsupported` と完全修飾するため import 不要）

walker を改修（`:106`）:
```rust
fn collect_refs_in_object(obj: &Object, depth: usize, out: &mut Vec<ObjectRef>) -> Result<()> {
    if depth >= MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(
            "page closure: inline object nesting exceeds MAX_INLINE_DEPTH".to_string(),
        ));
    }
    match obj {
        Object::Reference(r) => out.push(*r),
        Object::Array(items) => {
            for item in items {
                collect_refs_in_object(item, depth + 1, out)?;
            }
        }
        Object::Dictionary(dict) => {
            let is_pages_node = dict
                .get("Type")
                .and_then(|o| o.as_name())
                .map(|n| n == b"Pages")
                .unwrap_or(false);
            for (key, value) in dict.iter() {
                if is_pages_node && key == b"Kids" {
                    continue;
                }
                collect_refs_in_object(value, depth + 1, out)?;
            }
        }
        Object::Stream(stream) => {
            for (_key, value) in stream.dict.iter() {
                collect_refs_in_object(value, depth + 1, out)?;
            }
        }
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => {}
    }
    Ok(())
}
```

caller（`page_object_closure` 内、`collect_refs_in_object(obj, &mut refs_found);`）を:
```rust
        collect_refs_in_object(obj, 0, &mut refs_found)?;
```

**Step 4: テスト PASS を確認**

Run: `cargo test -p flpdf page_closure 2>&1 | grep "test result"`
Expected: PASS（新2件 + 既存 closure テスト緑）

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_closure.rs
git commit -m "fix(flpdf): bound page_closure ref-walker inline depth (flpdf-hn1g.9)"
```

---

### Task 3: `subset_prune::walk_refs`

**Files:**
- Modify: `crates/flpdf/src/subset_prune.rs:48`（import）、`:215`（walker）、`:209`（caller `walk_refs(obj, &mut queue);`）
- Test: 同ファイル既存 `#[cfg(test)] mod tests`

**Step 1: 失敗するテスト**

```rust
fn nested_arrays(depth: usize) -> Object {
    let mut o = Object::Null;
    for _ in 0..depth { o = Object::Array(vec![o]); }
    o
}

#[test]
fn walk_refs_errors_on_excessive_nesting() {
    let mut queue = Vec::new();
    let err = walk_refs(&nested_arrays(MAX_INLINE_DEPTH + 5), 0, &mut queue);
    assert!(matches!(err, Err(crate::Error::Unsupported(_))));
}

#[test]
fn walk_refs_accepts_nesting_up_to_the_limit() {
    let mut queue = Vec::new();
    let mut o = Object::Array(vec![Object::Reference(ObjectRef::new(9, 0))]);
    for _ in 0..(MAX_INLINE_DEPTH - 2) { o = Object::Array(vec![o]); }
    walk_refs(&o, 0, &mut queue).unwrap();
    assert_eq!(queue, vec![ObjectRef::new(9, 0)]);
}
```

tests に `use crate::object::MAX_INLINE_DEPTH;` を追加（無ければ）。

**Step 2: 失敗を確認**

Run: `cargo test -p flpdf subset_prune 2>&1 | tail -20`
Expected: コンパイルエラー（RED）

**Step 3: 実装**

`:48` を:
```rust
use crate::object::MAX_INLINE_DEPTH;
use crate::{Object, ObjectRef, Pdf, Result};
```

walker（`:215`）を:
```rust
fn walk_refs(obj: &Object, depth: usize, queue: &mut Vec<ObjectRef>) -> Result<()> {
    if depth >= MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(
            "subset prune: inline object nesting exceeds MAX_INLINE_DEPTH".to_string(),
        ));
    }
    match obj {
        Object::Reference(r) => queue.push(*r),
        Object::Array(arr) => {
            for item in arr {
                walk_refs(item, depth + 1, queue)?;
            }
        }
        Object::Dictionary(dict) => {
            for (_, val) in dict.iter() {
                walk_refs(val, depth + 1, queue)?;
            }
        }
        Object::Stream(stream) => {
            for (_, val) in stream.dict.iter() {
                walk_refs(val, depth + 1, queue)?;
            }
        }
        _ => {}
    }
    Ok(())
}
```

caller（`collect_reachable` 内、`walk_refs(obj, &mut queue);`）を `walk_refs(obj, 0, &mut queue)?;`。

**Step 4: PASS 確認**

Run: `cargo test -p flpdf subset_prune 2>&1 | grep "test result"`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/flpdf/src/subset_prune.rs
git commit -m "fix(flpdf): bound subset_prune ref-walker inline depth (flpdf-hn1g.9)"
```

---

### Task 4: `object_copy::rewrite_refs` ＋ `rewrite_dict`（相互再帰）

**Files:**
- Modify: `crates/flpdf/src/object_copy.rs:32`（import）、`:108`（caller）、`:118`（rewrite_refs）、`:146`（rewrite_dict）
- Test: 同ファイル `#[cfg(test)] mod tests`

**Step 1: 失敗するテスト**

```rust
fn nested_arrays(depth: usize) -> Object {
    let mut o = Object::Null;
    for _ in 0..depth { o = Object::Array(vec![o]); }
    o
}

#[test]
fn rewrite_refs_errors_on_excessive_nesting() {
    let map: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();
    let mut obj = nested_arrays(MAX_INLINE_DEPTH + 5);
    let err = rewrite_refs(&mut obj, 0, &map);
    assert!(matches!(err, Err(crate::Error::Unsupported(_))));
}

#[test]
fn rewrite_refs_accepts_nesting_up_to_the_limit() {
    let mut map = BTreeMap::new();
    map.insert(ObjectRef::new(3, 0), ObjectRef::new(99, 0));
    let mut obj = Object::Array(vec![Object::Reference(ObjectRef::new(3, 0))]);
    for _ in 0..(MAX_INLINE_DEPTH - 2) { obj = Object::Array(vec![obj]); }
    rewrite_refs(&mut obj, 0, &map).unwrap();
    // 限界内の Reference は remap される（Null にならない）
    // 最深部まで辿って 99 0 R になっていることを確認
}
```
（最深部確認はネスト剥がしループで。簡潔には `format!("{obj:?}").contains("99")` でも可）

tests に `use crate::object::MAX_INLINE_DEPTH;` を追加。

**Step 2: 失敗を確認** — Run: `cargo test -p flpdf object_copy 2>&1 | tail -20`（RED）

**Step 3: 実装**

`:32` を `use crate::{Error, Object, ObjectRef, Pdf, Result};` のまま（Error 既存）、新規 use 追加:
```rust
use crate::object::{Dictionary, MAX_INLINE_DEPTH};
```
（既存 `:31 use crate::object::Dictionary;` を上記にマージ）

`rewrite_refs`（`:118`）:
```rust
pub(crate) fn rewrite_refs(
    obj: &mut Object,
    depth: usize,
    map: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    if depth >= MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(
            "cross-document copy: inline object nesting exceeds MAX_INLINE_DEPTH".to_string(),
        ));
    }
    match obj {
        Object::Reference(r) => {
            let replacement = match map.get(r) {
                Some(&t) => Object::Reference(t),
                None => Object::Null,
            };
            *obj = replacement;
        }
        Object::Array(items) => {
            for item in items.iter_mut() {
                rewrite_refs(item, depth + 1, map)?;
            }
        }
        Object::Dictionary(dict) => rewrite_dict(dict, depth + 1, map)?,
        Object::Stream(stream) => rewrite_dict(&mut stream.dict, depth + 1, map)?,
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => {}
    }
    Ok(())
}
```

`rewrite_dict`（`:146`）:
```rust
fn rewrite_dict(
    dict: &mut Dictionary,
    depth: usize,
    map: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    for value in dict.values_mut() {
        rewrite_refs(value, depth, map)?;
    }
    Ok(())
}
```
（`rewrite_dict` は dict の中身を walk する一段。呼び出し元 `rewrite_refs` が
`depth + 1` を渡すので、`rewrite_dict` 内は同 depth で各値へ。各値の `rewrite_refs`
が再度エントリチェック。)

caller（`:108`、コピーループ内 `rewrite_refs(&mut obj, &map);`）を `rewrite_refs(&mut obj, 0, &map)?;`。

**Step 4: PASS 確認** — Run: `cargo test -p flpdf object_copy 2>&1 | grep "test result"`（PASS）

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_copy.rs
git commit -m "fix(flpdf): bound object_copy rewrite_refs inline depth (flpdf-hn1g.9)"
```

---

### Task 5: `linearization/plan::collect_direct_refs`

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs:37`（import）、`:107`（walker）、`:112-251`（9 callsite すべて `compute_closure` 内）
- Test: 同ファイル `#[cfg(test)] mod tests`

**Step 1: 失敗するテスト**

```rust
fn nested_arrays(depth: usize) -> Object {
    let mut o = Object::Null;
    for _ in 0..depth { o = Object::Array(vec![o]); }
    o
}

#[test]
fn collect_direct_refs_errors_on_excessive_nesting() {
    let mut out = Vec::new();
    let err = collect_direct_refs(&nested_arrays(MAX_INLINE_DEPTH + 5), 0, &mut out);
    assert!(matches!(err, Err(crate::Error::Unsupported(_))));
}

#[test]
fn collect_direct_refs_accepts_nesting_up_to_the_limit() {
    let mut out = Vec::new();
    let mut o = Object::Array(vec![Object::Reference(ObjectRef::new(4, 0))]);
    for _ in 0..(MAX_INLINE_DEPTH - 2) { o = Object::Array(vec![o]); }
    collect_direct_refs(&o, 0, &mut out).unwrap();
    assert_eq!(out, vec![ObjectRef::new(4, 0)]);
}
```

tests に `use crate::object::MAX_INLINE_DEPTH;`。

**Step 2: 失敗確認** — Run: `cargo test -p flpdf linearization::plan 2>&1 | tail -20`（RED）

**Step 3: 実装**

`:37` を `use crate::{Object, ObjectRef, Pdf, Result};` に変更（`Result` 追加）し、別行で
`use crate::object::MAX_INLINE_DEPTH;`。

walker（`:107`）:
```rust
fn collect_direct_refs(obj: &Object, depth: usize, out: &mut Vec<ObjectRef>) -> Result<()> {
    if depth >= MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(
            "linearization plan: inline object nesting exceeds MAX_INLINE_DEPTH".to_string(),
        ));
    }
    match obj {
        Object::Reference(r) => out.push(*r),
        Object::Array(arr) => {
            for elem in arr {
                collect_direct_refs(elem, depth + 1, out)?;
            }
        }
        Object::Dictionary(dict) => {
            for (_k, v) in dict.iter() {
                collect_direct_refs(v, depth + 1, out)?;
            }
        }
        Object::Stream(s) => {
            for (_k, v) in s.dict.iter() {
                collect_direct_refs(v, depth + 1, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}
```

`compute_closure` 内の全 callsite（`:193,:226,:230,:241,:251`）を `collect_direct_refs(<arg>, 0, <out>)?;`
に変更（再帰の `:112,:117,:123` は Step 3 の walker 内で対応済み）。`compute_closure` は
`Result` を返すので `?` で伝播可。

**Step 4: PASS 確認** — Run: `cargo test -p flpdf linearization::plan 2>&1 | grep "test result"`（PASS）

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "fix(flpdf): bound linearization plan collect_direct_refs inline depth (flpdf-hn1g.9)"
```

---

### Task 6: `linearization/writer::renumber_object`

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs:377`（walker）、その caller（`renumber_object(<obj>, renumber)?`）
- Test: 同ファイル `#[cfg(test)] mod tests`

**Step 1: 失敗するテスト**

```rust
fn nested_arrays(depth: usize) -> Object {
    let mut o = Object::Null;
    for _ in 0..depth { o = Object::Array(vec![o]); }
    o
}

#[test]
fn renumber_object_errors_on_excessive_nesting() {
    let renumber = /* 空でも Reference を含まなければ到達しない */;
    let err = renumber_object(&nested_arrays(MAX_INLINE_DEPTH + 5), 0, &renumber);
    assert!(matches!(err, Err(crate::Error::Unsupported(_))));
}
```
注: `RenumberMap`（`renumber`）の構築は既存テストの作り方を踏襲。深さ超過テストは
Reference を含まない純ネストで発火するため、空/最小の `RenumberMap` で良い。限界内
テストは Reference を1つ含め remap される `RenumberMap` を用意（既存テスト参照）。

tests に `use crate::object::MAX_INLINE_DEPTH;`。

**Step 2: 失敗確認** — Run: `cargo test -p flpdf linearization::writer 2>&1 | tail -20`（RED）

**Step 3: 実装**

walker（`:377`）に `depth` 引数を追加（戻り値 `Result<Object>` 維持）:
```rust
fn renumber_object(object: &Object, depth: usize, renumber: &RenumberMap) -> Result<Object> {
    if depth >= MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(
            "linearization writer: inline object nesting exceeds MAX_INLINE_DEPTH".to_string(),
        ));
    }
    match object {
        Object::Reference(r) => match renumber.new_for_original(*r) {
            Some(new_ref) => Ok(Object::Reference(new_ref)),
            None => Err(crate::Error::Unsupported(format!(
                "linearization writer: reference {r} has no entry in RenumberMap \
                 (planner / renumber inconsistency — would emit mixed old/new \
                 object numbers)"
            ))),
        },
        Object::Array(elements) => {
            let mut renumbered = Vec::with_capacity(elements.len());
            for e in elements {
                renumbered.push(renumber_object(e, depth + 1, renumber)?);
            }
            Ok(Object::Array(renumbered))
        }
        Object::Dictionary(dict) => {
            let mut new_dict = Dictionary::new();
            for (key, value) in dict.iter() {
                new_dict.insert(key, renumber_object(value, depth + 1, renumber)?);
            }
            Ok(Object::Dictionary(new_dict))
        }
        Object::Stream(stream) => {
            let mut new_dict = Dictionary::new();
            for (key, value) in stream.dict.iter() {
                new_dict.insert(key, renumber_object(value, depth + 1, renumber)?);
            }
            Ok(Object::Stream(Stream::new(new_dict, stream.data.clone())))
        }
        _ => Ok(object.clone()),
    }
}
```

import に `MAX_INLINE_DEPTH` を追加（`use crate::object::MAX_INLINE_DEPTH;` を新規行で。
既存 `crate::object::` import があればマージ）。全 caller（`renumber_object(x, renumber)?`）を
`renumber_object(x, 0, renumber)?` に変更（`grep -n "renumber_object(" writer.rs` で網羅）。

**Step 4: PASS 確認** — Run: `cargo test -p flpdf linearization::writer 2>&1 | grep "test result"`（PASS）

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/writer.rs
git commit -m "fix(flpdf): bound linearization writer renumber_object inline depth (flpdf-hn1g.9)"
```

---

### Task 7: `security/standard::decrypt_strings_in_value`

**Files:**
- Modify: `crates/flpdf/src/security/standard.rs:1450`（caller）、`:1453`（walker）
- Test: 同ファイル `#[cfg(test)] mod tests`

**Step 1: 失敗するテスト**

```rust
fn nested_arrays(depth: usize) -> Object {
    let mut o = Object::Null;
    for _ in 0..depth { o = Object::Array(vec![o]); }
    o
}

#[test]
fn decrypt_strings_in_value_errors_on_excessive_nesting() {
    let mut obj = nested_arrays(MAX_INLINE_DEPTH + 5);
    // Identity cipher: 深さガードが文字列到達前に発火する
    let err = decrypt_strings_in_value(&mut obj, StringCipher::Identity, 0);
    assert!(matches!(err, Err(_)));
}

#[test]
fn decrypt_strings_in_value_accepts_nesting_up_to_the_limit() {
    let mut obj = nested_arrays(MAX_INLINE_DEPTH - 1);
    decrypt_strings_in_value(&mut obj, StringCipher::Identity, 0).unwrap();
}
```
tests に `use crate::object::MAX_INLINE_DEPTH;`（`StringCipher` は `super::*` で可）。

**Step 2: 失敗確認** — Run: `cargo test -p flpdf security::standard 2>&1 | tail -20`（RED）

**Step 3: 実装**

walker（`:1453`）に `depth: usize` 追加:
```rust
fn decrypt_strings_in_value(
    object: &mut Object,
    cipher: StringCipher<'_>,
    depth: usize,
) -> Result<()> {
    if depth >= crate::object::MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(
            "decrypt: inline object nesting exceeds MAX_INLINE_DEPTH".to_string(),
        ));
    }
    match object {
        Object::String(bytes) => decrypt_cipher_bytes(bytes, cipher),
        Object::Array(values) => {
            for value in values {
                decrypt_strings_in_value(value, cipher, depth + 1)?;
            }
            Ok(())
        }
        Object::Dictionary(dict) => {
            for value in dict.values_mut() {
                decrypt_strings_in_value(value, cipher, depth + 1)?;
            }
            Ok(())
        }
        Object::Stream(stream) => {
            for value in stream.dict.values_mut() {
                decrypt_strings_in_value(value, cipher, depth + 1)?;
            }
            Ok(())
        }
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::Reference(_) => Ok(()),
    }
}
```
caller（`:1450`、wrapper 内 `decrypt_strings_in_value(object, cipher)`）を
`decrypt_strings_in_value(object, cipher, 0)`。
（`MAX_INLINE_DEPTH` は完全修飾 `crate::object::MAX_INLINE_DEPTH` を使用。security モジュールの
import を汚さない。`crate::Error` も完全修飾。）

**Step 4: PASS 確認** — Run: `cargo test -p flpdf security::standard 2>&1 | grep "test result"`（PASS）

**Step 5: Commit**

```bash
git add crates/flpdf/src/security/standard.rs
git commit -m "fix(flpdf): bound decrypt_strings_in_value inline depth (flpdf-hn1g.9)"
```

---

### Task 8: `security/standard::encrypt_strings_in_value`

**Files:**
- Modify: `crates/flpdf/src/security/standard.rs:1683`（caller）、`:1686`（walker）
- Test: 同ファイル `#[cfg(test)] mod tests`

**Step 1: 失敗するテスト**

```rust
#[test]
fn encrypt_strings_in_value_errors_on_excessive_nesting() {
    let mut obj = nested_arrays(MAX_INLINE_DEPTH + 5); // Task 7 で定義済みヘルパ
    let mut iv_gen = || [0u8; 16];
    let err = encrypt_strings_in_value(&mut obj, StringEncryptCipher::Identity, &mut iv_gen, 0);
    assert!(matches!(err, Err(_)));
}

#[test]
fn encrypt_strings_in_value_accepts_nesting_up_to_the_limit() {
    let mut obj = nested_arrays(MAX_INLINE_DEPTH - 1);
    let mut iv_gen = || [0u8; 16];
    encrypt_strings_in_value(&mut obj, StringEncryptCipher::Identity, &mut iv_gen, 0).unwrap();
}
```
（`StringEncryptCipher::Identity` の正確な variant 名は既存コード参照。`nested_arrays` は
Task 7 で同 tests モジュールに定義済みなら再定義しない。）

**Step 2: 失敗確認** — Run: `cargo test -p flpdf security::standard 2>&1 | tail -20`（RED）

**Step 3: 実装**

walker（`:1686`）に `depth: usize` を**末尾引数**で追加:
```rust
fn encrypt_strings_in_value<F>(
    object: &mut Object,
    cipher: StringEncryptCipher<'_>,
    iv_gen: &mut F,
    depth: usize,
) -> Result<()>
where
    F: FnMut() -> [u8; 16],
{
    if depth >= crate::object::MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(
            "encrypt: inline object nesting exceeds MAX_INLINE_DEPTH".to_string(),
        ));
    }
    match object {
        Object::String(bytes) => { /* 既存のまま */ }
        Object::Array(values) => {
            for value in values {
                encrypt_strings_in_value(value, cipher, iv_gen, depth + 1)?;
            }
            Ok(())
        }
        Object::Dictionary(dict) => {
            for value in dict.values_mut() {
                encrypt_strings_in_value(value, cipher, iv_gen, depth + 1)?;
            }
            Ok(())
        }
        Object::Stream(stream) => {
            for value in stream.dict.values_mut() {
                encrypt_strings_in_value(value, cipher, iv_gen, depth + 1)?;
            }
            Ok(())
        }
        /* scalars / Reference => Ok(()) 既存のまま */
    }
}
```
（`String` アームと scalar アームは既存ボディを保持。）
caller（`:1683`、wrapper 内 `encrypt_strings_in_value(object, cipher, iv_gen)`）を
`encrypt_strings_in_value(object, cipher, iv_gen, 0)`。

**Step 4: PASS 確認** — Run: `cargo test -p flpdf security::standard 2>&1 | grep "test result"`（PASS）

**Step 5: Commit**

```bash
git add crates/flpdf/src/security/standard.rs
git commit -m "fix(flpdf): bound encrypt_strings_in_value inline depth (flpdf-hn1g.9)"
```

---

### Task 9: `acroform_document_helper` — インライン軸の分離

**Files:**
- Modify: `crates/flpdf/src/acroform_document_helper.rs:751`（`collect_refs_in_object`）、`:781`（`collect_refs_in_dict`）、`:749`（`collect_reachable_refs` が呼ぶ箇所）
- Test: 同ファイル `#[cfg(test)] mod tests`

**背景:** 既存 `depth`（ref hop 軸, `DEFAULT_MAX_ACROFORM_DEPTH`）は据置。新たに
`inline_depth` を `collect_refs_in_object` / `collect_refs_in_dict` に追加し、インライン
Array/Dict 下降で `+1` / `>= MAX_INLINE_DEPTH` でチェック。`collect_reachable_refs` が
resolve 後に `collect_refs_in_object` を呼ぶ際は `inline_depth = 0` でリセット
（解決済みオブジェクトごとに新しいインライン走査）。ref hop 軸とインライン軸が独立に bound。

**Step 1: 失敗するテスト**

`collect_refs_in_object` は `&mut Pdf` を取るが、純 Array/Dict ネスト（Reference 無し）なら
`collect_reachable_refs`（=resolve）に到達しないため `pdf` は使われない。最小 Pdf を
既存 tests フィクスチャ（`Pdf::open_mem(..)` 系）で用意して直接呼ぶ:

```rust
#[test]
fn collect_refs_in_object_errors_on_excessive_inline_nesting() {
    let mut pdf = /* 既存 tests の最小 Pdf フィクスチャ */;
    let mut out = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let deep = nested_arrays(MAX_INLINE_DEPTH + 5); // Reference を含まない
    let err = collect_refs_in_object(&mut pdf, &deep, &mut out, &mut seen, 0, 0);
    assert!(matches!(err, Err(crate::Error::Unsupported(_))));
}
```
（引数順: `(pdf, obj, out, seen, depth, inline_depth)`。`nested_arrays` ヘルパを tests に追加。）

**Step 2: 失敗確認** — Run: `cargo test -p flpdf acroform_document_helper 2>&1 | tail -20`（RED）

**Step 3: 実装**

`:10` import に `MAX_INLINE_DEPTH` を追加（`crate::{... , DEFAULT_MAX_ACROFORM_DEPTH}` の隣、
別行 `use crate::object::MAX_INLINE_DEPTH;` でも可）。

`collect_reachable_refs`（`:749` 付近、最終行）:
```rust
    let obj = pdf.resolve(object_ref)?;
    collect_refs_in_object(pdf, &obj, out, seen, depth, 0) // inline_depth リセット
```

`collect_refs_in_object`（`:751`）に `inline_depth: usize` を追加:
```rust
fn collect_refs_in_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    obj: &Object,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
    inline_depth: usize,
) -> Result<()> {
    if inline_depth >= MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(
            "AcroForm: inline object nesting exceeds MAX_INLINE_DEPTH".to_string(),
        ));
    }
    match obj {
        Object::Reference(object_ref) => {
            // ref hop: depth+1, inline_depth は collect_reachable_refs 側で 0 リセット
            collect_reachable_refs(pdf, *object_ref, out, seen, depth + 1)
        }
        Object::Array(items) => {
            for item in items {
                collect_refs_in_object(pdf, item, out, seen, depth, inline_depth + 1)?;
            }
            Ok(())
        }
        Object::Dictionary(dict) => collect_refs_in_dict(pdf, dict, out, seen, depth, inline_depth + 1),
        Object::Stream(stream) => collect_refs_in_dict(pdf, &stream.dict, out, seen, depth, inline_depth + 1),
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => Ok(()),
    }
}
```

`collect_refs_in_dict`（`:781`）に `inline_depth: usize` を追加し、各値へ同 `inline_depth` で:
```rust
fn collect_refs_in_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
    inline_depth: usize,
) -> Result<()> {
    for (key, value) in dict.iter() {
        if key == b"P" {
            continue;
        }
        collect_refs_in_object(pdf, value, out, seen, depth, inline_depth)?;
    }
    Ok(())
}
```
（`collect_refs_in_dict` の呼び出し元 `collect_refs_in_object` が Dict/Stream アームで
`inline_depth + 1` を渡すので、dict 内の各値は次のエントリで再チェックされる。）

**Step 4: PASS 確認** — Run: `cargo test -p flpdf acroform_document_helper 2>&1 | grep "test result"`（PASS）

**Step 5: Commit**

```bash
git add crates/flpdf/src/acroform_document_helper.rs
git commit -m "fix(flpdf): bound acroform ref-walker inline-nesting axis (flpdf-hn1g.9)"
```

---

### Task 10: 全体ゲート（テスト・lint・カバレッジ）

**Step 1: 全テスト**

Run: `cargo test -p flpdf 2>&1 | grep -E "test result|error\["`
Expected: 全 suite PASS、0 failed

**Step 2: fmt / clippy**

Run: `cargo fmt -p flpdf && cargo clippy -p flpdf --all-targets 2>&1 | grep -E "warning|error" | head`
Expected: fmt 差分なし（CI Quality gate = `cargo fmt --check`）、clippy 警告ゼロ
（fmt が修正を入れたら `git add -u && git commit --amend --no-edit` 相当で取り込む or 個別 commit）

**Step 3: patch-coverage ゲート**

Run: `scripts/patch-coverage.sh --base main`
Expected: flpdf 変更行 100% カバー（exit 0）。未カバーがあればテスト追加。真にテスト不能な
行のみ `// cov:ignore: <理由>` で除外し PR 説明に記す。

**Step 4: doc-review grep（公開 doc に内部痕跡が無いか）**

Run: `grep -rnE '(///|//!).*flpdf-[0-9a-z.]+' crates/flpdf/src/object.rs`
Expected: 0 件（共有定数の doc に issue ID を書かない）

**Step 5: 最終 commit（あれば）**

```bash
git add -A
git commit -m "test(flpdf): cover ref-walker inline depth limits (flpdf-hn1g.9)"
```

---

## 完了基準

- 8 walker すべてに `MAX_INLINE_DEPTH` ガードが入り、深さ超過で `Err(Error::Unsupported)`。
- 共有定数は `object.rs` に1つ、rewrite_renumber も同定数を source（値 256 不変）。
- 各 walker に「超過=Err・限界=Ok・正常系回帰」テスト。flpdf 変更行 100% カバー。
- `cargo test -p flpdf` 全緑、`cargo fmt --check` 差分なし、clippy 警告ゼロ。
- acroform は ref hop 軸とインライン軸が独立に bound。
