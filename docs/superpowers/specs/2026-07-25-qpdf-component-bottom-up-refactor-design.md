# qpdf component bottom-up refactor design

**Issue:** flpdf-qxba
**Date:** 2026-07-25
**Oracle:** qpdf 11.9.0 (`v11.9.0`) — `libqpdf/`
**対応表:** [`docs/qpdf-correspondence.md`](../../qpdf-correspondence.md)

## Problem

pre-v1.0 の唯一のゴールは qpdf 出力の byte-identical 再現（`CLAUDE.md`）。しかし
個別の乖離を埋めるたびに専用の pass を足してきた結果、flpdf の責務分割が qpdf と
対応しなくなり、次の乖離をどこで直すべきかの判断が困難になっている。

対応表の実測がこれを裏付けている。

- `QPDFWriter.cc`(3,044 行) が flpdf 側では **10 ファイル 9,576 行**に分散し、
  xref 出力だけで 3 箇所ある。byte-parity の修正が片方の経路にしか入らない
- `QPDF_optimization.cc`(381 行) は独立モジュールが無く `linearization/plan.rs`(3,032 行)
  に埋没している
- qpdf の 2 つの汎用機構（`QPDFJob.cc:2606` の null 置換 + `QPDFWriter.cc:1491` の
  null 可視性）の副作用を、flpdf は参照種別ごとに **6 モジュール 2,496 行**で
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
| D1 | **全域移植** — 対応する qpdf ファイルの公開 API が全て移植されている。部分移植の場合は未移植項目を doc に明示列挙する | qpdf ヘッダの公開メンバと突き合わせ |
| D2 | **単一実装** — 同じ責務の実装がコードベース内に他に存在しない。呼び出し元が全てこのモジュールを通る | 旧実装の削除行数 > 0、grep で重複なし |
| D3 | **アドホック分岐ゼロ** — qpdf に根拠のない条件分岐が無い | doc に qpdf 行番号の根拠 |
| D4 | **対応行** — doc 先頭に `//! Mirrors qpdf 11.9.0 libqpdf/X.cc` | 機械チェック可能 |
| D5 | **ゲート通過** — byte baseline 不変 / patch coverage 100% / fmt / clippy | CI |

**D2 が核心。** PR #549 は `tokenizer.rs`(+626) 新設と同時に `parser.rs` を −365 行
している。これが「部品が完成した」ことの証拠になる。新モジュールを足しただけで旧実装が
残るなら、それは smear を 1 つ増やしただけで完成ではない。

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

D2 の対象: `writer.rs` の `parse_pdf_version`(452) / `static_version_string`(638) と、
`(u8, u8)` 生タプル 6 箇所（`writer.rs` / `overlay.rs` / `flpdf-cli/src/main.rs`）。

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

D2 の対象: `[f64; 6]` 生配列が散在し `IDENTITY` 定数と行列適用が重複している。
`page_form_xobject.rs`（`transformation_matrix`:503, `matrix_objects`:533）/
`page_annotation_flatten.rs`（`apply_matrix`:306, `read_xobj_bbox_and_matrix`:454）/
`overlay.rs`。

#### T0-3 `json/` ← `JSON.cc`(1,401) + `JSONHandler.cc`(189) — `flpdf-qxba.6`

qpdf 側の依存は Pipeline sink のみで PDF オブジェクトモデルに非依存。規模は大きいが
依存が無く機械的に全域移植できるため、**「完成」と言い切れる部品の代表格**。

現状 `json.rs`(159) は emitter のみで parser も schema validator も無い。

D1 の対象: JSON 値モデル / parse / schema check / Base64 / writer。

解錠するもの: `--json-input` 経路、`flpdf-iquk`、`flpdf-q28i`。

### Tier 1: バイト列 → トークン

#### T1-1 `tokenizer.rs` 全モード ← `QPDFTokenizer.cc`(965) — `flpdf-n9t0.1`（既存）

PR #549 が normal mode を確立済み。**#549 マージ後に着手**。

