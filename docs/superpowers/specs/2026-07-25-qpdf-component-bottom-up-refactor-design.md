# qpdf component bottom-up refactor design

**Issue:** flpdf-qxba
**Date:** 2026-07-25
**Oracle:** qpdf 11.9.0 (`v11.9.0`) — `libqpdf/`
**Oracle の取得:** `scripts/fetch-qpdf-source.sh`（commit `3b97c9bd` で pin / パスは
`--print-path`）。本書および各モジュール doc の qpdf 行番号引用はすべてこのツリーに
対するもの。11.9.0 は開発環境の Ubuntu パッケージ（`/usr/bin/qpdf`）に合わせた版で、
挙動オラクルのバイナリと一致している必要がある。
**対応表:** [`docs/qpdf-correspondence.md`](../../qpdf-correspondence.md)

## Problem

pre-v1.0 の唯一のゴールは qpdf 出力の byte-identical 再現（`CLAUDE.md`）。しかし
個別の乖離を埋めるたびに専用の pass を足してきた結果、flpdf の責務分割が qpdf と
対応しなくなり、次の乖離をどこで直すべきかの判断が困難になっている。

対応表の実測がこれを裏付けている。

- `QPDFWriter.cc`(3,044 行) が flpdf 側では **11 ファイル 13,177 行**に分散し、
  xref 出力だけで 3 箇所ある。byte-parity の修正が片方の経路にしか入らない
- `QPDF_optimization.cc`(381 行) は独立モジュールが無く `linearization/plan.rs`(3,032 行)
  に埋没している
- qpdf の 2 つの汎用機構（`QPDFJob.cc:2606` の null 置換 + `QPDFWriter.cc:1491` の
  null 可視性）の副作用を、flpdf は参照種別ごとに **4 モジュール 1,748 行**で
  特殊化して実装している

最後の項目が問題の性質をよく表している。qpdf 側は

```cpp
// (1) QPDFJob.cc:2597-2608 — 選択されなかったページを null に置換
pdf.replaceObject(page.getObjectHandle().getObjGen(), QPDFObjectHandle::newNull());

// (2) QPDFWriter.cc:1491 — 値が null の dict キーは書かない
for (auto& item: object.getDictAsMap()) {
    if (!item.second.isNull()) { ... }
}
// 配列 (:1128) には同じフィルタが無い → null 要素はそのまま残る
```

の 2 機構だけであり、「dict キーは drop / 配列要素は null 保持」という非対称性は
その帰結にすぎない。flpdf はこの帰結を観測し、`/Pg`・bead `/P`・OBJR `/P`・
outline dest ごとに別モジュールを作ってきた。参照の種類が増えるたびにモジュールが
1 つ増える構造になっている。

さらに、**モジュール単位で「完成」と呼べるものがほとんど無い**。多くが部分移植で
止まっており、どこまで qpdf に追随しているかがモジュール単位で判定できない。

## Goal

qpdf の 1 コンポーネント = 1 独立モジュールとして、**依存の少ないものから確実に
完成させて積み上げる**。責務分割は結果として達成する。

これは新しい方針ではない。このリポジトリで既に 3 回成功しているパターンの継続である。

| 先例 | 新設した部品 | 吸収した smear | 結果 |
|---|---|---|---|
| reader | `reader/file_object.rs` | reader の直読み経路 | consumer 単位で切替、完了 |
| plain writer（`flpdf-2tbp`） | `writer/{serialize,plain/*}.rs` | `writer.rs` の 3 経路分岐 | 6 層を順に積み legacy 除去 |
| tokenizer（`flpdf-h8kt` / PR #549） | `tokenizer.rs` | `parser.rs` の字句走査 −365 行 | good13 修正 + qtest 37-39 pass |

## 2 フェーズ

**Phase 1（bottom-up）**: 依存の少ない部品から確実に完成させて積み上げる。
**Phase 2（top-down）**: 部品が揃ったあと、対応表に従って上位レイヤーを qpdf に寄せる。

Phase 2 を後に置くのが要点。部品が smear を吸収したあとの上位レイヤーは薄くなっており、
対応関係も Phase 1 で確立済みなので、再配置が「名前を変えるだけ」の作業にならない。

## 優先順位の基準

