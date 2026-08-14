# flpdf ↔ qpdf 責務対応表

**Oracle:** qpdf 11.9.0 (`v11.9.0`) — `libqpdf/`
（`scripts/fetch-qpdf-source.sh` で取得。パスは `--print-path` で解決する。
本表のファイル名・行数はすべてこのツリーに対するもの。将来 v12 に追従する際は
`git log v11.9.0..v12.0.0 -- libqpdf/` が移植差分になる）
**調査日:** 2026-07-29（初版）/ 2026-08-02（Phase 2 着手時の再測 — `flpdf-1e5g`）
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
現状は **mirror 5 / correspondence 129**（`content_normalizer` / `matrix` /
`pdf_version` / `security/rc4` / `tokenizer` のみが `Mirrors`）。
本表で ✅ の `nntree.rs` / `json/` / `xref_entry.rs` / `bit_stream.rs` /
`bit_writer.rs` / `optimization.rs` / `pipeline.rs` が `correspondence` のままである
のは、責務境界が一致していても DoD 全体を検証していないためであり、
必ずしも矛盾ではない。**昇格は各部品の担当スライスで D1〜D5 を検証したうえで
行うこと**（D1 だけでは足りない）。

逆に、モジュール doc 自身が「公開 API 未完成」等を申告している場合は
責務境界が一致しているとも言えないため、本表でも ✅ にしてはならない。
2026-08-02 の再測ではこの観点で §7 の 3 行を ✅ から 🔀 に訂正した
（`filespec_helper.rs` / `embedded_files.rs` / `page_label_document_helper.rs`）。

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

