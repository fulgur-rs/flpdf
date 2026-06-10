# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

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
   `cargo llvm-cov --workspace --lcov --output-path <path>` を回した場合は
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
