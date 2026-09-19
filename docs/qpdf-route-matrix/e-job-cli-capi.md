# E. QPDFJob / CLI / C API 相当の consumer・adaptor

対象: `QPDFJob` の public surface（`initializeFromArgv` / `run` / `createQPDF` / `writeQPDF` /
`hasWarnings` / `getExitCode` / `getLogger` / `setMessagePrefix` 等）と private orchestration
（`doInspection` / `doCheck` / `handlePageSpecs` / `handleUnderOverlay` / `doSplitPages` /
`doJSON*` 等）の境界、`qpdf/qpdf.cc`（CLI 実行ファイル）、`libqpdf/qpdfjob-c.cc`（`QPDFJob` の
C API）、`libqpdf/qpdf-c.cc`（`QPDF` / `QPDFWriter` の C API）。flpdf 側は
`job/lifecycle.rs`（`QPDFJob`）、`job/mod.rs` / `json_inspect.rs` / `lib.rs` の re-export、
`crates/flpdf-cli/src/main.rs` が `QPDFJob` public を経由せず直接触る crate 項目、
`crates/flpdf-qtest-tools`（qtest 用 consumer）。

**Writer decode の境界:** qpdf の `QPDFJob::Members::decode_level` は job-level の既定値として
generalized だが、`decode_level_set` は `Config::decodeLevel` まで false のままである
（`include/qpdf/QPDFJob.hh:635-637`、`libqpdf/QPDFJob_config.cc:717-729`）。したがって
`setWriterOptions` は明示設定時だけ writer に decode level を渡し
（`libqpdf/QPDFJob.cc:2865-2875`）、未指定時の writer は `QPDFWriter` の decode none・
preserve-encryption=true を使う（`include/qpdf/QPDFWriter.hh:630-642`）。
The E-12 historical summary below uses “Job default generalized decode” as shorthand for the
job-level state; it does not mean that an unset decode level is replayed into the writer.

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
   `QPDFExc` で `qpdf_e_password` の場合のみ、`check_is_encrypted` / `check_requires_password` を
   `show_encryption` より先に評価し、`encryption_status` を立てて `nullptr`、status query が無い場合だけ
   `show_encryption` が `showEncryption` を呼んで `nullptr`。それ以外は再 throw。
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
- 同名別シンボル（例: `PdfWriter::register_progress_reporter` と `QPDFJob::register_progress_reporter`）は宣言元を確認して分離し、行の注記で断る。

