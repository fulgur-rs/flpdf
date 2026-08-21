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
- 前例が「同じ形」でも「同じ責務」とは限らない。エラー型・戻り値の選択は、
  その失敗が **どのコンポーネントの責務に属するか** で決まる。可視性や
  シグネチャの見た目が似ているだけの前例を根拠にしない。
- [`docs/qpdf-correspondence.md`](../../docs/qpdf-correspondence.md) で
  該当行を引く。**⚪ は「既知の逸脱」ではなく「逸脱候補（要承認）」**なので、
  ⚪ が付いていることを承認済みの根拠にしない。逆に、確定済みの逸脱は
  ✅ 等の行の本文にインラインで書かれていることがあるため、分類記号だけを
  見て「逸脱なし」と判断しない。行の注記と承認状態の両方を読む。

### 該当例
`TokenFilter` が `PipelineResult` を返しているから `StreamDataProvider` も、
と判断したが誤り。可視性の違い（`TokenFilter` は `pub(crate)`）を根拠に
持ち出したのも誤りで、公開 API であること自体は型を決めない —
`pub trait Pipeline` の `write`/`finish` 自身が `PipelineResult` を返す。

決めるのは責務の所在。`PipelineError` は pipeline 機構自身の失敗を表す型で、
qpdf でも `Pipeline` サブクラスが投げる例外に対応する。`StreamDataProvider`
は pipeline の段ではなく pipeline へ **書き込む側** であり、qpdf でも
`Pipeline` の派生ではなく `QPDFObjectHandle` の入れ子クラス
（`include/qpdf/QPDFObjectHandle.hh:72-127`）で、基底クラスの契約違反も
object handle 層から `std::logic_error` を投げる
（`libqpdf/QPDFObjectHandle.cc:75`）。失敗は object 層の責務に属するので、
移送先は `crate::Error::Internal`/`System`（`crate::Error` 自身の doc が
「qpdf の `std::logic_error`/`std::runtime_error` の公開分類に対応」と明記）。

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

## 6. 複数の既存実装を共有 primitive へ統合する前に、qpdf 非対応の挙動を機械可読にマークする

qpdf の 1 コンポーネントに対応する flpdf 実装が複数箇所に分散している状態
（例: `page_object_helper.rs` 側と `pages.rs` 側がそれぞれ独立に継承属性を
辿っていた）を 1 つの共有 primitive へ統合するとき、各実装が個別に持って
いた「qpdf に対応物のない flpdf 固有の挙動」（CLAUDE.md の逸脱分類 (C)。
(B) の恒久的な構造代替とは別物 —
境界条件のズレ、legacy な redirect 追跡、診断 warning の発行条件など）が、
統合によって黙って消える
か別の実装へ黙って混入する。これをレビュー（Codex Review 等）が事後的に
発見するたびに「qpdf 対応のある regression だから直す」か「qpdf 対応物の
ない独自挙動だから維持しない」かを都度精査・議論することになり、往復が
かさむ。

### ルール
- 統合に着手する前に、既存の各実装を **境界条件・エラー経路・診断発行の
  単位で** 突き合わせる（1 の「qpdf フィールド 1 つに対して flpdf のどれが
  対応するかを 1:1 で明示する」の実装版）。
- 突き合わせて見つかった「qpdf に対応物のない flpdf 固有の挙動」は、
  実装を統合する前に、削除するにせよ残すにせよ機械可読にマークする:
  - 関数・型・フィールド単位で切り離せるなら
    `#[deprecated(note = "no qpdf counterpart; ...")]`。呼び出し元が
    limited（典型的には `#[cfg(test)]` のみ）なら CI の `-D warnings` を
    壊さないよう呼び出し元に `#[allow(deprecated)]` を局所的に付ける。
    259 箇所規模の pub API 全体を一括 `#[deprecated]` にするような大規模
    cutover には使わない（pre-v1.0 の flpdf は後方/前方互換を考慮しない方針
    のため、大規模一括 `#[deprecated]` は個別の呼び出し元 `#[allow]` で
    吸収しきれず CI の `-D warnings` を割る）。
  - 分岐・ブロック単位でしか切り離せないなら
    `// qpdf-deviation: <理由>` /
    `// qpdf-deviation-start: <理由>` … `// qpdf-deviation-end`
    （`// cov:ignore` と同じ文法。`scripts/check-qpdf-deviation-markers.py
    --check` が書式を検証する）。
