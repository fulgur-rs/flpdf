# flpdf ↔ qpdf 責務対応表

**Oracle:** qpdf 11.9.0 (`v11.9.0`) — `libqpdf/`
**調査日:** 2026-07-25
**関連:** `flpdf-qxba`（部品積み上げによる責務分割）/
[設計書](superpowers/specs/2026-07-25-qpdf-component-bottom-up-refactor-design.md)

pre-v1.0 の byte-identical 模倣方針（`CLAUDE.md`）に対し、flpdf の責務分割が qpdf と
どこまで対応しているかのスナップショット。`flpdf-qxba` の work-list であり、Phase 1
完了後に再測する。

規模比較: qpdf `libqpdf/*.cc` = 41,459 行 / flpdf 実装部（`#[cfg(test)]` 以降を除く）= 61,792 行

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
| `QPDFObjectHandle.cc` | 2601 | `object.rs`(929) | 🔀 アクセサが各所に散在（`flpdf-mfir`） |
| `QPDF_Array/Dictionary/Stream/String/Name/Real/Integer/Bool/Null/InlineImage/Operator/Reserved/Unresolved/Destroyed.cc` | 1814 | `object.rs` の `Object` enum に統合 | 🔀 |
| `QPDFObject.cc` / `QPDFValue.cc` | 79 | `object.rs` / `cache.rs`(102) | ✅ |
| `QPDFObjGen.cc` | 68 | `object.rs` の `ObjectRef` | ✅ |
| `QPDFXRefEntry.cc` | 51 | `xref.rs`(1129) の一部 | ✅ |
| `PDFVersion.cc` | 68 | `writer.rs` の `parse_pdf_version` / `static_version_string` | 🔀 → **T0-1** |
| `QPDFMatrix.cc` | 140 | `overlay.rs` / `page_form_xobject.rs` / `page_annotation_flatten.rs` に `[f64; 6]` 生配列で散在 | 🔀 → **T0-2** |

## 2. パース / 読み取り

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF.cc` | 2667 | `reader.rs`(2454) + `reader/file_object.rs`(650) + `xref.rs`(1129) + `object_copy.rs`(184: `copyForeignObject`) | 🔀 |
| `QPDFParser.cc` | 519 | `parser.rs`(597) の `Parser<'a>`(101) | 🔀 型は存在し `content_stream.rs` も再利用している。qpdf API との差分は未精査 |
| `QPDFTokenizer.cc` | 965 | `tokenizer.rs`（normal mode / PR #549）+ `content_stream.rs`(484) に二重実装 | 🔀 → **T1-1**（`flpdf-n9t0.1`） |
| `InputSource` 系 5 ファイル | 625 | `Read + Seek` ジェネリクスで代替 | ⚪ |
| `QPDF_pages.cc` | 319 | `pages.rs`(741) + `page_tree_rebuild.rs`(390) | 🔀 |
| `QPDFExc.cc` / `QPDFSystemError.cc` | 123 | `error.rs`(125) | ✅ |

