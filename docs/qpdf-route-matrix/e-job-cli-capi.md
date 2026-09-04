# E. QPDFJob / CLI / C API 相当の consumer・adaptor

対象: `QPDFJob` の public surface（`initializeFromArgv` / `run` / `createQPDF` / `writeQPDF` /
`hasWarnings` / `getExitCode` / `getLogger` / `setMessagePrefix` 等）と private orchestration
（`doInspection` / `doCheck` / `handlePageSpecs` / `handleUnderOverlay` / `doSplitPages` /
`doJSON*` 等）の境界、`qpdf/qpdf.cc`（CLI 実行ファイル）、`libqpdf/qpdfjob-c.cc`（`QPDFJob` の
C API）、`libqpdf/qpdf-c.cc`（`QPDF` / `QPDFWriter` の C API）。flpdf 側は
`job/lifecycle.rs`（`QPDFJob`）、`job/mod.rs` / `json_inspect.rs` / `lib.rs` の re-export、
`crates/flpdf-cli/src/main.rs` が `QPDFJob` public を経由せず直接触る crate 項目、
`crates/flpdf-qtest-tools`（qtest 用 consumer）。

**前提訂正（本表作成時に判明）:** 本ファイルの初版スケルトンは「`libqpdf/qpdf-c.cc` が `QPDF` /
`QPDFWriter` / `QPDFJob` のどの public を叩くか」と書いていたが、これは誤り。
`rg -n 'QPDFJob' $Q/libqpdf/qpdf-c.cc` は 0 件で、`qpdf-c.cc` は `QPDF` と `QPDFWriter` しか
触らない。`QPDFJob` の C API は別ファイル `libqpdf/qpdfjob-c.cc`（161 行）にあり、両者は
別々の consumer として扱う（route matrix の E-22 / E-23）。

既知の debt（重複 issue を作らず本表から引用する）: `flpdf-xsq1`（pub 可視性）、`flpdf-7bkv`
（`json_inspect.rs` compatibility re-export の撤去）、`flpdf-ei0h`（命名）、`flpdf-hxmj`
（single-/multi-source `--pages` パイプライン統合）。

## qpdf 責務モデル

pinned qpdf 11.9.0 の `include/qpdf/QPDFJob.hh`（720 行）/ `libqpdf/QPDFJob.cc`（3116 行）/
`qpdf/qpdf.cc`（62 行）/ `libqpdf/qpdfjob-c.cc`（161 行）/ `libqpdf/qpdf-c.cc`（1963 行）を
`rg -n` と `sed -n` で読んだ範囲のみを引用する。flpdf のファイルは本節を書き終えるまで開いていない。

### M-0. `QPDFJob.hh` の public / private 境界（ネストクラスを漏らさずに数える）

`class QPDFJob`（`include/qpdf/QPDFJob.hh:43-718`）のアクセス指定子は、**ネストクラスの
`private:` を外側に漏らさずに** 数えると次の 5 区画になる（`.claude/rules/qpdf-port-design-patterns.md`
8 の「`QPDFJob.hh` を読むときの罠」）。

| 区画 | 行範囲 | 種別 | 中身 |
|---|---|---|---|
| 1 | `include/qpdf/QPDFJob.hh:45-136` | **public** | `LATEST_JOB_JSON` / `EXIT_*` 定数、`QPDFJob()`、`initializeFromArgv`、`initializeFromJson`、`setMessagePrefix` / `getMessagePrefix`、`getLogger` / `setLogger`、`setOutputStreams`（deprecated）、`registerProgressReporter`、`checkConfiguration`、`createsOutput` |
| 2 | `include/qpdf/QPDFJob.hh:137-166` | private | `CopyAttachmentFrom` / `AddAttachment` / `PageSpec` — 「public な Config クラスより前に定義が要る」ためここに置かれた private struct |
| 3 | `include/qpdf/QPDFJob.hh:167-424` | **public** | Config 系ネストクラス（`AttConfig` / `CopyAttConfig` / `PagesConfig` / `UOConfig` / `EncConfig` / `PageLabelsConfig` / `Config`）と、その後の `config()` / `run()` / `createQPDF()` / `writeQPDF()` / `hasWarnings()` / `getExitCode()` / `getEncryptionStatus()` / `doIfVerbose()` / `json_out_schema()` / `job_json_schema()` |
| 4 | `include/qpdf/QPDFJob.hh:425-568` | private | `RotationSpec` / `password_mode_e` / `UnderOverlay` / `PageLabelSpec`、および後述の private メソッド群 |
| 5 | `include/qpdf/QPDFJob.hh:569-716` | private | ネストクラス `Members`（全設定 state。`friend class QPDFJob`） |

区画 3 のネストクラスはそれぞれ自分の `private:` を持つ（例: `PagesConfig` は
`include/qpdf/QPDFJob.hh:254`、`UOConfig` は `include/qpdf/QPDFJob.hh:272`、`Config` は
`include/qpdf/QPDFJob.hh:353`）。`Config` の `};`（`include/qpdf/QPDFJob.hh:361`）の直後は
外側 `QPDFJob` の区画 3（public）に戻るので、`run()` / `createQPDF()` / `writeQPDF()` /
`hasWarnings()` / `getExitCode()` はいずれも **public**。

区画 4 の private メソッドは責務ごとに 5 群に分かれている:

- helper: `usage` / `json_schema` / `parse_object_id` / `parseRotationParameter` /
  `parseNumrange`（`include/qpdf/QPDFJob.hh:478-483`）
- 入力処理: `processFile` / `processInputSource` / `doProcess` / `doProcessOnce`
  （`include/qpdf/QPDFJob.hh:485-510`）
- 変換: `setQPDFOptions` / `handlePageSpecs` / `shouldRemoveUnreferencedResources` /
  `handleRotations` / `getUOPagenos` / `handleUnderOverlay` / `doUnderOverlayForPage` /
  `validateUnderOverlay` / `handleTransformations` / `addAttachments` / `copyAttachments`
  （`include/qpdf/QPDFJob.hh:512-532`）
- 検査: `doInspection` / `doCheck` / `showEncryption` / `doShowObj` / `doShowPages` /
  `doListAttachments` / `doShowAttachment`（`include/qpdf/QPDFJob.hh:534-541`）
- 出力生成 + JSON: `doSplitPages` / `setWriterOptions` / `setEncryptionOptions` /
  `maybeFixWritePassword` / `writeOutfile` / `writeJSON`（`include/qpdf/QPDFJob.hh:543-549`）、
  `doJSON` / `getWantedJSONObjects` / `doJSONObjects` / `doJSONObjectinfo` / `doJSONPages` /
  `doJSONPageLabels` / `doJSONOutlines` / `doJSONAcroform` / `doJSONEncrypt` /
  `doJSONAttachments` / `addOutlinesToJson`（`include/qpdf/QPDFJob.hh:551-565`）

### M-1. `run()` の呼び出し順序は 2 段（`createQPDF` → `writeQPDF`）

`QPDFJob::run()`（`libqpdf/QPDFJob.cc:513-519`）は 7 行しかない:

```
auto pdf = createQPDF();
if (pdf) { writeQPDF(*pdf); }
```

`createQPDF()` が `nullptr` を返す場合（`--is-encrypted` / `--requires-password` / 誤 password で
`--show-encryption`）は `writeQPDF` を呼ばない。この 2 段構成自体が public API として意図的に
公開されている（`include/qpdf/QPDFJob.hh:375-385`: 「QPDF オブジェクトを作ってから書き出す前に
改変できるようにするため」）。