## 1. オブジェクトモデル

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFObjectHandle.cc` | 2601 | `object.rs`(1301) + `object_handle.rs`(shared handle identity・parsed offset・遅延解決・live direct containment・`QPDF::newReserved`/`QPDF_Reserved`・`copyStream`/`StreamDataProvider` source dispatch) + `qpdf_null.rs`(9-37: `reference_is_null` / `value_is_null` = `isNull` の間接参照解決) + `overlay_annotations.rs`(1685-1737: `merge_resources_shallow` = `mergeResources`) + `overlay_appearance_stream.rs`（段階的 conflict merge の再現） | 🔀 アクセサが各所に散在（`flpdf-mfir`）。object identity / 遅延解決は `object_handle.rs` へ移行中（`flpdf-egzr.3.1`）。qpdf の array/dictionary/stream が保持する現在の forward child を正本とし、incremental dirty lookup 用に各 forward edge と一対一の immediate weak reverse edge を派生記録する。削除・置換後の旧 child は旧 root を返さない。`try_get_keys` は `QPDFObjectHandle::getKeys` → `QPDF_Dictionary::getKeys`（`QPDFObjectHandle.cc:997-1009`; `QPDF_Dictionary.cc:117-127`）に対応し、holder と全 child を lazy resolve して null value のキーを除外した `BTreeSet` を返す。child resolve 前に辞書 snapshot の borrow は終了し、resolver error は伝播する。`stream_filter.rs` の consuming stage は retained-key reduction 前に `try_get_keys` を使用する。`shallow_copy` は `QPDFObjectHandle::shallowCopy`（`QPDFObjectHandle.cc:2072-2079`）に対応し、stream は `QPDF_Stream::copy`（`QPDF_Stream.cc:140-145`）が `shallow` 引数を無視して無条件に `std::runtime_error` を投げるのに合わせて `Error::System` で拒否する。`QPDF_Dictionary::copy`/`QPDF_Array::copy` が direct な子に `shallowCopy` を掛けるため、コンテナに入れ子の direct stream も同じ拒否に到達する。qpdf の `QPDFObjectHandle::copyStream`（`QPDFObjectHandle.cc:2136-2151`）と `QPDF::copyStreamData`（`QPDF.cc:2216-2272`）を `ObjectHandle::copy_stream` と resolver-owned stream-copy boundary として実装済み（`flpdf-a8mk`）。Buffer は `Rc<Vec<u8>>` 共有、provider-backed source は source handle を保持する retry-aware provider、original-file source は qpdf の `ForeignStreamData` 相当として source の `StreamInput`/encryption state/object number/parsed offset/length と destination dictionary を copy 時に凍結し、destination resolver を warning sink として遅延 dispatch する。source `Pdf` 解放後も入力と暗号状態だけで読み続け、source 側へ警告を戻さない。`set_immediate_copy_from` は qpdf の source-side `setImmediateCopyFrom` に対応する。`QPDFObjectHandle::isReserved`/`QPDF::newReserved` は `ObjectState::Reserved` と `Pdf::new_reserved` に対応し、`ot_reserved` は null/missing/destroyed と区別して、materialize と全 ObjectHandle writer entrypoint で `QPDFObjectHandle: attempting to unparse a reserved object` を返す。 |
| `QPDFObjectHandle::StreamDataProvider` / `QPDF_Stream` | `QPDFObjectHandle.hh:68-127`; `QPDFObjectHandle.cc:48-90,1365-1428`; `QPDF_Stream.cc:571-620,640-660` | `object_handle.rs` の `StreamDataProvider`、`ObjectValue::Stream.stream_provider`、`replace_stream_data_provider`、callback adapter、`pipe_stream_source` | ✅ qpdf の provider ownership、通常/retry family の選択、identity forwarding、遅延・反復 invocation、`Pl_Count` による encoded-byte length 検証、buffer/provider の排他を canonical route で保持する。qpdf の `std::shared_ptr` container は `Rc<dyn StreamDataProvider>` に置換するが、これは内部所有表現だけの差であり、callback/error/finish/`/Length` の観測契約は変えない。登録 API は stable `ObjectRef` を必要とするため indirect stream に限定し、direct stream は登録時に `Error::System` で拒否する。既存 document-owned stream の provider/dictionary 置換は live graph mutation なので、writer 前に `Pdf::mark_object_handle_dirty` を要求する | ✅ |
| `QPDFObjectHandle::isNameAndEquals` / `isDictionaryOfType` / `getArrayNItems` / `getArrayItem` / `isOrHasName`（行数は上段に計上済み） | — | `object_handle.rs` の `try_is_name_and_equals` / `try_is_dictionary_of_type` / `try_array_len` / `try_array_item` / `try_is_or_has_name`（`QPDFObjectHandle.cc:456-466,759-785,1027-1039`） | ✅ holder と child を qpdf 順に lazy resolve。container borrow は resolver 再入前に解放し、配列全体を snapshot しない。`try_array_item` は `QPDF::decryptStream` が equal-length 確認後に使う valid-index 面のみで、qpdf が warning と特殊 null を返す invalid access は契約外 |
| `QPDFObjectHandle::typeWarning` / `warnIfPossible` / `objectWarning` / `warn` / `getIntValue` / `getIntValueAsInt`（行数は上段に計上済み） | — | `object_handle.rs` の `type_warning` / `warn_if_possible` / `object_warning` / `warn_through_context` / `context` と `DocumentResolver::warn`、`try_get_int_value` / `try_get_int_value_as_int`、`reader/resolver.rs` の `push_object_warning`（`QPDFObjectHandle.cc:502-543,2168-2212,2385-2396`; `QPDF.cc:487-494`） | 🔀 メッセージ文言は qpdf と完全一致。live parser が生成した direct value と canonical indirect handle は `HandleResolver::direct_handle` / `ChildHandles` から同じ weak document context と、qpdf の `QPDFParser` と同じ parse-call description template を持つ。非 null の top-level・array・dictionary・scalar は `input-description, object N G at offset $PO` を共有し、`QPDFValue` と同じ container offset shift を経て `DocumentResolver::warn` → `push_object_warning` で `Pdf::repair_diagnostics` と同じ収集先へ同順に届く。parsed null は qpdf と同じく description を持たない。literal null は containment parent の context を借りず、qpdf の `QPDF_Null::create` に対応する contextless 分岐をネスト後も維持する一方、missing-key null は `setChildDescription` に対応する Child description 経由で親の context を保持する（`QPDF_Null.cc:12-15`; `QPDFParser.cc:397-410`; `QPDFObject_private.hh:79-91`）。明示的 parse と programmatic direct は qpdf の contextless 分岐を維持する。no-context 分岐は qpdf のまま 2 通り — `typeWarning`/`objectWarning` は `throw QPDFExc`（`std::runtime_error` 派生、`QPDFExc.hh:29`）に対応する `Error::System`、`warnIfPossible` は `QPDFLogger::defaultLogger()->getError()` へ素の文言を書いて正常復帰する。`getKey`/`getKeys` の `typeWarning` は `try_get_key`/`try_get_keys` に実装済み。live parser の direct value は weak document context を持ち、stream_filter の consuming `/DecodeParms` 読み出しで qpdf と同じ回復可能な警告を `DocumentResolver::warn` へ送る。contextless の programmatic direct は qpdf と同じく `Error::System` 相当の throw を維持する。`asDictionary`/`asInteger` に対応する `try_as_dictionary`/`try_as_integer` は qpdf 同様 warning を出さない |
| `QPDF_Array/Dictionary/Stream/String/Name/Real/Integer/Bool/Null/InlineImage/Operator/Reserved/Unresolved/Destroyed.cc` | 1814 | `object.rs` の `Object` enum に統合 | 🔀 |
| `QPDFObject.cc` / `QPDFValue.cc` | 79 | `object.rs` の `Object` + `object_handle.rs` の `ObjectHandle` / `ObjectValue`（共有 identity・qpdf 互換 parsed offset・`IndirectState` 遅延解決・Pdf identity provenance） | 🔀 `object.rs` の `Object` は静的な値表現のみ。`QPDFValue` 相当の共有 identity・parsed offset・遅延解決状態は `object_handle.rs` が新たに担う（layer cutover 進行中）。Pdf identity provenance は live containment から分離して detach 後も保持する。両モジュールに分割されているため `✅` から変更 |
| `QPDFObjGen.cc` | 68 | `object.rs` の `ObjectRef` | ✅ |
| `QPDFXRefEntry.cc` | 51 | `xref_entry.rs`（`XrefEntry` = free / uncompressed / compressed の 3 variant）。consumer は `xref.rs` / `reader.rs` / `cache.rs` / `writer.rs` / `writer/{object_streams,plain/plan}.rs` / `linearization/{writer,plan}.rs` | ✅ `flpdf-qxba.9.2` で完全 cutover（`XrefOffset` 削除）。`xref.rs` 側に型定義は残っていない |
| `PDFVersion.cc` | 68 | `pdf_version.rs` の `PdfVersion` | ✅ |
| `QPDFMatrix.cc` | 140 | `matrix.rs` の `Matrix` / `Rectangle` | ✅ |

## 2. パース / 読み取り

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF.cc` | 2667 | `engine.rs`(475: `Pdf::empty`、ほか8つの public factory — `Pdf::open` / `open_with_repair` / `open_best_effort` / `open_with_options` / `open_mem` / `open_mem_with_options` / `open_mem_owned` / `open_mem_owned_with_options` —、`open_with_repair_mode`、`NEXT_PDF_ID`、`MAX_RESOLUTION_FALLBACKS`。`emptyPDF` / `processFile` / `processMemoryFile` の construction path) + `pdf.rs`(297: `Pdf<R>` container、`Drop` = `QPDF::~QPDF`、version/trailer/root/extension/page-enumeration-state accessors。`QPDF.hh:1438-1518`; `QPDF.cc:215-232,2323-2358,2647-2651`) + `reader.rs`(8185: object resolution, recovery, diagnostics, authentication, and `Pdf::get_xref_table` / `Pdf::get_all_objects`) + `reader/resolver.rs`(2367: canonical resolver。`QPDF::resolve` が触る `QPDF::Members` — `m->file` / `m->xref_table` / `m->obj_cache` / `m->resolving` / `m->resolved_object_streams` / `m->attempt_recovery` / `m->encp` — を `ResolverCore` に集約し、`Rc<RefCell<..>>` 経由で `ObjectHandle` の `Weak<dyn DocumentResolver>` から到達可能にする。`m->obj_cache` は canonical handle registry そのもので、`Pdf::get_object_handle`（= `QPDF::getObject`, `QPDF.cc:1952-1959`）と `Pdf::drop`（= `~QPDF`）の両方がここを見る。`Pdf::get_xref_table` は `QPDF::getXRefTable`（`QPDF.cc:2370-2377`）の effective source table snapshot、`Pdf::get_all_objects` は `fixDanglingReferences` と `m->obj_cache` enumeration（`QPDF.cc:1258-1294`）を canonical handle 上で実行する。`m->encp`（`flpdf-25kg.3.11`）は `Pdf::encryption` と同一の `Rc<RefCell<Option<EncryptionState>>>` を共有し、qpdf の `shared_ptr<EncryptionParameters>` を複数の owner が保持する形を再現する。`pipe_stream_data` は `QPDF::pipeStreamData` と同じく source read 前に `QPDF::decryptStream` 相当を呼び、同じ cell の method state / object-key cache を更新して AES/RC4 stage を前置する。`flpdf-25kg.3.5` slice 1 時点では `readObjectAtOffset`/`readObject`/`readStream` の uncompressed（xref type 1）経路のみ移植済み。`flpdf-25kg.3.5.1` で canonical type-1 stream framing recovery（malformed framing token、attempted token offset、live `recoverStreamLength` scan）を追加済み。ObjStm / xref stream と original stream bytes の source-dispatch consumer cutover は別 issue。resolve 時文字列復号と pipe 時ストリーム復号 primitive は移植済み。**行数は slice 進行中のため暫定値**) + `reader/file_object.rs`(1405) + `xref.rs`(1220) + `object_copy.rs`(342: `copyForeignObject`) + `cache.rs`(112: xref 由来の `ObjectCache` / `CacheEntry`。消費者は `reader.rs`) + `writer/object_streams.rs`(207-237: `compressible_objgens_qpdf_plan` = `getCompressibleObjGens`、`QPDF.cc:2392-2445`)  + `signatures.rs`(245-: `removeSecurityRestrictions`) + `page_closure.rs`(441: `page_object_closure`。`object_copy.rs` は pre-closed な集合しか受け取らず、両者で `copyForeignObject` 相当を構成する) + `ref_chain.rs`(159: `resolve_ref_chain` / `terminal_ref_of_chain` / `MAX_REF_CHAIN_DEPTH` — 深さ上限付き間接参照解決の共有プリミティブ。20 モジュールが使用) | 🔀 |
| `QPDF.cc`（xref registration/recovery と mutation 境界） | `516-575,686-708,1187-1210,1996-2005` | `xref.rs` の `XrefRegistration` が xref 読み取り・recovery merge ごとの object-number-wide `deleted_objects` free-row filter を所有し、`/Size` 検証後に破棄する。`ResolverCore` にはこの一時 state を渡さない。`reader.rs` の `Pdf::set_object` / `replace_object_handle` は canonical cache replacement だけを担い、この xref set を clear/add しない。canonical xref/cache removal と outstanding handle の null 化は `remove_object_handle` が担う。 | ✅ |
| `QPDF.hh`（`EncryptionParameters`） | 899-921 | `reader.rs`(54-69: `EncryptionState`)。qpdf は独立した2つの bool、`encrypted` / `encryption_initialized`（`QPDF.hh:907-908`）を持つが、flpdf はこれを単一の `Option<EncryptionState>`（`None` = 未初期化 or 認証済み未暗号化のいずれか、`Some` = 認証済み暗号化）に畳んでいる。安全性の根拠: `encryption_initialized` の唯一の用途は `initializeEncryption()`（`QPDF.cc:471` で1文書につき高々1回しか呼ばれない）内の再入防止ガード（`QPDF_encryption.cc:721,724`）で、flpdf の構造上この再入自体が起こり得ないため観測可能な挙動差は生じない。逸脱理由は `reader/resolver.rs` の `ResolverCore::encryption_parameters` doc にも記載（`flpdf-25kg.3.11`） | ⚪ |
| `QPDF::interpretCF` (`QPDF.hh`; `QPDF_encryption.cc`) | `1122-1127`; `700-716` | `reader.rs` の `interpret_cf_name` / `interpret_cf` / `interpret_cf_from_handle` | ✅ 値選択を共有し、ObjectHandle 版は `try_as_name` で lazy resolve。`crypt_filters` → built-in `/Identity` → `e_unknown`、non-name → `e_none` の順と resolver error 伝播を維持。`reader/resolver.rs` の pipe-time `decryptStream` consumer が live stream dictionary に対して使用する |
| `QPDF::decryptStream` (`QPDF_encryption.cc`) | `1045-1153` | `reader/resolver.rs` の `inspect_stream_encryption` / `pipe_stream_data` | ✅ `/XRef` early return、`/V >= 4` gate、typed direct `/Crypt` と equal-length array pairing、Crypt-before-Metadata precedence、unknown warning + `cf_stream` rewrite、qpdf の object-key cache、`PlAesPdf` / `PlRc4` 前置を source read 前に実行。stream dictionary の lazy resolve 中は encryption cell borrow を保持しない。legacy resolve-time payload 復号は consumer cutover まで維持 |
| `QPDFParser.cc` | 519 | `parser.rs` の `LiveInput` / `LiveTokenSource` / `LiveFileParser` は `InputSource` を一度だけ前進する file-object baseline（`QPDFParser.cc:27-518`）。canonical resolver の uncompressed type-1 consumer と、decoded-stream-relative `SliceLiveInput` 経由の ObjStm member consumer（`reader.rs::parse_object_stream_entry`）が使い、token 終端の one-character unread、diagnostic、top-level/nested/container/null の parsed offset、empty/dictionary/bad-token/depth recovery をここで共有する。uncompressed 側は canonical unresolved handle を同時に生成する。live canonical と context-none explicit の parser invocation は qpdf の parse-call description template を非 null handle に stamp し、container の render shift と null の無記述も維持する。`ObjectHandle::parse` は同じ live parser の context-none entry point で、warning を `Error`、nested `N G R` を `Error::Internal`、非 C whitespace の後続を parse error にする | 🔀 canonical uncompressed consumer は `StringDecrypter`（`flpdf-25kg.3.17`）を object-ref と shared `EncryptionState` に束縛し、`QPDF::readObject` / `QPDFParser` と同様に top-level・array・nested dictionary・stream dictionary の `tt_string` だけを token 時に復号する（`QPDF.cc:1331-1340`; `QPDFParser.cc:114-121,327-365`; `QPDF_encryption.cc:977-1039`）。完成した `/Type /Sig` + `/ByteRange` 辞書だけは raw `/Contents` bytes と parsed offset を復元する。ObjStm / context-none explicit parse / content mode は decrypter を渡さず、unknown word も callback 非呼出し。Content mode は既存 `Parser` を維持し、file-object live parser は content grammar を兼用しない |
| `QPDFTokenizer.cc` | 965 | `tokenizer.rs`（18 token types、owned value/raw/error bytes/offset、push/pull、pull-only `allowEOF`、`includeIgnorable`、space/comment、bad-token recovery、max length、`betweenTokens`、unread、inline-image `EI` discovery。`QPDFTokenizer.hh:34-193`; `QPDFTokenizer.cc:45-965`）+ `parser.rs` の content mode + `content_stream.rs` の `ParserCallbacks` orchestration + `object.rs` の `Operator` / `InlineImage`（`QPDFParser.cc:27-125,130-377`; `QPDFObjectHandle.cc:1770-1847`） | ✅ `QPDFTokenizer` の責務境界を移植済み。object/parser/content callback consumers は共有 tokenizer を使用し、旧 content lexer は削除 |
| `InputSource` 系 5 ファイル | 625 | `Read + Seek` ジェネリクスで代替。所有者は `reader/resolver.rs` の `ResolverCore`（`m->file` 相当）。`ResolverCore` のメソッドは `InputSource` の 3 操作 `seek`/`tell`/`read`（`InputSource.hh:71-74`）に限定し、`OffsetInputSource`（`QPDF.cc:406`）が担う header shift は `seek`/`tell` が適用する。例外は `rewind_underlying_source` 1 つで、これは wrapper が持つ `proxied`（`libqpdf/qpdf/OffsetInputSource.hh:24`）に相当する — `OffsetInputSource::rewind` は logical 0 に行く（`OffsetInputSource.cc:55-59`）ため `m->file` では表現できない。owned-window 系の legacy helper（`read_window` / `read_physical_input`）は `ResolverHandle` 側の `qpdf-legacy-tenant` で、`ResolverCore` の面には置かない | ⚪ |
| `QPDF_pages.cc` | 319 | `pages/repair.rs`（`QPDF_pages.cc:39-75` の `getAllPages` root correction と `:77-150` の `getAllPagesInternal` repair/enumeration を canonical `ObjectHandle` graph 上で実装） + `optimization/inherited_attrs.rs`（canonical page promotion/clone と衝突しない `Pdf::next_obj_gen` allocation） + `pages.rs` / `page_tree_rebuild.rs`（flatten/insert/remove と legacy consumer の残り） | 🔀 `flpdf-25kg.3.7` で repair/enumeration の canonical route を追加。`QPDF_pages.cc` 全体の cache/flatten/mutation consumer cutover は後続 issue |
| `QPDFExc.cc` / `QPDFSystemError.cc` | 123 | `error.rs`(125) | ✅ |

