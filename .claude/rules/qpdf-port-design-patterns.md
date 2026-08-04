# flpdf qpdf 移植 設計パターン（設計段階の予防ルール）

既存の [`pdf-rust-review-patterns.md`](pdf-rust-review-patterns.md) と
[`pdf-rust-doc-review-patterns.md`](pdf-rust-doc-review-patterns.md) が
「コードを書く／レビューする時」の規則であるのに対し、本文書は **その手前、
設計・ブレインストーミング段階** の規則。設計を誤ったまま実装に入ると、
コードレビューの規則はもう効かない。

根拠: qpdf の `StreamDataProvider` を移植する設計検討で、同種の誤りを
3 回連続で犯した。最終的に真因が「前提となる既存表現が qpdf と等価でない
こと」だと判明し、その前提を直す作業を先に切り出して本体の着手を取りやめた。
その診断過程を予防ルール化したもの。

---

## 貫く原則

**設計の出発点は常に qpdf のソース。flpdf の現状は「変更対象」であって
「制約」ではない。**

flpdf の既存構造から出発して「新しいものをこの構造にどう嵌めるか」を
考えた瞬間に、設計は壊れる。qpdf の構造から出発して
「flpdf は何になるべきか」を決める。

---

## 1. qpdf から出発する。flpdf の現状に嵌める発想をしない

### ルール
- 新しい qpdf 機能を移植するときは、まず qpdf 側の
  **データ構造・フィールド・呼び出し順序** を読み切る。flpdf 側の
  既存コードを開くのはその後。
- 「flpdf の既存 X に足すには」ではなく「qpdf の X に対応する flpdf の
  表現は何か。等価か。等価でないなら何を直すか」を問う。
- 対応関係を書き出すときは、qpdf のフィールド 1 つに対して flpdf の
  どれが対応するかを **1:1 で明示する**。1 つの flpdf フィールドが
  qpdf の複数の概念を兼ねていたら、それが 2 の対象。

### 該当例
`StreamDataProvider` の移植を、`ObjectValue::Stream` の `data: Vec<u8>` に
provider をどう足すかから考え始めた。qpdf の `QPDF_Stream` は
`stream_data`（`replaceStreamData(buffer)` でしか入らない）/
`stream_provider` / original（`parsed_offset`+`length` でソースから読む、
`libqpdf/QPDF_Stream.cc:606-620`）の 3 source を持ち、flpdf の `data` は
そのうち 2 つを 1 フィールドに畳んでいた。

## 2. 特殊ケースが要ると感じたら手を止める（中断規則）

**最重要**。設計が特殊ケースを要求し始めたら、それは設計の難所ではなく
**前提となる既存構造の逸脱が未修正だというシグナル**。

### 中断シグナル
- sentinel 値（空 `Vec` で「無い」を表す、`0` を「未知」に使う等）
- 型の穴を塞ぐための panic / エラー分岐の新設
- qpdf に対応物のない独自の N 分割・独自の中間表現
- 「この作業の範囲では原理的にテストできない」領域が生じる

### ルール
- 上記が要ると感じたら、その場で工夫して埋めない。**手を止めて、
  前提の逸脱を直す作業を先に切り出す**。工夫は逸脱の上に逸脱を積む。
- 逸脱を直した後に当該機能が「特殊ケース無しの純粋な追加」になるなら、
  その判断は正しい。ならないなら、まだ真因に届いていない。
- 特殊ケースを避けられないと結論する場合は、qpdf のどの挙動に対応するかを
  明示する。対応物が無いなら、それは逸脱であって設計ではない。

### 該当例
`StreamDataProvider` の移植先を探して連続して出た 3 案 —
(a) `data` を「パース済みバイト」と「置換済みバイト」に分割（qpdf に
対応物の無い独自 2 択構造）、(b) `materialize_value` に provider 分岐を
新設して panic、(c) 同じ箇所で空 `Vec` を返す（byte 捏造）。いずれも
真因（1 の該当例）を迂回する工夫だった。前提を直せば provider は
純粋な追加になる。

## 3. flpdf の既存前例は、qpdf 対応を確認してから根拠にする

flpdf の既存コードは「既に qpdf に合っているもの」と「未修正の逸脱」が
混在している。前例を無検証で引用すると逸脱を伝播させる。

### ルール
- 「flpdf では既に X がこうしている」を設計根拠にする前に、**その X 自体の
  qpdf 対応を確認する**。qpdf 側の対応物が何で、等価かを言えないなら
  根拠に使わない。
- 特に、公開 API と `pub(crate)` 内部機構を混同しない。qpdf 側で公開
  クラスなら、flpdf 側の内部専用ヘルパーの流儀は前例にならない。