### M-2. `createQPDF()` の固定順序（`libqpdf/QPDFJob.cc:428-481`）

1. `checkConfiguration()`（public、`libqpdf/QPDFJob.cc:566-642`）。
2. `processFile(pdf_sp, infilename, password, true, true)`（private、`libqpdf/QPDFJob.cc:1793-1804`）。
   `QPDFExc` で `qpdf_e_password` の場合のみ、`check_is_encrypted` / `check_requires_password` なら
   `encryption_status` を立てて `nullptr`、`show_encryption` なら `showEncryption` を呼んで `nullptr`。
   それ以外は再 throw。
3. `pdf.isEncrypted()` なら `encryption_status = qpdf_es_encrypted`。
4. `check_is_encrypted || check_requires_password` なら **ここで `nullptr`**（出力しない）。
5. `update_from_json` が非空なら `pdf.updateFromJSON(...)`（「他の変換より先」と明記）。
6. `page_specs` が非空なら `handlePageSpecs(pdf, page_heap)`（`libqpdf/QPDFJob.cc:2359-2633`）。
7. `rotations` が非空なら `handleRotations(pdf)`（`libqpdf/QPDFJob.cc:2635-2652`）。
8. `handleUnderOverlay(pdf)`（`libqpdf/QPDFJob.cc:1936-2043`、条件分岐なしで常に呼ぶ）。
9. `handleTransformations(pdf)`（`libqpdf/QPDFJob.cc:2137-2248`）。
10. `page_heap` の各 foreign QPDF に warning があれば `m->warnings = true`。

入力側の共通足場は `doProcessOnce`（`libqpdf/QPDFJob.cc:1695-1716`）で、**`QPDF` を作った直後に
`setQPDFOptions(*pdf)`**（`libqpdf/QPDFJob.cc:650-666`）を呼び、その後で `emptyPDF()` /
`createFromJSON()` / `processFile()` のいずれかを選ぶ。password recovery のリトライは
`doProcess`（`libqpdf/QPDFJob.cc:1718-1791`）が担当し、`QUtil::possible_repaired_encodings` の
各候補で `doProcessOnce` を呼び直す。

### M-3. `writeQPDF()` の 3 分岐（本領域の canonical/mixed 判別軸）

`QPDFJob::writeQPDF(QPDF& pdf)`（`libqpdf/QPDFJob.cc:483-511`）の冒頭が **本領域で最も重要な
分岐**:

```
if (!createsOutput())      doInspection(pdf);
else if (m->split_pages)   doSplitPages(pdf);
else                       writeOutfile(pdf);
```

`createsOutput()`（public、`libqpdf/QPDFJob.cc:528-532`）は `outfilename != nullptr || replace_input`。
その後、`pdf.getWarnings()` が非空なら `m->warnings = true`、`warnings && !suppress_warnings` なら
出力の有無で文言を変えて warning 行を出し、`report_mem_usage` なら
`QUtil::get_max_memory_usage()` を報告する。**「検査するか / 分割するか / 書くか」の判断は
呼び出し側ではなく `writeQPDF` の内側にある。**

`writeOutfile`（`libqpdf/QPDFJob.cc:3029-3091`）はさらに内側で分岐する: `replace_input` なら
`<infile>.~qpdf-temp#` を outfilename にし、`outfilename == "-"` なら `nullptr` にする。
`m->json_version` があれば `writeJSON(pdf)`（`libqpdf/QPDFJob.cc:3093-3116`）、無ければ
`QPDFWriter w(pdf)` をブロックスコープで作り `setWriterOptions(w)`（`libqpdf/QPDFJob.cc:2846-2937`）
してから `w.write()`。`replace_input` の場合の rename / backup / 削除もここ。

`doInspection`（`libqpdf/QPDFJob.cc:1645-1693`）は `check` / `show_npages` / `show_encryption` /
`check_linearization` / `show_linearization` / `show_xref` / `show_obj|show_trailer` / `show_pages` /
`list_attachments` / `attachment_to_show` を **この順で** 逐次判定し、最後に
`pdf.getWarnings()` が非空なら `m->warnings = true`。

### M-4. CLI 実行ファイルは public surface しか触らない

`qpdf/qpdf.cc`（全 62 行）の `realmain`（`qpdf/qpdf.cc:26-44`）は
`QPDFJob j;` → `j.initializeFromArgv(argv)` → `j.run()` → `return j.getExitCode()` の 4 手だけ。
`QPDFUsage` は usage メッセージ + `EXIT_ERROR`、その他 `std::exception` は
`whoami: what()` を stderr に出して `EXIT_ERROR`。**`handlePageSpecs` / `doCheck` /
`doListAttachments` / `writeOutfile` のような private orchestration に CLI からは一切触らない。**

### M-5. `QPDFJob` の C API も pure pass-through

`libqpdf/qpdfjob-c.cc:19-161` は `_qpdfjob_handle`（`QPDFJob j;` を持つだけ、
`libqpdf/qpdfjob-c.cc:11-17`）越しに `setLogger` / `getLogger` / `initializeFromArgv` /
`initializeFromJson` / `run` / `getExitCode` / `createQPDF` / `writeQPDF` /
`registerProgressReporter` を呼ぶだけで、**M-0 の区画 1・3（public）以外に手を伸ばす箇所が無い**。
`qpdfjob_run`（`libqpdf/qpdfjob-c.cc:88-96`）は `j.run(); return j.getExitCode();`、
`qpdfjob_create_qpdf`（`libqpdf/qpdfjob-c.cc:98-109`）と `qpdfjob_write_qpdf`
（`libqpdf/qpdfjob-c.cc:111-119`）は M-1 の 2 段構成をそのまま C に露出したもの。
例外は `wrap_qpdfjob`（`libqpdf/qpdfjob-c.cc:32-41`）が `getLogger()->getError()` へ
`getMessagePrefix() + ": " + what()` を書いて `EXIT_ERROR` を返す形で一本化されている。
CLI（M-4）と C API（M-5）が**独立に**同じ public surface しか使っていないことが、
public/private 境界の 2 つ目の witness になる。

### M-6. `QPDF` / `QPDFWriter` の C API（`QPDFJob` を経由しない別経路）

`libqpdf/qpdf-c.cc` は `QPDFJob` を 1 度も参照しない（`rg -n 'QPDFJob' $Q/libqpdf/qpdf-c.cc` → 0 件）。
`_qpdf_data` は `QPDF` と `QPDFWriter` を直接保持し、族ごとに次の public を叩く
（`libqpdf/qpdf-c.cc:24-64` の 5 つの static helper が代表）:

- 読み込み: `QPDF::processFile` / `QPDF::processMemoryFile` / `QPDF::emptyPDF` /
  `QPDF::createFromJSON` / `QPDF::updateFromJSON`（`libqpdf/qpdf-c.cc:24-35,266-310,1885-1923`）
- 書き出し: `QPDFWriter` のコンストラクタ 2 種 / `setOutputMemory` / `write`
  （`libqpdf/qpdf-c.cc:37-55,459-521`）と、`qpdf_set_*` 族が写す `QPDFWriter` の setter 群
  （`libqpdf/qpdf-c.cc:523-777`）
