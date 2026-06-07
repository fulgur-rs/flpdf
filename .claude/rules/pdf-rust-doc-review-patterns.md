# flpdf ドキュメンテーション レビュー方針（公開API doc）

`pdf-rust-review-patterns.md`（コードのレビュー）の **ドキュメント版**。
docs.rs に published される公開API doc（`crates/*/src/` の `///` / `//!`）の
品質レビュー方針を、コードベースの実態調査を根拠に予防ルール化したもの。
コードを書く前・doc を書く前・レビューする前に必ず確認すること。

調査時点の実態（公開src の doc コメント面、下記 grep で再現可能）:
beads issue ID 漏れ **123 行**（うち `crates/flpdf` が大半） /
内部ジャーゴン漏れ **28 行** / 日本語混在 **7 行** / doc 内 TODO **4 件**。
一方 `flpdf/src/lib.rs` の crate doc は intra-doc リンクと end-to-end 例を
備えており、土台は良好。本文書はその水準を全公開面へ広げるための指針。

---

## 貫く原則

**docs.rs の読者は flpdf の内部開発文脈を知らない外部利用者である。**
あらゆる判断はこの基準で行う。「自分（開発者）に分かるか」ではなく
「flpdf を初めて使う人に有用で、かつノイズが無いか」で公開 doc を評価する。

「公開面」の定義: `cargo doc -p flpdf`（および各公開クレート）で生成される
HTML に現れるか。テストファイル（`tests/`）の `///` / `//!` は published
されないため本文書の対象外。対象は `crates/*/src/` の公開項目に付く doc。

---

## 1. 内部トラッカー痕跡を doc に残さない（ノイズ／追跡不能）

**最頻出**。外部利用者が追跡できない内部識別子を公開 doc に書くと、
意味のないノイズになり、ドキュメントの信頼性を下げる。

### ルール
- `///` / `//!` に beads issue ID（`flpdf-xxx` / `flpdf-9hc.4.9` のような
  ドット連結の階層 ID を含む）を書かない。読者はそれを開けないため無価値。
- epic / stack layer N/N / follow-up / tracked as / deferred to /
  「once X lands」等の **内部進行管理語** を公開 doc から除く。
- 「なぜこの実装か」を残したい場合は issue ID ではなく **仕様の根拠** で書く
  （ISO 32000-2 の節番号、qpdf の観測挙動、PDF の構造的制約など、
  外部利用者が検証・参照できる事実）。
- トレーサビリティ（どの issue で入ったか）は git blame と beads が担う。
  doc の役目ではない。

### 該当例
`encrypt_setup.rs:1`（モジュール doc 冒頭に `flpdf-9hc.4.9`）,
`reader.rs:169`, `page_split.rs:36`, `page_closure.rs:36`（epic 参照）

---

## 2. 公開 doc と内部実装メモを分離する（境界）

実装の経緯・将来計画・未整理メモは外部利用者には不要。doc 化すると
1 と同じノイズになる。`///` / `//!`（doc）と `//`（通常コメント）を
役割で使い分ける。

### ルール
- 外部利用者に有用な「何を・どう使うか」だけを `///` / `//!` に書く。
  実装の経緯・設計判断の備忘・将来計画は通常コメント `//` に置く（doc 化しない）。
- doc 内に TODO / FIXME / XXX / HACK を書かない。残す必要があれば `//` か
  beads issue へ。公開 doc に未完了マーカーを晒さない。
- 「not yet supported」等の未実装言及は、その制約が **API に残る** なら
  現在形の制約として書く（例: "Linearized input is rejected."）。
  内部 follow-up への参照（「flpdf-… の後続で対応」）は外す。

### 該当例
doc 内 TODO/FIXME 4 件, `encrypt_setup.rs:190`（"not yet supported
(flpdf-9hc.4.9 follow-up)"）, `page_split.rs:63`

---

## 3. 公開 API doc の必須要素（rustdoc 慣習）

外部利用者が安全に使うために必要な情報を構造化して提供する。
rustdoc の慣習に沿わせることで、利用者が期待する位置に情報が見つかる。