- 統合後にレビューが指摘を出したら、まず「マーク済みか」を見る。マーク
  済みならメンテナ判断の再確認で足り、未マークなら真の regression の疑いを
  優先して精査する。
- qpdf に対応物があるのに統合で境界条件や診断が壊れた場合はマークしない
  — それは維持すべき regression であり、修正する。

### 該当例
PR #976（`pages.rs` の共有継承属性 walk への統合）で Codex Review が
3 件の regression 候補を指摘した。うち 2 件（100 番目の祖先を検査する前に
depth 上限に達する off-by-one、malformed parent での型 warning 消失）は
qpdf `QPDFPageObjectHelper::getAttribute`
（`libqpdf/QPDFPageObjectHelper.cc:236-247`）に対応がある真の regression
で修正した。残り 1 件
（`Pdf::set_object` による bare-reference の多段 redirect 追跡）は qpdf に
対応物のない test-only の legacy bridge（`docs/qpdf-correspondence.md:386`
に既存記載）で、メンテナ判断により維持しないと決めたが、これは実装の
統合が終わってからレビューで発見・議論した結果であり、統合前に境界差分を
洗い出して機械可読にマークしておけば、この 1 件は実装時点で切り離せて
いた。

## 7. メソッド名は qpdf の識別子から機械的に導出し、引用前に実在を検証する

flpdf のメソッド名は「qpdf のどの識別子を訳したものか」が常に一意に
辿れることを設計目標とする。命名を Rust の一般的な慣用に寄せるか
qpdf の識別子に寄せるかで両者が対立するとき、このクレートは
**qpdf 識別子への 1:1 対応（grep 可能性）を優先する**。

### ルール

- **qpdf の `getX()`/`isX()`/`hasX()` のうち、実際に計算・解決・qpdf の
  既定値/フォールバック処理を伴うものは prefix を落とさず
  `get_x`/`is_x`/`has_x` に写す**。`page_object_helper.rs`
  （`get_media_box`/`get_crop_box`/`get_resources` 等、いずれも
  `&mut self -> Result<...>` で継承属性 walk 等の実処理を伴う）と
  `annotation_helper.rs`（`get_subtype`/`get_rect`/`get_flags` 等）が
  確立した支配的な慣行で、Rust API Guidelines の C-GETTER（単純 getter は
  `get_` を省く）とは逆方向だが、それは意図的な選択。理由は「qpdf の
  `getTitle()` を読んだ人が flpdf 側で `grep get_title` すれば一致する」
  という 1:1 対応をコードの見た目より優先するため。
  **例外は、格納済みフィールドをそのまま返すだけの bare getter**
  （`&self -> T` で PDF I/O・解決・フォールバック計算を一切伴わない）—
  `job/lifecycle.rs` の `logger()`（`self.logger.clone()` 一行）・
  `message_prefix()`（`&self.message_prefix` 一行）はこちらに該当し、
  qpdf の `getLogger()`/`getMessagePrefix()` に対応するが prefix を
  落としても支配的慣行と矛盾しない（C-GETTER 通りで正しい）。
  **副作用/計算を伴う getter で prefix を落としているものは、支配的慣行との
  真の食い違いとして扱う** — `form_field_object_helper.rs` の
  `flags`/`value`（いずれも `&mut self -> Result<...>` で
  `resolve_inherited_integer`/`resolve_inherited_handle` を介した inheritable
  attribute 解決を伴い、qpdf の `getFlags`/`getValue` を直接写している）が
  該当し、`get_flags`/`get_value` へ寄せる余地がある。同じファイルの
  `partial_name()` も同列（PR #1015 Codex 再レビューで訂正——親を辿る
  解決を伴わない点だけを見て一度は bare getter 側へ分類したが、それは
  本項の基準ではない）: `string_key(self.field.clone(), b"T")` の内部で
  `self.resolved(field)` によるフィールドの解決と、値が無いときの
  空文字列へのフォールバック（`.unwrap_or_default()`）を行っており、
  「計算・解決・フォールバック処理を伴う」という本項の基準を満たす。
  qpdf の `getPartialName`（`QPDFFormFieldObjectHelper.cc:135-142`）も
  `/T` を解決し文字列でなければ空文字列にフォールバックしており、
  対応関係は正確。よって `partial_name` も `get_partial_name` へ寄せる
  余地がある対象として `flags`/`value` と同じ扱いにする。
  `filespec_helper.rs` の `size()`/`get_size()`・`creation_date()`/
  `get_creation_date()` のような prefix なし/ありの併存は、見かけは同じでも
  中身が違う——`size()` は `/Params /Size` の生の `Option<i64>`、
  `get_size()` はそれを負値/欠損を 0 にする qpdf の既定値ロジックまで含めて
  写した `usize`。qpdf 側で `getSize()` に対応するのは `get_size()` のみで、
  raw 版 `size()` 自身に写す元の qpdf 識別子は無い。この既存ペアを読むときは
  prefix の有無が「qpdf の `getX()` の既定値処理まで含めて再現しているか」の
  目印になる、という点だけを覚えておけば足り、**新規コードで qpdf の `getX()`
  に既定値/フォールバック処理があるからといって、この raw+`get_` の 2 層
  構造を新たに作ってよいという前例にはしない**——raw 側は本節後述の
  「qpdf 側に個別の識別子が無い」場合の独自命名の一種であり、正当化には
  そちらの基準（インライン抽出物の切り出し等、実際に独立した意味を持つ
  こと）を満たす必要がある。単に `get_x()` の下請けとして値を先取りする
  だけの raw アクセサを量産する理由にはしない。
  新規コードは、計算を伴う getter では `get_` を保持する側に合わせ、
  `form_field_object_helper.rs` のような drop 済み箇所は見つけ次第 issue化の
  対象とする（このルール自体を根拠に一括リネームを今すぐ強制はしない —
  3 の「前例は qpdf 対応を確認してから根拠にする」と同様、この不一致自体を
  新しい前例の論拠にしない）。
