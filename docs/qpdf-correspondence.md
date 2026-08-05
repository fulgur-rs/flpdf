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

**(b) 未完成の部品が ✅ になっていた 3 行 → 🔀**

いずれもモジュール doc 自身が未完成を申告しており、責務境界が一致しているとは
言えない。D4 索引でも `correspondence` に分類されている。

| 節 | qpdf | flpdf | doc の自己申告 |
|---|---|---|---|
| §7 | `QPDFFileSpecObjectHelper` / `QPDFEFStreamObjectHelper` | `filespec_helper.rs` | 「partial helper surface; 公開 API は未完成」 |
| §7 | `QPDFEmbeddedFileDocumentHelper.cc` | `embedded_files.rs` | 「完全な公開ヘルパー境界を持たない」 |
| §7 | `QPDFPageLabelDocumentHelper.cc` | `page_label_document_helper.rs` | 「公開 API 未完成 + single-implementation 監査未了」 |

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

## 1. オブジェクトモデル

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFObjectHandle.cc` | 2601 | `object.rs`(1301) + `object_handle.rs`(shared handle identity・parsed offset・遅延解決) + `qpdf_null.rs`(9-37: `reference_is_null` / `value_is_null` = `isNull` の間接参照解決) + `overlay_annotations.rs`(1685-1737: `merge_resources_shallow` = `mergeResources`) + `overlay_appearance_stream.rs`（段階的 conflict merge の再現） | 🔀 アクセサが各所に散在（`flpdf-mfir`）。object identity / 遅延解決は `object_handle.rs` へ移行中（`flpdf-egzr.3.1`） |
| `QPDFObjectHandle::isNameAndEquals` / `isDictionaryOfType` / `getArrayNItems` / `getArrayItem` / `isOrHasName`（行数は上段に計上済み） | — | `object_handle.rs` の `try_is_name_and_equals` / `try_is_dictionary_of_type` / `try_array_len` / `try_array_item` / `try_is_or_has_name`（`QPDFObjectHandle.cc:456-466,759-785,1027-1039`） | ✅ holder と child を qpdf 順に lazy resolve。container borrow は resolver 再入前に解放し、配列全体を snapshot しない。`try_array_item` は `QPDF::decryptStream` が equal-length 確認後に使う valid-index 面のみで、qpdf が warning と特殊 null を返す invalid access は契約外 |
| `QPDF_Array/Dictionary/Stream/String/Name/Real/Integer/Bool/Null/InlineImage/Operator/Reserved/Unresolved/Destroyed.cc` | 1814 | `object.rs` の `Object` enum に統合 | 🔀 |
| `QPDFObject.cc` / `QPDFValue.cc` | 79 | `object.rs` の `Object` + `object_handle.rs` の `ObjectHandle` / `ObjectValue`（共有 identity・qpdf 互換 parsed offset・`IndirectState` 遅延解決） | 🔀 `object.rs` の `Object` は静的な値表現のみ。`QPDFValue` 相当の共有 identity・parsed offset・遅延解決状態は `object_handle.rs` が新たに担う（layer cutover 進行中）。両モジュールに分割されているため `✅` から変更 |
| `QPDFObjGen.cc` | 68 | `object.rs` の `ObjectRef` | ✅ |
| `QPDFXRefEntry.cc` | 51 | `xref_entry.rs`（`XrefEntry` = free / uncompressed / compressed の 3 variant）。consumer は `xref.rs` / `reader.rs` / `cache.rs` / `writer.rs` / `writer/{object_streams,plain/plan}.rs` / `linearization/{writer,plan}.rs` | ✅ `flpdf-qxba.9.2` で完全 cutover（`XrefOffset` 削除）。`xref.rs` 側に型定義は残っていない |
| `PDFVersion.cc` | 68 | `pdf_version.rs` の `PdfVersion` | ✅ |
| `QPDFMatrix.cc` | 140 | `matrix.rs` の `Matrix` / `Rectangle` | ✅ |

## 2. パース / 読み取り

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF.cc` | 2667 | `reader.rs`(7898) + `reader/resolver.rs`(2367: canonical resolver。`QPDF::resolve` が触る `QPDF::Members` — `m->file` / `m->xref_table` / `m->obj_cache` / `m->resolving` / `m->resolved_object_streams` / `m->attempt_recovery` / `m->encp` — を `ResolverCore` に集約し、`Rc<RefCell<..>>` 経由で `ObjectHandle` の `Weak<dyn DocumentResolver>` から到達可能にする。`m->obj_cache` は canonical handle registry そのもので、`Pdf::get_object_handle`（= `QPDF::getObject`, `QPDF.cc:1952-1959`）と `Pdf::drop`（= `~QPDF`）の両方がここを見る。`m->encp`（`flpdf-25kg.3.11`）は `Pdf::encryption` と同一の `Rc<RefCell<Option<EncryptionState>>>` を共有し、qpdf の `shared_ptr<EncryptionParameters>` を複数の owner が保持する形を再現する — 現時点では flpdf-25kg.3.10 の pipe 時復号 primitive がまだ実装されておらず、この cell の唯一の書き手は `Pdf::authenticate_if_encrypted` のまま。`flpdf-25kg.3.5` slice 1 時点では `readObjectAtOffset`/`readObject`/`readStream` の uncompressed（xref type 1）経路のみ移植済みで、ObjStm / 暗号化（`m->encp` はパラメータの受け皿として到達可能になっただけで、resolve 時の文字列復号・pipe 時のストリーム復号はいずれも未対応。前者は `flpdf-25kg.3.5` AC2、後者は `flpdf-25kg.3.10` が担う） / xref stream / recovery は未対応。**行数は slice 進行中のため暫定値**) + `reader/file_object.rs`(1405) + `xref.rs`(1220) + `object_copy.rs`(342: `copyForeignObject`) + `cache.rs`(112: xref 由来の `ObjectCache` / `CacheEntry`。消費者は `reader.rs`) + `writer/object_streams.rs`(207-237: `compressible_objgens_qpdf_plan` = `getCompressibleObjGens`、`QPDF.cc:2392-2445`)  + `signatures.rs`(245-: `removeSecurityRestrictions`) + `page_closure.rs`(441: `page_object_closure`。`object_copy.rs` は pre-closed な集合しか受け取らず、両者で `copyForeignObject` 相当を構成する) + `ref_chain.rs`(159: `resolve_ref_chain` / `terminal_ref_of_chain` / `MAX_REF_CHAIN_DEPTH` — 深さ上限付き間接参照解決の共有プリミティブ。20 モジュールが使用) | 🔀 |
| `QPDF.hh`（`EncryptionParameters`） | 899-921 | `reader.rs`(191-206: `EncryptionState`)。qpdf は独立した2つの bool、`encrypted` / `encryption_initialized`（`QPDF.hh:907-908`）を持つが、flpdf はこれを単一の `Option<EncryptionState>`（`None` = 未初期化 or 認証済み未暗号化のいずれか、`Some` = 認証済み暗号化）に畳んでいる。安全性の根拠: `encryption_initialized` の唯一の用途は `initializeEncryption()`（`QPDF.cc:471` で1文書につき高々1回しか呼ばれない）内の再入防止ガード（`QPDF_encryption.cc:721,724`）で、flpdf の構造上この再入自体が起こり得ないため観測可能な挙動差は生じない。逸脱理由は `reader/resolver.rs` の `ResolverCore::encryption_parameters` doc にも記載（`flpdf-25kg.3.11`） | ⚪ |
| `QPDF::interpretCF` (`QPDF.hh`; `QPDF_encryption.cc`) | `1122-1127`; `700-716` | `reader.rs` の `interpret_cf_name` / `interpret_cf` / `interpret_cf_from_handle` | ✅ 値選択を共有し、ObjectHandle 版は `try_as_name` で lazy resolve。`crypt_filters` → built-in `/Identity` → `e_unknown`、non-name → `e_none` の順と resolver error 伝播を維持。production consumer cutover は `flpdf-25kg.3.12` |
| `QPDFParser.cc` | 519 | `parser.rs` の `Parser`（Object / Content mode）。Content mode は EOF → `None`、word → `Object::Operator`、間接参照化の抑止を共有 object grammar 上で実装し、`content_stream.rs` が使用（`QPDFParser.cc:27-125,130-377`） | 🔀 content branch は対応済み。file-object parser 全体の API / recovery 差分は未精査 |
| `QPDFTokenizer.cc` | 965 | `tokenizer.rs`（18 token types、owned value/raw/error bytes/offset、push/pull、pull-only `allowEOF`、`includeIgnorable`、space/comment、bad-token recovery、max length、`betweenTokens`、unread、inline-image `EI` discovery。`QPDFTokenizer.hh:34-193`; `QPDFTokenizer.cc:45-965`）+ `parser.rs` の content mode + `content_stream.rs` の `ParserCallbacks` orchestration + `object.rs` の `Operator` / `InlineImage`（`QPDFParser.cc:27-125,130-377`; `QPDFObjectHandle.cc:1770-1847`） | ✅ `QPDFTokenizer` の責務境界を移植済み。object/parser/content callback consumers は共有 tokenizer を使用し、旧 content lexer は削除 |
| `InputSource` 系 5 ファイル | 625 | `Read + Seek` ジェネリクスで代替。所有者は `reader/resolver.rs` の `ResolverCore`（`m->file` 相当）。`ResolverCore` のメソッドは `InputSource` の 3 操作 `seek`/`tell`/`read`（`InputSource.hh:71-74`）に限定し、`OffsetInputSource`（`QPDF.cc:406`）が担う header shift は `seek`/`tell` が適用する。例外は `rewind_underlying_source` 1 つで、これは wrapper が持つ `proxied`（`libqpdf/qpdf/OffsetInputSource.hh:24`）に相当する — `OffsetInputSource::rewind` は logical 0 に行く（`OffsetInputSource.cc:55-59`）ため `m->file` では表現できない。owned-window 系の legacy helper（`read_window` / `read_physical_input`）は `ResolverHandle` 側の `qpdf-legacy-tenant` で、`ResolverCore` の面には置かない | ⚪ |
| `QPDF_pages.cc` | 319 | `pages.rs`(741) + `page_tree_rebuild.rs`(390) + `optimization/inherited_attrs.rs`(575: `QPDF_pages.cc:39-138` の `getAllPagesInternal` 修復を移植。`linearization/plan.rs:773` と `linearization/writer.rs:2582` から呼ばれる) | 🔀 |
| `QPDFExc.cc` / `QPDFSystemError.cc` | 123 | `error.rs`(125) | ✅ |