**「依存の少なさ × 完成可能性」で決める。qtest pass 数では決めない。**

1. **依存が少ない** — 依存先が未完成なら、その部品も完成と呼べない
2. **完成を宣言できる** — qpdf 対応ファイルを全域移植でき、部分移植で終わらない

qtest の数字を追って個別対応を足すのは、行き詰まりの原因になった「場当たり的に pass を
増やす」やり方そのものである。QDF の null 可視性対応（`flpdf-9hc.42`）は実測で
qtest +9 の効果があったが、writer 中核を横断する変更を責務分割の前に入れることになる
ため、方針不一致として一度キャンセルされている。**この判断を設計に反映する。**

qtest pass 数は各部品の完了時に before/after を**報告する**指標であって、順序を
決める指標ではない。

## Definition of Done

モジュールが「完成」と言えるのは、次を全て満たすとき。

| # | 条件 | 検証方法 |
|---|---|---|
| D1 | **全域移植** — 対応する qpdf ファイルの公開 API が**全て**移植されている | qpdf ヘッダの公開メンバと突き合わせ |
| D2 | **単一実装** — 同じ責務の実装がコードベース内に他に存在しない。呼び出し元が全てこのモジュールを通る | 旧実装の削除行数 > 0、grep で重複なし |
| D3 | **アドホック分岐ゼロ** — qpdf に根拠のない条件分岐が無い | doc に qpdf 行番号の根拠 |
| D4 | **対応行** — doc 先頭に `//! Mirrors qpdf 11.9.0 libqpdf/X.cc` | 機械チェック可能 |
| D5 | **ゲート通過** — byte baseline 不変 / patch coverage 100% / fmt / clippy | CI |

**D2 が核心。** PR #549 は `tokenizer.rs`(+626) 新設と同時に `parser.rs` を −365 行
している。これが「部品が完成した」ことの証拠になる。新モジュールを足しただけで旧実装が
残るなら、それは smear を 1 つ増やしただけで完成ではない。

**D1 に例外を設けない。** 未移植項目を doc に列挙することは D1 を満たす代替手段では
なく、**D1 が未達である証拠**として扱う。部分移植のまま「完成」を宣言できてしまうと、
この設計が解消しようとしている状態そのものを再生産する。移植しきれない部品は
「未完成」のまま残し、issue を分割して残りを追跡する。

### D2 スコープは着手時に全数棚卸しする

本書の各部品に書いた D2 の対象は、**T0-1 を除き暫定**である。PR #550 の Codex
レビューは 3 巡にわたり、毎回「既存 production 実装とその呼び出し元の見落とし」を
指摘した（`ResourceFinder` / `Parser` / `normalize_content_stream` / `base64_encode` /
`overlay.rs` の行列 / `json.rs` の値モデル / `nntree` の builder）。
ファイル名からの推測で D2 スコープを書くと必ず漏れる。

各部品の着手時に、次を機械的に出してから実装計画を確定すること。

1. 対象責務に関わる `pub` / `pub(crate)` 定義の全列挙（`lib.rs` の re-export を含む）
2. その全呼び出し元（他クレート・CLI・テストを含む）

T0-1 はこの手順を適用済み（下記）。他の部品は着手時に同じ手順を踏む。

## 命名規則

**`QPDF` 接頭辞は C++ の名前空間なので落とす。それ以外はそのまま snake_case にする。**

| qpdf | flpdf | 備考 |
|---|---|---|
| `PDFVersion.cc` | `pdf_version.rs` | `QPDFVersion` ではないので `PDF` は型名の一部 |
| `QPDFMatrix.cc` | `matrix.rs` | |
| `QPDFTokenizer.cc` | `tokenizer.rs` | PR #549 で確立済み |
| `NNTree.cc` | `nntree.rs` | |
| `ContentNormalizer.cc` | `content_normalizer.rs` | |
| `JSON.cc` / `JSONHandler.cc` | `json/` | 複数ファイルはモジュールディレクトリ |

`qpdf_tokenizer.rs` のような接頭辞は付けない。対応の明示は D4 の doc 行が担う。
crate 内で意味が曖昧になる名前は避ける（`version.rs` は crate version とも読めるため
`pdf_version.rs` とする）。