- **qpdf の `doX()`（`QPDFJob` のコマンドディスパッチ）は `do` を落として
  `x()` に写す**。`do` 自体に意味はなく、動作を表すのは残りの語だけ
  だから: `doCheck`→`check`、`doInspection`→`inspect`、
  `doListAttachments`→`list_attachments`、`doSplitPages`→`split_pages`。
  `handleX()` は `handle` 自体に「入力を受けて処理を実行する」という
  意味があるので落とさない: `handlePageSpecs`→`handle_page_specs`。
  **既知の未解消の例外**: `job/overlay.rs` の `apply_overlay_specs` は
  `QPDFJob::handleUnderOverlay`（`QPDFJob.cc:1937`、`--overlay`/
  `--underlay` の適用全体を担う private メソッドで、qpdf 側に個別識別子が
  無いケースにも該当しない）に対応するが、`handle_under_overlay` に
  なっていない。リネームの影響範囲（呼び出し箇所・テスト関数名 40 箇所超）
  が大きいため `flpdf-ei0h` として別途追跡する。同様に
  `job/rotate_spec.rs` の `RotateSpec::parse` は qpdf の
  `QPDFJob::parseRotationParameter`（`QPDFJob.cc:369`、private メソッド）
  に対応するが `parse_rotation_parameter` になっていない（`main.rs` から
  直接呼ばれる、対応する `QPDFJob` public メソッドの薄いラッパーが無い
  ケース——8 の (D) 参照）。3 件目（PR #1015 Codex 再レビューで追加）は
  `job/overlay.rs:178` の `apply_overlays_to_page_with_sources`
  （同ファイル `:162` の doc comment 自身が `QPDFJob::doUnderOverlayForPage`
  の移植と明記——1ページ分のcontentを組み立てる側で、`doX`→`x`規則に従えば
  `under_overlay_for_page`相当になるはずだが独自命名になっている）。
  4 件目（PR #1015 Codex 再レビューでさらに追加）は `job/page_range.rs`
  の `PageRange::parse`（8 の (D) で `QUtil::parse_numrange` への
  対応を確認済み）で、`RotateSpec::parse` と同じ形の乖離
  （`parse_numrange` を反映していない）。qpdf の1関数呼び出しを
  `parse`（文字列→中間表現、page count 不要）と `resolve`
  （中間表現+page count→解決済み index 列）の2段階に分割している点は
  命名とは別の設計判断で、page count が未確定な時点（CLI引数の妥当性
  検証等）でも`parse`だけ先に走らせられる利点があるが、それ自体は
  `parse`という名前の正当化にはならない。この 4 件が監査時点で見つかった
  未対応の乖離で、この文書は「監査済みで全て一貫」とは主張しない。
