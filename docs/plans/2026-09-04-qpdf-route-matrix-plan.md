# qpdf Route Matrix（canonical / bridge / consumer 全経路棚卸し）Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** flpdf に残る「同じ qpdf 責務が複数経路に分散している状態」を、qpdf 11.9.0 の責務単位で棚卸しした route matrix（`docs/qpdf-route-matrix/`）として確定し、最初の bounded cutover と前提 issue・RED test・bridge caller ゼロ判定法を定義する（Beads `flpdf-3yn9.41`）。

**Architecture:** 成果物は production code を変えないドキュメント群と、その引用の実在を機械検証する小さなスクリプト。5 つの責務領域（A: ObjectHandle/Resolver、B: parser/xref recovery/diagnostics、C: stream data/decode/encryption、D: writer、E: Job/CLI/C API）を **1 領域 = 1 ファイル** で独立に棚卸しし（並列可）、その後 README で責任境界・二重正本トラッカー・cutover 計画を合成する。各行は qpdf source citation（`libqpdf/X.cc:NNN-MMM`）または実機 probe を必須とし、flpdf 側は `file.rs::symbol` と全 caller（production / test 別）を rg で列挙する。分類は canonical / bridge / mixed / unknown の 4 値のみ。

**Tech Stack:** pinned qpdf 11.9.0 source（`scripts/fetch-qpdf-source.sh --print-path` → `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`）、`/usr/bin/qpdf` 11.9.0（probe）、`rg`、Python 3（引用検証スクリプト、`scripts/tests/` の unittest 慣例に従う）、Beads (`bd`)。

---

## 前提と現状（着手時点の事実、2026-09-04、main `8fd1a2bf`）

- 旧 raw `Object` enum / `materialize` / `resolve_borrowed` / `set_object(Object)` は
  `flpdf-25kg.3.48.6`（closed, PR #1360）で削除済み。`crates/flpdf/src/object.rs` は存在しない。
  したがって issue 本文の「legacy Object/Dictionary 互換 cache」は **現時点では
  `cache.rs::ObjectCache`/`CacheEntry`（`reader.rs` 16 箇所, `engine.rs`, `resolver.rs`, `pdf.rs`,
  `linearization/plan.rs` が consumer）、`pdf.rs::legacy_resolution_state_synced`、
  `reader.rs::synchronize_cache_with_resolver_xref`、`object_handle.rs::legacy_dictionary_key`
  （`stream_filter.rs`/`parser.rs` が使用）、`resolver.rs::read_window`（`#[deprecated]`、
  qpdf 対応物なし）** 等の残存物を指す。棚卸しはこれを「推測」ではなく rg の出力で確定する。
- writer は `PdfWriter::write` → `writer.rs::emit_canonical_pdf` → `emit_canonical_pdf_inner`
  の中で「shared plain pipeline」と「legacy coordinator（QDF / encrypted / ObjStm 等の
  specialized modes、`writer.rs:3643-3893` 付近）」に分岐し、さらに `linearization/writer.rs::write_linearized`
  と `writer/pclm.rs` が別 route。renumber は `writer/rewrite_renumber.rs::CanonicalCatalogFirstRenumber`
  と `ObjectStreamRenumber` の 2 系統。
- **in-flight**: PR #1486（`feature/flpdf-hi08-encrypted-preserve-objstm`、Beads `flpdf-hi08` は closed）が
  `crates/flpdf/src/writer.rs` の encrypted Preserve route と `docs/qpdf-correspondence.md` を変更中。
  領域 D の行は **main + #1486** の状態を記述し、#1486 由来の差分を行内に「(#1486)」と明記する。
- stream data は `filters.rs::decode_stream_data*`/`encode_stream_data*`（whole-buffer, 一部 `pub`）、
  `ObjectHandle::pipe_stream_data`/`get_stream_data`/`get_raw_stream_data`（pipeline）、
  `stream_filter.rs` の whole-buffer adapter（doc が「legacy callers」と自己申告, `:856,:1801`）の 3 経路。
- 既存の機械検証: `scripts/qpdf-module-docs.py --check`（module doc の `//! Mirrors` / `//! qpdf correspondence:`）、
  `scripts/check-qpdf-deviation-markers.py --check`（`// qpdf-deviation` マーカー）。
  route matrix の引用検証は **これらとは別入力（`docs/qpdf-route-matrix/*.md`）** なので新スクリプトにする。
- 設計規則: `.claude/rules/qpdf-port-design-patterns.md`（特に 1「qpdf から出発」、3「前例は
  qpdf 対応を確認」、7「識別子は grep で実在検証」、8「pub 境界」）。**各 Task の subagent は
  作業前にこのファイルを Read すること。**

## 成果物の配置と形式（全 Task 共通）

