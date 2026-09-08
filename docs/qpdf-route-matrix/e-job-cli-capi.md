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

既知の open debt（重複 issue を作らず本表から引用する）: `flpdf-xsq1`（pub 可視性）、
`flpdf-ei0h`（命名）。`flpdf-7bkv`（JSON compatibility re-export 撤去）と
`flpdf-hxmj`（single-source `--pages` の `handle_page_specs` 接続）は closed。
後者は CLI 全体の orchestration 集約を完了した issue ではなく、残る E-10/E-21 の
consumer 移行には別の bounded slice が必要である（2026-09-06 readback）。

## qpdf 責務モデル

pinned qpdf 11.9.0 の `include/qpdf/QPDFJob.hh`（720 行）/ `libqpdf/QPDFJob.cc`（3116 行）/
`qpdf/qpdf.cc`（62 行）/ `libqpdf/qpdfjob-c.cc`（161 行）/ `libqpdf/qpdf-c.cc`（1963 行）を
`rg -n` と `sed -n` で読んだ範囲のみを引用する。flpdf のファイルは本節を書き終えるまで開いていない。

### M-0. `QPDFJob.hh` の public / private 境界（ネストクラスを漏らさずに数える）

`class QPDFJob`（`include/qpdf/QPDFJob.hh:43-718`）のアクセス指定子は、**ネストクラスの
`private:` を外側に漏らさずに** 数えると次の 4 区画になる（`.claude/rules/qpdf-port-design-patterns.md`
8 の「`QPDFJob.hh` を読むときの罠」）。区画 4 は 1 つの `private:`（`include/qpdf/QPDFJob.hh:425`）が
最後まで続く単一領域で、下表の 4a / 4b は **アクセス指定子ではなく内容による細分**。

| 区画 | 行範囲 | 種別 | 中身 |
|---|---|---|---|
| 1 | `include/qpdf/QPDFJob.hh:45-136` | **public** | `LATEST_JOB_JSON` / `EXIT_*` 定数、`QPDFJob()`、`initializeFromArgv`、`initializeFromJson`、`setMessagePrefix` / `getMessagePrefix`、`getLogger` / `setLogger`、`setOutputStreams`（deprecated）、`registerProgressReporter`、`checkConfiguration`、`createsOutput` |
| 2 | `include/qpdf/QPDFJob.hh:137-166` | private | `CopyAttachmentFrom` / `AddAttachment` / `PageSpec` — 「public な Config クラスより前に定義が要る」ためここに置かれた private struct |
| 3 | `include/qpdf/QPDFJob.hh:167-424` | **public** | Config 系ネストクラス（`AttConfig` / `CopyAttConfig` / `PagesConfig` / `UOConfig` / `EncConfig` / `PageLabelsConfig` / `Config`）と、その後の `config()` / `run()` / `createQPDF()` / `writeQPDF()` / `hasWarnings()` / `getExitCode()` / `getEncryptionStatus()` / `doIfVerbose()` / `json_out_schema()` / `job_json_schema()` |
| 4a | `include/qpdf/QPDFJob.hh:425-568` | private | `RotationSpec` / `password_mode_e` / `UnderOverlay` / `PageLabelSpec`、および後述の private メソッド群 |
| 4b | `include/qpdf/QPDFJob.hh:569-717` | private | ネストクラス `Members`（全設定 state。`friend class QPDFJob`）と、それを保持する唯一のデータメンバー `std::shared_ptr<Members> m;`（`include/qpdf/QPDFJob.hh:717`） |

区画 3 のネストクラスはそれぞれ自分の `private:` を持つ（例: `PagesConfig` は
`include/qpdf/QPDFJob.hh:254`、`UOConfig` は `include/qpdf/QPDFJob.hh:272`、`Config` は
`include/qpdf/QPDFJob.hh:353`）。`Config` の `};`（`include/qpdf/QPDFJob.hh:361`）の直後は
外側 `QPDFJob` の区画 3（public）に戻るので、`run()` / `createQPDF()` / `writeQPDF()` /
`hasWarnings()` / `getExitCode()` はいずれも **public**。

区画 4a の private メソッドは責務ごとに 5 群に分かれている:

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

`QPDFJob::run()`（`libqpdf/QPDFJob.cc:513-520`）は 8 行しかない:

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

`handleTransformations` の `remove_restrictions` は
`QPDFAcroFormDocumentHelper::disableDigitalSignatures()` を呼ぶだけで、成功時の
custom warning/messageは生成しない（`libqpdf/QPDFJob.cc:2137-2150`、
`libqpdf/QPDFAcroFormDocumentHelper.cc:419-439`）。CLIの
`--remove-restrictions` も同じくmutation後に独自の `removed restrictions` /
`removed signatures` 行を追加せず、document warningがある場合だけ通常の
`writeQPDF` completion boundaryへ委譲する。`--no-warn` は `QPDF::warn` の表示を
抑止するがwarning collectionと終了statusは保持する
（`libqpdf/QPDF.cc:487-504`、`libqpdf/QPDFJob.cc:650-666`）。

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
（`libqpdf/qpdf-c.cc:24-66` の 6 つの static helper — `call_read` / `call_read_memory` /
`call_init_write` / `call_init_write_memory` / `call_write` / `call_check` — が代表）:

- 読み込み: `QPDF::processFile` / `QPDF::processMemoryFile` / `QPDF::emptyPDF` /
  `QPDF::createFromJSON` / `QPDF::updateFromJSON`（`libqpdf/qpdf-c.cc:24-35,266-310,1885-1923`）
- 書き出し: `QPDFWriter` のコンストラクタ 2 種 / `setOutputMemory` / `write`
  （`libqpdf/qpdf-c.cc:37-55,459-521`）と、`qpdf_set_*` 族が写す `QPDFWriter` の setter 群
  （`libqpdf/qpdf-c.cc:523-777`）
- 検査: `qpdf_check_pdf`（`libqpdf/qpdf-c.cc:224-231`）は **`QPDFJob::doCheck` を呼ばず**、
  `call_check`（`libqpdf/qpdf-c.cc:58-66`）で `QPDFWriter` に `Pl_Discard` +
  `setDecodeLevel(qpdf_dl_all)` を設定して `write()` するだけ
- object handle: `qpdf_oh_*` 族（`libqpdf/qpdf-c.cc:841-1793`）が `QPDFObjectHandle` の public を写す
- ページ: `qpdf_get_num_pages` 他（`libqpdf/qpdf-c.cc:1795-1883`）が `QPDFPageDocumentHelper` /
  `QPDF` の page API を写す
- JSON: `qpdf_write_json`（`libqpdf/qpdf-c.cc:1924-1952`）は `QPDF::writeJSON` を直接呼ぶ
  （`QPDFJob::writeJSON` ではない — 名前が似ているが別責務）

エラーは `trap_errors`（`libqpdf/qpdf-c.cc:68-89`）が `QPDFExc` / `std::runtime_error` /
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

**caller 数え方の規約（領域 B / D と共通）**: 初回計測時のワークトリー（main `8fd1a2bf`）で
`rg -n --pcre2 '(\.|::)<sym>\s*\(|(?<![\w.:])<sym>\s*\(' crates --glob '*.rs'` を実行し、

- **除外**: コメント内の出現のみの行、宣言（`fn <sym>` / `struct` / `enum` …）、`use` 行（複数行 `use` の継続行を含む）、`impl <Type>` ヘッダ、文字列リテラル内。
- **計上**: 呼び出しサイトと型位置の参照。
- **production**: `crates/*/src/` のうち `#[cfg(test)]` / `mod tests {` の外側。**test**: `crates/*/tests/` と `mod tests {` 以降。`crates/*/examples/` は published されない consumer なので test 側に数え、該当する行では明示する。
- 同名別シンボル（例: `PdfWriter::register_progress_reporter` と `QPDFJob::register_progress_reporter`、`crates/flpdf/src/writer.rs` の `pub(crate) fn write_qpdf_to_memory` と `crates/flpdf-cli/src/main.rs` の同名 private 関数）は宣言元を確認して分離し、行の注記で断る。

`.claude/rules/qpdf-port-design-patterns.md` 8 に記録された行番号は 2026-08-21 時点の測定値で
既に drift しているため、**issue ID だけを引用し行番号は再測定した**（`main.rs` は 9313 行、
同ルールが前提にしていた約 4800 行ではない）。

2026-09-06 の追跡監査は main `4a2faf5c` を基準とする。以下の訂正行に明記した
caller と owner は再確認済み。それ以外の詳細な caller 数・行番号は上記初回計測値であり、
全行の現在値を再測定したという意味ではない。E-28 は既知の bridge caller を根拠に
mixed へ変更したが、未照合の個別 case/API まで canonical と認定していない。

| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint | callers (prod / test) | classification | canonical owner | remaining bridge callers / notes |
|---|---|---|---|---|---|---|---|
| E-1 | `QPDFJob::run`（`createQPDF` → `writeQPDF` の 2 段） | `libqpdf/QPDFJob.cc:513-520`, `include/qpdf/QPDFJob.hh:371-373` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::run`（pub、`:2950`） | prod: `run` leaf tracker 17（qpdf-job以外の同名 `run` を含む） / test: 231（同じ leaf 分母） | canonical | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::run` | `run` は通常経路で `create_qpdf` の返却文書を `write_qpdf` に渡し、完了後に `get_exit_code` を読む。暗号 status query だけは qpdf の `createQPDF` 前段の認証例外を保つため専用 status openへ早期分岐する。旧 `run_document_erased` / `run_document_stages` は撤去済み。 |
| E-2 | `QPDFJob::createQPDF`（`checkConfiguration` → `processFile` → `updateFromJSON` → `handlePageSpecs` → `handleRotations` → `handleUnderOverlay` → `handleTransformations`） | `libqpdf/QPDFJob.cc:428-481` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::create_qpdf`（pub、`:2701`） + private `prepare_document`（`:2584`） | prod: `create_qpdf` leaf tracker 3 / test: 13 | canonical | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::create_qpdf` | `.48.5` で update-JSON、single/multi-source page selection、rotation、under/overlay、全 transformation を create stage に統合した。multi-source の erased target と provider-backed source owner もこの境界で確定し、直接 create/write の間に返却文書を観測・変更できる。 |
| E-3 | `QPDFJob::writeQPDF` の 3 分岐（`!createsOutput()`→`doInspection` / `split_pages`→`doSplitPages` / else→`writeOutfile`） | `libqpdf/QPDFJob.cc:483-511`, `libqpdf/QPDFJob.cc:528-532` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_qpdf`（pub、`:2769`） | prod: `write_qpdf` leaf tracker 2 / test: 12 | canonical | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_qpdf` | `write_qpdf` 内で `creates_output`（output/replace-input、JSON の暗黙 stdout を含む）を先に判定し、inspection・split・JSON/ordinary outputを選択する。`write_qpdf` は `Result<()>`、終了 status は副作用のない `get_exit_code` で読む。 |
| E-4 | `QPDFJob::writeOutfile`（`replace_input` 前後処理、`json_version` 分岐、`QPDFWriter` ブロックスコープ、`setWriterOptions` → `write()`） | `libqpdf/QPDFJob.cc:3029-3091` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_qpdf`（pub）+ `crates/flpdf-cli/src/main.rs::write_with_pdf_writer`（private） | `write_with_pdf_writer` prod: 8 (flpdf-cli/src/main.rs) / test: 0；`write_qpdf_to_memory` prod: 2 (flpdf-cli/src/main.rs) / test: 0 | mixed | absent | **本領域で最大の二重正本**。CLI の通常出力は `QPDFJob` を通らず `main.rs` 自身の `PdfWriter` 経路（`crates/flpdf-cli/src/main.rs:314-331` / `crates/flpdf-cli/src/main.rs:334-345`）を辿る。呼び出し 10 箇所: `crates/flpdf-cli/src/main.rs:4310,4479,5806,5831,6021,6040,6204,7587,7641,7761`。**2026-09-07 更新（`flpdf-3yn9.48.4`）**: `replace_input` の rename/backup は `QPDFJob::write_qpdf` 自身の成功パスへ移した（`crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_qpdf`、`finish_replace_input` 呼び出し）。qpdf の `writeOutfile` が到達する全 caller に対して rename を行う（`libqpdf/QPDFJob.cc:3057-3086`）のと同じく、`create_qpdf()` → `write_qpdf()` の public 2 段契約を直接使う consumer（`flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs` 等）でも rename が完了するようになった。`run()` 側の重複呼び出し（旧 `finish_replace_input`/`remove_replace_input_temp`）は削除済み。`remove_replace_input_temp`（write 失敗時の一時ファイル削除）は qpdf に対応物が無い独自の safety net だったため、削除して qpdf の「失敗時は一時ファイルを残す」実挙動に揃えた。`main.rs` の 10 箇所の直接書き出し経路（下記）はこの移行の対象外（`flpdf-3yn9.48.7`/`flpdf-3yn9.48.8`）。`write_qpdf_to_memory` は `crates/flpdf/src/writer.rs` の同名 `pub(crate)` 関数と別物（caller 数を数えるときの同名衝突に注意） |
| E-5 | `QPDFJob::doSplitPages` | `libqpdf/QPDFJob.cc:2939-3027` | `crates/flpdf/src/job/page_split.rs::QPDFJob::split_pages`（pub、`crates/flpdf/src/job/page_split.rs:135`） | prod: 2 (flpdf/src/job/lifecycle.rs:2492, flpdf-cli/src/main.rs:5926) / test: 14 | mixed | `crates/flpdf/src/job/page_split.rs::QPDFJob::split_pages` | 実装は 1 本だが到達経路が 2 本 — `write_qpdf` 内（qpdf と同じ位置）と CLI の `--split-pages` 直呼び。`pub` は根拠 2（`QPDFJob` 自身の public メソッド）で legitimate |
| E-6 | `QPDFJob::writeJSON` / `doJSON` と `doJSON*` セクション群 | `libqpdf/QPDFJob.cc:3093-3116`, `libqpdf/QPDFJob.cc:1544-1643`, `include/qpdf/QPDFJob.hh:551-565` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_json_with_version`（pub、`crates/flpdf/src/job/lifecycle.rs:3565`）と `crates/flpdf/src/job/lifecycle.rs::write_configured_json`（private、`crates/flpdf/src/job/lifecycle.rs:3077`） | `QPDFJob::write_json_with_version`（メソッド）prod: 5 (flpdf-cli/src/main.rs:3997,4017, flpdf/src/job/lifecycle.rs:3956,3962,4008) / test: 0 | mixed | `crates/flpdf/src/job/json.rs::write_json_with_version_with_logger`（`pub(crate)`） | 実処理は `pub(crate)` の 1 本に集約済みだが、その上に **呼ばれていない `pub` の層が 2 枚**残っている（E-24）。セクション builder（`build_*_section`）は既に `pub(crate)` 化済みで、`json_inspect.rs` の compatibility re-export も撤去済み（E-25） |
| E-7 | `QPDFJob::doInspection`（10 分岐を逐次実行し最後に 1 回だけ warning/完了） | `libqpdf/QPDFJob.cc:1645-1693` | `crates/flpdf/src/job/lifecycle.rs::run_configured_inspection`（private、`:3211`） | prod: 1 / test: 0 | mixed | `crates/flpdf/src/job/lifecycle.rs::run_configured_inspection` | 分岐順序は qpdf と 1:1で、report helperは完了せず `write_qpdf` が全分岐後に warning drain・summary・memory reportを1回だけ行う。CLIの個別 inspection public APIは別の standalone consumerとして残るため、領域全体の classification は mixed。 |
| E-8 | `QPDFJob::doCheck` | `libqpdf/QPDFJob.cc:744-803` | `crates/flpdf/src/job/check.rs::QPDFJob::check`（pub、`crates/flpdf/src/job/check.rs:143`） | prod: 3 (flpdf/src/job/lifecycle.rs, flpdf-cli/src/main.rs) / test: 16 | mixed | `crates/flpdf/src/job/check.rs::QPDFJob::check` | `pub` は根拠 2 かつ `lib.rs` 冒頭 doc に明記あり（根拠 3 も満たす）。到達経路が QPDFJob の write-stage dispatcher と CLI 直呼びの 2 本である点だけが mixed |
| E-9 | `QPDFJob::doListAttachments` / `doShowAttachment` / `addAttachments` / `copyAttachments` | `libqpdf/QPDFJob.cc:876-911`, `include/qpdf/QPDFJob.hh:531-532,540-541` | `crates/flpdf/src/job/attachments.rs::QPDFJob::list_attachments`（pub、`crates/flpdf/src/job/attachments.rs:289`）ほか同 impl の 5 メソッド | `list_attachments` prod: 1 (`crates/flpdf-cli/src/main.rs:8043`) / test: 7 (`crates/flpdf/src/job/attachment_list.rs`, `crates/flpdf/src/job/attachments.rs`, `crates/flpdf/examples/pull_attachments.rs`) | mixed | `crates/flpdf/src/job/attachments.rs`（`QPDFJob` impl） | `QPDFJob` メソッド側は根拠 2 で legitimate。`.43` で test/example の free route `format_attachment_list` / `list_attachment_info` を削除し、テスト/example は `QPDFJob::list_attachments` に移行した。残る `format_attachment_list_with_sink` は production caller 1（`crates/flpdf/src/job/attachments.rs`）を持つ rendering sink、`AttachmentInfo` は別途 `flpdf-xsq1` で可視性を設計判断する public type として残る。 |
| E-10 | `QPDFJob::handlePageSpecs` | `libqpdf/QPDFJob.cc:2359-2633` | `crates/flpdf/src/job/page_specs.rs::QPDFJob::handle_page_specs` | CLI prod: 4 (`crates/flpdf-cli/src/main.rs:3592,6000,6165,6269`、2026-09-06 再測)。job 内からも到達 | mixed | `crates/flpdf/src/job/page_specs.rs::QPDFJob::handle_page_specs` | `flpdf-hxmj` は closed。single-source をこの job boundary に接続し、standalone collate/CombinedPlan 経路を撤去した限定 slice は完了済み。残る CLI の JSON page selection と page-operation 各経路は source cache、password、keep-files-open、変換と出力の orchestration を保持する。lifecycle の source orchestration は `prepare_document` に統合済みで、CLI 側の別 consumer 移行は後続 sliceに残る。2026-09-08（`flpdf-3yn9.48.8`）: `QPDFJobConfig::add_page_spec` を新設し JSON 経由と byte-identical であることを検証した（`config_add_page_spec_matches_the_json_configured_path_single_source`）。ただし `create_qpdf()`/`write_qpdf()` 自体は crate 全体で production caller 0（テストのみ）で、main.rs の 4 直接呼び出しはこの primitive を経由しない——`flpdf-3yn9.48.10`/`flpdf-q5ok` が attachment/argv で見つけたのと同じ欠落 Config surface（remove_restrictions/linearize/repair-mode）が理由で、CLI 配線は見送った |
| E-11 | `QPDFJob::handleUnderOverlay` / `doUnderOverlayForPage` | `libqpdf/QPDFJob.cc:1936-2043`, `libqpdf/QPDFJob.cc:1858-1911` | `crates/flpdf/src/job/overlay.rs::apply_overlay_specs`（pub free、`crates/flpdf/src/job/overlay.rs:550`）と `crates/flpdf/src/job/overlay.rs::overlay_verbose_report`（pub free、`crates/flpdf/src/job/overlay.rs:645`） | `apply_overlay_specs` prod: 3 (flpdf/src/job/lifecycle.rs:3137, flpdf-cli/src/main.rs:4463,5792) / test: 0；`overlay_verbose_report` prod: 2 (flpdf-cli/src/main.rs:4447,5776) / test: 0 | mixed | `crates/flpdf/src/job/overlay.rs::apply_overlay_specs` | CLI は `QPDFJob` を経由せず `flpdf::apply_overlay_specs` / `flpdf::overlay_verbose_report` を crate ルートから直接呼ぶ（8 (E) の debt、`flpdf-xsq1`）。命名も `handle_under_overlay` になっていない（`flpdf-ei0h`）。overlay source を開く処理は `lifecycle.rs:2765-2778` と CLI 側（`crates/flpdf-cli/src/main.rs:4872` 近傍）で別実装。2026-09-08（`flpdf-8uuw`）に通常 non-linearized rewrite の直接呼び出しは qpdfの create-stage 順序（overlay before transformations）へ切り替えたが、QPDFJob ownerへの完全統合自体は後続 consumer scope。 |
| E-12 | `QPDFJob::handleTransformations`（12 分岐: `remove_restrictions` → `externalize_inline_images` → `optimize_images` → `generate_appearances` → `flatten_annotations` → `coalesce_contents` → `flatten_rotation` → `remove_page_labels` → `page_label_specs` → `attachments_to_remove` → `attachments_to_add`→`addAttachments` → `attachments_to_copy`→`copyAttachments`） | `libqpdf/QPDFJob.cc:2137-2248`（12 分岐の分岐行は `libqpdf/QPDFJob.cc:2147,2151,2156,2178,2182,2185,2190,2196,2199,2230,2242,2245`、範囲としては `libqpdf/QPDFJob.cc:2147-2247`） | `crates/flpdf/src/job/lifecycle.rs::prepare_document_transformations`（private、`:3058`） | prod: 3 / test: 0 | mixed | `crates/flpdf/src/job/lifecycle.rs::prepare_document_transformations` | `.48.5` で qpdf と同じ変換順序を `create_qpdf` の内側へ移した。2026-09-08（`flpdf-8uuw`）で通常 non-linearized rewrite の retained direct routeも overlay → image → appearance → annotation → coalesce → rotation の順へ揃え、repository-owned inline-image probe と qpdf-zlib-compat byte comparisonを追加した。CLIのQPDFJob owner統合・linearized/page-operation別 route は後続 consumer scopeのため、領域全体は mixed。page label は `apply_page_label_transformations`、attachment は同じ preparation bodyに残し、qtest exceptions系は変更していない。 |
| E-13 | `QPDFJob::handleRotations` | `libqpdf/QPDFJob.cc:2635-2652` | `crates/flpdf/src/job/lifecycle.rs::apply_configured_rotations`（private）と `crates/flpdf/src/job/rotate.rs::apply_rotate_to_pages`（pub free） | mixed | mixed | absent | qpdfは同一ループで`parse_numrange(range,npages)`→`pageno-1`→`0 <= pageno < npages` filter→page applyを行う。flpdfのjob JSONとCLIは共有primitiveで同じ順序・empty document filterを実装したが、CLI適用ownerがJob外に残るため全体ownerはmixed。`PageRange::resolve`依存とempty guardは削除済み。 |
| E-14 | `QPDFJob::parseRotationParameter` | `libqpdf/QPDFJob.cc:368-415`, `include/qpdf/QPDFJob.hh:482` | `crates/flpdf/src/job/rotate_spec.rs::parse_rotation_parameter`（pub、`:35`）+ `RotationSpec` | prod: 3（range validation / job JSON / direct CLI） / test: 11 | mixed | `crates/flpdf/src/job/rotate_spec.rs::parse_rotation_parameter` | qpdfのprivate parserを共有Rust primitiveとして公開し、job JSONとCLI direct rotationの両経路が同じraw range・angle・relative契約を使う。rangeは全体文法を先に検査し、invalid parameterは`Error::Usage`でraw bytesを保持する。direct ConfigのNULは保持し、JSON consumerだけ`c_str()`境界で切る。旧`RotateSpec::parse`は削除した。ただしCLIの適用ownerは`QPDFJob::handleRotations`と別経路であり、owner closureは後続consumer移行で扱う。 |
| E-15 | `QUtil::parse_numrange`（`QPDFJob::parseNumrange` は例外処理を足した薄いラッパー） | `include/qpdf/QUtil.hh:464`, `libqpdf/QUtil.cc:1304-1438`, `libqpdf/QPDFJob.cc:399-425`, `libqpdf/QPDFJob_argv.cc:240-272`, `libqpdf/QPDFJob_config.cc:1055-1074` | `crates/flpdf/src/qutil.rs::parse_numrange`（pub、`:400`） | prod: rotation parser / lifecycle / CLI、test: qpdf contract vectors | mixed | `crates/flpdf/src/qutil.rs::parse_numrange` | signed `max`、max=0 syntax-only、raw bytes、NUL終端、group全体の文法先行検査、exclusion、position-based odd/even、QIntC narrowing/wrappingを共通primitiveへ移植した。rotation consumerで先行利用するが、`PageRange`の他consumerとowner closureは後続issueへ残す。 |
| E-16 | `QPDFJob::shouldRemoveUnreferencedResources` | `libqpdf/QPDFJob.cc:2250-2339`, `include/qpdf/QPDFJob.hh:515` | `crates/flpdf/src/job/resource_pruning.rs::should_remove_unreferenced_resources`（pub free） | prod: 3 (flpdf/src/job/page_merge.rs:854, flpdf/src/job/page_specs.rs:192, flpdf-cli/src/main.rs:5707) / test: 10 | mixed | `crates/flpdf/src/job/resource_pruning.rs::should_remove_unreferenced_resources` | 実装 1 本に対し呼び出し 3 経路。qpdf 側は `handlePageSpecs` からしか呼ばれない private メソッド。crate ルート `pub`（`crates/flpdf/src/lib.rs:190-193`）は 8 の (A)〜(E) 未記載の**新規 debt 候補** |
| E-17 | `QPDFJob::initializeFromArgv` / `initializeFromJson`（`QPDFArgParser` 経由の argv 解釈） | `include/qpdf/QPDFJob.hh:75-90`, `libqpdf/QPDFJob_argv.cc` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::initialize_from_argv`（pub、`crates/flpdf/src/job/lifecycle.rs:1781`） | prod: 3（すべて flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs:147,164,183）/ test: 23 (flpdf/tests/job_lifecycle_tests.rs) | mixed | absent | **flpdf-cli は `initialize_from_argv` を一度も呼ばない** — clap 定義（CLAUDE.md 逸脱分類 (B) の `QPDFArgParser` → clap）で独自に引数を解釈し、`QPDFJob` の setter を個別に叩く（`job.set_input_file` / `job.set_output_file` / `job.set_password` …）。qpdf の CLI は `initializeFromArgv` 1 本しか使わない（`qpdf/qpdf.cc:35`）。argv → 設定が CLI 側と library 側に分かれている。2026-09-06 確認: library initializer は限定的な手書きdispatchで `--rotate` などを未実装。CLIをそのまま接続できる canonical prerequisite は完成していない。2026-09-08（`flpdf-3yn9.48.6`）: 全 124 option の対応表を機械測定（本ファイル末尾「E-17/E-21 option correspondence table」）。`initialize_from_argv` は 11/124（9%）、`main.rs` 独自実装は 113/124（91%）で、想定と逆に `main.rs` の方が qpdf 文法（`@argfile`/`--` reset の edge case）まで含めて先行している。CLI 接続は本 issue では見送り。2026-09-08（`flpdf-q5ok`）: `initialize_from_argv` 自体に `@argfile` 展開（`expand_arg_files`、`lifecycle.rs:1789` の `--` 直後）と qpdf 準拠 `--` main-table reset（`lifecycle.rs:1800`）を実装し、想定していた前提の欠落を解消した。option 数自体（11/124）は変わらない — 未着手なのは個別 option の移植、CLI 接続の判断は依然保留 |
| E-18 | `QPDFJob::checkConfiguration` | `libqpdf/QPDFJob.cc:566-642`, `include/qpdf/QPDFJob.hh:129-130` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::check_configuration`（pub、`crates/flpdf/src/job/lifecycle.rs:3230`） | prod: 8 (flpdf/src/job/lifecycle.rs 4, flpdf-qtest-tools/src/driver/test_80_87.rs 4) / test: 0 | canonical | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::check_configuration` | qpdf と同じく `createQPDF` 冒頭（`crates/flpdf/src/job/lifecycle.rs:2383`）から呼ばれ、public としても露出。CLI は使わない（E-17 の帰結）が、それは「別の正本がある」のではなく「CLI が job 設定を組み立てない」ため |
| E-19 | `QPDFJob::getExitCode` / `hasWarnings` / `createsOutput` | `libqpdf/QPDFJob.cc:522-564` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::get_exit_code`（pub、`:4032`） + `crates/flpdf/src/job/lifecycle.rs::QPDFJob::complete`（pub、`:4062`） + `has_warnings`（pub）; document warning API は `crates/flpdf/src/reader.rs::Pdf::get_warnings` / `any_warnings` / `num_warnings` | `get_exit_code` leaf tracker prod: 17 / test: 15; `complete` prod: 20 / test: 16; `has_warnings` prod: 12 / test: 7 | mixed | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::get_exit_code` + `drain_document_warnings` | `get_exit_code` は logger/write/drain を行わない純粋な query。`write_qpdf` が `get_warnings` 相当の document drain、warning summary、memory reportを1回の enclosing completionとして実行し、JSON/check/linearizationの既存standalone public APIはその後 `get_exit_code`を返す。CLI direct completionは残るが、旧 `complete` がstatusを兼ねる経路は撤去済み。 |
| E-20 | `QPDFJob::getLogger` / `setLogger` / `setMessagePrefix` / `getMessagePrefix` / `registerProgressReporter` | `libqpdf/QPDFJob.cc:302-337`, `include/qpdf/QPDFJob.hh:92-123` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::logger`（pub、`crates/flpdf/src/job/lifecycle.rs:1308`）ほか 4 メソッド | `set_message_prefix` prod: 27 (flpdf/src/job/lifecycle.rs, flpdf-cli/src/main.rs, flpdf-qtest-tools) / test: 6；`QPDFJob::register_progress_reporter` prod: 3 (flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs:143,178, flpdf-qtest-tools/src/driver/test_80_87.rs:336) / test: 2 (flpdf/tests/job_lifecycle_tests.rs:302,1591) | canonical | `crates/flpdf/src/job/lifecycle.rs`（`QPDFJob` の logger/prefix impl） | `logger()` / `message_prefix()` の `get_` 省略は 7 の bare getter 例外に該当し正しい。CLI が `QPDFJob::new` を 25 回作って毎回 logger と prefix を設定し直しているのは E-17 / E-4 の帰結（job インスタンスが lifecycle を持たない）。`crates/flpdf/src/writer.rs:695` の `PdfWriter::register_progress_reporter` は同名の別シンボルで、`crates/flpdf/src/job/lifecycle.rs:1512`（`configure_writer_progress` 内）と `crates/flpdf/tests/linearize_objstm_generate_tests.rs:1403` はそちらの caller — 上の数から除外している |
| E-21 | CLI 実行ファイル consumer（`QPDFJob` public のみ 4 手） | `qpdf/qpdf.cc:26-44` | `crates/flpdf-cli/src/main.rs::main`（bin crate） | prod: 1 bin（`crates/flpdf-cli`、9313 行）/ test: 66 統合テストファイル（`crates/flpdf-cli/tests/*.rs`） | mixed | absent | qpdf の CLI は `initializeFromArgv` → `run` → `getExitCode` の 3 呼び出し（62 行）。flpdf-cli は 9313 行で、`QPDFJob::new` を 25 回作り、`run()` は 1 箇所（`--job-json-file`）でしか呼ばない。E-4 / E-12 / E-17 が示すとおり `writeOutfile` / `handleTransformations` / argv 解釈を自前で持つ。単純な可視性変更では閉じられず、canonical Job の作成・変換・出力・argv 責務の完成と consumer ごとの移行が前提（`flpdf-hxmj` は single-source 接続を完了して closed）2026-09-08（`flpdf-3yn9.48.10`）: `QPDFJobConfig::add_attachment`/`remove_attachment`/`copy_attachments_from` を新設し、JSON 経由と byte-identical であることを検証した（`crates/flpdf/tests/job_lifecycle_tests.rs::config_{add,remove}_attachment_matches_the_json_configured_path`/`config_copy_attachments_from_matches_the_json_configured_path`）。ただし `main.rs::run_add_attachment`/`run_remove_attachment`/`run_copy_attachments_from` の接続は見送った——`remove_restrictions`/linearize（`linearize_pass1` 含む）/repair-mode open option が`QPDFJobConfig` に未実装で、接続すると CLI の既存機能が退行するため。password/suppress_warnings は `QPDFJob::set_password`/`set_suppress_warnings` で既に直接設定可能。 |
| E-22 | `QPDFJob` の C API consumer（pure pass-through） | `libqpdf/qpdfjob-c.cc:19-161`, `qpdf/qpdfjob-ctest.c`（142 行、機械検証対象外の `.c`） | `crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs::main`（bin crate、281 行） | prod: 1 bin / test: qtest ハーネス（`crates/flpdf-qtest-tools/src/orchestrator.rs`）経由 | canonical | `crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs` | `QPDFJob` の public surface（`new` / `register_progress_reporter` / `initialize_from_argv` / `initialize_from_json` / `create_qpdf` / `write_qpdf` / `run` / `set_logger` / `set_message_prefix`）しか触らず、qpdf の C wrapper 構造を正しく踏襲している唯一の consumer。ただし `report_job_error`（`crates/flpdf/src/job/lifecycle.rs:3369`、pub）は qpdf 側では C wrapper 内の `wrap_qpdfjob`（`libqpdf/qpdfjob-c.cc:32-41`）に相当し、`QPDFJob` の public メソッドではない — 位置が違う |
| E-23 | `QPDF` / `QPDFWriter` の C API consumer（`QPDFJob` を経由しない） | `libqpdf/qpdf-c.cc:24-66`, `libqpdf/qpdf-c.cc:459-521`, `libqpdf/qpdf-c.cc:1924-1952` | `crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs::main`（bin crate、523 行） | prod: 1 bin / test: qtest ハーネス（`crates/flpdf-qtest-tools/src/orchestrator.rs`）経由 | canonical | `crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs` | `qpdf_check_pdf` は `QPDFJob::doCheck` ではなく `QPDFWriter` + `Pl_Discard` + `setDecodeLevel(qpdf_dl_all)` で `write()` する（`libqpdf/qpdf-c.cc:58-66`）— flpdf 側でも E-8 の `QPDFJob::check` とは別責務として扱う必要がある |
| E-24 | `QPDFJob::writeJSON` の library 入口（qpdf 側に対応する public 識別子なし） | `libqpdf/QPDFJob.cc:3093-3116`（private） | `crates/flpdf/src/job/json.rs::write_json`（pub、`crates/flpdf/src/job/json.rs:588`）と `crates/flpdf/src/job/json.rs::write_json_with_version`（pub、`crates/flpdf/src/job/json.rs:602`） | `write_json`（free）prod: 0 / test: 7 (`crates/flpdf/tests/job_json_tests.rs:35,63,82,105,131,153,162`)；`write_json_with_version`（free）prod: 1（`crates/flpdf/src/job/json.rs:593` — free `write_json` からの委譲のみ）/ test: 0 | bridge | `crates/flpdf/src/job/json.rs::write_json_with_version_with_logger`（`pub(crate)`） | 8 (A) が「唯一の呼び出し元は `QPDFJob::write_json` メソッド」と記録していた状態から進行しており、現在は **`QPDFJob::write_json_with_version` が `write_json_with_version_with_logger` を直接呼ぶ**（`crates/flpdf/src/job/lifecycle.rs:3580`）ため、free `pub` 2 本には production の caller が 1 つも残っていない。ただし **削除も `pub(crate)` 化もできない** — `crates/flpdf/tests/job_json_tests.rs:1` が `use flpdf::job::{write_json, …}` で free 関数を import し 7 箇所から呼んでおり、`tests/*.rs` は別コンパイル単位なので `pub` が要る。free `write_json_with_version` の唯一の caller はその free `write_json`（`crates/flpdf/src/job/json.rs:593`）。`crates/flpdf-cli/src/main.rs:3997,4017` や `crates/flpdf/src/job/lifecycle.rs:3956,3962,4008` の `write_json_with_version` は同名の `QPDFJob` **メソッド**で別物。8 (A)/(B) と同じ構図（free `pub` + 統合テスト caller + `QPDFJob` メソッドによる正当化なし）で、狭めるにはテスト側の書き換えが要る → `flpdf-xsq1`。`lib.rs` の crate ルート再輸出も無く、`flpdf::job::` からのみ到達可能 |
| E-25 | `doJSON*` セクション builder の historical public path | `include/qpdf/QPDFJob.hh:551-565`（すべて private） | `crates/flpdf/src/job/json_sections.rs::build_pages_section_with_options`（`pub(crate)`、`crates/flpdf/src/job/json_sections.rs:189`）ほか | prod: 自クレート内のみ / test: — | canonical | `crates/flpdf/src/job/json_sections.rs` | 8 (C)（`flpdf-7bkv`）の staged migration は **完了済み** — `crates/flpdf/src/job/mod.rs` の `pub use json_sections::{build_*_section, ...}` は存在せず、`crates/flpdf/src/json_inspect.rs` の compatibility re-export ブロックも撤去され、**素の 6 個**（`build_pages_section` / `build_outlines_section` / `build_pagelabels_section` / `build_acroform_section` / `build_encrypt_section` / `build_attachments_section`）は `rg -n 'fn build_(pages\|outlines\|pagelabels\|acroform\|encrypt\|attachments)_section\b' crates/flpdf/src` が 0 件で宣言自体が存在せず、残るのは `_with_options` / `_with_version` 付きの `pub(crate)` 版のみ。`write_qpdf_json_v2_selected_objects_with_options` も workspace 全体で 0 件。8 (C) が close の blocker として挙げた `crates/flpdf/tests/document_json_tests.rs` が import するのは `flpdf::document_json::write_json`、`flpdf::json_inspect::{DecodeLevel, JsonKey, JsonOutputError, StreamDataMode}`、`flpdf::job::{JsonJobOptions, JsonJobOutput, JsonStreamData, QPDFJob}`、`flpdf::pipeline::PlString`、`flpdf::Pdf` で、(C) の section builder は 1 つも含まれない — compat 経路を通らない。`flpdf-7bkv` は closed（2026-09-06 readback） |
| E-26 | `QPDFJob::handlePageSpecs` 内の AcroForm 刈り込み（qpdf 側に個別識別子なし） | `libqpdf/QPDFJob.cc:2610-2632` | `crates/flpdf/src/job/acroform_field_prune.rs::prune_acroform_after_subset`（pub free）+ `crates/flpdf/src/job/page_specs.rs::QPDFJob::prune_acroform_after_subset`（pub メソッド、`crates/flpdf/src/job/page_specs.rs:765`） | free 関数 prod: 2（いずれも `crates/flpdf/src/job/page_specs.rs:769,795` の同一クレート内呼び出し）/ test: 19 | bridge | `crates/flpdf/src/job/page_specs.rs::QPDFJob::prune_acroform_after_subset` | 8 (A) の記録どおり、free 関数側の `pub` は根拠 1〜3 のいずれも満たさない（`crates/flpdf/src/lib.rs:180-189` の crate ルート再輸出はあるが opening `//!` doc への明記が無い）。`pub(crate)` へ狭められる → `flpdf-xsq1` |
| E-27 | `QPDFObjectHandle::getParsedOffset` と stream DecodeParms type-warning の attribution（driver consumer） | `include/qpdf/QPDFObjectHandle.hh:419`, `libqpdf/QPDFObjectHandle.cc:1875-1882`, `qpdf/test_driver.cc` | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_get_parsed_offset`（`pub`） | prod: `try_get_parsed_offset`（resolve する canonical 版）は `flpdf-qtest-tools/src/driver/test_0_1.rs:298,355`・`driver/handle.rs:187`。bare `get_parsed_offset`（resolve しない convenience 版）は `flpdf-cli/src/main.rs:7242`・`flpdf-qtest-tools/src/metadata.rs:261,286` の 3 箇所が残る / test: 未再集計 | canonical | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_get_parsed_offset` | 単数形2本は `.42` で削除済み。2026-09-08（`.44`）: 旧 `qtest_object_value_source_offsets`/`qtest_array_item_source_offsets`/`qtest_decode_parms_source_offset`（と `Pdf::source_stream_data_offset`、A22 参照）を `driver/handle.rs::DecodeParamTypeWarning` の生成時点で `try_get_parsed_offset()` を直接キャプチャする形へ置換した——qpdf の `typeWarning`（`libqpdf/QPDFObjectHandle.cc:2168-2187`）自身が offending handle の `getParsedOffset()` を読むだけで、別経路での re-read を行わないのと同じ形。57件の golden fixture（`tests/driver_goldens.rs`）で byte-identical を確認。2026-09-08（`.25`）: prod caller 0 になっていた3 API・`source_stream_data_offset` 自体を撤去し、canonical entrypoint 1 本に統一した。ただし bare `get_parsed_offset`（解決しない convenience 版）の 3 caller は未移行。qpdf の `QPDFObjectHandle::getParsedOffset` は `if (dereference()) { return this->obj->getParsedOffset(); } else { return -1; }`（`libqpdf/QPDFObjectHandle.cc:1875-1881`）で解決を経るため、canonical に対応するのは `try_get_parsed_offset` 側であり、残る 3 caller はそちらへ寄せる対象。 |
| E-28 | `qpdf/test_driver.cc` consumer（`QPDF` / `QPDFObjectHandle` / helper の public API を呼ぶ 99 ケース） | `qpdf/test_driver.cc:3540-3562` | `crates/flpdf-qtest-tools/src/driver/mod.rs::run` と `driver/*.rs` | prod: 1 bin（`crates/flpdf-qtest-tools/src/bin/driver.rs`）/ test: `driver_cli.rs`, `driver_goldens.rs`, `xref_parsedoffset_cli.rs` | mixed | case/API ごとの領域 A〜D owner（下記詳細表参照） | **2026-09-07 P-2 完了**: 99 ケース全件（0/1 統合で 98 行）を imports・型経由メソッド・呼出し順序まで A〜D owner と照合し、`canonical` 24 / `mixed` 65 / `bridge` 9 / `unknown` 0 に分類した。詳細は「E-28 detail」表を参照。consumer 全体としては mixed 過半（test 0/1 の E-27 bridge を含む）のため引き続き `mixed`。新規発見の未追跡ギャップ 6 件を issue 化した（`flpdf-83jc`/`flpdf-wd2e`/`flpdf-jzj1`/`flpdf-6f6h`/`flpdf-cm84`/`flpdf-wkju`） |
| E-29 | `QPDFJob::createQPDF` の入力オープン側（`processFile` → `doProcess` → `doProcessOnce`。`QPDF` 構築直後に必ず `setQPDFOptions` を適用してから読む） | `libqpdf/QPDFJob.cc:428-481`, `libqpdf/QPDFJob.cc:1793-1804`, `libqpdf/QPDFJob.cc:1695-1716`, `libqpdf/QPDFJob.cc:650-666`（`noWarn` → `setSuppressWarnings` は `libqpdf/QPDFJob.cc:663-665`） | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::open_document_with_description`、`open_with_description`、`open_for_encryption_inspection_with_description`、`open_job_source`。CLI の通常入力と secondary source はこれらの job boundary または同じ `PdfOpenOptions` policy を使う。reopenable な page source は `crates/flpdf-cli/src/main.rs::open_page_source`、JSON input は `QPDFJob::create_from_json` を通る | 旧 `Pdf::open_with_options` / `Pdf::create_from_json` route: job boundary からは prod 0 / test 0 だが、direct `Pdf::open_with_options` は2つの意図的な exception route が残る（`python3 scripts/qpdf-route-callers.py --symbol open_with_options` は `crates/flpdf-cli/src/main.rs` に production caller 1 を報告する）。job の各 open boundary は job suppression を open 前に OR 済み。`open_page_source` は reopenable source のため direct `open_file_with_options` を残す。`run_copy_attachments_from` の attachment donor open も同じく direct `Pdf::open_with_options` を使う — qpdf の `copyAttachments`（`libqpdf/QPDFJob.cc:2100`）が donor を `processFile(other, ...)` で job 本体の main input slot と独立に開いており、donor を job 経由（`job.open_with_description`）で開くと `job.input_name()` が donor のパスで上書きされ、後続の duplicate-key エラー（`self.input_name()` を使用、qpdf の `pdf.getFilename()` @ `QPDFJob.cc:2127` に対応）が target ではなく donor を誤って名指すため。両 route とも同じ `suppress_warnings` option を明示適用する | mixed | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::open_with_description` | qpdf の `doProcessOnce` 境界に合わせ、ordinary open、overlay/underlay、copy-encryption、encryption probe、attachment copy、page source、JSON input の open-time warning delivery を `--no-warn` で抑止する。warning collection と completion/exit status は保持する。この noWarn 欠落自体は `flpdf-3yn9.47` で closed。残る donor open とCLI orchestrationの分離は別責務で、`flpdf-44hb` の donorごとの verbose→open→copy 順序は Job JSON経路にも該当する。 |