- **戻り値の形が qpdf と違うために動詞ごと変えるのは許容される—ただし
  crate 内で一貫していること**。`doJSONPages`/`doJSONPageLabels`/
  `doJSONOutlines`/`doJSONAcroform`/`doJSONEncrypt`/`doJSONAttachments`
  （qpdf 側は `void` で pipeline に直接書く）→
  `build_pages_section`/`build_pagelabels_section`/…（flpdf 側は `Json`
  値を返す設計）は 6 箇所全てに同じ「`do`+`JSON`を落とし`build_`+`_section`
  を付与」パターンが適用されており、単発の思いつきではなく意図した変換だと
  判断できる。1 箇所だけ動詞が違う場合はそちらを疑う。
- **qpdf 側に個別の識別子が無い（より大きな private メソッド内の
  インラインコードの抽出）場合、flpdf 側の独自命名は逸脱ではない**。
  訳す元の識別子が存在しないのだから「qpdf 名への翻訳」という基準自体が
  適用されない。`prune_acroform_after_subset`
  （qpdf 側は `QPDFJob::handlePageSpecs` 内のインラインコード、
  `QPDFJob.cc:2610-2632`）はこの例。ただし doc comment 側で
  「これは qpdf の `QPDFJob::prune_acroform_after_subset` の移植」のように
  **実在しない qpdf 識別子をでっち上げて引用してはならない**（次項）。
- **doc comment で qpdf の識別子（`QPDFJob::doX` 等）を引用する前に、
  pinned qpdf source（`scripts/fetch-qpdf-source.sh --print-path` /
  `libqpdf/*.cc`・`include/qpdf/*.hh`）に grep して実在を確認する**。
  「qpdf の命名スタイルに合ってそう」は実在の証拠にならない。特に
  `doX`/`handleX` は似た名前が両方存在する（`handleUnderOverlay` と
  `doUnderOverlayForPage` など）ため、雰囲気で片方を選んで引用すると誤る。

### 該当例

`docs/qpdf-correspondence.md` が `QPDFJob::prune_acroform_after_subset` を
実在する qpdf メソッドであるかのように記載していた（実際は
`handlePageSpecs` 内のインラインコードで、qpdf 側に個別の識別子は無い）。
同じセッションで `job/overlay.rs` の doc comment 4 箇所が
`QPDFJob::doUnderOverlay`（qpdf に実在しない）を引用していたことも
判明した。実在するのは `QPDFJob::handleUnderOverlay`
（`QPDFJob.cc:1937`、複数の `--overlay`/`--underlay` グループをまとめて
処理する側）と、その内部から呼ばれる `QPDFJob::doUnderOverlayForPage`
（`QPDFJob.cc:1859`、1 ページ分のコンテンツ文字列を組み立てる側）で、
4 箇所とも前者の責務を指していたため `handleUnderOverlay` に修正した。
どちらも「qpdf 風の識別子をパターンマッチで作文し、実在確認しないまま
docs に書いた」という同一の失敗パターン。`crates/flpdf/src/job/*.rs`
全 17 ファイルの `pub fn`/`fn` 宣言（`rg -c '^\s*(pub )?fn '` で数えた
関数シグネチャの総数）を対象にした命名監査では、この 2 件の doc 記載
ミスと、前項で挙げた `apply_overlay_specs`/`RotateSpec::parse`/
`apply_overlays_to_page_with_sources`/`PageRange::parse`
（未リネームの既知の例外、いずれも `flpdf-ei0h` で追跡中）以外に
high confidence の命名乖離は見つからなかった——`doX`→`x`、
`handleX`→`handle_x`、`parseX`→`x`（型に紐づく `parse` メソッド）の
変換は、その 4 件を除き一貫して正確に行われていた（この監査自体、
複数ラウンドの Codex レビューを経てようやくこの状態に至ったものであり、
更なる見落としが無いとは言い切れない）。