D2 の対象: `content_stream.rs`(484) の二重実装。

### Tier 2: オブジェクトモデルに依存するが構造として自己完結

#### T2-1 `content_normalizer.rs` ← `ContentNormalizer.cc`(75) + `Pl_QPDFTokenizer.cc`(66) — `flpdf-qxba.7`

依存: T1-1（`flpdf-n9t0.1`）。qpdf 側で `ContentNormalizer` は
`QPDFObjectHandle::TokenFilter` を継承するため tokenizer が完成していないと閉じられない。

解錠するもの: `--normalize-content`（`flpdf-w5ny`）。

#### T2-2 `nntree.rs` ← `NNTree.cc`(954) — `flpdf-qxba.8`

依存: `QPDF` 全体（reader）。現状 `name_number_tree.rs`(364) +
`name_tree_dests.rs`(286) に分かれ、挿入 / 分割ロジックが未移植。

D1 の対象: iterator / insert / split / repair。

### 後回し

| 部品 | 理由 |
|---|---|
| `pipeline.rs` ← `Pipeline.cc` + `Pl_*` | 依存は少ないが払いが小さい。`/ID` は既に byte-parity 済みで、原因はアルゴリズム（2 段階 MD5）であって抽象の欠落ではない。Tier 0 が片付いてから再評価 |
| `qtc.rs` ← `QTC.cc`(50) | qtest の coverage 突き合わせが必要になった時点で |
| `Pl_DCT` | 現状どの経路からも要求されていない |
| CLI / `QPDFJob` 系 | byte-parity への寄与ゼロ |

## Phase 2 — 対応表に従った上位レイヤーの再配置

Phase 1 完了後に着手する。着手時点で対応表を**再測**する（部品が吸収した分だけ smear は
減っているため、現時点の順序付けはその時点で無効になる）。

主な対象:

- `QPDF_optimization.cc`(381) — `linearization/plan.rs` に埋没。定義は 2416-2950 行に
  連続、呼び出しは 890-904 行に集中しており抽出境界は clean。移動と境界整理は別コミットに
  分ける。**objstm 経路の解錠は無い**（qpdf でも `optimize()` の呼び出し元は
  `QPDF_linearization.cc:495` と `QPDFWriter.cc:2553`＝`writeLinearized()` 内のみ。
  `flpdf-g6hb` が要る `getCompressibleObjGens` は `QPDF.cc:2393` の別物）
- `QPDFWriter.cc` 系（flpdf 側 10 ファイル 9,576 行）
- `QPDFObjectHandle` アクセサ（`flpdf-mfir`）
- `ResourceFinder` / `QPDFLogger`

### null 可視性と対応表 §A の扱い

`flpdf-9hc.42`（QDF / 暗号への null 可視性拡張）は **Phase 2 に置く**。writer 中核を
横断する変更であり、writer の責務分割が済む前に入れると場当たり的な追加になるため。
qtest +9 の実測値は順序を前倒しする根拠にしない。

対応表 §A の 6 モジュール 2,496 行の畳み込みも、writer の null 可視性が構造として
固まったあとの別設計とする。**挙動は検証済みで byte-identical を保っているため
壊してはならない。** 現時点で畳み込みの issue は作らない。

QDF / 暗号 / incremental の byte gate 新設は Phase 2 着手時の前提として持ち越す
（この 3 経路には現在 gated byte gate が無い）。

## 推奨順

```
flpdf-qxba.1, .2      （小・独立、いつでも）
   │
   ├─> T0-1 pdf_version.rs  (.4)  DoD と D4 足場の確立
   ├─> T0-2 matrix.rs       (.5)  3 ファイルの重複を吸収
   └─> T0-3 json/           (.6)  依存ゼロ、並行可

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

救済可能な qtest FAIL 20 件のうち本バックログが直接担当するのは
`content_normalizer`（good7/good15 = 4 件）程度であり、残りは `flpdf-oq7g` /
`flpdf-w5ny` 等で別途追跡する。**このバックログを「完了すれば qtest が埋まる」とは
読まないこと。**
