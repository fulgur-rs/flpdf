# flpdf-1ygo: trailer の `/Encrypt` を `/ID` の直後に書く Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** classic full-rewrite trailer で `/Encrypt` を qpdf と同じ位置（`/ID` の直後、
trailer 末尾）に書くようにし、暗号化文書の出力を qpdf 11.9.0 と全文 byte-identical にする。

**Architecture:** 修正点は1箇所のみ。`Dictionary::write_pdf_trailer`
(`crates/flpdf/src/object.rs:955`) は現在 `/ID` だけをループから除外して末尾に書いている。
`/Encrypt` も同様にループから除外して退避し、`/ID` を書いた直後（値がある場合のみ）に
書き足す。他に類似コードが4箇所あるが、いずれも変更しない。うち3箇所
（`write_qdf_trailer` / `write_incremental_trailer` / `writer/plain/xref.rs` の
xref-stream 分岐）は、実際には `/Encrypt` を含む辞書が到達しないことをコード追跡で
確認済み。残り1箇所（`writer.rs` の `write_pdf_full_rewrite_inner` 内
`XrefForm::Stream` 分岐、`--object-streams=generate --encrypt` で到達する）は
`/Encrypt` を含む辞書が実際に到達するが、この分岐は `write_pdf_with_id_writer`
経由でxref-stream辞書全体を既にプレーンな辞書順で書いており（`/Encrypt` の位置だけで
なく `/Type` `/Length` `/Filter` 等のフィールド順もqpdfと不一致）、qpdfとのbyte
parityを最初から取っていない既存の逸脱（`writer.rs:4242-4249` のコメント参照）。
`/Encrypt` の位置だけ直しても全体のbyte parityは達成できないため、この issue の
スコープ外として別issue（`flpdf-wt2w`）で追跡する（bd issue `flpdf-1ygo` の
design フィールドに調査の詳細を記録済み）。

**Tech Stack:** Rust, cargo test, `/usr/bin/qpdf` 11.9.0（オラクル比較用）。

**qpdf 対応関係:** `QPDFWriter.cc:1160-1236`（`writeTrailer`）+ `:2009-2031`
（`getTrimmedTrailer`）。`getTrimmedTrailer` が trailer コピーから `/ID` `/Encrypt`
`/Prev` 等を除去 → 残りをソート順で書く → `/ID` を特別に書く →
`which != t_lin_second` なら直後に `/Encrypt <objid> 0 R` を追記、という順序。
flpdf の `write_pdf_trailer` の実際の呼び出し元（classic full-rewrite 非QDF経路、
`writer.rs:4073`）には `t_lin_second` 相当の分岐が存在しないので、
「`/Encrypt` があれば常に `/ID` の直後に書く」で正しい。

**事前検証（RED 状態の実測）:** `--static-id --static-aes-iv --encrypt "" "" 128
--use-aes=y -- one-page.pdf` を flpdf と `qpdf 11.9.0`（`qpdf-zlib-compat` feature
でビルド）で実行し diff したところ、trailer 部分の11箇所の diff run はすべて
`/Encrypt` の位置ズレのみに起因（`/ID` の値自体は同一）と確認済み:

```
flpdf: ... /Size 9 /Encrypt 8 0 R /Info 2 0 R /Root 1 0 R /Size 9 /ID [<..><..>] >>
qpdf : ... /Info 2 0 R /Root 1 0 R /Size 9 /ID [<..><..>] /Encrypt 8 0 R >>
```
（実際は `/Encrypt` は先頭にあるが、上は位置関係を示す簡略表記）

---

### Task 1: `Dictionary::write_pdf_trailer` の単体テストを先に書く（RED）

**Files:**
- Modify: `crates/flpdf/src/object.rs`（`#[cfg(test)] mod tests` 内、既存の
  `trailer_emits_sorted_keys_with_id_last` 等のすぐ後に追加）

**Step 1: 失敗するテストを書く**

`crates/flpdf/src/object.rs` の `mod tests` 内、`trailer_without_id_is_sorted`
テストの直後に以下を追加する（既存テストの `Object::reference` / `Object::Integer`
の使い方に合わせる）:

```rust
    /// `/Encrypt` sorts alphabetically before `/ID`/`/Info`/`/Root`/`/Size`, but
    /// qpdf's `writeTrailer` special-cases it too: it is written right after
    /// `/ID`, at the very end of the trailer (`QPDFWriter.cc:1224-1231`, guarded
    /// on `which != t_lin_second` — a guard this function's only caller, the
    /// classic full-rewrite non-QDF path, never trips). Pin that qpdf order.
    #[test]
    fn trailer_emits_encrypt_right_after_id() {
        let mut d = Dictionary::new();
        // Inserted out of order; BTreeMap sorts /Encrypt first alphabetically —
        // write_pdf_trailer must still move it to the very end, after /ID.
        d.insert(b"Size", Object::Integer(8));
        d.insert(b"Encrypt", Object::reference(ObjectRef::new(5, 0)));
        d.insert(
            b"ID",
            Object::Array(vec![Object::Integer(1), Object::Integer(2)]),
        );
        d.insert(b"Info", Object::reference(ObjectRef::new(2, 0)));
        d.insert(b"Root", Object::reference(ObjectRef::new(1, 0)));
        let mut out = Vec::new();
        d.write_pdf_trailer(&mut out, None);
        assert_eq!(
            out,
            b"<< /Info 2 0 R /Root 1 0 R /Size 8 /ID [ 1 2 ] /Encrypt 5 0 R >>".to_vec()
        );
    }

    /// A trailer with `/Encrypt` but no `/ID` still moves `/Encrypt` to the end
    /// (qpdf's special-case fires independently of whether `/ID` is present).
    #[test]
    fn trailer_emits_encrypt_last_without_id() {
        let mut d = Dictionary::new();
        d.insert(b"Size", Object::Integer(8));
        d.insert(b"Encrypt", Object::reference(ObjectRef::new(5, 0)));
        d.insert(b"Root", Object::reference(ObjectRef::new(1, 0)));
        let mut out = Vec::new();
        d.write_pdf_trailer(&mut out, None);
        assert_eq!(out, b"<< /Root 1 0 R /Size 8 /Encrypt 5 0 R >>".to_vec());
    }
```

**Step 2: 失敗を確認する**

Run: `cargo test -p flpdf --lib trailer_emits_encrypt -- --nocapture`
Expected: 2 tests, both FAIL — 現状の出力は
`<< /Encrypt 5 0 R /Info 2 0 R /Root 1 0 R /Size 8 /ID [ 1 2 ] >>`
（`/Encrypt` が先頭）になっているはず。

**Step 3: コミット**

まだ実装を変えていないので、このタスクは Task 2 のコミットに含める
（テストのみの中間コミットは作らない — RED を確認したら Task 2 に進む）。

---

### Task 2: `write_pdf_trailer` を修正する（GREEN）

**Files:**
- Modify: `crates/flpdf/src/object.rs:930-976`（doc コメントと関数本体）

**Step 1: doc コメントを更新する**

現状（930-954行）の doc コメントに `/Encrypt` の特別扱いを追記する。既存の
「`/ID` が最後」という説明に、「`/Encrypt` も `/ID` の直後に特別扱いされる」ことを
追加する形。既存の qpdf 引用・doctest ライクな説明トーンを踏襲すること
（英語で書く — CLAUDE.md 公開doc方針: `object.rs` は `crates/*/src/` 配下の
published 面）。