## 8. `pub` 境界も qpdf の public/private 境界に倣う

**既定では、`pub`（クレート境界を超えて見える）は対応する qpdf 側にも
「本当に公開されている」ものがある場合に限る。** qpdf の private メソッド
内のインラインコードを切り出した flpdf 側の実装は、既定で `private`/
`pub(crate)` に留める。ただしこれは無条件の invariant ではなく、
後述の根拠 2（private な qpdf 対応物を `QPDFJob` public メソッド経由で
公開する）・根拠 3（qpdf に対応物が無い flpdf 独自のライブラリ機能）という
明示的な例外つきの既定値である——例えば `QPDFJob::check` は qpdf 側の
`doCheck` が private でも根拠 2 により `pub` でよい。

qpdf 自身の CLI 実行ファイル（`qpdf/qpdf.cc`、わずか 62 行）は
`QPDFJob` の小さな public surface（`initializeFromArgv`/`run`/
`getExitCode` 等）しか呼ばない。`handlePageSpecs`/`doCheck`/
`doListAttachments` のような private メソッド群は `run()` が内部で
orchestrate するだけで、CLI 側からは一切触れない。flpdf-cli も
`job.list_attachments(&mut pdf, verbose)`/`job.write_json(...)` のように
`QPDFJob` の public メソッド経由で呼ぶ箇所ではこの構造を正しく踏襲できている。

### ルール

- `pub` にしてよい根拠は次のいずれか:
  1. 対応する qpdf 側の識別子が実際に public である（例:
     `QUtil::parse_numrange` は namespace 関数として真に public —
     `QPDFJob::parseNumrange` という private ラッパー越しではなく
     こちらを見る）。
  2. qpdf 側は private でも、flpdf-cli（別 crate）が
     **その `QPDFJob` 自身の public メソッド経由で** 呼んでいる
     （`job.list_attachments()`/`job.write_json()`/`job.check()` 等、
     qpdf の `run()` が private 実装を内部 orchestrate するのと同じ形）。
  3. flpdf 独自の意図的なライブラリ機能として、crate ルートの doc
     （`lib.rs` 冒頭）に明記されている（`merge_documents`/`extract_pages`
     等。qpdf に単体の embeddable 対応物が無くても、flpdf が単体の
     Rust PDF ライブラリとして提供すると決めた機能は該当）。
  上記のどれにも当たらないなら `pub(crate)` 以下に落とす。
- **支援型（第 4 の根拠）**: 上記いずれかで legitimate な `pub` メソッドの
  引数/戻り値型に使われている型は、それ自体は qpdf に対応物が無くても
  pub でよい——Rust は private-in-public を禁止するため、public な
  メソッドのシグネチャに現れる型は最低でもメソッドと同じ可視性が要る。
  `AttachmentAddOptions`/`AttachmentCopyOptions`/`CheckError`/
  `JsonJobError`/`JsonJobOptions`/`JsonJobOutput`/`JsonStreamData`/
  `UsageError`/`PageSpecInput`/`SplitPageOptions` はこのカテゴリで、
  `main.rs` が直接 import していても debt ではない——`job.write_json()`/
  `job.check()` 等、根拠 2 で既に legitimate な `QPDFJob` public メソッドの
  シグネチャが要求しているだけ。