## 3. 書き込み — 最大の smear

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFWriter.cc` | 3044 | `writer.rs`(4492) + `writer/serialize.rs`(1008) + `writer/object_streams.rs`(739) + `writer/plain/{plan,body,xref}.rs`(898) + `linearization/writer.rs`(447) + `linearization/part1.rs`(370) + `linearization/back_patch.rs`(324) + `linearization/renumber.rs`(850) + `rewrite_renumber.rs`(448) = **9,576 行 / 10 ファイル** | 🔀 |

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
| `QPDF_encryption.cc` | 1410 | `security/standard.rs`(1879) + `writer.rs` の encryption context(~700) + `encrypt_setup.rs`(213) + `permissions.rs`(206) | 🔀 |
| `rijndael.cc` / `AES_PDF_native` / `RC4_native` / `MD5_native` / `SHA2_native` | 1716 | `security/primitives.rs`(188)（外部 crate） | ⚪ |
| `QPDFCryptoProvider.cc` / `QPDFCrypto_*` | 774 | provider 抽象が無い | ⚪ |
| ランダム源 3 ファイル | 185 | `writer.rs` の `fresh_id_bytes` 等に散在 | 🔀 |

## 6. Pipeline / フィルタ

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `Pipeline.cc` + `Pl_*.cc`（21 ファイル） | ~2,400 | 積層シンク抽象が無い。`Vec<u8>` バッファ + `out.len()` 直参照 | ❌ |
| `Pl_Count.cc` / `Pl_MD5.cc` | 114 | 無し（バッファから同等の値は取得可能） | ❌ |
| `Pl_Flate` / `Pl_LZWDecoder` / `Pl_PNGFilter` / `Pl_TIFFPredictor` / `SF_FlateLzwDecode` | 946 | `filters.rs`(859) | 🔀 |
| `Pl_ASCII85Decoder` | 108 | `ascii85.rs`(163) | ✅ |
| `Pl_ASCIIHexDecoder` | 96 | `ascii_hex.rs`(85) | ✅ |
| `Pl_RunLength` | 146 | `run_length.rs`(140) | ✅ |
| `Pl_AES_PDF` / `Pl_RC4` | 243 | `writer.rs` の `encrypt_stream_payload_for_writer` に埋没 | 🔀 |
| `Pl_QPDFTokenizer.cc` / `ContentNormalizer.cc` | 141 | 無し | ❌ → **T2-1** |
| `QPDFStreamFilter.cc` | 19 | filter 登録機構が無い | ❌ |
| `Pl_DCT.cc` | 326 | 無し | ❌ |
| `Pl_Base64` / `Pl_Concatenate` / `Pl_Discard` / `Pl_Function` / `Pl_OStream` / `Pl_StdioFile` / `Pl_String` / `Pl_SHA2` / `Pl_Buffer` | 632 | Rust の `Write` で代替 | ⚪ |

`/ID` が qpdf と非 parity だった原因は **アルゴリズム**（qpdf は 2 段階 MD5 で seed を
作る）であり、Pipeline 抽象の有無ではない。flpdf は全体をバッファするので任意の
バイト範囲をダイジェストできる。`--deterministic-id` の byte-parity は
`deterministic_id_qpdf_parity_tests` で既にゲート済み。

## 7. ドキュメント / オブジェクトヘルパー

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFAcroFormDocumentHelper.cc` | 1047 | `acroform_document_helper.rs`(1096) + `overlay_annotations.rs`(474: `transformAnnotations` / `addAndRenameFormFields`) + `overlay_appearance_stream.rs`(720: `adjustAppearanceStream`) | 🔀 |
| `QPDFPageObjectHelper.cc` | 1039 | `page_object_helper.rs`(766) + `page_form_xobject.rs`(637) + `resources.rs`(1229) + `page_annotation_flatten.rs`(596) + `overlay.rs`(2228: `placeFormXObject`) + `overlay_annotations.rs`(474: `copyAnnotations`) | 🔀 |
| `QPDFFormFieldObjectHelper.cc` | 852 | `annotation_helper.rs`(748) + `appearance.rs`(2022) + `default_appearance.rs`(167) | 🔀 |
| `QPDFPageDocumentHelper.cc` | 158 | `page_document_helper.rs`(236) | ✅ |
| `QPDFAnnotationObjectHelper.cc` | 226 | `annotation_helper.rs` + `page_annotation_enum.rs`(249) | 🔀 |
| `QPDFOutlineDocumentHelper` / `QPDFOutlineObjectHelper` | 198 | `outline_document_helper.rs`(1408) + `outline.rs`(145) | ✅ |
| `QPDFPageLabelDocumentHelper.cc` | 134 | `page_label_document_helper.rs`(934) | ✅ |
| `QPDFNameTreeObjectHelper` / `QPDFNumberTreeObjectHelper` / `NNTree.cc` | 1394 | `name_number_tree.rs`(364) + `name_tree_dests.rs`(286) | 🔀 → **T2-2** |
| `QPDFEmbeddedFileDocumentHelper.cc` | 122 | `embedded_files.rs`(188) + `attachment_list.rs`(306) | ✅ |
| `QPDFFileSpecObjectHelper` / `QPDFEFStreamObjectHelper` | 280 | `filespec_helper.rs`(1324) | ✅ |
| `ResourceFinder.cc` | 56 | `overlay_annotations.rs`(967-1111: オペレータ表) + `overlay_appearance_stream.rs`(2-3: `ResourceReplacer` / `ResourceFinder` token filter) | 🔀 `resources.rs` には実装が無い |
| `QPDFDocumentHelper.cc` / `QPDFObjectHelper.cc` | 12 | 基底トレイトが無い | ⚪ |

## 8. JSON

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `JSON.cc` | 1401 | `json.rs`(159) | ❌ emitter のみ → **T0-3** |
| `JSONHandler.cc` | 189 | 無し | ❌ → **T0-3** |
| `QPDF_json.cc` | 946 | `json_inspect.rs`(2661) の一部 | 🔀 |

