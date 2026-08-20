# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

## 最優先方針: qpdf byte-identical 完全模倣 (pre-v1.0)

**これは本ファイルの他のすべての指示・設計判断に優先する。** pre-v1.0 の flpdf の
唯一のゴールは、qpdf の出力を **byte-identical**（1 バイトも違わない）で再現すること。

- **完全模倣**: ロジック・アルゴリズム・データ構造・処理順序を qpdf に合わせる。
  qpdf がやることを、qpdf のやり方で、qpdf と同じ順序・同じ構造で行う。
- **独自改善をしない**: 「より効率的」「より綺麗」「より Rust らしい」という理由で
  qpdf と異なる実装・出力にしない。最適化・設計改善・リファクタの自由度は v1.0 到達後に
  再検討する（pre-v1.0 では後方/前方互換も考慮しない）。
- **qpdf が oracle（正解）**: 出力が qpdf と 1 バイトでも異なれば flpdf 側のバグと
  みなす。判断に迷ったら qpdf のソース・観測挙動を正とし、自分の好み・規格解釈より優先する
  （ISO 32000 はあくまで補助。qpdf が規格と違う挙動をするなら qpdf に合わせる）。
- **逸脱は必ず明示**: byte-identical を達成できない／しない箇所は、PR・コミット・beads
  issue に理由を 1 行残す。暗黙の逸脱を作らない。

### 逸脱の 2 分類

逸脱は「**出力バイトを変えるか**」で厳密に区別する。

#### (A) 出力バイトを変える逸脱 — DEFLATE 実装のみ（唯一）

flate2 は **Pure Rust を維持し zlib（C ライブラリ）に依存しない**ため、zlib バックエンドは
feature flag（`qpdf-zlib-compat`）に隔離してある。デフォルトは miniz_oxide（Pure Rust）で、
この場合のみ DEFLATE 圧縮の出力バイトが qpdf（zlib）と異なってよい。

- byte-identical を厳密に検証する場面（compat baseline 等）では `qpdf-zlib-compat`
  feature（zlib バックエンド）を使う。
- **この 1 点が唯一の例外**。これ以外で byte-identical を崩す逸脱は認めない。
- 関連の運用注意: llvm-cov / patch-coverage は **`--features qpdf-zlib-compat
  --ignore-run-fail` で回す**。`overlay::byte_gate` の byte-identical テストが
  この feature でゲートされているため、feature 無しの計測は 600 行超の偽陽性
  未カバー行を出す（`.github/workflows/ci.yml` の Coverage job と
  `scripts/patch-coverage.sh` の fresh-mode 既定が同じ形）。

#### (B) 出力バイトを変えない内部構造の代替 — 条件付きで許容

qpdf の C++ 固有の足場を Rust の標準的な仕組みで置き換えることは、**出力バイトに
一切影響しない場合に限り**許容する。該当例: `InputSource` 階層 → `Read + Seek`
ジェネリクス、汎用 `Pl_*` シンク → `Write` / `Vec<u8>`、crypto provider 抽象 →
外部 crate の直接利用、`QPDFArgParser` → clap。

許容の条件（すべて満たすこと）:

1. **出力バイトに影響しないこと。** 証明責任は提案側にある。既存の
   `qpdf-zlib-compat` gated byte テストが該当経路を守っていることを示すか、
   守っていなければ先にゲートを追加する。
2. **アルゴリズムと処理順序は qpdf のまま。** 代替してよいのは「入れ物」だけで、
   ロジック・データ構造の意味・処理順序を変えてはならない。
   （例: base64 のエンコード結果は同一。JSON の逐次出力順は qpdf のまま）
3. **明示的に記録すること。** 当該モジュールの doc に逸脱理由を 1 行、
   [`docs/qpdf-correspondence.md`](docs/qpdf-correspondence.md) の ⚪ 行に該当箇所を
   記載する。暗黙の逸脱を作らない。加えて、該当コード自体を機械可読にマークする:
   関数・型・フィールド単位で切り離せる逸脱は `#[deprecated(note = "...")]`、
   分岐・ブロック単位の逸脱は `// qpdf-deviation: <理由>`（複数行は
   `// qpdf-deviation-start: <理由>` … `// qpdf-deviation-end`）。CI の
   `scripts/check-qpdf-deviation-markers.py --check` が書式を検証する。これにより、
   複数の既存実装を 1 つの共有 primitive へ統合する際にレビューが「既知のマーク済み
   逸脱」と「未マークの regression」を機械的に見分けられる
   （[`.claude/rules/qpdf-port-design-patterns.md`](.claude/rules/qpdf-port-design-patterns.md)
   6 参照）。