## 3. 書き込み — 最大の smear

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFWriter.cc` | 3044 | `writer.rs`(4492) + `writer/serialize.rs`(1008) + `writer/object_streams.rs`(739) + `writer/plain/{plan,body,xref}.rs`(898) + `linearization/writer.rs`(3603) + `linearization/part1.rs`(370) + `linearization/back_patch.rs`(324) + `linearization/renumber.rs`(850) + `rewrite_renumber.rs`(893) = **13,177 行 / 11 ファイル**。加えて `object.rs`(412: `write_pdf` = `unparseObject` / 491: `write_pdf_qdf` / 585-: trailer `/ID` = `writeTrailer`。`writer.rs` と `linearization/writer.rs` が委譲) と `qpdf_null.rs`(38-57: `visible_entries` = `QPDFWriter.cc:1491` の null 値 dict キー抑制)。さらに `object_handle.rs`(1705-: `unparse_object` / 1745-: `unparse_object_qdf` / 2302-: `unparse_stream_body` / 2375-: `unparse_stream_body_qdf` / 2569-: `unparse_trailer` = `unparseObject`(`QPDFWriter.cc:1318-1605`、dict 分岐 `:1346-1527`、stream 分岐 `:1528-1605`) / `writeTrailer`(`:1160-1230`) の `ObjectHandle` 版。`object.rs` の materialize-to-`Object` bridge を経由せず `ObjectHandle` のグラフを直接歩く新 primitive 群（`flpdf-egzr.3.2.13`）。`unparse_stream_body_qdf` は最終レビューで見つかったギャップの修正（Task 9）: `write_pdf_stream_qdf`(`object.rs:1036`、real production callsite は `writer.rs:4437`)に対応する QDF+stream 形の primitive が欠けていた。`Dictionary::write_pdf_stream_qdf` 自身に `refiltered` 概念が無いため（唯一の呼び出し元 `write_stream_to_buf_qdf` は既に確定済みの `/Filter`/`/Length` を持つ dict しか渡さない）、`unparse_stream_body`（compact 版）と異なりこちらも `refiltered` パラメータを持たない。null 値 dict キー抑制(`:1490-1491`)は `try_is_null` 経由で `unparse_object`/`unparse_object_qdf`/`unparse_stream_body`/`unparse_stream_body_qdf` の4つに適用し、`unparse_trailer` は `writeTrailer` 自身と同様に無抑制。全て `pub(crate)`・`#[allow(dead_code)]`、production consumer 移行は `flpdf-egzr.3.2.5` 待ち | 🔀 |