- 検査: `qpdf_check_pdf`（`libqpdf/qpdf-c.cc:224-231`）は **`QPDFJob::doCheck` を呼ばず**、
  `call_check`（`libqpdf/qpdf-c.cc:57-64`）で `QPDFWriter` に `Pl_Discard` +
  `setDecodeLevel(qpdf_dl_all)` を設定して `write()` するだけ
- object handle: `qpdf_oh_*` 族（`libqpdf/qpdf-c.cc:841-1793`）が `QPDFObjectHandle` の public を写す
- ページ: `qpdf_get_num_pages` 他（`libqpdf/qpdf-c.cc:1795-1883`）が `QPDFPageDocumentHelper` /
  `QPDF` の page API を写す
- JSON: `qpdf_write_json`（`libqpdf/qpdf-c.cc:1924-1952`）は `QPDF::writeJSON` を直接呼ぶ
  （`QPDFJob::writeJSON` ではない — 名前が似ているが別責務）

エラーは `trap_errors`（`libqpdf/qpdf-c.cc:66-86`）が `QPDFExc` / `std::runtime_error` /
`std::exception` を `qpdf_e_system` / `qpdf_e_internal` に写して `QPDF_ERRORS` ビットを立てる。

### M-7. 本領域の判別軸（まとめ）

1. **`run` の 2 段** — flpdf 側に `createQPDF` 相当と `writeQPDF` 相当があり、consumer が
   その 2 つだけを呼ぶか。
2. **`writeQPDF` の 3 分岐（E-3）** — 「検査 / 分割 / 書き出し」の選択が flpdf 側でも
   `write_qpdf` 相当の**内側**にあるか、consumer 側に漏れているか。漏れていれば `mixed`。
3. **private orchestration への直接到達** — consumer が `handlePageSpecs` / `handleUnderOverlay` /
   `doJSON*` 相当に `QPDFJob` public を経由せず届いているか。届いていれば
   `.claude/rules/qpdf-port-design-patterns.md` 8 の debt。

## route matrix

