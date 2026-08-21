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
`matrix` / `pdf_version` / `security/rc4` / `tokenizer` のみが `Mirrors`）。
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

## 1. オブジェクトモデル

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFObjectHandle::makeResourcesIndirect` | `include/qpdf/QPDFObjectHandle.hh:789-793`; `libqpdf/QPDFObjectHandle.cc:1042-1060` | `object_handle.rs::make_resources_indirect` + `acroform_document_helper.rs::prepare_foreign_resource_plan` | ✅ direct second-level resource values are promoted in place through the canonical resolver before `mergeResources`; category dictionaries are not promoted and the walk is non-recursive. Tests cover direct/indirect categories, already-indirect values, non-dictionary top-level entries, alias identity, and the foreign AcroForm caller |
| `QPDFObjectHandle.cc` | 2601 | `object.rs`(1301) + `object_handle.rs`(shared handle identity・parsed offset・遅延解決・live direct containment・`QPDF::newReserved`/`QPDF_Reserved`・`copyStream`/`StreamDataProvider` source dispatch) + `qpdf_null.rs`(9-37: `reference_is_null` / `value_is_null` = `isNull` の間接参照解決) + `overlay_annotations.rs`(1685-1737: `merge_resources_shallow` = `mergeResources`) + `overlay_appearance_stream.rs`（段階的 conflict merge の再現） | 🔀 アクセサは `object.rs` / `object_handle.rs` に分割されたまま（旧 raw `Object` route の削除・統合は `flpdf-egzr.3.2.8` が担当、open。`flpdf-mfir` はその削除対象へのリファクタなので着手せず close 済み）。object identity / 遅延解決は `object_handle.rs` の production route への移行が完了済み（`flpdf-egzr.3.1`、2026-08-09 close）。qpdf の array/dictionary/stream が保持する現在の forward child を正本とし、incremental dirty lookup 用に各 forward edge と一対一の immediate weak reverse edge を派生記録する。削除・置換後の旧 child は旧 root を返さない。`try_get_keys` は `QPDFObjectHandle::getKeys` → `QPDF_Dictionary::getKeys`（`QPDFObjectHandle.cc:997-1009`; `QPDF_Dictionary.cc:117-127`）に対応し、holder と全 child を lazy resolve して null value のキーを除外した `BTreeSet` を返す。child resolve 前に辞書 snapshot の borrow は終了し、resolver error は伝播する。`stream_filter.rs` の consuming stage は retained-key reduction 前に `try_get_keys` を使用する。`shallow_copy` は `QPDFObjectHandle::shallowCopy`（`QPDFObjectHandle.cc:2072-2079`）に対応し、stream は `QPDF_Stream::copy`（`QPDF_Stream.cc:140-145`）が `shallow` 引数を無視して無条件に `std::runtime_error` を投げるのに合わせて `Error::System` で拒否する。`QPDF_Dictionary::copy`/`QPDF_Array::copy` が direct な子に `shallowCopy` を掛けるため、コンテナに入れ子の direct stream も同じ拒否に到達する。qpdf の `QPDFObjectHandle::copyStream`（`QPDFObjectHandle.cc:2136-2151`）と `QPDF::copyStreamData`（`QPDF.cc:2216-2272`）を `ObjectHandle::copy_stream` と resolver-owned stream-copy boundary として実装済み（`flpdf-a8mk`）。Buffer は `Rc<Vec<u8>>` 共有、provider-backed source は source handle を保持する retry-aware provider、original-file source は qpdf の `ForeignStreamData` 相当として source の `StreamInput`/encryption state/object number/parsed offset/length と destination dictionary を copy 時に凍結し、destination resolver を warning sink として遅延 dispatch する。source `Pdf` 解放後も入力と暗号状態だけで読み続け、source 側へ警告を戻さない。`set_immediate_copy_from` は qpdf の source-side `setImmediateCopyFrom` に対応する。`QPDFObjectHandle::isReserved`/`QPDF::newReserved` は `ObjectState::Reserved` と `Pdf::new_reserved` に対応し、`ot_reserved` は null/missing/destroyed と区別して、materialize と全 ObjectHandle writer entrypoint で `QPDFObjectHandle: attempting to unparse a reserved object` を返す。 |
| `QPDFObjectHandle::StreamDataProvider` / `QPDF_Stream` | `QPDFObjectHandle.hh:68-127`; `QPDFObjectHandle.cc:48-90,1365-1428`; `QPDF_Stream.cc:571-620,640-660` | `object_handle.rs` の `StreamDataProvider`、`ObjectValue::Stream.stream_provider`、`replace_stream_data_provider`、callback adapter、`pipe_stream_source` | ✅ qpdf の provider ownership、通常/retry family の選択、identity forwarding、遅延・反復 invocation、`Pl_Count` による encoded-byte length 検証、buffer/provider の排他を canonical route で保持する。qpdf の `std::shared_ptr` container は `Rc<dyn StreamDataProvider>` に置換するが、これは内部所有表現だけの差であり、callback/error/finish/`/Length` の観測契約は変えない。登録 API は stable `ObjectRef` を必要とするため indirect stream に限定し、direct stream は登録時に `Error::System` で拒否する。既存 document-owned stream の provider/dictionary 置換は live graph mutation なので、writer 前に `Pdf::mark_object_handle_dirty` を要求する | ✅ |
| `QPDFObjectHandle::isNameAndEquals` / `isDictionaryOfType` / `getArrayNItems` / `getArrayItem` / `isOrHasName`（行数は上段に計上済み） | — | `object_handle.rs` の `try_is_name_and_equals` / `try_is_dictionary_of_type` / `try_array_len` / `try_array_item` / `try_is_or_has_name`（`QPDFObjectHandle.cc:456-466,759-785,1027-1039`） | ✅ holder と child を qpdf 順に lazy resolve。container borrow は resolver 再入前に解放し、配列全体を snapshot しない。`try_array_item` は `QPDF::decryptStream` が equal-length 確認後に使う valid-index 面のみで、qpdf が warning と特殊 null を返す invalid access は契約外 |
| `QPDFObjectHandle::typeWarning` / `warnIfPossible` / `objectWarning` / `warn` / `getIntValue` / `getIntValueAsInt`（行数は上段に計上済み） | — | `object_handle.rs` の `type_warning` / `warn_if_possible` / `object_warning` / `warn_through_context` / `context` と `DocumentResolver::warn`、`try_get_int_value` / `try_get_int_value_as_int`、`reader/resolver.rs` の `push_object_warning`（`QPDFObjectHandle.cc:502-543,2168-2212,2385-2396`; `QPDF.cc:487-494`） | 🔀 メッセージ文言は qpdf と完全一致。live parser が生成した direct value と canonical indirect handle は `HandleResolver::direct_handle` / `ChildHandles` から同じ weak document context と、qpdf の `QPDFParser` と同じ parse-call description template を持つ。非 null の top-level・array・dictionary・scalar は `input-description, object N G at offset $PO` を共有し、`QPDFValue` と同じ container offset shift を経て `DocumentResolver::warn` → `push_object_warning` で `Pdf::repair_diagnostics` と同じ収集先へ同順に届く。parsed null は qpdf と同じく description を持たない。literal null は containment parent の context を借りず、qpdf の `QPDF_Null::create` に対応する contextless 分岐をネスト後も維持する一方、missing-key null は `setChildDescription` に対応する Child description 経由で親の context を保持する（`QPDF_Null.cc:12-15`; `QPDFParser.cc:397-410`; `QPDFObject_private.hh:79-91`）。明示的 parse と programmatic direct は qpdf の contextless 分岐を維持する。no-context 分岐は qpdf のまま 2 通り — `typeWarning`/`objectWarning` は `throw QPDFExc`（`std::runtime_error` 派生、`QPDFExc.hh:29`）に対応する `Error::System`、`warnIfPossible` は `QPDFLogger::defaultLogger()->getError()` へ素の文言を書いて正常復帰する。`getKey`/`getKeys` の `typeWarning` は `try_get_key`/`try_get_keys` に実装済み。live parser の direct value は weak document context を持ち、stream_filter の consuming `/DecodeParms` 読み出しで qpdf と同じ回復可能な警告を `DocumentResolver::warn` へ送る。contextless の programmatic direct は qpdf と同じく `Error::System` 相当の throw を維持する。`asDictionary`/`asInteger` に対応する `try_as_dictionary`/`try_as_integer` は qpdf 同様 warning を出さない |
| `QPDF_Array/Dictionary/Stream/String/Name/Real/Integer/Bool/Null/InlineImage/Operator/Reserved/Unresolved/Destroyed.cc` | 1814 | `object.rs` の `Object` enum に統合 | 🔀 |
| `QPDFObject.cc` / `QPDFValue.cc` | 79 | `object.rs` の `Object` + `object_handle.rs` の `ObjectHandle` / `ObjectValue`（共有 identity・qpdf 互換 parsed offset・`IndirectState` 遅延解決・Pdf identity provenance） | 🔀 `object.rs` の `Object` は静的な値表現のみ。`QPDFValue` 相当の共有 identity・parsed offset・遅延解決状態は `object_handle.rs` が担う。production route への cutover は完了済み（`flpdf-egzr.3.1`）。Pdf identity provenance は live containment から分離して detach 後も保持する。旧 raw `Object` route の最終削除は `flpdf-egzr.3.2.8`（open）待ち。両モジュールに分割されているため `✅` から変更 |
| `QPDFObjGen.cc` | 68 | `object.rs` の `ObjectRef` | ✅ |
| `QPDFXRefEntry.cc` | 51 | `xref_entry.rs`（`XrefEntry` = free / uncompressed / compressed の 3 variant）。consumer は `xref.rs` / `reader.rs` / `cache.rs` / `writer.rs` / `writer/{object_streams,plain/plan}.rs` / `linearization/{writer,plan}.rs` | ✅ `flpdf-qxba.9.2` で完全 cutover（`XrefOffset` 削除）。`xref.rs` 側に型定義は残っていない |
| `PDFVersion.cc` | 68 | `pdf_version.rs` の `PdfVersion` | ✅ |
| `QPDFMatrix.cc` | 140 | `matrix.rs` の `Matrix` / `Rectangle` | ✅ |
| `QPDFObjectHandle::mergeResources` / `shallowCopy` | `QPDFObjectHandle.cc:1063-1147,2072-2079` | `object_handle.rs:3692` + `page_annotation_flatten.rs:666-740`（widget appearance の既定リソース consumer） | ✅ live `ObjectHandle::merge_resources` を使用。missing category は top-level が direct の shallow copy になり、nested indirect child は handle を保持する。array の `isScalar` 判定と unique-name pool の second-level dictionary 判定は qpdf と同じく各 nested handle を解決し、解決エラーを伝播する。`overlay_annotations.rs` の `merge_resources_shallow` は別責務の name-conflict overlay merge として残る |

## 2. パース / 読み取り

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF.cc` | 2667 | `engine.rs`(475: `Pdf::empty`、ほか8つの public factory — `Pdf::open` / `open_with_repair` / `open_best_effort` / `open_with_options` / `open_mem` / `open_mem_with_options` / `open_mem_owned` / `open_mem_owned_with_options` —、`open_with_repair_mode`、`NEXT_PDF_ID`、`MAX_RESOLUTION_FALLBACKS`。`emptyPDF` / `processFile` / `processMemoryFile` の construction path) + `pdf.rs`(297: `Pdf<R>` container、`Drop` = `QPDF::~QPDF`、version/trailer/root/extension/page-enumeration-state accessors。`QPDF.hh:1438-1518`; `QPDF.cc:215-232,2323-2358,2647-2651`) + `reader.rs`(8185: object resolution, recovery, diagnostics, authentication, and `Pdf::get_xref_table` / `Pdf::get_all_objects`) + `reader/resolver.rs`(2367: canonical resolver。`QPDF::resolve` が触る `QPDF::Members` — `m->file` / `m->xref_table` / `m->obj_cache` / `m->resolving` / `m->resolved_object_streams` / `m->attempt_recovery` / `m->encp` — を `ResolverCore` に集約し、`Rc<RefCell<..>>` 経由で `ObjectHandle` の `Weak<dyn DocumentResolver>` から到達可能にする。`m->obj_cache` は canonical handle registry そのもので、`Pdf::get_object_handle`（= `QPDF::getObject`, `QPDF.cc:1952-1959`）と `Pdf::drop`（= `~QPDF`）の両方がここを見る。`Pdf::get_xref_table` は `QPDF::getXRefTable`（`QPDF.cc:2370-2377`）の effective source table snapshot、`Pdf::get_all_objects` は `fixDanglingReferences` と `m->obj_cache` enumeration（`QPDF.cc:1258-1294`）を canonical handle 上で実行する。`m->encp`（`flpdf-25kg.3.11`）は `Pdf::encryption` と同一の `Rc<RefCell<Option<EncryptionState>>>` を共有し、qpdf の `shared_ptr<EncryptionParameters>` を複数の owner が保持する形を再現する。`pipe_stream_data` は `QPDF::pipeStreamData` と同じく source read 前に `QPDF::decryptStream` 相当を呼び、同じ cell の method state / object-key cache を更新して AES/RC4 stage を前置する。`flpdf-25kg.3.5`/`.3.5.1`（ともに close 済み）で `readObjectAtOffset`/`readObject`/`readStream` の全 xref 形式（uncompressed type 1・ObjStm・canonical type-1 stream framing recovery を含む）が canonical resolver へ移植済み。`reader.rs`/`xref.rs` 自身の filter 呼び出し箇所の consumer cutover も `flpdf-egzr.3.2.10`（子 `.3.2.10.1`/`.3.2.10.2` close 済み、PR #859 merged）で完了し、production 経路の `decode_stream_data`/`encode_stream_data` 呼び出しはテストコードのみに残る。resolve 時文字列復号と pipe 時ストリーム復号 primitive は移植済み。残る raw `Object` route（`resolve_borrowed` と repair/recovery 経路）の削除は `flpdf-egzr.3.2.8`（open）が担当) + `reader/file_object.rs`(1405) + `xref.rs`(1220) + `object_copy.rs`(342: `copyForeignObject`) + `cache.rs`(112: xref 由来の `ObjectCache` / `CacheEntry`。消費者は `reader.rs`) + `writer/object_streams.rs`(207-237: `compressible_objgens_qpdf_plan` = `getCompressibleObjGens`、`QPDF.cc:2392-2445`)  + `signatures.rs`(245-: `removeSecurityRestrictions`) + `page_closure.rs`(441: `page_object_closure`。`object_copy.rs` は pre-closed な集合しか受け取らず、両者で `copyForeignObject` 相当を構成する) + `ref_chain.rs`(159: `resolve_ref_chain` / `terminal_ref_of_chain` / `MAX_REF_CHAIN_DEPTH` — 深さ上限付き間接参照解決の共有プリミティブ。20 モジュールが使用) | 🔀 |
| `QPDF.cc`（xref registration/recovery と mutation 境界） | `516-607,686-708,1187-1210,1996-2005` | `xref.rs` の `XrefRegistration` が xref 読み取り・recovery merge ごとの object-number-wide `deleted_objects` free-row filter を所有する。通常の `read_xref` は `/Size` 整合性検証までこの set を使い、その後 clear する（`:686-708`）。一方 `reconstruct_xref` の line scan は `:575` で clear してから `:576-607` の candidate xref-stream re-read に進み、その re-read は fresh registration を持つ。`ResolverCore` にはいずれの一時 state も渡さない。`reader.rs` の `Pdf::set_object` / `replace_object_handle` は canonical cache replacement だけを担い、この xref set を clear/add しない。canonical xref/cache removal と outstanding handle の null 化は `remove_object_handle` が担う。 | ✅ |
| `QPDFParser::parse` / `QPDF::readObject`（indirect handle生成とstream framingの境界） | `QPDFParser.cc:155-172`; `QPDF.cc:1331-1349` | `parser.rs::parse_qpdf_file_object_handle_with_diagnostics` がtokenize中にindirect `ObjectHandle`を生成し、`xref.rs::BootstrapHandleDocument` がpre-`Pdf`のObjStm/xref bootstrapで同じhandle graphを使う。stream tailのframingはcaller側で継続する。 | 🔀 pre-`Pdf` bootstrap ownerはqpdf parserの一時contextに対応し、post-openのcanonical resolver (`flpdf-25kg.3.5`)とは分離している |
| `QPDF.hh`（`EncryptionParameters`） | 899-921 | `reader.rs`(54-69: `EncryptionState`)。qpdf は独立した2つの bool、`encrypted` / `encryption_initialized`（`QPDF.hh:907-908`）を持つが、flpdf はこれを単一の `Option<EncryptionState>`（`None` = 未初期化 or 認証済み未暗号化のいずれか、`Some` = 認証済み暗号化）に畳んでいる。安全性の根拠: `encryption_initialized` の唯一の用途は `initializeEncryption()`（`QPDF.cc:471` で1文書につき高々1回しか呼ばれない）内の再入防止ガード（`QPDF_encryption.cc:721,724`）で、flpdf の構造上この再入自体が起こり得ないため観測可能な挙動差は生じない。逸脱理由は `reader/resolver.rs` の `ResolverCore::encryption_parameters` doc にも記載（`flpdf-25kg.3.11`） | ⚪ |
| `QPDF::interpretCF` (`QPDF.hh`; `QPDF_encryption.cc`) | `1122-1127`; `700-716` | `reader.rs` の `interpret_cf_name` / `interpret_cf` / `interpret_cf_from_handle` | ✅ 値選択を共有し、ObjectHandle 版は `try_as_name` で lazy resolve。`crypt_filters` → built-in `/Identity` → `e_unknown`、non-name → `e_none` の順と resolver error 伝播を維持。`reader/resolver.rs` の pipe-time `decryptStream` consumer が live stream dictionary に対して使用する |
| `QPDF::decryptStream` (`QPDF_encryption.cc`) | `1045-1153` | `reader/resolver.rs` の `inspect_stream_encryption` / `pipe_stream_data` | ✅ `/XRef` early return、`/V >= 4` gate、typed direct `/Crypt` と equal-length array pairing、Crypt-before-Metadata precedence、unknown warning + `cf_stream` rewrite、qpdf の object-key cache、`PlAesPdf` / `PlRc4` 前置を source read 前に実行。stream dictionary の lazy resolve 中は encryption cell borrow を保持しない。legacy resolve-time payload 復号は consumer cutover まで維持 |
| `QPDFParser.cc` | 519 | `parser.rs` の `LiveInput` / `LiveTokenSource` / `LiveFileParser` は `InputSource` を一度だけ前進する file-object baseline（`QPDFParser.cc:27-518`）。canonical resolver の uncompressed type-1 consumer と、decoded-stream-relative `SliceLiveInput` 経由の ObjStm member consumer（`reader.rs::parse_object_stream_entry`）が使い、token 終端の one-character unread、diagnostic、top-level/nested/container/null の parsed offset、empty/dictionary/bad-token/depth recovery をここで共有する。uncompressed 側は canonical unresolved handle を同時に生成する。live canonical と context-none explicit の parser invocation は qpdf の parse-call description template を非 null handle に stamp し、container の render shift と null の無記述も維持する。`ObjectHandle::parse` は同じ live parser の context-none entry point で、warning を `Error`、nested `N G R` を `Error::Internal`、非 C whitespace の後続を parse error にする | 🔀 canonical uncompressed consumer は `StringDecrypter`（`flpdf-25kg.3.17`）を object-ref と shared `EncryptionState` に束縛し、`QPDF::readObject` / `QPDFParser` と同様に top-level・array・nested dictionary・stream dictionary の `tt_string` だけを token 時に復号する（`QPDF.cc:1331-1340`; `QPDFParser.cc:114-121,327-365`; `QPDF_encryption.cc:977-1039`）。完成した `/Type /Sig` + `/ByteRange` 辞書だけは raw `/Contents` bytes と parsed offset を復元する。ObjStm / context-none explicit parse / content mode は decrypter を渡さず、unknown word も callback 非呼出し。Content mode は既存 `Parser` を維持し、file-object live parser は content grammar を兼用しない |
| `QPDFTokenizer.cc` | 965 | `tokenizer.rs`（18 token types、owned value/raw/error bytes/offset、push/pull、pull-only `allowEOF`、`includeIgnorable`、space/comment、bad-token recovery、max length、`betweenTokens`、unread、inline-image `EI` discovery。`QPDFTokenizer.hh:34-193`; `QPDFTokenizer.cc:45-965`）+ `parser.rs` の content mode + `content_stream.rs` の `ParserCallbacks` orchestration + `object.rs` の `Operator` / `InlineImage`（`QPDFParser.cc:27-125,130-377`; `QPDFObjectHandle.cc:1770-1847`） | ✅ `QPDFTokenizer` の責務境界を移植済み。object/parser/content callback consumers は共有 tokenizer を使用し、旧 content lexer は削除 |
| `InputSource` 系 5 ファイル | 625 | `Read + Seek` ジェネリクスで代替。所有者は `reader/resolver.rs` の `ResolverCore`（`m->file` 相当）。`ResolverCore` のメソッドは `InputSource` の 3 操作 `seek`/`tell`/`read`（`InputSource.hh:71-74`）に限定し、`OffsetInputSource`（`QPDF.cc:406`）が担う header shift は `seek`/`tell` が適用する。例外は `rewind_underlying_source` 1 つで、これは wrapper が持つ `proxied`（`libqpdf/qpdf/OffsetInputSource.hh:24`）に相当する — `OffsetInputSource::rewind` は logical 0 に行く（`OffsetInputSource.cc:55-59`）ため `m->file` では表現できない。owned-window 系の legacy helper（`read_window` / `read_physical_input`）は `ResolverHandle` 側の `qpdf-legacy-tenant` で、`ResolverCore` の面には置かない | ⚪ |
| `QPDF_pages.cc` | 319 | `pages/repair.rs`（`QPDF_pages.cc:39-75` の `getAllPages` root correction と `:77-150` の `getAllPagesInternal` repair/enumeration を canonical `ObjectHandle` graph 上で実装） + `optimization/inherited_attrs.rs`（canonical page promotion/clone と衝突しない `Pdf::next_obj_gen` allocation） + `pages.rs` / `pages/tree_rebuild.rs`（flatten/insert/remove と legacy consumer の残り） | 🔀 `flpdf-25kg.3.7` で repair/enumeration の canonical route を追加。`.3.2.6.15` では `QPDFPageObjectHelper::getAttribute` の bottom-up `/Parent` climb（`QPDFPageObjectHelper.cc:217-263`。`QPDF_optimization.cc:121-245`/`QPDF_pages.cc:154-180,205-248` は top-down push とツリー変異のオラクル）を、共有 `PageParentCursor` / `resolve_inherited_handle_with_max_depth` として live `ObjectHandle` で切り出した。直接親の identity、間接親の canonical `ObjectRef`、null/非辞書親、cycle/depth guard をこの境界で保持し、`/Rotate` の未指定を合成しない。`.3.2.6.16` では `tree_rebuild` の単一文書 consumer を canonical handle route に切り替え、選択ページの inherited `/MediaBox`・`/CropBox`・`/Resources`・`/Rotate` を再親子付け前に push、直接 non-scalar は `make_indirect_object_handle` で一度だけ昇格、既存 indirect 値は identity を保持し、duplicate は `shallow_copy`、root `/Kids`・`/Count`・各 leaf `/Parent` は live handle を replace/remove する。qpdf の absent `/Rotate` は合成しない。`QPDFObjectHandle.cc:1199-1209,2072-2079` の live replace/remove・shallow-copy がこの consumerの mutation oracleである。`QPDFJob.cc:2360-2632` の page-selection orchestration はこの境界の外であり、`page_extract` の foreign-copy raw `Object` 戻り値だけは明示的な materialize bridge として同ファイルに隔離している。`page_merge` / `page_closure` / `page_label` は別 consumer のまま残る |
| `QPDFExc.cc` / `QPDFSystemError.cc` | 123 | `error.rs`(125) | ✅ |

`flpdf-egzr.3.2.6.19` の `pages/tree_rebuild.rs` は、`QPDF_optimization.cc:159-228`
に合わせて選択ページへ inheritable attributes を materialize した後、元の page-tree
に残る `/Pages` node から `/MediaBox`・`/CropBox`・`/Resources`・`/Rotate` を remove する。
保持する root 以外の中間 node は引き続き orphan として `subset_prune` の xref-level GC に
委ねるが、writer が orphan を保存する場合にも qpdf の flattening-side cleanup を保つ。
`--pages` の CLI consumer で qpdf 11.9.0 と同じ root/kids/leaf の正規化 shape を
比較する回帰テストは `cli_pages_root_inheritable_qpdf.rs` が所有する。

`flpdf-egzr.3.2.6.26` では、subset extraction 後の name-level resource prune を
document-wide の独自 aggregate route ではなく、保持された各 leaf の
`PageObjectHelper::remove_unreferenced_resources` へ委譲する形に揃えた。これは qpdf の
`QPDFPageObjectHelper.cc:539-649` に合わせた parse-gated な page-local route であり、
剪定対象は `/Font` と `/XObject` のみ（各 category は shallow copy 後に変更）である。
旧 aggregate API とそれ専用の回帰テストは、qpdf 11.9.0 に対応物がないため削除した。
`QPDFJob.cc:2251-2337` の Auto 判定は tree rebuild 前に済ませ、`subset_prune` はその
結果が prune を許可した場合だけこの per-page route を実行する。xref-level の orphan
mark-and-sweep は引き続き `subset_prune` の責務として残す。共有 `/XObject` category、
継承 `/Resources`、非対象 resource category、重複ページの差分回帰は
`crates/flpdf-cli/tests/cli_tests.rs` が qpdf 11.9.0 と比較する。

## 3. 書き込み — 最大の smear

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFWriter.cc` | 3044 | `writer.rs`(4494) + `writer/serialize.rs`(1008) + `writer/object_streams.rs`(739) + `writer/encryption_state.rs`(258) + `writer/encrypted_strings.rs`(213) + `writer/plain/{plan,body,xref}.rs`(898) + `linearization/writer.rs`(3603) + `linearization/part1.rs`(370) + `linearization/back_patch.rs`(324) + `linearization/renumber.rs`(850) + `rewrite_renumber.rs`(893) = **13,650 行 / 13 ファイル**。加えて `object.rs`(412: `write_pdf` = `unparseObject` / 491: `write_pdf_qdf` / 585-: trailer `/ID` = `writeTrailer`。`writer.rs` と `linearization/writer.rs` が委譲) と `qpdf_null.rs`(38-57: `visible_entries` = `QPDFWriter.cc:1491` の null 値 dict キー抑制)。さらに `object_handle.rs`(1705-: `unparse_object` / 1745-: `unparse_object_qdf` / 2302-: `unparse_stream_body` / 2375-: `unparse_stream_body_qdf` / 2569-: `unparse_trailer` = `unparseObject`(`QPDFWriter.cc:1318-1605`、dict 分岐 `:1346-1527`、stream 分岐 `:1528-1605`) / `writeTrailer`(`:1160-1230`) の `ObjectHandle` 版。`object.rs` の materialize-to-`Object` bridge を経由せず `ObjectHandle` のグラフを直接歩く新 primitive 群（`flpdf-egzr.3.2.13`）。`unparse_stream_body_qdf` は最終レビューで見つかったギャップの修正（Task 9）: `write_pdf_stream_qdf`(`object.rs:1036`、real production callsite は `writer.rs:4437`)に対応する QDF+stream 形の primitive が欠けていた。`Dictionary::write_pdf_stream_qdf` 自身に `refiltered` 概念が無いため（唯一の呼び出し元 `write_stream_to_buf_qdf` は既に確定済みの `/Filter`/`/Length` を持つ dict しか渡さない）、`unparse_stream_body`（compact 版）と異なりこちらも `refiltered` パラメータを持たない。null 値 dict キー抑制(`:1490-1491`)は `try_is_null` 経由で `unparse_object`/`unparse_object_qdf`/`unparse_stream_body`/`unparse_stream_body_qdf` の4つに適用し、`unparse_trailer` は `writeTrailer` 自身と同様に無抑制。`writer/encryption_state.rs` の `WriterEncryptionState` は `QPDFWriter::Members` の暗号 state (`QPDFWriter.hh:641-663`)、`set_data_key` は `setDataKey` (`QPDFWriter.cc:842-847`) と `compute_data_key` (`QPDF_encryption.cc:325-356`)、`with_object_data_key` は非 ObjStm member の set/unparse/clear (`QPDFWriter.cc:1761-1796`) に対応する。source ID ではなく emitted ID と generation 0 を使い、`Option<u32>` が qpdf の `-1` sentinel を置換する。qpdf の明示 clear は正常系だけだが、Rust callback の `Err` 後にも clear するのは出力 byte を変えず stale state を残さない内部代替である。全て `pub(crate)`・`#[allow(dead_code)]`。`flpdf-a32l` は AES で暗号化済みの文字列を full / linearized writer の共通 serializer context で強制 hex 化し、RC4・非暗号化・ObjStm member は既存の heuristic を維持する（`QPDFWriter.cc:1567-1592`）。既存 primitive の production consumer 移行（`flpdf-egzr.3.2.5` + 子 `.5.1`〜`.5.4`）と暗号 state の consumer 移行（`flpdf-3yn9.11`/`.12`）はいずれも close 済みで、`PlAesPdf`/`PlRc4`/`run_writer_pipeline`/`adjust_aes_stream_length` は production コードで実使用されている。🔀 の根拠は ObjectHandle 移行の未完了ではなく、下記の「xref 出力が 3 箇所に分かれる」構造的 smear が独立に残っていること | 🔀 |

進捗計測の準備境界は `writer/pdf_writer.rs:499-522` に固定する。QDF/content-normalization
または non-none decode level の `PageDocumentHelper::get_all_pages()` による page-tree
修復を先に実行してから `get_object_count()` を取得し、qpdf の
`qdf_mode || normalize_content || stream_decode_level` による `doWriteSetup`→progress
snapshot (`QPDFWriter.cc:2114-2116,2189-2193`) と同じ順序で、修復が mint した indirect
object も `events_expected` に含める。specialized writer の同じ repair 境界は
`writer.rs:3010-3025` にある。linearized writer は既存の準備後 snapshot を維持する。

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
`ObjectHandle::{unparse_object_with_string_writer,unparse_object_qdf_with_string_writer}`
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

**renumber は重複していない**: `rewrite_renumber.rs` は `linearization/plan.rs` からも
使われる共有機構で、`linearization/renumber.rs` はその上に載る最終採番層。qpdf の
`obj_renumber` 1 本に対して 2 層構造だが、二重実装ではない。

## 4. 線形化 / 最適化

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF_linearization.cc` | 1796 | `linearization/`（`plan.rs` 3032, `hint_*` 1651, `check.rs` 726, `show.rs` 995, ほか）≒ 8,000 行 | 🔀 実装は 5+ モジュールに分散したまま（`optimization.rs` が達成したような単一モジュールへの集約は未達）。ObjectHandle 移行自体は完了: producer 側（`flpdf-3yn9.4`、plan.rs + hint_\*）と consumer 側（`flpdf-egzr.3.2.9`、check.rs + show.rs）が close 済みで、`check_consumer_production_uses_the_canonical_object_handle_route` / `show_consumer_production_uses_the_canonical_object_handle_route` が production 経路から `Object::` / `resolve_borrowed` / `decode_stream_data` / `page_refs` が消えたことを機械的に保証する。唯一の例外は `plan.rs` の `collect_direct_refs`（Object 版）で、`linearization/writer.rs::resolve_catalog_adbe_status`（§3 領域）専用の未解決 raw 値 shape-only walk であり、closure 計算本体は同ファイルの `collect_direct_handle_refs`（ObjectHandle 版）が担う。線形化書き込み経路自体（writer.rs 側、`flpdf-3yn9.5` 系列）は issue タイトルが明記する通り §3 `QPDFWriter.cc` のスライスであり本行の対応先ではない |
| `QPDF_optimization.cc` | 381 | `optimization.rs`（optimization orchestration、inherited-page preparation、object-user maps、compressed-object folding）+ `optimization/inherited_attrs.rs`(575) | ✅ `flpdf-qxba.9.3` / `.9.4` で完全 cutover。`linearization/plan.rs` 側に `ObjUser` / `update_object_maps` は残っていない |

`ObjUser` 分類（`ou_page` / `ou_thumb` / `ou_trailer_key` / `ou_root_key`）と
`updateObjectMaps` は `optimization.rs` に移設済み（`flpdf-qxba.9.3` / `.9.4`）。
`linearization/plan.rs` は consumer として呼ぶだけになった。

**objstm 経路の解錠は無い**: qpdf でも `optimize()` の呼び出し元は
`QPDF_linearization.cc:495` と `QPDFWriter.cc:2553`（`writeLinearized()` 内）のみで
linearize 専用。`flpdf-g6hb` が必要とする `getCompressibleObjGens` は
`QPDF.cc:2393` にある別物。

## 5. 暗号

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF_encryption.cc` | 1410 | `security/standard.rs`(1879) + `writer.rs` の encryption context(~700) + `encrypt_setup.rs`(213) + `permissions.rs`(206) + `security/password.rs`(100: `normalize_password` — auto/bytes/hex-bytes/unicode、SASLprep、revision 依存の切り詰め。`PasswordMode` は `lib.rs:233` から re-export され CLI の `--password-mode` が選択、`reader.rs:604` が呼ぶ) | 🔀 |
| `rijndael.cc` / `AES_PDF_native` / `MD5_native` / `SHA2_native` | 1668 | `security/primitives.rs`(188: AES と MD5) + `pipeline/sha2.rs` の `Sha2Digest`(SHA2)（外部 crate）。qpdf は `SHA2_native` へ `Pl_SHA2` 経由でしか到達しない（`QPDF_encryption.cc:246,296` が唯一の production 利用）ため、RustCrypto の SHA-2 hasher も `Pl_SHA2` 移植の内部に閉じている。`security/primitives.rs` の一括 `sha256`/`sha384`/`sha512` wrapper は consumer cutover で削除済み | ⚪ |
| `RC4.cc` / `RC4_native.cc` | 63 | `security/rc4.rs`(80)（明示長キー / C-string キー、state 保持、separate / in-place processing） | ✅ |
| `QPDFCryptoProvider.cc` / `QPDFCrypto_*` | 774 | provider 抽象が無い | ⚪ |
| ランダム源 3 ファイル | 185 | `writer.rs` の `fresh_id_bytes` 等に散在 | 🔀 |

## 6. Pipeline / フィルタ

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `Pipeline.cc`（積層シンク基盤のみ。個々の `Pl_*` は下記の各行で個別に分類） | 114 | `pipeline.rs`（public `Pipeline` trait、identifier/write/finish lifecycle、logic/runtime error channel）。⚪ qpdf が bare `Pipeline*` で回す `next` slot に対応する `PipelineRef`（borrowed / owned の 2 択）を持ち、`Flate` / `LzwDecoder` の `next` はこれを受け取る。逸脱の本体（stage の所有者）は下の `QPDFStreamFilter.cc` 行に記載 | ✅ |
| `Pl_Count.cc` | 48 | `pipeline/count.rs`（byte count、last byte、forwarding、finish lifecycle） | ✅ |
| `Pl_MD5.cc` | 66 | `pipeline/md5.rs`（enable/persist/reuse、hex digest、forwarding/error order）+ `filespec_helper.rs`（EmbeddedFile `/Params /CheckSum` production consumer） | ✅ |
| `Pl_Flate` / `SF_FlateLzwDecode` | 946 | `pipeline/flate.rs` + `stream_filter.rs` の `FlateLzwStreamFilter`（`/Predictor` `/Columns` `/Colors` `/BitsPerComponent` `/EarlyChange` の解釈、codec → predictor の chain 構築、`QIntC::to_uint` の range error timing）。`SF_FlateLzwDecode::getDecodePipeline`(`SF_FlateLzwDecode.cc:75-110`) 相当の `decode_pipeline` を持つ。構築順（sink 側から、predictor を作って `next` を差し替えてから codec を作り、その codec を返す）は qpdf のままで、内側になる predictor だけ `PipelineRef::Owned` が所有する。既知の逸脱: whole-buffer route の `pipe_codec` は `Pl_Flate` の warn callback を stage 構築側で設置するが、qpdf は `getDecodePipeline` の呼び出し側(`QPDF_Stream.cc:564-567`)で設置する。この route が構築する `Pl_Flate` はいずれも qpdf が当該 filter の iteration で設置するのと同じ callback を受け取るので、警告の文言・順序は変わらない。再現できないのは qpdf のもう一方のケース — cast は stage 単位ではなく filter 単位で 1 回走り(`:561-563` の guard の外)、stage を構築しない filter の iteration では別の場所で構築された stage に当たる。設置位置とこのケースは共に `QPDF_Stream::pipeStreamData` 移植の担当（`decode_pipeline` 側は qpdf 通り callback を設置しない） | ✅ |
| `Pl_LZWDecoder` | 189 | `pipeline/lzw.rs`（3-byte rotating buffer、1 入力 byte あたり 1 code、table 成長と code 幅遷移、eod latch、qpdf の 7 種の診断文言）+ `stream_filter.rs` 経由の production decode | ✅ |
| `Pl_PNGFilter` | 232 | `pipeline/png_filter.rs`（32-bit wrapping の row 幅算出、constructor の 3 種 rejection、未知 filter byte の無視、finish の zero-pad row、Up 固定 encoder）+ `filters.rs` / `writer/serialize.rs` の production consumer。⚪ row buffer の確保だけは constructor ではなく最初の write まで遅延（出力バイト・呼び出し境界・エラー timing に影響しない） | ✅ |
| `Pl_TIFFPredictor` | 175 | `pipeline/tiff_predictor.rs`（incremental row buffering、8-bit の byte differencing、packed sample の signed MSB bit I/O、finish 時の zero padding）+ `stream_filter.rs` / `filters.rs` の Predictor 2 production consumer。qpdf の TIFF fixture vectors と construction/write/finish error timing を pin。qpdf が filter instance に保持する stage ownership は、flpdf では `PipelineRef::Owned` が内側 predictor を保持する意図的な Rust ownership substitution | ✅ |
| `Pl_ASCII85Decoder` / `SF_ASCII85Decode` | 108 + 31 | `pipeline/ascii85_decoder.rs` + `stream_filter.rs`（`SF_ASCII85Decode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_ASCIIHexDecoder` / `SF_ASCIIHexDecode` | 96 + 31 | `pipeline/ascii_hex.rs` + `stream_filter.rs`（`SF_ASCIIHexDecode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_RunLength` / `SF_RunLengthDecode` | 146 + 38 | `pipeline/run_length.rs` + `stream_filter.rs`（`SF_RunLengthDecode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_AES_PDF` | 200 | `pipeline/aes.rs`（qpdf の contract を全量移植: block 単位の write バッファリング、first-block を IV として消費する復号側と IV を先頭へ書く暗号化側、ISO 32000-1 7.6.2 の padding とその strip、`useZeroIV` / `setIV` / `useStaticIV` / `disablePadding` / `disableCBC`）＋ `security/standard.rs` の AES single-buffer helper と `writer.rs` の stream consumer | 🔀 `reader/resolver.rs` の `QPDF::decryptStream` 対応は `PlAesPdf` を source-read pipeline の前段へ接続済み。legacy resolve-time single-buffer helper は consumer cutover まで併存するため、同じ qpdf モジュールが 2 箇所に存在する。⚪ `QPDFCryptoImpl::rijndael_init` / `rijndael_process` の crypto provider 抽象は `aes` / `cbc` crate の直接利用に置換（§ 逸脱候補の crypto provider 行と同じ代替）。block ごとに 1 回 process する呼び出し形は保持し、chaining 状態のみ provider 側ではなく cipher が持つ。既知の逸脱: qpdf の padding strip は PKCS#7 厳密ではなく（末尾バイトが不整合ならブロックを丸ごと残す、`Pl_AES_PDF.cc:184-196`）、既存の `security/primitives.rs` の `decrypt_padded::<Pkcs7>` は同じ入力を `Err` にする。`pipeline/aes.rs` は qpdf 側に合わせてあるので、cutover 前後で受理する文書が変わる |
| `Pl_RC4` | 43 | `pipeline/rc4.rs`（65,536-byte既定buffer、stateful `security/rc4.rs`、write/finish lifecycle）+ `reader/resolver.rs` の pipe-time decrypt stage + `reader.rs` / `writer.rs` の既存 stream consumer | ✅ |
| `Pl_QPDFTokenizer.cc` / `ContentNormalizer.cc` | 141 | `pipeline/qpdf_tokenizer.rs`（optional downstream を持つ token-filter runner、EOF-token → `handle_eof`、`ID` separator 注入、inline-image 切替、raw token/discard/output、`handle_eof` 成功後の永久 detach と finish/error timing）+ production consumer `content_normalizer.rs`（bad-token state、CR/string/name normalization） | ✅ |
| `QPDFObjectHandle::TokenFilter` / `QPDF_Stream::addTokenFilter` / `isDataModified` | `QPDFObjectHandle.hh:129-190,420-475,978-1010`; `QPDF_Stream.cc:321-324,488-620,663-666` | `ObjectHandle::add_token_filter` / `is_data_modified` が共有filter listとdecoded→token-filter→normalize/encodeのlazy pipeを担う。`form_field_object_helper/rendering.rs` の既存 `/AP/N` reuse は eager `replace_stream_data` からqpdf `ValueSetter`相当の `AppearanceTokenFilter` へ移行し、`writer/plain/body.rs` は `is_data_modified` をlone-Flate fast pathの条件に含める。`linearization/writer.rs` の `append_body_object`（`stream_is_data_modified` helper 経由）も同じ `willFilterStream` 由来のゲートを適用: qpdf の `writeLinearized` は `QPDF::optimize` の `skip_stream_parameters` probe と実書き込みの計2回 `pipeStreamData` を呼び、token filter は pipe 間で状態リセットしないため実書き込み側は exhausted filter のパススルー（= stale content）を再エンコードする。flpdf の linearized writer には optimize 相当の二重 pipe が無いため、token filter 自体は起動せず「既に materialize 済みの (pre-filter) バイトを decode→re-encode」するだけで同じ observed output に一致させる（`docs.rs` 非公開のモジュール内 doc 参照）。`writer.rs` の `emit_canonical_pdf_inner` fallback と `writer/plain/body.rs` の `!plan.canonical` 分岐について、同種のゲート配線の要否を `flpdf-vkka` で検証済み（close）: `plain/body.rs` は既に canonical handle 経由で `is_data_modified()` を参照しており、`emit_canonical_pdf_inner` 側は PR #831 の `materialize_for_normalization` narrowing 後、`Object::Stream` 分岐が構造的に到達不能なため追加配線は不要と確認された | ✅ |
| `QPDFStreamFilter.cc` | 19 | `stream_filter.rs`（`set_decode_params`、decode pipeline factory、specialized / lossy の既定分類）。`QPDFStreamFilter::getDecodePipeline`(`QPDFStreamFilter.hh:46-49`) に対応する `StreamFilter::decode_pipeline` は `stream_filter_for` が返す全 filter が実装し、`None` は qpdf の `nullptr`（11.9.0 でこれを返すのは `SF_Crypt` だけ、`QPDF_Stream.cc:52-56`）。qpdf-shaped の production caller は `ObjectHandle::pipe_stream_data` に接続済みで、public whole-buffer decode helper は従来経路として併存する。**⚪ (B) stage の所有者**: qpdf は構築した stage を filter instance 内に保持し呼び出し側へは非所有ポインタを返す（`QPDFStreamFilter.hh:47` が「pipeline は自クラスの instance 破棄時に delete されること」を要求し、`SF_FlateLzwDecode.cc:88`・`:108` の `pipelines.push_back` がそれを果たす）。flpdf は stage を値で返し、多段 chain の内側 stage は `pipeline.rs` の `PipelineRef::Owned` が持つ（上の `Pipeline.cc` 行も参照）。構築順・stage 数は不変で動くのは所有者だけ。出力バイト不変の根拠（分類 (B) 条件 1）は、この slot を通る本番 write path（`filters::encode_stream_data` → `encode_flate` の deflate）を `qpdf-zlib-compat` gated の `cmp_generate_objstm_tests` が qpdf golden で pin していること。**⚪ (B) registry の入れ物**: filter 名の registry は qpdf が `filter_factories` の `std::map`(`QPDF_Stream.cc:85-94`)、flpdf は `stream_filter_for` の `match`。この map は iterate されず名前引き(`:425-426`)にしか使われないので入れ物としては等価だが、run time に map を書き換える `QPDF_Stream::registerStreamFilter`(`:148-151`) に対応する API は flpdf に無く、`match` のままでは追加できない。入れ物は等価でも登録集合は等価でなく、qpdf の `SF_DCTDecode` (`SF_DCTDecode.hh:8-40`) / `/DCTDecode` factory (`QPDF_Stream.cc:91`) に対応する arm は `stream_filter.rs` の `b"DCTDecode" => Some(Box::new(DctStreamFilter))` として存在し、canonical `decode_pipeline` へ接続する。legacy whole-buffer route は passthrough-only のまま（writer passthrough は別責務、下の `Pl_DCT.cc` row が対応を記録）。`QPDF_Stream::filterable`(`QPDF_Stream.cc:378-485`) 相当の `/Filter` `/DecodeParms` shape 読み取りも同モジュールが持ち、`flpdf-25kg.3.4` 以降は `Object` 版(`decode_filter_specs_from_object`) と `ObjectHandle` 版(`decode_filter_specs_from_handle`) の 2 つの shape reader が同じ分岐順で共通の `FilterSpec` を組む。下流（codec stack、predictor geometry、`max_output`、warning 順序）は `filters.rs` の `decode_prepared_specs` 1 実装を共有（本番 caller は shape ごとに 1 つずつの計 2 つ）。ただし `max_filter_chain` は shape reader 側で適用するため呼び出しが 2 箇所に分かれる。各 reader 単体の precedence は絶対値テストで pin 済みで、2 者間の整合を見るのは `handle_reader_matches_object_reader_for_every_filter_shape`。2 reader が一致するのは direct な子までで、間接参照を解決するのは `ObjectHandle` 版のみ（qpdf のアクセサ側に合わせた意図的な差）。`ObjectHandle` 版の decode entry point(`filters.rs` の `decode_stream_data_from_handle` / `decode_stream_data_recovering_from_handle`) は本番 caller をまだ持たず、`flpdf-25kg.3.5` の resolver 配線で接続される。既知の逸脱: unfilterable ケースで qpdf は warning を出したうえで `getStreamData` を失敗させるが flpdf は同文言を warning 無しの `Err` としてのみ返す（D3）。handle reader は retained-key reduction 前に `try_get_keys` を用いて direct・indirect・dangling の nullish entry を省き、legacy reader は non-resolving の責務内で同等に direct-null を省く。**⚪ DecodeParams の所有 snapshot**: qpdf は `/DecodeParms` を `QPDFObjectHandle`（`shared_ptr`）のまま filter chain に複製するのに対し、flpdf の `DecodeParams` は所有 snapshot なので consumer が読むキーだけを保持する（`RETAINED_DECODE_PARAM_KEYS` の geometry 5 キーは全 filter で、`Crypt` 段は全キー。`SF_Crypt::setDecodeParms`(`QPDF_Stream.cc:33-50`) が `getKeys()` を走査して `/Type`・`/Name` 以外のキーで `filterable = false` にするため、`Crypt` 段の保持キー集合は filterability そのものを決める — キーを落とすと qpdf が拒否する stream を受理してしまう。name の byte 列を持つのは `CRYPT_NAME_PAYLOAD_DECODE_PARAM_KEYS` の 2 キー、すなわち crypt provider が読む `/Name` と `isDictionaryOfType("/CryptFilterDecodeParms")` が比較する `/Type` だけで、`Crypt` 段の未知キーを含む他の slot の name は `ParamValue::Other` に落ちる — その 2 キー以外で qpdf が非整数の種別を見る箇所は無く、`SF_FlateLzwDecode` は `isInteger()` だけを問う）。出力バイト・エラー timing には影響しない（書き出し側は source dictionary をそのまま複製し、この型から `/DecodeParms` を再構築しない）。filterability は逆にこの snapshot から決まるので、保持集合は各 `setDecodeParms` が読む集合と一致させる必要がある（`SF_FlateLzwDecode` は名前の無いキーを `else` 無しで無視するので geometry 5 キーで足り、`SF_Crypt` は `else` arm で拒否するので全キーが要る）。値そのものは qpdf 側も保存せず、`decryptStream` が live graph から読み直す。`QPDF_Stream.cc` 本体の行は §1 にあり、二重帰属を避けてここには再掲しない | ✅ |
| `Pl_DCT.cc` (buffer/decode) | 207 (`1-57,77-116,119-143,195-248,296-326`) | `pipeline/dct.rs` + `stream_filter.rs` の `DctStreamFilter`（`decode_pipeline` が canonical route。qpdf の buffered write、empty/repeated `finish` の downstream finish、libjpeg scanline 出力、error/cleanup を対応）; qpdf refs: `Pl_DCT.hh:30-70`, `Pl_DCT.cc:1-57,77-116,119-143,195-248,296-326`, `SF_DCTDecode.hh:8-40`。stage owner は qpdf の filter-instance 保持 + caller の non-owning pointer に対し、Rust は stage を値で返し `PipelineRef::Owned` と `next` の borrow で保持する correspondence class (B) | ✅ default backend は `libjpeg-turbo-rs = 0.8.0`、`qpdf-libjpeg-compat` は `flpdf-libjpeg-compat` を明示的に有効化する system libjpeg backend（no vendored library、runtime switch なし）。system-libjpeg の ABI boundary は `flpdf-libjpeg-compat`（`csrc/jpeg_compat.c/.h` + `ffi.rs`）が所有し、`BITS_IN_JSAMPLE == 8`、libjpeg 6b-compatible (`JPEG_LIB_VERSION >= 62`) capability/version guard、qpdf 相当の whole-buffer exhaustion (`invalid jpeg data reading from buffer`、fake EOI なし)、panic-contained callback を持つ。qpdf 11.9.0 の 8-bit scope を対象に、最小 image XObject の `qpdf --show-object=3 --filtered-stream-data` differential（2026-08-10 観測）は default/C とも qpdf stdout 12 bytes = canonical `DctSink` 12 bytes、mismatch 0、stderr 0。canonical consumer は `decode_pipeline`、legacy whole-buffer bridge caller は後続 `flpdf-3yn9.6` で cutover、writer passthrough は別責務として残す |
| `Pl_DCT.cc` (compression) | 119 (`58-76,117-118,144-194,249-295`) | `pipeline/dct.rs` に qpdf の圧縮constructor、圧縮destination、`Pl_DCT::compress` の対応はまだ無い。writer側のDCT passthroughはこのqpdf compression primitiveの代替ではなく、圧縮実装はwriter/compression follow-upに残す | ❌ missing |
| `Pl_Base64` / `Pl_Concatenate` / `Pl_OStream` / `Pl_String` | 282 | `pipeline/base64.rs` / `pipeline/concatenate.rs` / `pipeline/ostream.rs` / `pipeline/string.rs`（JSON serialization/output の本番 consumer を含む） | ✅ |
| `Pl_StdioFile.cc` | 46 | `pipeline/stdio_file.rs`（positive partial write の継続、zero/error—including `Interrupted`—の即時 Runtime 化、`EBADF` finish のみ Logic 化）+ `json_inspect.rs`（4096-byte buffer、top-level file は close/drop、side file は explicit finish） | ✅ |
| `Pl_Buffer` | 82 | `pipeline/buffer.rs`（accumulation、optional pass-through、finish readiness、buffer ownership transfer） | ✅ |
| `Pl_Discard.cc` | 23 | `pipeline/discard.rs`（public terminal identifier、no-op write/finish、finish 後の再利用）+ `filespec_helper.rs`（EmbeddedFile checksum terminal consumer） | ✅ |
| `Pl_Function.cc` | 62 | 専用 stage は未実装。使用箇所ごとの closure 実装 | ⚪ |
| `Pl_SHA2.cc` | 75 | `pipeline/sha2.rs`（SHA-256/384/512 の bit 選択、`resetBits`、digest access、optional next への write/finish forwarding と error 順序、再利用 lifecycle）。`Pl_SHA2.hh:9-11` の契約通り `finish()` 後の最初の `write()` は同じ bit size の新 cycle を開始し、連続 `finish()` は empty digest を生成する。native backend が finalize 後に同じ context を再初期化する挙動（`sha2.c:670-673`; `sha2big.c:209-228`）は RustCrypto の `finalize_reset` に対応する。⚪ `bits=0` のままの write/finish は qpdf では null crypto provider を dereference し、最初の finish 前の digest access は未初期化 result buffer を読むため、Rust では定義済み logic error に変換する。production consumer は `security/standard.rs` の `r5_salted_hash` / `r6_password_hash`（qpdf `hash_V5`、`QPDF_encryption.cc:239-311`）で、初期 hash は連結バッファを作らずpassword/salt/udata を 3 回 write し（`:246-249`）、R=6 ループは毎周 fresh な `Pl_SHA2` を算出 bit size で構築する（`:295-299`）。qpdf が identifier を `"sha2"` に固定している（`Pl_SHA2.cc:8`）のに合わせ、callsite も同じ値を渡す | ✅ |

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
| `QPDFAcroFormDocumentHelper.cc` | 1047 | `acroform_document_helper.rs`(217-590: `analyze` / `traverseField` 相当の live `ObjectHandle` association cache、direct Widget の orphan fallback、`invalidateCache`、`removeFormFields` の forward-map 起点 cache cleanup; 862-943: frozen-cache `addAndRenameFormFields`; 1229-1337: `getNeedAppearances` / `setNeedAppearances` / `generateAppearancesIfNeeded`) + `page_annotation_flatten.rs`(596-612: `/Fields` guard 後の Widget identity gate) + `signatures.rs`(`disableDigitalSignatures` consumer) + `overlay_annotations.rs`(2263: `transformAnnotations`、`/DA` の resource-name replacement consumer) + `overlay_appearance_stream.rs`(720: `adjustAppearanceStream`、AP stream consumer) | 🔀 |
| `QPDFPageObjectHelper.cc` | 1039 | `page_object_helper.rs`(766) + `pages.rs`(98: inherited `/MediaBox`/`/CropBox`/`/Resources`/`/Rotate` lookup) + `page_form_xobject.rs`(637) + `resources.rs`(1229: `ResourceFinder` を使う resource pruning consumer) + `page_annotation_flatten.rs`(596-612: field-associated Widget のみ `/DR` を appearance resources に merge) + `overlay.rs`(2228: `placeFormXObject`) + `overlay_annotations.rs`(2263: `copyAnnotations`) | 🔀 `pages.rs` の terminal chase は parsed qpdf child reference の意味ではなく、一時的な `Pdf::set_object` bare-reference bridge の互換境界だけをカバーする。qpdf の `QPDF::replaceObject` は indirect replacement を拒否する（`QPDF.cc:1986-1991`）ため、その bridge cycle の synthetic-null fallback を qpdf の null-as-absent inheritance と解釈しない。|
| `QPDFFormFieldObjectHelper.cc` | 852 | `form_field_object_helper.rs` + `form_field_object_helper/rendering.rs` + `default_appearance.rs`（field lookup/mutation と Tx/Ch appearance generation。`QPDFFormFieldObjectHelper.cc:472-478` に従い Btn appearance は production dispatch から除外）。既存 `/AP/N` は qpdf の `ValueSetter` 相当を同じ stream に登録する `AppearanceTokenFilter`（qpdf `QPDFFormFieldObjectHelper.cc:766-860`）で更新し、ラッパーが無い stream の EOF fallback（同 `:524-570`）も保持する。CLI の `generate_missing_appearances` は non-`/Btn` を `/AP/N` の有無で skip せず canonical helper へ渡す（qpdf `QPDFAcroFormDocumentHelper.cc:393-415`）。`crates/flpdf-cli/tests/cli_acroform_transforms.rs::generate_appearances_tx_reuses_existing_ap` は `/NeedAppearances true` の既存 stream を `--compress-streams=y` で再書き込み、`DecodeLevel::Generalized` 後の `/Tx BMC`/`Tf` と no-wrapper source preservation を確認する。qpdf 11.9.0 pinned source と `/usr/bin/qpdf` の live probe でも同じ入力の既存 AP が generated body を含む出力へ変化することを確認済み。token-filter primitive 自体の変更は本 issue の scope 外 | 🔀 |
| `QPDFPageDocumentHelper.cc` | 158 | `page_document_helper.rs`(`get_all_pages` + page mutation APIs) + `page_extract.rs`(`extract_pages`/`extract_page`) + `job/page_merge.rs`(`merge_documents`)。`overlay.rs` の source/destination page snapshot も `get_all_pages()` を通り、`QPDF_pages.cc:39-138` 相当の repair（欠落 `/MediaBox` の Letter fallback と warning）を Form 化・placement 前に適用する。両モジュールとも `Pdf::empty()` へ委譲（`emptyPDF()` + `addPage()` の library-level 経路、doc に明記） | 🔀 `emptyPDF()` 自体（`QPDF.cc:34-51,290-293`）の canonical 実装は `engine.rs`(475: `Pdf::empty()` は `open_mem_owned` へ委譲し、両者で `emptyPDF()` / `processMemoryFile()` 相当の construction path を担う)。⚪ qpdf の `emptyPDF()` は default-construct 済み `QPDF` を遅延初期化する `void` メンバー関数だが、flpdf の `Pdf` に「未初期化」状態が無いため static factory（`Result<Self>` を返す）に置き換えている。バイト列・parse 経路（`open_mem_owned` = `processMemoryFile` 相当）は同一。QPDFJob 相当のバージョン蓄積（`max_input_version`）は library level のこれらの関数ではなく `job/`（`flpdf-jq0z`）が担う想定 |
| `QPDFAnnotationObjectHelper.cc` | 226 | `annotation_helper.rs` + `page_annotation_enum.rs`(249) + `page_annotation_flatten.rs` | 🔀 `page_annotation_flatten.rs` の `AppearanceTarget::Bridge`/`has_bare_reference_redirect` は flpdf の一時的な `Pdf::set_object` bare-reference bridge のみをカバーする代替経路で、parsed qpdf object は one-hop/live のまま `AnnotationObjectHelper` が qpdf の `getPageContentForAppearance`（`:78-226`）を忠実に実装する。同種の bridge パターンは `QPDFOutlineDocumentHelper` 行（本表 §7、`outline_document_helper.rs`）を参照。 |
| `QPDFOutlineDocumentHelper` / `QPDFOutlineObjectHelper` | 198 | `outline_document_helper.rs`(693) + `outline_object_helper.rs`(237) | ✅ live `ObjectHandle` route: `OutlineItem.object` retains canonical identity; `OutlineItem::title`/`count`/`dest`/`dest_page` recompute fresh from the live object on every call (no caching), matching qpdf's `getTitle`/`getCount`/`getDest`/`getDestPage` (`QPDFOutlineObjectHelper.cc:47-98`), while `parent`/`kids` are captured once at construction, matching qpdf's cached `getParent`/`getKids`. `/Dest` and `/A /GoTo /D` use qpdf-shaped handle accessors; named destinations use `HandleNameTree`, cached per session in `OutlineDocumentHelper::dest_dict`/`names_dest` (`QPDFOutlineDocumentHelper.cc:60-90`); JSON consumes the handles directly. The narrow terminal-handle chase only covers flpdf's temporary `Pdf::set_object` bare-reference bridge; parsed qpdf objects stay one-hop/live. |
| `QPDFPageLabelDocumentHelper.cc` | 134 | `page_label_document_helper.rs`(1037) + `nntree.rs` (`HandleNumberTree`) | ✅ canonical ObjectHandle route for `hasPageLabels`, `getLabelForPage`, `getLabelsForPageRange`, and `pageLabelDict`; typed page-operation adapters and JSON migration remain downstream |
| `QPDFNameTreeObjectHelper` / `QPDFNumberTreeObjectHelper` / `NNTree.cc` | 1394 (`34-75,106-168,216-390,391-520,560-700`) | `nntree.rs`（shared canonical `ObjectHandle` engine + `NameTree`/`NumberTree` wrappers）+ consumer adapters。qpdf の live `QPDFObjectHandle`/`QPDF_Array` mutation（`NNTree.cc:34-75` の iterator value 更新、`:106-168` の limits、`:216-390` の split/insert、`:391-520` の remove/deepen、`:560-700` の find）に対応し、`ResolvedArray` は `ObjectHandle::set_array_items` で alias を保持したまま更新、direct kid の indirect 化は `Pdf::make_indirect_from_object_handle`、root split は既存 root slot を維持する。dirty propagation で raw compatibility read/writer も canonical mutation を観測する。`Object` は root の compatibility projection と test-only raw fixture boundary に限定し、production tree mutation は `Pdf::set_object` を経由しない。`legacy_terminal_handle`/`root_handle` の bare-reference chase は同種の bridge パターン（本表 §7、`pages.rs`/`page_annotation_flatten.rs`/`outline_document_helper.rs` 行を参照）で、qpdf の `QPDF::replaceObject` は indirect replacement を拒否する（`QPDF.cc:1986-1991`）ため対応物は無い | ✅ |
| `QPDFEmbeddedFileDocumentHelper.cc` | 122 | `embedded_files.rs`(678) | ✅ D1 完成（`flpdf-jzy7`）: `has_embedded_files`/`get_embedded_files`/`get_embedded_file`/`replace_embedded_file`/`remove_embedded_file` が `QPDFEmbeddedFileDocumentHelper.hh` の公開 API と 1:1 対応。モジュール doc の自己申告も更新済み。D2 は未達のまま — `job/json_sections.rs` の `build_attachments_section` はこのヘルパーを経由せず `NameTree` を直接歩く（`flpdf-q2fo` で解消予定） |
| `QPDFFileSpecObjectHelper` / `QPDFEFStreamObjectHelper` | 280 | `filespec_helper.rs`(1478) | ✅ D1 完成（`flpdf-d9sq`）。D2 は未達のまま — `job/json_sections.rs::filespec_dict_to_json` が `FileSpec`/`EmbeddedFileStream` を経由せず同じ Mac/DOS 優先順位ロジックを再実装している（`flpdf-q2fo` で解消予定）。旧 `copy_attachments_from`（`copyForeignObject` 以前の独自 `sanitize_imported_object` walk）は `flpdf-s5cw.7` で `QPDFJob::copy_attachments`（`job/attachments.rs`）へ置き換えられ削除済み |
| `ResourceFinder.cc` | 56 | `resource_finder.rs`（operator/name tracking、qpdf `getNames()` 相当のカテゴリ横断 flat set、resource type/offset 集約）。production consumer は `resource_replacer.rs` と `resources.rs` の resource pruning | ✅ |
| `QPDFAcroFormDocumentHelper.cc` anonymous `ResourceReplacer` | — | `resource_replacer.rs`（`ResourceFinder` の name offsets を exact-byte 置換）。production consumer は `overlay_annotations.rs` の `/DA` と `overlay_appearance_stream.rs` の AP streams | ✅ |
| `QPDFDocumentHelper.cc` / `QPDFObjectHelper.cc` | 12 | 基底トレイトが無い | ⚪ |

## 8. JSON

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `JSON.cc` | 1401 | `json/`（全 write helper、blob callback、unparse が public `Pipeline` 境界を使用。serializer は caller-owned outer pipeline を finish しない） | ✅ |
| `JSONHandler.cc` | 189 | `json/` | ✅ |
| `QPDF_json.cc` 入力側（`QPDF_json.cc:1-833`: `JSONReactor` / `createFromJSON` / `updateFromJSON` / `importJSON` / `test_json_validators`） | 833 | `json/input.rs`（reactor・validators・provider・value factory） + `json/document.rs`（rootless seed・create/update/import 境界） + `tests/json_document_tests.rs`（flpdf-authored fixture と qpdf 11.9.0 differential） | ✅ `.15.4` で入力境界を実装。create は `QPDF_json.cc:54-63` の rootless seed、update は omitted object を保持し、parser/semantic error の境界と update page flags を qpdf どおりに分離する。⚪ (B) `validate_pdf_version` は `QPDF::validatePDFVersion`（`QPDF.cc:366-384`）の byte-slice 置換で、`QPDF_json.cc:503-518` の全入力消費条件を保持する。⚪ (B) `JsonDescription` は `QPDFValue::Description` の共有 mutation（`QPDF_json.cc:721-730`）を per-handle Rust value で置換するが、input/object/offset の観測契約は不変 |
| `QPDF_json.cc` 出力側（`QPDF_json.cc:834-946`: free function `writeJSONStreamFile`(834-849) + `QPDF::writeJSON` ×2 overload(851-946)） | 113 | `document_json.rs`(361: `write_json` = 6 引数 overload(851-861)、`write_json_key` = `complete`/`first_key` overload(863-946)、`write_json_stream_file` = `writeJSONStreamFile`。side file は `PlStdioFile` explicit finish) | ✅ 入出力とも qpdf の別責務境界に対応。`qpdf --json-output=2` は complete overload と同一バイトを書くため、`crates/flpdf/tests/document_json_tests.rs` が 7 fixture で qpdf 出力と直接照合する |
| `QPDFObjectHandle::getJSON` / `QPDFObjectHandle::writeJSON`（行数は §1 の `QPDFObjectHandle.cc` に計上済み。ここは所在の相互参照） | — | `object_handle.rs` の `ObjectHandle::get_json` / `ObjectHandle::write_json`（`QPDFObjectHandle.cc:1613-1647` の外側 dispatch と `qpdf/JSON_writer.hh:16-135` の pipeline 境界）、`json_inspect.rs` の `pdf_object_to_json`（getJSON false の consumer） | 🔀 canonical ObjectHandle writer は移送済み。`false` は間接 identity を先に検査して `"N G R"` を出力し、array/dictionary child は非再帰の reference dispatch、stream は `QPDF_Stream::writeJSON` と同じく dictionary のみを出力する。`true` の一段解決 primitive も writer に実装済みで、document-level `QPDF::writeJSON` の object-map は `flpdf-25kg.3.37` で cutover 済み。`json_inspect.rs` の `ordered_qpdf_*` は本番 bridge ではなく、既存の pipeline-write 境界テスト専用で保持する |
| `QPDF_Stream::writeStreamJSON`（行数は §1 の `QPDF_Stream.cc` に計上済み。ここは所在の相互参照） | — | `object_handle.rs` の `ObjectHandle::write_stream_json`（`QPDF_Stream.cc:207-295` の mode validation、`no_data_key`、二重試行、dict normalization、payload routing、effective decode level） + `document_json.rs` の object-map framing / side-file ownership。`json_inspect.rs` の `stream_payload_with_decode_status` は既存の公開 raw-payload helper とテスト oracle に限定 | ✅ `flpdf-3yn9.9` で qpdf の 1 関数責務へ統合。旧 `Object/Stream` payload/dict bridge は本番経路から外し、`QPDF_json.cc:917-925` 相当の consumer は canonical handle を呼ぶ。非 file entry は既存 flpdf の変換失敗時接頭辞を保つため canonical 結果を先に buffer 化する |

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
| `QPDFJob.cc` | 3116 | `flpdf-cli/src/main.rs`(6796) + `job/lifecycle.rs`（JSON create/update/write、ordinary open、ordinary page inspection、JSON inspection と共有 completion、`QPDFJob.cc:429-516,843-875,1646-1714`）+ `job/json.rs`（`QPDFJob::writeJSON` の出力選択と `doJSON` 固定順、`QPDFJob.cc:1545-1640,3094-3115`）+ `job/json_sections.rs`（`doJSONPages` / `doJSONPageLabels` / `doJSONOutlines` / `doJSONAcroform` / `doJSONAttachments` / `doJSONEncrypt`、`QPDFJob.cc:1030-1330`） + `job/attachments.rs`（`doListAttachments` / `doShowAttachment` の info/save と completion（`QPDFJob.cc:876-927`）、`addAttachments` の provider-backed 追加（`QPDFJob.cc:2046-2087`）、および `copyAttachments` の cross-document 添付コピー（`QPDFJob.cc:2089-2135`）） + `job/page_specs.rs`（`handlePageSpecs` のspec解決・collate・source lifecycle・最終順序、`QPDFJob.cc:2360-2632`、single-document の `QPDFJob::prune_acroform_after_subset` を含む） + `overlay.rs`（`handleUnderOverlay` の source/destination 全 page 取得と修復前置、`QPDFJob.cc:1937-2015`） + `job/page_merge.rs`(1117) + `check.rs`(360) + `attachment_list.rs`（EmbeddedFiles/FileSpec/EF の traversal・metadata projection） + `acroform_field_prune.rs`（job boundary が委譲する canonical field-tree walk） + page 操作群 | 🔀 `job/lifecycle.rs` はJSON create/update/write、JSON read-only inspection、ordinary page-count/page-list inspectionのcanonical boundaryを移植済み。`job/attachments.rs` は添付 inspection の info/save と共有 completion、`--add-attachment` の provider-backed 追加、および `--copy-attachments-from` の cross-document コピー（`copyForeignObject` 経由、重複キーの集約 throw を含む）を移植済み。`job/page_specs.rs` はordinary multi-source `--pages` のjob boundaryを移植し、foreign copy・AcroForm field collision・PageLabels・collate orderを `job/page_merge.rs`/page helperへ接続した。single-document の AcroForm field pruning も `QPDFJob::prune_acroform_after_subset` から同じ job 層へ接続した。argv/config、通常rewrite、`--remove-attachment` orchestration、linearizationの残りconsumerは後続sliceで集約する。ordinary page-list formatterの出力形状は既存flpdf contractを保持し、qpdf `doShowPages` との完全な出力整合は別consumer scopeとする。 |
| `QPDFJob.cc` `createQPDF` / `doInspection` + `QPDFJob_config.cc` `jsonInput` / `updateFromJson` | `459-516,1646-1714; 305-309,328-332` | `job/lifecycle.rs` のJSON create/update/open/inspect（`flpdf-25kg.5.2.1/.2`）+ `flpdf-cli/src/main.rs` の `run_json_input_inspection`、`check.rs::check_pdf_with_limits` + `page_combine.rs::CombinedPlan::build_repeated` + retained `open_job_pdf` for other routes | 🔀 `--json-input` / `--update-from-json` のJSON outputとread-only `--show-npages`/`--show-pages`はQPDFJobの一つのdocument/logger lifecycleへ移行済み。`--check`は専用のqpdf-shaped report rendererを保ち、generic summaryの二重出力を避ける。通常rewrite・rotate・page-tree選択・その他inspectionは後続Job sliceで同じ状態へ接続する。JSON主入力の `--pages` は一時PDFを経由せず、同じ文書のObjectHandle/xrefを `build_repeated` で計画化する。qpdf 11.9.0のupdate-before-inspection順序を `cli_json_input.rs` で固定する。 |
| `QPDFJob_config` / `_argv` / `_json` / `QPDFArgParser` / `QPDFUsage` | 3164 | clap で代替 | ⚪ |
| `QPDFLogger.cc` | 255 | `logger.rs`（private stdout tracker、shared info/warn/error/save routes、standard stdout/stderr/discard、reset/following、save collision、custom sink ownership）+ `reader/resolver.rs` / `reader.rs`（文書 warning の append-then-route、suppression、live logger replacement）+ `flpdf-cli/src/main.rs`（下記 qpdf-equivalent consumers） | ✅ `QPDFLogger.cc:9-40,43-51,80-254`。`diagnostics.rs` は logger ではなく collection-only value store として維持する |

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
- `error`: check error と top-level fatal result/usage diagnostic

以下の direct output は意図的に retained とする。

- `run_show_stream` の passthrough-codec marker: flpdf-only fallback 表示で、qpdf は
  unfilterable stream を同じ marker へ変換しない
- native `rewrite --static-id` warning、`--remove-restrictions` intent diagnostic、
  `copy-attachments-from` count: flpdf-only surface
- clap 前後の immediate option-validation diagnostics: production `QPDFJob` aggregation
  (`flpdf-25kg.5.2`) より前の CLI shell responsibility。最終 dispatch error と usage result
  は logger 済みだが、この validation 群は同 downstream cutover まで direct stderr を維持する

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

## 10. インフラ

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QUtil.cc` | 2003 | 各所に散在 | 🔀 |
| `QTC.cc` | 50 | 無し | ❌ |
| `BitStream.cc` / `BitWriter.cc` | 111 | `bit_stream.rs`（MSB-first bit 読み取り、Rust の error 値）/ `bit_writer.rs`（MSB-first bit 詰め、Pipeline stage）。production consumer は `linearization/hint_stream.rs`（hint stream の生成・読み取り）と `linearization/show.rs`（`read_h_page_offset` / `read_h_shared_object` / `read_h_generic` の hint decoder）、および `bit_writer.rs` 自身 | ✅ `flpdf-qxba.9.1` で cutover。`linearization/hint_stream.rs` 側に bit 実装は残っていない |
| `Buffer.cc` / `MD5.cc` | 286 | `Vec<u8>` / 外部 crate | ⚪ |
| `qpdf-c.cc` / `qpdfjob-c.cc` / `qpdflogger-c.cc` | 2237 | — | ➖ |

---

## flpdf-only

### A. 2 つの汎用機構を参照種別ごとに特殊化したもの — 1,748 行

| flpdf | 行 | 対応する qpdf 挙動 |
|---|---|---|
| `outline_dest_remap.rs` | 898 | 削除ページ参照の null 化（配列要素） |
| `struct_tree_pg.rs` | 379 | `/Pg` の key drop |
| `thread_bead_p.rs` | 293 | bead `/P` の key drop |
| `objr_obj_annot_p.rs` | 178 | OBJR 経由 annotation の `/P` key drop |

**この表に含めない隣接モジュール**（責務が別で、畳み込みの対象にしてはならない）:

| flpdf | 行 | 理由 |
|---|---|---|
| `acroform_field_prune.rs` | 497 | qpdf 側に**明示的な対応パスがある**（`QPDFJob.cc:2610-2632` "Remove unreferenced form fields"）。副作用ではなく移植対象 |
| `subset_prune.rs` | 251 | `/Resources` の stale 名前エントリ剪定（`removeUnreferencedResources` 相当）と、xref レベルの orphan mark-and-sweep の 2 責務。null 可視性とは独立 |

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

**この主張が及ぶのは上表の 4 モジュール 1,748 行のみ。** `acroform_field_prune.rs` と
`subset_prune.rs` は qpdf 側に別の対応先を持つ独立した責務であり、2 機構に還元できない。

**区別すべきこと**: 挙動は検証済みで byte-identical を保っているので壊してはならない。
機構が異なるだけ（in-place 個別修復 vs. null 置換 + writer の null 可視性）。
畳み込みは byte リスクを伴う別の設計判断であり、writer の責務分割が固まったあとに検討する。

### B. `QPDFJob::handlePageSpecs` 相当の分解 — 4,158 行

`job/page_specs.rs` / `job/page_split.rs` / `job/page_merge.rs`(1117) / `page_rotate.rs`(632) / `page_extract.rs`(435) /
`page_range.rs`(379) / `page_splice.rs`(304) / `page_combine.rs`(278) / `page_plan.rs`(210) /
`rotate_spec.rs`(204) / `page_collate.rs`(145)

`job/page_specs.rs` がqpdfのjob-level orchestration（`QPDFJob.cc:2360-2632`）を所有し、
`job/page_merge.rs` と `PageDocumentHelper` がforeign page copy/page-tree primitiveを所有する。
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
は primary Catalog/trailer の全 graph を foreign-copy closure に含め、
`/Pages` と writer/xref-owned trailer keys だけを target 側で再構築する。
その結果 `/Info`、`/ID[0]`、未知の trailer entries、`/ViewerPreferences` や
その他の Catalog siblings は primary の値と indirect-reference identity を
保ったまま remap され、secondary の Catalog/trailer metadata は継承されない。
`page_merge_tests.rs::merge_preserves_primary_catalog_and_trailer_metadata` と
CLI の qpdf 11.9.0 differential test、および fixture の live probe で確認する。

### C. qpdf に機能そのものが無いもの

| flpdf | 行 | 備考 |
|---|---|---|
| `standard_font_metrics.rs` | 4,633 | qpdf にフォント幅テーブルは存在しない（`grep -rl Helvetica libqpdf/` が 0 件） |
| `signatures.rs` の**検査 API のみ** | — | 署名の読み取り検査。qpdf に相当機能なし |
| `qdf_fix.rs` | 1,219 | qpdf では `qpdf/fix-qdf.cc`（libqpdf 外の別バイナリ）。object stream (`/Type /ObjStm`) / cross-reference stream (`/Type /XRef`) 形式の QDF 入力にも対応（`st_in_ostream_*` / `st_in_xref_stream_dict` 相当、flpdf-9hc.43） |


`object_copy.rs`(184) は `QPDF.cc` の `copyForeignObject` に相当するため
[§2 パース / 読み取り](#2-パース--読み取り) の `QPDF.cc` 行に移した。

なお、`object_copy.rs` の `copy_objects` / `page_closure.rs` は旧来の
pre-closed raw `Object` 経路であり、canonical parity の責務ではない。
qpdf 11.9.0 の `QPDF::copyForeignObject`（`QPDF.cc:2019-2272`）に対応する
正本は `object_copy::copy_foreign_object` で、`reserveObjects` / 完全な
`ObjectHandle` graph replacement / `/Pages` 境界 / per-source map reuse を
ここで担う。stream の Buffer/provider/original-source 選択は
`reader/resolver.rs` の resolver-owned boundary に委譲し、qpdf の
`ot_reserved` は外部に露出しない内部 reservation sentinel として
destination-owned indirect null slot で表現する。

`page_extract.rs::extract_pages` はこの canonical foreign-copy route へ切り替え済みで、
qpdf の source-side inherited-attribute preparation と destination-side page-tree
mutation を組み合わせる。`job/page_merge.rs` も `pushInheritedAttributesToPage` 相当の
source preparation と live-handle による destination `/Parent` replacement を使うが、
primary の document-level / AcroForm / PageLabels merge を一つの map で処理するため、
その merge-specific union copy と共有 raw closure は別の consumer cutover として残る。

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
- `overlay::byte_gate`（`--lib` 実行）

| 経路 | library byte gate | CLI byte gate |
|---|---|---|
| classic full rewrite（`--static-id`） | `cmp_diff_zero_tests` ✅ | `compat_baseline_static_id` ✅ |
| objstm generate（非 linearized） | `cmp_generate_objstm_tests` ✅ | `compat_matrix_baseline` ✅ |
| linearize（classic） | `cmp_linearize_tests` ✅ | `cli_byte_identical` ✅ |
| linearize + objstm | `cmp_linearize_objstm_tests` ✅ | ✅ |
| overlay / underlay | `overlay::byte_gate` ✅ | `cli_byte_identical_overlay` ✅ |
| `--deterministic-id` | `deterministic_id_qpdf_parity_tests` ✅ | — |
| null 可視性 | `cmp_null_visibility_tests` ✅ | — |
| QDF | 🟡 **部分的にあり**（下記）。`overlay::byte_gate` の QDF 12 件を含む | 🟡 `cli_byte_identical_overlay.rs` の QDF 3 件 |
| 暗号化出力 | ❌ gated byte gate 無し | 🟡 `encrypt_cli_tests` の `encrypted_document_is_byte_identical_to_qpdf` / `cli_linearize_encrypt_aes128_byte_identical_to_qpdf` 2件（`qpdf-zlib-compat` 関数レベル gate、CI 列挙済み） |
| incremental update | ❌ gated byte gate 無し | ❌ |

### QDF の既存カバレッジ（部分的）

「QDF に byte gate 無し」は誤り。次の 3 系統が既に存在する。

| テスト | 内容 | CI |
|---|---|---|
| `writer_tests.rs:2170,2201` | `tests/golden/references/qdf-contents-ref-array/qdf-static-id.pdf` と `qdf-ignore-newline/qdf-static-id.pdf` に対する完全一致比較 | ✅ 列挙済み |
| `qdf_tests.rs:1300` | `qdf_golden_minimal_is_byte_identical_to_qpdf_modulo_id` — `tests/fixtures/qdf-golden/minimal.qdf` に対し trailer `/ID` 行を除いて完全一致 | 既定テストに含まれる |
| `overlay.rs` の `overlay::byte_gate` | **QDF byte-identity テスト 12 件** — `three_page_*_qdf_is_byte_identical` 3 件(1320, 1339, 1357) と annotation-copy 系 `*_is_byte_identical_qdf` 9 件(1528-1889) | ✅ `--lib overlay::byte_gate` で列挙済み |
| `cli_byte_identical_overlay.rs`(293-338) | 上記の CLI 版 QDF variant（`--qdf --no-original-object-ids`） | ✅ 列挙済み |

### QDF で「穴」になりえない組み合わせ

QDF は他の出力モードと**排他**であり、次の組み合わせに byte gate を作っても
意図した writer 経路を通らない。

| 組み合わせ | 排他の実装箇所 |
|---|---|
| QDF × ObjStm | `qdf_tests.rs:734` `qdf_overrides_generate_mode_no_objstm` — QDF が `Generate` を上書きし ObjStm を出さない |
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

qpdf の `QPDFPageObjectHelper::coalesceContentStreams`（`QPDFPageObjectHelper.cc:474-476`）から
`QPDFObjectHandle::coalesceContentStreams`（`QPDFObjectHandle.cc:1550-1572`）へ委譲される
coalesce は、`QPDF.cc:1912-1917` の `newStream()` で空の stream dictionary を新規作成する。
したがって入力の先頭 stream にだけある非 filter metadata もコピーしない。flpdf の
`pages.rs::coalesce_page_contents` も `Dictionary::new()` で結果 stream を生成する。
canonical provider-backed pipeline と content-tree の全面 cutover は
`flpdf-qynx.7` の責務であり、この対応は dictionary shape の差だけを固定する。

## 集計

| 状態 | qpdf 側の該当行数 | 内訳 |
|---|---|---|
| ✅ 境界一致 | 5,255 | 責務境界は一致。**再配置は不要だが「完成」ではない** — DoD D1〜D5 の充足は各スライスで別途検証する |
| 🔀 smeared | 27,138 | 再配置の主対象。qpdf 全体の 65% |
| ❌ missing | 169 | `Pl_DCT.cc` compression(119) / `QTC`(50) |
| ⚪ 逸脱候補 | 6,660 | 要承認（下記の方針矛盾を参照） |
| ➖ 対象外 | 2,237 | C API |
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
