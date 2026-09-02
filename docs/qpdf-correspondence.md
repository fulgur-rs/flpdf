# flpdf ↔ qpdf 責務対応表

**Oracle:** qpdf 11.9.0 (`v11.9.0`) — `libqpdf/`
（`scripts/fetch-qpdf-source.sh` で取得。パスは `--print-path` で解決する。
本表のファイル名・行数はすべてこのツリーに対するもの。将来 v12 に追従する際は
`git log v11.9.0..v12.0.0 -- libqpdf/` が移植差分になる）
**調査日:** 2026-07-29（初版）/ 2026-08-02（Phase 2 着手時の再測 — `flpdf-1e5g`）/
2026-08-16（`flpdf-egzr`/`flpdf-3yn9` 大量クローズ後の再測）
**関連:** `flpdf-qxba`（部品積み上げによる責務分割）/
[設計書](superpowers/specs/2026-07-25-qpdf-component-bottom-up-refactor-design.md)

pre-v1.0 の byte-identical 模倣方針（`CLAUDE.md`）に対し、flpdf の責務分割が qpdf と
どこまで対応しているかのスナップショット。`flpdf-qxba` の work-list であり、Phase 1
完了後に再測する。

### 2026-08-02 の再測（`flpdf-1e5g` / Phase 2 着手時）

分類と対応先モジュールは「維持する」対象であり、行数と違って追随義務がある。
今回の再測では**両方向**の訂正が出た。

**(a) 完成した部品が 🔀 のまま残っていた 3 行 → ✅**

| 節 | qpdf | 訂正前 | 訂正後 | 由来 |
|---|---|---|---|---|
| §1 | `QPDFXRefEntry.cc` | 🔀 `xref.rs` に埋没 | ✅ `xref_entry.rs` | `qxba.9.2` |
| §4 | `QPDF_optimization.cc` | 🔀 `plan.rs` に埋没 | ✅ `optimization.rs` | `qxba.9.3` / `.9.4` |
| §10 | `BitStream.cc` / `BitWriter.cc` | 🔀 `hint_stream.rs` に埋没 | ✅ `bit_stream.rs` / `bit_writer.rs` | `qxba.9.1` |

**(b) 未完成の部品が ✅ になっていた 2 行 → 🔀**

いずれもモジュール doc 自身が未完成を申告しており、責務境界が一致しているとは
言えない。D4 索引でも `correspondence` に分類されている。

| 節 | qpdf | flpdf | doc の自己申告 |
|---|---|---|---|
| §7 | `QPDFFileSpecObjectHelper` / `QPDFEFStreamObjectHelper` | `filespec_helper.rs` | 「partial helper surface; 公開 API は未完成」 |
| §7 | `QPDFEmbeddedFileDocumentHelper.cc` | `embedded_files.rs` | 「完全な公開ヘルパー境界を持たない」 |

**(c) 責務の帰属誤り（§8 / §9）**

`json_inspect.rs` が持つ `build_*_section` 群は `QPDF_json.cc` ではなく
`QPDFJob.cc` の `doJSON*` 族に対応する（詳細は §9 の内訳表）。この誤りは
PR #613/#614 で実害を出しており、地図が誤ったままだと後続スライスで同じ
実装ミスを再生産する。あわせて:

- `QPDF_json.cc` を入力側(1-833) / 出力側(834-946) の 2 行に分割した。1 行に 🔀 と ❌ を
  混在させると集計できず、**未実装の JSON 入力が ❌ の work-list から消える**ため。
  出力側の先頭は `QPDF::` 接頭辞を持たない free function `writeJSONStreamFile`(834-849)
  であり、`QPDF::writeJSON` から呼ばれる side-file 書き出し（flpdf 側は
  `json_inspect.rs::write_file_mode_side_file`）。接頭辞で検索すると取りこぼす
- `doJSONObjects` は v1 分岐（自前で組み立て、flpdf に対応物なし）と v2 分岐
  （`QPDF::writeJSON` へ委譲するだけ）を分けた。まとめると二重帰属になり
  v1 の欠落が隠れる
- `optimization/inherited_attrs.rs` のパス誤記（`linearization/` としていた）を
  §2 / §4 の両方で訂正した

**本表の ✅ と D4 の `Mirrors` は別の述語である（混同しないこと）**:

- 本表の ✅ = 「**対応が明確で責務境界も一致**」（凡例）。実装の完成度は含意しない
- D4 索引の `Mirrors` = `//! Mirrors qpdf 11.9.0 libqpdf/X.cc`。これは DoD の
  D4 であり、`Mirrors` を名乗るには **D1〜D5 すべて**（全域移植 / 単一実装 /
  アドホック分岐ゼロ / 対応行 / ゲート通過）を満たす必要がある

したがって「本表で ✅ なら `Mirrors` にできる」は成り立たない。
現状は **mirror 5 / correspondence 146**（2026-08-16 再測。`content_normalizer` /
`matrix` / `pdf_version` / `encryption/rc4` / `tokenizer` のみが `Mirrors`）。
本表で ✅ の `nntree.rs` / `json/` / `xref_entry.rs` / `bit_stream.rs` /
`bit_writer.rs` / `optimization.rs` / `pipeline.rs` / `embedded_files.rs` /
`filespec_helper.rs` が `correspondence` のままである
のは、責務境界が一致していても DoD 全体を検証していないためであり、
必ずしも矛盾ではない。**昇格は各部品の担当スライスで D1〜D5 を検証したうえで
行うこと**（D1 だけでは足りない）。

逆に、モジュール doc 自身が「公開 API 未完成」等を申告している場合は
責務境界が一致しているとも言えないため、本表でも ✅ にしてはならない。
2026-08-02 の再測ではこの観点で §7 の 3 行を ✅ から 🔀 に訂正した
（`filespec_helper.rs` / `embedded_files.rs` / `page_label_document_helper.rs`）。

### 2026-08-16 の再測

2026-08-14〜08-16 にかけて `flpdf-egzr.3.2`（ObjectHandle consumer cutover）・
`flpdf-3yn9`（Tier A〜D ヘルパー境界確定）系列で 70 件以上の issue が close
され、本表が「pending」として記述していた箇所の多くが完了事実になっていた。
bd (`bd show`) と実コードを突き合わせて次を訂正した。

**(a) モジュール doc の自己申告が解消され ✅ に訂正した 2 行**

| 節 | qpdf | 訂正前 | 訂正後 | 由来 |
|---|---|---|---|---|
| §7 | `QPDFEmbeddedFileDocumentHelper.cc` | 🔀 D1 未達 | ✅ D1 完成（D2 は `flpdf-q2fo` まで未達のまま） | `flpdf-jzy7`。`embedded_files.rs` 冒頭の自己申告文言も併せて訂正 |
| §7 | `QPDFFileSpecObjectHelper`/`QPDFEFStreamObjectHelper` | 🔀 D1 未達 | ✅ D1 完成（D2 は同上） | `flpdf-d9sq`。`filespec_helper.rs` は既に自己申告のヘッジが無かった |

これに連動して §9 の `doJSONAttachments`/`doJSONPageLabels` 内訳表（旧 🔀）も
✅ に揃えた。`doJSONPageLabels` は §7 側が既に ✅ だったにもかかわらず表が
追随していなかった既存の drift でもある。

**(b) 「pending」記述を完了事実に訂正した箇所（状態記号は変更なし）**

以下は closed issue を裏付けに文言だけを過去形化した。記号は元々 🔀 の
根拠が ObjectHandle 移行の未完了ではなく責務境界の smear そのものだったため、
維持している（下記 (c) 参照）。

- §1 `QPDFObjectHandle.cc` / `QPDFObject.cc`・`QPDFValue.cc`: `flpdf-egzr.3.1`
  （reader cutover）・`flpdf-mfir` の close を反映。旧 raw `Object` route の
  最終削除は `flpdf-egzr.3.2.8`（open）
- §2 `QPDF.cc`（reader.rs/xref.rs 行）: `flpdf-egzr.3.2.10` + 子
  `.3.2.10.1`/`.3.2.10.2`（PR #859 merged）で reader.rs/xref.rs 自身の filter
  呼び出しが production では ObjectHandle 経由のみになったことを確認し、
  「別 issue」「行数は暫定値」という古い注記を除去
- §3 `QPDFWriter.cc`: `flpdf-egzr.3.2.5`（+ 子 4件）・`flpdf-3yn9.11`/`.12`
  の close を反映。`flpdf-egzr.3.2.15` セクション（暗号化 emission surface）
  も「後続 cutover が使用する（予定）」を「使用している（実績）」に訂正
- §6 `TokenFilter` 行の `flpdf-vkka` 注記: 「ゲート未配線」を、close 済みの
  検証結果（`plain/body.rs` は対応済み、`emit_canonical_pdf_inner` 側は
  PR #831 後に該当分岐が構造的に到達不能）に置き換え

**(c) §4 `QPDF_linearization.cc` は ✅ 化を検討し見送った**

producer（`flpdf-3yn9.4`）・consumer（`flpdf-egzr.3.2.9`）は close 済みで、
production 経路から `Object::`/`resolve_borrowed` が消えたことをテストが
機械的に保証している。しかし ✅ の判定基準は「ObjectHandle 移行の完了」では
なく「責務境界の一致」であり、`linearization/` は依然 `plan.rs`/`hint_*`/
`check.rs`/`show.rs` など 5+ モジュールに分散したまま — `optimization.rs` が
2026-08-02 に ✅ を得た決め手（単一モジュールへの完全集約、旧所在地の空化）
を満たしていない。また `flpdf-3yn9.5`（線形化書き込み経路）は issue タイトル
自身が「§3 `QPDFWriter.cc` スライス」と宣言しており、§4 の根拠に使うのは
帰属を誤る。記号は 🔀 のまま維持し、行内の説明のみ更新した。

**(d) 検証可能性テーブルの stale 記述を訂正**

- 「null 可視性」行の `cmp_null_visibility_tests` ⚠ CI 未列挙は stale —
  `flpdf-qxba.2` で解消済みで `.github/workflows/ci.yml` に実際に列挙されて
  いることを確認し ✅ に訂正
- 「暗号化出力」行の CLI 列 ❌ は不正確 — `encrypt_cli_tests` に
  `qpdf-zlib-compat` 関数レベル gate の byte-identical テストが 2 件存在し
  CI で実行されていることを確認し 🟡 に訂正（library 側は引き続き gate 無し）

**(e) mirror / correspondence カウントを再測**

`scripts/qpdf-module-docs.py --check` は同期済み（exit 0）だったが、本表
冒頭の「correspondence 129」が古く、実測は 146（mirror は 5 のまま変化なし）。
Phase 2 進行に伴う新規モジュール分割（`job/`, `document_json.rs`,
`optimization/inherited_attrs.rs` 等）が主因。

**機械可読なモジュール索引:** [`qpdf-module-doc-index.md`](qpdf-module-doc-index.md) は
各 source module 先頭の対応行から生成する。この索引は注釈の欠落と drift を検査する
ためのものであり、本書の責務分類・状態・実装判断を置き換えない。

規模比較: qpdf `libqpdf/*.cc` = 41,459 行 / flpdf 実装部 = **68,504 行**

### 行数の位置づけ — スナップショットであり維持対象ではない

**本表の行数は調査時点のスナップショットで、正確さを維持しない。** コードが変われば
即座にずれる性質のもので、追随コストに見合う価値がない。

行数の役割は **相対的な規模感の判断**に限る。「`QPDFWriter.cc` 相当が 10 ファイル以上に
分散している」「smeared が全体の 7 割を占める」といった判断ができれば足り、
個々の値が最新かどうかは問わない。

したがって:

- 行数のずれ自体は**不具合ではない**。指摘されても再計測の義務を負わない
- ただし**分類（✅ / 🔀 / ❌ / ⚪ / ➖）と対応先モジュールは維持する**。これらは
  work-list の実体であり、誤ると着手判断を誤らせる
- 数値を更新する場合は「計測方法」（下記）に従い、集計との整合も同時に取る

**行数の計測方法**: 末尾の `#[cfg(test)] mod tests` より前を production とする。
「最初の `#[cfg(test)]` まで」で数えてはならない。Rust は item 単位の `#[cfg(test)]`
ヘルパーの**後に production コードが再開する**ため、大幅な過小評価になる
（例: `linearization/writer.rs` は 448 行目に test helper があり、terminal
`mod tests` は 3604 行目。誤った方法では 447 行、正しくは 3,603 行で 8.1 倍の差）。
なお item 単位の test helper（各数行）は本計測に含まれたままである。

| 記号 | 意味 |
|---|---|
| ✅ | **境界一致** — 対応が明確で責務境界も一致。**実装の完成度は含意しない**（DoD D1〜D5 の充足は別途検証が必要。D4 索引の `Mirrors` とは別の述語 — 下記参照） |
| 🔀 | **smeared** — 実装はあるが複数モジュールに散在、または別モジュールに埋没 |
| ❌ | **missing** — flpdf に対応物が無い |
| ⚪ | **逸脱候補** — Rust/エコシステムで代替済み。移植しない提案（要承認） |
| ➖ | **対象外** — C API 等 |

---

### Classic xref trailer validation (2026-09-02)

qpdf's `QPDF::read_xrefTable` validates the first classic trailer's visible
`/Size` and every classic section's `/Prev` before the xref load completes
(`libqpdf/QPDF.cc:846-945`). The `readTrailer` source position is restored
before qpdf constructs the `QPDFExc` (`QPDF.cc:1313-1327`), so
`xref.rs::validate_classic_trailer` and its `classic_trailer_offset` preserve
the same byte location. Strict `repair=false` opens propagate the detail to
the qtest driver's `QPDFExc::createWhat` boundary; repair-enabled opens retain
the qpdf warning sequence before reconstruction. `error-condition 9-11` is the
consumer coverage for these three paths.

### qtest metadata consumers (2026-08-12)

`crates/flpdf-qtest-tools/src/metadata.rs` ports the pinned qpdf 11.9.0
`qpdf/test_xref.cc:7-44` and `qpdf/test_parsedoffset.cc:13-140` helpers as
thin consumers of `Pdf::get_xref_table`, `Pdf::get_all_objects`, and
`ObjectHandle::get_parsed_offset`. It deliberately owns only grouping,
sorting, formatting, and qpdf-shaped diagnostics; parsing, xref construction,
resolution, and provenance stay in `flpdf`.

The initial file-open boundary also preserves the platform CRT diagnostic via
`driver::crt_open_error_message`, matching qpdf's `QUtil::safe_fopen`
(`libqpdf/QUtil.cc:453-518`) and `QPDFSystemError::createWhat`
(`libqpdf/QPDFSystemError.cc:13-29`). The differential tests cover missing
paths and Windows directory-open failures for both metadata helpers.

`flpdf-enoa` は `QPDF::resolve` の persistent `obj_cache` gate
(`libqpdf/QPDF.cc:1700-1704`) を qtest の parsed-offset warning attribution にも
適用する。`Pdf::qtest_object_value_source_offsets` と
`qtest_array_item_source_offsets` は同一 ObjectBody/array container を一度だけ
bounded-read/retry し、`test_0_1` は DecodeParms warning を source ref ごとに batch
してから元の warning 順序で出力する。qpdf に存在しない flpdf の
`resolution_fallbacks_remaining` を filter index ごとに消費する再読は増やさず、
既存の qtest-only offset boundary に閉じ込める。

### qtest renumber consumer (2026-08-31)

`qpdf/test_renumber.cc:14-22,24-117,119-166,168-259` is ported by
`crates/flpdf-qtest-tools/src/renumber.rs` and
`src/bin/test_renumber.rs`. It uses `Pdf::get_all_objects`, the public
`ObjectHandle` value/type accessors, `PdfWriter`'s memory output and renumbered
object/xref result APIs, then reloads through `Pdf::open_mem_owned`. The
recursive comparison deliberately skips stream payloads and preserves qpdf's
upstream xref self-comparisons at `test_renumber.cc:147,153-154`.

The signed linearization differential also fixed the canonical writer boundary:
`linearization/renumber.rs` interleaves Preserve source containers with plain
open-document objects, while `linearization/writer.rs` suppresses a preserved
member's duplicate plain emission and reports each source container's logical
output identity. This mirrors `QPDFWriter::preserveObjectStreams` and
`enqueueObject` (`QPDFWriter.cc:1072-1125,1939-1966`) and is covered by the
eight-case qpdf 11.9.0 helper differential.