```
docs/qpdf-route-matrix/
  README.md                            # Task 0 骨格 → Task 6/7 で合成（方法・分類定義・境界/不変条件・二重正本トラッカー・cutover 計画）
  a-objecthandle-resolver.md           # Task 1
  b-parser-recovery-diagnostics.md     # Task 2
  c-stream-pipeline-encryption.md      # Task 3
  d-writer.md                          # Task 4
  e-job-cli-capi.md                    # Task 5
scripts/check-qpdf-route-matrix.py     # Task 8（引用の実在検証）
scripts/tests/test_check_qpdf_route_matrix.py
```

### 行フォーマット（各領域ファイルの表は必ずこの 8 列）

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|

- **qpdf responsibility owner**: `QPDF::resolve` のような実在識別子。実在は `rg -n 'QPDF::resolve\b' $Q/libqpdf/QPDF.cc` で確認してから書く（規則 7）。
- **qpdf evidence**: `libqpdf/QPDF.cc:1700-1753` 形式（`include/qpdf/X.hh:NNN-MMM` も可）。source で決着しない挙動は
  `probe: <コマンド> → <観測結果>` を書く。**推測で埋めない**。
- **flpdf current entrypoint**: `crates/flpdf/src/reader.rs::Pdf::resolve`（visibility を `pub`/`pub(crate)`/private で併記）。
- **callers (prod / test)**: `rg -n '\bsymbol\(' crates --glob '!**/tests/**'` 等で数え、`prod: N (file1, file2) / test: M` の形。
  同一ファイル内 `#[cfg(test)]` は test に数える（`rg -n 'mod tests' file` で境界行を確認）。
- **classification**: 下記 4 値のみ。
- **canonical owner**: その責務の flpdf 側正本（1 つだけ）。無ければ `absent`。
- **remaining bridge callers / notes**: bridge/mixed の場合は残 caller を全列挙。unknown の場合は必要な probe を書く。

### 分類定義（README にも同文を置く）

- **canonical** — flpdf の当該 entrypoint がその qpdf 責務の唯一の正本で、アルゴリズム・呼び出し順序が
  cite した qpdf code と 1:1 に対応し、production caller が全てここを通る。
- **bridge** — 旧表現と canonical 表現を翻訳するためだけに存在し、それ自体に qpdf 対応物が無い経路。
  残 caller を全列挙する（ゼロなら削除候補）。
- **mixed** — 1 つの qpdf 責務が flpdf 側で 2 つ以上の経路に分かれ、順序・採番・診断のいずれかが
  経路間で異なりうる状態。または 1 つの flpdf 経路が 2 つ以上の qpdf 責務を畳んでいる状態。
- **unknown** — qpdf source / 既存 probe では責務境界を決められない。必要な追加 source 箇所か
  probe コマンドを書き、推測で分類しない。

### subagent 共通ルール（Task 1〜5 のプロンプトに必ず含める）

1. 作業前に `.claude/rules/qpdf-port-design-patterns.md` を Read する。
2. **qpdf 側を先に読む**（`$Q=/home/ubuntu/.cache/flpdf/qpdf-11.9.0`）。flpdf のコードを開くのは
   qpdf の責務・state・call order を書き出した後。
3. 識別子・行範囲は書く前に `rg -n` / `sed -n` で実在確認する。行範囲は `sed -n 'N,Mp'` で読んだ範囲だけを書く。
4. flpdf 側の caller は rg の出力を根拠にし、数字と file 名を行に書く。
5. 担当ファイル 1 つだけを書く。他ファイル・production code・Beads は触らない。**git commit しない**（親が行う）。
6. 決められない行は `unknown` にして必要 probe を書く。埋めるための推測禁止。
7. 文章は日本語、識別子・引用は原文のまま。

---

## Task 0: 骨格と方法論（README）

**Files:**
- Create: `docs/qpdf-route-matrix/README.md`

**Step 1: README を作成**

内容（見出しのみ固定、本文はこの plan の「成果物の配置と形式」「分類定義」を転記）:

```markdown
# flpdf ↔ qpdf route matrix（canonical / bridge / consumer 棚卸し）

**Oracle:** qpdf 11.9.0（`scripts/fetch-qpdf-source.sh --print-path`）。本表の引用は
`scripts/check-qpdf-route-matrix.py --check` で実在（ファイル・行範囲・識別子）を検証する。
**関連:** `docs/qpdf-correspondence.md`（責務対応表。本表はその上に「経路」軸を足したもので、
対応表の行を置き換えない）/ Beads `flpdf-3yn9.41`

## 1. 目的
## 2. 方法（qpdf から出発、rg による caller 列挙、probe の書式）
## 3. 分類定義（canonical / bridge / mixed / unknown）
## 4. 領域別 matrix（別ファイル）
- [A. ObjectHandle / Resolver](a-objecthandle-resolver.md)
- [B. parser / xref recovery / diagnostics](b-parser-recovery-diagnostics.md)
- [C. stream data / decode / encryption](c-stream-pipeline-encryption.md)
- [D. writer](d-writer.md)
- [E. QPDFJob / CLI / C API](e-job-cli-capi.md)
## 5. 責任境界と不変条件（Task 6 で記入）
## 6. 二重正本トラッカー（Task 6 で記入）
## 7. cutover 計画と最初の bounded cutover（Task 7 で記入）
## 8. unknown と必要 probe 一覧（Task 6 で集約）
```

