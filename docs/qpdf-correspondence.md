# flpdf ↔ qpdf 責務対応表

**Oracle:** qpdf 11.9.0 (`v11.9.0`) — `libqpdf/`
（`scripts/fetch-qpdf-source.sh` で取得。パスは `--print-path` で解決する。
本表のファイル名・行数はすべてこのツリーに対するもの。将来 v12 に追従する際は
`git log v11.9.0..v12.0.0 -- libqpdf/` が移植差分になる）
**調査日:** 2026-07-29（初版）/ 2026-08-02（Phase 2 着手時の再測 — `flpdf-1e5g`）/
2026-08-16（`flpdf-egzr`/`flpdf-3yn9` 大量クローズ後の再測）
**関連:** `flpdf-qxba`（部品積み上げによる責務分割）/
[設計書](superpowers/specs/2026-07-25-qpdf-component-bottom-up-refactor-design.md) /
[route matrix](qpdf-route-matrix/README.md)（`flpdf-3yn9.41`。本表の「責務対応」の上に
「その責務へ至る経路が 1 本か」の軸を足した canonical / bridge / mixed / unknown の棚卸しと
cutover 計画。本表の行を置き換えず、引用は `scripts/check-qpdf-route-matrix.py --check` で検証する）

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
各 source module の先頭 doc ブロック内にある対応行から生成する。この索引は注釈の欠落と
drift を検査するためのものであり、本書の責務分類・状態・実装判断を置き換えない。

Rustdoc の Modules 一覧では doc ブロックの最初の段落が summary になるため、module の
目的説明を対応行より前に置くこと。`qpdf correspondence:` / `Mirrors qpdf` の対応行を
先頭段落にすると、対応分類だけが summary として表示される。この順序は
`scripts/tests/test_qpdf_module_docs.py` でも検査する。

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

### Shared xref trailer and previous-section route (2026-09-08)

qpdf's `QPDF::read_xref` inserts the initial `xref_offset` into a local
`visited` set before reading either a classic table or an xref stream, then
checks the returned `/Prev` offset before the next section is read
(`libqpdf/QPDF.cc:626-719`). `read_xrefTable` and `reconstruct_xref` both call
the same `readTrailer` (`QPDF.cc:894` and `QPDF.cc:565`), so parser warnings,
empty-object handling, and the post-dictionary `stream` lookahead have one
owner (`QPDF.cc:1312-1328`).

`flpdf-3yn9.48.18` routes both classic owner variants and reconstruction
candidate discovery through `crates/flpdf/src/xref.rs::read_trailer`. It uses
the existing handle-producing parser so indirect trailer children retain the
caller-provided document identity, converts parser diagnostics to
`QpdfExc { object: "trailer" }`, appends qpdf's empty-object warning, and uses
`Tokenizer::read_token(true, 0)` only for the dictionary-followed-by-`stream`
lookahead. The classic fixed-width xref reader remains on its own
`ByteCursor`/`readLine`-equivalent route. The `/Prev` merge helper seeds its
local visitor with the already-read nonzero `LoadedXref.startxref`, so a
self-referential initial `/Prev` is rejected before the section is parsed a
second time.

Live qpdf 11.9.0 probes and the xref regression tests record the observable
contract: a dictionary followed by `stream` reports
`(trailer, offset 134): stream keyword found in trailer`; an empty trailer
candidate reports `(trailer, offset 53): empty object treated as null`; and a
self-`/Prev` section retains one three-warning recovery sequence. The warning
detail, object attribution, offset, and order match the Rust route.

### Canonical trailer nested value descriptions (`flpdf-7yb9k`, 2026-09-17)

qpdf constructs `QPDFParser` for `readTrailer` with the literal object
description `trailer` (`libqpdf/QPDF.cc:1312-1317`). The parser shares one
`input->getName() + ", " + object_description + " at offset $PO"` template
with every non-null scalar and container it creates
(`libqpdf/qpdf/QPDFParser.hh:14-27`; `libqpdf/QPDFParser.cc:219-277,304-365,394-444`).
Each value keeps its own parsed offset through the set-once description path
(`libqpdf/qpdf/QPDFValue.hh:60-105`; `libqpdf/QPDFValue.cc:14-32`), and
`typeWarning`/`objectWarning` render that value description
(`libqpdf/QPDFObjectHandle.cc:2168-2212`).

The canonical flpdf `CanonicalTrailerParser` now supplies the same shared
`filename, trailer at offset $PO` template through `HandleResolver` while
`read_trailer` parses the dictionary. The existing top-level setter remains for callers that stage a description
before parsing; nested canonical values receive their own token offsets
without a post-parse recursive tagging pass. A synthetic
classic-xref fixture with `/ID 7` at offset 393 compares qpdf 11.9.0 and flpdf
stderr, exit status, and output bytes in
`crates/flpdf-cli/tests/cmp_trailer_value_description_tests.rs`.

### Input-source lifecycle (2026-09-03)

qpdf constructs a `QPDF` with an `InvalidInputSource`, leaves its trailer
uninitialized, and replaces the active source with a fresh invalid source in
`closeInputSource` (`libqpdf/QPDF.cc:55-106,198-213,271-281`). The replacement
is pointer-level: `ForeignStreamData` that already captured the old source
continues to own it (`QPDF.cc:2265-2273`). flpdf now exposes the same lifecycle
through `Pdf::uninitialized()` and `Pdf::close_input_source()`. The canonical
resolver catches the closed-source `std::logic_error` at qpdf's `resolve`
boundary and retains the invalid source name when `root_handle()` raises the
final missing-`/Root` exception. `invalid-objects 1` / test 73 verifies the
uninitialized error, warning, final error, and exit status against qpdf 11.9.0.

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
bounded-read/retry する設計だった。`flpdf-3yn9.48.44` で `test_0_1` の
DecodeParms warning attribution を値自身の `try_get_parsed_offset` へ移したため、
これら 3 つの source-reread API（`qtest_decode_parms_source_offset` を含む）は
production caller 0 になった（残る参照は `tests/qpdf_route_hygiene_tests.rs` の
存在チェックのみ）。2026-09-08（`.25`）: その 3 本と `Pdf::source_stream_data_offset`、
および qpdf に存在しない `resolution_fallbacks_remaining` の fallback budget を削除した。
残る参照は `crates/flpdf/tests/qpdf_route_hygiene_tests.rs` がこれらの不在を検査する
hygiene テストだけで、qtest-only の offset boundary 自体が無くなった。

`flpdf-hpq1` では、値自身の `try_get_parsed_offset` をwarningのsource prefixとして
再構成する経路も撤去した。qpdfの`typeWarning`はrendered descriptionをQPDFExcの
object fieldへ渡し、exception offsetは0のまま保持する
（`libqpdf/QPDFObjectHandle.cc:2168-2188`; `libqpdf/QPDFValue.cc:14-61`）。
したがってObjStm memberの`object stream N`とdecoded-member offsetはcanonical
`ObjectHandle::description`から一度だけ取得し、`qtest-tools`の2回のfilter probeへ
同じ説明を渡す。`stream_decode_parms_objstm` fixtureと`driver_goldens`がこの
warning prefix/orderをpinned qpdf 11.9.0とbyte-identicalに固定する。

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

2026-09-10（`flpdf-oq7g`）: Preserve linearization の pre-/O emission は、plain
open-document object と source ObjStm container を assigned object number 順に
一つの列として出力する。qpdf の `enqueuePart(part4)` が ObjStm member を初めて
見た位置で container を出力する責務（`QPDFWriter.cc:2543-2561,2606-2624,2636-2651`）を、
flpdf の `linearization/writer.rs` に反映した。`objstm-lin-openaction-preserve-bearing.pdf`
（`/OpenAction` action dict を source ObjStm に保持し、JS stream は plain）を
`cmp_linearize_objstm_tests.rs::openaction_preserve_objstm_byte_identical_to_qpdf` で
qpdf 11.9.0 と full-byte 比較し、container-before-plain の part4 順序を固定する。

## 1. オブジェクトモデル

`QPDF_Stream` の stream-local fields (`libqpdf/qpdf/QPDF_Stream.hh:101-107`) は
共通の `QPDFValue` subclass payload ではないため、flpdf の `StreamValue` として
`ObjectValue::Stream(Box<StreamValue>)` に分離する。これにより scalar と
container の共通 enum layout は stream subclass の 56-byte shape を inline
で保持せず、stream value だけがその payload allocation を負担する。

`flpdf-rghuz` では、`QPDFAcroFormDocumentHelper::traverseField` の分析 cacheを
qpdfの raw identity boundaryへ寄せた。qpdf は `isIndirect()` と `QPDFObjGen::set`
で generationを射影せず field associationを保持する
（`QPDFAcroFormDocumentHelper.cc:235-286`）。flpdfも field-treeの visited setと
`get_form_fields`の内部順序を`QpdfObjGen`へ寄せ、qualified-name更新は live handle
climbを使う。direct objectだけが従来の警告対象で、generation 65535の indirect
fieldはcacheから落とさない。公開`ObjectRef` mapは既存 projection boundaryとして残す。

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFObjectHandle::makeResourcesIndirect` | `include/qpdf/QPDFObjectHandle.hh:789-793`; `libqpdf/QPDFObjectHandle.cc:1042-1060` | `object_handle.rs::make_resources_indirect` + `acroform_document_helper.rs::prepare_foreign_resource_plan` | ✅ direct second-level resource values are promoted in place through the canonical resolver before `mergeResources`; category dictionaries are not promoted and the walk is non-recursive. Tests cover direct/indirect categories, already-indirect values, non-dictionary top-level entries, alias identity, and the foreign AcroForm caller |
| `QPDFObjectHandle::makeDirect` | `libqpdf/QPDFObjectHandle.cc:2091-2133,2154-2157` | `object_handle.rs::make_direct`; `reader.rs::make_indirect_from_object_handle`; `qtest-tools::driver::run_test_4` | ✅ the receiver is rebound to a recursively direct copy, each repeated indirect occurrence is copied independently, a per-call identity set reports qpdf's loop text, `allow_streams=true` retains streams, and qpdf's shared-allocation promotion is used for the `/Info` copy. Focused mutation tests and all five `mutability.test` cases cover the success and failure paths |
| `QPDFObjectHandle.cc` | 2601 | `object.rs`(1301) + `object_handle.rs`(shared handle identity・parsed offset・遅延解決・forward direct containment・`QPDF::newReserved`/`QPDF_Reserved`・`copyStream`/`StreamDataProvider` source dispatch) + `object_handle.rs::try_is_null` / `reader.rs::resolve`（`isNull` の canonical 間接参照解決） + `writer/rewrite_renumber.rs::visible_raw_dict_entries`（raw `Object` 境界の writer dict 可視性） + `acroform_document_helper.rs`（`DrMap` = qpdf `dr_map`） + `overlay_appearance_stream.rs`（段階的 conflict merge の再現） | 🔀 アクセサは `object.rs` / `object_handle.rs` に分割されたまま（旧 raw `Object` route の削除・統合は `flpdf-egzr.3.2.8` が担当、open。`flpdf-mfir` はその削除対象へのリファクタなので着手せず close 済み）。object identity / 遅延解決は `object_handle.rs` の production route への移行が完了済み（`flpdf-egzr.3.1`、2026-08-09 close）。qpdf の array/dictionary/stream が保持する現在の forward child を正本とし、flpdf も production では同じ forward graph だけを保持する。unit-test の containment-root assertion だけは test-only registry の一度きりの走査から導出し、削除・置換後の旧 child は旧 root を返さない。`try_get_keys` は `QPDFObjectHandle::getKeys` → `QPDF_Dictionary::getKeys`（`QPDFObjectHandle.cc:997-1009`; `QPDF_Dictionary.cc:117-127`）に対応し、holder と全 child を lazy resolve して null value のキーを除外した `BTreeSet` を返す。child resolve 前に辞書 snapshot の borrow は終了し、resolver error は伝播する。`stream_filter.rs` の consuming stage は retained-key reduction 前に `try_get_keys` を使用する。`shallow_copy` は `QPDFObjectHandle::shallowCopy`（`QPDFObjectHandle.cc:2072-2079`）に対応し、stream は `QPDF_Stream::copy`（`QPDF_Stream.cc:140-145`）が `shallow` 引数を無視して無条件に `std::runtime_error` を投げるのに合わせて `Error::System` で拒否する。`QPDF_Dictionary::copy`/`QPDF_Array::copy` が direct な子に `shallowCopy` を掛けるため、コンテナに入れ子の direct stream も同じ拒否に到達する。qpdf の `QPDFObjectHandle::copyStream`（`QPDFObjectHandle.cc:2136-2151`）と `QPDF::copyStreamData`（`QPDF.cc:2216-2272`）を `ObjectHandle::copy_stream` と resolver-owned stream-copy boundary として実装済み（`flpdf-a8mk`）。Buffer は `Rc<Vec<u8>>` 共有、provider-backed source は source handle を保持する retry-aware provider、original-file source は qpdf の `ForeignStreamData` 相当として source の `StreamInput`/encryption state/object number/parsed offset/length と destination dictionary を copy 時に凍結し、destination resolver を warning sink として遅延 dispatch する。source `Pdf` 解放後も入力と暗号状態だけで読み続け、source 側へ警告を戻さない。`set_immediate_copy_from` は qpdf の source-side `setImmediateCopyFrom` に対応する。`QPDFObjectHandle::isReserved`/`QPDF::newReserved` は `ObjectValue::Reserved` と `Pdf::new_reserved` に対応し、`ot_reserved` は null/missing/destroyed と区別して、materialize と全 ObjectHandle writer entrypoint で `QPDFObjectHandle: attempting to unparse a reserved object` を返す。 |
| `QPDFObjectHandle::StreamDataProvider` / `QPDF_Stream` | `QPDFObjectHandle.hh:68-127`; `QPDFObjectHandle.cc:48-90,1365-1428`; `QPDF_Stream.cc:571-620,640-660` | `object_handle.rs` の `StreamDataProvider`、`ObjectValue::Stream.stream_provider`、`replace_stream_data_provider`、callback adapter、`pipe_stream_source` | ✅ qpdf の provider ownership、通常/retry family の選択、identity forwarding、遅延・反復 invocation、`Pl_Count` による encoded-byte length 検証、buffer/provider の排他を canonical route で保持する。qpdf の `std::shared_ptr` container は `Rc<dyn StreamDataProvider>` に置換するが、これは内部所有表現だけの差であり、callback/error/finish/`/Length` の観測契約は変えない。登録 API は stable `ObjectRef` を必要とするため indirect stream に限定し、direct stream は登録時に `Error::System` で拒否する。既存 document-owned stream の provider/dictionary 置換は共有 canonical handle graph を直接更新し、writer は同じ live value を観測する（明示通知は不要） | ✅ |
| `QPDFObjectHandle::getDict` | `include/qpdf/QPDFObjectHandle.hh:968-970`; `libqpdf/QPDFObjectHandle.cc:313-324,1257-1262,2215-2222` | `object_handle.rs::ObjectHandle::try_get_stream_dict`（`pub`） | ✅ qpdf の public stream-dictionary boundary を `try_dereference` → stream type assertion → live nested dictionary handle の順で再現する。non-stream と uninitialized は `Error::System` の `operation for stream attempted on object of type ...`、resolver failure は既存 error として返す。既存 `as_stream_dict`（`pub`）は非 resolving・`Option` の silent observation であり、`getDict` の対応物ではない。`flpdf-3yn9.48.106` でtest 42/98のconsumer caller-side `Pdf::resolve`を撤去し、canonical `try_*`/`try_get_stream_dict`へcutoverした。 |
| `QPDFObjectHandle::setFilterOnWrite` / `getFilterOnWrite` | `include/qpdf/QPDFObjectHandle.hh:972-982`; `libqpdf/QPDFObjectHandle.cc:1265-1273`; `libqpdf/QPDF_Stream.cc:114-118,154-164` | `object_handle.rs::ObjectHandle::set_filter_on_write` / `get_filter_on_write`; `ObjectValue::Stream.filter_on_write`; `writer/plain/body.rs::canonical_stream_filter_plan` | ✅ qpdf の stream-local default `true` と shared handle state を保持し、`false` は `QPDFWriter::willFilterStream` の metadata/normalize/compress/retry 分岐より先に全 filtering を抑止する。raw payload と既存 filter metadata の出力は既存の unfiltered writer route に委譲し、cache fingerprint は state mutation で無効化する。`filter-on-write` qtest の test_70 と Rust API/writer regression tests がこの契約を検証する |
| `QPDF_Stream::registerStreamFilter` / `QPDF_Stream::filterable` | `QPDF.hh:186-194`; `QPDF.cc:295-300`; `QPDF_Stream.cc:33-50,72-94,147-152,378-485` | `stream_filter.rs` の `register_stream_filter` / `stream_filter_for` と `object_handle.rs` の `prepare_stream_filter_plan` | ✅ qpdfのprocess-global `filter_factories` map、同名登録の置換、alias展開後の全factory lookup、未知名を残したままの全factory構築、`/DecodeParms` のfull handle配送、decode-only拡張を1つのRust registryへ統合した。`stream_filter.rs` の `StreamFilter`、`PipelineRef`、`OwnedDecodePipeline` はそれぞれ `QPDFStreamFilter` と `Pipeline*` の公開境界に対応する。Rustの `OnceLock`/`Mutex` は安全なmap共有の実装詳細で、qpdfのlock/poison semanticsは主張しない。旧 `FilterSpec`/縮約`DecodeParams`/`ParamValue`/retention snapshot層とwhole-buffer recovering routeは `.48.96` までに撤去済み。 | ✅ |
| `QPDFObjectHandle::isArray` / `isDictionary` / `isNameAndEquals` / `isString` / `isDictionaryOfType` / `isStreamOfType` / `getArrayNItems` / `getArrayItem` / `isOrHasName`（行数は上段に計上済み） | `QPDFObjectHandle.hh:330-374` | `object_handle.rs` の `try_is_array` / `try_is_dictionary` / `try_is_name_and_equals` / `try_is_string` / `try_is_dictionary_of_type` / `try_is_stream_of_type` / `try_array_len` / `try_array_item` / `try_is_or_has_name`（`QPDFObjectHandle.cc:240-267,327-330,402-471,759-785,1027-1039`） | ✅ holder を qpdf 順に lazy resolveし、`try_is_array` / `try_is_dictionary` / `try_is_string` は value kind だけを検査して child collectionやname/string payloadをsnapshotしない。`isStreamOfType` は qpdf の `isStream() && getDict().isDictionaryOfType(...)` をそのまま stream 内辞書へ委譲し、container borrow は resolver 再入前に解放する。配列全体をsnapshotせず、`try_array_len` は長さだけを読み、`try_array_item` はvalid-index面のみを契約に含める。owned collectionが必要な `try_as_array` / `try_as_dictionary` とpayloadが必要な `as_*` snapshot accessorは別責務として残す |
| `QPDFObjectHandle::typeWarning` / `warnIfPossible` / `objectWarning` / `warn` / `getIntValue` / `getIntValueAsInt`（行数は上段に計上済み） | — | `object_handle.rs` の `type_warning` / `warn_if_possible` / `object_warning` / `warn_through_context` / `context` と `DocumentResolver::warn`、`try_get_int_value` / `try_get_int_value_as_int`、`reader/resolver.rs` の `push_object_warning`（`QPDFObjectHandle.cc:502-543,2168-2212,2385-2396`; `QPDF.cc:487-494`） | 🔀 メッセージ文言は qpdf と完全一致。live parser が生成した direct value と canonical indirect handle は `HandleResolver::direct_handle` / `ChildHandles` から同じ weak document context と、qpdf の `QPDFParser` と同じ parse-call description template を持つ。非 null の top-level・array・dictionary・scalar は `input-description, object N G at offset $PO` を共有し、`QPDFValue` と同じ container offset shift を経て `DocumentResolver::warn` → `push_object_warning` で `Pdf::repair_diagnostics` と同じ収集先へ同順に届く。parsed null は qpdf と同じく description を持たない。literal null は containment parent の context を借りず、qpdf の `QPDF_Null::create` に対応する contextless 分岐をネスト後も維持する一方、missing-key null は `setChildDescription` に対応する Child description 経由で親の context を保持する（`QPDF_Null.cc:12-15`; `QPDFParser.cc:397-410`; `QPDFObject_private.hh:79-91`）。明示的 parse と programmatic direct は qpdf の contextless 分岐を維持する。no-context 分岐は qpdf のまま 2 通り — `typeWarning`/`objectWarning` は `throw QPDFExc`（`std::runtime_error` 派生、`QPDFExc.hh:29`）に対応する `Error::System`、`warnIfPossible` は `QPDFLogger::defaultLogger()->getError()` へ素の文言を書いて正常復帰する。`getKey`/`getKeys` の `typeWarning` は `try_get_key`/`try_get_keys` に実装済み。live parser の direct value は weak document context を持ち、stream_filter の consuming `/DecodeParms` 読み出しで qpdf と同じ回復可能な警告を `DocumentResolver::warn` へ送る。contextless の programmatic direct は qpdf と同じく `Error::System` 相当の throw を維持する。`asDictionary`/`asInteger` に対応する `try_as_dictionary`/`try_as_integer` は qpdf 同様 warning を出さない |
| `QPDFObjectHandle` type-check accessors, array/dictionary bounds, geometry, and iterators | `QPDFObjectHandle.hh:239-267,597-637,666-734`; `QPDFObjectHandle.cc:332-453,474-740,759-853,856-1023`; `qpdf/test_driver.cc:1407-1549` | `ObjectHandle::try_get_bool_value` / `try_get_int_value` / `try_get_real_value` / `try_get_numeric_value` / `try_get_name` / `try_get_string_value` / `try_get_utf8_value` / `try_get_operator_value` / `try_get_inline_image_value`; `try_get_array_n_items` / `try_get_array_item` / `try_get_array_as_vector`; signed-index array mutators; `try_get_key_if_dict` / `try_get_dict_as_map`; `try_is_rectangle` / `try_get_array_as_rectangle` / `try_is_matrix` / `try_get_array_as_matrix`; `ArrayItems` / `DictItems` | ✅ warning-producing accessors dereference before type inspection and return qpdf's zero-like fallbacks; invalid array positions use object warnings and contextual nulls; geometry checks use silent number predicates and qpdf's zero defaults; cursors retain canonical child handles and use explicit uninitialized end values. `flpdf-qtest-tools::driver::run_test_42` drains the same `Pdf::repair_diagnostics` sequence, and the six `type-checks.test` cases pass against the pinned qpdf expected output |
| `QPDFObjectHandle::newFromMatrix(QPDFMatrix)` overload | `include/qpdf/QPDFObjectHandle.hh:254-285`; `libqpdf/QPDFObjectHandle.cc:1987-2002` | `ObjectHandle::new_from_qpdf_matrix` using [`crate::Matrix`] | ✅ the standalone identity-default matrix type is kept separate from the nested all-zero `ObjectHandleMatrix`, while both constructor overloads produce the same six-number array shape |
| `QPDFObjectHandle::getTypeCode` / `getTypeName` | `include/qpdf/QPDFObjectHandle.hh:311-316`; `libqpdf/QPDFObjectHandle.cc:240-250`; `include/qpdf/Constants.h:108-128` | `object_handle.rs::ObjectValue::type_code` / `type_name` と `ObjectHandle::type_code` / `type_name`（`object_handle.rs:1025-1068,5708-5758`） | ✅ qpdfの `qpdf_object_type_e` ordinalをRust enumのdiscriminantから独立した明示的matchで保持する。handle側は `try_dereference` 後にvalue-layer code/nameを読む。`ObjectValue::Reference` はflpdfの既存 `set_object` redirect専用状態で、qpdfのvalue familyには対応物がないため後続のraw-route削除で消す |
| `QPDFObjectHandle::unparse` / `unparseResolved` と `QPDF_Array::unparse` / `QPDF_Dictionary::unparse` / `QPDF_Stream::unparse` | `libqpdf/QPDFObjectHandle.cc:1574-1593`; `libqpdf/QPDF_Array.cc:122-149`; `libqpdf/QPDF_Dictionary.cc:58-68`; `libqpdf/QPDF_Stream.cc:173-178` | `object_handle.rs::unparse` / `unparse_resolved` / `try_unparse_resolved` と `unparse_resolved_into`（`object_handle.rs:5900-5980,6120-6270`） | ✅ value/child handleを直接たどり、receiverとarray/dictionary childをqpdfの順序で解決する。辞書のnull値は省略し、配列のnull要素は保持し、間接childは参照形のまま出力する。`QPDF_Stream::unparse`に合わせ、間接streamは自身の参照形を返す。`unparse_resolved`の非fallible null fallbackと`try_unparse_resolved`のlogic-error境界を分離し、`unparse_materialize*`およびraw `Object` treeはこの経路から除去した。最終的なraw `Object`/materialize削除は`flpdf-25kg.3.48.6`の責務 |
| `QPDFObjectHandle::getTypeCode` / `getTypeName` | `include/qpdf/QPDFObjectHandle.hh:311-316`; `libqpdf/QPDFObjectHandle.cc:240-250`; `include/qpdf/Constants.h:108-128` | `object_handle.rs::ObjectValue::type_code` / `type_name` と `ObjectHandle::type_code` / `type_name`（`object_handle.rs:1025-1068,5708-5758`） | ✅ qpdfの `qpdf_object_type_e` ordinalをRust enumのdiscriminantから独立した明示的matchで保持する。handle側は `try_dereference` 後にvalue-layer code/nameを読む。`ObjectValue::Reference` はflpdfの既存 `set_object` redirect専用状態で、qpdfのvalue familyには対応物がないため後続のraw-route削除で消す |
| `QPDFObjectHandle::unparse` / `unparseResolved` と `QPDF_Array::unparse` / `QPDF_Dictionary::unparse` / `QPDF_Stream::unparse` | `libqpdf/QPDFObjectHandle.cc:1574-1593`; `libqpdf/QPDF_Array.cc:122-149`; `libqpdf/QPDF_Dictionary.cc:58-68`; `libqpdf/QPDF_Stream.cc:173-178` | `object_handle.rs::unparse` / `unparse_resolved` / `try_unparse_resolved` と `unparse_resolved_into`（`object_handle.rs:5900-5980,6120-6270`） | ✅ value/child handleを直接たどり、receiverとarray/dictionary childをqpdfの順序で解決する。辞書のnull値は省略し、配列のnull要素は保持し、間接childは参照形のまま出力する。`QPDF_Stream::unparse`に合わせ、間接streamは自身の参照形を返す。`unparse_resolved`の非fallible null fallbackと`try_unparse_resolved`のlogic-error境界を分離し、`unparse_materialize*`およびraw `Object` treeはこの経路から除去した。最終的なraw `Object`/materialize削除は`flpdf-25kg.3.48.6`の責務 |
| `QPDF_Array/Dictionary/Stream/String/Name/Real/Integer/Bool/Null/InlineImage/Operator/Reserved/Unresolved/Destroyed.cc` | 1814 | `object.rs` の `Object` enum に統合 | 🔀 |
| `QPDFObject.cc` / `QPDFValue.cc` | `libqpdf/qpdf/QPDFObject_private.hh:19-151,176-180`; `libqpdf/qpdf/QPDFValue.hh:18-153`; `libqpdf/QPDF.cc:1843-1858` | `object_handle.rs` の outer `ObjectSlot` + `SharedValueState` / `ObjectValue` / `ValueIdentity`、`reader/resolver.rs::ObjectCacheEntry` | 🔀 `SharedValueState` が qpdf の一つの `QPDFValue` pointer に対応し、payload、active identity、parsed offset、optional description state を共有する。qpdf の `Description` は `std::shared_ptr` で保持され、flpdf は parser-owned Template を parse call ごとに `Rc<Vec<u8>>` で共有し、より大きい Json/Child variant を box 化して description-free value の inline layout を小さく保つ。stream-local token-filter list と normalization marker は ObjectValue::Stream に置き、qpdf に対応しない mutation-generation counter は保持しない。outer `ObjectSlot` は別個の qpdf `QPDFObject` counterpart として handle/cache/provenance identity を保つが、source extent の `end_before_space` / `end_after_space` は qpdf の `ObjCache` に合わせて `ResolverCore::ObjectCacheEntry` 側へ置く。これは `assign` の value-pointer assignment、`swapWith` の value-pointer swap と ObjGen 復元、`updateCache` の cache-entry extent 更新に対応する。qpdf が reverse containment index を持たないため、production の child ownership は `ObjectValue` の forward handles だけで表現する。containment-root assertion は unit-test-only の forward-graph scan に分離され、production layout/実行時 mutation には追加 state を持たない。 |
| `QPDFObjGen.cc` / `QPDFObjGen.hh` | 68 | `qpdf_obj_gen.rs::QpdfObjGen`（raw xref registration・`ObjCache`・`ObjectHandle` の signed object/generation identity）+ `object_ref.rs::ObjectRef`（qpdf parser の valid indirect-reference surface）+ `reader/resolver.rs::ResolverCore::object_cache` / `object_handle.rs::ValueIdentity` | ✅ `QpdfObjGen` は qpdf と同じ signed `i32` の object/generation pair と raw xref・canonical cache/handle identity、object-number-wide free-row境界、`isIndirect`を担当する。`ObjectRef` は Rust 側の広い public/parser projection であり、raw identity へ入るときは `try_from_object_ref` の checked signed-int 境界を通す。`ObjectRef` の `1 <= object` と `0 <= generation < 65535` projection、object header の generation gate、linearization synthetic identity は raw QpdfObjGen と混同しない。object number 0 は qpdf の raw xref/free identity としてのみ保持し、`ObjectHandle` の indirect/projection にはしない。ただし解決は行う — qpdf は `QPDF::getObject`（`libqpdf/QPDF.cc:1952-1959`）でも `QPDF::resolve` でも `isIndirect()` や generation 範囲で分岐せず、xref に行のない identity を null に帰着させる（`libqpdf/QPDF.cc:1745-1748`）。`N G R` 範囲外の generation も同じ扱い。 |
| `QPDFXRefEntry.cc` | 51 | `xref_entry.rs`（`XrefEntry` = free / uncompressed / compressed の 3 variant）。consumer は `xref.rs` / `reader.rs` / `writer.rs` / `writer/{object_streams,plain/plan}.rs` / `linearization/{writer,plan}.rs` | ✅ `flpdf-qxba.9.2` で完全 cutover（`XrefOffset` 削除）。`xref.rs` 側に型定義は残っていない。`ResolverCore::raw_source_xref_entries` がqpdfの一つの `QPDFObjGen` keyed xref tableの正本で、`ObjectRef` keyedなsource viewはこの表からprojectionする。 |
| `PDFVersion.cc` | 68 | `pdf_version.rs` の `PdfVersion` | ✅ |
| `QPDFMatrix.cc` | 140 | `matrix.rs` の `Matrix` / `Rectangle` | ✅ |
| `QPDFObjectHandle::mergeResources` / `shallowCopy` | `QPDFObjectHandle.cc:431-434,1063-1153,2072-2079` | `object_handle.rs:5070` + `page_annotation_flatten.rs:666-740`（widget appearance の既定リソース consumer） | ✅ live `ObjectHandle::merge_resources` を使用し、receiver・other・各top-level resource categoryをqpdfの`isDictionary`/`isArray`相当で自己解決してから分岐する。missing category は top-level が direct の shallow copy になり、nested indirect child は handle を保持する。array の `isScalar` 判定と unique-name pool の second-level dictionary 判定は qpdf と同じく各 nested handle を解決し、解決エラーを伝播する。`acroform_document_helper.rs` の `DrMap` と `overlay_appearance_stream.rs` が name-conflict overlay merge を担う |
| `QPDFObjectHandle::getResourceNames` | `QPDFObjectHandle.hh:831-835`; `QPDFObjectHandle.cc:1156-1170` | `object_handle.rs::ObjectHandle::get_resource_names` + `try_get_resource_names` | ✅ second-level keys from every dictionary-valued resource category are collected through the canonical handle resolver; the public facade is available to `flpdf-qtest-tools`, and resolver failures remain a `Result` at the Rust boundary. |

`flpdf-y88w` では、dirty-tracking bridge 撤去後に残った13個の `_pdf: &mut Pdf`
parameters を qpdf の責務境界ごとに再監査した。`QPDFEFStreamObjectHelper::newFromStream`
（`QPDFEFStreamObjectHelper.hh:93`）、`QPDFPageObjectHelper` の handle-only helper
（`QPDFPageObjectHelper.cc:212-215,539-651`）、NNTree の live array/key mutation
（`QPDFObjectHandle.cc:869-955`）、および page repair/inherited-child の
`replaceKey`（`QPDF_optimization.cc:229-235`）は document を受け取らないため、flpdf
の対応 helper からも引数を削除した。実際の resolver、allocation、warning ownership を
必要とする上位 caller の `&mut Pdf` は維持しており、これは bridge を別 API へ移した変更ではない。

`QPDFValue` の object description は qpdf の `std::string` 相当なので、flpdf の
`ObjectDescription` と parser/resolver template は raw byte semantics を保ったまま、
parser-owned Template を parse call ごとの `Rc<Vec<u8>>` として共有する。
`ObjectHandle` の object warning は `Diagnostic::raw_message` / `message_bytes()` と
document logger の byte route を通り、UTF-8 変換は既存の表示用 `Diagnostic::message`
や `Error` 境界に限定する。これにより `QPDFValue.cc:14-61` の `$OG` / `$PO` / `$VD`
展開と、`QPDFObjectHandle.cc:2168-2212` の object warning は non-UTF-8 input source
description を失わない。

`qpdf/test_driver.cc:2139-2213` の test 60 は、`ObjectHandle::make_resources_indirect`、
`merge_resources`、`get_unique_resource_name` とlive `Pdf::trailer`を通るqtest consumerとして
実装済みである。4回のconflict merge結果とQDF/static-ID `a.pdf`をpinned qpdf 11.9.0の
`test60.out` / `unique-resources.pdf`と比較し、対応する `merge-dictionary 2,3` の
同一run結果を `harness.log` と `qtest-results.xml` で確認する。driver独自のresource
traversal・allocation・trailer snapshotは追加していない。

`flpdf-tcfj` では、qpdf 11.9.0 の `QPDF::resolve` が `isUnresolved` を確認して永続 `m->obj_cache` を一度だけ更新する責務（`QPDF.cc:1700-1753`）に合わせ、xref bootstrap の raw object view と handle-native view を `SharedBootstrapCache` の同一状態へ束ねる。`BootstrapHandleDocument`、再帰ガード、ObjStm の解決済み集合、診断、reconstruction trigger は xref-loading operation 全体で共有し、`read_uncompressed_object` の `FileObjectDiagnostic` は一度だけ転送する。raw view が先に materialize した値は handle slot へ seed し、handle view が先に解決した値は raw lookup から再利用するため、同じ bootstrap object の再パースと警告の二重出力を避ける。

`flpdf-uwn0` では qpdf の `makeIndirectFromQPDFObject` (`QPDF.cc:1882-1894`) / `replaceObject` (`QPDF.cc:1986-1993`) が source xref と別に `m->obj_cache` へ登録する allocation と、参照解決で同じ cache に入る dangling null を object-ref view で区別する。qpdf の `getAllObjects` は `fixDanglingReferences` 後の `m->obj_cache` 全体 (`QPDF.cc:1258-1294`) を列挙し、live probe でも `newIndirectNull()` は列挙される。flpdf の `ResolverCore::allocated_object_refs` はこの provenance だけを canonical allocation 境界で記録し、canonical object-ref views が allocated indirect null を落とさないようにする。`ResolverHandle::make_indirect_from_object_handle`（A12、qpdf の `makeIndirectFromQPDFObject`/`makeIndirectObject` と1:1）からのこの記録には legacy cache/memo の互換 bridge や qpdf-deviation marker を追加しない。一方 `ResolverHandle::replace_object`/`swap_objects`（A16/A17）末尾の同じ記録には、`flpdf-3yn9.48.172`（`crates/flpdf/src/reader/resolver.rs`）で逸脱 marker を追加した（分類 (C)：出力バイトは変わらない）——qpdf の `updateCache` 自体が呼び出し元を区別せず、`resolve` の dangling-reference 経路も `replaceObject`/`swapObjects` と同じ `updateCache(og, ..., -1, -1)` を通るため、この2箇所での「document-owned allocation」と「単なる dangling 解決」の区別自体に qpdf 側の対応物がない（`docs/qpdf-route-matrix/a-objecthandle-resolver.md` の A16/A17 行を参照）。**2026-09-19 追記**: `ResolverHandle::replace_object` の foreign-owner guard （`belongs_exclusively_to_pdf`）は**これとは別の逸脱で、分類 (C) には該当しない**——(C) は「出力バイトを変えない」ことが前提だが、この guard は変える。qpdf の `QPDF::replaceObject`（`QPDF.cc:1986-1993`）は indirect / uninitialized の handle しか弾かず `checkOwnership` を呼ばないので、foreign な indirect 子を持つ direct 値は qpdf では成功して書き出される一方、flpdf は `Unsupported` を返して何も書かない。未解決の挙動乖離として marker を残し、**route matrix A16** で追跡する（guard は `replace_object` にのみ存在し、A17 が追う `swap_objects` は持たない）。

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

`flpdf-lsue` では、`QPDF::Members::xref_table` の raw `QPDFObjGen` view と、
parser が `N G R` として受理できる `ObjectRef` viewを分離したまま、同じqpdf責務へ
接続する。`showXRefTable` は raw viewを表示し、`ResolverCore::object_cache` と
`ObjectHandle::ValueIdentity` も raw identity を保持したうえで、通常のresolver/writer
consumerへは必要な場合だけ valid viewを射影する。`QPDF.cc:689-718,1149-1184,1212-1236`
に対応し、raw viewの
`/Prev`後処理・reconstruction seed・canonical handoffは `flpdf-lsue` で固定する。

### `flpdf-r3vn` raw-identity consumer cutover (2026-09-11)

The raw identity must remain raw after the resolver boundary. qpdf's
`QPDF::readObject` binds `StringDecrypter` to the `QPDFObjGen` read from the
object header (`libqpdf/QPDF.cc:1330-1345`), and `QPDF::pipeStreamData` carries
the same identity through original/foreign stream delivery
(`libqpdf/QPDF.cc:120-180,2477-2538`). The object-key algorithm appends the low
two generation bytes without applying the parser's `N G R` gate
(`libqpdf/QPDF_encryption.cc:325-357,954-968`).

flpdf's resolver now carries that raw key through original stream piping,
parse-time string decryption, and xref-stream cache protection. `unparse`
also emits raw `N G R` syntax from `ObjectHandle::qpdf_obj_gen()` when the
projection to `ObjectRef` is unavailable (`libqpdf/QPDFObjectHandle.cc:1574-1593`).
The effective xref snapshot retains projectionless default entries, while JSON
and CLI object selectors accept qpdf's signed generation surface. Writer
preserve-unreferenced seeds copy a raw-generation orphan into a fresh
generation-zero output identity because no valid in-file `N G R` edge can name
the source header; the output graph and qpdf-visible orphan value are retained.

`QPDFJob::parse_object_id` is also an output-time operation, not an argv
validation step: `doJSONObjects` writes the v1 object-map opener before calling
`getWantedJSONObjects`, while the v2 path constructs that selector set before
delegating to `QPDF::writeJSON` (`libqpdf/QPDFJob.cc:929-997`). flpdf therefore
keeps raw `jsonObject`/`--json-object` strings on the job boundary and parses
them only when the selected object section begins. A selector overflow after
the pages or other preceding sections has been written leaves the same partial
JSON prefix on stdout; the CLI then explicitly finishes the logger's save
pipeline even on this error path, matching qpdf's observable process output.

### `flpdf-fo71t` raw identity consumer cutover (2026-09-14)

qpdf treats `QPDFObjGen` as the identity of an indirect object, independently
of whether that pair can be written as a parser-valid `N G R` reference.
`QPDFValue::getDescription` expands `$OG` and default descriptions from its
raw `og` (`libqpdf/QPDFValue.cc:14-61`); `copyForeignObject` keeps its
per-source `ObjCopier` map and loop set keyed by raw `QPDFObjGen`
(`libqpdf/QPDF.cc:2018-2213`); and `mergeResources` reuses resource values
through a raw `QPDFObjGen` map (`libqpdf/QPDFObjectHandle.cc:1063-1128`).

The same boundary applies to the higher-level consumers. `getAllObjects`
walks the raw `obj_cache` (`libqpdf/QPDF.cc:1285-1294`), FileSpec creation
inserts the embedded-file handle without re-projecting it
(`libqpdf/QPDFFileSpecObjectHelper.cc:85-107`), and page, outline, and form
walks use `QPDFObjGen::set` for their indirect seen state
(`libqpdf/QPDF_pages.cc:39-138`,
`libqpdf/QPDFOutlineDocumentHelper.cc:5-23`,
`libqpdf/QPDFFormFieldObjectHelper.cc:35-131`). Form-XObject resource
traversal likewise retains raw handle identity in qpdf's `forEachXObject`
queue (`libqpdf/QPDFPageObjectHelper.cc:317-349`).

flpdf now uses `ObjectHandle::qpdf_obj_gen()` and
`QpdfObjGen::is_indirect()` for these identity/discriminator decisions:
description expansion, persistent foreign-copy maps and visited sets,
resource reuse, FileSpec embedded-stream preservation, Form-XObject resource
traversal, and page/outline/form cycle guards. `ObjectRef` remains only the
checked valid `N G R` projection and the existing public APIs whose return
type is explicitly `ObjectRef`; an unprojectable page identity crosses that
boundary as an explicit error rather than a panic. The public
`Pdf::get_all_objects` route already returns raw cache handles, and its
regression test asserts that a matching `5 65536 obj` entry is retained.

The foreign copier also retains qpdf's per-source `to_copy` queue across a
failed replacement pass and clears it only after every reserved object has
been replaced (`include/qpdf/QPDF.hh:891-897`, `libqpdf/QPDF.cc:2066-2093`).
This keeps a retry from treating a partially replaced reservation as a
completed copy. The page-selection preserve route applies the same raw-cache
rule before copying primary unreferenced objects: `QPDFWriter::enqueueObjectsStandard`
seeds directly from `getAllObjects` (`libqpdf/QPDFWriter.cc:2907-2913`), so
`canonical_live_object_handles` keeps a raw generation such as `5 65536`
through the copy boundary and only narrows destination writer references where
the consumer explicitly requires `ObjectRef`.

### `flpdf-ymuj.6.10` single raw ValueIdentity (2026-09-14)

qpdf's `QPDFValue` owns one raw `QPDFObjGen` (`libqpdf/qpdf/QPDFValue.hh:149-152`);
`QPDFObject::assign`/`swapWith` do not maintain a second public-reference
identity (`libqpdf/qpdf/QPDFObject_private.hh:117-143`). flpdf's
`ValueIdentity` therefore keeps only `Option<QpdfObjGen>` as its canonical
object identity. `ObjectRef` is derived at the checked valid `N G R`
projection boundary (`qpdf_obj_gen.rs::to_object_ref`); raw generations such
as 65535/65536 remain available to resolver, writer, JSON, xref, and encryption
consumers without being projected as a valid `ObjectRef`. Object number 0
retains the separate non-indirect contract from `flpdf-3sbf`.

### `flpdf-ymuj.6.11` live parser integer ownership (2026-09-14)

qpdf's live parser does not materialize an owned token for the ordinary
integer-reference candidate. `QPDFTokenizer::nextToken` reuses its
`val`/`raw_val` buffers (`libqpdf/QPDFTokenizer.cc:77-89,921-925`), while
`QPDFParser::parseRemainder` retains only two integer values and source offsets
until it sees the following token (`libqpdf/QPDFParser.cc:140-175`). flpdf's
general tokenizer and external `LiveTokenSource::next_token` contract remain
owned, but the canonical live file parser uses a compact `LiveToken::Integer`
path backed by `Tokenizer::get_integer`; its raw capacity is cleared and
reused, and only non-integer tokens that may be replayed retain owned bytes.
This preserves qpdf's delimiter unread, integer overflow diagnostics,
`N G R` validity gate, offsets, and all name/string/filter consumers while
removing the per-integer `Token` allocation from the document parser.

### `flpdf-ymuj.6.12` selected linearization hint payload ownership (2026-09-14)

qpdf's `QPDF::generateHintStream` receives the writer's compression choice,
counts the uncompressed bit positions for `/S` and `/O`, and writes through one
`Pl_Buffer`; `Pl_Flate` is inserted only for the selected compressed mode
(`libqpdf/QPDF_linearization.cc:1758-1795`). `QPDFWriter::writeLinearized`
selects that mode from `compress_streams`/`qdf_mode` and retains the completed
hint object for pass 2 (`libqpdf/QPDFWriter.cc:2289-2323,2656-2801,2875-2882`).
The canonical flpdf linearization writer now uses `HintStreamMode` and
`SelectedHintStream`, retaining only the selected raw or Flate payload while
keeping the uncompressed `/S` and `/O` offsets. The public dual-form encoder
remains an inspection/test helper for existing `show.rs` consumers; it is not
used by the canonical writer route and no new bridge caller is added.

### `flpdf-ymuj.6.13` writer-owned linearization RenumberMap (2026-09-14)

qpdf's linearized writer keeps one mutable object-renumber map in its writer
state and uses it across layout, xref/hint calculation, and generation discard
(`libqpdf/QPDFWriter.cc:2536-2554,2858,2869-2882`). flpdf's canonical
`PdfWriter` route now moves its owned `RenumberMap` into
`write_linearized_impl`; the two-pass emitter mutates that owner directly
instead of cloning the complete `Vec`/`BTreeMap` pair before pass 1. The
test-only borrowed compatibility helper clones explicitly at its boundary.
ObjStm relocation maps and qpdf-required generation-removal state remain
separate and are not removed by this ownership slice.

### `flpdf-ymuj.6.15` linearization page reach from object-user ownership (2026-09-14)

qpdf classifies linearization parts by walking the canonical
`object_to_obj_users` relationship. `QPDF::calculateLinearizationData` counts
page users while separating first-page, later-page, thumbnail, outline, root,
and other users (`libqpdf/QPDF_linearization.cc:963-1105`); the same retained
relationship supplies later-page shared-object identifiers
(`libqpdf/QPDF_linearization.cc:1350-1410`). The maps are the paired ownership
in `QPDF::Members` (`include/qpdf/QPDF.hh:1515-1517`), not a second
object-to-page map.

flpdf's production `LinearizationPlan::from_pdf` now derives both private-page
classification and Part 4 reach counts from `Optimization::page_users`, a
borrowed page-only view over `object_to_users`
(`crates/flpdf/src/optimization.rs:114-115,161-170`). It no longer clones
every page closure into `all_closures` or materializes a temporary `page_reach`
map. Ordered page closures remain owned only where the linearization output
order requires them; page partitioning, shared hints, thumbnail/outline
handling, ObjStm routing, and manual-plan fallbacks remain on their existing
qpdf-corresponding boundaries.

### `flpdf-ymuj.6.17` ObjStm page ownership from object-user ownership (2026-09-14)

qpdf places Part 7 objects page by page: each page dictionary and its
`lc_other_page_private` members are emitted in the page's section, while the
same object-user classification drives the page object count and excludes
Part 8/non-page containers (`libqpdf/QPDF_linearization.cc:1224-1264,1388-1400`).
The source of that ownership is the retained `object_to_obj_users` map
(`include/qpdf/QPDF.hh:1515-1517`), not a materialized per-page set table.

flpdf's production ObjStm anchor and page-offset hint routes now use
`Optimization::page_users` to identify the unique non-first-page owner of a
Part 7 member. Manually constructed plans retain a bounded vector-scan
fallback. This removes only the writer/hint `Vec<BTreeSet<ObjectRef>>`
re-materialization; `per_page_private_objects` remains for qpdf-corresponding
page byte-length and manual-plan contracts. Part 7 ordering, Part 8
co-location exclusion, and generate/preserve/disable ObjStm behavior remain
unchanged.

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF.cc` | 2667 | production `writer/object_streams/eligibility.rs::compressible_objgens_qpdf_plan` は `QPDF.cc:2393-2474` のLIFO walk、object-number単位のvisited判定、live cacheのupper_bound相当、stale generationの`removeObject`副作用を所有する。`reader.rs::Pdf::remove_object_handle` → `reader/resolver.rs::remove_object` はxref削除→alias null化/objgen解除→cache削除の順を再現（`QPDF.cc:1996-2005`）。C++ vector<bool> はRustのBTreeSetで同じobject-number-wide visited契約を保つ。plain Generateがsetup-time snapshotを最初のconsumerとして使い、旧reader wrapperはtest-only、writer eligibility/Preserve/linearizedの残consumerは移行対象。`reader.rs::get_object_stream_data` → `reader/resolver.rs::get_object_stream_data` が `QPDF::getObjectStreamData`（`QPDF.cc:2381-2390`）の document-owned type-2 mapping を所有する。既存 caller map の保持/上書き、object-number と container-number、解決を伴わない current xref 読み取りを維持し、plain Preserve は compressible walk の前に取得する（`QPDFWriter.cc:1941-1967`）。reader xref API全体と残 specialized/linearized lookup は維持する。`engine.rs`(475: `Pdf::empty`、ほか8つの public factory — `Pdf::open` / `open_with_repair` / `open_best_effort` / `open_with_options` / `open_mem` / `open_mem_with_options` / `open_mem_owned` / `open_mem_owned_with_options` —、`open_with_repair_mode`、`NEXT_PDF_ID`。`emptyPDF` / `processFile` / `processMemoryFile` の construction path) + `pdf.rs`(297: `Pdf<R>` container、`Drop` = `QPDF::~QPDF`、version/trailer/root/extension/page-enumeration-state accessors。`QPDF.hh:1438-1518`; `QPDF.cc:215-232,2323-2358,2647-2651`) + `reader.rs`(8185: object resolution, recovery, diagnostics, authentication, and `Pdf::get_xref_table` / `Pdf::get_all_objects`) + `reader/resolver.rs`(2367: canonical resolver。`QPDF::resolve` が触る `QPDF::Members` — `m->file` / `m->xref_table` / `m->obj_cache` / `m->resolving` / `m->resolved_object_streams` / `m->attempt_recovery` / `m->encp` — を `ResolverCore` に集約し、`Rc<RefCell<..>>` 経由で `ObjectHandle` の `Weak<dyn DocumentResolver>` から到達可能にする。`m->obj_cache` は canonical handle registry そのもので、`Pdf::get_object_handle`（= `QPDF::getObject`, `QPDF.cc:1952-1959`）と `Pdf::drop`（= `~QPDF`）の両方がここを見る。`Pdf::get_xref_table` は `QPDF::getXRefTable`（`QPDF.cc:2370-2377`）の effective source table snapshot、`Pdf::get_all_objects` は `fixDanglingReferences` と `m->obj_cache` enumeration（`QPDF.cc:1258-1294`）を canonical handle 上で実行する。`m->encp`（`flpdf-25kg.3.11`）は `Pdf::encryption` と同一の `Rc<RefCell<Option<EncryptionState>>>` を共有し、qpdf の `shared_ptr<EncryptionParameters>` を複数の owner が保持する形を再現する。`pipe_stream_data` は `QPDF::pipeStreamData` と同じく source read 前に `QPDF::decryptStream` 相当を呼び、同じ cell の method state / object-key cache を更新して AES/RC4 stage を前置する。`flpdf-25kg.3.5`/`.3.5.1`（ともに close 済み）で `readObjectAtOffset`/`readObject`/`readStream` の全 xref 形式（uncompressed type 1・ObjStm・canonical type-1 stream framing recovery を含む）が canonical resolver へ移植済み。`reader.rs`/`xref.rs` 自身の filter 呼び出し箇所の consumer cutover も `flpdf-egzr.3.2.10`（子 `.3.2.10.1`/`.3.2.10.2` close 済み、PR #859 merged）で完了し、`.48.49` で production 経路の `decode_stream_data`/`encode_stream_data` legacy wrapper と非qtest callerを撤去した。qtest exception の recovering API は別セッションの残スコープである。resolve 時文字列復号と pipe 時ストリーム復号 primitive は移植済み。残る raw `Object` route（`resolve_borrowed` と repair/recovery 経路）の削除は `flpdf-egzr.3.2.8`（close済み）) + `xref.rs`(6191。`reader/file_object.rs` は `.48.151` で撤去) + `object_copy.rs`(342: `copyForeignObject`) + `cache.rs`(112: xref 由来の `ObjectCache` / `CacheEntry`。消費者は `reader.rs`) + `writer/object_streams/eligibility.rs`(263: qpdfの `getCompressibleObjGens` eligibility traversal) + `reader.rs`(491: `Pdf::remove_security_restrictions`) + `acroform_document_helper.rs`(649: `AcroFormDocumentHelper::disable_digital_signatures`) + `signatures.rs`(read-only inspection and flpdf-only SigFlags/value helpers) | 🔀 |
| `QPDF.cc`（xref registration/recovery と mutation 境界） | `516-607,686-708,1187-1210,1996-2005` | `xref.rs` の `XrefRegistration` が xref 読み取り・recovery merge ごとの object-number-wide `deleted_objects` free-row filter を所有する。通常の `read_xref` は `/Size` 整合性検証までこの set を使い、その後 clear する（`:686-708`）。一方 `reconstruct_xref` の line scan は `:575` で clear してから `:576-607` の candidate xref-stream re-read に進み、その re-read は fresh registration を持つ。`discard_lower_generations` は通常のread_xrefチェーン完了後だけで呼び、reconstructionのline scanで見つかった複数generationは保持する（`flpdf-k4tn`, qpdf `QPDF.cc:710-718` の適用境界）。`ResolverCore` にはいずれの一時 state も渡さない。`reader.rs` の `Pdf::set_object` / `replace_object` は canonical cache replacement だけを担い、この xref set を clear/add しない。canonical xref/cache removal と outstanding handle の null 化は `remove_object_handle` が担う。 | ✅ |
| `QPDFParser::parse` / `QPDF::readObject`（indirect handle生成とstream framingの境界） | `QPDFParser.cc:155-172`; `QPDF.cc:1331-1349` | `parser.rs::parse_qpdf_file_object_handle_with_diagnostics` はtokenize中にindirect `ObjectHandle`を生成する。`engine.rs` がparse前に構築したcanonical resolverは classic trailer に加え、xref stream の `readObjectAtOffset`/`readStream` からも同じ handle/cache を所有する。通常のObjStm memberは canonical resolverの `resolveObjectsInStream` を使い、owner-less standalone xref loaderのbootstrap memberも `.48.14` で同じ direct parser / Specialized stream accessor / cache metadata順序へ揃えた。`.48.15.1` で canonical `Pdf::open` の `LoadedXrefState` は bootstrap cacheを保持せず、trailer/xref-stream handleの再bindも行わない。owner-less loader固有の bounded reconstruction window、`BootstrapHandleState`、warning replay/live deliveryは residual として後続sliceに残る。 | 🔀 canonical production xref-stream read と canonical open handoff は `flpdf-3yn9.48.13` / `.48.15.1` で移行済み。owner-less loaderの第2 document stateとbootstrap rebind/replayは `.48.15`/`.48.72` で解消済み（public `load_xref_and_trailer*`/`LoadedXref` ごと撤去） |
| `QPDF.hh`（`EncryptionParameters`） | 899-921 | `encryption/state.rs`（`EncryptionState`, `EncryptionInspectionState`, `EncryptionMode`）と `reader.rs` の個別 projection（`encryption_version`, `encryption_revision`, `encryption_length_bits`, `trimmed_user_password`, `encryption_methods`）。qpdf の `EncryptionParameters` 自体は private state であり、公開 inspection contract は `isEncrypted` の個別出力、`getTrimmedUserPassword`、`ownerPasswordMatched`、`userPasswordMatched`、`allow*`。flpdf は qpdf にない集約 `EncryptionInfo` / `Pdf::encryption_info` を持たず、同じ個別責務を公開する。qpdf は独立した2つの bool、`encrypted` / `encryption_initialized`（`QPDF.hh:907-908`）を持つが、flpdf はこれを単一の `Option<EncryptionState>`（`None` = 未初期化 or 認証済み未暗号化のいずれか、`Some` = 認証済み暗号化）に畳んでいる。安全性の根拠: `encryption_initialized` の唯一の用途は `initializeEncryption()`（`QPDF.cc:471` で1文書につき高々1回しか呼ばれない）内の再入防止ガード（`QPDF_encryption.cc:721,724`）で、flpdf の構造上この再入自体が起こり得ないため観測可能な挙動差は生じない。逸脱理由は `reader/resolver.rs` の `ResolverCore::encryption_parameters` doc にも記載（`flpdf-25kg.3.11`） | 🔀 `.3` で `reader.rs::encrypt_dictionary_handle` から `parse_inspection_state` / `authenticate` まで canonical `ObjectHandle` accessor に切り替え、`/CF`・`/Perms`・標準ハンドラ入力を raw `Dictionary` の materialize なしで読む。writer donor-copy の raw snapshot は writer slice で除去する |
| `QPDF::interpretCF` (`QPDF.hh`; `QPDF_encryption.cc`) | `1122-1127`; `700-716` | `encryption/crypt_filters.rs` の `interpret_cf` / `interpret_cf_from_handle` | ✅ 値選択を共有し、ObjectHandle 版は `try_as_name` で lazy resolve。`crypt_filters` → built-in `/Identity` → `e_unknown`、non-name → `e_none` の順と resolver error 伝播を維持。`reader/resolver.rs` の pipe-time `decryptStream` consumer が live stream dictionary に対して使用する |
| `QPDF::decryptStream` (`QPDF_encryption.cc`) | `1045-1153` | `reader/resolver.rs` の `inspect_stream_encryption` / `pipe_stream_data` | ✅ `/XRef` early return、`/V >= 4` gate、typed direct `/Crypt` と equal-length array pairing、Crypt-before-Metadata precedence、unknown warning + `cf_stream` rewrite、qpdf の object-key cache、`PlAesPdf` / `PlRc4` 前置を source read 前に実行。stream dictionary の lazy resolve 中は encryption cell borrow を保持しない。resolve-time payload 復号の重複は qpdf-deviation として解消対象（本issueではconsumer cutoverを範囲外とする） |
| `QPDFParser.cc` | 519 | `parser.rs` の `LiveInput` / `LiveTokenSource` / `LiveFileParser` は `InputSource` を一度だけ前進する file-object baseline（`QPDFParser.cc:27-518`）。canonical resolver の uncompressed type-1 consumer と、decoded-stream-relative `SliceLiveInput` 経由の ObjStm member consumer（`reader.rs::parse_object_stream_entry`）が使い、token 終端の one-character unread、diagnostic、top-level/nested/container/null の parsed offset、empty/dictionary/bad-token/depth recovery をここで共有する。uncompressed 側は canonical unresolved handle を同時に生成する。live canonical と context-none explicit の parser invocation は qpdf の parse-call description template を非 null handle に stamp し、container の render shift と null の無記述も維持する。`ObjectHandle::parse` / `parse_with_description` は同じ context-none entry point で、warning を `Error`、nested `N G R` を `Error::Internal`、非 C whitespace の後続を parse error にする。`ObjectHandle::parse_with_context` は同じ live parserをcanonical resolver/cacheへ接続し、qpdfの未解決参照identityとdocument warning sinkを維持する | 🔀 canonical uncompressed consumer は `StringDecrypter`（`flpdf-25kg.3.17`）を object-ref と shared `EncryptionState` に束縛し、`QPDF::readObject` / `QPDFParser` と同様に top-level・array・nested dictionary・stream dictionary の `tt_string` だけを token 時に復号する（`QPDF.cc:1331-1340`; `QPDFParser.cc:114-121,327-365`; `QPDF_encryption.cc:977-1039`）。`.6.40` では完成時の署名判定を qpdf の `isNameAndEquals` / `isString` 相当の `try_is_name_and_equals` / `try_is_string` へ切り替え、`contents_string` 相当のraw captureを最初に短絡判定する。通常の辞書では間接 `/Type` / `/Contents` を解決せず、name/string payloadの一時コピーを避ける。raw capture branchが間接 `/Type` を解決すると parse guard が再入を検出するが、document-owned な production 経路では `ResolverHandle::resolve_indirect_inner` の error branch（`reader/resolver.rs:5494-5502`）がそれを捕捉して warning を積み、handle を null へ解決して `Ok(())` を返すため、`try_is_name_and_equals` は `Ok(false)` になり署名判定は不成立として進む。re-entrant error がそのまま呼び出し元へ伝播するのは `DocumentResolver` を直接実装したテスト double の場合のみ。完成した `/Type /Sig` + `/ByteRange` 辞書だけは従来どおり raw `/Contents` bytes と parsed offset を復元する。ObjStm / context-none explicit parse / content mode は decrypter を渡さず、unknown word も callback 非呼出し。Content mode は既存 `Parser` を維持し、file-object live parser は content grammar を兼用しない |
| `QPDFTokenizer.cc` | 965 | `tokenizer.rs`（18 token types、owned value/raw/error bytes/offset、push/pull、pull-only `allowEOF`、`includeIgnorable`、space/comment、bad-token recovery、max length、`betweenTokens`、unread、inline-image `EI` discovery。`QPDFTokenizer.hh:34-193`; `QPDFTokenizer.cc:45-965`）+ `parser.rs` の content mode + `content_stream.rs` の `ParserCallbacks` orchestration + `object.rs` の `Operator` / `InlineImage`（`QPDFParser.cc:27-125,130-377`; `QPDFObjectHandle.cc:1770-1847`） | ✅ `QPDFTokenizer` の責務境界を移植済み。object/parser/content callback consumers は共有 tokenizer を使用し、旧 content lexer は削除 |
| `InputSource` 系 5 ファイル | 625 | `Read + Seek` ジェネリクスで代替。所有者は `reader/resolver.rs` の `ResolverCore`（`m->file` 相当）。`ResolverCore` のメソッドは `InputSource` の 3 操作 `seek`/`tell`/`read`（`InputSource.hh:71-74`）に限定し、`OffsetInputSource`（`QPDF.cc:406`）が担う header shift は `seek`/`tell` が適用する。例外は `rewind_underlying_source` 1 つで、これは wrapper が持つ `proxied`（`libqpdf/qpdf/OffsetInputSource.hh:24`）に相当する — `OffsetInputSource::rewind` は logical 0 に行く（`OffsetInputSource.cc:55-59`）ため `m->file` では表現できない。owned-window 系の legacy helper（`read_window` / `read_to_owned`）は 2026-09-08（`flpdf-3yn9.48.25`）に `MAX_RESOLUTION_FALLBACKS` / `resolution_fallbacks_remaining` ごと削除済みで、現在は `crates/flpdf/tests/qpdf_route_hygiene_tests.rs` が不在を検査する。撤去前は `ResolverHandle` 側で `#[deprecated]` により記録していた（関数単位で切り離せるため CLAUDE.md 分類 (C) のマーク方式は comment block ではなく `#[deprecated]`）、`ResolverCore` の面には置かない | ⚪ |

2026-09-12（`flpdf-3yn9.48.34`）: parser dictionary warnings now retain the
tokenizer-decoded slash-prefixed key as raw bytes through the internal
`ParserDiagnostic` transport. Document-owned parses deliver the same bytes to
`QpdfExc`/`Diagnostics`, and contextless `ObjectHandle::parse` throws the
qpdf-shaped `QPDFExc` with the `parsed object` filename. The parser no longer
uses the legacy slash-removal helper and does not call `QPDF_Name::normalizeName`;
that qpdf routine remains available to the unparse/JSON and writer name-emission
owners.

`QPDFObjectHandle::parsePageContents` keeps the `all_description` produced by
`arrayOrStreamToStreamArray` when it enters `parseContentStream_data`
(`libqpdf/QPDFObjectHandle.cc:1438-1485,1740-1850`). The canonical flpdf
content-parser retains that source description for qpdf exception formatting;
object callbacks carry the parsed object span. In particular, an EOF inside an
inline image uses the end position returned after the tokenizer consumes the
truncated image, matching qpdf's `input->tell()`.
For document-owned handles, the same recovery diagnostics are delivered only
through the owning `DocumentResolver` warning sink, which preserves qpdf's
`QPDFObjectHandle::warn` collection/logger boundary. Detached parses have no
owning warning sink, so the first recoverable diagnostic returns the formatted
`QPDFExc::what()` as an `Error::System`; no recovered callback object or
`handleEOF` follows that throw, matching `QPDFParser::warn`'s null-context arm
(`libqpdf/QPDFParser.cc:487-498`). The qtest `parsing 10` regression pins the
complete document-owned diagnostic context; the other test-37 parsing rows
retain their existing object spans and `handleEOF` output while draining the
document diagnostics after each parse.

pre-`Pdf` の xref bootstrap も qpdf の `QPDF::Members::file` / `InputSource`
の遅延 read 境界（`QPDF.hh:67-97,1453-1457`、`QPDF.cc:245-275`）を保つ。
`BootstrapHandleDocument` は handle state と diagnostics を先に共有するが、
入力 snapshot は `OnceCell<Rc<[u8]>>` として未解決 indirect object または
indirect `/Length` の実解決時だけ初期化する。direct-only の trailer/xref
metadata path は入力全体を複製しない。これは qpdf の object/stream lazy read
を generic `Read + Seek` の static resolver lifetime に合わせるための内部
ownership 実装であり、PDF bytes、warning、xref/cache identity は変更しない。
2026-09-14（`flpdf-ymuj.6.20`）: canonical `Pdf::open` の xref loading は
`engine.rs` の complete-source read を撤去し、`ResolverHandle` の live
`Read + Seek` boundary から header、tail、current xref section を必要な範囲
だけ読む。`/Prev` が現在の section window の外にある場合も、その section
だけを同じ source から取得し、reconstruction は chunked live scan として
実行する。offset 0 の qpdf guard、linearized file の前後両方向の `/Prev`、
warning order、canonical cache identity は変更しない。byte-slice loader は
ownerless test-only path に限定され、canonical route に complete input
snapshot は残らない。2026-09-18（`flpdf-3yn9.48.151`）: その ownerless
test-only path 自体を削除し、xref loading の経路は
`load_xref_state_from_source` の 1 本になった。
| `QPDF_pages.cc` | 319 | `pages/repair.rs`（`QPDF_pages.cc:39-75` の `getAllPages` root correction と `:77-150` の `getAllPagesInternal` repair/enumeration を canonical `ObjectHandle` graph 上で実装） + `optimization/inherited_attrs.rs`（canonical page promotion/clone と衝突しない `Pdf::next_obj_gen` allocation） + `pages.rs` / `pages/tree_rebuild.rs`（flatten/insert/remove と legacy consumer の残り） | 🔀 `flpdf-25kg.3.7` で repair/enumeration の canonical route を追加。`.3.2.6.15` では `QPDFPageObjectHelper::getAttribute` の bottom-up `/Parent` climb（`QPDFPageObjectHelper.cc:217-263`。`QPDF_optimization.cc:121-245`/`QPDF_pages.cc:154-180,205-248` は top-down push とツリー変異のオラクル）を、共有 `PageParentCursor` / `resolve_inherited_handle_with_max_depth` として live `ObjectHandle` で切り出した。直接親の identity、間接親の canonical `ObjectRef`、null/非辞書親、cycle/depth guard をこの境界で保持し、`/Rotate` の未指定を合成しない。`.3.2.6.16` では `tree_rebuild` の単一文書 consumer を canonical handle route に切り替え、選択ページの inherited `/MediaBox`・`/CropBox`・`/Resources`・`/Rotate` を再親子付け前に push、直接 non-scalar は `make_indirect_from_object_handle` で共有 allocation を in-place 昇格、既存 indirect 値は identity を保持し、duplicate は `shallow_copy`、root `/Kids`・`/Count`・各 leaf `/Parent` は live handle を replace/remove する。qpdf の absent `/Rotate` は合成しない。`QPDFObjectHandle.cc:1199-1209,2072-2079` の live replace/remove・shallow-copy がこの consumerの mutation oracleである。`QPDFJob.cc:2360-2632` の page-selection orchestration はこの境界の外であり、`page_extract` uses canonical `copyForeignObject`/`ObjectHandle`; `page_merge` / `page_label` remain separate consumers |
`flpdf-ihyup.2` (2026-09-18) keeps the repaired `/Pages` root and leaf sequence
as live `ObjectHandle` values through inherited-attribute push, optimization
object-map construction, linearization content probing, and tree rebuild. This
matches qpdf's `m->all_pages` raw `QPDFObjGen` identity; only the existing
public `ObjectRef` result surfaces remain explicit projection boundaries.

`flpdf-mkyw` replaces the Rust recursion in the page-tree repair, inherited-attribute,
and tree-rebuild walks with explicit heap frames. This is category (B): qpdf's
child order, global visited/seen behavior, repair/mutation order, inherited-key
stack push/pop, and existing explicit max-depth API are unchanged; only the
Rust call-stack container changes. The deep-tree regression asserts that qpdf
11.9.0 and flpdf produce identical `--check` and `--json=2` stdout and identical
`--pages . 1-z --` bytes on a 2000-level tree.

The byte-identical claim holds for every tree qpdf itself can walk. Past that
depth qpdf has no observable output to match: measured with 11.9.0, a linear
2000-level tree succeeds for both, while a 50000-level tree makes qpdf itself
die of a stack overflow (SIGSEGV) where the heap-frame walk still completes.
Memory then grows linearly with depth instead of exhausting the thread stack.

| `QPDFExc.cc` / `QPDFSystemError.cc` | 123 | `error.rs`(125) | ✅ |

`.27.1` adds the public `QpdfExc` / `QpdfErrorCode` primitive in `error.rs`, mirroring
qpdf's independent error code, raw filename/object/message fields, signed file position,
and `createWhat` formatting. `QpdfExc::what_bytes()` intentionally exposes the observable
NUL-terminated `what()` bytes; getters retain complete fields. Resolver/diagnostic consumer
cutover and removal of duplicate formatters remain in the follow-up `.48.27` layers.

⚪ **(B) `Error` carries qpdf's exception-class axis as variants** (route matrix B32,
`docs/qpdf-route-matrix/b-parser-recovery-diagnostics.md`): qpdf classifies a raised
error along two independent axes — which C++ exception class carries it (`QPDFExc`,
or a bare `std::logic_error`/`std::runtime_error`/`std::range_error`; B32 cites concrete
throw sites, e.g. `libqpdf/QPDF.cc:481,1231`, `libqpdf/QPDFParser.cc:163`,
`libqpdf/QPDFTokenizer.cc:241,248,770`, `libqpdf/QPDFLogger.cc:200,252`, and
`libqpdf/QPDF.cc:1101` for `std::range_error`) and, only for `QPDFExc`, its independent
`qpdf_error_code_e` (`include/qpdf/Constants.h:84-95`). `crates/flpdf/src/error.rs::Error`
carries axis 1 as variants instead of mirroring the C++ class hierarchy. **Axis 2 is not
folded away**: `Error::QpdfExc` wraps `QpdfExc`, which stores `error_code: QpdfErrorCode`
and exposes it through `QpdfExc::get_error_code` (`crates/flpdf/src/error.rs:116,148`), so
production callers match on both layers exactly as qpdf's own callers do — only the legacy
variants that predate `QpdfExc` carry their classification in the variant alone. The code
takes no part in rendering: `QpdfExc::new` passes only filename, object, offset, and message
to `create_what` (`crates/flpdf/src/error.rs:133-136`), matching `QPDFExc::createWhat`, so
two otherwise identical exceptions with different codes render identically — as in qpdf.
The module documentation (`crates/flpdf/src/error.rs`, the `//!` header) records this
deviation as CLAUDE.md category (B) condition 3 requires, and
`Internal` is the 1:1 projection of axis 1 onto `std::logic_error`, and `System` together with its byte-preserving counterpart `SystemBytes` (`crates/flpdf/src/error.rs:246-247,321`) onto `std::runtime_error` (`crates/flpdf/src/error.rs:223-224`). This satisfies CLAUDE.md
deviation category (B): the fold changes only the Rust "container" a caller matches on —
where qpdf's own callers instead dispatch by C++ class and by `getErrorCode()` — not the
classification's meaning. Condition 1 (no output-byte impact): this substitution reaches no file bytes at all, so
there is no route for a `qpdf-zlib-compat` gated file-byte test to cover. `Error` values are
returned to a caller or rendered as diagnostic text; the one qpdf-observable surface is that
text, and `crates/flpdf/src/error.rs::qpdf_exc_what_matches_qpdf_c_string_boundaries_and_display_projection`
(`:532`, a plain `#[test]` — it needs no feature gate because no DEFLATE backend is involved)
pins `what_bytes` byte-for-byte across the filename/object/offset/message boundaries,
embedded NULs included. Naming a gated file-byte test here would be naming one that does not
and cannot exercise this boundary. Unlike
a serialization-layer substitution (e.g. `InputSource`/`Pipeline`) where the substituted
container sits on the write path: `Error` values are never themselves written into a PDF
file, only returned to a caller or rendered as that diagnostic text, so the container shape
carries no *additional* byte risk beyond whether each call site is assigned the same
classification qpdf's two axes would assign it — and that per-call-site accuracy is
exactly the open question B32's `mixed` status already tracks (see below), not something
this container-design note independently proves. Where a variant does render
qpdf-observable text, the byte-identical rendering is owned by `QpdfExc::what_bytes`/
`create_what` (B30 above, already qpdf-verified byte-for-byte), which takes no error code
at all: `QpdfExc::new` passes only filename, object, offset and message
(`crates/flpdf/src/error.rs:133-136`), matching `QPDFExc::createWhat`, so neither the
stored `QpdfErrorCode` nor which `Error` variant wraps it reaches the rendered text. This entry is the
`docs/qpdf-correspondence.md` half of condition 3's required two-location record; the
module-doc half is the deviation statement at `crates/flpdf/src/error.rs` の `//!` ヘッダ. It does
not change B32's `mixed` classification or resolve its open correctness questions: whether
each existing per-code call site (`Parse` for `qpdf_e_damaged_pdf`, `Pages` for
`qpdf_e_pages`, etc.) is populated the way qpdf's `qpdf_error_code_e` would assign it, and
whether the reconstruction-trigger guard at `crates/flpdf/src/reader/resolver.rs:2062-2079`
matches qpdf's `catch (QPDFExc&)` (`libqpdf/QPDF.cc:1614`) for every producer, remain open.

`flpdf-15qk` completes the `QPDF_pages.cc` cache boundary: `Pdf::page_list_cache`
stores the prepared root and ordered leaf identities after the canonical repair
walk, `PageDocumentHelper` consumers reuse it across JSON sections, and
`Pdf::update_all_pages_cache` plus page-tree rebuild/clear boundaries mirror
`QPDF::updateAllPagesCache` and the mutation-owned cache invalidation contract
(`QPDF_pages.cc:141-150`; `QPDF.hh:671-704`).

`flpdf-ymuj.6.38` extends that existing cache to the linearization and ObjStm
planning consumers. `pages::page_refs` returns the prepared non-empty sequence
when the cache is valid; an unprepared document and qpdf's empty
`m->all_pages` sentinel retain the bounded `PageWalk` fallback. When the
linearization content-normalization probe is active, it first obtains the page
sequence through the same `initializeSpecialStreams`-ordered preparation;
writer-side QDF/decode triggers already seed the cache in
`initialize_special_streams`. This preserves qpdf's direct-outline and
`optimize` call order (`QPDFWriter.cc:1911-1935,2114-2116`). The linearized
page-dictionary filter likewise obtains its sequence through the repair/cache
boundary after object-stream setup (`QPDFWriter.cc:2125-2149`). Internal
optimization, linearization, and tree-rebuild consumers now retain the prepared
`ObjectHandle` sequence rather than re-projecting and re-looking up each page;
the public `pages::page_refs` / `PageDocumentHelper::get_all_pages` surfaces
still expose their owned `Vec<ObjectRef>` contract. No second page-tree
traversal is performed, and
`update_all_pages_cache`/tree-rebuild/page-splice invalidation remains authoritative
(`QPDF_pages.cc:39-75,141-150`).

`QPDF::removePage` first delegates membership lookup to `findPage`, which
flattens the page tree and throws a `qpdf_e_pages` `QPDFExc` for a non-member
page. Its exception uses the input source name, the `page object` description,
zero offset, and `page object not referenced in /Pages tree`
(`QPDF_pages.cc:254-316`); `QPDFPageDocumentHelper::removePage` is a direct
delegation (`QPDFPageDocumentHelper.cc:50-52`). `test_driver.cc:862-872`
removes the same page twice, so `page_api_1.out2` records
`page_api_1.pdf (page object: object 4 0): page object not referenced in /Pages tree`
with exit status 2. flpdf's `PageDocumentHelper::remove_page` now raises the
canonical `Error::Pages` with that complete `QPDFExc::what()` text from the
resolver's source description; the qtest driver propagates it unchanged.

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
判定は writer の emission boundary に委ね、multi-source merge の保護参照コピーも
明示的な削除 sweep を追加せず writer の同じ boundary へ渡す。共有 `/XObject` category、
継承 `/Resources`、非対象 resource category、重複ページの差分回帰は
`crates/flpdf-cli/tests/cli_tests.rs` が qpdf 11.9.0 と比較する。

| `QPDF::resolve` / `QPDF::resolveObjectsInStream`（xref object-read/cache boundary） | `QPDF.cc:1700-1857`; `QPDF.cc:1541-1697` | `engine.rs` が parse 前に作る `ResolverHandle` を `xref.rs::CanonicalTrailerOwner` として渡し、active xref stream、hybrid `/XRefStm`、`/Prev` chain、reconstruction candidate の object read を `ResolverHandle::resolve_at_offset_with_optional_description`（live `readObjectAtOffset` → `readObject` → `readStream`）へ統一する。`/Type`/`/W`/`/Index`/`/Size`/filter は `XrefObjectContext` から同じ canonical handle/cache と warning snapshot を参照する。`.48.14`でbootstrap ObjStmの specialized decode順序をqpdf責務へ揃え、`.48.15.1`でcanonical recovery candidateも live ownerから直接 trailer/object handleを生成して `LoadedXrefState` handoff後のrebind/second teardownを無くした。`.48.72`でowner-less public loader/exportとproduction callerを撤去し、残るBootstrapHandleState/bounded reconstruction windowはtest-only scaffoldingとして隔離した。 | 🔀 `.48.13` / `.48.15.1` / `.48.72` / `.48.73` で canonical production xref-stream/read, canonical handoff, public route撤去, warning live deliveryを完了。残るtest-only bootstrap reconstructionはqpdfのproduction document ownerを迂回しない |

### Persistent null after a resolution loop (`flpdf-64dx9`, 2026-09-17)

qpdf 11.9.0 の `QPDF::resolve` は、`isUnresolved` を確認してから
`m->resolving` の再入を検出する。loop 時は warning の後に同じ `obj_cache` slotを
`QPDF_Null` へ更新して return する（`QPDF.cc:1699-1714`）。外側の
`readObjectAtOffset` は parse 後にも `isUnresolved(og)` を確認し、既に loop結果が
cacheされていれば parsed value で上書きしない（`QPDF.cc:1639-1697`）。一方、
`resolveObjectsInStream` の member loopは `updateCache` を無条件に呼ぶ
（`QPDF.cc:1755-1833`）。この違いを uncompressed object read と ObjStm member
更新の境界として保持する。

flpdf は `ResolverHandle::cache_parsed_object_if_unresolved` を type-1/raw xrefの
parse結果へ適用し、`BootstrapHandleDocument::resolve_indirect_inner` も同じ
canonical handleが loop中に解決済みなら parsed value と offsetを適用しない。
これにより self-referential stream `/Length` は qpdf と同じ恒久 `null` になり、
stream payload recovery の warning sequence は保持される。ObjStm member側の
無条件更新は変更しない。`indirect_length_adjacent_endstream_tests.rs` と
`flpdf-cli/tests/cmp_issue_117_loop_tests.rs` が cache value、diagnostics、linearized
outputを qpdf 11.9.0 と比較する。

`flpdf-na1b` では、qpdf の raw `m->xref_table` walk (`QPDF.cc:1239-1254`) を
valid `ObjectRef` view と分けたまま canonical `ResolverCore` の解決境界へ接続する。
`QpdfObjGen` を expected identity として `readObjectAtOffset` 相当へ渡すため、
generation 65536 の in-use row も実体との mismatch、reconstruction、missing-after-
recovery warning の qpdf 順序を保持する。valid ObjGen は既存 canonical handle/cacheへ
変換し、範囲外の raw row は ObjectRef を捏造せず raw resolution boundary で消費する。
これにより `--check` の raw generation 診断を qpdf と一致させる
(`QPDF.cc:1580-1632`)。

`.48.73` では、canonical `Pdf::open` の xref/recovery warningを `ResolverHandle::push_qpdf_warning`へ qpdfのcall orderで直接配送し、engineのinstall/replayと`DeferredDiagnosticsGuard`を撤去した。`.48.72`でpublic owner-less loaderとproduction callerを撤去し、残っていたBootstrapHandleState、bounded reconstruction window、detach/drop helperというtest-only scaffoldingは `.48.151` で機構ごと削除した（下記）。

`flpdf-3yn9.48.72` では、owner-less public `load_xref_and_trailer*`/`LoadedXref`
surfaceを削除し、in-tree inspection callersを `Pdf::open` + `get_xref_table` /
`trailer`へ移行した。qpdfに対応する独立 xref loaderは無く、read_xrefはprivate
である（`include/qpdf/QPDF.hh:77-97,306-315,1000-1013`）。Bootstrap cache/
detachは owner-less test scaffolding として残る bounded reconstruction testsを除き、
production caller 0 を確認した。canonical warning sinkの責務は `.48.73` で完了済みで、
その test-only scaffolding 自体は `.48.151` で削除した。

`flpdf-3yn9.48.151` では、owner-less bootstrap xref parser を撤去した。xref
loading の各関数が持っていた `Option<&dyn CanonicalTrailerOwner>` を
`&dyn CanonicalTrailerOwner` に変え、`if let Some(owner) { canonical } else
{ bootstrap }` の分岐を canonical 側だけに畳んだ結果、第 2 の PDF object
parser 一式（`BootstrapHandleState` / `BootstrapCache` /
`BootstrapHandleDocument` / `BootstrapHandleParser` / `XrefHandleCache` /
`XrefReadContext` / `XrefReadContextSpec` / `XrefDetachedHandles` と、
それを支えていた `crates/flpdf/src/reader/file_object.rs` 全体）が到達不能に
なり削除できた。qpdf は `QPDF::processInputSource`（`QPDF.cc:245-275`）以降
`m->file` と `obj_cache` を 1 つずつしか持たないので、flpdf 側も 1 つになる。

同時に消えた qpdf 非対応の挙動:

* `RecoveryPolicy`（`RequireEndstream` / `Bounded`）— qpdf の
  `m->attempt_recovery` 1 bit（`QPDF.cc:1391`）を 2 値の別概念へ翻訳していた
  named deviation。
* reconstruction 候補の bounded reference-read window（`flpdf-qwh0`）と
  bootstrap recursive hub の `stacker::maybe_grow`（`flpdf-ag95`）。両方とも
  `xref.rs` の `qpdf-deviation` marker で記録していたもので、マーカーごと
  撤去した。
* `startxref == 0` の retry-at-offset-0 detour（上記の逸脱候補表の行）。
* bootstrap 側 ObjStm 展開の 4 箇所の未マーク逸脱 — `as_stream_dict()` 失敗、
  `/N`・`/First` 非整数、メンバ parse error で `Err` を伝播し、既定 xref 行の
  warning を出さなかった点。qpdf はいずれも warn 側で処理する
  （`QPDF.cc:1760-1795,2617-2644`）。**これらはカバレッジを失ったのではなく、
  qpdf 非対応の挙動が消えた**。

撤去に先立ち `flpdf-3yn9.48.151.2` で bootstrap ObjStm test の 1:1 等価監査を
行い、canonical `ResolverHandle::resolve_object_stream_with_failure_kind` 側に
存在しなかった 3 挙動（重複キー warning の raw bytes 保持、回復可能な decode
warning 後のメンバ保持、メンバ内 direct 値への description 継承）は canonical
側の test として先に追加してある。

`flpdf-8q38` では、65個の行頭偽 object headerを候補 xref streamの余剰payloadへ置いた
fixtureを qpdf 11.9.0 と canonical `Pdf::open` へ入力した。qpdfのEOFまでの候補read
（`QPDF.cc:577-608,1542-1697`）と同様に、canonical routeは候補 `1000/0` と size warning
を保持した。production `engine.rs` の xref loader callerは canonical ownerを必ず渡し、
ownerless loaderは `#[cfg(test)]` のみなので、bounded candidate/reference windowは
通常openのbridgeではない。残るtest-only scaffoldingはこのissueでは変更せず、qpdfに対応物
のない性能hardeningは `flpdf-qwh0` で別途扱う。

`flpdf-1f9f` では、owner-less bootstrap の ObjStm member parser にも member の description
context を渡すようにした。そのため member 本体だけでなく、辞書・配列内の nested direct value も
同じ `ObjectDescription::Template` を持つ。qpdf は member の警告を 3 つの断片から組み立てる —
decoded InputSource 名（`<file> object stream N`、`libqpdf/QPDF.cc:1793-1805`）、parser に渡す
`object M 0` description（`:1451-1459`）、parsed offset — を `QPDFParser::warn` が
`QPDFExc` に束ねる（`libqpdf/QPDFParser.cc:509-513`）。flpdf の description template は
その**レンダリング済み prefix 全体**を保持し、`$PO` が offset のプレースホルダになる
（`crates/flpdf/src/object_handle.rs:940`）ため、template には入力 description と
`object stream N` を含める。これは canonical reader の
`reader/resolver.rs::object_stream_description_template` と同形。対象はdescriptionの
伝播だけで、specialized decode、header map、effective xref、bounded reconstruction ownerは
`.48.14`の責務を変更しない。

`flpdf-x8bje` では残っていた bootstrap parser の per-call template 再生成と
canonical `ParsedObjectAtOffset` の `Rc`→`Vec` 再コピーを除去した。bootstrap の
各 parser invocation は一つの `Rc<Vec<u8>>` を保持し、canonical cache は同じ owner を
`set_shared_description` へ移送する。JSON/Child description variant、`$PO`/`$OG` の
rendering、parsed offset、warning/output contract は変更しない。

`flpdf-92r5` では、owner-less bootstrap の `BootstrapHandleDocument` も
qpdf の `QPDF_Stream::warn`（`libqpdf/QPDF_Stream.cc:695-698`）から
`QPDF::warn`（`libqpdf/QPDF.cc:487-494`）へ渡る stream warningを、parsed
offsetと入力descriptionを保持したままstate diagnosticsへ追加するようにした。
`ObjectHandle::stream_data_warning`（`crates/flpdf/src/object_handle.rs:6650-6685`）の
parsed offsetなしの `object_warning` fallbackも同じBootstrap warning sinkへ流れるため、
recoverableなFlate warningでObjStm memberのdecodeを中断しない。canonical
`ResolverHandle` route、reconstruction-only bounded window、qtest exceptionsの
責務は変更しない。

`flpdf-buy0` では、qpdfの `m->warnings` 単一sink（`libqpdf/QPDF.cc:487-494`）に
合わせ、classic trailerの復旧warningと同じhopのhybrid `/XRefStm` read warningを
呼出順に並べるようにした。canonical ownerのdeferred live warningをhop全体の前に
一括spliceせず、classic sectionではbuffered trailer diagnosticsを先に、hybrid
readのlive diagnosticsを後に配置する。合成PDFをqpdf 11.9.0と比較し、
`stream keyword found in trailer` → `stream filter type is not name or array`
の順序をRED/GREENで固定した。

`flpdf-qwh0` では、qpdf 11.9.0 が `reconstruct_xref` の候補ごとに参照 object を offset から EOF まで読む (`QPDF.cc:585-589,1542-1697`) のに対し、flpdf の reconstruction bootstrap contextだけは line-scan で既知になった次の uncompressed offsetまで参照先 readを制限していた。qpdfに対応物のないflpdf固有の malformed-input 性能/DoS hardeningであり、`xref.rs` の `qpdf-deviation` markerに記録していた。**2026-09-18（`flpdf-3yn9.48.151`）にこの bounded read window は bootstrap 機構ごと削除し、マーカーも撤去した。** 候補探索は canonical owner の unbounded な `readObjectAtOffset` を通るようになり、qpdf と同じ scan 形になっている（同一 fixture 形状での実測: qpdf 11.9.0 は候補 500 件で 0.03 秒、5,000 件で 2.09 秒と、qpdf 自身が二次的に伸びる）。

`flpdf-ag95` では、qpdf 11.9.0 の `QPDF::resolve` が `m->resolving` による cycle 検出だけを行い、indirect-reference chain の深さ上限を持たない (`QPDF.cc:1699-1753`) ことに合わせ、bootstrap の recursive hub を `stacker::maybe_grow` で実行していた。qpdfに対応物のないRust側stack policyとして `xref.rs` の `qpdf-deviation` markerに記録していたが、**2026-09-18（`flpdf-3yn9.48.151`）に bootstrap 機構ごと削除した**。canonical resolver 側の indirect chain 解決は `crates/flpdf/src/reader/resolver.rs` が担う。

## 3. 書き込み — 最大の smear

### Multi-source `--pages` source-backed Preserve ObjStm handoff (`flpdf-clq9`, 2026-09-07)

qpdf keeps the primary `QPDF` in place during `QPDFJob::handlePageSpecs`, so
`QPDFWriter::preserveObjectStreams` can retain the primary xref type-2
member-to-container mapping and rebuild the selected source container
(`QPDFJob.cc:2360-2632`; `QPDF.cc:2019-2213`; `QPDFWriter.cc:1939-1967,
1621-1740`). flpdf's canonical multi-source merge necessarily creates a fresh
target, so `job/page_merge.rs` installs target-owned source-container
placeholders, preserves indirect `/Extends`, and records copied primary members
as type-2 source rows before the existing plain Preserve planner runs. Foreign
page graphs remain on `object_copy.rs::copy_foreign_object`; the source
container body, `/N`, `/First`, and filters remain writer-owned and are rebuilt
by `writer/plain/body.rs`. qpdf-zlib differential tests cover the default and
`--preserve-unreferenced` page-merge paths, including an indirect `/Extends`
chain.

### Multi-source `--pages` primary inherited attribute handoff (`flpdf-k4bp`, 2026-09-10)

qpdf's page-selection pipeline preserves the primary document while foreign
pages are inserted, and its page-optimization pass pushes `/MediaBox`,
`/CropBox`, `/Resources`, and `/Rotate` from `/Pages` nodes to leaves. Direct
non-scalar values are promoted once to shared indirect objects and removed from
their source `/Pages` dictionaries (`QPDF_optimization.cc:121-228`;
`QPDF_pages.cc:154-274`). The fresh-target flpdf merge already used this
canonical source preparation for secondary inputs but skipped the primary before
copying its selected pages. The primary now uses the same
`push_inherited_attributes_to_pages` boundary; no second inheritance model or
legacy bridge is introduced. The qpdf differential covers the resulting
indirect `/MediaBox`, page geometry, warning stream, and output bytes.

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFWriter.cc` | 3044 | `writer/write_object.rs::WriteObject` が `QPDFWriter.cc:1036-1054,1761-1809` の共通 writeObject 制御順序と採番済み ID の open/close を所有し、plain の通常 object と source-backed container の入口を接続する。Rust 内部 trait は C++ writer の member access を各 consumer の借用 state で置換する byte-neutral な足場（分類B）であり、progress 前に body を生成しない。生成 container、旧 ObjStm member helper、QDF/normalize specialized、linearized は残 consumer。PCLm と specialized standard は shared live consumerへ移行済み。`writer.rs`(4494) + `writer/serialize.rs`(1008) + `writer/object_streams/{eligibility,planning,emission}.rs`(739) + `writer/encryption_state.rs`(258) + `writer/encrypted_strings.rs`(213) + `writer/plain/{plan,body,xref}.rs`(898) + `linearization/writer.rs`(3603) + `linearization/part1.rs`(370) + `linearization/back_patch.rs`(324) + `linearization/renumber.rs`(850) + `writer/rewrite_renumber.rs`(893) = **13,650 行 / 13 ファイル**。加えて `object.rs`(650: `write_pdf` = `unparseObject` / 768: `write_pdf_qdf` / 910-: trailer `/ID` = `writeTrailer`。`writer.rs` と `linearization/writer.rs` が委譲) と `writer/object.rs::visible_dict_entries`（canonical `ObjectHandle` 境界） + `writer/rewrite_renumber.rs::visible_raw_dict_entries`（raw `Object` 境界） = `QPDFWriter.cc:1490-1491` の null 値 dict キー抑制。さらに `writer/object.rs` に qpdf の writer-owned live-handle emission trait/walkers（`unparseObject`/`unparseChild`/`writeTrailer`、`QPDFWriter.cc:1072-1810,2236-2376,2907-3035`）を集約し、`object_handle.rs` は graph identity・payload・mutation・JSON の責務だけを保持する。旧 handle-owned emission implementationは削除し、production/test caller は writer boundary の canonical surface を直接利用する。`write_stream_body_qdf` は最終レビューで見つかったギャップの修正（Task 9）: `write_pdf_stream_qdf`(`object.rs:1036`、real production callsite は `writer.rs:4437`)に対応する QDF+stream 形の primitive が欠けていた。`Dictionary::write_pdf_stream_qdf` 自身に `refiltered` 概念が無いため（唯一の呼び出し元 `write_stream_to_buf_qdf` は既に確定済みの `/Filter`/`/Length` を持つ dict しか渡さない）、`write_stream_body`（compact 版）と異なりこちらも `refiltered` パラメータを持たない。null 値 dict キー抑制(`:1490-1491`)は `try_is_null` 経由で `write_object`/`write_object_qdf`/`write_stream_body`/`write_stream_body_qdf` の4つに適用し、`write_trailer` は `writeTrailer` 自身と同様に無抑制。`writer/encryption_state.rs` の `WriterEncryptionState` は `QPDFWriter::Members` の暗号 state (`QPDFWriter.hh:641-663`)、`set_data_key` は `setDataKey` (`QPDFWriter.cc:842-847`) と `compute_data_key` (`QPDF_encryption.cc:325-356`)、`with_object_data_key` は非 ObjStm member の set/unparse/clear (`QPDFWriter.cc:1761-1796`) に対応する。source ID ではなく emitted ID と generation 0 を使い、`Option<u32>` が qpdf の `-1` sentinel を置換する。qpdf の明示 clear は正常系だけだが、Rust callback の `Err` 後にも clear するのは出力 byte を変えず stale state を残さない内部代替である。全て `pub(crate)`・`#[allow(dead_code)]`。`flpdf-a32l` は AES で暗号化済みの文字列を full / linearized writer の共通 serializer context で強制 hex 化し、RC4・非暗号化・ObjStm member は既存の heuristic を維持する（`QPDFWriter.cc:1567-1592`）。既存 primitive の production consumer 移行（`flpdf-egzr.3.2.5` + 子 `.5.1`〜`.5.4`）と暗号 state の consumer 移行（`flpdf-3yn9.11`/`.12`）はいずれも close 済みで、`PlAesPdf`/`PlRc4`/`run_writer_pipeline`/`adjust_aes_stream_length` は production コードで実使用されている。`flpdf-25kg.3.48.4` では、qpdf `QPDFWriter.cc:1072-1157,1334-1360,1488-1505,2907-2925` の writer-owned live-handle boundary に合わせ、full-rewrite Catalog の output-only `/Extensions` 復元を `CatalogExtensionsSnapshot`/`restore_catalog_extensions` に統合し、linearization の pass-1/final `/ID` 構築を `generate_id_handle` と `ObjectHandle` へ移した。`QPDFObjectHandle.cc:1575-1642` の unparse 契約、`QPDF_Stream.cc:571-620,640-685` の stream/provider 契約は既存の canonical emission surface として利用する。writer donor-copy の raw `Object` 境界は後続 slice に残す。既存 primitive の production consumer 移行（`flpdf-egzr.3.2.5` + 子 `.5.1`〜`.5.4`）と暗号 state の consumer 移行（`flpdf-3yn9.11`/`.12`）はいずれも close 済みで、`PlAesPdf`/`PlRc4`/`run_writer_pipeline`/`adjust_aes_stream_length` は production コードで実使用されている。🔀 の根拠は ObjectHandle 移行の未完了ではなく、下記の「xref 出力が 3 箇所に分かれる」構造的 smear が独立に残っていること | 🔀 |
| `QPDFJob.cc:2833-2925` / `QPDFWriter.cc:217-265,1356-1435,2176-2182` | qpdf job version-spec parsing, writer version/extension pair selection, and Catalog `/Extensions /ADBE` reconciliation | `pdf_version.rs::parse_pdf_version_spec` + `flpdf-cli/src/main.rs::CliVersionOptions`/`parse_cli_version_options`/`apply_cli_version_options` + `writer.rs::effective_pdf_version_and_ext`/`inject_adbe_extension`/`strip_adbe_extension` | ✅ qtest `extensions-dictionary.test` 156/156 (qpdf 11.9.0; `test_driver 34` and force-1.8.5 QDF/non-QDF checks); malformed raw option values follow `QPDFJob::parse_version` and `QPDFWriter::parseVersion` rather than strict `M.m[.E]` validation |

### Linearization does not impose an additional PDF-version floor (`flpdf-043td`, 2026-09-17)

`QPDFWriter::setLinearization` only enables the linearized writer mode
(`libqpdf/QPDFWriter.cc:313-318`). In `doWriteSetup`, qpdf raises the minimum
version for emitted object streams and combines the input, encryption, and
explicit version floors (`libqpdf/QPDFWriter.cc:2059-2184`), but it has no
linearization-specific `1.2` contribution. The same final version is emitted
by `writeHeader` in both passes (`libqpdf/QPDFWriter.cc:2258-2279`). The flpdf
linearization consumer therefore uses the shared source/min/object-stream/
encryption version selection without passing a synthetic linearization floor.
The low-version qpdf differential regression covers `%PDF-1.0` and `%PDF-1.1`
inputs and full output bytes.

qpdf keeps `deterministic_id` and `static_id` as independent writer settings. `QPDFWriter::generateID`
checks `static_id` before entering the deterministic digest branch, so both options together are
accepted and produce the same identifier as `--static-id` alone (`libqpdf/QPDFJob.cc:2879-2882`,
`libqpdf/QPDFWriter.cc:1836-1878`). flpdf's `uses_deterministic_id` applies the same effective
selection in the plain, PCLm, and linearized writer routes. The deterministic setting remains
active for encryption preflight, so deterministic-plus-encryption is still rejected when
`static_id` is present, but with qpdf's other logic error: the static `generateID` succeeds during
the encryption setup, and `pushMD5Pipeline` (`libqpdf/QPDFWriter.cc:1011-1014`) then fails with
"Deterministic ID computation enabled after ID generation has already occurred." instead of the
`generateID has no data` message (`deterministic_id_encryption_error`).

`parse_pdf_version` remains the strict `PDFVersion` value parser for validated input headers. Job options use the separate qpdf-shaped raw parser: a trailing dot remains part of the header, extra components after the second dot are consumed only as the extension-level integer prefix, and non-numeric raw version text is retained. `QUtil::string_to_int` overflow is a range error, not a clamp (`QPDFJob.cc:2833-2844`, `QPDFWriter.cc:744-757`, `QUtil.cc:371-393`).

`QPDFWriter::getTrimmedTrailer` (`QPDFWriter.cc:2009-2031`) returns an

The byte-oriented job-JSON entry point keeps raw bytes through JSON parsing, but the `minVersion` and `forceVersion` fields are rejected when their bytes are not valid UTF-8 because the writer's raw-version storage is a Rust `String`; this avoids silently replacing qpdf-carried bytes with U+FFFD.

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

The D14 plain-classic slice now keeps the live trimmed trailer handle in
`writer/plain/plan.rs::PlainWritePlan::trailer_handle` and sends classic
trailer bytes through `writer/object.rs::TrailerKind` and
`ObjectWriterEmission::write_trailer_with_ref_map_and_kind`. The xref row and
`startxref` framing remain in `writer/plain/xref.rs`; specialized,
legacy xref-stream, and linearized trailer callers remain follow-up route
consumers rather than new semantic trailer implementations.

### Missing literal `/Size` remains absent on normal writer routes (`flpdf-5jcwv`, 2026-09-17)

qpdf's `getTrimmedTrailer` removes only writer-owned history and xref-stream
keys; it does not add `/Size` (`libqpdf/QPDFWriter.cc:2009-2031`). The normal
`writeTrailer` loop replaces the value only when `getKeys()` contains the
literal `/Size` key (`libqpdf/QPDFWriter.cc:1160-1236`). `t_lin_second` is a
separate linearization form that writes `/Size` unconditionally
(`QPDFWriter.cc:1170-1172`).

The canonical flpdf plain and PCLm trailer builder now preserves a missing or
misspelled `/Size` key while replacing an existing literal key. The plain
xref-stream live-trailer serializer applies the same predicate. The existing
linearized first-page route already follows the qpdf predicate; its main
second-half trailer remains an independently generated `/Size`-only form.
The synthetic bad9 differential covers normal rewrite bytes and generated
xref-stream key visibility against qpdf 11.9.0.

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
`Pl_Flate::setCompressionLevel` (`Pl_Flate.cc:221-224`) 自体は範囲検証せず、
zlibの `deflateInit` 失敗を `QPDF::pipeStreamData` (`QPDF.cc:2477-2538`) が
stream単位のwarningへ変換して `QPDFWriter` (`QPDFWriter.cc:1287-1314`) の
unfiltered retryへ渡す。flpdfも同じlazy failure/retry境界を使い、範囲外levelで
write全体を使用法エラーにしない。

⚪ **DEFLATE/INFLATE バックエンドの選択**（CLAUDE.md 逸脱分類 (A) と、その
inflate 側の内部的な対応物）。`pipeline/flate.rs` は 2 つの codec を feature で
切り替える。

- **deflate（圧縮）**: 既定は `flate2` + miniz_oxide（Pure Rust）、
  `qpdf-zlib-compat` で `flate2` + 古典 libz。qpdf は常に zlib なので、
  既定ビルドでは圧縮バイトが qpdf と異なってよい。これが CLAUDE.md が認める
  **唯一の出力バイトを変える逸脱 (A)** で、byte-identical の検証は
  `--features qpdf-zlib-compat` で行う。
- **inflate（伸長）**: 既定は `zlib-rs`（Pure Rust の zlib 移植）を直接使い、
  `qpdf-zlib-compat` では `flate2::Decompress`（libz）。**伸長は決定的**なので、
  正常なストリームに対しては両者とも元のバイト列を復元し、**出力バイトは
  変わらない**。(A) の圧縮側とは別物である点に注意する。

破損ストリームの診断文言は qpdf の `strm.msg` に合わせてある。
`qpdf --check qtest/qpdf/fuzz-16214.pdf` は
`stream inflate: inflate: data: invalid code lengths set` を出し、flpdf は
既定ビルドで `zlib_rs::Inflate::error_message()`、compat ビルドで
`flate2` の `error.message()` から同じ文言を取る。

`Pl_Flate` 自体のロジック・呼び出し順序・warning 境界は qpdf のまま
（上記の `setCompressionLevel` / lazy failure retry を参照）で、
置き換えているのは codec の実体だけ。

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

### `writeXRefStream` layout owner (`flpdf-3yn9.48.58`, 2026-09-07)

qpdf 11.9.0 の `QPDFWriter::writeXRefStream`（`QPDFWriter.cc:2392-2495`）は、
`xref_id` を payload 作成前に xref map へ登録し、`f1 = max(bytesNeeded(max_offset +
hint_length), bytesNeeded(max_id))`、`f2 = bytesNeeded(max_ostream_index)`、
`esize = 1 + f1 + f2` を一度だけ決める。type 1 行の第 3 field は常に 0 で、type 2
だけが ObjStm member index を持つ。`skip_compression` は linearization の pass 1
だけに適用され、PNG predictor と `/FlateDecode` の辞書宣言を残したまま Flate
圧縮を省略する（`QPDFWriter.cc:2418-2432`）。

flpdf はこれを `writer/serialize.rs::xref_stream` の
`build_entries_with_self` と `encode_payload_for_policy` に集約した。plain writer、
linearized first-half、linearized second-half はこの owner から同じ self-entry、
field width、type 0/1/2 row、raw/predictor/Flate payload を受け取り、linearization
固有の `/Index`、`/Prev`、固定 region padding、`startxref` だけを consumer 側に残す。
plain 側で source generation を field 3 や width の計算へ流し込む旧経路は削除した。
`QPDFWriter::calculateXrefStreamPadding`（`:2498-2507`）に対応する region sizing は
既存の同じ serializer module が引き続き所有する。

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

### Non-linearized `writeStandard` final-sink/queue boundary (`flpdf-ymuj.4`, 2026-09-12)

qpdf 11.9.0 は `initializePipelineStack` の base `Pl_Count` を final target の直前に
置き（`QPDFWriter.cc:916-931`）、`writeStandard` が optional `Pl_MD5`、header、
standard/PCLm seed queue、live `writeObject` walk、xref、trailer/EOF の順にその active
pipeline へ書く（`:2991-3044`）。`PipelinePopper` は stream/filter 専用ではなく、
`willFilterStream` の `Pl_Buffer`、stream/object-stream/xref/hint の encryption または
buffer scope、linearization pass の discard scope、そして deterministic-ID の `Pl_MD5`
scopeを push/pop する共通の nested pipeline-stack lifecycle である
（`:935-971,965-1034,1288-1292,1553-1558,1639-1655,2433-2437,2667-2675,2875-2880`）。
その destructor は active nested `Pl_Count` を finish して nested stages を pop し、必要なら
buffer を返すが、qpdf の base final target を nested popper が finish するという意味ではない。
deterministic-ID では `computeDeterministicIDData` が ID cutoff で digest を取得して MD5 を
disable し（`:1027-1033,1213-1217`）、`writeStandard` が ID と EOF を書いた後に
`pp_md5` を解放してその MD5 scope を閉じる（`:3036-3044`）。これは stream payload の
`Pl_Buffer`/encryption segment scopeとは別の、文書全体の ID-digest scopeである。
文書全体の base `m->pipeline->finish()` は `write()` が `writeStandard`/`writeLinearized` の
後に行う（`:933-956,2187-2205`）。

flpdf の non-linearized `PdfWriter` は `WriterOutputSink` を final `OutputTarget` とし、
plain/QDF/Preserve/Generate/暗号化と PCLm を**同じ一つの** `writer::output::OutputSink`
へ渡す。`OutputSink` は final target が実際に受理した byte だけを checked `u64`
position と MD5 へ反映する。従って object と xref の位置は final sink の座標であり、
short write・`Interrupted`・`WriteZero` もこの境界で処理する。`finish_segment` は stream
payload の境界で呼ばれるが、その意味は qpdf の nested `PipelinePopper` と同一ではなく、
Rust の `OutputTarget` adapter に依存する。`WriterOutput::Pipeline` target は設定された
final pipeline の `finish()` を segment boundary で呼ぶことがあり、`WriterOutput::Writer`
は flush、Memory は no-op である。`finish_document` は全 PDF の emission が EOF まで成功
した後に呼ばれ、Pipeline adapter では `finish_output` が configured final pipeline を
finalize する。この adapter lifecycle distinction は qpdf の nested popper が base target を
finish するという対応付けではない（`writer/output.rs`; `writer.rs::WriterOutputSink`;
`writer.rs::PdfWriter::write`）。

stream payload、ObjStm member/pair body、xref-stream encoded payload は `/Length`、`/First`、
member pair offset、xref encoding を決めるためだけの bounded local `Vec` である。
local sink の位置はその buffer 内の座標で、完成した payload を final `OutputSink` に消費した
時点だけ final 座標と digest が進む（`writer/output.rs::with_buffer_sink`、
`writer/serialize.rs`、`writer/plain/xref.rs`）。ObjStm body は一 container を書き終えると
drop され、complete document/body buffer や planned multi-stream payload cache は
non-linearized route に残さない。一方、linearization の pass buffer/back-patch region は
final pass だけが保持し、pass 1 は下記の direct sink route を使う。

### Linearized ObjStm payload ownership (`flpdf-ymuj.6.39`, 2026-09-16)

qpdf の `QPDFWriter::writeObjectStream` は pass 2 で ObjStm の pair/body を 1 本の
`std::shared_ptr<Buffer> stream_buffer` に受け、同じ buffer から `/Length`・暗号化後の
payload・最終 pipeline への書き出しを行う（`libqpdf/QPDFWriter.cc:1636-1750`）。
`PipelinePopper` が `Pl_Buffer` の shared pointer を回収するため、writer の
`stream_buffer` と sink の間で payload を深く複製しない
（`libqpdf/QPDFWriter.cc:881-884,925-965`、`include/qpdf/Pl_Buffer.hh:50-58`）。

flpdf の linearized ObjStm consumer も `writer/object_streams/emission.rs::wrap_objstm_body_as_handle`
で `ObjStmBody` の所有権を移し、非圧縮なら元の `Vec<u8>` を、圧縮なら一度だけ生成した
Flate の `Vec<u8>` を `Rc<Vec<u8>>` にする。`ObjectHandle` と linearization writer は同じ
`Rc` を共有し、後者はその payload を `/Length` 計算、暗号化 pipeline、または通常の
stream serializerへ渡す。従って従来の `data.clone()` による container handle と sink 用
payload の二重保持を除去し、出力 bytes・dictionary・暗号化境界は変えない。所有権共有は
`ObjectHandle::stream` / `as_stream_data` の既存 qpdf対応（`QPDF_Stream::stream_data` の
`shared_ptr<Buffer>`）を利用し、回帰は圧縮・非圧縮の両モードで同一 allocation を検査する。

### Linearized source-backed ObjStm `/Extends` preservation (`flpdf-eetrz`, 2026-09-17)

qpdf の `writeObjectStream` は、Preserveで元containerがnullでない場合にその辞書を参照し、
`/Extends` がindirectなら `unparseChild` でwriterの採番へ変換して`/First`の後へ複写する。
Generateのnull placeholderにはこのsource辞書がないため`/Extends`を生成しない
（`libqpdf/QPDFWriter.cc:1621-1758`、特に`:1730-1739`、`unparseChild`は`:1144-1162`）。

flpdf のlinearized canonical emitterは `RoutedObjStmBatch` のsource identityを
`ObjStmLayout`へ保持し、`append_objstm_container_object` がsource handleを一度だけ確認する。
indirect `/Extends`だけを既存の`RenumberMap`でoutput refへremapし、共有wrapperと固定辞書順の
emissionへ渡す。direct値、欠損値、Generateのsourceなしcontainerは出力しない。
`good17`系3 fixtureのqpdf 11.9.0 live probeと、手書きtype-2 xref chain回帰を
`qpdf-zlib-compat`で比較し、linearized outputの`/Extends`・`/L`/`/E`/`/T`を含むfull bytesを
一致させる。

### Linearized pass-1 ownership (`flpdf-ymuj.5`, 2026-09-13)

qpdf 11.9.0 の `QPDFWriter::writeLinearized` は pass 1 の開始時に、指定された
`lin_pass1_filename` なら `Pl_StdioFile`、それ以外なら `pushDiscardFilter` を pipeline
へ積み、deterministic ID 使用時だけ `Pl_MD5` を重ねる
（`QPDFWriter.cc:2656-2676`）。pass 1 は header、padding、first-half xref、body、main
xref をその場で forward-write し、完全な body を `Members` に保持しない。pass 1 の後に
保持するのは `file_size`、xref位置、hint offset/length と `Pl_Buffer` の hint bytes
だけであり、`QPDFWriter.hh:688-690` にも pass-1 body member は存在しない。hint buffer は
`QPDFWriter.cc:2872-2885` で作られ、debug file は `:2886-2900` で4つの offset/length
コメントを追記する。`--linearize-pass1` 自体は qpdf が「valid PDFではない debugging
file」と定義する option (`libqpdf/qpdf/auto_job_help.hh:1000-1005`) で、qtest は final file と
pass-1 file を別々に検査する (`qpdf/qtest/linearize-pass1.test:19-29`)。

flpdf は `Pass1OutputTarget` を discard または `BufWriter<File>` として構築し、既存の
`writer/output.rs::OutputSink` で accepted-byte position と deterministic-ID MD5 を一つの
forward boundaryへ統合する。`linearization/writer.rs::do_write_pass` は両 pass とも
`OutputSink`へ直接 emissionし、`LinearizedPassOutput` は xref/offset/length/range metadata
だけを返す。pass 1 の classic xref と ObjStm xref stream は qpdf と同じ zero/forward
representation をその場で書くため、seekable temporary PDFも pass-1 body Vecも不要である。
final pass は pass-1 のxref mapへhint object長を適用し、最終Part-1辞書、first-page xref、
`/Prev`、`/ID`、main xrefを先に確定してから、設定済み `Writer`/`Pipeline` の
`OutputTarget`へforward-writeする。従ってcanonical routeは完全なfinal PDF Vecを保持せず、
memory sinkを選んだ場合だけsink自身が契約どおり最終bytesを保持する。pass-1 artifactの
debug commentsはmetadataから追記し、body再走査による`startxref`探索やpass-1 body cloneを
行わない。`LinearizedDocument::back_patch` は既存のin-memory inspection/helper契約のため
残すが、canonical sink routeの出力後修正には使わない。

### Linearized xref offset-map ownership (`flpdf-ymuj.6.7`, 2026-09-14)

qpdf の `QPDFWriter::Members::xref` は writer-owned の単一 map
（`include/qpdf/QPDFWriter.hh:668-670`）であり、`writeXRefTable` と
`writeXRefStream` は同じ map を読む。linearization の hint 補正は row を出力する
境界で行う（`libqpdf/QPDFWriter.cc:2361-2373,2407-2462`）。

flpdf の canonical linearization route も、pass-1 の `xref_offsets` map を最終物理
offsetへ更新し、Part-1 metadata・first-page xref・main xref・WriterResult が同じ
ownerを借用する。first-page xref のvirtual座標は全map cloneではなく、boundedな
xref entry payloadを作る時だけ導出する。resolverのsource xref table
（`flpdf-ymuj.8`）とxref payload stage owner（closed `flpdf-3yn9.48.58`/
`flpdf-qynx.5.4.1`）は別責務である。

qpdf の standard writer は `enqueueObjectsStandard`（`QPDFWriter.cc:2907-2925`）で `/Root`
と trimmed trailer の seed を queue に積み、`unparseChild` が indirect child を書く直前に
同じ queue へ追加する（`:1072-1157`）。flpdf の `writer/plain/body.rs::LiveQueue` と
`LiveObjectEmitter` は Preserve/Generate membership setup を先に登録してから、実際の
object/stream dictionary emission で surviving indirect child を発見し、その場で採番・enqueue
する。PCLm も `writer/pclm.rs::Plan` の page/content/image/synthetic/root initial order だけを
保持し、`EmissionQueue::enqueue_handle` が body emission 中に child を発見する。したがって
順序・採番は事前の完全 child prewalk ではなく first-seen live emission に従う。

### Writer raw ObjGen renumber boundary (`flpdf-kod35`, 2026-09-14)

qpdf's `QPDFWriter::enqueueObject` and `unparseChild` key the writer-owned
`obj_renumber` table by the complete `QPDFObjGen`, including generations that
are valid in an object header but cannot be written as a parsed `N G R`
projection (`libqpdf/QPDFWriter.cc:1072-1157`). flpdf's `LiveQueue` therefore
keeps `raw_old_to_new: BTreeMap<QpdfObjGen, ObjectRef>` alongside the public
projection map, and the live static/dynamic child serializers consult the raw
identity before deciding whether to recurse. The `ObjectRef` callbacks remain
legacy adapters for projection-only callers; they convert at the writer
boundary and never manufacture an `ObjectRef` for an out-of-range generation.
`WriteObject` also receives the raw identity, preserving QDF original-object
comments and top-level output lookup for the same handles. The regression is
covered by `qpdf_obj_gen_header_tests.rs` for compact, QDF, and encrypted-QDF
output.

PCLm は `doWriteSetup` が強制する decode-none、uncompressed、unencrypted policy
（`QPDFWriter.cc:2068-2096`）の後にも、通常 writer と同じ `willFilterStream` policy を通す。
flpdf の `canonical_stream_output_for_rewrite` は data-modified、filter-on-write、metadata
cleartext、normalization、Flate preserve/recompress、provider の first `will_retry=true` call と
unfiltered retry を扱い、PCLm は返った一 stream payload を一度だけ final sink に流す
（`writer/plain/body.rs`; `writer.rs::write_pclm`）。この local payload は qpdf の
`willFilterStream` 内 `Pl_Buffer` に対応し、PDF 全体を保持する cache ではない。

deterministic ID は qpdf の `pushMD5Pipeline` と `generateID` の cutoff
（`QPDFWriter.cc:1011-1034,1213-1217,1823-1878`）に合わせ、flpdf の final `OutputSink` が
header/body/xref/trailer の `/ID [` までを digest し、`write_deterministic_id_inline` が `[` の
直後に digest を suspend してから ID bytes を出す。よって local stream/ObjStm/xref buffer
の構築中の bytes、および `/ID` 後の bytes はその digest に入らない
（`writer/output.rs`; `writer.rs::write_deterministic_id_inline`; `writer/plain/xref.rs`）。
この documentation slice は crate public API、CLI/qtest scope、または qpdf の object-graph
ownership modelを変更しない。

### Linearized raw ObjGen identity (`flpdf-474u8`, 2026-09-14)

qpdf の linearization でも `QPDFWriter::Members::obj_renumber` は
`std::map<QPDFObjGen, int>` のままであり、`enqueueObject` / `unparseChild` の全 pass が
object number と generation の組を同じキーとして使う
（`include/qpdf/QPDFWriter.hh:668-670`; `libqpdf/QPDFWriter.cc:1057-1157`）。
`calculateLinearizationData` の part 分類・hint 用の page/object-user 関係も
`QPDFObjGen` を保持し、`discardGeneration` は hint の object-number-only view を作る
直前の限定された変換である（`libqpdf/QPDFWriter.cc:2510-2654,2858`）。

flpdf は `optimization.rs` の正準 object-user/inverse map、linearization plan の private
part view、`linearization/renumber.rs` の forward/reverse slot table を
`QpdfObjGen` で保持する。`ObjectRef` の plan fields と既存 hint API は、PDF の `N G R`
境界を要求する caller 用の checked projection に限定した。projection できない raw
generation は sentinel identity に置き換えず、raw slot と `RenumberMap::new_for_raw` を
通じて出力 generation-zero object number へ割り当てる。page/trailer/root-key の user
分類と open-document/outline precedence は `QPDF_optimization.cc:57-118,264-381` および
`QPDF_linearization.cc:963-1064,1173-1449` に合わせる。

linearized の compact/QDF/stream/暗号化 serializer は、object、stream dictionary、
trailer `/ID`、ObjStm member の全 child walk に raw map を渡す。したがって
`5 65536 obj` のような header-only raw identity は dictionary に埋め込まれず、通常の
indirect child と同じく output map の参照 token になる。Generate/Preserve の ObjStm
再配置でも raw reverse table を同時に更新し、raw member 自体は gen-0 ObjStm eligibility
へ投影しない。stale generation の removed set は writer 境界で一度だけ raw set にし、
pass 1、hint、pass 2、ObjStm body が同じ借用 set を使う。`qpdf_obj_gen_map_from_object_ref_map`
および `qpdf_obj_gen_set_from_object_ref_set` は、明示的に checked projection を受ける
非-linearized/test adapter のみの責務として残る。

`qpdf_obj_gen_header_tests.rs` は raw Catalog/page child、page-shared child、raw stream、
ObjStm併用、encrypted linearized write を実出力で検証し、raw writer unit test は
dictionary-key omission と array-position `null` を確認する。 pinned qpdf 11.9.0 の
`--check-linearization` でもこれらの追加ケースは警告なしで通過する。

### Encrypted-input generated ObjStm placement (`flpdf-0msjh`, 2026-09-16)

qpdf は `generateObjectStreams` の global even-split で選んだ member を
`filterCompressedObjects` により一度だけ container の user 集合へ折りたたみ、
`calculateLinearizationData` の page-by-page `lc_other_page_private` 順に従って
container を各 page group の末尾へ置く（`libqpdf/QPDFWriter.cc:1970-2006`、
`libqpdf/QPDF_optimization.cc:340-380`、`libqpdf/QPDF_linearization.cc:1223-1264`）。
そのため、ある member が first-page 側の ObjStm に入っていても、後続 page の closure
に現れたことだけで second-half の plain anchor として扱ってはならない。

flpdf の `second_half_container_anchors` は、second-half batch だけでなく open-document
と first-half を含む全 routed ObjStm member set を plain anchor 候補から除外する。
これにより encrypted input の Generate でも、page-private container はその page の
plain object の直後に採番・出力され、page-offset hint が `lengthNextN` と一致する。
`objstm-lin-disc-2-250-2.pdf` を qpdf 11.9.0 で暗号化した回帰は RC4-128、AES-128、
AES-256 の各方式で top-level/rewrite 両 surfaceを通し、`qpdf --check-linearization` の
warning なしを確認する。対象 fixture の最終 live audit では、固定 ID/IV の qpdf 出力と
flpdf 出力が 3 方式・2 surface の全 6 組で byte-identical になり、暗号化なしの Generate、
既存の ObjStm byte-parity、Part 7/8、multiple-container、progress/sink error 経路は
変更しない。これは qpdf の既存 user/part precedence を補うもので、qpdf-deviation marker
は追加しない。

### Linearized raw identity follow-ups (`flpdf-pwyo2`, 2026-09-15)

`flpdf-474u8` の raw slot移行後に残っていた八つの linearization consumer gapを、
qpdf 11.9.0 の責務境界へ再接続した。`QPDFWriter::willFilterStream` は
Catalog `/Metadata` の参照同一性ではなく、各 stream dictionary の
`/Type /Metadata` を判定し、`!encrypt_metadata` のときだけ data keyを外す
（`libqpdf/QPDFWriter.cc:1234-1314,1537-1556`）。flpdf の
`linearization/writer.rs::append_body_object_with_raw_identity` も同じ raw stream
dictionary判定を使い、`metadata_ref` を `None == None` の代替identityとして使わない。

content normalization と stream-parameter omission は同じ raw setを共有する。
qpdf は `initializeSpecialStreams` で各 page の `/Contents` の直接の stream/array
memberから `getObjGen()` を記録し（`QPDFWriter.cc:1912-1936`）、
`QPDF::optimize` の `skip_stream_parameters` がその streamを再filterすると
`/Filter` と `/DecodeParms` を走査対象から外す
（`QPDF_optimization.cc:261-333`）。flpdf は
`linearization/plan.rs` の `BTreeSet<QpdfObjGen>` でこれを保持し、raw streamにも
正規化を適用し、raw identityで parameter edgeをclosure/reachabilityから外す。

CLIのcreate-stageで既に正規化したstreamも同じwriter-side
`content_normalization` stateを有効なままlinearized consumerへ渡すため、
`content_normalization_applied` markerは二重tokenizeを防ぎながらqpdfの
非圧縮normalize policyを選択できる。`flpdf-0s1ey` の
`cli_linearize_normalize_content_is_byte_identical_to_qpdf` が通常stream、
multi-contents、shared ObjStm、direct page leafをfull-byte比較する。

Part 7/8 のraw追加はglobal suffixではなく、qpdfの page-by-page／raw set順へ mergeする。
Part 8 の既存 hint entryとraw entryは `RenumberMap` のphysical output unit順に統合し、
page shared identifiersは出力番号ではなく qpdfの `obj_user_to_objects` の
`QPDFObjGen`順を使う。second-half generated ObjStm containerのanchorも、raw plain
peerを含む該当partの最初のcompressed member位置から決める。根拠は
`QPDF_linearization.cc:1228-1270,1351-1402` と
`QPDFWriter.cc:1057-1118,2579-2654` である。

Outlinesは raw `/Outlines` root identityを `RenumberMap::new_for_raw` へ渡し、
`pushOutlinesToPart` のroot-first順を保持して `/O` hint table の first object/countを
計算する（`QPDF_linearization.cc:1406-1432,1614-1631`）。これにより raw-only outline
でも hint keyが欠落せず、子itemを含む連続 output unitを指す。

回帰は `qpdf_obj_gen_header_tests.rs` の raw metadata、raw page content、Part 7/8、
raw outline、ObjStm anchorケースと、`hint_page` の跨ぎpart shared-ID unit testで固定した。
Part 8/ObjStmケースは live qpdf 11.9.0 の `--check-linearization` でも警告なしで通過する。
今回の変更は qpdf が定義する挙動の不足を埋めるもので、qpdf-deviation markerを追加しない。

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

The D16/D1 setup slice adds `writer.rs::WriterSetupState`: the common write
boundary captures generated ID material and builds `EncryptionParameters` once
after qpdf-shaped option normalization. Standard and linearized output then
assign route-specific `/Encrypt` object slots from that shared state through
`EncryptionParameters::into_context`; they no longer rebuild the Standard
dictionary, donor file key, metadata exemption, or data-key parameters at
different route-local times.

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

### Non-linearized source-backed Preserve ObjStm numbering (`flpdf-hi08`, 2026-09-04)

qpdf の encrypted non-linearized standard writer も、出力が暗号化されるかどうかに
かかわらず、Preserve された source ObjStm の member に最初に到達した時点で
container と source object-number 順の全 member を予約する
（`QPDFWriter.cc:1057-1118,1939-1967`）。暗号化された source を平文へ書き直す
場合も同じ Preserve map と enqueue 順を使うため、`setPreserveEncryption(false)` は
ObjStm の採番 policy を変更しない。container は再構築され、`/Extends` の間接参照
だけが引き継がれる（`QPDFWriter.cc:1621-1740`）。

flpdf の specialized non-QDF writer route は `source_xref_entries` から各 Preserve
batch の source container を再構成し、既存の `ObjectStreamRenumber` に
`SourceBacked` group として渡す。これにより output encryption と encrypted-source
decryption の両方で、ordinary object・source container・member range を qpdf の
reservation order に interleave し、trailer と `/Extends` の参照も同じ map で remap
する。qpdf の `writeEncryptionDictionary` に合わせ、`/Encrypt` は body の後置 slot
に残る（`QPDFWriter.cc:2244-2255`）。

**renumber は重複していない**: `writer/rewrite_renumber.rs` は `linearization/plan.rs` からも
使われる共有機構で、`linearization/renumber.rs` はその上に載る最終採番層。qpdf の
`obj_renumber` 1 本に対して 2 層構造だが、二重実装ではない。

## 4. 線形化 / 最適化

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF_linearization.cc` | 1796 | `linearization/`（`plan.rs` 7176, `hint_*` 3741, `check.rs` 3467, `show.rs` 2642, ほか）≒ 17,000 行 | ✅ qpdf `QPDF_linearization.cc:452-470` と同じく、`check.rs` の `/T` は xref parser が保持する `first_xref_item_offset` (`QPDF.cc:845-869,1110-1120`) に対する whitespace 消費後の位置比較だけを行う（構造探索・subsection再解析・flpdf 固有の hard failure は除去済み、`qpdf-deviation` マーカーも撤去済み）。初回または `/Prev` の classic xref row parse が後続 row で失敗しても、object 0 row で観測した offset を reconstruction へ保持する qpdf の mutable-state 挙動 (`QPDF.cc:626-708,846-869`) を `flpdf-7yvv` の side channel で再現する。`flpdf-1quo` で check consumer は primary/overflow hint stream を qpdf と同じ buffer に連結し、Page Offset / Shared Object / Outline の各 hint table の object count・length・shared membership・physical offset を qpdf の object-user 分類と実 xref extent に照合する。実装は 5+ モジュールに分散したまま（`optimization.rs` が達成したような単一モジュールへの集約は未達）。ObjectHandle 移行自体は完了: producer 側（`flpdf-3yn9.4`、plan.rs + hint_*）と consumer 側（`flpdf-egzr.3.2.9`、check.rs + show.rs）が close 済み。`check_consumer_production_uses_the_canonical_object_handle_route` / `show_consumer_production_uses_the_canonical_object_handle_route` は production 経路から `Object::` / `resolve_borrowed` / `decode_stream_data` / `page_refs` が消えたことを機械的に保証する。残存する `plan.rs` の `collect_direct_refs`（Object 版）は `#[cfg(test)]` の fixture walker に限定され、production closure と writer の計算は同ファイルの `collect_direct_handle_refs`（ObjectHandle 版）が担う。線形化書き込み経路自体（writer.rs 側、`flpdf-3yn9.5` 系列）は issue タイトルが明記する通り §3 `QPDFWriter.cc` のスライスであり本行の対応先ではない
`flpdf-rbyc6.1` では qpdf の `calculateLinearizationData` が first-page object を `lc_first_page_private` に置けない場合に `stopOnError` で停止する境界（`QPDF_linearization.cc:1188-1195`、`QPDF.cc:2590-2592`）を、`Optimization` の raw object-user map と `LinearizationPlan::first_page_is_private` で再現する。通常の page-0 private object は従来どおり線形化し、trailer の non-`/Encrypt` key などで page object が shared になる入力は qpdf と同じ `DamagedPdf` error と exit 2 で停止する。synthetic trailer-user fixture と qpdf-qtest `bad35.pdf` の exit/status/stderr/zero-byte output を比較する。 |
`flpdf-psgss` では qpdf の Preserve linearization における source ObjStm の Part-9 配置順（`/Pages` user set、thumbnail、`lc_outlines`、remaining `lc_other`）を、折り畳み済み `Optimization` user map と source container identity から `objstm_batches_preserve` の batch order へ反映する。さらに `second_half_container_anchors` も同じ source-container の category key を使い、plain `lc_other` object の後ろへ誤って固定しない。qpdf は `QPDF_linearization.cc:1279-1337` の set/vector 順で part9 を作り、その順に `QPDFWriter.cc:2575-2651` が container/member 番号を予約する。手書き type-2 xref fixture と、qtest の `nontrivial-crypt-filter-decrypted.pdf` / `enc-XI-attachments-base.pdf` を qpdf 11.9.0 と比較し、qpdf-zlib-compat で full-byte parity と `--check-linearization` clean を確認する。 |
| `QPDF_optimization.cc` | 381 | `optimization.rs`（optimization orchestration、inherited-page preparation、object-user maps、compressed-object folding）+ `optimization/inherited_attrs.rs`(575) | ✅ `flpdf-qxba.9.3` / `.9.4` で完全 cutover。`linearization/plan.rs` 側に `ObjUser` / `update_object_maps` は残っていない。⚪ `inherited_attrs.rs` の inheritable key null 判定（`push_node_attributes` / `push_child_reference`）は `Pdf::resolve_to_terminal` で `Pdf::set_object` bare-reference redirect の終端まで辿る。qpdf 自身のオブジェクトグラフは「あるオブジェクトの値が別の参照そのもの」という形を持てない（対応物なし）ため、`pages.rs` の `resolve_inherited_handle_with_max_depth`（bottom-up の姉妹関数）と同じ理由で同じ補償を行っている。⚪ `Optimization::update_object_maps` の reference-valued handle 再ディスパッチも `Pdf::set_object` が作る flpdf 固有形状だけを対象にし、qpdf parsed graph には追加の対応物を作らない（`QPDFParser.cc:26-90,140-176`）。 |

線形化の stream-parameter reachability は `writeLinearized` の
`skip_stream_parameters`（`QPDFWriter.cc:2543-2553`）と
`QPDF_optimization.cc:274-333` に合わせ、refilter 判定済みの参照元 stream
identity ごとに `/Filter` / `/DecodeParms` edge を除外する。probe と emission
は同じ `willFilterStream` 相当の metadata/content-normalization policy を使い、
共有 parameter object は保存される別 stream から引き続き到達可能にする
（flpdf-p045）。D27 の pre-write sweep 撤去後もこの probe は全 object の事前走査へ戻さず、
`Optimization::update_object_maps` の page/trailer/root 起点 callback 内でだけ実行する。
そのため qpdf と同じ到達範囲外の stream の source bytes や間接 `/Length` holder を
linearization planning が解決しない（`flpdf-3yn9.44.1`）。同じ境界を ObjStm
eligibility にも適用し、`QPDF::getCompressibleObjGens`
（`QPDF.cc:2393-2474`）の到達可能 walk から `/Length` 除外集合を返す。
非 linearized の Preserve/Generate planner もこの結果だけを使い、xref 全件の
事前走査を行わない（`flpdf-3yn9.44.1.1`）。ただし qpdf の writer setup 自体は
`getObjectCount`（`QPDF.cc:1271-1283`）で xref 全体を解決し、`resolve`
（`QPDF.cc:1699-1753`）の warning/null 回復を適用するため、「qpdf が orphan を
一切解決しない」とは一般化しない。
また、linearization optimizer callback の indirect parameter 判定は最初の
`willFilterStream` 相当 probe の結果をそのまま使い、同一 callback 内の二重 probe
を避ける（qpdf の retry を含む一回の probe と後段 emission は別である）。

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

### Malformed copy-encryption `/Length` fallback (`flpdf-67h8d`, 2026-09-17)

qpdf 11.9.0 の `QPDFWriter::copyEncryptionParameters` は V>1 の donor `/Length` を
`getIntValueAsInt() / 8` で読み、非整数または欠落を warning とともに 0 として受け入れる
（`QPDFWriter.cc:651-702`、`QPDFObjectHandle.cc:503-543,2168-2189`）。その値は
`setEncryptionParametersInternal` で出力辞書の `/Length`（bit単位）へ戻され、V<5 では
長さ 0 の file key を再導出し、V>=5 では認証済み key を保持する
（`QPDFWriter.cc:777-840`、`QPDF_encryption.cc:374-402`）。これは reader の malformed
`/Length` を 128 bit とみなす復号時 fallback（`QPDF_encryption.cc:835-853`）とは別の
writer-side copy semantics である。

flpdf は `Pdf::writer_copy_encryption_source` が donor の warning sink を保持したまま
writer-side `/Length` を `writer_length_bits` として snapshotし、`make_direct(false)` 後も
非整数 fallback の `Some(0)` を失わないようにした。canonical copy builder はこの 0 を
拒否せず、V<5 では空の file key と `/Length 0` を出力する。`flpdf-cli/tests/
cmp_copy_encryption_length_tests.rs` は qpdf 11.9.0 の warning status、stderr、出力と
`qpdf-zlib-compat` の全 byte を比較してこの境界を固定する。

`QPDF::compute_data_key`（`QPDF_encryption.cc:325-357`）の共有 Rust primitive は
qpdf の未使用 `encryption_R` 引数を省略している。これは reader state に存在しない
値を sentinel で補うことを避ける、出力不変の内部 signature 代替である。Algorithm 3.1
が実際に読む V、object ID、generation、AES salt の順序と bytes は変更せず、reader の
`(obj,gen)` cache と writer の generation 0 呼び出し契約も保持する。writer state の
`_encryption_r` は qpdf `QPDFWriter::Members` の state 対応として保持するが、鍵計算へは
渡さない。

### Reader V/R acceptance set (`flpdf-3yn9.48.163`, 2026-09-18)

qpdf の `initializeEncryption` は `/R ∈ 2..=6` かつ `/V ∈ {1,2,4,5}` の全 20 通りを
受理する（`QPDF_encryption.cc:787-795`）。flpdf は
`(V,R) ∈ (1|2, 2|3) | (4,4) | (5,5|6)` の 7 通りに限定し、V<5 側 handler の選択も
`V` ではなく `R∈{5,6}` で分岐していた。`encryption/state.rs` は handler 選択を
`/V == 5` に、受理述語と `encryption/standard.rs` の validator を qpdf の 20 通りへ
広げた（`hash_V5` の R<6 分岐に合わせ、V=5 の password 認証は R>=6 のときだけ
Algorithm 2.B を使う）。これで到達可能になった V<5/R>=4 と V=4/R≠4 の version floor は
qpdf と同じ `/R` keyed 規則（`QPDFWriter.cc:806-814`）に揃え、copy path の R=4 は
`copyEncryptionParameters` の V>=4 AES 強制（`QPDFWriter.cc:674-679`）を反映する。
`crates/flpdf-cli/tests/cmp_copy_encryption_key_derivation_tests.rs` の
`copy_encryption_accepts_the_qpdf_vr_set` が V=4/R=3、V=4/R=5、V=2/R=4 の
primary/copy 両 route を qpdf 11.9.0 と byte 比較する。

### Copy-encryption の V<5 鍵再導出 (`flpdf-3yn9.48.161`, 2026-09-18)

qpdf の `QPDFWriter::copyEncryptionParameters` は donor の `/V`・`/R`・`/Length`
（V==1 は固定の 5 バイト）と、`QPDF::getPaddedUserPassword()`
（`QPDF_encryption.cc:1206-1210`）を `setEncryptionParametersInternal` へ渡す
（`QPDFWriter.cc:651-702`）。そこでは V>=5 のみ `getEncryptionKey()` の認証済み鍵を
そのまま使い、V<5 は `QPDF::compute_encryption_key(user_password, EncryptionData(V, R,
/Length / 8, …))` で鍵を **再導出** する（`QPDFWriter.cc:832-839`、
`QPDF_encryption.cc:360-403`）。鍵長は `min(16, /Length / 8)` で、donor の handler
組み合わせも、reader が実際に認証で使った鍵長との整合も、一切検査しない。
preserve-encryption（primary input）も同じ関数を通る（`QPDFWriter.cc:2099-2101`）。

flpdf は以前 canonical copy builder と `--copy-encryption` の donor 境界
（`flpdf-cli/src/main.rs`）で `/Length` のレンジ・V/R handler 組み合わせ・
`file_key.len() == /Length / 8` を検証しており、qpdf に対応物のない独自述語だった。
この 3 つを撤去し、`CopyEncryptionSource::padded_user_password`
（`Pdf::writer_copy_encryption_source` が `EncryptionState::user_password` から
snapshot する）と、qpdf の `compute_encryption_key_from_password` を逐語訳した
`encryption/standard.rs::compute_encryption_key_from_password` に置き換えた。
既存の `compute_file_key` / `compute_file_key_v4` は flpdf 側の handler 検証
（qpdf は reader の `initializeEncryption` で行う）を保ったままこの共有 core へ
委譲する。検証済み `/Length` は常に `min(16, n) == n` を満たすため出力は不変。
`/O`・`/U` を 32 バイトへ射影する `v_lt_5_32_byte_parameter` は、qpdf が
`std::string::c_str()` から 32 バイトを読む挙動（短い entry は over-read）に対し、
reader 側 `required_v_lt_5_32_byte_string_from_handle` と同じ NUL 埋めを行う。
なお切り詰め・NUL 埋めのどちらの arm も、認証を通った donor からは到達しない——
qpdf も flpdf も reader が V<5 の `/O`//`U` を NUL 埋めしたうえで正確に 32 バイトを
要求し（`QPDF_encryption.cc:805-813`）、40 バイトの `/O` を持つ donor は両者とも
open 時点で失敗する（実測: qpdf `incorrect length for /O and/or /U in encryption
dictionary` / flpdf `malformed /Encrypt dictionary: /O entry is not 32 bytes`、
どちらも exit 2）。この射影が効くのは、任意の辞書から `CopyEncryptionSource` を
直接構築するライブラリ呼び出しだけである。
負の `/Length` は qpdf の `QIntC` 変換が投げる `std::range_error` に対応して
`Error::System` を返す。

`flpdf-cli/tests/cmp_copy_encryption_key_derivation_tests.rs`（whole-file
`qpdf-zlib-compat` gate、CI 列挙済み）が malformed `/Length`（V2R3 032 / V2R3 240 /
V5R6 128）と `--password-is-hex-key`（4 長）を `--copy-encryption` / preserve
両経路で qpdf 11.9.0 と byte 比較する。hex-key 経路では `getPaddedUserPassword()` が
空のままなので qpdf は空パスワードから鍵を導出し、qpdf 自身の出力が qpdf 自身の
`--check` を通らなくなるが、CLAUDE.md の oracle 方針に従い flpdf もその出力を再現する。

**残る逸脱**: V=4 donor の `/Length 040` は 5 バイトの file key、すなわち 14 バイトの
per-object AES key を生む。qpdf はそのバッファを crypto provider に渡し、provider は
24/32 以外の長さを AES-128 に写して 16 バイトを読む（`QPDFCrypto_gnutls.cc:197-213`、
`QPDFCrypto_openssl.cc:225-241`）ため、末尾 2 バイトは未定義の over-read になる。
flpdf は既存方針どおりこれを捏造せず拒否する（`pipeline/aes.rs` と
`writer/encrypted_strings.rs` の `qpdf-deviation` マーカー、および本節下表の
`QPDF_encryption.cc` 行の記載）。この 1 形状だけ exit code が qpdf と異なる。

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDF_encryption.cc` | 1410 | `encryption.rs` (facade) + `encryption/state.rs` + `encryption/crypt_filters.rs` + `encryption/keys.rs` + `encryption/standard.rs`(1879) + `encryption/permissions.rs`(206) + `encryption/password.rs`(380: `password_bytes_for_read` + `password_candidates_for_read` — qpdf `QPDFJob.cc:1734-1790` の read-side hex decode、raw-byte pass-through、alternate encoding retry と suppress gate、`QUtil.cc:1821-1900` の PDFDoc/WinAnsi/MacRoman candidates、V=5 の 127-byte 切り詰めは Standard handler が担当。`--password-is-hex-key` は `QPDF_encryption.cc:933-934` の通り decoded key に通常の 32-byte 上限を適用せず、`QPDFJob.cc:1245-1252` の JSON bits も実 key 長を報告する。AES provider は 16/24/32 以外の鍵長を AES-128（先頭 16 バイト、`QPDFCrypto_gnutls.cc:197-213` / `QPDFCrypto_openssl.cc:225-244` の default arm）へ投影し、24 バイトは AES-192 を選ぶ。16 バイト未満は qpdf が鍵バッファを over-read する未定義挙動のため flpdf は拒否する（reader 側 `encryption/state.rs::aes128_object_key` と writer 側 `writer/encrypted_strings.rs` / `pipeline/aes.rs` の `qpdf-deviation` マーカー。writer 側は copy-encryption の V=4 `/Length 040` donor から到達する）) | 🔀 |
| `rijndael.cc` / `AES_PDF_native` / `MD5_native` / `SHA2_native` | 1668 | `encryption/primitives.rs`(106: AES single-block ECB と MD5) + `pipeline/sha2.rs` の `Sha2Digest`(SHA2)（外部 crate）。AES-CBC は `pipeline/aes.rs` の `PlAesPdf` に一本化済みで、`encryption/primitives.rs` には V=5 R=6 Algorithm 10/13 の single-block ECB だけが残る。qpdf は `SHA2_native` へ `Pl_SHA2` 経由でしか到達しない（`QPDF_encryption.cc:246,296` が唯一の production 利用）ため、RustCrypto の SHA-2 hasher も `Pl_SHA2` 移植の内部に閉じている。`encryption/primitives.rs` の一括 `sha256`/`sha384`/`sha512` wrapper は consumer cutover で削除済み | ⚪ |
| `RC4.cc` / `RC4_native.cc` | 63 | `encryption/rc4.rs`(80)（明示長キー / C-string キー、state 保持、separate / in-place processing） | ✅ |
| `QPDFCryptoProvider.cc` / `QPDFCrypto_*` | 774 | provider 抽象が無い | ⚪ |
| ランダム源 3 ファイル | 185 | `writer.rs` の `fresh_id_bytes` 等に散在 | 🔀 |

### Accessor warning chains for invalid `/ID` and `/Pages` (`flpdf-6gmnc`, 2026-09-17)

qpdf の `QPDFWriter::copyEncryptionParameters` は、欠落 `/ID` を
`getKey("/ID").getArrayItem(0).getStringValue()` で辿る
（`QPDFWriter.cc:651-702`; `QPDFObjectHandle.cc:758-785,965-989,2168-2189`）。
そのため `invalid-id-xref.pdf` では null 配列の warning と、そこから返った null の
文字列 warning が `dictionary key /ID -> null returned from invalid array access` の
記述連鎖付きで発生する。flpdf は通常の source-ID生成では欠落 `/ID` を無音の
`hasKey` 境界で扱い、copy-encryption のみ `source_permanent_id_value_handle` の
warning-producing accessor chainを実行する。classic trailer handleにも
`input, trailer at offset N` のqpdf descriptionを付与し、warningの文言・文脈を保持する。

linearization の空ページツリーでは、qpdf の `getAllPages`（`QPDF_pages.cc:39-150`）と
`QPDF_optimization` の inherited-attribute walk（`QPDF_optimization.cc:57-245`）が、
null `/Pages` に対する `hasKey`、`getKeys`、`getKey("/Kids")`、配列長取得を行う。
flpdf は `PageWalk` と `optimization/inherited_attrs.rs` の canonical
`try_*` accessorsで同じ連鎖を発生させ、linearization planの事前 page-cache境界も
`initializeSpecialStreams` 相当の順序に揃えた。

Pinned qpdf 11.9.0（`3b97c9bd266b7c32ea36d3536e22dab77412886d`）の live probeでは、
`invalid-id-xref.pdf` の `--static-id` が qpdf/flpdf とも exit 3、1012 bytes、stderr
完全一致。`issue-119`/`issue-120`/`issue-143` の
`--deterministic-id --linearize` はともに exit 2・出力0 bytesで、warning行数は
それぞれ 10/14/26（qpdf/flpdf一致）。最終的な no-pages error offset は別の
`flpdf-qlwe5` scopeであり、このissueではwarning accessor chainだけを固定する。

## 6. Pipeline / フィルタ

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `Pipeline.cc`（積層シンク基盤のみ。個々の `Pl_*` は下記の各行で個別に分類） | 114 | `pipeline.rs`（public `Pipeline` trait、identifier/write/finish lifecycle、logic/runtime error channel）。⚪ qpdf が bare `Pipeline*` で回す `next` slot に対応する `PipelineRef`（borrowed / owned の 2 択）を持ち、`Flate` / `LzwDecoder` の `next` はこれを受け取る。**⚪ (B) stage の所有者**: qpdf は構築した stage を filter instance 内に保持し呼び出し側へは非所有ポインタを返す（`QPDFStreamFilter.hh:47`、`SF_FlateLzwDecode.cc:88`・`:108`）。flpdf は stage を値で返し、多段 chain の内側 stage は `pipeline.rs` の `PipelineRef::Owned` が持つ。構築順・stage 数・出力 bytes は不変で、動くのは所有者だけ。この slot を通る本番 write path は `qpdf-zlib-compat` gated の `cmp_generate_objstm_tests` で qpdf golden に pin する | ✅ |
| `Pl_Count.cc` | 48 | `pipeline/count.rs`（byte count、last byte、forwarding、finish lifecycle） | ✅ |
| `Pl_MD5.cc` | 66 | `pipeline/md5.rs`（enable/persist/reuse、hex digest、forwarding/error order）+ `filespec_helper/embedded_file_stream.rs`（EmbeddedFile `/Params /CheckSum` production consumer） | ✅ |
| `Pl_Flate` / `SF_FlateLzwDecode` | 946 | `pipeline/flate.rs` + `stream_filter.rs` の `FlateLzwStreamFilter`（`/Predictor` `/Columns` `/Colors` `/BitsPerComponent` `/EarlyChange` の解釈、codec → predictor の chain 構築、`QIntC::to_uint` の range error timing）。`SF_FlateLzwDecode::getDecodePipeline`(`SF_FlateLzwDecode.cc:75-110`) 相当の `get_decode_pipeline` を持つ。構築順（sink 側から、predictor を作って `next` を差し替えてから codec を作り、その codec を返す）は qpdf のままで、内側になる predictor だけ `PipelineRef::Owned` が所有する。既知の逸脱: whole-buffer route の `pipe_codec` は `Pl_Flate` の warn callback を stage 構築側で設置するが、qpdf は `getDecodePipeline` の呼び出し側(`QPDF_Stream.cc:564-567`)で設置する。この route が構築する `Pl_Flate` はいずれも qpdf が当該 filter の iteration で設置するのと同じ callback を受け取るので、警告の文言・順序は変わらない。再現できないのは qpdf のもう一方のケース — cast は stage 単位ではなく filter 単位で 1 回走り(`:561-563` の guard の外)、stage を構築しない filter の iteration では別の場所で構築された stage に当たる。設置位置とこのケースは共に `QPDF_Stream::pipeStreamData` 移植の担当（`get_decode_pipeline` 側は qpdf 通り callback を設置しない） | ✅ |
| `Pl_LZWDecoder` | 189 | `pipeline/lzw.rs`（3-byte rotating buffer、1 入力 byte あたり 1 code、table 成長と code 幅遷移、eod latch、qpdf の 7 種の診断文言）+ `stream_filter.rs` 経由の production decode | ✅ |
| `Pl_PNGFilter` | 232 | `pipeline/png_filter.rs`（32-bit wrapping の row 幅算出、constructor の 3 種 rejection、未知 filter byte の無視、finish の zero-pad row、Up 固定 encoder）+ `writer/serialize.rs` の production consumer。⚪ row buffer の確保だけは constructor ではなく最初の write まで遅延（出力バイト・呼び出し境界・エラー timing に影響しない） | ✅ |
| `Pl_TIFFPredictor` | 175 | `pipeline/tiff_predictor.rs`（incremental row buffering、8-bit の byte differencing、packed sample の signed MSB bit I/O、finish 時の zero padding）+ `stream_filter.rs` の Predictor 2 production consumer。qpdf の TIFF fixture vectors と construction/write/finish error timing を pin。qpdf が filter instance に保持する stage ownership は、flpdf では `PipelineRef::Owned` が内側 predictor を保持する意図的な Rust ownership substitution。qpdf head `cf047b20721b18b15525c04b6970e562c90c4a6a`（`Pl_TIFFPredictor.cc:38-48`）の `bits_per_pixel` / wide row geometry preflight を constructor に追加し、overflow が `previous` 状態領域へ到達しないようにした。preflight 後の representable geometry は pinned qpdf 11.9.0 の wrapped row width を保持し、既存の partial-row/packed-row bytes を変えない。qpdf に対応物のない optional memory limit は `.48.96` で撤去した | ✅ |
| `Pl_ASCII85Decoder` / `SF_ASCII85Decode` | 108 + 31 | `pipeline/ascii85_decoder.rs` + `stream_filter.rs`（`SF_ASCII85Decode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_ASCIIHexDecoder` / `SF_ASCIIHexDecode` | 96 + 31 | `pipeline/ascii_hex.rs` + `stream_filter.rs`（`SF_ASCIIHexDecode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_RunLength` / `SF_RunLengthDecode` | 146 + 38 | `pipeline/run_length.rs` + `stream_filter.rs`（`SF_RunLengthDecode::getDecodePipeline` 相当の単段 stage 構築を含む） | ✅ |
| `Pl_AES_PDF` | 200 | `pipeline/aes.rs`（qpdf の contract を全量移植: block 単位の write バッファリング、first-block を IV として消費する復号側と IV を先頭へ書く暗号化側、ISO 32000-1 7.6.2 の padding とその strip、`useZeroIV` / `setIV` / `useStaticIV` / `disablePadding` / `disableCBC`）＋ `PlAesPdf::decrypt_to_vec`（qpdf `decryptString` の `Pl_Buffer` + `Pl_AES_PDF` 組（`QPDF_encryption.cc:1013-1021`）に対応する one-shot）と `writer.rs` の stream consumer | 🔀 `reader/resolver.rs` の `QPDF::decryptStream` 対応は `PlAesPdf` を source-read pipeline の前段へ接続済み。resolve-time 経路も `encryption/standard.rs` の `decrypt_cipher_bytes` 経由で同じ `PlAesPdf` を通るため、AES 実装は qpdf と同じく 1 つだけ。⚪ `QPDFCryptoImpl::rijndael_init` / `rijndael_process` の crypto provider 抽象は `aes` / `cbc` crate の直接利用に置換（§ 逸脱候補の crypto provider 行と同じ代替）。block ごとに 1 回 process する呼び出し形は保持し、chaining 状態のみ provider 側ではなく cipher が持つ。**解消済みの逸脱**: 以前は `encryption/primitives.rs` の `decrypt_padded::<Pkcs7>` が別実装として併存し、qpdf が受理する入力（ブロック長に満たない末尾＝`Pl_AES_PDF.cc:107-118` の zero-pad、padding として不整合な末尾＝`:183-196` の strip 見送り）を `Err` にしていた。この厳密版を削除して `PlAesPdf` へ一本化したので、受理する文書は qpdf と一致する |
| `Pl_RC4` | 43 | `pipeline/rc4.rs`（65,536-byte既定buffer、stateful `encryption/rc4.rs`、write/finish lifecycle）+ `reader/resolver.rs` の pipe-time decrypt stage + `reader.rs` / `writer.rs` の既存 stream consumer | ✅ |
| `Pl_QPDFTokenizer.cc` / `ContentNormalizer.cc` | 141 | `pipeline/qpdf_tokenizer.rs`（optional downstream を持つ token-filter runner、EOF-token → `handle_eof`、`ID` separator 注入、inline-image 切替、raw token/discard/output、`handle_eof` 成功後の永久 detach と finish/error timing）+ production consumer `content_normalizer.rs`（bad-token state、CR/string/name normalization） | ✅ |
| `QPDFObjectHandle::TokenFilter` / `QPDF_Stream::addTokenFilter` / `isDataModified` | `QPDFObjectHandle.hh:129-190,420-475,978-1010`; `QPDF_Stream.cc:321-324,488-620,663-666` | `ObjectHandle::add_token_filter` / `is_data_modified` が共有filter listとdecoded→token-filter→normalize/encodeのlazy pipeを担う。`form_field_object_helper/rendering.rs` の既存 `/AP/N` reuse は eager `replace_stream_data` からqpdf `ValueSetter`相当の `AppearanceTokenFilter` へ移行し、`writer/plain/body.rs` は `is_data_modified` をlone-Flate fast pathの条件に含める。`linearization/writer.rs` の `append_body_object`（`stream_is_data_modified` helper 経由）も同じ `willFilterStream` 由来のゲートを適用: qpdf の `writeLinearized` は `QPDF::optimize` の `skip_stream_parameters` probe と実書き込みの計2回 `pipeStreamData` を呼び、token filter は pipe 間で状態リセットしないため実書き込み側は exhausted filter のパススルー（= stale content）を再エンコードする。flpdf の linearized writer には optimize 相当の二重 pipe が無いため、token filter 自体は起動せず「既に materialize 済みの (pre-filter) バイトを decode→re-encode」するだけで同じ observed output に一致させる（`docs.rs` 非公開のモジュール内 doc 参照）。`writer.rs` の `emit_canonical_pdf_inner` fallback と `writer/plain/body.rs` の `!plan.canonical` 分岐について、同種のゲート配線の要否を `flpdf-vkka` で検証済み（close）: `plain/body.rs` は既に canonical handle 経由で `is_data_modified()` を参照しており、`emit_canonical_pdf_inner` 側は PR #831 の `materialize_for_normalization` narrowing 後、`Object::Stream` 分岐が構造的に到達不能なため追加配線は不要と確認された | ✅ |
| `QPDFStreamFilter.cc` | 19 | `stream_filter.rs`（public `StreamFilter` の `set_decode_params` / `get_decode_pipeline` / specialized / lossy hook、`register_stream_filter` と built-in/custom registry）。`QPDFStreamFilter::getDecodePipeline`（`include/qpdf/QPDFStreamFilter.hh:46-49`）に対応する`StreamFilter`はnative full handleをsetterへ渡し、`None`はqpdfの`nullptr`（11.9.0でこれを返すのは`SF_Crypt`だけ、`QPDF_Stream.cc:52-56`）。qpdf-shaped production callerは`ObjectHandle::pipe_stream_data`に接続済みで、`.48.49` で非qtestの public whole-buffer decode/encode helperを撤去し、`.48.93` でqtest test 0/1もcanonical pipe/loggerへ移行、`.48.96` でrecovering public APIとlimit extensionを削除した。旧`DecodeParams`/`ParamValue` snapshot責務も削除済みで、runtime registryも `.48.46` で同じcanonical lookupへ接続した。 | ✅ |
| `Pl_DCT.cc` (buffer/decode) | 207 (`1-57,77-116,119-143,195-248,296-326`) | `pipeline/dct.rs` + `stream_filter.rs` の `DctStreamFilter`（`get_decode_pipeline` が canonical route。qpdf の buffered write、empty/repeated `finish` の downstream finish、libjpeg scanline 出力、error/cleanup を対応）; qpdf refs: `Pl_DCT.hh:30-70`, `Pl_DCT.cc:1-57,77-116,119-143,195-248,296-326`, `SF_DCTDecode.hh:8-40`。stage owner は qpdf の filter-instance 保持 + caller の non-owning pointer に対し、Rust は stage を値で返し `PipelineRef::Owned` と `next` の borrow で保持する correspondence class (B) | ✅ default backend は `libjpeg-turbo-rs = 0.8.0`、`qpdf-libjpeg-compat` は `flpdf-libjpeg-compat` を明示的に有効化する system libjpeg backend（no vendored library、runtime switch なし）。system-libjpeg の ABI boundary は `flpdf-libjpeg-compat`（`csrc/jpeg_compat.c/.h` + `ffi.rs`）が所有し、`BITS_IN_JSAMPLE == 8`、libjpeg 6b-compatible (`JPEG_LIB_VERSION >= 62`) capability/version guard、qpdf 相当の whole-buffer exhaustion (`invalid jpeg data reading from buffer`、fake EOI なし)、panic-contained callback を持つ。qpdf 11.9.0 の 8-bit scope を対象に、最小 image XObject の `qpdf --show-object=3 --filtered-stream-data` differential（2026-08-10 観測）は default/C とも qpdf stdout 12 bytes = canonical `DctSink` 12 bytes、mismatch 0、stderr 0。canonical consumer は `get_decode_pipeline`、legacy whole-buffer bridge route は `.48.49` / `.48.96` で撤去済み、writer passthrough は別責務として残す |
| `Pl_DCT.cc` (compression) | 119 (`58-76,117-118,144-194,249-295`) | `pipeline/dct.rs` の `PlDct::new_compressor` が qpdf の圧縮constructor、whole-buffer `finish`、JPEG出力、downstream `finish` を対応し、`job/image_optimization.rs` の `ImageOptimizer` が `QPDFJob.cc:102-236,2156-2174` の metadata/threshold 判定、サイズ比較、provider-backed DCT XObject置換を所有する。RGB/Gray は qpdf の default sampling、CMYK は `JCS_CMYK` の 1x1 sampling を選ぶ。 | ✅ qpdf 11.9.0 `image-optimization.test` 24/24、`crates/flpdf-cli/tests/image_optimization.rs::optimize_images_emits_qpdf_identical_jpeg_bytes_for_gray_rgb_and_cmyk` による pinned qpdf との Gray/RGB/CMYK raw JPEG bytes 自動比較を確認 |

`Pl_DCT.cc` の decode は `output_components` をそのまま scanline 幅に使い、component count を 1/3/4 に制限しない（`libqpdf/Pl_DCT.cc:297-326`）。一方、`libjpeg-turbo-rs = 0.8.0` の `decode_image_inner` は 1/3/4-component の分岐だけを持ち、その他は `N components not yet supported` になるため、2-component JPEG は default backend の恒久的な能力制限として扱う（`flpdf-twm6`）。qpdf と同じ decode parity が必要な caller は、明示的な `qpdf-libjpeg-compat` feature で system-libjpeg backend へ切り替える。既存の 1/3/4-component の decode/encode bytes はこの記録だけでは変更しない。
| `Pl_Base64` / `Pl_Concatenate` / `Pl_OStream` / `Pl_String` | 282 | `pipeline/base64.rs` / `pipeline/concatenate.rs` / `pipeline/ostream.rs` / `pipeline/string.rs`（JSON serialization/output の本番 consumer を含む） | ✅ |
| `Pl_StdioFile.cc` | 46 | `pipeline/stdio_file.rs`（positive partial write の継続、zero/error—including `Interrupted`—の即時 Runtime 化、`EBADF` finish のみ Logic 化）+ `json_inspect.rs`（4096-byte buffer、top-level file は close/drop、side file は explicit finish） | ✅ |
| `Pl_Buffer` | 82 | `pipeline/buffer.rs`（accumulation、optional pass-through、finish readiness、buffer ownership transfer） | ✅ |
| `Pl_Discard.cc` | 23 | `pipeline/discard.rs`（public terminal identifier、no-op write/finish、finish 後の再利用）+ `filespec_helper/embedded_file_stream.rs`（EmbeddedFile checksum terminal consumer） | ✅ |
| `Pl_Function.cc` | 62 | `include/qpdf/Pl_Function.hh:37-62` / `libqpdf/Pl_Function.cc:10-61` はコンストラクタを3つ持つ——C++ネイティブな `std::function` を受ける1つ（それ自体はC ABI固有ではない）と、C関数ポインタ+`void*`を受ける2つ。qpdf 11.9.0自身のソースで実際に呼ばれるのは後者のC-style overloadのみで、`qpdf-c.cc:1936` の `qpdf_write_json` と `qpdflogger-c.cc:58` の custom logger（いずれもC APIラッパー）に限られ、`std::function` overload の呼び出し元はqpdfのコードベースに存在しない。qpdf core の PDF reader/writer は `Pl_Function` を直接使用しない。実在する呼び出しが全てC APIラッパー経由のC-style overloadである以上、移植すべき非C-API production consumer が無く、`qpdf_write_json` の出力コールバックには `Json::write` に渡す caller-supplied `Pipeline`（`json/writer.rs:97`、C 側と同じくシリアライズ済み JSON バイトを受け取る境界。`Json::make_blob` は逆方向の producer closure で対応物ではない）、custom logger destination には `QPDFLogger` の `PipelineHandle` 型セッター（`logger.rs` の `set_info`/`set_warn`/`set_error`/`set_save`）を、それぞれ canonical route とする | ➖ |
| `Pl_SHA2.cc` | 75 | `pipeline/sha2.rs`（SHA-256/384/512 の bit 選択、`resetBits`、digest access、optional next への write/finish forwarding と error 順序、再利用 lifecycle）。`Pl_SHA2.hh:9-11` の契約通り `finish()` 後の最初の `write()` は同じ bit size の新 cycle を開始し、連続 `finish()` は empty digest を生成する。native backend が finalize 後に同じ context を再初期化する挙動（`sha2.c:670-673`; `sha2big.c:209-228`）は RustCrypto の `finalize_reset` に対応する。⚪ `bits=0` のままの write/finish は qpdf では null crypto provider を dereference し、最初の finish 前の digest access は未初期化 result buffer を読むため、Rust では定義済み logic error に変換する。production consumer は `encryption/standard.rs` の `r5_salted_hash` / `r6_password_hash`（qpdf `hash_V5`、`QPDF_encryption.cc:239-311`）で、初期 hash は連結バッファを作らずpassword/salt/udata を 3 回 write し（`:246-249`）、R=6 ループは毎周 fresh な `Pl_SHA2` を算出 bit size で構築する（`:295-299`）。qpdf が identifier を `"sha2"` に固定している（`Pl_SHA2.cc:8`）のに合わせ、callsite も同じ値を渡す | ✅ |

`Pl_DCT.cc` の error handler（`libqpdf/Pl_DCT.cc:24-31,83-142`）は libjpeg の `format_message` をそのまま保存するため、qpdf の marker 診断はリンク先 libjpeg の `JERR_UNKNOWN_MARKER`（`/usr/include/jerror.h:132`）に依存する。`flpdf-69n1` で確認したとおり、default の `libjpeg-turbo-rs = 0.8.0` は reserved marker の実バイトを `InvalidMarker` に渡さず、`Unsupported marker type 0xNN` を安全に再現できない。これは adapter 側で推測 remap しない恒久的な backend limitation とし、qpdf と同一の marker 診断が必要な caller は明示的な `qpdf-libjpeg-compat` feature（system libjpeg backend）を有効化する。`flpdf-401z` ではこの差が診断文言だけでなく accept/reject 自体にも及ぶことを確認し、default `PlDct` に `ScanlineDecoder` 前の marker pre-pass を追加して reserved marker を flpdf 固有の `unsupported JPEG marker 0xNN` で reject する。これにより qpdf 相当の system libjpeg（`qpdf-libjpeg-compat`）と同じ reject 判定になり、既存の非 reserved-marker JPEG の bytes は変更しない。exact な `Unsupported marker type 0x02` 文言が必要な caller は引き続き compat backend を使う。

`/ID` が qpdf と非 parity だった原因は **アルゴリズム**（qpdf は 2 段階 MD5 で seed を
作る）であり、Pipeline 抽象の有無ではない。flpdf は全体をバッファするので任意の
バイト範囲をダイジェストできる。`--deterministic-id` の byte-parity は
`deterministic_id_qpdf_parity_tests` で既にゲート済み。

`QPDF_Stream::filterable` の filter factory lookup（`QPDF_Stream.cc:419-435`）は
`/DecodeParms` の読み取り（`:439-459`）より先に完了する。この順序は、現行の
canonical な `ObjectHandle::prepare_stream_filter_plan` route で保持している。旧
Dictionary/value adapter と object-shaped production reader は legacy object-model route
の削除（`d18ce346`）で除去済みである。未知フィルタでは planner が unfilterable を返し、
`ObjectHandle::get_stream_data` は qpdf の `getStreamData` と同じ
`getStreamData called on unfilterable stream` を返す。pipe caller は qpdf と同じく raw
source pathへ進める。未知フィルタと長さ不整合を組み合わせても、factory判定が先に行われるため
`/DecodeParms` の長さ検査は実行しない。qpdf 11.9.0 の既存 fixture
`tests/fixtures/test_driver/stream_unsupported_filter_skips_decode_parms.pdf` と
`.out` を oracle/golden とし、`scripts/qpdf-test-driver-diff.sh --check` で51 fixture・
11 CLI probe の一致を確認する（`flpdf-vatj`）。

## 7. ドキュメント / オブジェクトヘルパー

Outline helper の live accessor は、Rust では外部の
`&mut OutlineDocumentHelper` を受け取るため、各 accessor の入口で
`ObjectHandle::belongs_to_pdf` による所有 PDF の一致を確認する。これは
qpdf の `QPDFOutlineObjectHelper::Members::dh` が private かつ生成時に
固定されることに対応する Rust 側の ownership guard であり、named
destination 解決や既存の synthetic `Pdf::set_object` bridge の挙動を
変更するものではない。

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFAcroFormDocumentHelper.cc` | 1047 | `acroform_document_helper.rs`(217-590: `analyze` / `traverseField` 相当の live `ObjectHandle` association cache、direct Widget の orphan fallback、`invalidateCache`、`removeFormFields` の forward-map 起点 cache cleanup; 862-943: frozen-cache `addAndRenameFormFields`; 1229-1337: `getNeedAppearances` / `setNeedAppearances` / `generateAppearancesIfNeeded`) + `page_annotation_flatten.rs`(596-612: `/Fields` guard 後の Widget identity gate) + `page_object_helper.rs`(foreign `copy_annotations_from` が page `/Annots` を append した後、増分field-tree登録で被覆できないWidgetがある場合だけ共有AcroForm cacheを無効化) + `acroform_document_helper.rs`(`disableDigitalSignatures` consumer) + `acroform_document_helper.rs`(`transformAnnotations`、`DrMap`、`/DA` の resource-name replacement consumer) + `overlay_appearance_stream.rs`(720: `adjustAppearanceStream`、AP stream consumer) | 🔀 canonical constructor now eagerly analyzes (`QPDFAcroFormDocumentHelper.cc:14-21`) and `Pdf` retains the live association cache across sequential helper facades, matching `QPDFJob::get_afdh_for_qpdf` (`QPDFJob.cc:1847-1856`); foreign transform keeps one per-annotation copy loop; stream kids propagate `stream objects cannot be cloned`; non-dictionary `/Parent` follows qpdf warning/return semantics (`QPDFFormFieldObjectHelper.cc:36-47`). The former survey/placement helpers were removed; canonical transform/copy remains in `acroform_document_helper.rs` and `page_object_helper.rs`. `page_annotation_flatten.rs`'s `remove_acroform` invalidates that shared cache after removing `/AcroForm`, matching `QPDFJob.cc:2141-2193`'s discipline that `flattenAnnotations` uses its own scope-local `QPDFAcroFormDocumentHelper` (discarded on return) rather than the `run()`-level shared `afdh`, so a later `flattenRotation` step's `make_afdh()` always re-analyzes the post-removal state instead of observing the pre-removal association. Foreign copy preserves the warm cache when `addAndRenameFormFields` has registered every copied Widget through the new field trees, and invalidates it only for an unregistered orphan Widget; the page-selection field-tree-only route retains its separate deferred invalidation boundary. |
`test_driver.cc:1551-1609`/`:1611-1629` の AcroForm consumerも、`get_form_fields`（terminal fieldのみを `field_to_annotations` の ObjectRef順で返す。direct orphanはqpdfの`QPDFObjGen(0,0)`に合わせてnull handleを1件返す）、`get_annotations_for_field`、`get_widget_annotations_for_page`、`get_field_for_annotation_handle` という同じlive cacheの公開handle経路へ接続した。`run_test_43` は qpdf のfield metadata・親chain・page Widget・appearance選択を、`run_test_44` は `setV` 相当のlive mutationとQDF writerをそれぞれ呼び出す。旧 `fields()` のraw `/Fields` preorderをconsumerで代用していない。 |
`qpdf/test_driver.cc:2761-2805` の test 80 は、`run_test_80` が `PageDocumentHelper::get_all_pages`、`AcroFormDocumentHelper::transform_annotations`、live `/Annots` append、`add_and_rename_form_fields`、foreign `PageObjectHelper::copy_annotations_from`、`PdfWriter` の QDF/static-ID 出力へ順に接続する。pinned qpdf 11.9.0 の fixture `flpdf-qtest/vendor/qpdf-qtest/qpdf/{appearances-1.pdf,appearances-1-rotated.pdf,minimal.pdf}` と golden `test80{a,b}{1,2}.pdf` に対し、stdout は `test 80 done\n`、stderr は空、exit は 0、a/b の4出力は byte-identical。foreign `/DR` の eager `copyForeignObject` と field clone 先行も qpdf の allocation order（`QPDFAcroFormDocumentHelper.cc:729-737,811-823,914-917`）に合わせ、QDF の `%% Original object ID` まで一致させる。

`flpdf-ihyup.1` では、page-selection のAcroForm field pruningを qpdf の raw identity境界へ寄せた。`QPDFFormFieldObjectHelper::getTopLevelField` は `QPDFObjGen::set` を使って live `QPDFObjectHandle` を返し、`N G R` のgeneration範囲へ射影しない（`QPDFFormFieldObjectHelper.cc:35-46`）。flpdf は `FormFieldObjectHelper::get_top_level_field_handle` と `AcroFormDocumentHelper::top_level_field_handles` をcanonical routeとし、`job/page_specs.rs::collect_primary_fields` は `QpdfObjGen`集合で候補と `/Fields`順を照合する。従来の `ObjectRef` projectionは compatibility viewに残るが、production page-selectionからは raw generation 65535の `ObjectRef` projection errorを除去する。
| `QPDFPageObjectHelper.cc` | 1039 | `page_object_helper.rs`(766) + `pages.rs`(98: inherited `/MediaBox`/`/CropBox`/`/Resources`/`/Rotate` lookup) + `page_form_xobject.rs`(637) + `resources.rs`(1229: `ResourceFinder` を使う resource pruning consumer) + `page_annotation_flatten.rs`(596-612: field-associated Widget のみ `/DR` を appearance resources に merge) + `job/overlay.rs`(2228: `placeFormXObject`) | 🔀 `pages.rs` の terminal chase は parsed qpdf child reference の意味ではなく、一時的な `Pdf::set_object` bare-reference bridge の互換境界だけをカバーする。qpdf の `QPDF::replaceObject` は indirect replacement を拒否する（`QPDF.cc:1986-1991`）ため、その bridge cycle の synthetic-null fallback を qpdf の null-as-absent inheritance と解釈しない。⚪ `resources.rs` の `form_xobjects_in_resources`/`remove_unreferenced_resources_in_form_xobjects` も同じ理由で `/XObject` category の Form 判定を `Pdf::resolve_to_terminal` で終端まで辿る（対応物なし、`optimization/inherited_attrs.rs` の同種補償と同じ形）。|
| `QPDFFormFieldObjectHelper.cc` | 852 | `form_field_object_helper.rs` + `form_field_object_helper/rendering.rs` + `default_appearance.rs`（field lookup/mutation と Tx/Ch appearance generation。`QPDFFormFieldObjectHelper.cc:472-478` に従い Btn appearance は production dispatch から除外）。既存 `/AP/N` は qpdf の `ValueSetter` 相当を同じ streamに登録する `AppearanceTokenFilter`（qpdf `QPDFFormFieldObjectHelper.cc:766-852`）で更新し、state dictionaryの`/AS`選択も`AnnotationObjectHelper`へ委譲する。新規APはqpdfどおり`/ProcSet`だけを初期Resourcesに置き、fontは既存AP `/Resources`→`/AcroForm /DR`で実際に見つかった場合だけ同じhandleを追加する（qpdf `:779-849`）；見つからないFont合成・`/FormType`追加は行わない。encodingはqpdf `QUtil`のASCII/WinAnsi/MacRomanを選ぶ。CLI の `generate_missing_appearances` は non-`/Btn` を `/AP/N` の有無で skip せず canonical helper へ渡す（qpdf `QPDFAcroFormDocumentHelper.cc:393-415`）。`crates/flpdf-cli/tests/cli_acroform_transforms.rs::generate_appearances_tx_reuses_existing_ap` は `/NeedAppearances true` の既存 stream を `--compress-streams=y` で再書き込み、`DecodeLevel::Generalized` 後の `/Tx BMC`/`Tf` と no-wrapper source preservation を確認する。qpdf 11.9.0 pinned source と `/usr/bin/qpdf` の live probe でも同じ入力の既存AP、無`/DR`、MacRoman入力を確認済み。token-filter primitive自体の変更は本 issueのscope外 | 🔀 |
| `QPDFPageDocumentHelper.cc` | 158 | `page_document_helper.rs`(`get_all_pages` + page mutation APIs) + `page_extract.rs`(`extract_pages`/`extract_page`) + `job/page_merge.rs`(`merge_documents`)。`job/overlay.rs` の source/destination page snapshot も `get_all_pages()` を通り、`QPDF_pages.cc:39-138` 相当の repair（欠落 `/MediaBox` の Letter fallback と warning）を Form 化・placement 前に適用する。両モジュールとも `Pdf::empty()` へ委譲（`emptyPDF()` + `addPage()` の library-level 経路、doc に明記）。`Pdf::uninitialized()` は qpdf の `QPDF()` の未処理状態、`Pdf::close_input_source()` は `closeInputSource()` の入力ソース差し替えをそれぞれ canonical な resolver state として公開する |
| `QPDFAnnotationObjectHelper.cc` | 226 | `annotation_object_helper.rs` + `page_annotation_flatten.rs` | 🔀 `page_annotation_flatten.rs` の `AppearanceTarget::Bridge`/`has_bare_reference_redirect` は flpdf の一時的な `Pdf::set_object` bare-reference bridge のみをカバーする代替経路で、parsed qpdf object は one-hop/live のまま `AnnotationObjectHelper` が qpdf の `getPageContentForAppearance`（`:78-226`）を忠実に実装する。同種の bridge パターンは `QPDFOutlineDocumentHelper` 行（本表 §7、`outline_document_helper.rs`）を参照。 |
| `QPDFOutlineDocumentHelper` / `QPDFOutlineObjectHelper` | 198 | `outline_document_helper.rs`(576) + `outline_object_helper.rs`(381) | ✅ live `ObjectHandle` route: `OutlineItem.object` retains canonical identity; `OutlineItem::get_title`/`get_count`/`get_dest`/`get_dest_page` (in `outline_object_helper.rs`, implementing `QPDFOutlineObjectHelper.cc` directly) recompute fresh from the live object on every call (no caching), matching qpdf's `getTitle`/`getCount`/`getDest`/`getDestPage` (`QPDFOutlineObjectHelper.cc:47-98`), while `parent`/`kids` are captured once at construction, matching qpdf's cached `getParent`/`getKids`. `/Dest` and `/A /GoTo /D` use qpdf-shaped handle accessors; the name/string branch delegates to `OutlineDocumentHelper::resolve_named_dest` (in `outline_document_helper.rs`, implementing `resolveNamedDest`), which uses the handle-native `NameTree`, cached per session in `OutlineDocumentHelper::dest_dict`/`names_dest` (`QPDFOutlineDocumentHelper.cc:60-90`) — the same split as qpdf's `getDest()` calling `m->dh.resolveNamedDest()`; JSON consumes the handles directly. `OutlineItem` holds no `&mut Pdf<R>` (an arena entry, not a live qpdf-style object helper), so its accessors take `helper: &mut OutlineDocumentHelper<'_, R>` in place of qpdf's `QPDFOutlineObjectHelper::m->dh` reference; tree construction (`get_tree`/`build_item`) stays on `OutlineDocumentHelper` since it needs sequential `&mut Pdf<R>` access across both qpdf constructors (document-level top-level walk and per-node recursive constructor), which the arena flattens into one pass — `OutlineTree::get_outlines_for_page`'s `by_page` cache stays on the arena-lifetime `OutlineTree` rather than moving to `OutlineDocumentHelper::initialize_by_page`, since `Pdf::outline()` mints a fresh `OutlineDocumentHelper` per call and a cache there would never hit. The narrow terminal-handle chase only covers flpdf's temporary `Pdf::set_object` bare-reference bridge; parsed qpdf objects stay one-hop/live. |
| `QPDFPageLabelDocumentHelper.cc` | 134 | `page_label_document_helper.rs`(1037) + `nntree.rs` (`NumberTree`) | ✅ canonical ObjectHandle route for `hasPageLabels`, `getLabelForPage`, `getLabelsForPageRange`, and `pageLabelDict`; `flpdf-25qd` moves page-operation reconstruction consumers to raw `/S`/`/P` handles, while typed compatibility APIs and JSON migration remain separate |
`flpdf-hrgj` closes the remaining page-operation consumer boundary: primary
raw label copies register their foreign-map provenance as
`WriterObjectOrderKey::primary` for QDF Original object IDs, while
foreign/split-page labels preserve indirect handles so the canonical writer
ownership check reports qpdf's error. The source/probe basis is
`QPDFJob.cc:2511-2593,2960-3010` and `QPDFWriter.cc:1072-1082,1774-1787`;
typed `LabelRange` inspection projection remains the bounded compatibility view from
`flpdf-1j3p`; the qpdf-less rendered display-string bridge was removed by
`flpdf-3yn9.48.101`.

2026-09-15（`flpdf-hzv1w`）では、qpdf の `QPDF::getRoot`（`QPDF.cc:2355-2368`）が
要求するのは Catalog 辞書であり indirect identity ではないことを反映し、page-label
の semantic Catalog read/write（`page_label_document_helper.rs` の
`hasPageLabels`/再構成、raw 再構成）と `QPDFJob::handleTransformations` 相当の
`--set-page-labels`/`--remove-page-labels` を `Pdf::root_handle()` 経由へ揃えた。
これにより direct `/Root` でも live Catalog を変更できる。`root_ref()` は
identity/numbering 用の参照取得に限って残し、全 caller inventory（production 99、
24 files / test 196）は consumer/identity residual として分類した。fixture
`compat/direct-root-one-page.pdf` の `1:D`・`1:r`・`1:A` は qpdf 11.9.0 と
status/stdout/stderr および output bytes が一致する。

| `QPDFNameTreeObjectHelper` / `QPDFNumberTreeObjectHelper` / `NNTree.cc` | 1394 (`34-75,106-168,216-390,391-520,560-700`) | `nntree.rs`（shared canonical `ObjectHandle` engine + handle-native public `NameTree`/`NameTreeCursor` and `NumberTree`/`NumberTreeCursor`）+ consumer adapters。qpdf の live `QPDFObjectHandle`/`QPDF_Array` mutation（`NNTree.cc:34-75` の iterator value 更新、`:106-168` の limits、`:216-390` の split/insert、`:391-520` の remove/deepen、`:560-700` の find）に対応し、`ResolvedArray` は `ObjectHandle::set_array_items` で alias を保持したまま更新、direct kid の indirect 化は `Pdf::make_indirect_from_object_handle`、root split は既存 root slot を維持する。canonical handle graph の live mutation を writer がそのまま観測する。public NameTree/NumberTree helpers now keep root・key/value・cursor mutation on live handles; the shared engine is entirely handle-native; no raw Object fixture, projection, or bare-reference compatibility route remains | 🔀 |
| `QPDFEmbeddedFileDocumentHelper.cc` | 122 | `embedded_files.rs`(678) | ✅ D1 完成（`flpdf-jzy7`）: `has_embedded_files`/`get_embedded_files`/`get_embedded_file`/`replace_embedded_file`/`remove_embedded_file` が `QPDFEmbeddedFileDocumentHelper.hh` の公開 API と 1:1 対応。モジュール doc の自己申告も更新済み。D2 は未達のまま — `job/json_sections.rs` の `build_attachments_section` はこのヘルパーを経由せず `NameTree` を直接歩く（`flpdf-q2fo` で解消予定） |
| `QPDFFileSpecObjectHelper` / `QPDFEFStreamObjectHelper` | 280 | `filespec_helper/filespec.rs` + `filespec_helper/embedded_file_stream.rs` + `filespec_helper/shared.rs` | ✅ D1 完成（`flpdf-d9sq`）。2026-08-23 の `flpdf-3yn9.34` で qpdf の2 helper責務へ物理分割し、high-level attachment file I/O は `job/attachments.rs` に移設した。FileSpec/EFの読み書き・stream decodeはcanonical `ObjectHandle`とprovider pathを維持する。D2 は未達のまま — `job/json_sections.rs::filespec_dict_to_json` が `FileSpec`/`EmbeddedFileStream` を経由せず同じ Mac/DOS 優先順位ロジックを再実装している（`flpdf-q2fo` で解消予定）。旧 `copy_attachments_from`（`copyForeignObject` 以前の独自 `sanitize_imported_object` walk）は `flpdf-s5cw.7` で `QPDFJob::copy_attachments`（`job/attachments.rs`）へ置き換えられ削除済み |
| `ResourceFinder.cc` | 56 | `resource_finder.rs`（operator/name tracking、qpdf `getNames()` 相当のカテゴリ横断 flat set、resource type/offset 集約）。production consumer は `resource_replacer.rs` と `resources.rs` の resource pruning | ✅ |
| `QPDFAcroFormDocumentHelper.cc` anonymous `ResourceReplacer` | — | `resource_replacer.rs`（`ResourceFinder` の name offsets を exact-byte 置換）。production consumer は `acroform_document_helper.rs` の `/DA` と `overlay_appearance_stream.rs` の AP streams | ✅ |
| `QPDFDocumentHelper.cc` / `QPDFObjectHelper.cc` | 12 | 基底トレイトが無い | ⚪ |

新規 Tx/Ch appearance の Form XObject は、qpdf と同じく payload を先に持つ streamへ独立した辞書を構築し、`replaceDict` で丸ごと差し替える。qpdf の `QPDFFormFieldObjectHelper.cc:773-778` と `QPDF_Stream.cc:688-692` に対応し、flpdf は `form_field_object_helper/rendering.rs` から `ObjectHandle::replace_stream_dict`（`object_handle.rs:5918-5950`）を呼ぶ。これにより `newStream(data)` が一時的に設定した `/Length` は生成辞書に持ち越されない。`crates/flpdf-cli/tests/cli_tests.rs::generate_appearances_new_stream_dictionary_matches_qpdf` が pinned qpdf 11.9.0 の `--generate-appearances --show-object` stdout/stderr/status を直接比較する。

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
| `QPDFObjectHandle::getJSON` / `QPDFObjectHandle::writeJSON`（行数は §1 の `QPDFObjectHandle.cc` に計上済み。ここは所在の相互参照） | — | `object_handle.rs` の `ObjectHandle::get_json` / `ObjectHandle::write_json`（`QPDFObjectHandle.cc:1613-1647` の外側 dispatch と `libqpdf/qpdf/JSON_writer.hh:16-135` の pipeline 境界）、`json_inspect.rs` の `pdf_object_to_json`（getJSON false の consumer） | 🔀 canonical ObjectHandle writer は移送済み。`false` は間接 identity を先に検査して `"N G R"` を出力し、array/dictionary child は非再帰の reference dispatch、stream は `QPDF_Stream::writeJSON` と同じく dictionary のみを出力する。`true` の一段解決 primitive も writer に実装済みで、document-level `QPDF::writeJSON` の object-map は `flpdf-25kg.3.37` で cutover 済み。`.3` では `json_inspect.rs::qpdf_resolve_top_level_object` と historical stream payload が canonical handle を直接返す。`ordered_qpdf_*` は本番 bridge ではなく、既存の pipeline-write 境界テスト専用で保持する |
| `QPDF_Stream::writeStreamJSON`（行数は §1 の `QPDF_Stream.cc` に計上済み。ここは所在の相互参照） | — | `object_handle.rs` の `ObjectHandle::write_stream_json`（`QPDF_Stream.cc:207-295` の mode validation、`no_data_key`、二重試行、dict normalization、payload routing、effective decode level） + `document_json.rs` の object-map framing / side-file ownership | ✅ `flpdf-3yn9.9` で qpdf の 1 関数責務へ統合。旧 `Object/Stream` payload/dict bridge は本番経路から外し、`QPDF_json.cc:917-925` 相当の consumer は canonical handle を呼ぶ。`.40` で残存していたtest-only JSON payload helperも撤去した。非 file entry は既存 flpdf の変換失敗時接頭辞を保つため canonical 結果を先に buffer 化する |

`.40` で `json_inspect.rs::qpdf_resolve_top_level_object` と historical stream payload helper は
撤去済みであり、現在のJSON stream経路は `ObjectHandle::write_stream_json` と
`document_json.rs` のみである。

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

`flpdf-gd1q` では inline JSON blob の遅延 provider も qpdf の
`StreamBlobProvider::operator()` (`QPDF_Stream.cc:96-107`) と同じく
`pipeStreamData` の false 戻り値を独自の runtime error に変換しない。provider が
throw した `Error::Internal` / runtime error はそのまま JSON blob serialization の
失敗として伝播するが、false のみの場合は qpdf と同じく blob writer を成功扱いにする。

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
| `QPDFJob.cc` | 3116 | `flpdf-cli/src/main.rs`(6796) + `job/lifecycle.rs`（JSON create/update/write、ordinary open、ordinary page inspection、JSON inspection と共有 completion、progress logger fallback、`QPDFJob.cc:429-516,843-875,1646-1714,2926-2935`）+ `job/json.rs`（`QPDFJob::writeJSON` の出力選択と `doJSON` 固定順、`QPDFJob.cc:1545-1640,3094-3115`）+ `job/json_sections.rs`（`doJSONPages` / `doJSONPageLabels` / `doJSONOutlines` / `doJSONAcroform` / `doJSONAttachments` / `doJSONEncrypt`、`QPDFJob.cc:1030-1330`） + `job/attachments.rs`（`doListAttachments` / `doShowAttachment` の info/save と completion（`QPDFJob.cc:876-927`）、`addAttachments` の provider-backed 追加（`QPDFJob.cc:2046-2087`）、および `copyAttachments` の cross-document 添付コピー（`QPDFJob.cc:2089-2135`）） + `job/lifecycle.rs`（`QPDFJob::parse_collate` の `QPDFJob::Config::collate` vector parser、`QPDFJob_config.cc:95-125`、`QUtil::string_to_ull` の `QUtil.cc:396-425`） + `job/page_specs.rs`（`handlePageSpecs` のspec解決・collate・source lifecycle・最終順序、`QPDFJob.cc:2360-2632`、single-document の post-subset AcroForm field pruning インライン処理（`QPDFJob.cc:2610-2632`、qpdf側に個別関数名は無く flpdf 側の命名 `prune_acroform_after_subset`）を含む） + `job/overlay.rs`（`handleUnderOverlay` の source/destination 全 page 取得と修復前置、`QPDFJob.cc:1937-2015`） + `job/page_merge.rs`(1117) + `job/rotate_spec.rs`（`--rotate` スペック解析、qpdf側の private `parseRotationParameter` に対応） + `job/rotate.rs`（range適用と page-helper facade） + `job/page_range.rs`（ページ範囲 mini-language、qpdf側の private `QPDFJob::parseNumrange` および public `QUtil::parse_numrange` に対応。`PageRange::parse_numrange`+`resolve` は qpdf の max=0 syntax-only 呼出しを保持し、`resolve` は同じ `QUtil::parse_numrange` を page_count で呼ぶ。qpdf も argv/config の構文検証と各変換時の解決で同じ primitive を2モード利用する） + `job/page_combine.rs` + `job/page_plan.rs`（`handlePageSpecs` の multi-input combination / single-document selection planning への分解） + `job/attachment_list.rs`（EmbeddedFiles/FileSpec/EF の traversal・metadata projection） + `job/acroform_field_prune.rs`（job boundary が委譲する canonical field-tree walk） + page 操作群 | 🔀 `job/lifecycle.rs` はJSON create/update/write、JSON read-only inspection、ordinary page-count/page-list inspectionのcanonical boundaryを移植済み。`job/attachments.rs` は添付 inspection の info/save と共有 completion、`--add-attachment` の provider-backed 追加、および `--copy-attachments-from` の cross-document コピー（`copyForeignObject` 経由、重複キーの集約 throw を含む）を移植済み。`job/page_specs.rs` はordinary multi-source `--pages` のjob boundaryを移植し、foreign copy・AcroForm field collision・PageLabels・collate orderを `job/page_merge.rs`/page helperへ接続した。single-document の post-subset AcroForm field pruning インライン処理も同じ job 層へ接続した。argv/config、通常rewrite、`--remove-attachment` orchestration、linearizationの残りconsumerは後続sliceで集約する。`showPages`/`withImages` は `job/inspection.rs` のcanonical `doShowPages` output（page identity、optional image details、content references）へ接続した。`job/page_range.rs`/`job/page_combine.rs`/`job/page_plan.rs` は2026-08-21に `flpdf-tvda` の再検証（誤検知だった非job消費者の主張を撤回）を経てjob/へ移動した。⚪ `PdfOpenOptions::verbose` / `message_prefix`（`flpdf-25kg.19`）は qpdf では `QPDFJob` の private メンバー（`QPDFJob.hh:589,608`）で、`doProcess` の verbose retry 診断（`QPDFJob.cc:1717-1791`、`doIfVerbose` `:339-345`）を reader の認証境界で出すために `password_mode` / `suppress_password_recovery` と同じ形で job ポリシーを移送した入れ物の差（挙動・出力は qpdf のまま）。`writer.rs::normalize_encryption_passwords` は `QPDFJob::maybeFixWritePassword`（`QPDFJob.hh:547`、private）の移植だが `flpdf-cli` の `main.rs` から直接呼ばれるため `pub`（rule 8 の根拠 1〜3 のいずれにも該当せず、`flpdf-xsq1` で可視性 debt として追跡）。⚪（`flpdf-3yn9.48.192`）`job/lifecycle.rs::QPDFJob::encryption_status`/`take_primary_copy_encryption`: qpdf の multi-source `--pages` は `handlePageSpecs(QPDF& pdf, ...)` がプライマリ `QPDF` を参照で受け取り in-place に変異させるため（`QPDFJob.cc:2359-2362`）、`createQPDF` の `pdf.isEncrypted()` チェック（`:449-453`、public `getEncryptionStatus`、`QPDFJob.hh:400-402`、に対応）と `writeQPDF` の暗号継承判定は同じ 1 個の `QPDF` を見るだけで済み、両者の間に明示的な受け渡しは要らない。flpdf の canonical multi-source merge はプライマリを消費し fresh target を返す（`flpdf-clq9` の逸脱、本表 §3 参照）ため `create_qpdf` はプライマリの暗号状態を `encryption_status` フィールドへ、copy-encryption donor を `primary_copy_encryption` フィールドへそれぞれ保存する。`write_qpdf` を同じ `QPDFJob` インスタンスで呼ぶ経路（`run_page_operations_with_qpdf_job`）は内部でこの state を消費するだけで足りるが（**2026-09-19 訂正**: 当初ここに併記していた `run_empty_page_extraction` は該当しない——同関数は `run_page_extraction_after_plan` へ文書を渡し、その完了経路が別の `split_job`/`write_job` を作るので、create 段の job は write 段の job ではない）、`flpdf-cli` の `run_page_extraction_from_multiple_sources` は create 段と write 段を別々の `QPDFJob` インスタンスへ分ける既存 CLI 構造（`transform_job`/`write_job` 等と同じ形）を使うため、この 2 メソッドで pre-merge snapshot を明示的に取り出す必要がある。`encryption_status` は qpdf の public getter への 1:1 対応（ビットマスクを `(bool, bool)` へ置換した入れ物の差）、`take_primary_copy_encryption` は qpdf に対応物の無い橋渡しで、**CLAUDE.md の逸脱分類 (C)**（(B) は「qpdf にある概念の入れ物を Rust の標準的な仕組みへ置き換える」枠なので、対応する qpdf 識別子が存在しない本件は該当しない）。挙動・出力バイトは qpdf のままで、単一 `QPDF` インスタンス内で暗黙に共有される state を明示的な accessor で公開するだけ。(C) の要件に従い `crates/flpdf/src/job/lifecycle.rs` の宣言直前に `// qpdf-deviation:` マーカーを置いた。 |
| `QPDFJob.cc` `createQPDF` / `doInspection` + `QPDFJob_config.cc` `jsonInput` / `updateFromJson` / `jobJsonFile` | `459-516,1646-1714; 305-309,328-332,774-784` | `job/lifecycle.rs` のJSON create/update/open/inspect/partial-init（`flpdf-25kg.5.2.1/.2`）+ `flpdf-cli/src/main.rs` の `run_json_input_inspection`、`job/check.rs::QPDFJob::check` + retained `open_job_pdf` for other routes | 🔀 `--json-input` / `--update-from-json` のJSON outputとread-only `--show-npages`/`--show-pages`/`--show-xref`はQPDFJobの一つのdocument/logger lifecycleへ移行済み。JSON input/update inspection は `run_job_inspection_on_pdf` から `remove_restrictions`/`coalesce_contents` も canonical create-stage transformation owner へ渡し、qpdf の `createQPDF` → `handleTransformations` → `doInspection` 順序を保持する。`--job-json-file` は qpdf の partial initialize 境界を `initialize_from_json_partial` で保持し、missing-output の最終診断を run/checkConfiguration 側へ委譲する。`--check`は専用のqpdf-shaped report rendererを保ち、generic summaryの二重出力を避ける。通常rewrite・rotate・page-tree選択・その他inspectionは後続Job sliceで同じ状態へ接続する。JSON主入力の `--pages` は一時PDFを経由せず、同じ文書のObjectHandle/xrefを `QPDFJob::handle_page_specs` で計画化する。qpdf 11.9.0のupdate-before-inspection順序を `cli_json_input.rs` で固定する。 |
| `QPDFJob::Config::showEncryption` / `QPDFJob::showEncryption` | `QPDFJob_config.cc:551-555`; `QPDFJob.cc:442-445,700-742,1646-1658` | `job/lifecycle.rs::open_for_encryption_inspection` + `flpdf-cli/src/main.rs::run_show_encryption` + `job/check.rs::QPDFJob::show_encryption`（top-level `--show-encryption` と native subcommandが共有） + `encryption/state.rs::EncryptionInspectionState` | 🔀 qpdfの認証前parsed encryption stateを保持し、wrong-passwordでもR/P/password/match/permission/method reportを完了する。暗号化されたdocumentのdecryption state (`EncryptionState`) は認証成功時だけ有効にし、qpdfの `User password` recovery は V<5 の owner-password pathだけで行う。`create_qpdf` の partial-open では `is_encrypted`/`requires_password` の status を `show_encryption` report より先に確定する（`QPDFJob.cc:436-448`、`flpdf-nrulb`）。 |
| `QPDFJob_config` / `_argv` / `_json` / `QPDFArgParser` | 3164 | `flpdf-cli/src/arg_parser.rs` + `SegmentHandler` / `QpdfSegmentHandler` in `main.rs` + clap | ⚪。QPDFJobの使用エラー分類は [`UsageError`](../crates/flpdf/src/error.rs) + `Error::Usage` として job lifecycle から CLI の `usage_exit` へ伝播し、`QPDFUsage` の別catch経路（`qpdf/qpdf.cc:10-23,34-39`）を再現する。CLI の入口は `std::env::args_os()` とし、qpdf argv grammar の residual/segment tokens、`--pages`/`--overlay`/attachment の path、`QPDFJob` の input description を `OsString`/raw bytes のまま保持する。UTF-8 が必要な selector・range・日付などだけを各 option parser の境界で検証し、非UTF-8 argv を `std::env::args()` の unwrap で失わない。`flpdf-v1xw` では argv token を raw bytes と `OsString` 投影の二重キャリア `RawArg` で運び、clap の parse 後に `raw_option_value` / `apply_raw_overrides` が byte-oriented な値（password 系）を raw 側で上書きする。qpdf は argv を 1 度しか走査する（`QPDFArgParser.cc:433-569`）ため、`SegmentHandler` は各 named-segment token と終端 callback を同じ走査中に `QpdfSegmentHandler` へ届け、typed state を downstream consumer へ渡す。この (B) の入れ物の差を含め、受理するコマンドラインと出力は qpdf と同じことを `cli_arg_parsing_segments.rs` と qtest `arg-parsing` 25 ケースで検証する。 |

`flpdf-749p` では、qpdf の `addChoices` value callbacks（`auto_job_init.hh:100-104`）と
`QPDFJob_config.cc:701-747,751-763` の setter が argv 順に状態を上書きする契約を、clap の
self-override へ接続した。適用先は **値がその occurrence の時点で検証済みになる
option に限る** — clap の `value_enum` か、`arg_parser.rs` の
`QPDF_REQUIRED_PARAMETER_OPTIONS` に `{...}` の choice として登録され
`invalid_required_choice_message` が occurrence ごとに検証するもの（`--stream-data`、
`--object-streams`、`--decode-level`、`--compress-streams`、`--normalize-content`、
`--newline-before-endstream`、`--flatten-annotations`、`--keep-files-open`、
`--password-mode`、`--password-file`、`--json-stream-data`、resource policy）。
`--json-key` は qpdf 自身が repeatable と明記しているため対象外。
`--pages`/`--add-attachment`/`--copy-attachments-from` の segment accumulation は
既存の `ArgParser` 境界に残す。

command 全体へ `args_override_self` を掛けない理由は 2 つある。第一に、値の検証を
clap の後で行う option では、上書きされた occurrence の検証が丸ごと飛ぶ。qpdf は
`Config::compressionLevel` を argv occurrence ごとに呼び、`QUtil::string_to_int`
（`QPDFJob_config.cc:135-139`）が最初の値の overflow を先に弾く。第二に、
`--job-json-file` のように qpdf が occurrence ごとに partial initialize を走らせる
option は、後勝ちにすると設定そのものが失われる。choice 値の option は clap が
parse 時点で検証を終えているため、この 2 つの問題がない。

入力・出力 selector も例外で、`Config::emptyInput` / `Config::replaceInput`
（`QPDFJob_config.cc:27-39,54-62`）は 2 回目の指定を usage error にする。qpdf では
どの occurrence も自分の `ArgParser::argEmpty` / `argReplaceInput` callback を通って
setter に届く（`QPDFJob_argv.cc:91-96`）ため、この判定は argv 層に置く必要がある
（clap の self-override は job に届く前に重複を畳んでしまう）。`arg_parser.rs` の
top-level token loop で 2 回目を検出し、qpdf と同じ文言・同じ exit code で返す。
qpdf は argv 順で最初に問題のあるトークンで失敗するため、この診断は即座に返さず
保留し、より前の unknown option があればそちらを優先し、より後ろの prescan 失敗
（missing parameter・invalid choice）にはこちらを渡す。

`flpdf-1qhb` では `--job-json-file` を clap の self-override 対象にせず
`ArgAction::Append` で occurrence を保持し、`QPDFJob::initialize_from_json_partial_bytes`
を argv 順に同じjobへ適用する。これは qpdf の `Config::jobJsonFile` が各 occurrenceで
`initializeFromJson(..., true)`を呼ぶ契約（`QPDFJob_config.cc:774-784`）に対応し、
input/outputなど非加算設定の重複は後勝ちではなくqpdfのusage errorとして残す。

### job-json CLI occurrence layering (`flpdf-u40ck`, 2026-09-16)

qpdf の `QPDFArgParser::parseArgs` は argv を一度だけ左から走査し、各
option callbackをその場で呼ぶ（`libqpdf/QPDFArgParser.cc:433-555`）。
`Config::jobJsonFile` はその既存Configへ `initializeFromJson(..., true)` を
重ねるため、`password`/`passwordFile`/`passwordMode` と input/output selector の
勝者・重複エラーは、JSONとCLIの相対位置を含むoccurrence順で決まる
（`libqpdf/QPDFJob_config.cc:16-62,449-460,625-697,774-784`、
`libqpdf/QPDFJob_json.cc:611-625`）。最終的なstateは `createQPDF` で一度だけ
入力へ渡され、`run` が単一のwrite/inspection consumerを選ぶ
（`libqpdf/QPDFJob.cc:428-480,513-520`）。

flpdf は `arg_parser.rs` が保持する raw residual argv を
`main.rs::preflight_qpdf_cli_events` から
`QPDFJob::initialize_from_raw_argv`（`crates/flpdf/src/job/argv.rs`、
qpdfの`QPDFArgParser::parseArgs`本体の移植）へ直接渡し、`run_job_json_files`
がそのjobをそのまま`run()`する。`JobJsonFile`、empty/input/output/
replace-input、password/password-file、password interpretation、recovery、
check-linearizationはすべて`initialize_from_raw_argv`内部の同じargv scanが
左から適用し、`main.rs`側の個別event再構築は行わない
（`crates/flpdf-cli/src/main.rs::preflight_qpdf_cli_events` / `run_job_json_files`、
`crates/flpdf/src/job/argv.rs::initialize`）。新しいargv parser、JSON schema、
qpdf非対応bridgeは追加していない。2026-09-19（`flpdf-3yn9.48.189`）で、旧来の
手書きイベント列挙（`main.rs::qpdf_cli_events`/`JobJsonCliEvent`、curated 26
optionのみ認識）をこの直接呼び出しへ置き換えた——`--repair`
（qpdf非対応の唯一のtop-levelフラグ、`libqpdf/qpdf/auto_job_init.hh`の124
option registryに存在しないことを確認済み）のみargvから除外し、
`--password-file`は`--job-json-file`不在時のみ除外する（そのConfig
callbackはargv scan中に即座にファイルを読む2つの副作用持ちcallbackの一つ、
もう一つは`jobJsonFile`自身——discardされる検証passで実行すると、後段の
clap駆動経路が同じファイルを再度読み、警告が二重出力される）。

`crates/flpdf-cli/tests/cli_job_json.rs` の
`job_json_file_password_follows_argv_order`、
`job_json_file_password_file_follows_argv_order`、
`job_json_file_password_mode_follows_argv_order`、
`job_json_file_password_is_hex_key_follows_argv_order`、
`job_json_file_empty_input_selector_follows_argv_order` と、
`crates/flpdf/src/job/lifecycle.rs::tests::partial_job_json_preserves_preconfigured_qpdf_state`
が、qpdf 11.9.0とのstatus/stdout/stderrと共有Configの差分を固定する。

### job-json selector usage boundary (`flpdf-n9q36`, 2026-09-16)

qpdf の `Config::jobJsonFile` は `initializeFromJson(..., true)` の失敗だけを
その JSON file の文脈で扱う（`libqpdf/QPDFJob_config.cc:774-784`）。その後の
位置 input/output や `--empty` / `--replace-input` は同じ argv scan の通常 callback
から Config setterへ入り、`QPDFArgParser::usage` → qpdf CLIの usage exit の
bare usage 境界を通る（`libqpdf/QPDFJob_argv.cc:71-82,402-430`、
`qpdf/qpdf.cc:12-22,37-38`）。したがって JSON より後ろの selector 重複は
`error with job-json file` の文脈を持たず、JSON より前の selectorに対する
重複を JSON handler が検出した場合だけ job-json 文脈を持つ。

flpdf の `run_job_json_files` は、JSON file eventでは既存の
`format_job_json_error` を使い続け、CLI selector eventでは `QPDFJob` setterが
返す typed `Error::Usage` をそのまま外側の `find_usage_error` → `usage_exit`へ
渡す。この差分は診断の所有境界だけを直し、共有 Config、argv occurrence順、
exit code、`For help:` blockは変更しない。`cli_job_json.rs` の
`job_json_file_selector_errors_follow_argv_order` が input/output の JSON前後
4ケースを qpdf 11.9.0 と status/stdout/stderrで固定する。新しい bridgeや
qpdf-deviation markerは追加しない。

### job-json file-open error boundary (`flpdf-tyu7s`, 2026-09-16)

qpdf の `Config::jobJsonFile` は JSON の read と partial initialization の両方を
同じ try/catch で包み、失敗を `error with job-json file <path>:` の文脈へ
変換する（`libqpdf/QPDFJob_config.cc:774-784`）。その例外は argv parser の
usage boundaryを通り、CLI は `Run <prog> --job-json-help` と `For help:` block
を含む qpdf 形式で報告する（`libqpdf/QPDFJob_argv.cc:408-415`、
`qpdf/qpdf.cc:11-23,32-41`）。

flpdf の `JobJsonFile` event は従来、read failureだけを一般の
`error_with_file`へ渡していた。これを既存の `qpdf_json_input_open_error`で
`open <path>` と portableな strerrorへ正規化してから
`job_json_event_error`へ渡すようにし、JSON parse/config failureと同じ
job-json reporting boundaryへ揃えた。`cli_job_json.rs::job_json_file_missing_reports_job_json_context_and_usage`
が missing fileの exit/stdout/stderrを qpdf 11.9.0と比較する。新しい
file-error wrapper、bridge、qpdf-deviation markerは追加しない。

### argv-order parse validation (`flpdf-godwa`, 2026-09-16)

qpdf の `QPDFArgParser::parseArgs` は argv を左から一度だけ走査し、各 option の
required parameter / choices 検証と callbackをその occurrenceで実行する
（`libqpdf/QPDFArgParser.cc:433-551`）。main option tableでは `rotate`、
optional `collate`、`json`、`json-output` がそれぞれ callbackまたはchoicesとして
登録されている（`libqpdf/qpdf/auto_job_init.hh:108,113,126-127`）。
`Config::rotate` と `Config::collate` は setter内で直ちに usageを投げ、
`Config::jobJsonFile` はファイルreadとpartial JSON初期化をその場で実行する
（`libqpdf/QPDFJob_config.cc:95-125,253-263,312-325,774-784`）。

flpdf は既存の raw residual argv projectionを拡張し、これらの parse-time eventを
`JobJsonFile`・selector stateと同じ順序で preflightする。optional JSON choicesは
qpdfの choice errorを作り、rotate/collateは既存の `QPDFJob::Config` parserへ渡す。
これにより `--rotate=91 --json-output=1`、
`--job-json-file=<missing> --rotate=91`、JSON/collateとの順序逆転で、最初に
失敗する qpdf callbackの診断境界を保持する。対象は argv parse validationであり、
job-JSONの変換オプション適用は別issue `flpdf-uwu7`に残る。

`cli_job_json.rs::top_level_parse_errors_follow_qpdf_argv_order` が8ケースの
exit/stdout/stderrをqpdf 11.9.0と比較する。新しいargv parser、bridge、
qpdf-deviation markerは追加しない。

### job-json prepared job state (`flpdf-7wct0`, 2026-09-16)

qpdf の CLI は 1 つの `QPDFJob` に `initializeFromArgv` と `run` を続けて
呼び、`jobJsonFile` は同じ Configへ partial JSONを重ねる
（`qpdf/qpdf.cc:27-44`; `include/qpdf/QPDFJob.hh:78-90`;
`libqpdf/QPDFJob_config.cc:774-784`）。JSON `passwordFile` は通常の
`Config::passwordFile` callbackとして、その jobの stateへ先頭行を保存する
（`libqpdf/qpdf/auto_job_json_init.hh:29-31`; `libqpdf/QPDFJob_config.cc:661-680`）。

flpdf の argv-order preflight は、JSON bytesだけを別の jobへ再生するのをやめ、
preflightで構築した同じ `QPDFJob`を `run_job_json_files`へ moveする。CLIの
`PasswordFile` eventも preflight中の occurrence位置で一度だけ適用するため、
JSON内の passwordFile と FIFO/可変/一時 side fileを二重に読まない。既存の
JSON byte cacheと実行側の再初期化は撤去し、logger/policyを準備済み jobへ設定後、
qpdf同様に1回だけ `run`する。image transformation wiringは `flpdf-uwu7`の
別責務に残す。`cli_job_json.rs::job_json_password_file_is_read_once_like_qpdf`
は Linuxで1回だけ値を流す FIFOを qpdf 11.9.0 と flpdfへ渡し、両方が timeout
なしで成功することを固定する。新しい parser、side-file cache、bridge、
qpdf-deviation markerは追加しない。

### argv-order immediate callback validation (`flpdf-sk77s`, 2026-09-16)

qpdf の main option table は `addRequiredParameter` / `addOptionalParameter` /
`addChoices` で callback と choice 集合を登録し、`QPDFArgParser::parseArgs` は
required parameter / choices を確認した直後に callback を同じ argv occurrenceで
実行する（`libqpdf/qpdf/auto_job_init.hh:92-127`;
`libqpdf/QPDFArgParser.cc:433-555`）。`compression-level`、`ii-min-bytes`、
`keep-files-open-threshold`、`oi-min-area`/`height`/`width`、`split-pages` は
`QUtil::string_to_int/uint` を callback 内で直ちに呼び、`show-object` は
object/generation selectorを同時点で parseする（`libqpdf/QPDFJob_config.cc:95-139,232-235,350-353,597-609,766-770`;
`libqpdf/QPDFJob.cc:929-941`; `libqpdf/QUtil.cc:388-425`）。callbackのruntime
errorも argv parser の `QPDFUsage` boundaryへ戻る
（`libqpdf/QPDFJob_argv.cc:408-415`; `libqpdf/QPDFArgParser.cc:337-344`）。

flpdf は既存の raw residual argv projectionにこの8 optionの eventを追加し、
prepared `QPDFJob` の preflightで左から同じ順に検証する。numeric conversion
failureは `usage_exit`へ渡し、image thresholdは comma-listの collate parserを
流用せず qpdfの direct unsigned conversionを通す。split/threshold/show-objectの
設定は同じ prepared jobへ保持し、JSONと後続 routeの検証が後ろの job-json error
に追い越されないようにする。`cli_job_json.rs::top_level_parse_errors_follow_argv_order`
と `top_level_image_thresholds_keep_qpdf_unsigned_prefix_semantics` が qpdf 11.9.0
との exit/stdout/stderr を比較する。新しい argv parser、bridge、side-file cache、
qpdf-deviation markerは追加しない。

### job-json directory read diagnostic (`flpdf-jhaqf`, 2026-09-16)

qpdf の `QPDFJob::Config::jobJsonFile` は `QUtil::read_file_into_string` の
例外を job-json contextへ包むが、directoryを特別扱いする分岐は持たない
（`libqpdf/QPDFJob_config.cc:774-784`; `libqpdf/QUtil.cc:490-525,1167-1214`）。
Linux の qpdf 11.9.0 で `/tmp`、`/home`、`tests`を渡すと出る
`basic_string::_M_create` は qpdf sourceに存在せず、libstdc++ の
`std::string` allocation artifactである。

flpdf は `std::fs::read` が返す `IsADirectory`を `open <path>: Is a directory`
として報告する。この directory-only branchには、toolchain依存の qpdf内部
文言を hardcodeしない理由を `qpdf-deviation` markerで記録する。missing、
permissionなど qpdfが意図している通常の strerror wordingはこの扱いに含めない。
`cli_job_json.rs::job_json_file_directory_keeps_the_portable_flpdf_diagnostic`
は Linuxで qpdfの artifactとflpdfの安定したportable診断をcharacterizeし、
両者のstatus/stdoutと各内側メッセージを検証する。新しいparserやbridgeは追加しない。

2026-09-19（`flpdf-3yn9.48.189`）: `crates/flpdf-cli/src/main.rs::preflight_qpdf_cli_events`
が `QPDFJob::initialize_from_raw_argv`（`crates/flpdf/src/job/argv.rs`）へ
直接委譲するようになったため、`--job-json-file` の open failureは
`main.rs::qpdf_json_input_open_error` ではなく
`crates/flpdf/src/job/argv.rs::job_json_file_open_error`を通る。同じ
理由（qpdf 11.9.0 の `basic_string::_M_create` toolchain artifactは再現
対象外）で同じ `IsADirectory` → `"Is a directory"` の `qpdf-deviation`
markerをこちらにも複製した——1つの deviationに対応する実装が2箇所に
分かれた状態で、片方だけ記録すると対応表が stale になるため
（CLAUDE.md 分類 (C) の記録条件）。上記のtestはこの新しいcode pathも
経由して検証する。

### job-json non-UTF-8 fatal path boundary (`flpdf-ktd5p`, 2026-09-16)

qpdf の `QPDFJob::Config::jobJsonFile` は、argv から受け取った raw
`std::string` pathで `read_file_into_string` を呼び、例外を同じ raw pathで
`error with job-json file <path>:` に包む（`libqpdf/QPDFJob_config.cc:774-784`）。
`QUtil::safe_fopen` / `QPDFSystemError` も `std::string` の path bytesを
そのまま `open <path>: <strerror>`へ連結する（`libqpdf/QUtil.cc:490-525`、
`libqpdf/QPDFSystemError.cc:5-29`）。

flpdf の `RawArg` / `path_description` はこの raw-byte境界を既に持つが、
`format_job_json_error` と `qpdf_json_input_open_error` が `Path::display()`へ
戻し、さらに `CliExitError.message: String` がその結果を固定していた。
`Error::SystemBytes` / `raw_message`（`flpdf-8k4e`）を inner errorに再利用し、
job-json専用の raw exit carrier と byte formatter を通すことで、outer context、
inner open message、`For help:` blockを qpdfの出力順のまま保つ。通常の
`CliExitError` callersは変更しない。`cli_job_json.rs::job_json_file_missing_preserves_non_utf8_path_bytes`
は Linux の `OsStringExt::from_vec` で `\\xff\\xfe`を含む pathを qpdf 11.9.0
と比較し、status/stdout/stderr全体と raw bytesの保持を固定する。新しい
parser、bridge、qpdf-deviation markerは追加しない。

| `QPDFLogger.cc` | 255 | `logger.rs`（private stdout tracker、shared info/warn/error/save routes、standard stdout/stderr/discard、reset/following、save collision、custom sink ownership）+ `reader/resolver.rs` / `reader.rs`（文書 warning の append-then-route、suppression、live logger replacement）+ `flpdf-cli/src/main.rs`（下記 qpdf-equivalent consumers） | ✅ `QPDFLogger.cc:9-40,43-51,80-254`。`diagnostics.rs` は logger ではなく collection-only value store として維持する |

`QPDFArgParser` の help-table 境界は、`flpdf-cli/src/arg_parser.rs` の raw/canonical 二重 argv と
`flpdf-cli/src/main.rs` の `qpdf_sole_help_topic` / `qpdf_compat_help_usage_error` に接続した。
`QPDFArgParser.cc:433-555` と `qpdf/qpdf.cc:10-39` に対応し、qpdf 互換の top-level では
`--help=usage` / `--help=exit-status` の source-derived body、expanded argv の sole-option
判定、first unknown の argv 順、single-dash の原文診断を保持する。`flpdf help <subcommand>` と
`flpdf rewrite --help` は native clap surface として別の dispatch 境界に残す。help topic の
related-option と footer は `libqpdf/qpdf/auto_job_help.hh`（qpdf 11.9.0 pin）に対応し、未移植の
topic body は後続の parity slice として扱う。

2026-09-17（`flpdf-3yn9.48.147`）: `QPDFJob::initialize_from_raw_argv` を追加し、
`initialize_from_argv` は同じbyte-preserving parserへ委譲するようにした。main/pages/
encryption/underlay-overlay/attachment/copy-attachment/page-label option table、
`@argfile`、top-level `--` reset、job-json occurrence order、Unix non-UTF-8 argvを
既存の一つの `JobConfiguration` に反映する。qpdf 11.9.0とのwriter output differentialと
job lifecycle回帰を追加した。flpdf-cliのproduction consumerはまだこのprerequisiteへ
切り替えていないため、CLI全体のargv routeはE-17/E-21 mixedのまま別issueで扱う。

qpdf の CLI は `qpdf/qpdf.cc:27-60` の native `char* argv[]` を
`QPDFJob::initializeFromArgv`（`QPDFJob_argv.cc:418-427`）へ渡し、
`QPDFArgParser.cc:12-29,438-502` は argv token を `std::string` として扱う。
そのため Unix の non-UTF-8 path byte も `QPDFJob_config.cc:16-50` の input/output
name、`QPDF.cc:245-255` の file boundary、logger output まで保持される。
flpdf は `QPDFJob::open_with_description` / `open_document_with_description` と
`QPDFJob::input_name_bytes` をこの境界に対応させ、CLI の `args_os` から qpdf-shaped
check banner と open error を raw bytes で出力する。Windows は qpdf の
`wmain` → `QUtil::call_main_from_wmain`（`QUtil.cc:1895-1935`）と同じく Unicode
argv を UTF-8 化するため、invalid-byte の実ファイル回帰は Unix/Linux に限定する。
ただしこれは `wmain` から渡される process argv の制約であり、`@argfile` の内容には
適用されない。`QPDFArgParser::readArgsFromFile` は `QUtil::read_lines_from_file`
（`QPDFArgParser.cc:347-360`; `QUtil.cc:1260-1288`）から得た raw bytes をそのまま
後続の `std::string` argv として使うため、Windows でも argfile 内の byte-oriented
password 値は process argv とは別に保持される。

入力ソース名は qpdf の `FileInputSource::filename` / `InputSource::getName`
（`libqpdf/FileInputSource.cc:14-18,87-90`; `include/qpdf/InputSource.hh:69-74`）から
`QPDFParser` の `QPDFExc` へ raw `std::string` のまま渡され、`QPDF::warn` はその
例外を warning collection に追加して同じ `what()` を logger へ同期配送する
（`libqpdf/QPDFParser.cc:487-518`; `libqpdf/QPDF.cc:487-504`;
`libqpdf/QPDFExc.cc:19-50`）。flpdf でも `PdfOpenOptions::description`、resolver の
source description、`Diagnostic::description` を `Vec<u8>` とし、logger と
qtest-driver の output boundary が non-UTF-8 Unix path を再変換なしで出力する。
qpdf 11.9.0 の `qpdf --check` に byte `0xff` を含む path を渡した実測でも、各
warning line はその byte を保持する。

E-29（`flpdf-3yn9.47`）では、qpdf の `doProcessOnce` が `QPDF` 構築直後に
`setQPDFOptions` を呼び、`noWarn` をその文書へ適用してから `processFile` を始める境界
（`QPDFJob.cc:650-666,1695-1711`）を、`QPDFJob::open_with_description`、
`open_document_with_description`、`open_for_encryption_inspection_with_description`、
`open_job_source` と JSON seed に揃えた。CLI の overlay/underlay、copy-encryption、
encryption probe、attachment copy、page source、JSON input もこの policy を受ける。
page-spec の source だけは qpdf の close/reopen 相当の reopenable reader が必要なため
`open_page_source` が direct `Pdf::open_file_with_options` を残すが、open 前に同じ
`PdfOpenOptions::suppress_warnings` を設定する。qpdf の suppression は logger への配送だけを
止め、warning collection と completion/exit status は保持する（`QPDF.cc:328-331,488-504`）。

2026-09-15（`flpdf-rer4k`）では、JSON output route の create-stage jobにも
`passwordMode`、`passwordIsHexKey`、`suppressPasswordRecovery`、`suppressRecovery`、
`ignoreXrefStreams` を設定した。JSONのprimaryは既存どおり直接開くが、
`--copy-attachments-from` の各donorは `QPDFJob::copyAttachments` の
`processFile` 相当として `open_job_source` を通るため、これらの設定がdonorにも
適用される（`QPDFJob.cc:650-666,1695-1711,2089-2135`）。qpdf 11.9.0との
`cli_json_donor_policy.rs` differential は、4つのissue対象flagに加えて
`--password-is-hex-key`も、終了コード・stdout・stderrまで確認する。
donor認証失敗の診断は qpdf と同じく donor path 付きの `invalid password` とする。

`--encrypt` の引数表も qpdf と同じ遷移を保つ。qpdf は3番目の positional
引数または `--bits` を消費した時点で `40-bit encryption`、`128-bit encryption`、
`256-bit encryption` の option tableへ切り替え、未知・非対応引数の診断にその名前を
含める（`QPDFJob_argv.cc:173-228`, `libqpdf/qpdf/auto_job_init.hh:133-163`,
`QPDFArgParser.cc:496-502`）。flpdf は `parse_encrypt_segment` で同じ時点に key lengthを
確定し、`unrecognized_encrypt_argument` から同じ suffixを生成する。key length確定前は
qpdfの `encryption options must be terminated with --` を維持する。

`flpdf-sikc.2` では、この argv の raw `std::string` 境界を password・JSON・報告出力の
下流にも延長した。qpdf の `QPDFJob_config` は top-level/`--encrypt`/page・overlay
segment・donor password を文字列 bytes のまま保持し（`QPDFJob_config.cc:169-172,
450-453,684-697,994-1007,1088-1097`）、flpdf は CLI の `OsString` と
`PdfOpenOptions::password: Vec<u8>` へ直接渡す。JSON input の source description と
JSON stream side-file prefix は `QPDF_json.cc:796-849` の raw filename boundary に
合わせ、overlay/add-attachment verbose line は `QPDFJob.cc:1937-1982,2057-2070`、
show-linearization の dump/warning は `QPDFJob.cc:1658-1674` と
`QPDF_linearization.cc:838-860` に合わせて byte pipeline で構成する。attachment の
show/remove は qpdf の `std::string` key（`QPDFJob_config.cc:507-547`）を
`QPDFEmbeddedFileDocumentHelper` の name-tree lookup（`QPDFEmbeddedFileDocumentHelper.cc:73-82,106-119`）へ
同じ正規化で渡し、`--split-pages` の `%d`/stem/extension 合成も
`QPDFJob.cc:2940-3025` の raw output string を保つ。UTF-8 が必要な option grammar の
部分だけは従来どおり境界で検証する。

`QPDFJob::Config::keepFilesOpen` / `keepFilesOpenThreshold` は `job/lifecycle.rs` の job configuration と `job/page_specs.rs::QPDFJob::handle_page_specs` に接続した。未指定時は qpdf の `page_specs` 上の異なる source index 数を閾値（既定200）と比較し、明示 y/n はその値を優先する。CLIとjob JSONのpage-spec callerは全specのsource identity/policyを先に確定し、各secondary sourceのparse直後・次sourceを開く前に `Pdf::set_input_source_stay_open(false)` を適用する。primaryはqpdfと同じくkeep-openのまま保持する。file source は `Pdf::open_file_with_options` の reopenable readerを使い、qpdfの `ClosedFileInputSource::before`/`after` 相当で secondary source を close/reopen する（`QPDFJob_config.cc:342-353`, `QPDFJob.cc:2374-2427`, `ClosedFileInputSource.cc:18-35,97-103`）。

`--job-json-file` の page-transform fields `splitPages`、`rotate`、`removeRestrictions` は、qpdf の生成 JSON handler (`QPDFJob_json.cc:611-624`, `auto_job_json_init.hh`) と Config/Job call order (`QPDFJob_config.cc:535-540,597-609`; `QPDFJob.cc:369-411,428-520,2137-2150,2635-2651,2940-3025`) に対応して `job/lifecycle.rs` の canonical configuration から page split、rotation、security/signature mutation へ接続した。 |

`splitPages` の値は qpdf の `int` と同じく signed のまま job configuration に保持する。したがって負の非ゼロ値は `checkConfiguration` の truthy split branch を通過し、`doSplitPages` の `QIntC::to_size(m->split_pages)` (`QPDFJob.cc:2970`, `QIntC.hh:112-216`) で初めて `integer out of range converting ...` を返す。flpdf も同じ page-split boundary まで値を保持し、parser の独自 early usage error に変換しない（`QPDFJob_config.cc:597-609`, `QPDFJob.cc:567-631`; `flpdf-sp4g`）。 |

`flpdf-thb2` では、qpdf が `checkConfiguration` の途中で JSON の暗黙出力先を
`-` に確定してからsplit/stdout conflictを検査する順序
（`libqpdf/QPDFJob.cc:572-591`）に合わせ、flpdfの
`QPDFJob::check_configuration` でも `json_version` と出力ファイル未指定を
実効stdoutとして扱うようにした。これにより `json=2` と非zero
`splitPages` の組合せをwrite stageまで進めず、qpdfと同じusage errorにする。
当初は immutable snapshot の上で `implicit_json_stdout` ローカルを合成する
形だったが、`flpdf-3yn9.48.150.1` で qpdf の可変 `m->outfilename`
ライフサイクルそのものを移植し、この合成は撤去した（次項）。

`flpdf-3yn9.48.150.1` では、`QPDFJob::createsOutput`
（`libqpdf/QPDFJob.cc:528-531` = `m->outfilename != nullptr || m->replace_input`）
を `QPDFJob::creates_output` として新設し、その前提となる `m->outfilename`
の3つの書き換えを `job/lifecycle.rs` へ移植した——
(1) `checkConfiguration` の JSON 既定出力先 `-`（`:582-586`）、
(2) `writeOutfile` 入口の replace-input 一時パス代入と `-` のクリア
（`:3031-3041`）、(3) verbose `wrote file` 直後・rename 直前の再クリア
（`:3063-3065`）。`check_configuration` は (1) のため `&mut self` になる
（qpdf 側も非 const）。同じ述語が `writeQPDF` の dispatch（`:486`）と
warning summary（`:497`）で異なる答えを返すのは、この書き換えの結果であり、
stdout の bare suffix は stdout の特別扱いではない。これに伴い
`output_destination()`（JSON→`-` フォールバックを持つ独自ヘルパー）、
`write_qpdf` の `|| json_version.is_some()`、`write_configured_json` の
`filter(|path| *path != "-")`、job-JSON partial init の `require_output`
JSON 例外という4つの補償を撤去し、`configure_writer_progress` の出力名も
qpdf と同じく書き換え後の `m->outfilename` を読む。
`check_configuration` の `QUtil::same_file` 比較（`:627-631`）は暗黙の
`-` も対象にするため、cwd に `-` という名前のファイルが実在すると
input と alias して usage error になる（qpdf と同じ `stat` 比較の帰結、
`crates/flpdf-cli/tests/cli_job_json.rs` で pin）。
`complete()` の bool 引数撤去は `flpdf-cli` の `--json` 経路が
`write_qpdf`/`writeOutfile` を迂回している（`flpdf-3yn9.48.150.3`）間は
実施できないため、この issue の範囲外。

`flpdf-kt4z` では、qpdfの `QPDFJob::writeOutfile` が成功write後・replace-input
rename前に `pdf.closeInputSource()` を呼ぶ（`libqpdf/QPDFJob.cc:3068-3086`）ことを、
flpdfの保持donorまで拡張した。qpdfの `page_heap` は `createQPDF` のローカル寿命だが、
flpdfは遅延foreign stream providerのため `page_source_documents` と
`overlay_sources` をwriteまで保持する。`QPDFJob::write_qpdf` のreplace-input境界で
primary、page donor、overlay/underlay donorの全resolverをcloseし、multi-sourceと
self-overlayのcontroller状態を回帰テストで固定した。これはE-4のclose-before-rename
責務であり、qtest exceptionsとCLI/rootの経路は対象外である。

`flpdf-waly` では、classic xrefの`readTrailer` → hybrid `/XRefStm` object read →
`processXRefStream` builderというqpdfの呼出順
（`libqpdf/QPDF.cc:876-927,951-962,1038-1065`）を、flpdfの二つの診断channel間でも
保持した。`parse_xref_from_start_with_owner`のclassic hybrid段からbuilder診断を
別sinkへ分離し、合成fixtureで`stream keyword found in trailer` → `expected endobj` →
`Cross-reference stream data has the wrong size`の順序をRED/GREENで固定した。
`.48.73` ではその後、canonical ownerのlive warning sinkへ各局所診断をqpdfの
呼出境界で直接配送し、`DeferredDiagnosticsGuard`による二チャネル補正を撤去した。

`coalesceContents` も生成 handler (`auto_job_json_init.hh:311-313`)、Config (`QPDFJob_config.cc:88-91`)、変換順序 (`QPDFJob.cc:2185-2188`) に対応し、既存の provider-backed `ObjectHandle::coalesce_content_streams` を `job/lifecycle.rs` から呼ぶ。

`flattenRotation` も生成 handler (`auto_job_json_init.hh:377-382`)、Config (`QPDFJob_config.cc:204-207`)、変換順序 (`QPDFJob.cc:2190-2194`) に対応し、既存の `flatten_rotation_on_pages` (`QPDFPageObjectHelper.cc:862-991`) を `job/lifecycle.rs` から呼ぶ。`coalesceContents` の直後に配置して、qpdfのページ変換順序を保つ。

2026-09-10（`flpdf-v7vr`）では、top-level の no-`--pages` `--rotate`/`--split-pages`
consumerも `QPDFJobConfig::rotate` / `split_pages` へ raw parameterを渡し、
`create_qpdf` の rotation → `prepare_document_transformations` → `write_qpdf` の
canonical境界へ接続した。これにより `--rotate` と `--flatten-rotation` は qpdf の
`handleRotations` → `handleTransformations` 順で同じ live documentへ適用される。
旧 `run_rewrite_with_page_ops_opened` の direct `PdfWriter` routeはcaller closure後に
削除した。`--pages` extractionのpost-plan rotate/image consumerも
`flpdf-3yn9.48.94` で page-selection completion後の
`QPDFJob::apply_transformations`へ接続し、rotationを先行させてから
underlay/overlayとimage transformationを同じJob ownerへ渡すようにした。
旧 `apply_rotate_specs` / `apply_image_transformations` のproduction callerは0である。
E-12/E-13の責務はこのbounded cutoverでcanonicalへ更新したが、E-4/E-10/E-21や
qtest exceptions、route-wide parity closureは別スコープとして残る。

`--set-page-labels` / `--remove-page-labels` は、`QPDFJob_argv.cc:375-392` の
option-table、`QPDFJob_config.cc:1101-1151` の文法・typed Config、
`QPDFJob.cc:2196-2228` の変換責務を、`flpdf-3yn9.48.6.1` と
`flpdf-3yn9.48.7.1` で段階的に接続した。argv/JSON は raw prefix bytes を保持する
`PageLabelSpec` へ集約し、ordinary/native rewrite は `QPDFJob::apply_transformations`
から `PageLabelDocumentHelper::page_label_dict_bytes` を使うため、qpdf の `/S`・`/P`・
`/St` 省略規則と `/PageLabels` replacement order を共有する。旧 CLI の他の
transformation caller はこの bounded cutover の残 caller として後続 `.48.7` cohort に残る。

`generateAppearances` も生成 handler (`auto_job_json_init.hh:383-385`)、Config (`QPDFJob_config.cc:218-221`)、変換順序 (`QPDFJob.cc:2177-2180`) に対応し、既存の `AcroFormDocumentHelper::generate_appearances_if_needed` (`QPDFAcroFormDocumentHelper.cc:393-417`) を `job/lifecycle.rs` から `coalesceContents` の前に呼ぶ。

`checkLinearization` も生成 handler (`auto_job_json_init.hh:217-219`)、Config (`QPDFJob_config.cc:80-85`)、inspection順序 (`QPDFJob.cc:1646-1666`) に対応し、既存の `QPDFJob::check_linearization` を job JSON の `checkLinearization` option から呼ぶ。Config と同じく output file を要求しない inspection-only route とし、linearized check の warning/status は共有 completion へ渡す。

`flpdf-egzr.8.10` では、残る generated job-JSON handlers も同じ lifecycleへ接続した。`showLinearization`、`showXref`、`showObject`、`filteredStreamData`、`rawStreamData`、`listAttachments`、`showAttachment` は `QPDFJob::doInspection` の順序 (`QPDFJob.cc:1646-1689`) で既存の inspection/attachment primitiveへ委譲する。`copyEncryption` / `encryptionFilePassword` は認証済み donor の `writer_copy_encryption_source` を `PdfWriter`へ渡し、`compressionLevel` は `QPDFJob.cc:2847-2851` と同じ writer 開始境界で適用する。`passwordMode`、`passwordIsHexKey`、`ignoreXrefStreams`、`suppressPasswordRecovery`、`suppressRecovery` は全 job-owned input source の `PdfOpenOptions`へ伝播し、`allowInsecure` は256-bit encryptionの nested handlerで検査する。`isEncrypted` / `requiresPassword` は `QPDFJob.cc:535-557` の0/2/3を job statusへ写像し、`reportMemoryUsage` は `QUtil::get_max_memory_usage` (`QUtil.cc:1941-2002`) 相当を completion 後に stderrへ出力する。`jobJsonFile` は同じ `JobConfiguration` へ `partial=true` の再帰 dispatch を行い、各 JSON dictionary を qpdf と同じキー順で適用する。したがって添付・collate・overlay などの追記型設定は両方の文書から残り、`pages` の重複指定や入力・出力設定の重複は qpdf と同じ usage error になる。include cycle はスタック枯渇を避ける既存のRust側防御として拒否する。

`.48.2` では `QUtil::parse_numrange` (`QUtil.cc:1304-1429`) を raw-byte `qutil::parse_numrange` として先行移植し、`parseRotationParameter` (`QPDFJob.cc:369-415`) の angle/relative/raw-range stateを、job JSONではlifecycle内のNUL処理後に直接parseし、CLI configurationでは `QPDFJobConfig::rotate` 経由で同じparserへ渡す。qpdf-private parser/stateのRust visibilityは `flpdf-3yn9.48.98` でcrate-internalへ揃えた。これは rotation consumerの限定sliceであり、pages/overlay/page-plan/combine/specsの既存`PageRange` consumerは後続移行として残る。

2026-09-10（`flpdf-iym2`）では、`job/page_range.rs::PageRange` の重複 parser と
resolver を削除し、`QUtil::parse_numrange` を parse (`max=0`) と resolve
（正の page count）の唯一の実装 ownerにした（`QUtil.cc:1304-1429`、
`QUtil.hh:464`）。`PageRange::all` は qpdf の `handlePageSpecs` が omitted
range を `1-z` に置換する既定値を表し、`PageRange::parse_numrange("")` / `empty` は
explicit empty selectionとして別状態に保つ。`--pages` の raw positional
heuristicも `QPDFJob_argv.cc:253-272` と同じく、range parse → file open fallback
→元の numeric-range usage error の順へ揃えた。named `--file=` は qpdf の
`called_pages_file` / `called_pages_range` stateを変えないため、まだ positional
file が無い場合または直前に positional range を消費済みの場合だけ次の
positional tokenをfileとして扱い、positional file済み・range未消費なら通常の
range heuristicへ入る（`auto_job_init.hh:128-132`, `QPDFJob_argv.cc:243-251`）。
range errors は
`QPDFJob.cc:259-270` の source-name framingに対応する下流で保持する。
`Endpoint` / `PageRangeEntry` / `Parity` の public visibility debt、qtest
exceptions、overlay 全体の owner cutoverはこの限定sliceの対象外である。

2026-09-19（`flpdf-3yn9.48.179`）で、上記の `Endpoint` / `PageRangeEntry` /
`Parity` visibility debt を解消した。3型は `PageRange` が `raw: Vec<u8>` へ
一本化される前の構造化endpoint語彙の名残で、qpdf 側に対応物が無く
（`QUtil::parse_numrange` は `std::vector<int>` を返すのみ）、
`.claude/rules/qpdf-port-design-patterns.md` 8 の rule-8 4根拠
（qpdf 側 public 対応／`QPDFJob` public method 経由／crate doc 明記／
legitimate な pub シグネチャの支援型）をいずれも満たさず、かつ
re-export chain 以外にクレート内の呼び出しが一切無かった（`pub(crate)` に
狭めるプローブで `-D warnings` 下の `dead_code` エラー 3 件を実測し、
narrowing では閉じられないことを確認した）ため、3型と `job/mod.rs` /
`lib.rs` の re-export を削除した。page-operation/overlay の owner closure も
併せて確認した：overlay の `--from`/`--to`/`--repeat` は `QPDFJob.cc:
1827,1837,1839` と同じ page count（source/dest/source）で
`PageRange::resolve` を呼び、page-operation の `PagePlan::build` も
`QPDFJob.cc:266` と同じく source 自身の page count で呼んでおり、qpdf との
不整合は無い。`qutil::parse_numrange` 自体は rotation parser／lifecycle／
CLI から直接到達する複数 entrypoint が残るため、E-15 行の分類は `mixed`
のまま。

rotationのpage countは`QPDFJob::handleRotations` (`QPDFJob.cc:2638`) の`QIntC::to_int(size_t)`に合わせ、共有`qutil::qpdf_size_to_int`でchecked narrowingする。empty documentでも`parse_numrange(range, 0)`とsigned `pageno` filterを通過させ、先行empty guardや飽和値は置かない。

`flpdf-nv86` では、`--empty --is-encrypted` / `--empty --requires-password` を qpdf 11.9.0 と同じく「空 document は unencrypted」として無言の exit 2 にする。`run_encryption_status` は input filename を要求する前に empty-input の status resultを返し、通常の file-backed status queryの open/error/report境界は変更しない (`QPDFJob.cc:429-456,535-557`)。

`testJsonSchema` の schema 不一致は、qpdf の `doJSON` (`QPDFJob.cc:1631-1642`) と同じく、生成済みJSONを出力へ流し終えた後に固定ヘッダーと各エラーを `QPDFLogger` の error pipeline へ書き出し、ジョブを失敗させずに戻る。flpdfの `job/json.rs::validate_json_schema` はこの責務を `QPDFJob` の logger から受け取り、JSON parse / 出力 pipeline の実障害だけをエラーとして返す。

`showNpages` も生成 handler (`auto_job_json_init.hh:235-237`)、Config (`QPDFJob_config.cc:573-579`)、inspection順序 (`QPDFJob.cc:1646-1655`) に対応し、既存の `QPDFJob::show_npages` を job JSON の `showNpages` option から呼ぶ。`/Pages /Count` はページツリーを再走査せず、qpdfと同じ generic accessor のwarning/zero fallbackを通し、`check`・`showEncryption`・`checkLinearization` との複合時も `doInspection` の順序で一度だけ completion する。残る schema-valid option の未接続責務は別の bounded Job JSON slices で扱う。

`showPages` は生成 handler (`auto_job_json_init.hh:241-243`)、Config (`QPDFJob_config.cc:581-587`)、`doShowPages` (`QPDFJob.cc:842-874`) を同じ `job/inspection.rs` のcanonical routeへ接続した。各ページの `page N: obj gen R`、`content:` と `getPageContents()` のstream referenceを出力し、`withImages` (`auto_job_json_init.hh:247-249`, `QPDFJob_config.cc:654-658`) 指定時だけ `images:` のname/reference/width/heightをqpdf順で追加する。`showPages` はoutput fileを要求せず、`withImages`単独は要求を解除しない。top-level `--show-pages` も同じrouteを使い、旧来のeffective page-attribute formatterは使用しない。

`showNpages` の追補では、`QPDFJob::checkConfiguration` が JSON の暗黙 stdout を設定した後に inspection-only output conflict を検査する順序 (`QPDFJob.cc:582-595`) を保持し、`showNpages` と `json`/`jsonOutput` の併用を拒否する。bare job-JSON の非文字列値は生成 `JSONHandler` の `value at <path> is not of expected type` (`QPDFJob_json.cc:124-135`, `JSONHandler.cc:127-188`) を使用する。`check` は `JobSetter::setCheckMode` (`QPDFJob.cc:745-752`) を通して `QPDF::getRoot` の Catalog `/Type` warning・修復 (`QPDF.cc:2354-2366`) を有効にし、job logger による live warning を診断再配送と二重化しない。

`flpdf-25kg.5.3` では、qpdf 11.9.0 の `QPDF::warn` が warning を collection に追加してから loggerへ同期配送し、配送例外をcatchしない境界 (`QPDF.cc:487-504`) を、post-openの `check`／linearization／page-content検査へ適用した。check側は各lazy consumerの直前と直後のdiagnostic増加を使って、`Error::System`/`Error::Internal` がwarning sink由来の場合だけ `Operation` として返し、rootなし・壊れたPDFの構造errorは通常のreportへ集約する。これにより `getExtensionLevel` 相当の `/Extensions /ADBE /ExtensionLevel` accessorを最終diagnostic snapshotより前に実行し、late warningを順序どおり一度だけ収集してexit 3へ反映する (`QPDFJob.cc:744-803`)。`NNTree` は構造的な `QPDFExc` 相当だけをrepair warningへ変換し、node解決・`deepen`・iteratorのlogger failureはそのまま伝播する (`NNTree.cc:585-663,819-899`)。writerも `QPDFWriter::write` のroot/Catalog/extension preflightにcatchを置かず (`QPDFWriter.cc:2034-2056,2059-2184`)、full-rewrite／linearizedのCatalog snapshotとextension-level解決でlogger failureをmetadata不在やdefault levelへ変換しない。

`flpdf-nyzp` では、同じ `QPDFJob::doCheck` の try 境界に属する encryption report と linearization check の失敗も `map_in_try_error` → `finish_check_error` へ通す。これにより qpdf と同じ bare `ERROR: <what()>`、最後の単一 `qpdf: errors detected`、および logger 配送失敗の `Operation` 保持を、既存の extension/page/writer 検査と同じ job boundary で実現する (`QPDFJob.cc:752-794`)。

`optimizeImages` は `QPDFJob.cc:2151-2174` の変換順序に合わせ、inline image の外部化を先に行ったうえで、`PageObjectHelper::for_each_image(true)` が返す page/Form XObject を `job/image_optimization.rs` で走査する。`Pl_DCT.cc:249-295` 相当の JPEG 圧縮結果が元 `/Length` より短い場合だけ、元辞書を shallow-copy した新 stream に `/Filter /DCTDecode` と null `/DecodeParms` を設定し、provider として遅延登録する。qpdf 11.9.0 `image-optimization.test` の24行および最適化JPEG raw bytesを照合済みである。job JSON の生成 handler (`auto_job_json_init.hh:317-322,386-400`) と Config (`QPDFJob_config.cc:176-180,232-235,357-360,422-447`) は `job/lifecycle.rs` の `JobConfiguration` に接続し、`iiMinBytes` / `oiMin*` は qpdf の `QUtil::string_to_uint` と同じ unsigned-prefix parser を通す。`externalizeInlineImages` と `optimizeImages` を併用した場合は、明示 externalize が `keepInlineImages` より優先する qpdf の条件を保ったまま canonical image phase を一度だけ実行する。
`flpdf-wz4i` では、qpdf の `createQPDF` が image transformation と attachment add/remove/copy を同じ document lifecycle で実行する責務（`QPDFJob.cc:459-480,2046-2140,2147-2247`）に合わせ、top-level image flags を attachment mutation/inspection routesへ渡す。qpdf に対応する conflict は無いため、clap の image/attachment conflict を残さず、既存の canonical image と attachment primitives を順序どおり共有する。
top-level argvの `--externalize-inline-images` も生成 handler (`auto_job_init.hh:38-55`) と同じbare option責務を `flpdf-cli` の `Cli`/`RewriteCommand` から `ImageTransformOptions` へ接続する。`--ii-min-bytes` は `QPDFJob_config.cc:232-235` の閾値をそのまま共有し、明示externalizeのみなら `PageObjectHelper::externalize_inline_images`、optimize併用なら qpdf順の共有image phaseを一度だけ呼ぶ。qpdf `inline-images.test` の22ケースで、EOF warning/exit status、named colorspace、damaged image、threshold、nested Form、QDF bytesを同一runの `harness.log` と `qtest-results.xml` で照合する。
flpdf 側の repository-level oracle coverage は `crates/flpdf-cli/tests/cli_inline_images_transform.rs` が qpdf 11.9.0（PATH 上の実行ファイル、`--version` で版を検証）を直接実行し、8通りの externalize/optimize/keep 条件、`--pages`、nested Form、named colorspace、画像なし、damaged inline image、inclusive threshold を比較する。比較は 2 層で、page-image JSON（qpdf 出力・flpdf 出力の両方を qpdf で読む）に加えて、`--pages` / nested Form / damaged の各ケースでは `--qdf --static-id` の **whole-file byte 比較**と stderr・exit code の一致も確認する。JSON だけでは 2 種類の穴が塞げない。(1) nested Form: qpdf の `doJSONPages` は page 直下の XObject しか列挙しない（`QPDFJob.cc:1044` → `QPDFPageObjectHelper.cc:376-384`）のに対し `externalizeInlineImages` は Form XObject の resources へも再帰する（`QPDFPageObjectHelper.cc:430-434`）ため、変換の有無にかかわらず両側とも `[]` になる。inline のまま残る truth table 行と閾値超えのケースも、XObject が生成されないため同じく `[]` になる。(2) `--pages` の選択違い: こちらは配列が空なのではなく、**どちらのページを残しても同一の正規化メタデータ 1 件**になる（object 参照は正規化で落とし、異なる pixel payload は JSON に現れない）ため区別できない。いずれも whole-file byte 比較でのみ検出できる。`--qdf` は stream 圧縮を無効化するので DEFLATE 逸脱 (A) が出力に現れず、この byte 比較に `qpdf-zlib-compat` feature は不要である（qpdf 自身の `inline-images.test:93-94` も whole-file 比較を使う）。fixture は flpdf-authored で、qpdf-qtest の vendor copy や qtest-only shim は追加しない。

`flpdf-w2fk` では、top-level `--check` / `--show-*` の inspection route も、qpdf の `createQPDF` → `handleTransformations` → `writeQPDF` / `doInspection` 順序（`QPDFJob.cc:459-516,1646-1714,2147-2174`）に合わせ、既存の `ImageTransformOptions` を開いた documentへ適用してから report consumerへ渡す。これにより image transform warning、status、show-pages の object identity は変換済み document を観測する。

`flpdf-42xx` では、top-level の `--check` / `--show-*` と
`--optimize-images` / `--externalize-inline-images` の組み合わせを qpdf 11.9.0
と同じく受理する。`optimizeImages` / `externalizeInlineImages` は自分のフラグ
だけを立てて `require_outfile` に触れず（`QPDFJob_config.cc:174-180,443-447`）、
`checkConfiguration` にも両フラグを見る分岐が 1 つも無い
（`QPDFJob.cc:566-641`）ため、qpdf 側に禁止分岐が存在しない。

ただし qpdf は **inspection でも画像変換を実際に実行する**。`createQPDF` は
`handleTransformations` を無条件に呼び（`QPDFJob.cc:474`）、`writeQPDF` の
`createsOutput()` 分岐（`:484-491`）はその変換済みドキュメントに対して
inspection を走らせる。したがって inspection の観測面（stdout・stderr・exit
code）は変換結果を反映する。実測でも
`qpdf --show-npages --optimize-images qtest/qpdf/bad-data.pdf` は変換由来の
warning を出して exit 3 になる（変換なしなら exit 0）。

`flpdf-uwu7.1` では、この同じ Config state をjob-json後のargv layeringから
変更できるよう、`QPDFJobConfig::keep_inline_images` と
`ii_min_bytes`/`oi_min_width`/`oi_min_height`/`oi_min_area` を個別のmutation
boundaryとして公開した。各thresholdは `QUtil::string_to_uint` 相当の
`parse_qpdf_collate_uint`を通り、partial JSONで既に設定された他のimage stateを
保持する（`QPDFJob_config.cc:176-180,232-235,422-447,774-784`）。既存の
`externalize_inline_images(min_bytes)` と `optimize_images(options)` は通常の
configuration consumerの合成入口として残り、別のargv parserやimage pipelineは
追加しない。

`flpdf-uwu7` では、qpdfの `Config::jobJsonFile` が同じ Config に
`initializeFromJson(..., true)` を重ねた後、後続の argv callback を同じ stateへ
適用する責務（`QPDFJob_config.cc:774-784,422-447`）を、CLIの既存
`qpdf_cli_events` replayへ接続した。`--externalize-inline-images`、
`--optimize-images`、`--keep-inline-images` は既存のJSON threshold/policyを
上書きしない個別flag setter (`QPDFJobConfig::set_externalize_inline_images`,
`set_optimize_images`, `keep_inline_images`)を出現順に呼び、`--ii-min-bytes` /
`--oi-min-*` は同じConfigのthreshold setterへ直ちに渡す。これにより
`QPDFJob::handleTransformations` の外部化→最適化順序
（`QPDFJob.cc:2151-2174`）を保ったまま、job JSON後の画像argv layeringを
qpdfと同じ共有Jobで実行する。`job_json_image_optimization.rs` は、JSON後の
externalize/optimize/keepとthresholdの3組をqpdf 11.9.0と比較する。

`flpdf-w2fk` で、top-level の inspection route（`run_check` / `run_show_*` /
`--json` 経路）へ image option を配線し、report の前に変換を実行するようにした。
ただし `--show-encryption` は例外で、認証に失敗した入力では変換を行わない —
qpdf の password-error catch は `createQPDF` から `return nullptr` するため
`handleTransformations`（`QPDFJob.cc:473`）に到達しない（`:437-448`）。

attachment 系フラグ（`--list-attachments` / `--show-attachment` /
`--remove-attachment` / `--add-attachment` / `--copy-attachments-from`）との
併用は `flpdf-wz4i` で受理するようにした。qpdf 側に禁止分岐が無い
（`QPDFJob_config.cc:443-447` は自分のメンバーしか触らず、`checkConfiguration`
（`QPDFJob.cc:566-641`）に両フラグを見る分岐が無い）ためで、attachment phase は
image phase と同じ `handleTransformations` 内で後に走る（`:2151-2177` →
`:2230-2247`）。`--show-attachment` は qpdf 同様、引数確定の時点で標準出力を
save pipeline として予約する（`QPDFJob.cc:621-625`）——これにより
`setSave` が info を標準エラーへ移し（`QPDFLogger.cc:197-200`）、verbose の
image 診断が payload と混ざらない。

`flpdf-osf9` ではこの同じ job transformation boundary を
`--generate-appearances` / `--flatten-annotations` にも拡張した。qpdf は
`handleTransformations` (`QPDFJob.cc:2138-2194`) を attachment inspection・
attachment mutation・linearization inspection より前に無条件で走らせ、
`checkConfiguration` (`QPDFJob.cc:566-641`) にはこれらの組み合わせを拒否する
分岐が無い。flpdf は `InspectionTransformOptions` から
`QPDFJob::apply_transformations` を通し、`AcroFormDocumentHelper` と
`PageDocumentHelper` の canonical primitive を同じ順序で再利用する。
`crates/flpdf-cli/tests/cli_inspection_transform_combinations.rs` は qpdf
11.9.0 の list/show/add/copy/remove/linearization 組み合わせの exit・stdout・
stderr parity を固定する。

top-level `--flatten-annotations=all|screen|print` も `auto_job_init.hh:117` / `QPDFJob_config.cc:190-200` の choices を `flpdf-cli` の shared `run_rewrite` route に接続し、通常 rewrite と linearize rewrite の両方で `PageDocumentHelper::flatten_annotations` (`QPDFPageDocumentHelper.cc:55-77`) を実行する。`NeedAppearances` 時の `warnIfPossible` と stream filter warning の parsed-offset/suppression 境界も qpdf の warning/status contract に合わせる。

CLI の page-operation route も同じ順序を保つ。qpdf は `createQPDF` で
`--pages` / `--rotate` / `--flatten-rotation` を文書へ適用した後、
`writeQPDF` の `setWriterOptions` で linearization を設定する
（`QPDFJob.cc:450-507,2137-2248,2847-2945`）。flpdf は page selection と
rotation の完了後に `QPDFJob::write_qpdf` へ渡し、`--split-pages` では
`doSplitPages` 相当の各 chunk writer に同じ
linearization 設定を再適用する（`QPDFJob::write_qpdf` と
`crates/flpdf/src/job/page_split.rs`）。rewrite の linearized branch でも
`--flatten-rotation` を writer planning 前に実行する。

2026-09-10（`flpdf-0saq`）: qpdf は `writeQPDF` から `doSplitPages` に入り、各 chunk
ごとに生成する `QPDFWriter` へ `setWriterOptions` を適用する（`QPDFJob.cc:483-511,2847-2903,2939-3027`）。
flpdf の top-level / `rewrite` page-operation route も、ページ選択後の canonical writer
設定へ明示的な `--encrypt` を渡し、`--split-pages` の全 chunk を暗号化する。qpdf 11.9.0
との V4 AES-128 byte comparison を `page_ops_qpdf_matrix.rs::pages_encrypt_then_split_outputs_encrypted_chunks_like_qpdf`
で固定した。`--copy-encryption`、`--decrypt`、`--coalesce-contents` など別の未対応組合せは
この変更の対象外である。

2026-09-17（`flpdf-3yn9.48.143`）: qdf サブコマンドは qpdf の独立 writer 実装ではなく、
通常の `QPDFJob` に qdf/preserve-unreferenced writer option を設定した経路である
（`qpdf/qpdf.cc:26-44`, `QPDFJob.cc:483-511,2847-2937`）。flpdf の `run_qdf` は
直接 `PdfWriter` を呼ばず、`run_rewrite` の `create_qpdf` → `write_qpdf` 境界へ接続した。
`qdf-fix` は手編集 QDF の byte-level 修復であり、この Job cutover の対象外に保持する。

2026-09-17（`flpdf-3yn9.48.142`）: rewrite サブコマンドの `--pages … --split-pages`
も、ページ選択後の CLI-local split helper を削除して `QPDFJob::write_qpdf` の
`writeQPDF` → `doSplitPages` 分岐へ接続した。qpdf の `setWriterOptions` が最初の
chunk の page-copy 後に行われる順序に合わせ、暗号化パスワード通知と弱暗号判定は
`job/page_split.rs` の chunk writer 設定時へ移し、resource/page-copy 診断より前に
通知されないことを `cli_pages_verbose_diagnostics.rs` の qpdf 11.9.0 differential
で固定した。qpdf の `doSplitPages` は private なので `SplitPageOptions` と
`QPDFJob::split_pages` は crate 内 API に狭めた。

### Attachment mutation with split-pages output dispatch (`flpdf-8q13h`, 2026-09-16)

qpdf は attachment の add/remove/copy を `handleTransformations` 内で完了した後、
`writeQPDF` の `split_pages` 分岐から `doSplitPages` を呼ぶ。split の output path は
literal な出力先ではなく、各 chunk の番号を挿入する template として使われ、各 fresh
chunk writer に同じ writer options が設定される（`libqpdf/QPDFJob.cc:2138-2247`,
`libqpdf/QPDFJob.cc:483-492,2847-2903,2940-3027`; `libqpdf/QPDFJob_config.cc:598-609`）。

flpdf の attachment 3 経路は既に同じ `QPDFJob` の create/write boundary と canonical
attachment transformation を使っていたが、共有 `configure_attachment_job` が
`PageOpArgs::split_pages` を job configuration へ渡していなかった。既存の
`QPDFJob::write_qpdf` / `QPDFJob::split_pages` を再利用してこの設定だけを接続し、
add/remove/copy と `--pages . 1` + add の qdf whole-file parity test で qpdf の numbered
chunk output を固定する。新しい attachment-specific split writer、filename rewrite、
compatibility bridge、qpdf-deviation marker は追加しない。

### Empty primary with attachment mutation (`flpdf-c3d2x`, 2026-09-16)

qpdf の `Config::emptyInput` は `infilename` を null ではなく空文字列にして primary input
の slot を消費する（`libqpdf/QPDFJob_config.cc:27-40`）。そのため `createQPDF` は
`emptyPDF()` を作成し（`libqpdf/QPDFJob.cc:428-450,1695-1716`）、`--pages` があれば
その foreign source を取り込んだ後、通常の `handleTransformations` で attachment mutation
を適用する（`libqpdf/QPDFJob.cc:465-474,2138-2247`）。単一の positional は output file
として扱われ、qpdf の `checkConfiguration` はこの empty primary を input missing とせず
通常の output 必須判定へ進む（`libqpdf/QPDFJob.cc:567-595`）。

flpdf の attachment 3 経路も、empty 時だけ positional `(input, output)` を
`(None, output)` へ remap し、既存 `QPDFJobConfig::empty_input` と同じ empty-document
creation boundary を使用する。非 empty の input/output 判定、page spec の source、
attachment transformation、writer completion は変更しない。add/copy の file output、
stdout、`--empty --pages <source> 1` の add/copy を qpdf 11.9.0 と qdf whole-file parity
で固定し、専用の empty-document writer や sentinel は追加しない。

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

`QPDFJob::createQPDF`（`QPDFJob.cc:428-481`）は update-JSON、page selection、rotation、
under/overlay、transformationsを完了してからdocumentを返し、`writeQPDF`
（同 `:484-503`）は `createsOutput()`（同 `:529-532`）に応じて inspection / split /
writeを選ぶ。flpdfも `QPDFJob::create_qpdf` 内の `prepare_document` /
`prepare_document_transformations` と `QPDFJob::write_qpdf` にこの境界を集約し、`run` は
create→write→`get_exit_code` の合成だけを担う。multi-source page selectionのerased target
とprovider-backed source ownerはcreate stageからwrite stageまで保持する。2026-09-08
（`flpdf-8uuw`）では、通常 non-linearized rewrite の direct CLI route も
`handle_under_overlay` を image/appearance/annotation transformations より前へ移し、
qpdf の `handleUnderOverlay` → `handleTransformations` 順を repository-owned inline-image
probe と qpdf-zlib-compat byte comparison で固定した。page-operation別 route と
QPDFJob ownerへの完全統合は残る。2026-09-15
（`flpdf-tgpv7`）では、qpdf にこの拒否が存在しないことを再確認し、linearized
rewrite でも同じ canonical Job の overlay → transformation → writer route を使うようにした。

2026-09-10（`flpdf-m6kt`）: top-level no-`--pages` の `--rotate` / `--split-pages` も
`--generate-appearances` / `--flatten-annotations` を `run_rewrite_with_qpdf_job` の
`create_qpdf` → `write_qpdf` 境界へ渡すようにした。qpdf の
`QPDFJob::createQPDF`（`QPDFJob.cc:466-473`）→ `handleTransformations`
（`:2137-2194`）→ `writeQPDF` / `doSplitPages`（`:483-511,2940-3027`）の順を、
rotate/split × generate/flatten の4セルで qpdf-zlib-compatible byte differential として固定。
新規 Tx/Ch appearance は qpdf の `/Tx BMC\nEMC\n` 初期buffer + `ValueSetter`
token-filter（`QPDFFormFieldObjectHelper.cc:766-852`）を使うため、通常書込みでは生成内容、
split の foreign copy では qpdf と同じ初期bufferを観測する。Job-level の未指定 decode level は
qpdf の default generalized（`QPDFJob.hh:635-637`）を JSON/inspection 側で保持する一方、writer
へは `decode_level_set` が true のときだけ渡す（`QPDFJob.cc:2865-2875`）。したがって writer の
未指定 decode は qpdf `QPDFWriter` の default none であり、`compressStreams: "n"` でも入力暗号化を
保持する。この job/writer の状態分離を `flpdf-skim` で固定した。

`writeQPDF` は選択した処理の後に文書のopen-time/lazy warningを集約し、warning summaryと
memory reportを一度だけ出力する。終了コード3の判定は `QPDFJob.cc:534-563`、inspection側の
warning集約は `doInspection`（同 `:1646-1693`）がoracleである。flpdfでは
`write_qpdf` が `get_warnings` 相当のdrainとcompletionを行い、`get_exit_code` は logger/
documentを変更しない純粋なqueryになった。JSON versionに出力先が無い場合はqpdfの暗黙
stdout outputとして扱い、resulting-file suffixを選ぶ。既存の standalone CLI completion
consumerは別の段階移行として残る。

`.48.7` では、ordinary/rewrite の `run_rewrite_opened` を `QPDFJob` の
configuration → `apply_transformations` → `write_qpdf` 境界へ移し、qpdf の
underlay/overlay → image → appearance → annotation → coalesce → rotation →
page-label/output 順序を一つの Job owner へ集約した。残る direct CLI callers は
JSON/page-operation/inspection cohort であり、`.48.8`〜`.48.10` の後続範囲である。

2026-09-10（`flpdf-3yn9.48.76`）では、top-level `--pages` と no-output inspection
の consumer を `QPDFJobConfig::empty_input`/`add_page_spec`/`collate` と
`QPDFJob::run`（`create_qpdf` → `write_qpdf`）へ接続した。これは qpdf の
`Config::emptyInput`/`collate`、`handlePageSpecs`、`writeQPDF`/`doInspection`
（`QPDFJob_config.cc:27-40,95-125`, `QPDFJob.cc:428-511,1645-1693,2359-2633`）に
対応し、`--empty --pages ... -- --show-pages` が空の primary を直接表示して
しまう bypass を除去する。page-operation output の4つの直接 caller は依然
mixed として残り、別の bounded cutover で扱う。

`flpdf-ddk1` では、qpdf の output sink が `Pl_StdioFile("qpdf output", ...)`
（`QPDFWriter.cc:101-110`。named file でも standard output でも identifier は同じ）
として入力ファイル名とは独立した責務を持つことに合わせ、`PdfWriter` の file sink 自身が
失敗時に `qpdf output: Pl_StdioFile::write: <system message>` を組み立てる
（`Pl_StdioFile.cc:25-37`）。qpdf 非対応の sink（`set_output_writer` 等）は identifier を
持たず bare `Error::Io` のまま返す。失敗の shape を sink 側で決めるため、`write_qpdf` の
error path で I/O error を一括変換する必要がなく、writer 内部の lazy input read が返す
bare `Error::Io` の分類も壊れない。`job_error_message_with_input` の入力名装飾は
open/read/parse failure と `BadPassword` に対して従来どおり保持する。
`/dev/full` への実測で qpdf 11.9.0 と CLI 出力が一致することを
`cli_logger_routing` の differential で検証する。

`QPDFJob::handleTransformations` の `remove_restrictions` 分岐は
`QPDFAcroFormDocumentHelper::disableDigitalSignatures` を呼ぶだけで、成功時の独自
メッセージを出さない（`QPDFJob.cc:2137-2150`、`QPDFAcroFormDocumentHelper.cc:419-439`）。
flpdf CLIもこのmutationを保持し、`--remove-restrictions` による `removed restrictions`
や `removed signatures` の補助メッセージは発行しない。実際の文書warningだけが
`QPDF::warn` のcollectionと通常のcompletion summaryへ進み、`--no-warn` はその表示だけを
抑止する（`QPDF.cc:487-504`）。

`flpdf-3yn9.48.102` では、qpdf の public `QPDF::removeSecurityRestrictions` と
`QPDFAcroFormDocumentHelper::disableDigitalSignatures` がともに `void` であること
（`include/qpdf/QPDF.hh:603-607`; `include/qpdf/QPDFAcroFormDocumentHelper.hh:166-170`）に合わせ、flpdf の `Result<bool>` を
`Result<()>` へ狭めた。変更有無を観測する qpdf-less projection と `changed` の追跡は
撤去し、Rust 固有に必要なエラー伝播だけを残している。`/Perms`・`/SigFlags`・署名
フィールドの mutation 順序は変更せず、既存の10 fixture byte differentialで qpdf
11.9.0 との一致を再確認した。

`flpdf-innn` では、`WriterConfiguration::normalize_encryption_passwords` が
`maybeFixWritePassword` の user→owner 順を `PasswordWriteNotice` の列として保持し、
CLI と `QPDFJob::write_qpdf` が各noticeをその場で対応する logger pipeline へ fallibly
配送する。`write_qpdf` は `outputFile "-"` の save pipeline を診断より先に予約し、
`QPDFJob.cc:339-345,614-626,2655-2723,2750-2751` の順序・失敗伝播を維持する。

### `QPDFLogger` の CLI consumer cutover と retained direct routes

qpdf の route ownership は `QPDFJob.cc:343,498-502,625,709-925,2934,3051-3054,3094-3115`
と照合した。flpdf CLI は invocation ごとに 1 個の private `QPDFLogger` を共有し、次を
logger consumer に移行済みである。

- `save`: JSON stdout、raw/filtered stream stdout、attachment stdout、rewrite/QDF の
  output `-`。いずれも document open / info write より先に `saveToStandardOutput`
  相当を設定し、独立した stdout terminal を作らない
- `info`: check summary、show object、show pages/npages、attachment listing、encryption /
  linearization inspection、rewrite/page-operation verbose output
- `warn`: document warning、warning completion summary、normalization warning
- `error`: check error と top-level の通常 fatal error
- `usage`: `UsageError` を `usage_exit` へ直接渡し、qpdf の空行・help block付き exit-2 を再現

添付の mutation route も同じ writer 境界に接続する。qpdf は
`handleTransformations` 内で `addAttachments` / `removeEmbeddedFile` を完了してから
`writeOutfile` を呼び、`setWriterOptions` の全設定を一度だけ適用する
（`QPDFJob.cc:2137-2248,2847-2945,3029-3058`）。flpdf の
`run_all_attachment_mutations` は `configure_attachment_job` と
`run_configured_attachment_job` を通じて mutation を Job に積み、
`QPDFJob::create_qpdf` → `QPDFJob::write_qpdf` へ渡すため、
`--stream-data`、`--decode-level`、`--newline-before-endstream`、ObjStm、QDF、
encryption、decrypt、linearization、version、ID、progress を attachment output にも
適用する。`QPDFWriter.cc:1538-1564,1735-1755` の stream/object-stream framing と
QDF/linearization の writer 内優先順位もこの共通経路で保持する。

`flpdf-w0ne` では `--remove-attachment` の診断も qpdf の job logger 境界へ揃える。
qpdf は成功した removal を `doIfVerbose` で報告し、`writeOutfile` の成功後に
`wrote file` を報告する一方、missing key は `attachment <key> not found` を throw する
（`QPDFJob.cc:2230-2241,3030-3062`）。flpdf は remove route に `verbose` を渡し、key の
raw bytes を保持した info/error message と同じ completion order を使う。

上記の旧 `flpdf-5nle` 記述は `flpdf-3yn9.48.81`（2026-09-10）で
supersede された。現在の `run_all_attachment_mutations` は direct `PdfWriter`、手動
normalization、手動 warning completionを持たず、`QPDFJobConfig`へ設定して
`QPDFJob::create_qpdf` → `QPDFJob::write_qpdf`へ渡す。これにより
`handleTransformations` の remove → add → copy 順、全 writer option、donorの
per-file direct open（`open_job_source`）、stdout予約、warning summary、
`--replace-input` renameを一つのJob境界で共有する。stdout出力時のcompletion
suffixも、qpdfが `writeOutfile` 内で `outfilename` を `nullptr` にする順序
（`QPDFJob.cc:3033-3040,493-503`）に合わせる。

`flpdf-7l5e` では、名前付き JSON output も同じ completion boundary を使う。
`QPDFJob::writeOutfile` は `writeJSON` が file pipeline を閉じた直後、かつ
`writeQPDF` の warning summary より前に、明示 output path がある場合の
`doIfVerbose` `wrote file` info を出す（`QPDFJob.cc:3042-3062` の後に `:493-503`）。
flpdf は `QPDFJob::write_json_with_version` の中で、serialize 後・`complete` の
手前に同じ message を出す。出力名は `m->outfilename` と同じ raw path bytes で
書く（`Path::display()` は非 UTF-8 バイトを U+FFFD に置換してしまう）。JSON を
stdout に流す場合は qpdf 側で `m->outfilename` が null になる
（`QPDFJob.cc:3036-3040`）のと同じく output path が無いため抑止する。

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

- native `rewrite --static-id` warning: qpdf-compatible CLI surfaceではないため残る
  flpdf-only test diagnostic（出力先は qpdf-compatible logger error route）
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

`flpdf-lomd` では、canonical xref stream readの結果にも
`parsed_xref_streams` provenanceを保持するようにした。qpdfは
`read_xrefStream`の`readObjectAtOffset`で歴史的streamをobj_cacheへ置き、
effective xref tableとcache enumerationを別々に扱う
（`libqpdf/QPDF.cc:951-962,1239-1295,1640-1686`）。flpdfは最終`Pdf`構築時に
そのhandleを`qpdf_parsed_xref_stream_refs`へ登録し、free/supersededな履歴streamを
`live_object_refs`から除外する。current effective xref rowがあるObjectRefは
registrationでshadowされるため、active xref streamの可視性は変えない。
incremental RED/GREEN fixtureでobject-cache visibilityとlive filteringを確認した。

### A6/A7/A8 Job/CLI JSON accessor cohort `flpdf-3yn9.48.31` (2026-09-08)

The first bounded Job/CLI cohort keeps the existing `QPDFJob::doJSONPages` and
`doJSONEncrypt` section ownership (`QPDFJob.cc:1030-1093,1206-1279`) while
removing the caller-side `Pdf::resolve` bridge from
`crates/flpdf/src/job/json_sections.rs`. `collect_content_refs` and
`image_to_json` use the canonical `ObjectHandle::try_dereference` and
`try_as_*` accessors; the encryption projection uses `try_as_dictionary`,
`try_as_integer`, and `try_as_name`, with `effective_length_bits` retaining its
resolver-owned integer inspection.

This follows qpdf's accessor order: `as*`/`is*` dereference on entry
(`QPDFObjectHandle.cc:240-446`), `getKey`/`hasKey` resolve the dictionary holder
and preserve qpdf's type-warning fallback (`QPDFObjectHandle.cc:965-989`), and
warning or exception delivery remains at the object-handle boundary
(`QPDFObjectHandle.cc:2168-2212`). The production区画 of
`json_sections.rs` now has zero `Pdf::resolve`/`resolve_handle` calls and zero
non-resolving `.as_dictionary()`/`.as_array()`/`.as_integer()`/`.as_name()`/
`.get_key()`/`.has_key()`/`.is_null()` calls; the two stream dictionary views
remain after the resolving step as the qpdf `getDict` equivalent
(`QPDFObjectHandle.cc:1257-1262`). JSON section order, helper ownership, and
`Result` error propagation are unchanged. Remaining Job/CLI files are tracked
as later bounded cohorts under `flpdf-3yn9.48.31`.

The page-object-helper residual cohort `flpdf-3yn9.48.23.1` applies the same
boundary to `PageObjectHelper`'s resource, annotation, rectangle, matrix, and
inherited-attribute consumers. Its production route has zero explicit
`Pdf::resolve`/`resolve_handle`/`resolve_handle_ref` or panic `get_key`/
`has_key` callers. `try_as_*`/`try_is_null` are used for document-owned child
handles; direct inline-image parser values and post-resolution numeric
fallbacks retain their non-resolving inspection. The qpdf oracle is
`libqpdf/QPDFObjectHandle.cc:240-446,965-989,2168-2189`, and the new error-path
coverage verifies that an unowned indirect page handle propagates the resolver
error instead of falling through to a fallback.

`flpdf-5snx` では、qpdf の `dereference()` が未初期化 handleで falseを
返すだけであること（`libqpdf/QPDFObjectHandle.cc:2376-2383`）に合わせ、
`try_as_dictionary` / `try_as_name` は `Ok(None)`、`try_is_null` は
`Ok(false)`へ短絡するようにした。これは qpdf の `asDictionary` /
`asName`（同 `:265-268,283-286`）と `isNull`（同 `:353-356`）の
責務に対応し、unparseResolved / getJSONのような値要求経路のInternal errorや
initialized handleのresolver errorは変更しない。既存のA6/A7 consumer移行、
canonical owner、qtest exceptionsは対象外である。

### JSON pages content normalization and warning route `flpdf-wkunn` (2026-09-14)

`QPDFJob::doJSONPages` calls `QPDFPageObjectHelper::getPageContents` for each
page (`libqpdf/QPDFJob.cc:1030-1077`). That public helper delegates to
`QPDFObjectHandle::getPageContents`, whose `getKey("/Contents")` result is
normalized by `arrayOrStreamToStreamArray` (`libqpdf/QPDFObjectHandle.cc:1438-1493`).
The normalization owns both the stream list and the `qpdf_e_damaged_pdf` warning
for a non-stream array member or a non-null value that is neither a stream nor an
array; null and missing values remain empty without warning.

The former `job/json_sections.rs::collect_content_refs` projection bypassed that
ObjectHandle boundary and silently discarded malformed `/Contents` values. The
JSON pages consumer now uses `PageObjectHelper::get_page_contents` and serializes
each returned handle through the existing non-dereferencing JSON object route.
This preserves qpdf's direct/array/null normalization, canonical stream identity,
and document-owned warning sink without adding a JSON-only diagnostic or a legacy
bridge. The regression is covered by the pinned qpdf 11.9.0 CLI comparison on
`tests/fixtures/compat/chained-indirect-contents.pdf`.

### A6/A7/A8 Job/page/resource/JSON consumer slice `flpdf-3yn9.48.23.8` (2026-09-10)

The remaining production consumers in the Job page-selection, page-tree,
resource-pruning, overlay-appearance, and document-JSON modules now use the
canonical resolving ObjectHandle accessors. This matches qpdf's
`QPDFObjectHandle` entry resolution (`libqpdf/QPDFObjectHandle.cc:240-446,
759-785,965-989`) and its warning/error boundary (`:2168-2189`) without adding
a consumer-local resolver facade. Job/page ordering and resource ownership stay
anchored to `QPDFJob.cc:2251-2632`, `QPDF_pages.cc:39-150`, and
`QPDFPageObjectHelper.cc:224-263,318-399,486-649`; JSON object identity and
section ordering stay anchored to `QPDFJob.cc:958-1620,3094-3116` and
`QPDF_json.cc:852-905`.

The production route contract is zero for `Pdf::resolve`, `resolve_handle`,
`resolve_handle_ref`, non-resolving key accessors, and non-resolving dictionary,
array, integer, name, and null inspection across the eight scoped files.
`as_stream_dict` and the silent post-resolution string/real type observations
remain only where no resolving Rust counterpart exists. The focused route test
and `pages` unresolved-child regression cover caller-zero and `Result`
propagation. qtest and qtest-exceptions routes are not part of this row.

### A6/A7/A8 linearization check/show accessor slice `flpdf-3yn9.48.23.9` (2026-09-10)

The bounded linearization read-side consumers now use qpdf-shaped resolving
accessors for parameter and hint-table values. This follows qpdf's
`QPDFObjectHandle` entry-resolution contract (`libqpdf/QPDFObjectHandle.cc:
240-446,759-785,965-989,2168-2189`) and the linearization data loading/check
boundaries (`libqpdf/QPDF_linearization.cc:84-230,419-470`). The production
route contract is zero for removable non-resolving key, dictionary, array,
integer, name, null, and explicit resolve bridge calls in
`linearization/check.rs` and `linearization/show.rs`.

The existing `check-linearization`, `show-linearization`, page-operation, and
deep-linearization qpdf comparisons cover the byte/status/warning-neutral
cutover. Silent real/string observations without a resolving `try_as_*`
counterpart, writer emission, and qtest exception attribution remain outside
this bounded row.

### A6/A7 optimization accessor slice `flpdf-3yn9.48.23.11` (2026-09-10)

The optimization orchestration and inherited-page-attribute walk now use the
canonical resolving `ObjectHandle` accessors. This follows qpdf's direct
`/Outlines` normalization and ordered inherited-key push in
`libqpdf/QPDF_optimization.cc:70-78,117-187,190-245`, together with the
entry-resolution and key/null fallback contract in
`libqpdf/QPDFObjectHandle.cc:240-446,965-989,2168-2189`.

The production route contract is zero for `Pdf::resolve`, `resolve_handle`,
`resolve_handle_ref`, and panic `get_key`/`has_key` in
`optimization.rs` and `optimization/inherited_attrs.rs`. Inheritable keys are
looked up on the live dictionary, indirect values are resolved before the
qpdf null-as-absent test, and direct/indirect page-tree traversal keeps its
existing order and mutation boundary. The heap-backed traversal remains the
documented container-only deviation from qpdf's recursive call stack. qtest
exceptions and the active CLI, writer, and stream routes remain outside this
bounded row.

### A6/A7 AcroForm field-prune accessor slice `flpdf-3yn9.48.23.12` (2026-09-10)

The page-subset AcroForm field-prune consumer now uses live
`ObjectHandle` resolution at each dictionary, array, and scalar observation.
This follows qpdf's `handlePageSpecs` page-null/field-retention boundary
(`libqpdf/QPDFJob.cc:2585-2645`) and the field/widget traversal rules in
`libqpdf/QPDFAcroFormDocumentHelper.cc:235-365`, with the common lazy accessor
contract from `libqpdf/QPDFObjectHandle.cc:240-446,965-989,2168-2189`.

The scoped production route has zero explicit `Pdf::resolve` or
`resolve_handle*` calls and no non-resolving dictionary/array/name/null
observations or panic key accessors. Indirect `/Fields`, `/Kids`, `/Subtype`,
and widget `/P` values are resolved through the same fallible handle path;
direct field entries remain ignored and the existing field identity, depth,
cycle, and mutation order are unchanged. qtest exceptions and the separate
merge/drop-family behavior remain outside this bounded row.

### A6/A7 outline/destination remap accessor slice `flpdf-3yn9.48.23.13` (2026-09-10)

The page-subset outline, named-destination, link-annotation, and
`/OpenAction` remap consumer now resolves its catalog and surviving-page
handles through the canonical `ObjectHandle` accessors. The page-driven
removed-page null-out and surviving-destination remap remain owned by the
same qpdf job boundary (`libqpdf/QPDFJob.cc:2469-2470,2585-2608`), while the
lazy type/key and warning/error behavior follows
`libqpdf/QPDFObjectHandle.cc:240-446,965-989,2168-2189`.

The scoped production route has zero explicit `Pdf::resolve` or
`resolve_handle*` calls and no non-resolving dictionary/array/name/null or
panic key accessors. Existing raw destination-array handling, visited sets,
direct/indirect annotation support, and page-driven null-out ordering are
unchanged. Struct-tree/thread-bead drop-family behavior, qtest exceptions,
and unrelated writer/CLI/stream routes remain outside this bounded row.
### A6/A7 thread-bead accessor slice `flpdf-3yn9.48.23.14` (2026-09-10)

The article-thread bead `/P` cleanup now traverses the live bead ring through
canonical `ObjectHandle` resolution. qpdf has no standalone bead resolver;
the observed page-subset behavior is the composition of page-driven null-out
(`libqpdf/QPDFJob.cc:2469-2470,2599-2608`) and dictionary null visibility
(`libqpdf/QPDFWriter.cc:1110-1160`). The common lazy handle and
warning/error contract follows `libqpdf/QPDFObjectHandle.cc:2375-2383,
240-446,965-989,2168-2189`.

The scoped production route has zero `Pdf::resolve` and
`resolve_handle_ref` callers. The helper's former `resolve_handle_ref` facade
was deleted after its only production caller closed; indirect identities are
captured directly from the canonical handle before `try_dereference`, while
ring order, visited-cycle handling, direct/indirect entries, and `/P`
remap/drop semantics remain unchanged. qtest exceptions and the separate
merge/drop semantic issue remain outside this bounded row.

### A6/A7 page-extract page-parent mutation slice `flpdf-3yn9.48.23.15` (2026-09-10)

The library-level page extraction route now follows qpdf's live page-insertion
mutation boundary. qpdf's `QPDF::insertPage` performs duplicate-page
`shallowCopy`, replaces `/Parent` through `replaceKey`, and then inserts the
page into `/Kids` (`libqpdf/QPDF_pages.cc:205-250`). `replaceKey` resolves its
dictionary receiver at entry (`libqpdf/QPDFObjectHandle.cc:1200-1208`), while
`shallowCopy` resolves before copying (`libqpdf/QPDFObjectHandle.cc:2073-2079`);
`QPDFPageDocumentHelper::addPage` delegates to that page operation
(`libqpdf/QPDFPageDocumentHelper.cc:36-53`).

The scoped production route had two explicit `Pdf::resolve` callers. The
`extract_pages` `/Parent` write now relies on canonical `ObjectHandle::replace_key`
resolution, and the duplicate-page path relies on canonical
`ObjectHandle::shallow_copy` receiver resolution. Selection order,
duplicate shallow-clone identity, shared child handles, PageLabels, and the
page-merge writer-order provenance remain unchanged. qtest exceptions and the
separate merge/drop semantic issue remain outside this bounded row.

### QPDFObjectHandle shallowCopy receiver-resolution primitive (`flpdf-3yn9.48.112`, 2026-09-15)

Pinned qpdf 11.9.0 makes `QPDFObjectHandle::shallowCopy` own the receiver
resolution boundary: `libqpdf/QPDFObjectHandle.cc:2073-2079` calls
`dereference()` before dispatching to `obj->copy()`. The existing flpdf
`ObjectHandle::shallow_copy` previously copied an unresolved indirect slot
without entering its document resolver, which forced callers to add an
out-of-band `try_dereference`/`Pdf::resolve` step. It now performs the same
canonical receiver resolution while retaining qpdf's indirect-child stop rule,
reserved-object copy, and `QPDF_Stream::copy` runtime error
(`libqpdf/QPDF_Stream.cc:141-145`). The page-splice duplicate-page consumer
therefore no longer performs a redundant caller-side dereference. E-28
test21's qtest consumer cutover is completed in the dependent `.48.113` slice.

### A6/A7 AcroForm appearance renderer accessor slice `flpdf-3yn9.48.23.16` (2026-09-10)

The Tx/Ch appearance renderer now reads and mutates the live widget graph
through canonical `ObjectHandle` accessors. qpdf's
`QPDFFormFieldObjectHelper::generateTextAppearance` selects the existing or
new `/AP/N`, validates its rectangle, resolves font resources, and installs the
`ValueSetter` token filter in that order
(`libqpdf/QPDFFormFieldObjectHelper.cc:766-852`). The underlying qpdf
`QPDFObjectHandle` key and typed accessors resolve their receiver at entry
(`libqpdf/QPDFObjectHandle.cc:240-446,789-824,965-989`), and
`generateAppearancesIfNeeded` owns the Tx/Ch dispatch
(`libqpdf/QPDFAcroFormDocumentHelper.cc:393-415`).

The scoped production route had four explicit `Pdf::resolve` calls: the local
resolver helper and the widget boundaries in installation, Tx generation, and
Ch generation. They were removed in favor of `try_dereference`,
`try_get_key`, `try_is_dictionary`, `try_as_array`, `try_as_name`,
`try_is_number`, `try_get_numeric_value`, and `try_is_null`. Silent
`as_stream_dict`/parser-token and string/real observations remain only after
canonical dereference where no semantically identical resolving `try_as_*`
counterpart exists; warning-producing value fallbacks are not substituted.
Existing `/AP/N` reuse, `/Rect`/`/BBox`, `/DR` font fallback, encoding,
token-filter replacement, warning order, and error propagation are unchanged.
qtest exceptions and the separate merge/drop semantic issue remain outside
this bounded row.
### A6/A7/A8 FormField field-tree accessor slice `flpdf-3yn9.48.23.17` (2026-09-10)

The FormField helper's parent-chain, inherited-value, button/value, choices,
and `NeedAppearances` graph observations now use live canonical handles. qpdf's
`QPDFFormFieldObjectHelper` performs these walks through `getKey` and typed
accessors, preserving the field identity and cycle guard
(`libqpdf/QPDFFormFieldObjectHelper.cc:30-236,267-285`), while button/value
mutation follows `QPDFFormFieldObjectHelper.cc:300-469`. The resolving
contract for those accessors is owned by `QPDFObjectHandle`
(`libqpdf/QPDFObjectHandle.cc:240-446,965-989,2168-2189`), while document-level
appearance-marker access follows `QPDFAcroFormDocumentHelper`
(`libqpdf/QPDFAcroFormDocumentHelper.cc:365-415`).

The scoped production route had four explicit resolver sites: the shared
helper and `clear_need_appearances_after_generation`. The former
`resolved` helper was replaced with canonical `try_dereference`, and field
dictionary/key/array/name/integer/null boundaries now use the corresponding
`try_*` accessors. Silent string, boolean, and other observations without a
semantically identical resolving `try_as_*` counterpart remain only after
canonical dereference, so malformed-input warning and fallback behavior is not
changed by a warning-producing substitute. Direct-parent identity guards,
inherited lookup order, checkbox/radio/pushbutton mutation order, and
`NeedAppearances` removal remain unchanged. qtest exceptions and the separate
merge/drop semantic issue remain outside this bounded row.

### A6/A7/A8 writer rewrite-renumber accessor slice `flpdf-3yn9.48.23.18` (2026-09-10)

The Catalog-first and object-stream renumber walks in
`crates/flpdf/src/writer/rewrite_renumber.rs` now use the canonical live-handle
accessor boundary. qpdf's `QPDFWriter::enqueueObject` first distinguishes
indirect identity, then recursively inspects direct arrays and dictionaries;
the same queue is consumed by standard writing
(`libqpdf/QPDFWriter.cc:1072-1141,2907-3044`). Source-backed object streams
use the two-pass `writeObjectStream` path and inspect `/Extends` through the
same handle semantics (`libqpdf/QPDFWriter.cc:1606-1758`). The qpdf accessors
resolve before type/null observation (`libqpdf/QPDFObjectHandle.cc:240-446,
857-866,965-1015`), while indirect identity itself remains non-resolving
(`include/qpdf/QPDFObjectHandle.hh:353,1630-1641`).

The scoped production route removed seven explicit `Pdf::resolve` calls and
three non-resolving `is_null` observations. Direct array/dictionary/null
boundaries now use `try_as_array`, `try_as_dictionary`, and `try_is_null`;
stream dictionaries retain the silent `as_stream_dict` observation only after
the preceding canonical accessor has resolved the stream. Catalog-first BFS,
array-versus-dictionary null visibility, stream-parameter exclusions,
removed-reference filtering, source-backed `/Extends` traversal, and depth/
cycle/error behavior are unchanged. The existing module route contract now
guards this production slice, and `canonical_children_propagate_resolution_errors`
keeps resolver failures as `Result` errors. qtest and qtest-exceptions routes,
active `.48.7/.48.10/.48.49` sessions, and shared writer emission ownership
remain outside this bounded row.
### A6/A7/A8 linearization plan accessor slice `flpdf-3yn9.48.23.19` (2026-09-10)

The remaining linearization planning walk in
`crates/flpdf/src/linearization/plan.rs` now uses live canonical handles at
each page/resource closure, inherited-parent, reachable-object, root/page, and
outline observation. qpdf's `QPDF::optimize` owns the ordered live page and
inherited-attribute traversal (`libqpdf/QPDF_optimization.cc:57-118`), and
`QPDF::calculateLinearizationData` owns object-user categorization
(`libqpdf/QPDF_linearization.cc:963-1140`) and the subsequent part ordering
(`libqpdf/QPDF_linearization.cc:1147-1265`). Its handle operations
resolve at the accessor boundary (`libqpdf/QPDFObjectHandle.cc:240-446,
965-1015,2375-2383`), while writer setup does not add a separate cache-warmup
walk (`libqpdf/QPDFWriter.cc:2536-2554`).

The scoped production route removed seven explicit `Pdf::resolve` calls. The
existing `try_is_dictionary_of_type`, `try_as_dictionary`, `try_get_key`,
`try_has_key`, and `try_is_stream_of_type` operations now own those resolution
boundaries, preserving resource-first DFS, `/Parent` ancestry, page-tree
boundary exclusion, resurrectable-null edge context, object-stream reachability,
outline routing, and Result/error propagation. The production route contract
now includes `linearization/plan.rs`, and
`page_tree_classification_propagates_resolution_errors` keeps resolver failures
fallible. qtest and qtest-exceptions routes, active `.48.7/.48.10/.48.49`
sessions, linearization emission, and separate Part 2/3 semantic issues remain
outside this bounded row.

### A6/A7/A8 linearization writer accessor slice `flpdf-3yn9.48.23.20` (2026-09-10)

The remaining linearized writer consumers in
`crates/flpdf/src/linearization/writer.rs` now use qpdf-shaped live-handle
resolution. qpdf's `writeObjectStream` detects stream members and writes the
two-pass container through the same `writeObject`/`unparseObject` boundary
(`libqpdf/QPDFWriter.cc:1606-1810`); those object-handle operations resolve at
entry and preserve qpdf's key/error fallback semantics
(`libqpdf/QPDFObjectHandle.cc:240-446,965-1015,1257-1262,1575-1593`). The
linearized writer reaches this preparation after the common setup ordering
(`libqpdf/QPDFWriter.cc:2536-2561`).

The scoped production route removed four explicit `Pdf::resolve` calls and
five panic `get_key` calls. ObjStm member classification now uses canonical
`try_dereference` before the necessary silent `as_stream_dict` observation;
the body, outline, and Catalog paths rely on canonical `try_*`/writer
serialization boundaries, and fixed Type/Length/Filter/N/First key order is
unchanged. Source/member ordering, `/Extends`, encryption/newline behavior,
outline hint ownership, ADBE status, direct/indirect identity, and Result
propagation remain unchanged. The production route contract covers only the
target functions and checks stream-observation ordering, while
`body_object_append_propagates_member_resolution_errors` covers fallible
emission. qtest and qtest-exceptions routes, active `.48.7/.48.10/.48.49`
sessions, linearization planning/check/show, shared emission redesign, and
unrelated hint/Part 2/3 semantics remain outside this bounded row.

### Linearization page-user view ownership `flpdf-ymuj.6.8` (2026-09-14)

qpdf retains one bidirectional object-user relationship in `QPDF::Members`:
`obj_user_to_objects` and `object_to_obj_users` are declared at
`include/qpdf/QPDF.hh:1515-1517`, populated together by
`QPDF_optimization.cc:289-296`, and consumed directly by
`QPDF_linearization.cc:1063-1105,1350-1410` for page/document classification
and shared-object identifiers. qpdf does not build a second resident
object-to-page map for linearization.

flpdf's `Optimization::user_to_objects/object_to_users` at
`crates/flpdf/src/optimization.rs:21-23` is the corresponding canonical
owner. `LinearizationPlan::all_referenced_pages` and
`Optimization::referenced_pages` were a flpdf-only derived map and per-object
set materialization. `flpdf-ymuj.6.8` removes those copies and exposes a
borrowed `page_users` view over the retained object-user set. Outline/shared
hint and ObjStm-container consumers collect only the sorted page values they
need at their boundary; plans without a populated page-user map retain their
manual-fixture fallback. The qpdf object-user map, page ordering, page-0
exclusion, hint payload, and ObjStm routing responsibilities are unchanged.

### Compact object-user set representation `flpdf-ymuj.6.9` (2026-09-14)

qpdf's `QPDF::Members::object_to_obj_users` value is an ordered
`std::set<ObjUser>` (`include/qpdf/QPDF.hh:1515-1517`). Its comparator orders
user kind, page number, and key (`libqpdf/QPDF_optimization.cc:32-53`), and
both `updateObjectMapsInternal` and `filterCompressedObjects` rely on set
deduplication while preserving that order (`libqpdf/QPDF_optimization.cc:282-
296,340-380`).

flpdf keeps the same qpdf-shaped table and semantics in
`Optimization::object_to_users`. `flpdf-ymuj.6.9` changes only the Rust value
representation: tiny ordered user sets use a compact sorted form and promote
to a tree-backed ordered set at higher cardinality. `users_for`, borrowed
iteration, `contains`, object-user classification, ObjStm folding, page hints,
and output/error behavior remain on the same canonical responsibility boundary;
no unordered hash or second user table is introduced.

### A6/A7/A8 standard writer accessor slice `flpdf-3yn9.48.23.21` (2026-09-10)

The remaining standard-writer observations in `crates/flpdf/src/writer.rs` now
use live canonical handles at the accessor boundary. qpdf's ordinary object
and ObjStm emission owns source-handle resolution and dictionary observation in
`QPDFWriter::writeObjectStream`/`writeObject`
(`libqpdf/QPDFWriter.cc:1606-1810`); its special-stream setup and PCLm queue
walk are the corresponding writer-owned boundaries
(`libqpdf/QPDFWriter.cc:1914-1931,2928-2954`). The resolving contract belongs
to `QPDFObjectHandle` (`libqpdf/QPDFObjectHandle.cc:240-446,965-1015,
1257-1262,1575-1593`).

The scoped production route replaced seven explicit `Pdf::resolve` calls and
one non-resolving `/Root` `is_null` observation. PCLm source observation, the
specialized-root check, QDF pre-scan and main body emission, source-backed
`/Extends`, and page-content container discovery now use
`try_dereference`, `try_get_key`, and `try_is_null` (or the canonical typed
accessors that resolve their receiver). `as_stream_dict` remains only after a
canonical dereference; direct `as_array`/`as_string` observations used by the
generated-ID helpers are intentionally outside this bounded row. Object-stream
member serialization remains owned by the writer's canonical serializer, and
object numbering, marker placement, content normalization, error propagation,
and output ordering are unchanged. The route contract is
`crates/flpdf/tests/writer_accessor_route_contract_tests.rs`, including the
stream-observation ordering check. qtest and qtest-exceptions routes, active
`.48.7/.48.10/.48.49` sessions, shared live queue/emission redesign, and
generated direct-ID helper migration remain outside this slice.
This is the `writer.rs` coordinator slice only: `writer/pclm.rs::Plan::build`
still has its separate mixed-route `is_null`/`Pdf::resolve` observations at
`pclm.rs:40,57,63,87`, so the complete PCLm planner route is intentionally not
closed here.

### QPDFJob `doInspection` combined top-level consumer `flpdf-giz3` (2026-09-10)

The top-level CLI now routes combined inspection selections through the
qpdf-shaped `QPDFJob` inspection configuration. `QPDFJob::Config`-equivalent
inspection setters select the existing report-only owners, and
`QPDFJob::inspect_configured` runs the existing independent branch column and
one warning/completion boundary. This follows qpdf's
`QPDFJob::doInspection` order (`libqpdf/QPDFJob.cc:1646-1693`) and its
no-output `writeQPDF` branch (`libqpdf/QPDFJob.cc:483-511,528-564`), rather
than adding a combination-specific CLI shim.

The `--check-linearization` conflicts that qpdf accepts were removed for the
literal 15 combinations tracked by `flpdf-giz3`; writer-only flags remain
no-ops when qpdf selects inspection, while create-stage
`--remove-restrictions` and `--coalesce-contents` use the existing job
transformation order. Attachment extraction reserves the save pipeline before
the first info report, matching `QPDFJob.cc:614-626`. The focused differential
matrix covers all 15 combinations plus representative multi-inspection orders;
`--with-images` and an explicit `--normalize-content` value are copied into the
same Job configuration for the `show-pages` and `show-object` consumers
(`QPDFJob_config.cc:414-417,654-656`; `QPDFJob.cc:816-829`). qtest exceptions
and unrelated transformation/page-operation routes remain outside this row.

2026-09-15 (`flpdf-7vov`): top-level `--check-linearization` no longer rejects
`--add-attachment`, `--remove-attachment`, or `--copy-attachments-from` at the
Clap boundary. These mutations are queued on the same `QPDFJob` and therefore
run in `createQPDF` before the output-free inspection column. An explicit output
path is rejected with qpdf's single `no output file may be given for this option`
usage error; the three mutation-operation flags remain mutually exclusive with
one another.

`flpdf-5qbs` closes the remaining argv-surface mismatch in this inspection row.
qpdf accepts repeated `doInspection` selectors: the boolean Config setters are
idempotent, while `Config::showObject` and `Config::showAttachment` overwrite
their stored selector with the last occurrence (`QPDFJob.cc:1645-1693`;
`QPDFJob_config.cc:378-380,543-545,766-768`). The top-level clap surface now
models those same last-occurrence/idempotent semantics with self-overrides.
`--list-attachments` and `--show-attachment` are left as independent
inspection consumers so both execute in qpdf order; only the three
mutation-operation group members remain mutually exclusive at the argv layer.
`crates/flpdf-cli/tests/cli_inspection_argv.rs` fixes the qpdf 11.9.0
status/stdout/stderr differential for repeated inspection flags, selector
last-wins, and list-plus-show attachment output.

### `qpdfjob-c` wrapper のエラー境界

qpdf の `wrap_qpdfjob`（`libqpdf/qpdfjob-c.cc:32-40`）は、
`QPDFJob::initializeFromJson`/`run` が投げた例外を job logger の error
pipeline へ prefix・区切り・本文・改行の順に送り、`EXIT_ERROR`へ変換する。
flpdf は `QPDFJob::report_job_error` の canonical route を qtest の
Rust consumerへ公開し、`qpdfjob_ctest.rs` がこの wrapper の継続順序だけを
担う。通常の `QPDFJob::run` の `UsageError` contractや、CLIの別の usage
表示経路は変更しない。

### `qpdf-ctest` test02 の C API 報告境界

qpdf の `test02` は `qpdf_read` → `qpdf_init_write` → `qpdf_write` を
`qpdf-c.cc` の `trap_errors` 境界に置き、最後に `report_errors` で retained
warning をすべて drain してから terminal error を1件出し、`C test 2 done`
を印字する（`qpdf/qpdf-ctest.c:35-68,161-170`、
`libqpdf/qpdf-c.cc:68-89,266-282`）。入力 open、writer の output 設定、writer
本体の失敗もこの同じ report boundary に含まれ、helper のプロセス status は
成功のままである。

`crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs::run_test2` は、入力 open の
失敗を `qpdf_file_io_message` から `QpdfExc` へ投影し、入力が開けた場合は
`PdfWriter::set_output_file` と `PdfWriter::write` の結果を同じ completion
経路へ送る。writer の失敗時も `write_pdf_diagnostics` を先に呼ぶため、
qpdf と同じ warning → terminal error → `C test 2 done` の順序になる。
bad-password を含む input-side `OpenFailure` は既存の retained diagnostics
を `write_open_error_report` から replay する。C ABI 自体は実装せず、PDF の
reader/writer semantics は canonical `Pdf` / `PdfWriter` が所有する。

`crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs` は、通常成功、input-open、
writer-open、missing `/Root` による writer failure、repair warning の
writer-open 前後、repair warning の bad-password 前後を qpdf 11.9.0 の live
`qpdf-ctest` と突き合わせて固定する。既存の `c-api`/`error-condition` の
qtest fixture は別の manifest scope であり、この adapter のテストはその
portable report contract を担う。

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
| `job/page_subset.rs` | — | `/Resources` の stale 名前エントリ剪定（`removeUnreferencedResources` 相当）を qpdf の page/job 責務へ分離。xref レベルの orphan 判定は writer の emission boundary に委ねる。null 可視性とは独立 |

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
`job/page_subset.rs` は qpdf 側に明示的な対応先を持つ独立した責務であり、2 機構に還元できない。

**区別すべきこと**: 挙動は検証済みで byte-identical を保っているので壊してはならない。
機構が異なるだけ（in-place 個別修復 vs. null 置換 + writer の null 可視性）。
畳み込みは byte リスクを伴う別の設計判断であり、writer の責務分割が固まったあとに検討する。

### B. `QPDFJob::handlePageSpecs` 相当の分解 — 4,158 行

`job/page_specs.rs` / `job/page_split.rs` / `job/page_merge.rs`(1117) / `job/rotate.rs`(632) / `page_extract.rs`(435) /
`job/page_range.rs`(379) / `page_splice.rs`(304) / `job/page_combine.rs`(278) / `job/page_plan.rs`(210) /
`job/rotate_spec.rs`(204)

`job/page_specs.rs` がqpdfのjob-level orchestration（`QPDFJob.cc:2360-2632`）を所有し、
`job/page_merge.rs` と `PageDocumentHelper` がforeign page copy/page-tree primitiveを所有する。
2026-09-15（`flpdf-kiou0`）: qpdf の `QPDFPageData` は各 page spec の range 解決前に
`getAllPages()` を呼び、既存 `/Pages` graph の `/Type` 修復・direct kid 昇格結果を
その spec に渡す（`QPDFJob.cc:259-269`, `QPDF_pages.cc:39-138`）。flpdf の
multi-source `handle_page_specs_into` も各 `PagePlan::build` の前に
`PageDocumentHelper::get_all_pages` を通すようにし、direct leaf と mistyped page-tree
fixture の warning、page count、QDF output bytes を qpdf 11.9.0 と一致させる。
これは fixture 固有分岐や別の page walker を追加せず、既存 canonical repair owner の
呼び出し順を qpdf に合わせる修正である。
`.40` では `resources.rs` に残る `QPDFPageObjectHelper::removeUnreferencedResources`
相当の page/Form mutation と、`job/resource_pruning.rs` に移した
`QPDFJob::shouldRemoveUnreferencedResources` 相当の `auto|yes|no` policy を分離した。
後者は `QPDFJob.cc:2251-2339` の共有リソース探索だけを所有し、前者の
`/Font`・`/XObject` pruning algorithmを再実装しない。
`.48.99` では qpdf の private heuristicに対応するflpdfのsilent free wrapperを
`job/resource_pruning.rs` の `pub(crate)` 境界へ狭め、job/rootのpublic re-exportを撤去した。
publicな `RemoveUnreferencedResources` enumと `QPDFJobConfig` の設定setterは、qpdfの
public Config設定面に対応するため保持する。
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
`PageLabelDocumentHelper::write_reconstructed_labels_raw` を通る fresh chunk
生成を実装し、CLI の単一入力・複数入力 split 出力を同じ job route に切り替えた。
`flpdf-h0a9` では、page-spec の verbose diagnostics も `QPDFJob` の canonical route へ接続した。
qpdf は `Config::verbose` (`QPDFJob_config.cc:637-645`) を job に保持し、
`handlePageSpecs` の source processing、raw filename をキーにした `std::map` 順の
`shouldRemoveUnreferencedResources` preflight、primary page removal、page addition を
`doIfVerbose` (`QPDFJob.cc:340-345,2360-2472`) から同じ info logger へ流す。
flpdf は `Pdf` の caller-provided input description bytes と `QPDFJob` の message prefix/logger
を使い、`resource_pruning.rs` の finding callback を一度だけ実行した結果を page merge へ渡す。
`Config::emptyInput` の primary だけは内部 source-map key が空文字列で表示名が `empty PDF`
となる qpdf の分離 (`QPDFJob_config.cc:27-38`, `QPDF.cc:290-293`) も保持する。
これにより absolute/non-UTF-8 argv path、finding/no-finding の分岐、source preflight 順、
`--pages` 後の split preflight を CLI 固有の synthetic template なしで qpdf と揃える
（`QPDFJob.cc:2251-2339,2440-2565,2940-3025`; differential coverage:
`crates/flpdf-cli/tests/cli_pages_verbose_diagnostics.rs`）。
`.50qd.1` では secondary source の認証を `QPDFJob.cc:2400-2412` の
`page_spec.password` 境界に合わせ、top-level primary password を distinct secondary の
fallback にしない。global な password mode/weak-crypto policy は共有するが、credential
本体は spec-local に限定する。
`.50qd.2` では `QPDFJob.cc:1714-1715` の全 input version floor と
`QPDFJob.cc:2847-2918` の writer 設定境界を、multi-source `--pages` の
fresh merged document に明示的に伝播する。primary と全 secondary の
`M.m`/`/Extensions /ADBE /ExtensionLevel` の pairwise max は
`WriterOptions::input_version_floor` に保持し、明示 `--min-version` の raw
文字列を正規化せず、`WriterConfiguration::set_minimum_pdf_version` で
source floor の後に適用する。`QPDFWriter.cc:217-250` の numeric tie では
incumbent の raw version を保持して extension level だけ更新し、
`--force-version` は従来どおり最終的に優先させる。
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

`.d499` では、同じ fresh target への変換が線形化時の part 6 の object order を
失わせないようにした。qpdf は primary を元の `QPDFObjGen` のまま保持し、foreign
page の graph は `copyForeignObject` の discovery 順で新しい object number を割り当て、
`calculateLinearizationData` の first-page private/shared 集合を `std::set<QPDFObjGen>`
で並べる（`QPDF.cc:2019-2213`; `QPDF_linearization.cc:963-1024,1188-1211`）。
flpdf の `job/page_merge.rs` はこの primary source order と foreign allocation order を
内部の linearization provenance として target object に対応付け、`linearization/plan.rs`
の part 2/3 の sort key だけへ渡す。通常の merge/writer の object allocation と出力順は
変更しない。qpdf 11.9.0 の `three-page.pdf` + `one-page.pdf` に対する
`--static-id --stream-data=uncompress --linearize --pages` の全出力 bytes を比較する
`cli_linearize_multi_source_qpdf.rs` でこの境界を固定する。

`flpdf-j2bt` では、同じ primary page の重複選択で qpdf が
`shallowCopyPage` を後続 foreign `copyForeignObject` より先に allocate する
順序も writer provenance へ渡す。qpdf の generated ObjStm は
`getCompressibleObjGens` の候補順を保ち、各 container 内では destination
object number 順に member を予約するため（`QPDFJob.cc:2515-2555`;
`QPDFWriter.cc:1057-1118,1970-2005`; `QPDFWriter.hh:680`）、flpdf の
`page_extract.rs::append_selection_kids` は shallow clone を foreign/destination
order group に登録する。`cli_pages_objstm_order_qpdf.rs` の duplicate-page
qpdf-zlib gate が clone と後続 foreign page の全 bytes を比較する。

`flpdf-kgsv` では、multi-source page-selection target の writer provenance を
linearized Generate にも渡す。qpdf は `getCompressibleObjGens` の候補を一度
global に split してから、`assignCompressedObjectNumbers` が各 ObjStm の
member を source/destination order で予約するため（`QPDF.cc:2393-2474`;
`QPDFWriter.cc:1057-1118,1970-2005`; `QPDF_linearization.cc:963-1045`）、
`linearization/plan.rs` は split 前と container 内の両方で
`Pdf::writer_object_order_key` を使う。qpdf-zlib differential test は
multi-source `--pages` + `--linearize` + `--object-streams=generate` の全 bytes
を固定する。

`flpdf-sof0` では、page-spec occurrence の境界を writer key の比較順にも
反映する。`restore_occurrence_writer_provenance` が qpdf の同一 destination
allocator に相当する `original_object_ref` を occurrence 順に更新した後、
`WriterObjectOrderKey::occurrence_rank` を `object_ref` より先に比較する。
これにより fresh target の source-grouped な target reference が
`handlePageSpecs` の occurrence 順を覆さず、QDF の Original object ID の値は
別フィールドのまま保持される（`QPDFJob.cc:2517-2555`;
destination identity allocation は `QPDF.cc:1870-1897`、QDF の Original
object ID emission は `QPDFWriter.cc:1681-1689,1774-1787`、ObjStm の
ordering は `QPDFWriter.cc:1057-1118,1970-2005`）。
`cli_pages_objstm_order_qpdf.rs` の occurrence-order 3 tests が primary →
foreign → primary duplicate を、通常 Generate・QDF Generate・linearized
Generate の各 ObjStm 経路で qpdf 11.9.0 と比較する。

`flpdf-lv0j` では、page selection の同じ occurrence で行われる
`fixCopiedAnnotations` 相当の field/annotation/appearance/resource allocation も
page graph や duplicate shallow-copy と同じ destination allocator 順へ含める。
qpdf は `handlePageSpecs` の selected-page loop 内で `shallowCopyPage` と
`fixCopiedAnnotations` を occurrence ごとに実行し（`QPDFJob.cc:2517-2585`、
`QPDFPageObjectHelper.cc:654-660`）、transform の field tree・annotation・appearance
stream はその場の `makeIndirectObject` で割り当てる
（`QPDFAcroFormDocumentHelper.cc:699-1047`、`QPDF.cc:1870-1897`）。flpdf は
canonical resolver の `allocated_object_order` を read-only checkpoint 差分として
replay helper の前後で取得し、初回登録順を失わずに `page_specs.rs` の finalizer が
page allocation と replay allocation を occurrence 順に `WriterObjectOrderKey` へ反映
する。AcroForm helper も qpdf同様にAP内の全 `copyStream` を先に行い、その後に
各 copied stream の matrix/resource adjustment を行うため
（`QPDFAcroFormDocumentHelper.cc:967-1010`）、direct `/DR` の resource dictionary
allocation も同じ event order になる。これにより QDF writer の
`%% Original object ID`（`QPDFWriter.cc:1681-1705,1770-1800`）だけでなく、後続の
writer-order consumer も同じ event order を観測し、copy 経路や qtest-only shim は
増やさない。`cli_pages_objstm_order_qpdf.rs` の annotated replay differential gate が
`three-page.pdf` → `form-fields-and-annotations.pdf` → duplicate primary と
direct-DR interleave の全 bytesを、QDF・Generate ObjStm・linearized Generateを含めて
qpdf 11.9.0 と比較する。

`flpdf-pz5h` では、ObjStm を生成しない linearize の page-offset hint についても
page-selection の occurrence provenance を保持する。qpdf は
`calculateLinearizationData` で first-page/Part-8 の配置を決めた後、各ページの共有
identifier を `obj_user_to_objects[page]` の `std::set<QPDFObjGen>` 順に追加する
（`QPDF_linearization.cc:963-1265,1388-1402`）。fresh merge target の ObjectRef 番号で
この列を再ソートすると、primary → foreign → primary duplicate の形状で source order
を失い、physical hint-table index だけが qpdf と反転する。flpdf は
`LinearizationPlan::shared_hints` の provenance order を classic path ではそのまま
符号化し、ObjStm folding が synthetic container を導入する場合だけ既存の
container-aware sort を適用する。`cli_pages_objstm_order_qpdf.rs` の
`duplicate_page_after_foreign_linearized_hint_stream_matches_qpdf` が
`--static-id --linearize` の qpdf-zlib 全 bytes を比較し、hint payload 内の共有
identifier 列を固定する。

`flpdf-8gwx` では、同じ AcroForm/annotation replay の結果を linearization の
part-4 open-document order へ渡す。qpdf は `handlePageSpecs` の occurrence loop で
`fixCopiedAnnotations` を実行し、`makeIndirectObject`/`copyForeignObject` の
allocation identity を保持したまま、`calculateLinearizationData` の
`lc_open_document` を page-selection 後の destination `std::set<QPDFObjGen>` 順で part 4 に置く
（`QPDFJob.cc:2517-2584`; `QPDF.cc:1891-2095`; `QPDFAcroFormDocumentHelper.cc:699-1047`;
`QPDF_linearization.cc:963-1265`）。page-selection target の fresh ObjectRef 番号で
`part4_open_document_plain` を並べると、この destination allocation order が失われるため、
flpdf は primary/foreign allocation を表す provenance projection
`Pdf::writer_object_order_key` で Part 2/3 と同じように part 4 を並べる。
`cli_pages_objstm_order_qpdf.rs` の annotated multi-source linearize（重複なし/重複あり）
全 bytes gate が qpdf 11.9.0 との一致を固定し、qtest route や compatibility bridge は
追加しない。

`flpdf-mwyo` では、classic hint page が Part 3 と Part 8 の shared object を同時に
参照する場合も、section 順ではなく qpdf の destination `ObjGen` 順で
`shared_identifiers` を出力する。qpdf は `obj_user_to_objects[page]` の set を走査して
shared-table index へ変換するため（`QPDF_linearization.cc:1350-1410`）、Part-8 object
7 と Part-3 object 20 の synthetic cross-section fixture では `[3,2]` が正しく、
`[2,3]` は構造的に valid でも byte parity を壊す。flpdf は page-selection target の
allocation provenance projection を `LinearizationPlan` から hint builder へ渡し、
fresh target number に戻らず classic/ObjStm の既存 ordering contract と整合させる。
`cli_pages_objstm_order_qpdf.rs` の qpdf-zlib differential test がこの4-byte hint差を
固定する。

`flpdf-obsc` では、`QPDFJob::doSplitPages` が chunk 作成前に行う
`shouldRemoveUnreferencedResources` の verbose side effect も同じ job boundary に
接続した。qpdf は Auto 判定の開始、最初の共有 resource finding、または共有なしの
完了を `doIfVerbose` で info logger へ送り、その後に各 chunk の `wrote file` を出す
（`QPDFJob.cc:340-345,2251-2340,2940-3025`）。flpdf は heuristic の BFS/policy を
`job/resource_pruning.rs` に残したまま finding callback を `job/page_split.rs` の
message prefix/logger へ渡し、`get_all_pages` と resource mutation より前に実行する。
`SplitPageOptions` の Auto/Yes/No は job-JSON を含む全 split caller へ伝播し、
`cli_split_pages_verbose_qpdf.rs` が qpdf 11.9.0 の stdout/stderr と chunk report の
順序を比較する。

`.u3iq` では、qpdf の main option table (`libqpdf/qpdf/auto_job_init.hh:124`) に登録される
`--remove-unreferenced-resources=auto|yes|no` を flpdf-cli の top-level `Cli` にも
公開する。選択値は `--pages` の `QPDFJob::handlePageSpecs` 相当と
`--split-pages` の `doSplitPages` 相当へそのまま渡し、plain rewrite では qpdf と同じく
`/Resources` entry の剪定を行わない（`QPDFJob_config.cc:751-761`、
`QPDFJob.cc:2442-2455,2961-2967`）。pages/split の no/yes 各 mode は
`cli_top_level_remove_unreferenced_resources.rs` が qpdf 11.9.0 の stdout と
生成 bytes を比較する。

`flpdf-2wby` では、qpdf の `auto_job_init.hh:65-66` が登録する
`--preserve-unreferenced-resources` synonym も同じ境界へ接続した。qpdf の
callback は `QPDFJob_config.cc:471-474` で `removeUnreferencedResources("no")`
と等価な `re_no` を選ぶため、flpdf の argv preprocessor は clap 前に
`--remove-unreferenced-resources=no` へ正規化する。重複した policy field や
CLI-only adapter は増やさない。qpdf の argv 文法は long option に単一ダッシュ
綴りも許すため、この synonym は clap option を持たない（単一ダッシュ経路の
`known_long_options` 変換に乗らない）ことを踏まえ、綴り別分岐より前で
両方をまとめて正規化する。

同じ value option を 2 回以上指定したときの last-setting 挙動は、この
synonym に限らず flpdf 全体でまだ qpdf と一致していない（clap が
`cannot be used multiple times` で先に失敗する）。`flpdf-749p` で追跡する。

### C. qpdf に機能そのものが無いもの

| flpdf | 行 | 備考 |
|---|---|---|
| `signatures.rs` の**検査 API のみ** | — | 署名の読み取り検査。qpdf に相当機能なし |
| `qdf_fix.rs` | 1,219 | qpdf では `qpdf/fix-qdf.cc`（libqpdf 外の別バイナリ）。object stream (`/Type /ObjStm`) / cross-reference stream (`/Type /XRef`) 形式の QDF 入力にも対応（`st_in_ostream_*` / `st_in_xref_stream_dict` 相当、flpdf-9hc.43） |
| `job/attachments.rs` の library-level convenience helpers（`add_attachment_from_path` / `ascii_filename_fallback` / `extract_attachment` / `write_attachment` / `extract_attachment_to_path`） | 469-696 | qpdf の `QPDFJob::addAttachments` / `doShowAttachment`（`QPDFJob.cc:914-926,2046-2087`）は Job/CLI の設定・logger pipeline を所有し、これらの crate-level path/buffer/fallback API に直接対応する公開 API はない。5関数は flpdf 固有の category-(C) として `#[deprecated]` で記録し、qpdf の canonical attachment helpers（`QPDFEmbeddedFileDocumentHelper` / `QPDFFileSpecObjectHelper`）の代替とは扱わない |


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

`.4wq4` では、single-source `PageSpecJobOutput::InPlace` の page-selection
completion（navigation remap、structural `/Pg`/`/P` drop、subset prune、
AcroForm prune）を `QPDFJob::complete_in_place_page_selection` に集約する。
これは qpdf の `QPDFJob::createQPDF` における
`handlePageSpecs` → `handleRotations` → `handleTransformations` の順序
（`QPDFJob.cc:428-535,2137-2210,2360-2632`）に対応する。CLI の overlay、
split、writer/output と、QPDFJob の transformation/inspection continuation は
それぞれの consumer boundary に残し、page-selection helper が qpdf にない
横断的な output abstraction を作らない。

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

### `flpdf-77kv` の正式マーカー監査（2026-09-04）

`flpdf-77kv`（qpdf-deviation 監査: 2次候補6件の精査とマーク）がスコープとする
6候補のうち、`stream_filter.rs` は既にマーク済みだった。もう1件の既存マーク
候補として bd issue が記録していた `ref_chain.rs`
（`resolve_ref_chain`/`terminal_ref_of_chain`/`MAX_REF_CHAIN_DEPTH`）は、
本PR時点のツリーには存在しない — 該当機能自体が候補リスト作成後の別作業で
削除されており、新たなマーカーを追加すべき対象は残っていない。したがって
本PRが実際に新規マークしたのは残り4候補（`stream_filter.rs` を含めた
既存分と合わせて現存する候補は5件）。この表は `flpdf-77kv` の候補リストに
限定したものであり、直前の2件（`reserve_objects` の owner 再検証、
`stacker::maybe_grow` によるスタック保護）を含む repository 全体の
未マーク候補を網羅する監査ではない。それらは ⚪（逸脱候補・要承認、まだ
確定していない）のまま別途の精査対象として残る。ここでのマーカーは
対応関係の欠落を機械可読にするものであり、既存の ⚪ を承認済みに変更する
ものではない。

| flpdf | qpdf 11.9.0 との照合 | 記録範囲 |
|---|---|---|
| `object_copy.rs` の `ForeignObjectCopier::direct_visiting` | `reserveObjects` / `replaceForeignIndirectObjects`（`QPDF.cc:2101-2213`）に direct dictionary/array cycle 用 visited set はない。これは `ObjectHandle::replace_key` でのみ作れる direct graph の防御である。フィールド単位で切り離せるため `#[deprecated]`（アクセスする関数群に `#[allow(deprecated)]`）で記録し、comment block マーカーは使わない。 | reservation と replacement の各 direct-cycle guard |
| ~~`xref.rs::load_xref_state_with_options` の `startxref` offset-0 経路~~（2026-09-18、`flpdf-3yn9.48.151` で解消） | qpdf の `xref_offset == 0` チェック（`QPDF.cc:450-452`）は、`startxref` が解析できない場合と、構文的に正しい `startxref` が明示的に offset 0 を指す場合の両方で、即座に `damagedPDF("can't find startxref")` を投げ `read_xref` を一切呼ばない。flpdf の retry-at-offset-0 detour は owner-less loader 専用で、canonical owner を必須化した時点で `startxref == 0` が無条件に line-scan recovery へ抜けるようになり、detour と `qpdf-deviation` マーカーごと削除した。`push_repair_diagnostics` 自体は qpdf の `reconstruct_xref` 3行警告シーケンスを忠実に再現するだけで、対応物のない挙動は detour 側にあった。 | 解消済み。現行の `load_xref_state_from_window` は `startxref == 0` を repair 時に直接 `recover_xref_from_linear_scan` へ渡す |
| `object_handle.rs::ObjectSlot::pdf_unique_ids`（`flpdf-ymuj.3.2` で削除） | qpdf は document-level `unique_id`（`QPDF.hh:1454`, `QPDF.cc:2294-2296`）と各 value の `QPDF*` back-pointer（`QPDFValue.hh:60-80,149-152`）を持つが、別の per-object numeric id set は持たない。`active_pdf_unique_id`（単一値）は qpdf の `QPDF*` back-pointer への container 表現代替（CLAUDE.md 分類 (B)）であり、direct value のownership判定もこの単一値または未所有状態に揃えた。 | 旧history setと `attach_child_to_parent` のsubtree stampingを撤去し、`flpdf-ymuj.6.3` で production の reverse containment edge も撤去した。現在の ownership は `active_pdf_unique_id` の単一 value 表現と forward child graph だけで決まり、unit-test の root assertion は test-only scan から導出する。 |
| `reader/resolver.rs::ResolverHandle::read_window` / `read_to_owned` | qpdf の `InputSource` は live `seek`/`tell`/`read`（`InputSource.hh:71-74`）で、`readStream` もその source を保存・復元して読む（`QPDF.cc:1360-1398`）。bounded owned-window helper は qpdf にない。関数単位で切り離せるため `#[deprecated]`（呼び出し元は `#[allow(deprecated)]`）で記録し、comment block マーカーは使わない。 | `read_window` / `read_to_owned` の legacy owned-buffer seam **2026-09-08（`flpdf-3yn9.48.25`）に解消**: helper 2 本と `MAX_RESOLUTION_FALLBACKS` / `resolution_fallbacks_remaining` を削除し、`qpdf_route_hygiene_tests.rs` が不在を検査する。live `seek`/`tell`/`read` のみが残る。 |
| `xref.rs::LoadedXref::repair_diagnostics` / `XrefStreamFailure::diagnostics`（`flpdf-3yn9.48.177` で監査、コードマーカーなし） | qpdf の `m->warnings`（`QPDF.cc:487-494`）は push_back のみで `warn()` 呼び出し順に即時配送される単一 sink。flpdf は複数の xref candidate（/Prev chain・hybrid section・reconstruction fallback）を試すため、Rust の関数境界を跨いで分割生成される診断を `Diagnostics` 値として一旦保持し、`deliver_canonical_diagnostics` で drain する。「一時バッファ」という見た目から (C)（対応物なし）候補に見えるが、実際に監査した結果は container 代替（分類 (B)）: `prepend_repair_diagnostics`（`xref.rs:2150`、成功パスの唯一の呼び出し元 `:1041` で、時系列的に先行する `initial_diagnostics` を後発の `loaded.loaded.repair_diagnostics` の**前**へ結合し直すだけで、qpdf の `warn()` 呼び出し順を回復している）、`merge_recovered_qpdf_state`（`:2153`、`accumulated`（先行）→`recovered`（後発）の順で結合）、および 3 箇所の `mem::take`（`xref.rs:1040,1129,2153`）を全経路たどっても、順序の入れ替えや内容破棄は無い——`deliver_canonical_diagnostics` に届くか、次のバッファへスレッドされるかのいずれかで、握りつぶされる経路は無い。よって CLAUDE.md 分類 (C) のコードマーカー（`// qpdf-deviation`）は付けない。`XrefStreamFailure::diagnostics`（`xref.rs:3248`）も同型の container（エラーの隣に診断を運ぶだけ）。 | `docs/qpdf-route-matrix/b-parser-recovery-diagnostics.md` の B27 行を対応させて更新済み。B29 の snapshot consumer（CLI lazy warning emission 等）は別issue範囲として着手せず — 単一 `ResolverCore::repair_diagnostics` collection への正当な public accessor consumer であり別 sink ではないため |

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
| QDF × linearize | `libqpdf/QPDFWriter.cc:2068-2080` — linearized setup clears `qdf_mode` before deriving QDF defaults; flpdf accepts the combination and its canonical writer clears QDF in `crates/flpdf/src/writer.rs:772-773` (CLI parity: `crates/flpdf-cli/tests/cli_qdf.rs`) |
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
| `std::shared_ptr<QPDFValue>` → `Rc<RefCell<..>>`（`object_handle.rs`） | 79 | 無し（`Rc` による共有 identity の内部所有権機構自体。live direct containment の weak reverse index は `#[cfg(test)]` 限定の containment 検査補助で、production の scheduling には関与しない（`flpdf-3yn9.48.24` で dirty bookkeeping を撤去）。共有 identity と各 object の serialization rule は変えず、Pdf identity provenance は別フィールドで保持。byte-identical suite で確認済み） |
| `std::shared_ptr<Buffer> QPDF_Stream::stream_data`（`libqpdf/qpdf/QPDF_Stream.hh:104`） → `Rc<Vec<u8>>`（`object_handle.rs` の `ObjectValue::Stream`） | 1 | 無し（共有の意味論は同一。`QPDFObjectHandle::newStream(QPDF*, shared_ptr<Buffer>)` / `replaceStreamData(shared_ptr<Buffer>, ..)` / `QPDF_Stream::getStreamDataBuffer` に対応する `ObjectHandle::stream` / `replace_stream_data` / `as_stream_data` が buffer を共有したまま受け渡す。`Rc<[u8]>` ではなく `Rc<Vec<u8>>` なのは、`Rc::<[u8]>::from(vec)` が refcount ヘッダを前置できず payload 全体を memcpy するため。二段の間接になるのは `shared_ptr<Buffer>` と偶然一致するだけで対応関係ではない — qpdf が `Buffer` 型を要するのは C++ が borrow/own を型で表せず実行時フラグに畳むからで（`include/qpdf/Buffer.hh:35-46` が所有・非所有の両コンストラクタを持つ）、その面は既存の `Buffer` → `Vec<u8>` 行が扱う。`Rc` なのは `Repr` が `Rc<RefCell<..>>` ベースで `ObjectValue` がそもそも `!Send` のため。`replace_stream_data` は `QPDF_Stream::replaceFilterData`（`QPDF_Stream.cc:668-684`）に対応する共有 helper を通り、zero length では `/Length` を削除、nonzero では正確な integer を設定する（`flpdf-25kg.4.5`）。byte-identical suite（`qpdf-zlib-compat`）で確認済み） |
| `QPDF_Array` borrow / slash 付き canonical name string → `Vec<ObjectHandle>` の単一 child clone / slash 無し decoded `Vec<u8>`、および live array mutation（`object_handle.rs`） | 0 | 無し。`try_array_item` は `QPDF_Array::at` と同じ valid index の child identity を `Rc` clone で返し、name predicate は同じ decoded bytes を比較するだけで出力しない。`set_array_item` / `set_array_items` / `insert_array_item` / `append_array_item` / `erase_array_item` は `QPDFObjectHandle.cc:869-955` と `QPDF_Array.cc:10-26,220-313` の bounds→warning、ownership、live child containment、`setFromVector` の clear-before-check / partial-prefix 順序を保持する。`nntree.rs` の canonical NNTree engine はこの live mutation boundary を `set_array_items` から利用し、旧 `replace_array_item(s)` は qpdf の warning/ownership/insert/erase 契約を持たない compatibility bridge として残る。 |

配列・辞書の `ArrayItemCursor` / `DictItemCursor` もこの ObjectHandle 境界で qpdf の identity を保持する。qpdf の `operator*` は内部 `ivalue` への参照を返すため C++ の `auto&` はカーソル移動を観測するが、コピーされた `QPDFObjectHandle` は選択 child の shared identity を保ったまま移動後も安定する。Rust の `current()` は安全な値返却 API なので後者に対応し、移動後は新しい `current()` を読む。辞書は qpdf の visible key snapshot を維持し、snapshot 内の削除済み key は initialized null、非辞書 receiver は qpdf の contextual warning/null contract、snapshot end だけが uninitialized を返す（`libqpdf/QPDFObjectHandle.cc:2398-2561`; `qpdf/test_driver.cc:1418-1434`）。

### production reverse-containment bookkeeping の除去（`flpdf-ymuj.6.3`）

qpdf の `QPDF_Array` / `QPDF_Dictionary` は forward child handles だけを保持し、
`push_back` / `replaceKey` / `removeKey` で上向きの親indexを更新しない
（`QPDF_Array.cc:33-48,235-286`; `QPDF_Dictionary.cc:10-18,51-56,117-150`）。
`QPDFValue::ChildDescr` の弱い親は optional な object description の一部で、
warning text の `-> dictionary key $VD` を組み立てるための診断文脈であり、
containment root の逆引きではない（`QPDFValue.hh:41-58,74-84`;
`QPDFObject_private.hh:77-92`）。したがって flpdf の
production の ObjectHandle state は qpdf に対応物のない reverse edge を保持せず、
production layout からこの bookkeeping を除去した。**逆引き（lookup）**は
`containing_object_refs`/`containing_object_refs_for_pdf`（いずれも `#[cfg(test)]`）
による current containment-root assertion に限られる。test-only thread-local
registry が live slot の forward children を一度走査して現在の root を導出し、
production の mutation・allocation には追加 state を持たない。ownership・warning・
writer scheduling・output bytes は qpdf と同じ forward graph と value identity だけで
決まる。`active_pdf_unique_id` の単一 value owner 表現とは混同しない。qpdf の forward
teardown（`QPDF.cc:215-235`; `QPDF_Array.cc:103-119`; `QPDF_Dictionary.cc:51-56`）
とも整合する。

qpdf 側の `QPDFValue::ChildDescr` weak parent は description 用だけであり、
array/dictionary の child storage は forward handles のみである
（`QPDFValue.hh:41-58,74-84`; `QPDF_Array.cc:39-58,103-119`;
`QPDF_Dictionary.cc:11-19,50-56`）。flpdf でも shared value alias の propagation
用 owner list は保持しない。

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

### Form pipe/filter overload contract (`flpdf-3yn9.48.66`)

`QPDFPageObjectHelper::pipeContents` (`libqpdf/QPDFPageObjectHelper.cc:518-524`)
and `QPDFObjectHandle::filterAsContents`
(`libqpdf/QPDFObjectHandle.cc:1762-1767`) call the legacy
`pipeStreamData` overload and intentionally ignore its boolean result. The
overall-success boolean from the qpdf 10+ overload is distinct from
`filtering_attempted` (`QPDFObjectHandle.cc:1301-1341`); a false result is not
an exception on these Form routes. Provider/source/pipeline exceptions still
propagate, while `pipeContentStreams` retains its explicit false-to-damaged
content error (`QPDFObjectHandle.cc:1709-1737`). flpdf mirrors this through
`ObjectHandle::filter_as_contents` and the Form branch of
`PageObjectHelper::pipe_contents`; regression coverage exercises false,
provider-error, unknown-filter, filter-setter, and sink-error boundaries
against the pinned qpdf 11.9.0 probe.

### `--check` unknown content-filter error boundary (`flpdf-b8xcx`, 2026-09-16)

qpdf's `QPDFObjectHandle::pipeContentStreams`
(`libqpdf/QPDFObjectHandle.cc:1702-1737`) calls the five-argument
`pipeStreamData` overload. That overload deliberately returns
`filtering_attempted`, not the six-argument overload's overall source/pipeline
success (`libqpdf/QPDFObjectHandle.cc:1300-1325`). An unknown filter can leave
the raw source readable while keeping `filtering_attempted` false; qpdf turns
that result into the typed damaged-PDF error `errors while decoding content
stream`, including the content-stream object identity.

`QPDFJob::doCheck` (`libqpdf/QPDFJob.cc:745-803`) parses every page's content
streams after the full stream traversal, reports that exception as a page
error, and exits through `errors detected`. flpdf's canonical
`ObjectHandle::pipe_content_streams` now consumes the same signal from its
six-argument primitive: `!succeeded || !filtering_attempted` enters the same
typed error boundary. The writer callers continue to use the six-argument
overall-success plus out-parameter contract and retain their qpdf-compatible
raw fallback behavior. The differential regression
`cli_check_exitcodes.rs::check_unfilterable_content_stream_matches_qpdf` uses
an unknown `/PlateDecode` filter and compares qpdf 11.9.0's exit code, stdout,
stderr, and object-specific error text.

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

The attachment-name key follows the same byte-oriented boundary. qpdf's
`QPDFNameTreeObjectHelper` exposes `getUTF8Value()` results
(`QPDFNameTreeObjectHelper.cc:88-98`), and an explicitly UTF-8-prefixed PDF
string returns the bytes after its BOM without revalidating them
(`QPDF_String.cc:162-171`). `JSON::Writer::encode_string` only escapes JSON
syntax/control bytes (`JSON.cc:216-274`); it does not apply lossy UTF-8
conversion. `build_attachments_section_with_version` therefore retains each
key as `Vec<u8>` through sorting and `json_dictionary`, so distinct malformed
UTF-8 keys cannot collapse before qpdf-shaped JSON serialization.

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

### `QPDFWriter::willFilterStream` と refiltered stream dictionary

`.48.64`でwriterのstream dictionary処理をqpdfの責務境界へ戻した。
`canonical_stream_output_with_rewrite_policy`はpipeの2回試行、provider/token-filterの
buffer、`filtering_attempted`を所有するが、辞書キーを先に削除しない。
`writer/object.rs::prepare_stream_dict_entries`が`QPDFWriter.cc:1440-1486`のshallow-copy
ownerとして`/Length`、空`/DecodeParms`、`/Crypt`、成功filter時の`/Filter`/`/DecodeParms`
を出力用copyで処理する。compressionの有無は独立した`add_flate_filter`で表し、metadata
decodeやuncompressではsource filterを消してもFlateを追加しない。外部streamの
`/F`・`/FFilter`・`/FDecodeParms`は全分岐で保持する。

`tests/oracle/qpdf_refiltered_stream_dictionary_probe.cc`はqpdf 11.9.0でrefilter、decode、
filter-on-write veto、metadata、retry provider、token filterを実測し、Rustの
`refiltered_stream_dictionary_tests.rs`が辞書値・retry flag・provider回数・token EOFを
同じrunの契約として固定する。plain cache、specialized、linearizedの各consumerには
同じ`StreamDictionaryOptions`を渡し、linearizedの専用pre-probe自体は変更していない。

2026-09-10（`flpdf-vo76`）: `/F`・`/FFilter`・`/FDecodeParms` を持つ external-file
stream fixture でも、`--stream-data=preserve` の出力を qpdf 11.9.0 と同一runで byte比較した。
`crates/flpdf/tests/cmp_diff_zero_tests.rs::preserve_external_file_stream_matches_qpdf_11_9`
が、in-body payload、直値 `/Length`、外部参照キーの保持をまとめて固定する。

### Linearized `willFilterStream` probe parity (`flpdf-q2nka`, 2026-09-17)

`QPDFWriter::willFilterStream` は、filter planが空でも raw `pipeStreamData` を1回実行し、
planがある場合はwarningを配送したまま2回試行する（`QPDFWriter.cc:1254-1315`）。
flpdfのlinearization probeはこの境界で早期return・warning抑止をしていたため、
壊れたstreamのdecode warning回数と最後のsource-read位置がqpdfより1 path少なかった。
`writer/plain/body.rs::canonical_stream_filter_probe` をqpdfのraw pipe/retry境界へ揃え、
`broken-lzw.pdf`を含む10 fixtureでlinearizeのstderr/statusを、qpdf-zlib-compatでは
出力bytesもqpdf 11.9.0と一致させた。plain側のlegacy `isDataModified` early returnは
このlinearized consumerのscope外である。

2026-09-18（`flpdf-8od1h`）: 上記でscope外としていたplain側の`isDataModified`
early return自体を`canonical_stream_will_be_refiltered_with_policy`から撤去し、
canonical `canonical_stream_filter_probe`へ統一した。**この撤去は production 挙動を
変えていない**（2026-09-19 追記）——当該 wrapper の production 呼び出し元は
`linearization/plan.rs`の`is_data_modified()`の`else`分岐（collapse前の行番号は
`:214`。`flpdf-3yn9.48.184`でこの分岐自体を撤去したため現在は該当行が存在しない）
のみにあり、早期 return（`if handle.is_data_modified()`）は到達しない。plain 経路の caller は
`f8d151deb`（2026-09-12）で既に撤去済みで、そこが実際の cutover。qpdfの`willFilterStream`
（`QPDFWriter.cc:1254`）は`isDataModified() || compress_streams || stream_decode_level`
をfilterフラグへ畳むだけで早期returnを持たない。modified streamを含むlibrary
RED/GREENテスト（`modified_streams_use_the_canonical_refilter_probe`）と
qpdf-zlib-compat byte比較で検証済み（route matrix C22 は `canonical` へ再分類）。

2026-09-19（`flpdf-3yn9.48.184`）: 上記で到達不能と確認した
`linearization/plan.rs`側の`is_data_modified()`分岐（両アームが同じ実引数の
呼び出しに収束済みで vestigial）自体を撤去し、
`canonical_stream_will_be_refiltered_with_policy(handle, options, true,
normalize_content)`の単一呼び出しへ一本化した。出力バイトへの影響は無い
（`cargo test -p flpdf`と`qpdf-zlib-compat`のbyte-identicalテストで確認済み）。

### Linearization stop diagnostics retain qpdf source state (`flpdf-qlwe5`, 2026-09-17)

qpdfのlinearization writerはpage-tree preparationとstream/object setupの後に、
`QPDF::stopOnError`を`damagedPDF("", message)`として現在のInputSource offsetで投げる
（`QPDF_linearization.cc:1190-1194`; `QPDF.cc:2590-2593,2635-2643`）。ページ循環は
`getAllPagesInternal`の`visited`判定位置と、その時点の`last_object_description`を
そのまま`qpdf_e_pages`へ渡す（`QPDF_pages.cc:77-107`）。

flpdfはlinearized writer setupでcanonical page preparationを`getObjectCount`前に置き、
successful `readObject` framing後の`InputSource::last_offset`をqpdfの
endobj/end-after-space境界へ保持する。linearization probeが必要とする
payload-relative offsetは共有source stateとは別に扱う。これにより`filter-on-write-out.pdf`の
`no pages found`（offset 331）と`pages-loop.pdf`の循環（object 3 0）をqpdfと一致させる。
通常rewrite/checkのdiagnosticsは変更せず、q2nkaが所有するlinearized stream probe後の
stop-on-error consumer境界だけを固定する。

### Stream readObject last-offset ownership (`flpdf-o5pr0`, 2026-09-17)

qpdf's `readStream` validates the payload boundary and `endstream`, then
`readObject` consumes the following `endobj` token. The shared InputSource
last offset therefore remains at qpdf's post-endobj framing boundary; qpdf
resets it explicitly only for `readTrailer`
(`libqpdf/QPDF.cc:1312-1328,1330-1357,1360-1399`; `QPDFTokenizer.cc:920-965`).
flpdf now updates the shared last-offset state at the live trailing-token
boundary and no longer rewinds every successful stream parse to payload start.
The self-referential `/Length` recovery remains qpdf-identical, while a valid
indirect-length stream regression asserts the post-endobj source offset.
### qtest 診断キャプチャのスレッド限定 error overlay (`flpdf-39jj2`, 2026-09-17)

**逸脱分類 (C): qpdf に対応物が一切ない flpdf 固有の挙動。出力バイトには影響しない。**

`QPDFObjectHandle::warnIfPossible` は context を持たない値に対して
`QPDFLogger::defaultLogger()->getError()` へ素の文言を書く
（`libqpdf/QPDFObjectHandle.cc:2168-2212`）。この経路は default logger を
直接名指すため、qtest の lib テストが並列に走ると、あるテストのキャプチャが
別テストの警告を吸い込む競合になる。

qpdf 自身のこの問題への答えは **スレッドごとに別の `QPDFLogger` インスタンスを
作ること**で（`include/qpdf/QPDFLogger.hh:33-42` が multi-thread capture の
理由として明記）、default logger 側にスレッド限定の差し替え機構は無い。
しかし `warnIfPossible` が `defaultLogger()` をハードコードしている以上、
別インスタンスではこの経路の警告を捕捉できない。

そこで `crates/flpdf/src/logger.rs` に、所有スレッドにだけ見える error
pipeline の overlay（`LoggerState::error_capture` と
`QPDFLogger::with_error_capture`）を置く。`get_error` は呼び出しスレッドが
overlay の所有者のときだけそれを返し、他スレッドには通常の error sink を
返す。qpdf の `QPDFLogger::getError` は 1 本の error pipeline を無条件に
返すので、この分岐は qpdf に対応物が無い。該当箇所は
`// qpdf-deviation:` / `// qpdf-deviation-start:` … `// qpdf-deviation-end`
で機械可読にマークしてある。

`get_warn` も同じ overlay を参照する（`flpdf-gvung`, 2026-09-17）。明示的な
warn sink が無いとき qpdf の `getWarn` は error pipeline を返す仕様
（`include/qpdf/QPDFLogger.hh:47-48` の "warn -- whatever error points to"）
なので、overlay 有効時に「error が指す先」へ追従するのが qpdf の既定と
整合する。**repair 診断はこの `get_warn` 経路を通る**ため、overlay が
`get_error` だけに効いていた間は qtest の汚染回帰テストが
process-global 実装でも通ってしまい、判別力を持たなかった。

overlay が無効な通常経路の挙動は qpdf と同一で、出力バイト・warning 文言・
配送順はいずれも変わらない。

### `QPDF::getRoot` の test_driver consumer

`libqpdf/QPDF.cc:2355-2368` の `QPDF::getRoot` は trailer の `/Root` を
解決し、dictionaryでなければ `unable to find /Root dictionary` を投げる。
`test_driver.cc:3155-3159,3252,3285` のtest88/93/94はこの検証を通過して
から後続操作へ進むため、qtest driverも`Pdf::root_handle()`を使う。公開APIの
document-neutralなエラーを、qpdfの`QPDFExc::createWhat`
（`libqpdf/QPDFExc.cc:19-51`）と同じfilename付きbyte表示へ戻す処理は、driver
boundaryに限定している。Pinned qpdfで非dictionary `/Root`を与えたtest93は、
修復警告3行の後に`<filename>: unable to find /Root dictionary`を返す。

`flpdf-tdf8` では、`EmbeddedFileDocumentHelper` の読み取り・挿入・削除の
catalog acquisition を `Pdf::root_handle()` へ統一した。これにより qpdf の
`QPDFEmbeddedFileDocumentHelper.cc:33-70` と同じく direct/indirect 両方の
`/Root` dictionary を受け付け、missing/non-dictionary root は
`unable to find /Root dictionary` として伝播する。valid catalog の
`/Names` / `/EmbeddedFiles` 欠損は従来どおり空の name tree として扱う。

`flpdf-3yn9.48.20`ではpublic `Pdf::make_indirect_object_handle`のclone/独自採番を撤去し、
`QPDF.cc:1872-1897`のinitialized検査、canonical count、同じQObjectのcache登録、
`newIndirect`の順に統一した。`ValueIdentity`は`QPDFValue.hh:68-72`の共有objgen/owning QPDFを
表し、replacementで値を共有する別QObjectにも昇格が伝わる。cache lookupとgetAllObjectsは
各keyでidentityを更新する一方、getObjectCountはkeyの最大値を読むだけで列挙しない。
`tests/oracle/qpdf_make_indirect_object{,_states}_probe.cc`とRustのowner testsが
alias/indirect/reserved/destroyed/未解決/最大ID/writer反映を検証する。
履歴trailer参照は`QPDFParser.cc:168-175`のcache登録副作用に合わせ、既存bootstrapの
参照集合をopen時にcanonical cacheへ未解決登録する。getAllObjects時のlate登録・強制解決と
Pdf側の永続集合は撤去した。過去trailerにだけ99があるfixtureでも列挙前のfactoryが100を採番する。

`flpdf-3yn9.48.22`（2026-09-09）で、qpdfの `m->obj_cache` を二重化していた
`crates/flpdf/src/cache.rs` の `ObjectCache` / `CacheEntry` と公開exportを削除した。
`Pdf::get_all_objects` は `ResolverCore::object_cache` の `getAllObjects` 対応を維持し、
writer/linearizationの遅延key走査はsource xrefとcanonical cacheのunionを使うprivate
`canonical_object_refs` / `canonical_live_object_refs`へ移行した。`removeObject`後に
残すtombstoneや `synchronize_cache_with_resolver_xref` / `compressed_member_parents` は
追加せず、stale-generationのremoved setはcompressible walk単位で返す。これは
`QPDF.hh:868-889,1467`、`QPDF.cc:1239-1295,1756-1833,1985-2005,2284-2291`
の責務と一致し、qpdf absent の facade cache/synchronization/provenanceをcanonical
document stateへ混ぜない。

### JSON v2 object-map generation and object-zero parity `flpdf-rbja1` (2026-09-16)

qpdf の `read_xref` は `/Prev` chain を読み終えた後、同じ object number の lower
generation を `removeObject` で xref table と object cache から除去する
（`libqpdf/QPDF.cc:650-725`、`1995-2005`）。その後の `getAllObjects` は
`fixDanglingReferences` を一度済ませた canonical `obj_cache` を raw
`QPDFObjGen` key の順にそのまま列挙する（`libqpdf/QPDF.cc:1239-1269,1285-1295`）。
flpdf はこれまで xref の projection だけを prune していたため、先に trailer から
cache に入った `4 0` が最新の `4 1` と併存し得た。`xref.rs::discard_lower_generations`
は除去した raw key を canonical `ResolverHandle::discard_cached_generations` へ渡し、
cache cell と exact trailer reference も同じ境界で除去する。これは他の generation の
dangling reference を object map から誤って消さないための exact-key cleanup である。

object number zero は parser の `ObjectRef` projection には入らないが、qpdf の raw xref
walk には残る。`readObjectAtOffset` は expected `QPDFObjGen(0, 0)` を「offset の実体を
検査しない probe」として扱い、header mismatch の recovery を行わない
（`libqpdf/QPDF.cc:1541-1553`）。flpdf は `QpdfObjGen(0, 0)` を canonical cache に
materializeし、raw free/type-0 row を qpdf と同じ warning 後の null にし、in-use row は
同じ probe reader を通した後に object-zero slot を null にする。これにより malformed
`issue-143.pdf` の `obj:0 0 R` と warning order、および正常な in-use object-zero xref
row の JSON key が qpdf 11.9.0 と一致する。

回帰は `crates/flpdf-cli/tests/cli_json_object_generation.rs` の synthetic incremental
generation / object-zero tests と、qpdf-qtest の `issue-143.pdf`、既存の dangling-container
および historical-incremental live probes で確認する。通常の trailer-only `0 0 R` は
raw xref row ではないため、従来どおり object map に追加しない。

### A6/A7/A8 final facade cleanup `flpdf-3yn9.48.23.10` (2026-09-12)

qpdf 11.9.0 の `QPDFObjectHandle` typed/null accessors は入口で
`dereference()` し（`libqpdf/QPDFObjectHandle.cc:240-446`）、dictionary key
accessorsも同じ境界で `getKey`/`getKeys`/`hasKey` を処理する
（`libqpdf/QPDFObjectHandle.cc:965-1015`）。明示 `QPDF::resolve` は private で
`QPDFObject` からだけ `Resolver` を通って呼ばれる
（`include/qpdf/QPDF.hh:770-781,1031`; `libqpdf/QPDF.cc:1699-1753`）。

`.23.10` はこの責務に合わせ、通常 flpdf build から `Pdf::resolve`、test-only
`Pdf::resolve_handle`、panic `ObjectHandle::get_key`/`has_key` を撤去した。
core source と全 core/CLI test・example caller は既存の fallible `try_*` routeへ
移行し、`DictItemCursor::current` は live child lookupだけを
`try_get_key`へ切り替えて既存の cursor value contractを保つ。この worktree
（`origin/main` `2928b4ef2` ベース）の非qtest post-cleanup censusは
対象facadeの production/test caller 0件で、
`resolve_handle_ref`/`resolve_qpdf_json_handle`も不存在である。

qtest-toolsの直接 facade callerは、別セッションで扱う qtest-exception boundary
として変更していない。互換のため `Pdf::resolve` と panic key methods は
`qtest-driver` feature にのみ hidden で残し、通常 buildからは公開されない。
source route contract は
`crates/flpdf/tests/final_accessor_route_tests.rs`、qpdf source mirrorは
`/home/ubuntu/.cache/flpdf/qpdf-11.9.0`（pinned HEAD
`3b97c9bd266b7c32ea36d3536e22dab77412886d`）である。

### qtest E-28 tree/mutation accessor cutover `flpdf-3yn9.48.103` (2026-09-15)

qpdf 11.9.0 の test 46/48/89（`qpdf/test_driver.cc:1645-1921,3162-3172`）
には document-level の明示 `QPDF::resolve` 呼び出しがなく、tree value の
typed read は public `QPDFObjectHandle::getStringValue` /
`getUTF8Value`、type-mismatch mutation は public `replaceKey` が内部の
`dereference()` 境界を処理する。根拠は
`libqpdf/QPDFObjectHandle.cc:659-689` と
`libqpdf/QPDFObjectHandle.cc:1197-1209` である。

flpdf-qtest-tools は number/name-tree の値を
`ObjectHandle::try_get_string_value` /
`try_get_utf8_value`、Bad3 の `/Kids` を
`try_get_key` → `try_get_array_item`、test 89 の object 5 mutation を
`ObjectHandle::replace_key` へ移し、qpdf にない caller-side
`Pdf::resolve` を対象 3 ケースから撤去した。挙動は qpdf の
`getKey("/Kids").getArrayItem(0).isIndirect()` と一致し、full qtest
survey は同一 run の `harness.log` + `qtest-results.xml` で
regressions 0、parity verdict OK を確認した。

`.48.103` 監査時点では case 31/42/98 の明示 resolve は別の残差であった。特に case 42/98 の
stream dictionary 読み出しは qpdf の public `getDict` に対応する
flpdf の resolving primitive が未確定のため、その slice では混ぜなかった。

### qtest E-28 test 42/98 canonical accessor cutover `flpdf-3yn9.48.106` (2026-09-15)

qpdf test 42 の `page.getKey("/Contents").getDict()`（`qpdf/test_driver.cc:1407-1551`）は、
`try_get_key` の receiver resolution と public `try_get_stream_dict` へ移し、対象関数内の
caller-side `Pdf::resolve` 7 箇所を撤去した。test 98（`qpdf/test_driver.cc:3425-3450`）は
`writeJSON`/`getJSON(..., true)` の内部 resolution を使用し、stream dictionary mutation を
`try_get_stream_dict` へ移して、対象関数内の `Pdf::resolve` 2 箇所を撤去した。
qpdf の `getDict` の public boundary と lazy assertion 順序は
`libqpdf/QPDFObjectHandle.cc:313-324,1257-1262` に対応し、既存の非 resolving
`as_stream_dict` は代用しない。type-checks の test-driver 42、qpdf-json survey、Rust focused
tests を確認し、case 42/98 の route classification を `canonical` へ更新した。

### qtest E-28 test 2 caller-side resolve cutover `flpdf-3yn9.48.107` (2026-09-15)

qpdf test 2（`qpdf/test_driver.cc:286-308`）は、`getKey` の連鎖で `/Info`、`/Encrypt`、
`/Root`、`/Pages`、`/Kids`、`/Contents` を取得し、暗号辞書の `/O`・`/U` はそのまま
`unparse`、content stream は `pipeStreamData` へ渡す。`QPDFObjectHandle::getKey` は
receiverを解決して辞書値を返す（public declaration `include/qpdf/QPDFObjectHandle.hh:768-773`、
implementation `libqpdf/QPDFObjectHandle.cc:979-989`）。`unparse` は間接値を参照形のまま
返し（`libqpdf/QPDFObjectHandle.cc:1575-1593`）、`pipeStreamData` はstream accessor側で
解決する（`libqpdf/QPDFObjectHandle.cc:1300-1341`）。

flpdf の `ObjectHandle::try_get_key`（`object_handle.rs:3742`）と
`ObjectHandle::get_stream_data`（`object_handle.rs:6218`）はそれぞれ qpdf の resolving
accessor boundaryを担い、後者はqpdfのpipe結果をbufferへ集める。したがって `run_test_2` から `/O`・`/U`・`/Contents` 前の
caller-side `resolve_handle` 3箇所を削除し、間接暗号値の `unparse` と stream read の
解決責務を正本へ戻した。共有 `resolve_handle` は test 4〜9 の `type_code`、
`pipe_stream_data`、`is_null` 用に保持する。source guard、test 2 differential、focused
qtest/workspace gatesで、case 2を`canonical`へ再分類した。E-28全体は他のmixed caseが
残るためmixedのままである。

### qtest E-28 test 6 caller-side resolve cutover `flpdf-3yn9.48.108` (2026-09-15)

qpdf test 6（`qpdf/test_driver.cc:422-439`）は、`root.getKey("/Metadata")` で得た値を
public `isStream()`で解決して型確認し、その後 `pipeStreamData(..., 0, qpdf_dl_none)`へ
渡す。`isStream()`は `dereference()` 後にstream型を判定し
（`libqpdf/QPDFObjectHandle.cc:437-440`）、`pipeStreamData`もstream accessor側で解決する
（`libqpdf/QPDFObjectHandle.cc:1300-1341`）。decode level noneではfilterを実行せず、
暗号化されていれば復号だけを行う。

flpdfの `ObjectHandle::type_code`（`object_handle.rs:7013`）はqpdfのresolve付き型確認を、
`ObjectHandle::pipe_stream_data`（`object_handle.rs:6191`）はstream pipelineを担う。
`run_test_6`からcaller-side `resolve_handle` 1箇所を削除し、test 7〜9の既存明示解決は
今回の対象外として保持した。metadata fixtureでdecode level noneの出力契約を確認し、case 6を
`canonical`へ再分類した。

### qtest E-28 test 3 qpdf array accessor cutover `flpdf-3yn9.48.109` (2026-09-15)

qpdfの`test_3`は、trailerから`/QStreams`を取得した後、公開`getArrayNItems()`で件数を
取り、公開`getArrayItem()`を各indexへ順に呼び出して各streamを処理する
（`qpdf/test_driver.cc:311-322`、宣言は`include/qpdf/QPDFObjectHandle.hh:725-733`、
実装は`libqpdf/QPDFObjectHandle.cc:758-785`）。非配列receiverではcount accessorが
qpdfのtype warningを記録して0件として扱う。各itemのstream dataはその後
`pipeStreamData(..., qpdf_ef_normalize, qpdf_dl_generalized)`へ渡される。

flpdfの`run_test_3`は、配列全体を先にsnapshot化する`try_get_array_as_vector`をやめ、
既存canonicalの`try_get_array_n_items` → `try_get_array_item`を同じcount/item順で呼ぶように
した。count後のdiagnostic drain、各header/normalize pipe、pipe後のdiagnostic drainは
変更していない。focused testsで正常出力、非配列warning、pipeline failureを確認した。
さらにqpdf live probeの`good14.pdf` trailerは
`/QStreams [ 7 0 R 8 0 R 10 0 R 11 0 R 12 0 R 13 0 R ]`を返し、qpdfのraw/filtered
stream 7の先頭は同じ`A %here is a comment` bytesだった。flpdf driverも同fixtureで
`-- stream 0 --`から同じstream prefixを出力し、normalize warningをqpdf-compatibleな
filename/offset付きで排出する。case 3は`canonical`へ再分類した。

### qtest E-28 test 11 canonical root cutover `flpdf-3yn9.48.110` (2026-09-15)

qpdfの`test_11`はpublic `QPDF::getRoot()`でtrailerの`/Root`を取得し、辞書でなければ
`damagedPDF`を投げ、check modeではCatalog `/Type`を検査・修復してlive Catalog handleを
返す（`qpdf/test_driver.cc:538-550`、`include/qpdf/QPDF.hh:311-313`、
`libqpdf/QPDF.cc:2354-2367`）。そのhandleから`getKey("/QStream")`を呼び、公開の
`getStreamData()`と`getRawStreamData()`へ渡す（`libqpdf/QPDFObjectHandle.cc:1288-1298`、
`libqpdf/QPDF_Stream.cc:362-376`）。

flpdfの`run_test_11`は従来、semantic Catalog accessにqpdf対応物のない
`root_ref()` → `get_object_handle()` identity projectionを挟んでいた。既存canonicalの
`Pdf::root_handle()`へ切り替え、qpdfと同じdirect/indirect `/Root` dictionary boundaryを
使うようにした。`try_get_key`、`get_stream_data(DecodeLevel::Generalized)`、
`get_raw_stream_data`は変更せず、`stream-data.pdf`のflpdf driver outputとqpdfの
`test11.out`を`cmp`で比較して一致を確認した。case 11は`canonical`へ再分類した。

### qtest E-28 test 19 resolving key cutover `flpdf-3yn9.48.111` (2026-09-15)

qpdfの`test_19`はpage listからduplicate pageを追加した後、両pageの`/Contents`を
public `getKey()`で取得し、返されたstream referenceの`getObjGen()`を比較する
（`qpdf/test_driver.cc:818-832`、`include/qpdf/QPDFObjectHandle.hh:762-768`、
`libqpdf/QPDFObjectHandle.cc:978-989`）。`getKey()`はreceiverを解決してから辞書値を
返すため、caller側で別のresolve経路を挟まない。

flpdfの`run_test_19`は、既存のPageDocumentHelper snapshotをmutation後に再取得する
境界を保持したまま、`last_handle.get_key` / `newpage_handle.get_key`を
`try_get_key`へ切り替えた。これでqpdfのresolving key accessor責務をcanonical handleへ
戻し、test 21のshallow-copy error用explicit `Pdf::resolve`は別scopeとして残した。
qpdf `page_api_1.pdf`の`test 19` outputとflpdf driver outputを比較し、focused unit
testとsource guardも通過した。case 19は`canonical`へ再分類した。

### qtest E-28 test 21 resolving shallow-copy cutover `flpdf-3yn9.48.113` (2026-09-15)

qpdfの`test_21`はpage listの先頭pageからpublic resolving `getKey("/Contents")`を取得し、
その結果へreceiverを解決するpublic `shallowCopy`を適用する。qpdfの
`QPDFObjectHandle::getKey`は`include/qpdf/QPDFObjectHandle.hh:762-768`/
`libqpdf/QPDFObjectHandle.cc:978-989`、`shallowCopy`のreceiver解決は
`include/qpdf/QPDFObjectHandle.hh:874-881`/
`libqpdf/QPDFObjectHandle.cc:2073-2079`、stream拒否は
`libqpdf/QPDF_Stream.cc:141-145`が責務を持つ。

flpdfの`run_test_21`は`get_key`とcaller-side `Pdf::resolve`を撤去し、`.48.112`でreceiver解決を
持たせたcanonical `shallow_copy`へ`try_get_key`の結果を直接渡した。未到達の
`you can't see this` footerと`stream objects cannot be cloned`のError::System境界は保持した。
`shallow_array.pdf`に対するqpdf `shallow_stream.out`とflpdf driver stderrのexit 2出力、focused
error-contract test、source guardを比較し、case 21を`canonical`へ再分類した。

### qtest E-28 test 17 resolving root and array cutover `flpdf-3yn9.48.114` (2026-09-15)

qpdfの`test_17`は`QPDF::getRoot()`でCatalogを取得し、public `getKey("/Pages")` →
`getKey("/Kids")`を通してから、public `getArrayItem(0/1)`で重複したpage object identityを
確認する（`qpdf/test_driver.cc:776-793`、`include/qpdf/QPDF.hh:311-313`、
`libqpdf/QPDF.cc:2354-2367`、`include/qpdf/QPDFObjectHandle.hh:725-728,762-768`）。
`getKey`はreceiverの`dereference()`を経由し（`libqpdf/QPDFObjectHandle.cc:253-267,978-989`）、
`getArrayItem`もarray receiverを解決してから子handleを返す
（`libqpdf/QPDFObjectHandle.cc:758-785`）。

flpdfの`run_test_17`は従来、Catalogを`root_ref()` → `get_object_handle()`へ投影し、
`/Kids`を非解決の`as_array()`で取り出していた。これはCatalogのsemantic root boundaryと
indirect `/Kids` arrayのresolution orderのいずれもqpdfと一致しない。既存canonicalの
`Pdf::root_handle()`、`ObjectHandle::try_get_key`、`ObjectHandle::try_get_array_item`へ
切り替え、PageDocumentHelperのduplicate-page repair、warning drain、page removal、
`/Contents` identity、filtered stream assertionは変更しなかった。

pinned qpdf 11.9.0 と flpdf の`page_api_2.pdf` test driver出力は、両方exit 0、結合出力
158 bytesで`cmp`一致した。さらにindirect `/Kids`を持つqpdf-shaped fixtureで同じ
resolving array accessorを通る回帰テストと対象関数のsource guardを追加し、case 17を
`canonical`へ再分類した。

### qtest E-28 test 73 resolving unparse cutover `flpdf-3yn9.48.115` (2026-09-15)

qpdfの`test_73`は`closeInputSource()`後に`getRoot().getKey("/Pages").unparseResolved()`を
直接呼ぶ（`qpdf/test_driver.cc:2489-2500`）。`QPDFObjectHandle::unparseResolved`は自身の
receiverを`dereference()`してから値の`unparse()`へ委譲するため、caller側に別の
`QPDF::resolve`は存在しない（`include/qpdf/QPDFObjectHandle.hh:1159-1161`、
`libqpdf/QPDFObjectHandle.cc:1574-1593`）。source closeの責務は
`QPDF::closeInputSource`（`include/qpdf/QPDF.hh:162-166`、`libqpdf/QPDF.cc:278-281`）である。

flpdfの`run_test_73`は従来、canonical `root_handle`/`try_get_key`後にqpdfに対応物のない
`resolve_once`（`Pdf::resolve`）を`/Pages`へ前置し、非fallible `unparse_resolved`を呼んでいた。
既存のfallible `ObjectHandle::try_unparse_resolved`へ直接移し、qpdfのreceiver-resolutionと
エラー境界を同じaccessorへ戻した。`Pdf::uninitialized`、`close_input_source`、warning
drain、closed-source error/statusは変更していない。test75など別のchained accessorが
`resolve_once`を必要とするconsumerはこのsliceの対象外である。

pinned qpdf 11.9.0 と flpdf の`invalid-objects` test73は、どちらもexit 2、350 bytesで
`cmp`一致した。cached root/pagesをclose前に解決する回帰テストとtest73専用source guardを
追加し、case 73を`canonical`へ再分類した。

### qtest E-28 test 87 canonical key enumeration cutover `flpdf-3yn9.48.116` (2026-09-16)

qpdfの`test_87`は、dictionaryのnull-valued entryをmissing keyと同一視し、`unparse()`、
`getKeys()`、`getJSON(JSON::LATEST)`の全てで除外する（`qpdf/test_driver.cc:3086-3103`）。
public `QPDFObjectHandle::getKeys`はreceiverを解決してからdictionaryへ委譲し
（`include/qpdf/QPDFObjectHandle.hh:777-780`、`libqpdf/QPDFObjectHandle.cc:998-1009`）、
`QPDF_Dictionary::getKeys`は各valueをresolving `isNull()`で判定してnull相当のkeyを除外する
（`libqpdf/QPDF_Dictionary.cc:59-78,118-125`）。

flpdfの`run_test_87`は従来、qpdfに対応物のない`direct_non_null_keys`でraw dictionaryを
走査し、direct-only fixture上で非解決`is_null()`を呼んでいた。既存public canonical
`ObjectHandle::try_get_keys`（`crates/flpdf/src/object_handle.rs:3068-3085`）はreceiverと
全childを解決し、null除外・辞書順・resolver error propagationを担うため、3つのgetKeys
assertionをこのaccessorへ移し、local helperを削除した。unparse/replace/JSONのassertionは
qpdfの順序とまま保持した。

source guardと`test 87 done` driver smokeを確認し、qpdf 11.9.0との差分検証でstatus/stdout/
stderrを一致させた。case 87を`mixed`から`canonical`へ再分類した。

### qtest E-28 test 31 lazy null accessor cutover `flpdf-3yn9.48.104` (2026-09-15)

qpdf の `QPDFObjectHandle::isNull()` は public accessor であり、実装は
`dereference()` 後に `ot_null` を判定する（`include/qpdf/QPDFObjectHandle.hh:318-325`、
`libqpdf/QPDFObjectHandle.cc:353-356`）。qpdf test 31 も
`parse(&pdf, "[7 0 R]").getArrayItem(0).isNull()` を呼び、caller が
`QPDF::resolve` を先に呼ぶ構造ではない（`qpdf/test_driver.cc:1174-1214`）。

flpdf の既存 canonical 実装 `ObjectHandle::try_is_null` を public API
（`crates/flpdf/src/object_handle.rs:2996-3008`）として公開し、
`run_test_31` の null item 判定を `try_is_null` へ移した。これにより
`crates/flpdf-qtest-tools/src/driver/test_26_33.rs::run_test_31` の
caller-side `Pdf::resolve` はゼロになり、既存の indirect/direct assertion
と parse error の検証は保持される。case 42/98 の stream dictionary
`getDict` 相当はこの bounded slice の対象外である。

### Linearized root ADBE output ownership (`flpdf-3yn9.48.60`)

`linearization/writer.rs::do_write_pass` emits each pass's Catalog through
`ObjectWriterEmission::output_root_copy_with_adbe`. Progress precedes root
reconciliation and unparse, matching `QPDFWriter.cc:1773-1794`. The final
version/extension pair is fixed before the passes; each pass makes an output
shallow copy while sharing existing direct Extensions values
(`QPDFWriter.cc:1347-1435,2786-2808`). A first-root callback failure leaves
the source extension level unchanged; a second-pass callback or final sink
failure retains the first pass's shared changes. The linearized route no
longer calls the Catalog snapshot/restore helpers. Permanent
`prepareFileForWrite` remains on the common writer boundary.

This is a bounded linearized cutover. Specialized standard output and PCLm
now use the live root serializer (`flpdf-s07c` and `flpdf-3yn9.48.86`). QDF
and content normalization still use the old snapshot/restore helpers because
their pre-emission planning boundary has not yet moved; `flpdf-ay5b` owns that
remaining residual. D25 remains mixed until that root consumer migrates; the
full helper-removal condition is not met here.

### Top-level inspection overlay lifecycle (`flpdf-p4og4`)

Top-level inspection commands that accept `--overlay`/`--underlay` now use the
same `QPDFJob::createQPDF` → `writeQPDF` lifecycle as rewrite and page-selection
jobs. This preserves qpdf's `handleRotations` → `handleUnderOverlay` →
`handleTransformations` ordering (`libqpdf/QPDFJob.cc:466-473`), applies the
job's password and recovery policy to every donor (`libqpdf/QPDFJob.cc:1818-1845`),
and keeps donor path attribution at the job error boundary. The CLI regression
cases compare rotation, segment password mode, and donor-password failure
against qpdf 11.9.0.

### Combined show-encryption partial-open lifecycle (`flpdf-hy1g2`)

The combined top-level inspection route now also preserves qpdf's
`createQPDF` password-error boundary. When `show_encryption` is configured,
`QPDFJob::create_qpdf` uses the existing partial encryption-inspection opener;
an encrypted document without a derived file key emits the parsed
`showEncryption` report and returns the successful null-document result before
update, transformation, or `writeQPDF` continuation. Ordinary open failures
remain errors, while successful authentication continues through the existing
create-stage and combined-inspection lifecycle. This corresponds to
`libqpdf/QPDFJob.cc:432-448,459-480,513-520` and is covered by the six
wrong-password combined-inspection cases plus a missing-input differential in
`cli_inspection_combinations.rs`.

### Linearized Generate setup snapshot (`flpdf-9zbro`, 2026-09-15)

qpdf computes Generate ObjStm membership during writer setup, before
`prepareFileForWrite` directizes indirect Catalog `/Extensions` values
(`libqpdf/QPDFWriter.cc:1970-2006,2034-2055`). The subsequent linearized plan
consumes that membership while assigning the part layout
(`libqpdf/QPDFWriter.cc:2537-2645`); it does not replace it with a
post-preparation reachability walk.

flpdf's `PdfWriter::WriterSetupState::generated_compressible` is now passed to
`LinearizationPlan`. The plan keeps setup-time eligible references in its
linearized object universe and uses the setup stale-generation set and
even-split count. This preserves an indirect `/Extensions` dictionary as the
same ObjStm member qpdf emits after Catalog directization. The linearized
two-pass layout and emission remain dedicated consumers; only the Generate
membership boundary was changed. The live regression is
`cmp_linearize_objstm_tests.rs::indirect_extensions_linearized_objstm_is_byte_identical_to_qpdf`.

### Linearized Generate page/object-stream setup order (`flpdf-svhr3` + `flpdf-bmo81`, 2026-09-17)

qpdf gates `initializeSpecialStreams` on qdf/normalization/decode state, then
computes Preserve/Generate object-stream membership, and only afterward walks
linearized pages to remove page dictionaries from the membership map
(`libqpdf/QPDFWriter.cc:1912-1936,1970-2006,2114-2150`). The writer's object
count and common preparation follow that setup (`QPDFWriter.cc:2187-2200`).

flpdf now keeps the same boundary in both consumers: `LinearizationPlan` runs
page preparation before planning only for the special-stream trigger; the
default linearized Generate/Preserve path plans object streams first and then
repairs/pages-scans. `PdfWriter::write` applies the same delayed page repair
after Generate capture and before `get_object_count`. This avoids direct `/Kids`
promotion changing the even-split count. The live qpdf/flpdf regression uses 98
reachable eligible objects and a direct page kid, where qpdf emits one ObjStm;
the test compares warning/status, container count, and output bytes.

### Direct outline first-half ordering (`flpdf-oqz1e`, 2026-09-15)

qpdf promotes a direct Catalog `/Outlines` dictionary during
`QPDF::optimize`, before `calculateLinearizationData` builds the part6
sequence. `pushOutlinesToPart` then emits the plain outline root before the
remaining `lc_outlines` set; when that set contains a generated ObjStm
container, the root therefore precedes the container
(`libqpdf/QPDF_optimization.cc:57-82`; `libqpdf/QPDF_linearization.cc:1188-1216,1406-1432`).

flpdf's first-half placement now carries the outline-batch boundary and the
plain outline-root identity into `RenumberMap::place_objstm_members_per_half`.
It emits ordinary first-page containers, the outline root, outline containers,
and then ineligible outline streams, preserving the existing q9o3 stream
ordering. The strict live regression is
`cmp_linearize_objstm_tests.rs::direct_outlines_linearized_objstm_is_byte_identical_to_qpdf`,
with a default-feature root/container ordering guard in
`linearize_objstm_generate_tests.rs`.

### QPDFJob writer/inspection option acceptance (`flpdf-urjhr`, 2026-09-15)

qpdf 11.9.0 の `QPDFJob::checkConfiguration` は、output-free inspection と
`--encrypt` / `--decrypt` / `--linearize` の組み合わせを相互排他にしない。
`createQPDF` が先に `handleTransformations` を実行し、`writeQPDF` が
`createsOutput()` に応じて `doInspection` または `writeOutfile` を選ぶためである
（`libqpdf/QPDFJob.cc:428-520,567-642`）。JSON outputでも同じcreate-stage順序を
保ち、attachment mutationはJSON serialization前に適用される
（`libqpdf/QPDFJob.cc:2044-2248,3030-3057`; `libqpdf/QPDFJob_config.cc:311-324`）。

flpdfはtop-level `Cli` の該当conflicts_withを除去し、JSON routeでは既存の
`QPDFJob::apply_transformations`へattachment設定を渡してから
`QPDFJob::write_json_with_version`を呼ぶ。`cli_qpdf_conflict_matrix.rs` は、
attachmentのadd/remove/copy、encrypt/decrypt/linearizeとJSON、encrypt/decryptと
check/show-npagesの10通りをpinned qpdf 11.9.0とstatus/stdout/stderr比較する。
qpdfに対応する独自compatibility bridgeやdeviation markerは追加しない。

### QPDFJob remaining writer/inspection conflict acceptance (`flpdf-zet2u`, 2026-09-15)

qpdf 11.9.0の同じ `checkConfiguration` / `createQPDF` / `writeQPDF` 境界に対し、
top-level clapが残していた9組の誤った相互排他を除去した。対象は
`--encrypt` と `--check-linearization`/`--show-encryption`/`--show-pages`、
`--decrypt` と `--show-encryption`/`--show-pages`、
`--compress-streams=n`/`--qdf` と `--json`、および
`--rotate=90`/`--pages . 1 --` と `--check-linearization` である。
writer-only設定はqpdfと同じくoutput-free inspectionまたはJSON serializerでは
writerを起動せず、inspection/JSONの処理を継続する。`--json-output`も
`Config::jsonOutput`が`json`を設定する同一責務のため、最後の2つのwriter-option
受理を対称に適用した（`QPDFJob_config.cc:311-324`）。

`crates/flpdf-cli/tests/cli_qpdf_conflict_matrix.rs` は既存10組にこの9組と
`--json-output`の2組を加え、qpdf 11.9.0との終了コード・stdout・stderr・JSON
出力を比較する。別のinspection consumerの未移行conflictをこのbounded sliceへ
取り込まず、qpdfに対応するbridgeやdeviation markerも追加しない。

### QPDFJob JSON writer-option conflict acceptance (`flpdf-p50gt`, 2026-09-15)

qpdf 11.9.0 は `--static-id`、`--deterministic-id`、
`--coalesce-contents` を JSON stdout/file output と相互排他にしない。
`checkConfiguration` にこの9組の検査はなく、`createQPDF` が create-stage
変換を終えてから `writeQPDF` の JSON consumer を選ぶためである
（`libqpdf/QPDFJob.cc:459-480,567-642,3030-3057`;
`libqpdf/QPDFJob_config.cc:88-92,162-166,247-325,619-623`）。
ID の2設定は JSON serializer では writer-only のため出力処理を妨げず、
`coalesce-contents` は JSON serialization 前の create-stage で適用される。

flpdf は top-level `--json` / `--json-output` の clap conflict からこの3
optionを除去し、3分岐の JSON route で `coalesce_contents` を既存の
`QPDFJob::apply_transformations` 境界へ渡してから
`QPDFJob::write_json_with_version` を呼ぶ。テストは3 option × 3 JSON mode
の9組について qpdf 11.9.0 と exit status・stdout・stderr・JSON output
bytes を比較する。未監査の route-specific conflict はこの issue の範囲に
含めず、独自 bridge や qpdf-deviation marker は追加しない。

### Donor password error ownership (`flpdf-ghk8d`, 2026-09-15)

qpdf の `QPDFJob::copyAttachments` は各 donor を `processFile` で開き、
password exception をそのまま copy loop の外へ伝播させる
（`libqpdf/QPDFJob.cc:2089-2100`）。CLI の `qpdf/qpdf.cc` はこの
`std::exception` を最上位で `qpdf: <what()>` として表示する
（`qpdf/qpdf.cc:32-43`）。従って donor 認証失敗の分類は job/library
境界で保持し、donor path の文字列化は CLI reporting 境界に限定する。

flpdf は `prepare_document_transformations` で donor path を job の入力名に
保持し、public `QPDFJob::apply_transformations` からは
`Error::Encrypted(BadPassword)`（または diagnostics を伴う同じ source）を
返す。CLI は typed bad-password だけを `QPDFJob::report_job_error` へ渡し、
qpdf と同じ donor-path 診断を出して既存の exit-2 sentinel で終了する。
この分離により通常書き出しと JSON の表示を変えず、ライブラリ利用者が
`Error::Encrypted` を pattern-match できる。回帰は
`crates/flpdf/tests/job_lifecycle_tests.rs` の public API テストと
`crates/flpdf-cli/tests/cli_json_donor_policy.rs` の通常/JSON differential
で固定し、独自 error variant や deviation marker は追加しない。

### JSON rotation consumer (`flpdf-lvvvk`, 2026-09-16)

qpdf は `createQPDF` で page selection の後に `handleRotations` を実行し、
その後に underlay/overlay と `handleTransformations` を続ける
（`libqpdf/QPDFJob.cc:459-480`）。`Config::rotate` は raw parameter を検証して
rotation mapへ保存し（`libqpdf/QPDFJob.cc:368-415`;
`libqpdf/QPDFJob_config.cc:786-790`）、`handleRotations` は canonicalな
page列挙へ各 rangeの `rotatePage` を適用する
（`libqpdf/QPDFJob.cc:2635-2652`）。JSON serializer はこの create-stage
完了後の document を読む。

flpdf の JSON route は `cli.page_ops.rotate` を既存の
`QPDFJob::Config::rotate` へ積み、`apply_transformations` が所有する
`apply_configured_rotations` 境界を通してから JSON stdout/file serializerを
呼ぶ。これにより `--pages` がある場合も page selection後の output-page
numberingと、qpdfの rotation → transformation orderを共有する。対象の
`--rotate=90/180/270` × `--json`/`--json=2` 6組と JSON file 3組を
qpdf 11.9.0とstatus/stdout/stderr/bytes比較し、coalesce部分は統合済み
`flpdf-p50gt`（PR #2002）として別責務で維持する。独自の JSON rotation
routeやdeviation markerは追加しない。

### Standalone inspection rotation consumer (`flpdf-9r7ti`, 2026-09-16)

qpdf の `createQPDF` は page selection後に `handleRotations` を呼び、
`writeQPDF` の `doInspection` が同じ準備済みdocumentを読む
（`libqpdf/QPDFJob.cc:459-480,483-490`）。`doInspection` の
`doShowObj`分岐もこの順序の後段にあるため、`--rotate` と
`--show-object` の組み合わせは回転後のページ辞書を出力する
（`libqpdf/QPDFJob.cc:1645-1689,806-835`）。

flpdf の `QPDFJob::Config::rotate` / `apply_configured_rotations` は既に
同じrange parser・page helper責務を持っていたが、standalone inspectionの
`InspectionTransformOptions`がrotationを運ばず、`run_show_object`等の
manual-open consumerが回転前documentを表示していた。`flpdf-9r7ti` はraw
`OsString` rotation parameter sliceを共通inspection configuratorへ渡し、
既存の `QPDFJob::apply_transformations` 境界でrotationを他の変換より前に
適用する。個別show-object分岐や別parserは追加していない。

`crates/flpdf-cli/tests/cli_inspection_combinations.rs::standalone_show_object_inspection_applies_rotation_before_the_consumer`
が `--rotate=90/180/270 --show-object=3,0` の qpdf 11.9.0
status/stdout/stderrを比較し、既存のJSON/output/overlay rotation consumerは
それぞれのcanonical Job routeを継続利用する。

### Standalone rotation usage preflight (`flpdf-9orwb`, 2026-09-16)

qpdf の `Config::rotate` callback は argv 初期化中に
`parseRotationParameter` を呼ぶため、`createQPDF` の `processFile` より前に
不正値を usage として確定する（`libqpdf/QPDFJob_config.cc:786-790`、
`libqpdf/QPDFJob.cc:368-415,428-435`）。この境界は `--check`、各
`--show-*`、attachment inspection、`--is-encrypted`、
`--requires-password` に共通する。

flpdf は top-level raw rotation parametersを dispatch/input open前に既存の
`QPDFJob::Config::rotate`へ preflightし、typed `UsageError`を共通の
`usage_exit`へ渡す。その後の各consumerは従来どおり同じ raw parameterを
実ジョブへ設定して、validな rotationの適用・inspection順序を変えない。
`cli_inspection_combinations.rs::standalone_inspection_reports_invalid_rotation_before_input_open`
は standalone inspection/status 12経路の missing-inputで qpdf 11.9.0の
exit/stdout/stderrを比較する。個別parser、bridge、qpdf-deviation markerは
追加しない。

### QPDFJob conflict inventory and canonical CLI routing (`flpdf-sg6tu`, 2026-09-16)

qpdf 11.9.0 の最終 configuration check は、`--replace-input` と
output/`--split-pages`/`--json`/`--empty`、output-free consumer と
output、`--requires-password` と `--is-encrypted`、および同一 input/output
だけを usage 境界として扱う（`libqpdf/QPDFJob.cc:567-631`）。
writer-only 設定、JSON input/update、overlay/underlay、attachment mutation、
image/annotation/content transformation、password と password-file は
configuration state に積まれ、相互排他にされない
（`libqpdf/QPDFJob_config.cc:27-40,143-171,364-381,528-679,766-769,794-831,1088-1166`）。

qpdf は `createQPDF` で input/JSON input、page/rotation、underlay/overlay、
全 transformation を完了し、`writeQPDF` で output-free なら同じ document を
`doInspection` に渡す。inspection の各 report と attachment
remove/add/copy は独立した順序付き branch であり、前段の変異結果を後段が観測する
（`libqpdf/QPDFJob.cc:428-520,1646-1693,2046-2247`）。

flpdf はこの責務に合わせ、top-level `Cli` の qpdf 非対応
`conflicts_with` を定義側から全数監査した。qpdf semantic guard（output-free
inspection/output、JSON implicit output、password-status pair）は残し、
writer-only・transformation・JSON input/update・overlay・attachment の
非対応 guard は除去した。`attachment_op` の parser-level ArgGroup も除去し、
既存の `QPDFJob::prepare_document_transformations` が所有する
remove → add → copy orderへ接続した。top-level attachment mutation は
`run_all_attachment_mutations` から既存の `QPDFJob` create/write boundaryへ
入り、JSON input/update、page selection、overlay、inspection を同じ state で扱う。
`password`/`password-file` は raw argv の最後の setter を保持する。

`args_conflicts_with_subcommands` と `--repair`（qpdf にない flpdf native
extension）との recovery guard、native `rewrite` の mode guard は qpdf flat
option conflict とは別の native surface として残る。qpdf semantic usage の
本文と `For help:` 報告境界は `flpdf-qjip8` の後続責務であり、この slice は
受理集合と create/inspection routingを対象にする。

`crates/flpdf-cli/tests/cli_qpdf_conflict_matrix.rs` は writer/create-stage
22 種 × inspection 12 種の 264 組を定義側の固定配列で列挙し、qpdf 11.9.0 と
exit status/stdout/stderrを比較する。JSON input/update selector、mixed
attachment mutation、attachment + overlay、password setter order、
job-json-file の check-linearization も個別 differential test で固定する。
独自 bridge や qpdf-deviation markerは追加しない。

### JSON input/update inspection continuation (`flpdf-lm4bc`, 2026-09-16)

qpdf の `createQPDF` は JSON input の作成と update を完了した後、page
selection/rotation、underlay/overlay、`handleTransformations` を同じ
documentへ順に適用する。`writeQPDF` はその準備済み documentを
output-freeなら `doInspection` へ渡し、`checkConfiguration` は
`show-attachment` の save pipelineを inspection report より前に予約する
（`libqpdf/QPDFJob.cc:428-520,614-626,914-925,1646-1693,1937-2015,2138-2194`）。

flpdf の `run_json_input_inspection` は、JSON input/update の作成・更新後に
`QPDFJob::apply_transformations` を呼び、rotation、overlay/underlay、その他の
create-stage transformationを canonical job ownerへ渡す。`show-attachment` は
JSON import/open より前に同じ logger の save pipelineを予約し、最後は
`QPDFJob::inspect_configured` の独立 report/completionへ接続する。これにより
`show-npages` の先行 info が attachment payload の stdoutを消費せず、JSON
input/update の overlay/underlayも `show-object` が観測する。

`crates/flpdf-cli/tests/cli_qpdf_conflict_matrix.rs::json_input_and_update_inspection_reserve_stdout_before_attachment`
は JSON input と update-from-JSON の `show-npages` + `show-attachment` を、
`crates/flpdf-cli/tests/cli_qpdf_conflict_matrix.rs::json_input_and_update_inspection_apply_overlay_before_show_object`
は両入力経路の overlay/underlay + `show-object` を qpdf 11.9.0 と
status/stdout/stderr 比較する。新しい bridgeや qpdf-deviation markerは追加しない。

### Top-level attachment mutation with a single inspection (`flpdf-awthm`, 2026-09-15)

qpdf's `createQPDF` always completes `handleTransformations`, including
attachment remove/add/copy, before `writeQPDF` chooses the output-free
`doInspection` column (`libqpdf/QPDFJob.cc:428-489,1646-1693,2046-2248`). The
top-level flpdf CLI now sends any single inspection selector combined with an
attachment mutation through the existing `QPDFJob` configuration and
completion boundary. `--check`, `--list-attachments`, and `--show-npages`
therefore inspect the mutated document and preserve mutation-stage exit-2
errors. The page-selection inspection route receives the same attachment
configuration before `QPDFJob::run`, so its selected document follows the
same transformation/inspection order. qpdf 11.9.0 status/stdout/stderr
differential coverage is in
`crates/flpdf-cli/tests/cli_inspection_combinations.rs`.

### Multi-source primary document graph retention (`flpdf-lrm3u`, 2026-09-15)

qpdf's `QPDFJob::handlePageSpecs` keeps the primary `QPDF` in place while it
removes and re-adds the primary pages (`libqpdf/QPDFJob.cc:2462-2472`). The
same job then updates only selected-page labels and AcroForm field ownership
(`libqpdf/QPDFJob.cc:2514-2632`). Consequently, the primary Catalog, the
non-structural values on its root `/Pages` dictionary, and the non-writer-owned
trailer entries remain part of the writer graph. `QPDFWriter::getTrimmedTrailer`
removes only `/ID`, `/Encrypt`, `/Prev`, and the seven xref-structural keys;
`/F`, `/FFilter`, and `/FDecodeParms` are retained and enqueued
(`libqpdf/QPDFWriter.cc:2009-2031,2907-2925`). A direct trailer `/Root` is
also emitted directly by `unparseChild` rather than being promoted to an
indirect Catalog (`libqpdf/QPDF.cc:2349-2358`; `libqpdf/QPDFWriter.cc:1144-1155`).

flpdf's fresh `Pdf::empty()` merge target now copies the primary root
`/Pages` non-structural graph through `object_copy::copy_foreign_value`, aligns
the page-merge trailer skip list with qpdf, and restores a direct primary
`/Root` shape after the shared Catalog mutation boundary. The field-map snapshot
is taken after Catalog copying only for the qpdf job consumer; the generic
`merge_documents` path keeps its page-graph-only field selection semantics.
Foreign-copy reservation applies the qpdf writer's stale-generation cleanup
only to the primary page-merge graph when `getCompressibleObjGens` actually runs:
Preserve with source ObjStms and no `--preserve-unreferenced`, or Generate. It
leaves a missing lower generation as an indirect null for Disable, Preserve with
`--preserve-unreferenced`, and ordinary `copyForeignObject` calls, matching
`getObject` and `removeObject` (`libqpdf/QPDF.cc:1952-1959,1996-2005`;
`libqpdf/QPDFWriter.cc:1939-1983`). The preserve-unreferenced live queue also
sorts imported handles by the recorded primary source identity before assigning
output numbers, matching `getAllObjects` (`libqpdf/QPDF.cc:1285-1294` and
`libqpdf/QPDFWriter.cc:2907-2925`). No legacy closure bridge or qpdf-deviation
marker was added.

`crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs::pages_preserves_primary_document_graph_edge_fixtures`
compares qpdf 11.9.0 with flpdf for the four original fixtures plus
`direct-root-one-page` and the unindexed stale-generation fixture across its
writer-mode exceptions. A separate regression covers preserve-unreferenced
trailer graph order, including `/Info` before `/F`. The sweep is bounded to
primary document-graph retention; the unrelated
`null-visible-stale-generation-objstm` member-order difference and the
`flpdf-x267z` page-count issue remain separate.

### PCLm seed joins the one standard-writer queue (`flpdf-3yn9.48.152`, 2026-09-18)

qpdf has one `object_queue` and two seed passes: `writeStandard` calls
`enqueueObjectsPCLm` when `m->pclm` is set and `enqueueObjectsStandard`
otherwise, then runs the same write loop, `writeXRefTable`/`writeXRefStream`
and `writeTrailer` for both (`libqpdf/QPDFWriter.cc:2906-2955,2999-3005`).
flpdf now has the same shape: `writer/plain/body.rs::enqueue_objects_pclm` and
`::enqueue_objects_standard` seed the one `LiveQueue`, and
`::initialize_live_queue` selects between them from `options.pclm`. The former
`writer.rs::write_pclm` route — with its own body loop, classic-xref loop,
late-trailer numbering and `pclm.rs::Plan`/`Builder`/`EmissionQueue`/`Item`
planner — is gone; `LiveQueue::new` is unchanged.

Three deviations closed with it, each pinned by a qpdf 11.9.0 byte golden in
`tests/fixtures/pclm/`:

* `doWriteSetup` changes only `stream_decode_level`, `compress_streams` and
  `encrypted` for PCLm (`libqpdf/QPDFWriter.cc:2071-2076`). flpdf additionally
  forced `qdf = false` and `object_streams = Disable`, so PCLm + QDF and
  PCLm + generated object streams were unreachable. Both now match qpdf byte
  for byte.
* The image-transform stream is a real indirect object in the source document
  (`QPDFObjectHandle::newStream(&m->pdf, ...)`), allocated inside the strip
  loop. `Item::Synthetic` had no qpdf counterpart and left the QDF
  `%% Original object ID:` lines unrepresentable; `Pdf::new_stream_with_data`
  now allocates it where qpdf does.
* A missing trailer `/Root` reported `Error::Missing("/Root")` on the PCLm
  route where `QPDF::getRoot` reports `unable to find /Root dictionary`
  (`libqpdf/QPDF.cc:2355-2360`); PCLm now shares the standard message.

`write_pclm` also normalized a missing trailing newline on
`extra_header_text` a second time. qpdf normalizes only in
`QPDFWriter::setExtraHeaderText` (`libqpdf/QPDFWriter.cc:269-278`), which
`PdfWriter::set_extra_header_text` already mirrors, so the route-local repeat
was removed with the route.

One divergence found while gating this was left open at the time and is now
closed by `flpdf-3yn9.48.160` (see the next section): `pages::page_refs`
dropped a `/Kids` leaf that is not a dictionary, while `QPDF::getAllPages`
keeps it, so qpdf wrote such a leaf as the first PCLm object and flpdf did not.

### `page_refs` classifies a `/Kids` leaf the way qpdf does (`flpdf-3yn9.48.160`, 2026-09-18)

`QPDF::getAllPagesInternal` decides whether a kid is an interior node or a
leaf page with `kid.hasKey("/Kids")` (`libqpdf/QPDF_pages.cc:100-103`), and
`QPDF::getAllPages` enters that recursion only for a `/Pages` root that
reports `/Kids` (`libqpdf/QPDF_pages.cc:69-71`). `pages::PageWalk` — the
non-repairing walk `pages::page_refs` uses when the prepared page cache is
empty — dispatched on `/Type` instead, so it dropped a non-dictionary leaf and
classified a `/Pages` root labelled `/Type /Page` as a page of its own. Both
now follow qpdf's `/Kids` dispatch. A non-dictionary leaf is returned
unchanged, which is byte-exact because every repair the leaf arm attempts (the
`/MediaBox` default and the `/Type` override) is an "ignoring key replacement
request" no-op on a non-dictionary receiver
(`libqpdf/QPDFObjectHandle.cc:1199-1208`).

Observed against qpdf 11.9.0: `tests/fixtures/pclm/mini-pclm-nondict-page-*`
pins the complete PCLm output (`1 0 obj\n42\nendobj` first, 1117 bytes) and a
second scenario — `tests/fixtures/compat/three-page.pdf` with its first page
replaced by an integer — is byte-identical through the same route. A
CLI sweep over the same input (plain write, `--qdf`, `--linearize`,
`--object-streams=generate|disable`, `--decode-level=all`,
`--normalize-content`, `--flatten-annotations`,
`--remove-unreferenced-resources`, `--pages`, `--split-pages`,
`--overlay`, `--underlay`, `--json-output`, `rewrite`, `--check`,
`--show-npages`, `--json-key=pages`) produces identical output bytes, stdout,
stderr and exit codes before and after: those consumers reach the page list
through `PageDocumentHelper::get_all_pages` / `pages::repair`, which already
implemented qpdf's leaf dispatch and its repairs.

The one CLI-observable change is a diagnostic, on the multi-source `--pages`
merge (`--empty --pages in.pdf 1-z --` and `--collate`), which reads its
source page list through `page_refs` directly. qpdf writes both outputs with
warnings (exit 3); flpdf rejected them before this change and still does
(exit 2, no output), but the message moved from `--pages: merge produced 1
pages for 2 selected pages` — the count mismatch left by dropping the leaf —
to `object 3 0 R is not a page dictionary or Form XObject`, raised by the
page-copy step now that the list is qpdf-correct. Closing that one belongs to
the page-copy path, which has to tolerate a non-dictionary page the way
`QPDFPageDocumentHelper::addPage` does.

Three page-tree classification divergences remain open, all of them cases
where qpdf's leaf arm performs a repair that actually lands on a dictionary
and that `PageWalk` does not perform at all (`pages::repair` does). Measured
against qpdf 11.9.0 on purpose-built fixtures. Two of the originally listed
gaps are closed: an indirect kid without `/Kids` is now pushed as a
`PageNode::Leaf` (`crates/flpdf/src/pages.rs`) and a direct kid is promoted
with `make_indirect_object_handle` before it is yielded, both byte-gated
against qpdf-generated PCLm goldens (`mini-pclm-nondict-kid-*` for a direct
kid, `mini-pclm-nondict-page-*` for an indirect one).

What remains is the **dictionary** dispatch, which still keys on `/Type`:
a dictionary kid with neither `/Type` nor `/Kids` is a page for qpdf (which
also writes `/Type /Page` into it) and is dropped here; a dictionary kid
carrying both `/Type /Page` and `/Kids` is an interior node for qpdf (which
rewrites its `/Type` to `/Pages`) and is returned as a page here. A repeated
non-dictionary leaf is also lost, because `PageWalk`'s `seen` set is shared
between interior nodes and leaves while qpdf shallow-copies a duplicate page.
Closing these means giving `page_refs` qpdf's repairing walk, which is the
`pages::repair` / `PageWalk` consolidation, not a change to the dispatch.

The writer and `--check` routes already reach the full leaf arm through
`pages::repair`: measured on a non-dictionary kid fixture, qpdf 11.9.0 and
flpdf both emit the same six warnings in the same order (key-containment,
key-retrieval, `MediaBox is undefined`, `ignoring key replacement request`,
`/Type key should be /Page but is not`, `ignoring key replacement request`).
The diagnostic gap is specific to the non-repair `PageWalk` route.

### PCLm Generate setup membership (`flpdf-xom94`, 2026-09-18)

qpdf computes Generate ObjStm membership in `doWriteSetup`, before
`prepareFileForWrite` directizes an indirect Catalog `/Extensions` dictionary
(`libqpdf/QPDFWriter.cc:1970-2006,2034-2055,2187-2200`). flpdf's PCLm route
already used the shared standard `LiveQueue`, but its setup capture predicates
excluded `options.pclm`. Removing those three exclusions lets PCLm + Generate
consume the same `WriterSetupState.generated_compressible` snapshot as the
other standard consumers. The new `mini-pclm-ext-indirect-objstm.pdf` golden
and `pclm_qpdf_byte_gate_tests.rs` compare the complete output bytes against a
pinned qpdf 11.9.0 C++ oracle. No qpdf-deviation marker is added.

`flpdf-3yn9.48.164` (2026-09-18) superseded those three predicates entirely.
The qdf, content-normalization, encryption, copied-encryption, encrypted-source
and extra-header-text routes reached the same post-`prepareFileForWrite` rewalk
that PCLm did, so the capture is now gated on nothing but
`effective_object_stream_mode(&options) == Generate`, matching qpdf's bare
`switch (m->object_stream_mode)` (`libqpdf/QPDFWriter.cc:2125-2139`). The
`plain::eligible` route predicate had no qpdf counterpart and was removed with
them, and `build_live_object_stream_plan` now requires the setup snapshot on
its Generate arm instead of rewalking the prepared graph.

### flpdf-cli --json joins writeQPDF/writeOutfile/writeJSON (`flpdf-3yn9.48.150.3`, 2026-09-18)

qpdf reaches JSON output through one structure: `createQPDF` runs
`checkConfiguration`, which defaults the JSON destination to `-` when no output
file was given (`libqpdf/QPDFJob.cc:428-431,582-586`); `writeQPDF` dispatches on
`createsOutput()`; and `writeOutfile` rewrites the output name before
`writeJSON` picks its destination from it
(`libqpdf/QPDFJob.cc:484-489,3031-3042,3093-3115`). flpdf-cli's `--json` route
bypassed all of it — it selected a `JsonJobOutput` itself and called
`QPDFJob::write_json_with_version` directly, leaving `json_version` and the
output file unset on the job, so `creates_output()` answered `false` for every
JSON invocation. The route now configures the job (input/output file, JSON
version and mode, keys, object selectors, stream data, stream prefix, schema
validation, decode level), calls `QPDFJob::check_configuration`, and hands the
created document to `QPDFJob::write_qpdf`.

Two qpdf divergences on the job's own JSON arm were fixed with it, both observed
against qpdf 11.9.0 through the job-JSON route before the cutover:

* `writeJSON` opens its destination with `QUtil::safe_fopen(..., "w")`
  (`libqpdf/QPDFJob.cc:3103-3104`), whose failure reads `open <path>:
  <strerror>`; flpdf reported `open JSON output <path>: ...`.
* `writeJSON` calls `usage()` when file-mode stream data has no prefix and no
  output name to derive one from (`:3105-3110`). That `QPDFUsage` escapes
  `writeOutfile`/`writeQPDF`/`run` uncaught and reaches the CLI's `usageExit`
  (`qpdf/qpdf.cc:37-38`); flpdf reported it as an ordinary job error and folded
  it into an exit status, losing qpdf's usage block. `QPDFJob::write_qpdf` and
  `QPDFJob::run` now propagate `Error::Usage` unreported, as
  `QPDFJob::create_qpdf` already did.

Deviations recorded with the cutover:

* `QPDFJobConfig`'s new JSON selectors (`json`, `json_output`, `json_key`,
  `json_object`, `json_stream_data`, `json_stream_prefix`, `test_json_schema`,
  `decode_level`) take parsed values where qpdf's `Config` callbacks parse
  strings (`libqpdf/QPDFJob_config.cc:253-340,717-732`); flpdf's argv boundaries
  own that parse and report the same messages. `decode_level` sets the JSON
  consumer's level only, because flpdf keeps the writer's copy of qpdf's single
  `m->decode_level` in the writer configuration. `json_key` keeps qpdf's
  `std::set` semantics by ignoring a repeated key.
* `job/lifecycle.rs::JobOutputWriter` batches fragments before the save
  pipeline. Both of qpdf's JSON destinations are buffered before reaching the
  operating system — a `FILE*` through `Pl_StdioFile` (`:3103-3104`) and the
  C++ stream whose `stdout` the CLI puts in line-buffered mode
  (`qpdf/qpdf.cc:30`, `libqpdf/QUtil.cc:780-784`) — and flpdf's file
  destination already batched through `StdioBuffer`. Output bytes are
  unchanged. flpdf-cli's own `PipelineWriter` batching, which covered only the
  route it called directly, was removed with it.
* `open_verified_json_output`'s file-identity comparison after the path check
  has no qpdf counterpart and went with the CLI's destination choice. qpdf
  compares paths with `QUtil::same_file` in `checkConfiguration`
  (`libqpdf/QPDFJob.cc:626-630`) and then opens with `safe_fopen`, which is what
  the job now does.
* flpdf-cli's own `reject_same_json_output` preflight went with it, since
  `check_configuration` performs qpdf's check at qpdf's position in the
  sequence. Two diagnostics gained qpdf's wording: an aliased destination now
  reports `input file and output file are the same; use --replace-input to
  intentionally overwrite the input file` instead of `... choose a different
  --json-output path`, and an unusable destination path now reaches
  `safe_fopen`'s `open <path>: <strerror>` (with the create-stage warnings that
  precede it) instead of the flpdf-only `unable to inspect --json-output file
  <path>: ...` raised before the input was opened.

One pre-existing divergence found while gating this and deliberately left open:
`--json --split-pages` writes JSON in flpdf and splits in qpdf. `writeQPDF`
dispatches `split_pages` ahead of `writeOutfile`, so qpdf emits `out.json-1`
and no JSON at all (`libqpdf/QPDFJob.cc:484-489`). flpdf's `write_qpdf` has the
same dispatch, but the CLI's JSON route does not configure `split_pages` on the
job, so the JSON arm still wins. That is a route configuration gap, not a
dispatch one.

### QPDFObjectHandle getValueAs family

| qpdf | 行 | flpdf | 状態 |
|---|---|---|---|
| `QPDFObjectHandle::getValueAsBool` / `getValueAsInt` / `getValueAsUInt` / `getValueAsReal` / `getValueAsNumber` / `getValueAsName` / `getValueAsString` / `getValueAsUTF8` / `getValueAsOperator` / `getValueAsInlineImage` | `include/qpdf/QPDFObjectHandle.hh:601-606,640-711`; `libqpdf/QPDFObjectHandle.cc:484-748`; `qpdf/test_driver.cc:2973-3062` | `object_handle.rs::ObjectHandle::try_get_value_as_bool` / `try_get_value_as_int` / `try_get_value_as_int_as_int` / `try_get_value_as_uint` / `try_get_value_as_uint_as_uint` / `try_get_value_as_real` / `try_get_value_as_number` / `try_get_value_as_name` / `try_get_value_as_string` / `try_get_value_as_utf8` / `try_get_value_as_operator` / `try_get_value_as_inline_image` | ✅ the receiver resolves before a silent type check; wrong-type values return `None` without `typeWarning`; names retain qpdf's slash-prefixed canonical spelling; the UTF-8 accessor follows qpdf's `asString()` boundary through `try_as_string`; integer and unsigned-integer saturation retains qpdf's `warnIfPossible` messages. This is distinct from the warning-producing `try_get_*_value` getXValue family above. `try_get_value_as_string` remains on qpdf's separate `getValueAsString`/`isString` boundary. |

The pinned qpdf `getUTF8Value` and `getValueAsUTF8` implementations both call
the private `asString()` helper (`libqpdf/QPDFObjectHandle.cc:680-702`), while
`getStringValue` and `getValueAsString` intentionally use their own
`isString()` paths (`:659-678`). flpdf preserves that split: the warning-
producing `try_get_utf8_value` and silent `try_get_value_as_utf8` call the
resolving `ObjectHandle::try_as_string`, and the raw string accessors remain
on their direct string-value boundaries. The selected encryption and page-label
consumers likewise use `try_as_string` instead of repeating
`try_dereference` plus `as_string`; `pdf_string::unparse_binary` is a separate
byte-serialization responsibility and is not part of this correspondence.