## P0 — 前提（コード移動なし）

| Issue | 作業 | 根拠 |
|---|---|---|
| `flpdf-qxba.1` | qpdf 11.9.0 ソースの pin | 設計書・doc の行番号引用の根拠。現在 `/tmp` に 8 コピー散在しており揮発する |
| `flpdf-qxba.2` | `cmp_null_visibility_tests` を `ci.yml` に追加 | whole-file gated byte テスト 11 件中唯一の CI 漏れ。既存の穴なので独立に塞ぐ |
| `flpdf-qxba.3` | D4 対応行の書式確定 + 検査スクリプト | T0-1 が最初の適用例になる |

## Phase 1 — 部品バックログ

### Tier 0: PDF オブジェクトモデルに依存しない

#### T0-1 `pdf_version.rs` ← `PDFVersion.cc`(68) — `flpdf-qxba.4`

依存ゼロ（qpdf 側の include も `QUtil` のみ）。公開メンバが 7 つのみで全域移植が現実的。

D1 の対象: コンストラクタ（major/minor/extension）、`operator<`、`operator==`、
`updateIfGreater`、`getVersion`、`getMajor`/`getMinor`/`getExtensionLevel`。

#### T0-1 の D2 棚卸し（全数）

過去 3 巡の Codex レビューは毎回「既存 production 実装とその呼び出し元の見落とし」を
指摘した。同じ誤りを繰り返さないため、T0-1 については機械的に全数を出す。

**既存の version 関連定義**

| 定義 | 位置 | 可視性 |
|---|---|---|
| `parse_pdf_version` | `writer.rs:452` | **`pub`**（`lib.rs:249` から re-export） |
| `effective_pdf_version` | `writer.rs:511` | **`pub`**（`lib.rs:249` から re-export） |
| `effective_pdf_version_and_ext` | `writer.rs:666` | `pub` |
| `force_version_below_1_5` | `writer.rs:468` | `pub(crate)` |
| `encryption_version_floor` | `writer.rs:615` | private |
| `static_version_string` | `writer.rs:638` | private |

`lib.rs:259` の `pub fn version()` は crate バージョンであり無関係（`pdf_version.rs`
という名前にした理由でもある）。

**`parse_pdf_version` の呼び出し元（production のみ）**

| ファイル | 行 |
|---|---|
| `writer.rs`（内部） | 472, 521, 527, 533, 569, 573, 685, 686, 687, 700, 3343, 3363, 3370 |
| `writer/plain/plan.rs` | 129, 269 |
| `overlay.rs` | 2087, 2088 |
| `flpdf-cli/src/main.rs` | 29（import）, 2101, 2107, 3226, 3229, 3247, 4235, 4238, 4246 |

**`effective_pdf_version` / `_and_ext` の呼び出し元（production のみ）**

| ファイル | 行 |
|---|---|
| `writer.rs` | 674, 3166, 3236 |
| `writer/plain/plan.rs` | 116 |
| `linearization/writer.rs` | 75（import）, 2737 |

**公開 API への影響**: `parse_pdf_version` と `effective_pdf_version` は
`lib.rs` から re-export されている公開 API である。`(u8, u8)` を `PdfVersion` に
置き換えると公開シグネチャが変わる。pre-1.0 では後方互換を考慮しない方針
（`CLAUDE.md`）なので進めてよいが、**T0-1 は公開 API 変更を伴う**ことを認識しておく。
`flpdf-cli/tests/cli_linearize.rs`(514-585) が両関数の公開 API を直接テストしている。

**D2 の完了条件**: 上表のすべての呼び出し元が `PdfVersion` を通ること。
`writer.rs` 内部の 13 箇所だけを直しても、`overlay.rs` / `writer/plain/plan.rs` /
CLI の 8 箇所が生タプルのまま残れば D2 未達。

**スコープ境界** — qpdf は値型とポリシーを分けている。ポリシー側は writer に残す。

| 責務 | qpdf | T0-1 に含むか |
|---|---|---|
| 値型（比較・更新・文字列化） | `PDFVersion.cc` | 含む |
| 最小バージョン決定 | `QPDFWriter::setMinimumPDFVersion`(217-258) | 含まない |
| 暗号方式ごとの版数下限 | `QPDFWriter.cc:806-814` | 含まない |
| 非互換暗号の無効化 | `QPDFWriter::disableIncompatibleEncryption`(705) | 含まない |
| `/Extensions /ADBE` の出力 | `QPDFWriter.cc:1396-1422` | 含まない |

