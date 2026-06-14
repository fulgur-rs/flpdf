# 引き継ぎプロンプト — L3/ihb byte-identity 仕上げ（flpdf-6pcx）

> 次セッションの冒頭にこの内容を貼って作業を再開する。コマンド・パス・識別子は原文のまま。

---

## ゴールと方針
flpdf の `--linearize --object-streams=generate`（および `preserve`）出力を qpdf 11.9.0 と
**byte-identical** にする（CLAUDE.md 最優先方針: pre-v1.0 は qpdf 完全模倣。唯一の例外は
DEFLAT 実装で、厳密な byte 比較は `qpdf-zlib-compat` feature=zlib バックエンドで行う）。

これは epic **flpdf-vvjr**（ObjStm linearized 完全 byte-parity）の最終段。まず `bd prime` を実行し、
次を読む: `bd show flpdf-6pcx`（次の作業）、`bd show flpdf-ihb.1`（計測済み qpdf リファレンス＋設計 spec、
**再導出不要**）、`bd show flpdf-vvjr`（L3 全体）、`bd show flpdf-zbf9`（副産物バグ）。

## 作業場所
- リポジトリ: `/home/ubuntu/flpdf`
- worktree: `/home/ubuntu/flpdf/.worktrees/flpdf-vvjr-objstm-linearized-parity`
  （branch `flpdf-vvjr-objstm-linearized-parity`、origin に push 済み、tip `7cde46f`、main の上に WIP ~8 コミット）
- 絶対パス `/home/ubuntu/flpdf/...` を編集に使わない（別 worktree=main を指す）。常にこの worktree 内で作業。

## 完了済み（マージ済み・触らない）
- **L1 #360**（`flpdf-9hc.13.12` closed）: flat 全経路の deterministic /ID 直書き。`patch_deterministic_id` 撤廃。
- **L2 #361**（`flpdf-u5m8` closed）: classic-linearized 直書き（qpdf 2-pass）。

## 完了済み（WIP branch 上・未マージ） — ihb 構造再設計
`flpdf-cxef` / `flpdf-d0jf` / `flpdf-i7n1` / `flpdf-l7qf` すべて closed。独立検証済みの到達点:
**objstm-linearized 出力の オブジェクト番号付け・compressed member-set・/O（three-page=10, two-page=8）・
`qpdf --check-linearization` clean がすべて qpdf 一致**。`cmp_linearize` 9/9（classic 非objstm）byte-identical 維持、
full suite green、clippy/fmt/doc クリーン、patch-coverage 100%。

### 再導出不要の核心知見（ihb.1 design 由来）
- qpdf numbering: **second-half(1-5) → first-half standalone(6-12) → first-half compressed members(13-16)**。
  圧縮メンバは **各 half の末尾**に番号付け（→ 各 xref stream を番号順に見ると type-1→type-2、interleave 無し）。
  これが flpdf-56u「全 ObjStm を高位 tail に集約」の誤前提を覆した核心。
- qpdf chain: **startxref→first-half xref、/Prev は first-half→main、/T = main_xref_offset − 1**。
- qpdf member-set（three-page）: `{first-page Font dict, Font, Info, Pages-tree}` を **1つの first-half ObjStm** に圧縮、
  **Catalog は standalone**。

## 次の作業 = flpdf-6pcx（in_progress、byte-identity）
オブジェクト構造は一致済み。**残りは byte レベルのみ**。順に:

1. **version floor**: `crates/flpdf/src/writer.rs` の `effective_pdf_version`(~492) で、
   **object stream が出力に存在するとき**バージョンを `(1,5)` に floor する（現状 `%PDF-1.3` vs qpdf `1.5`＝byte-8 差）。
   注意: 「mode が Disable でない」ではなく「実際に objstm が出る」条件にする
   （`cmp_linearize` 非objstm goldens は 1.2 のまま＝壊さない）。