`flpdf-5nle` で attachment mutation の output boundary を更新した。`QPDFJob::handleTransformations` の
`addAttachments` / `removeEmbeddedFile` 相当の mutation 後、`run_add_attachment` と
`run_remove_attachment` は共通の `top_level_writer_options` を
`writer_configuration` → `write_with_pdf_writer` へ渡す。content normalization は mutation 後に
行い、linearization と `linearize_pass1` も同じ writer に渡す。これは qpdf の
`writeQPDF` → `writeOutfile` → `setWriterOptions` の順序
（`libqpdf/QPDFJob.cc:484-507,2137-2248,2847-2945,3029-3058`）に対応する。

`flpdf-w0ne` では remove route の diagnostics も同じ E-9/E-4 境界へ揃える。
`run_remove_attachment` は mutation 成功時の `removed attachment <key>` を verbose info sink
へ送り、writer 完了後に `wrote file <output>` を送り、missing key は qpdf の
`attachment <key> not found` を raw bytes のまま返す（`libqpdf/QPDFJob.cc:2230-2241,3030-3062`）。

### E-28 detail: `qpdf/test_driver.cc` case-level classification (P-2)

2026-09-07、`flpdf-3yn9.48.11` の一環として 99 ケース全件（case 0〜98。case 0/1 は共有関数
`test_0_1`/`run_test_0_1` のため 1 行に統合）を qpdf `qpdf/test_driver.cc` の実装と
`crates/flpdf-qtest-tools/src/driver/*.rs` の対応関数で突き合わせ、imports・型経由メソッド・
実際の呼出し順序を含めて分類した。各ケースが実際に触れる flpdf entrypoint を
`docs/qpdf-route-matrix/{a,b,c,d}-*.md`（および本ファイルの E 行）の既存分類と突き合わせ、
qtest が pass することだけを根拠に canonical と認定していない。

