# Outlines Multi-Container byte-parity Verification Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** K=200 アウトラインアイテムで 3つの ObjStm コンテナに分散する fixture を追加し、`group_length` 連続性を qpdf 11.9.0 と byte-identical で検証する。

**Architecture:** 既存の `gen_outlines_gap.py 80 80` と同じスクリプトを K=200 S=80 で呼び出し、281 eligible オブジェクト → ceil(281/100)=3 コンテナ（94+94+93）を生成。全コンテナがアウトライン優先規則で ContainerPart::Rest → part4_batches に連続配置される。コード変更なし、テスト追加のみ。

**Tech Stack:** Rust, Python3, qpdf 11.9.0, `tests/golden/regenerate.sh`, `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

---

## Task 1: fixture PDF を生成して tests/fixtures/compat/ に配置

**Files:**
- Create: `tests/fixtures/compat/objstm-lin-outlines-200-80.pdf`

**Step 1: fixture を生成**

```bash
python3 docs/plans/tools/gen_outlines_gap.py 200 80 > tests/fixtures/compat/objstm-lin-outlines-200-80.pdf
```

**Step 2: qpdf で構文チェック**

```bash
qpdf --check tests/fixtures/compat/objstm-lin-outlines-200-80.pdf
```

期待値: "No syntax or stream encoding errors found"

---

## Task 2: qpdf golden を生成して tests/golden/references/ に配置

**Files:**
- Create: `tests/golden/references/objstm-lin-outlines-200-80/linearize-objstm.pdf`

**Step 1: ディレクトリ作成**

```bash
mkdir -p tests/golden/references/objstm-lin-outlines-200-80
```

**Step 2: ObjStm linearize golden 生成**

```bash
qpdf --linearize --object-streams=generate --deterministic-id \
  tests/fixtures/compat/objstm-lin-outlines-200-80.pdf \
  tests/golden/references/objstm-lin-outlines-200-80/linearize-objstm.pdf
```

**Step 3: linearization check**

```bash
qpdf --check-linearization tests/golden/references/objstm-lin-outlines-200-80/linearize-objstm.pdf
```

期待値: "no linearization errors"

**Step 4: /O キー（アウトライン hint）が存在することを確認**

```bash
python3 - << 'EOF'
with open("tests/golden/references/objstm-lin-outlines-200-80/linearize-objstm.pdf", "rb") as f:
    data = f.read()
idx = data[:2000].find(b"/O ")
assert idx >= 0, "/O key not found in linearization dict"
print("OK: /O key found:", data[idx:idx+20])
EOF
```

---

## Task 3: regenerate.sh を更新（再生成時の自動化）

**Files:**
- Modify: `tests/golden/regenerate.sh`

**Step 1: regenerate.sh を読む（変更箇所の特定）**

```bash
grep -n "outlines-80-80\|G6HB2_FIX\|useoutlines-80-80" tests/golden/regenerate.sh | head -20
```

**Step 2: G6HB2_FIX に新 fixture を追加**

`tests/golden/regenerate.sh` の `G6HB2_FIX` 連想配列に以下を追加:

```bash
    [objstm-lin-outlines-200-80]="gen_outlines_gap.py 200 80"
```

既存の `[objstm-lin-outlines-80-80]="gen_outlines_gap.py 80 80"` 行の直後に挿入。

**Step 3: ObjStm golden 生成ループに新 fixture を追加**

regenerate.sh の以下の行を見つける:

```bash
for stem in objstm-lin-sharedfonts-100 objstm-lin-mixed-60-70 \
            objstm-lin-threepage-2-120 objstm-lin-disc-2-250-2 \
            objstm-lin-openaction-80-80 objstm-lin-outlines-80-80 \
            objstm-lin-useoutlines-80-80; do