- **大元の宣言だけを `pub(crate)` に変える単純な付け替えでは可視性は
  狭められない（ただし再構築すれば片方だけ閉じることは可能——
  PR #1015 Codex 再レビューで是正、過大な主張だった）**: ある項目が複数の
  パス（例: `crate::job::X` と `crate::json_inspect::X`）から到達可能な
  とき、大元の宣言を `pub` から `pub(crate)` に変えるだけでは、もう片方の
  `pub use crate::job::X` が `error[E0364]: X is only public within the
  crate, and cannot be re-exported outside` でコンパイル不能になる
  （`job/mod.rs` の `pub use json_sections::{build_acroform_section, ...}`
  を `pub(crate)` に変えて実測、2026-08-21）。**これが示すのは
  「単純な付け替えでは失敗する」ことだけで、「絶対に片方だけ閉じられない」
  ことではない**——例えば `json_sections`（現状 `job/mod.rs` の
  `mod json_sections;` で private）自体を `pub(crate)` にし、
  `json_inspect.rs` 側が `crate::job::json_sections::{...}` を直接
  re-export するよう変え、`job/mod.rs` 側の `pub use` を削除すれば、
  `crate::json_inspect::X` は残したまま `crate::job::X` だけを閉じられる。
  ただしこれは「re-export 元の付け替え」ではなく module 構造そのものの
  再編（どのモジュールを再輸出のsource of truthにするか）を要する、
  というのが正確な言い方。
- **QPDFJob.hh を読むときの罠**: `Config`/`PagesConfig`/`UOConfig` 等の
  argv パーサー用ネストクラスは、それぞれ自分の `private:` を持つ。
  ネストクラスの `};` で閉じた後、外側 `QPDFJob` 自身の直近の
  `public:`/`private:` に戻る。ネストクラス内の `private:` を外側の
  以降のメンバーにまで「漏らして」数えると、実際は public な
  `run()`/`createQPDF()`/`writeQPDF()`/`hasWarnings()`/`getExitCode()`
  等を private と誤判定する（実際にこのルール作成中に踏んだ）。
  ネスト深さを追わない単純な正規表現抽出は信用せず、疑わしい箇所は
  該当行の直前を目視で確認する。
- flpdf-cli からも flpdf 自身の production コードからも参照されず、
  `#[cfg(test)]` からしか届いていない `pub` 項目は、qpdf の private
  対応物と同じ扱いにする——テストは同一クレート内なので `pub(crate)`
  で十分。「かつて必要だったから `pub` のまま」を放置しない。
- flpdf-cli が job/ の分解済み部品（型・関数）に **直接** 手を伸ばしている
  箇所は、qpdf の CLI が `QPDFJob` の private 実装に一切触れない構造との
  食い違いそのもの。visibility を pub のままにする理由にせず、
  「`QPDFJob` 自身に public メソッドを生やしてそちら経由にできないか」を
  検討する。今すぐ塞げない場合は issue化して構造 debt として記録する
  （単純な visibility 変更では済まないことが多い — `flpdf-hxmj` 参照）。

### 該当例

`job/mod.rs` の `pub use` 一覧を flpdf-cli・doctest・統合テスト
（`tests/*.rs`、別 crate 扱いで `pub` しか見えない）・`lib.rs` の
crate ルート re-export と突き合わせたところ、5 群に分かれた
（最初の版はここを 2 群に単純化して誤り、PR #1015 の Codex レビュー
（複数ラウンド）で段階的に是正した）。

**(A) `QPDFJob` メソッドは legitimate、free 関数側は未検証の debt
（PR #1015 Codex 再レビューで是正——見出しが本文の結論と矛盾していた）**:
`prune_acroform_after_subset`/`prune_acroform_after_subset_with_max_depth`/
`DEFAULT_MAX_ACROFORM_DEPTH`/`write_json`（`job/json.rs:257`）。
flpdf-cli の `main.rs:4753` は
`QPDFJob::prune_acroform_after_subset`（`job/page_specs.rs:546-551` で
free 関数へ委譲する `QPDFJob` メソッド）を呼んでおり、これは根拠 2 を
満たす——**ただし根拠 2 が正当化するのはこの `QPDFJob` メソッドの
pub-nessであって、free 関数自身の pub-ness ではない**。free 関数
`acroform_field_prune::prune_acroform_after_subset` 自体の呼び出し元は
`job/page_specs.rs:550` の `super::` 修飾された同一 crate 内呼び出し
1 箇所のみで、`pub(crate)` でも当該呼び出しはそのままコンパイルできる。
`tests/page_extract_thread_bead_p_tests.rs:113` が free 関数を直接呼ぶ
統合テストである点、`lib.rs:200-206` が crate ルートへ flat re-export
している点は事実だが、根拠 3（「crate ルートの doc（lib.rs 冒頭）に
明記されている」）が要求する opening `//!` doc への明記は無い
（`merge_documents`/`extract_pages` は `lib.rs:63-72` に明記されている
のと対照的）。`write_json` も同型（PR #1015 Codex 再レビューで追加、
監査対象から漏れていた）: 唯一の呼び出し元は `job/lifecycle.rs:269`の
`QPDFJob::write_json`メソッドのみで、しかも `lib.rs` からの
crate ルート再輸出すら無い（根拠3の一部さえ満たさない、
`prune_acroform_after_subset`よりさらに弱いケース）。3 根拠のいずれも
厳密には満たさない **未検証の debt** として`flpdf-xsq1` で追跡し、
`pub(crate)` へ狭めるか opening doc に明記するかの設計判断を別途行う。