### ルール
- 全公開項目に **1 行要約**（命令法・末尾ピリオド）。最初の段落が
  docs.rs の一覧に出るため、ここで「何をするものか」を言い切る。
- `Result` を返す関数には `# Errors`（どんな場合に `Err` になるか）。
  panic しうる関数には `# Panics`。`unsafe` 関数・トレイトには `# Safety`。
- 主要な公開型・エントリポイントには `# Examples`。`lib.rs` の
  end-to-end 例（`Pdf::open` → 検査 → write）を手本にする。
- 実態として `# Errors` 94 / `# Panics` 7 は整備済み。**欠落箇所を補う**
  方針で、既存の良いパターンを横展開する。

### 該当例（手本）
`lib.rs:1`（crate doc の構成・intra-doc リンク・`no_run` 例）

---

## 4. リンクと例の健全性（腐敗防止）

doc 内の参照と例は「書いて終わり」ではなく、コンパイル・リンクが
壊れていないことを CI で担保する。腐った例は無いより有害。

### ルール
- 他の API への参照は **intra-doc リンク**（`` [`Pdf::open`] ``）で書く。
  プレーンな `Pdf::open` 文字列はリンクにならず、リネーム時に追従できない。
- doc 例は **doctest として成立**させる。コンパイル不能な擬似コードは
  `text` / `ignore`、実行だけ避けたいものは `no_run` を明示。
  `should_panic` / `compile_fail` も意図通りに使う。
- 壊れた intra-doc リンクを残さない。`cargo doc` の
  `broken_intra_doc_links` warning をゼロに保つ。
- doc フェンス行は調査時点で 184 行（開閉ペアで約 92 ブロック）。CI で
  `cargo test --doc` を回し、
  API 変更で例が腐るのを防ぐ。

### 該当例
`lib.rs`（intra-doc リンクと `no_run` 例の正しい使い方）

---

## 5. 公開 doc は英語で統一する（一貫性）

docs.rs は国際的な利用者に公開される面。公開 doc 面の言語を英語に統一し、
レビュー過程で生じた日本語の痕跡を残さない。

### ルール
- `crates/*/src/` の `///` / `//!`（published される面）は **英語のみ**。
  日本語の説明文を公開 doc に置かない。
- 非公開面（`//` 通常コメント、非公開関数の doc）の日本語は対象外。
  あくまで「docs.rs に出る面」に限った規律。
- レビュー由来の語（「指摘1」「指摘2」「roborev low 指摘」等）は、英語に
  翻訳するのではなく **削除** し、仕様・挙動の記述に置き換える
  （その痕跡自体が doc に不要なメタ情報）。

### 該当例
`json_inspect.rs:342-345`（関数 doc がまるごと日本語）,
`page_split.rs:42`, `resources.rs:62`（"roborev low 指摘2"）, `:968`（"指摘1 fix"）

---

## 補足

- **検出の足がかり（grep）**
  - issue ID: `grep -rnE '(///|//!).*flpdf-[0-9a-z]{2,3}' crates/*/src/`
  - 内部ジャーゴン: `grep -rnE '(///|//!).*(epic|stack layer|follow-up|deferred|tracked as)' crates/*/src/`
  - 日本語混在: `grep -rnP '(///|//!).*[ぁ-んァ-ヶ一-龠]' crates/*/src/`
  - doc 内 TODO: `grep -rnE '(///|//!).*(TODO|FIXME|XXX|HACK)' crates/*/src/`
- **公開面か否かの最終判定**は `cargo doc -p <crate>` の生成物で確認する。
  `tests/` のコメントは published されないので対象外。
- **強制力**: 本文書は人手レビューの指針。将来的に
  `#![deny(rustdoc::broken_intra_doc_links)]` と CI の `cargo test --doc` を
  導入すれば 4 を機械化できる。1・2・5 は文字列 grep を CI チェックに
  落とし込める（上記 grep がゼロ件であることを検査）。
- 1〜5 は「内部痕跡の混入」「公開/非公開の境界」「rustdoc 必須要素」
  「リンク・例の腐敗」「言語の一貫性」という、公開 doc 面の構造的な落とし穴。