```rust
    /// Serialize a document trailer dictionary in qpdf's trailer key order,
    /// appending to `out`.
    ///
    /// qpdf writes the trailer with every key in sorted (`BTreeMap`) order
    /// **except `/ID` and `/Encrypt`, which are pulled out and emitted last, in
    /// that order** — structurally the same special-casing it applies to
    /// `/Length` in stream dictionaries (see
    /// [`write_pdf_stream`](Self::write_pdf_stream)). `/Encrypt` sorts
    /// alphabetically before `/ID`, `/Info`, `/Root`, and `/Size`, but qpdf's
    /// `writeTrailer` writes it last regardless (`QPDFWriter.cc:1224-1231`,
    /// guarded on `which != t_lin_second` — a guard this function's only
    /// caller, the classic full-rewrite non-QDF path, never trips, so
    /// `/Encrypt` is always moved when present). Verified against
    /// `qpdf --static-id --encrypt` 11.9.0:
    /// `<< /Info .. /Root .. /Size N /ID [..] /Encrypt N 0 R >>`. Layout
    /// otherwise matches [`write_pdf`](Self::write_pdf) (compact, one line).
    /// If neither key is present the output is plain sorted order.
    ///
    /// When `id_writer` is `Some`, the `/ID` *value* is produced by that closure
    /// (the `b" /ID "` key token is still emitted) instead of serializing the
    /// dictionary's stored `/ID` value. This lets the caller compute the `/ID`
    /// directly from the bytes written so far — used by the deterministic-`/ID`
    /// writer to emit qpdf's content-derived identifier inline rather than via a
    /// placeholder-then-patch step. When `id_writer` is `None`, the stored
    /// `/ID` value is routed through [`write_id_style_value`] to reproduce
    /// qpdf's `writeTrailer` compact `[<hex1><hex2>]` shape without spaces
    /// (qpdf's trailer hand-rolls `/ID`; the generic array serializer would
    /// otherwise insert separating spaces). The closure runs only when the
    /// `/ID` key is present in the dictionary; if it is absent, `id_writer`
    /// is ignored.
    pub(crate) fn write_pdf_trailer(&self, out: &mut Vec<u8>, id_writer: Option<TrailerIdWriter>) {
        out.extend_from_slice(b"<<");
        let mut id_value: Option<&Object> = None;
        let mut encrypt_value: Option<&Object> = None;
        for (key, value) in self.iter() {
            if key == b"ID" {
                id_value = Some(value);
                continue;
            }
            if key == b"Encrypt" {
                encrypt_value = Some(value);
                continue;
            }
            out.extend_from_slice(b" /");
            write_name_escaped(out, key);
            out.push(b' ');
            value.write_pdf(out);
        }
        if let Some(value) = id_value {
            out.extend_from_slice(b" /ID ");
            match id_writer {
                Some(write_id) => write_id(out),
                None => write_id_style_value(out, value),
            }
        }
        if let Some(value) = encrypt_value {
            out.extend_from_slice(b" /Encrypt ");
            value.write_pdf(out);
        }
        out.extend_from_slice(b" >>");
    }
```

**Step 2: テストを実行して成功を確認する**

Run: `cargo test -p flpdf --lib trailer_emits_encrypt -- --nocapture`
Expected: 2 tests PASS

Run: `cargo test -p flpdf --lib object:: -- --nocapture`（既存の trailer/stream
関連ユニットテストが全部緑のままであることの回帰確認。特に
`trailer_emits_sorted_keys_with_id_last` / `trailer_id_writer_substitutes_value_but_keeps_id_last`
/ `trailer_without_id_is_sorted` — `/Encrypt` キーが無いので今回の変更は no-op のはず）
Expected: 全て PASS

**Step 3: コミット**

```bash
git add crates/flpdf/src/object.rs
git commit -m "fix(writer): write trailer /Encrypt right after /ID, matching qpdf

qpdf's QPDFWriter::writeTrailer (QPDFWriter.cc:1224-1231) writes /Encrypt
last, after /ID, regardless of its alphabetical position. flpdf's
write_pdf_trailer only special-cased /ID, so /Encrypt (sorting before
/ID//Info//Root//Size) landed at the front of the trailer instead."
```

---

### Task 3: 既存の `trailer_order_tests.rs`（非暗号化）が壊れていないことを確認

**Files:**
- No changes — 確認のみ

**Step 1: 実行**

Run: `cargo test -p flpdf --test trailer_order_tests -- --nocapture`
Expected: 2 tests PASS（このテストは `/Encrypt` を含まないので今回の変更は no-op
のはずだが、明示的に確認する）

このタスクにコミットは無い（確認のみ）。

---

### Task 4: CLI e2e — 実際の `/usr/bin/qpdf` との全文 byte 比較テストを追加

**Files:**
- Modify: `crates/flpdf-cli/tests/encrypt_cli_tests.rs`