- [`docs/qpdf-correspondence.md`](../../docs/qpdf-correspondence.md) で
  該当行を引く。**⚪ は「既知の逸脱」ではなく「逸脱候補（要承認）」**なので、
  ⚪ が付いていることを承認済みの根拠にしない。逆に、確定済みの逸脱は
  ✅ 等の行の本文にインラインで書かれていることがあるため、分類記号だけを
  見て「逸脱なし」と判断しない。行の注記と承認状態の両方を読む。

### 該当例
`TokenFilter` が `PipelineResult` を返しているから `StreamDataProvider` も、
と判断したが誤り。`TokenFilter` は `pub(crate)` の flpdf 内部専用、qpdf の
`StreamDataProvider` は公開クラス（`include/qpdf/QPDFObjectHandle.hh:72-127`）。
qpdf の `throw` の移送先は `crate::Error::Internal`/`System`
（`crate::Error` 自身の doc が「qpdf の `std::logic_error`/`std::runtime_error`
の公開分類に対応」と明記）。

## 4. 記録された依存順序を疑う

issue に書かれた依存関係・受け入れ基準は、前任セッションの理解であって
qpdf の事実ではない。

### ルール
- 着手前に、**前提の逸脱がどちらの作業の担当範囲にあるか** で依存方向を
  検証する。既存の登録が逆向きなら直す。
- 受け入れ基準が qpdf のソースと食い違う場合、qpdf を正とする
  （CLAUDE.md 最優先方針）。基準の側を直す。
- 依存を直したら、親 epic・被依存側の記述も併せて更新する。

### 該当例
`StreamDataProvider` 移植の受け入れ基準は「`pipeStreamData` 移植の側を
こちらに依存させる」と指示していたが、provider を純粋な追加にするための
lazy original 読み出し経路（qpdf の `QPDF::Pipe::pipeStreamData`）は
`pipeStreamData` 移植の側にあり、実際の依存は逆方向だった。

## 5. 逐語訳の粒度を守る（省略も追加もしない）

C++ から Rust へ写すとき、qpdf 側にある要素を落とすのも、無い要素を足すのも
どちらも逸脱。

### ルール
- **基底クラスの既定実装は契約の一部**。qpdf の virtual メソッドが
  既定実装（委譲・throw）を持つなら、Rust の trait デフォルトメソッドへ
  写す。「契約だけにする」と称して落とさない。
- **`throw` は panic ではない**。qpdf の例外は呼び出し側が通常の制御フローとして
  catch するもので、このクレートでは `crate::Error::Internal`
  （`std::logic_error`）/ `Error::System`（`std::runtime_error`）に対応する。
  panic は（このワークスペースの profile では unwind するとはいえ）Rust の
  一般的なエラー処理手段ではなく、`catch_unwind` は本リポジトリでも panic の
  発生を検証するテストでしか使っていない。両者を等価に扱わない。
- 分岐ロジックの置き場所も写す。qpdf が呼び出し側に持つ分岐
  （例: `supportsRetry()` を見てどちらを呼ぶか）を、被呼び出し側の
  既定実装に移さない。
- 同一シグネチャファミリーの単純な委譲オーバーロード（`QPDFObjGen` 版と
  `int, int` 版など）の 1 本化は、出力バイトに影響しない「入れ物」の統合として
  CLAUDE.md の逸脱分類 (B) に該当する。記録は **2 箇所必須** — 当該モジュールの
  doc に逸脱理由を 1 行、かつ
  [`docs/qpdf-correspondence.md`](../../docs/qpdf-correspondence.md) の
  ⚪ 行に該当箇所を記載する（CLAUDE.md 分類 (B) 条件 3）。片方だけでは
  対応表が stale になる。

### 該当例
`StreamDataProvider` trait の形を、既定実装を全廃 → 復活 → panic を
`Error::Internal` へ、と 3 往復した。qpdf の基底クラス実装
（`libqpdf/QPDFObjectHandle.cc:48-90`）を最初に読み切っていれば 1 度で済んだ。

---

## 補足

- **本文書を読むタイミング**: 作業に着手する時、設計やブレインストーミングに
  入る時、既存設計を変更する時。コードを開く前。
- **既存 2 本との関係**: 本文書（設計）→ `pdf-rust-review-patterns.md`
  （実装・コードレビュー）→ `pdf-rust-doc-review-patterns.md`（公開 doc）の順で
  適用範囲が下りていく。設計を誤ると下 2 本は誤った設計を綺麗に磨くだけになる。
- 1〜5 は「出発点」「中断シグナル」「前例の検証」「依存順序」「逐語訳の粒度」という、
  qpdf 移植の設計段階に固有の落とし穴。