したがって `effective_pdf_version` / `effective_pdf_version_and_ext` /
`encryption_version_floor` / `inject_adbe_extension` / `strip_adbe_extension` は
writer に残し、内部表現のみ `PdfVersion` に置き換える。

規模は最小だが、**DoD と D4 対応行の書式を最小リスクで一度通しで検証する足場**として
先頭に置く。

#### T0-2 `matrix.rs` ← `QPDFMatrix.cc`(140) — `flpdf-qxba.5`

依存は `Matrix` / `Rectangle` 値型のみ。

D1 の対象: `concat` / `scale` / `translate` / `rotatex90` / `transform` /
`transformRectangle` / `getAsMatrix` / `unparse`。

D2 の対象: `[f64; 6]` 生配列が散在し、`IDENTITY` 定数・行列積・点/矩形変換が
複数箇所で重複実装されている。

| モジュール | 実装（全数） |
|---|---|
| `overlay.rs` | `IDENTITY_MATRIX`(82), `qpdf_concat`(87), `qpdf_scale`(101), `qpdf_translate`(106), `matrix_unparse`(123), `matrix_or_identity`(996), `transform_bbox`(1013) |
| `overlay_annotations.rs` | `matrix_to_object`(1256), `qpdf_real`(1272), `concat_matrices`(1284), `IDENTITY`(1297), `transform_rect_by_cm`(1338), `apply_matrix_to_point`(1365) — doc に `QPDFMatrix` 相当と明記されている |
| `job/rotate.rs` | `type Mat`(306), `apply_matrix`(309), `mat_mul`(314), `translate`(326), `rotate_origin`(333), `rotation_matrix`(345), `transform_box`(355), `wrap_content_with_matrix`(463) |
| `page_form_xobject.rs` | `get_matrix_for_transformations`(513), `matrix_objects`(533) |
| `page_annotation_flatten.rs` | `apply_matrix`(306), `read_xobj_bbox_and_matrix`(454) |

同じプリミティブ（恒等行列・行列積・点変換・矩形変換・シリアライズ）が
**5 モジュールに 4〜7 重に実装されている**。関数単位で漏れなく移行しないと、
5 ファイルすべてに触れても D2 未達になる。

`overlay.rs` / `overlay_annotations.rs` / `job/rotate.rs` を落とすと重複実装が残り、D2 を満たさない。
**5 モジュールすべてとその呼び出し元**を移行対象に含めること。

#### T0-3 `json/` ← `JSON.cc`(1,401) + `JSONHandler.cc`(189) — `flpdf-qxba.6`

PDF オブジェクトモデルには非依存。規模は大きいが機械的に全域移植できる。

現状 `json.rs`(159) は emitter のみで parser も schema validator も無い。

D1 の対象: JSON 値モデル / parse / schema check / Base64 / writer。

D2 の対象:

| 既存実装 | 位置 | 備考 |
|---|---|---|
| `JsonValue` 値モデル + `write` シリアライザ | `json.rs`(16-159) | **production の JSON 出力経路そのもの** |
| `base64_encode` | `json_inspect.rs`(542-563) | inline JSON stream data の出力（695 行）で使用 |
| `JsonValue` の利用 | `json_inspect.rs`(7: `use crate::json::JsonValue`) | |
| `flpdf::json::write` の呼び出し | `flpdf-cli/src/main.rs`(1939, 1943) | `--json` 出力の実体 |
| `flpdf::json::JsonValue` の利用 | `flpdf-cli/src/main.rs`(2005-2006) | |

`json.rs` 本体と CLI 呼び出し元を移行対象から外すと、移植した writer が CLI 出力を
支配せず旧経路が並存する。**`json.rs` / `json_inspect.rs` / CLI 呼び出し元の 3 つを
すべて移行スコープに含めること。**

**Pipeline 依存の扱い（方針改訂により解決）**: `JSON.cc` は依存ゼロではなく、
`Pl_Base64` / `Pl_Concatenate` / `Pl_String` の 3 つの Pipeline sink を使う。