**分類の見方**: 「no A-D row」（A〜D の 4 領域は object-handle resolver / parser-recovery /
stream-pipeline-encryption / writer internals の 4 責務軸のみを対象とし、page/document
helper 層や content-stream parsing 層を最初から対象にしていない）はそれ自体は逸脱の証拠では
ない。そのケースの主要 entrypoint が同一 crate 内の他の production 呼び出し元（テスト専用や
qtest 専用ではない）と共有される、qpdf の同名 API を素直に写した canonical primitive である
ことを確認できた場合は `canonical` とし、その根拠を注記した。逆に A〜D いずれかの既存行が
`mixed`/`bridge` と判定している entrypoint に触れる場合はそのケースも `mixed`/`bridge` とした。
`bridge` と `mixed` の両方の entrypoint に触れるケース（case 26/27 が該当。A18 `mark_object_handle_dirty` と
D1 `writer.write()` の両方を呼ぶ）は、逸脱の重い側である `bridge` を採る。

| case | qpdf test fn | flpdf owner fn | classification | A-D/E owner refs / notes |
|---|---|---|---|---|
| 0/1 | `qpdf/test_driver.cc:201-286` | `crates/flpdf-qtest-tools/src/driver/test_0_1.rs::run_test_0_1` | bridge | 既知・追跡済み: E-27（`filters::decode_stream_data_recovering_with_limits` 経由の DecodeParms warning/source-offset 再構成、bridge）。`flpdf-3yn9.48.25` / `flpdf-3yn9.48.28` / `flpdf-3yn9.48.37` / `flpdf-3yn9.48.44` が cutover を追跡する。 |
| 2 | `qpdf/test_driver.cc:287-310` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_2` | mixed | A8（`get_key`/`dict_key` 経由、mixed。canonical owner は `try_get_key`）。stream 読み出しは `get_stream_data` — C4/C5 canonical。 |
| 3 | `qpdf/test_driver.cc:311-324` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_3` | mixed | A8 mixed。`pipe_stream_data` は C1/C3 canonical、`STREAM_ENCODE_NORMALIZE` は qpdf の `qpdf_ef_normalize` と一致。 |
| 4 | `qpdf/test_driver.cc:325-374` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_4` | mixed | A8（直接 `get_key`）mixed、A18（`mark_object_handle_dirty`）bridge（背景）。`PdfWriter::write` は D1 mixed（4 分岐 vs qpdf 2 分岐）。`make_direct`/配列 mutation 系 API に A-D 行なし（対象範囲外）。 |
| 5 | `qpdf/test_driver.cc:375-422` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_5` | canonical | `PageDocumentHelper::get_all_pages`/`PageObjectHelper::get_images`/`get_page_contents` — A-D 行なし（page/annotation helper 層は対象範囲外）が、case 26/39/94 と同型の production canonical primitive（page_merge/page_split 等広く共有）で既知の逸脱なし。 |
| 6 | `qpdf/test_driver.cc:423-441` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_6` | mixed | A8 mixed。`pipe_stream_data`（`DecodeLevel::None`）は C1/C3 canonical。doc comment が `get_stream_data` を使わない理由（`filtering_attempted=false` で throw）を明記。 |
| 7 | `qpdf/test_driver.cc:442-457` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_7` | mixed | A8 mixed、A18 bridge（背景）。`replace_stream_data` は C38 canonical。`PdfWriter::write` は D1 mixed。 |
| 8 | `qpdf/test_driver.cc:458-495` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_8` | mixed | A8 mixed、A18 bridge。`replace_stream_data_provider` は C38 canonical。`PdfWriter::write`（`set_linearization(true)`）は D1 mixed。 |
| 9 | `qpdf/test_driver.cc:496-521` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_9` | mixed | `dict_key`/`get_key` は使わず `pdf.resolve(&root)`（A7 背景）のみ。`get_stream_data`/`replace_stream_data` は C4/C5/C38 canonical。`new_stream_with_data`/`new_stream` に A-D 行なし。`PdfWriter::write` は D1 mixed。 |
| 10 | `qpdf/test_driver.cc:522-537` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_10` | mixed | `ObjectHandle::add_page_contents` に A-D 行なし（CLAUDE.md (B) の入れ物代替、doc comment 記載済み）。`PdfWriter::write` は D1 mixed。 |
| 11 | `qpdf/test_driver.cc:538-552` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_11` | mixed | 直接 `get_key`（A8 mixed）。`get_stream_data` は C4/C5 canonical。`get_raw_stream_data` は C 領域プロースで言及されるが独立行 ID なし。 |
| 12 | `qpdf/test_driver.cc:553-569` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_12` | mixed | `linearization::show_linearization_pdf_with_warnings`（`crates/flpdf/src/linearization/show.rs:1138`）は `QPDFJob::show_linearization`（`crates/flpdf/src/job/lifecycle.rs:3167-3182`）が呼ぶのと**同一の canonical primitive**（grep で確認済み、qtest 専用ラッパーではない）。qpdf 側は `doInspection` の `show_linearization` 分岐に対応 — E-7（`doInspection`、mixed）に従属するため `mixed`。 |
| 13 | `qpdf/test_driver.cc:570-591` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_13` | mixed | case 12 と同じ `show_linearization_pdf_with_warnings` core。E-7 に従属し `mixed`。 |
| 14 | `qpdf/test_driver.cc:592-660` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_14` | mixed | A17 が本ファイル（`crates/flpdf-qtest-tools/src/driver/test_10_17.rs:295,323`）を `Pdf::swap_objects` の production caller として明記済み、`mixed`（qpdf に無い tombstone 掃除分岐あり）。 |
| 15 | `qpdf/test_driver.cc:661-744` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_15` | mixed | `PageDocumentHelper::get_all_pages`/`add_page`/`add_page_at` に A-D 行なし（owned-snapshot vs qpdf live-vector、CLAUDE.md (B)、doc comment 記載済み）。`PdfWriter::write` は D1 mixed。 |
| 16 | `qpdf/test_driver.cc:745-776` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_16` | mixed | `flpdf-83jc` で全件移植済み。qpdf の `getAllPages()` は自身の cache への live reference を返すため同じ binding を読み直すが、`PageDocumentHelper::get_all_pages` は owned snapshot を返す（case 15/18 と同じ CLAUDE.md (B) 逸脱）ので、`Pdf::update_all_pages_cache`（`pub`、`crates/flpdf/src/pdf.rs:369`）の後に再取得して同じ refreshed 状態を観測する。これで後半の 3 assert と `a.pdf` 書き出しまで qpdf と同じ順序で通る。 |
| 17 | `qpdf/test_driver.cc:777-795` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_17` | mixed | 直接 `get_key`（A8 mixed）。`PageDocumentHelper::get_all_pages`/`remove_page` に A-D 行なし。`get_stream_data` は C4/C5 canonical。 |
| 18 | `qpdf/test_driver.cc:796-817` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_18` | mixed | `PageDocumentHelper` 呼び出しに A-D 行なし（owned-snapshot vs qpdf live-cache、CLAUDE.md (B)）。`PdfWriter::write` は D1 mixed。 |
| 19 | `qpdf/test_driver.cc:818-834` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_19` | mixed | 末尾の `/Contents` objgen 比較で直接 `get_key`（A8 mixed）。`PageDocumentHelper` に A-D 行なし。writer 呼び出しなし。 |
| 20 | `qpdf/test_driver.cc:835-851` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_20` | mixed | 直接 `get_key`（A8 mixed）。`shallow_copy`/`append_array_item` に A-D 行なし。`PdfWriter::write` は D1 mixed。 |
| 21 | `qpdf/test_driver.cc:852-862` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_21` | mixed | 直接 `get_key`（A8 mixed）、`pdf.resolve`（A7 背景）。stream 上の `shallow_copy` に A-D 行なし — qpdf が throw する期待エラー経路（`?` で移植）。writer 呼び出しなし。 |
| 22 | `qpdf/test_driver.cc:863-874` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_22` | canonical | `PageDocumentHelper::get_all_pages`/`remove_page` — A-D 行なし（対象範囲外）だが case 5/26 と同型の共有 production primitive、既知の逸脱なし。 |
| 23 | `qpdf/test_driver.cc:875-882` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_23` | canonical | case 22 と同じ `PageDocumentHelper` primitive のみ、既知の逸脱なし。 |
| 24 | `qpdf/test_driver.cc:883-947` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_24` | mixed | `new_reserved`/`replace_reserved`/`make_direct`/直接 `replace_key` に A-D 行なし（reserved-object lifecycle は対象範囲外）。`pdf.resolve`（A7 背景）。`PdfWriter::write` は D1 mixed。 |
| 25 | `qpdf/test_driver.cc:948-976` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_25` | mixed | 直接 `get_key`（A8 mixed）、`pdf.resolve`/`oldpdf.resolve`（A7 背景）。`copy_foreign_object` に A-D 行なし。`PdfWriter::write` は D1 mixed。 |
| 26 | `qpdf/test_driver.cc:977-1002` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_26` | bridge | `PageDocumentHelper::add_page`、`Pdf::copy_foreign_object`、`ObjectHandle::replace_key`、`PdfWriter` — A-D 行なし（対象範囲外）だが `merge_documents` 等が広く共有する production primitive。既知の逸脱なし。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`mark_object_handle_dirty`（A18 bridge） を呼ぶため `bridge`。 |
| 27 | `qpdf/test_driver.cc:1003-1078` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_27` | bridge | `replace_stream_data_with_callback` は C38 canonical。`copy_foreign_object`/`set_immediate_copy_from`/`PdfWriter` に A-D 行なし。provider-copy の入れ物代替は CLAUDE.md (B) として doc comment に記載済み。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`mark_object_handle_dirty`（A18 bridge） を呼ぶため `bridge`。 |
| 28 | `qpdf/test_driver.cc:1079-1096` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_28` | canonical | `copy_foreign_object` のエラー経路のみ、case 26/27 と同一 canonical primitive。 |
| 29 | `qpdf/test_driver.cc:1097-1147` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_29` | mixed | `copy_foreign_object`、`PdfWriter::write`、`ObjectHandle::replace_key` の所有権チェック。`Error::Internal` は `std::logic_error` 対応（`.claude/rules/qpdf-port-design-patterns.md` §3）と一致。既知の逸脱なし。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 |
| 30 | `qpdf/test_driver.cc:1148-1173` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_30` | mixed | `writer_copy_encryption_source` は canonical boundary `crates/flpdf/src/reader.rs::Pdf::writer_copy_encryption_source` を再利用（doc comment 明記、独自 snapshot ではない）。`PdfWriter::copy_encryption_parameters` に A-D 行なし。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 |
| 31 | `qpdf/test_driver.cc:1174-1214` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_31` | bridge | `ObjectHandle::parse`/`parse_with_description`/`parse_with_context` — 対象範囲外だが crate 唯一の production parse entrypoint。`Error::Internal` vs `Error::Parse` は qpdf の `std::logic_error` vs `std::runtime_error` 分岐と一致。。分類規則により、A7（`bridge`）が owner の `Pdf::resolve`（`test_26_33.rs:530`） に触れるため `bridge`。 |
| 32 | `qpdf/test_driver.cc:1215-1236` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_32` | mixed | `PdfWriter`（`set_linearization`/`set_compress_streams`/`set_extra_header_text`）のみ、production API。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 |
| 33 | `qpdf/test_driver.cc:1237-1251` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_33` | mixed | `PdfWriter::set_output_pipeline` にカスタム `Pipeline` 実装。sink の入れ物代替のみで CLAUDE.md (B) として doc comment に記載済み、挙動変化なし。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 |
| 34 | `qpdf/test_driver.cc:1252-1265` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_34` | mixed | `root_handle`/`get_key` は canonical だが、**未追跡ギャップ、`flpdf-wd2e` で追跡開始**: `flpdf-wd2e` で `Pdf::get_version_as_pdf_version`（`QPDF::getVersionAsPDFVersion`、`libqpdf/QPDF.cc:2305-2320`）と `Pdf::get_extension_level`（`QPDF::getExtensionLevel`、`:2329-2345`）を移植し、driver-local の `version_prefix_major_minor` は削除した。`get_extension_level` は `adobe_extension_level` と `/Root` → `/Extensions` → `/ADBE` → `/ExtensionLevel` の walk を共有し、qpdf 同様 `isInteger()` を確認した上でのみ `try_get_int_value_as_int`（`object_handle.rs:3436`）を通すため、クランプ時の warning も非整数時の無警告も qpdf と一致する。driver-local `catalog_extension_level` は per-hop の診断を出すため残置。`PdfVersion` の `major`/`minor` が `u8` である点と、overflow を `None` に落としている点は qpdf（`int` + `QUtil::string_to_int` の `range_error`）と異なり、別 issue で追跡する。 |
| 35 | `qpdf/test_driver.cc:1266-1312` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_35` | canonical | `root_handle`/`resolved_key` の resolve/get_key chain、`get_stream_data(DecodeLevel::Generalized)` は C4/C5 canonical。`BTreeMap` 順序は qpdf の `std::map` 反復順と一致（忠実、逸脱でない）。 |
| 36 | `qpdf/test_driver.cc:1313-1340` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_36` | canonical | case 35 と同じ root/key chain、`get_raw_stream_data` は C6 canonical、`PlFlate` は canonical pipeline stage。qpdf の raw `Pl_Flate(a_inflate)` + `qpdf_dl_none` を正確に再現。 |
| 37 | `qpdf/test_driver.cc:1341-1350` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_37` | canonical | `PageDocumentHelper::get_all_pages`、`ObjectHandle::parse_page_contents` + `ObjectHandleParserCallbacks`。qpdf の `ParserCallbacks`/`terminateParsing`（`/Abort` → `ParseControl::Stop`）を inline-image 特殊ケース含め正確に再現。対象範囲外だが production API。 |
| 38 | `qpdf/test_driver.cc:1351-1360` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_38` | canonical | `root_handle`/`resolved_key`/配列反復/`unparse_resolved` — case 34/35 と同じ canonical primitive。 |
| 39 | `qpdf/test_driver.cc:1361-1377` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_39` | canonical | `PageDocumentHelper::get_all_pages`、`PageObjectHelper::get_resources(false)`（非 mutating 呼び出しの理由を doc comment が qpdf の `getImages` 実装 `libqpdf/QPDFPageObjectHelper.cc:318-383` を根拠に説明）、`ObjectHandle::is_image(true)`。 |
| 40 | `qpdf/test_driver.cc:1378-1391` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_40` | mixed | `PdfWriter`（`set_pclm`/`set_static_id`）のみ、production API。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 |
| 41 | `qpdf/test_driver.cc:1392-1406` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_41` | mixed | `PageDocumentHelper::get_all_pages`、`ObjectHandle::add_content_token_filter` + `TokenFilter` 実装（canonical trait）。合成トークンの raw byte 表現は `tokenizer.rs` の canonical escaping 規則で検証済み（捏造でない）。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 |
| 42 | `qpdf/test_driver.cc:1407-1551` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_42` | bridge | canonical `try_*` accessor family（`try_get_key`/`try_is_array`/`try_get_name` 等、全て `pub`）+ `Rectangle`/`Matrix`。case 2-9（`test_02_09.rs`）と異なりこのファイルは warning 発行アクセサに直接到達できる。。分類規則により、A7（`bridge`）が owner の `Pdf::resolve`（`test_42_49.rs:61-70` ほか計 7 箇所） に触れるため `bridge`。 |
| 43 | `qpdf/test_driver.cc:1552-1611` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_43` | canonical | `AcroFormDocumentHelper`/`FormFieldObjectHelper`/`AnnotationObjectHelper` — canonical production document-helper port。field loop 前に eager `analyze()` 相当を drain（qpdf のコンストラクタ時解析と一致、doc comment 明記）。 |
| 44 | `qpdf/test_driver.cc:1612-1631` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_44` | mixed | `AcroFormDocumentHelper::get_form_fields`、`FormFieldObjectHelper::set_value_string`/`field_type`/`fully_qualified_name`/`value_as_string`、`PdfWriter`（QDF/static-id/suppress-original-ids）。既知の逸脱なし。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 |
| 45 | `qpdf/test_driver.cc:1632-1645` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_45` | mixed | `PdfWriter::write` は canonical だが、既知・追跡済みギャップ: `Pdf::repair_diagnostics()` は open 時診断に限らず、writer の stream warning も各ハンドルの resolver 経由で同じ collection に入る（`writer.rs:6540-6592` の `pdf_writer_reprocesses_an_invalid_compression_level_without_filtering` が `write()` 後に両方の write 時 warning を確認している）。case 45 は `getWarnings()` が非空かを見るだけなので現行 API で移植可能。残るギャップは write 時 warning による `exit(3)` ゲートで、E-19 と `flpdf-3yn9.48.26` が追跡する。 |
| 46 | `qpdf/test_driver.cc:1646-1784` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_46` | bridge | `NumberTree`/`NumberTreeCursor`（`nntree.rs`）— `QPDFNumberTreeObjectHelper`/その iterator の直接 port（wrap 挙動・値エイリアシングまで一致、doc comment 明記）。。分類規則により、A7（`bridge`）が owner の `tree_string_value` ヘルパー経由の `Pdf::resolve`（`test_42_49.rs:20-`） に触れるため `bridge`。 |
| 47 | `qpdf/test_driver.cc:1785-1798` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_47` | canonical | `PageLabelDocumentHelper::get_labels_for_page_range` — pair 型の戻り値により qpdf の flat-vector 等価性チェックが tautological（逸脱ではない）。`checked_sub` は qpdf の unguarded だが等価な underflow 形状に一致。 |
| 48 | `qpdf/test_driver.cc:1799-1923` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_48` | bridge | `NameTree`/`NameTreeCursor` — `QPDFNameTreeObjectHelper`/その iterator の直接 port、case 46 と同じ wrap/エイリアシングパターン。。分類規則により、A7（`bridge`）が owner の `Pdf::resolve` に触れるため `bridge`。 |
| 49 | `qpdf/test_driver.cc:1924-1939` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_49` | canonical | `OutlineDocumentHelper::get_tree`/`OutlineItem::get_title`/`get_dest` — tree 構築の副作用順序（qpdf コンストラクタ時 `/Outlines` walk）を `get_tree` を page listing 前に呼ぶことで保存（`outline_object_helper.rs:266-285` 引用、doc comment 明記）。 |
| 50 | `qpdf/test_driver.cc:1940-1955` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_50` | mixed | A7（`pdf.resolve` x2）+ A18（`mark_object_handle_dirty`）。`merge_resources`/`get_resource_names`/`pdf_object_to_json` は A-D 行なしだが qpdf の `mergeResources`/`getResourceNames`/`getJSON`（`dereference_indirect=false` 契約）と doc 検証済みで 1:1。 |
| 51 | `qpdf/test_driver.cc:1956-1999` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_51` | mixed | A7（`resolve_and_drain` 反復）+ D1（`writer.write`）。ケース固有の未解決点（新規issue化はせず注記のみ）: `FormFieldObjectHelper::from_object_handle`（`pub`、`crates/flpdf/src/form_field_object_helper.rs:114`）が direct/null ハンドルを受けるため、qpdf が null/範囲外 `/Kids[1]` field で no-op になるケース（`QPDFObjectHandle::replaceKey` no-op）に忠実な経路は存在する。driver 側が `FormFieldObjectHelper::new` + `FIELD_MUST_BE_INDIRECT`（`test_50_55.rs:183,223,232,244`）に留まっているだけで、API 側の制約ではない（現行 fixture では顕在化しない）。 |
| 52 | `qpdf/test_driver.cc:2000-2024` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_52` | mixed | A7（`resolve_and_drain`）+ D1。`FormFieldObjectHelper::set_value` に A-D 行なし。新規ギャップなし。 |
| 53 | `qpdf/test_driver.cc:2025-2043` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_53` | mixed | A9（`Pdf::get_all_objects`、mixed）+ D1（`preserve_unreferenced_objects` 付き writer）。A7 の使用なし（`.resolve()` 呼び出しなし）。 |
| 54 | `qpdf/test_driver.cc:2044-2056` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_54` | canonical | `Pdf::version()`（対象範囲外の bare accessor）と writer 設定 API（`.write()` は呼ばない、D1 の対象外）。A7/A18/D1 いずれにも触れない。qpdf の `getFinalVersion` 二重呼び出し挙動を doc comment で明示的に再現。 |
| 55 | `qpdf/test_driver.cc:2057-2115` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_55` | mixed | A18（trailer 上の `mark_object_handle_dirty`）+ D1。`PageDocumentHelper::get_all_pages`/`PageObjectHelper::get_form_xobject_for_page`（`QPDFPageObjectHelper.cc:706-733` 引用、対象範囲外）。 |
| 56 | `qpdf/test_driver.cc:2116-2121` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_56`（`test_56_59_body` 委譲） | mixed | D1 のみ — 共有本体に明示的な `.resolve()`/`mark_object_handle_dirty` 呼び出しなし。ページコピー/配置は `PageObjectHelper`/`Pdf::copy_foreign_object`（対象範囲外）。 |
| 57 | `qpdf/test_driver.cc:2122-2127` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_57`（`test_56_59_body` 委譲） | mixed | 同上、D1 のみ。 |
| 58 | `qpdf/test_driver.cc:2128-2133` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_58`（`test_56_59_body` 委譲） | mixed | 同上、D1 のみ。 |
| 59 | `qpdf/test_driver.cc:2134-2139` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_59`（`test_56_59_body` 委譲） | mixed | 同上、D1 のみ。 |
| 60 | `qpdf/test_driver.cc:2140-2215` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_60` | mixed | A18（trailer 上の dirty x3）+ D1。`merge_resources`/`get_unique_resource_name`/`shallow_copy`/`make_resources_indirect`（対象範囲外、doc comment が「公開 canonical な resource/live-trailer 経路」と明記）。 |
| 61 | `qpdf/test_driver.cc:2216-2262` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_61` | canonical | `.resolve()`/writer なし。`Error` variant マッピング、`qutil::safe_fopen`/`int_to_string_base`/`to_utf8`、`ReadSeek::as_any` downcast、`Discard` pipeline（canonical `Pipeline` trait）、`NameTree::new`+`Drop`。qpdf の呼出し順序移植ではなく例外クラス境界の Rust-native 再現と doc comment が明記。 |
| 62 | `qpdf/test_driver.cc:2263-2289` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_62` | canonical | `.resolve()`/writer なし。`Pdf::trailer()`（memoized live handle）+ `try_get_int_value`/`try_get_uint_value`/`try_get_int_value_as_int`/`try_get_uint_value_as_uint`（A6 `try_*` canonical family）。`QPDFLogger::default_logger()` エラーキャプチャは正当な logger 機構。 |
| 63 | `qpdf/test_driver.cc:2290-2342` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_63` | mixed | D1 のみ。`EncryptParams::v5_r6`（C 領域、本パスでは個別再検証せず — doc が `interpretR3EncryptionParameters` との permission-bit 対応を論証済み）。 |
| 64 | `qpdf/test_driver.cc:2343-2348` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_64`（`test_64_67_body` 委譲） | mixed | D1 のみ — case 56-59 と同型、共有本体に明示的な resolve/dirty 呼び出しなし。 |
| 65 | `qpdf/test_driver.cc:2349-2354` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_65`（`test_64_67_body` 委譲） | mixed | 同上、D1 のみ。 |
| 66 | `qpdf/test_driver.cc:2355-2360` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_66`（`test_64_67_body` 委譲） | mixed | 同上、D1 のみ。 |
| 67 | `qpdf/test_driver.cc:2361-2366` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_67`（`test_64_67_body` 委譲） | mixed | 同上、D1 のみ。 |
| 68 | `qpdf/test_driver.cc:2367-2388` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_68` | mixed | A7（`dict_key`/`resolve_handle` x2）のみ、writer なし。主要 entrypoint `ObjectHandle::get_stream_data`（C5 canonical）/`get_raw_stream_data`（C6 canonical）自体は canonical。`Error::Unsupported`（unfilterable stream）は `QPDF_Stream.cc:344-360` に忠実。 |
| 69 | `qpdf/test_driver.cc:2389-2404` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_69` | mixed | D1 のみ（page 毎 writer loop）。`Pdf::set_immediate_copy_from`/`Pdf::empty`/`PageInput::foreign`/`PageDocumentHelper::add_page`（対象範囲外）。A7 の使用なし。 |
| 70 | `qpdf/test_driver.cc:2405-2416` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_70` | mixed | D1 のみ。`ObjectHandle::set_filter_on_write` は C39 canonical — これが無ければ canonical になるところ。 |
| 71 | `qpdf/test_driver.cc:2417-2459` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_71` | mixed | A7（`pdf.resolve` x4、明示）のみ、writer なし。`Pdf::get_object_handle`（A3 canonical）+ `PageObjectHelper::for_each_xobject`/`for_each_image`/`for_each_form_xobject`/`get_images`/`get_form_xobjects`（対象範囲外、`QPDFPageObjectHelper.cc:318-395` 引用）。qpdf の panic-parity（空 vector への `pages[0]` インデックス）も意図的に再現。 |
| 72 | `qpdf/test_driver.cc:2460-2488` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_72` | mixed | A7（`chase_key` x3）のみ、writer なし。`ObjectHandle::parse_as_contents`/`add_token_filter`/`get_stream_data(Specialized)`（page-content/token-filter 層、本パスでは個別未検証）。form-XObject 分岐を常に取る理由を doc comment が明記（fixture 保証、ハードコードではない）。 |
| 73 | `qpdf/test_driver.cc:2489-2502` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_73` | mixed | A7（`resolve_once`）+ A19（`Pdf::close_input_source`、canonical）、writer なし。uninitialized handle / closed-source read エラーを doc 検証・test assert 済みで逐語再現。 |
| 74 | `qpdf/test_driver.cc:2503-2547` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_74` | mixed | D1 のみ。`NumberTree`/`NameTree` insert/cursor API（対象範囲外）を `trailer_key_handle` 経由で使用、明示的な `.resolve()` 呼び出しなし。 |
| 75 | `qpdf/test_driver.cc:2548-2605` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_75` | mixed | A7（`chase_key`/`chase_array_item`/`resolve_once`、`/Kids`→`/Limits` 検査）+ D1。tree module は case 74 と同じ。 |
| 76 | `qpdf/test_driver.cc:2606-2650` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_76` | mixed | D1 のみ（`.resolve()` 呼び出しなし）。`FileSpec`/`EmbeddedFileStream`/`EmbeddedFileDocumentHelper`（C 領域隣接の `filespec_helper`、本パスでは個別未検証 — 次回 case 監査 slice での確認候補）。 |
| 77 | `qpdf/test_driver.cc:2651-2663` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_77` | mixed | D1 のみ。`EmbeddedFileDocumentHelper::remove_embedded_file`、case 76 と同じ留保。 |
| 78 | `qpdf/test_driver.cc:2664-2704` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_78` | mixed | D1 のみ。`replace_stream_data_with_callback`/`replace_stream_data_with_retry_callback`（C38 canonical）+ `pipe_stream_data`（C1/C3 canonical）— 概ね canonical。ケース固有の留保（新規issue化はせず注記のみ）: 他ケース（50/55/60/79）と異なり trailer mutation 後に `mark_object_handle_dirty` 呼び出しがない（`// cov:ignore` 付き）。 |
| 79 | `qpdf/test_driver.cc:2705-2760` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_79` | mixed | A7（`chase_key`）+ A18（`mark_object_handle_dirty`）+ D1。`copy_stream`（C36 canonical）、`replace_stream_data`（C38 canonical）、`get_stream_data`（C5 canonical）— stream mutation 面自体は canonical。 |
| 80 | `qpdf/test_driver.cc:2761-2807` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_80` | mixed | `PageDocumentHelper::get_all_pages`/`resolve`/`try_get_key`、`AcroFormDocumentHelper::transform_annotations`/`add_and_rename_form_fields`、`PageObjectHelper::copy_annotations_from`（対象範囲外）。`PdfWriter::write` は D1 mixed。 |
| 81 | `qpdf/test_driver.cc:2808-2819` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_81` | canonical | `ObjectHandle::try_get_int_value` → `Error::QpdfExc`/`QpdfErrorCode::Object`。A6/A8 は領域行としては mixed だが、本ケースの実使用は resolving `try_*` family で個別逸脱なし。 |
| 82 | `qpdf/test_driver.cc:2820-2863` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_82` | canonical | `try_is_name_and_equals`/`try_is_dictionary_of_type`/`try_is_stream_of_type`/`try_is_or_has_name`、全て resolving `try_*` family。case 81 と同じ A6/A8 の背景注記。 |
| 83 | `qpdf/test_driver.cc:2864-2884` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_83` | mixed | `QPDFJob::new().initialize_from_json_bytes` → E-17（`initializeFromArgv`/`initializeFromJson`、mixed。CLI はこの経路に未到達）。 |
| 84 | `qpdf/test_driver.cc:2885-2973` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_84` | mixed | `QPDFJob` の config/run/check_configuration/register_progress_reporter/set_output_streams → E-1（`run`、mixed）、E-18（`checkConfiguration`、canonical）、E-19（`getExitCode`/`hasWarnings`、mixed）、E-20（logger/progress、canonical）。少なくとも 2 つの mixed E 行に触れる。 |
| 85 | `qpdf/test_driver.cc:2974-3064` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_85` | mixed | ローカル `value_as_*` が plain `as_boolean`/`as_integer`/`as_real_literal`/`as_real`/`as_name`/`as_string`/`as_operator`/`as_inline_image`（全 `pub`）+ `pdf_string::utf8_value` をラップ。A6 の背景注記（non-resolving family、受信側が意図的に未解決のためこちらが正しい family だが行自体は mixed のまま）。 |
| 86 | `qpdf/test_driver.cc:3065-3085` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_86` | mixed | **未追跡ギャップ、`flpdf-jzj1` で追跡開始**: `flpdf-jzj1` で `qutil::utf8_to_pdf_doc` を移植し、qpdf の 2 引数版が返す representability の `bool` も `utf8_to_pdf_doc_checked`/`utf8_to_ascii_checked`（`include/qpdf/QUtil.hh:326,332` の第 2 オーバーロード相当）として公開したため、qpdf 自身の test_86 が持つ `assert(QUtil::utf8_to_ascii(...))` / `assert(!QUtil::utf8_to_pdf_doc(...))`（`test_driver.cc:3074-3077`）まで移植済み（driver 自身の doc comment に明記）。残りは `filespec_helper::encode_utf16be`/`pdf_string::utf8_value`/`pdf_string::new_unicode_string`（全 `pub`）。 |
| 87 | `qpdf/test_driver.cc:3086-3105` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_87` | mixed | mutation/unparse は canonical `ObjectHandle::parse`/`replace_key`/`unparse`（`pub`）+ `json_inspect::pdf_object_to_json`（`pub`）。getKeys の null-omission 確認は canonical `ObjectHandle::try_get_keys`（`pub`、`crates/flpdf/src/object_handle.rs:2820`、receiver 解決と lazily-null 省略を契約に含む）が直接使えるにもかかわらず、ローカル `direct_non_null_keys` ヘルパーで代替している（direct dict 限定 fixture でのみ十分、doc 明記済み — 可視性ギャップではなく driver 側の未移行）。 |
| 88 | `qpdf/test_driver.cc:3106-3162` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_88` | canonical | qpdf-11 の mutate/get family（`replace_key_and_get_new`/`_old`、`append_array_item_and_get_new`、`insert_array_item(_and_get_new)`、`erase_array_item(_and_get_old)`、`remove_key(_and_get_old)`）全て `pub`、ギャップなし。 |
| 89 | `qpdf/test_driver.cc:3163-3174` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_89`（`mod.rs::run_test_89_from_json` 経由で `Pdf::create_from_json_with_options`、`crates/flpdf/src/json/document.rs:93`、`pub`） | bridge | document-construction boundary は public でギャップなし。以降の mutation/warning 本体は case 88/93 と同じ canonical accessor 群（`trailer`/`root_handle`/`get_object_handle`/`resolve`/`replace_key`/`try_get_array_item`）。`Pdf::create_from_json` 自体には A-D/E 専用行が無い（E-29 は `QPDFJob` 側の別 wrapper、mixed — その分類をそのまま借用しない）。。分類規則により、A7（`bridge`）が owner の `Pdf::resolve`（`test_88_98.rs:202`） に触れるため `bridge`。 |
| 90 | `qpdf/test_driver.cc:3175-3187` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_90` | canonical | `Pdf::update_from_json`（`crates/flpdf/src/json/document.rs:129`、`pub`）、ギャップなし。以降は case 88/93 と同じ canonical accessor。 |
| 91 | `qpdf/test_driver.cc:3188-3195` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_91` | canonical | `document_json::write_json`（`pub`、qpdf の `(version, pipeline, decode_level, stream_data_mode, file_prefix, wanted_objects)` 引数順と一致）+ `StdoutPipeline`（`Pipeline` 実装）。CLAUDE.md (B) の入れ物代替と doc comment に明記済み、未解決 bridge ではない。 |
| 92 | `qpdf/test_driver.cc:3196-3244` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_92` | mixed | **未追跡ギャップ、`flpdf-6f6h` で追跡開始**: `ObjectHandle::owning_pdf_unique_id`（`pub`、`crates/flpdf/src/object_handle.rs:1590`）が所有文書 identity を公開しており、4 件の identity assertion は現行 API で移植可能（未移植なのは driver 側の都合）。destroyed handle 上の `unparse` throw 挙動も未移植（`ObjectHandle::unparse` は `Result` を返さず null-unparse へ fallback）。「所有者 drop 後の survive/die」自体は `is_destroyed`/`is_indirect`/`as_dictionary` で検証済み。 |
| 93 | `qpdf/test_driver.cc:3245-3271` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_93` | canonical | `trailer`、`get_key`、`root_handle`、`parse`、`is_same_object_as`、`replace_key`、`make_indirect_from_object_handle`、`is_indirect` — 全 `pub`、ギャップなし。 |
| 94 | `qpdf/test_driver.cc:3272-3373` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_94` | canonical | `PageObjectHelper::get_media_box`/`get_crop_box`/`get_bleed_box`/`get_trim_box`/`get_art_box` — `.claude/rules/qpdf-port-design-patterns.md` §7 が明記する確立済み `get_` prefix canonical 慣行。live-identity/copy-on-fallback 挙動も doc 検証済み。 |
| 95 | `qpdf/test_driver.cc:3374-3399` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_95` | canonical | ローカル `is_scalar` は `ObjectHandle::type_code()`（`pub`）をラップするのみ、ギャップなし。 |
| 96 | `qpdf/test_driver.cc:3400-3414` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_96` | canonical | `ObjectHandle::parse` + `pdf_string::unparse_binary`（`pub`）、ギャップなし。 |
| 97 | `qpdf/test_driver.cc:3415-3424` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_97` | mixed | `ObjectHandle::try_get_array_item`（`pub`、`crates/flpdf/src/object_handle.rs:3230`）が receiver dereference と範囲外 warning を内包しており、`run_test_89`（`test_88_98.rs:211`）も既にこれを使っている。`flpdf-cm84` の cutover で `run_test_97` も同 accessor へ移行済み。残り（`trailer_key_handle`/`resolve`/`shallow_copy`/`unparse`）は canonical。 |
| 98 | `qpdf/test_driver.cc:3425-3450` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_98` | bridge | **移植済み（`flpdf-wkju`）**: `ObjectHandle::write_json`/`get_json` を `pub` にし（qpdf 側も public — `include/qpdf/QPDFObjectHandle.hh:1198,1205`、`pub` 境界規則の根拠 1）、qpdf の 2 部構成をそのまま移した。前半は全 6 オブジェクトで `writeJSON` と `getJSON(...).write(...)` の等価性（qpdf 自身が `QPDFObjectHandle.hh:1200-1202` で保証すると明記）を確認し、後半は content stream の辞書を変更してから `get_stream_json`（`pub`、`object_handle.rs:6325`、C44 が facade として追跡）の encode 結果を qpdf の期待バイト列と比較する。fixture `tests/fixtures/qpdf-test98-minimal.pdf` は qpdf の `examples/qtest/npages/minimal.pdf` と byte 一致（763 B）。分類は `bridge` のまま — A7 が owner の `Pdf::resolve` を 2 箇所で呼ぶため。 |