背景: 既存の `static_aes_iv_matches_the_vector_qpdf_writes`（934-1000行）の doc
コメントは「IV 以外に 2 つの既知の乖離（`/U` padding、trailer `/Encrypt` 位置）が
あるので whole-document 比較ではなく IV のみを比較している」と明記している。
`/U` padding は `flpdf-bv2r`（PR #639、既に main にマージ済み）で解消済み。この
issue が解消すれば残る既知の乖離はゼロになるので、全文比較テストを新規に追加する。
既存の IV-only テストは（IV 単体の観測というテスト意図が別物なので）そのまま残す
（`flpdf-l5sk` の precedent — 既存の自己決定性テストを残したのと同じ理由）。

**Step 1: 失敗するテストを書く（この時点ではまだ RED にはならない — Task 2 で
既に修正済みのため、むしろこのタスクは「新しい確認テストを追加して GREEN で通る
ことを確認する」タスクになる。RED を確認したい場合は `git stash`（Task 2 の修正が
既に commit 済みのため working tree には何も残っておらず、意図した RED は
再現できない）ではなく、Task 2 の**親コミット**を一時的な worktree または
detached checkout でビルドし、そこで同じテストを実行して fail することを
確認する。単に「Task 2 の修正がなければこのテストは fail していたはず」という
事実を把握した上で進めてもよい。）**

`static_aes_iv_matches_the_vector_qpdf_writes` のすぐ後（1000行目の後）に追加:

**実装時の追加調査（必須）**: 256-bit（V=5/AESV3, R=6）が実際に
byte-identical を達成できるか、実装前に必ず実測で確認すること。qpdf の
V5 鍵導出・`/U` `/UE` `/O` `/OE` `/Perms` 計算はいずれも
`QUtil::initializeWithRandomBytes` でランダムなソルトを混ぜており
（`QPDF_encryption.cc:1198,610,629,652`）、`--static-id`/`--static-aes-iv`
はこれを一切固定しない。そのため **qpdf 自身の256-bit出力も同一コマンドの
2回実行で一致しない**（実測で確認: `qpdf --static-id --static-aes-iv
--encrypt "" "" 256 --` を2回実行して diff すると暗号化文字列の途中から
食い違う）。したがって256-bitでの byte-identical assertion はそもそも
成立しない主張であり、以下のスニペットは **128-bit のみ**を対象にする:

```rust
/// Whole-document byte parity for AES-128 (V=4/AESV2) encrypted output
/// against real qpdf.
///
/// Once `flpdf-l5sk` (the static IV), `flpdf-bv2r` (the `/U` padding), and
/// `flpdf-1ygo` (this trailer `/Encrypt` position) all landed, the only
/// remaining flpdf/qpdf divergence for
/// `--static-id --static-aes-iv --encrypt <user> <owner> 128 --use-aes=y`
/// output is the DEFLATE backend — hence the `qpdf-zlib-compat` gate below.
/// Supersedes the vector-only comparison in
/// `static_aes_iv_matches_the_vector_qpdf_writes` as the byte-identical proof;
/// that test stays too (it documents the narrower IV claim on its own and
/// needs no feature gate).
///
/// 256-bit (V=5/AESV3, R=6) is intentionally NOT covered here: its file
/// encryption key and `/U`/`/UE`/`/O`/`/OE`/`/Perms` values mix in random
/// salt bytes that neither `--static-id` nor `--static-aes-iv` seed
/// (`QPDF_encryption.cc:1198,610,629,652`), so even qpdf's own 256-bit
/// output is not stable run-to-run — a byte-identity assertion there would
/// be inherently flaky, independent of flpdf's correctness.
#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn encrypted_document_is_byte_identical_to_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ours = tmp.path().join("flpdf.pdf");
    let theirs = tmp.path().join("qpdf.pdf");
    let input = fixture(ONE_PAGE_FIXTURE);

    let args = [
        "--static-id",
        "--static-aes-iv",
        "--encrypt",
        "",
        "",
        "128",
        "--use-aes=y",
        "--",
    ];

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(&input)
        .arg(&ours)
        .assert()
        .success();

    let qpdf = std::process::Command::new("qpdf")
        .args(args)
        .arg(&input)
        .arg(&theirs)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf reference run failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let mine = std::fs::read(&ours).unwrap();
    let reference = std::fs::read(&theirs).unwrap();
    assert_eq!(
        mine, reference,
        "AES-128 (V=4/AESV2): flpdf output must be byte-identical to qpdf 11.9.0"
    );
}
```