## 3. 書き込み — 最大の smear

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFWriter.cc` | 3044 | `writer.rs`(4494) + `writer/serialize.rs`(1008) + `writer/object_streams.rs`(739) + `writer/encryption_state.rs`(258) + `writer/encrypted_strings.rs`(213) + `writer/plain/{plan,body,xref}.rs`(898) + `linearization/writer.rs`(3603) + `linearization/part1.rs`(370) + `linearization/back_patch.rs`(324) + `linearization/renumber.rs`(850) + `rewrite_renumber.rs`(893) = **13,650 行 / 13 ファイル**。加えて `object.rs`(412: `write_pdf` = `unparseObject` / 491: `write_pdf_qdf` / 585-: trailer `/ID` = `writeTrailer`。`writer.rs` と `linearization/writer.rs` が委譲) と `qpdf_null.rs`(38-57: `visible_entries` = `QPDFWriter.cc:1491` の null 値 dict キー抑制)。さらに `object_handle.rs`(1705-: `unparse_object` / 1745-: `unparse_object_qdf` / 2302-: `unparse_stream_body` / 2375-: `unparse_stream_body_qdf` / 2569-: `unparse_trailer` = `unparseObject`(`QPDFWriter.cc:1318-1605`、dict 分岐 `:1346-1527`、stream 分岐 `:1528-1605`) / `writeTrailer`(`:1160-1230`) の `ObjectHandle` 版。`object.rs` の materialize-to-`Object` bridge を経由せず `ObjectHandle` のグラフを直接歩く新 primitive 群（`flpdf-egzr.3.2.13`）。`unparse_stream_body_qdf` は最終レビューで見つかったギャップの修正（Task 9）: `write_pdf_stream_qdf`(`object.rs:1036`、real production callsite は `writer.rs:4437`)に対応する QDF+stream 形の primitive が欠けていた。`Dictionary::write_pdf_stream_qdf` 自身に `refiltered` 概念が無いため（唯一の呼び出し元 `write_stream_to_buf_qdf` は既に確定済みの `/Filter`/`/Length` を持つ dict しか渡さない）、`unparse_stream_body`（compact 版）と異なりこちらも `refiltered` パラメータを持たない。null 値 dict キー抑制(`:1490-1491`)は `try_is_null` 経由で `unparse_object`/`unparse_object_qdf`/`unparse_stream_body`/`unparse_stream_body_qdf` の4つに適用し、`unparse_trailer` は `writeTrailer` 自身と同様に無抑制。`writer/encryption_state.rs` の `WriterEncryptionState` は `QPDFWriter::Members` の暗号 state (`QPDFWriter.hh:641-663`)、`set_data_key` は `setDataKey` (`QPDFWriter.cc:842-847`) と `compute_data_key` (`QPDF_encryption.cc:325-356`)、`with_object_data_key` は非 ObjStm member の set/unparse/clear (`QPDFWriter.cc:1761-1796`) に対応する。source ID ではなく emitted ID と generation 0 を使い、`Option<u32>` が qpdf の `-1` sentinel を置換する。qpdf の明示 clear は正常系だけだが、Rust callback の `Err` 後にも clear するのは出力 byte を変えず stale state を残さない内部代替である。全て `pub(crate)`・`#[allow(dead_code)]`。`flpdf-a32l` は AES で暗号化済みの文字列を full / linearized writer の共通 serializer context で強制 hex 化し、RC4・非暗号化・ObjStm member は既存の heuristic を維持する（`QPDFWriter.cc:1567-1592`）。既存 primitive の production consumer 移行は `flpdf-egzr.3.2.5`、暗号 state の consumer 移行は `flpdf-3yn9.12` 待ち | 🔀 |

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