**range 別サマリ**（98 行 = 99 ケース、0/1 統合）:

| range | canonical | mixed | bridge | unknown |
|---|---|---|---|---|
| 0/1, 2-25（25 行） | 3（5, 22, 23） | 21 | 1（0） | 0 |
| 26-49（24 行） | 9（28, 35, 36, 37, 38, 39, 43, 47, 49） | 9（29, 30, 32, 33, 34, 40, 41, 44, 45） | 6（26, 27, 31, 42, 46, 48） | 0 |
| 50-79（30 行） | 3（54, 61, 62） | 27 | 0 | 0 |
| 80-98（19 行） | 9（81, 82, 88, 90, 91, 93, 94, 95, 96） | 8（80, 83, 84, 85, 86, 87, 92, 97） | 2（89, 98） | 0 |
| **合計** | **24** | **65** | **9** | **0** |

全 98 行（99 ケース）で `classification` 列が `unknown` の行はゼロ。case 5/12/13/22/23 は初回監査で `unknown` 候補だったが、`show_linearization_pdf_with_warnings` が `QPDFJob::show_linearization` と同一 primitive であることを production コードの grep で確認した上で `mixed`（E-7 従属）に、`PageDocumentHelper` 系の 3 ケースは同型 canonical 前例（case 26/39/94）との一貫性を取って `canonical` に、それぞれ再分類した。