RED を確認したい場合、Task 2 の親コミットを一時 worktree にチェックアウトして
同じテストを実行する（`git stash` は使わない — Task 2 は既に commit 済みなので
stash は無関係な未追跡ファイルしか退避せず、意図した RED を再現できない）:

```bash
git worktree add /tmp/flpdf-pre-fix-check <Task-2のコミットの親>
cd /tmp/flpdf-pre-fix-check
cargo test -p flpdf-cli --features qpdf-zlib-compat --test encrypt_cli_tests \
  encrypted_document_is_byte_identical_to_qpdf -- --nocapture
cd - && git worktree remove /tmp/flpdf-pre-fix-check
```
Expected（親コミット側）: FAIL（`/Encrypt` 位置の diff で assert_eq! が落ちる）

**Step 2: テストを実行して成功を確認する（修正を戻した状態で）**

Run: `cargo test -p flpdf-cli --features qpdf-zlib-compat --test encrypt_cli_tests \
  encrypted_document_is_byte_identical_to_qpdf -- --nocapture`
Expected: PASS（128-bit のみ。256-bit は対象外 — 理由は上記）

既存テストも壊れていないことを確認:
Run: `cargo test -p flpdf-cli --features qpdf-zlib-compat --test encrypt_cli_tests`
Expected: 全て PASS

**Step 3: 既存テストの doc コメントを更新する**

`static_aes_iv_matches_the_vector_qpdf_writes` の doc コメント（934-953行）から
「trailer `/Encrypt` 位置」の既知乖離への言及を外し、`encrypted_document_is_byte_identical_to_qpdf`
へのポインタに置き換える（qpdf-rust-doc-review-patterns.md 方針: 内部issue番号では
なく事実で書く）。以下のように更新:

```rust
/// `--static-aes-iv` exists so that output can be compared with qpdf's, so the
/// bytes have to match *qpdf's*, not merely be stable across flpdf runs. The
/// test above pins determinism and would stay green for any vector at all.
///
/// qpdf's static vector is `14 * (1 + i)` (`libqpdf/Pl_AES_PDF.cc:133-137`,
/// reached from `QPDFWriter::setStaticAesIV`, `libqpdf/QPDFWriter.cc:292-297`)
/// and CBC writes it at the head of every ciphertext (`:161-163`), so it is
/// observable in the output.
///
/// This compares the vector itself rather than the whole document — see
/// `encrypted_document_is_byte_identical_to_qpdf` below for the full-file
/// comparison. That test requires the `qpdf-zlib-compat` feature (DEFLATE
/// output must match qpdf's zlib backend); this one does not, because the
/// initialization vector precedes the ciphertext and is independent of how
/// the payload was compressed.
#[test]
fn static_aes_iv_matches_the_vector_qpdf_writes() {
```

**Step 4: コミット**

```bash
git add crates/flpdf-cli/tests/encrypt_cli_tests.rs
git commit -m "test(cli): add whole-document qpdf byte-parity test for AES encryption

Now that flpdf-l5sk, flpdf-bv2r, and this trailer /Encrypt fix have all
landed, --static-id --static-aes-iv --encrypt output has no known qpdf
divergence besides the DEFLATE backend (qpdf-zlib-compat gate)."
```

---

### Task 5: CI にテストを配線する

**Files:**
- Modify: `.github/workflows/ci.yml`

**Step 1: 明示的なテスト行を追加する**

`.github/workflows/ci.yml` の既存の `qpdf-zlib-compat` ゲート済みテスト一覧
（151-197行あたり、`cli_byte_identical_overlay` の後）に以下を追加する:

```yaml
          # Gated behind qpdf-zlib-compat (DEFLATE backend must match qpdf's
          # zlib); the default `cargo test` above only runs the IV-only variant.
          cargo test -p flpdf-cli --features qpdf-zlib-compat --test encrypt_cli_tests \
            encrypted_document_is_byte_identical_to_qpdf
```