**renumber は重複していない**: `rewrite_renumber.rs` は `linearization/plan.rs` からも
使われる共有機構で、`linearization/renumber.rs` はその上に載る最終採番層。qpdf の
`obj_renumber` 1 本に対して 2 層構造だが、二重実装ではない。

## 4. 線形化 / 最適化

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF_linearization.cc` | 1796 | `linearization/`（`plan.rs` 3032, `hint_*` 1651, `check.rs` 726, `show.rs` 995, ほか）≒ 8,000 行 | 🔀 |
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
| `Pl_ASCII85Decoder` / `SF_ASCII85Decode` | 108 + 31 | `pipeline/ascii85.rs` + `stream_filter.rs`（`SF_ASCII85Decode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_ASCIIHexDecoder` / `SF_ASCIIHexDecode` | 96 + 31 | `pipeline/ascii_hex.rs` + `stream_filter.rs`（`SF_ASCIIHexDecode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_RunLength` / `SF_RunLengthDecode` | 146 + 38 | `pipeline/run_length.rs` + `stream_filter.rs`（`SF_RunLengthDecode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_AES_PDF` | 200 | `pipeline/aes.rs`（qpdf の contract を全量移植: block 単位の write バッファリング、first-block を IV として消費する復号側と IV を先頭へ書く暗号化側、ISO 32000-1 7.6.2 の padding とその strip、`useZeroIV` / `setIV` / `useStaticIV` / `disablePadding` / `disableCBC`）＋ `security/standard.rs` の AES single-buffer helper と `writer.rs` の stream consumer | 🔀 `reader/resolver.rs` の `QPDF::decryptStream` 対応は `PlAesPdf` を source-read pipeline の前段へ接続済み。legacy resolve-time single-buffer helper は consumer cutover まで併存するため、同じ qpdf モジュールが 2 箇所に存在する。⚪ `QPDFCryptoImpl::rijndael_init` / `rijndael_process` の crypto provider 抽象は `aes` / `cbc` crate の直接利用に置換（§ 逸脱候補の crypto provider 行と同じ代替）。block ごとに 1 回 process する呼び出し形は保持し、chaining 状態のみ provider 側ではなく cipher が持つ。既知の逸脱: qpdf の padding strip は PKCS#7 厳密ではなく（末尾バイトが不整合ならブロックを丸ごと残す、`Pl_AES_PDF.cc:184-196`）、既存の `security/primitives.rs` の `decrypt_padded::<Pkcs7>` は同じ入力を `Err` にする。`pipeline/aes.rs` は qpdf 側に合わせてあるので、cutover 前後で受理する文書が変わる |
| `Pl_RC4` | 43 | `pipeline/rc4.rs`（65,536-byte既定buffer、stateful `security/rc4.rs`、write/finish lifecycle）+ `reader/resolver.rs` の pipe-time decrypt stage + `reader.rs` / `writer.rs` の既存 stream consumer | ✅ |
| `Pl_QPDFTokenizer.cc` / `ContentNormalizer.cc` | 141 | `pipeline/qpdf_tokenizer.rs`（optional downstream を持つ token-filter runner、EOF-token → `handle_eof`、`ID` separator 注入、inline-image 切替、raw token/discard/output、`handle_eof` 成功後の永久 detach と finish/error timing）+ production consumer `content_normalizer.rs`（bad-token state、CR/string/name normalization） | ✅ |
| `QPDFObjectHandle::TokenFilter` / `QPDF_Stream::addTokenFilter` / `isDataModified` | `QPDFObjectHandle.hh:129-190,420-475,978-1010`; `QPDF_Stream.cc:321-324,488-620,663-666` | `ObjectHandle::add_token_filter` / `is_data_modified` が共有filter listとdecoded→token-filter→normalize/encodeのlazy pipeを担う。`form_field_object_helper/rendering.rs` の既存 `/AP/N` reuse は eager `replace_stream_data` からqpdf `ValueSetter`相当の `AppearanceTokenFilter` へ移行し、`writer/plain/body.rs` は `is_data_modified` をlone-Flate fast pathの条件に含める。`linearization/writer.rs` の `append_body_object`（`stream_is_data_modified` helper 経由）も同じ `willFilterStream` 由来のゲートを適用: qpdf の `writeLinearized` は `QPDF::optimize` の `skip_stream_parameters` probe と実書き込みの計2回 `pipeStreamData` を呼び、token filter は pipe 間で状態リセットしないため実書き込み側は exhausted filter のパススルー（= stale content）を再エンコードする。flpdf の linearized writer には optimize 相当の二重 pipe が無いため、token filter 自体は起動せず「既に materialize 済みの (pre-filter) バイトを decode→re-encode」するだけで同じ observed output に一致させる（`docs.rs` 非公開のモジュール内 doc 参照）。`writer.rs` の `emit_canonical_pdf_inner` fallback と `writer/plain/body.rs` の `!plan.canonical` 分岐は同種のゲートが未配線（flpdf-vkka で追跡） | ✅ |
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
| `QPDFAcroFormDocumentHelper.cc` | 1047 | `signatures.rs`(280-447: `disableDigitalSignatures` / `analyze` / `traverseField`) + `acroform_document_helper.rs`(1096) + `overlay_annotations.rs`(2263: `transformAnnotations` / `addAndRenameFormFields`、`/DA` の resource-name replacement consumer) + `overlay_appearance_stream.rs`(720: `adjustAppearanceStream`、AP stream consumer) | 🔀 |
| `QPDFPageObjectHelper.cc` | 1039 | `page_object_helper.rs`(766) + `page_form_xobject.rs`(637) + `resources.rs`(1229: `ResourceFinder` を使う resource pruning consumer) + `page_annotation_flatten.rs`(596) + `overlay.rs`(2228: `placeFormXObject`) + `overlay_annotations.rs`(2263: `copyAnnotations`) | 🔀 |
| `QPDFFormFieldObjectHelper.cc` | 852 | `form_field_object_helper.rs` + `form_field_object_helper/rendering.rs` + `default_appearance.rs`（field lookup/mutation と Tx/Ch appearance generation。`QPDFFormFieldObjectHelper.cc:472-478` に従い Btn appearance は production dispatch から除外） | 🔀 |
| `QPDFPageDocumentHelper.cc` | 158 | `page_document_helper.rs`(236) + `page_extract.rs`(`extract_pages`/`extract_page`) + `page_merge.rs`(`merge_documents`)。両モジュールとも `Pdf::empty()` へ委譲（`emptyPDF()` + `addPage()` の library-level 経路、doc に明記） | 🔀 `emptyPDF()` 自体（`QPDF.cc:34-51,290-293`）の canonical 実装は `engine.rs`(475: `Pdf::empty()` は `open_mem_owned` へ委譲し、両者で `emptyPDF()` / `processMemoryFile()` 相当の construction path を担う)。⚪ qpdf の `emptyPDF()` は default-construct 済み `QPDF` を遅延初期化する `void` メンバー関数だが、flpdf の `Pdf` に「未初期化」状態が無いため static factory（`Result<Self>` を返す）に置き換えている。バイト列・parse 経路（`open_mem_owned` = `processMemoryFile` 相当）は同一。QPDFJob 相当のバージョン蓄積（`max_input_version`）は library level のこれらの関数ではなく `job/`（`flpdf-jq0z`）が担う想定 |
| `QPDFAnnotationObjectHelper.cc` | 226 | `annotation_helper.rs` + `page_annotation_enum.rs`(249) | 🔀 |
| `QPDFOutlineDocumentHelper` / `QPDFOutlineObjectHelper` | 198 | `outline_document_helper.rs`(476) + `outline.rs`(143) | ✅ live `ObjectHandle` route: `OutlineItem.object`/`dest` retain canonical identity, `/Dest` and `/A /GoTo /D` use qpdf-shaped handle accessors, named destinations use `HandleNameTree`, and JSON consumes the handles directly. The narrow terminal-handle chase only covers flpdf's temporary `Pdf::set_object` bare-reference bridge; parsed qpdf objects stay one-hop/live. |
| `QPDFPageLabelDocumentHelper.cc` | 134 | `page_label_document_helper.rs`(1037) + `nntree.rs` (`HandleNumberTree`) | ✅ canonical ObjectHandle route for `hasPageLabels`, `getLabelForPage`, `getLabelsForPageRange`, and `pageLabelDict`; typed page-operation adapters and JSON migration remain downstream |
| `QPDFNameTreeObjectHelper` / `QPDFNumberTreeObjectHelper` / `NNTree.cc` | 1394 (`34-75,106-168,216-390,391-520,560-700`) | `nntree.rs`（shared canonical `ObjectHandle` engine + `NameTree`/`NumberTree` wrappers）+ consumer adapters。qpdf の live `QPDFObjectHandle`/`QPDF_Array` mutation（`NNTree.cc:34-75` の iterator value 更新、`:106-168` の limits、`:216-390` の split/insert、`:391-520` の remove/deepen、`:560-700` の find）に対応し、`ResolvedArray` は `ObjectHandle::set_array_items` で alias を保持したまま更新、direct kid の indirect 化は `Pdf::make_indirect_from_object_handle`、root split は既存 root slot を維持する。dirty propagation で raw compatibility read/writer も canonical mutation を観測する。`Object` は root の compatibility projection と test-only raw fixture boundary に限定し、production tree mutation は `Pdf::set_object` を経由しない | ✅ |
| `QPDFEmbeddedFileDocumentHelper.cc` | 122 | `embedded_files.rs`(678) | 🔀 モジュール doc 自身が「完全な公開ヘルパー境界を持たない」と申告。D1 未達 |
| `QPDFFileSpecObjectHelper` / `QPDFEFStreamObjectHelper` | 280 | `filespec_helper.rs`(1324) | 🔀 モジュール doc 自身が「partial helper surface; 公開 API は未完成」と申告。D1 未達。なお `json_inspect.rs` が同じ責務を再実装しているため D2 も未達（`flpdf-q2fo` で解消） |
| `ResourceFinder.cc` | 56 | `resource_finder.rs`（operator/name tracking と resource type/offset 集約。flat `getNames()` oracle view は categorized map から test 内で導出）。production consumer は `resource_replacer.rs` と `resources.rs` の resource pruning | ✅ |
| `QPDFAcroFormDocumentHelper.cc` anonymous `ResourceReplacer` | — | `resource_replacer.rs`（`ResourceFinder` の name offsets を exact-byte 置換）。production consumer は `overlay_annotations.rs` の `/DA` と `overlay_appearance_stream.rs` の AP streams | ✅ |
| `QPDFDocumentHelper.cc` / `QPDFObjectHelper.cc` | 12 | 基底トレイトが無い | ⚪ |

## 8. JSON

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `JSON.cc` | 1401 | `json/`（全 write helper、blob callback、unparse が public `Pipeline` 境界を使用。serializer は caller-owned outer pipeline を finish しない） | ✅ |
| `JSONHandler.cc` | 189 | `json/` | ✅ |
| `QPDF_json.cc` 入力側（`QPDF_json.cc:1-833`: `JSONReactor` / `createFromJSON` / `updateFromJSON` / `importJSON` / `test_json_validators`） | 833 | 無し。flpdf に `--json-input` 相当は存在しない | ❌ |
| `QPDF_json.cc` 出力側（`QPDF_json.cc:834-946`: free function `writeJSONStreamFile`(834-849) + `QPDF::writeJSON` ×2 overload(851-946)） | 113 | `document_json.rs`(361: `write_json` = 6 引数 overload(851-861)、`write_json_key` = `complete`/`first_key` overload(863-946)、`write_json_stream_file` = `writeJSONStreamFile`。side file は `PlStdioFile` explicit finish) | ✅ 出力側は境界一致。入力側（下記 ❌ 行）が未実装なので `QPDF_json.cc` 全体としては D1 未達。`qpdf --json-output=2` は complete overload と同一バイトを書くため、`crates/flpdf/tests/document_json_tests.rs` が 7 fixture で qpdf 出力と直接照合する |
| `QPDFObjectHandle::getJSON` / `QPDFObjectHandle::writeJSON`（行数は §1 の `QPDFObjectHandle.cc` に計上済み。ここは所在の相互参照） | — | `json_inspect.rs` の `pdf_object_to_json`（`getJSON`）/ `ordered_qpdf_object`・`ordered_qpdf_dict`・`RawPdfJsonWriter`（`writeJSON` の raw incremental 版） | 🔀 `getJSON` は `dereference_indirect=false` のモードのみ。`pdf_object_to_json` は引数を持たず間接ハンドルを常に `"N G R"` として出力するため、`true`（間接参照を解決して出力）のモードは未実装。`writeJSON` 相当は `document_json.rs` から呼ばれるが `json_inspect.rs` 側に置いたまま（分離は未着手） |
| `QPDF_Stream::writeStreamJSON`（行数は §1 の `QPDF_Stream.cc` に計上済み。ここは所在の相互参照） | — | `json_inspect.rs` の `stream_payload_with_decode_status` / `normalized_emitted_stream_dict` + `document_json.rs` の object entry writer に inline された `"data"` / `"datafile"` / `"dict"` 出力 | 🔀 1 関数に対応する flpdf 実装が存在せず、payload/dict 導出と出力が別モジュールに分かれている。qpdf の `no_data_key` / `attempt` 二重試行は未移植 |

## 9. Job / CLI

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFJob.cc` | 3116 | `flpdf-cli/src/main.rs`(6796) + `job/json.rs`（`QPDFJob::writeJSON` の出力選択部分、`QPDFJob.cc:3094-3115`）+ **`json_inspect.rs` の `doJSON*`**（下記。`QPDF::writeJSON` 相当は `document_json.rs` に分離済み — §8）+ `overlay*.rs` + `page_merge.rs`(1117) + `check.rs`(360) + `attachment_list.rs`(1074: `QPDFJob.cc:876-911` の `doListAttachments` 移植。`<file> has no embedded files` は infilename を要するため CLI 側に残す) + `acroform_field_prune.rs`(497: `QPDFJob.cc:2610-2632` の "Remove unreferenced form fields"。`prune_acroform_after_subset` が CLI から呼ばれる) + page 操作群 | 🔀 `job/json.rs` は JSON 出力選択だけを移植した slice で、`QPDFJob` 全体の移植ではない。完全な `QPDFJob` 集約モジュールは依然**存在しない**。集約は `flpdf-q2fo`(D1) / `flpdf-ukux`(D2) / `flpdf-s5cw`(D3) |
| `QPDFJob_config` / `_argv` / `_json` / `QPDFArgParser` / `QPDFUsage` | 3164 | clap で代替 | ⚪ |
| `QPDFLogger.cc` | 255 | `logger.rs`（private stdout tracker、shared info/warn/error/save routes、standard stdout/stderr/discard、reset/following、save collision、custom sink ownership）+ `reader/resolver.rs` / `reader.rs`（文書 warning の append-then-route、suppression、live logger replacement）+ `flpdf-cli/src/main.rs`（下記 qpdf-equivalent consumers） | ✅ `QPDFLogger.cc:9-40,43-51,80-254`。`diagnostics.rs` は logger ではなく collection-only value store として維持する |

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