qpdf は 1 クラスで standard / linearized / encrypted / objstm を統一的に扱う。flpdf は
経路ごとに分岐しており **xref 出力が 3 箇所**に分かれる。byte-parity の修正が片方の
経路にしか入らない構造的リスクがここに集中している。`write_pdf_full_rewrite_inner`
は単独で約 1,250 行。

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
| `rijndael.cc` / `AES_PDF_native` / `MD5_native` / `SHA2_native` | 1668 | `security/primitives.rs`(188)（外部 crate） | ⚪ |
| `RC4.cc` / `RC4_native.cc` | 63 | `security/rc4.rs`(80)（明示長キー / C-string キー、state 保持、separate / in-place processing） | ✅ |
| `QPDFCryptoProvider.cc` / `QPDFCrypto_*` | 774 | provider 抽象が無い | ⚪ |
| ランダム源 3 ファイル | 185 | `writer.rs` の `fresh_id_bytes` 等に散在 | 🔀 |

## 6. Pipeline / フィルタ

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `Pipeline.cc`（積層シンク基盤のみ。個々の `Pl_*` は下記の各行で個別に分類） | 114 | `pipeline.rs`（public `Pipeline` trait、identifier/write/finish lifecycle、logic/runtime error channel） | ✅ |
| `Pl_Count.cc` | 48 | `pipeline/count.rs`（byte count、last byte、forwarding、finish lifecycle） | ✅ |
| `Pl_MD5.cc` | 66 | `pipeline/md5.rs`（enable/persist/reuse、hex digest、forwarding/error order）+ `filespec_helper.rs`（EmbeddedFile `/Params /CheckSum` production consumer） | ✅ |
| `Pl_Flate` / `SF_FlateLzwDecode` | 946 | `pipeline/flate.rs` + `stream_filter.rs` の `FlateLzwStreamFilter`（`/Predictor` `/Columns` `/Colors` `/BitsPerComponent` `/EarlyChange` の解釈、codec → predictor の chain 構築、`QIntC::to_uint` の range error timing） | ✅ |
| `Pl_LZWDecoder` | 189 | `pipeline/lzw.rs`（3-byte rotating buffer、1 入力 byte あたり 1 code、table 成長と code 幅遷移、eod latch、qpdf の 7 種の診断文言）+ `stream_filter.rs` 経由の production decode | ✅ |
| `Pl_PNGFilter` | 232 | `pipeline/png_filter.rs`（32-bit wrapping の row 幅算出、constructor の 3 種 rejection、未知 filter byte の無視、finish の zero-pad row、Up 固定 encoder）+ `filters.rs` / `writer/serialize.rs` の production consumer。⚪ row buffer の確保だけは constructor ではなく最初の write まで遅延（出力バイト・呼び出し境界・エラー timing に影響しない） | ✅ |
| `Pl_TIFFPredictor` | 175 | 無し。`/Predictor 2` は pipeline 構築時点で `Error::Unsupported` として拒否する（明示的逸脱） | ❌ |
| `Pl_ASCII85Decoder` / `SF_ASCII85Decode` | 108 + 31 | `pipeline/ascii85.rs` + `stream_filter.rs` | ✅ |
| `Pl_ASCIIHexDecoder` / `SF_ASCIIHexDecode` | 96 + 31 | `pipeline/ascii_hex.rs` + `stream_filter.rs` | ✅ |
| `Pl_RunLength` / `SF_RunLengthDecode` | 146 + 38 | `pipeline/run_length.rs` + `stream_filter.rs` | ✅ |
| `Pl_AES_PDF` | 200 | `pipeline/aes.rs`（qpdf の contract を全量移植: block 単位の write バッファリング、first-block を IV として消費する復号側と IV を先頭へ書く暗号化側、ISO 32000-1 7.6.2 の padding とその strip、`useZeroIV` / `setIV` / `useStaticIV` / `disablePadding` / `disableCBC`）＋ `security/standard.rs` の AES single-buffer helper と `writer.rs` の stream consumer | 🔀 stage は移植済みだが production caller はまだ single-buffer helper 側にあり、同じ qpdf モジュールが 2 箇所に存在する。cutover は `flpdf-qynx.10`、pipe 時 decrypt への接続は `flpdf-25kg.3.10`。⚪ `QPDFCryptoImpl::rijndael_init` / `rijndael_process` の crypto provider 抽象は `aes` / `cbc` crate の直接利用に置換（§ 逸脱候補の crypto provider 行と同じ代替）。block ごとに 1 回 process する呼び出し形は保持し、chaining 状態のみ provider 側ではなく cipher が持つ。既知の逸脱: qpdf の padding strip は PKCS#7 厳密ではなく（末尾バイトが不整合ならブロックを丸ごと残す、`Pl_AES_PDF.cc:184-196`）、既存の `security/primitives.rs` の `decrypt_padded_mut::<Pkcs7>` は同じ入力を `Err` にする。`pipeline/aes.rs` は qpdf 側に合わせてあるので、cutover 前後で受理する文書が変わる |
| `Pl_RC4` | 43 | `pipeline/rc4.rs`（65,536-byte既定buffer、stateful `security/rc4.rs`、write/finish lifecycle）+ `reader.rs` / `writer.rs` の本番stream consumer | ✅ |
| `Pl_QPDFTokenizer.cc` / `ContentNormalizer.cc` | 141 | `pipeline/qpdf_tokenizer.rs`（optional downstream を持つ token-filter runner、EOF-token → `handle_eof`、`ID` separator 注入、inline-image 切替、raw token/discard/output、`handle_eof` 成功後の永久 detach と finish/error timing）+ production consumer `content_normalizer.rs`（bad-token state、CR/string/name normalization） | ✅ |
| `QPDFStreamFilter.cc` | 19 | `stream_filter.rs`（`set_decode_params`、decode pipeline factory、specialized / lossy の既定分類）。`QPDF_Stream::filterable`(`QPDF_Stream.cc:378-485`) 相当の `/Filter` `/DecodeParms` shape 読み取りも同モジュールが持ち、`flpdf-25kg.3.4` 以降は `Object` 版(`decode_filter_specs_from_object`) と `ObjectHandle` 版(`decode_filter_specs_from_handle`) の 2 つの shape reader が同じ分岐順で共通の `FilterSpec` を組む。下流（codec stack、predictor geometry、`max_output`、warning 順序）は `filters.rs` の `decode_prepared_specs` 1 実装を共有（本番 caller は shape ごとに 1 つずつの計 2 つ）。ただし `max_filter_chain` は shape reader 側で適用するため呼び出しが 2 箇所に分かれる。各 reader 単体の precedence は絶対値テストで pin 済みで、2 者間の整合を見るのは `handle_reader_matches_object_reader_for_every_filter_shape`。2 reader が一致するのは direct な子までで、間接参照を解決するのは `ObjectHandle` 版のみ（qpdf のアクセサ側に合わせた意図的な差）。`ObjectHandle` 版の decode entry point(`filters.rs` の `decode_stream_data_from_handle` / `decode_stream_data_recovering_from_handle`) は本番 caller をまだ持たず、`flpdf-25kg.3.5` の resolver 配線で接続される。既知の逸脱: unfilterable ケースで qpdf は warning を出したうえで `getStreamData` を失敗させるが flpdf は同文言を warning 無しの `Err` としてのみ返す（D3）/ qpdf が許容する null 値の `/DecodeParms` キーを flpdf は拒否する（`flpdf-h8mv`）。⚪ qpdf は `/DecodeParms` を `QPDFObjectHandle`（`shared_ptr`）のまま filter chain に複製するのに対し、flpdf の `DecodeParams` は所有 snapshot なので consumer が読むキーだけを保持する（`RETAINED_DECODE_PARAM_KEYS` の geometry 5 キーは全 filter で、`/Name` は唯一の読み手である crypt provider に合わせ `Crypt` 段のみ。name の byte 列を持つのもその 1 キーだけで、他の slot の name は `ParamValue::Other` に落ちる — qpdf 側も `isInteger()` 以外で非整数の種別を見ない）。出力バイト・filterability・エラー timing に影響しない（`SF_FlateLzwDecode` は名前の無いキーを `else` 無しで無視し、`SF_Crypt::setDecodeParms` は値を保存せず `decryptStream` が live graph から読み直す。書き出し側は source dictionary をそのまま複製し、この型から `/DecodeParms` を再構築しない）。`QPDF_Stream.cc` 本体の行は §1 にあり、二重帰属を避けてここには再掲しない | ✅ |
| `Pl_DCT.cc` | 326 | 無し。`json_inspect.rs` の `DecodeLevel::All`(758) が DCT デコードを doc で約束しつつ encoded バイトへフォールバックしている | ❌ 消費者あり |
| `Pl_Base64` / `Pl_Concatenate` / `Pl_OStream` / `Pl_String` | 282 | `pipeline/base64.rs` / `pipeline/concatenate.rs` / `pipeline/ostream.rs` / `pipeline/string.rs`（JSON serialization/output の本番 consumer を含む） | ✅ |
| `Pl_StdioFile.cc` | 46 | `pipeline/stdio_file.rs`（positive partial write の継続、zero/error—including `Interrupted`—の即時 Runtime 化、`EBADF` finish のみ Logic 化）+ `json_inspect.rs`（4096-byte buffer、top-level file は close/drop、side file は explicit finish） | ✅ |
| `Pl_Buffer` | 82 | `pipeline/buffer.rs`（accumulation、optional pass-through、finish readiness、buffer ownership transfer） | ✅ |
| `Pl_Discard` / `Pl_Function` | 85 | 専用 stage は未実装。使用箇所ごとの discard / closure 実装 | ⚪ |
| `Pl_SHA2.cc` | 75 | `pipeline/sha2.rs`（SHA-256/384/512 の bit 選択、`resetBits`/`write`/`finish` lifecycle、optional next への forwarding。`next` への転送は `finish()` の毎回無条件で qpdf 通り実行し、digest 自体だけを一部 error にする — 詳細は次行）。⚪ 4状態を defined logic error に変換する、理由は2種類。(1) メモリ安全性（対応する byte 列が存在しない）: `bits=0`（`resetBits` を一度も呼ばない）で `write`/`finish` すると null な `shared_ptr<QPDFCryptoImpl>` を dereference する。`finish()` 前の digest 読み取りも `SHA2_native::shaXXXsum` の未初期化メモリを読む。`libtests/sha2.cc` はどちらも未検証。(2) 決定的だが `sha2` crate の公開APIでは再現不能: `finish()` は crypto provider ポインタを null 化しないため、以下2状態はどちらも UB ではない。二重 `finish()` は `sph_sha2` の `md_helper.c`（"The context is NOT reinitialized by this function"）が示す通り、既に finalize 済みの running state に同一の padding block を再度 `RFUN` で圧縮し1回目と異なる digest を生成する。`finish()` 後に `resetBits()` を挟まず `write()` した場合も同じ `sc->count`/`sc->buf` の上に新規データが積まれ、同様に決定的だが別の結果になる。SHA-384 はどちらの再現も `sha2` crate の公開APIでは原理的に不可能（`sph_sha384_close` は 8×u64 語の running state を 48 バイト=6語に truncate して出力するが（`sha2big.c`: `sha384_close(cc, dst, 6)`）、`Sha384::finalize()` も同じ48バイトしか返さず残り2語を再構築する手段が無い。256/512 も leftover partial-block バイトが公開APIで取得できないため実質同様）。2種のうち一部だけ再現するのではなく4状態とも一律 error にする。出力バイトに影響しない（このパスへ到達する production consumer が存在しない） | ✅ |