2. **zlib バックエンドで計測**（最重要）: 現状の差（flpdf 2497 vs qpdf 2521、~24 バイト＋offset drift）は
   **miniz 計測なので DEFLATE 例外ノイズが混入**している。`--features qpdf-zlib-compat` を付けて生成・比較し、
   **真の byte 差**を分離せよ。例:
   `cargo build -p flpdf-cli --features flpdf/qpdf-zlib-compat`（or 既存の cmp_linearize と同じ public API テスト経路）。
   qpdf 側は `qpdf --linearize --object-streams=generate --deterministic-id tests/fixtures/compat/three-page.pdf q.pdf`。

3. **真の object-serialization 差を解消**: version 修正後に残る zlib-feature 下の差分（dict 整形・xref-stream
   `/W`/エンコード・stream framing 等）を qpdf に合わせる。`qpdf --show-linearization` のフィールド一致＋
   `cmp -l` の差分位置で詰める。

4. **byte-identity ゲート追加**: feature-gated（`#![cfg(feature = "qpdf-zlib-compat")]`）の
   `cmp_linearize_objstm` テスト（three/two-page generate の golden 一致＋ multi-source-ObjStm の preserve fixture）。
   `cmp_linearize_tests` に倣う（`crates/flpdf/tests/cmp_linearize_tests.rs`、golden は `tests/golden/regenerate.sh` 系）。

## 並行する別件（6pcx とは独立、必要に応じて）
- **flpdf-zbf9**（open）: objstm-bearing 入力で source の `/Type /ObjStm` `/Type /XRef` 構造コンテナが
  live body object として leak。テスト `acceptance_gate_objstm_bearing_input`（`crates/flpdf-cli/tests/cli_linearize_objstm.rs`）が
  `#[ignore]` 隔離中。zbf9 着地時に un-ignore。object-model 変更（source 構造コンテナを live set から除外）。
- **single-page /O=9 vs qpdf 6**: 単一ページで qpdf は page-private resource dict を圧縮、flpdf は plain のまま。
  別 object-model gap。必要なら issue 化（現状 multi-page は一致済み）。

## 検証ゲート（毎回）
- `qpdf --check-linearization <out>` clean（three/two-page）。
- `cargo test -p flpdf --features qpdf-zlib-compat` で `cmp_linearize_tests` 9/9 **byte-identical 維持**（classic を壊さない）。
- `cargo test -p flpdf -p flpdf-cli` green / `cargo fmt --check` / `RUSTFLAGS="-D warnings" cargo clippy -p flpdf -p flpdf-cli --all-targets --all-features` / `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc -p flpdf --no-deps`。
- `scripts/patch-coverage.sh --base main` で flpdf 変更行 100%（**qpdf-zlib-compat 無し**で計測＝miniz 固定。CLAUDE.md 参照）。
- PR 前に `/roborev-refine`（L1/L2 とも指摘ゼロだった）。

## PR / 仕上げ時の注意
- WIP ~8 コミットは PR 化時に squash 推奨（`git reset --soft <main 直後> && git commit` で tree 維持）。
- PR base = `main`。`gh pr create` は GraphQL 401 になるので **REST**（`gh api repos/fulgur-rs/flpdf/pulls -X POST ...`）で作る。
- beads auto-export の `git add failed` 警告は良性。`bd dolt push` で保存。
- subagent に投げる場合: 客観ゲート（check-linearization / cmp byte-identity / coverage）が機械検証なので、
  fresh-context subagent + 「cascade したら止めて報告」HARD GUARD で安全に進められる（本epic はそれで完遂してきた）。

## 完了の定義（vvjr クローズ条件）
`qpdf-zlib-compat` 下で flpdf の `--linearize --object-streams=generate --deterministic-id` 出力が
three/two-page で qpdf 11.9.0 と **byte-identical**、preserve も同様、`qpdf --check-linearization` clean、
全ゲート緑。その後 6pcx → vvjr を close、L4（`flpdf-ari7`: ObjStm /ID 直書き parity）へ。