**(B) 未検証の debt（PR #1015 Codex 再レビューで訂正）**:
`format_attachment_list`/`format_attachment_list_with_sink`/
`list_attachment_info`/`AttachmentInfo` の 4 つ。一度は
`format_attachment_list`/`list_attachment_info`/`AttachmentInfo` の
3 つだけを「doctest/統合テストが直接使っているので根拠 3 を満たす」として
legitimate 側に残したが、これは (A) で free 関数
`prune_acroform_after_subset` に適用した基準（re-export + 個別の
テスト使用だけでは根拠 3 が要求する opening `//!` doc への明記を
満たさない）と矛盾する二重基準だった。同じ基準を一貫して適用すると、
4 つとも opening doc に明記が無い点は共通しており、`format_attachment_list`
の doctest と `list_attachment_info` の統合テストは存在はするが
opening doc 明記の代替にはならない。4 つまとめて `flpdf-xsq1` で
debt として追跡し、`pub(crate)` へ狭めるか opening doc に明記するかの
設計判断を別途行う（`AttachmentInfo` は `list_attachment_info` の
戻り値型である以上、private-in-public によりその可視性判断に従属する）。

**(C) `pub` のまま — 狭めるには API 縮小そのものが要る**:
`build_pages_section`/`build_outlines_section`/`build_pagelabels_section`/
`build_acroform_section`/`build_encrypt_section`/
`build_attachments_section`/`write_qpdf_json_v2_selected_objects_with_options`/
`write_qpdf_json_v2_selected_objects_to_output_with_options`。
flpdf-cli からは (B) と同様 `job.write_json()` 経由でしか呼ばれないが、
`json_inspect.rs:31-40` がこれら 8 個を `crate::job::{...}` から
`pub use` で crate ルート近くへ再輸出しており（同ファイル自身のコメントが
「staged migration の間、historical な public path を残す」と明言）、
`tests/document_json_tests.rs:9-13` が実際に
`flpdf::json_inspect::write_qpdf_json_v2_selected_objects_with_options`
を統合テストから直接使っている。前掲の re-export ルールの通り、
`job/mod.rs` 側だけ `pub(crate)` に変えると `json_inspect.rs` の
`pub use` が `E0364` でコンパイル不能になることを実測済み——単純な
付け替えでは対策できない（PR #1015 Codex 再レビューで訂正:
前掲のルール改訂で示した「`json_sections` 自体を `pub(crate)` にし
`json_inspect.rs` が直接 re-export する」という module 再編オプションは
ここにも技術的には適用できるが、`json_inspect.rs` 自身のコメントが
「staged migration の間、historical な public path を残す」ことを
明言している以上、この API 自体を最終的に消す方針が既に決まっており、
module 再編で永続化する意味が無い）。狭めるには `json_inspect.rs` の
compatibility re-export 自体を撤去し、`document_json_tests.rs` を
`QPDFJob::write_json()` 経由の書き方へ書き換える、という
staged-migration の完了そのものが要る。これは可視性の issue ではなく
API 移行の issue なので、`flpdf-7bkv`（旧 `flpdf-wy3k` は前提の誤りにより
close 済み）として追跡する。