- `run_show_info` / `run_show_catalog` / `run_show_metadata` / `run_show_outline` /
  `run_show_fonts`: flpdf-only inspection で、qpdf 11.9.0 `QPDFJob` に同じ command consumer がない
- `run_show_stream` の passthrough-codec marker: flpdf-only fallback 表示で、qpdf は
  unfilterable stream を同じ marker へ変換しない
- native `rewrite --static-id` warning、`--remove-restrictions` intent diagnostic、
  `copy-attachments-from` count: flpdf-only surface
- clap 前後の immediate option-validation diagnostics: production `QPDFJob` aggregation
  (`flpdf-25kg.5.2`) より前の CLI shell responsibility。最終 dispatch error と usage result
  は logger 済みだが、この validation 群は同 downstream cutover まで direct stderr を維持する

### `QPDFJob.cc` の `doJSON*` 族 — `json_inspect.rs` に埋没

`QPDFJob.cc:958-1620` の JSON セクション生成が `json_inspect.rs` にある。
同ファイルは `QPDF::writeJSON` 相当の serialization も担う。一方、
`QPDFJob::writeJSON` の `QPDFJob.cc:3094-3115` にある top-level 出力先・
stream side-file prefix の選択だけは `job/json.rs` が担い、その後の JSON
構築と出力 lifecycle は `json_inspect.rs` へ委譲する。
**§8 の `QPDF_json.cc` 行と混同しないこと**（`QPDF_json.cc` は JSON 入力と
`writeJSON` であって、セクション生成ではない）。