本監査で issue 化したギャップ 6 件（2026-09-07 の再検証で、うち 5 件は当初「API 不在」としていた前提が誤りで、実際には driver 側の未移行だと判明。各行を修正済み）: `flpdf-83jc`（case 16, updateAllPagesCache）、`flpdf-wd2e`（case 34, getExtensionLevel/getVersionAsPDFVersion）、`flpdf-jzj1`（case 86, utf8_to_ascii/utf8_to_pdf_doc）、`flpdf-6f6h`（case 92, getOwningQPDF）、`flpdf-cm84`（case 97, getArrayItem 単一 index）、`flpdf-wkju`（case 98, write_json/get_json/write_stream_json が pub(crate) 限定で全体が未実行 stub）。case 51（`FIELD_MUST_BE_INDIRECT`、現行 fixture 非顕在化）と case 78（trailer mutation 後の `mark_object_handle_dirty` 欠落、cov:ignore 済み）は新規 issue 化せず本表に注記のみ残した — いずれも fixture 上は無害で、実装cutoverの緊急性は無いと判断した。

2026-09-08（`flpdf-thb2`）: qpdfの `checkConfiguration` が JSON の暗黙stdout
出力先を先に `-` として確定する順序（`libqpdf/QPDFJob.cc:572-591`）を、
`QPDFJob::check_configuration` のsplit/stdout validationへ反映した。
`json=2`、`splitPages=1`、`outputFile`省略の組合せをqpdfと同じusage errorで
止めるRED/GREENテストを追加し、write stageへの誤到達を防ぐ。E-18の
canonical checkConfiguration責務だけを補正し、qtest exceptionsは対象外。