caller 数は本ワークトリー（main `8fd1a2bf`）で
`rg -n --pcre2 '(\.|::)<sym>\s*\(|(?<![\w.:])<sym>\s*\('  crates --glob '*.rs'` を実行し、
`tests/` / `examples/` / 各ファイルの `mod tests {` 以降を test、それ以外を prod として数え直したもの。
`.claude/rules/qpdf-port-design-patterns.md` 8 に記録された行番号は 2026-08-21 時点の測定値で
既に drift しているため、**issue ID だけを引用し行番号は再測定した**（`main.rs` は 9313 行、
同ルールが前提にしていた約 4800 行ではない）。

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|
| E-1 | `QPDFJob::run`（`createQPDF` → `writeQPDF` の 2 段） | `libqpdf/QPDFJob.cc:513-519`, `include/qpdf/QPDFJob.hh:371-373` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::run`（pub、`crates/flpdf/src/job/lifecycle.rs:2527`） | prod: 8 (flpdf-cli/src/main.rs, flpdf-qtest-tools/src/driver/test_80_87.rs, flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs) / test: 53 | mixed | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::run` | flpdf の `run` は `create_qpdf` → `run_document_erased`（private）で、**`write_qpdf` を呼ばない**。qpdf の `run` は `createQPDF` → `writeQPDF` の 2 呼び出しだけ。CLI からの `run` 到達は `crates/flpdf-cli/src/main.rs:2883`（`run_job_json_file`、`--job-json-file` 経路）**1 箇所のみ** |
| E-2 | `QPDFJob::createQPDF`（`checkConfiguration` → `processFile` → `updateFromJSON` → `handlePageSpecs` → `handleRotations` → `handleUnderOverlay` → `handleTransformations`） | `libqpdf/QPDFJob.cc:428-481` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::create_qpdf`（pub、`crates/flpdf/src/job/lifecycle.rs:2382`） | prod: 3 (flpdf/src/job/lifecycle.rs, flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs) / test: 7 | mixed | absent | flpdf の `create_qpdf` は「設定検査 + 入力を開く」まで。qpdf が `createQPDF` の内側に持つ変換 5 段は `run_document_erased`（`crates/flpdf/src/job/lifecycle.rs:2624`、private）と `run_document_stages`（`crates/flpdf/src/job/lifecycle.rs:2755`、private）に移っており、**public 2 段契約（`create_qpdf` → `write_qpdf`）だけを使う consumer は変換を一切通らない**。実際の外部 consumer は `qpdfjob_ctest.rs:149,152,165` のみで、そこは `--deterministic-id --progress` のように変換を要求しない argv しか使っていないため差が顕在化していない |
| E-3 | `QPDFJob::writeQPDF` の 3 分岐（`!createsOutput()`→`doInspection` / `split_pages`→`doSplitPages` / else→`writeOutfile`） | `libqpdf/QPDFJob.cc:483-511`, `libqpdf/QPDFJob.cc:528-532` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_qpdf`（pub、`crates/flpdf/src/job/lifecycle.rs:2431`） | prod: 2 (flpdf/src/job/lifecycle.rs, flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs) / test: 4 | mixed | absent | 3 分岐は `write_qpdf` の中ではなく `run_document_stages` の末尾（`crates/flpdf/src/job/lifecycle.rs:2902-2933`）にあり、しかも qpdf と条件が違う — flpdf は「inspection フラグ 10 種の OR」→「`json_version`」→「`check` または出力先なし」→「`write_qpdf`」の 4 段。qpdf は `createsOutput()` 1 個で分ける。flpdf の `write_qpdf` 自身は qpdf の `writeOutfile` + `doSplitPages` 分岐 + 完了処理を畳んだもの（E-4 / E-5） |
| E-4 | `QPDFJob::writeOutfile`（`replace_input` 前後処理、`json_version` 分岐、`QPDFWriter` ブロックスコープ、`setWriterOptions` → `write()`） | `libqpdf/QPDFJob.cc:3029-3091` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_qpdf`（pub）+ `crates/flpdf-cli/src/main.rs::write_with_pdf_writer`（private） | `write_with_pdf_writer` prod: 8 (flpdf-cli/src/main.rs) / test: 0；`write_qpdf_to_memory` prod: 2 (flpdf-cli/src/main.rs) / test: 0 | mixed | absent | **本領域で最大の二重正本**。CLI の通常出力は `QPDFJob` を通らず `main.rs` 自身の `PdfWriter` 経路（`crates/flpdf-cli/src/main.rs:314-331` / `crates/flpdf-cli/src/main.rs:334-345`）を辿る。呼び出し 10 箇所: `crates/flpdf-cli/src/main.rs:4310,4479,5806,5831,6021,6040,6204,7587,7641,7761`。`replace_input` の rename/backup は flpdf 側では `QPDFJob::run` 内（`crates/flpdf/src/job/lifecycle.rs:2546-2552`）にあり、`write_qpdf` にも `main.rs` 経路にも無い。`write_qpdf_to_memory` は `crates/flpdf/src/writer.rs` の同名 `pub(crate)` 関数と別物（caller 数を数えるときの同名衝突に注意） |
| E-5 | `QPDFJob::doSplitPages` | `libqpdf/QPDFJob.cc:2939-3027` | `crates/flpdf/src/job/page_split.rs::QPDFJob::split_pages`（pub、`crates/flpdf/src/job/page_split.rs:135`） | prod: 2 (flpdf/src/job/lifecycle.rs:2492, flpdf-cli/src/main.rs:5926) / test: 14 | mixed | `crates/flpdf/src/job/page_split.rs::QPDFJob::split_pages` | 実装は 1 本だが到達経路が 2 本 — `write_qpdf` 内（qpdf と同じ位置）と CLI の `--split-pages` 直呼び。`pub` は根拠 2（`QPDFJob` 自身の public メソッド）で legitimate |
| E-6 | `QPDFJob::writeJSON` / `doJSON` と `doJSON*` セクション群 | `libqpdf/QPDFJob.cc:3093-3116`, `libqpdf/QPDFJob.cc:1544-1643`, `include/qpdf/QPDFJob.hh:551-565` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_json_with_version`（pub、`crates/flpdf/src/job/lifecycle.rs:3565`）と `crates/flpdf/src/job/lifecycle.rs::write_configured_json`（private、`crates/flpdf/src/job/lifecycle.rs:3077`） | `QPDFJob::write_json_with_version`（メソッド）prod: 5 (flpdf-cli/src/main.rs:3212,3232, flpdf/src/job/lifecycle.rs:3103,3120,3559) / test: 0 | mixed | `crates/flpdf/src/job/json.rs::write_json_with_version_with_logger`（`pub(crate)`） | 実処理は `pub(crate)` の 1 本に集約済みだが、その上に **呼ばれていない `pub` の層が 2 枚**残っている（E-24）。セクション builder（`build_*_section`）は既に `pub(crate)` 化済みで、`json_inspect.rs` の compatibility re-export も撤去済み（E-25） |
| E-7 | `QPDFJob::doInspection`（10 分岐を逐次実行し最後に 1 回だけ warning/完了） | `libqpdf/QPDFJob.cc:1645-1693` | `crates/flpdf/src/job/lifecycle.rs::run_configured_inspection`（private、`crates/flpdf/src/job/lifecycle.rs:2936`） | prod: 1 (flpdf/src/job/lifecycle.rs:2913) / test: 0 | mixed | absent | 分岐順序は qpdf と 1:1（`crates/flpdf/src/job/lifecycle.rs:2950-3010`）で、完了も末尾 1 回（`complete(false)`）。ところが CLI は同じ責務に **`QPDFJob` の個別 public メソッド**（`job.check` / `job.show_npages` / `job.show_xref` / `job.show_pages` / `job.show_object` / `job.show_stream` / `job.dump_object` / `job.show_encryption` / `job.check_linearization` / `job.show_linearization` / `job.list_attachments`）で到達する。各 public メソッドは `inspect`（`crates/flpdf/src/job/lifecycle.rs:3526`）経由で **その場で `complete` する** のに対し `doInspection` 経路は `*_report`（`pub(crate)`、完了しない）を使う — 完了/warning 境界が経路で異なる |
| E-8 | `QPDFJob::doCheck` | `libqpdf/QPDFJob.cc:744-803` | `crates/flpdf/src/job/check.rs::QPDFJob::check`（pub、`crates/flpdf/src/job/check.rs:143`） | prod: 3 (flpdf/src/job/lifecycle.rs:2924, flpdf-cli/src/main.rs:3155,3582) / test: 16 | mixed | `crates/flpdf/src/job/check.rs::QPDFJob::check` | `pub` は根拠 2 かつ `lib.rs` 冒頭 doc に明記あり（根拠 3 も満たす）。到達経路が 2 本（`run_document_stages` と CLI 直呼び）である点だけが mixed |
| E-9 | `QPDFJob::doListAttachments` / `doShowAttachment` / `addAttachments` / `copyAttachments` | `libqpdf/QPDFJob.cc:876-911`, `include/qpdf/QPDFJob.hh:531-532,540-541` | `crates/flpdf/src/job/attachments.rs::QPDFJob::list_attachments`（pub、`crates/flpdf/src/job/attachments.rs:289`）ほか同 impl の 5 メソッド | `list_attachments` prod: 1 (flpdf-cli/src/main.rs:7665) / test: 2 | mixed | `crates/flpdf/src/job/attachments.rs`（`QPDFJob` impl） | `QPDFJob` メソッド側は根拠 2 で legitimate。一方 free 関数 `format_attachment_list` / `format_attachment_list_with_sink` / `list_attachment_info` / `AttachmentInfo`（`crates/flpdf/src/job/attachment_list.rs`）は `src/` に prod caller が 1 つも無い — `format_attachment_list` は test 4 のみ、`list_attachment_info` は test 3（`crates/flpdf/src/job/attachments.rs` の `mod tests`）と `crates/flpdf/examples/pull_attachments.rs:40` のみ。8 (B) の debt がそのまま残る → `flpdf-xsq1` |
| E-10 | `QPDFJob::handlePageSpecs` | `libqpdf/QPDFJob.cc:2359-2633` | `crates/flpdf/src/job/page_specs.rs::QPDFJob::handle_page_specs`（pub、`crates/flpdf/src/job/page_specs.rs:828`） | prod: 5 (flpdf/src/job/lifecycle.rs:2680, flpdf/src/job/page_specs.rs:862, flpdf-cli/src/main.rs:5277,5454,5578) / test: 18 | mixed | `crates/flpdf/src/job/page_specs.rs::QPDFJob::handle_page_specs` | CLI が同じ `QPDFJob` public メソッドを 3 箇所から個別に呼ぶ（single-source / multi-source / collate の分岐が CLI 側にある）。`flpdf-hxmj`（`--pages` パイプライン統合）の対象そのもの。source cache・`keep_files_open` の解決も `lifecycle.rs` 側（`crates/flpdf/src/job/lifecycle.rs:2644-2679`）と CLI 側で別実装 |
| E-11 | `QPDFJob::handleUnderOverlay` / `doUnderOverlayForPage` | `libqpdf/QPDFJob.cc:1936-2043`, `libqpdf/QPDFJob.cc:1858-1911` | `crates/flpdf/src/job/overlay.rs::apply_overlay_specs`（pub free、`crates/flpdf/src/job/overlay.rs:550`）と `crates/flpdf/src/job/overlay.rs::overlay_verbose_report`（pub free、`crates/flpdf/src/job/overlay.rs:645`） | `apply_overlay_specs` prod: 3 (flpdf/src/job/lifecycle.rs:2779, flpdf-cli/src/main.rs:4463,5792) / test: 0；`overlay_verbose_report` prod: 2 (flpdf-cli/src/main.rs:4447,5776) / test: 0 | mixed | `crates/flpdf/src/job/overlay.rs::apply_overlay_specs` | CLI は `QPDFJob` を経由せず `flpdf::apply_overlay_specs` / `flpdf::overlay_verbose_report` を crate ルートから直接呼ぶ（8 (E) の debt、`flpdf-xsq1`）。命名も `handle_under_overlay` になっていない（`flpdf-ei0h`）。overlay source を開く処理は `lifecycle.rs:2765-2778` と CLI 側（`crates/flpdf-cli/src/main.rs:4872` 近傍）で別実装 |
| E-12 | `QPDFJob::handleTransformations`（`remove_restrictions` → `externalize_inline_images` → `optimize_images` → `generate_appearances` → `flatten_annotations` → `coalesce_contents` → `flatten_rotation` → `remove_page_labels` の 8 分岐） | `libqpdf/QPDFJob.cc:2137-2248`（8 分岐は `libqpdf/QPDFJob.cc:2147-2199`） | `crates/flpdf/src/job/lifecycle.rs::run_document_stages`（private、`crates/flpdf/src/job/lifecycle.rs:2755`） | prod: 3（すべて `crates/flpdf/src/job/lifecycle.rs` 内の自己呼び出し 2642,2695,2699）/ test: 0 | mixed | absent | qpdf の 8 段と同じ順序を保つ実装が `run_document_stages` にあるが、CLI はこれを通らず各部品を個別に呼ぶ — `flpdf::optimize_images`（`crates/flpdf-cli/src/main.rs:3043,3073,4292,4348,5725,5995` の 6 箇所）、`flpdf::flatten_rotation_on_pages`（`crates/flpdf-cli/src/main.rs:4412`）ほか。順序の一致は CLI 側では保証されていない |
| E-13 | `QPDFJob::handleRotations` | `libqpdf/QPDFJob.cc:2635-2652` | `crates/flpdf/src/job/lifecycle.rs::apply_configured_rotations`（private、`crates/flpdf/src/job/lifecycle.rs:2710`）と `crates/flpdf/src/job/rotate.rs::apply_rotate_to_pages`（pub free、`crates/flpdf/src/job/rotate.rs`） | `apply_rotate_to_pages` prod: 2 (flpdf/src/job/lifecycle.rs:2750, flpdf-cli/src/main.rs:5872) / test: 16 | mixed | absent | qpdf の `handleRotations` は「範囲解決 → ページ適用」を 1 つのループで行うのに対し、flpdf ではその前半（`PageRange::resolve` + 0-base 変換 + 空文書ガード）が `apply_configured_rotations` にしかなく、`apply_rotate_to_pages` は後半（適用）だけを持つ。CLI は前半を自前で書いてから後半を呼ぶ。**責務の片側しか持たないので canonical owner は `absent`**（E-7 / E-12 と同型）。`apply_rotate_to_pages` は crate ルートには再輸出されておらず（`crates/flpdf/src/lib.rs` に無い）`flpdf::job::` からのみ到達するが、8 の (A)〜(E) いずれにも記載が無い**新規の debt 候補** |
| E-14 | `QPDFJob::parseRotationParameter` | `libqpdf/QPDFJob.cc:368-415`, `include/qpdf/QPDFJob.hh:482` | `crates/flpdf/src/job/rotate_spec.rs::RotateSpec::parse`（pub、`crates/flpdf/src/job/rotate_spec.rs:74`） | prod: 2 (flpdf/src/job/lifecycle.rs:1950, flpdf-cli/src/main.rs:5853) / test: 多数（`crates/flpdf/src/job/rotate_spec.rs` 内） | mixed | `crates/flpdf/src/job/rotate_spec.rs::RotateSpec::parse` | 8 (D) の debt がそのまま残存 — qpdf 側は private メソッドのみで `QPDFJob` public の薄いラッパーも無いのに `main.rs` が直接呼ぶ。可視性は `flpdf-hxmj`、命名（`parse_rotation_parameter` になっていない）は `flpdf-ei0h` |
| E-15 | `QUtil::parse_numrange`（`QPDFJob::parseNumrange` は例外処理を足した薄いラッパー） | `include/qpdf/QUtil.hh:464`, `libqpdf/QPDFJob.cc:417-426` | `crates/flpdf/src/job/page_range.rs::PageRange::parse`（pub、`crates/flpdf/src/job/page_range.rs:94`）+ `crates/flpdf/src/job/page_range.rs::PageRange::resolve`（pub、`crates/flpdf/src/job/page_range.rs:131`） | `PageRange::parse` prod: 15 (flpdf-cli/src/main.rs 10 箇所: 4594,4625,4733,4742,4751,4886,4888,4895,4897,4906、flpdf/src/job/lifecycle.rs, flpdf/src/job/rotate_spec.rs) / test: 67 (flpdf/src/job/page_combine.rs, page_plan.rs, page_range.rs, page_specs.rs の `mod tests` と flpdf/tests/job_lifecycle_tests.rs)。ほかに doc コメント参照 7 |  mixed | `crates/flpdf/src/job/page_range.rs::PageRange` | `pub` 自体は 8 の根拠 1 で legitimate（qpdf 側 `QUtil::parse_numrange` が真に public）。ただし qpdf の 1 関数を `parse`（page count 不要）と `resolve`（page count 必須）へ 2 分割している点は qpdf の処理順序からの逸脱で、`flpdf-ei0h` に記録済み。route としては「構文検証だけ先に走らせる」CLI 経路と「解決まで一気に行う」job 経路の 2 本 |
| E-16 | `QPDFJob::shouldRemoveUnreferencedResources` | `libqpdf/QPDFJob.cc:2250-2339`, `include/qpdf/QPDFJob.hh:515` | `crates/flpdf/src/job/resource_pruning.rs::should_remove_unreferenced_resources`（pub free） | prod: 3 (flpdf/src/job/page_merge.rs:854, flpdf/src/job/page_specs.rs:192, flpdf-cli/src/main.rs:5707) / test: 10 | mixed | `crates/flpdf/src/job/resource_pruning.rs::should_remove_unreferenced_resources` | 実装 1 本に対し呼び出し 3 経路。qpdf 側は `handlePageSpecs` からしか呼ばれない private メソッド。crate ルート `pub`（`crates/flpdf/src/lib.rs:190-193`）は 8 の (A)〜(E) 未記載の**新規 debt 候補** |
| E-17 | `QPDFJob::initializeFromArgv` / `initializeFromJson`（`QPDFArgParser` 経由の argv 解釈） | `include/qpdf/QPDFJob.hh:75-90`, `libqpdf/QPDFJob_argv.cc` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::initialize_from_argv`（pub、`crates/flpdf/src/job/lifecycle.rs:1521`） | prod: 3（すべて flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs:147,164,183）/ test: 16 (flpdf/tests/job_lifecycle_tests.rs) | mixed | absent | **flpdf-cli は `initialize_from_argv` を一度も呼ばない** — clap 定義（CLAUDE.md 逸脱分類 (B) の `QPDFArgParser` → clap）で独自に引数を解釈し、`QPDFJob` の setter を個別に叩く（`job.set_input_file` / `job.set_output_file` / `job.set_password` …）。qpdf の CLI は `initializeFromArgv` 1 本しか使わない（`qpdf/qpdf.cc:35`）。argv → 設定の正本が CLI 側と library 側で 2 本ある状態 |
| E-18 | `QPDFJob::checkConfiguration` | `libqpdf/QPDFJob.cc:566-642`, `include/qpdf/QPDFJob.hh:129-130` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::check_configuration`（pub、`crates/flpdf/src/job/lifecycle.rs:3230`） | prod: 8 (flpdf/src/job/lifecycle.rs 4, flpdf-qtest-tools/src/driver/test_80_87.rs 4) / test: 0 | canonical | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::check_configuration` | qpdf と同じく `createQPDF` 冒頭（`crates/flpdf/src/job/lifecycle.rs:2383`）から呼ばれ、public としても露出。CLI は使わない（E-17 の帰結）が、それは「別の正本がある」のではなく「CLI が job 設定を組み立てない」ため |
| E-19 | `QPDFJob::getExitCode` / `hasWarnings` / `createsOutput` | `libqpdf/QPDFJob.cc:522-564` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::complete`（pub、`crates/flpdf/src/job/lifecycle.rs:3641`）+ `crates/flpdf/src/job/lifecycle.rs::QPDFJob::has_warnings`（pub、`crates/flpdf/src/job/lifecycle.rs:3616`） | `complete` prod: 13 (flpdf/src/job/check.rs 2, flpdf/src/job/lifecycle.rs 5, flpdf-cli/src/main.rs 6) / test: 5；`has_warnings` prod: 8 (flpdf-cli/src/main.rs 6, flpdf-qtest-tools/src/driver/test_80_87.rs 2) / test: 6 | mixed | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::complete` | qpdf は「状態を溜めて `getExitCode()` で 1 回だけ判定」。flpdf は `complete(creates_output)` を **各ステージが個別に呼ぶ** 設計で、CLI からも 6 箇所（`crates/flpdf-cli/src/main.rs:5828,6037,6864,7285,7600,7774`）呼ばれる。`--is-encrypted` / `--requires-password` の 4 値 exit code は `run_encryption_status`（`crates/flpdf/src/job/lifecycle.rs:2563`、private）が別に持つ |
| E-20 | `QPDFJob::getLogger` / `setLogger` / `setMessagePrefix` / `getMessagePrefix` / `registerProgressReporter` | `libqpdf/QPDFJob.cc:302-337`, `include/qpdf/QPDFJob.hh:92-123` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::logger`（pub、`crates/flpdf/src/job/lifecycle.rs:1308`）ほか 4 メソッド | `set_message_prefix` prod: 27 (flpdf/src/job/lifecycle.rs, flpdf-cli/src/main.rs, flpdf-qtest-tools) / test: 6；`register_progress_reporter` prod: 4 (flpdf/src/job/lifecycle.rs, flpdf-qtest-tools/src/driver/test_80_87.rs, flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs) / test: 3 | canonical | `crates/flpdf/src/job/lifecycle.rs`（`QPDFJob` の logger/prefix impl） | `logger()` / `message_prefix()` の `get_` 省略は 7 の bare getter 例外に該当し正しい。CLI が `QPDFJob::new` を 25 回作って毎回 logger と prefix を設定し直しているのは E-17 / E-4 の帰結（job インスタンスが lifecycle を持たない） |
| E-21 | CLI 実行ファイル consumer（`QPDFJob` public のみ 4 手） | `qpdf/qpdf.cc:26-44` | `crates/flpdf-cli/src/main.rs::main`（bin crate） | prod: 1 bin（`crates/flpdf-cli`、9313 行）/ test: 66 統合テストファイル（`crates/flpdf-cli/tests/*.rs`） | mixed | absent | qpdf の CLI は `initializeFromArgv` → `run` → `getExitCode` の 3 呼び出し（62 行）。flpdf-cli は 9313 行で、`QPDFJob::new` を 25 回作り、`run()` は 1 箇所（`--job-json-file`）でしか呼ばない。E-4 / E-12 / E-17 が示すとおり `writeOutfile` / `handleTransformations` / argv 解釈を自前で持つ。単純な可視性変更では閉じられず、`flpdf-hxmj` の CLI 経路一本化が前提 |
| E-22 | `QPDFJob` の C API consumer（pure pass-through） | `libqpdf/qpdfjob-c.cc:19-161`, `qpdf/qpdfjob-ctest.c`（142 行、機械検証対象外の `.c`） | `crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs::main`（bin crate、281 行） | prod: 1 bin / test: qtest ハーネス（`crates/flpdf-qtest-tools/src/orchestrator.rs`）経由 | canonical | `crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs` | `QPDFJob` の public surface（`new` / `register_progress_reporter` / `initialize_from_argv` / `initialize_from_json` / `create_qpdf` / `write_qpdf` / `run` / `set_logger` / `set_message_prefix`）しか触らず、qpdf の C wrapper 構造を正しく踏襲している唯一の consumer。ただし `report_job_error`（`crates/flpdf/src/job/lifecycle.rs:3369`、pub）は qpdf 側では C wrapper 内の `wrap_qpdfjob`（`libqpdf/qpdfjob-c.cc:32-41`）に相当し、`QPDFJob` の public メソッドではない — 位置が違う |
| E-23 | `QPDF` / `QPDFWriter` の C API consumer（`QPDFJob` を経由しない） | `libqpdf/qpdf-c.cc:24-64`, `libqpdf/qpdf-c.cc:459-521`, `libqpdf/qpdf-c.cc:1924-1952` | `crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs::main`（bin crate、523 行） | prod: 1 bin / test: qtest ハーネス（`crates/flpdf-qtest-tools/src/orchestrator.rs`）経由 | canonical | `crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs` | `qpdf_check_pdf` は `QPDFJob::doCheck` ではなく `QPDFWriter` + `Pl_Discard` + `setDecodeLevel(qpdf_dl_all)` で `write()` する（`libqpdf/qpdf-c.cc:57-64`）— flpdf 側でも E-8 の `QPDFJob::check` とは別責務として扱う必要がある |
| E-24 | `QPDFJob::writeJSON` の library 入口（qpdf 側に対応する public 識別子なし） | `libqpdf/QPDFJob.cc:3093-3116`（private） | `crates/flpdf/src/job/json.rs::write_json`（pub、`crates/flpdf/src/job/json.rs:588`）と `crates/flpdf/src/job/json.rs::write_json_with_version`（pub、`crates/flpdf/src/job/json.rs:602`） | `write_json`（free）prod: 0 / test: 0；`write_json_with_version`（free）prod: 1（`crates/flpdf/src/job/json.rs:593` — 上の死んだ `write_json` からの呼び出しのみ）/ test: 0 | bridge | `crates/flpdf/src/job/json.rs::write_json_with_version_with_logger`（`pub(crate)`） | 8 (A) が「唯一の呼び出し元は `QPDFJob::write_json` メソッド」と記録していた状態から進行しており、現在は **`QPDFJob::write_json_with_version` が `write_json_with_version_with_logger` を直接呼ぶ**（`crates/flpdf/src/job/lifecycle.rs:3580`）ため、`pub` 2 本は互いを呼び合うだけの閉じた死枝になっている（free `write_json` は caller ゼロ、free `write_json_with_version` の唯一の caller はその死んだ `write_json`）。`crates/flpdf-cli/src/main.rs:3212,3232` や `crates/flpdf/src/job/lifecycle.rs:3103,3120,3559` の `write_json_with_version` は同名の `QPDFJob` **メソッド**で別物。`lib.rs` の crate ルート再輸出も無く、`flpdf::job::` からのみ到達可能。削除候補 → `flpdf-xsq1` |
| E-25 | `doJSON*` セクション builder の historical public path | `include/qpdf/QPDFJob.hh:551-565`（すべて private） | `crates/flpdf/src/job/json_sections.rs::build_pages_section_with_options`（`pub(crate)`、`crates/flpdf/src/job/json_sections.rs:189`）ほか | prod: 自クレート内のみ / test: — | canonical | `crates/flpdf/src/job/json_sections.rs` | 8 (C)（`flpdf-7bkv`）の staged migration は **完了済み** — `crates/flpdf/src/job/mod.rs` の `pub use json_sections::{build_*_section, ...}` は存在せず、`crates/flpdf/src/json_inspect.rs` の compatibility re-export ブロックも撤去され、**素の 6 個**（`build_pages_section` / `build_outlines_section` / `build_pagelabels_section` / `build_acroform_section` / `build_encrypt_section` / `build_attachments_section`）は `rg -n 'fn build_(pages|outlines|pagelabels|acroform|encrypt|attachments)_section\b' crates/flpdf/src` が 0 件で宣言自体が存在せず、残るのは `_with_options` / `_with_version` 付きの `pub(crate)` 版のみ。`write_qpdf_json_v2_selected_objects_with_options` も workspace 全体で 0 件。8 (C) が close の blocker として挙げた `crates/flpdf/tests/document_json_tests.rs` は `flpdf::document_json::write_json` と `flpdf::json_inspect::{DecodeLevel, JsonKey, JsonOutputError, StreamDataMode}`（いずれも型/エラーで、(C) の section builder ではない）だけを import しており、compat 経路を通らない。`flpdf-7bkv` は close 判定の対象（本表では close 操作は行わない） |
| E-26 | `QPDFJob::handlePageSpecs` 内の AcroForm 刈り込み（qpdf 側に個別識別子なし） | `libqpdf/QPDFJob.cc:2610-2632` | `crates/flpdf/src/job/acroform_field_prune.rs::prune_acroform_after_subset`（pub free）+ `crates/flpdf/src/job/page_specs.rs::QPDFJob::prune_acroform_after_subset`（pub メソッド、`crates/flpdf/src/job/page_specs.rs:765`） | free 関数 prod: 2（いずれも `crates/flpdf/src/job/page_specs.rs:769,795` の同一クレート内呼び出し）/ test: 19 | bridge | `crates/flpdf/src/job/page_specs.rs::QPDFJob::prune_acroform_after_subset` | 8 (A) の記録どおり、free 関数側の `pub` は根拠 1〜3 のいずれも満たさない（`crates/flpdf/src/lib.rs:180-189` の crate ルート再輸出はあるが opening `//!` doc への明記が無い）。`pub(crate)` へ狭められる → `flpdf-xsq1` |
| E-27 | `QPDFObjectHandle::getParsedOffset`（parsed offset の報告） | `include/qpdf/QPDFObjectHandle.hh:419`（outer class の `public:` は `include/qpdf/QPDFObjectHandle.hh:68` から）, `qpdf/test_parsedoffset.cc` | `crates/flpdf/src/reader.rs::Pdf::qtest_object_value_source_offsets`（pub、`crates/flpdf/src/reader.rs:861`）ほか同ファイルの `qtest_*` 5 メソッド | `qtest_object_value_source_offsets` prod: 1 (flpdf-qtest-tools/src/driver/test_0_1.rs:260) / test: 0；`qtest_array_item_source_offsets` prod: 1 (同 :267) / test: 0；`qtest_decode_parms_source_offset` prod: 1 (同 :282) / test: 0；`qtest_object_value_source_offset`（単数形）prod: 0 / test: 0；`qtest_array_item_source_offset`（単数形）prod: 0 / test: 0 | bridge | absent | qpdf は object handle 側に public な `getParsedOffset()` を置くが、flpdf は `Pdf` 側に `qtest_` 接頭辞つきの複数取り版を置いている（7 の「qpdf 識別子からの導出」に反する命名）。**別 crate の `flpdf-qtest-tools` からしか見えないため `pub(crate)` にできない**という構造的緊張がある（8 の「test-only な `pub` は `pub(crate)` へ」は同一クレート内テストを前提にしている）。単数形 2 本は caller ゼロで、緊張と無関係に削除可能 |
| E-28 | `qpdf/test_driver.cc` consumer（`QPDF` / `QPDFObjectHandle` / helper の public を総当たりで叩く 99 ケース） | `qpdf/test_driver.cc:3540-3557`（`std::map<int, void (*)(QPDF&, char const*)> test_functions` に 0〜98 の 99 エントリ）, `qpdf/test_driver.cc:3559-3562` | `crates/flpdf-qtest-tools/src/driver/mod.rs::run`（`crates/flpdf-qtest-tools/src/driver/mod.rs:33`、13 ファイル 12,000 行超） | prod: 1 bin（`crates/flpdf-qtest-tools/src/bin/driver.rs`）/ test: 3 (`crates/flpdf-qtest-tools/tests/driver_cli.rs`, `driver_goldens.rs`, `xref_parsedoffset_cli.rs`) | unknown | unknown | 本表（領域 E）は `QPDFJob` / CLI / C API の境界を対象としており、`test_driver.cc` が触る API 面は領域 A（ObjectHandle）・B（parser）・C（stream）に散る。**この consumer の route 分類は各領域の行が確定してからでないと決められない** — 必要な作業は「`driver/*.rs` が呼ぶ `flpdf::` 公開項目を全列挙し、A〜D の canonical owner 行と突き合わせる」こと（下記 probe P-2） |