| qpdf `QPDFJob.cc` | flpdf `json_inspect.rs` |
|---|---|
| `doJSONObjects`(958) の **v1 分岐**(960-981) / `doJSONObjectinfo`(1002) | **対応物なし**。どちらも JSON v1 専用（`doJSONObjectinfo` は `QPDFJob.cc:1620` の version guard 内、`objects` の schema も `json_schema:1357` で v1 限定）。flpdf CLI は `--json=2` のみを受け付け、`main.rs:1914` が `objects` / `objectinfo` を「v1 でのみ有効」と明示的に拒否する |
| `doJSONObjects`(958) の **v2 分岐**(981-997) | 自前では何も組み立てず `QPDF::writeJSON` に委譲するだけ。実体は §8 の `QPDF_json.cc` 出力側の行を参照（ここに再掲すると二重帰属になる） |
| `doJSONPages`(1030) | `build_pages_section` |
| `doJSONPageLabels`(1095) | `build_pagelabels_section` |
| `doJSONOutlines`(1143) | `build_outlines_section` |
| `doJSONAcroform`(1159) | `build_acroform_section` |
| `doJSONEncrypt`(1206) | `build_encrypt_section` |
| `doJSONAttachments`(1281) | `build_attachments_section` |
| `json_schema`(1332) / `json_out_schema`(1533) | `JsonKey` ほか |
| `doJSON`(1545) | `write_qpdf_json_v2_selected_objects*` |