(B) は「qpdf のやり方を尊重しつつ Rust として不自然な移植を避ける」ための枠であり、
**qpdf の設計が気に入らないという理由での逸脱は pre-v1.0 では認めない**。
v1.0 以降は想定 consumer の要件に応じて qpdf 完全トレースを離れる領域が出る想定だが、
それは別途判断する。

この方針の bd メモリ版（`bd prime` で自動注入）: `bd recall pre-v1-0-qpdf-byte-identical-qtest-parity`。

### 設計に入る前に

issue に着手する前・設計やブレインストーミングを始める前に、
[`.claude/rules/qpdf-port-design-patterns.md`](.claude/rules/qpdf-port-design-patterns.md)
を読むこと。flpdf の現状から出発して設計を誤るパターンを 6 カテゴリで
予防ルール化したもの（出発点／中断シグナル／前例の検証／依存順序／逐語訳の粒度／
複数実装統合前の機械可読マーキング）。
**設計を誤ったまま実装に入ると、下の Coding Rules はもう効かない。**

## Coding Rules

Before writing or reviewing code, consult the review patterns in
[`.claude/rules/pdf-rust-review-patterns.md`](.claude/rules/pdf-rust-review-patterns.md).
これは過去のレビュー（Gemini Code Assist）で頻出した4カテゴリの落とし穴
（不要な`.clone()`、PDF間接参照の解決漏れ、符号なしキャストのオーバーフロー、
グラフ走査の境界/深さ制御）を予防ルール化したもの。

公開API向けドキュメント（docs.rs に published される `crates/*/src/` の doc コメント）を
書く・レビューするときは、併せて
[`.claude/rules/pdf-rust-doc-review-patterns.md`](.claude/rules/pdf-rust-doc-review-patterns.md)
を確認すること。beads issue ID・内部ジャーゴンの漏れ、公開/非公開コメントの境界、
rustdoc 必須要素、intra-doc リンク／doctest の健全性、公開 doc の英語統一を
5カテゴリで予防ルール化したもの。

## Test Coverage（PR 作成前ゲート）

PR を作成する**前**（beads「Session Completion」の品質ゲート手順の一部）に、
**変更行のテストカバレッジ**を確認すること。

1. 実行: `scripts/patch-coverage.sh [--base <親ブランチ>]`
   （スタック PR では親ブランチを `--base` に渡す。直前に
   `cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail
   --lcov --output-path <path>` を回した場合は
   `--lcov <path>` で再利用すると、重い再ビルドを省ける。**作業を commit してから
   実行すること** — カバレッジは作業ツリーを計測する一方ゲートは HEAD を diff する
   ため、dirty tree では偽グリーンを避けるためエラーになる。意図的に上書きする場合のみ
   `--allow-dirty`。）
2. **`flpdf`**: 変更行は 100% カバーが必須。スクリプトがゲートし、未カバー
   変更行があれば `exit≠0`。未カバー行はテストを追加するか、真にテスト不能な
   行のみ `// cov:ignore: <理由>`（行）または `// cov:ignore-start` …
   `// cov:ignore-end`（ブロック）で除外し、除外理由を PR 説明に 1 行記す。
3. **`flpdf-cli`**: 未カバー変更行は報告のみ（ブロックしない）。可能な範囲で
   テストを追加する努力目標。
4. **質的チェック（数値の後）**: 行カバレッジ 100% は「行が実行された」ことしか
   保証しない。新規/変更した公開挙動の **エラーアーム・境界値・空/極端入力** に
   対応するテストが実在するか（assertion が実質的か）を確認してから
   `gh pr create` する。

設計の根拠は
[`docs/plans/2026-06-10-patch-coverage-gate-design.md`](docs/plans/2026-06-10-patch-coverage-gate-design.md)。

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->


## Build & Test

_Add your build and test commands here_

```bash
# Example:
# npm install
# npm test
```

## Architecture Overview

_Add a brief overview of your project architecture_

## Conventions & Patterns

_Add your project-specific conventions here_