`CLAUDE.md` の逸脱条項が 2 分類に改訂され、「(B) 出力バイトを変えない内部構造の
代替」の枠ができた。この 3 sink はすべて (B) に該当するため、`base64` crate と
`Write` / `Vec<u8>` で代替してよい。`pipeline.rs` の先行は不要で、T0-3 は
Tier 0 のまま進められる。

ただし (B) は無条件ではない。着手時に次を満たすこと。

1. **出力バイトに影響しないことを検証する** — base64 は `JSON.cc:184-191` の
   `writeBlob` が改行挿入なし・標準アルファベットのみであることを確認済み。
   `json/` の出力を守る gated byte テストが無ければ先に追加する
2. **逐次出力の順序は qpdf のまま** — sink を差し替えても
   `writeDictionaryOpen` 等の呼び出し順を変えない
3. **記録する** — `json/` の doc に逸脱理由を 1 行、対応表の ⚪ 行に記載

解錠するもの: `--json-input` 経路、`flpdf-iquk`、`flpdf-q28i`。

### Tier 1: バイト列 → トークン

#### T1-1 `tokenizer.rs` 全モード ← `QPDFTokenizer.cc`(965) — `flpdf-n9t0.1`（既存）

PR #549 が normal mode を確立済み。**#549 マージ後に着手**。

D2 の対象: `content_stream.rs`(484) の二重実装。

### Tier 2: オブジェクトモデルに依存するが構造として自己完結

#### T2-1 `content_normalizer.rs` ← `ContentNormalizer.cc`(75) + `Pl_QPDFTokenizer.cc`(66) — `flpdf-qxba.7`

依存: T1-1（`flpdf-n9t0.1`）。qpdf 側で `ContentNormalizer` は
`QPDFObjectHandle::TokenFilter` を継承するため tokenizer が完成していないと閉じられない。

**これは「解錠」ではなく「置き換え」**: `--normalize-content` は既に実装済みである。
`content_stream.rs`(439-475) の `normalize_content_stream` が production にあり、
`flpdf-cli/src/main.rs`(1050, 2204, 2260) から配線されている。その doc には qpdf との
既知のバイト差が列挙されている。

D2 の対象: ライブラリ関数（`normalize_content_stream`）と CLI 呼び出し元の**両方**を
新モジュールへ移行する。新モジュールを足すだけでは CLI が乖離した旧実装を使い続け、
D2 を満たさない。

なお beads の `flpdf-w5ny` と epic `flpdf-n9t0` は `--normalize-content` を「未実装」と
記載しているが、これは stale。

#### T2-2 `nntree.rs` ← `NNTree.cc`(954) — `flpdf-qxba.8`

**依存の狭め方（重要）**: 当初「`QPDF` 全体（reader）に依存」と書いていたが、これは
成立しない。対応表は `QPDF.cc` を 🔀 に分類しており、優先順位の基準（「依存先が
未完成なら、その部品も完成と呼べない」）と Phase 2 の開始条件（Phase 1 完了後）を
同時に適用すると、**T2-2 は Phase 1 で完成できず reader は Phase 2 で統合できない**
という循環になる。

そこで T2-2 の依存を **reader の具体 API に狭める**: 名前ツリー / 数値ツリーの走査に
必要なのは `Pdf::resolve` 系のオブジェクト解決のみで、これは既に安定している。
`QPDF.cc` 全体の統合を待つ必要はない。着手時に必要な API を列挙し、それらが
安定していることを確認したうえで進める。

**「挿入 / 分割が未移植」は誤り。** 既に production 実装がある。

| 既存 API | 位置 | 備考 |
|---|---|---|
| `NameTree::as_map` / `NumberTree::as_map` | `nntree.rs` | canonical name/number-tree 読み取り |
| `NameTree` / `NumberTree` insertion | `nntree.rs` | `/Kids` リーフへの**分割**を含む |
| `insert_name_tree_dest` | `name_tree_dests.rs`(116) | 上記 builder 経由の再構築による**挿入** |
| `delete_name_tree_dest` | `name_tree_dests.rs`(149) | 同じ read/rebuild 経路による**削除**。`lib.rs`(193) から re-export |