**qpdf 側の `doJSON*` は辞書を直接歩かず、ヘルパーの薄い JSON 化層でしかない。**
flpdf の現行実装はヘルパーを経由せず辞書を直接歩いており、PR #613/#614 で
以下が同時に露出した: `preferredname` の Mac/DOS 優先順位バグが
`json_inspect.rs` と `filespec_helper.rs` の**両方で独立に発生**、
`modificationdate` が `QPDFEFStreamObjectHelper::getCreationDate()` を経由せず
qpdf 側のコピペバグ（`QPDFJob.cc:1319-1322`）を再現できていなかった、
`fieldtype` の先頭 `/` 欠落（`getFieldType()` 未経由）、
`build_acroform_section` の走査モデルが qpdf と構造的に別物（`flpdf-d949`）。

**同じ責務が 2 箇所に実装されている状態そのものが D2 違反**であり、
ヘルパー側が D1 未達であることとは独立した問題である。Tier D1（`flpdf-q2fo`）は
「ヘルパーへ載せ替える」だけでなく、載せ替え先のヘルパーが D1 を満たすことを
前提とするため、下表の 🔀 を先に解消する必要がある。

| doJSON* | 経由すべきヘルパー | §7 の状態 |
|---|---|---|
| `doJSONAcroform` | `QPDFAcroFormDocumentHelper` + `QPDFFormFieldObjectHelper` + `QPDFAnnotationObjectHelper` + `QPDFPageDocumentHelper` | 🔀 / 🔀 / 🔀 / 🔀 |
| `doJSONAttachments` | `QPDFEmbeddedFileDocumentHelper` + `QPDFFileSpecObjectHelper` + `QPDFEFStreamObjectHelper` | 🔀 / 🔀（いずれもモジュール doc 自身が公開 API 未完成と申告。加えて `json_inspect.rs` の再実装により D2 も未達） |
| `doJSONPages` | `QPDFPageDocumentHelper` + `QPDFPageObjectHelper` | 🔀 / 🔀 |
| `doJSONOutlines` | `QPDFOutlineDocumentHelper` | ✅ |
| `doJSONPageLabels` | `QPDFPageLabelDocumentHelper` | 🔀 |

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