**Step 2: 各領域ファイルの空テンプレートを作成**

5 ファイルそれぞれに、見出し `# <領域名>`、`## qpdf 責務モデル`（qpdf 側の state / call order / error boundary を先に書く欄）、
`## route matrix`（8 列表ヘッダのみ）、`## unknown / probe` の 3 節を置く。

**Step 3: commit**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-3yn9.41
git add docs/qpdf-route-matrix docs/plans/2026-09-04-qpdf-route-matrix-plan.md
git commit -m "docs: scaffold qpdf route matrix for flpdf-3yn9.41"
```

---

## Task 1〜5 は独立（並列 dispatch 可）。各 subagent は担当ファイル 1 つだけを書く。

## Task 1: 領域 A — ObjectHandle / Resolver（object identity, lazy resolve, ownership, teardown）

**Files:**
- Modify: `docs/qpdf-route-matrix/a-objecthandle-resolver.md`

**Step 1: qpdf 責務モデルを書く（flpdf を開く前）**

読む箇所（実在確認済み。行範囲は読んだ範囲に更新すること）:
- `include/qpdf/QPDF.hh:724-996`（nested `Writer`/`Resolver`/`StreamCopier`/`ParseGuard`/`Pipe`/`JobSetter`/`ObjCache`/`ResolveRecorder` — qpdf が **誰に** private 責務を開けているかの一覧）
- `libqpdf/QPDF.cc:1239-1294`（`getAllObjects`/`fixDanglingReferences`）、`:1700-1753`（`resolve`）、`:1843-1901`（`makeIndirectObject`/`newIndirectNull`）、`:1952-1993`（`getObject`/`replaceObject`）、`swapObjects`、`:55-106,198-213,271-281`（構築・`closeInputSource`・破棄）
- `libqpdf/QPDFObjectHandle.cc`（`dereference`/`isUnresolved` 周辺）、`libqpdf/qpdf/QPDFObject_private.hh:19-180`、`libqpdf/qpdf/QPDFValue.hh:18-152`

書く内容: state（`m->obj_cache`/`m->resolving`/`m->resolved_object_streams`/`m->file`）、call order（`getObject` は解決しない → `resolve` が xref 種別で dispatch → `updateCache`）、error/warning boundary（loop warning、damaged object → null、`std::logic_error` 相当）、teardown（`~QPDF` の disconnect）。

**Step 2: flpdf entrypoint と caller を列挙**

```bash
W=/home/ubuntu/flpdf/.worktrees/flpdf-3yn9.41
rg -n '^\s*pub(\(crate\))? fn ' $W/crates/flpdf/src/reader.rs $W/crates/flpdf/src/pdf.rs $W/crates/flpdf/src/engine.rs $W/crates/flpdf/src/cache.rs
rg -n 'pub(\(crate\))? fn |pub(\(crate\))? struct |pub(\(crate\))? trait ' $W/crates/flpdf/src/reader/resolver.rs | head -80
rg -n 'legacy_resolution_state_synced|synchronize_cache_with_resolver_xref|ObjectCache|CacheEntry|legacy_dictionary_key|read_window|resolve_to_terminal|ObjectValue::Reference' $W/crates --glob '*.rs'
```

必須行（最低限これらは行にする。追加は可）: `Pdf::get_object_handle`、`Pdf::resolve`、`Pdf::get_all_objects`、`Pdf::replace_object`、`Pdf::swap_objects`、`Pdf::make_indirect_object_handle`、`Pdf::delete_object`（qpdf 対応物の有無を確認）、`Pdf::close_input_source`/`Drop`、`cache.rs::ObjectCache`（qpdf `m->obj_cache` との対応か、二重正本か）、`legacy_resolution_state_synced`+`synchronize_cache_with_resolver_xref`、`object_handle.rs::legacy_dictionary_key`、`resolver.rs::read_window`（deprecated）、`resolve_to_terminal*`、`ObjectValue::Reference`（対応表 §1 が「qpdf value family に対応物なし、後続で消す」と明記）。

**Step 3: 分類と unknown を書く。担当ファイルのみ保存。commit しない。**

---

## Task 2: 領域 B — parser / xref recovery / warning・error・diagnostics

**Files:**
- Modify: `docs/qpdf-route-matrix/b-parser-recovery-diagnostics.md`

**Step 1: qpdf 責務モデル**

- `libqpdf/QPDFParser.cc:1-519`（`parse`/`parseRemainder`、warning の出し方、`QPDFExc` の構築）
- `libqpdf/QPDF.cc:400-1000`（`parse`/`read_xref`/`read_xrefTable`/`read_xrefStream`/`reconstruct_xref`/`readTrailer`）、`:1300-1400`（`readObjectAtOffset`/`readStream`）、`:487-494`（`warn`）
- `libqpdf/QPDFExc.cc:1-81`、`libqpdf/QPDFLogger.cc:1-255`、`libqpdf/QPDFTokenizer.cc`（読み取り境界だけ）

書く内容: warning は `m->warnings` 1 本（sink は 1 つ、順序が契約）、`attempt_recovery` の分岐、`reconstruct_xref` が ObjStm 中身を走査しないこと（`QPDF.cc:611-614`）、`QPDFExc::createWhat` の文言境界。

**Step 2: flpdf entrypoint と caller**

```bash
W=/home/ubuntu/flpdf/.worktrees/flpdf-3yn9.41
rg -n '^\s*pub(\(crate\))? fn ' $W/crates/flpdf/src/parser.rs $W/crates/flpdf/src/xref.rs $W/crates/flpdf/src/reader/file_object.rs $W/crates/flpdf/src/diagnostics.rs $W/crates/flpdf/src/logger.rs $W/crates/flpdf/src/error.rs $W/crates/flpdf/src/tokenizer.rs
rg -n 'fn (reconstruct|recover|repair)[a-z_]*' $W/crates/flpdf/src
rg -n 'repair_diagnostics\(|push_warning\(|push_object_warning\(|DocumentResolver::warn|warn_if_possible\(' $W/crates --glob '*.rs' | cut -d: -f1 | sort | uniq -c | sort -rn
rg -n 'recover_objstm_compressed_entries|resolution_fallbacks_remaining' $W/crates/flpdf/src
```

必須行: parse entry（handle parser vs `parser.rs` の別経路の有無）、xref load（classic/stream/hybrid）、`reconstruct_xref` 相当と `recover_objstm_compressed_entries`（qpdf 対応物なし — `bd` メモリ参照）、warning sink（`repair_diagnostics` / `QPDFLogger` / `DocumentResolver::warn` の 3 者が 1 sink か）、`Error::System`/`Internal`/`Parse` の qpdf 例外分類対応。

---

## Task 3: 領域 C — stream data provider / decode / retry / filter / encryption / `/Length`

**Files:**
- Modify: `docs/qpdf-route-matrix/c-stream-pipeline-encryption.md`

**Step 1: qpdf 責務モデル**

- `libqpdf/QPDF_Stream.cc:1-698` 全部（`stream_data`/`stream_provider`/original の 3 source、`pipeStreamData` の decode level 分岐と retry、`filterable`、`replaceStreamData`/`replaceFilterData`）
- `libqpdf/QPDF.cc:2381-2560`（`Pipe::pipeStreamData`、`pipeStreamData`、`decryptStream`、`copyStreamData`）
- `libqpdf/QPDFWriter.cc`（`willFilterStream`、stream の `unparseObject` 部分。行は `rg -n 'willFilterStream' $Q/libqpdf/QPDFWriter.cc` で確定）
- `libqpdf/QPDF_encryption.cc`（`decryptStream`/`decryptString`、`compute_data_key`）

書く内容: 復号は **pipe 時**（resolve 時ではない）、provider の normal/retry family、`/Length` の検証（`Pl_Count`）、decode level と filter 登録集合。

**Step 2: flpdf entrypoint と caller**

```bash
W=/home/ubuntu/flpdf/.worktrees/flpdf-3yn9.41
rg -n '^\s*pub(\(crate\))? fn [a-z_]*(stream|decode|encode|pipe|filter)[a-z_]*' $W/crates/flpdf/src/object_handle.rs $W/crates/flpdf/src/filters.rs $W/crates/flpdf/src/stream_filter.rs $W/crates/flpdf/src/pipeline.rs $W/crates/flpdf/src/writer.rs
for s in decode_stream_data decode_stream_data_recovering decode_stream_data_with_limits decode_stream_data_from_handle encode_stream_data encode_stream_data_from_handle pipe_stream_data get_stream_data get_raw_stream_data apply_stream_compress_policy write_stream_payload_with_pipeline; do echo "== $s"; rg -n "\b$s\(" $W/crates --glob '*.rs' | cut -d: -f1 | sort | uniq -c | sort -rn; done
rg -n 'decrypt' $W/crates/flpdf/src/reader/resolver.rs | head -30
```

必須行: `QPDF_Stream::pipeStreamData` ↔ `ObjectHandle::pipe_stream_data`/`get_stream_data` と `filters.rs::decode_stream_data*`（whole-buffer, `pub`）の関係（mixed か bridge か）、`stream_filter.rs` whole-buffer adapter（doc が legacy と自己申告）、`QPDF::decryptStream` ↔ resolver 内の decrypt 位置、`QPDFWriter::willFilterStream` ↔ `apply_stream_compress_policy`、`/Length` 検証、`copyStreamData` ↔ `copy_stream`/`copied_stream_data_provider`。

---

## Task 4: 領域 D — writer（reachability, ObjStm planning/renumber/emission, xref/trailer, encryption, linearize）

**Files:**
- Modify: `docs/qpdf-route-matrix/d-writer.md`

**Step 1: qpdf 責務モデル**

- `libqpdf/QPDFWriter.cc:1057-1157`（`enqueueObject`/`enqueueObjectsStandard`）、`:1621-1740`（`writeObjectStream`/`writeObject`）、`:1939-2010`（`preserveObjectStreams`/`generateObjectStreams`）、`write()`/`writeStandard`/`writeLinearized`/`writeXRefTable`/`writeXRefStream`/`writeTrailer`/`writeEncryptionDictionary`/`setEncryptionParameters*`（行は各々 `rg -n 'QPDFWriter::<name>' $Q/libqpdf/QPDFWriter.cc` で確定）
- `libqpdf/QPDF_linearization.cc`、`libqpdf/QPDF_optimization.cc`（object universe と renumber 順）
- `include/qpdf/QPDFWriter.hh:55-99,440-617`（public / private 境界）

書く内容: 単一の `enqueueObject` が採番の正本（container-first）、Preserve/Generate/Disable の 3 モードの分岐位置、encryption dictionary は body の後、linearize は pass1→hint→pass2（反復なし）、xref table/stream の選択条件。

**Step 2: flpdf entrypoint と caller（**PR #1486 の差分を含める**）**

```bash
W=/home/ubuntu/flpdf/.worktrees/flpdf-3yn9.41
cd /home/ubuntu/flpdf && gh pr diff 1486 -- crates/flpdf/src/writer.rs > /tmp/claude-1000/-home-ubuntu-flpdf/3082153e-8c00-4bc9-b81e-2132c813727e/scratchpad/pr1486-writer.diff; cd $W
rg -n '^\s*pub(\(crate\))? fn |^pub(\(crate\))? (struct|enum) ' $W/crates/flpdf/src/writer.rs $W/crates/flpdf/src/writer/*.rs $W/crates/flpdf/src/writer/*/*.rs $W/crates/flpdf/src/linearization/writer.rs $W/crates/flpdf/src/linearization/renumber.rs $W/crates/flpdf/src/optimization.rs
sed -n '3576,3900p' $W/crates/flpdf/src/writer.rs   # plain pipeline vs legacy coordinator の分岐
for s in CanonicalCatalogFirstRenumber ObjectStreamRenumber ObjectStreamGroup emit_bodies emit_objstm_body_from_handles_with_writer write_linearized filter_objstm_batches_for_output reachable_object_set; do echo "== $s"; rg -n "\b$s\b" $W/crates --glob '*.rs' | cut -d: -f1 | sort | uniq -c | sort -rn; done
```

必須行: `enqueueObject` ↔ `CanonicalCatalogFirstRenumber` / `ObjectStreamRenumber` / `linearization/renumber.rs`（3 系統 = mixed の疑い）、`preserveObjectStreams` ↔ Preserve batch 導出（plain / encrypted (#1486) / linearized の各 route）、`generateObjectStreams` ↔ `object_streams/{eligibility,planning}`、`writeObject`/`writeObjectStream` ↔ `plain/body.rs`/`object_streams/emission.rs`、xref/trailer、encryption dictionary の位置、`write_linearized`、`pclm.rs`、「shared plain pipeline」と「legacy coordinator」の分岐条件（`writer.rs:3643-3893`）を **どの WriterOptions の組で辿るか** の表。

---

## Task 5: 領域 E — QPDFJob / CLI / C API 相当の consumer・adaptor

**Files:**
- Modify: `docs/qpdf-route-matrix/e-job-cli-capi.md`

**Step 1: qpdf 責務モデル**

- `include/qpdf/QPDFJob.hh`（public surface。**ネストクラスの `private:` に釣られない**—規則 8 の罠）、`libqpdf/QPDFJob.cc`（`run`/`createQPDF`/`writeQPDF`/`doInspection`/`handlePageSpecs`/`handleUnderOverlay` の呼び出し順。行は rg で確定）
- `qpdf/qpdf.cc:1-62`（CLI は `QPDFJob` の public しか触らない）
- `libqpdf/qpdf-c.cc:1-1963`（C API が `QPDF`/`QPDFWriter`/`QPDFJob` のどの public を叩くか）

**Step 2: flpdf entrypoint と caller**

```bash
W=/home/ubuntu/flpdf/.worktrees/flpdf-3yn9.41
rg -n '^\s*pub(\(crate\))? fn ' $W/crates/flpdf/src/job/lifecycle.rs $W/crates/flpdf/src/job/mod.rs
rg -n '^pub use ' $W/crates/flpdf/src/job/mod.rs $W/crates/flpdf/src/json_inspect.rs $W/crates/flpdf/src/lib.rs
rg -no 'flpdf::[A-Za-z_:]+' $W/crates/flpdf-cli/src/main.rs | sort | uniq -c | sort -rn      # CLI が直接触る crate 項目
rg -no 'flpdf::[A-Za-z_:]+' $W/crates/flpdf-qtest-tools/src --glob '*.rs' | sort | uniq -c | sort -rn | head -60
rg -n 'QPDFJob::|job\.[a-z_]+\(' $W/crates/flpdf-cli/src/main.rs | cut -d: -f1 | sort | uniq -c
```

必須行: `QPDFJob::run`/`createQPDF`/`writeQPDF` ↔ `job/lifecycle.rs`、CLI が `QPDFJob` public を経由せず直接呼ぶ項目（規則 8 (A)〜(E) の既知 debt `flpdf-xsq1`/`flpdf-7bkv`/`flpdf-ei0h`/`flpdf-hxmj` を行に引用し重複 issue を作らない）、`json_inspect.rs` の compatibility re-export（staged migration）、`flpdf-qtest-tools`（`qpdf-c`/`test_driver` の consumer 相当）が `Pdf`/`PdfWriter` のどの public を使うか。

---

## Task 6: 合成 — 責任境界・不変条件、二重正本トラッカー、unknown 集約（README §5/§6/§8）

**Files:**
- Modify: `docs/qpdf-route-matrix/README.md`

**Step 1: 5 ファイルを読み、領域ごとに「state / call order / error・warning boundary / 不変条件」を README §5 に 1 表ずつ書く**

各領域 4〜8 行。不変条件は「壊すと byte が変わる」ものを優先（例: 採番は enqueue 順、warning sink は 1 本で順序保持、復号は pipe 時、`/Length` は body の後）。

**Step 2: §6 二重正本トラッカー**

`mixed`/`bridge` に分類された flpdf symbol を全部並べ、追跡コマンドを **実行してその出力（件数と file）を日付付きで貼る**:

```bash
W=/home/ubuntu/flpdf/.worktrees/flpdf-3yn9.41
for s in ObjectCache CacheEntry legacy_resolution_state_synced synchronize_cache_with_resolver_xref legacy_dictionary_key read_window resolve_to_terminal decode_stream_data encode_stream_data CanonicalCatalogFirstRenumber ObjectStreamRenumber; do printf '%-40s prod=%s test=%s\n' "$s" "$(rg -n "\b$s\b" $W/crates --glob '*.rs' --glob '!**/tests/**' | rg -v 'mod tests|#\[cfg\(test\)\]' | wc -l)" "$(rg -n "\b$s\b" $W/crates --glob '**/tests/**' | wc -l)"; done
```

（symbol リストは Task 1〜5 の結果で置き換える。prod/test の分離は `mod tests` 以降を除外する精密版を Task 8 のスクリプトで提供してもよいが、まず rg の粗い件数で可）

**Step 3: §8 unknown 集約** — 各ファイルの unknown 行を 1 表に集め、必要 probe を列挙。

**Step 4: commit**

```bash
git add docs/qpdf-route-matrix && git commit -m "docs: synthesize route boundaries and dual-truth tracker"
```

---

## Task 7: cutover 計画と最初の bounded cutover（README §7）＋ 子 issue 登録

**Files:**
- Modify: `docs/qpdf-route-matrix/README.md`
- Beads: 子 issue（`--parent flpdf-3yn9.41` ではなく **epic `flpdf-3yn9` の子** として作り、`flpdf-3yn9.41` に `related` を付ける。実装 issue の親は epic）

**Step 1: 各 route family（resolver / parser・diagnostics / stream / writer / encryption / job）ごとに cutover 順を書く**

順序の基準は「依存の少なさ × 完成可能性」（qtest pass 数で決めない）。各 family に prerequisite（別 issue）と「この family を切り替えても qpdf call order を壊さない理由」を書く。

**Step 2: 最初の bounded cutover を 1 つ選び、次を定義**

- 対象 consumer（file::symbol の列挙）と **非対象** consumer（触らないと明記）
- qpdf differential RED test の定義: fixture、コマンド（`qpdf` 11.9.0 側と flpdf 側）、比較方法（`qpdf-zlib-compat` で byte 比較 / `qpdf-test-compare` / `--show-xref`）、現状 **FAIL する** ことの根拠
- 完了時の bridge caller ゼロ判定: §6 のコマンドで対象 symbol が `prod=0` になること
- 次の cutover に進む条件

候補は matrix の結果で決めるが、着手時点で見えている候補は (a) `writer.rs::emit_canonical_pdf_inner` の legacy coordinator（hi08 の発生源、#1486 で encrypted Preserve のみ修正）、(b) `cache.rs::ObjectCache` + `legacy_resolution_state_synced` の二重 state、(c) `filters.rs` whole-buffer decode。**選定理由を README に書く**。

**Step 3: 子 issue を作る（design/acceptance/non-goals/qpdf citation を含める）**

```bash
cd /home/ubuntu/flpdf
bd create --type task --priority 1 --parent flpdf-3yn9 \
  --title "<family>: <bounded cutover title>" \
  --labels pre-v1,qpdf-parity \
  --description "$(cat <<'EOF'