### 分類別件数

| 分類 | 件数 | 行 |
|---|---|---|
| canonical | 5 | E-18, E-20, E-22, E-23, E-25 |
| bridge | 3 | E-24, E-26, E-27 |
| mixed | 19 | E-1, E-2, E-3, E-4, E-5, E-6, E-7, E-8, E-9, E-10, E-11, E-12, E-13, E-14, E-15, E-16, E-17, E-19, E-21 |
| unknown | 1 | E-28 |

（合計 28 行。canonical 5 + bridge 3 + mixed 19 + unknown 1 = 28。`E-18` は `mixed` に見えるが、CLI が使わないのは「別の正本がある」からではなく E-17 の帰結であるため `canonical`）

### `main.rs` が直接 import する job/ 項目のうち、8 の (A)〜(E) に未記載のもの

`.claude/rules/qpdf-port-design-patterns.md` 8 (E) は「この監査は `job/mod.rs` の `pub use` 起点で、
`main.rs` が job/ の型・関数を直接 import している箇所を全数走査したものではない」と明記している。
本表がその全数走査にあたる。`crates/flpdf-cli/src/main.rs:6-28` の `use flpdf::…` 全 5 ブロックを
`crates/flpdf/src/job/mod.rs` の `pub use` 一覧と突き合わせた結果、**(A)〜(E) と支援型（第 4 の根拠）
のどれにも記載が無い job/ 由来の項目は次の 11 行**（`crates/flpdf-cli/src/main.rs:6-28` の `use` に加えて
`rg -no 'flpdf::[A-Za-z_:]+' crates/flpdf-cli/src/main.rs` の完全修飾参照も突き合わせた。うち最後の 1 行
（`PageSpecJobOutput` / `JobExitCode`）は「未記載だが第 4 の根拠で legitimate」で debt ではない）:

| 項目 | 宣言 | `main.rs` での prod 呼び出し | 備考 |
|---|---|---|---|
| `apply_rotate_to_pages` | `crates/flpdf/src/job/rotate.rs` | `crates/flpdf-cli/src/main.rs:5872` | E-13。`QPDFJob` public メソッドを経由しない |
| `flatten_rotation_on_pages` | `crates/flpdf/src/job/rotate.rs` | `crates/flpdf-cli/src/main.rs:4412` | E-12 |
| `optimize_images` | `crates/flpdf/src/job/image_optimization.rs` | `crates/flpdf-cli/src/main.rs:3043,3073,4292,4348,5725,5995` | E-12。6 箇所と本領域最多 |
| `should_remove_unreferenced_resources` | `crates/flpdf/src/job/resource_pruning.rs` | `crates/flpdf-cli/src/main.rs:5707` | E-16 |
| `copy_duplicate_page_annotations` | `crates/flpdf/src/job/page_specs.rs` | `crates/flpdf-cli/src/main.rs:5714` | qpdf 側は `handlePageSpecs` 内のインラインコード（`libqpdf/QPDFJob.cc:2359-2633`）で個別識別子なし → 7 の「独自命名は逸脱でない」に該当するが、`pub` の根拠は別途要る |
| `OverlaySpec` / `OverlayKind` | `crates/flpdf/src/job/overlay.rs` | `apply_overlay_specs` の引数型（`crates/flpdf-cli/src/main.rs:4463,5792`） | 8 (E) が挙げた `apply_overlay_specs` の**支援型**。第 4 の根拠が働くのは「legitimate な `pub` メソッドのシグネチャ」に対してであり、`apply_overlay_specs` 自身が debt である以上こちらも従属 debt |
| `CombinedPage` / `InputSpec` | `crates/flpdf/src/job/page_combine.rs` | `crates/flpdf-cli/src/main.rs` の `--pages` 経路 | `PageRange`（根拠 1）と違い qpdf 側に対応 public 識別子なし |
| `SelectedPage` | `crates/flpdf/src/job/page_plan.rs` | 同上 | 同上 |
| `ImageOptimizationOptions` / `RemoveUnreferencedResources` | `crates/flpdf/src/job/image_optimization.rs` / `crates/flpdf/src/job/resource_pruning.rs` | `optimize_images` / `should_remove_unreferenced_resources` の引数型 | 従属 debt（上と同じ理由） |
| `FlattenAnnotationsMode`（+ `FlattenAnnotationsMode::qpdf_flags`） | `crates/flpdf/src/job/lifecycle.rs:46`（`qpdf_flags` は `crates/flpdf/src/job/lifecycle.rs:57`） | `crates/flpdf-cli/src/main.rs:1941,7810,7814,7818` で `qpdf_flags()` を呼び、得た `(required, forbidden)` で `PageDocumentHelper::flatten_annotations` を直接叩く（`crates/flpdf-cli/src/main.rs:4300,4396`） | **第 4 の根拠に該当しない**（probe で確定）— `rg -n 'FlattenAnnotationsMode' crates/flpdf/src` の全 7 ヒットに `pub fn` シグネチャは 1 つも無く、`crates/flpdf/src/job/lifecycle.rs:209` の private フィールドと `initialize_from_json` の解析でしか使われない。CLI は `QPDFJob` を完全に迂回して flatten を実行している（E-12） |
| `PageSpecJobOutput` / `JobExitCode` | `crates/flpdf/src/job/page_specs.rs` / `crates/flpdf/src/job/lifecycle.rs` | `job.handle_page_specs` / `job.check` / `job.run` の戻り値型 | **第 4 の根拠で legitimate**（未記載だが debt ではない）— 根拠 2 で legitimate な `QPDFJob` public メソッドのシグネチャが要求している |