`page_merge.rs`(1117) / `page_rotate.rs`(632) / `page_split.rs`(454) / `page_extract.rs`(435) /
`page_range.rs`(379) / `page_splice.rs`(304) / `page_combine.rs`(278) / `page_plan.rs`(210) /
`rotate_spec.rs`(204) / `page_collate.rs`(145)

分解自体は妥当だが、qpdf のどの関数に相当するかが doc から追えない。

### C. qpdf に機能そのものが無いもの

| flpdf | 行 | 備考 |
|---|---|---|
| `standard_font_metrics.rs` | 4,633 | qpdf にフォント幅テーブルは存在しない（`grep -rl Helvetica libqpdf/` が 0 件） |
| `signatures.rs` の**検査 API のみ** | — | 署名の読み取り検査。qpdf に相当機能なし |
| `qdf_fix.rs` | 764 | qpdf では `qpdf/fix-qdf.cc`（libqpdf 外の別バイナリ） |
| `fonts.rs` | 192 | `--show-fonts` の実体（`font_entries`(30) / `font_entries_with_max_depth`(43)）。qpdf にフォント一覧機能は無い（`qpdf --help=all` に font 関連の記載なし） |


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
ミューテータ側（`check_array_item_ownership`）はまだこの shallow 比較に揃って
おらず、`belongs_exclusively_to_pdf` の子孫再帰に依存したまま（flpdf-25kg.3.16.7.1
で追跡）。

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
| null 可視性 | `cmp_null_visibility_tests` ⚠ **CI 未列挙**（`flpdf-qxba.2`） | — |
| QDF | 🟡 **部分的にあり**（下記）。`overlay::byte_gate` の QDF 12 件を含む | 🟡 `cli_byte_identical_overlay.rs` の QDF 3 件 |
| 暗号化出力 | ❌ gated byte gate 無し | ❌ |
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
CI で走らない。11 件中 `cmp_null_visibility_tests` のみが漏れている。

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
| `std::shared_ptr<Buffer> QPDF_Stream::stream_data`（`libqpdf/qpdf/QPDF_Stream.hh:104`） → `Rc<Vec<u8>>`（`object_handle.rs` の `ObjectValue::Stream`） | 1 | 無し（共有の意味論は同一。`QPDFObjectHandle::newStream(QPDF*, shared_ptr<Buffer>)` / `replaceStreamData(shared_ptr<Buffer>, ..)` / `QPDF_Stream::getStreamDataBuffer` に対応する `ObjectHandle::stream` / `replace_stream_data` / `as_stream_data` が buffer を共有したまま受け渡す。`Rc<[u8]>` ではなく `Rc<Vec<u8>>` なのは、`Rc::<[u8]>::from(vec)` が refcount ヘッダを前置できず payload 全体を memcpy するため（`page_split.rs:376-386` に同じ罠を実測付きで記録）。二段の間接になるのは `shared_ptr<Buffer>` と偶然一致するだけで対応関係ではない — qpdf が `Buffer` 型を要するのは C++ が borrow/own を型で表せず実行時フラグに畳むからで（`include/qpdf/Buffer.hh:35-46` が所有・非所有の両コンストラクタを持つ）、その面は既存の `Buffer` → `Vec<u8>` 行が扱う。`Arc` ではなく `Rc` なのは `Repr` が `Rc<RefCell<..>>` ベースで `ObjectValue` がそもそも `!Send` のため。`replace_stream_data` は `QPDF_Stream::replaceFilterData`（`QPDF_Stream.cc:668-684`）に対応する共有 helper を通り、zero length では `/Length` を削除、nonzero では正確な integer を設定する（`flpdf-25kg.4.5`）。byte-identical suite（`qpdf-zlib-compat`）で確認済み） |
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

## 集計

| 状態 | qpdf 側の該当行数 | 内訳 |
|---|---|---|
| ✅ 境界一致 | 4,020 | 責務境界は一致。**再配置は不要だが「完成」ではない** — DoD D1〜D5 の充足は各スライスで別途検証する |
| 🔀 smeared | 27,540 | 再配置の主対象。qpdf 全体の 66% |
| ❌ missing | 1,002 | `Pl_DCT.cc` compression(119) / `QPDF_json.cc` 入力側(833) / `QTC`(50) |
| ⚪ 逸脱候補 | 6,660 | 要承認（下記の方針矛盾を参照） |
| ➖ 対象外 | 2,237 | C API |
| **合計** | **41,459** | qpdf `libqpdf/*.cc` の実測 41,459 行と一致 |

本文の各行を機械的に集計した値である（`状態` 列の記号ごとに `行` 列を合算）。
**この合計もスナップショットであり、維持対象ではない**（上記「行数の位置づけ」参照）。
読み取るべきは「smeared が 7 割を占める」という規模感であって、個々の値ではない。
数値を更新する場合に限り、合計が qpdf 実測と一致することを確認する。
過去に 41,336 と記載して 123 行の欠損があったが、内訳は集計漏れ 185 行
（`ランダム源 3 ファイル` 行）と汎用 `Pl_*` 行の過大記載 −62 行だった。
どの qpdf ファイルもいずれかの行に属していることは確認済み。

**❌ の数え方**: 以前は `Pipeline.cc` + `Pl_*.cc` 21 ファイル計 ~2,400 行を丸ごと
missing として傘で数えていたが、個々の `Pl_*` は下の各行で 境界一致 / smeared /
逸脱候補として個別に分類されており**二重計上**だった。傘の行を `Pipeline.cc`
本体（114 行）に限定し、真に未マップな qpdf 行だけを ❌ に数えるよう改めた。