## 1. オブジェクトモデル

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFObjectHandle::makeResourcesIndirect` | `include/qpdf/QPDFObjectHandle.hh:789-793`; `libqpdf/QPDFObjectHandle.cc:1042-1060` | `object_handle.rs::make_resources_indirect` + `acroform_document_helper.rs::prepare_foreign_resource_plan` | ✅ direct second-level resource values are promoted in place through the canonical resolver before `mergeResources`; category dictionaries are not promoted and the walk is non-recursive. Tests cover direct/indirect categories, already-indirect values, non-dictionary top-level entries, alias identity, and the foreign AcroForm caller |
| `QPDFObjectHandle::makeDirect` | `libqpdf/QPDFObjectHandle.cc:2091-2133,2154-2157` | `object_handle.rs::make_direct`; `reader.rs::make_indirect_from_object_handle`; `qtest-tools::driver::run_test_4` | ✅ the receiver is rebound to a recursively direct copy, each repeated indirect occurrence is copied independently, a per-call identity set reports qpdf's loop text, `allow_streams=true` retains streams, and qpdf's shared-allocation promotion is used for the `/Info` copy. Focused mutation tests and all five `mutability.test` cases cover the success and failure paths |
| `QPDFObjectHandle.cc` | 2601 | `object.rs`(1301) + `object_handle.rs`(shared handle identity・parsed offset・遅延解決・live direct containment・`QPDF::newReserved`/`QPDF_Reserved`・`copyStream`/`StreamDataProvider` source dispatch) + `object_handle.rs::try_is_null` / `reader.rs::resolve`（`isNull` の canonical 間接参照解決） + `writer/rewrite_renumber.rs::visible_raw_dict_entries`（raw `Object` 境界の writer dict 可視性） + `acroform_document_helper.rs`（`DrMap` = qpdf `dr_map`） + `overlay_appearance_stream.rs`（段階的 conflict merge の再現） | 🔀 アクセサは `object.rs` / `object_handle.rs` に分割されたまま（旧 raw `Object` route の削除・統合は `flpdf-egzr.3.2.8` が担当、open。`flpdf-mfir` はその削除対象へのリファクタなので着手せず close 済み）。object identity / 遅延解決は `object_handle.rs` の production route への移行が完了済み（`flpdf-egzr.3.1`、2026-08-09 close）。qpdf の array/dictionary/stream が保持する現在の forward child を正本とし、incremental dirty lookup 用に各 forward edge と一対一の immediate weak reverse edge を派生記録する。削除・置換後の旧 child は旧 root を返さない。`try_get_keys` は `QPDFObjectHandle::getKeys` → `QPDF_Dictionary::getKeys`（`QPDFObjectHandle.cc:997-1009`; `QPDF_Dictionary.cc:117-127`）に対応し、holder と全 child を lazy resolve して null value のキーを除外した `BTreeSet` を返す。child resolve 前に辞書 snapshot の borrow は終了し、resolver error は伝播する。`stream_filter.rs` の consuming stage は retained-key reduction 前に `try_get_keys` を使用する。`shallow_copy` は `QPDFObjectHandle::shallowCopy`（`QPDFObjectHandle.cc:2072-2079`）に対応し、stream は `QPDF_Stream::copy`（`QPDF_Stream.cc:140-145`）が `shallow` 引数を無視して無条件に `std::runtime_error` を投げるのに合わせて `Error::System` で拒否する。`QPDF_Dictionary::copy`/`QPDF_Array::copy` が direct な子に `shallowCopy` を掛けるため、コンテナに入れ子の direct stream も同じ拒否に到達する。qpdf の `QPDFObjectHandle::copyStream`（`QPDFObjectHandle.cc:2136-2151`）と `QPDF::copyStreamData`（`QPDF.cc:2216-2272`）を `ObjectHandle::copy_stream` と resolver-owned stream-copy boundary として実装済み（`flpdf-a8mk`）。Buffer は `Rc<Vec<u8>>` 共有、provider-backed source は source handle を保持する retry-aware provider、original-file source は qpdf の `ForeignStreamData` 相当として source の `StreamInput`/encryption state/object number/parsed offset/length と destination dictionary を copy 時に凍結し、destination resolver を warning sink として遅延 dispatch する。source `Pdf` 解放後も入力と暗号状態だけで読み続け、source 側へ警告を戻さない。`set_immediate_copy_from` は qpdf の source-side `setImmediateCopyFrom` に対応する。`QPDFObjectHandle::isReserved`/`QPDF::newReserved` は `ObjectValue::Reserved` と `Pdf::new_reserved` に対応し、`ot_reserved` は null/missing/destroyed と区別して、materialize と全 ObjectHandle writer entrypoint で `QPDFObjectHandle: attempting to unparse a reserved object` を返す。 |
| `QPDFObjectHandle::StreamDataProvider` / `QPDF_Stream` | `QPDFObjectHandle.hh:68-127`; `QPDFObjectHandle.cc:48-90,1365-1428`; `QPDF_Stream.cc:571-620,640-660` | `object_handle.rs` の `StreamDataProvider`、`ObjectValue::Stream.stream_provider`、`replace_stream_data_provider`、callback adapter、`pipe_stream_source` | ✅ qpdf の provider ownership、通常/retry family の選択、identity forwarding、遅延・反復 invocation、`Pl_Count` による encoded-byte length 検証、buffer/provider の排他を canonical route で保持する。qpdf の `std::shared_ptr` container は `Rc<dyn StreamDataProvider>` に置換するが、これは内部所有表現だけの差であり、callback/error/finish/`/Length` の観測契約は変えない。登録 API は stable `ObjectRef` を必要とするため indirect stream に限定し、direct stream は登録時に `Error::System` で拒否する。既存 document-owned stream の provider/dictionary 置換は live graph mutation なので、writer 前に `Pdf::mark_object_handle_dirty` を要求する | ✅ |
| `QPDFObjectHandle::setFilterOnWrite` / `getFilterOnWrite` | `include/qpdf/QPDFObjectHandle.hh:972-982`; `libqpdf/QPDFObjectHandle.cc:1265-1273`; `libqpdf/QPDF_Stream.cc:114-118,154-164` | `object_handle.rs::ObjectHandle::set_filter_on_write` / `get_filter_on_write`; `ObjectValue::Stream.filter_on_write`; `writer/plain/body.rs::canonical_stream_filter_plan` | ✅ qpdf の stream-local default `true` と shared handle state を保持し、`false` は `QPDFWriter::willFilterStream` の metadata/normalize/compress/retry 分岐より先に全 filtering を抑止する。raw payload と既存 filter metadata の出力は既存の unfiltered writer route に委譲し、cache fingerprint は state mutation で無効化する。`filter-on-write` qtest の test_70 と Rust API/writer regression tests がこの契約を検証する |
| `QPDF_Stream::registerStreamFilter` / `QPDF_Stream::filterable` | `QPDF_Stream.cc:33-50,72-94,148-152,378-485` | `stream_filter.rs` の `stream_filter_for` / `FilterSpec` / `DecodeParams` / `decode_filter_specs_from_object` / `decode_filter_specs_from_handle` と `filters.rs` の `decode_prepared_specs` | 🔀 qpdf の filter factory は `filter_factories` の `std::map` を名前引きに使い、`registerStreamFilter` で runtime 登録できる。flpdf は `match` registry で登録 API がなく、登録集合も qpdf と同一ではない。`QPDF_Stream::filterable` の `/Filter` の factory lookup、`/DecodeParms` の shape・長さ検証、`setDecodeParms` の順序を2つの shape readerで保ち、下流の codec/predictor・limit・warning order は `decode_prepared_specs` で共有する。qpdf が filter chain に複製する `shared_ptr` の `/DecodeParms` に対し、flpdf の `DecodeParams` は consumer が読むキーだけを持つ所有 snapshot（`Crypt` は全キー）だが、writer の出力 bytes と error timing は変えない | 🔀 |
| `QPDFObjectHandle::isNameAndEquals` / `isDictionaryOfType` / `isStreamOfType` / `getArrayNItems` / `getArrayItem` / `isOrHasName`（行数は上段に計上済み） | `QPDFObjectHandle.hh:366-374` | `object_handle.rs` の `try_is_name_and_equals` / `try_is_dictionary_of_type` / `try_is_stream_of_type` / `try_array_len` / `try_array_item` / `try_is_or_has_name`（`QPDFObjectHandle.cc:456-471,759-785,1027-1039`） | ✅ holder と child を qpdf 順に lazy resolve。`isStreamOfType` は qpdf の `isStream() && getDict().isDictionaryOfType(...)` をそのまま stream 内辞書へ委譲し、container borrow は resolver 再入前に解放する。配列全体を snapshot せず、`try_array_item` は valid-index 面のみを契約に含める |
| `QPDFObjectHandle::typeWarning` / `warnIfPossible` / `objectWarning` / `warn` / `getIntValue` / `getIntValueAsInt`（行数は上段に計上済み） | — | `object_handle.rs` の `type_warning` / `warn_if_possible` / `object_warning` / `warn_through_context` / `context` と `DocumentResolver::warn`、`try_get_int_value` / `try_get_int_value_as_int`、`reader/resolver.rs` の `push_object_warning`（`QPDFObjectHandle.cc:502-543,2168-2212,2385-2396`; `QPDF.cc:487-494`） | 🔀 メッセージ文言は qpdf と完全一致。live parser が生成した direct value と canonical indirect handle は `HandleResolver::direct_handle` / `ChildHandles` から同じ weak document context と、qpdf の `QPDFParser` と同じ parse-call description template を持つ。非 null の top-level・array・dictionary・scalar は `input-description, object N G at offset $PO` を共有し、`QPDFValue` と同じ container offset shift を経て `DocumentResolver::warn` → `push_object_warning` で `Pdf::repair_diagnostics` と同じ収集先へ同順に届く。parsed null は qpdf と同じく description を持たない。literal null は containment parent の context を借りず、qpdf の `QPDF_Null::create` に対応する contextless 分岐をネスト後も維持する一方、missing-key null は `setChildDescription` に対応する Child description 経由で親の context を保持する（`QPDF_Null.cc:12-15`; `QPDFParser.cc:397-410`; `QPDFObject_private.hh:79-91`）。明示的 parse と programmatic direct は qpdf の contextless 分岐を維持する。no-context 分岐は qpdf のまま 2 通り — `typeWarning`/`objectWarning` は `throw QPDFExc`（`std::runtime_error` 派生、`QPDFExc.hh:29`）に対応する `Error::System`、`warnIfPossible` は `QPDFLogger::defaultLogger()->getError()` へ素の文言を書いて正常復帰する。`getKey`/`getKeys` の `typeWarning` は `try_get_key`/`try_get_keys` に実装済み。live parser の direct value は weak document context を持ち、stream_filter の consuming `/DecodeParms` 読み出しで qpdf と同じ回復可能な警告を `DocumentResolver::warn` へ送る。contextless の programmatic direct は qpdf と同じく `Error::System` 相当の throw を維持する。`asDictionary`/`asInteger` に対応する `try_as_dictionary`/`try_as_integer` は qpdf 同様 warning を出さない |
| `QPDFObjectHandle` type-check accessors, array/dictionary bounds, geometry, and iterators | `QPDFObjectHandle.hh:239-267,597-637,666-734`; `QPDFObjectHandle.cc:332-453,474-740,759-853,856-1023`; `qpdf/test_driver.cc:1407-1549` | `ObjectHandle::try_get_bool_value` / `try_get_int_value` / `try_get_real_value` / `try_get_numeric_value` / `try_get_name` / `try_get_string_value` / `try_get_utf8_value` / `try_get_operator_value` / `try_get_inline_image_value`; `try_get_array_n_items` / `try_get_array_item` / `try_get_array_as_vector`; signed-index array mutators; `try_get_key_if_dict` / `try_get_dict_as_map`; `try_is_rectangle` / `try_get_array_as_rectangle` / `try_is_matrix` / `try_get_array_as_matrix`; `ArrayItems` / `DictItems` | ✅ warning-producing accessors dereference before type inspection and return qpdf's zero-like fallbacks; invalid array positions use object warnings and contextual nulls; geometry checks use silent number predicates and qpdf's zero defaults; cursors retain canonical child handles and use explicit uninitialized end values. `flpdf-qtest-tools::driver::run_test_42` drains the same `Pdf::repair_diagnostics` sequence, and the six `type-checks.test` cases pass against the pinned qpdf expected output |
| `QPDFObjectHandle::newFromMatrix(QPDFMatrix)` overload | `include/qpdf/QPDFObjectHandle.hh:254-285`; `libqpdf/QPDFObjectHandle.cc:1987-2002` | `ObjectHandle::new_from_qpdf_matrix` using [`crate::Matrix`] | ✅ the standalone identity-default matrix type is kept separate from the nested all-zero `ObjectHandleMatrix`, while both constructor overloads produce the same six-number array shape |
| `QPDFObjectHandle::getTypeCode` / `getTypeName` | `include/qpdf/QPDFObjectHandle.hh:311-316`; `libqpdf/QPDFObjectHandle.cc:240-250`; `include/qpdf/Constants.h:108-128` | `object_handle.rs::ObjectValue::type_code` / `type_name` と `ObjectHandle::type_code` / `type_name`（`object_handle.rs:1025-1068,5708-5758`） | ✅ qpdfの `qpdf_object_type_e` ordinalをRust enumのdiscriminantから独立した明示的matchで保持する。handle側は `try_dereference` 後にvalue-layer code/nameを読む。`ObjectValue::Reference` はflpdfの既存 `set_object` redirect専用状態で、qpdfのvalue familyには対応物がないため後続のraw-route削除で消す |
| `QPDFObjectHandle::unparse` / `unparseResolved` と `QPDF_Array::unparse` / `QPDF_Dictionary::unparse` / `QPDF_Stream::unparse` | `libqpdf/QPDFObjectHandle.cc:1574-1593`; `libqpdf/QPDF_Array.cc:122-149`; `libqpdf/QPDF_Dictionary.cc:58-68`; `libqpdf/QPDF_Stream.cc:173-178` | `object_handle.rs::unparse` / `unparse_resolved` / `try_unparse_resolved` と `unparse_resolved_into`（`object_handle.rs:5900-5980,6120-6270`） | ✅ value/child handleを直接たどり、receiverとarray/dictionary childをqpdfの順序で解決する。辞書のnull値は省略し、配列のnull要素は保持し、間接childは参照形のまま出力する。`QPDF_Stream::unparse`に合わせ、間接streamは自身の参照形を返す。`unparse_resolved`の非fallible null fallbackと`try_unparse_resolved`のlogic-error境界を分離し、`unparse_materialize*`およびraw `Object` treeはこの経路から除去した。最終的なraw `Object`/materialize削除は`flpdf-25kg.3.48.6`の責務 |
| `QPDFObjectHandle::getTypeCode` / `getTypeName` | `include/qpdf/QPDFObjectHandle.hh:311-316`; `libqpdf/QPDFObjectHandle.cc:240-250`; `include/qpdf/Constants.h:108-128` | `object_handle.rs::ObjectValue::type_code` / `type_name` と `ObjectHandle::type_code` / `type_name`（`object_handle.rs:1025-1068,5708-5758`） | ✅ qpdfの `qpdf_object_type_e` ordinalをRust enumのdiscriminantから独立した明示的matchで保持する。handle側は `try_dereference` 後にvalue-layer code/nameを読む。`ObjectValue::Reference` はflpdfの既存 `set_object` redirect専用状態で、qpdfのvalue familyには対応物がないため後続のraw-route削除で消す |
| `QPDFObjectHandle::unparse` / `unparseResolved` と `QPDF_Array::unparse` / `QPDF_Dictionary::unparse` / `QPDF_Stream::unparse` | `libqpdf/QPDFObjectHandle.cc:1574-1593`; `libqpdf/QPDF_Array.cc:122-149`; `libqpdf/QPDF_Dictionary.cc:58-68`; `libqpdf/QPDF_Stream.cc:173-178` | `object_handle.rs::unparse` / `unparse_resolved` / `try_unparse_resolved` と `unparse_resolved_into`（`object_handle.rs:5900-5980,6120-6270`） | ✅ value/child handleを直接たどり、receiverとarray/dictionary childをqpdfの順序で解決する。辞書のnull値は省略し、配列のnull要素は保持し、間接childは参照形のまま出力する。`QPDF_Stream::unparse`に合わせ、間接streamは自身の参照形を返す。`unparse_resolved`の非fallible null fallbackと`try_unparse_resolved`のlogic-error境界を分離し、`unparse_materialize*`およびraw `Object` treeはこの経路から除去した。最終的なraw `Object`/materialize削除は`flpdf-25kg.3.48.6`の責務 |
| `QPDF_Array/Dictionary/Stream/String/Name/Real/Integer/Bool/Null/InlineImage/Operator/Reserved/Unresolved/Destroyed.cc` | 1814 | `object.rs` の `Object` enum に統合 | 🔀 |
| `QPDFObject.cc` / `QPDFValue.cc` | 79 | `object.rs` の `Object` + `object_handle.rs` の `ObjectHandle` / `ObjectValue`（共有 identity・qpdf 互換 parsed offset・`ObjectValue` に統合された unresolved/reserved/destroyed value・Pdf identity provenance） | 🔀 `object.rs` の `Object` は静的な値表現のみ。`QPDFValue` 相当の共有 identity・parsed offset・遅延解決状態は `object_handle.rs` が担う。`flpdf-25kg.10`/`.11`/`.13` で qpdf の value-layer state を `ObjectValue` に統合し、`ObjectState` の冗長 wrapper を削除した。Pdf identity provenance は live containment から分離して detach 後も保持する。旧 raw `Object` route の最終削除は `flpdf-egzr.3.2.8`（open）待ち。 |
| `QPDFObjGen.cc` | 68 | `object.rs` の `ObjectRef` | ✅ |
| `QPDFXRefEntry.cc` | 51 | `xref_entry.rs`（`XrefEntry` = free / uncompressed / compressed の 3 variant）。consumer は `xref.rs` / `reader.rs` / `cache.rs` / `writer.rs` / `writer/{object_streams,plain/plan}.rs` / `linearization/{writer,plan}.rs` | ✅ `flpdf-qxba.9.2` で完全 cutover（`XrefOffset` 削除）。`xref.rs` 側に型定義は残っていない |
| `PDFVersion.cc` | 68 | `pdf_version.rs` の `PdfVersion` | ✅ |
| `QPDFMatrix.cc` | 140 | `matrix.rs` の `Matrix` / `Rectangle` | ✅ |
| `QPDFObjectHandle::mergeResources` / `shallowCopy` | `QPDFObjectHandle.cc:431-434,1063-1153,2072-2079` | `object_handle.rs:5070` + `page_annotation_flatten.rs:666-740`（widget appearance の既定リソース consumer） | ✅ live `ObjectHandle::merge_resources` を使用し、receiver・other・各top-level resource categoryをqpdfの`isDictionary`/`isArray`相当で自己解決してから分岐する。missing category は top-level が direct の shallow copy になり、nested indirect child は handle を保持する。array の `isScalar` 判定と unique-name pool の second-level dictionary 判定は qpdf と同じく各 nested handle を解決し、解決エラーを伝播する。`acroform_document_helper.rs` の `DrMap` と `overlay_appearance_stream.rs` が name-conflict overlay merge を担う |
| `QPDFObjectHandle::getResourceNames` | `QPDFObjectHandle.hh:831-835`; `QPDFObjectHandle.cc:1156-1170` | `object_handle.rs::ObjectHandle::get_resource_names` + `try_get_resource_names` | ✅ second-level keys from every dictionary-valued resource category are collected through the canonical handle resolver; the public facade is available to `flpdf-qtest-tools`, and resolver failures remain a `Result` at the Rust boundary. |

`qpdf/test_driver.cc:2139-2213` の test 60 は、`ObjectHandle::make_resources_indirect`、
`merge_resources`、`get_unique_resource_name` とlive `Pdf::trailer`を通るqtest consumerとして
実装済みである。4回のconflict merge結果とQDF/static-ID `a.pdf`をpinned qpdf 11.9.0の
`test60.out` / `unique-resources.pdf`と比較し、対応する `merge-dictionary 2,3` の
同一run結果を `harness.log` と `qtest-results.xml` で確認する。driver独自のresource
traversal・allocation・trailer snapshotは追加していない。

`flpdf-tcfj` では、qpdf 11.9.0 の `QPDF::resolve` が `isUnresolved` を確認して永続 `m->obj_cache` を一度だけ更新する責務（`QPDF.cc:1700-1753`）に合わせ、xref bootstrap の raw object view と handle-native view を `SharedBootstrapCache` の同一状態へ束ねる。`BootstrapHandleDocument`、再帰ガード、ObjStm の解決済み集合、診断、reconstruction trigger は xref-loading operation 全体で共有し、`read_uncompressed_object` の `FileObjectDiagnostic` は一度だけ転送する。raw view が先に materialize した値は handle slot へ seed し、handle view が先に解決した値は raw lookup から再利用するため、同じ bootstrap object の再パースと警告の二重出力を避ける。

`flpdf-uwn0` では qpdf の `makeIndirectFromQPDFObject` (`QPDF.cc:1882-1894`) / `replaceObject` (`QPDF.cc:1986-1993`) が source xref と別に `m->obj_cache` へ登録する allocation と、参照解決で同じ cache に入る dangling null を object-ref view で区別する。qpdf の `getAllObjects` は `fixDanglingReferences` 後の `m->obj_cache` 全体 (`QPDF.cc:1258-1294`) を列挙し、live probe でも `newIndirectNull()` は列挙される。flpdf の `ResolverCore::allocated_object_refs` はこの provenance だけを canonical allocation 境界で記録し、`object_refs()` / `live_object_refs()` が allocated indirect null を落とさないようにする。legacy cache/memo の互換 bridge や qpdf-deviation marker は追加しない。

`flpdf-25kg.2.5.12` では、qpdf の `makeIndirectObject` が `nextObjGen` → `getObjectCount` → `fixDanglingReferences` の順で新規番号を決める契約 (`QPDF.cc:1239-1294,1872-1901`) を `Pdf::make_indirect_object_handle` の allocation boundary に適用した。repairで再構築されるobjectがある場合も、canonical resolverを先に準備してから既存の番号走査を行うため、recovered objectとの番号衝突を起こさない。

`flpdf-25kg.2.5.11` では qpdf `test_driver.cc:2025-2041` の test 53を、`Pdf::make_indirect_object_handle`、live Root handle mutation、`Pdf::get_all_objects`、および `PdfWriter::set_preserve_unreferenced_objects` のcanonical経路へ接続した。repair中に発生するdiagnosticsは `emit_new_diagnostics` で最初のobject出力前にflushし、qpdfのobject enumerationとwriter比較を同じdriver consumerで検証する。

### `test_driver` test 62 の整数幅アクセサ

qpdf `test_driver.cc:2262-2287` の `getUIntValue` / `getIntValueAsInt` /
`getUIntValueAsUInt` は、`ObjectHandle::try_get_uint_value`、
`ObjectHandle::try_get_int_value_as_int`、
`ObjectHandle::try_get_uint_value_as_uint` に対応する。負値の 0 への変換、
`INT_MIN` / `INT_MAX` / `UINT_MAX` への飽和、および qpdf と同じ警告文は
`object_handle.rs` の canonical accessor が担う。`flpdf-test-driver` は値の
検査をこの API に委譲し、qtest の stderr 境界だけを qpdf の生の警告行へ戻す。
Rust unit test と qtest の `error-condition 45`
（`test_driver 62 minimal.pdf`）が、qpdf 11.9.0 の値・警告順・出力を固定する。

## 2. パース / 読み取り

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF.cc` | 2667 | `engine.rs`(475: `Pdf::empty`、ほか8つの public factory — `Pdf::open` / `open_with_repair` / `open_best_effort` / `open_with_options` / `open_mem` / `open_mem_with_options` / `open_mem_owned` / `open_mem_owned_with_options` —、`open_with_repair_mode`、`NEXT_PDF_ID`、`MAX_RESOLUTION_FALLBACKS`。`emptyPDF` / `processFile` / `processMemoryFile` の construction path) + `pdf.rs`(297: `Pdf<R>` container、`Drop` = `QPDF::~QPDF`、version/trailer/root/extension/page-enumeration-state accessors。`QPDF.hh:1438-1518`; `QPDF.cc:215-232,2323-2358,2647-2651`) + `reader.rs`(8185: object resolution, recovery, diagnostics, authentication, and `Pdf::get_xref_table` / `Pdf::get_all_objects`) + `reader/resolver.rs`(2367: canonical resolver。`QPDF::resolve` が触る `QPDF::Members` — `m->file` / `m->xref_table` / `m->obj_cache` / `m->resolving` / `m->resolved_object_streams` / `m->attempt_recovery` / `m->encp` — を `ResolverCore` に集約し、`Rc<RefCell<..>>` 経由で `ObjectHandle` の `Weak<dyn DocumentResolver>` から到達可能にする。`m->obj_cache` は canonical handle registry そのもので、`Pdf::get_object_handle`（= `QPDF::getObject`, `QPDF.cc:1952-1959`）と `Pdf::drop`（= `~QPDF`）の両方がここを見る。`Pdf::get_xref_table` は `QPDF::getXRefTable`（`QPDF.cc:2370-2377`）の effective source table snapshot、`Pdf::get_all_objects` は `fixDanglingReferences` と `m->obj_cache` enumeration（`QPDF.cc:1258-1294`）を canonical handle 上で実行する。`m->encp`（`flpdf-25kg.3.11`）は `Pdf::encryption` と同一の `Rc<RefCell<Option<EncryptionState>>>` を共有し、qpdf の `shared_ptr<EncryptionParameters>` を複数の owner が保持する形を再現する。`pipe_stream_data` は `QPDF::pipeStreamData` と同じく source read 前に `QPDF::decryptStream` 相当を呼び、同じ cell の method state / object-key cache を更新して AES/RC4 stage を前置する。`flpdf-25kg.3.5`/`.3.5.1`（ともに close 済み）で `readObjectAtOffset`/`readObject`/`readStream` の全 xref 形式（uncompressed type 1・ObjStm・canonical type-1 stream framing recovery を含む）が canonical resolver へ移植済み。`reader.rs`/`xref.rs` 自身の filter 呼び出し箇所の consumer cutover も `flpdf-egzr.3.2.10`（子 `.3.2.10.1`/`.3.2.10.2` close 済み、PR #859 merged）で完了し、production 経路の `decode_stream_data`/`encode_stream_data` 呼び出しはテストコードのみに残る。resolve 時文字列復号と pipe 時ストリーム復号 primitive は移植済み。残る raw `Object` route（`resolve_borrowed` と repair/recovery 経路）の削除は `flpdf-egzr.3.2.8`（close済み）) + `reader/file_object.rs`(1405) + `xref.rs`(1220) + `object_copy.rs`(342: `copyForeignObject`) + `cache.rs`(112: xref 由来の `ObjectCache` / `CacheEntry`。消費者は `reader.rs`) + `writer/object_streams/eligibility.rs`(263: qpdfの `getCompressibleObjGens` eligibility traversal) + `reader.rs`(491: `Pdf::remove_security_restrictions`) + `acroform_document_helper.rs`(649: `AcroFormDocumentHelper::disable_digital_signatures`) + `signatures.rs`(read-only inspection and flpdf-only SigFlags/value helpers) + `ref_chain.rs`(159: `resolve_ref_chain` / `terminal_ref_of_chain` / `MAX_REF_CHAIN_DEPTH` — 深さ上限付き間接参照解決の共有プリミティブ。20 モジュールが使用) | 🔀 |
| `QPDF.cc`（xref registration/recovery と mutation 境界） | `516-607,686-708,1187-1210,1996-2005` | `xref.rs` の `XrefRegistration` が xref 読み取り・recovery merge ごとの object-number-wide `deleted_objects` free-row filter を所有する。通常の `read_xref` は `/Size` 整合性検証までこの set を使い、その後 clear する（`:686-708`）。一方 `reconstruct_xref` の line scan は `:575` で clear してから `:576-607` の candidate xref-stream re-read に進み、その re-read は fresh registration を持つ。`ResolverCore` にはいずれの一時 state も渡さない。`reader.rs` の `Pdf::set_object` / `replace_object` は canonical cache replacement だけを担い、この xref set を clear/add しない。canonical xref/cache removal と outstanding handle の null 化は `remove_object_handle` が担う。 | ✅ |
| `QPDFParser::parse` / `QPDF::readObject`（indirect handle生成とstream framingの境界） | `QPDFParser.cc:155-172`; `QPDF.cc:1331-1349` | `parser.rs::parse_qpdf_file_object_handle_with_diagnostics` がtokenize中にindirect `ObjectHandle`を生成し、`xref.rs::BootstrapHandleDocument` がpre-`Pdf`のObjStm/xref bootstrapで同じhandle graphを使う。stream tailのframingはcaller側で継続する。 | 🔀 pre-`Pdf` bootstrap ownerはqpdf parserの一時contextに対応し、post-openのcanonical resolver (`flpdf-25kg.3.5`)とは分離している |
| `QPDF.hh`（`EncryptionParameters`） | 899-921 | `encryption/state.rs` (`EncryptionState`, `EncryptionMode`, `EncryptionInfo`)。qpdf は独立した2つの bool、`encrypted` / `encryption_initialized`（`QPDF.hh:907-908`）を持つが、flpdf はこれを単一の `Option<EncryptionState>`（`None` = 未初期化 or 認証済み未暗号化のいずれか、`Some` = 認証済み暗号化）に畳んでいる。安全性の根拠: `encryption_initialized` の唯一の用途は `initializeEncryption()`（`QPDF.cc:471` で1文書につき高々1回しか呼ばれない）内の再入防止ガード（`QPDF_encryption.cc:721,724`）で、flpdf の構造上この再入自体が起こり得ないため観測可能な挙動差は生じない。逸脱理由は `reader/resolver.rs` の `ResolverCore::encryption_parameters` doc にも記載（`flpdf-25kg.3.11`） | 🔀 `.3` で `reader.rs::encrypt_dictionary_handle` から `parse_inspection_state` / `authenticate` まで canonical `ObjectHandle` accessor に切り替え、`/CF`・`/Perms`・標準ハンドラ入力を raw `Dictionary` の materialize なしで読む。writer donor-copy の raw snapshot は writer slice で除去する |
| `QPDF::interpretCF` (`QPDF.hh`; `QPDF_encryption.cc`) | `1122-1127`; `700-716` | `encryption/crypt_filters.rs` の `interpret_cf` / `interpret_cf_from_handle` | ✅ 値選択を共有し、ObjectHandle 版は `try_as_name` で lazy resolve。`crypt_filters` → built-in `/Identity` → `e_unknown`、non-name → `e_none` の順と resolver error 伝播を維持。`reader/resolver.rs` の pipe-time `decryptStream` consumer が live stream dictionary に対して使用する |
| `QPDF::decryptStream` (`QPDF_encryption.cc`) | `1045-1153` | `reader/resolver.rs` の `inspect_stream_encryption` / `pipe_stream_data` | ✅ `/XRef` early return、`/V >= 4` gate、typed direct `/Crypt` と equal-length array pairing、Crypt-before-Metadata precedence、unknown warning + `cf_stream` rewrite、qpdf の object-key cache、`PlAesPdf` / `PlRc4` 前置を source read 前に実行。stream dictionary の lazy resolve 中は encryption cell borrow を保持しない。resolve-time payload 復号の重複は qpdf-deviation として解消対象（本issueではconsumer cutoverを範囲外とする） |
| `QPDFParser.cc` | 519 | `parser.rs` の `LiveInput` / `LiveTokenSource` / `LiveFileParser` は `InputSource` を一度だけ前進する file-object baseline（`QPDFParser.cc:27-518`）。canonical resolver の uncompressed type-1 consumer と、decoded-stream-relative `SliceLiveInput` 経由の ObjStm member consumer（`reader.rs::parse_object_stream_entry`）が使い、token 終端の one-character unread、diagnostic、top-level/nested/container/null の parsed offset、empty/dictionary/bad-token/depth recovery をここで共有する。uncompressed 側は canonical unresolved handle を同時に生成する。live canonical と context-none explicit の parser invocation は qpdf の parse-call description template を非 null handle に stamp し、container の render shift と null の無記述も維持する。`ObjectHandle::parse` / `parse_with_description` は同じ context-none entry point で、warning を `Error`、nested `N G R` を `Error::Internal`、非 C whitespace の後続を parse error にする。`ObjectHandle::parse_with_context` は同じ live parserをcanonical resolver/cacheへ接続し、qpdfの未解決参照identityとdocument warning sinkを維持する | 🔀 canonical uncompressed consumer は `StringDecrypter`（`flpdf-25kg.3.17`）を object-ref と shared `EncryptionState` に束縛し、`QPDF::readObject` / `QPDFParser` と同様に top-level・array・nested dictionary・stream dictionary の `tt_string` だけを token 時に復号する（`QPDF.cc:1331-1340`; `QPDFParser.cc:114-121,327-365`; `QPDF_encryption.cc:977-1039`）。完成した `/Type /Sig` + `/ByteRange` 辞書だけは raw `/Contents` bytes と parsed offset を復元する。ObjStm / context-none explicit parse / content mode は decrypter を渡さず、unknown word も callback 非呼出し。Content mode は既存 `Parser` を維持し、file-object live parser は content grammar を兼用しない |
| `QPDFTokenizer.cc` | 965 | `tokenizer.rs`（18 token types、owned value/raw/error bytes/offset、push/pull、pull-only `allowEOF`、`includeIgnorable`、space/comment、bad-token recovery、max length、`betweenTokens`、unread、inline-image `EI` discovery。`QPDFTokenizer.hh:34-193`; `QPDFTokenizer.cc:45-965`）+ `parser.rs` の content mode + `content_stream.rs` の `ParserCallbacks` orchestration + `object.rs` の `Operator` / `InlineImage`（`QPDFParser.cc:27-125,130-377`; `QPDFObjectHandle.cc:1770-1847`） | ✅ `QPDFTokenizer` の責務境界を移植済み。object/parser/content callback consumers は共有 tokenizer を使用し、旧 content lexer は削除 |
| `InputSource` 系 5 ファイル | 625 | `Read + Seek` ジェネリクスで代替。所有者は `reader/resolver.rs` の `ResolverCore`（`m->file` 相当）。`ResolverCore` のメソッドは `InputSource` の 3 操作 `seek`/`tell`/`read`（`InputSource.hh:71-74`）に限定し、`OffsetInputSource`（`QPDF.cc:406`）が担う header shift は `seek`/`tell` が適用する。例外は `rewind_underlying_source` 1 つで、これは wrapper が持つ `proxied`（`libqpdf/qpdf/OffsetInputSource.hh:24`）に相当する — `OffsetInputSource::rewind` は logical 0 に行く（`OffsetInputSource.cc:55-59`）ため `m->file` では表現できない。owned-window 系の legacy helper（`read_window` / `read_physical_input`）は `ResolverHandle` 側の `qpdf-legacy-tenant` で、`ResolverCore` の面には置かない | ⚪ |

`QPDFObjectHandle::parsePageContents` keeps the `all_description` produced by
`arrayOrStreamToStreamArray` when it enters `parseContentStream_data`
(`libqpdf/QPDFObjectHandle.cc:1438-1485,1740-1850`). The canonical flpdf
content-parser callback carries that source description together with qpdf's
`content` or `stream data` object description and the parser offset. In
particular, an EOF inside an inline image uses the end position returned after
the tokenizer consumes the truncated image, matching qpdf's `input->tell()`.
The qtest `parsing 10` regression pins this complete diagnostic context; the
other test-37 parsing rows retain their existing object spans and `handleEOF`
output.

pre-`Pdf` の xref bootstrap も qpdf の `QPDF::Members::file` / `InputSource`
の遅延 read 境界（`QPDF.hh:67-97,1453-1457`、`QPDF.cc:245-275`）を保つ。
`BootstrapHandleDocument` は handle state と diagnostics を先に共有するが、
入力 snapshot は `OnceCell<Rc<[u8]>>` として未解決 indirect object または
indirect `/Length` の実解決時だけ初期化する。direct-only の trailer/xref
metadata path は入力全体を複製しない。これは qpdf の object/stream lazy read
を generic `Read + Seek` の static resolver lifetime に合わせるための内部
ownership 実装であり、PDF bytes、warning、xref/cache identity は変更しない。
| `QPDF_pages.cc` | 319 | `pages/repair.rs`（`QPDF_pages.cc:39-75` の `getAllPages` root correction と `:77-150` の `getAllPagesInternal` repair/enumeration を canonical `ObjectHandle` graph 上で実装） + `optimization/inherited_attrs.rs`（canonical page promotion/clone と衝突しない `Pdf::next_obj_gen` allocation） + `pages.rs` / `pages/tree_rebuild.rs`（flatten/insert/remove と legacy consumer の残り） | 🔀 `flpdf-25kg.3.7` で repair/enumeration の canonical route を追加。`.3.2.6.15` では `QPDFPageObjectHelper::getAttribute` の bottom-up `/Parent` climb（`QPDFPageObjectHelper.cc:217-263`。`QPDF_optimization.cc:121-245`/`QPDF_pages.cc:154-180,205-248` は top-down push とツリー変異のオラクル）を、共有 `PageParentCursor` / `resolve_inherited_handle_with_max_depth` として live `ObjectHandle` で切り出した。直接親の identity、間接親の canonical `ObjectRef`、null/非辞書親、cycle/depth guard をこの境界で保持し、`/Rotate` の未指定を合成しない。`.3.2.6.16` では `tree_rebuild` の単一文書 consumer を canonical handle route に切り替え、選択ページの inherited `/MediaBox`・`/CropBox`・`/Resources`・`/Rotate` を再親子付け前に push、直接 non-scalar は `make_indirect_object_handle` で一度だけ昇格、既存 indirect 値は identity を保持し、duplicate は `shallow_copy`、root `/Kids`・`/Count`・各 leaf `/Parent` は live handle を replace/remove する。qpdf の absent `/Rotate` は合成しない。`QPDFObjectHandle.cc:1199-1209,2072-2079` の live replace/remove・shallow-copy がこの consumerの mutation oracleである。`QPDFJob.cc:2360-2632` の page-selection orchestration はこの境界の外であり、`page_extract` uses canonical `copyForeignObject`/`ObjectHandle`; `page_merge` / `page_label` remain separate consumers |
| `QPDFExc.cc` / `QPDFSystemError.cc` | 123 | `error.rs`(125) | ✅ |

`flpdf-15qk` completes the `QPDF_pages.cc` cache boundary: `Pdf::page_list_cache`
stores the prepared root and ordered leaf identities after the canonical repair
walk, `PageDocumentHelper` consumers reuse it across JSON sections, and
`Pdf::update_all_pages_cache` plus page-tree rebuild/clear boundaries mirror
`QPDF::updateAllPagesCache` and the mutation-owned cache invalidation contract
(`QPDF_pages.cc:141-150`; `QPDF.hh:671-704`).

`flpdf-egzr.3.2.6.19` の `pages/tree_rebuild.rs` は、`QPDF_optimization.cc:159-228`
に合わせて選択ページへ inheritable attributes を materialize した後、元の page-tree
に残る `/Pages` node から `/MediaBox`・`/CropBox`・`/Resources`・`/Rotate` を remove する。
保持する root 以外の中間 node は引き続き orphan として writer-owned reachability cleanup に
委ねるが、writer が orphan を保存する場合にも qpdf の flattening-side cleanup を保つ。
`--pages` の CLI consumer で qpdf 11.9.0 と同じ root/kids/leaf の正規化 shape を
比較する回帰テストは `cli_pages_root_inheritable_qpdf.rs` が所有する。

`flpdf-egzr.3.2.6.26` では、subset extraction 後の name-level resource prune を
document-wide の独自 aggregate route ではなく、保持された各 leaf の
`PageObjectHelper::remove_unreferenced_resources` へ委譲する形に揃えた。これは qpdf の
`QPDFPageObjectHelper.cc:539-649` に合わせた parse-gated な page-local route であり、
剪定対象は `/Font` と `/XObject` のみ（各 category は shallow copy 後に変更）である。
旧 aggregate API とそれ専用の回帰テストは、qpdf 11.9.0 に対応物がないため削除した。
`QPDFJob.cc:2251-2337` の Auto 判定は tree rebuild 前に済ませ、job の page-subset boundary はその
結果が prune を許可した場合だけこの per-page route を実行する。xref-level の orphan
mark-and-sweep は `writer/reachability.rs` の責務とする。共有 `/XObject` category、
継承 `/Resources`、非対象 resource category、重複ページの差分回帰は
`crates/flpdf-cli/tests/cli_tests.rs` が qpdf 11.9.0 と比較する。

| `QPDF::resolve` / `QPDF::resolveObjectsInStream`（bootstrap cache completion） | `QPDF.cc:1700-1857` | `xref.rs::parse_xref_stream` は xref stream の file-object を `read_file_object_handle` で一度だけ parse し、`parser.rs` の ObjStm member と同じ canonical `ObjectHandle` graphへ installする。 | 🔀 `.2` で raw+handle の二重 parse と live parser の finished-tree conversion を除去。`.3` では post-open `ObjectCache::Resolved` と ObjStm reconciliation を handle identity に切り替えた。pre-`Pdf` bootstrap と `LoadedXref` の raw trailer boundary は後続の writer/final route に残る |

## 3. 書き込み — 最大の smear

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFWriter.cc` | 3044 | `writer.rs`(4494) + `writer/serialize.rs`(1008) + `writer/object_streams/{eligibility,planning,emission}.rs`(739) + `writer/encryption_state.rs`(258) + `writer/encrypted_strings.rs`(213) + `writer/plain/{plan,body,xref}.rs`(898) + `linearization/writer.rs`(3603) + `linearization/part1.rs`(370) + `linearization/back_patch.rs`(324) + `linearization/renumber.rs`(850) + `writer/rewrite_renumber.rs`(893) = **13,650 行 / 13 ファイル**。加えて `object.rs`(650: `write_pdf` = `unparseObject` / 768: `write_pdf_qdf` / 910-: trailer `/ID` = `writeTrailer`。`writer.rs` と `linearization/writer.rs` が委譲) と `writer/object.rs::visible_dict_entries`（canonical `ObjectHandle` 境界） + `writer/rewrite_renumber.rs::visible_raw_dict_entries`（raw `Object` 境界） = `QPDFWriter.cc:1490-1491` の null 値 dict キー抑制。さらに `writer/object.rs` に qpdf の writer-owned live-handle emission trait/walkers（`unparseObject`/`unparseChild`/`writeTrailer`、`QPDFWriter.cc:1072-1810,2236-2376,2907-3035`）を集約し、`object_handle.rs` は graph identity・payload・mutation・JSON の責務だけを保持する。旧 handle-owned emission implementationは削除し、production/test caller は writer boundary の canonical surface を直接利用する。`write_stream_body_qdf` は最終レビューで見つかったギャップの修正（Task 9）: `write_pdf_stream_qdf`(`object.rs:1036`、real production callsite は `writer.rs:4437`)に対応する QDF+stream 形の primitive が欠けていた。`Dictionary::write_pdf_stream_qdf` 自身に `refiltered` 概念が無いため（唯一の呼び出し元 `write_stream_to_buf_qdf` は既に確定済みの `/Filter`/`/Length` を持つ dict しか渡さない）、`write_stream_body`（compact 版）と異なりこちらも `refiltered` パラメータを持たない。null 値 dict キー抑制(`:1490-1491`)は `try_is_null` 経由で `write_object`/`write_object_qdf`/`write_stream_body`/`write_stream_body_qdf` の4つに適用し、`write_trailer` は `writeTrailer` 自身と同様に無抑制。`writer/encryption_state.rs` の `WriterEncryptionState` は `QPDFWriter::Members` の暗号 state (`QPDFWriter.hh:641-663`)、`set_data_key` は `setDataKey` (`QPDFWriter.cc:842-847`) と `compute_data_key` (`QPDF_encryption.cc:325-356`)、`with_object_data_key` は非 ObjStm member の set/unparse/clear (`QPDFWriter.cc:1761-1796`) に対応する。source ID ではなく emitted ID と generation 0 を使い、`Option<u32>` が qpdf の `-1` sentinel を置換する。qpdf の明示 clear は正常系だけだが、Rust callback の `Err` 後にも clear するのは出力 byte を変えず stale state を残さない内部代替である。全て `pub(crate)`・`#[allow(dead_code)]`。`flpdf-a32l` は AES で暗号化済みの文字列を full / linearized writer の共通 serializer context で強制 hex 化し、RC4・非暗号化・ObjStm member は既存の heuristic を維持する（`QPDFWriter.cc:1567-1592`）。既存 primitive の production consumer 移行（`flpdf-egzr.3.2.5` + 子 `.5.1`〜`.5.4`）と暗号 state の consumer 移行（`flpdf-3yn9.11`/`.12`）はいずれも close 済みで、`PlAesPdf`/`PlRc4`/`run_writer_pipeline`/`adjust_aes_stream_length` は production コードで実使用されている。`flpdf-25kg.3.48.4` では、qpdf `QPDFWriter.cc:1072-1157,1334-1360,1488-1505,2907-2925` の writer-owned live-handle boundary に合わせ、full-rewrite Catalog の output-only `/Extensions` 復元を `CatalogExtensionsSnapshot`/`restore_catalog_extensions` に統合し、linearization の pass-1/final `/ID` 構築を `generate_id_handle` と `ObjectHandle` へ移した。`QPDFObjectHandle.cc:1575-1642` の unparse 契約、`QPDF_Stream.cc:571-620,640-685` の stream/provider 契約は既存の canonical emission surface として利用する。writer donor-copy の raw `Object` 境界は後続 slice に残す。既存 primitive の production consumer 移行（`flpdf-egzr.3.2.5` + 子 `.5.1`〜`.5.4`）と暗号 state の consumer 移行（`flpdf-3yn9.11`/`.12`）はいずれも close 済みで、`PlAesPdf`/`PlRc4`/`run_writer_pipeline`/`adjust_aes_stream_length` は production コードで実使用されている。🔀 の根拠は ObjectHandle 移行の未完了ではなく、下記の「xref 出力が 3 箇所に分かれる」構造的 smear が独立に残っていること | 🔀 |
| `QPDFJob.cc:2833-2925` / `QPDFWriter.cc:217-265,1356-1435,2176-2182` | qpdf job version-spec parsing, writer version/extension pair selection, and Catalog `/Extensions /ADBE` reconciliation | `pdf_version.rs::parse_pdf_version_spec` + `flpdf-cli/src/main.rs::CliVersionOptions`/`parse_cli_version_options`/`apply_cli_version_options` + `writer.rs::effective_pdf_version_and_ext`/`inject_adbe_extension`/`strip_adbe_extension` | ✅ qtest `extensions-dictionary.test` 156/156 (qpdf 11.9.0; `test_driver 34` and force-1.8.5 QDF/non-QDF checks) |

`QPDFWriter::getTrimmedTrailer` (`QPDFWriter.cc:2009-2031`) returns an
`unsafeShallowCopy` before `enqueueObjectsStandard` and `writeTrailer`; the
copy keeps the immediate child handles intact because the writer only mutates
top-level trailer keys. The subsequent
`getKeys()` traversal therefore applies null-valued dictionary-key visibility
in plain, QDF, and encrypted writer modes alike. `writeTrailer`'s own loop
does not repeat the `isNull()` check because it receives that already-trimmed
view. The canonical flpdf writer keeps the same separation: `suppress_null_values`
is mode-independent, while `removed_refs` separately excludes explicitly
deleted identities. The regression and qpdf 11.9.0 probe are tracked in
`flpdf-9hc.42`.

### `test_driver` test 29

`qpdf/test_driver.cc:1096-1145` deliberately constructs a mixed-ownership
graph that ordinary `replaceKey` does not reject: a foreign `/QTest` handle is
placed in an ownerless direct dictionary, and that dictionary is then attached
to the secondary PDF's live trailer. `QPDFWriter` catches the resulting
write-time logic error (`QPDFWriter.cc:1072-1155`). It then repeats the setup
with an indirect root whose source `QPDF` is destroyed before writing, so the
retained child reaches `QPDF_Destroyed::unparse` rather than being silently
converted to null (`QPDF.cc:215-236`, `QPDF_Destroyed.cc:18-29`). The final
direct foreign-root insertion exercises `QPDFObjectHandle::checkOwnership`
(`QPDFObjectHandle.cc:2355-2365`).

flpdf's `run_test_29` now constructs both live-trailer graphs through
`ObjectHandle::replace_key` and feeds them to the real `PdfWriter`. The writer
owner boundary reports qpdf's full mixed-object message, while
`ObjectHandle::unsafe_shallow_copy` mirrors
`QPDFObjectHandle::unsafeShallowCopy` (`QPDFObjectHandle.cc:2082-2088`) and
`QPDF_Dictionary::copy(true)` (`QPDF_Dictionary.cc:36-47`), preserving the
destroyed child until the canonical writer unparse emits qpdf's destroyed
handle message. The driver-level regression asserts the three logic-error
lines and keeps the common `test 29 done` footer at `driver::run`'s boundary.

`flpdf-egzr.3.2.20` で、`QPDF::getTrailer` (`QPDF.hh:311`, `QPDF.cc:2349-2352`)
に対応する production caller は `Pdf::trailer` / `Pdf::trailer_key_handle` へ移行した。
`writer/rewrite_renumber.rs::visible_raw_dict_entries` と raw trailer serializer は
test-only の legacy projection として残り、production の PCLm・QDF・暗号化 writer は
live `ObjectHandle` から trailer と `/ID` を取得する。

`QPDFJob::Config::compressionLevel` (`QPDFJob_config.cc:135-139`) は
`QPDFJob::setWriterOptions` (`QPDFJob.cc:2847-2851`) で `Pl_Flate` の共有
compression levelへ適用され、`recompressFlate` (`QPDFJob_config.cc:498-503`、
`QPDFJob.cc:2870-2872`) は `QPDFWriter::willFilterStream`
(`QPDFWriter.cc:1260-1270`) の lone-`/FlateDecode` preserve gateだけを解除する。
flpdfは `WriterSettings`/`WriterOptions` の `compression_level` と
`pipeline/flate.rs::Flate::set_compression_level` で同じ設定順を保持し、
CLIのtop-levelとnative rewriteの両方をcanonical `PdfWriter`へ接続する。

進捗計測の準備境界は `writer.rs:538-563` に固定する。QDF/content-normalization
または non-none decode level の `PageDocumentHelper::get_all_pages()` による page-tree
修復を先に実行してから `get_object_count()` を取得し、qpdf の
`qdf_mode || normalize_content || stream_decode_level` による `doWriteSetup`→progress
snapshot (`QPDFWriter.cc:2114-2116,2189-2193`) と同じ順序で、修復が mint した indirect
object も `events_expected` に含める。specialized writer の同じ repair 境界は
`writer.rs:3010-3025` にある。linearized writer は既存の準備後 snapshot を維持する。

進捗callbackの失敗は `QPDFWriter.cc:2957-2982` の
`ProgressReporter::reportProgress` 呼び出しからwrite全体へ例外が伝播するqpdfの責務に
合わせ、flpdfの `PdfWriter::register_progress_reporter` /
`QPDFJob::register_progress_reporter` は `FnMut(u8) -> Result<()>` を受け、standard・
ObjStm・linearizationの全イベントで `Result` をその場で伝播する。完了後にfirst errorを
検査する迂回や、callback failureを成功扱いにするlegacy routeは維持しない
（`flpdf-egzr.8.8`）。

`--progress` のCLI consumerは、qpdf 11.9.0の `QPDFJob::Config::progress`
（`QPDFJob_config.cc:478-481`）から `setWriterOptions` 内のfallback reporter登録
（`QPDFJob.cc:2926-2935`）を経て、writerの `indicateProgress`
（`QPDFWriter.cc:2187-2193,2957-2987`）へ到達する。flpdfは
`flpdf-cli/src/main.rs` のCLI writer境界で既存の
`job/lifecycle.rs::QPDFJob::configure_writer_progress` を呼び出し、
`QPDFJob::set_progress` により設定だけを渡す。callbackの文言・info/save channel・
0..100のイベント計算はそれぞれJob/Loggerとcanonical `PdfWriter`が所有し、CLIに
別のlegacy bridgeを置かない。qpdf 11.9.0の実測では通常の `OUTPUT` へはinfo/stdout、
`OUTPUT=-` へはstderrに `0%` から `100%` のprogressを出し、PDF bytesはstdoutに残る。
qtest `progress-reporting` の3行はこの同一責務境界を検証する。

qpdf は 1 クラスで standard / linearized / encrypted / objstm を統一的に扱う。flpdf は
経路ごとに分岐しており **xref 出力が 3 箇所**に分かれる。byte-parity の修正が片方の
経路にしか入らない構造的リスクがここに集中している。`emit_canonical_pdf_inner`
は単独で約 1,250 行。

`flpdf-3yn9.12` の stream encryption 対応は、`QPDFWriter.cc:935-999` の
`PipelinePopper`/`pushEncryptionFilter`/`adjustAESStreamLength` を
`writer.rs::run_writer_pipeline`、`pipe_writer_stream_payload`、
`adjust_aes_stream_length` に対応させる。`QPDFWriter.cc:1239-1314` の
`willFilterStream` 相当は既存の `reencode_stream_for_compress` の結果をそのまま
消費し、`QPDFWriter.cc:1528-1560` の cleartext metadata 分岐は
`stream_encryption` と `encrypt_stream` を通じて dictionary string、payload、
AES `/Length` 調整を同時に平文化する。dictionary の string serializer は
`writer/encrypted_strings.rs::EncryptedStringEmitter::write_stream_dict` の
`encrypt_strings` 引数で key-clear を表現する。linearization の hint stream は
layout 非対象のため旧 in-place bridge を維持し、canonical full-rewrite と ObjStm
container は新しい pipeline route を使う。

`flpdf-8f1o` では plain writer の `PlainWritePlan` が、qpdf の
`willFilterStream` が返す stream data buffer と同じ責務を
`CachedStreamOutput` に保持し、`plain/body.rs` の emission が再利用する。
provider を計画時の discard probe と emission で二重に呼ばないため、警告を発行する
legacy provider に抑制フラグを追加せず、qpdf の一回計算／再利用境界を保つ。
specialized writer と linearized writer の別経路は、それぞれの qpdf 呼び出し契約を
別 slice で扱う。

### ObjectHandle emission-time encryption surface (`flpdf-egzr.3.2.15`, 2026-08-15)

qpdf の暗号化は Object tree を事前に書き換えない。`QPDFWriter.cc:842-847`
の `setDataKey` が object number/generation ごとの data key を設定し、
`QPDFWriter.cc:1761-1796` が非 ObjStm member の unparse 前後でその key を
set/clear する。文字列は `QPDFWriter.cc:1567-1599` の unparse 時点でだけ
暗号化し、AES は hex、RC4 は通常の string 表現を選ぶ。stream dictionary の
metadata cleartext 例外と payload 用 encryption filter は
`QPDFWriter.cc:1528-1557` / `:965-998` の責務であり、dictionary string と
payload を同じ責務に混ぜない。

flpdf はこの境界を `writer/encrypted_strings.rs` の additive API として
`ObjectHandle` に接続した。`EncryptedStringEmitter::write_handle_object` は
`writer::ObjectWriterEmission::{write_object_with_string_writer,write_object_qdf_with_string_writer}`
を data-key scope 内で呼び、`write_handle_stream_dict` は同じ callback を
stream dictionary にだけ適用する。`/Encrypt` object と ObjStm member は
個別 key を持たず平文のままにし、stream payload は既存の canonical pipeline
へ委譲する。`/Sig` の `/Contents` は例外で、qpdf の
`f_hex_string | f_no_encryption` (`QPDFWriter.cc:1501`) に合わせて callback を
迂回し、平文の hex string として出力する。`EncryptionContext::encrypt_dict_handle` と
`write_encryption_dictionary_handle` は `QPDFWriter.cc:2244-2255` の直接
Encrypt-map emission を handle tree で再現し、直接の `/O` `/U` `/OE` `/UE`
`/Perms` だけを hex 化する。`flpdf-egzr.3.2.5`（close 済み）writer cutover が
この surface を production consumer として使用している。

### Non-linearized encrypted Generate ObjStm numbering (`flpdf-cecz`, 2026-08-31)

qpdf 11.9.0 の standard writer は、`QPDFWriter.cc:1072-1118` の enqueue 中に
generated ObjStm の最初の member へ到達すると、`assignCompressedObjectNumbers`
（`:1057-1066`）で container とその全 member の番号を直ちに予約する。
`getCompressibleObjGens` は `QPDF.cc:2393-2440` で `/Encrypt` を候補から除外し、
全 body の書き出し後に `writeEncryptionDictionary`（`:2244-2255`）が
`openObject(0)` で `/Encrypt` を末尾へ割り当てる。実測では複数 ObjStm の
container-first 番号、source object-number 順の member index、type-2 xref、
`/Encrypt` の後置がこの順序になる。

flpdf の非 linearized encrypted Generate route は `ObjectStreamRenumber` を
同じ canonical walk として利用し、通常 object と container の chunks を番号順に
interleave してから xref を確定する。ObjStm dictionary は qpdf の固定順
`/Type /ObjStm /Length ... /Filter ... /N ... /First ...` で直接 emission し、
copy-encryption の Generate でも同じ xref-stream/container route を使う。
QDF、linearized encrypted Generate、非 Generate の copy-encryption はこの issue の
scope 外であり、それぞれ既存の dedicated route と `flpdf-j4ph` が担当する。

**renumber は重複していない**: `writer/rewrite_renumber.rs` は `linearization/plan.rs` からも
使われる共有機構で、`linearization/renumber.rs` はその上に載る最終採番層。qpdf の
`obj_renumber` 1 本に対して 2 層構造だが、二重実装ではない。

## 4. 線形化 / 最適化

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF_linearization.cc` | 1796 | `linearization/`（`plan.rs` 7176, `hint_*` 3741, `check.rs` 3467, `show.rs` 2642, ほか）≒ 17,000 行 | ✅ qpdf `QPDF_linearization.cc:452-470` と同じく、`check.rs` の `/T` は xref parser が保持する `first_xref_item_offset` (`QPDF.cc:845-869,1110-1120`) に対する whitespace 消費後の位置比較だけを行う（構造探索・subsection再解析・flpdf 固有の hard failure は除去済み、`qpdf-deviation` マーカーも撤去済み）。初回または `/Prev` の classic xref row parse が後続 row で失敗しても、object 0 row で観測した offset を reconstruction へ保持する qpdf の mutable-state 挙動 (`QPDF.cc:626-708,846-869`) を `flpdf-7yvv` の side channel で再現する。`flpdf-1quo` で check consumer は primary/overflow hint stream を qpdf と同じ buffer に連結し、Page Offset / Shared Object / Outline の各 hint table の object count・length・shared membership・physical offset を qpdf の object-user 分類と実 xref extent に照合する。実装は 5+ モジュールに分散したまま（`optimization.rs` が達成したような単一モジュールへの集約は未達）。ObjectHandle 移行自体は完了: producer 側（`flpdf-3yn9.4`、plan.rs + hint_*）と consumer 側（`flpdf-egzr.3.2.9`、check.rs + show.rs）が close 済み。`check_consumer_production_uses_the_canonical_object_handle_route` / `show_consumer_production_uses_the_canonical_object_handle_route` は production 経路から `Object::` / `resolve_borrowed` / `decode_stream_data` / `page_refs` が消えたことを機械的に保証する。残存する `plan.rs` の `collect_direct_refs`（Object 版）は `#[cfg(test)]` の fixture walker に限定され、production closure と writer の計算は同ファイルの `collect_direct_handle_refs`（ObjectHandle 版）が担う。線形化書き込み経路自体（writer.rs 側、`flpdf-3yn9.5` 系列）は issue タイトルが明記する通り §3 `QPDFWriter.cc` のスライスであり本行の対応先ではない
| `QPDF_optimization.cc` | 381 | `optimization.rs`（optimization orchestration、inherited-page preparation、object-user maps、compressed-object folding）+ `optimization/inherited_attrs.rs`(575) | ✅ `flpdf-qxba.9.3` / `.9.4` で完全 cutover。`linearization/plan.rs` 側に `ObjUser` / `update_object_maps` は残っていない。⚪ `inherited_attrs.rs` の inheritable key null 判定（`push_node_attributes` / `push_child_reference`）は `Pdf::resolve_to_terminal` で `Pdf::set_object` bare-reference redirect の終端まで辿る。qpdf 自身のオブジェクトグラフは「あるオブジェクトの値が別の参照そのもの」という形を持てない（対応物なし）ため、`pages.rs` の `resolve_inherited_handle_with_max_depth`（bottom-up の姉妹関数）と同じ理由で同じ補償を行っている。⚪ `Optimization::update_object_maps` の reference-valued handle 再ディスパッチも `Pdf::set_object` が作る flpdf 固有形状だけを対象にし、qpdf parsed graph には追加の対応物を作らない（`QPDFParser.cc:26-90,140-176`）。 |

線形化の stream-parameter reachability は `writeLinearized` の
`skip_stream_parameters`（`QPDFWriter.cc:2543-2553`）と
`QPDF_optimization.cc:274-333` に合わせ、refilter 判定済みの参照元 stream
identity ごとに `/Filter` / `/DecodeParms` edge を除外する。probe と emission
は同じ `willFilterStream` 相当の metadata/content-normalization policy を使い、
共有 parameter object は保存される別 stream から引き続き到達可能にする
（flpdf-p045）。

`flpdf-xrgz` では producer の Part 4/first-half routing でも qpdf の `is_root`
precedence (`QPDF_linearization.cc:1090-1127`) を保持し、page からも参照される
Catalog を Part 3 shared hint に混ぜない。二ページ共有-resource fixture の
`object count mismatch for page 0` / phantom shared entry を実 qpdf 11.9.0 probe
で固定する。

`ObjUser` 分類（`ou_page` / `ou_thumb` / `ou_trailer_key` / `ou_root_key`）と
`updateObjectMaps` は `optimization.rs` に移設済み（`flpdf-qxba.9.3` / `.9.4`）。
`linearization/plan.rs` は consumer として呼ぶだけになった。

**objstm 経路の解錠は無い**: qpdf でも `optimize()` の呼び出し元は
`QPDF_linearization.cc:495` と `QPDFWriter.cc:2553`（`writeLinearized()` 内）のみで
linearize 専用。`flpdf-g6hb` が必要とする `getCompressibleObjGens` は
`QPDF.cc:2393` にある別物。

## 5. 暗号

### Encrypted writer matrix (`flpdf-25kg.6.1`, 2026-08-31)

qpdf 11.9.0 の Standard handler は、`QPDFWriter.cc:777-840` の V/R/CFM ごとの
version floor と `QPDF_encryption.cc:601-660,1180-1204` の V=5 random input 順を
持つ。`flpdf-cli/tests/encrypt_cli_tests.rs` の
`encrypted_writer_direct_handler_matrix_matches_qpdf_after_decrypt` は、固定された
入力・password・permission と全 6 direct handler（V=1/R=2、V=2/R=3、V=4 RC4/R=4、
V=4 AES/R=4、V=5/R=5、V=5/R=6）を qpdf で復号して QDF に再出力し、semantic/structural
bytes を比較する。qpdf-zlib-compat の
`encrypted_writer_deterministic_direct_handler_matrix_is_byte_identical_to_qpdf` は
V5 以外の deterministic 4 handler を raw bytes で比較し、
`encrypted_writer_copy_encryption_tuple_is_byte_identical_to_qpdf` は固定 V4 AES-128
donor の copy-encryption tuple を direct encryption と独立に比較する。

V5 の `/O` `/U` `/OE` `/UE` `/Perms` は qpdf CLI の CSPRNG（同じ qpdf invocation
でも毎回変化）を含むため raw qpdf CLI byte gate の対象外とし、既存の test-only
`V5Randomness` seam（`.6.5`）で flpdf の deterministic repeat を検証し、qpdf とは
復号後の QDF で比較する。production default は引き続き OS CSPRNG である。

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF_encryption.cc` | 1410 | `encryption.rs` (facade) + `encryption/state.rs` + `encryption/crypt_filters.rs` + `encryption/keys.rs` + `encryption/standard.rs`(1879) + `encryption/permissions.rs`(206) + `encryption/password.rs`(380: `password_bytes_for_read` + `password_candidates_for_read` — qpdf `QPDFJob.cc:1734-1790` の read-side hex decode、raw-byte pass-through、alternate encoding retry と suppress gate、`QUtil.cc:1821-1900` の PDFDoc/WinAnsi/MacRoman candidates、V=5 の 127-byte 切り詰めは Standard handler が担当) | 🔀 |
| `rijndael.cc` / `AES_PDF_native` / `MD5_native` / `SHA2_native` | 1668 | `encryption/primitives.rs`(106: AES single-block ECB と MD5) + `pipeline/sha2.rs` の `Sha2Digest`(SHA2)（外部 crate）。AES-CBC は `pipeline/aes.rs` の `PlAesPdf` に一本化済みで、`encryption/primitives.rs` には V=5 R=6 Algorithm 10/13 の single-block ECB だけが残る。qpdf は `SHA2_native` へ `Pl_SHA2` 経由でしか到達しない（`QPDF_encryption.cc:246,296` が唯一の production 利用）ため、RustCrypto の SHA-2 hasher も `Pl_SHA2` 移植の内部に閉じている。`encryption/primitives.rs` の一括 `sha256`/`sha384`/`sha512` wrapper は consumer cutover で削除済み | ⚪ |
| `RC4.cc` / `RC4_native.cc` | 63 | `encryption/rc4.rs`(80)（明示長キー / C-string キー、state 保持、separate / in-place processing） | ✅ |
| `QPDFCryptoProvider.cc` / `QPDFCrypto_*` | 774 | provider 抽象が無い | ⚪ |
| ランダム源 3 ファイル | 185 | `writer.rs` の `fresh_id_bytes` 等に散在 | 🔀 |

## 6. Pipeline / フィルタ

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `Pipeline.cc`（積層シンク基盤のみ。個々の `Pl_*` は下記の各行で個別に分類） | 114 | `pipeline.rs`（public `Pipeline` trait、identifier/write/finish lifecycle、logic/runtime error channel）。⚪ qpdf が bare `Pipeline*` で回す `next` slot に対応する `PipelineRef`（borrowed / owned の 2 択）を持ち、`Flate` / `LzwDecoder` の `next` はこれを受け取る。**⚪ (B) stage の所有者**: qpdf は構築した stage を filter instance 内に保持し呼び出し側へは非所有ポインタを返す（`QPDFStreamFilter.hh:47`、`SF_FlateLzwDecode.cc:88`・`:108`）。flpdf は stage を値で返し、多段 chain の内側 stage は `pipeline.rs` の `PipelineRef::Owned` が持つ。構築順・stage 数・出力 bytes は不変で、動くのは所有者だけ。この slot を通る本番 write path は `qpdf-zlib-compat` gated の `cmp_generate_objstm_tests` で qpdf golden に pin する | ✅ |
| `Pl_Count.cc` | 48 | `pipeline/count.rs`（byte count、last byte、forwarding、finish lifecycle） | ✅ |
| `Pl_MD5.cc` | 66 | `pipeline/md5.rs`（enable/persist/reuse、hex digest、forwarding/error order）+ `filespec_helper/embedded_file_stream.rs`（EmbeddedFile `/Params /CheckSum` production consumer） | ✅ |
| `Pl_Flate` / `SF_FlateLzwDecode` | 946 | `pipeline/flate.rs` + `stream_filter.rs` の `FlateLzwStreamFilter`（`/Predictor` `/Columns` `/Colors` `/BitsPerComponent` `/EarlyChange` の解釈、codec → predictor の chain 構築、`QIntC::to_uint` の range error timing）。`SF_FlateLzwDecode::getDecodePipeline`(`SF_FlateLzwDecode.cc:75-110`) 相当の `decode_pipeline` を持つ。構築順（sink 側から、predictor を作って `next` を差し替えてから codec を作り、その codec を返す）は qpdf のままで、内側になる predictor だけ `PipelineRef::Owned` が所有する。既知の逸脱: whole-buffer route の `pipe_codec` は `Pl_Flate` の warn callback を stage 構築側で設置するが、qpdf は `getDecodePipeline` の呼び出し側(`QPDF_Stream.cc:564-567`)で設置する。この route が構築する `Pl_Flate` はいずれも qpdf が当該 filter の iteration で設置するのと同じ callback を受け取るので、警告の文言・順序は変わらない。再現できないのは qpdf のもう一方のケース — cast は stage 単位ではなく filter 単位で 1 回走り(`:561-563` の guard の外)、stage を構築しない filter の iteration では別の場所で構築された stage に当たる。設置位置とこのケースは共に `QPDF_Stream::pipeStreamData` 移植の担当（`decode_pipeline` 側は qpdf 通り callback を設置しない） | ✅ |
| `Pl_LZWDecoder` | 189 | `pipeline/lzw.rs`（3-byte rotating buffer、1 入力 byte あたり 1 code、table 成長と code 幅遷移、eod latch、qpdf の 7 種の診断文言）+ `stream_filter.rs` 経由の production decode | ✅ |
| `Pl_PNGFilter` | 232 | `pipeline/png_filter.rs`（32-bit wrapping の row 幅算出、constructor の 3 種 rejection、未知 filter byte の無視、finish の zero-pad row、Up 固定 encoder）+ `filters.rs` / `writer/serialize.rs` の production consumer。⚪ row buffer の確保だけは constructor ではなく最初の write まで遅延（出力バイト・呼び出し境界・エラー timing に影響しない） | ✅ |
| `Pl_TIFFPredictor` | 175 | `pipeline/tiff_predictor.rs`（incremental row buffering、8-bit の byte differencing、packed sample の signed MSB bit I/O、finish 時の zero padding）+ `stream_filter.rs` / `filters.rs` の Predictor 2 production consumer。qpdf の TIFF fixture vectors と construction/write/finish error timing を pin。qpdf が filter instance に保持する stage ownership は、flpdf では `PipelineRef::Owned` が内側 predictor を保持する意図的な Rust ownership substitution。qpdf head `cf047b20721b18b15525c04b6970e562c90c4a6a`（`Pl_TIFFPredictor.cc:38-48`）の `bits_per_pixel` / wide row geometry preflight を constructor に追加し、overflow が `previous` 状態領域へ到達しないようにした。preflight 後の representable geometry は pinned qpdf 11.9.0 の wrapped row width を保持し、既存の partial-row/packed-row bytes を変えない。qpdf head の `memory_limit > 0 && bpr > memory_limit / 2` は `DecodeLimits::max_tiff_memory` として decode preflight・prefix・実行へ伝播し、`None`/`Some(0)` は既定の unlimited を維持する | ✅ |
| `Pl_ASCII85Decoder` / `SF_ASCII85Decode` | 108 + 31 | `pipeline/ascii85_decoder.rs` + `stream_filter.rs`（`SF_ASCII85Decode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_ASCIIHexDecoder` / `SF_ASCIIHexDecode` | 96 + 31 | `pipeline/ascii_hex.rs` + `stream_filter.rs`（`SF_ASCIIHexDecode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_RunLength` / `SF_RunLengthDecode` | 146 + 38 | `pipeline/run_length.rs` + `stream_filter.rs`（`SF_RunLengthDecode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_AES_PDF` | 200 | `pipeline/aes.rs`（qpdf の contract を全量移植: block 単位の write バッファリング、first-block を IV として消費する復号側と IV を先頭へ書く暗号化側、ISO 32000-1 7.6.2 の padding とその strip、`useZeroIV` / `setIV` / `useStaticIV` / `disablePadding` / `disableCBC`）＋ `PlAesPdf::decrypt_to_vec`（qpdf `decryptString` の `Pl_Buffer` + `Pl_AES_PDF` 組（`QPDF_encryption.cc:1013-1021`）に対応する one-shot）と `writer.rs` の stream consumer | 🔀 `reader/resolver.rs` の `QPDF::decryptStream` 対応は `PlAesPdf` を source-read pipeline の前段へ接続済み。resolve-time 経路も `encryption/standard.rs` の `decrypt_cipher_bytes` 経由で同じ `PlAesPdf` を通るため、AES 実装は qpdf と同じく 1 つだけ。⚪ `QPDFCryptoImpl::rijndael_init` / `rijndael_process` の crypto provider 抽象は `aes` / `cbc` crate の直接利用に置換（§ 逸脱候補の crypto provider 行と同じ代替）。block ごとに 1 回 process する呼び出し形は保持し、chaining 状態のみ provider 側ではなく cipher が持つ。**解消済みの逸脱**: 以前は `encryption/primitives.rs` の `decrypt_padded::<Pkcs7>` が別実装として併存し、qpdf が受理する入力（ブロック長に満たない末尾＝`Pl_AES_PDF.cc:107-118` の zero-pad、padding として不整合な末尾＝`:183-196` の strip 見送り）を `Err` にしていた。この厳密版を削除して `PlAesPdf` へ一本化したので、受理する文書は qpdf と一致する |
| `Pl_RC4` | 43 | `pipeline/rc4.rs`（65,536-byte既定buffer、stateful `encryption/rc4.rs`、write/finish lifecycle）+ `reader/resolver.rs` の pipe-time decrypt stage + `reader.rs` / `writer.rs` の既存 stream consumer | ✅ |
| `Pl_QPDFTokenizer.cc` / `ContentNormalizer.cc` | 141 | `pipeline/qpdf_tokenizer.rs`（optional downstream を持つ token-filter runner、EOF-token → `handle_eof`、`ID` separator 注入、inline-image 切替、raw token/discard/output、`handle_eof` 成功後の永久 detach と finish/error timing）+ production consumer `content_normalizer.rs`（bad-token state、CR/string/name normalization） | ✅ |
| `QPDFObjectHandle::TokenFilter` / `QPDF_Stream::addTokenFilter` / `isDataModified` | `QPDFObjectHandle.hh:129-190,420-475,978-1010`; `QPDF_Stream.cc:321-324,488-620,663-666` | `ObjectHandle::add_token_filter` / `is_data_modified` が共有filter listとdecoded→token-filter→normalize/encodeのlazy pipeを担う。`form_field_object_helper/rendering.rs` の既存 `/AP/N` reuse は eager `replace_stream_data` からqpdf `ValueSetter`相当の `AppearanceTokenFilter` へ移行し、`writer/plain/body.rs` は `is_data_modified` をlone-Flate fast pathの条件に含める。`linearization/writer.rs` の `append_body_object`（`stream_is_data_modified` helper 経由）も同じ `willFilterStream` 由来のゲートを適用: qpdf の `writeLinearized` は `QPDF::optimize` の `skip_stream_parameters` probe と実書き込みの計2回 `pipeStreamData` を呼び、token filter は pipe 間で状態リセットしないため実書き込み側は exhausted filter のパススルー（= stale content）を再エンコードする。flpdf の linearized writer には optimize 相当の二重 pipe が無いため、token filter 自体は起動せず「既に materialize 済みの (pre-filter) バイトを decode→re-encode」するだけで同じ observed output に一致させる（`docs.rs` 非公開のモジュール内 doc 参照）。`writer.rs` の `emit_canonical_pdf_inner` fallback と `writer/plain/body.rs` の `!plan.canonical` 分岐について、同種のゲート配線の要否を `flpdf-vkka` で検証済み（close）: `plain/body.rs` は既に canonical handle 経由で `is_data_modified()` を参照しており、`emit_canonical_pdf_inner` 側は PR #831 の `materialize_for_normalization` narrowing 後、`Object::Stream` 分岐が構造的に到達不能なため追加配線は不要と確認された | ✅ |
| `QPDFStreamFilter.cc` | 19 | `stream_filter.rs`（`set_decode_params`、decode pipeline factory、specialized / lossy の既定分類）。`QPDFStreamFilter::getDecodePipeline`(`QPDFStreamFilter.hh:46-49`) に対応する `StreamFilter::decode_pipeline` は `stream_filter_for` が返す全 filter が実装し、`None` は qpdf の `nullptr`（11.9.0 でこれを返すのは `SF_Crypt` だけ、`QPDF_Stream.cc:52-56`）。qpdf-shaped の production caller は `ObjectHandle::pipe_stream_data` に接続済みで、public whole-buffer decode helper は従来経路として併存する。registry、`QPDF_Stream::filterable` の shape reader、`DecodeParams` snapshot の責務記録は §1 の `QPDF_Stream` 行に集約し、ここには再掲しない | ✅ |
| `Pl_DCT.cc` (buffer/decode) | 207 (`1-57,77-116,119-143,195-248,296-326`) | `pipeline/dct.rs` + `stream_filter.rs` の `DctStreamFilter`（`decode_pipeline` が canonical route。qpdf の buffered write、empty/repeated `finish` の downstream finish、libjpeg scanline 出力、error/cleanup を対応）; qpdf refs: `Pl_DCT.hh:30-70`, `Pl_DCT.cc:1-57,77-116,119-143,195-248,296-326`, `SF_DCTDecode.hh:8-40`。stage owner は qpdf の filter-instance 保持 + caller の non-owning pointer に対し、Rust は stage を値で返し `PipelineRef::Owned` と `next` の borrow で保持する correspondence class (B) | ✅ default backend は `libjpeg-turbo-rs = 0.8.0`、`qpdf-libjpeg-compat` は `flpdf-libjpeg-compat` を明示的に有効化する system libjpeg backend（no vendored library、runtime switch なし）。system-libjpeg の ABI boundary は `flpdf-libjpeg-compat`（`csrc/jpeg_compat.c/.h` + `ffi.rs`）が所有し、`BITS_IN_JSAMPLE == 8`、libjpeg 6b-compatible (`JPEG_LIB_VERSION >= 62`) capability/version guard、qpdf 相当の whole-buffer exhaustion (`invalid jpeg data reading from buffer`、fake EOI なし)、panic-contained callback を持つ。qpdf 11.9.0 の 8-bit scope を対象に、最小 image XObject の `qpdf --show-object=3 --filtered-stream-data` differential（2026-08-10 観測）は default/C とも qpdf stdout 12 bytes = canonical `DctSink` 12 bytes、mismatch 0、stderr 0。canonical consumer は `decode_pipeline`、legacy whole-buffer bridge caller は後続 `flpdf-3yn9.6` で cutover、writer passthrough は別責務として残す |
| `Pl_DCT.cc` (compression) | 119 (`58-76,117-118,144-194,249-295`) | `pipeline/dct.rs` の `PlDct::new_compressor` が qpdf の圧縮constructor、whole-buffer `finish`、JPEG出力、downstream `finish` を対応し、`job/image_optimization.rs` の `ImageOptimizer` が `QPDFJob.cc:102-236,2156-2174` の metadata/threshold 判定、サイズ比較、provider-backed DCT XObject置換を所有する。RGB/Gray は qpdf の default sampling、CMYK は `JCS_CMYK` の 1x1 sampling を選ぶ。 | ✅ qpdf 11.9.0 `image-optimization.test` 24/24、`crates/flpdf-cli/tests/image_optimization.rs::optimize_images_emits_qpdf_identical_jpeg_bytes_for_gray_rgb_and_cmyk` による pinned qpdf との Gray/RGB/CMYK raw JPEG bytes 自動比較を確認 |
| `Pl_Base64` / `Pl_Concatenate` / `Pl_OStream` / `Pl_String` | 282 | `pipeline/base64.rs` / `pipeline/concatenate.rs` / `pipeline/ostream.rs` / `pipeline/string.rs`（JSON serialization/output の本番 consumer を含む） | ✅ |
| `Pl_StdioFile.cc` | 46 | `pipeline/stdio_file.rs`（positive partial write の継続、zero/error—including `Interrupted`—の即時 Runtime 化、`EBADF` finish のみ Logic 化）+ `json_inspect.rs`（4096-byte buffer、top-level file は close/drop、side file は explicit finish） | ✅ |
| `Pl_Buffer` | 82 | `pipeline/buffer.rs`（accumulation、optional pass-through、finish readiness、buffer ownership transfer） | ✅ |
| `Pl_Discard.cc` | 23 | `pipeline/discard.rs`（public terminal identifier、no-op write/finish、finish 後の再利用）+ `filespec_helper/embedded_file_stream.rs`（EmbeddedFile checksum terminal consumer） | ✅ |
| `Pl_Function.cc` | 62 | `include/qpdf/Pl_Function.hh:37-62` / `libqpdf/Pl_Function.cc:10-61` はコンストラクタを3つ持つ——C++ネイティブな `std::function` を受ける1つ（それ自体はC ABI固有ではない）と、C関数ポインタ+`void*`を受ける2つ。qpdf 11.9.0自身のソースで実際に呼ばれるのは後者のC-style overloadのみで、`qpdf-c.cc:1936` の `qpdf_write_json` と `qpdflogger-c.cc:58` の custom logger（いずれもC APIラッパー）に限られ、`std::function` overload の呼び出し元はqpdfのコードベースに存在しない。qpdf core の PDF reader/writer は `Pl_Function` を直接使用しない。実在する呼び出しが全てC APIラッパー経由のC-style overloadである以上、移植すべき非C-API production consumer が無く、`qpdf_write_json` の出力コールバックには `Json::write` に渡す caller-supplied `Pipeline`（`json/writer.rs:97`、C 側と同じくシリアライズ済み JSON バイトを受け取る境界。`Json::make_blob` は逆方向の producer closure で対応物ではない）、custom logger destination には `QPDFLogger` の `PipelineHandle` 型セッター（`logger.rs` の `set_info`/`set_warn`/`set_error`/`set_save`）を、それぞれ canonical route とする | ➖ |
| `Pl_SHA2.cc` | 75 | `pipeline/sha2.rs`（SHA-256/384/512 の bit 選択、`resetBits`、digest access、optional next への write/finish forwarding と error 順序、再利用 lifecycle）。`Pl_SHA2.hh:9-11` の契約通り `finish()` 後の最初の `write()` は同じ bit size の新 cycle を開始し、連続 `finish()` は empty digest を生成する。native backend が finalize 後に同じ context を再初期化する挙動（`sha2.c:670-673`; `sha2big.c:209-228`）は RustCrypto の `finalize_reset` に対応する。⚪ `bits=0` のままの write/finish は qpdf では null crypto provider を dereference し、最初の finish 前の digest access は未初期化 result buffer を読むため、Rust では定義済み logic error に変換する。production consumer は `encryption/standard.rs` の `r5_salted_hash` / `r6_password_hash`（qpdf `hash_V5`、`QPDF_encryption.cc:239-311`）で、初期 hash は連結バッファを作らずpassword/salt/udata を 3 回 write し（`:246-249`）、R=6 ループは毎周 fresh な `Pl_SHA2` を算出 bit size で構築する（`:295-299`）。qpdf が identifier を `"sha2"` に固定している（`Pl_SHA2.cc:8`）のに合わせ、callsite も同じ値を渡す | ✅ |

`/ID` が qpdf と非 parity だった原因は **アルゴリズム**（qpdf は 2 段階 MD5 で seed を
作る）であり、Pipeline 抽象の有無ではない。flpdf は全体をバッファするので任意の
バイト範囲をダイジェストできる。`--deterministic-id` の byte-parity は
`deterministic_id_qpdf_parity_tests` で既にゲート済み。

`QPDF_Stream::filterable` の filter factory lookup（`QPDF_Stream.cc:419-435`）は
`/DecodeParms` の読み取り（`:439-459`）より先に完了する。未知フィルタと長さ不整合を
組み合わせた場合も、`decode_filter_specs_from_object`、resolver 付き Object reader、
`decode_filter_specs_from_handle` の3経路がこの順序で同じ `unsupported stream filter`
エラーを返す。qpdf 11.9.0 の既存 fixture
`tests/fixtures/test_driver/stream_unsupported_filter_skips_decode_parms.pdf` と
`.out` を oracle/golden とし、`scripts/qpdf-test-driver-diff.sh --check` で51 fixture・
11 CLI probe の一致を確認する（`flpdf-vatj`）。

## 7. ドキュメント / オブジェクトヘルパー

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFAcroFormDocumentHelper.cc` | 1047 | `acroform_document_helper.rs`(217-590: `analyze` / `traverseField` 相当の live `ObjectHandle` association cache、direct Widget の orphan fallback、`invalidateCache`、`removeFormFields` の forward-map 起点 cache cleanup; 862-943: frozen-cache `addAndRenameFormFields`; 1229-1337: `getNeedAppearances` / `setNeedAppearances` / `generateAppearancesIfNeeded`) + `page_annotation_flatten.rs`(596-612: `/Fields` guard 後の Widget identity gate) + `page_object_helper.rs`(foreign `copy_annotations_from` が page `/Annots` を append した後の共有 AcroForm cache 無効化、`QPDFAcroFormDocumentHelper.hh:76-83` の手動 mutation 契約) + `acroform_document_helper.rs`(`disableDigitalSignatures` consumer) + `acroform_document_helper.rs`(`transformAnnotations`、`DrMap`、`/DA` の resource-name replacement consumer) + `overlay_appearance_stream.rs`(720: `adjustAppearanceStream`、AP stream consumer) | 🔀 canonical constructor now eagerly analyzes (`QPDFAcroFormDocumentHelper.cc:14-21`) and `Pdf` retains the live association cache across sequential helper facades, matching `QPDFJob::get_afdh_for_qpdf` (`QPDFJob.cc:1847-1856`); foreign transform keeps one per-annotation copy loop; stream kids propagate `stream objects cannot be cloned`; non-dictionary `/Parent` follows qpdf warning/return semantics (`QPDFFormFieldObjectHelper.cc:36-47`). The former survey/placement helpers were removed; canonical transform/copy remains in `acroform_document_helper.rs` and `page_object_helper.rs`. `page_annotation_flatten.rs`'s `remove_acroform` invalidates that shared cache after removing `/AcroForm`, matching `QPDFJob.cc:2141-2193`'s discipline that `flattenAnnotations` uses its own scope-local `QPDFAcroFormDocumentHelper` (discarded on return) rather than the `run()`-level shared `afdh`, so a later `flattenRotation` step's `make_afdh()` always re-analyzes the post-removal state instead of observing the pre-removal association. Foreign orphan-widget append follows the same invalidate-after-manual-page-mutation contract. |
`test_driver.cc:1551-1609`/`:1611-1629` の AcroForm consumerも、`get_form_fields`（terminal fieldのみを `field_to_annotations` の ObjectRef順で返す。direct orphanはqpdfの`QPDFObjGen(0,0)`に合わせてnull handleを1件返す）、`get_annotations_for_field`、`get_widget_annotations_for_page`、`get_field_for_annotation_handle` という同じlive cacheの公開handle経路へ接続した。`run_test_43` は qpdf のfield metadata・親chain・page Widget・appearance選択を、`run_test_44` は `setV` 相当のlive mutationとQDF writerをそれぞれ呼び出す。旧 `fields()` のraw `/Fields` preorderをconsumerで代用していない。 |
`qpdf/test_driver.cc:2761-2805` の test 80 は、`run_test_80` が `PageDocumentHelper::get_all_pages`、`AcroFormDocumentHelper::transform_annotations`、live `/Annots` append、`add_and_rename_form_fields`、foreign `PageObjectHelper::copy_annotations_from`、`PdfWriter` の QDF/static-ID 出力へ順に接続する。pinned qpdf 11.9.0 の fixture `flpdf-qtest/vendor/qpdf-qtest/qpdf/{appearances-1.pdf,appearances-1-rotated.pdf,minimal.pdf}` と golden `test80{a,b}{1,2}.pdf` に対し、stdout は `test 80 done\n`、stderr は空、exit は 0、a/b の4出力は byte-identical。foreign `/DR` の eager `copyForeignObject` と field clone 先行も qpdf の allocation order（`QPDFAcroFormDocumentHelper.cc:729-737,811-823,914-917`）に合わせ、QDF の `%% Original object ID` まで一致させる。
| `QPDFPageObjectHelper.cc` | 1039 | `page_object_helper.rs`(766) + `pages.rs`(98: inherited `/MediaBox`/`/CropBox`/`/Resources`/`/Rotate` lookup) + `page_form_xobject.rs`(637) + `resources.rs`(1229: `ResourceFinder` を使う resource pruning consumer) + `page_annotation_flatten.rs`(596-612: field-associated Widget のみ `/DR` を appearance resources に merge) + `job/overlay.rs`(2228: `placeFormXObject`) | 🔀 `pages.rs` の terminal chase は parsed qpdf child reference の意味ではなく、一時的な `Pdf::set_object` bare-reference bridge の互換境界だけをカバーする。qpdf の `QPDF::replaceObject` は indirect replacement を拒否する（`QPDF.cc:1986-1991`）ため、その bridge cycle の synthetic-null fallback を qpdf の null-as-absent inheritance と解釈しない。⚪ `resources.rs` の `form_xobjects_in_resources`/`remove_unreferenced_resources_in_form_xobjects` も同じ理由で `/XObject` category の Form 判定を `Pdf::resolve_to_terminal` で終端まで辿る（対応物なし、`optimization/inherited_attrs.rs` の同種補償と同じ形）。|
| `QPDFFormFieldObjectHelper.cc` | 852 | `form_field_object_helper.rs` + `form_field_object_helper/rendering.rs` + `default_appearance.rs`（field lookup/mutation と Tx/Ch appearance generation。`QPDFFormFieldObjectHelper.cc:472-478` に従い Btn appearance は production dispatch から除外）。既存 `/AP/N` は qpdf の `ValueSetter` 相当を同じ streamに登録する `AppearanceTokenFilter`（qpdf `QPDFFormFieldObjectHelper.cc:766-860`）で更新し、state dictionaryの`/AS`選択も`AnnotationObjectHelper`へ委譲する。新規APはqpdfどおり`/ProcSet`だけを初期Resourcesに置き、fontは既存AP `/Resources`→`/AcroForm /DR`で実際に見つかった場合だけ同じhandleを追加する（qpdf `:779-849`）；見つからないFont合成・`/FormType`追加は行わない。encodingはqpdf `QUtil`のASCII/WinAnsi/MacRomanを選ぶ。CLI の `generate_missing_appearances` は non-`/Btn` を `/AP/N` の有無で skip せず canonical helper へ渡す（qpdf `QPDFAcroFormDocumentHelper.cc:393-415`）。`crates/flpdf-cli/tests/cli_acroform_transforms.rs::generate_appearances_tx_reuses_existing_ap` は `/NeedAppearances true` の既存 stream を `--compress-streams=y` で再書き込み、`DecodeLevel::Generalized` 後の `/Tx BMC`/`Tf` と no-wrapper source preservation を確認する。qpdf 11.9.0 pinned source と `/usr/bin/qpdf` の live probe でも同じ入力の既存AP、無`/DR`、MacRoman入力を確認済み。token-filter primitive自体の変更は本 issueのscope外 | 🔀 |
| `QPDFPageDocumentHelper.cc` | 158 | `page_document_helper.rs`(`get_all_pages` + page mutation APIs) + `page_extract.rs`(`extract_pages`/`extract_page`) + `job/page_merge.rs`(`merge_documents`)。`job/overlay.rs` の source/destination page snapshot も `get_all_pages()` を通り、`QPDF_pages.cc:39-138` 相当の repair（欠落 `/MediaBox` の Letter fallback と warning）を Form 化・placement 前に適用する。両モジュールとも `Pdf::empty()` へ委譲（`emptyPDF()` + `addPage()` の library-level 経路、doc に明記） | 🔀 `emptyPDF()` 自体（`QPDF.cc:34-51,290-293`）の canonical 実装は `engine.rs`(475: `Pdf::empty()` は `open_mem_owned` へ委譲し、両者で `emptyPDF()` / `processMemoryFile()` 相当の construction path を担う)。⚪ qpdf の `emptyPDF()` は default-construct 済み `QPDF` を遅延初期化する `void` メンバー関数だが、flpdf の `Pdf` に「未初期化」状態が無いため static factory（`Result<Self>` を返す）に置き換えている。バイト列・parse 経路（`open_mem_owned` = `processMemoryFile` 相当）は同一。QPDFJob 相当のバージョン蓄積（`max_input_version`）は library level のこれらの関数ではなく `job/`（`flpdf-jq0z`）が担う想定 |
| `QPDFAnnotationObjectHelper.cc` | 226 | `annotation_object_helper.rs` + `page_annotation_flatten.rs` | 🔀 `page_annotation_flatten.rs` の `AppearanceTarget::Bridge`/`has_bare_reference_redirect` は flpdf の一時的な `Pdf::set_object` bare-reference bridge のみをカバーする代替経路で、parsed qpdf object は one-hop/live のまま `AnnotationObjectHelper` が qpdf の `getPageContentForAppearance`（`:78-226`）を忠実に実装する。同種の bridge パターンは `QPDFOutlineDocumentHelper` 行（本表 §7、`outline_document_helper.rs`）を参照。 |
| `QPDFOutlineDocumentHelper` / `QPDFOutlineObjectHelper` | 198 | `outline_document_helper.rs`(576) + `outline_object_helper.rs`(381) | ✅ live `ObjectHandle` route: `OutlineItem.object` retains canonical identity; `OutlineItem::get_title`/`get_count`/`get_dest`/`get_dest_page` (in `outline_object_helper.rs`, implementing `QPDFOutlineObjectHelper.cc` directly) recompute fresh from the live object on every call (no caching), matching qpdf's `getTitle`/`getCount`/`getDest`/`getDestPage` (`QPDFOutlineObjectHelper.cc:47-98`), while `parent`/`kids` are captured once at construction, matching qpdf's cached `getParent`/`getKids`. `/Dest` and `/A /GoTo /D` use qpdf-shaped handle accessors; the name/string branch delegates to `OutlineDocumentHelper::resolve_named_dest` (in `outline_document_helper.rs`, implementing `resolveNamedDest`), which uses the handle-native `NameTree`, cached per session in `OutlineDocumentHelper::dest_dict`/`names_dest` (`QPDFOutlineDocumentHelper.cc:60-90`) — the same split as qpdf's `getDest()` calling `m->dh.resolveNamedDest()`; JSON consumes the handles directly. `OutlineItem` holds no `&mut Pdf<R>` (an arena entry, not a live qpdf-style object helper), so its accessors take `helper: &mut OutlineDocumentHelper<'_, R>` in place of qpdf's `QPDFOutlineObjectHelper::m->dh` reference; tree construction (`get_tree`/`build_item`) stays on `OutlineDocumentHelper` since it needs sequential `&mut Pdf<R>` access across both qpdf constructors (document-level top-level walk and per-node recursive constructor), which the arena flattens into one pass — `OutlineTree::get_outlines_for_page`'s `by_page` cache stays on the arena-lifetime `OutlineTree` rather than moving to `OutlineDocumentHelper::initialize_by_page`, since `Pdf::outline()` mints a fresh `OutlineDocumentHelper` per call and a cache there would never hit. The narrow terminal-handle chase only covers flpdf's temporary `Pdf::set_object` bare-reference bridge; parsed qpdf objects stay one-hop/live. |
| `QPDFPageLabelDocumentHelper.cc` | 134 | `page_label_document_helper.rs`(1037) + `nntree.rs` (`NumberTree`) | ✅ canonical ObjectHandle route for `hasPageLabels`, `getLabelForPage`, `getLabelsForPageRange`, and `pageLabelDict`; typed page-operation adapters and JSON migration remain downstream |
| `QPDFNameTreeObjectHelper` / `QPDFNumberTreeObjectHelper` / `NNTree.cc` | 1394 (`34-75,106-168,216-390,391-520,560-700`) | `nntree.rs`（shared canonical `ObjectHandle` engine + handle-native public `NameTree`/`NameTreeCursor` and `NumberTree`/`NumberTreeCursor`）+ consumer adapters。qpdf の live `QPDFObjectHandle`/`QPDF_Array` mutation（`NNTree.cc:34-75` の iterator value 更新、`:106-168` の limits、`:216-390` の split/insert、`:391-520` の remove/deepen、`:560-700` の find）に対応し、`ResolvedArray` は `ObjectHandle::set_array_items` で alias を保持したまま更新、direct kid の indirect 化は `Pdf::make_indirect_from_object_handle`、root split は既存 root slot を維持する。dirty propagation で canonical mutation を観測する。public NameTree/NumberTree helpers now keep root・key/value・cursor mutation on live handles; the shared engine is entirely handle-native; no raw Object fixture, projection, or bare-reference compatibility route remains | 🔀 |
| `QPDFEmbeddedFileDocumentHelper.cc` | 122 | `embedded_files.rs`(678) | ✅ D1 完成（`flpdf-jzy7`）: `has_embedded_files`/`get_embedded_files`/`get_embedded_file`/`replace_embedded_file`/`remove_embedded_file` が `QPDFEmbeddedFileDocumentHelper.hh` の公開 API と 1:1 対応。モジュール doc の自己申告も更新済み。D2 は未達のまま — `job/json_sections.rs` の `build_attachments_section` はこのヘルパーを経由せず `NameTree` を直接歩く（`flpdf-q2fo` で解消予定） |
| `QPDFFileSpecObjectHelper` / `QPDFEFStreamObjectHelper` | 280 | `filespec_helper/filespec.rs` + `filespec_helper/embedded_file_stream.rs` + `filespec_helper/shared.rs` | ✅ D1 完成（`flpdf-d9sq`）。2026-08-23 の `flpdf-3yn9.34` で qpdf の2 helper責務へ物理分割し、high-level attachment file I/O は `job/attachments.rs` に移設した。FileSpec/EFの読み書き・stream decodeはcanonical `ObjectHandle`とprovider pathを維持する。D2 は未達のまま — `job/json_sections.rs::filespec_dict_to_json` が `FileSpec`/`EmbeddedFileStream` を経由せず同じ Mac/DOS 優先順位ロジックを再実装している（`flpdf-q2fo` で解消予定）。旧 `copy_attachments_from`（`copyForeignObject` 以前の独自 `sanitize_imported_object` walk）は `flpdf-s5cw.7` で `QPDFJob::copy_attachments`（`job/attachments.rs`）へ置き換えられ削除済み |
| `ResourceFinder.cc` | 56 | `resource_finder.rs`（operator/name tracking、qpdf `getNames()` 相当のカテゴリ横断 flat set、resource type/offset 集約）。production consumer は `resource_replacer.rs` と `resources.rs` の resource pruning | ✅ |
| `QPDFAcroFormDocumentHelper.cc` anonymous `ResourceReplacer` | — | `resource_replacer.rs`（`ResourceFinder` の name offsets を exact-byte 置換）。production consumer は `acroform_document_helper.rs` の `/DA` と `overlay_appearance_stream.rs` の AP streams | ✅ |
| `QPDFDocumentHelper.cc` / `QPDFObjectHelper.cc` | 12 | 基底トレイトが無い | ⚪ |

`qpdf/test_driver.cc:2073-2137` の `test_56`–`test_59` と `:2303-2364` の
`test_64`–`test_67` は、`PageObjectHelper::get_form_xobject_for_page`、
`Pdf::copy_foreign_object`、`PageObjectHelper::get_resources`/
`ObjectHandle::merge_resources`、`PageObjectHelper::place_form_xobject`、
`PageObjectHelper::add_page_contents`、`PdfWriter` のQDF/static-ID経路を通る
qtest consumerとして実装済みである。pinned qpdf 11.9.0との同一fixture比較で8件の
`a.pdf`出力が一致し、対応するqtest比較行は `form-xobject 4,6,8,10,12,14,16,18`
へ昇格した。driver側にForm XObjectの独自traversal・allocation・compatibility bridgeは
追加していない。両関数とも `PdfWriter::write()` 実行後に診断drain呼び出しを追加した
（write中に到達する未resolveオブジェクトが新規repair diagnosticを生む可能性があり、
qpdfのwarn()コールバックはwrite()実行中も同期的に出力するため）。`test_64_67_body`は
さらに主文書側の診断drainをループ末尾からループ内（各ページの`add_page_contents`直後）
へ移し、`test_56_59_body`と同じper-iteration順序に揃えた。

## 8. JSON

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `JSON.cc` | 1401 | `json/`（全 write helper、blob callback、unparse が public `Pipeline` 境界を使用。serializer は caller-owned outer pipeline を finish しない） | ✅ |
| `JSONHandler.cc` | 189 | `json/` | ✅ |
| `QPDF_json.cc` 入力側（`QPDF_json.cc:1-833`: `JSONReactor` / `createFromJSON` / `updateFromJSON` / `importJSON` / `test_json_validators`） | 833 | `json/input.rs`（reactor・validators・provider・value factory） + `json/document.rs`（rootless seed・create/update/import 境界） + `tests/json_document_tests.rs`（flpdf-authored fixture と qpdf 11.9.0 differential） | ✅ `.15.4` で入力境界を実装。create は `QPDF_json.cc:54-63` の rootless seed、update は omitted object を保持し、parser/semantic error の境界と update page flags を qpdf どおりに分離する。⚪ (B) `validate_pdf_version` は `QPDF::validatePDFVersion`（`QPDF.cc:366-384`）の byte-slice 置換で、`QPDF_json.cc:503-518` の全入力消費条件を保持する。⚪ (B) `JsonDescription` は `QPDFValue::Description` の共有 mutation（`QPDF_json.cc:721-730`）を per-handle Rust value で置換するが、input/object/offset の観測契約は不変 |
| `QPDF_json.cc` 出力側（`QPDF_json.cc:834-946`: free function `writeJSONStreamFile`(834-849) + `QPDF::writeJSON` ×2 overload(851-946)） | 113 | `document_json.rs`(361: `write_json` = 6 引数 overload(851-861)、`write_json_key` = `complete`/`first_key` overload(863-946)、`write_json_stream_file` = `writeJSONStreamFile`。side file は `PlStdioFile` explicit finish) | ✅ 入出力とも qpdf の別責務境界に対応。`qpdf --json-output=2` は complete overload と同一バイトを書くため、`crates/flpdf/tests/document_json_tests.rs` が 7 fixture で qpdf 出力と直接照合する |
| `QPDFObjectHandle::getJSON` / `QPDFObjectHandle::writeJSON`（行数は §1 の `QPDFObjectHandle.cc` に計上済み。ここは所在の相互参照） | — | `object_handle.rs` の `ObjectHandle::get_json` / `ObjectHandle::write_json`（`QPDFObjectHandle.cc:1613-1647` の外側 dispatch と `qpdf/JSON_writer.hh:16-135` の pipeline 境界）、`json_inspect.rs` の `pdf_object_to_json`（getJSON false の consumer） | 🔀 canonical ObjectHandle writer は移送済み。`false` は間接 identity を先に検査して `"N G R"` を出力し、array/dictionary child は非再帰の reference dispatch、stream は `QPDF_Stream::writeJSON` と同じく dictionary のみを出力する。`true` の一段解決 primitive も writer に実装済みで、document-level `QPDF::writeJSON` の object-map は `flpdf-25kg.3.37` で cutover 済み。`.3` では `json_inspect.rs::qpdf_resolve_top_level_object` と historical stream payload が canonical handle を直接返す。`ordered_qpdf_*` は本番 bridge ではなく、既存の pipeline-write 境界テスト専用で保持する |
| `QPDF_Stream::writeStreamJSON`（行数は §1 の `QPDF_Stream.cc` に計上済み。ここは所在の相互参照） | — | `object_handle.rs` の `ObjectHandle::write_stream_json`（`QPDF_Stream.cc:207-295` の mode validation、`no_data_key`、二重試行、dict normalization、payload routing、effective decode level） + `document_json.rs` の object-map framing / side-file ownership。`json_inspect.rs` の `stream_payload_with_decode_status` は既存の公開 raw-payload helper とテスト oracle に限定 | ✅ `flpdf-3yn9.9` で qpdf の 1 関数責務へ統合。旧 `Object/Stream` payload/dict bridge は本番経路から外し、`QPDF_json.cc:917-925` 相当の consumer は canonical handle を呼ぶ。非 file entry は既存 flpdf の変換失敗時接頭辞を保つため canonical 結果を先に buffer 化する |

`qpdf/test_driver.cc:3162-3185` の test 89/90 は、qpdf JSON入力境界の下流consumerとして
`Pdf::create_from_json_with_options` / `Pdf::update_from_json` とlive ObjectHandle mutationへ
接続済みである。test 89はfilenameをPDFとして開かずrootless JSON documentを作成し、test 90は
通常PDFへpartial updateを適用する。各type-mismatch warningはqpdfの発生順にdrainされ、
qpdf-json比較行111/112の同一run結果を `harness.log` と `qtest-results.xml` の両方で確認する。
両テストとも file-open 自体はドライバ境界で行い（`crt_open_error_message`/`open_error_bytes`
経由でqpdfの`QUtil::safe_fopen`/`QPDFSystemError`相当のCRTテキストへ翻訳、
`QUtil.cc:490-518`/`QPDFSystemError.cc:12-28`）、`Pdf`側のsource-basedオーバーロードへ
既にopenした`File`を渡す。`create_from_json_with_options`は`import_json`失敗時に
`pdf.repair_diagnostics()`を`Error::with_open_diagnostics`で終端エラーへ付帯する
（既存の`load_xref_and_trailer_with_repair`と同じ`Error::OpenFailure`パターン）。
test 90はupdate失敗時（`import_json`はupdateでは`&mut self`のPDFを保持したまま返す）に
先に診断をdrainしてから終端エラーを伝播し、最終`/Root`変異は`root_handle`ローカルヘルパー
ではなく`Pdf::root_handle()`（qpdfの`getRoot()`のdictionary検証を保持、`QPDF.cc:2355-2368`）
を経由する。

`qpdf/test_driver.cc:2864-2882` の test 83 は、qpdf の `QPDFJob::initializeFromJson` を
完全初期化（`partial=false`）で呼び出すconsumerとして `test_80_87.rs::run_test_83` に
接続した。driverは`arg2`をbyte readしてから`calling initializeFromJson`を出力し、既存の
`job/lifecycle.rs::QPDFJob::initialize_from_json`へ委譲する。`Error::Usage`はqpdfの
`usage:`、その他のエラーは`exception:`としてstderrへ流し、主入力PDFのopenを行わない
dispatchも維持する。`job-partial.json`の実機出力は`usage: an input file name is required`
となる（`QPDFJob.cc:567-637`, `QPDFJob_config.cc:774-784`）。test 84のfluent Config/API
surfaceはこのconsumerの範囲外で、別sliceに残す。

`qpdf/test_driver.cc:2884-2971` の test 84 は、`QPDFJob::Config` の fluent setter、
`checkConfiguration`/`run`、custom progress reporter、private loggerへの
`setOutputStreams`を5つのscenarioで検証する。`job/lifecycle.rs` はこれらを
`QPDFJobConfig`のborrowed proxy、既存の`QPDFJob::run`/`check_configuration`、
`register_progress_reporter`、および`QPDFLogger::set_output_streams`へ接続し、driverは
qpdfのシナリオ順とcapture出力を保つ。qpdf 11.9.0の`test_driver 84 -`とRust driverは
`filter-progress.pl`適用後のstdout/stderrが一致し、`a.pdf`も同じwriter設定で生成される。
Clapのcommand/parse stack guardはPR #1409の`stacker::maybe_grow`が所有し、このJob API
sliceでは再実装しない。

### `flpdf-25kg.3.37` bounded consumer cutover (2026-08-15)

`document_json.rs` の object-map enumeration は `Pdf::get_all_objects()` に、通常の
`"value"` entry と trailer は `ObjectHandle::write_json(2, ..., true, depth)` に切り替えた。
したがって `QPDFObjectHandle.cc:1613-1647` の outer-only dereference と、
`QPDF_Array.cc:153-187` / `QPDF_Dictionary.cc:72-95` の nested indirect identity を同じ
canonical writerで通る。`QPDF_Stream::writeStreamJSON` の payload/datafile、decode retry、
historical stream view はこのPRの責務外であり、stream entryだけは
`flpdf-3yn9.9` の後続cutoverへ残す。

Oracle probe:

```text
qpdf --json=2 --json-key=qpdf --json-stream-data=none \
  tests/fixtures/compat/qdf-contents-ref-array.pdf -
```

qpdf 11.9.0 の object 5 は `{"value": ["6 0 R", "7 0 R"]}` を返す。flpdf は同じ
fixtureを `crates/flpdf/tests/document_json_tests.rs` の byte differential に追加し、
`cargo test -p flpdf --test document_json_tests --quiet` で照合する。outline destination
については、未解決 outer handleを同じfixtureから取得して
`pdf_dest_to_json`へ渡し、`["6 0 R", "7 0 R"]` を確認する。reserved handleはqpdfの
true-mode dispatchどおり `QPDFObjectHandle: attempting to get JSON from a reserved object`
で失敗する。

### `flpdf-3yn9.9` bounded stream consumer cutover (2026-08-15)

`QPDF_Stream::writeStreamJSON` (`libqpdf/QPDF_Stream.cc:207-295`) に対応する
`ObjectHandle::write_stream_json` は、`None` / `Inline` / `File` の引数検証、
inline の `no_data_key`、`pipeStreamData` の最大二回試行と raw fallback、
`/Length`・成功した decode 時の `/Filter`/`/DecodeParms` 除去、`data`/
`datafile`/`dict` の出力、実効 `DecodeLevel` の返却を一つの責務として持つ。
ストリーム source は `ObjectHandle::pipe_stream_data` (`object_handle.rs`:
4393-) を通り、辞書の shallow copy は `ObjectHandle::shallow_copy` と
`remove_key` を使う。

`document_json.rs` は `QPDF_json.cc:917-925` 相当の object-map framing と、
`writeJSONStreamFile` (`QPDF_json.cc:834-849`) 相当の side-file 作成・明示 finish
だけを所有する。非 file の stream value は canonical writer の完成結果を
`Buffer` に受けてから object key を書くため、変換失敗時の既存 sink prefix を
維持する。旧 `json_inspect.rs` の split payload/dict writer は本番 consumer から
除去し、Ordered JSON writer は pipeline 境界の test-only oracle として限定した。

確認済みの qpdf 11.9.0 差分:

- Flate stream の inline/file 出力は decoded payload と正規化後 dictionary を一致。
- 未対応 filter は qpdf と同じく二回目の raw payload に落ち、`/Filter` を保持。
- inline `no_data_key` は payload を discard しつつ effective decode level を保持。
- pipeline / filename の不正組み合わせは qpdf の `writeStreamJSON` 文言で拒否。

主な検証:

```text
cargo test -p flpdf --lib object_json_writer_tests --quiet
cargo test -p flpdf --test document_json_tests --quiet
cargo test -p flpdf --lib json_inspect::tests::side_file --quiet
cargo test -p flpdf-cli --test cli_json --quiet
```

破損した遅延オブジェクトについても、qpdfの `QPDF::resolve` が診断をwarningへ送り、
対象をnullへフォールバックしてJSON本体を完了する挙動を採用する。CLIの
`lazy_object_failure_matches_qpdf_null_fallback` は、qpdf 11.9.0のstdoutとflpdfのstdoutを
直接比較し、非ゼロ終了も確認する。

## 9. Job / CLI

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFJob.cc` | 3116 | `flpdf-cli/src/main.rs`(6796) + `job/lifecycle.rs`（JSON create/update/write、ordinary open、ordinary page inspection、JSON inspection と共有 completion、progress logger fallback、`QPDFJob.cc:429-516,843-875,1646-1714,2926-2935`）+ `job/json.rs`（`QPDFJob::writeJSON` の出力選択と `doJSON` 固定順、`QPDFJob.cc:1545-1640,3094-3115`）+ `job/json_sections.rs`（`doJSONPages` / `doJSONPageLabels` / `doJSONOutlines` / `doJSONAcroform` / `doJSONAttachments` / `doJSONEncrypt`、`QPDFJob.cc:1030-1330`） + `job/attachments.rs`（`doListAttachments` / `doShowAttachment` の info/save と completion（`QPDFJob.cc:876-927`）、`addAttachments` の provider-backed 追加（`QPDFJob.cc:2046-2087`）、および `copyAttachments` の cross-document 添付コピー（`QPDFJob.cc:2089-2135`）） + `job/lifecycle.rs`（`QPDFJob::parse_collate` の `QPDFJob::Config::collate` vector parser、`QPDFJob_config.cc:95-125`、`QUtil::string_to_ull` の `QUtil.cc:396-425`） + `job/page_specs.rs`（`handlePageSpecs` のspec解決・collate・source lifecycle・最終順序、`QPDFJob.cc:2360-2632`、single-document の post-subset AcroForm field pruning インライン処理（`QPDFJob.cc:2610-2632`、qpdf側に個別関数名は無く flpdf 側の命名 `prune_acroform_after_subset`）を含む） + `job/overlay.rs`（`handleUnderOverlay` の source/destination 全 page 取得と修復前置、`QPDFJob.cc:1937-2015`） + `job/page_merge.rs`(1117) + `job/rotate_spec.rs`（`--rotate` スペック解析、qpdf側の private `parseRotationParameter` に対応） + `job/rotate.rs`（range適用と page-helper facade） + `job/page_range.rs`（ページ範囲 mini-language、qpdf側の private `QPDFJob::parseNumrange` および public `QUtil::parse_numrange` に対応。`PageRange::parse`+`resolve` の2段階分割はflpdf独自で、qpdf自体は1関数でparse+resolveを行う） + `job/page_combine.rs` + `job/page_plan.rs`（`handlePageSpecs` の multi-input combination / single-document selection planning への分解） + `job/attachment_list.rs`（EmbeddedFiles/FileSpec/EF の traversal・metadata projection） + `job/acroform_field_prune.rs`（job boundary が委譲する canonical field-tree walk） + page 操作群 | 🔀 `job/lifecycle.rs` はJSON create/update/write、JSON read-only inspection、ordinary page-count/page-list inspectionのcanonical boundaryを移植済み。`job/attachments.rs` は添付 inspection の info/save と共有 completion、`--add-attachment` の provider-backed 追加、および `--copy-attachments-from` の cross-document コピー（`copyForeignObject` 経由、重複キーの集約 throw を含む）を移植済み。`job/page_specs.rs` はordinary multi-source `--pages` のjob boundaryを移植し、foreign copy・AcroForm field collision・PageLabels・collate orderを `job/page_merge.rs`/page helperへ接続した。single-document の post-subset AcroForm field pruning インライン処理も同じ job 層へ接続した。argv/config、通常rewrite、`--remove-attachment` orchestration、linearizationの残りconsumerは後続sliceで集約する。`showPages`/`withImages` は `job/inspection.rs` のcanonical `doShowPages` output（page identity、optional image details、content references）へ接続した。`job/page_range.rs`/`job/page_combine.rs`/`job/page_plan.rs` は2026-08-21に `flpdf-tvda` の再検証（誤検知だった非job消費者の主張を撤回）を経てjob/へ移動した。 |
| `QPDFJob.cc` `createQPDF` / `doInspection` + `QPDFJob_config.cc` `jsonInput` / `updateFromJson` / `jobJsonFile` | `459-516,1646-1714; 305-309,328-332,774-784` | `job/lifecycle.rs` のJSON create/update/open/inspect/partial-init（`flpdf-25kg.5.2.1/.2`）+ `flpdf-cli/src/main.rs` の `run_json_input_inspection`、`job/check.rs::QPDFJob::check` + retained `open_job_pdf` for other routes | 🔀 `--json-input` / `--update-from-json` のJSON outputとread-only `--show-npages`/`--show-pages`はQPDFJobの一つのdocument/logger lifecycleへ移行済み。`--job-json-file` は qpdf の partial initialize 境界を `initialize_from_json_partial` で保持し、missing-output の最終診断を run/checkConfiguration 側へ委譲する。`--check`は専用のqpdf-shaped report rendererを保ち、generic summaryの二重出力を避ける。通常rewrite・rotate・page-tree選択・その他inspectionは後続Job sliceで同じ状態へ接続する。JSON主入力の `--pages` は一時PDFを経由せず、同じ文書のObjectHandle/xrefを `QPDFJob::handle_page_specs` で計画化する。qpdf 11.9.0のupdate-before-inspection順序を `cli_json_input.rs` で固定する。 |
| `QPDFJob::Config::showEncryption` / `QPDFJob::showEncryption` | `QPDFJob_config.cc:551-555`; `QPDFJob.cc:442-445,700-742,1646-1658` | `job/lifecycle.rs::open_for_encryption_inspection` + `flpdf-cli/src/main.rs::run_show_encryption` + `job/check.rs::QPDFJob::show_encryption`（top-level `--show-encryption` と native subcommandが共有） + `encryption/state.rs::EncryptionInspectionState` | 🔀 qpdfの認証前parsed encryption stateを保持し、wrong-passwordでもR/P/password/match/permission/method reportを完了する。暗号化されたdocumentのdecryption state (`EncryptionState`) は認証成功時だけ有効にし、qpdfの `User password` recovery は V<5 の owner-password pathだけで行う。 |
| `QPDFJob_config` / `_argv` / `_json` / `QPDFArgParser` | 3164 | clap で代替 | ⚪。QPDFJobの使用エラー分類は [`UsageError`](../crates/flpdf/src/error.rs) + `Error::Usage` として job lifecycle から CLI の `usage_exit` へ伝播し、`QPDFUsage` の別catch経路（`qpdf/qpdf.cc:10-23,34-39`）を再現する。 |
| `QPDFLogger.cc` | 255 | `logger.rs`（private stdout tracker、shared info/warn/error/save routes、standard stdout/stderr/discard、reset/following、save collision、custom sink ownership）+ `reader/resolver.rs` / `reader.rs`（文書 warning の append-then-route、suppression、live logger replacement）+ `flpdf-cli/src/main.rs`（下記 qpdf-equivalent consumers） | ✅ `QPDFLogger.cc:9-40,43-51,80-254`。`diagnostics.rs` は logger ではなく collection-only value store として維持する |

`QPDFJob::Config::keepFilesOpen` / `keepFilesOpenThreshold` は `job/lifecycle.rs` の job configuration と `job/page_specs.rs::QPDFJob::handle_page_specs` に接続した。未指定時は qpdf の `page_specs` 上の異なる source index 数を閾値（既定200）と比較し、明示 y/n はその値を優先する。CLIとjob JSONのpage-spec callerは全specのsource identity/policyを先に確定し、各secondary sourceのparse直後・次sourceを開く前に `Pdf::set_input_source_stay_open(false)` を適用する。primaryはqpdfと同じくkeep-openのまま保持する。file source は `Pdf::open_file_with_options` の reopenable readerを使い、qpdfの `ClosedFileInputSource::before`/`after` 相当で secondary source を close/reopen する（`QPDFJob_config.cc:342-353`, `QPDFJob.cc:2374-2427`, `ClosedFileInputSource.cc:18-35,97-104`）。

`--job-json-file` の page-transform fields `splitPages`、`rotate`、`removeRestrictions` は、qpdf の生成 JSON handler (`QPDFJob_json.cc:611-624`, `auto_job_json_init.hh`) と Config/Job call order (`QPDFJob_config.cc:535-540,597-609`; `QPDFJob.cc:369-411,428-520,2137-2150,2635-2651,2940-3025`) に対応して `job/lifecycle.rs` の canonical configuration から page split、rotation、security/signature mutation へ接続した。 |

`splitPages` の値は qpdf の `int` と同じく signed のまま job configuration に保持する。したがって負の非ゼロ値は `checkConfiguration` の truthy split branch を通過し、`doSplitPages` の `QIntC::to_size(m->split_pages)` (`QPDFJob.cc:2970`, `QIntC.hh:112-216`) で初めて `integer out of range converting ...` を返す。flpdf も同じ page-split boundary まで値を保持し、parser の独自 early usage error に変換しない（`QPDFJob_config.cc:597-609`, `QPDFJob.cc:567-631`; `flpdf-sp4g`）。 |

`coalesceContents` も生成 handler (`auto_job_json_init.hh:311-313`)、Config (`QPDFJob_config.cc:88-91`)、変換順序 (`QPDFJob.cc:2185-2188`) に対応し、既存の provider-backed `ObjectHandle::coalesce_content_streams` を `job/lifecycle.rs` から呼ぶ。

`flattenRotation` も生成 handler (`auto_job_json_init.hh:377-382`)、Config (`QPDFJob_config.cc:204-207`)、変換順序 (`QPDFJob.cc:2190-2194`) に対応し、既存の `flatten_rotation_on_pages` (`QPDFPageObjectHelper.cc:862-991`) を `job/lifecycle.rs` から呼ぶ。`coalesceContents` の直後に配置して、qpdfのページ変換順序を保つ。

`generateAppearances` も生成 handler (`auto_job_json_init.hh:383-385`)、Config (`QPDFJob_config.cc:218-221`)、変換順序 (`QPDFJob.cc:2177-2180`) に対応し、既存の `AcroFormDocumentHelper::generate_appearances_if_needed` (`QPDFAcroFormDocumentHelper.cc:393-417`) を `job/lifecycle.rs` から `coalesceContents` の前に呼ぶ。

`checkLinearization` も生成 handler (`auto_job_json_init.hh:217-219`)、Config (`QPDFJob_config.cc:80-85`)、inspection順序 (`QPDFJob.cc:1646-1666`) に対応し、既存の `QPDFJob::check_linearization` を job JSON の `checkLinearization` option から呼ぶ。Config と同じく output file を要求しない inspection-only route とし、linearized check の warning/status は共有 completion へ渡す。

`flpdf-egzr.8.10` では、残る generated job-JSON handlers も同じ lifecycleへ接続した。`showLinearization`、`showXref`、`showObject`、`filteredStreamData`、`rawStreamData`、`listAttachments`、`showAttachment` は `QPDFJob::doInspection` の順序 (`QPDFJob.cc:1646-1689`) で既存の inspection/attachment primitiveへ委譲する。`copyEncryption` / `encryptionFilePassword` は認証済み donor の `writer_copy_encryption_source` を `PdfWriter`へ渡し、`compressionLevel` は `QPDFJob.cc:2847-2851` と同じ writer 開始境界で適用する。`passwordMode`、`passwordIsHexKey`、`ignoreXrefStreams`、`suppressPasswordRecovery`、`suppressRecovery` は全 job-owned input source の `PdfOpenOptions`へ伝播し、`allowInsecure` は256-bit encryptionの nested handlerで検査する。`isEncrypted` / `requiresPassword` は `QPDFJob.cc:535-557` の0/2/3を job statusへ写像し、`reportMemoryUsage` は `QUtil::get_max_memory_usage` (`QUtil.cc:1941-2002`) 相当を completion 後に stderrへ出力する。`jobJsonFile` は partial initialize 境界を保った再帰展開とし、include cycleを拒否する。

`testJsonSchema` の schema 不一致は、qpdf の `doJSON` (`QPDFJob.cc:1631-1642`) と同じく、生成済みJSONを出力へ流し終えた後に固定ヘッダーと各エラーを `QPDFLogger` の error pipeline へ書き出し、ジョブを失敗させずに戻る。flpdfの `job/json.rs::validate_json_schema` はこの責務を `QPDFJob` の logger から受け取り、JSON parse / 出力 pipeline の実障害だけをエラーとして返す。

`showNpages` も生成 handler (`auto_job_json_init.hh:235-237`)、Config (`QPDFJob_config.cc:573-579`)、inspection順序 (`QPDFJob.cc:1646-1655`) に対応し、既存の `QPDFJob::show_npages` を job JSON の `showNpages` option から呼ぶ。`/Pages /Count` はページツリーを再走査せず、qpdfと同じ generic accessor のwarning/zero fallbackを通し、`check`・`showEncryption`・`checkLinearization` との複合時も `doInspection` の順序で一度だけ completion する。残る schema-valid option の未接続責務は別の bounded Job JSON slices で扱う。

`showPages` は生成 handler (`auto_job_json_init.hh:241-243`)、Config (`QPDFJob_config.cc:581-587`)、`doShowPages` (`QPDFJob.cc:842-874`) を同じ `job/inspection.rs` のcanonical routeへ接続した。各ページの `page N: obj gen R`、`content:` と `getPageContents()` のstream referenceを出力し、`withImages` (`auto_job_json_init.hh:247-249`, `QPDFJob_config.cc:654-658`) 指定時だけ `images:` のname/reference/width/heightをqpdf順で追加する。`showPages` はoutput fileを要求せず、`withImages`単独は要求を解除しない。top-level `--show-pages` も同じrouteを使い、旧来のeffective page-attribute formatterは使用しない。

`showNpages` の追補では、`QPDFJob::checkConfiguration` が JSON の暗黙 stdout を設定した後に inspection-only output conflict を検査する順序 (`QPDFJob.cc:582-595`) を保持し、`showNpages` と `json`/`jsonOutput` の併用を拒否する。bare job-JSON の非文字列値は生成 `JSONHandler` の `value at <path> is not of expected type` (`QPDFJob_json.cc:124-135`, `JSONHandler.cc:127-188`) を使用する。`check` は `JobSetter::setCheckMode` (`QPDFJob.cc:745-752`) を通して `QPDF::getRoot` の Catalog `/Type` warning・修復 (`QPDF.cc:2354-2366`) を有効にし、job logger による live warning を診断再配送と二重化しない。

`flpdf-25kg.5.3` では、qpdf 11.9.0 の `QPDF::warn` が warning を collection に追加してから loggerへ同期配送し、配送例外をcatchしない境界 (`QPDF.cc:487-504`) を、post-openの `check`／linearization／page-content検査へ適用した。check側は各lazy consumerの直前と直後のdiagnostic増加を使って、`Error::System`/`Error::Internal` がwarning sink由来の場合だけ `Operation` として返し、rootなし・壊れたPDFの構造errorは通常のreportへ集約する。これにより `getExtensionLevel` 相当の `/Extensions /ADBE /ExtensionLevel` accessorを最終diagnostic snapshotより前に実行し、late warningを順序どおり一度だけ収集してexit 3へ反映する (`QPDFJob.cc:744-803`)。`NNTree` は構造的な `QPDFExc` 相当だけをrepair warningへ変換し、node解決・`deepen`・iteratorのlogger failureはそのまま伝播する (`NNTree.cc:585-663,819-899`)。writerも `QPDFWriter::write` のroot/Catalog/extension preflightにcatchを置かず (`QPDFWriter.cc:2034-2056,2059-2184`)、full-rewrite／linearizedのCatalog snapshotとextension-level解決でlogger failureをmetadata不在やdefault levelへ変換しない。

`optimizeImages` は `QPDFJob.cc:2151-2174` の変換順序に合わせ、inline image の外部化を先に行ったうえで、`PageObjectHelper::for_each_image(true)` が返す page/Form XObject を `job/image_optimization.rs` で走査する。`Pl_DCT.cc:249-295` 相当の JPEG 圧縮結果が元 `/Length` より短い場合だけ、元辞書を shallow-copy した新 stream に `/Filter /DCTDecode` と null `/DecodeParms` を設定し、provider として遅延登録する。qpdf 11.9.0 `image-optimization.test` の24行および最適化JPEG raw bytesを照合済みである。job JSON の生成 handler (`auto_job_json_init.hh:317-322,386-400`) と Config (`QPDFJob_config.cc:176-180,232-235,357-360,422-447`) は `job/lifecycle.rs` の `JobConfiguration` に接続し、`iiMinBytes` / `oiMin*` は qpdf の `QUtil::string_to_uint` と同じ unsigned-prefix parser を通す。`externalizeInlineImages` と `optimizeImages` を併用した場合は、明示 externalize が `keepInlineImages` より優先する qpdf の条件を保ったまま canonical image phase を一度だけ実行する。
top-level `--flatten-annotations=all|screen|print` も `auto_job_init.hh:117` / `QPDFJob_config.cc:190-200` の choices を `flpdf-cli` の shared `run_rewrite` route に接続し、通常 rewrite と linearize rewrite の両方で `PageDocumentHelper::flatten_annotations` (`QPDFPageDocumentHelper.cc:55-77`) を実行する。`NeedAppearances` 時の `warnIfPossible` と stream filter warning の parsed-offset/suppression 境界も qpdf の warning/status contract に合わせる。

`QPDF::initializeEncryption` (`QPDF_encryption.cc:718-751`) は、`/ID` が無い、配列でない、
要素数が2でない、または第1要素が文字列でない場合に `invalid /ID in trailer dictionary` を
warning として記録し、空の `id1` で暗号鍵導出を継続する。`flpdf-ez48` で
`first_file_id_handle` はこの値と validity を分離し、reader の open-time warning sink が
一度だけ同じ非致命経路へ送る。正確に2要素で第1要素が文字列なら、空文字列も有効値として
警告しない。

`flpdf-5lsj` では、`QPDF::readTrailer` が `InputSource::getLastOffset()` を保持したまま
`initializeEncryption` の `damagedPDF("trailer", message)` に渡す責務
（`QPDF.cc:1313-1327,2625-2628`）も移植した。初期xref/trailer読込をbyte snapshotで
行うflpdfでは、その論理`startxref`をresolverの共有入力sourceへseedし、
`push_trailer_warning_at` が `(trailer, offset N): ...` を診断とloggerへ一度だけ渡す。
pinned qpdf 11.9.0 と `/usr/bin/qpdf` の malformed `/ID` probe（offset 416）で一致する。

`flpdf-25kg.5.4` では、qpdf の `QPDF::resolve` / `resolveObjectsInStream` が構造・member
warningを配送してから `updateCache`/member valueを確定する順序
（`QPDF.cc:1560-1833,1700-1753`）を維持する。暗号の unknown `/StrF`・`/StmF` fallbackも
`QPDF_encryption.cc:976-1005,1041-1154` と同じくwarning成功後に `cf_string`/`cf_stream` を
`AES`へ書き換え、R6 `/Perms` warningは認証済み`EncryptionState`のreader commitより先に
配送する。warning sink failureではcache/stateを未commitのままcallerへ返し、正常sinkでの
retry時だけfallbackを一度確定する。

暗号の責務境界は `QPDFJob.cc:2753-2761` の `setEncryptionOptions` が新規RC4書き込みだけを
`allow_weak_crypto` で拒否する形であり、既存のRC4/R=5入力を読む経路にはこの拒否がない。
flpdfも `PdfOpenOptions` のread-side opt-in/error gateを撤去し、`--allow-weak-crypto` は
`parse_encrypt_segment` のwriter policyに限定する。これは既存挙動維持の例外ではなく、qpdf
11.9.0のread/write responsibilityへの収束である。

`QPDFJob::writeQPDF`（`QPDFJob.cc:484-503`）は、出力またはinspectionを完了した後に
文書のopen-time/lazy warningを集約し、`createsOutput()`（同 `:529-532`）に応じて
`operation succeeded with warnings` または `operation succeeded with warnings; resulting
file may have some problems` を一度だけ出力する。終了コード3の判定は同 `:534-563`、
inspection側のwarning集約は `doInspection`（同 `:1646-1693`）がoracleである。
flpdfは `job/lifecycle.rs::QPDFJob::write_json` / `QPDFJob::inspect` と、未移行のCLI経路では
`crates/flpdf-cli/src/main.rs` の `finish_warning_state` / `finish_operation_warnings`
を共通完了境界とし、rewrite/QDF/page-operation/attachment-write/JSON fileをoutput-producing
経路、show/list/stream/JSON stdout/encryption inspectionをinspection経路として同じsuffix
選択を行う。すべての成功出力を先に完了してから終了コード3を返し、fatal errorの途中では
success summaryを出さない。JSONの `--json-output PATH` はqpdfの出力ファイル相当として
resulting-file suffixを持ち、stdout JSONはinspection suffixを持つ。

### `QPDFLogger` の CLI consumer cutover と retained direct routes

qpdf の route ownership は `QPDFJob.cc:343,498-502,625,709-925,2934,3051-3054,3094-3115`
と照合した。flpdf CLI は invocation ごとに 1 個の private `QPDFLogger` を共有し、次を
logger consumer に移行済みである。

- `save`: JSON stdout、raw/filtered stream stdout、attachment stdout、rewrite/QDF の
  output `-`。いずれも document open / info write より先に `saveToStandardOutput`
  相当を設定し、独立した stdout terminal を作らない
- `info`: check summary、show object、show pages/npages、attachment listing、encryption /
  linearization inspection、rewrite/page-operation verbose output
- `warn`: document warning、warning completion summary、normalization/signature warning
- `error`: check error と top-level の通常 fatal error
- `usage`: `UsageError` を `usage_exit` へ直接渡し、qpdf の空行・help block付き exit-2 を再現

`flpdf-25kg.5.5` では、top-level `--show-linearization` も `QPDFJob::open` が
設定した同じ `Pdf` を `QPDFJob::show_linearization` に渡す。これは qpdf の
`setQPDFOptions` による logger/suppression 設定（`QPDFJob.cc:650-665`）、同じ
documentへの `isLinearized` / `showLinearizationData`（同 `:1646-1674`）、および
writer側の同一document利用（同 `:3030-3058`）に対応する。show用の hint decode は
`show_linearization_pdf_with_warnings` が既存 `Pdf` 上で行い、入力名・warning
抑制・custom sink・完了statusはjobの共有境界から配送する。path helperがdefault
loggerで再openする経路はtop-level CLIから除去し、`--show-linearization` のwarning
と `--no-warn` はqpdf 11.9.0とのCLI differential testで固定する。

以下の direct output は意図的に retained とする。

- `run_show_stream` の passthrough-codec marker: flpdf-only fallback 表示で、qpdf は
  unfilterable stream を同じ marker へ変換しない
- native `rewrite --static-id` warning、`--remove-restrictions` intent diagnostic:
  flpdf-only surface（出力先は qpdf-compatible logger error route）
- clap 自身が parse/usage のために直接終了する help・構文エラー、および logger の
  stderr sink 自体が失敗した場合の last-resort diagnostic: qpdf job logger の
  command-boundary より前後にある irreducible CLI fallback。その他の CLI text
  diagnostics は `QPDFLogger` の platform-aware text route を通る

### `QPDFJob.cc` の `doJSON*` 族 — job 層への段階移設

`QPDFJob.cc:958-1620` の JSON セクション生成のうち、6 section builder（AcroFormを含む）と
`doJSON` の固定順序は `job/json_sections.rs` / `job/json.rs` へ移設した。
`QPDF::writeJSON` 相当の serialization は §8 の `document_json.rs` と
`json_inspect.rs` の canonical ObjectHandle writerが担う。一方、`QPDFJob::writeJSON` の
`QPDFJob.cc:3094-3115` にある top-level 出力先・stream side-file prefix の選択も
`job/json.rs` が所有する。
**§8 の `QPDF_json.cc` 行と混同しないこと**（`QPDF_json.cc` は JSON 入力と
`writeJSON` であって、セクション生成ではない）。

| qpdf `QPDFJob.cc` | flpdf owner |
|---|---|
| `doJSONObjects`(958) の **v1 分岐**(960-981) / `doJSONObjectinfo`(1002) | **対応物なし**。どちらも JSON v1 専用（`doJSONObjectinfo` は `QPDFJob.cc:1620` の version guard 内、`objects` の schema も `json_schema:1357` で v1 限定）。flpdf CLI は `--json=2` のみを受け付け、`main.rs:1914` が `objects` / `objectinfo` を「v1 でのみ有効」と明示的に拒否する |
| `doJSONObjects`(958) の **v2 分岐**(981-997) | 自前では何も組み立てず `QPDF::writeJSON` に委譲するだけ。実体は §8 の `QPDF_json.cc` 出力側の行を参照（ここに再掲すると二重帰属になる） |
| `doJSONPages`(1030) | `job/json_sections.rs::build_pages_section` |
| `doJSONPageLabels`(1095) | `job/json_sections.rs::build_pagelabels_section` |
| `doJSONOutlines`(1143) | `job/json_sections.rs::build_outlines_section` |
| `doJSONAcroform`(1159) | `job/json_sections.rs::build_acroform_section` |
| `doJSONEncrypt`(1206) | `job/json_sections.rs::build_encrypt_section` |
| `doJSONAttachments`(1281) | `job/json_sections.rs::build_attachments_section` |
| `json_schema`(1332) / `json_out_schema`(1533) | `JsonKey` ほか |
| `doJSON`(1545) | `job/json.rs::write_qpdf_json_v2_selected_objects*` |

**qpdf 側の `doJSON*` は辞書を直接歩かず、ヘルパーの薄い JSON 化層でしかない。**
flpdf の6 section実装は現在 `job/json_sections.rs` にまとまり、AcroForm も
`PageDocumentHelper`、`AcroFormDocumentHelper`、`FormFieldObjectHelper`、
`AnnotationObjectHelper` を経由する。
PR #613/#614 では
以下が同時に露出した: `preferredname` の Mac/DOS 優先順位バグが
`job/json_sections.rs` と `filespec_helper.rs` の**両方で独立に発生**、
`modificationdate` が `QPDFEFStreamObjectHelper::getCreationDate()` を経由せず
qpdf 側のコピペバグ（`QPDFJob.cc:1319-1322`）を再現できていなかった、
`fieldtype` の先頭 `/` 欠落（`getFieldType()` 未経由）と、
`build_acroform_section` の走査モデルが qpdf と構造的に別物（`flpdf-d949`）だった。
この AcroForm 経路は q2fo でページ順 Widget 投影へ切り替えた。

**同じ責務が 2 箇所に実装されている状態そのものが D2 違反**であり、
q2fo は AcroForm について旧 `json_inspect` 経路を削除し、ヘルパー上の
単一実装へ切り替えた。

| doJSON* | 経由すべきヘルパー | §7 の状態 |
|---|---|---|
| `doJSONAcroform` | `QPDFAcroFormDocumentHelper` + `QPDFFormFieldObjectHelper` + `QPDFAnnotationObjectHelper` + `QPDFPageDocumentHelper` | 🔀 / 🔀 / 🔀 / 🔀（このJSON経路自体のD2要件——単一実装で canonical helper を経由すること——はq2foで満たした。旧`json_inspect`の重複実装は削除済み。§7の🔀は各ヘルパー自体の他責務が未完了であることを指し、この行の完了とは独立） |
| `doJSONAttachments` | `QPDFEmbeddedFileDocumentHelper` + `QPDFFileSpecObjectHelper` + `QPDFEFStreamObjectHelper` | ✅ / ✅（D1 は完成済み。`job/json_sections.rs` の再実装により D2 はなお未達 — `flpdf-q2fo` で解消予定） |
| `doJSONPages` | `QPDFPageDocumentHelper` + `QPDFPageObjectHelper` | 🔀 / 🔀 |
| `doJSONOutlines` | `QPDFOutlineDocumentHelper` | ✅ |
| `doJSONPageLabels` | `QPDFPageLabelDocumentHelper` | ✅ |

### `qpdfjob-c` wrapper のエラー境界

qpdf の `wrap_qpdfjob`（`libqpdf/qpdfjob-c.cc:32-40`）は、
`QPDFJob::initializeFromJson`/`run` が投げた例外を job logger の error
pipeline へ prefix・区切り・本文・改行の順に送り、`EXIT_ERROR`へ変換する。
flpdf は `QPDFJob::report_job_error` の canonical route を qtest の
Rust consumerへ公開し、`qpdfjob_ctest.rs` がこの wrapper の継続順序だけを
担う。通常の `QPDFJob::run` の `UsageError` contractや、CLIの別の usage
表示経路は変更しない。

## 10. インフラ

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QUtil.cc` | 2003 | `qutil.rs`（`same_file`、qpdf `QUtil.cc:574-610`、および `utf8_to_ascii`/`utf8_to_win_ansi`/`utf8_to_mac_roman`、qpdf `QUtil.cc:1528-1667`）をcanonicalなQUtil責務として公開。password recovery側の別変換表はその読取り責務に限定され、AcroForm appearanceからは参照しない | 🔀 `QUtil`全体の移植ではなく、jobのfilesystem identity guardとappearanceが必要とする3つのpublic conversion contractを先行移植 |
| `QTC.cc` | 50 | 無し | ❌ |
| `BitStream.cc` / `BitWriter.cc` | 111 | `bit_stream.rs`（MSB-first bit 読み取り、Rust の error 値）/ `bit_writer.rs`（MSB-first bit 詰め、Pipeline stage）。production consumer は `linearization/hint_stream.rs`（hint stream の生成・読み取り）と `linearization/show.rs`（`read_h_page_offset` / `read_h_shared_object` / `read_h_generic` の hint decoder）、および `bit_writer.rs` 自身 | ✅ `flpdf-qxba.9.1` で cutover。`linearization/hint_stream.rs` 側に bit 実装は残っていない |
| `Buffer.cc` / `MD5.cc` | 286 | `Vec<u8>` / 外部 crate | ⚪ |
| `qpdf-c.cc` / `qpdfjob-c.cc` / `qpdflogger-c.cc` | 2237 | — | ➖ |

---

## flpdf-only

### A. 2 つの汎用機構を参照種別ごとに特殊化したもの — 1,748 行

| flpdf | 行 | 対応する qpdf 挙動 |
|---|---|---|
| `job/outline_dest_remap.rs` | 898 | 削除ページ参照の null 化（配列要素） |
| `struct_tree_pg.rs` | 379 | `/Pg` の key drop |
| `thread_bead_p.rs` | 293 | bead `/P` の key drop |
| `objr_obj_annot_p.rs` | 178 | OBJR 経由 annotation の `/P` key drop |

**この表に含めない隣接モジュール**（責務が別で、畳み込みの対象にしてはならない）:

| flpdf | 行 | 理由 |
|---|---|---|
| `job/acroform_field_prune.rs` | 497 | qpdf 側に**明示的な対応パスがある**（`QPDFJob.cc:2610-2632` "Remove unreferenced form fields"）。副作用ではなく移植対象 |
| `job/page_subset.rs` + `writer/reachability.rs` | — | `/Resources` の stale 名前エントリ剪定（`removeUnreferencedResources` 相当）と、xref レベルの orphan mark-and-sweep を qpdf の page/job と writer 責務へ分離。null 可視性とは独立 |

**挙動は検証済み**（各モジュール doc に「qpdf 11.9.0 observed behaviour」の節がある）。
qpdf 側はこれを専用パスで実装していない:

```cpp
// (1) QPDFJob.cc:2597-2608 — 選択されなかったページを null に置換
//     "This prevents those objects from being preserved by being referred to
//      from other places, such as the outlines dictionary."
pdf.replaceObject(page.getObjectHandle().getObjGen(), QPDFObjectHandle::newNull());

// (2) QPDFWriter.cc:1491 (unparse) / :1133 (enqueue) — 値が null の dict キーは書かない
for (auto& item: object.getDictAsMap()) {
    if (!item.second.isNull()) { ... }
}
// 配列 (:1128) には同じフィルタが無い → null 要素はそのまま残る
```

flpdf が「dict キーは drop / 配列要素は null 保持」という非対称性として観測し種別ごとに
実装していたものは、この 2 機構の副作用。`QPDFObjectHandle::isNull()` は間接参照を
解決するため、`/Pg 5 0 R` で obj 5 が null なら `/Pg` キーごと消える。

**この主張が及ぶのは上表の 4 モジュール 1,748 行のみ。** `job/acroform_field_prune.rs` と
`job/page_subset.rs` と `writer/reachability.rs` は qpdf 側に別々の対応先を持つ独立した責務であり、2 機構に還元できない。

**区別すべきこと**: 挙動は検証済みで byte-identical を保っているので壊してはならない。
機構が異なるだけ（in-place 個別修復 vs. null 置換 + writer の null 可視性）。
畳み込みは byte リスクを伴う別の設計判断であり、writer の責務分割が固まったあとに検討する。

### B. `QPDFJob::handlePageSpecs` 相当の分解 — 4,158 行

`job/page_specs.rs` / `job/page_split.rs` / `job/page_merge.rs`(1117) / `job/rotate.rs`(632) / `page_extract.rs`(435) /
`job/page_range.rs`(379) / `page_splice.rs`(304) / `job/page_combine.rs`(278) / `job/page_plan.rs`(210) /
`job/rotate_spec.rs`(204)

`job/page_specs.rs` がqpdfのjob-level orchestration（`QPDFJob.cc:2360-2632`）を所有し、
`job/page_merge.rs` と `PageDocumentHelper` がforeign page copy/page-tree primitiveを所有する。
`.40` では `resources.rs` に残る `QPDFPageObjectHelper::removeUnreferencedResources`
相当の page/Form mutation と、`job/resource_pruning.rs` に移した
`QPDFJob::shouldRemoveUnreferencedResources` 相当の `auto|yes|no` policy を分離した。
後者は `QPDFJob.cc:2251-2339` の共有リソース探索だけを所有し、前者の
`/Font`・`/XObject` pruning algorithmを再実装しない。
`.18` では `QPDFJob.cc:2251-2337,2442-2455,2520-2555` の
`--remove-unreferenced-resources={auto,yes,no}` を `job/page_specs.rs` から
`job/page_merge.rs` の初回 foreign-page copy 境界へ渡す。Auto は source ごとに
`shouldRemoveUnreferencedResources` を一度だけ判定し、初回の unique page にだけ
`QPDFPageObjectHelper::removeUnreferencedResources` 相当を適用する。重複選択は
qpdf と同じ shallow page copy とし、汎用 `merge_documents` の library-level route
には job-level resource policy を混ぜない。CLI の複数source経路ではこの pre-copy
処理を正本とし、completion後の document-wide resource passを二重適用しない。
`doSplitPages`（`QPDFJob.cc:2940-3027`）とwriter/output命名は `job/page_split.rs` に移設済み。
`PageDocumentHelper::add_page(PageInput::Foreign)`、`PageObjectHelper::copy_annotations_from`、
`PageLabelDocumentHelper::write_reconstructed_labels_with_prefix_presence` を通る fresh chunk
生成を実装し、CLI の単一入力・複数入力 split 出力を同じ job route に切り替えた。
`.50qd.1` では secondary source の認証を `QPDFJob.cc:2400-2412` の
`page_spec.password` 境界に合わせ、top-level primary password を distinct secondary の
fallback にしない。global な password mode/weak-crypto policy は共有するが、credential
本体は spec-local に限定する。
`.50qd.2` では `QPDFJob.cc:1714-1715` の全 input version floor と
`QPDFJob.cc:2847-2918` の writer 設定境界を、multi-source `--pages` の
fresh merged document に明示的に伝播する。primary と全 secondary の
`M.m`/`/Extensions /ADBE /ExtensionLevel` の pairwise max を既存の
`--min-version` と合成し、`--force-version` は従来どおり最終的に優先させる。
`.50qd.3` では `QPDFJob.cc:2462-2472` の「primary QPDF をページ操作の
base として in-place 更新する」責務と、`QPDFJob.cc:2590-2632` の
`/Pages`・`/PageLabels`・AcroForm の選択ページ側更新を分離した。`job/page_merge.rs`
は選択 page graph を `QPDFPageDocumentHelper::addPage` 相当の canonical
`copyForeignObject` route で先にコピーし、primary Catalog/trailer の各値を
`object_copy::copy_foreign_value`（`replaceForeignIndirectObjects` 相当）へ渡す。
同じ per-source map を維持したまま `/Pages` と writer/xref-owned trailer keys だけを
target 側で再構築する。
その結果 `/Info`、`/ID[0]`、未知の trailer entries、`/ViewerPreferences` や
その他の Catalog siblings は primary の値と indirect-reference identity を
保ったまま remap され、secondary の Catalog/trailer metadata は継承されない。
`page_merge_tests.rs::merge_preserves_primary_catalog_and_trailer_metadata` と
CLI の qpdf 11.9.0 differential test、および fixture の live probe で確認する。

### C. qpdf に機能そのものが無いもの

| flpdf | 行 | 備考 |
|---|---|---|
| `signatures.rs` の**検査 API のみ** | — | 署名の読み取り検査。qpdf に相当機能なし |
| `qdf_fix.rs` | 1,219 | qpdf では `qpdf/fix-qdf.cc`（libqpdf 外の別バイナリ）。object stream (`/Type /ObjStm`) / cross-reference stream (`/Type /XRef`) 形式の QDF 入力にも対応（`st_in_ostream_*` / `st_in_xref_stream_dict` 相当、flpdf-9hc.43） |


`object_copy.rs` の `copy_foreign_object` / `copy_foreign_value` は `QPDF.cc` の
`copyForeignObject` / `replaceForeignIndirectObjects` に相当する。以前存在した
`page_closure.rs::page_object_closure` と `object_copy.rs::copy_objects` は、
pre-closed な `ObjectRef` 集合を raw `Object` に materialize して書き戻す
flpdf 専用 route だったため、`.3.2.8.23` で examples と test-only Form
XObject importer を canonical `copy_foreign_object` に移した後、module/API と
専用 tests ごと削除した。qpdf 11.9.0 の `QPDF::copyForeignObject`
（`QPDF.cc:2019-2272`）に対応する正本は `object_copy::copy_foreign_object` の
みであり、`reserveObjects` / 完全な `ObjectHandle` graph replacement /
`/Pages` 境界 / per-source map reuse をここで担う。stream の
Buffer/provider/original-source 選択は `reader/resolver.rs` の resolver-owned
boundary に委譲し、qpdf の `ot_reserved` は外部に露出しない内部 reservation
sentinel として destination-owned indirect null slot で表現する。

`page_extract.rs::extract_pages` はこの canonical foreign-copy route へ切り替え済みで、
qpdf の source-side inherited-attribute preparation と destination-side page-tree
mutation を組み合わせる。`job/page_merge.rs` も `pushInheritedAttributesToPage` 相当の
source preparation と live-handle による destination `/Parent` replacement を使い、
選択 page graph の legacy pre-closed copy を削除した。primary の document-level /
AcroForm / PageLabels merge は Catalog/trailer の各 direct value を同じ persistent
foreign map でコピーし、`--preserve-unreferenced` は qpdf の live object cache を
`copy_foreign_object` で列挙する。removed-page nulling と `/Pages` 再構築も canonical
handle mutation で行うため、page merge に raw metadata closure bridge は残さない。
`Object::Reference` を値として保持する `Pdf::set_object` holder chain は qpdf の
`copyForeignObject` が拒否する shape であり、後方互換 adapter は追加せず明示的 rejection
を維持する。

⚪ `reserveObjects` 相当（reservation）だけでなく `replaceForeignIndirectObjects`
相当（replacement）でも、直接（非間接）dictionary/array が作る identity cycle を
`direct_visiting`（`ForeignObjectCopier` フィールド）で bound する。qpdf の該当
2 関数（`QPDF.cc:2101-2213`）はいずれも direct cycle 用の visited set を持たない。
実際にパースされた PDF はこの形を表現できない（直接値は自分自身を参照するための
アドレス可能な identity を持たない）ため、qpdf 側にこの bound の対応物は無い。
公開 `ObjectHandle::replace_key` API 経由でのみ構築可能な入力への防御であり、
出力バイトには影響しない。

⚪ `reserve_objects`（`ForeignObjectCopier`）は各ノードの `owning_pdf_unique_id`
を root の `source_id` と照合し、不一致なら拒否する。`ObjectHandle::replace_key`
は `QPDFObjectHandle::checkOwnership`（`QPDFObjectHandle.cc:2355-2365`）と同じ
shallow 比較（`self`/`value` 自身の owning document のみ、子孫は辿らない）を
実装済み（flpdf-25kg.3.8.1.2）だが、qpdf の `checkOwnership` 自体が shallow で
ある以上、直接（非間接）コンテナに数ホップ下でネストした foreign indirect object
は qpdf でも挿入時には検出されない（`QPDF::copyForeignObject` 自身の呼び出し側
向けドキュメントが、この状況を避けるのは呼び出し側の責務だと明記している）。
`reserveObjects`/`replaceForeignIndirectObjects`（`QPDF.cc:2101-2213`）自身にも
対応するチェックは無いため、`reserve_objects` のこの再検証は「未実装ギャップの
暫定穴埋め」ではなく、qpdf のこの境界そのものが持つ shallow-check の弱点に対する
flpdf 独自の追加防御であり、公開 `ObjectHandle` API 経由でのみ構築可能な入力への
防御として、実パースされた PDF の出力バイトには影響しない。`QPDF_Array` の各
ミューテータ側（`check_array_item_ownership`）も `QPDF_Array::checkOwnership`
（`QPDF_Array.cc:10-26`）と同じく、挿入される値自身の owning document だけを
比較する shallow 判定へ揃えた（flpdf-25kg.3.16.7.1）。`belongs_exclusively_to_pdf`
の子孫再帰は、`replace_object` などのforeign replacement防御に残る別責務であり、
array ownership checkからは呼び出さない。qpdfのfile parserは非nullのdirect値にも
`QPDFParser::setDescription`（`QPDFParser.cc:394-444`）経由でQPDF contextを付ける一方、
literal nullは共有`QPDF_Null::create`（`QPDFParser.cc:395-410`）のためownerlessのまま。
flpdfもparser生成経路だけsource PDF identityをstampし、programmatic/legacy direct値の
ownerless性を維持している。

⚪ `reserve_objects` と `replace_foreign_indirect_objects` の両方を
`stacker::maybe_grow`（`OBJECT_COPY_STACK_RED_ZONE`/`OBJECT_COPY_STACK_GROWTH_SIZE`）
で包む。個別の indirect object から成る非循環チェーン（A → B → C → …、各々が
別オブジェクト番号）はパーサの container-nesting 上限（`MAX_PARSE_DEPTH`）で
bound されないため、十分に長い参照チェーンを持つ実在の PDF がこの再帰で
コールスタックを枯渇させ得る。qpdf の `reserveObjects`/
`replaceForeignIndirectObjects`（`QPDF.cc:2101-2213`）にもこの経路の深さ制限は
無いため、qpdf parity の欠落ではなく flpdf 実装固有の Rust スタック安全性対応
であり、出力バイトには影響しない。

---

## 検証可能性（safety net）

byte golden の無い書き込み経路は安全に移動できない。🔀 行の着手順はここで決まる。

- `tests/golden/references/` — 123 ディレクトリ
- whole-file gated（`#![cfg(feature = "qpdf-zlib-compat")]`）byte テスト — 11 ファイル
- `tests/golden/compat-matrix.md`
- `job::overlay::byte_gate`（`--lib` 実行）

| 経路 | library byte gate | CLI byte gate |
|---|---|---|
| classic full rewrite（`--static-id`） | `cmp_diff_zero_tests` ✅ | `compat_baseline_static_id` ✅ |
| objstm generate（非 linearized） | `cmp_generate_objstm_tests` ✅ | `compat_matrix_baseline` ✅ |
| linearize（classic） | `cmp_linearize_tests` ✅ | `cli_byte_identical` ✅ |
| linearize + objstm | `cmp_linearize_objstm_tests` ✅ | ✅ |
| overlay / underlay | `job::overlay::byte_gate` ✅ | `cli_byte_identical_overlay` ✅ |
| `--deterministic-id` | `deterministic_id_qpdf_parity_tests` ✅ | — |
| null 可視性 | `cmp_null_visibility_tests` ✅ | — |
| QDF | 🟡 **部分的にあり**（下記）。`job::overlay::byte_gate` の QDF 12 件を含む | 🟡 `cli_byte_identical_overlay.rs` の QDF 3 件 |
| 暗号化出力 | ❌ gated byte gate 無し | 🟡 `encrypt_cli_tests` の `encrypted_document_is_byte_identical_to_qpdf` / `cli_linearize_encrypt_aes128_byte_identical_to_qpdf` 2件（`qpdf-zlib-compat` 関数レベル gate、CI 列挙済み） |
| PDF incremental append: not applicable | qpdf 11.9.0 has no incremental append writer; `/Prev` is reader-side xref history | flpdf `PdfWriter` always emits a fresh full rewrite; reader-side `/Prev` parsing remains |

### QDF の既存カバレッジ（部分的）

「QDF に byte gate 無し」は誤り。次の 3 系統が既に存在する。

| テスト | 内容 | CI |
|---|---|---|
| `writer_tests.rs:2170,2201` | `tests/golden/references/qdf-contents-ref-array/qdf-static-id.pdf` と `qdf-ignore-newline/qdf-static-id.pdf` に対する完全一致比較 | ✅ 列挙済み |
| `qdf_tests.rs:1300` | `qdf_golden_minimal_is_byte_identical_to_qpdf_modulo_id` — `tests/fixtures/qdf-golden/minimal.qdf` に対し trailer `/ID` 行を除いて完全一致 | 既定テストに含まれる |
| `job/overlay.rs` の `job::overlay::byte_gate` | **QDF byte-identity テスト 12 件** — `three_page_*_qdf_is_byte_identical` 3 件(1320, 1339, 1357) と annotation-copy 系 `*_is_byte_identical_qdf` 9 件(1528-1889) | ✅ `--lib job::overlay::byte_gate` で列挙済み |
| `cli_byte_identical_overlay.rs`(293-338) | 上記の CLI 版 QDF variant（`--qdf --no-original-object-ids`） | ✅ 列挙済み |

### QDF の組み合わせ整理

QDF のテキスト整形は、オブジェクトストリーム形式とは独立して qpdf の
writer に適用される。一方、linearize と暗号化出力は qpdf の設定境界で
排他になる。

| 組み合わせ | 排他の実装箇所 |
|---|---|
| QDF × ObjStm | `qdf_tests.rs:749,913,1013` — QDF preserves explicit `Preserve`/`Generate`; `Disable` keeps the classic no-ObjStm form, matching qpdf's mode-independent writer setup |
| QDF × linearize | `flpdf-cli/src/main.rs:1466` `--qdf and --linearize cannot be used together` |
| QDF × 暗号化出力 | `writer.rs:3135` `--encrypt / --copy-encryption-from cannot be combined with --qdf` |

**残る有効な穴**: 暗号化された**入力**からの QDF 出力（復号 → QDF）、および
現状 fixture が無い QDF オプションの組み合わせ。Phase 2 で null 可視性を QDF 経路に
広げる際に必要になるのはこちら。

gated テストは `.github/workflows/ci.yml` の bytes-identical ジョブに手で列挙しないと
CI で走らない。ファイル全体が gated な 11 件は全て列挙済み（`cmp_null_visibility_tests`
の列挙漏れは `flpdf-qxba.2` で解消済み）。新規に file-level gate を追加する際は
同様の手動列挙が必要な点に注意。

---

## 逸脱候補（⚪）— 要承認

`CLAUDE.md` は DEFLATE バックエンドを「唯一の例外」とし「逸脱は必ず明示」を求めている。
⚪ に分類した 7,099 行は提案であり決定ではない。

| 逸脱候補 | qpdf 行数 | byte 影響 |
|---|---|---|
| `InputSource` 階層 → `Read + Seek` ジェネリクス | 625 | 無し（入力側のみ） |
| `QPDFArgParser` / `QPDFJob_*` → clap | 3,164 | 無し（CLI 挙動 parity は別途必要） |
| crypto provider 抽象 → 外部 crate 直接利用 | 2,442 | 無し（アルゴリズムは同一） |
| `Buffer` / `Pl_Buffer` / 汎用 `Pl_*` → `Vec<u8>` / `Write` | 856 | 無し |
| `QPDFDocumentHelper` / `QPDFObjectHelper` 基底 → トレイト無し | 12 | 無し |
| `std::shared_ptr<QPDFValue>` → `Rc<RefCell<..>>`（`object_handle.rs`） | 79 | 無し（`Rc` による共有 identity の内部所有権機構自体。live direct containment の weak reverse index は qpdf の現在の forward membership から派生する incremental dirty bookkeeping で、stale owner の誤った scheduling を現在の graph に一致させる。共有 identity と各 object の serialization rule は変えず、Pdf identity provenance は別フィールドで保持。byte-identical suite で確認済み） |
| `std::shared_ptr<Buffer> QPDF_Stream::stream_data`（`libqpdf/qpdf/QPDF_Stream.hh:104`） → `Rc<Vec<u8>>`（`object_handle.rs` の `ObjectValue::Stream`） | 1 | 無し（共有の意味論は同一。`QPDFObjectHandle::newStream(QPDF*, shared_ptr<Buffer>)` / `replaceStreamData(shared_ptr<Buffer>, ..)` / `QPDF_Stream::getStreamDataBuffer` に対応する `ObjectHandle::stream` / `replace_stream_data` / `as_stream_data` が buffer を共有したまま受け渡す。`Rc<[u8]>` ではなく `Rc<Vec<u8>>` なのは、`Rc::<[u8]>::from(vec)` が refcount ヘッダを前置できず payload 全体を memcpy するため。二段の間接になるのは `shared_ptr<Buffer>` と偶然一致するだけで対応関係ではない — qpdf が `Buffer` 型を要するのは C++ が borrow/own を型で表せず実行時フラグに畳むからで（`include/qpdf/Buffer.hh:35-46` が所有・非所有の両コンストラクタを持つ）、その面は既存の `Buffer` → `Vec<u8>` 行が扱う。`Rc` なのは `Repr` が `Rc<RefCell<..>>` ベースで `ObjectValue` がそもそも `!Send` のため。`replace_stream_data` は `QPDF_Stream::replaceFilterData`（`QPDF_Stream.cc:668-684`）に対応する共有 helper を通り、zero length では `/Length` を削除、nonzero では正確な integer を設定する（`flpdf-25kg.4.5`）。byte-identical suite（`qpdf-zlib-compat`）で確認済み） |
| `QPDF_Array` borrow / slash 付き canonical name string → `Vec<ObjectHandle>` の単一 child clone / slash 無し decoded `Vec<u8>`、および live array mutation（`object_handle.rs`） | 0 | 無し。`try_array_item` は `QPDF_Array::at` と同じ valid index の child identity を `Rc` clone で返し、name predicate は同じ decoded bytes を比較するだけで出力しない。`set_array_item` / `set_array_items` / `insert_array_item` / `append_array_item` / `erase_array_item` は `QPDFObjectHandle.cc:869-955` と `QPDF_Array.cc:10-26,220-313` の bounds→warning、ownership、live child containment、`setFromVector` の clear-before-check / partial-prefix 順序を保持する。`nntree.rs` の canonical NNTree engine はこの live mutation boundary を `set_array_items` から利用し、旧 `replace_array_item(s)` は qpdf の warning/ownership/insert/erase 契約を持たない compatibility bridge として残る。 |

直接構築された深いコンテナの破棄は、qpdf 11.9.0 の `QPDFObject`/`QPDFValue` と
`QPDF_Array`/`QPDF_Dictionary` の shared-pointer ownership（`QPDFObject_private.hh:19-24,176-179`、
`QPDFValue.hh:18-27`、`QPDF_Array.hh:9-50`、`QPDF_Dictionary.hh:11-38`、
`QPDFObjectHandle.cc:1944-2013`）では既定デストラクタが再帰的に辿る。固定版 qpdf の
live probe は深さ 5,000 では exit 0、50,000 と 100,000 では構築完了後に exit 139
となった。flpdf はこの一点を Rust safety hardening として、最終所有者の direct
`Array`/`Dictionary`/`Stream` dictionary edge だけを heap worklist で解放する。
共有 alias・indirect/resolver identity・PDF bytes は変更しない。

現時点の証拠ではいずれも出力バイトに影響しない。

### 方針上の位置づけ（解決済み）

当初、上表は `CLAUDE.md` の「DEFLATE が唯一の許容された逸脱」条項と矛盾していた。
これを受けて `CLAUDE.md` の逸脱条項を **2 分類**に改訂した。

- **(A) 出力バイトを変える逸脱** — DEFLATE 実装のみ（従来どおり唯一）
- **(B) 出力バイトを変えない内部構造の代替** — 条件付きで許容（新設）

上表の 7,099 行はすべて (B) に該当する。ただし (B) は無条件ではなく、
`CLAUDE.md` の 3 条件を満たす必要がある。

1. 出力バイトに影響しないこと（証明責任は提案側。gated byte テストで担保。
   守られていない経路は先にゲートを追加する）
2. アルゴリズムと処理順序は qpdf のまま（代替してよいのは「入れ物」だけ）
3. 明示的に記録すること（モジュール doc に 1 行 + 本表の ⚪ 行）

したがって各項目は **着手時に条件 1 を検証したうえで**適用する。表に載っている
ことは「無条件で承認済み」を意味しない。

---

### Pl_AES_PDF production consumer correction (flpdf-qynx.10)

The `Pl_AES_PDF` production cutover is complete. Reader string decryption,
reader stream decryption, writer stream encryption, writer encrypted-string
emission, V=5 `/UE` and `/OE` wrapping, R=6 Algorithm 2.B's repeated AES step,
and R=5/R=6 `/Perms` verification/construction all use the canonical
`pipeline/aes.rs::PlAesPdf` stage. The no-padding zero/specified-IV helper
preserves qpdf's `process_with_aes` state across repeated writes
(`libqpdf/QPDF_encryption.cc:209-236,601-663`). The direct CBC and single-block
AES helpers formerly in `encryption/standard.rs`, `encryption/state.rs`, and
`encryption/primitives.rs` were removed. `disableCBC` remains test-only, while
`useZeroIV` and `disablePadding` are production controls because qpdf uses both
in its V=5 and `/Perms` consumers.

### DCT whole-buffer consumer correction (flpdf-n9t0.9)

The DCT row above predates the qtest `qpdf_dl_all` follow-up. The legacy
whole-buffer adapter is no longer passthrough-only: it drives the same
`PlDct` stage as `decode_pipeline`, preserving qpdf's buffered finish,
scanline output, output-limit enforcement, and codec diagnostics. The writer
encoded-stream passthrough remains a separate responsibility. The qpdf
11.9.0 `test_driver` differential covers `/DCTDecode`, `/DCT`, non-null
`/ColorTransform` `DecodeParms`, malformed JPEG input, 57 fixtures, and 11
CLI probes.

### coalesceContentStreams correspondence

### qtest document-construction helper ports (`flpdf-egzr.5`)

`flpdf-qtest-tools::document_construction` ports the two qpdf test programs
without introducing a shell-out or a test-only document builder. The
`pdf_from_scratch` binary follows `qpdf/pdf_from_scratch.cc:33-79`: it creates
`Pdf::empty()` (qpdf `QPDF::emptyPDF`, `libqpdf/QPDF.cc:290-293`), promotes the
parsed font and procset with the document-owned indirect-object route, creates
the `First Page` stream, inserts the page through
`PageDocumentHelper::add_page` (qpdf `QPDFPageDocumentHelper.cc:37-40`), and
writes `a.pdf` with static IDs and preserved stream data. Its usage,
`invalid test N`, stdout, status 2, and output-write failure boundaries mirror
`pdf_from_scratch.cc:14-19,75-101`.

The `test_many_nulls` binary follows `qpdf/test_many_nulls.cc:18-41`: it builds
one outer array containing 20 inner arrays of 20,000 shared null handles,
stores the outer array under the trailer `/Nulls`, appends one direct page to
`/Pages/Kids`, and writes with generated object streams and a deterministic ID.
The release qtest path enables `qpdf-zlib-compat`, so the pinned qpdf 11.9.0
helper and Rust helper produce byte-identical output; the qpdf-test-compare
and `qpdf --check` steps also pass. The full qtest survey promotes the five
previously helper-blocked rows (`from-scratch` 1-2 and `many-nulls` 1-3) to
`passing`; unchanged allowlist regressions remain separately classified by
the survey.

qpdf の `QPDFPageObjectHelper::coalesceContentStreams`（`QPDFPageObjectHelper.cc:474-476`）から
`QPDFObjectHandle::coalesceContentStreams`（`QPDFObjectHandle.cc:1550-1572`）へ委譲される
coalesce は、`QPDF.cc:1912-1917` の `newStream()` と
`QPDF_Stream::replaceStreamData`（`QPDF_Stream.cc:651-685`）で、空の dictionary を持つ
provider-backed stream を登録する。`arrayOrStreamToStreamArray`
（`QPDFObjectHandle.cc:1438-1485`）が非 stream 要素を警告して無視し、
`pipeContentStreams`（同 `:1710-1737`）が specialized decode と条件付き LF を実行する。
flpdf は `PageObjectHelper::coalesce_content_streams` /
`ObjectHandle::coalesce_content_streams` を唯一の production route とする。手動 `Vec` 結合、
入力 metadata のコピー、legacy stream write-back は削除済みである。

### ObjectHandle consumer slice `flpdf-25kg.3.48.5` (2026-08-30)

The remaining reachable consumer routes audited against qpdf 11.9.0 are now
handle-native. `form_field_object_helper/rendering.rs` uses the
`ObjectHandleParserCallbacks` content boundary for `/DA` `Tf` replacement;
`job/overlay.rs` uses `QPDF::newStream` semantics through
`Pdf::new_stream_with_data`; and `job/json_sections.rs` projects every
`QPDFJob::doJSONAttachments` Filespec and `/EF` ditems entry through
`FileSpec`/`EmbeddedFileStream`. The JSON route preserves qpdf's direct-handle
`unparse()` fields, name precedence, empty preferred-name string, all `/EF`
keys, warning/exit behavior, and the 11.9.0 CreationDate-backed
`modificationdate` quirk (`QPDFJob.cc:1281-1330`).

The earlier D2 notes on the `QPDFEmbeddedFileDocumentHelper` and
`QPDFFileSpecObjectHelper` rows are superseded by this slice. Live probes for
normal, all-key, direct-Filespec, malformed-scalar, and non-stream-EF inputs
match `/usr/bin/qpdf` 11.9.0 in JSON stdout and exit status. Existing raw xref
bootstrap and documented synthetic `Pdf::set_object` bare-reference bridges
remain outside this consumer slice.

## 集計

| 状態 | qpdf 側の該当行数 | 内訳 |
|---|---|---|
| ✅ 境界一致 | 5,255 | 責務境界は一致。**再配置は不要だが「完成」ではない** — DoD D1〜D5 の充足は各スライスで別途検証する |
| 🔀 smeared | 27,138 | 再配置の主対象。qpdf 全体の 65% |
| ❌ missing | 169 | `Pl_DCT.cc` compression(119) / `QTC`(50) |
| ⚪ 逸脱候補 | 6,598 | 要承認（下記の方針矛盾を参照） |
| ➖ 対象外 | 2,299 | C API |
| **合計** | **41,459** | qpdf `libqpdf/*.cc` の実測 41,459 行と一致 |

本文の各行を機械的に集計した値である（`状態` 列の記号ごとに `行` 列を合算）。
**この合計もスナップショットであり、維持対象ではない**（上記「行数の位置づけ」参照）。
読み取るべきは「smeared が 6 割台を占める」という規模感であって、個々の値ではない。
数値を更新する場合に限り、合計が qpdf 実測と一致することを確認する。
過去に 41,336 と記載して 123 行の欠損があったが、内訳は集計漏れ 185 行
（`ランダム源 3 ファイル` 行）と汎用 `Pl_*` 行の過大記載 −62 行だった。
どの qpdf ファイルもいずれかの行に属していることは確認済み。

**2026-08-16 再測**: `flpdf-egzr`/`flpdf-3yn9` 系の ObjectHandle 移行・Tier
ヘルパー D1 完成が 70 件以上 close されたのを受けて状態記号を再点検した。
実際に記号が動いたのは `QPDFEmbeddedFileDocumentHelper.cc`(122行) と
`QPDFFileSpecObjectHelper`/`QPDFEFStreamObjectHelper`(280行) の 🔀→✅ の
2 行のみ（計 402 行が smeared → 境界一致に移動）。他の多くの行は
ObjectHandle 移行という**実装手段**が完了していても、qpdf 側の 1 ファイルに
対し flpdf 側が複数モジュールへ分散したままという**責務境界の smear**は
解消していないため記号を維持した（詳細は各行および冒頭「2026-08-16 の
再測」節）。

**❌ の数え方**: 以前は `Pipeline.cc` + `Pl_*.cc` 21 ファイル計 ~2,400 行を丸ごと
missing として傘で数えていたが、個々の `Pl_*` は下の各行で 境界一致 / smeared /
逸脱候補として個別に分類されており**二重計上**だった。傘の行を `Pipeline.cc`
本体（114 行）に限定し、真に未マップな qpdf 行だけを ❌ に数えるよう改めた。

### `test_driver` test 50

`qpdf/test_driver.cc:1940-1953` は trailer の `/Dict1` と `/Dict2` を live handle として
取得し、`mergeResources` 後の `d1.getJSON(JSON::LATEST)`を出力する。続く
`mergeResources(d2.getKey("/k1"))` は top-level type mismatch の no-op であり、その後
`d1.getResourceNames()`が返す resource dictionary の二段目のキーを sorted set の順に
`std::cout`へ出力する。`getResourceNames`の公開契約は
`include/qpdf/QPDFObjectHandle.hh:831-835`、実装は
`libqpdf/QPDFObjectHandle.cc:1156-1170`で、receiver と各top-level valueのdictionary
判定を行い、dictionary-valued entryのキーをunionする。

flpdfは既存の canonical `ObjectHandle::merge_resources` と
`ObjectHandle::get_resource_names`を使い、driverは返されたname bytesをそのまま
stdoutへ書く。警告は既存の`emit_new_diagnostics`でconsumer出力前に排出し、resource
traversalやwarning formatterをdriverへ複製しない。Pinned qpdf 11.9.0の
`merge-dict.pdf`における該当出力は次の10行と`test 50 done`（exit 0）である。

```text
/A
/B
/C
/a
/b
/c
/d
/e
/indirect2
/recursive
test 50 done
```

qtestの`merge-dictionary 1`はこのdriver consumerを比較し、JSONのmerged body、nameの
順序、footer、exit 0を同一runで検証する。qpdfの`test_50`本体は`QPDFWriter`を呼ばず、
`test 50 done`は共通driver boundaryが出力する。

### `test_driver` test 17

`qpdf/test_driver.cc:776-793` は重複した `/Pages /Kids` を含む
`page_api_2.pdf` に対して `getAllPages()` を呼び、後続のページ削除と内容検査を
行う。qpdf 11.9.0 の成功出力は明示的な stdout ではなく、canonical page-tree
repair (`pages/repair.rs:297-302`) が記録する
`kid 1 (from 0) appears more than once in the pages tree; creating a new page object as a copy`
warning と `test 17 done` の組合せになる。`run_test_17` は最初の
`PageDocumentHelper::get_all_pages()` 直後に `emit_new_diagnostics` を一度だけ
呼び、filename/object/offset を保持したqpdfのwarning順序をdriver側で再生成せず
排出する。Pinned qpdf と Rust driver は `page-api 5` で exit 0、stdout/stderrを
結合した出力まで一致する。

### `test_driver` test 69

`qpdf/test_driver.cc:2388-2402` は `setImmediateCopyFrom(true)` の後に
`getAllPages()` を呼び、各ページを新しいPDFへforeign copyして
`auto-<i>.pdf`へ書き出す。`issue-449.pdf`ではページ修復が
`object 3 0 at offset 139` と `object 4 0 at offset 211` の
`MediaBox is undefined; setting to letter / ANSI A` warningをこの最初の
page-list operationで記録し、`test 69 done`の前に2行を出力する。
`run_test_69`はcanonical `PageDocumentHelper::get_all_pages()`直後に
`emit_new_diagnostics`を一度だけ呼び、foreign copy/writerの実装順と警告順を
分離する。Pinned qpdfとRust driverは `copy-foreign-objects 11` の
stdout/stderr/exitを一致させる。

### `test_driver` test 51

`qpdf/test_driver.cc:1955-1997` は `r1`、`checkbox1`、`checkbox2`、`r2` の順に
操作名を出力し、`QPDFFormFieldObjectHelper::setV`を呼ぶ。buttonの値処理は
`libqpdf/QPDFFormFieldObjectHelper.cc:300-326`で分岐し、radioのwidgetが見つからない
場合は同ファイル`348-412`の`unable to set the value of this radio button`、checkboxの
annotationが見つからない場合は`416-469`の`unable to set the value of this checkbox`を
公開API `QPDFObjectHandle::warnIfPossible`（`include/qpdf/QPDFObjectHandle.hh:1257-1263`）
へ記録する。flpdfはこの責務を`FormFieldObjectHelper::set_value`と
`set_radio_button_value`/`set_checkbox_value`へ置き、driver固有のwarning formatterは
追加しない。

`run_test_51`は各`setV`相当の操作直後に既存の`emit_new_diagnostics`を呼ぶ。これにより
qpdf 11.9.0の`button-set-broken.pdf`で得られる、操作名とwarningの順序を保った次の
combined output（exit 0）になる。

```text
setting r1 via parent
WARNING: button-set-broken.pdf, object 5 0 at offset 995: unable to set the value of this radio button
turning checkbox1 on
turning checkbox2 off
WARNING: button-set-broken.pdf, object 7 0 at offset 1354: unable to set the value of this checkbox
setting r2 via child
test 51 done
```

Pinned qpdfとの同一run比較で`interactive-form 12`のstdout/stderr/exitを一致させ、
qtestのXMLでも同行をpassingへ移す。writerは従来どおりQDF出力を完了し、warningの
filename/object/offsetは既存のObjectHandle warning sinkから排出する。

### `test_driver` test 81

`qpdf/test_driver.cc:2807-2817` の ownerless `newNull().getIntValue()` は、
`libqpdf/QPDFObjectHandle.cc:502-513,2168-2189` の
`QPDFExc(qpdf_e_object)` を consumer が捕捉して正常終了する。flpdf は
`ObjectHandle::try_get_int_value` の no-context `Error::System` を同じ
canonical type-warning boundary として利用し、qtest driver は警告を再生成せず
捕捉だけを行う。Pinned qpdf 11.9.0 の `test_driver 81 -` は exit 0、stdout
`test 81 done`、stderr空を返す。

### `QPDF::getRoot` の test_driver consumer

`libqpdf/QPDF.cc:2355-2368` の `QPDF::getRoot` は trailer の `/Root` を
解決し、dictionaryでなければ `unable to find /Root dictionary` を投げる。
`test_driver.cc:3155-3159,3252,3285` のtest88/93/94はこの検証を通過して
から後続操作へ進むため、qtest driverも`Pdf::root_handle()`を使う。公開APIの
document-neutralなエラーを、qpdfの`QPDFExc::createWhat`
（`libqpdf/QPDFExc.cc:19-51`）と同じfilename付きbyte表示へ戻す処理は、driver
boundaryに限定している。Pinned qpdfで非dictionary `/Root`を与えたtest93は、
修復警告3行の後に`<filename>: unable to find /Root dictionary`を返す。