## Why
<matrix の該当行と RED test 根拠>
## Scope
<対象 consumer 列挙>
## Non-goals
<非対象 consumer 列挙、bridge 削除は別 slice>
## Acceptance Criteria
- <RED test> が GREEN
- scripts で <symbol> の prod caller = 0（bridge 削除 slice の場合）
- 既存 qpdf-zlib-compat byte gates 緑
EOF
)"
# bd create のエラー後は必ず bd search してから再実行（重複作成防止）
bd dep add <new-id> <prerequisite-id>   # 依存があれば
bd dep cycles
```

**Step 4: README §7 に issue ID を書き戻して commit**

```bash
git add docs/qpdf-route-matrix && git commit -m "docs: define first bounded cutover and prerequisites"
```

---

## Task 8: 引用検証スクリプト `scripts/check-qpdf-route-matrix.py`

**Files:**
- Create: `scripts/check-qpdf-route-matrix.py`
- Create: `scripts/tests/test_check_qpdf_route_matrix.py`
- Modify: `.github/workflows/ci.yml`（Quality job の `qpdf-module-docs.py --check` の直後に 1 step 追加）

検証内容:
1. `docs/qpdf-route-matrix/*.md` 中の `` `libqpdf/X.cc:N` `` / `` `libqpdf/X.cc:N-M` `` / `` `include/qpdf/X.hh:N[-M]` `` / `` `qpdf/X.cc:N[-M]` `` について、`scripts/fetch-qpdf-source.sh --print-path` 配下にファイルが存在し、`N<=M<=行数` であること。
2. `` `crates/<crate>/src/<path>.rs::<Symbol>` `` について、ファイルが存在し `rg`-相当の正規表現 `\b(fn|struct|enum|trait|type|const|static)\s+<last-segment>\b` がその file にマッチすること（`Pdf::resolve` のような `Type::method` は最後の segment `resolve` を `fn resolve\b` で探す）。
3. 表の classification 列が 4 値のいずれかであること。

**Step 1: 失敗するテストを書く**（`scripts/tests/` の既存 unittest の書式に合わせる。`ls scripts/tests` で確認）

```python
import subprocess, sys, tempfile, pathlib, unittest
SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "check-qpdf-route-matrix.py"

class CheckRouteMatrix(unittest.TestCase):
    def run_check(self, md: str, qpdf_root: pathlib.Path, repo_root: pathlib.Path):
        d = repo_root / "docs" / "qpdf-route-matrix"; d.mkdir(parents=True, exist_ok=True)
        (d / "x.md").write_text(md, encoding="utf-8")
        return subprocess.run([sys.executable, str(SCRIPT), "--check", "--root", str(repo_root), "--qpdf-root", str(qpdf_root)], capture_output=True, text=True)

    def test_out_of_range_line_is_error(self):
        with tempfile.TemporaryDirectory() as t:
            root = pathlib.Path(t); q = root / "q"; (q / "libqpdf").mkdir(parents=True)
            (q / "libqpdf" / "QPDF.cc").write_text("a\nb\nc\n")
            r = self.run_check("| x | `libqpdf/QPDF.cc:2-9` | canonical |\n", q, root)
            self.assertNotEqual(r.returncode, 0); self.assertIn("QPDF.cc:2-9", r.stdout + r.stderr)

    def test_missing_symbol_is_error(self):
        with tempfile.TemporaryDirectory() as t:
            root = pathlib.Path(t); q = root / "q"; (q / "libqpdf").mkdir(parents=True)
            src = root / "crates/flpdf/src"; src.mkdir(parents=True); (src / "reader.rs").write_text("pub fn resolve() {}\n")
            r = self.run_check("| `crates/flpdf/src/reader.rs::Pdf::nope` | canonical |\n", q, root)
            self.assertNotEqual(r.returncode, 0); self.assertIn("nope", r.stdout + r.stderr)

    def test_bad_classification_is_error(self):
        with tempfile.TemporaryDirectory() as t:
            root = pathlib.Path(t); q = root / "q"; (q / "libqpdf").mkdir(parents=True)
            r = self.run_check("| # | a | b | c | d | classification |\n|---|---|---|---|---|---|\n| 1 | a | b | c | d | legacy |\n", q, root)
            self.assertNotEqual(r.returncode, 0)

    def test_valid_document_passes(self):
        with tempfile.TemporaryDirectory() as t:
            root = pathlib.Path(t); q = root / "q"; (q / "libqpdf").mkdir(parents=True)
            (q / "libqpdf" / "QPDF.cc").write_text("a\nb\nc\n")
            src = root / "crates/flpdf/src"; src.mkdir(parents=True); (src / "reader.rs").write_text("impl Pdf { pub fn resolve() {} }\n")
            r = self.run_check("| 1 | `QPDF::resolve` | `libqpdf/QPDF.cc:1-3` | `crates/flpdf/src/reader.rs::Pdf::resolve` | prod: 1 | canonical | x | - |\n", q, root)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

if __name__ == "__main__":
    unittest.main()
```

**Step 2: 実行して失敗を確認**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-3yn9.41 && python3 -m unittest scripts/tests/test_check_qpdf_route_matrix.py
```
Expected: FAIL（script が無い）

**Step 3: スクリプトを書く**（`argparse`、`--check`、`--root`（既定: リポジトリルート）、`--qpdf-root`（既定: `fetch-qpdf-source.sh --print-path`）。エラーは `file:line: message` で stdout、exit 1。`__pycache__` を残さない: `PYTHONDONTWRITEBYTECODE=1` で実行し、生成されたら `find scripts -name __pycache__ -exec rm -rf {} +`。）

**Step 4: テスト緑・実 doc で `--check` 緑を確認**

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/tests/test_check_qpdf_route_matrix.py
python3 scripts/check-qpdf-route-matrix.py --check
```
Expected: `OK` / exit 0（実 doc の引用に誤りがあればここで発見し、doc を直す）

**Step 5: CI に追加して commit**

```yaml
      - name: Check qpdf route matrix citations
        run: python3 scripts/check-qpdf-route-matrix.py --check
```
（Quality job は qpdf source を fetch しているか確認: `qpdf-module-docs.py --check` が source を必要としないなら、この step の前に `scripts/fetch-qpdf-source.sh` を実行する step を足す）

```bash
git add scripts/check-qpdf-route-matrix.py scripts/tests/test_check_qpdf_route_matrix.py .github/workflows/ci.yml docs/qpdf-route-matrix
git commit -m "ci: verify qpdf route matrix citations"
```

---

## Task 9: 対応表へのポインタ、最終レビュー、PR

**Files:**
- Modify: `docs/qpdf-correspondence.md`（冒頭の「関連:」行の直後に 1 段落: route matrix の場所と「本表の行を置き換えない」旨）

**Step 1: 最終レビュー（規則 7）** — 全 `.md` の qpdf 識別子を機械抽出して実在確認:

```bash
Q=/home/ubuntu/.cache/flpdf/qpdf-11.9.0
rg -o 'QPDF[A-Za-z_]*::[A-Za-z_]+' docs/qpdf-route-matrix | cut -d: -f2- | sort -u | while read s; do rg -q "$s" $Q/libqpdf $Q/include $Q/qpdf || echo "NOT FOUND: $s"; done
```
Expected: 出力なし。

**Step 2: 品質ゲート**

```bash
cargo fmt --all -- --check
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
python3 scripts/check-qpdf-route-matrix.py --check
git status --short   # __pycache__ や a.pdf 等の混入がないこと
scripts/patch-coverage.sh   # Rust 変更なし → changed 0 を確認（空 diff の疑いは bd 操作でブランチが main に戻っていないか `git branch --show-current` で確認）
```

**Step 3: commit / push / PR**

```bash
git add docs/qpdf-correspondence.md && git commit -m "docs: link route matrix from qpdf correspondence"
git push -u origin feature/flpdf-3yn9-41
gh pr create --title "docs: qpdf route matrix and first bounded cutover plan (flpdf-3yn9.41)" --body "<Summary / 成果物 / 子 issue 一覧 / Test plan（上記ゲート）/ Compat matrix: production 変更なし>"
bd dolt push
```

**Step 4: Beads** — `flpdf-3yn9.41` の notes に matrix の場所・第 1 cutover issue ID・unknown 件数を追記。close は PR merge 後（ユーザー確認）。