`job/` 以外（`fix_qdf` / `normalize_content_stream` / `pages` / `parse_pdf_version` /
`parse_pdf_version_spec` / `qpdf_version` / `qutil::same_file` / `pipeline::*` /
`writer::DecodeLevel` / `Pdf` / `PdfWriter` / `ObjectHandle` / `AcroFormDocumentHelper` /
`PageDocumentHelper` / `PageObjectHelper` / `json_inspect::{DecodeLevel, JsonKey, JsonObjectSelector}`）は
領域 A〜D の対象なので本表では扱わない。

### 8 の (A)〜(E) の現況（再測定）

| 群 | 記録された項目 | 2026-09-04 の実測 |
|---|---|---|
| (A) | `prune_acroform_after_subset` 系 3 個 + `write_json` | `prune_acroform_after_subset` 系は debt のまま（E-26）。`write_json` は **記録より悪化** — 呼び出し元が消え prod/test とも 0（E-24）。ルールが記した `job/json.rs:257` は現在 `crates/flpdf/src/job/json.rs:588` |
| (B) | `format_attachment_list` / `format_attachment_list_with_sink` / `list_attachment_info` / `AttachmentInfo` | 4 個とも `crates/flpdf/src/job/mod.rs` に `pub use` されたまま。prod caller 0（E-9） |
| (C) | `build_*_section` 6 個 + `write_qpdf_json_v2_selected_objects*` 2 個 | **解消済み**（E-25）。`flpdf-7bkv` は close 判定の対象 |
| (D) | `RotateSpec` | debt のまま（E-14） |
| (E) | `overlay_verbose_report` / `apply_overlay_specs` / `collate` | `overlay_verbose_report` / `apply_overlay_specs` は debt のまま（E-11）。**`collate` は消滅** — `fn collate` は workspace に 0 件で、`page_collate.rs` というファイル自体が存在しない（`crates/flpdf/src/job/` の全 22 ファイルを `ls` で確認） |