## 9. Job / CLI

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFJob.cc` | 3116 | `flpdf-cli/src/main.rs`(6491) + `overlay*.rs`(3422) + `page_merge.rs`(1117) + `check.rs`(360) + page 操作群 | 🔀 |
| `QPDFJob_config` / `_argv` / `_json` / `QPDFArgParser` / `QPDFUsage` | 3164 | clap で代替 | ⚪ |
| `QPDFLogger.cc` | 255 | `diagnostics.rs`(80) | 🔀 |

## 10. インフラ

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QUtil.cc` | 2003 | 各所に散在 | 🔀 |
| `QTC.cc` | 50 | 無し | ❌ |
| `BitStream.cc` / `BitWriter.cc` | 111 | `linearization/hint_stream.rs` に埋没 | 🔀 |
| `Buffer.cc` / `MD5.cc` / `RC4.cc` | 301 | `Vec<u8>` / 外部 crate | ⚪ |
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
| `signatures.rs` | 1,338 | 電子署名検査。qpdf に相当機能なし |
| `qdf_fix.rs` | 764 | qpdf では `qpdf/fix-qdf.cc`（libqpdf 外の別バイナリ） |
| `page_closure.rs` / `ref_chain.rs` / `qpdf_null.rs` | 341 | |

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
| QDF | 🟡 **部分的にあり**（下記） | 🟡 `overlay::byte_gate` の QDF 3 件 |
| 暗号化出力 | ❌ gated byte gate 無し | ❌ |
| incremental update | ❌ gated byte gate 無し | ❌ |

### QDF の既存カバレッジ（部分的）

「QDF に byte gate 無し」は誤り。次の 3 系統が既に存在する。

| テスト | 内容 | CI |
|---|---|---|
| `writer_tests.rs:2170,2201` | `tests/golden/references/qdf-contents-ref-array/qdf-static-id.pdf` と `qdf-ignore-newline/qdf-static-id.pdf` に対する完全一致比較 | ✅ 列挙済み |
| `qdf_tests.rs:1300` | `qdf_golden_minimal_is_byte_identical_to_qpdf_modulo_id` — `tests/fixtures/qdf-golden/minimal.qdf` に対し trailer `/ID` 行を除いて完全一致 | 既定テストに含まれる |
| `overlay::byte_gate` | `three-page-overlay-one-page-qdf.pdf` ほか QDF 出力 3 件の byte-identical | ✅ `--lib overlay::byte_gate` で列挙済み |

**残る穴**: QDF × ObjStm、QDF × 暗号、QDF × linearize の組み合わせ。Phase 2 で
null 可視性を QDF 経路に広げる際に必要になるのはこれらであって、QDF 全体ではない。

gated テストは `.github/workflows/ci.yml` の bytes-identical ジョブに手で列挙しないと
CI で走らない。11 件中 `cmp_null_visibility_tests` のみが漏れている。

---

## 逸脱候補（⚪）— 要承認

`CLAUDE.md` は DEFLATE バックエンドを「唯一の例外」とし「逸脱は必ず明示」を求めている。
⚪ に分類した約 6,900 行は提案であり決定ではない。

| 逸脱候補 | qpdf 行数 | byte 影響 |
|---|---|---|
| `InputSource` 階層 → `Read + Seek` ジェネリクス | 625 | 無し（入力側のみ） |
| `QPDFArgParser` / `QPDFJob_*` → clap | 3,164 | 無し（CLI 挙動 parity は別途必要） |
| crypto provider 抽象 → 外部 crate 直接利用 | 2,490 | 無し（アルゴリズムは同一） |
| `Buffer` / `Pl_Buffer` / 汎用 `Pl_*` → `Vec<u8>` / `Write` | 933 | 無し |
| `QPDFDocumentHelper` / `QPDFObjectHelper` 基底 → トレイト無し | 12 | 無し |

現時点の証拠ではいずれも出力バイトに影響しない。承認されたら各モジュール doc に
逸脱理由を 1 行残す。

---

## 集計

| 状態 | qpdf 側の該当行数 |
|---|---|
| ✅ mirrors | 約 4,000 |
| 🔀 smeared | 約 22,000 |
| ❌ missing | 約 4,300 |
| ⚪ 逸脱候補 | 約 6,900 |
| ➖ 対象外 | 約 2,200 |
