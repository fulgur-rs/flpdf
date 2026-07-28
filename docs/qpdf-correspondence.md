# flpdf ↔ qpdf 責務対応表

**Oracle:** qpdf 11.9.0 (`v11.9.0`) — `libqpdf/`
（`scripts/fetch-qpdf-source.sh` で取得。パスは `--print-path` で解決する。
本表のファイル名・行数はすべてこのツリーに対するもの。将来 v12 に追従する際は
`git log v11.9.0..v12.0.0 -- libqpdf/` が移植差分になる）
**調査日:** 2026-07-25
**関連:** `flpdf-qxba`（部品積み上げによる責務分割）/
[設計書](superpowers/specs/2026-07-25-qpdf-component-bottom-up-refactor-design.md)

pre-v1.0 の byte-identical 模倣方針（`CLAUDE.md`）に対し、flpdf の責務分割が qpdf と
どこまで対応しているかのスナップショット。`flpdf-qxba` の work-list であり、Phase 1
完了後に再測する。

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
| ✅ | **mirrors** — 対応が明確で責務境界も一致 |
| 🔀 | **smeared** — 実装はあるが複数モジュールに散在、または別モジュールに埋没 |
| ❌ | **missing** — flpdf に対応物が無い |
| ⚪ | **逸脱候補** — Rust/エコシステムで代替済み。移植しない提案（要承認） |
| ➖ | **対象外** — C API 等 |

---

## 1. オブジェクトモデル

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFObjectHandle.cc` | 2601 | `object.rs`(1301) + `qpdf_null.rs`(9-37: `reference_is_null` / `value_is_null` = `isNull` の間接参照解決) + `overlay_annotations.rs`(1685-1737: `merge_resources_shallow` = `mergeResources`) + `overlay_appearance_stream.rs`（段階的 conflict merge の再現） | 🔀 アクセサが各所に散在（`flpdf-mfir`） |
| `QPDF_Array/Dictionary/Stream/String/Name/Real/Integer/Bool/Null/InlineImage/Operator/Reserved/Unresolved/Destroyed.cc` | 1814 | `object.rs` の `Object` enum に統合 | 🔀 |
| `QPDFObject.cc` / `QPDFValue.cc` | 79 | `object.rs` の `Object` | ✅ |
| `QPDFObjGen.cc` | 68 | `object.rs` の `ObjectRef` | ✅ |
| `QPDFXRefEntry.cc` | 51 | `xref.rs`(1129) の一部 | 🔀 独立した型境界が無く `xref.rs` に埋没 |
| `PDFVersion.cc` | 68 | `pdf_version.rs` の `PdfVersion` | ✅ |
| `QPDFMatrix.cc` | 140 | `matrix.rs` の `Matrix` / `Rectangle` | ✅ |

## 2. パース / 読み取り

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF.cc` | 2667 | `reader.rs`(2454) + `reader/file_object.rs`(650) + `xref.rs`(1129) + `object_copy.rs`(184: `copyForeignObject`) + `cache.rs`(102: xref 由来の `ObjectCache` / `CacheEntry`。消費者は `reader.rs`) + `writer/object_streams.rs`(207-237: `compressible_objgens_qpdf_plan` = `getCompressibleObjGens`、`QPDF.cc:2392-2445`)  + `signatures.rs`(245-: `removeSecurityRestrictions`) + `page_closure.rs`(207: `page_object_closure`。`object_copy.rs` は pre-closed な集合しか受け取らず、両者で `copyForeignObject` 相当を構成する) + `ref_chain.rs`(77: `resolve_ref_chain` / `terminal_ref_of_chain` / `MAX_REF_CHAIN_DEPTH` — 深さ上限付き間接参照解決の共有プリミティブ。20 モジュールが使用) | 🔀 |
| `QPDFParser.cc` | 519 | `parser.rs` の `Parser`（Object / Content mode）。Content mode は EOF → `None`、word → `Object::Operator`、間接参照化の抑止を共有 object grammar 上で実装し、`content_stream.rs` が使用（`QPDFParser.cc:27-125,130-377`） | 🔀 content branch は対応済み。file-object parser 全体の API / recovery 差分は未精査 |
| `QPDFTokenizer.cc` | 965 | `tokenizer.rs`（18 token types、owned value/raw/error bytes/offset、push/pull、pull-only `allowEOF`、`includeIgnorable`、space/comment、bad-token recovery、max length、`betweenTokens`、unread、inline-image `EI` discovery。`QPDFTokenizer.hh:34-193`; `QPDFTokenizer.cc:45-965`）+ `parser.rs` の content mode + `content_stream.rs` の `ParserCallbacks` orchestration + `object.rs` の `Operator` / `InlineImage`（`QPDFParser.cc:27-125,130-377`; `QPDFObjectHandle.cc:1770-1847`） | ✅ `QPDFTokenizer` の責務境界を移植済み。object/parser/content callback consumers は共有 tokenizer を使用し、旧 content lexer は削除 |
| `InputSource` 系 5 ファイル | 625 | `Read + Seek` ジェネリクスで代替 | ⚪ |
| `QPDF_pages.cc` | 319 | `pages.rs`(741) + `page_tree_rebuild.rs`(390) + `linearization/inherited_attrs.rs`(575: `QPDF_pages.cc:39-138` の `getAllPagesInternal` 修復を移植。`linearization/plan.rs:773` と `linearization/writer.rs:2582` から呼ばれる) | 🔀 |
| `QPDFExc.cc` / `QPDFSystemError.cc` | 123 | `error.rs`(125) | ✅ |