既存の行のインデント・コメントスタイル（`#` コメント + `run:` 内の複数行）に
厳密に合わせること。実際のファイルを読んでから挿入位置と書式を決める
（`sed -n '176,198p' .github/workflows/ci.yml` で確認）。

**Step 2: 確認**

`actionlint` があれば流す。無ければ YAML の構文だけ目視確認
（`python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"`）。

**Step 3: コミット**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run the new AES byte-parity test explicitly under qpdf-zlib-compat"
```

---

### Task 6: patch-coverage・fmt・clippy・全テストスイート

**Files:** なし（ゲート実行のみ）

**Step 1: フォーマット**

Run: `cargo fmt --check`
Expected: 差分なし（差分があれば `cargo fmt` してから再コミット）

**Step 2: clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: warning ゼロ

**Step 3: 全テストスイート（デフォルト features）**

Run: `cargo test --workspace`
Expected: 全て PASS

**Step 4: qpdf-zlib-compat 全テストスイート**

Run: `cargo test --workspace --features qpdf-zlib-compat`
Expected: 全て PASS

**Step 5: patch-coverage**

作業を commit してから（dirty tree だとゲートがエラーになる — CLAUDE.md 参照）:

```bash
scripts/patch-coverage.sh --base main
```

Expected: `flpdf` の変更行 100% カバー。もし未カバー行があれば Task 1/4 のテストで
埋まっていない分岐がないか確認する（`write_pdf_trailer` の `encrypt_value` が
`None` のままの分岐は既存テスト `trailer_without_id_is_sorted` 等で既にカバー
されているはず）。

**Step 6: 質的チェック**

CLAUDE.md の Test Coverage セクション Step 4 に従い、以下を目視確認する:
- `encrypt_value` が `Some` の場合の分岐 → Task 1 の新規ユニットテストでカバー
- `encrypt_value` が `None` の場合の分岐（既存の暗号化なしパス）→ 既存の
  `trailer_emits_sorted_keys_with_id_last` 等でカバー
- `/ID` と `/Encrypt` の両方がある場合の順序 → Task 1 の
  `trailer_emits_encrypt_right_after_id` でカバー
- `/Encrypt` のみ（`/ID` 無し）の場合 → Task 1 の
  `trailer_emits_encrypt_last_without_id` でカバー
- 実ファイルでの全文一致 → Task 4 の CLI テストでカバー（128-bit のみ。256-bit は
  qpdf 自身の出力が非決定的なため対象外 — Task 4 参照）

このタスクにコミットは無い（確認のみ、Step 1 のフォーマット崩れがあった場合のみ
コミットする）。

---

### Task 7: bd issue のクローズ準備・PR 作成

**Files:** なし

**Step 1: bd issue のノートを更新**

```bash
bd update flpdf-1ygo --notes "$(cat <<'NOTES'
実装完了。write_pdf_trailer (object.rs) に /Encrypt の特別扱いを追加。
実測: --static-id --static-aes-iv --encrypt "" "" 128 --use-aes=y --
one-page.pdf を qpdf-zlib-compat ビルドで qpdf 11.9.0 と比較し、全文
byte-identical になったことを確認。
NOTES
)"
```

**Step 2: `superpowers:verification-before-completion` に従い最終確認**

Task 6 のゲート結果を再掲できる状態にしておく（コマンドと実際の出力）。

**Step 3: `superpowers:finishing-a-development-branch` に従い PR を作成**

CLAUDE.md の Session Completion 手順（push 必須）に従う。PR 本文には:
- 何を直したか（trailer `/Encrypt` の位置）
- なぜ他の4箇所を変更しなかったか — うち3箇所（QDF / incremental /
  `writer/plain/xref.rs` の xref-stream 分岐）は到達不能、残り1箇所
  （`writer.rs` の legacy `XrefForm::Stream` 分岐）は到達可能だが
  xref-stream辞書全体が既にqpdfとbyte parityを取っていない既存の逸脱で
  あり `flpdf-wt2w` として別途追跡（要約を記載）
- 副産物として `flpdf-txag`（linearize+encrypt 機能欠如）と `flpdf-wt2w`
  （xref-stream形式のbyte parity欠如）を別issueとして起票した旨

**Step 4: `bd close flpdf-1ygo`**（ユーザー確認後）