いずれも `lib.rs` から re-export されている。

**`outline_document_helper.rs` は単なる呼び出し元ではない。** 私的な NNTree 実装を
production で持っている: `find_name_tree_value`(573: qpdf 流の targeted lookup と
二分探索)、`name_tree_begin_preflight`(657)、`enumerate_name_tree_entries`(937)、
`repair_name_tree`(1039)。上記 API だけを移行すると**第 2 の NNTree 実装が production に
残る**ため、この私的経路も列挙して統合すること。

D2 の対象: 上記 API と、その呼び出し元である `embedded_files.rs` /
`page_label_document_helper.rs` / `outline_document_helper.rs`（上記の私的実装を含む）/
`json_inspect.rs` の**すべて**。qpdf の iterator / insert / split を既存 production 経路の**隣に**追加すると
D2 を満たさない。

D1 の対象: iterator / insert / split / repair（qpdf 側との差分は着手時に精査する）。

### 後回し

| 部品 | 理由 |
|---|---|
| `pipeline.rs` ← `Pipeline.cc` + `Pl_*` | 依存は少ないが払いが小さい。`/ID` は既に byte-parity 済みで、原因はアルゴリズム（2 段階 MD5）であって抽象の欠落ではない。Tier 0 が片付いてから再評価 |
| `qtc.rs` ← `QTC.cc`(50) | qtest の coverage 突き合わせが必要になった時点で |
| `Pl_DCT` | **消費者は既にいる**。`json_inspect.rs:758` の `DecodeLevel::All` は doc で
「lossy filter（`DCTDecode` / `JPXDecode` 等）を含めて全ストリームをデコードする」と
約束しているが、`stream_payload_for_decode_level`(795-801) は `All` を
`Generalized` / `Specialized` と同一に扱い、DCT 未対応のため encoded JPEG バイトへ
フォールバックする（`json_inspect.rs:788` の doc と 8397 行のテストコメントに記載）。
**T0-3 / decode 系の作業と一緒にスケジュールするか、`DecodeLevel::All` の doc と
API を実態に合わせて狭めるか**を決めること。「消費者がいない」を理由に後回しにはできない |

**`QPDFJob.cc` 本体は後回しにしない。** 対応表が示すとおり `QPDFJob.cc` は overlay /
page 操作 / check / オーケストレーションに対応しており、`overlay.rs` のように出力
バイトを変える処理を含む。後回しにできるのは上表の**引数解釈基盤に限る**。
`QPDFJob.cc` 本体の再配置は Phase 2 の対象。
| CLI の引数解釈基盤（`QPDFArgParser` / `QPDFJob_argv` / `QPDFJob_config` / `QPDFJob_json` / `QPDFUsage`） | clap 置換は出力バイトに影響しない |

## Phase 2 — 対応表に従った上位レイヤーの再配置

Phase 1 完了後に着手する。着手時点で対応表を**再測**する（部品が吸収した分だけ smear は
減っているため、現時点の順序付けはその時点で無効になる）。

主な対象:

- `QPDF_optimization.cc`(381) — `linearization/plan.rs` に埋没。定義は 2416-2950 行に
  連続、呼び出しは 890-904 行に集中しており抽出境界は clean。移動と境界整理は別コミットに
  分ける。**objstm 経路の解錠は無い**（qpdf でも `optimize()` の呼び出し元は
  `QPDF_linearization.cc:495` と `QPDFWriter.cc:2553`＝`writeLinearized()` 内のみ。
  `flpdf-g6hb` が要る `getCompressibleObjGens` は `QPDF.cc:2393` の別物）
- `QPDFWriter.cc` 系（flpdf 側 11 ファイル 13,177 行）
- `QPDFObjectHandle` アクセサ（`flpdf-mfir`）
- `ResourceFinder` / `QPDFLogger`

### null 可視性と対応表 §A の扱い

`flpdf-9hc.42`（QDF / 暗号への null 可視性拡張）は **Phase 2 に置く**。writer 中核を
横断する変更であり、writer の責務分割が済む前に入れると場当たり的な追加になるため。
qtest +9 の実測値は順序を前倒しする根拠にしない。