## 3. 書き込み — 最大の smear

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFWriter.cc` | 3044 | `writer.rs`(4492) + `writer/serialize.rs`(1008) + `writer/object_streams.rs`(739) + `writer/plain/{plan,body,xref}.rs`(898) + `linearization/writer.rs`(3603) + `linearization/part1.rs`(370) + `linearization/back_patch.rs`(324) + `linearization/renumber.rs`(850) + `rewrite_renumber.rs`(893) = **13,177 行 / 11 ファイル**。加えて `object.rs`(412: `write_pdf` = `unparseObject` / 491: `write_pdf_qdf` / 585-: trailer `/ID` = `writeTrailer`。`writer.rs` と `linearization/writer.rs` が委譲) と `qpdf_null.rs`(38-57: `visible_entries` = `QPDFWriter.cc:1491` の null 値 dict キー抑制) | 🔀 |

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
| `QPDF_optimization.cc` | 381 | 独立モジュール無し — `linearization/plan.rs` に埋没 + `inherited_attrs.rs`(575) | 🔀 → Phase 2 |

`ObjUser` 分類（`ou_page` / `ou_thumb` / `ou_trailer_key` / `ou_root_key`）と
`updateObjectMaps`。定義は `plan.rs:2416-2950` に連続、呼び出しは `plan.rs:890-904`
に集中しており抽出境界は clean。

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
| `Pipeline.cc`（積層シンク基盤のみ。個々の `Pl_*` は下記の各行で個別に分類） | 114 | 抽象が無い。`Vec<u8>` バッファ + `out.len()` 直参照 | ❌ |
| `Pl_Count.cc` / `Pl_MD5.cc` | 114 | 無し（バッファから同等の値は取得可能） | ❌ |
| `Pl_Flate` / `SF_FlateLzwDecode` | 946 | `pipeline/flate.rs` + `stream_filter.rs` の `FlateLzwStreamFilter`（`/Predictor` `/Columns` `/Colors` `/BitsPerComponent` `/EarlyChange` の解釈、codec → predictor の chain 構築、`QIntC::to_uint` の range error timing） | ✅ |
| `Pl_LZWDecoder` | 189 | `pipeline/lzw.rs`（3-byte rotating buffer、1 入力 byte あたり 1 code、table 成長と code 幅遷移、eod latch、qpdf の 7 種の診断文言）+ `stream_filter.rs` 経由の production decode | ✅ |
| `Pl_PNGFilter` | 232 | `pipeline/png_filter.rs`（32-bit wrapping の row 幅算出、constructor の 3 種 rejection、未知 filter byte の無視、finish の zero-pad row、Up 固定 encoder）+ `filters.rs` / `writer/serialize.rs` の production consumer。⚪ row buffer の確保だけは constructor ではなく最初の write まで遅延（出力バイト・呼び出し境界・エラー timing に影響しない） | ✅ |
| `Pl_TIFFPredictor` | 175 | 無し。`/Predictor 2` は pipeline 構築時点で `Error::Unsupported` として拒否する（明示的逸脱） | ❌ |
| `Pl_ASCII85Decoder` / `SF_ASCII85Decode` | 108 + 31 | `pipeline/ascii85.rs` + `stream_filter.rs` | ✅ |
| `Pl_ASCIIHexDecoder` / `SF_ASCIIHexDecode` | 96 + 31 | `pipeline/ascii_hex.rs` + `stream_filter.rs` | ✅ |
| `Pl_RunLength` / `SF_RunLengthDecode` | 146 + 38 | `pipeline/run_length.rs` + `stream_filter.rs` | ✅ |
| `Pl_AES_PDF` | 200 | `security/standard.rs` の AES single-buffer helper と `writer.rs` の stream consumer に分散。Pipeline 統合は `flpdf-qynx.10` | 🔀 |
| `Pl_RC4` | 43 | `pipeline/rc4.rs`（65,536-byte既定buffer、stateful `security/rc4.rs`、write/finish lifecycle）+ `reader.rs` / `writer.rs` の本番stream consumer | ✅ |
| `Pl_QPDFTokenizer.cc` / `ContentNormalizer.cc` | 141 | `pipeline/qpdf_tokenizer.rs`（optional downstream を持つ token-filter runner、EOF-token → `handle_eof`、`ID` separator 注入、inline-image 切替、raw token/discard/output、`handle_eof` 成功後の永久 detach と finish/error timing）+ production consumer `content_normalizer.rs`（bad-token state、CR/string/name normalization） | ✅ |
| `QPDFStreamFilter.cc` | 19 | `stream_filter.rs`（`set_decode_params`、decode pipeline factory、specialized / lossy の既定分類） | ✅ |
| `Pl_DCT.cc` | 326 | 無し。`json_inspect.rs` の `DecodeLevel::All`(758) が DCT デコードを doc で約束しつつ encoded バイトへフォールバックしている | ❌ 消費者あり |
| `Pl_Base64` / `Pl_Concatenate` / `Pl_Discard` / `Pl_Function` / `Pl_OStream` / `Pl_StdioFile` / `Pl_String` / `Pl_SHA2` / `Pl_Buffer` | 570 | Rust の `Write` で代替 | ⚪ |

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
| `QPDFPageLabelDocumentHelper.cc` | 134 | `page_label_document_helper.rs`(934) | ✅ |
| `QPDFNameTreeObjectHelper` / `QPDFNumberTreeObjectHelper` / `NNTree.cc` | 1394 | `nntree.rs`（shared engine）+ `name_number_tree.rs`（compatibility wrapper）+ consumer adapters | ✅ |
| `QPDFEmbeddedFileDocumentHelper.cc` | 122 | `embedded_files.rs`(678) | ✅ |
| `QPDFFileSpecObjectHelper` / `QPDFEFStreamObjectHelper` | 280 | `filespec_helper.rs`(1324) | ✅ |
| `ResourceFinder.cc` | 56 | `resource_finder.rs`（operator/name tracking と resource type/offset 集約。flat `getNames()` oracle view は categorized map から test 内で導出）。production consumer は `resource_replacer.rs` と `resources.rs` の resource pruning | ✅ |
| `QPDFAcroFormDocumentHelper.cc` anonymous `ResourceReplacer` | — | `resource_replacer.rs`（`ResourceFinder` の name offsets を exact-byte 置換）。production consumer は `overlay_annotations.rs` の `/DA` と `overlay_appearance_stream.rs` の AP streams | ✅ |
| `QPDFDocumentHelper.cc` / `QPDFObjectHelper.cc` | 12 | 基底トレイトが無い | ⚪ |

## 8. JSON

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `JSON.cc` | 1401 | `json/` | ✅ |
| `JSONHandler.cc` | 189 | `json/` | ✅ |
| `QPDF_json.cc` | 946 | `json_inspect.rs`(2661) の一部 | 🔀 |

## 9. Job / CLI

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFJob.cc` | 3116 | `flpdf-cli/src/main.rs`(6491) + `overlay*.rs` + `page_merge.rs`(1117) + `check.rs`(360) + `attachment_list.rs`(306: `--list-attachments` の整形出力) + `acroform_field_prune.rs`(497: `QPDFJob.cc:2610-2632` の "Remove unreferenced form fields"。`prune_acroform_after_subset` が CLI から呼ばれる) + page 操作群 | 🔀 |
| `QPDFJob_config` / `_argv` / `_json` / `QPDFArgParser` / `QPDFUsage` | 3164 | clap で代替 | ⚪ |
| `QPDFLogger.cc` | 255 | `diagnostics.rs`(80) | 🔀 |

## 10. インフラ

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QUtil.cc` | 2003 | 各所に散在 | 🔀 |
| `QTC.cc` | 50 | 無し | ❌ |
| `BitStream.cc` / `BitWriter.cc` | 111 | `linearization/hint_stream.rs` に埋没 | 🔀 |
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
| ✅ mirrors | 3,026 | 責務境界も一致。触らない |
| 🔀 smeared | 28,493 | 再配置の主対象。qpdf 全体の 69% |
| ❌ missing | 604 | `Pipeline.cc`(114) / `Pl_Count`+`Pl_MD5`(114) / `Pl_DCT`(326) / `QTC`(50) |
| ⚪ 逸脱候補 | 7,099 | 要承認（下記の方針矛盾を参照） |
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
missing として傘で数えていたが、個々の `Pl_*` は下の各行で mirrors / smeared /
逸脱候補として個別に分類されており**二重計上**だった。傘の行を `Pipeline.cc`
本体（114 行）に限定し、真に未マップな qpdf 行だけを ❌ に数えるよう改めた。
