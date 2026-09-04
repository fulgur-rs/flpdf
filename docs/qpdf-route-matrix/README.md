# flpdf ↔ qpdf route matrix（canonical / bridge / consumer 棚卸し）

**Oracle:** qpdf 11.9.0（`scripts/fetch-qpdf-source.sh --print-path` で解決される pinned source と
`/usr/bin/qpdf` 11.9.0 の実機挙動）。本表の引用（`libqpdf/X.cc:N-M`、`crates/.../file.rs::Symbol`）は
`scripts/check-qpdf-route-matrix.py --check` でファイル・行範囲・識別子の実在を検証する。
**関連:** [`docs/qpdf-correspondence.md`](../qpdf-correspondence.md)（責務対応表。本表はその上に
「経路（route）」軸を足したもので、対応表の行を置き換えない）/ Beads `flpdf-3yn9.41`（親 epic `flpdf-3yn9`）
**調査日:** 2026-09-04（main `8fd1a2bf` + in-flight PR #1486 の writer 差分を含む）

## 1. 目的

同じ qpdf 責務が flpdf の複数経路に分散していると、個別テストが通っても全 consumer で同じ挙動になる
保証がなく、route を一括切替したときに差分の責任箇所も追えない（直近の例: encrypted non-linearized
Preserve ObjStm の採番が、qpdf の enqueue 時 container-first に対して一部 route だけ Catalog-first +
container-above-max だった — `flpdf-hi08` / PR #1486）。本表は残る mixed route を横断して

1. 各 qpdf 責務の **canonical owner を 1 つだけ** 定め、
2. legacy bridge を「旧表現を翻訳する層」としてのみ残し、その **残 caller を機械的に追跡** し、
3. consumer を 1 つずつ cutover するための **順序・前提・RED test・完了判定** を定義する

ための preflight である。production semantics・public API はこの文書では変えない。

## 2. 方法

- **qpdf から出発する。** 各領域はまず qpdf 側の state / call order / error・warning boundary を
  書き出し、その後で flpdf の entrypoint を対応付ける（`.claude/rules/qpdf-port-design-patterns.md` 1）。
- **識別子・行範囲は書く前に実在確認する**（同 7）。qpdf 側は `rg -n '<symbol>' $Q/libqpdf/<file>` と
  `sed -n 'N,Mp'` で読んだ範囲だけを引用する。flpdf 側は `rg -n` の出力を根拠に caller を数える。
- **caller の数え方**: `rg -n '\b<symbol>\b' crates --glob '*.rs'` を production（`src/` の非 `#[cfg(test)]`
  部分）と test（`tests/` および `mod tests` 以降）に分けて `prod: N (files) / test: M` と書く。
- **probe の書式**: source だけで観測挙動が決まらない行は `probe: <コマンド> → <観測>` を evidence 列に書く。
  probe を実行していない推測は書かない（unknown にする）。
- **in-flight 差分**: PR #1486（`feature/flpdf-hi08-encrypted-preserve-objstm`）が `writer.rs` を変更中の
  ため、領域 D は main + #1486 の状態を記述し、#1486 由来の行に `(#1486)` を付ける。

### 行フォーマット（各領域ファイルの表は 8 列固定）

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|

- **qpdf responsibility owner**: `QPDF::resolve` のような実在識別子（複数可）。
- **qpdf evidence**: `libqpdf/QPDF.cc:1700-1753` / `include/qpdf/QPDF.hh:724-996` / `probe: ...`。
- **flpdf current entrypoint**: `crates/flpdf/src/reader.rs::Pdf::resolve`（visibility を併記: `pub` / `pub(crate)` / private）。
- **callers (prod / test)**: `prod: 3 (writer.rs, job/lifecycle.rs) / test: 12`。
- **classification**: §3 の 4 値のみ。
- **canonical owner**: その責務の flpdf 側正本（1 つだけ）。無ければ `absent`。
- **remaining bridge callers / notes**: bridge / mixed は残 caller を全列挙。unknown は必要 probe。

## 3. 分類定義

| 分類 | 定義 |
|---|---|
| **canonical** | flpdf の当該 entrypoint がその qpdf 責務の唯一の正本で、アルゴリズム・呼び出し順序が cite した qpdf code と 1:1 に対応し、production caller が全てここを通る。 |
| **bridge** | 旧表現と canonical 表現を翻訳するためだけに存在し、それ自体に qpdf 対応物が無い経路。残 caller を全列挙する（ゼロなら削除候補）。bridge に qpdf semantics を足さない。 |
| **mixed** | 1 つの qpdf 責務が flpdf 側で 2 つ以上の経路に分かれ、順序・採番・診断のいずれかが経路間で異なりうる状態。または 1 つの flpdf 経路が 2 つ以上の qpdf 責務を畳んでいる状態。 |
| **unknown** | qpdf source / 既存 probe では責務境界を決められない。必要な追加 source 箇所か probe コマンドを書き、推測で分類しない。 |

`docs/qpdf-correspondence.md` の ✅ / 🔀 / ⚪ とは別の述語である: 対応表は「責務の対応と境界一致」、
本表は「その責務に至る **経路が 1 本か**」を問う。✅ の行でも consumer 側に bridge が残っていれば
本表では mixed / bridge になりうる。

## 4. 領域別 matrix

- [A. ObjectHandle / Resolver — object identity, lazy resolve, ownership, teardown](a-objecthandle-resolver.md)
- [B. parser / xref recovery / warning・error・diagnostics](b-parser-recovery-diagnostics.md)
- [C. stream data provider / decode / retry / filter / encryption / `/Length`](c-stream-pipeline-encryption.md)
- [D. writer — reachability, ObjStm planning / renumber / emission, xref / trailer, encryption, linearize](d-writer.md)
- [E. QPDFJob / CLI / C API 相当の consumer・adaptor](e-job-cli-capi.md)

## 5. 責任境界と不変条件

（Task 6 で記入）

## 6. 二重正本トラッカー

（Task 6 で記入 — mixed / bridge に分類された symbol の追跡コマンドと日付付き件数）

## 7. cutover 計画と最初の bounded cutover

（Task 7 で記入）

## 8. unknown と必要 probe 一覧

（Task 6 で集約）