**(D) 未解消の debt**: `RotateSpec`（`RotateSpec::parse`）。
`main.rs` から直接呼ばれており、対応する qpdf 側は `QPDFJob.hh:482`
private メソッド `parseRotationParameter` のみ（`QUtil` 等どこにも
public な対応物が無いことを確認済み）で、`prune_acroform_after_subset`
（(A)）と違って `QPDFJob` 側に対応する public メソッドの薄いラッパーも
無い——真の意味で qpdf の CLI 構造と食い違っている。

`PageRange` は根拠 1 に該当し debt ではない（PR #1015 Codex 再レビューで
訂正）: 対応する qpdf 側は `QPDFJob::parseNumrange`
（`QPDFJob.cc:418-425`）だが、この実装は例外処理を足しただけの薄い
ラッパーで、実処理は `QUtil::parse_numrange`（`QUtil.hh:464`、namespace
関数として真に public）に委譲している。根拠 1 の例示そのもの
（`QPDFJob::parseNumrange` ではなく `QUtil::parse_numrange` を見る）が
指す状況と一致するため、`PageRange::parse`（+`resolve`。2段階分割は
flpdf 独自で qpdf 自体は1関数で行うが、分割自体は (B) の型で言う
「入れ物」の違いに過ぎない）は根拠 1 で正当化される。`lib.rs:203` から
crate ルートへ再輸出されており、実行可能な doc example を持つ点も
根拠 1 の補強になる（PR #1011 で `job/page_combine.rs`/
`job/page_plan.rs` が job/ 内へ移動済みであることを反映——移動前に
「job/ の外側にある public library API」という別根拠として二重に
legitimate と記載していたのは、この PR 自体の rebase 後の状態と
食い違っていたため訂正した。`PageRange` が `InputSpec::range`/
`InputSpec::new`/`CombinedPlan::build`/`PagePlan::build` という
job/ 内の他モジュールからも使われている事実自体は変わらないが、
それは根拠 1 とは別の追加根拠にはならない）。
`RotateSpec` には `PageRange` のような library-API 側の依存は無く、
`flpdf-hxmj`（single-source/multi-source `--pages` パイプライン統合）で
CLI 経路を一本化すれば `pub(crate)` に落とせる見込み。

**(E) 未検証の debt（PR #1015 Codex 再レビューで追加、スコープ外だった）**:
`overlay_verbose_report`/`apply_overlay_specs`/`collate`（`job/mod.rs:58`、
`page_collate.rs` から再輸出）。`main.rs:3398-3414,4816-4832` の
overlay 2 つに加え、`collate` も `main.rs:4391,4653` から `QPDFJob` の
public メソッドを経由せず `flpdf::` crate ルートから直接呼ばれており、
根拠 1〜3 のいずれにも該当しない。この `(A)`〜`(D)` の監査は
`job/mod.rs` の `pub use` 一覧を起点にしたもので、`main.rs` が job/ の
型・関数を直接 import している箇所を全数走査したものではなかった
（**この文書は監査の網羅性を主張しない**——`job/mod.rs` 起点で
見つかった範囲の記録である。実際、この一文自体も複数ラウンドの
Codex レビューを経て書き足した項目が追加され続けている）。
`apply_overlay_specs` は既に `flpdf-ei0h`（命名）で追跡中のため、
可視性側も `overlay_verbose_report`/`collate` と合わせて同じ
`flpdf-xsq1` に記録する。

---

## 補足

- **本文書を読むタイミング**: 作業に着手する時、設計やブレインストーミングに
  入る時、既存設計を変更する時。コードを開く前。
- **既存 2 本との関係**: 本文書（設計）→ `pdf-rust-review-patterns.md`
  （実装・コードレビュー）→ `pdf-rust-doc-review-patterns.md`（公開 doc）の順で
  適用範囲が下りていく。設計を誤ると下 2 本は誤った設計を綺麗に磨くだけになる。
- 1〜8 は「出発点」「中断シグナル」「前例の検証」「依存順序」「逐語訳の粒度」
  「複数実装統合前の機械可読マーキング」「メソッド名の導出と実在検証」
  「`pub` 境界の qpdf 対応」という、qpdf 移植の設計段階に固有の落とし穴。