2026-09-08（`flpdf-kt4z`）: qpdf `writeOutfile` は成功write後、rename前に
`pdf.closeInputSource()` を行う（`libqpdf/QPDFJob.cc:3068-3086`）。qpdfの
`createQPDF` ではpage donorの `page_heap` がcreate stageのローカル寿命で
消える一方、flpdfはprovider-backed foreign streamのため
`page_source_documents` と `overlay_sources` をwrite境界まで保持する。
`QPDFJob::write_qpdf` のreplace-input境界でprimaryだけでなく両方の保持donorを
`Pdf::close_input_source()` し、multi-source page selectionとself-overlayの
RED/GREENテストで各resolver/controllerのclose状態を確認する。E-4の出力寿命
責務を補正し、`flpdf-pr6b` の広いP3追跡と重複するself-overlay/page-merge原因は
このP2 sliceで同じclose境界に統合した。CLI、qtest exceptions、rootは変更しない。

2026-09-08（`flpdf-waly`）: qpdfのclassic xref tableは`readTrailer`後に
optional `/XRefStm`を読み、xref stream objectのread warningを出してから
`processXRefStream`のrecoverable builder warningを出す
（`libqpdf/QPDF.cc:876-927,951-962,1038-1065`）。flpdfはhybrid builder診断を
`previous.loaded.repair_diagnostics`へ先に混ぜていたため、canonical live read warning
と順序が逆転した。classic parserからhybrid build診断を別sinkへ分離し、
`DeferredDiagnosticsGuard`のread診断を先にspliceするRED/GREEN fixtureで
`trailer → expected endobj → wrong size`を固定した。E-4/既存buy0のbounded修正で、
qtest exceptionsとrootは対象外。

### 分類別件数

| 分類 | 件数 | 行 |
|---|---|---|
| canonical | 5 | E-18, E-20, E-22, E-23, E-25 |
| bridge | 3 | E-24, E-26, E-27 |
| mixed | 21 | E-1, E-2, E-3, E-4, E-5, E-6, E-7, E-8, E-9, E-10, E-11, E-12, E-13, E-14, E-15, E-16, E-17, E-19, E-21, E-28, E-29 |
| unknown | 0 | —（E-28 の case 別詳細は「E-28 detail」表を参照。99 ケース全件を照合済みで case-level unknown は 0） |

（合計 29 行。canonical 5 + bridge 3 + mixed 21 + unknown 0 = 29。`E-18` は `mixed` に見えるが、CLI が使わないのは「別の正本がある」からではなく E-17 の帰結であるため `canonical`）

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
| `apply_rotate_to_pages` | `crates/flpdf/src/job/rotate.rs` | なし（prod 0、test 16） | E-13。Job/CLIは`PageObjectHelper::rotate_page`へ直接移行済み。旧public batch helperとtest callerはscope外で保持 |
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

| 群 | 記録された項目 | 2026-09-05 の実測 |
|---|---|---|
| (A) | `prune_acroform_after_subset` 系 3 個 + `write_json` | **性質は不変** — 4 個とも「free `pub` + 統合テストからの caller + `QPDFJob` メソッドによる正当化なし」のまま（E-26 / E-24）。変わったのは caller の所在だけで、`write_json` は `QPDFJob::write_json` メソッド経由の production caller を失い（現在は `write_json_with_version_with_logger` を直接呼ぶ）、残るのは `crates/flpdf/tests/job_json_tests.rs` の 7 箇所。`tests/*.rs` は別コンパイル単位なので `pub(crate)` 化にはテスト書き換えが要る。ルールが記した `job/json.rs:257` は現在 `crates/flpdf/src/job/json.rs:588` |
| (B) | `format_attachment_list_with_sink` / `AttachmentInfo` | 2 個が `crates/flpdf/src/job/mod.rs` に `pub use` されたまま。前者は QPDFJob の logger sink として prod caller 1、後者の可視性は `flpdf-xsq1` の設計判断に残る（E-9） |
| (C) | `build_*_section` 6 個 + `write_qpdf_json_v2_selected_objects*` 2 個 | **解消済み**（E-25）。`flpdf-7bkv` は closed（2026-09-06 readback） |
| (D) | qpdf rotation parser (`parse_rotation_parameter` / `RotationSpec`) | **E-14の限定slice完了** — job JSONとdirect CLIの両経路が同じraw range・angle・relative stateを使用するが、CLI適用ownerはmixedとして後続移行に残る |
| (E) | `overlay_verbose_report` / `apply_overlay_specs` / `collate` | `overlay_verbose_report` / `apply_overlay_specs` は debt のまま（E-11）。**`collate` は消滅** — `fn collate` は workspace に 0 件で、`page_collate.rs` というファイル自体が存在しない（`crates/flpdf/src/job/` の全 22 ファイルを `ls` で確認） |

### 2026-09-08 `.48.7` ordinary/rewrite Job cutover

`flpdf-3yn9.48.7` moved the ordinary/rewrite `run_rewrite_opened` consumer onto
`QPDFJob::apply_transformations` and `QPDFJob::write_qpdf`. The direct
`PdfWriter`, overlay, image, appearance, annotation, coalesce, rotation, and
page-label routes in that cohort are gone; remaining E-4/E-11/E-12 direct
callers belong to JSON/page-operation/inspection cohorts tracked separately.
The E-4/E-11/E-12/E-21 rows above retain their original matrix row identity and
are re-measured against this note during the next full route audit.

## unknown / probe

| ID | 決められないこと | 必要な source / probe |
|---|---|---|
| P-1 | E-2 の public 2段契約の観測 — create が返す時点の変換済み状態、consumer の追加変更、write の最終出力を確認する | `initialize_from_json` または対応する Config で同じ変換設定を構築し、`create_qpdf()` → 返却PDF観測/追加変更 → `write_qpdf()` を qpdf の同等 API/CLI と比較する。現 `initialize_from_argv` は `--rotate` 等を扱わないため、同 argv を渡す旧 probe は E-17 の未実装に阻まれる。変換の欠落は source で確認済みだが、出力差と lifetime/warning 契約を differential で確定する。semantic 非等価性と mixed route 分類は別軸 |
| P-2 | **解決済み（2026-09-07）**: E-28 の case/API owner 対応付け | `driver/*.rs` の 99 ケース全件を imports・型経由メソッド・実際の呼出し順序を含めて qpdf の同じ case と A〜D owner に対応付けた。結果は「E-28 detail」表（`canonical` 24 / `mixed` 65 / `bridge` 9 / `unknown` 0）。未追跡だった 6 件のギャップは新規 issue 化（`flpdf-83jc`/`flpdf-wd2e`/`flpdf-jzj1`/`flpdf-6f6h`/`flpdf-cm84`/`flpdf-wkju`）。実装cutover自体は各 issue の担当範囲で別途行う（本 issue の対象外） |
| P-3 | E-19 の exit code 等価性 — flpdf は `complete(creates_output)` を各ステージが呼ぶのに対し qpdf は `getExitCode()` で 1 回だけ判定する。複数の inspection フラグを同時に指定したとき warning 集計と exit code が一致するか | `qpdf --check --show-npages --show-xref warn.pdf; echo $?` と `flpdf` の同等呼び出しで exit code と stderr 行数を比較。`libqpdf/QPDFJob.cc:534-564` が判定を 1 回しか行わない点が根拠 |
| P-4 | E-21 の cutover 前提 — `flpdf-cli` の 10 箇所の直接書き出し（E-4）を `QPDFJob::write_qpdf` へ寄せたとき、`replace_input` の rename/backup（現在 `QPDFJob::run` 内、`crates/flpdf/src/job/lifecycle.rs:2546-2552`）がどこに属するか。qpdf では `writeOutfile` の内側（`libqpdf/QPDFJob.cc:3069-3091`） | `libqpdf/QPDFJob.cc:3029-3091` を再読し、`temp_out` のスコープと `pdf.closeInputSource()` の位置を flpdf の `finish_replace_input`（`crates/flpdf/src/job/lifecycle.rs:3170`）と 1:1 で突き合わせる |


## 2026-09-06 再監査の issue 対応

親 epic は `flpdf-3yn9.48`。下表は責務と実装 issue の対応であり、完了状態は `bd show <id>` で確認する。
各 issue の受入条件に qpdf 根拠、最初の consumer、残 caller と削除条件を記録した。

| 対象行 | Beads issue | 責務 / 移行 slice |
|---|---|---|
| `E-14` / `E-15` | `flpdf-3yn9.48.2` | QUtil::parse_numrange を正本化し rotation consumer を移行する |
| `E-1` / `E-2` / `E-10` / `E-11` / `E-12` / `E-13` / `E-29` | `flpdf-3yn9.48.3` | QPDFJob::createQPDF の変換完了・source lifetime・例外契約を移植する |
| `E-4` / `E-6` / `E-21` | `flpdf-3yn9.48.4` | QPDFJob::writeOutfile の出力寿命・JSON・replace-input を正本化する |
| `E-1` / `E-3` / `E-5` / `E-6` / `E-7` / `E-19` | `flpdf-3yn9.48.5` | QPDFJob::writeQPDF/doInspection/getExitCode と run 二段契約を統合する |
| `E-17` / `E-21` | `flpdf-3yn9.48.6` | QPDFJob argv→Config を qpdf の正本として完成させ通常CLI入口を接続する |
| `E-4` / `E-11` / `E-12` / `E-21` | `flpdf-3yn9.48.7` | CLI ordinary/rewrite 変換consumerを canonical QPDFJob に移行する |
| `E-4` / `E-5` / `E-10` / `E-11` / `E-13` / `E-14` / `E-16` / `E-21` / `E-29` | `flpdf-3yn9.48.8` | CLI pages/collate/rotate/split のsource・plan・output orchestrationをJobへ移行する |
| `E-6` / `E-7` / `E-8` / `E-21` | `flpdf-3yn9.48.9` | CLI combined inspection・JSON consumer を canonical writeQPDF に移行する |
| `E-9` / `E-21` / `E-29` | `flpdf-3yn9.48.10` | CLI attachment mutation入口を canonical Job lifecycle に移行する |
| `E-28` | `flpdf-3yn9.48.11` | qtest test_driver consumerをcase・API責務単位でcanonical ownerに対応付ける |
| `E-28` | `flpdf-83jc` | case 16: **解決済み** — `update_all_pages_cache` 後に `get_all_pages` を再取得して後半の 3 assert と書き出しを移植（owned snapshot 由来の差は再取得で吸収） |
| `E-28` | `flpdf-wd2e` | case 34: **解決済み** — `Pdf::get_version_as_pdf_version`/`Pdf::get_extension_level` を移植（qpdf 側も public）。残るのは `PdfVersion` の `u8` 幅と overflow の扱いで、別 issue |
| `E-28` | `flpdf-jzj1` | case 86: **解決済み** — `utf8_to_pdf_doc` を移植し、representability を返す `utf8_to_pdf_doc_checked`/`utf8_to_ascii_checked` で bool assertion も移植 |
| `E-28` | `flpdf-6f6h` | case 92: `owning_pdf_unique_id`（`pub`）が所有文書identityを公開しており同一性assertは移植可能。残るのは`unparse`のthrow挙動 |
| `E-28` | `flpdf-cm84` | case 97: **解決済み** — `ObjectHandle::try_get_array_item`（`pub`）へ移行済み |
| `E-28` | `flpdf-wkju` | case 98: **解決済み** — `write_json`/`get_json` を `pub` 化し test_98 を移植（qpdf 側も public）。`write_stream_json` は `get_stream_json` facade 経由で到達 |
| `E-27` | `flpdf-3yn9.48.25` | test0 cutover後にsource metadata再parse・window・64回retry budgetを撤去する |
| `E-19` | `flpdf-3yn9.48.26` | QPDF::getWarnings drain・anyWarningsを移植しJob完了consumerを移行する |
| `E-27` | `flpdf-3yn9.48.28` | getParsedOffsetをlazy dereference契約へ揃えcheck consumerを移行する |
| `E-27` | `flpdf-3yn9.48.37` | QPDF_Stream::filterable のwarningをparsed-offset付き正本経路に統一する |
| `E-27` | `flpdf-3yn9.48.44` | qtest test0/1をcanonical pipe/loggerへ移し手製stream診断を撤去する |
| `E-9` / `E-29` | `flpdf-44hb` | copyAttachments のdonor open・verbose・warning順 |
| `E-9` / `E-24` / `E-26` / `E-14` / `E-16` | `flpdf-xsq1` | 残る公開surfaceとtest consumerの段階整理 |
| `E-14` / `E-15` | `flpdf-ei0h` | qpdf命名対応（max=0先行検証の誤記は訂正） |