```

`objstm-lin-useoutlines-80-80` の後に `objstm-lin-outlines-200-80` を追加:

```bash
for stem in objstm-lin-sharedfonts-100 objstm-lin-mixed-60-70 \
            objstm-lin-threepage-2-120 objstm-lin-disc-2-250-2 \
            objstm-lin-openaction-80-80 objstm-lin-outlines-80-80 \
            objstm-lin-useoutlines-80-80 objstm-lin-outlines-200-80; do
```

**Step 4: 変更を確認**

```bash
grep -n "200-80\|outlines" tests/golden/regenerate.sh
```

---

## Task 4: cmp_linearize_objstm_tests.rs にテストを追加

**Files:**
- Modify: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

**Step 1: 既存の outlines テストを参照**

```bash
grep -n -A5 "outlines_objstm\|outlines-80-80" crates/flpdf/tests/cmp_linearize_objstm_tests.rs
```

**Step 2: テストを末尾に追加**

`crates/flpdf/tests/cmp_linearize_objstm_tests.rs` のファイル末尾に以下を追加:

```rust
// outlines-200-80 (flpdf-vvjr.3): outline tree with K=200 items spans 3 ObjStm
// containers (281 eligible objects, even split: ceil(281/100)=3, 94+94+93).
// All three containers route to ContainerPart::Rest (outline priority applies even
// for the mixed container). Verifies group_length consecutiveness in the multi-
// container case: first_object..first_object+nobjects-1 covers all three.
#[test]
fn outlines_multi_container_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("objstm-lin-outlines-200-80.pdf", "objstm-lin-outlines-200-80");
}

#[test]
fn outlines_multi_container_objstm_byte_identical_to_qpdf() {
    assert_strict("objstm-lin-outlines-200-80.pdf", "objstm-lin-outlines-200-80");
}
```

**Step 3: ファイル末尾を確認**

```bash
tail -20 crates/flpdf/tests/cmp_linearize_objstm_tests.rs
```

---

## Task 5: テスト実行で byte-identical を検証

**Step 1: structural テストのみ先に実行（feature なし）**

```bash
cargo test -p flpdf --test cmp_linearize_objstm_tests \
  outlines_multi_container_objstm_structurally_byte_identical_to_qpdf 2>&1 | tail -10
```

期待値: `test ... ok`

**Step 2: strict byte-identical テスト実行（qpdf-zlib-compat feature 必須）**

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests \
  outlines_multi_container_objstm_byte_identical_to_qpdf 2>&1 | tail -10
```

期待値: `test ... ok`

**Step 3: 既存テストのリグレッションなし確認**

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests 2>&1 | tail -5
```

期待値: `test result: ok. N passed; 0 failed`

**Step 4: テストが失敗した場合の調査**

もし `byte_identical` が失敗した場合は `group_length` の計算を確認:
- `compute_outline_hint_info` の `units` が 3 コンテナを収集しているか
- `build_outline_hint_table` の連続範囲が正しいか
- ObjStm コンテナが実際に連続番号かを `qpdf --qdf` の出力で確認

---

## Task 6: patch-coverage 確認 → コミット

**Step 1: patch-coverage（テスト追加なのでコードカバレッジ変化なし）**

```bash
# テストファイルは cov 対象外（tests/ディレクトリ）なのでスキップ可
# fixture/golden は binary なので cov 対象外
echo "Coverage gate: no src/ code changed, skip"
```

**Step 2: git add して commit**

```bash
git add tests/fixtures/compat/objstm-lin-outlines-200-80.pdf \
        tests/golden/references/objstm-lin-outlines-200-80/ \
        tests/golden/regenerate.sh \
        crates/flpdf/tests/cmp_linearize_objstm_tests.rs
git commit -m "test(flpdf-vvjr.3): verify multi-container outline group_length with K=200 fixture

Add objstm-lin-outlines-200-80 fixture (281 eligible objects → 3 ObjStm
containers via even split). All three route to ContainerPart::Rest and are
numbered consecutively, confirming group_length summation is correct in the
multi-container case. byte-identical to qpdf 11.9.0."
```