`/ID` が qpdf と非 parity だった原因は **アルゴリズム**（qpdf は 2 段階 MD5 で seed を
作る）であり、Pipeline 抽象の有無ではない。flpdf は全体をバッファするので任意の
バイト範囲をダイジェストできる。`--deterministic-id` の byte-parity は
`deterministic_id_qpdf_parity_tests` で既にゲート済み。

## 7. ドキュメント / オブジェクトヘルパー

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFAcroFormDocumentHelper.cc` | 1047 | `signatures.rs`(280-447: `disableDigitalSignatures` / `analyze` / `traverseField`) + `acroform_document_helper.rs`(1096) + `overlay_annotations.rs`(2263: `transformAnnotations` / `addAndRenameFormFields`、`/DA` の resource-name replacement consumer) + `overlay_appearance_stream.rs`(720: `adjustAppearanceStream`、AP stream consumer) | 🔀 |
| `QPDFPageObjectHelper.cc` | 1039 | `page_object_helper.rs`(766) + `page_form_xobject.rs`(637) + `resources.rs`(1229: `ResourceFinder` を使う resource pruning consumer) + `page_annotation_flatten.rs`(596) + `overlay.rs`(2228: `placeFormXObject`) + `overlay_annotations.rs`(2263: `copyAnnotations`) | 🔀 |
| `QPDFFormFieldObjectHelper.cc` | 852 | `annotation_helper.rs`(748) + `appearance.rs`(2022) + `default_appearance.rs`(167) | 🔀 |
| `QPDFPageDocumentHelper.cc` | 158 | `page_document_helper.rs`(236) + `page_extract.rs`(435: `emptyPDF()` + `addPage()` 経路。doc に明記) | 🔀 |
| `QPDFAnnotationObjectHelper.cc` | 226 | `annotation_helper.rs` + `page_annotation_enum.rs`(249) | 🔀 |
| `QPDFOutlineDocumentHelper` / `QPDFOutlineObjectHelper` | 198 | `outline_document_helper.rs`(1499) + `outline.rs`(145) | ✅ |
| `QPDFPageLabelDocumentHelper.cc` | 134 | `page_label_document_helper.rs`(934) | 🔀 モジュール doc 自身が「公開 API 未完成 + single-implementation 監査未了」と申告。D1 / D2 未達 |
| `QPDFNameTreeObjectHelper` / `QPDFNumberTreeObjectHelper` / `NNTree.cc` | 1394 | `nntree.rs`（shared engine）+ `name_number_tree.rs`（compatibility wrapper）+ consumer adapters | ✅ |
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
| `QPDFLogger.cc` | 255 | `diagnostics.rs`(80) | 🔀 |

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
| `std::shared_ptr<QPDFValue>` → `Rc<RefCell<..>>`（`object_handle.rs`） | 79 | 無し（共有 identity の内部所有権機構のみ。byte-identical suite で確認済み） |
| `std::shared_ptr<Buffer> QPDF_Stream::stream_data`（`libqpdf/qpdf/QPDF_Stream.hh:104`） → `Rc<Vec<u8>>`（`object_handle.rs` の `ObjectValue::Stream`） | 1 | 無し（共有の意味論は同一。`QPDFObjectHandle::newStream(QPDF*, shared_ptr<Buffer>)` / `replaceStreamData(shared_ptr<Buffer>, ..)` / `QPDF_Stream::getStreamDataBuffer` に対応する `ObjectHandle::stream` / `replace_stream_data` / `as_stream_data` が buffer を共有したまま受け渡す。`Rc<[u8]>` ではなく `Rc<Vec<u8>>` なのは、`Rc::<[u8]>::from(vec)` が refcount ヘッダを前置できず payload 全体を memcpy するため（`page_split.rs:376-386` に同じ罠を実測付きで記録）。二段の間接になるのは `shared_ptr<Buffer>` と偶然一致するだけで対応関係ではない — qpdf が `Buffer` 型を要するのは C++ が borrow/own を型で表せず実行時フラグに畳むからで（`include/qpdf/Buffer.hh:35-46` が所有・非所有の両コンストラクタを持つ）、その面は既存の `Buffer` → `Vec<u8>` 行が扱う。`Arc` ではなく `Rc` なのは `Repr` が `Rc<RefCell<..>>` ベースで `ObjectValue` がそもそも `!Send` のため。byte-identical suite（`qpdf-zlib-compat`）で確認済み） |
| `QPDF_Array` borrow / slash 付き canonical name string → `Vec<ObjectHandle>` の単一 child clone / slash 無し decoded `Vec<u8>`（`object_handle.rs`） | 0 | 無し。`try_array_item` は `QPDF_Array::at` と同じ valid index の child identity を `Rc` clone で返し、name predicate は同じ decoded bytes を比較するだけで出力しない。本 branch では全 primitive が `pub(crate)` + `dead_code` で production consumer は後続 `flpdf-25kg.3.12` のため、既存 output/diagnostic path 自体が不変。invalid array access は契約外なので qpdf の warning timing も変更しない |

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
| ✅ 境界一致 | 3,813 | 責務境界は一致。**再配置は不要だが「完成」ではない** — DoD D1〜D5 の充足は各スライスで別途検証する |
| 🔀 smeared | 27,540 | 再配置の主対象。qpdf 全体の 66% |
| ❌ missing | 1,209 | `QPDF_json.cc` 入力側(833) / `Pl_DCT`(326) / `QTC`(50) |
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