## E-17/E-21 option correspondence table（2026-09-08、`flpdf-3yn9.48.6` 監査）

`libqpdf/qpdf/auto_job_init.hh`（generate_auto_job が生成する qpdf の実 argv option table。
`libqpdf/QPDFJob_argv.cc` はこれを include するだけで option 一覧自体はここにある）から
機械的に抽出した qpdf 側の全 option 名（help/main/pages/encryption/40・128・256-bit
encryption/underlay-overlay/attachment/copy attachment/set page labels の 11 テーブル、
重複名込み 141 エントリ、ユニーク名で 124）を、(a) `crates/flpdf/src/job/lifecycle.rs::
initialize_from_argv` の現在の literal 分岐、(b) `crates/flpdf-cli/src/main.rs` の clap 定義
（`long = "..."` の明示指定 + 暗黙 kebab-case フィールド名の両方を機械検索）と突き合わせた。

**見出し数値**:
- qpdf option 総数（ユニーク名）: 124
- `initialize_from_argv` 対応済み: **11**（`check`/`decrypt`/`deterministic-id`/
  `keep-files-open`/`keep-files-open-threshold`/`object-streams`/`password`/`progress`/
  `remove-page-labels`/`set-page-labels`/`static-id`）
- `main.rs` 独自 clap parser 対応済み（明示+暗黙, ヒューリスティック検索）: **113**
- 上記 11 は全て `main.rs` 側にも独立実装がある（`initialize_from_argv` 側だけの
  option は 0 件）
- 手法上「未確認」（`main.rs` 側にヒューリスティックで見つからなかった）: 11 —
  `externalize-inline-images`/`force-R5`/`force-V4`/`job-json-help`/`json-help`/
  `modify-other`/`preserve-unreferenced-resources`/`replace-input`/
  `report-memory-usage`/`show-crypto`/`warning-exit-0`。**これは「未実装の証明」ではない**
  — 検索は完全な名前一致（明示 `long=` 文字列 or snake_case フィールド名の grep）のみで、
  `--replace-input` のように help 文言にしか現れず実装が別名フィールド／別メカニズム
  （PR #1678 `flpdf-kt4z` は `crates/flpdf` 側の `replace_input` Config API 自体を修正して
  おり、`main.rs` が同じ機能を別経路で提供している可能性が高い）を持つケースを
  取りこぼす。11 件は個別に手動確認が必要な残タスクとして記録するに留める。

**この監査が確定させた、本 issue の当初診断を訂正する発見**: 「`initialize_from_argv`
を完成させてから `main.rs` をそこへ繋ぐ」という issue の想定作業順序は、実態と逆転している。
`initialize_from_argv` は qpdf option の 9%（11/124）しか実装していない一方、`main.rs` は
qpdf option の実に 91%（113/124）を**独自の clap 実装で**既に持っており、しかも
`flpdf-wxec`/`flpdf-qqp5`（本 issue 着手時に再確認、いずれも CLOSED/merged — 下記参照）の
ように `main.rs` 側は qpdf の argv 文法（`--` reset、`@argfile` 展開後の sole-option 判定）の
細かい edge case まで実測・修正済みで、`initialize_from_argv` にはその文法（`@argfile`
展開、qpdf 準拠の `--` reset semantics）が一切無い。**`main.rs` を今
`initialize_from_argv` 経由へ繋ぎ変えると、`main.rs` が既に持つ 100+ option の
実装・qpdf 文法の edge case fix をすべて失う regression になる** —
これが「通常CLIエントリポイントを接続する」作業を本 issue で見送った理由（詳細は
`flpdf-3yn9.48.6` の bd notes）。

**既存 grammar issue の再確認結果**（着手時に再確認、issue 記載通り再利用検討）:
- `flpdf-wxec`（`@argfile` 内 `--version`/`--copyright` の sole-option 判定）: CLOSED、
  PR #1588 で `main.rs` 側に実装済み。2026-09-08（`flpdf-q5ok`）:
  `initialize_from_argv` にも同形の `@argfile` 展開（`expand_arg_files`）を実装した。
  ただし `--version`/`--copyright`/`--help` 自体は `initialize_from_argv` に未実装
  （sole-option 判定の対象が無い）ため、この issue の sole-option gate 固有の
  edge case はまだ再現していない。
- `flpdf-qqp5`（top-level `--` の qpdf 準拠 reset semantics）: CLOSED、PR #1605 で
  `main.rs` 側に実装済み。2026-09-08（`flpdf-q5ok`）: `initialize_from_argv` の
  `--` 処理も one-shot フラグから main-table reset（`lifecycle.rs:1800`、`--`
  の後ろのオプションも認識される）へ揃え、`main.rs` の
  `top_level_double_dash_resets_to_main_options_like_qpdf` と同じ形を
  `argv_top_level_double_dash_resumes_the_main_option_table` で検証した。
- `flpdf-glm2.1`（`--newline-before-endstream=never` の bare flag 化）: 依然 OPEN、
  `main.rs` 側の別の未解決 issue。本 issue のスコープ外。

**受け入れ基準の残り**: 2026-09-08（`flpdf-q5ok`）で `@argfile` 展開と qpdf 準拠 `--`
reset の文法基盤は実装済み。残る「raw argv bytes/nested `--`/parameter dispatch/
jobJsonFile/usage error を同じ Config へ」という統合は、個別 option（113 件、
`main.rs` の既存実装を `initialize_from_argv` 側へ retrofit する形）の移植が
主体で、依然として本 issue 単体では収まらない規模。CLI 接続（E-21）の判断は
その完了後に別途行う。
未接続の CLI cohort（`.48.7`〜`.48.10`）は元々の記載通り依存 PR で段階移行する。

`flpdf-42xx` は E-12 の image option parser boundary にある inspection conflict を
qpdf 11.9.0 と同じ受理 semanticsへ揃える。`--check` / `--show-*` と
`--optimize-images` / `--externalize-inline-images` は usage conflictにせず、
output無しの inspection routeへ進む。writer output を新たに作る変更ではない。

なお qpdf は inspection route でも変換自体は実行する（`createQPDF` の
`handleTransformations`、`QPDFJob.cc:474` が `writeQPDF` の `createsOutput()`
分岐 `:484-491` より前）。本 slice は受理境界のみを揃えたもので、image option を
inspection route へ配線するのは `flpdf-w2fk` の範囲である。

### option 別対応表

`yes` は該当箇所に実装ありと確認済み、空欄は未確認/未実装。`main.rs` 列の性質は上記の
ヒューリスティック検索の限界を参照。

| qpdf table | option | initialize_from_argv | main.rs (flpdf-cli) |
|---|---|---|---|
| help | copyright |  | yes |
| help | job-json-help |  |  |
| help | json-help |  |  |
| help | show-crypto |  |  |
| help | version |  | yes |
| main | add-attachment |  | yes |
| main | allow-weak-crypto |  | yes |
| main | check | yes | yes |
| main | check-linearization |  | yes |
| main | coalesce-contents |  | yes |
| main | collate |  | yes |
| main | compress-streams |  | yes |
| main | compression-level |  | yes |
| main | copy-attachments-from |  | yes |
| main | copy-encryption |  | yes |
| main | decode-level |  | yes |
| main | decrypt | yes | yes |
| main | deterministic-id | yes | yes |
| main | empty |  | yes |
| main | encrypt |  | yes |
| main | encryption-file-password |  | yes |
| main | externalize-inline-images |  |  |
| main | filtered-stream-data |  | yes |
| main | flatten-annotations |  | yes |
| main | flatten-rotation |  | yes |
| main | force-version |  | yes |
| main | generate-appearances |  | yes |
| main | ignore-xref-streams |  | yes |
| main | ii-min-bytes |  | yes |
| main | is-encrypted |  | yes |
| main | job-json-file |  | yes |
| main | json |  | yes |
| main | json-input |  | yes |
| main | json-key |  | yes |
| main | json-object |  | yes |
| main | json-output |  | yes |
| main | json-stream-data |  | yes |
| main | json-stream-prefix |  | yes |
| main | keep-files-open | yes | yes |
| main | keep-files-open-threshold | yes | yes |
| main | keep-inline-images |  | yes |
| main | linearize |  | yes |
| main | linearize-pass1 |  | yes |
| main | list-attachments |  | yes |
| main | min-version |  | yes |
| main | newline-before-endstream |  | yes |
| main | no-original-object-ids |  | yes |
| main | no-warn |  | yes |
| main | normalize-content |  | yes |
| main | object-streams | yes | yes |
| main | oi-min-area |  | yes |
| main | oi-min-height |  | yes |
| main | oi-min-width |  | yes |
| main | optimize-images |  | yes |
| main | overlay |  | yes |
| main | pages |  | yes |
| main | password | yes | yes |
| main | password-file |  | yes |
| main | password-is-hex-key |  | yes |
| main | password-mode |  | yes |
| main | preserve-unreferenced |  | yes |
| main | preserve-unreferenced-resources |  |  |
| main | progress | yes | yes |
| main | qdf |  | yes |
| main | raw-stream-data |  | yes |
| main | recompress-flate |  | yes |
| main | remove-attachment |  | yes |
| main | remove-page-labels | yes | yes |
| main | remove-restrictions |  | yes |
| main | remove-unreferenced-resources |  | yes |
| main | replace-input |  |  |
| main | report-memory-usage |  |  |
| main | requires-password |  | yes |
| main | rotate |  | yes |
| main | set-page-labels | yes | yes |
| main | show-attachment |  | yes |
| main | show-encryption |  | yes |
| main | show-encryption-key |  | yes |
| main | show-linearization |  | yes |
| main | show-npages |  | yes |
| main | show-object |  | yes |
| main | show-pages |  | yes |
| main | show-xref |  | yes |
| main | split-pages |  | yes |
| main | static-aes-iv |  | yes |
| main | static-id | yes | yes |
| main | stream-data |  | yes |
| main | suppress-password-recovery |  | yes |
| main | suppress-recovery |  | yes |
| main | test-json-schema |  | yes |
| main | underlay |  | yes |
| main | update-from-json |  | yes |
| main | verbose |  | yes |
| main | warning-exit-0 |  |  |
| main | with-images |  | yes |
| pages | file |  | yes |
| pages | password | yes | yes |
| pages | range |  | yes |
| encryption | bits |  | yes |
| encryption | owner-password |  | yes |
| encryption | user-password |  | yes |
| 40-bit encryption | annotate |  | yes |
| 40-bit encryption | extract |  | yes |
| 40-bit encryption | modify |  | yes |
| 40-bit encryption | print |  | yes |
| 128-bit encryption | accessibility |  | yes |
| 128-bit encryption | annotate |  | yes |
| 128-bit encryption | assemble |  | yes |
| 128-bit encryption | cleartext-metadata |  | yes |
| 128-bit encryption | extract |  | yes |
| 128-bit encryption | force-V4 |  |  |
| 128-bit encryption | form |  | yes |
| 128-bit encryption | modify |  | yes |
| 128-bit encryption | modify-other |  |  |
| 128-bit encryption | print |  | yes |
| 128-bit encryption | use-aes |  | yes |
| 256-bit encryption | accessibility |  | yes |
| 256-bit encryption | allow-insecure |  | yes |
| 256-bit encryption | annotate |  | yes |
| 256-bit encryption | assemble |  | yes |
| 256-bit encryption | cleartext-metadata |  | yes |
| 256-bit encryption | extract |  | yes |
| 256-bit encryption | force-R5 |  |  |
| 256-bit encryption | form |  | yes |
| 256-bit encryption | modify |  | yes |
| 256-bit encryption | modify-other |  |  |
| 256-bit encryption | print |  | yes |
| underlay/overlay | file |  | yes |
| underlay/overlay | from |  | yes |
| underlay/overlay | password | yes | yes |
| underlay/overlay | repeat |  | yes |
| underlay/overlay | to |  | yes |
| attachment | creationdate |  | yes |
| attachment | description |  | yes |
| attachment | filename |  | yes |
| attachment | key |  | yes |
| attachment | mimetype |  | yes |
| attachment | moddate |  | yes |
| attachment | replace |  | yes |
| copy attachment | password | yes | yes |
| copy attachment | prefix |  | yes |