`.claude/rules/qpdf-port-design-patterns.md` 8 に記録された行番号は 2026-08-21 時点の測定値で
既に drift しているため、**issue ID だけを引用し行番号は再測定した**（`main.rs` は **2026-09-19 マージ後再測で 11,866 行**。旧記載の 9313 行は stale、
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
| E-4 | `QPDFJob::writeOutfile`（`replace_input` 前後処理、`json_version` 分岐、`QPDFWriter` ブロックスコープ、`setWriterOptions` → `write()`） | `libqpdf/QPDFJob.cc:3029-3091` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_qpdf`（pub） | `write_qpdf` は canonical owner（route caller script: prod 8 / test 29）；`write_qpdf_to_memory` prod: 0 / test-only core callers: 19 | canonical | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_qpdf` | **2026-09-17（`flpdf-3yn9.48.144`）**: rewrite page-extraction の ordinary branchから CLI private `write_with_pdf_writer` を撤去し、qpdf の通常 `writeQPDF` → `writeOutfile` 境界へ接続した。qdf の cutover（`flpdf-3yn9.48.143`）と合わせ、CLI production の直接 `PdfWriter` consumerは0。`write_qpdf_to_memory` は `#[cfg(test)]` の byte-neutral scaffoldingであり、公開APIでも production routeでもないため E-4 の canonical ownerを分岐させない。既存の replace-input/write-stage cutover（`flpdf-3yn9.48.4`）は保持する。|
| E-5 | `QPDFJob::doSplitPages` | `libqpdf/QPDFJob.cc:2939-3027` | `crates/flpdf/src/job/page_split.rs::QPDFJob::split_pages`（`pub(crate)`、`crates/flpdf/src/job/page_split.rs:135`） | prod: 1 (`crates/flpdf/src/job/lifecycle.rs::write_qpdf`) / test: 14 | canonical | `crates/flpdf/src/job/lifecycle.rs::write_qpdf` → `crates/flpdf/src/job/page_split.rs::QPDFJob::split_pages` | 2026-09-17（`flpdf-3yn9.48.142`）で rewrite の `--pages … --split-pages` 直呼びを撤去し、`write_qpdf` の `split_pages` 分岐へ統合。qpdf の `writeQPDF` → `doSplitPages` と同じ Job 内 dispatch、warning/completion/status、chunk writer 設定を使う。qpdf の `doSplitPages` は private なので `SplitPageOptions` と `QPDFJob::split_pages` も crate 内へ狭めた。2026-09-16（`flpdf-8q13h`）で attachment の add/remove/copy route も `configure_attachment_job` から既存 `Config::splitPages` へ接続し、qpdf の `handleTransformations` → `writeQPDF` → `doSplitPages`（`libqpdf/QPDFJob.cc:2138-2247,483-492`）と同じ numbered chunk dispatch を使う。|
| E-6 | `QPDFJob::writeJSON` / `doJSON` と `doJSON*` セクション群 | `libqpdf/QPDFJob.cc:3093-3116`, `libqpdf/QPDFJob.cc:1544-1643`, `include/qpdf/QPDFJob.hh:551-565` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_json_with_version`（pub）と `crates/flpdf/src/job/lifecycle.rs::write_configured_json`（private） | `QPDFJob::write_json_with_version`（メソッド）prod: 1（`QPDFJob::write_json` 自身からの内部委譲のみ）/ test: 8（`crates/flpdf/tests/job_lifecycle_tests.rs` 6、`job_json_tests.rs` 1、`document_json_tests.rs` 1） | canonical | `crates/flpdf/src/job/lifecycle.rs::write_configured_json` → `crates/flpdf/src/job/json.rs::write_json_with_version_with_logger`（`pub(crate)`） | JSON の実処理は public `QPDFJob` method から `pub(crate)` serializer へ 1 本化され、free `job/json.rs` writer 2 本は E-24 の `flpdf-xsq1` で撤去済み。セクション builder（`build_*_section`）も `pub(crate)` 化済みで、`json_inspect.rs` の compatibility re-export も撤去済み（E-25）。flpdf-cli の `--json` 経路は `QPDFJob::write_json_with_version` の直接呼び出しをやめ、qpdf と同じ `writeQPDF` → `writeOutfile` → `writeJSON`（`crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_qpdf` → `crates/flpdf/src/job/lifecycle.rs::write_configured_json`）を通る。**2026-09-19（`flpdf-3yn9.48.178`）**: 残っていた 2 つの直接 consumer を qpdf の C API 面との対応関係で整理した。qpdf C API test binary（`crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs` の JSON tests 46/47）は、対応する qpdf 側の `qpdf-ctest.c` ケースが叩く `qpdf_write_json`（`libqpdf/qpdf-c.cc:1924-1952`）が `QPDF::writeJSON` を直接呼び `QPDFJob` を一切経由しない（`rg -n 'QPDFJob' libqpdf/qpdf-c.cc` は 0 件）にもかかわらず `QPDFJob::write_json_with_version` を経由していたため、E-23/E-24 の owner `crates/flpdf/src/document_json.rs::write_json` の直接呼び出しへ付け替えた。出力バイトは変わらない——`qpdf_ctest_json_cases_42_through_47_match_qpdf`（`crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs`）が実機 `qpdf --json-output=2 ...` オラクルと突き合わせたまま green のまま。この付け替えにより `write_json_with_version` の生存する呼び出しは `write_json`（引数省略の便宜メソッド）自身の内部委譲 1 箇所のみになり、canonical owner（`write_configured_json`）を経由しない外部 bypass は消えた。残るのは `write_json`/`write_json_with_version` 自身の `pub` 可視性が `.claude/rules/qpdf-port-design-patterns.md` 8 の 3 根拠いずれも満たさない別軸の debt（flpdf-cli は呼ばない、qpdf 側 `QPDFJob::writeJSON` は private、`lib.rs` 冒頭 doc 未記載）で、production consumer が既に無いことは E-4 の `write_qpdf_to_memory` と同様 canonical owner の分岐条件にしない。可視性 debt は `flpdf-3yn9.48.182` で追跡する。以上により本行を `mixed` から `canonical` に変更する |
| E-7 | `QPDFJob::doInspection`（10 分岐を逐次実行し最後に 1 回だけ warning/完了） | `libqpdf/QPDFJob.cc:1645-1693` | `crates/flpdf/src/job/lifecycle.rs::run_configured_inspection`（private、`:4039`。**2026-09-19 再々訂正**: `:3211` / `:4005` / `:4017` はいずれも別のコードか別リビジョンの行。このブランチで実測した宣言行は `:4039`） | prod: 2（`write_qpdf` `lifecycle.rs:3376` と `inspect_configured` `:3721`。`mod tests` は `:5813` 以降なのでどちらも production。**merge で 2 度巻き戻っているので実測値を記録する**） / test: 0 | mixed | `crates/flpdf/src/job/lifecycle.rs::run_configured_inspection` | 分岐順序は qpdf と 1:1で、report helperは完了せず `write_qpdf` が全分岐後に warning drain・summary・memory reportを1回だけ行う。CLIの個別 inspection public APIは別の standalone consumerとして残るため、領域全体の classification は mixed。**2026-09-19（`flpdf-t3as9`）**: `flpdf-3yn9.48.186`（PR #2179、本ノート記載時点で未マージ）が9分岐（`--check`/`--show-npages`/`--show-pages`/`--show-xref`/`--check-linearization`/`--show-linearization`/`--show-encryption`/`--list-attachments`/`--show-attachment`）を combined dispatch へ widen する一方、`--show-object` だけを明示的に除外した。理由は、`JobObjectSelector`の generation range が `ObjectRef` の `u16` 表現に合わせて `0..=u16::MAX` にクランプされ、`5 65536 obj` のような qpdf の raw object-header generation（`u16::MAX` 超）を `Null` に誤射影し、かつ `Object` 分岐が通常の xref-backed `pdf.get_object_handle` を使っていた（CLI standalone 側の `run_show_object` は `Pdf::get_object_handle_by_raw_identity` を使う）ため。本 issue で `JobObjectSelector::Object` を `{ number: i32, generation: i32 }` に変え、generation range を qpdf 自身の `parse_object_id`（`libqpdf/QPDFJob.cc:929-940`、範囲クランプなし）に合わせ、解決経路を `Pdf::get_object_handle_by_raw_identity`（`QPDF::getObjectByID`、`libqpdf/QPDF.cc:1973-1977`）に揃えた。`crates/flpdf-cli/src/main.rs::top_level_inspection_combination_requested` 自体を widen して `--show-object` を combined dispatch に載せ、standalone `run_show_object` の top-level 呼び出しを削る作業（`run_show_object` 自体は `Commands::` subcommand 用に残る）は本 issue のスコープ外で、PR #2179（もしくはそのフォローアップ）側が本 issue のマージ後に行う。`--show-object` は「個別 inspection public APIは別の standalone consumerとして残る」caveat の対象としてまだ残っている、この行で pending な唯一のフラグである。 **2026-09-19（`flpdf-3yn9.48.206`）解消**: `top_level_inspection_combination_requested` の `migrated_flag_selected` に `args.show_object.is_some()` を追加し、`main.rs` の top-level standalone dispatch 分岐（`args.show_object.as_deref()` にマッチして `run_show_object` を呼ぶ分岐）を削除した。直前の段落の「解決経路が xref-backed lookup のまま」という記述は、本 issue 着手前から既に stale だった——`flpdf-t3as9`（コミット `cb16771b8`、本行の直前の更新）の時点で `JobObjectSelector::Object` は `Pdf::get_object_handle_by_raw_identity` へ既に切り替わっており（`crates/flpdf/src/job/lifecycle.rs:4171-4177`、`JobObjectSelector::Object` の doc コメントにも明記）、その後のマージ整理コミット（`7450d68d4`）が古い記述をそのまま書き戻してしまっていた。selector 整数パース（`parse_job_object_selector`/`parse_job_selector_integer`、`lifecycle.rs`）も qpdf の `parse_object_id`/`string_to_int` 挙動と等価であることを確認済み（number<=0→NoObject、generation<0→Null、それ以外は raw identity、オーバーフロー時のエラー文言も一致）。「`run_show_object` 自体は `Commands::` subcommand 用に残る」という記述も誤りだった——`--show-object` を受け付ける `Commands::` variant はこの enum に存在しない（`enum Commands` の全 variant を確認: `Check`/`CheckLinearization`/`Pages`/`Qdf`/`QdfFix`/`ZlibFlate`/`Rewrite`/`ShowEncryption`/`IsEncrypted`/`RequiresPassword`/`ShowEncryptionKey`）。`run_show_object` の production 側の呼び出し元は top-level standalone 分岐 1 箇所のみだったため、この分岐の削除に伴い `run_show_object`/`ShowObjectSelector`/`parse_show_object_selector` を削除した（`qpdf_selector_integer` は `--compress=N`/`--compression-level` で引き続き使うため残した）。`qpdf --show-object=<selector> FILE` と qpdf 11.9.0 の byte 比較（`trailer`/`N`/`N,G`/末尾カンマ/負の generation/`u16::MAX` 超の generation/`0`/非数値/raw・filtered stream data/`--empty`）はすべて一致した。region 全体の classification は引き続き `mixed`——`Commands::Pages --show-npages`/`Commands::Check`/`Commands::CheckLinearization`/`Commands::ShowEncryption` 等、qpdf に無い flpdf-native subcommand 面の standalone consumer（`run_show_npages`/`run_check`/`run_check_linearization`/`run_show_encryption` 等）は本行の対象領域内に残る別の standalone consumer であり、`--show-object` 単体の cutover では region 全体は canonical化しない。E-19/E-21 は本 issue の変更で classification 自体は変わらない（E-19 は E-21 の CLI dispatch 統合完了が前提のまま、E-21 は E-7 とは独立に依然 mixed）。E-21 行の `crates/flpdf-cli` 行数・`run()` 呼出し行は本 issue の削除（`main.rs` が 136 行減）で古くなったため実測値に更新した。 |

2026-09-16（`flpdf-c3d2x`）: attachment mutation の top-level route も、`--empty` 時は
`QPDFJobConfig::empty_input` を使って primary slot を明示的に消費し、single positional を
output に remap してから E-2 create / E-3 write boundary へ渡す。これは qpdf の
`Config::emptyInput` → `createQPDF` → `handleTransformations` → `writeQPDF` 順
（`libqpdf/QPDFJob_config.cc:27-50`; `libqpdf/QPDFJob.cc:428-492,1695-1716,2138-2247`）
を attachment add/copy と `--pages` foreign-source の両方で保つ。既存の empty/page
operation Job route と同じ mapping であり、empty 専用 writer や compatibility bridge は
追加しない。

2026-09-10（`flpdf-giz3`）: top-level CLI の複数 inspection selector は、既存の
`QPDFJob::run_configured_inspection` と同じ qpdf順の列へ接続された。
`QPDFJob::Config` 相当の inspection setters と `inspect_configured` が、単一PDF・単一
completion境界で report-only consumer を実行する。`--check-linearization` と qpdf が受理する
15個の併用フラグの conflict を除去し、writer-only option は no-output inspection で無効、
`--remove-restrictions`/`--coalesce-contents` は create-stage transformation として扱う。
`--with-images` と明示的な `--normalize-content` もcombined Job設定へ伝播する。
qtest exceptions と他の transformation/page-operation の未解消ルートは対象外である。

2026-09-15（`flpdf-7vov`）: `--check-linearization` と top-level attachment
mutation の併用も同じ `QPDFJob::run` に接続した。qpdf と同じく output-free
なら mutation を create stage で適用してから inspection を実行し、output
path があれば `no output file may be given for this option` を返す。

| E-8 | `QPDFJob::doCheck` | `libqpdf/QPDFJob.cc:744-803` | `crates/flpdf/src/job/lifecycle.rs::run_configured_inspection` → `crates/flpdf/src/job/check.rs::QPDFJob::run_check_report`（private）; public `QPDFJob::check` remains a library/test API | prod: `run_check_report` 3 (public `check`/`check_and_show_xref` plus lifecycle dispatcher) / test: 0 | canonical | `crates/flpdf/src/job/check.rs::QPDFJob::run_check_report` inside `QPDFJob::write_qpdf` | **2026-09-17（`flpdf-3yn9.48.145`）**: standalone top-level `--check` と `check` subcommand の CLI direct open/`QPDFJob::check` consumerを撤去し、qpdf の `createQPDF` → no-output `writeQPDF` → `doInspection` → `doCheck` boundaryへ接続した。public `QPDFJob::check` と qtest/C API test consumerは変更せず、combined inspection・JSON・`check-linearization` は別routeとして残す。|
| E-9 | `QPDFJob::doListAttachments` / `doShowAttachment` / `addAttachments` / `copyAttachments` | `libqpdf/QPDFJob.cc:876-911`, `include/qpdf/QPDFJob.hh:531-532,540-541` | `crates/flpdf/src/job/attachments.rs::QPDFJob::list_attachments`（pub）ほか同 impl の 5 メソッド | `format_attachment_list_with_sink` prod: 1 (`crates/flpdf/src/job/attachments.rs`) / test: 0。`list_attachments` は CLI から呼ばれる public Job method | canonical | `crates/flpdf/src/job/attachments.rs`（`QPDFJob` impl） | `QPDFJob` メソッド側は根拠 2 で legitimate。`.43` で buffer-returning free route `format_attachment_list` / `list_attachment_info` を削除し、`flpdf-xsq1` で残る sink helper を `pub(crate)` に狭め、job/lib.rs と crate-root の public re-export を撤去した。E-9 の caller-zero `AttachmentInfo` public projection も `flpdf-3yn9.48.97` で撤去済み。**2026-09-19（`flpdf-3yn9.48.186`）**: flpdf-cli top-level `--list-attachments`/`--show-attachment` の単一フラグ dispatch を standalone 関数から E-7 の combined `job.config()`+`job.run()` 経路へ統合した（詳細は E-7 行参照）。この変更は dispatch 構造のみで、E-9 の mixed classification 根拠（`format_attachment_list_with_sink` の可視性 debt、`flpdf-xsq1` で別途追跡）には影響しないため、本行の classification は変更しない。 **2026-09-19 再分類（mixed → canonical）**: この行が唯一の mixed 根拠として挙げる `format_attachment_list_with_sink` の可視性 debt は、同じセルが記録するとおり 既に解消済み——`flpdf-xsq1` は CLOSED で、helper は `crates/flpdf/src/job/attachment_list.rs:52` で `pub(crate)`、production caller は `job/attachments.rs:481` の 1 箇所のみ、`lib.rs` と `job/mod.rs` の re-export も 0 件。buffer-returning free route（`format_attachment_list`/`list_attachment_info`）は `.43` で削除済みで、production 実装は sink 経路 1 本。qpdf 側も `QPDFJob::doListAttachments`（`libqpdf/QPDFJob.cc:877`、caller は `:1685` の `doInspection` 1 箇所）の 1 実装なので、1 責務 1 実装で対応する。 |
| E-10 | `QPDFJob::handlePageSpecs` | `libqpdf/QPDFJob.cc:2359-2633` | `crates/flpdf/src/job/page_specs.rs::QPDFJob::handle_page_specs` | CLI direct `handle_page_specs` prod: 1 (`crates/flpdf-cli/src/main.rs:4460`、`apply_json_page_specs`。**2026-09-19 マージ後再測**: single-source を `create_qpdf` へ移した `.48.187`（#2180）と本変更の multi-source 統合で CLI 直呼びは 1 箇所になった。旧記載の `:4807,8180` は統合前の値)。job 内からも到達 | mixed | `crates/flpdf/src/job/page_specs.rs::QPDFJob::handle_page_specs` | `flpdf-hxmj` は closed。single-source をこの job boundary に接続し、standalone collate/CombinedPlan 経路を撤去した限定 slice は完了済み。`flpdf-3yn9.48.146` で rewrite の `--empty --pages` consumerも `QPDFJob::Config::empty_input`/`add_page_spec`/`collate` → `create_qpdf` のsource・password・keep-files-open・copy lifecycleへ cutoverし、CLI側の `open_page_source` / direct `handle_page_specs` callerを1つ減らした。**2026-09-19（`flpdf-3yn9.48.192`）**: 残る3つの直接 caller のうち multi-source page-operation output（`run_page_extraction_from_multiple_sources`）を `QPDFJob::Config::add_page_spec`/`collate`/`remove_unreferenced_resources`/`writer_configuration` → `create_qpdf`（`prepare_document` の merge 分岐、qpdf の `createQPDF` → `handlePageSpecs` と同じ呼び出し順）へ cutover した。`create_qpdf` のマージ分岐はプライマリを消費し fresh target を返すため、呼び出し元は返却文書から `primary_encrypted`/`primary_copy_encryption` を読めない — この issue で新設した `QPDFJob::encryption_status`/`take_primary_copy_encryption`（qpdf の public `getEncryptionStatus`、`include/qpdf/QPDFJob.hh:402`、に対応する 1 個目、対応物の無い 2 個目）がこの pre-merge snapshot を公開する。旧 CLI 直書きの `open_page_source`（reopenable page source 用の direct `open_file_with_options`）はこの cutover で不要になり撤去した。qpdf 実機 11.9.0（`--deterministic-id`、`qpdf-zlib-compat`）との byte 比較、および `cli_preserve_unreferenced_pages.rs` 9/9・`page_job_route_cutover_tests.rs` の新規構造テストで検証済み。**2026-09-19 マージ後訂正**: 残る直接 caller は **1 つ**（JSON page selection の `apply_json_page_specs`、`crates/flpdf-cli/src/main.rs:4459`）。single-source page-operation output は `flpdf-3yn9.48.187`（#2180）で cutover 済みで、本変更が multi-source を移した。この 1 件が残るため行は mixed のままで、別の canonical cutover scope とする。2026-09-08（`flpdf-3yn9.48.8`）: `QPDFJobConfig::add_page_spec` を新設し JSON 経由と byte-identical であることを検証した（`config_add_page_spec_matches_the_json_configured_path_single_source`）。2026-09-10（`flpdf-3yn9.48.76`）: top-level `--pages` と no-output inspection の consumer は `QPDFJobConfig::empty_input`/`add_page_spec`/`collate` と `QPDFJob::run`（`create_qpdf` → `write_qpdf`）へ cutover し、`--empty --pages ... -- --show-pages` の page-selection bypass を解消した。 |
E-10 page-merge inherited-attribute note (`flpdf-k4bp`, 2026-09-10): the primary source must pass through the existing qpdf-shaped `push_inherited_attributes_to_pages` preparation before its selected graph is copied into the fresh target. This preserves direct non-scalar `/MediaBox` promotion and leaf inheritance in the multi-source consumer; secondary-source preparation was already canonical. |
2026-09-15（`flpdf-kiou0`）: `QPDFPageData` の `getAllPages()` → range resolution 順序
（`libqpdf/QPDFJob.cc:259-269`）に合わせ、multi-source の各 `PagePlan::build` 前に
`PageDocumentHelper::get_all_pages` を実行する。これにより既存の canonical page-tree
repair が `/Type` と direct kid を補正してから spec の page count/order に反映される。
`direct-leaf-kid.pdf` と `mistyped-page-tree.pdf` の concat/`--collate=2` で、qpdf
11.9.0 の exit 3、warning stderr、stdout、QDF output bytes を differential test で固定する。
| E-11 | `QPDFJob::handleUnderOverlay` / `doUnderOverlayForPage` | `libqpdf/QPDFJob.cc:1936-2043`, `libqpdf/QPDFJob.cc:1858-1911` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::prepare_document_transformations`（private、`crates/flpdf/src/job/lifecycle.rs:3716`）→ `crates/flpdf/src/job/overlay.rs::handle_under_overlay` / `::overlay_verbose_report` | `handle_under_overlay` prod: 1 (`crates/flpdf/src/job/lifecycle.rs:3745`) / test: 9；`overlay_verbose_report` prod: 1 (`crates/flpdf/src/job/lifecycle.rs:3742`) / test: 0 | canonical | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::prepare_document_transformations` | `.92` で `--pages` post-plan consumer は `QPDFJob::apply_transformations` と `configure_cli_overlay_specs` を通り、CLI の free-helper direct caller は0になった。Job が donor open、verbose report、underlay/overlay mutation、source lifetimeを所有する。free helper の同一crate unit testsは保持するが production routeは一本。top-level inspectionは `flpdf-clpz`/`flpdf-p4og4` で既にQPDFJob routeへ接続済み。 |
2026-09-14（`flpdf-clpz` bounded slice）: top-level `--check`/combined inspection で underlay/overlay の path・password・from/to/repeat を `QPDFJobConfig::overlay/underlay` へ積み、`QPDFJob::apply_transformations` の create-stage（overlay → transformations）を通してから `doInspection` 相当を実行するようにした。`--check`＋`--show-pages` の qpdf 11.9.0 live differential を追加。rewrite/page-operation の既存 direct caller は残るため E-11 全体の mixed 判定は維持する。 |
2026-09-14（`flpdf-p4og4` bounded slice）: overlay/underlay を伴う top-level inspection も `QPDFJob::run`（`create_qpdf` → `write_qpdf`）へ接続した。これにより qpdf と同じ `rotate` → donor open/underlay/overlay → create-stage transformation → `doInspection` の順序、donor への password/recovery open policy、donor path 付きエラーを共有する。`--overlay`/`--underlay` の rewrite direct caller と page-selection 後の別 consumer は残るため E-11 全体の mixed 判定は維持する。 |
2026-09-14（`flpdf-hy1g2` bounded slice）: combined top-level inspection で `--show-encryption` を含む場合も、wrong-password の primary open は既存の partial encryption-inspection primitiveへ接続した。qpdf の `QPDFJob.cc:432-448` と同じく、parsed encryption reportを出して成功したnull-documentとして `writeQPDF`/後続inspectionをスキップし、通常のopen errorはexit 2のまま保持する。`--check`/`--show-pages`/`--show-npages`/`--show-xref`/`--show-linearization`/`--list-attachments` との wrong-password 6組と missing-input differentialを追加した。standalone inspection、変換conflict、page-selection後の別consumerは変更していない。
| E-12 | `QPDFJob::handleTransformations`（12 分岐: `remove_restrictions` → `externalize_inline_images` → `optimize_images` → `generate_appearances` → `flatten_annotations` → `coalesce_contents` → `flatten_rotation` → `remove_page_labels` → `page_label_specs` → `attachments_to_remove` → `attachments_to_add`→`addAttachments` → `attachments_to_copy`→`copyAttachments`） | `libqpdf/QPDFJob.cc:2137-2248`（12 分岐の分岐行は `libqpdf/QPDFJob.cc:2147,2151,2156,2178,2182,2185,2190,2196,2199,2230,2242,2245`、範囲としては `libqpdf/QPDFJob.cc:2147-2247`） | `crates/flpdf/src/job/lifecycle.rs::prepare_document_transformations`（private、`:3716`）を `QPDFJob::create_qpdf` と `QPDFJob::apply_transformations` の共通 owner として使用 | prod: 4（`crates/flpdf/src/job/lifecycle.rs` の全 caller） / test: 0 | canonical | `crates/flpdf/src/job/lifecycle.rs::prepare_document_transformations` | `.48.5` で qpdf と同じ変換順序を `create_qpdf` の内側へ移した。2026-09-08（`flpdf-8uuw`）で通常 non-linearized rewrite の retained direct routeも overlay → image → appearance → annotation → coalesce → rotation の順へ揃え、repository-owned inline-image probe と qpdf-zlib-compat byte comparisonを追加した。2026-09-10（`flpdf-v7vr`）で top-level の no-`--pages` rotate/split consumerも raw `QPDFJobConfig`（rotate/split/resource policy）へ移し、`create_qpdf` → `write_qpdf` を通る canonical routeにした。2026-09-10（`flpdf-m6kt`）で top-level no-`--pages` rotate/split も `generate_appearances` / `flatten_annotations` を同じ create-stage Jobへ渡すようにし、rotate/split × generate/flatten の4セルを qpdf-zlib-compatible byte differential で固定した。2026-09-14（`flpdf-3yn9.48.94`）で `--pages` post-plan の rotation/image consumerも、page selection完了後に同じ `QPDFJob::apply_transformations`へ raw Configとして積むようにした。これにより旧 `apply_rotate_specs` / `apply_image_transformations` の production callerは0になり、qpdfの rotation → underlay/overlay → image → appearance/annotation/coalesce/flatten orderを共通 ownerへ集約した。page labels・attachmentsも同じ preparation bodyに残る。E-4/E-10/E-21など他のJob/CLI境界とqtest exceptions、route-wide parity closureは別スコープである。 |
2026-09-13（`flpdf-1emn` bounded slice）: `--coalesce-contents` は top-level の `--linearize` と page-operation（`--pages`/`--rotate`/`--split-pages`）で受理し、top-level は `QPDFJob` の create-stage configuration、`rewrite` page-operation は page selection 後の同じ transformation owner へ渡す。`multi-contents-one-page.pdf` の qpdf 11.9.0 との status/stdout/stderr/byte 比較を `cli_linearize_page_ops_qpdf.rs` と `cli_page_operation_transforms_qpdf.rs` で固定した。`remove_restrictions`、`decrypt`、`copy_encryption`、appearance/annotation の残る rewrite page-op guard と、page-selection 後の image/overlay order は未解消であり、行全体は `mixed` のまま。 |
2026-09-14（`flpdf-1emn` follow-up slice）: `rewrite` の `--pages` page-operation でも `--generate-appearances` / `--flatten-annotations=all` を拒否せず、page selection 後の qpdf transformation ownerへ渡すようにした。2 fixtureのlive differential（status/stdout/stderr/bytes）を `cli_page_operation_transforms_qpdf.rs` へ追加。`remove_restrictions`、`decrypt`、`copy_encryption` のguardと、page-selection後のimage/overlay順序は残る。 |
2026-09-14（`flpdf-1emn` follow-up slice）: `rewrite` の `--remove-restrictions` も page selection 後の同じ transformation ownerへ渡し、通常/rotate/pages/split の各経路で qpdf の署名制限除去を適用するようにした。署名fixtureのstatus/stdout/stderr/byte differentialを `cli_page_operation_transforms_qpdf.rs` で固定。`decrypt`、`copy_encryption` のguardと、page-selection後のimage/overlay順序は残る。 |
2026-09-15（`flpdf-hzv1w`）: `QPDFJob::handleTransformations` の page-label mutation は
`Pdf::root_handle()` を semantic Catalog 境界として使うように補正した。qpdf の
`QPDF::getRoot`（`libqpdf/QPDF.cc:2355-2368`）と同じく direct/indirect `/Root` を
受理し、page-label helper の read/reconstruction と top-level
`--set-page-labels`/`--remove-page-labels` が同じ live handle を変更する。
`compat/direct-root-one-page.pdf` の `1:D`・`1:r`・`1:A` で qpdf 11.9.0 と
status/stdout/stderr/output bytes を比較した。`root_ref()` の production 99 caller
（24 files）は identity/consumer residual として棚卸しし、本 bounded slice では
semantic page-label caller 5 箇所だけを移行した。

2026-09-14（`flpdf-ydwg` bounded slice）: `--json` / `--json-output` も top-level `InspectionTransformOptions` を `QPDFJob::apply_transformations` へ渡し、JSON serializerの前に qpdf の create-stage 順序（image → appearance → annotation → rotation）を実行するようにした。`inherited-rotate-one-page.pdf` と `form-fields-and-annotations.pdf` の stdout/file JSON を qpdf 11.9.0 と比較し、status/stdout/stderr/file bytes を固定。既存の page-selection post-plan consumer と E-11 の overlay direct caller は残るため、E-12/E-11 の行全体は mixed のまま。flatten wrapper stream は qpdf `newStream` 相当の `new_stream_with_data` に揃えた。 |
2026-09-14（`flpdf-ca9zm` bounded slice）: `--show-pages`/`--show-npages`/`--show-xref`/`--show-linearization` と `--remove-restrictions`/`--coalesce-contents`/`--flatten-annotations=all` の12組、および `--list-attachments`＋`--coalesce-contents` を受理し、既存の combined QPDFJob inspection boundary または InspectionTransformOptions へ接続した。qpdf 11.9.0 の13組 status/stdout/stderr differentialを追加。`--show-object`/`--show-encryption`、`--check-linearization`、JSONの未接続transform、rewrite/page-operation の残る別組合せはこのsliceの外側で、E-12行全体はmixedのまま。 |
2026-09-14（`flpdf-jq40m` bounded slice）: `--json-input` と `--update-from-json` の inspection routeも `remove_restrictions`/`coalesce_contents` を `run_job_inspection_on_pdf` から canonical create-stage transformation ownerへ渡すようにした。qpdf 11.9.0 と multi-content JSON inputの `--show-pages --coalesce-contents`、および update-from-JSONの同組合せを比較し、coalesced content streamの出力を固定した。JSON/page-operationの残る別consumerと、E-12全体の mixed 判定は維持する。
2026-09-14（`flpdf-5qbs` bounded slice）: qpdf `doInspection` の同一argv再指定（boolは冪等、`showObject`/`showAttachment`は最後の値）を top-level clap の self-override で受理し、`--list-attachments`＋`--show-attachment` は独立consumerとして qpdf順に両方実行するようにした。qpdf 11.9.0 の repeated inspection / selector last-wins / list-plus-show を `cli_inspection_argv.rs` で status/stdout/stderr differential 固定。mutation attachment operation の相互排他と page-operation 後の別consumerは対象外。 |
2026-09-15（`flpdf-3yn9.48.102`）: qpdf の public `removeSecurityRestrictions()` / `disableDigitalSignatures()` はいずれも `void`（`include/qpdf/QPDF.hh:603-607`; `include/qpdf/QPDFAcroFormDocumentHelper.hh:166-170`）であり、flpdf の変更有無を返す `Result<bool>` は対応しない projection だった。両 API を `Result<()>` に狭め、Job の捨てられていた `changed` 値と deviation marker を撤去した。`/Perms`、`/SigFlags`、署名フィールドの mutation と byte differential は維持する。 |
| E-13 | `QPDFJob::handleRotations` | `libqpdf/QPDFJob.cc:2635-2652` | `crates/flpdf/src/job/lifecycle.rs::apply_configured_rotations`（private、`:3690`）。CLIの全 rotation consumerは `QPDFJob::Config::rotate` → `QPDFJob::apply_transformations` または `create_qpdf` を経由する | prod: 4（`crates/flpdf/src/job/lifecycle.rs`） / test: 0 | canonical | `crates/flpdf/src/job/lifecycle.rs::apply_configured_rotations` | qpdfと同じく、同一ループで`parse_numrange(range,npages)`→`pageno-1`→`0 <= pageno < npages` filter→page applyを行う。2026-09-14（`flpdf-3yn9.48.94`）で `--pages` post-plan の直接 `apply_rotate_specs` を撤去し、page-selection completion後の output-page numberingも raw rotation parameterをJobへ積んで同じ ownerで適用するようにした。`apply_rotate_specs` のproduction caller/定義は0、`PageRange::resolve`依存とempty guardは削除済み。旧 `apply_rotate_to_pages` batch helperは `flpdf-v55s` で削除し、Job/CLIの canonical rotation ownerは変更しない。 |
| E-14 | `QPDFJob::parseRotationParameter` | `libqpdf/QPDFJob.cc:368-415`, `include/qpdf/QPDFJob.hh:482` | `crates/flpdf/src/job/rotate_spec.rs::parse_rotation_parameter`（`pub(crate)`、`:35`）+ crate-internal `RotationSpec` / `RotationParameter` | prod: 2（`crates/flpdf/src/job/lifecycle.rs`） / test: 11 | canonical | `crates/flpdf/src/job/rotate_spec.rs::parse_rotation_parameter` | qpdfのprivate parser/stateに合わせてvisibilityをcrate内へ狭めた。job JSONはlifecycle内でNUL処理後に同じparserを直接呼び、CLI configurationは`QPDFJobConfig::rotate`経由で同じparserを呼ぶ。CLIのtest-only parser helperとpublic re-exportは`flpdf-3yn9.48.98`で撤去。range、angle、relative、raw bytes、JSONのNUL境界は既存core testsとJob config testsで保持する。 |
| E-15 | `QUtil::parse_numrange`（`QPDFJob::parseNumrange` は例外処理を足した薄いラッパー） | `include/qpdf/QUtil.hh:464`, `libqpdf/QUtil.cc:1304-1429`, `libqpdf/QPDFJob.cc:399-425`, `libqpdf/QPDFJob_argv.cc:240-272`, `libqpdf/QPDFJob_config.cc:1055-1074` | `crates/flpdf/src/qutil.rs::parse_numrange`（pub、`:558`） | prod: rotation parser / lifecycle / CLI、test: qpdf contract vectors | mixed | `crates/flpdf/src/qutil.rs::parse_numrange` | signed `max`、max=0 syntax-only、raw bytes、NUL終端、group全体の文法先行検査、exclusion、position-based odd/even、QIntC narrowing/wrappingを共通primitiveへ移植した。rotation consumerで先行利用し、`PageRange`の他consumer（page-plan/page-combine/page-specs/overlay/argv/lifecycle）も全てこのprimitiveへ委譲する唯一の実装ownerに揃っている。2026-09-10（`flpdf-iym2`）で duplicate `PageRange` parser/resolverを削除し、parse/resolveの唯一の実装ownerをこのqutil primitiveへ統一した。qpdfのomitted `--pages` range `1-z`は`PageRange::all`、explicit emptyは`PageRange::parse_numrange("")`/`empty`として別表現に保つ。CLI positional dispatchもrange parse → openable-file fallback → original usage errorへ揃えた。2026-09-19（`flpdf-3yn9.48.179`）: `Endpoint`/`PageRangeEntry`/`Parity`（`PageRange`が raw bytes へ一本化される前の構造化endpoint語彙。qpdf側に対応物なし——`QUtil::parse_numrange`は`std::vector<int>`を返すのみ）の可視性を rule-8 の4根拠（qpdf側 public 対応／`QPDFJob` public method 経由／crate doc 明記／legitimate な pub シグネチャの支援型）で判定したところいずれも満たさず、かつ再export chain 以外にクレート内の呼び出しが一切無いことをgrepで確認した。`pub(crate)`へ狭めるプローブでは`-D warnings`下で`dead_code`エラー3件（未使用のenum/struct）を実測し、narrowing では閉じられないことを示したため、3型を削除した（`job/mod.rs`・`lib.rs`の re-export も削除）。overlayの`--from`/`--to`/`--repeat`は`QPDFJob.cc:1827,1837,1839`と同じ page count（source/dest/source）で`PageRange::resolve`を呼び、page-operationの`PagePlan::build`も`QPDFJob.cc:266`と同じくsource自身のpage countで呼んでおり、owner closure に qpdf との不整合は無い。ただし`qutil::parse_numrange`自体は rotation parser／lifecycle／CLI から直接到達する複数 entrypoint が残るため、行全体の分類は `mixed` のまま。 |
| E-16 | `QPDFJob::shouldRemoveUnreferencedResources` | `libqpdf/QPDFJob.cc:2250-2339`, `include/qpdf/QPDFJob.hh:515` | `crates/flpdf/src/job/resource_pruning.rs::should_remove_unreferenced_resources`（`pub(crate)`）+ `::should_remove_unreferenced_resources_with_report`（`pub(crate)`） | prod: silent wrapper 1（`crates/flpdf/src/job/page_merge.rs:1048`（**2026-09-19 訂正**: `:968` は field-name collision の説明コメント行））+ report variant 2（`crates/flpdf/src/job/page_specs.rs:269`、`crates/flpdf/src/job/page_split.rs:203`。**2026-09-19 訂正**: split-page 経路の caller が欠けていた） / test: 9（silent wrapper） | canonical | `crates/flpdf/src/job/resource_pruning.rs::should_remove_unreferenced_resources_with_report` | qpdf 側は private メソッドで、caller は `handlePageSpecs`（`QPDFJob.cc:2454`）と `doSplitPages`（`:2940` 開始、呼び出しは `:2961`）の 2 箇所（**2026-09-19 訂正**: 「`handlePageSpecs` からしか呼ばれない」は誤りで、本行が追記した split-page 経路がまさに 2 つ目）。qpdf private heuristic に対応する free wrapperは `flpdf-3yn9.48.99` でcrate-internalへ狭め、job/rootのpublic re-exportを撤去した。publicな `RemoveUnreferencedResources` enumと`QPDFJobConfig` setter、PageObjectHelperのpage/Form APIは別のqpdf対応面として保持する。（**2026-09-19 失効**: 「silent page-merge wrapper と verbose report variant の経路差が残るため mixed」という旧結論は、下記の再分類で置き換えられた。）2026-09-18（`flpdf-3yn9.48.159` probe）: qpdf 11.9.0 をoracleに `--pages`/`--split-pages`/multi-source/`--empty` の**48セル**（3 mode × [5 fixture × {`--pages`, `--split-pages`} + multi-source 6]、`probe159/e16_probe.sh`）でbyte・verbose文言・warning文言・exit codeを実測し、全セル一致（観測可能な出力差なし）。**stdin（`-`）入力は本 probe の対象外**——committed runner に該当シナリオは無い。`page_merge.rs`のAuto armは`merge_documents`（`:1006`）が`RemoveUnreferencedResources::No`を決め打ちするためproduction未到達（唯一の呼び出し元はunit test）。cutoverは純粋な構造統合として扱ってよい。**2026-09-19（`flpdf-3yn9.48.180`）訂正**: 「silent wrapperとverbose report variantの経路差」の実体を精査した。`should_remove_unreferenced_resources`（silent）は`should_remove_unreferenced_resources_with_report(pdf, \|_\| Ok(()))`の薄い委譲であり、重複実装ではなく単一 primitive への2エントリポイント——qpdfの`shouldRemoveUnreferencedResources`も`doIfVerbose`で内部的にverbosityを分岐する1メソッドのみで、呼び出し元は`libqpdf/QPDFJob.cc:2454,2961`の2箇所。残る唯一の実体差は、silent側の唯一の呼び出し元`merge_documents_with_resource_mode_and_preserve_primary`（`crates/flpdf/src/job/page_merge.rs:1048`）の`Auto`分岐が production 未到達であること——`merge_documents`が`No`を決め打ちするため、`Auto`へ到達するのは`#[cfg(test)]`のunit test（`page_merge.rs:1958`）のみ。job/CLIのproduction経路（`page_specs.rs`、`handle_page_specs` → `run()`）は既にverbose report variantを直接使っており canonical。silent側はqpdfに対応するAuto動作を持つが到達不能という点だけが残るため、mixedのまま維持する。 **2026-09-19 再分類（mixed → canonical）**: silent wrapper `should_remove_unreferenced_resources` は `_with_report` へ 「何もしない report コールバック」を渡すだけの 1 行の委譲で、独立した実装を持たない。唯一の非テスト caller である `page_merge.rs:1048` の `Auto` arm へ到達する経路は `merge_documents_with_resource_mode_and_preserve_primary`（`:1035`）のみで、その production caller は `merge_documents`（`:1006`）が `RemoveUnreferencedResources::No` を決め打ちする 1 本だけ——`Auto` を渡すのは `mod tests`（`:1729` 以降）内の `:1958` だけである。よって production 実装は `_with_report` の 1 本で、経路差は残っていない。E-4 が `write_qpdf_to_memory` （`#[cfg(test)]` scaffolding）について「公開 API でも production route でもないため canonical owner を分岐させない」とするのと同じ基準を適用した。 |
| E-17 | `QPDFJob::initializeFromArgv` / `initializeFromJson`（`QPDFArgParser` 経由の argv 解釈） | `include/qpdf/QPDFJob.hh:75-90`, `libqpdf/QPDFJob_argv.cc` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::initialize_from_argv`（pub、`crates/flpdf/src/job/lifecycle.rs:1781`） | prod: 3（すべて flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs:147,164,183）/ test: 23 (flpdf/tests/job_lifecycle_tests.rs) | mixed | absent | **flpdf-cli は `initialize_from_argv` を一度も呼ばない** — clap 定義（CLAUDE.md 逸脱分類 (B) の `QPDFArgParser` → clap）で独自に引数を解釈し、`QPDFJob` の setter を個別に叩く（`job.set_input_file` / `job.set_output_file` / `job.set_password` …）。qpdf の CLI は `initializeFromArgv` 1 本しか使わない（`qpdf/qpdf.cc:35`）。argv → 設定が CLI 側と library 側に分かれている。2026-09-06 確認: library initializer は限定的な手書きdispatchで `--rotate` などを未実装。CLIをそのまま接続できる canonical prerequisite は完成していない。2026-09-08（`flpdf-3yn9.48.6`）: 全 124 option の対応表を機械測定（本ファイル末尾「E-17/E-21 option correspondence table」）。`initialize_from_argv` は 11/124（9%）、`main.rs` 独自実装は 113/124（91%）で、想定と逆に `main.rs` の方が qpdf 文法（`@argfile`/`--` reset の edge case）まで含めて先行している。CLI 接続は本 issue では見送り。2026-09-08（`flpdf-q5ok`）: `initialize_from_argv` 自体に `@argfile` 展開（`expand_arg_files`、`lifecycle.rs:1789` の `--` 直後）と qpdf 準拠 `--` main-table reset（`lifecycle.rs:1800`）を実装し、想定していた前提の欠落を解消した。option 数自体（11/124）は変わらない — 未着手なのは個別 option の移植、CLI 接続の判断は依然保留 |
| E-18 | `QPDFJob::checkConfiguration` | `libqpdf/QPDFJob.cc:566-642`, `include/qpdf/QPDFJob.hh:129-130` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::check_configuration`（pub、`crates/flpdf/src/job/lifecycle.rs:3230`） | prod: 8 (flpdf/src/job/lifecycle.rs 4, flpdf-qtest-tools/src/driver/test_80_87.rs 4) / test: 0 | canonical | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::check_configuration` | qpdf と同じく `createQPDF` 冒頭（`crates/flpdf/src/job/lifecycle.rs:2383`）から呼ばれ、public としても露出。CLI は使わない（E-17 の帰結）が、それは「別の正本がある」のではなく「CLI が job 設定を組み立てない」ため |
| E-19 | `QPDFJob::getExitCode` / `hasWarnings` / `createsOutput` | `libqpdf/QPDFJob.cc:522-564` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::get_exit_code`（pub、`:5023`） + `crates/flpdf/src/job/lifecycle.rs::QPDFJob::complete`（pub、`:5053`） + `complete_report`（pub、`:5083`） + `has_warnings`（pub） ; document warning API は `crates/flpdf/src/reader.rs::Pdf::get_warnings` / `any_warnings` / `num_warnings` | `get_exit_code` leaf tracker prod: 22 / test: 20; `complete` prod: 19 / test: 15; `has_warnings` prod: 11 / test: 7 | mixed | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::get_exit_code` + `drain_document_warnings` + `complete_report` | `get_exit_code` は logger/write/drain を行わない純粋な query。`write_qpdf` が `get_warnings` 相当の document drain、warning summary、memory reportを1回の enclosing completionとして実行し、JSON/check/linearizationの既存standalone public APIはその後 `get_exit_code`を返す。CLI direct completionは残るが、旧 `complete` がstatusを兼ねる経路は撤去済み。 **2026-09-19（`flpdf-3yn9.48.188`）反映**: main.rs の16直接呼出し（`get_exit_code` 9 / `complete(false)` 2 / `has_warnings` 5、2026-09-19実測）を1件ずつ監査した。7件の`get_exit_code`（`run_json_document`/`run_page_operations_with_qpdf_job`/`run_rewrite_with_qpdf_job`/`run_rewrite_opened`/split・write分岐/`run_configured_attachment_job`）は`write_qpdf`直後の即時呼び出しで、`write_qpdf`自身が内部で`drain_document_warnings`+`complete`を既に実行済みのため`run()`自身の完了パターンと同型（該当箇所は`create_qpdf`をrun()経由でなくCLI側で個別に呼ぶper-consumer document pipeline構造に属し、E-7/E-9/E-10/E-21の対象）。5件の`has_warnings`は状態問い合わせのみ——`run_page_extraction_from_multiple_sources`/`run_page_extraction_from_single_source`がsource opening用の先行`QPDFJob`とwrite用の後続`QPDFJob`の間でwarning状態を引き継ぐための橋渡しで、qpdfの`page_heap` foreign QPDF warning fold（`QPDFJob.cc:474-479`）と同じ意図を、flpdf-cliが単一`QPDFJob`ではなく複数`QPDFJob`インスタンスへ分解している構造（E-7/E-9/E-21）越しに再現している。残る2件の`complete(false)`（`finish_show_encryption`/`finish_warning_state`、main.rs）は`record_document_warnings`/`record_warnings`の後で`complete(false)`→`get_exit_code`を個別に組み立てており、これは`inspect`/`inspect_configured`/`write_qpdf`が既に共有する同じ末尾（`libqpdf/QPDFJob.cc:483-511,535-564`）の重複実装だった。16箇所の監査で見つかった唯一の独自組み立てだったため、新設した `complete_report`（[`self.complete(false)?; Ok(self.get_exit_code())`] のみを行う、qpdf側に個別の識別子がない private inline tail の抽出）へ両呼び出し元を統合した。残り14件はqpdf writeQPDF完了境界の意味的な複製ではなく、flpdf-cliがqpdfの単一`run()`呼び出し列ではなく複数consumer関数・複数`QPDFJob`インスタンスへ分解されている構造自体に起因するため、本行は引き続き `mixed`——`canonical`化にはE-21のCLI dispatch統合（`flpdf-3yn9.48.186`/`.187`が担当）が前提。 |
| E-20 | `QPDFJob::getLogger` / `setLogger` / `setMessagePrefix` / `getMessagePrefix` / `registerProgressReporter` | `libqpdf/QPDFJob.cc:302-337`, `include/qpdf/QPDFJob.hh:92-123` | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::logger`（pub、`crates/flpdf/src/job/lifecycle.rs:1308`）ほか 4 メソッド | `set_message_prefix` prod: 27 (flpdf/src/job/lifecycle.rs, flpdf-cli/src/main.rs, flpdf-qtest-tools) / test: 6；`QPDFJob::register_progress_reporter` prod: 3 (flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs:143,178, flpdf-qtest-tools/src/driver/test_80_87.rs:336) / test: 2 (flpdf/tests/job_lifecycle_tests.rs:302,1591) | canonical | `crates/flpdf/src/job/lifecycle.rs`（`QPDFJob` の logger/prefix impl） | `logger()` / `message_prefix()` の `get_` 省略は 7 の bare getter 例外に該当し正しい。CLI が `QPDFJob::new` を 13 回作って（**2026-09-19 再計測**、旧記載 25 は stale）毎回 logger と prefix を設定し直しているのは E-17 / E-4 の帰結（job インスタンスが lifecycle を持たない）。`crates/flpdf/src/writer.rs:695` の `PdfWriter::register_progress_reporter` は同名の別シンボルで、`crates/flpdf/src/job/lifecycle.rs:1512`（`configure_writer_progress` 内）と `crates/flpdf/tests/linearize_objstm_generate_tests.rs:1403` はそちらの caller — 上の数から除外している |
| E-21 | CLI 実行ファイル consumer（`QPDFJob` public のみ 4 手） | `qpdf/qpdf.cc:26-44` | `crates/flpdf-cli/src/main.rs::main`（bin crate） | prod: 1 bin（`crates/flpdf-cli`、**11,734 行、2026-09-19（`flpdf-3yn9.48.206`）再測**（直前の 11,866 行は `.48.206` が `run_show_object`/`ShowObjectSelector`/`parse_show_object_selector` を削除する前の値。旧記載 12595 行は `.48.187`/`.48.192` の統合前））/ test: 111 統合テストファイル（2026-09-19 実測）（`crates/flpdf-cli/tests/*.rs`） | mixed | absent | qpdf の CLI は `initializeFromArgv` → `run` → `getExitCode` の 3 呼び出し（62 行）。**2026-09-19（`flpdf-3yn9.48.180`）訂正**: 以下の行数・呼出し数は stale。flpdf-cli は現在 12595 行、`QPDFJob::new` は 13 回、`run()` は 4 箇所（`run_top_level_page_selection_inspection`:4067、`run_combined_top_level_inspection`:4135、`run_job_json_files`:4504、`run_check`:5319）から呼ばれる——E-4/E-12/E-17 の cutover 以降さらに前進しているが、依然 CLI 全体では「qpdf は全フラグ組合せで同一の `run()` 呼出し列を使う」という構造には揃っていない（詳細は E-7/E-9 行、follow-up `flpdf-3yn9.48.186`/`.187`/`.188` を参照）。E-17 が示すとおり argv 解釈を自前で持ち、E-12 の `handleTransformations` 相当は `job.apply_transformations()`（public メソッド）へ委譲しているものの、`run()` の外から CLI 自身が駆動している（`main.rs:469,4850`）——qpdf の CLI は `handleTransformations` を一度も呼ばず `run()` に任せる（**2026-09-19 訂正**: ここに挙げていた `writeOutfile` は失効。E-4 は cutover 完了で `QPDFJob::write_qpdf` を canonical owner とし、CLI production の直接 `PdfWriter` consumer は 0 と記録している）。単純な可視性変更では閉じられず、canonical Job の作成・変換・出力・argv 責務の完成と consumer ごとの移行が前提（`flpdf-hxmj` は single-source 接続を完了して closed）2026-09-08（`flpdf-3yn9.48.10`）: `QPDFJobConfig::add_attachment`/`remove_attachment`/`copy_attachments_from` を新設し、JSON 経由と byte-identical であることを検証した（`crates/flpdf/tests/job_lifecycle_tests.rs::config_{add,remove}_attachment_matches_the_json_configured_path`/`config_copy_attachments_from_matches_the_json_configured_path`）。ただし `main.rs::run_add_attachment`/`run_remove_attachment`/`run_copy_attachments_from` の接続は見送った——`remove_restrictions`/linearize（`linearize_pass1` 含む）/repair-mode open option が`QPDFJobConfig` に未実装で、接続すると CLI の既存機能が退行するため。password/suppress_warnings は `QPDFJob::set_password`/`set_suppress_warnings` で既に直接設定可能。 **2026-09-19（`flpdf-3yn9.48.206`）再測**: 上記「`run()` は4箇所」の呼出し行番号は `flpdf-3yn9.48.180` 時点のもので、その後の複数マージ・本 issue の `run_show_object` 削除で stale。このブランチで実測した呼出し行（`.run()?` を呼ぶ行、宣言行ではない）は `run_top_level_page_selection_inspection`:3962、`run_combined_top_level_inspection`:4030、`run_job_json_files`:4160、`run_check`:4956 の4箇所で、依然 4 箇所のまま変わらない。E-7 の `--show-object` cutover（本 issue）は `run()` 呼出し箇所数・構造そのものには影響しない——削除したのは `run()` を経由しない standalone 関数（`run_show_object`）なので、E-21 の classification（mixed）も変わらない。 |
| E-22 | `QPDFJob` の C API consumer（pure pass-through） | `libqpdf/qpdfjob-c.cc:19-161`, `qpdf/qpdfjob-ctest.c`（142 行、機械検証対象外の `.c`） | `crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs::main`（bin crate、281 行） | prod: 1 bin / test: qtest ハーネス（`crates/flpdf-qtest-tools/src/orchestrator.rs`）経由 | canonical | `crates/flpdf-qtest-tools/src/bin/qpdfjob_ctest.rs` | `QPDFJob` の public surface（`new` / `register_progress_reporter` / `initialize_from_argv` / `initialize_from_json` / `create_qpdf` / `write_qpdf` / `run` / `set_logger` / `set_message_prefix`）しか触らず、qpdf の C wrapper 構造を正しく踏襲している唯一の consumer。ただし `report_job_error`（`crates/flpdf/src/job/lifecycle.rs:3369`、pub）は qpdf 側では C wrapper 内の `wrap_qpdfjob`（`libqpdf/qpdfjob-c.cc:32-41`）に相当し、`QPDFJob` の public メソッドではない — 位置が違う |
| E-23 | `QPDF` / `QPDFWriter` の C API consumer（`QPDFJob` を経由しない） | `libqpdf/qpdf-c.cc:24-66`, `libqpdf/qpdf-c.cc:459-521`, `libqpdf/qpdf-c.cc:1924-1952` | `crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs::main`（bin crate、523 行） | prod: 1 bin / test: qtest ハーネス（`crates/flpdf-qtest-tools/src/orchestrator.rs`）経由 | canonical | `crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs` | `qpdf_check_pdf` は `QPDFJob::doCheck` ではなく `QPDFWriter` + `Pl_Discard` + `setDecodeLevel(qpdf_dl_all)` で `write()` する（`libqpdf/qpdf-c.cc:58-66`）— flpdf 側でも E-8 の `QPDFJob::check` とは別責務として扱う必要がある。**2026-09-19（`flpdf-3yn9.48.178`）**: JSON tests 46/47 も `qpdf_write_json`（`libqpdf/qpdf-c.cc:1924-1952`）が `QPDF::writeJSON` を直接叩く形に合わせ、`crates/flpdf/src/document_json.rs::write_json` 直呼び（`PlOStream` 経由）へ修正した——従来は `QPDFJob` の JSON job API を経由しており、この consumer が「`QPDFJob` を経由しない」という行の看板と矛盾していた。詳細は E-6 行を参照 |
| E-24 | `QPDFJob::writeJSON` の独立 free library 入口（qpdf 側に対応する public 識別子なし） | `libqpdf/QPDFJob.cc:3093-3116`（private） | 独立 free writer は撤去済み。canonical は `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_json` と `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_json_with_version` | `job/json.rs` の free `write_json` / `write_json_with_version` は宣言・re-export ともに 0。`crates/flpdf/tests/job_json_tests.rs` は `flpdf-3yn9.48.182` で削除済み（外部 crate から `pub(crate)` は見えないため） | canonical | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::write_json_with_version`（`pub`）→ `crates/flpdf/src/job/json.rs::write_json_with_version_with_logger`（`pub(crate)`） | `flpdf-xsq1` で統合テストの free-function consumer を canonical `QPDFJob::write_json` へ移し、job/json.rs の不要な free entrypoint 2 本と job/lib.rs の re-export を削除した。qpdf の document/job owner に対応する実装は lifecycle method と内部 serializer に一本化され、独立 bridge caller は残っていない。 |
| E-25 | `doJSON*` セクション builder の historical public path | `include/qpdf/QPDFJob.hh:551-565`（すべて private） | `crates/flpdf/src/job/json_sections.rs::build_pages_section_with_options`（`pub(crate)`、`crates/flpdf/src/job/json_sections.rs:189`）ほか | prod: 自クレート内のみ / test: — | canonical | `crates/flpdf/src/job/json_sections.rs` | 8 (C)（`flpdf-7bkv`）の staged migration は **完了済み** — `crates/flpdf/src/job/mod.rs` の `pub use json_sections::{build_*_section, ...}` は存在せず、`crates/flpdf/src/json_inspect.rs` の compatibility re-export ブロックも撤去され、**素の 6 個**（`build_pages_section` / `build_outlines_section` / `build_pagelabels_section` / `build_acroform_section` / `build_encrypt_section` / `build_attachments_section`）は `rg -n 'fn build_(pages\|outlines\|pagelabels\|acroform\|encrypt\|attachments)_section\b' crates/flpdf/src` が 0 件で宣言自体が存在せず、残るのは `_with_options` / `_with_version` 付きの `pub(crate)` 版のみ。`write_qpdf_json_v2_selected_objects_with_options` も workspace 全体で 0 件。8 (C) が close の blocker として挙げた `crates/flpdf/tests/document_json_tests.rs` が import するのは `flpdf::document_json::write_json`、`flpdf::json_inspect::{DecodeLevel, JsonKey, JsonOutputError, StreamDataMode}`、`flpdf::document_json::write_json` / `flpdf::json_inspect::{DecodeLevel, JsonOutputError, StreamDataMode}` / `flpdf::pipeline::PlString` / `flpdf::Pdf`（**2026-09-19 訂正**: 旧記載の `flpdf::job::{JsonJobOptions, JsonJobOutput, JsonStreamData, QPDFJob}` は `flpdf-3yn9.48.182` で除去済み）、`flpdf::pipeline::PlString`、`flpdf::Pdf` で、(C) の section builder は 1 つも含まれない — compat 経路を通らない。`flpdf-7bkv` は closed（2026-09-06 readback） |
| E-26 | `QPDFJob::handlePageSpecs` 内の AcroForm 刈り込み（qpdf 側に個別識別子なし） | `libqpdf/QPDFJob.cc:2610-2632` | `crates/flpdf/src/job/acroform_field_prune.rs::prune_acroform_after_subset`（`pub(crate)` free implementation）+ `crates/flpdf/src/job/page_specs.rs::QPDFJob::prune_acroform_after_subset`（pub メソッド、`crates/flpdf/src/job/page_specs.rs:765`） | free implementation prod: 2（いずれも同一 crate の `page_specs.rs`）/ test: module tests only | canonical | `crates/flpdf/src/job/page_specs.rs::QPDFJob::prune_acroform_after_subset` | `flpdf-xsq1` で crate 内委譲だけの free helper を `pub(crate)` に狭め、crate root/job module の public re-export を撤去した。qpdf の Job page-spec responsibility は public method owner に残り、独立 public bridge caller はない。 |
| E-27 | `QPDFObjectHandle::getParsedOffset` と stream DecodeParms type-warning の attribution（driver consumer） | `include/qpdf/QPDFObjectHandle.hh:419`, `libqpdf/QPDFObjectHandle.cc:1875-1882`, `qpdf/test_driver.cc` | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_get_parsed_offset` と stream `stream_data_filterable`/`pipe_stream_data` logger | prod: `try_get_parsed_offset` は `crates/flpdf/src/json/input.rs` 1箇所。qtest test 0/1の手製 parsed-offset callerは `.48.93` で0。bare `get_parsed_offset` は `flpdf-cli/src/main.rs` 1箇所と `flpdf-qtest-tools/src/metadata.rs` 2箇所など別consumerに残る / test: caller censusで確認 | canonical | `crates/flpdf/src/object_handle.rs::ObjectHandle::try_get_parsed_offset` + canonical stream pipe/logger | `.44`/`.25` でsource reparse/window/retry budgetを撤去し、`.48.93` でqtest stream warning/errorをcanonical pipe/loggerへ移した。qpdfの `typeWarning` が offending handleのdescription/parsed offsetと `QPDF::warn` loggerを使う順序を保持する。qpdfにないDecodeLimits/recovering wrapperは `.48.96` で撤去済みであり、このdriver routeのownerではない。 |
| E-28 | `qpdf/test_driver.cc` consumer（`QPDF` / `QPDFObjectHandle` / helper の public API を呼ぶ 99 ケース） | `qpdf/test_driver.cc:3540-3562` | `crates/flpdf-qtest-tools/src/driver/mod.rs::run` と `driver/*.rs` | prod: 1 bin（`crates/flpdf-qtest-tools/src/bin/driver.rs`）/ test: `driver_cli.rs`, `driver_goldens.rs`, `xref_parsedoffset_cli.rs` | mixed | case/API ごとの領域 A〜D owner（下記詳細表参照） | **2026-09-17 `.48.115` / `.48.116` / `.48.117` / `.48.118` / `.48.120` / `.48.121` / `.48.122` / `.48.123` / `.48.125` / `.48.127` / `.48.128` / `.48.133` / `.48.134` / `.48.135` / `.48.136` / `.48.137` / `.48.138` / `.48.139` / `.48.140` 反映**: 99 ケース全件（0/1 統合で 98 行）を imports・型経由メソッド・呼出し順序まで A〜D owner と照合し、現行は `canonical` 51 / `mixed` 48 / `bridge` 0 / `unknown` 0。test 2/3/4/5/6/7/8/9/11/17/19/21/31/34/38/42/46/48/50/52/68/71/72/73/75/85/86/87/89/92/97/98 のcaller-side `Pdf::resolve`・accessor bridge・driver-local重複walkは撤去済み。case 4 は qpdf public `isNull()` 対応の `try_is_null()`、case 7/8 は qpdf public `isStream()` 対応の resolving `type_code()`、case 9 は qpdf public `getRoot()` 対応の `root_handle()`へ移行したが、D1 writer境界のためcase-level分類はmixedのまま。詳細は「E-28 detail」表を参照。**2026-09-18 `.48.155` 反映**: 48 件の `mixed` case を driver 実装と 1 件ずつ再照合し、分類変更 0 件（集計は `canonical` 51 / `mixed` 48 / `bridge` 0 / `unknown` 0 のまま）を確認した。45 件は D1、残る 4 件は E-7（case 12/13）・E-17（case 83）・E-19（case 84）に固定される。stale な A7/A8/A9/E-1 参照の訂正と、A7/A8 が除外している qtest-driver hidden feature（`ObjectHandle::get_key` / `Pdf::resolve`）を使う 12 件の明示は「E-28 detail」表の各行に記載した。consumer 全体としてはmixedも残るため引き続き`mixed`。**2026-09-19（`flpdf-3yn9.48.165`）反映**: D1（`crates/flpdf/src/writer.rs::PdfWriter::write`、`d-writer.md`）を qpdf の `write()`/`doWriteSetup` と同じ 2 分岐＋PCLm 1 段構造に揃え `canonical` へ再分類した。D1 のみが mixed 根拠だった 30 ケース（4/7/8/9/10/15/16/18/29/32/33/40/41/44/51/52/53/55/56/57/58/59/60/63/69/74/76/77/78/79）を `canonical` へ再分類し、集計は `canonical` 81 / `mixed` 18 / `bridge` 0 / `unknown` 0（論理ケース、0/1 は 2 ケースとして数える）。**上の 2026-09-18 時点の「45 件は D1」は誤りで、実際は 43 件**（case 14 は D1 ではなく A17 の tombstone 掃除分岐に従属しており、当時「4 件」の内訳から漏れていた）。残る 18 件のうち 12 件は qtest-driver hidden feature（case 20/24/25/26/27/30/64/65/66/67/70/80）、2 件は E-7（case 12/13）、1 件は E-17（case 83）、2 件は E-19（case 45/84）、1 件は A17（case 14）に従属し引き続き `mixed`。consumer 全体としては引き続き `mixed`。**2026-09-19（`flpdf-3yn9.48.207`）反映**: qtest-driver hidden feature（`qtest-driver` feature 限定の `doc(hidden)` の非解決 `ObjectHandle::get_key`/明示 `Pdf::resolve`）に固定されていた残る 12 件（case 20/24/25/26/27/30/64/65/66/67/70/80）を resolving `try_get_key`/`try_get_array_item` チェーンへ置換し、`canonical` へ再分類した——`Pdf::resolve` が内部で呼ぶ `ObjectHandle::try_dereference` 自体は `pub(crate)` で crate 外の qtest-tools からは直接呼べないため、`try_dereference` への単純な置換ではなく、receiver を自ら解決する resolving accessor（`try_get_key` 等）へ移した箇所と、直後の呼び出しが receiver を解決するため explicit-resolve 自体が冗長だった箇所（case 25/80）の 2 パターンがある。全 12 件を qpdf 11.9.0 オラクルの実機出力と byte-identical であることを個別に確認した。集計は `canonical` 93 / `mixed` 6 / `bridge` 0 / `unknown` 0（論理ケース）。残る 6 件は E-7（case 12/13）・E-17（case 83）・E-19（case 45/84）・A17（case 14）に従属し引き続き `mixed`。consumer 全体としては引き続き `mixed`。 |
| E-29 | `QPDFJob::createQPDF` の入力オープン側（`processFile` → `doProcess` → `doProcessOnce`。`QPDF` 構築直後に必ず `setQPDFOptions` を適用してから読む） | `libqpdf/QPDFJob.cc:428-481`, `libqpdf/QPDFJob.cc:1793-1804`, `libqpdf/QPDFJob.cc:1695-1716`, `libqpdf/QPDFJob.cc:650-666`（`noWarn` → `setSuppressWarnings` は `libqpdf/QPDFJob.cc:663-665`） | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::open_document_with_description`、`open_with_description`、`open_for_encryption_inspection_with_description`、`open_job_source`。CLI の通常入力と secondary source はこれらの job boundary または同じ `PdfOpenOptions` policy を使う。multi-source `--pages` の page source open も `flpdf-3yn9.48.192` で旧 CLI 直書きの `open_page_source`（direct `open_file_with_options`、撤去済み）から `open_job_source` へ cutover した。JSON input は `QPDFJob::create_from_json` を通る | 旧 `Pdf::open_with_options` / `Pdf::create_from_json` route: job boundary からは prod 0 / test 0 だが、direct `Pdf::open_with_options` は意図的な exception route が残る（`python3 scripts/qpdf-route-callers.py --symbol open_with_options` は `crates/flpdf-cli/src/main.rs` に production caller 1 を報告する）。job の各 open boundary は job suppression を open 前に OR 済み。`run_copy_attachments_from` の attachment donor open も同じく direct `Pdf::open_with_options` を使う — qpdf の `copyAttachments`（`libqpdf/QPDFJob.cc:2100`）が donor を `processFile(other, ...)` で job 本体の main input slot と独立に開いており、donor を job 経由（`job.open_with_description`）で開くと `job.input_name()` が donor のパスで上書きされ、後続の duplicate-key エラー（`self.input_name()` を使用、qpdf の `pdf.getFilename()` @ `QPDFJob.cc:2127` に対応）が target ではなく donor を誤って名指すため。両 route とも同じ `suppress_warnings` option を明示適用する | mixed | `crates/flpdf/src/job/lifecycle.rs::QPDFJob::open_with_description` | qpdf の `doProcessOnce` 境界に合わせ、ordinary open、overlay/underlay、copy-encryption、encryption probe、attachment copy、page source、JSON input の open-time warning delivery を `--no-warn` で抑止する。warning collection と completion/exit status は保持する。この noWarn 欠落自体は `flpdf-3yn9.47` で closed。残る donor open とCLI orchestrationの分離は別責務で、`flpdf-44hb` の donorごとの verbose→open→copy 順序は Job JSON経路にも該当する。 |

E-17 row supersession note（2026-09-17）: 上記 E-17 行の `lifecycle.rs:1781`、11/124 集計、および `flpdf-3yn9.48.6` / `.48.7` 時点の「未実装」記述は履歴スナップショットであり、現行状態を表さない。現行の canonical raw argv initializer、全 qpdf option table、`@file` / top-level `--` / job-JSON occurrence 境界は、下記 `flpdf-3yn9.48.147` の節と option correspondence table によって上書きされる。production CLI consumer を切り替えていないため E-17/E-21 の classification は引き続き `mixed` である。


`flpdf-5nle` で attachment mutation の output boundary を更新した。`QPDFJob::handleTransformations` の
`addAttachments` / `removeEmbeddedFile` 相当の mutation 後、`run_all_attachment_mutations` は
`configure_attachment_job` / `run_configured_attachment_job` を通じて共通の
`top_level_writer_options` を
`QPDFJob::create_qpdf` → `QPDFJob::write_qpdf` へ渡す。content normalization は mutation 後に
行い、linearization と `linearize_pass1` も同じ Job writer に渡す。これは qpdf の
`writeQPDF` → `writeOutfile` → `setWriterOptions` の順序
（`libqpdf/QPDFJob.cc:484-507,2137-2248,2847-2945,3029-3058`）に対応する。

`flpdf-w0ne` では remove route の diagnostics も同じ E-9/E-4 境界へ揃える。
`run_all_attachment_mutations` の remove operation は mutation 成功時の `removed attachment <key>` を verbose info sink
へ送り、writer 完了後に `wrote file <output>` を送り、missing key は qpdf の
`attachment <key> not found` を raw bytes のまま返す（`libqpdf/QPDFJob.cc:2230-2241,3030-3062`）。

2026-09-10（`flpdf-0saq`）: E-5 の page-operation output で明示的な `--encrypt` を許可し、
qpdf の `writeQPDF` → `doSplitPages` → chunk ごとの `setWriterOptions`
（`libqpdf/QPDFJob.cc:483-511,2847-2903,2939-3027`）に対応させた。top-level と `rewrite`
の page-operation route は、ページ選択後の writer configuration へ encryption parameters を
渡す。`page_ops_qpdf_matrix.rs::pages_encrypt_then_split_outputs_encrypted_chunks_like_qpdf`
で qpdf 11.9.0 と 3 chunk の暗号化・byte parity を検証した。copy-encryption/decrypt と
その他の create-stage mutation の組合せは `flpdf-1emn` など別 scope に残す。

2026-09-14（flpdf-1emn follow-up slice）: rewrite の --decrypt を page-operation の writer boundary へ渡し、暗号化3ページfixtureの pages/rotate/split で qpdf の cleartext output と status/stdout/stderr/byte parity を固定した。copy_encryption の guard と、page-selection後の image/overlay 順序は残る。
2026-09-14（flpdf-1emn follow-up slice）: rewrite の --copy-encryption も page-operation の writer boundaryへ渡し、plaintext three-page fixtureとAES donorの pages/rotate/split で qpdf 11.9.0の復号後JSON parityを固定した。page-selection後の image/overlay 順序は残る。
2026-09-14（flpdf-1emn follow-up slice）: rewrite page-selection の post-plan 順序を page selection/rotation → underlay/overlay → image transformation に揃え、overlayで導入されたinline imageも optimize-images の対象にした。inline-image overlay fixtureのqpdf 11.9.0 byte parityを overlay_transform_order_route_tests で固定した。

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
`bridge` と `mixed` の両方の entrypoint に触れるケースは、逸脱の重い側である `bridge` を採る。
（この規則で `bridge` になっていた case 26/27 は、根拠だった A18 が `flpdf-3yn9.48.24` で
canonical になったため `mixed` に戻った。）

**2026-09-18 再照合（48 件の `mixed` case を driver 実装と 1 件ずつ突き合わせ）**:
2026-09-18 時点の triage は E-28 について文書の集計だけを再測定しており、48 件の
`mixed` case 個々を `crates/flpdf-qtest-tools/src/driver/*.rs` に対して再照合していなかった。
本パスでは 48 件すべての `run_test_N`（共有本体 `test_56_59_body` / `test_64_67_body` と
同一ファイル内のローカル helper を含む transitive closure）を読み、触れる flpdf entrypoint を
列挙して A〜D/E 行の**現在の**分類と突き合わせた。

結果は **分類変更 0 件**（48 件すべて `mixed` のまま）。固定要因の内訳は、D1
（`QPDFWriter::write` / `crates/flpdf/src/writer.rs::PdfWriter::write`、`d-writer.md` で依然
`mixed`）に固定される 45 件と、case 12/13 が E-7（`doInspection`、`mixed`）、case 83 が E-17
（`initializeFromJson`、`mixed`）、case 84 が E-19（`hasWarnings`、`mixed`）に固定される 4 件。
`PdfWriter` を構築しないのはこの 4 件だけであることを機械的に確認した。
逆向きの掃引（`canonical` 51 件側で `PdfWriter` を構築するもの）も行い、該当は case 54 と
case 75 の 2 件だけだった。case 54 は `PdfWriter::new` までで `.write()` を呼ばないため
D1 の対象外（同行の注記どおり）。**case 75 は `.write()` を呼びながら `canonical` のままで、
本表冒頭の分類規則（`mixed` な A〜D 行の entrypoint に触れるケースはそれに従う）の唯一の
例外になっている** ——同行は `erase-nntree.pdf` での qpdf 11.9.0 との byte-identical 一致を
根拠にしている。本 issue は `mixed` 行の再照合が対象なので case 75 の再分類は行わず、
規則と実態の食い違いとしてここに記録するにとどめる。したがって「D1 に触れる → `mixed`」は
本表の既定であって例外のない規則ではない。

一方、記載時点から該当行の分類が変わっていた参照は stale として訂正した ——A7・A8・A9 は
既に `canonical`（case 20/24/25/53）、E-1 も既に `canonical`（case 84）。あわせて case 45 の
「`PdfWriter::write` は canonical」という D1 行と矛盾する記述と、case 16 の D1 参照欠落を直した。

また、A7/A8 が自身の caller 数から「別 qtest session の例外」として除外している
qtest-driver hidden feature（`ObjectHandle::get_key` / `Pdf::resolve`、いずれも `qtest-driver`
feature 限定の `doc(hidden)`）を実際に使っている 12 件 ——case 20/24/25/26/27/30/64/65/66/67/70/80
——を特定し、各行に明記した。これらを resolving accessor（`try_get_key` /
`try_get_array_item` / `Pdf::root_handle`）へ移すのは分類の再照合ではなく実装 cutover なので、
本パスでは行わない。gated symbol の集合は `crates/flpdf/src` 全体の
`cfg(feature = "qtest-driver")` を走査して確定した ——ゲートは `ObjectHandle::get_key`・
`ObjectHandle::has_key`・`Pdf::resolve`・`pages::repair` の可視性切替の 4 箇所しかなく、
`has_key` は driver crate の `#[cfg(test)]` から、`pages::repair` は `tokenizer_runner` から
しか使われておらず、99 ケースのいずれからも到達しない。

case 63 と case 76/77 に残っていた「本パスでは個別未検証」の留保は、
当該 entrypoint に production caller が実在することを確認して解消した。

**2026-09-19（`flpdf-3yn9.48.165`）**: `d-writer.md` の D1（`QPDFWriter::write` /
`crates/flpdf/src/writer.rs::PdfWriter::write`）を `canonical` へ再分類した
（qpdf の `write()`/`doWriteSetup` と同じ 2 分岐＋PCLm 1 段構造に揃った）。上の
2026-09-18 パスは「45 件は D1」と記していたが、これは誤りで正しくは 43 件
——case 14 は D1 ではなく A17（`Pdf::swap_objects` の qpdf に無い tombstone
掃除分岐）に従属しており、「E-7/E-17/E-19 に固定される 4 件」の内訳から漏れて
いた。D1 のみが mixed 根拠だった 43 件のうち、qtest-driver hidden feature の
12 件（case 20/24/25/26/27/30/64/65/66/67/70/80）と E-19 に別途従属する 1 件
（case 45、`getWarnings()` 経由の write 時 warning exit(3) ゲート）を除く
**30 件を `canonical` へ再分類した**（4/7/8/9/10/15/16/18/29/32/33/40/41/44/
51/52/53/55/56/57/58/59/60/63/69/74/76/77/78/79）。各行に
`**2026-09-19（flpdf-3yn9.48.165）**` 注記を追加した。D1 以外の owner に従属
する 18 件（前述 12 件 + case 12/13 が E-7 + case 45/84 が E-19 + case 83 が
E-17 + case 14 が A17）は引き続き `mixed` のまま。

上（2026-09-18 パス）の「case 75 が『`.write()` を呼びながら `canonical`』で
本表冒頭の分類規則の**唯一の例外**」という記述も、この再分類で意味を失った
——「D1 に触れる → `mixed`」自体がもう規則ではないので、case 75 はもはや例外
ではなく、単に「D1 を含めどの owner にも従属しない `canonical` ケース」の 1
つになった。

ここで前提にしている本表の分類規則は「あるケースが直接呼ぶ A〜D/E
entrypoint が `mixed`/`bridge` ならそのケースも `mixed`/`bridge`」であって、
「そのケースの実行が内部で辿る flpdf 側のコードパス」ではない——D1
（`PdfWriter::write`）は非常に多くのケースから直接呼ばれるためこの基準の
対象になるが、D1 が内部で呼ぶ D2/D3/D11/D14/D16/D23 等（依然 `mixed`）は
対象にならない。これは新しい規則ではなく、上記 case 54（`PdfWriter::new` の
みで `.write()` を呼ばないため D1 の対象外）の扱いと同じ基準を D1 の再分類
後に適用しているだけである。

**2026-09-19（`flpdf-3yn9.48.207`）**: 上のパスが「実装 cutover なので本パス
では行わない」と留保していた qtest-driver hidden feature 12 件
（case 20/24/25/26/27/30/64/65/66/67/70/80）の cutover を実施した。**2026-09-19 訂正**: これを「hidden surface に紐づく残り全件」と読める書き方をしていたが、誤り——`driver/test_34_41.rs::resolved_key`（`:71-82`）と `driver/test_88_98.rs::resolved_key`（`:37-45`）が同じ bridge を内部で使っており、case 35/36 と 90/94 がそこを通る。これらは main 時点で既に `canonical` 判定なのでこの PR は分類を変えていないが、判定基準との食い違いとして `flpdf-j4n3a` で追跡する。本パスが cutover したのは上記 12 件である。
`ObjectHandle::get_key` は resolving `try_get_key` へ機械的に置換できた（**2026-09-19 訂正**: ここを当初「非解決 `get_key`」と書いたのは誤り。`get_key`（`object_handle.rs:4649-4652`）は `try_get_key` に委譲するので receiver は解決する。変わるのは `#[doc(hidden)]` な hidden surface か公開 `try_*` かという露出と、panic か `Result` 伝播かというエラー処理だけで、解決の有無ではない）。一方、明示 `Pdf::resolve`（case 24/25/80）は内部で呼ぶ
`ObjectHandle::try_dereference` 自体が `pub(crate)` で crate 外の
qtest-tools からは直接呼べないため、単純な関数置換にはならなかった:
case 24 は qpdf の `res1.getArrayItem(0).getArrayItem(1).getIntValueAsInt()`
（`qpdf/test_driver.cc:935-936`）に合わせて `try_get_array_item` の
resolving チェーンへ書き換え、case 25/80 は直後の呼び出し
（`try_get_key`／`AcroFormDocumentHelper::transform_annotations` 内部の
`try_as_array`）が receiver を自ら解決するため、独立した explicit-resolve
ステップ自体が元々冗長だったと判明し削除した（削除後の構造は qpdf 自身が
明示 resolve を持たないことと一致する）。全 12 件を qpdf 11.9.0 の実機出力
（`/usr/bin/qpdf` 11.9.0、pinned source と同一バージョン）と exit
status・stdout/stderr・生成 PDF のバイト列で比較し byte-identical を確認した。
12 件を `canonical` へ再分類し、E-28 自身の集計は `canonical` 92 /
`mixed` 7（論理ケース。0/1 統合で表は 98 行）になった。残る 7 件は
case 12/13 が E-7、case 14 が A17、case 45/84 が E-19、case 63 が D1/D16、
case 83 が E-17 で、いずれもこのパスの対象外（**2026-09-19 訂正**: 当初
93/6 と書き case 63 を落としていたが、詳細表の case 63 行は D16 依存を
理由に `mixed` のままで、表を数え直すと mixed は 12/13/14/45/63/83/84 の
7 件）。

| case | qpdf test fn | flpdf owner fn | classification | A-D/E owner refs / notes |
|---|---|---|---|---|
| 0/1 | `qpdf/test_driver.cc:201-286` | `crates/flpdf-qtest-tools/src/driver/test_0_1.rs::run_test_0_1` | canonical | `.48.93` で raw pipe → source-read-free `stream_data_filterable` probe → canonical filtered `pipe_stream_data`/loggerへ移行。DecodeParms warning、codec failure、Crypt identity、stdout/stderr orderingを58 fixture + 11 CLI probeでqpdf 11.9.0と比較。qpdfにない `DecodeLimits`/recovering wrapperは `.48.96` で撤去済みであり、qtest callerには残らない。 |
| 2 | `qpdf/test_driver.cc:286-308` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_2` | canonical | qpdf の `getKey` → `unparse`（`/O`・`/U`）と `pipeStreamData`（`/Contents`）の call orderを、resolving `try_get_key` と同じpipe経路をbuffer化する canonical `get_stream_data` で再現する。`QPDFObjectHandle::unparse` は間接値を参照形のまま返すため `/O`・`/U` はcaller-side resolveを行わず、`get_stream_data` がstream自身を解決する。`.48.107` で対象関数内の `resolve_handle` 3箇所を撤去し、test 2 differential/source guardで確認。 |
| 3 | `qpdf/test_driver.cc:311-322`; public `getArrayNItems`/`getArrayItem`: `include/qpdf/QPDFObjectHandle.hh:725-733`, `libqpdf/QPDFObjectHandle.cc:758-785` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_3` | canonical | qpdfと同じ`getArrayNItems` → `getArrayItem`のcount/item順を、resolving `try_get_array_n_items` → `try_get_array_item`で再現する。`pipe_stream_data` は C1/C3 canonical、`STREAM_ENCODE_NORMALIZE` は qpdf の `qpdf_ef_normalize` と一致。非配列時のwarning/空反復もqpdfのcount accessorに対応する。 |
| 4 | `qpdf/test_driver.cc:325-374` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_4` | canonical | `try_get_key` と qpdf public `isNull()` 対応の `try_is_null()` へ移行し、caller-side `resolve_handle` と非解決 `.is_null()` は撤去済み。`make_direct`/配列 mutation 系 API に A-D 行なし（対象範囲外）。記載時点の「`PdfWriter::write` は D1 mixed（4 分岐 vs qpdf 2 分岐）のためcase-level分類はmixed」は stale。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 5 | `qpdf/test_driver.cc:374-420` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_5` | canonical | qpdfの`getRoot`→`getKey`→`isArray`→`getArrayNItems`/`getArrayItem`→`getUTF8Value`/`getNumericValue`の各receiver境界を、`Pdf::root_handle`→`try_get_key`→`try_is_array`→`try_get_array_n_items`/`try_get_array_item`→`try_get_utf8_value`/`try_get_numeric_value`で再現した。`.48.138`でtest5のcaller-side `resolve_handle`と非解決`as_array` snapshotを撤去し、page/image/content helper routeは既存canonical production primitiveとして保持する。 |
| 6 | `qpdf/test_driver.cc:422-439` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_6` | canonical | qpdfのpublic `isStream()`（`libqpdf/QPDFObjectHandle.cc:437-440`）に対応する resolving `type_code()`で`/Metadata`のstream型を確認し、`pipeStreamData(..., qpdf_dl_none)`（`libqpdf/QPDFObjectHandle.cc:1300-1341`）に対応する canonical `pipe_stream_data`へ渡す。`.48.108` でcaller-side `resolve_handle`を撤去し、DecodeLevel::Noneの「復号はするがfilterしない」契約を保持。 |
| 7 | `qpdf/test_driver.cc:441-455` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_7` | canonical | qpdf public `isStream()`のreceiver解決に対応する`qstream.type_code()`を直接使い、`.48.139`でcaller-side `resolve_handle`を撤去した。`replace_stream_data` は C38 canonical。`PdfWriter::write` は D1 mixed。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 8 | `qpdf/test_driver.cc:457-493` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_8` | canonical | qpdf public `isStream()`のreceiver解決に対応する`qstream.type_code()`を直接使い、`.48.139`でcaller-side `resolve_handle`を撤去した。`replace_stream_data_provider` は C38 canonical。`PdfWriter::write`（`set_linearization(true)`）は D1 mixed。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 9 | `qpdf/test_driver.cc:495-519` | `crates/flpdf-qtest-tools/src/driver/test_02_09.rs::run_test_9` | canonical | qpdf public `QPDF::getRoot()`に対応する`Pdf::root_handle()`へ移行し、caller-side `resolve_handle`と直接trailer `/Root`取得を`.48.140`で撤去した。`get_stream_data`/`replace_stream_data` は C4/C5/C38 canonical。`new_stream_with_data`/`new_stream` に A-D 行なし。`PdfWriter::write` は D1 mixed。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 10 | `qpdf/test_driver.cc:522-537` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_10` | canonical | `ObjectHandle::add_page_contents` に A-D 行なし（CLAUDE.md (B) の入れ物代替、doc comment 記載済み）。`PdfWriter::write` は D1 mixed。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 11 | `qpdf/test_driver.cc:538-550`; `QPDF::getRoot`: `libqpdf/QPDF.cc:2354-2367`; stream reads: `libqpdf/QPDFObjectHandle.cc:1288-1298`, `libqpdf/QPDF_Stream.cc:362-376` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_11` | canonical | qpdfのpublic `getRoot`に対応する`Pdf::root_handle`でdirect/indirect Catalogを解決し、`try_get_key` → `get_stream_data` / `get_raw_stream_data`のcanonical stream accessorsを使う。`root_ref` → `get_object_handle`のqpdf-less identity projectionはこのcaseから撤去し、`stream-data.pdf`のfiltered/raw outputはqpdf `test11.out`と一致。 |
| 12 | `qpdf/test_driver.cc:553-569` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_12` | mixed | `linearization::show_linearization_pdf_with_warnings`（`crates/flpdf/src/linearization/show.rs:1138`）は `QPDFJob::show_linearization`（`crates/flpdf/src/job/lifecycle.rs:3167-3182`）が呼ぶのと**同一の canonical primitive**（grep で確認済み、qtest 専用ラッパーではない）。qpdf 側は `doInspection` の `show_linearization` 分岐に対応 — E-7（`doInspection`、mixed）に従属するため `mixed`。 |
| 13 | `qpdf/test_driver.cc:570-591` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_13` | mixed | case 12 と同じ `show_linearization_pdf_with_warnings` core。E-7 に従属し `mixed`。 |
| 14 | `qpdf/test_driver.cc:592-660` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_14` | mixed | A17 が本ファイル（`crates/flpdf-qtest-tools/src/driver/test_10_17.rs:295,323`）を `Pdf::swap_objects` の production caller として明記済み、`mixed`（qpdf に無い tombstone 掃除分岐あり）。 |
| 15 | `qpdf/test_driver.cc:661-744` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_15` | canonical | `PageDocumentHelper::get_all_pages`/`add_page`/`add_page_at` に A-D 行なし（owned-snapshot vs qpdf live-vector、CLAUDE.md (B)、doc comment 記載済み）。`PdfWriter::write` は D1 mixed。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 16 | `qpdf/test_driver.cc:745-776` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_16` | canonical | `flpdf-83jc` で全件移植済み。qpdf の `getAllPages()` は自身の cache への live reference を返すため同じ binding を読み直すが、`PageDocumentHelper::get_all_pages` は owned snapshot を返す（case 15/18 と同じ CLAUDE.md (B) 逸脱）ので、`Pdf::update_all_pages_cache`（`pub`、`crates/flpdf/src/pdf.rs:369`）の後に再取得して同じ refreshed 状態を観測する。これで後半の 3 assert と `a.pdf` 書き出しまで qpdf と同じ順序で通る。**2026-09-18 再照合**: `PdfWriter::write` を呼ぶため D1 mixed に固定される（記載時点の本行は D1 参照を欠いていた）。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 17 | `qpdf/test_driver.cc:776-795`; public `QPDF::getRoot`: `include/qpdf/QPDF.hh:311-313`, `libqpdf/QPDF.cc:2354-2367`; public `getKey` / `getArrayItem`: `include/qpdf/QPDFObjectHandle.hh:725-728,762-768`, `libqpdf/QPDFObjectHandle.cc:253-267,758-785,978-989` | `crates/flpdf-qtest-tools/src/driver/test_10_17.rs::run_test_17` | canonical | qpdfの resolving `getRoot` → `getKey("/Pages")` → `getKey("/Kids")` → `getArrayItem(0/1)` 順を、`Pdf::root_handle` → `try_get_key` → `try_get_array_item` で再現する。qpdfにない `root_ref` → `get_object_handle` Catalog identity投影と非解決 `as_array` は撤去。`PageDocumentHelper::get_all_pages`/`remove_page` の重複ページ修復・warning drain、`/Contents` identity、filtered stream確認は保持する。`page_api_2.pdf` の qpdf/flpdf driver出力は exit 0・158 bytesで一致し、indirect `/Kids` の resolving accessor 回帰も追加した。 |
| 18 | `qpdf/test_driver.cc:796-817` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_18` | canonical | `PageDocumentHelper` 呼び出しに A-D 行なし（owned-snapshot vs qpdf live-cache、CLAUDE.md (B)）。`PdfWriter::write` は D1 mixed。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 19 | `qpdf/test_driver.cc:818-832`; public `getKey`: `include/qpdf/QPDFObjectHandle.hh:762-768`, `libqpdf/QPDFObjectHandle.cc:978-989` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_19` | canonical | 末尾の`/Contents` objgen比較をqpdfのresolving `getKey`に対応する`try_get_key`で実行する。PageDocumentHelperのowned-snapshot再取得とindirect `/Contents` identity比較は保持し、writerは呼ばない。test 21のshallow-copy error用explicit resolveは別scope。 |
| 20 | `qpdf/test_driver.cc:835-851` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_20` | canonical | `shallow_copy`/`append_array_item` に A-D 行なし。**2026-09-19（`flpdf-3yn9.48.207`）**: qtest-driver hidden feature の非解決 `ObjectHandle::get_key`（`qtest-driver` feature 限定の `doc(hidden)`、内部は `try_get_key` へ委譲済み）を撤去し、resolving `try_get_key` へ置換した（`crates/flpdf-qtest-tools/src/driver/test_18_25.rs:141,143`、qpdf 側は `qpdf/test_driver.cc:839,842` の `trailer.getKey`）。D1（`PdfWriter::write`）が `flpdf-3yn9.48.165` で `canonical` へ再分類済みのため、この置換により case-level 分類も `canonical` になる。 |
| 21 | `qpdf/test_driver.cc:852-862`; public `getKey`: `include/qpdf/QPDFObjectHandle.hh:762-768`, `libqpdf/QPDFObjectHandle.cc:978-989`; public `shallowCopy`: `include/qpdf/QPDFObjectHandle.hh:874-881`, `libqpdf/QPDFObjectHandle.cc:2073-2079` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_21` | canonical | qpdf の resolving `getKey` と resolving `shallowCopy` に対応する `try_get_key` → `shallow_copy` を使い、caller-side `Pdf::resolve` を撤去した。`QPDF_Stream::copy` の `stream objects cannot be cloned`（`libqpdf/QPDF_Stream.cc:141-145`）と未到達 footer を保持し、writer は呼ばない。 |
| 22 | `qpdf/test_driver.cc:863-874` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_22` | canonical | `PageDocumentHelper::get_all_pages`/`remove_page` — A-D 行なし（対象範囲外）だが case 5/26 と同型の共有 production primitive、既知の逸脱なし。 |
| 23 | `qpdf/test_driver.cc:875-882` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_23` | canonical | case 22 と同じ `PageDocumentHelper` primitive のみ、既知の逸脱なし。 |
| 24 | `qpdf/test_driver.cc:883-947` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_24` | canonical | `new_reserved`/`replace_reserved`/`make_direct`/直接 `replace_key` に A-D 行なし（reserved-object lifecycle は対象範囲外）。**2026-09-19（`flpdf-3yn9.48.207`）**: qtest-driver hidden feature の明示 `Pdf::resolve`（`qtest-driver` feature 限定の `doc(hidden)`。内部は `handle.try_dereference()` のみで、その `try_dereference` 自体は `pub(crate)` のため crate 外の qtest-tools からは直接呼べない）を 2 箇所とも撤去した。旧コードは `pdf.resolve(&res1_first)?` の後で非解決 `as_array()` スナップショットを読んでいたが、qpdf の `res1.getArrayItem(0).getArrayItem(1).getIntValueAsInt()`（`qpdf/test_driver.cc:935-936`）は各ホップの receiver を `getArrayItem`/`getIntValueAsInt` 自身が解決する構造なので、`res1.try_get_array_item(0)?.try_get_array_item(1)?.try_get_int_value_as_int()?` という resolving accessor チェーンへ書き換え、独立した explicit-resolve ステップ自体を無くした（`crates/flpdf-qtest-tools/src/driver/test_18_25.rs:326-333`）。qpdf 11.9.0 オラクル（`minimal.pdf`、`reserved-objects.out`/`reserved-objects.pdf`）と exit 0・stdout・出力 PDF とも byte-identical であることを確認済み。D1（`PdfWriter::write`）が `flpdf-3yn9.48.165` で `canonical` へ再分類済みのため、この置換により case-level 分類も `canonical` になる。 |
| 25 | `qpdf/test_driver.cc:948-976` | `crates/flpdf-qtest-tools/src/driver/test_18_25.rs::run_test_25` | canonical | `copy_foreign_object` に A-D 行なし。**2026-09-19（`flpdf-3yn9.48.207`）**: 2 つの qtest-driver hidden feature（`qtest-driver` feature 限定の `doc(hidden)`）をどちらも撤去した。非解決 `ObjectHandle::get_key` は 2 箇所とも resolving `try_get_key` に置換した（`crates/flpdf-qtest-tools/src/driver/test_18_25.rs:391,403`；qpdf は `oldpdf.getTrailer().getKey("/QTest")`/`oldpdf.getRoot().getKey("/Pages")`、`qpdf/test_driver.cc:964,967`）。明示 `Pdf::resolve` はもともと直後の（当時は非解決の）`get_key` 呼び出しの前に置かれていただけで、`try_get_key` 自身が receiver を解決してから `/Pages` を読むため独立した resolve ステップは不要と判明し、そのまま削除した——qpdf 側にも対応する明示 resolve は無く `getRoot().getKey(...)` の連鎖が receiver を暗黙に解決する。qpdf 11.9.0 オラクル（`minimal.pdf`/`copy-foreign-objects-in.pdf`、`copy-foreign-objects-25.out`/`copy-foreign-objects-out1.pdf`）と exit 0・stdout・出力 PDF とも byte-identical であることを確認済み。D1（`PdfWriter::write`）が `flpdf-3yn9.48.165` で `canonical` へ再分類済みのため、この置換により case-level 分類も `canonical` になる。 |
| 26 | `qpdf/test_driver.cc:977-1002` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_26` | canonical | `PageDocumentHelper::add_page`、`Pdf::copy_foreign_object`、`ObjectHandle::replace_key`、`PdfWriter` — A-D 行なし（対象範囲外）だが `merge_documents` 等が広く共有する production primitive。**2026-09-19（`flpdf-3yn9.48.207`）**: qtest-driver hidden feature の非解決 `ObjectHandle::get_key`（`qtest-driver` feature 限定の `doc(hidden)`、内部は `try_get_key` へ委譲済み）を撤去し、resolving `try_get_key` へ置換した（`crates/flpdf-qtest-tools/src/driver/test_26_33.rs:133`）。qpdf 11.9.0 オラクル（`minimal.pdf`/`copy-foreign-objects-in.pdf`、`copy-foreign-objects-26.out`/`copy-foreign-objects-out2.pdf`）と exit 0・stdout・出力 PDF とも byte-identical、かつ新規回帰テスト `test_26_copies_o3_page_and_qtest_without_crossing_page_boundaries` で確認済み。D1（`PdfWriter::write`）が `flpdf-3yn9.48.165` で `canonical` へ再分類済みのため、この置換により case-level 分類も `canonical` になる。 |
| 27 | `qpdf/test_driver.cc:1003-1078` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_27` | canonical | `replace_stream_data_with_callback` は C38 canonical。`copy_foreign_object`/`set_immediate_copy_from`/`PdfWriter` に A-D 行なし。provider-copy の入れ物代替は CLAUDE.md (B) として doc comment に記載済み。**2026-09-19（`flpdf-3yn9.48.207`）**: qtest-driver hidden feature の非解決 `ObjectHandle::get_key` を 2 箇所とも resolving `try_get_key` へ置換した（`crates/flpdf-qtest-tools/src/driver/test_26_33.rs:225-226`）。qpdf 11.9.0 オラクル（`minimal.pdf`/`copy-foreign-objects-in.pdf`、`copy-foreign-objects-27.out`/`copy-foreign-objects-out3.pdf`）と exit 0・stdout・出力 PDF とも byte-identical、かつ既存の `test_27_writes_live_trailer_copies_and_provider_streams` で確認済み。D1（`PdfWriter::write`）が `flpdf-3yn9.48.165` で `canonical` へ再分類済みのため、この置換により case-level 分類も `canonical` になる。 |
| 28 | `qpdf/test_driver.cc:1079-1096` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_28` | canonical | `copy_foreign_object` のエラー経路のみ、case 26/27 と同一 canonical primitive。 |
| 29 | `qpdf/test_driver.cc:1097-1147` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_29` | canonical | `copy_foreign_object`、`PdfWriter::write`、`ObjectHandle::replace_key` の所有権チェック。`Error::Internal` は `std::logic_error` 対応（`.claude/rules/qpdf-port-design-patterns.md` §3）と一致。既知の逸脱なし。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 30 | `qpdf/test_driver.cc:1148-1173` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_30` | canonical | `writer_copy_encryption_source` は canonical boundary `crates/flpdf/src/reader.rs::Pdf::writer_copy_encryption_source` を再利用（doc comment 明記、独自 snapshot ではない）。`PdfWriter::copy_encryption_parameters` に A-D 行なし。**2026-09-19（`flpdf-3yn9.48.207`）**: ローカル helper `page_contents`（`crates/flpdf-qtest-tools/src/driver/test_26_33.rs:80`）の qtest-driver hidden feature の非解決 `ObjectHandle::get_key` を resolving `try_get_key` へ置換した。qpdf 11.9.0 オラクル手順（`qpdf/qtest/encryption.test` の "copy encryption parameters" ケースと同じ手順を再現: `qpdf --encrypt user owner 128 --use-aes=y --extract=n -- minimal.pdf a.pdf` で暗号化 donor を作り、`test_driver 30 minimal.pdf a.pdf` を実行）で検証した。stdout は `test 30 done\n` で exit 0、出力 `b.pdf` を `qpdf --show-encryption b.pdf --password=owner` した結果は qpdf 自身の `copied-encryption.out` と byte-identical（qpdf の当該 qtest ケース自体が `b.pdf` を直接 `cmp` せずこの `--show-encryption` 経由でのみ検証するため、本 note もそれに倣う）。かつ既存の `test_30_consumes_the_canonical_copy_encryption_source` で確認済み。D1（`PdfWriter::write`）が `flpdf-3yn9.48.165` で `canonical` へ再分類済みのため、この置換により case-level 分類も `canonical` になる。 |
| 31 | `qpdf/test_driver.cc:1174-1214` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_31` | canonical | `ObjectHandle::parse`/`parse_with_description`/`parse_with_context` — 対象範囲外だが crate 唯一の production parse entrypoint。`Error::Internal` vs `Error::Parse` は qpdf の `std::logic_error` vs `std::runtime_error` 分岐と一致。間接 null の判定は qpdf public `isNull()`（`libqpdf/QPDFObjectHandle.cc:353-356`）に対応する public `ObjectHandle::try_is_null`（`crates/flpdf/src/object_handle.rs:3002`）が lazy dereference を担い、caller-side `Pdf::resolve` は残さない。既存の `is_direct` 検証は保持する。 |
| 32 | `qpdf/test_driver.cc:1215-1236` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_32` | canonical | `PdfWriter`（`set_linearization`/`set_compress_streams`/`set_extra_header_text`）のみ、production API。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 33 | `qpdf/test_driver.cc:1237-1251` | `crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_33` | canonical | `PdfWriter::set_output_pipeline` にカスタム `Pipeline` 実装。sink の入れ物代替のみで CLAUDE.md (B) として doc comment に記載済み、挙動変化なし。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 34 | `qpdf/test_driver.cc:1252-1265` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_34` | canonical | qpdfの`getPDFVersion` → `getExtensionLevel` → `getRoot().getKey("/Extensions").unparse()` → `getVersionAsPDFVersion`の順を、`Pdf::version` → `get_extension_level` → `root_handle` → `try_get_key` → qpdf同様の非解決`unparse` → `get_version_as_pdf_version`で再現する。`catalog_extension_level`とcase34のdriver-local root/key walkを削除し、clamp warningの同期出力もcanonical API経由で保持した。`PdfVersion`の`major`/`minor`の`u8`幅とoverflow扱いはqpdf（`int` + `QUtil::string_to_int`の`range_error`）と異なり、別issueのまま。 |
| 35 | `qpdf/test_driver.cc:1266-1312` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_35` | canonical | `root_handle`/`resolved_key` の resolve/get_key chain、`get_stream_data(DecodeLevel::Generalized)` は C4/C5 canonical。`BTreeMap` 順序は qpdf の `std::map` 反復順と一致（忠実、逸脱でない）。 |
| 36 | `qpdf/test_driver.cc:1313-1340` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_36` | canonical | case 35 と同じ root/key chain、`get_raw_stream_data` は C6 canonical、`PlFlate` は canonical pipeline stage。qpdf の raw `Pl_Flate(a_inflate)` + `qpdf_dl_none` を正確に再現。 |
| 37 | `qpdf/test_driver.cc:1341-1350` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_37` | canonical | `PageDocumentHelper::get_all_pages`、`ObjectHandle::parse_page_contents` + `ObjectHandleParserCallbacks`。qpdf の `ParserCallbacks`/`terminateParsing`（`/Abort` → `ParseControl::Stop`）を inline-image 特殊ケース含め正確に再現。対象範囲外だが production API。 |
| 38 | `qpdf/test_driver.cc:1351-1360` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_38` | canonical | qpdfの`getRoot` → `getKey("/QTest")` → `getArrayNItems`/`getArrayItem` → `unparseResolved`を、`Pdf::root_handle` → `try_get_key` → `try_get_array_n_items`/`try_get_array_item` → `try_unparse_resolved`で再現する。qpdf-less driver-local `root_handle`/`resolved_key`/`resolved_terminal`、非解決`as_array`、非fallible`unparse_resolved`はcase38から撤去し、case34/35/36のdiagnostic helper scopeは保持する。 |
| 39 | `qpdf/test_driver.cc:1361-1377` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_39` | canonical | qpdfの`QPDFPageObjectHelper::getImages`に対応する`PageObjectHelper::get_images`でdirect image XObjectを列挙し、`try_get_stream_dict` → `try_get_key` → `try_unparse_resolved`でfilter/color-spaceを読む。qpdf-less `get_resources(false)`/`resolve_once`/非解決dict・stream accessorsはcase39から撤去し、qpdfのresource traversal owner（`libqpdf/QPDFPageObjectHelper.cc:318-383`）へ委譲する。 |
| 40 | `qpdf/test_driver.cc:1378-1391` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_40` | canonical | `PdfWriter`（`set_pclm`/`set_static_id`）のみ、production API。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 41 | `qpdf/test_driver.cc:1392-1406` | `crates/flpdf-qtest-tools/src/driver/test_34_41.rs::run_test_41` | canonical | `PageDocumentHelper::get_all_pages`、`ObjectHandle::add_content_token_filter` + `TokenFilter` 実装（canonical trait）。合成トークンの raw byte 表現は `tokenizer.rs` の canonical escaping 規則で検証済み（捏造でない）。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 42 | `qpdf/test_driver.cc:1407-1551` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_42` | canonical | qpdfの`getKey(...).getDict()`境界を、trailer/key/page/contentsの resolving `try_get_key` と `try_get_stream_dict` で逐語的に再現する。warning-producing `try_*` accessor family（`try_is_array`/`try_get_name` 等）+ `Rectangle`/`Matrix` は既存canonical ownerを使用し、対象関数内のcaller-side `Pdf::resolve`は0。`flpdf-3yn9.48.106` でbounded cutover済み。 |
| 43 | `qpdf/test_driver.cc:1552-1611` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_43` | canonical | `AcroFormDocumentHelper`/`FormFieldObjectHelper`/`AnnotationObjectHelper` — canonical production document-helper port。field loop 前に eager `analyze()` 相当を drain（qpdf のコンストラクタ時解析と一致、doc comment 明記）。 |
| 44 | `qpdf/test_driver.cc:1612-1631` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_44` | canonical | `AcroFormDocumentHelper::get_form_fields`、`FormFieldObjectHelper::set_value_string`/`field_type`/`fully_qualified_name`/`value_as_string`、`PdfWriter`（QDF/static-id/suppress-original-ids）。既知の逸脱なし。。本表の分類規則（A〜D の既存行が `mixed`/`bridge` と判定した entrypoint に触れるケースはそれに従う）により、`writer.write()`（D1 mixed） を呼ぶため `mixed`。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 45 | `qpdf/test_driver.cc:1632-1645` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_45` | mixed | `PdfWriter::write` は D1 mixed のため case-level 分類は `mixed`（**2026-09-18 再照合**: 記載時点の「`PdfWriter::write` は canonical」は D1 行と矛盾していたので訂正した。`crates/flpdf-qtest-tools/src/driver/test_42_49.rs:542-545` が `PdfWriter::new` → `write` を呼ぶ）。加えて既知・追跡済みギャップ: `Pdf::repair_diagnostics()` は open 時診断に限らず、writer の stream warning も各ハンドルの resolver 経由で同じ collection に入る（`writer.rs:6540-6592` の `pdf_writer_reprocesses_an_invalid_compression_level_without_filtering` が `write()` 後に両方の write 時 warning を確認している）。case 45 は `getWarnings()` が非空かを見るだけなので現行 API で移植可能。残るギャップは write 時 warning による `exit(3)` ゲートで、E-19 と `flpdf-3yn9.48.26` が追跡する。 |
| 46 | `qpdf/test_driver.cc:1646-1784` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_46` | canonical | `NumberTree`/`NumberTreeCursor`（`nntree.rs`）— `QPDFNumberTreeObjectHelper`/その iterator の直接 port（wrap 挙動・値エイリアシングまで一致、doc comment 明記）。値の消費は qpdf の public `getStringValue`（`libqpdf/QPDFObjectHandle.cc:659-677`）に対応する `ObjectHandle::try_get_string_value`、`/Kids` 判定は `getKey("/Kids").getArrayItem(0).isIndirect()` に対応する `try_get_key` → `try_get_array_item` へ `.48.103` で移行し、明示 `Pdf::resolve` は残らない。 |
| 47 | `qpdf/test_driver.cc:1785-1798` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_47` | canonical | qpdfの`getRoot` → `getKey("/Pages")` → `getKey("/Count")` → `getIntValue`を、`Pdf::root_handle` → `try_get_key` → `try_get_key` → `try_get_int_value`でreceiver境界ごとに再現してから`PageLabelDocumentHelper::get_labels_for_page_range`へ渡す。pair 型の戻り値により qpdf の flat-vector 等価性チェックが tautological（逸脱ではない）。`checked_sub` は qpdf の unguarded だが等価な underflow 形状に一致。 |
| 48 | `qpdf/test_driver.cc:1799-1923` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_48` | canonical | `NameTree`/`NameTreeCursor` — `QPDFNameTreeObjectHelper`/その iterator の直接 port、case 46 と同じ wrap/エイリアシングパターン。文字列値は qpdf の public `getStringValue`/`getUTF8Value`（`libqpdf/QPDFObjectHandle.cc:659-689`）に対応する `try_get_string_value`/`try_get_utf8_value` へ `.48.103` で移行し、明示 `Pdf::resolve` は残らない。 |
| 49 | `qpdf/test_driver.cc:1924-1939` | `crates/flpdf-qtest-tools/src/driver/test_42_49.rs::run_test_49` | canonical | `OutlineDocumentHelper::get_tree`/`OutlineItem::get_title`/`get_dest` — tree 構築の副作用順序（qpdf コンストラクタ時 `/Outlines` walk）を `get_tree` を page listing 前に呼ぶことで保存（`outline_object_helper.rs:266-285` 引用、doc comment 明記）。 |
| 50 | `qpdf/test_driver.cc:1939-1953`; public `getKey` / `mergeResources` / `getResourceNames`: `include/qpdf/QPDFObjectHandle.hh:762-768,794-835`, `libqpdf/QPDFObjectHandle.cc:978-989,1063-1153,1156-1170` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_50` | canonical | qpdf の `getTrailer().getKey` → `mergeResources` → non-resolving top-level `getJSON` → type-mismatch `mergeResources(getKey("/k1"))` → `getResourceNames` の順を、`trailer_key_handle` → `merge_resources` → `pdf_object_to_json` → resolving `try_get_key` → `merge_resources` → `get_resource_names` で再現する。qpdf-less caller-side `Pdf::resolve` 3箇所と hidden direct `get_key` routeを撤去し、merge/get-resource primitive自身の解決・診断境界へ戻した。pinned qpdf/flpdf `merge-dict.pdf` は双方 exit 0、stdout/stderr byte-identical。 |
| 51 | `qpdf/test_driver.cc:1956-1999` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_51` | canonical | A7のcaller-side `resolve_and_drain`、非解決`get_key`/`as_array`、ObjectRef-only `FormFieldObjectHelper::new`を`.48.135`で撤去し、`Pdf::root_handle` → `try_get_key` → `try_get_array_n_items`/`try_get_array_item` → `try_is_string`/`try_get_utf8_value`と`FormFieldObjectHelper::from_object_handle`へ移行した。qpdfがdirect/null handleへ`setV`する契約と各操作後のwarning順を保持。`.48.136`でcase52の最後の共有helperもcaller-zero後に削除し、case51にはqpdf-less A7 bridgeを残さない。D1 `writer.write()`は混在するためcase-level分類はmixedのまま。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 52 | `qpdf/test_driver.cc:2000-2024` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_52` | canonical | qpdfの`getRoot` → `getKey` → `getArrayNItems`/`getArrayItem` → `getKey`/`isString`/`getUTF8Value`の各accessor境界を、`Pdf::root_handle` → resolving `try_*` accessors → `FormFieldObjectHelper::from_object_handle`へ移行した。`.48.136`でcase52の`resolve_and_drain`、非解決`get_key`/`as_array`、ObjectRef-only helper、`FIELD_MUST_BE_INDIRECT`をcaller-zero確認後に撤去。直接 `/Fields` のtext field回帰、pinned qpdf/flpdfのappearance-streams 27/27、raw non-UTF-8 argのstatus/stdout/stderr、qdf canonicalized outputを一致確認。D1 `writer.write()`は混在するためcase-level分類はmixedのまま。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 53 | `qpdf/test_driver.cc:2025-2043` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_53` | canonical | D1（`preserve_unreferenced_objects` 付き writer）に固定される。A7 の使用なし（`.resolve()` 呼び出しなし）。**2026-09-18 再照合**: 記載時点の「A9（`Pdf::get_all_objects`、mixed）」は stale ——A9 は既に `canonical`。残る mixed 根拠は D1 のみ。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 54 | `qpdf/test_driver.cc:2044-2056` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_54` | canonical | `Pdf::version()`（対象範囲外の bare accessor）と writer 設定 API（`.write()` は呼ばない、D1 の対象外）。A7/D1 いずれにも触れない（A18 は `flpdf-3yn9.48.24` で撤去済み）。qpdf の `getFinalVersion` 二重呼び出し挙動を doc comment で明示的に再現。 |
| 55 | `qpdf/test_driver.cc:2057-2115` | `crates/flpdf-qtest-tools/src/driver/test_50_55.rs::run_test_55` | canonical | D1（A18 は `flpdf-3yn9.48.24` で撤去済み）。`PageDocumentHelper::get_all_pages`/`PageObjectHelper::get_form_xobject_for_page`（`QPDFPageObjectHelper.cc:706-733` 引用、対象範囲外）。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 56 | `qpdf/test_driver.cc:2116-2121` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_56`（`test_56_59_body` 委譲） | canonical | D1 のみ — 共有本体に明示的な `.resolve()` 呼び出しなし。ページコピー/配置は `PageObjectHelper`/`Pdf::copy_foreign_object`（対象範囲外）。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 57 | `qpdf/test_driver.cc:2122-2127` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_57`（`test_56_59_body` 委譲） | canonical | 同上、D1 のみ。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 58 | `qpdf/test_driver.cc:2128-2133` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_58`（`test_56_59_body` 委譲） | canonical | 同上、D1 のみ。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 59 | `qpdf/test_driver.cc:2134-2139` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_59`（`test_56_59_body` 委譲） | canonical | 同上、D1 のみ。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 60 | `qpdf/test_driver.cc:2140-2215` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_60` | canonical | D1（A18 は `flpdf-3yn9.48.24` で撤去済み）。`merge_resources`/`get_unique_resource_name`/`shallow_copy`/`make_resources_indirect`（対象範囲外、doc comment が「公開 canonical な resource/live-trailer 経路」と明記）。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 61 | `qpdf/test_driver.cc:2216-2262` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_61` | canonical | `.resolve()`/writer なし。`Error` variant マッピング、`qutil::safe_fopen`/`int_to_string_base`/`to_utf8`、`ReadSeek::as_any` downcast、`Discard` pipeline（canonical `Pipeline` trait）、`NameTree::new`+`Drop`。qpdf の呼出し順序移植ではなく例外クラス境界の Rust-native 再現と doc comment が明記。 |
| 62 | `qpdf/test_driver.cc:2263-2289` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_62` | canonical | `.resolve()`/writer なし。`Pdf::trailer()`（memoized live handle）+ `try_get_int_value`/`try_get_uint_value`/`try_get_int_value_as_int`/`try_get_uint_value_as_uint`（A6 `try_*` canonical family）。`QPDFLogger::default_logger()` エラーキャプチャは正当な logger 機構。 |
| 63 | `qpdf/test_driver.cc:2290-2342` | `crates/flpdf-qtest-tools/src/driver/test_56_63.rs::run_test_63` | mixed | **2026-09-19 訂正**: D1 だけではない。`run_test_63` は `PdfWriter::set_encryption_parameters`（`test_56_63.rs:637`）を直接呼んでおり、`setEncryptionParameters*` 族を所有する D16（`d-writer.md:467`、`mixed`）に従属する。D1 の canonical 化だけでは本ケースは canonical にならないため `mixed` を維持する。 **2026-09-18 再照合**: 記載時点の「本パスでは個別再検証せず」の留保を解消した ——`EncryptParams::v5_r6`（`crates/flpdf/src/encryption.rs::EncryptParams`）は qtest 専用ではなく、`crates/flpdf/src/job/lifecycle.rs:970`・`crates/flpdf/src/job/argv.rs:1712`・`crates/flpdf-cli/src/main.rs:6061` の production caller と共有される。受け側 `PdfWriter::set_encryption_parameters` も `crates/flpdf-cli/src/main.rs:585` が使う。qpdf の `setR6EncryptionParameters`（`qpdf/test_driver.cc:2298`）に対応する A-D 行は無いが、本表の「no A-D row」規則を満たす canonical primitive。当時「`mixed` 根拠は D1 のみ」と記したが、この判断は本行冒頭の 2026-09-19 訂正（D16 従属の発見）で失効した——現在の `mixed` 根拠は D16。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類された。当初はこれに伴い本ケースの case-level 分類も `canonical` へ更新したが、**同日中のレビュー（本行冒頭の 2026-09-19 訂正、`236e5277af`）で D16 従属が判明し `mixed` へ差し戻した**。`flpdf-3yn9.48.198`: 本行の notes 末尾がこの差し戻しの後もなお「`canonical` へ更新した」と読めるまま残っていたための表記修正で、classification セル自体は `236e5277af` の時点から一貫して `mixed`。 |
| 64 | `qpdf/test_driver.cc:2343-2348` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_64`（`test_64_67_body` 委譲） | canonical | 同じ qpdf 構文（`resources.getKey("/XObject").replaceKey(...)`、`qpdf/test_driver.cc:2331`）を、case 56-59 の共有本体は既に resolving `try_get_key` で書いている（`crates/flpdf-qtest-tools/src/driver/test_56_63.rs:136`）。**2026-09-19（`flpdf-3yn9.48.207`）**: 共有本体 `test_64_67_body`（`crates/flpdf-qtest-tools/src/driver/test_64_71.rs:121-123`）の qtest-driver hidden feature の非解決 `ObjectHandle::get_key`（`qtest-driver` feature 限定の `doc(hidden)`、内部は `try_get_key` へ委譲済み）を撤去し、case 56-59 と同じ resolving `try_get_key` へ置換した。qpdf 11.9.0 オラクル（`fxo-bigsmall.pdf`/`fxo-smallbig.pdf`、`fx-overlay-64.pdf`）と exit 0・stdout・出力 PDF とも byte-identical、かつ既存の `test_64_67_body_runs_the_canonical_placement_route`/`test_64_writes_the_form_xobject_shrink_expand_output` で確認済み。D1（`PdfWriter::write`）が `flpdf-3yn9.48.165` で `canonical` へ再分類済みのため、この置換により case-level 分類も `canonical` になる。 |
| 65 | `qpdf/test_driver.cc:2349-2354` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_65`（`test_64_67_body` 委譲） | canonical | 同上、case 64 の置換で解消。**2026-09-19（`flpdf-3yn9.48.207`）**: case 64 と共有する `test_64_67_body` の置換により、qpdf 11.9.0 オラクル（`fxo-bigsmall.pdf`/`fxo-smallbig.pdf`、`fx-overlay-65.pdf`）と exit 0・stdout・出力 PDF とも byte-identical であることを確認済み。 |
| 66 | `qpdf/test_driver.cc:2355-2360` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_66`（`test_64_67_body` 委譲） | canonical | 同上、case 64 の置換で解消。**2026-09-19（`flpdf-3yn9.48.207`）**: case 64 と共有する `test_64_67_body` の置換により、qpdf 11.9.0 オラクル（`fxo-bigsmall.pdf`/`fxo-smallbig.pdf`、`fx-overlay-66.pdf`）と exit 0・stdout・出力 PDF とも byte-identical であることを確認済み。 |
| 67 | `qpdf/test_driver.cc:2361-2366` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_67`（`test_64_67_body` 委譲） | canonical | 同上、case 64 の置換で解消。**2026-09-19（`flpdf-3yn9.48.207`）**: case 64 と共有する `test_64_67_body` の置換により、qpdf 11.9.0 オラクル（`fxo-bigsmall.pdf`/`fxo-smallbig.pdf`、`fx-overlay-67.pdf`）と exit 0・stdout・出力 PDF とも byte-identical であることを確認済み。 |
| 68 | `qpdf/test_driver.cc:2367-2388` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_68` | canonical | qpdfの`QPDF::getRoot` → `getKey("/QStream")` → `getStreamData`/`getRawStreamData`順を、`Pdf::root_handle` → `try_get_key` → canonical stream accessorsで再現する。qpdfにないcaller-side `dict_key`/`resolve_handle` x2を撤去し、local helperのcaller-zero確認後に定義も削除した。pinned qpdf/flpdfの`stream_dct.pdf`は双方exit 2、stdout 79 bytes・stderr 54 bytesでbyte-identical。 |
| 69 | `qpdf/test_driver.cc:2389-2404` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_69` | canonical | D1 のみ（page 毎 writer loop）。`Pdf::set_immediate_copy_from`/`Pdf::empty`/`PageInput::foreign`/`PageDocumentHelper::add_page`（対象範囲外）。A7 の使用なし。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 70 | `qpdf/test_driver.cc:2405-2416` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_70` | canonical | `ObjectHandle::set_filter_on_write` は C39 canonical。**2026-09-19（`flpdf-3yn9.48.207`）**: qtest-driver hidden feature の非解決 `ObjectHandle::get_key` を 2 箇所とも resolving `try_get_key` へ置換した（`crates/flpdf-qtest-tools/src/driver/test_64_71.rs:349-350`）。qpdf 11.9.0 オラクル（`filter-on-write.pdf`、`filter-on-write-out.pdf`）と exit 0・stdout "test 70 done"・出力 PDF とも byte-identical であることを確認済み。D1（`PdfWriter::write`）が `flpdf-3yn9.48.165` で `canonical` へ再分類済みのため、この置換により case-level 分類も `canonical` になる。 |
| 71 | `qpdf/test_driver.cc:2417-2459` | `crates/flpdf-qtest-tools/src/driver/test_64_71.rs::run_test_71` | canonical | qpdfのpage/form XObject traversal順を、`Pdf::get_object_handle` → resolving `try_get_key` chain → `PageObjectHelper::for_each_xobject`/`for_each_image`/`for_each_form_xobject`/`get_images`/`get_form_xobjects`で再現する。qpdfにないcaller-side `Pdf::resolve` x4を撤去し、qpdfのpanic-parity（空 vectorへの`pages[0]`インデックス）とmap/output orderを保持した。`QPDFPageObjectHelper.cc:318-395`。 |
| 72 | `qpdf/test_driver.cc:2460-2488` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_72` | canonical | qpdfのpage → `/Resources` → `/XObject` → `/Fx1` chainを、`try_get_key`のreceiver解決と既存PageObjectHelper canonical methodsで再現する。qpdfにないcaller-side `chase_key`/`resolve_once` 3箇所を撤去した。`ObjectHandle::parse_as_contents`/`add_token_filter`/`get_stream_data(Specialized)`（page-content/token-filter 層、本パスでは個別未検証）。form-XObject 分岐を常に取る理由を doc comment が明記（fixture 保証、ハードコードではない）。 |
| 73 | `qpdf/test_driver.cc:2489-2502`; public `closeInputSource` / `unparseResolved`: `include/qpdf/QPDF.hh:162-166`, `include/qpdf/QPDFObjectHandle.hh:1159-1161`, `libqpdf/QPDF.cc:278-281,2354-2367`, `libqpdf/QPDFObjectHandle.cc:1574-1593` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_73` | canonical | qpdfの `closeInputSource` 後に `getRoot` → `getKey("/Pages")` → resolving `unparseResolved` を行う順を、`Pdf::close_input_source` → `root_handle` → `try_get_key` → `try_unparse_resolved` で再現する。qpdfにない `resolve_once` 前置きと非fallible `unparse_resolved` を撤去し、uninitialized handle / closed-source warning・error・statusを保持。 |
| 74 | `qpdf/test_driver.cc:2503-2547` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_74` | canonical | D1 のみ。`NumberTree`/`NameTree` insert/cursor API（対象範囲外）を `trailer_key_handle` 経由で使用、明示的な `.resolve()` 呼び出しなし。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 75 | `qpdf/test_driver.cc:2548-2605` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_75` | canonical | qpdfのNameTree/NumberTree mutation後の`getKey` → `getArrayItem` → `getKey` → `getArrayItem` → `getIntValue`/`getArrayNItems`順を、`try_get_utf8_value`/`try_get_key`/`try_get_array_item`/`try_get_int_value`/`try_get_array_n_items`で再現する。qpdfにないcaller-side `chase_key`/`chase_array_item`/`resolve_once`と非解決`as_array`/`as_integer`をcase75から撤去し、`chase_key`/`resolve_once`はcase79の別スコープのため保持する。NameTree/NumberTree mutationとwriterは既存canonical ownerを通る。pinned qpdf/flpdfの`erase-nntree.pdf`は双方exit 0、stdout 13 bytes・stderr空、生成`a.pdf` 2377 bytesでbyte-identical。 |
| 76 | `qpdf/test_driver.cc:2606-2650` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_76` | canonical | D1 のみ（`.resolve()` 呼び出しなし）。**2026-09-18 再照合**: 記載時点の「個別未検証」の留保を解消した ——`FileSpec::create_file_spec_from_path`（`crates/flpdf/src/job/attachments.rs:148,597`）、`EmbeddedFileDocumentHelper::replace_embedded_file`（`crates/flpdf/src/job/attachments.rs:173`）、`EmbeddedFileDocumentHelper::get_embedded_files`（`crates/flpdf/src/job/attachment_list.rs:67`、`crates/flpdf/src/job/json_sections.rs:835`）は `--add-attachment`/`--list-attachments`/JSON attachments 経路の production caller と共有される。handle を取る `FileSpec::create_file_spec` と `EmbeddedFileStream::create_ef_stream` も、公開 API `FileSpecBuilder::build`（`crates/flpdf/src/filespec_helper/filespec.rs:507-508`、`crates/flpdf/src/lib.rs:177` で再輸出）が同じものを呼ぶ。いずれも qtest 専用ラッパーではない。`mixed` 根拠は D1 のみ。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 77 | `qpdf/test_driver.cc:2651-2663` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_77` | canonical | D1 のみ。**2026-09-18 再照合**: case 76 と同じ留保を解消した ——`EmbeddedFileDocumentHelper::remove_embedded_file` は `crates/flpdf/src/job/lifecycle.rs:3916`（`--remove-attachment`）と共有される production primitive。`mixed` 根拠は D1 のみ。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 78 | `qpdf/test_driver.cc:2664-2704` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_78` | canonical | D1 のみ。`replace_stream_data_with_callback`/`replace_stream_data_with_retry_callback`（C38 canonical）+ `pipe_stream_data`（C1/C3 canonical）— 概ね canonical。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 79 | `qpdf/test_driver.cc:2705-2760` | `crates/flpdf-qtest-tools/src/driver/test_72_79.rs::run_test_79` | canonical | D1 のみ（A7 の qpdf-less `chase_key`/`resolve_once` は `.48.129` でcase79から撤去）。`copy_stream`（C36 canonical）、`replace_stream_data`（C38 canonical）、`get_stream_data`（C5 canonical）— stream mutation 面自体は canonical。 **2026-09-19（`flpdf-3yn9.48.165`）**: D1（`PdfWriter::write`）が qpdf の `writeLinearized`/`writeStandard` 2 分岐＋PCLm 1 段構造へ揃い `canonical` へ再分類されたため、case-level 分類も `canonical` へ更新した。 |
| 80 | `qpdf/test_driver.cc:2761-2807` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_80` | canonical | `PageDocumentHelper::get_all_pages`/`try_get_key`、`AcroFormDocumentHelper::transform_annotations`/`add_and_rename_form_fields`、`PageObjectHelper::copy_annotations_from`（対象範囲外）。qpdf の test_80 は `page1.getKey("/Annots")`（`qpdf/test_driver.cc:2775`）だけで明示 resolve を持たない。**2026-09-19（`flpdf-3yn9.48.207`）**: qtest-driver hidden feature の明示 `Pdf::resolve`（`qtest-driver` feature 限定の `doc(hidden)`。内部は `handle.try_dereference()` のみで、`try_dereference` 自体は `pub(crate)` のため crate 外の qtest-tools からは直接呼べない）を 2 箇所とも撤去した。どちらも直後の呼び出し（`page1.try_get_key(b"/Annots")` / `AcroFormDocumentHelper::transform_annotations` 内部の resolving `try_as_array`）が receiver を自ら解決するため、独立した explicit-resolve ステップは元々冗長で、削除しても qpdf の構造（明示 resolve なし）に一致する（`crates/flpdf-qtest-tools/src/driver/test_80_87.rs:100-109`）。qpdf 11.9.0 オラクル（`appearances-1.pdf`/`appearances-1-rotated.pdf` × `minimal.pdf`、`test80a{1,2}.pdf`/`test80b{1,2}.pdf`）と exit 0・stdout "test 80 done"・両出力 PDF とも byte-identical、かつ既存の `test_80_writes_both_annotation_outputs` で確認済み。D1（`PdfWriter::write`）が `flpdf-3yn9.48.165` で `canonical` へ再分類済みのため、この置換により case-level 分類も `canonical` になる。 |
| 81 | `qpdf/test_driver.cc:2808-2819` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_81` | canonical | `ObjectHandle::try_get_int_value` → `Error::QpdfExc`/`QpdfErrorCode::Object`。A6/A8 は領域行としては mixed だが、本ケースの実使用は resolving `try_*` family で個別逸脱なし。 |
| 82 | `qpdf/test_driver.cc:2820-2863` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_82` | canonical | `try_is_name_and_equals`/`try_is_dictionary_of_type`/`try_is_stream_of_type`/`try_is_or_has_name`、全て resolving `try_*` family。case 81 と同じ A6/A8 の背景注記。 |
| 83 | `qpdf/test_driver.cc:2864-2884` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_83` | mixed | `QPDFJob::new().initialize_from_json_bytes` → E-17（`initializeFromArgv`/`initializeFromJson`、mixed。CLI はこの経路に未到達）。 |
| 84 | `qpdf/test_driver.cc:2885-2973` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_84` | mixed | `QPDFJob` の config/run/check_configuration/register_progress_reporter/set_output_streams → E-1（`run`、canonical）、E-18（`checkConfiguration`、canonical）、E-19（`getExitCode`/`hasWarnings`、mixed）、E-20（logger/progress、canonical）。**2026-09-18 再照合**: 記載時点の「E-1（`run`、mixed）」「少なくとも 2 つの mixed E 行」は stale ——E-1 は既に `canonical` で、現在触れる mixed な E 行は E-19 のみ。`crates/flpdf-qtest-tools/src/driver/test_80_87.rs:331,355` の `job.has_warnings()` がその境界で、`PdfWriter` は本 case が直接構築しない（D1 には `job.run()` 経由で間接的に到達する）。 |
| 85 | `qpdf/test_driver.cc:2973-3062`; public `getValueAs...`: `include/qpdf/QPDFObjectHandle.hh:601-606,640-711`, `libqpdf/QPDFObjectHandle.cc:484-748` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_85` | canonical | qpdfのwarning-free `getValueAs...` family（wrong-typeはfalse、receiverはresolve、int/uint clampのみwarning）を、`try_get_value_as_bool` / `try_get_value_as_int` / `try_get_value_as_uint` / `try_get_value_as_real` / `try_get_value_as_number` / `try_get_value_as_name` / `try_get_value_as_utf8` / `try_get_value_as_operator` / `try_get_value_as_inline_image`へ1:1に移行した。旧local `value_as_*` と非解決 `as_*` routeは撤去し、qpdf test85のassertion順・duplicated UTF-8 block・clamp warningを保持する。 |
| 86 | `qpdf/test_driver.cc:3065-3085`; public `newUnicodeString` / `getStringValue` / `getUTF8Value`: `include/qpdf/QPDFObjectHandle.hh:507,689-701`, `libqpdf/QPDFObjectHandle.cc:659-701,1927-1930`; string implementation: `libqpdf/QPDF_String.cc:28-34,162-173` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_86` | canonical | qpdfの `QUtil` checked conversionsとUTF-16変換を先に検証した後、`newUnicodeString` の結果を `ObjectHandle::string` へ渡し、`try_get_string_value` → `try_get_utf8_value` の順で public handle boundaryから読み出す。qpdfにない direct `pdf_string::utf8_value(&stored)` readbackを撤去し、`pdf_string::utf8_value(utf16_val)` は独立したUTF-16 transcoder probeとして残す。 |
| 87 | `qpdf/test_driver.cc:3086-3103`; public `getKeys`: `include/qpdf/QPDFObjectHandle.hh:777-780`, `libqpdf/QPDFObjectHandle.cc:998-1009`, `libqpdf/QPDF_Dictionary.cc:59-78,118-125` | `crates/flpdf-qtest-tools/src/driver/test_80_87.rs::run_test_87` | canonical | qpdfの `unparse` → `getKeys` → `replaceKey` → `getJSON` の null-omission assertionを、`ObjectHandle::parse`/`unparse`/`replace_key`/`try_get_keys`/`json_inspect::pdf_object_to_json` で再現する。qpdfに対応物のない `direct_non_null_keys`（direct-only `as_dictionary`/非解決`is_null`）を撤去し、receiver/childのlazy resolutionとnull除外をcanonical primitiveへ戻した。 |
| 88 | `qpdf/test_driver.cc:3106-3162` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_88` | canonical | qpdf-11 の mutate/get family（`replace_key_and_get_new`/`_old`、`append_array_item_and_get_new`、`insert_array_item(_and_get_new)`、`erase_array_item(_and_get_old)`、`remove_key(_and_get_old)`）全て `pub`、ギャップなし。 |
| 89 | `qpdf/test_driver.cc:3163-3174` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_89`（`mod.rs::run_test_89_from_json` 経由で `Pdf::create_from_json_with_options`、`crates/flpdf/src/json/document.rs:93`、`pub`） | canonical | document-construction boundary は public でギャップなし。以降の mutation/warning 本体は case 88/93 と同じ canonical accessor 群（`trailer`/`root_handle`/`get_object_handle`/`replace_key`/`try_get_array_item`）。qpdf の `replaceKey` は receiver を内部 dereference する（`libqpdf/QPDFObjectHandle.cc:1197-1209`）ため、`.48.103` で明示 `Pdf::resolve` を除去した。 |
| 90 | `qpdf/test_driver.cc:3175-3187` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_90` | canonical | `Pdf::update_from_json`（`crates/flpdf/src/json/document.rs:129`、`pub`）、ギャップなし。以降は case 88/93 と同じ canonical accessor。 |
| 91 | `qpdf/test_driver.cc:3188-3195` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_91` | canonical | `document_json::write_json`（`pub`、qpdf の `(version, pipeline, decode_level, stream_data_mode, file_prefix, wanted_objects)` 引数順と一致）+ `StdoutPipeline`（`Pipeline` 実装）。CLAUDE.md (B) の入れ物代替と doc comment に明記済み、未解決 bridge ではない。 |
| 92 | `qpdf/test_driver.cc:3196-3244`; public `getOwningQPDF` / `isIndirect` / `isDictionary` / `isScalar` / `getDict` / `unparse`: `include/qpdf/QPDFObjectHandle.hh:351-362,861-865,968-970,1157-1161`, `libqpdf/QPDFObjectHandle.cc:332-335,432-453,1575-1593,2571-2574`, `libqpdf/QPDF.cc:229-235`, `libqpdf/QPDF_Destroyed.cc:24-29` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_92` | canonical | qpdfのroot/page/resources/contents handle chainを、`Pdf::root_handle` → `try_get_key` → `try_get_array_item` → `try_is_dictionary`/`try_is_scalar` → `try_get_stream_dict`で同じ解決順に再現する。所有者drop後のidentity/indirectness/destroyed状態を保持し、qpdfの`unparse()`がdestroyed rootで`unparseResolved`へ到達して投げるlogic errorを`try_unparse_resolved`で同じ文言にする。qpdf-less `root_handle` helper、対象関数内の`resolved_key`/`Pdf::resolve`/非解決`as_*`経路、GAPを撤去した。pinned qpdf/flpdfの`minimal.pdf` case92は双方exit 0、stdout 13 bytesで`cmp`一致。 |
| 93 | `qpdf/test_driver.cc:3245-3271` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_93` | canonical | `trailer`、`get_key`、`root_handle`、`parse`、`is_same_object_as`、`replace_key`、`make_indirect_from_object_handle`、`is_indirect` — 全 `pub`、ギャップなし。 |
| 94 | `qpdf/test_driver.cc:3272-3373` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_94` | canonical | `PageObjectHelper::get_media_box`/`get_crop_box`/`get_bleed_box`/`get_trim_box`/`get_art_box` — `.claude/rules/qpdf-port-design-patterns.md` §7 が明記する確立済み `get_` prefix canonical 慣行。live-identity/copy-on-fallback 挙動も doc 検証済み。 |
| 95 | `qpdf/test_driver.cc:3374-3399` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_95` | canonical | ローカル `is_scalar` は `ObjectHandle::type_code()`（`pub`）をラップするのみ、ギャップなし。 |
| 96 | `qpdf/test_driver.cc:3400-3414` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_96` | canonical | `ObjectHandle::parse` + `pdf_string::unparse_binary`（`pub`）、ギャップなし。 |
| 97 | `qpdf/test_driver.cc:3414-3422`; public `isArray` / `getArrayNItems` / `getArrayItem` / `shallowCopy`: `include/qpdf/QPDFObjectHandle.hh:337,725-728`, `libqpdf/QPDFObjectHandle.cc:426-429,758-785,2072-2079` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_97` | canonical | qpdfの`getKey("/Nulls").getArrayItem(0).isArray() && getArrayNItems()` → `shallowCopy()` → `unparse()`の短絡順を、`trailer_key_handle` → `try_get_array_item` → `try_is_array` → 短絡した`try_get_array_n_items` → `shallow_copy` → `unparse`で再現する。`try_is_array`が非配列時のcount accessorを短絡するため、qpdfにない`Pdf::resolve`、不要なtype warning、非解決`as_array` snapshotを撤去した。pinned qpdf/flpdfの`many-nulls.pdf` case97は双方exit 0、stdout 13 bytesで`cmp`一致し、非配列itemのwarningなし回帰も固定する。 |
| 98 | `qpdf/test_driver.cc:3425-3450` | `crates/flpdf-qtest-tools/src/driver/test_88_98.rs::run_test_98` | canonical | **移植済み（`flpdf-wkju`、caller cutover `flpdf-3yn9.48.106`）**: `ObjectHandle::write_json`/`get_json` の`dereference=true`内部resolutionで全6オブジェクトを検証し、content streamの辞書mutationは`try_get_stream_dict`で担う。`get_stream_json`（`pub`、`object_handle.rs:6325`、C44 facade）をqpdfの期待バイト列と比較する。fixture `tests/fixtures/qpdf-test98-minimal.pdf` はqpdfの`examples/qtest/npages/minimal.pdf`とbyte一致（763 B）。対象関数内のcaller-side `Pdf::resolve`は0。case 98はqpdf公開`getStreamJSON`に対応する経路を使う。C44自体も`flpdf-3yn9.48.196`（2026-09-19）でcanonicalへ再分類済み（単一entrypointが単一qpdf責務に対応し`mixed`定義に該当しない）。 |

**range 別サマリ**（各 range 行と「合計（物理行）」は物理行単位。「合計（論理ケース）」のみ
`0/1` を 2 ケースとして数える）:

<!-- route-matrix-aggregate: range-summary unit=physical detail-table=e-28 -->

| range | 物理行 | canonical | mixed | bridge | unknown |
|---|---|---|---|---|---|
| 0/1, 2-25 | 25 | 22（0/1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25） | 3（12, 13, 14） | 0（—） | 0（—） |
| 26-49 | 24 | 23（26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 46, 47, 48, 49） | 1（45） | 0（—） | 0（—） |
| 50-79 | 30 | 29（50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79） | 1（63） | 0（—） | 0（—） |
| 80-98 | 19 | 17（80, 81, 82, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98） | 2（83, 84） | 0（—） | 0（—） |
| **合計（物理行）** | **98** | **91** | **7** | **0** | **0** |
| **合計（論理ケース）** | **99** | **92** | **7** | **0** | **0** |

全 98 行（99 ケース）で `classification` 列が `unknown` の行はゼロ。case 5/12/13/22/23 は初回監査で `unknown` 候補だったが、`show_linearization_pdf_with_warnings` が `QPDFJob::show_linearization` と同一 primitive であることを production コードの grep で確認した上で `mixed`（E-7 従属）に、`PageDocumentHelper` 系の 3 ケースは同型 canonical 前例（case 26/39/94）との一貫性を取って `canonical` に、それぞれ再分類した。

本監査で issue 化したギャップ 6 件（2026-09-07 の再検証で、うち 5 件は当初「API 不在」としていた前提が誤りで、実際には driver 側の未移行だと判明。各行を修正済み）: `flpdf-83jc`（case 16, updateAllPagesCache）、`flpdf-wd2e`（case 34, getExtensionLevel/getVersionAsPDFVersion）、`flpdf-jzj1`（case 86, utf8_to_ascii/utf8_to_pdf_doc）、`flpdf-6f6h`（case 92, getOwningQPDF）、`flpdf-cm84`（case 97, getArrayItem 単一 index）、`flpdf-wkju`（case 98, write_json/get_json/write_stream_json が pub(crate) 限定で全体が未実行 stub）。case 51（`FIELD_MUST_BE_INDIRECT`、現行 fixture 非顕在化）と case 78（trailer mutation 後の `mark_object_handle_dirty` 欠落、cov:ignore 済み）は新規 issue 化せず本表に注記のみ残した — いずれも fixture 上は無害で、実装cutoverの緊急性は無いと判断した。

2026-09-15（`flpdf-3yn9.48.103`）: qpdf の test 46/48/89 に残っていた明示 `Pdf::resolve` bridge を撤去した。number/name-tree の typed value は resolving `try_get_string_value`/`try_get_utf8_value`、Bad3 `/Kids` は resolving `try_get_key`/`try_get_array_item`、test 89 の type-mismatch mutation は `replace_key` 自身の resolving boundary を使う。対象 3 ケースは `canonical` へ再分類し、case 31/42/98 の explicit-resolve 残差は別 bounded audit scope として残した。

2026-09-15（`flpdf-3yn9.48.104`）: qpdf の test 31 は public `QPDFObjectHandle::isNull()` で間接参照を lazy dereference してから null を判定する（`libqpdf/QPDFObjectHandle.cc:353-356`）。flpdf の public `ObjectHandle::try_is_null`（`object_handle.rs:3002`）へ test 31 の null item 判定を移し、caller-side `Pdf::resolve` を撤去した。case 31 を `canonical` へ再分類し、残る explicit-resolve bridge は case 42/98 として別 bounded audit scope に残す。

2026-09-15（`flpdf-3yn9.48.106`）: qpdf test 42 の`page.getKey("/Contents").getDict()`（`qpdf/test_driver.cc:1407-1551`）を、`try_get_key`→`try_get_stream_dict`へcutoverし、対象関数内の`Pdf::resolve`を7箇所から0にした。test 98（`qpdf/test_driver.cc:3425-3450`）は`writeJSON`/`getJSON(..., true)`の内部resolutionを使用し、stream dictionary mutationを`try_get_stream_dict`へ移して、対象関数内の`Pdf::resolve`を2箇所から0にした。qpdfの`getDict`責務は`QPDFObjectHandle.cc:313-324,1257-1262`により、qpdf pinのpublic boundaryと一致する。type-checksのtest_driver 42およびqpdf-json suiteの実機survey、Rust focused testsを通過し、case 42/98は`canonical`へ再分類した。

2026-09-15（`flpdf-3yn9.48.107`）: qpdf test 2（`qpdf/test_driver.cc:286-308`）の `/O`・`/U` は public `getKey` の返した値をそのまま `unparse` し、`/Contents` は public `pipeStreamData` に解決を委譲する。flpdf の `try_get_key`、`ObjectHandle::unparse`、同じpipe経路をbuffer化する `get_stream_data` がこの境界を担うため、`run_test_2` のcaller-side `resolve_handle` 3箇所を撤去した。qpdf 11.9.0とのtest 2 differential、対象関数のsource guard、test 4の必要な明示解決を確認し、case 2を`canonical`へ再分類した。

2026-09-15（`flpdf-3yn9.48.108`）: qpdf test 6（`qpdf/test_driver.cc:422-439`）は public `isStream()`で`/Metadata`の型をresolve後に確認し、`pipeStreamData(..., qpdf_dl_none)`で復号済み・未decodeのデータを読む。flpdfの resolving `type_code()`とcanonical `pipe_stream_data`がこの責務を担うため、`run_test_6`のcaller-side `resolve_handle` 1箇所を撤去し、case 6を`canonical`へ再分類した。

2026-09-15（`flpdf-3yn9.48.109`）: qpdf test 3（`qpdf/test_driver.cc:311-322`）は公開`getArrayNItems()`で`/QStreams`の件数を取得し、公開`getArrayItem()`を各indexへ順に適用してから`pipeStreamData`を呼ぶ。flpdfの`run_test_3`を`try_get_array_n_items` → `try_get_array_item`へ移し、配列全体のdriver-side snapshotを撤去した。非配列warning、各streamのnormalize/pipe、diagnostic順序を保持するfocused testsと`good14.pdf`のlive probeを確認し、case 3を`canonical`へ再分類した。

2026-09-15（`flpdf-3yn9.48.110`）: qpdf test 11（`qpdf/test_driver.cc:538-550`）はpublic `QPDF::getRoot()`でCatalog辞書を解決してから`getKey`、`getStreamData`、`getRawStreamData`を呼ぶ。flpdfの`run_test_11`を`Pdf::root_handle()`へ移し、`root_ref()` → `get_object_handle()`というidentity projectionをsemantic Catalog readから撤去した。`stream-data.pdf`に対するflpdf driver outputはqpdf `test11.out`とbyte一致し、case 11を`canonical`へ再分類した。

2026-09-15（`flpdf-3yn9.48.111`）: qpdf test 19（`qpdf/test_driver.cc:818-832`）はpage listからduplicate pageを追加した後、public `getKey("/Contents")`で両pageの共有stream identityを比較する。flpdfの`run_test_19`をnon-resolving `get_key`からcanonical resolving `try_get_key`へ移し、PageDocumentHelperのsnapshot再取得とidentity比較を保持した。qpdf `page_api_1.pdf`の`test 19` output、focused unit test、source guardを確認し、case 19を`canonical`へ再分類した。

2026-09-15（`flpdf-3yn9.48.113`）: qpdf test 21（`qpdf/test_driver.cc:852-860`）はpublic resolving `getKey("/Contents")`の結果へ、receiverを内部解決する`shallowCopy`を直接適用する。flpdfの`run_test_21`を`try_get_key` → canonical resolving `shallow_copy`へ移し、caller-side `Pdf::resolve`を0にした。qpdf `shallow_stream.out`（`shallow_array.pdf`入力）とflpdf driverのstderrを比較し、exit 2・`stream objects cannot be cloned`の一致、source guard、focused error-contract testを確認してcase 21を`canonical`へ再分類した。

2026-09-15（`flpdf-3yn9.48.114`）: qpdf test 17（`qpdf/test_driver.cc:776-793`）はpublic `getRoot()`後にresolving `getKey()`を連鎖させ、`getArrayItem(0/1)`で重複したpage identityを確認してから`getAllPages()`の修復・削除・stream readへ進む。flpdfの`run_test_17`を`root_handle` → `try_get_key` → `try_get_array_item`へ移し、qpdfにないCatalogの`root_ref`投影と非解決`as_array`を撤去した。pinned qpdf/flpdfの`page_api_2.pdf` driver outputはexit 0・158 bytesで`cmp`一致し、indirect `/Kids` regression、route guard、focused testを確認してcase 17を`canonical`へ再分類した。

2026-09-15（`flpdf-3yn9.48.115`）: qpdf test 73（`qpdf/test_driver.cc:2489-2500`）は`closeInputSource`後に`getRoot().getKey("/Pages").unparseResolved()`を直接呼び、`unparseResolved`自身がreceiverを解決する。flpdfの`run_test_73`を`try_unparse_resolved`へ移し、qpdf-less `resolve_once` bridgeと非fallible `unparse_resolved`を撤去した。pinned qpdf/flpdfの`invalid-objects` test73はexit 2・350 bytesで`cmp`一致し、cached-pages回帰とsource guardも通過した。case 73を`canonical`へ再分類した。

2026-09-16（`flpdf-3yn9.48.116`）: qpdf test 87（`qpdf/test_driver.cc:3086-3103`）はdictionaryのdirect/indirect null valueを`unparse`・`getKeys`・`getJSON`の全境界でmissing相当として扱う。flpdfの`run_test_87`をlocal `direct_non_null_keys`からpublic `try_get_keys`へ移し、receiver/childのlazy resolutionとnull除外をcanonical object-model primitiveへ委譲した。source guard、driver case87の`test 87 done`、qpdf differentialでcase87を`canonical`へ再分類した。

2026-09-16（`flpdf-3yn9.48.117`）: qpdf test 97（`qpdf/test_driver.cc:3414-3422`）はpublic `getArrayItem(0)`の結果へ`getArrayNItems()`を適用し、receiver自身の解決とarray count/type-warning境界をqpdf accessorへ委譲する。flpdfの`run_test_97`を`try_get_array_n_items`へ移し、qpdf-less `Pdf::resolve` bridgeと非解決`as_array().len()` snapshotを撤去した。pinned qpdf/flpdfの`many-nulls.pdf` test97は双方exit 0・13 bytesで`cmp`一致し、case 97を`canonical`へ再分類した。

2026-09-16（`flpdf-3yn9.48.118`）: qpdf test 92（`qpdf/test_driver.cc:3196-3244`）は`getOwningQPDF`でroot/page/resources/contentsのowner identityを確認し、QPDF drop後にindirect handleをdisconnect/destroyしてからdestroyed rootの`unparse()` logic errorを捕捉する。flpdfの`run_test_92`をqpdf-less `root_handle`/`resolved_key`/caller-side `Pdf::resolve`/非解決`as_*`からcanonical `Pdf::root_handle`・`try_get_key`・`try_get_array_item`・`try_is_*`・`try_get_stream_dict`・`try_unparse_resolved`へ移した。非配列/直接子の型保持とdestroyed unparseのerror messageを回帰固定し、case 92を`canonical`へ再分類した。

2026-09-16（`flpdf-3yn9.48.120`）: qpdf test 85（`qpdf/test_driver.cc:2973-3062`）のwarning-free `getValueAs...` familyを、`try_get_value_as_*` canonical primitiveへ移行した。local `value_as_*`/非解決`as_*` consumer routeを撤去し、wrong-typeのsilent absence、indirect receiver resolution、int/uint clamp warning、source literal/name/UTF-8/operator/inline-image値をqpdf順で保持した。qpdf/flpdf test85 stdout/stderrはcmp一致し、case 85を`canonical`へ再分類した。

2026-09-16（`flpdf-3yn9.48.121`）: qpdf test 86（`qpdf/test_driver.cc:3065-3085`）は`newUnicodeString`の結果をpublic `getStringValue`、続けて`getUTF8Value`で読み出す。flpdfの`run_test_86`はchecked QUtil変換、UTF-16 transcoder、`new_unicode_string`のbyte assertionを保持しつつ、stored bytesのreadbackを`ObjectHandle::string` → `try_get_string_value` → `try_get_utf8_value`へ移した。qpdfにない直接`pdf_string::utf8_value(&stored)` routeを撤去し、pinned qpdf/flpdf test86は双方exit 0・stdout/stderr byte-identicalで、case 86を`canonical`へ再分類した。

2026-09-16（`flpdf-3yn9.48.122`）: qpdf test 50（`qpdf/test_driver.cc:1939-1953`）は`getTrailer().getKey`で辞書handleを得た後、`mergeResources`自身にreceiver/otherの解決を委譲し、non-resolving `getJSON`、type-mismatchの`mergeResources(getKey("/k1"))`、`getResourceNames`へ進む。flpdfの`run_test_50`からcaller-side `Pdf::resolve` 3箇所とhidden direct `get_key`を撤去し、canonical `merge_resources`/`try_get_key`/`get_resource_names`境界へ移した。pinned qpdf/flpdf `merge-dict.pdf`は双方exit 0、stdout 465 bytes・stderr空でbyte-identical、case 50を`canonical`へ再分類した。

2026-09-16（`flpdf-3yn9.48.123`）: qpdf test 71（`qpdf/test_driver.cc:2417-2459`）はpage/form XObject helperの各 traversalがreceiver accessorsへ解決を委譲し、直接のpublic resolver前置きを持たない。flpdfの`run_test_71`からpage → `/Resources` → `/XObject` → `/Fx1` chainのcaller-side `Pdf::resolve` 4箇所を撤去し、`try_get_key`と既存PageObjectHelper canonical methodsへ移した。pinned qpdf/flpdfの`nested-form-xobjects.pdf`は双方exit 0、stdout 2187 bytes・stderr空でbyte-identical、case 71を`canonical`へ再分類した。

2026-09-16（`flpdf-3yn9.48.125`）: qpdf test 72（`qpdf/test_driver.cc:2460-2488`）はpageの`getKey("/Resources").getKey("/XObject").getKey("/Fx1")` chainを直接呼び、form helperのparse/token-filter処理へ進む。flpdfの`run_test_72`から`chase_key` 3箇所を撤去し、`try_get_key`のreceiver解決と既存PageObjectHelper canonical methodsへ移した。shared `chase_key` はcase75の別スコープのため保持する。pinned qpdf/flpdfの`nested-form-xobjects.pdf`は双方exit 0、stdout 1511 bytes・stderr空でbyte-identical、case 72を`canonical`へ再分類した。

2026-09-16（`flpdf-3yn9.48.127`）: qpdf test 68（`qpdf/test_driver.cc:2367-2388`）は`getRoot`でCatalogを得て`getKey("/QStream")`、最初の`getStreamData`だけを広いexception catchで処理し、その後のAll/raw readを独立して実行する。flpdfの`run_test_68`からqpdfにないcaller-side `dict_key`/`resolve_handle` x2を撤去し、`Pdf::root_handle` → `try_get_key` → `get_stream_data`/`get_raw_stream_data`へ移行した。local helperは`rg -n 'dict_key|resolve_handle' crates/flpdf-qtest-tools/src/driver/test_64_71.rs`でcaller-zeroを確認して削除した。pinned qpdf/flpdfの`stream_dct.pdf`は双方exit 2、stdout 79 bytes・stderr 54 bytesでbyte-identical、case 68を`canonical`へ再分類した。

2026-09-16（`flpdf-3yn9.48.128`）: qpdf test 75（`qpdf/test_driver.cc:2548-2605`）はNameTree/NumberTree mutation後のlive handleへ`getKey`、`getArrayItem`、`getIntValue`、`getArrayNItems`を各receiverのaccessor境界で適用する。flpdfの`run_test_75`を`try_get_utf8_value`/`try_get_key`/`try_get_array_item`/`try_get_int_value`/`try_get_array_n_items`へ移し、qpdf-less `chase_key`/`chase_array_item`/`resolve_once`、非解決array snapshotを撤去した。`chase_key`/`resolve_once`はcase79の別スコープのため保持し、`chase_array_item`はcaller-zero確認後に削除した。canonical helper unit testも`try_*` accessorsへ更新した。pinned qpdf/flpdfの`erase-nntree.pdf`は双方exit 0、stdout 13 bytes・stderr空、生成`a.pdf` 2377 bytesでbyte-identical、case 75を`canonical`へ再分類した。

2026-09-16（`flpdf-3yn9.48.129`）: qpdf test 79（`qpdf/test_driver.cc:2705-2758`）はpageの`getKey("/Contents")`をreceiverのaccessor境界で解決してからstream copyへ進む。flpdfの`run_test_79`を`page.try_get_key(b"/Contents")`へ移し、qpdf-less `chase_key`/`resolve_once`を`test_72_79.rs`からcaller-zero確認後に撤去した。`copy_stream`/stream mutation/QDF static-ID writerは保持し、case79はD1 writer routeだけが残るmixedとして記録する。pinned qpdf/flpdfの`minimal.pdf` test79は双方exit 0、stdout 13 bytes・stderr空、生成`a.pdf` 2370 bytesでbyte-identical、case79のA7 caller-side bridgeを撤去した。

2026-09-16（`flpdf-3yn9.48.130`）: qpdf test 47（`qpdf/test_driver.cc:1784-1796`）はpublic `getRoot` → `getKey("/Pages")` → `getKey("/Count")` → `getIntValue`を各receiverのaccessor境界で適用する。flpdfの`run_test_47`を`Pdf::root_handle` → `try_get_key` → `try_get_key` → `try_get_int_value`へ移し、qpdf-less `root_ref`/`chase_key`/非解決`as_integer`を撤去した。local `chase_key`のcase47/unit-test callerはzeroになりhelper自体を削除、PageLabelDocumentHelperとunderflow/output順序は保持した。pinned qpdf/flpdfの`name-tree.pdf` test47は双方exit 0、stdout 27 bytes・stderr空でbyte-identical、case47のstale bridge evidenceを修正した。

2026-09-16（`flpdf-3yn9.48.132`）: qpdf test 39（`qpdf/test_driver.cc:1361-1377`）は`QPDFPageObjectHelper::getImages`でdirect image XObjectを列挙し、`getDict().getKey("/Filter").unparseResolved()`/`/ColorSpace`へ進む。flpdfの`run_test_39`をcanonical `PageObjectHelper::get_images` → `try_get_stream_dict` → `try_get_key` → `try_unparse_resolved`へ移し、qpdf-less manual resource walkとcase39の`resolve_once` 5 callersを撤去した。pinned qpdf/flpdfの`image-streams.pdf` test39は双方exit 0、stdout 478 bytes・stderr空でbyte-identical、case39のcanonical route evidenceを実装と一致させた。

2026-09-16（`flpdf-3yn9.48.133`）: qpdf test 38（`qpdf/test_driver.cc:1351-1360`）はCatalogの`getKey("/QTest")`後に`getArrayNItems`/`getArrayItem`をqpdfのaccessor境界で適用し、各itemを`unparseResolved`する。flpdfの`run_test_38`を`Pdf::root_handle` → `try_get_key` → `try_get_array_n_items`/`try_get_array_item` → `try_unparse_resolved`へ移し、qpdf-less driver-local resolutionと非解決array snapshotを撤去した。case34/35/36のdiagnostic helperは保持し、pinned qpdf/flpdfの`override-compressed-object.pdf` test38は双方exit 0、stdout 57 bytes・stderr空でbyte-identical。malformed lazy array itemでも双方のrepair warning（stdout 16 bytes、stderr 65 bytes）の順序・内容が一致する回帰テストを追加し、case38のcanonical route evidenceを実装と一致させた。

2026-09-16（`flpdf-3yn9.48.134`）: qpdf test 34（`qpdf/test_driver.cc:1252-1263`）のversion/extension observationを、既存production `Pdf::get_extension_level`・`Pdf::root_handle`・`ObjectHandle::try_get_key`・`Pdf::get_version_as_pdf_version`へ移行した。qpdfが実際に呼ぶ`getKey(...).unparse()`の非解決出力は保持し、qpdf-less `catalog_extension_level`とcase34のdriver-local root/key walkだけを削除した。pinned qpdf/flpdfの`minimal.pdf`、`extensions-adbe.pdf`、`extensions-other.pdf`、`extensions-adbe-other.pdf` test34はexit 0でstdout/stderr byte-identical、clamp warning regressionもqpdfのwarning契約を保持する。

2026-09-17（`flpdf-3yn9.48.135`）: qpdf test 51（`qpdf/test_driver.cc:1955-1997`）は`getRoot` → `getKey` → `getArrayNItems`/`getArrayItem` → `getKey`/`isString`/`getUTF8Value`の各accessor境界を経て、live handleを`QPDFFormFieldObjectHelper`へ直接渡す。flpdfの`run_test_51`からqpdf-less `resolve_and_drain`、非解決array snapshot、ObjectRef-only `FormFieldObjectHelper::new`を撤去し、canonical resolving accessorsと`from_object_handle`へ移行した。case52の共有helperは保持し、pinned qpdf/flpdfの`button-set.pdf`/`button-set-broken.pdf` test51はstatus、stdout、stderr、生成QDF bytesが一致した。case-levelはD1 writer混在のためmixedのまま。

2026-09-17（`flpdf-3yn9.48.136`）: qpdf test 52（`qpdf/test_driver.cc:1999-2022`、`QPDFFormFieldObjectHelper.cc:11-17,300-326`）は各accessorがreceiverを解決し、`QPDFFormFieldObjectHelper`へfield handleを直接渡す。flpdfの`run_test_52`を`Pdf::root_handle`、`try_get_key`、`try_get_array_n_items`、`try_get_array_item`、`try_is_string`、`try_get_utf8_value`、`FormFieldObjectHelper::from_object_handle`へ移し、qpdfの`newString(arg2)`に対応するraw bytes境界と診断flush順を保持した。case51/52のcaller-zeroを確認して共有`resolve_and_drain`と`FIELD_MUST_BE_INDIRECT`を削除。直接field回帰とsource guardを追加し、pinned qpdf/flpdfのappearance-streamsを27/27で完走、raw non-UTF-8 argもstdout/stderr/status一致、生成PDFはqpdf canonicalization後にbyte-identical。D1 writer混在のためcase-level分類はmixedのまま。

2026-09-17（`flpdf-3yn9.48.137`）: qpdf test 4（`qpdf/test_driver.cc:325-374`）は`getKey("/QTest2")`後にpublic `isNull()`を呼び、`isNull()`自身がhandleをdereferenceしてからnull判定する（`libqpdf/QPDFObjectHandle.cc:353-356`）。flpdfの`run_test_4`を`try_get_key` → `try_is_null`へ移し、qpdf-less caller-side `resolve_handle`と非解決`is_null`を撤去した。source guard、qpdf/flpdfのtest4 fixture 5件（status/stdout/stderr一致）、mutability literal suite 5/5を確認した。`PdfWriter::write`のD1境界が残るためcase-level分類はmixedのまま。

2026-09-17（`flpdf-3yn9.48.138`）: qpdf test 5（`qpdf/test_driver.cc:374-420`）は`getRoot`/`getKey`後、`isArray()`で各配列のreceiverを解決し、`getArrayNItems()`/`getArrayItem()`を順に適用してから`getUTF8Value()`/`getNumericValue()`を呼ぶ。flpdfの`run_test_5`を`try_is_array` → `try_get_array_n_items`/`try_get_array_item` → `try_get_utf8_value`/`try_get_numeric_value`へ移し、qpdf-less `resolve_handle`と非解決`as_array` snapshotを撤去した。source guard、qpdf/flpdf numeric-and-string fixture 3件のstatus/stdout/stderr一致、literal qtest 3/3を確認し、case5のclassificationはcanonicalのまま更新した。

2026-09-17（`flpdf-3yn9.48.139`）: qpdf tests 7/8（`qpdf/test_driver.cc:441-493`）は`getRoot`→`getKey("/QStream")`後にpublic `isStream()`でreceiverを解決する。flpdfの`run_test_7`/`run_test_8`は既存のresolving `type_code()`を直接使い、caller-side `resolve_handle`を撤去した。test9の別責務のroot resolutionは保持し、pinned qpdf/flpdfのtests 7/8/9はstatus/stdout/stderr/生成`a.pdf`が一致した。D1 writer境界のためcase-level分類はmixedのまま。

2026-09-17（`flpdf-3yn9.48.140`）: qpdf test9（`qpdf/test_driver.cc:495-519`）はpublic `QPDF::getRoot()`でCatalogを取得する。flpdfの`run_test_9`を`Pdf::root_handle()`へ移行し、直接trailer `/Root`取得とcaller-side `resolve_handle`を撤去した。test7/8/test9のcaller-zero後に共有helperも削除し、pinned qpdf/flpdf test9のstatus/stdout/stderr/生成`a.pdf`と非辞書`/Root`のstatus/stdout/stderrが一致した。D1 writer境界のためcase-level分類はmixedのまま。

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

<!-- route-matrix-aggregate: document-tally unit=area-physical file=e-job-cli-capi.md -->

| 分類 | 件数 | 行 |
|---|---|---|
| canonical | 21 | E-1, E-2, E-3, E-4, E-5, E-6, E-8, E-9, E-11, E-12, E-13, E-14, E-16, E-18, E-20, E-22, E-23, E-24, E-25, E-26, E-27 |
| bridge | 0 | — |
| mixed | 8 | E-7, E-10, E-15, E-17, E-19, E-21, E-28, E-29 |
| unknown | 0 | — |

（合計 29 行。分類別の内訳は直上の表だけに書く。`E-18` は `mixed` に見えるが、CLI が使わないのは「別の正本がある」からではなく E-17 の帰結であるため `canonical`。`E-4` も同様に `canonical`——一度は `mixed` 側に誤って列挙されていた）

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
| `apply_rotate_to_pages` | absent（旧 `crates/flpdf/src/job/rotate.rs`） | なし（prod 0、test 0） | E-13。Job/CLIは`PageObjectHelper::rotate_page`へ直接移行済み。qpdfにない事前 validation と dead public batch helper、専用 `RotateMode`/`RotateOp` を `flpdf-v55s` で撤去し、旧helperへのテスト呼び出しを解消して有効動作テストをcanonical helperへ移行、専用invalid-targetテスト2件を削除した。 |
| `flatten_rotation_on_pages` | `crates/flpdf/src/job/rotate.rs` | `crates/flpdf/src/job/lifecycle.rs::prepare_document_transformations` | E-12。CLIのproduction direct callerは削除済み（qtest-v7vrのbounded cutover）。 |
| `optimize_images` | `crates/flpdf/src/job/image_optimization.rs` | `crates/flpdf-cli/src/main.rs:3043,3073,4292,4348,5725,5995` | E-12。6 箇所と本領域最多 |
| `should_remove_unreferenced_resources` | `crates/flpdf/src/job/resource_pruning.rs` | `crates/flpdf/src/job/page_merge.rs:968`（`pub(crate)`、Job内部callerのみ） | E-16。qpdf-private heuristicのfree public exportは`.48.99`で撤去済み |
| `copy_duplicate_page_annotations` | `crates/flpdf/src/job/page_specs.rs` | `crates/flpdf-cli/src/main.rs:5714` | qpdf 側は `handlePageSpecs` 内のインラインコード（`libqpdf/QPDFJob.cc:2359-2633`）で個別識別子なし → 7 の「独自命名は逸脱でない」に該当するが、`pub` の根拠は別途要る |
| `OverlaySpec` / `OverlayKind` | `crates/flpdf/src/job/overlay.rs` | `handle_under_overlay` の引数型。CLI は同名のローカル型（`crates/flpdf-cli/src/main.rs:6831,6842`）を parse に使い、library 型へは `build_overlay_specs` / `build_overlay_specs_with_suppression`（`crates/flpdf-cli/src/main.rs:7263,7271` が `Vec<flpdf::OverlaySpec<BufReader<File>>>` を返す）で変換する | 8 (E) が挙げた `handle_under_overlay` の**支援型**。第 4 の根拠が働くのは「legitimate な `pub` メソッドのシグネチャ」に対してであり、`handle_under_overlay` 自身が debt である以上こちらも従属 debt |
| `CombinedPlan` / `CombinedPage` / `InputSpec` | `crates/flpdf/src/job/page_combine.rs`（`#[cfg(test)]`、`pub(crate)`） | unit test のみ | qpdf 側に対応する public 識別子がないため、E-10 で public re-export と CLI依存を撤去。qpdf の public page-spec 設定は `QPDFJob::PagesConfig::pageSpec`（`include/qpdf/QPDFJob.hh:243-250`） |
| `PagePlan` / `SelectedPage` | `crates/flpdf/src/job/page_plan.rs`（`pub(crate)`） | `job/page_specs.rs` の内部 planner と unit test のみ | qpdf-less な中間表現を public API に残さず、`PageRange` と `QPDFJob` の page-spec 境界を public surface とする。E-10 で crate root/job module re-export と public example 依存を撤去 |
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
| (A) | `prune_acroform_after_subset` 系 3 個 + free `write_json` | **bounded slice 完了** — AcroForm free helpers は `pub(crate)`、job/json.rs の free writers は削除し、integration tests は `QPDFJob::write_json` method に移行した。remaining CLI/public decisions は E-11/E-29 と別 scope。 |
| (B) | `format_attachment_list_with_sink` / `AttachmentInfo` | `format_attachment_list_with_sink` は `pub(crate)` かつ job 内 caller のみになり、job/lib.rs の public re-export と crate-root export を撤去した。`AttachmentInfo` の caller-zero public projection は `flpdf-3yn9.48.97` で撤去し、qpdf job listing の sink/Job ownerだけを残した（E-9）。 |
| (C) | `build_*_section` 6 個 + `write_qpdf_json_v2_selected_objects*` 2 個 | **解消済み**（E-25）。`flpdf-7bkv` は closed（2026-09-06 readback） |
| (D) | qpdf rotation parser (`parse_rotation_parameter` / `RotationSpec`) | **E-14の限定slice完了** — qpdf-private parser/stateをcrate-internalへ狭め、job JSONのlifecycle直接parseとCLIの`QPDFJobConfig::rotate`経路だけを残した。parserのrouteはcanonical、`PageRange`/page-operation ownerのE-15/E-12残差は別スコープ |
| (E) | `overlay_verbose_report` / `handle_under_overlay` / `collate` | `overlay_verbose_report` / `handle_under_overlay` は debt のまま（E-11）。**`collate` は消滅** — `fn collate` は workspace に 0 件で、`page_collate.rs` というファイル自体が存在しない（`crates/flpdf/src/job/` の全 22 ファイルを `ls` で確認） |

### 2026-09-08 `.48.7` ordinary/rewrite Job cutover

`flpdf-3yn9.48.7` moved the ordinary/rewrite `run_rewrite_opened` consumer onto
`QPDFJob::apply_transformations` and `QPDFJob::write_qpdf`. The direct
`PdfWriter`, overlay, image, appearance, annotation, coalesce, rotation, and
page-label routes in that cohort are gone; remaining E-4/E-11/E-12 direct
callers belong to JSON/page-operation/inspection cohorts tracked separately.
The E-4/E-11/E-12/E-21 rows above retain their original matrix row identity and
are re-measured against this note during the next full route audit.

### E-9 / E-21 / E-29 attachment mutation consumer update (2026-09-10, `flpdf-3yn9.48.81`)

The three top-level mutation consumers (`run_add_attachment`,
`run_remove_attachment`, and `run_copy_attachments_from`) now configure one
`QPDFJob` and use `create_qpdf()` → `write_qpdf()` → `get_exit_code()`.
`QPDFJob::prepare_document_transformations` therefore owns the qpdf order,
writer configuration, warning completion, stdout reservation, and
`--replace-input` boundary. The copy donor remains a direct per-donor
`open_job_source` open inside `copy_attachments_with_opener`, preserving
qpdf's target filename and verbose/open-warning order (`QPDFJob.cc:2089-2135`);
it is not routed through `job.open_with_description`. The qtest exception
routes and the separate page-operation replace-input issue remain outside this
slice.

### E-9 / E-29 JSON donor input-policy propagation (`flpdf-rer4k`, 2026-09-15)

The JSON output route now sets the same global donor-open policies as the
ordinary attachment job: `passwordMode`, `passwordIsHexKey`,
`suppressPasswordRecovery`, `suppressRecovery`, and `ignoreXrefStreams`.
`QPDFJob::copyAttachments` opens each donor through the configured job policy,
so `open_job_source` receives the qpdf 11.9.0 interpretation and recovery
boundary (`QPDFJob.cc:650-666,1695-1711,2089-2135`). The focused
`cli_json_donor_policy.rs` matrix compares qpdf/flpdf exit status, JSON stdout,
and diagnostics for damaged, xref-stream, hex-password, password-recovery,
and hex-key donors. Donor authentication failures retain qpdf's donor-path
`invalid password` diagnostic.

### E-9 / E-21 donor password error ownership (`flpdf-ghk8d`, 2026-09-15)

qpdf's `QPDFJob::copyAttachments` opens each donor with `processFile` and lets
the password exception escape the copy loop (`libqpdf/QPDFJob.cc:2089-2100`).
The qpdf CLI catches that `std::exception` at its outer boundary and renders
`qpdf: <what()>` (`qpdf/qpdf.cc:32-43`). The library error classification is
therefore kept separate from the path-bearing CLI diagnostic.

flpdf no longer converts a donor `BadPassword` to `SystemBytes` inside
`prepare_document_transformations`. The public `QPDFJob::apply_transformations`
returns the typed `Encrypted(BadPassword)` source, while the CLI transformation
boundary calls `QPDFJob::report_job_error` using the failed donor name already
retained by `open_job_source`, then returns the existing exit-2 sentinel. Both
normal copy output and JSON copy output compare their donor-path stderr with
qpdf in `cli_json_donor_policy.rs`; this bounded ownership correction adds no
bridge or qpdf-deviation marker.

### E-9 / E-12 single inspection with attachment mutation (`flpdf-awthm`, 2026-09-15)

qpdf applies attachment remove/add/copy during `createQPDF` before
`writeQPDF` selects its output-free `doInspection` column
(`libqpdf/QPDFJob.cc:428-489,1646-1693,2046-2248`). The top-level CLI now
routes any single inspection selector accompanied by an attachment mutation
through the same combined `QPDFJob` configuration, so `--check`,
`--list-attachments`, and `--show-npages` observe the mutated document and
receive mutation errors before emitting inspection output. The `--pages`
combined route receives the same attachment configuration before `run()`,
preserving qpdf's page-selection → transformation → inspection lifecycle.
The qpdf 11.9.0 status/stdout/stderr matrix is covered by
`cli_inspection_combinations.rs` (eight non-page cases and two page-selection
cases). The JSON conflict cells remain the separately scoped
`flpdf-urjhr` slice.

### E-11 / E-12 linearized rewrite with overlay or underlay (`flpdf-tgpv7`, 2026-09-15)

qpdf has no configuration conflict between underlay/overlay and linearized
output. `createQPDF` applies `handleUnderOverlay` before
`handleTransformations`, while `writeQPDF` configures the linearized writer
after the create stage (`libqpdf/QPDFJob.cc:428-507,1937-2043,2835-2920`).
The rewrite CLI now keeps those options on the existing `QPDFJob` instead of
returning a local conflict error. Overlay, underlay, and linearize-before-
overlay argument order are compared byte-for-byte with qpdf 11.9.0 under
`qpdf-zlib-compat` in `overlay_transform_order_route_tests.rs`; each output
also passes `qpdf --check` as linearized.

## unknown / probe

| ID | 決められないこと | 必要な source / probe |
|---|---|---|
| P-1 | E-2 の public 2段契約の観測 — create が返す時点の変換済み状態、consumer の追加変更、write の最終出力を確認する | `initialize_from_json` または対応する Config で同じ変換設定を構築し、`create_qpdf()` → 返却PDF観測/追加変更 → `write_qpdf()` を qpdf の同等 API/CLI と比較する。現 `initialize_from_argv` は `--rotate` 等を扱わないため、同 argv を渡す旧 probe は E-17 の未実装に阻まれる。変換の欠落は source で確認済みだが、出力差と lifetime/warning 契約を differential で確定する。semantic 非等価性と mixed route 分類は別軸 |
| P-2 | **解決済み（2026-09-07、`.48.103` 反映 2026-09-15）**: E-28 の case/API owner 対応付け | `driver/*.rs` の 99 ケース全件を imports・型経由メソッド・実際の呼出し順序を含めて qpdf の同じ case と A〜D owner に対応付けた。結果は「E-28 detail」表（`canonical` 28 / `mixed` 67 / `bridge` 3 logical / `unknown` 0）。実装cutover自体は各 issue の担当範囲で別途行う（本 issue の対象外） |
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
| `E-11` | `flpdf-3yn9.48.92` | `--pages` post-plan overlay/underlay consumerをcanonical QPDFJob ownerへ移行する |
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
| `E-27` | `flpdf-3yn9.48.44` | qtest test0/1のwarning attributionをcanonical handleへ移す（前段） |
| `E-27` | `flpdf-3yn9.48.93` | qtest test0/1をcanonical pipe/loggerへ移しrecovering event/手製stream診断を撤去する |
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

`--generate-appearances` は create-stage の AcroForm 変換だけを有効にし、writer の
stream decode level を暗黙には変更しない。qpdf の `Members::decode_level` の初期値は
`generalized` だが、`QPDFJob::setWriterOptions` が `QPDFWriter::setDecodeLevel` を呼ぶのは
`decode_level_set` が真のときだけである（`include/qpdf/QPDFJob.hh:632-637`、
`libqpdf/QPDFJob.cc:2847-2875`）。従って `--generate-appearances` 単独では
`QPDFWriter::doWriteSetup` の `stream_decode_level` 起点の
`initializeSpecialStreams`／page-tree walk（`libqpdf/QPDFWriter.cc:2114-2116`）を
発生させない。その writer 設定を変える経路は 3 つある——`--decode-level` の明示指定、
`--stream-data` の明示指定、そして **QDF の暗黙既定**である。qpdf は
`initializeSpecialStreams` を `m->qdf_mode || m->normalize_content ||
m->stream_decode_level` で起動するため（`libqpdf/QPDFWriter.cc:2114-2116`）、
`--qdf` は decode level の明示なしに page-tree walk を起こす。flpdf 側も
`WriterSettings::to_write_options` が `qdf_mode && !decode_level_set` のとき
`DecodeLevel::Generalized` を既定に置く（`crates/flpdf/src/writer/settings.rs:123-128`）。
実測でも `--qdf` 単独で qpdf・flpdf とも `direct; converting to indirect` 警告を出す。
この区別は `crates/flpdf-cli/src/main.rs` の writer configuration consumer と
D26 の page-repair trigger の両方で維持する。QDF を除外し忘れると D26 の
trigger 一覧が不完全になる。

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

## 2026-09-15: qpdf-accepted writer and attachment inspection combinations (`flpdf-urjhr`)

qpdf 11.9.0 の `QPDFJob::checkConfiguration` は、output-free inspection と
`--encrypt` / `--decrypt` / `--linearize` の組み合わせを拒否しない。
`createQPDF` の `handleTransformations` と `writeQPDF` の
`!createsOutput() -> doInspection` 分岐が責務を分けているためである
（`libqpdf/QPDFJob.cc:428-520,567-642`）。JSON outputも同じく、
`writeOutfile` 内でJSON serializerを選択する前にcreate-stageを完了する
（`libqpdf/QPDFJob_config.cc:311-324`; `libqpdf/QPDFJob.cc:3030-3057`）。

flpdf はこのqpdf境界に合わせ、top-level `Cli` の qpdfに存在しない
conflicts_withを、10通り（attachment mutation/JSON、encrypt・decrypt・linearizeと
JSON、encrypt・decryptとcheck/show-npages）から除去した。attachment mutationを伴う
JSONは既存の `QPDFJob::apply_transformations` に設定を渡してから
`QPDFJob::write_json_with_version` を呼び、独自の検査前mutation routeを追加しない。

`crates/flpdf-cli/tests/cli_qpdf_conflict_matrix.rs::qpdf_writer_and_attachment_conflicts_match_qpdf`
がpinned qpdf 11.9.0とのstatus/stdout/stderrを10通り比較する。`flpdf-awthm` の
list/check/show-attachment mutation consumerは別のbounded sliceとして残る。

### E-17 / E-21 remaining writer/inspection conflict acceptance (`flpdf-zet2u`, 2026-09-15)

The top-level CLI no longer rejects the nine qpdf-accepted writer/inspection
combinations left after `flpdf-urjhr`: encryption with linearization or page/
encryption inspection, decryption with encryption/page inspection,
`compress-streams=n` or `qdf` with JSON, and rotation/page selection with
`check-linearization`. The change removes only clap conflict edges; qpdf's
existing `QPDFJob::createQPDF` -> `writeQPDF`/`doInspection` ordering remains the
owner (`libqpdf/QPDFJob.cc:428-520,566-641,1646-1693`). Since qpdf's
`Config::jsonOutput` calls `json`, the `qdf` and `compress-streams=n` acceptance
is covered for `--json-output` as well.

The expanded `cli_qpdf_conflict_matrix.rs` compares the original ten cells, the
nine remaining cells, and two JSON-output symmetry cells against qpdf 11.9.0,
including exit status, stdout, stderr, and JSON output bytes. Other
inspection-route conflict edges remain outside this bounded E-17/E-21 slice.

### E-17 / E-21 JSON writer-option conflict acceptance (`flpdf-p50gt`, 2026-09-15)

qpdf 11.9.0 also accepts the nine combinations formed by
`--static-id`, `--deterministic-id`, or `--coalesce-contents` with each of
`--json-output=2`, `--json`, and `--json=2`. These options are not rejected by
`QPDFJob::checkConfiguration`; qpdf completes create-stage transformations
before selecting JSON serialization (`libqpdf/QPDFJob.cc:459-480,567-642,3030-3057`;
`libqpdf/QPDFJob_config.cc:88-92,162-166,247-325,619-623`).

The top-level flpdf JSON declarations now retain their unrelated bounded
inspection conflicts but remove the six clap conflict entries that caused
these nine combinations. The three JSON input branches pass
`coalesce_contents` through the existing
`QPDFJob::apply_transformations` boundary before
`QPDFJob::write_json_with_version`; the ID flags remain writer-only settings
for JSON. `cli_qpdf_conflict_matrix.rs::qpdf_id_and_coalesce_json_conflicts_match_qpdf`
compares all nine cells with qpdf 11.9.0 for exit status, stdout, stderr, and
JSON output bytes. This is a bounded E-17/E-21 correction and does not claim
route-wide removal of other conflict edges.

### E-13 / E-21 JSON rotation consumer (`flpdf-lvvvk`, 2026-09-16)

qpdf applies page selection, then `handleRotations`, then underlay/overlay and
the remaining `handleTransformations` before JSON serialization
(`libqpdf/QPDFJob.cc:459-480,2635-2652`). Its `Config::rotate` stores the
validated raw rotation parameter before that create-stage lifecycle
(`libqpdf/QPDFJob.cc:368-415`; `libqpdf/QPDFJob_config.cc:786-790`).

The flpdf JSON route now queues every `cli.page_ops.rotate` parameter on the
existing `QPDFJob::Config::rotate` before opening its input branches. The
existing `QPDFJob::apply_transformations` boundary then applies the rotation
after JSON page selection and before the JSON writer, preserving qpdf output
page numbering and transformation order. The qpdf differential tests cover
`--rotate=90/180/270` with both `--json` and `--json=2`, plus the corresponding
`--json-output=2` file outputs. The coalesce JSON overlap was resolved by the
merged `flpdf-p50gt` PR #2002; this issue owns rotation only and makes no
route-wide JSON parity claim.

### E-13 / E-21 standalone inspection rotation consumer (`flpdf-9r7ti`, 2026-09-16)

qpdfの `createQPDF` applies `handleRotations` before `writeQPDF` selects the
`doInspection` consumer, including `doShowObj`
（`libqpdf/QPDFJob.cc:459-480,483-490,1645-1689,2635-2652`）。従って
`--rotate`と`--show-object`は回転後のページ辞書を観測する。

flpdfのstandalone inspection callersは、`InspectionTransformOptions`が保持する
raw rotation parametersを共通 `configure_top_level_inspection_transformations`
へ渡し、既存の `QPDFJob::apply_transformations` / `apply_configured_rotations`
で処理する。これにより `run_show_object`を含む各inspection routeが同じ
rotation-before-consumer境界を共有する。qpdf differential testは
`cli_inspection_combinations.rs::standalone_show_object_inspection_applies_rotation_before_the_consumer`
で90/180/270度を固定し、個別show-object用のrotation parserやbridgeは追加しない。

### E-13 / E-21 standalone rotation usage preflight (`flpdf-9orwb`, 2026-09-16)

qpdfの `Config::rotate` は argv callback中に
`parseRotationParameter`を実行し、`createQPDF`の入力 `processFile`より前に
不正値をusageとして報告する（`libqpdf/QPDFJob_config.cc:786-790`、
`libqpdf/QPDFJob.cc:368-415,428-435`）。

flpdfはtop-level raw rotation parametersをdispatch/input open前に既存の
`QPDFJob::Config::rotate`へpreflightし、typed `UsageError`を共通の
`usage_exit`へ渡す。valid値は従来の各standalone consumerが同じConfig setterへ
設定し、既存のcreate-stage rotation/inspection順序を保持する。
`cli_inspection_combinations.rs::standalone_inspection_reports_invalid_rotation_before_input_open`
は12個のstandalone inspection/status routeについて qpdf 11.9.0の
exit/stdout/stderrをmissing inputで比較する。個別parser、bridge、deviation markerは
追加しない。

### E-17 / E-21 full conflict declaration audit (`flpdf-sg6tu`, 2026-09-16)

qpdf 11.9.0 の `QPDFJob::checkConfiguration` は
`--replace-input` と output/`--split-pages`/`--json`/`--empty`、
output-free inspection と output、および `--requires-password` と
`--is-encrypted`だけを usage 境界にする
（`libqpdf/QPDFJob.cc:567-631`）。`createQPDF` は
page/rotation、underlay/overlay、`handleTransformations` を先に実行し、
`writeQPDF` は同じ documentを `doInspection` または outputへ渡す
（`libqpdf/QPDFJob.cc:428-520,1646-1693,2046-2247`）。

`flpdf-sg6tu` では `main.rs` の conflicts_with 系 27 宣言を
定義側で列挙し、qpdf semantic guardだけを保持した。writer-only、
transformation、JSON input/update、overlay、attachment、password/password-file
の qpdf 非対応 guard は撤去し、attachment mutationの parser-level ArgGroupも
撤去した。既存の `QPDFJob::check_configuration`、
`create_qpdf`、`write_qpdf`、`run_configured_inspection` を
canonical ownerとして使い、remove → add → copy、page selection → overlay →
transformation → inspectionの順を保つ。native subcommand境界と
`--repair`を含む recovery safety guard、native `rewrite` の
encryption mode guardは qpdf flat grammarとは別のため残す。

`cli_qpdf_conflict_matrix.rs` は writer/create-stage 22 種 × inspection
12 種を固定配列で列挙し、264 組の qpdf 11.9.0 status/stdout/stderrを比較する。
JSON input/updateの selector と attachment mutation、mixed attachment order、
attachment + overlay、password setter order、job-json-fileの
check-linearizationも個別の qpdf differentialで固定する。qpdfに対応する
独自 bridgeや deviation markerは追加しない。

### E-12 JSON input/update inspection continuation (`flpdf-lm4bc`, 2026-09-16)

qpdf の `createQPDF` は JSON input の作成と update を終えてから
rotation、underlay/overlay、`handleTransformations` を同じ document に適用し、
`writeQPDF` は output-free の場合にその documentを `doInspection` へ渡す
（`libqpdf/QPDFJob.cc:428-520,1646-1693,1937-2015,2138-2194`）。
`checkConfiguration` は `show-attachment` の save pipeline を先行して予約する
（`libqpdf/QPDFJob.cc:614-626,914-925`）。

flpdf の JSON input/update inspection route は、overlay/underlay と create-stage
transformations を `QPDFJob::apply_transformations` へ積み、JSON update後に
同じ job documentへ適用する。`show-attachment` の stdout は JSON import/open
前に予約し、inspection は `QPDFJob::inspect_configured` の独立 report/completion
へ接続する。これで `show-npages` の info report が attachment payload の
save streamを先取りせず、overlay/underlay が `show-object` の観測対象になる。

`cli_qpdf_conflict_matrix.rs` の JSON input/update differential は、両入力経路の
`show-npages` + `show-attachment`、overlay/underlay + `show-object` を qpdf 11.9.0
と status/stdout/stderr で固定する。新しい bridgeや deviation markerは追加しない。

### E-12 follow-up: job-json and CLI occurrence order (`flpdf-u40ck`, 2026-09-16)

qpdfの argv parserは各callbackを入力順に実行し
（`libqpdf/QPDFArgParser.cc:433-555`）、`Config::jobJsonFile`は各JSONを
既存Configへ `initializeFromJson(..., true)` として積む
（`libqpdf/QPDFJob_config.cc:16-62,449-460,625-697,774-784`、
`libqpdf/QPDFJob_json.cc:611-625`）。その後 `createQPDF`/`run` はその最終stateを
一度だけ消費する（`libqpdf/QPDFJob.cc:428-480,513-520`）。

`flpdf-u40ck` は `arg_parser.rs` のraw residual argvを
`main.rs::qpdf_cli_events`へ渡し、`run_job_json_files`でJSON file、
input/output selector、password/password-file、password mode/hex-key/recovery、
check-linearizationをoccurrence順に同じ `QPDFJob`へ適用する。partial JSONの初回も
既存configurationを保持するため、argv前置のCLI stateがJSON handlerで失われない。
`cli_job_json.rs` の5つのorder regressionと、lifecycleの
`partial_job_json_preserves_preconfigured_qpdf_state`が qpdf 11.9.0 の
status/stdout/stderrおよびConfig layeringを固定する。image transformation setter
のjob-json後argv layering実行経路は親issue `flpdf-uwu7`に残るが、
`flpdf-uwu7.1`で`QPDFJobConfig`のkeep/threshold mutation boundaryは追加済み。

### E-12 follow-up: job-json selector usage attribution (`flpdf-n9q36`, 2026-09-16)

qpdf の `Config::jobJsonFile` は JSON の partial initialization failureだけを
file-scoped errorへ変換する（`libqpdf/QPDFJob_config.cc:774-784`）。同じ argv scanの
後続 positional input/output は通常の Config callbackであり、重複時は
`QPDFArgParser::usage` / qpdf CLIの usage exit による bare usage errorとなる
（`libqpdf/QPDFJob_argv.cc:71-82,402-430`; `qpdf/qpdf.cc:12-22,37-38`）。

flpdf は `run_job_json_files` の JSON file eventだけを
`format_job_json_error`で包み、CLI selector eventの typed `Error::Usage`は
`main`の共通 usage exitへ渡すようにした。これにより JSON 後の input/output
重複は qpdf と同じ帰属・`For help:` blockになり、JSON handler 内で検出される
JSON前 selectorとの重複は従来どおり job-json fileへ帰属する。
`cli_job_json.rs::job_json_file_selector_errors_follow_argv_order` は JSON前後の
input/output 4ケースを qpdf 11.9.0 と status/stdout/stderrで固定する。
新しい bridgeや deviation markerは追加しない。

### E-12 follow-up: job-json file-open error attribution (`flpdf-tyu7s`, 2026-09-16)

qpdfの `Config::jobJsonFile` は `read_file_into_string` と
`initializeFromJson(..., true)`を同じ例外境界で処理し、file path付きの
job-json errorへ変換する（`libqpdf/QPDFJob_config.cc:774-784`）。
argv parserのusage境界とCLIの `usageExit` により、`Run --job-json-help` と
`For help:` blockも同じ診断へ含まれる（`libqpdf/QPDFJob_argv.cc:408-415`;
`qpdf/qpdf.cc:11-23,32-41`）。

flpdfは `JobJsonFile` の read failureを既存の
`qpdf_json_input_open_error`で `open <path>` とportableな strerrorへ正規化し、
`job_json_event_error`へ渡すようにした。JSON parse/config failureと同じ
job-json attributionを保持し、Rust固有の `(os error N)`を出さない。
`cli_job_json.rs::job_json_file_missing_reports_job_json_context_and_usage`で
missing job-jsonのexit/stdout/stderrをqpdf 11.9.0と比較する。新しいbridgeや
deviation markerは追加しない。


### E-17 bounded canonical raw argv initializer (`flpdf-3yn9.48.147`, 2026-09-17)

The pinned qpdf boundary is `QPDFArgParser::parseArgs` → `QPDFJob::Config`
callbacks → `run` / `getExitCode` (`qpdf/qpdf.cc:27-43`,
`libqpdf/QPDFArgParser.cc:429-566`, and `qpdf/auto_job_init.hh`). The
library now exposes `QPDFJob::initialize_from_raw_argv`, and the UTF-8
`initialize_from_argv` convenience wrapper delegates to it. The parser owns
the generated main/pages/encryption/underlay-overlay/attachment/copy-attachment/
page-label option tables, raw Unix path/password bytes, one-level `@file`
expansion, top-level `--` reset, immediate parameter/choice validation, and
same-job `jobJsonFile` layering.

The existing qtest/C API consumers remain on this canonical initializer, while
the production CLI is intentionally not switched in this prerequisite issue;
E-17/E-21 therefore remain `mixed` until the separate flat-CLI consumer
migration. Focused raw-boundary tests cover the option registry, non-UTF-8
argv, page/encryption segments, job-JSON occurrence ordering, and a qpdf 11.9.0
output differential.

### E-17 / E-21 argv-order parse validation (`flpdf-godwa`, 2026-09-16)

qpdfの `QPDFArgParser::parseArgs` は required parameter / choices と各 callbackを
argv occurrence順に処理する（`libqpdf/QPDFArgParser.cc:433-551`）。
`rotate`、optional `collate`、`json`、`json-output`はmain option tableで
それぞれ登録され、`Config::rotate` / `Config::collate` / `Config::jobJsonFile`は
callback内で直ちに検証・readを行う（`libqpdf/qpdf/auto_job_init.hh:108,113,126-127`;
`libqpdf/QPDFJob_config.cc:95-125,253-263,312-325,774-784`）。

flpdfは既存raw residual argv eventを拡張し、parse-time optionsとJobJsonFile/selector
stateを一つのpreflightで左から処理する。preflightで構築したjob stateを実行側へ
引き渡すため、JSONとその side fileを二重readしない。これにより先行した
rotate/json/json-output/collateまたは
missing job-json fileの診断が後続の固定順 validationに追い越されない。
`cli_job_json.rs::top_level_parse_errors_follow_qpdf_argv_order`で8つの相対順を
qpdf 11.9.0とexit/stdout/stderr比較する。job-JSON transformation wiringは
`flpdf-uwu7`の別責務であり、新しいbridgeやdeviation markerは追加しない。

### E-12 follow-up: job-json prepared job state (`flpdf-7wct0`, 2026-09-16)

qpdf CLIは1つの `QPDFJob`に `initializeFromArgv` → `run`を続けて呼び、
`jobJsonFile`は同じ Configへ `initializeFromJson(..., true)`を重ねる
（`qpdf/qpdf.cc:27-44`; `include/qpdf/QPDFJob.hh:78-90`;
`libqpdf/QPDFJob_config.cc:774-784`）。JSONの `passwordFile`も通常の
`Config::passwordFile` callbackとして同じjob stateへ先頭行を保存する
（`libqpdf/qpdf/auto_job_json_init.hh:29-31`; `libqpdf/QPDFJob_config.cc:661-680`）。

flpdfは argv-order preflightが構築した `QPDFJob`自体を execution routeへ moveし、
JSON bytesを別jobへ再初期化しない。CLI `PasswordFile`も preflight中に occurrence順で
一度だけ適用するため、JSON内 passwordFile を含む side fileは二重readされない。
`cli_job_json.rs::job_json_password_file_is_read_once_like_qpdf`は Linuxの one-shot
FIFOを qpdf 11.9.0 と flpdfへ渡し、両者の非ブロック成功を固定する。image
transformation wiringは `flpdf-uwu7`の別責務に残し、新しい parser、cache、bridge、
deviation markerは追加しない。

### E-17 / E-21 immediate callback validation continuation (`flpdf-sk77s`, 2026-09-16)

qpdfの main option table は `addRequiredParameter` / `addOptionalParameter` /
`addChoices` で callback と choices を登録し、`QPDFArgParser::parseArgs` は
required parameter / choices の検証直後に同じ argv occurrenceの callbackを実行する
（`libqpdf/qpdf/auto_job_init.hh:92-127`; `libqpdf/QPDFArgParser.cc:433-555`）。
`compression-level`、`ii-min-bytes`、`keep-files-open-threshold`、
`oi-min-area`/`height`/`width`、`split-pages`、`show-object` は callback内で
numericまたは object selector を即時検証し、callbackの runtime errorは
`QPDFUsage`へ変換される（`libqpdf/QPDFJob_config.cc:95-139,232-235,350-353,597-609,766-770`;
`libqpdf/QPDFJob.cc:929-941`; `libqpdf/QPDFJob_argv.cc:408-415`;
`libqpdf/QPDFArgParser.cc:337-344`）。

flpdf は `qpdf_cli_events` にこの8 optionを追加し、prepared `QPDFJob` の
preflightで左から同じ順に検証する。numeric failureは `usage_exit`へ渡し、
image thresholdは qpdfの `QUtil::string_to_uint` に対応する direct unsigned
conversionを使う。split/threshold/show-objectの状態はprepared jobへ保持する。
`cli_job_json.rs::top_level_parse_errors_follow_qpdf_argv_order` と
`top_level_image_thresholds_keep_qpdf_unsigned_prefix_semantics` が qpdf 11.9.0と
status/stdout/stderrを比較する。新しい parser、bridge、side-file cache、
deviation markerは追加しない。

### E-17 preflight canonical initializer cutover (`flpdf-3yn9.48.189`, 2026-09-19)

`main.rs::preflight_qpdf_cli_events`（当時 `qpdf_cli_events` という26variant
event enumの手書き再実装、上記2セクションが記録した curated subsetのみ認識）を、
`QPDFJob::initialize_from_raw_argv`（`flpdf-3yn9.48.147`）への直接呼び出しへ
置き換えた。qpdfの `initializeFromArgv → run → getExitCode`
（`qpdf/qpdf.cc:27-43`）と同じ境界で、non-subcommand invocation全てに対し
unconditionalに（clapより前の元の位置のまま）実行する。

先行する2回の同種の置き換え試行（`flpdf-nx50y`）はいずれもgreenなtest suiteを
regressionさせてrevertされ、その実証的証拠が本issueに記録されていた——
本swapが実証したのは、その entanglement が **one-way**（2方向ではない）
だったという点: 先行試行の Direction B（`--repair` filter + `--job-json-file`
gatingの両方を同時に変更）が示した2つ目のregression
（`standalone_inspection_reports_invalid_rotation_before_input_open`相当）は
gating単体が原因であり、`--repair` filterとは独立していた。`--repair`
filterのみを適用しgatingなしでunconditional呼び出しのままにしたところ、
先行試行が壊した3つのtest fileすべてがgreenになった。

flpdf-cli固有のargv調整は2つのみ: (1) `--repair`は
`libqpdf/qpdf/auto_job_init.hh`の124option registryに存在しない唯一の
top-level flag（`PasswordArgs`/`PageOpArgs`のflattened fieldを含め全field
監査済み）としてargvから除外する。(2) `--password-file`は
`--job-json-file`不在時のみ除外する——そのConfig callbackは
`libqpdf/QPDFJob_config.cc`中でargv scan中に即座にファイルを読む唯一2つの
callback（`passwordFile:661`、`jobJsonFile:774`）の一つで、discardされる
検証passで実行すると後段のclap駆動経路が同じファイルを再度読み警告が
二重出力される。message prefixは raw argv[0] ではなく `progname()` から
設定し、`FLPDF_PROGNAME`（qtest harness shim、qpdf非対応）を引き続き
尊重する。

この swap 自体で E-17/E-21 の分類は変わらない（`mixed` のまま）——
flpdf-cli の他の192箇所超の setter 呼び出しは未移行。ただし
`crates/flpdf/src/job/argv.rs::apply_job_json_file` の open-error rendering
（non-UTF-8 pathでlossy、Rustの `(os error N)` suffix付き、CLI側の
既存修正済み `qpdf_json_input_open_error` と不一致）という新規bugを
本番経路が初めて通ったことで発見・修正した。約12件のtestが clap の
汎用 "cannot be used with" や不完全な usage-error blockを assertしていた
（実際のqpdf 11.9.0は "no output file may be given for this option" や
先頭空行+完全な "For help:" blockを返す）既存の逸脱で、これも修正した。
残る argv.rs wording gap は Attachment table の unrecognized-token の 1 件で、
このissueのスコープ外として `flpdf-3yn9.48.193` へ分離した。
**2026-09-19 更新**: 当初ここに併記していた split-pages の overflow wording は
本 PR で解消済み——`parse_job_split_pages` が `qpdf_string_to_int_checked` の
メッセージを捨てて独自文字列に差し替えていたのを、30 行下の
`parse_job_compression_level` と同形（`Overflow(message) => Err(Error::System(message))`）
に揃え、i32/i64 両方の overflow を qpdf 実測文言で固定するテストを追加した。
`cli_job_json.rs` の `split-pages-before-job-json` skip も撤去済み。

### E-12 follow-up: job-json directory read diagnostic (`flpdf-jhaqf`, 2026-09-16)

qpdf の `Config::jobJsonFile` は `read_file_into_string` の例外を job-json
contextへ包むが、directory専用の意味論やエラーメッセージは定義しない
（`libqpdf/QPDFJob_config.cc:774-784`; `libqpdf/QUtil.cc:490-525,1167-1214`）。
Linux の qpdf 11.9.0 が返す `basic_string::_M_create` は pinned qpdf sourceに
存在しない libstdc++ `std::string` allocation artifactであり、Rust側で
hardcodeしない。

flpdf は `IsADirectory` を `open <path>: Is a directory`として報告する。
directory-only branchには `qpdf-deviation` markerを置き、通常の missing/
permission wordingとは分離する。`cli_job_json.rs::job_json_file_directory_keeps_the_portable_flpdf_diagnostic`
は qpdf の artifactとflpdfのportable診断を Linux でcharacterizeし、exit 2・
stdout・各内側メッセージを検証する。新しいparserやbridgeは追加しない。

### E-12 follow-up: job-json non-UTF-8 fatal path (`flpdf-ktd5p`, 2026-09-16)

qpdf は native `char* argv`を `std::string`として `QPDFJob::Config::jobJsonFile`
へ渡し、read failureを raw path付きの job-json errorへ変換する
（`qpdf/qpdf.cc:27-60`; `libqpdf/QPDFJob_argv.cc:402-427`;
`libqpdf/QPDFJob_config.cc:774-784`）。内側の `open <path>: <strerror>`も
`QUtil::safe_fopen`と`QPDFSystemError`が同じ bytesで組み立てる
（`libqpdf/QUtil.cc:490-525`; `libqpdf/QPDFSystemError.cc:5-29`）。

flpdf は existing `path_description`/`emit_logger_error`と、closed
`flpdf-8k4e`の `Error::SystemBytes`/`raw_message`を利用する。job-jsonだけを
`CliRawExitError`とbyte formatterへ接続し、`format_job_json_error`と
`qpdf_json_input_open_error`の `Path::display()`による U+FFFD置換を除去する。
`cli_job_json.rs::job_json_file_missing_preserves_non_utf8_path_bytes`は Linuxで
raw `\\xff\\xfe` pathを qpdf 11.9.0 と比較し、status/stdout/stderr全体の一致を
確認する。通常の `CliExitError` callers、parser、bridge、deviation markerは
変更しない。

### E-17 / E-21 job-json-file bare main-table option whitelist (`flpdf-3yn9.48.190`, 2026-09-19)

qpdf の main option table は多数の bare（値なし）callback を `addBare` で
登録し（`libqpdf/qpdf/auto_job_init.hh:30-95`）、`QPDFArgParser::parseArgs`
は `--job-json-file` の有無に関わらず、同じ 1 回の左→右 argv scan の中で
各 callback をその出現位置で実行する（`libqpdf/QPDFArgParser.cc:433-555`,
`libqpdf/QPDFJob_argv.cc`）。例えば `qpdf --empty --qdf
--job-json-file=job.json out.pdf` は `qpdf --empty --qdf out.pdf` と同じ
`%QDF-1.0` マーカーを書く（11.9.0 実機で確認済み）。

`main.rs` は `--job-json-file` 経路専用に `qpdf_cli_events`/
`preflight_qpdf_cli_events`（前掲の E-12 follow-up 群）で raw residual argv
から別の `QPDFJob` を組み立てており、clap が解釈済みの `Cli` フィールド
（`args.qdf` 等）はこの経路では一切参照されない（`args.no_warn` と
`warning-exit-0` グローバルの 2 つだけが値渡しで例外的に橋渡しされており、
いずれも「on にしか倒せない」単調な OR として安全）。`qpdf_cli_events` の
whitelist はこの bare callback のうち 24 個を欠いており、`--job-json-file`
と組み合わせると無言で drop されていた（`flpdf-3yn9.48.190`、`--qdf` が
確認済みの再現例）。

flpdf は、qpdf callback が副作用のない純粋な `QPDFJob` configuration 代入
（`crates/flpdf/src/job/argv.rs::Parser::parse_main_argument` の main-table
bare arm）であり、かつ既存の public `QPDFJob`/`Config` setter が既にある
24 option を `qpdf_cli_events`/`preflight_qpdf_cli_events` に追加した
（`crates/flpdf` 側の新規 public API は追加していない）。named-segment opener
（`--pages`/`--overlay`/`--encrypt`/`--add-attachment`/
`--copy-attachments-from`/`--underlay`、`is_named_segment_option` が別途
処理）、既存 whitelist 済み option、`args.no_warn`/`warning-exit-0` の
既存橋渡しで実質的に動作済みの option は対象外。値を取る
`addRequiredParameter`/`addOptionalParameter`/`addChoices` option と、
public setter が存在しない残り 11 個の bare option（`--decrypt` 等）は
`flpdf-3yn9.48.191` へ分離した。**2026-09-19 追記**: `--linearize` も当初この
whitelist に入れていたが、flpdf の `set_linearization(value, pass1)` が
`linearize` と `linearize_pass1` を同時に代入するため job JSON の
`linearizePass1` を消す回帰になる（qpdf の `Config::linearize()` は
`libqpdf/QPDFJob_config.cc:362-368` で `linearize` だけを立てる）。撤去して
`flpdf-3yn9.48.191` へ移した。また `is_named_segment_option`
（`crates/flpdf-cli/src/main.rs`）は 6 個しか持たないが、qpdf では
`--set-page-labels` も segment opener である（`libqpdf/QPDFJob_argv.cc:377` の
`selectOptionTable(O_SET_PAGE_LABELS)`）——この欠落は本 PR 以前からの別問題で
`flpdf-sydkv` で追跡する——前者は `flpdf-3yn9.48.189` の regression #2
と同型の validation-timing リスクを個別に検証する必要があり、後者は
`crates/flpdf` への新規 public API 追加を要するため。

`main.rs::tests::job_json_file_route_applies_qdf_like_the_ordinary_route`
（issue の再現例そのもの、`%QDF-1.0` マーカーの有無を byte 比較）と
`::job_json_file_route_applies_deterministic_id_like_the_ordinary_route`
（`--deterministic-id` の効果を 2 回の独立実行の byte 一致/不一致で観測）が
実際の wiring を検証する。`qpdf_cli_events`/`preflight_qpdf_cli_events`は
main.rs private のため、これらのテストは `crates/flpdf-cli/src/main.rs`
自身の `#[cfg(test)] mod tests` に置く（`cli_job_json.rs` 等の別ファイル
統合テストからは到達できない）。新しい bridge や deviation marker は
追加しない。

## E-10 primary document graph retention in distinct-secondary `--pages` (`flpdf-lrm3u`, 2026-09-15)

qpdf keeps the primary `QPDF` as the page-job base while
`QPDFJob::handlePageSpecs` removes and re-adds pages
(`libqpdf/QPDFJob.cc:2462-2472`). Its later page loop changes PageLabels and
AcroForm field ownership, but does not replace the primary Catalog or trailer
(`libqpdf/QPDFJob.cc:2514-2632`). The writer retains all trailer keys outside
the exact `getTrimmedTrailer` set, including `/F`, `/FFilter`, and
`/FDecodeParms`, and enqueues their referenced graph
(`libqpdf/QPDFWriter.cc:2009-2031,2907-2925`). Direct `/Root` remains a direct
Catalog value through `getRoot` and `unparseChild`
(`libqpdf/QPDF.cc:2349-2358`; `libqpdf/QPDFWriter.cc:1144-1155`).

`job/page_merge.rs` owns the fresh-target handoff for this route. It copies
primary Catalog siblings and root `/Pages` non-structural values through the
canonical foreign copier, preserves qpdf's trailer key boundary, and restores
the source direct/indirect `/Root` shape after shared page and AcroForm
mutations. The primary copier receives the writer-mode boundary: qpdf's
stale-generation removal is enabled only when Preserve has source ObjStms
without `--preserve-unreferenced`, or when Generate invokes
`getCompressibleObjGens`; Disable, Preserve+`--preserve-unreferenced`, and
ordinary foreign-copy calls retain an absent lower generation as an indirect
null (`libqpdf/QPDF.cc:1952-1959,2392-2433`; `libqpdf/QPDFWriter.cc:1939-1983`).
The preserve-unreferenced queue orders imported handles by primary source
identity before assigning output numbers, matching qpdf's `getAllObjects`
seed order (`libqpdf/QPDF.cc:1285-1294`; `libqpdf/QPDFWriter.cc:2907-2925`).
Generic `merge_documents` and the qpdf job consumer retain their separate
field-selection boundaries.

The qpdf differential regression
`crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs::pages_preserves_primary_document_graph_edge_fixtures`
covers `acroform-sig-parent-pure-widget-kid`,
`null-visible-preserve-empty-removed`,
`null-visible-stale-generation`,
`pages-ext-firstpage-shared-one-page`,
`trailer-external-file-keys`, and `direct-root-one-page`, plus the existing
primary metadata test. The preserve-unreferenced order regression is kept in
the same test module. This is a bounded E-10 primary-graph slice; shared-page
page loss and ObjStm member-order differences are not folded into it.