## unknown / probe

| ID | 決められないこと | 必要な source / probe |
|---|---|---|
| P-1 | E-2 の実害の有無 — `create_qpdf` → `write_qpdf` の 2 段だけを使う consumer が変換（`--pages` / `--rotate` / `--overlay` / `--flatten-annotations` …）を要求したときに、qpdf と flpdf で出力が変わるか。現状の唯一の外部 consumer（`crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs`）は変換を要求しない argv しか使っていないため観測されていない | `qpdf --deterministic-id --rotate=90 in.pdf out.pdf` の出力と、`QPDFJob::initialize_from_argv(同 argv)` → `create_qpdf()` → `write_qpdf()` の 2 段だけを呼ぶテストバイナリの出力を byte 比較する。差が出れば E-2 は mixed ではなく「public 契約が qpdf と非等価」という別分類 |
| P-2 | E-28（`qpdf/test_driver.cc` consumer）の route 分類 | `rg -no 'flpdf::[A-Za-z_:]+' $W/crates/flpdf-qtest-tools/src/driver --glob '*.rs' \| sort -u` で公開項目を全列挙し、領域 A〜D の canonical owner 行と突き合わせる。領域 A〜D の matrix が埋まるまで判定不能 |
| P-3 | E-19 の exit code 等価性 — flpdf は `complete(creates_output)` を各ステージが呼ぶのに対し qpdf は `getExitCode()` で 1 回だけ判定する。複数の inspection フラグを同時に指定したとき warning 集計と exit code が一致するか | `qpdf --check --show-npages --show-xref warn.pdf; echo $?` と `flpdf` の同等呼び出しで exit code と stderr 行数を比較。`libqpdf/QPDFJob.cc:534-564` が判定を 1 回しか行わない点が根拠 |
| P-4 | E-21 の cutover 前提 — `flpdf-cli` の 10 箇所の直接書き出し（E-4）を `QPDFJob::write_qpdf` へ寄せたとき、`replace_input` の rename/backup（現在 `QPDFJob::run` 内、`crates/flpdf/src/job/lifecycle.rs:2546-2552`）がどこに属するか。qpdf では `writeOutfile` の内側（`libqpdf/QPDFJob.cc:3069-3091`） | `libqpdf/QPDFJob.cc:3029-3091` を再読し、`temp_out` のスコープと `pdf.closeInputSource()` の位置を flpdf の `finish_replace_input`（`crates/flpdf/src/job/lifecycle.rs:3170`）と 1:1 で突き合わせる |