対応表 §A の 4 モジュール 1,748 行の畳み込みも、writer の null 可視性が構造として
固まったあとの別設計とする。**挙動は検証済みで byte-identical を保っているため
壊してはならない。** 現時点で畳み込みの issue は作らない。

畳み込みの対象は `outline_dest_remap.rs` / `struct_tree_pg.rs` / `thread_bead_p.rs` /
`objr_obj_annot_p.rs` の 4 つに限る。`acroform_field_prune.rs` は qpdf 側に明示的な
対応パス（`QPDFJob.cc:2610-2632`）を持ち、`subset_prune.rs` は `/Resources` の stale
名前エントリ剪定と orphan mark-and-sweep という独立した責務なので、**畳み込むと
必要な処理を失う**。

byte gate の新設は Phase 2 着手時の前提として持ち越す。ただし QDF は**既に部分的な
カバレッジがある**（`writer_tests.rs:2170,2201` の qpdf golden 完全一致、
`qdf_tests.rs:1300` の `/ID` 行を除く完全一致、`overlay::byte_gate` の QDF 12 件（library）と `cli_byte_identical_overlay.rs` の 3 件（CLI）。
前 2 者と `overlay::byte_gate` は CI 列挙済み）。

**QDF × ObjStm / QDF × 暗号 / QDF × linearize は穴になりえない。** QDF はこれらと
排他だからである（`qdf_tests.rs:734` で QDF が `Generate` を上書き、
`flpdf-cli/src/main.rs:1466` が `--qdf --linearize` を拒否、`writer.rs:3135` が
`--encrypt` との併用を拒否）。gate を作っても意図した writer 経路を通らない。

新設が必要なのは**暗号化された入力からの QDF 出力（復号 → QDF）**、fixture の無い
QDF オプションの組み合わせ、および**暗号・incremental 単体**。詳細は対応表の
「QDF の既存カバレッジ」節。

## 推奨順

```
flpdf-qxba.1, .2      （小・独立、いつでも）
   │
   ├─> T0-1 pdf_version.rs  (.4)  DoD と D4 足場の確立
   └─> T0-2 matrix.rs       (.5)  5 モジュールの重複を吸収

T0-3 json/ (.6)  着手可（Tier 0）
   Pipeline sink は CLAUDE.md (B) の枠で代替可。ただし着手時に
   出力バイト非影響の検証と gated byte gate の有無確認を行うこと。
   規模は T0-1/T0-2 と二桁違う（json_inspect.rs の push 型転換を含む）。

PR #549 merge ──> T1-1 tokenizer 全モード (flpdf-n9t0.1)
                       │
                       └─> T2-1 content_normalizer.rs (.7)

T2-2 nntree.rs (.8)   （reader 依存、独立に進行可）

── Phase 1 完了後 ──
Phase 2: 対応表を再測 → optimization / writer 系 / null 可視性
```

## 進行中の作業との干渉

- **PR #549**（`fix/flpdf-h8kt-qpdf-tokenizer`）OPEN → T1-1 はマージ後
- **`flpdf-80b6`**（P1, in_progress）plain-writer pipeline のレビュー指摘対応 →
  `writer.rs` に触る T0-1 はこれの完了後か、衝突しない範囲で

## 報告項目（順序決定には使わない）

各部品の完了時に before/after を記録する。

- qtest `basic-parsing` pass 数（現状 28/69。`test_driver` 21 件は shim 無しで
  恒久的に対象外のため実質上限は 48）
- allowlist スコープの pass 数
- 旧実装の削除行数（D2 の証拠）

**行数は維持対象ではない。** 対応表・本書に記載した行数は調査時点のスナップショットで、
コードが変われば即座にずれる。追随コストに見合わないため、ずれ自体は不具合として
扱わない（[`docs/qpdf-correspondence.md`](../../qpdf-correspondence.md) の
「行数の位置づけ」参照）。維持するのは**分類と対応先モジュール**であり、これらは
work-list の実体なので誤ると着手判断を誤らせる。

救済可能な qtest FAIL 20 件のうち本バックログが直接担当するのは
`content_normalizer`（good7/good15 = 4 件）程度であり、残りは `flpdf-oq7g` /
`flpdf-w5ny` 等で別途追跡する。**このバックログを「完了すれば qtest が埋まる」とは
読まないこと。**
