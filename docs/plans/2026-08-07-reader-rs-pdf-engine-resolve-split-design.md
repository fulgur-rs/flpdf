# reader.rs 分割設計: pdf.rs / engine.rs / resolve.rs / obj_cache.rs

> **For Claude:** this is a durable-decision document under `AGENTS.md` §7. The
> qpdf oracle facts below (QPDF.cc method inventory, `QPDF::Members` field
> layout, the `QPDF_*.cc` file split, the `qpdf-rs` crate structure) were
> verified against pinned qpdf 11.9.0 source and live lookups; review them for
> correctness like any other claim. No bd issue exists yet for this work —
> file one (or a small epic) before implementing; this document is the design
> that issue's acceptance criteria should be built from.

**Status:** design only. No code changes, no bd issues filed yet (deliberate
— see 「次のステップ」). Produced via `superpowers:brainstorming` after the
`flpdf-25kg.3.19` (`Pdf::empty()`) session surfaced that `reader.rs` is an
uncomfortable home for the new factory: the name "reader" doesn't semantically
fit a zero-external-input constructor, and more broadly `reader.rs` has become
a catch-all for several qpdf responsibilities that don't share a home in qpdf
itself.

## 動機

1. **対応表の粗さが誤診断を招く**: `QPDF.cc` を対応表で1行に集約していたため、
   今回のセッションだけで `emptyPDF()` の帰属を2回間違えた（最初は
   flpdf-cli、次に flpdf 独自の「QPDFJob」概念）。実際には
   `crates/flpdf/src/job/` が既に QPDFJob の受け皿として存在していた。
2. **`reader.rs` 自体の肥大化**（8700行超）がレビュー・ナビゲーションを
   阻害している。
3. **flpdf 固有の概念（「reader」「writer」のような分類語）を排除して
   qpdf の実際のクラス・ファイル構成に寄せる**ことが、将来 flpdf 独自実装を
   洗い出して置き換えていく作業の前提になる。

スコープは今回 `QPDF.cc`（および `reader.rs` がその責務を実装している範囲）に
限定する。同種の分解手法は将来 `QPDFWriter.cc` 等の肥大ファイルにも
展開しうるが、それは別途判断する。

## qpdf 側の事実確認

### QPDF.cc は117メソッド、qpdf自身に章立てコメントは無い

`libqpdf/QPDF.cc`（2667行）を `rg -n "^QPDF::"` で全メソッドを列挙し
（117件）、責務ごとに分類した。qpdf 自身のソース・ヘッダには章立てコメントは
無く（`rg` で確認済み、`// ----` 等のセクション区切りは0件）、分類は
メソッドの実際の役割から判断した。

### QPDF クラス自体は既に複数の `.cc` ファイルに分割されている

`libqpdf/` を見ると、`QPDF` クラスの実装は `QPDF.cc`（コア）以外に
`QPDF_encryption.cc` / `QPDF_json.cc` / `QPDF_linearization.cc` /
`QPDF_optimization.cc` / `QPDF_pages.cc` に分かれている。これらは
`docs/qpdf-correspondence.md` に既に個別の行があり、flpdf 側にも既存の
受け皿がある:

| qpdf ファイル | flpdf 受け皿 |
|---|---|
| `QPDF_encryption.cc` | `security/standard.rs` + `writer.rs` の encryption context + `encrypt_setup.rs` + `permissions.rs` + `security/password.rs` |
| `QPDF_json.cc` | `document_json.rs`（出力側のみ、入力側は❌） |
| `QPDF_linearization.cc` | `linearization/` |
| `QPDF_optimization.cc` | `optimization.rs` |
| `QPDF_pages.cc` | `pages.rs` + `page_tree_rebuild.rs` |

**発見**: `reader.rs` の `impl<R: Read + Seek> Pdf<R>` ブロック（607-3326行、
99メソッド）を実際に読むと、上記5ファイルに対応するエントリポイント
メソッド（`is_encrypted`/`authenticate_if_encrypted`/`permissions` 等10個。
`signatures` は下記の通り QPDF_encryption.cc とは無関係なので除く）が
**reader.rs 側にも重複して存在する**。これは「reader.rs が大きすぎる」
問題の一部が、実は「本来別の場所にあるべきコードが reader.rs に
漏れ出している」問題であることを示す。

同様に `linearized_hint_ref`（`reader.rs:1815`、実装コメントが
`QPDF_linearization.cc:139-141` を明記）も既存の `linearization/` の
責務が reader.rs 側に漏れ出している一例。

### `QPDF::Members` の実フィールドが Document/Engine の混在を裏付ける

`QPDF.hh:1440-1518` の `Members` クラスを確認すると、フィールドは
以下のように分類できる（Linearization/Optimization/pages/encryption 用の
フィールドは上表の既存受け皿と対応するので除外）:

- **Document 相当**（PDFの中身そのもの）: `pdf_version`, `trailer`,
  `obj_cache`, `deleted_objects`, `unique_id`
- **Engine 相当**（bytesから解決する処理系の状態）: `log`, `tokenizer`,
  `file`（InputSource）, `xref_table`, `resolving`, `resolved_object_streams`,
  `reconstructed_xref`, `in_parse`, `parsed`, `warnings`,
  `attempt_recovery` 等

qpdf 自身はこれを1つの `Members` struct にまとめている（`QPDF` クラスを
2つの型に分けてはいない）。「Document」「Engine」という語彙自体は qpdf の
用語ではなく、今回の分析のために持ち出した分析軸である。ブレスト中の
検討では最終ファイル名として採用しない方向で進んだが、「Engine」は
議論の結果 `engine.rs` として採用した（qpdf 語彙に忠実な代替が無く、
分析用の英単語をそのままファイル名にする妥協として承認済み — 詳細は
下記「決定」節）。「Document」はファイル名としては採用せず、責務は
`pdf.rs`（trivial アクセサ）と `obj_cache.rs`（object cache 直接操作）に
分割した。

### 参考: `qpdf-rs`（crates.io の既存 Rust ラッパー）

https://github.com/ancwrd1/qpdf-rs（`qpdf` crate、libqpdf への薄い FFI
ラッパー）のソース構成を確認: `lib.rs` / `array.rs` / `dict.rs` /
`error.rs` / `object.rs` / `scalar.rs` / `stream.rs` / `writer.rs` の8
ファイルのみ。**「reader」に相当するファイルは存在せず**、`QPdf::empty()`
を含む open 系メソッドは全部 `lib.rs` 直下にある。

このクレートは C++ 側に resolve/xref/cache の複雑さを隠しているため
（Rust 側には現れない）、flpdf のような**再実装**が必要とする
「resolve/seek 機構をどうファイル分割するか」には答えを持たない。
ただし以下は裏付けとして採用する:
- 「reader」という分類語自体が qpdf 語彙にもコミュニティ実装にも無い
- `Pdf::open()` / `Pdf::empty()`（`QPdf::empty()` と同名）という
  公開API名はそのまま維持してよい
- `writer.rs` という名前は妥当（`QPDFWriter.cc` という実在ファイルに対応）

## 決定: 4ファイルへの分割

`struct Pdf<R>` は **1つの型のまま**（qpdf の `Members` が1つの struct で
あることに合わせる。型レベルで `Document`/`Engine` を分離する設計は
不採用 — qpdf 自身が持たない構造を新設することになるため）。`impl` ブロック
だけを責務ごとに複数ファイルへ分割する。

### モジュール階層: `engine.rs`/`resolve.rs`/`obj_cache.rs` は `pdf` の子モジュール

`struct Pdf<R>` の全フィールドは現状 `pub`/`pub(crate)` 修飾なし、つまり
モジュール private（`reader.rs:78-` で確認済み）。Rust の private は
「定義モジュールとその子孫」にしか見えないため、`pdf.rs`/`engine.rs`/
`resolve.rs`/`obj_cache.rs` を `lib.rs` 直下の並列モジュール（`pub mod pdf;`
`pub mod engine;` ...）として追加すると、`engine.rs`/`resolve.rs`/
`obj_cache.rs` は `Pdf` のフィールドに一切アクセスできない
（フィールドを `pub(crate)` に広げてクレート全体へ mutation を晒すか、
どちらかを迫られる）。

既存の `reader.rs` は既にこの問題を解決済みで、`file_object`/`resolver`
サブモジュールを `reader.rs` 冒頭で `pub(crate) mod file_object;` /
`pub(crate) mod resolver;` と**子モジュールとして**宣言している
（`reader.rs:2-3`）。同じパターンに従い、`engine.rs`/`resolve.rs`/
`obj_cache.rs` は `pdf.rs` の子モジュール（`pdf::engine` /
`pdf::resolve` / `pdf::obj_cache`、ファイルパスは
`crates/flpdf/src/pdf/engine.rs` 等）として宣言する。`lib.rs` からは
`pub mod pdf;` のみ追加すればよい。

**既存 `crates/flpdf/src/engine.rs`（PR #657 でマージ済み、`Pdf::empty()`
のみ）への影響**: 現状は `lib.rs:102` の `pub mod engine;` で並列モジュール
として宣言されており、実際に存在する（`empty()` は既存の
`pub fn open_mem_owned` を呼ぶだけで `Pdf` の private フィールドに触れない
ため、たまたま並列モジュールのままコンパイルが通っている）。`struct Pdf<R>`
を `pdf.rs` へ移し、`engine.rs` を `pdf::engine` へ再配置する際、
配線を更新する必要がある: (1) `lib.rs:102` の `pub mod engine;` を削除し
（中身は `pdf.rs` 内の `mod engine;` 宣言に置き換わる）、(2) `lib.rs:258`
の `pub use reader::{EncryptionInfo, Pdf, PdfOpenOptions, Permissions};`
を `pub use pdf::Pdf;`（他の型の移動先に応じて分割）に retarget する。
公開パス `flpdf::Pdf` 自体は変わらない（re-export 元が変わるだけ）。

**`security/standard.rs`/`encrypt_setup.rs`/`permissions.rs`/
`object_copy.rs` は同じ解決法が使えない**: これらは `engine.rs` 等と違い
**既存の**（本設計が新設しない）`lib.rs` 直下の並列モジュールで、独自の
既存責務を持つため `pdf` の子へ付け替えるのは大きすぎる変更になる。
一方 `is_encrypted`（`self.encryption.borrow()` を直接読む、
`reader.rs:183,721-723`）や `take_foreign_object_map`（`self.
foreign_object_maps` を直接読む、`reader.rs:110,1981-`）のような
エントリポイントは `Pdf` の private フィールドに直接アクセスするため、
そのまま `security/*`/`object_copy.rs` に移すとコンパイルできない。
2通りの解決策があり、実装時に選ぶ: (a) 該当フィールドだけ
`pub(crate)` にする（クレート内アクセスは許すが、公開APIは晒さない。
`is_dirty`/`live_object_refs` 等 `obj_cache.rs` グループが返す値の型は
既に外部公開されているため、これは新規の公開面拡大ではない）。
(b) エントリポイント自体は `Pdf` が定義されるモジュール
（`pdf.rs`/`pdf::obj_cache` 等）に残し、`security/*`/`object_copy.rs` へは
**既に `&Dictionary`/`&mut EncryptionState` 等の抽出済み値だけを取る
純粋関数**（`required_revision`/`interpret_cf`/`crypt_filter_modes` は
既にこの形）だけを移す。エントリポイント本体はこれらの純粋関数を
呼ぶだけの薄いラッパーとして `Pdf` 側に留まる。(b) の方が公開面を一切
広げずに済むため望ましいが、`authenticate_if_encrypted` 全体をこの形に
分解できるかは実装時に確認する。

### `pdf.rs`（新規）
- `struct Pdf<R>` 定義、`Drop` impl
- **真に trivial な**（qpdf 側でも1ステップの field 返却のみ）直接
  フィールドアクセサ: `version`, `trailer`, `root_ref`,
  `ever_called_get_all_pages`, `mark_get_all_pages_called`。
  根拠: qpdf の `QPDF::getTrailer()`（`QPDF.cc:2349-2352`）は
  `return m->trailer;` のみで解決処理を一切行わない
- 「reader.rs にあるのがおかしい」の根本原因（crate全体で使う中心型が
  narrow-purpose に見えるファイル名の下にある）をここで解消

### `engine.rs`（新規、orchestration 層）
- `Pdf` を返す factory 全部: `open`, `open_with_repair`, `open_best_effort`,
  `open_with_options`, `empty`, `open_mem`, `open_mem_owned`,
  `open_mem_with_options`, `open_mem_owned_with_options`、および実際の
  構築処理を行う private helper `open_with_repair_mode`（xref 読み込み・
  各フィールド構築・resolver 設置・暗号認証を行い、公開 `open*` 全部が
  これに委譲する。ここを含めないと構築のオーケストレーション本体が
  reader.rs に残る）
  （qpdf の `processFile`/`processMemoryFile`/`emptyPDF` に対応）
- 解決のエントリポイント: `resolve_object_handle`,
  `resolve_object_handle_to_terminal(_ref)` 等、呼び出し側から見える
  「解決してくれ」という要求の受け口。**`get_object_handle` はここに
  含めない**（`obj_cache.rs` 参照 — 実処理は cache 登録のみで resolve では
  ない）
- 上記が内部で `resolve.rs` の一次プリミティブを呼ぶ、という二層構造

### `resolve.rs`（新規、`reader/resolver.rs` の `ResolverCore` を統合・拡張）
- resolve（参照→値の解決処理全体）と seek（InputSource内オフセット移動・
  バイト読み取り）の一次プリミティブ。qpdf の `resolve()` 実メソッド名に
  対応する、最も qpdf 語彙に忠実な名前
- 対象: `lift`/`lift_bounded`/`lift_dictionary*`, `read_object_at*`,
  `resolve_compressed_entry`, `decrypt_resolved_object`,
  `collect_object_stream_chain`, header/startxref 探索, xref 読み取り
  （`xref.rs` の既存 API を呼ぶだけで xref.rs 自体は変更しない — 詳細は
  「非目標」参照）, `resolve_to_cache`, `native_parse_uncompressed_value` 等
- **`source_xref_offsets`, `source_xref_entries`, `source_header_offset`,
  `previous_xref_offset`, `last_xref_form`, `compressed_parent` もここに
  含める**（後述の「未決定」から格上げ）。これらは qtest 専用ではなく
  production consumer が存在する: `writer.rs:1004-1005,1027,1151,1469`
  （xref stream 生成時の source offset 参照）と
  `subset_prune.rs:196`（`compressed_parent`）。ソース document の
  xref 由来構造情報を返す resolve/seek 隣接の状態なので、`resolve.rs`
  が正しい置き場所
- **`adobe_extension_level`, `trailer_handle`, `trailer_key_handle` も
  ここに含める**（`pdf.rs` からの再分類）。根拠: qpdf の
  `QPDF::getExtensionLevel()`（`QPDF.cc:2328-2346`）は
  `getRoot().getKey("/Extensions").getKey("/ADBE").getKey("/ExtensionLevel")`
  という多段の間接参照 chain walk を行う実処理で、`getTrailer()` のような
  単純な field 返却ではない。flpdf 側でも `adobe_extension_level` は
  `resolve()` を呼び、`trailer_handle`/`trailer_key_handle` は
  `resolve.rs` に割り当てた `lift()` を呼ぶ（`reader.rs:1218-1298` で確認
  済み）ため、双方の実装が一致してこの分類を裏付ける
- 既存 `reader/resolver.rs` の `ResolverCore<R>`（`pub(crate)`,
  `object_cache: BTreeMap<ObjectRef, ObjectHandle>` 等）が既にこの領域の
  一部を担っているため、実装時に統合対象を精査する

### `obj_cache.rs`（新規）
- object cache への直接操作（qpdf `Members::obj_cache` フィールドに対応、
  resolve を経由しない document 自身の CRUD）: `set_object`, `delete_object`,
  `is_dirty`, `dirty_object_refs`, `clear_dirty`, `object_refs`,
  `live_object_refs`, `is_canonical_object_handle`,
  `next_available_object_ref`, `object_number_is_available`, `unique_id`,
  `mark_object_dirty`, `mark_object_handle_mutated`,
  `mark_object_handle_dirty`, `make_indirect_object_handle`,
  `get_all_object_handles`, `resolved_count`, `deleted_object_refs`
  （`resolved_count`/`deleted_object_refs` は `self.cache` への直接委譲、
  `reader.rs:1655-1661` で確認済み）
- **`get_object_handle` も engine.rs からここへ再分類**。自身の doc
  コメント（`reader.rs:1849-1850`）が「This does not perform file I/O or
  force object-body parsing」と明記し、qpdf の `QPDF::getObject`
  （`QPDF.cc:1951-1959`、obj_cache の登録/参照のみ）に対応する
  identity 操作であって resolve ではない。`is_canonical_object_handle`/
  `make_indirect_object_handle`/`get_all_object_handles` と同じ cache
  identity API 群としてまとめる

### 新規ファイルを作らず既存へ委譲するもの
- 暗号/認証エントリ（`is_encrypted`, `authenticate_if_encrypted`,
  `encrypt_dictionary`, `encryption_ref`, `uses_weak_crypto`,
  `encryption_info`, `permissions`, `user_password_matched`,
  `owner_password_matched`, `encryption_file_key`）は `QPDF_encryption.cc`
  の既存受け皿（`security/standard.rs` / `encrypt_setup.rs` /
  `permissions.rs`）へ移す。reader.rs 側の実装は重複であり、削除対象。
  **この10個の公開メソッドだけでは実装が終わらない**: `authenticate_if_
  encrypted`（`reader.rs:945-`）は reader.rs 内で private 定義されている
  `EncryptionState`(`reader.rs:197-`)/`EncryptionMode`(`reader.rs:470-`)/
  `required_revision`/`required_version`/`required_permissions`
  (`reader.rs:4228-4292`)/`interpret_cf`(`reader.rs:4263-`)/
  `crypt_filter_modes`(`reader.rs:4293-`) に依存する。これらのヘルパー型・
  関数も同じ移動対象に含める（`docs/qpdf-correspondence.md:136` が
  `interpret_cf` 系を既に `QPDF::interpretCF`（`QPDF_encryption.cc:700-716`）
  対応として記録済み）。含めないと暗号ロジックの大半が reader.rs に
  残ったまま、10個の薄いラッパーだけを移動することになる
- `signatures`（`reader.rs:871-873`、`crate::signatures::signatures` への
  薄い委譲）は上記グループに**含めない**。qpdf の `QPDF.cc` に signature
  関連コードは0件、`QPDFAcroFormDocumentHelper.cc` にあるのも
  `disableDigitalSignatures()`（削除のみ）で検査/読み取り API は無い
  （`docs/qpdf-correspondence.md:367` の記載も同じ）。qpdf に対応物のない
  flpdf 独自機能なので、既存 `signatures.rs` へ移す
- `take_foreign_object_map`/`set_foreign_object_map`（`reader.rs:1981-1995`）
  は `obj_cache.rs` に**含めない**。qpdf `Members::obj_cache`
  （`QPDF.hh:1467`）とは別フィールドの `Members::object_copiers`
  （`QPDF.hh:1476`、`ObjCopier::object_map`）に対応し、production caller は
  `object_copy.rs` の `copyForeignObject` 実装のみ（grep で確認済み）。
  既存 `object_copy.rs` へ移す
- `linearized_hint_ref`（`reader.rs:1815-1837`、コメントが
  `QPDF_linearization.cc:139-141` を明記）は既存 `linearization/` へ移す
- JSON 出力準備群（`QpdfPreparedObjects`(`reader.rs:186-189`),
  `prepare_qpdf_json_objects`(`reader.rs:1728-1777`),
  `qpdf_json_live_object_refs`(`reader.rs:1788-1798`),
  `resolve_qpdf_json_object`(`reader.rs:2943-2967`),
  `resolve_qpdf_json_object_borrowed`(`reader.rs:2976-`)）は
  `pdf.rs`/`engine.rs`/`resolve.rs`/`obj_cache.rs` のどれにも含めない。
  production caller は `document_json.rs:151`
  （`prepare_qpdf_json_objects`）で、既存の `QPDF_json.cc` 出力側の受け皿
  （`document_json.rs`、上表参照）に対応する。実装は resolve/cache 両方に
  触れる（`self.resolver`/`self.cache` を直接操作）ため、
  `document_json.rs` 側からこの2ファイルの private API を呼ぶ形になるか、
  `document_json.rs` 自身へ実装ごと移すかは実装時に決める

### 未決定（実装時に判断してよい細部）
- qtest 専用の source-offset introspection（`qtest_decode_parms_source_offset`
  / `qtest_object_value_source_offset` / `qtest_array_item_source_offset` /
  `qtest_read_source_object_with_retry` 等、`qtest_` 接頭辞を持つ本当に
  production consumer の無いもの）: qpdf に対応物が無い flpdf 独自のテスト
  基盤。`resolve.rs` に同居させるか独立ファイルにするかは実装時に決める。
  `source_xref_offsets`/`compressed_parent` 等は production consumer が
  あるため上記 `resolve.rs` へ格上げ済み（この bullet からは除外）
- `warnings`（`repair_diagnostics`, `push_warning`, `recovered_stream_eol`）
  の最終置き場所（`engine.rs` か `pdf.rs` か、小規模なので実装時判断）

## 非目標

- `struct Pdf<R>` の型レベル分割（`Document`/`Engine` を別型にする設計）は
  今回採用しない
- 公開 API 名 `Pdf::open()` / `Pdf::empty()` 等は変更しない
  （後方互換自体は考慮しないが、この命名自体は維持する）
- `QPDFWriter.cc` 等、他の肥大 qpdf ファイルへの同種分解は今回の
  スコープ外（将来別途判断）
- `xref.rs` / `object_copy.rs` / `pages.rs` など、既に qpdf 対応が
  取れているファイルの**既存ロジックの変更**は無い。`resolve.rs` は
  `xref.rs` の既存 API を呼ぶだけで `xref.rs` 自体は変更しない。
  `object_copy.rs` へは `take_foreign_object_map`/`set_foreign_object_map`
  の追加移動のみ（上記参照）

## 次のステップ

このセッションでは設計のみ。以下は別セッションで:

1. bd issue（epic + サブタスク）を本設計に基づいて作成する
2. 実装順序の決定（`pdf.rs` 抽出 → `obj_cache.rs` 抽出 →
   `resolve.rs`/`reader/resolver.rs` 統合 → `engine.rs` 抽出 →
   暗号/認証エントリの既存ファイルへの移動、が依存の少ない順と思われるが
   要検証）
3. `docs/qpdf-correspondence.md` の `QPDF.cc` 行（§1、現行本文
   `reader.rs`(7898) + `reader/resolver.rs`(...) + `reader/file_object.rs`
   (1405) + `xref.rs`(1220) + `object_copy.rs`(342: `copyForeignObject`) +
   `cache.rs`(112) + `writer/object_streams.rs`(207-237) +
   `signatures.rs`(245-: `removeSecurityRestrictions`) +
   `page_closure.rs`(441) + `ref_chain.rs`(159)）を更新する際は、
   **`reader.rs` と `reader/resolver.rs` の2項目だけを本設計の4ファイル
   （`pdf.rs`/`engine.rs`/`resolve.rs`/`obj_cache.rs`）に差し替える**。
   `reader/file_object.rs`/`xref.rs`/`object_copy.rs`/`cache.rs`/
   `writer/object_streams.rs`/`signatures.rs`/`page_closure.rs`/
   `ref_chain.rs` は本設計が触れない別責務なので、既存の記載をそのまま
   残す（`docs/qpdf-correspondence.md:372-373` が `object_copy.rs` を
   `QPDF.cc` 行にあえて置いている理由を明記しており、同じ理由で他の
   7ファイルも残す必要がある。`QPDF_encryption.cc`/`QPDF_json.cc`/
   `QPDF_linearization.cc`/`QPDF_optimization.cc`/`QPDF_pages.cc` は
   これとは別に既に独立行を持つため、そちらとの二重帰属だけを避ける）。
   上記「新規ファイルを作らず既存へ委譲するもの」で個別に触れた
   `signatures.rs`/`object_copy.rs`/`linearization/`/`document_json.rs`
   への移動は、それぞれの既存行（§7/§8）を個別に更新する
4. 各ステップは出力バイトに影響しない「入れ物」の変更のみ
   （CLAUDE.md 分類(B)）なので、バイト差分ゼロを都度確認しながら進める。
   AGENTS.md §7 の acceptance-criteria 階層として、各ステップの検証には
   具体的なコマンドを紐付ける: `cargo test -p flpdf`（lib 全件 + 全
   integration + doctest）に加え、`--features qpdf-zlib-compat` での
   byte-identical テスト群（`cmp_linearize_tests.rs` の
   `assert_*_byte_identical`、`crates/flpdf-cli/tests/cli_byte_identical.rs`、
   `deterministic_id_qpdf_parity_tests.rs`、`compat_matrix_tests.rs`）を
   移動対象メソッドが実際に通る経路であることを確認したうえで実行する。
   特に resolve/認証まわりの移動は、単体テストだけでは値の materialize
   有無や byte 出力まで確認できないため、上記 byte-identical スイートを
   必ず含める。**`authenticate_if_encrypted`/暗号ヘルパーの移動には上記
   4本では不十分**（いずれも暗号化フィクスチャ・パスワード・
   `PdfOpenOptions` を使わないため認証経路を通らない）。代わりに
   `qpdf-zlib-compat` gated かつ暗号化ラウンドトリップを検証する
   `crates/flpdf-cli/tests/encrypt_cli_tests.rs`
   （`--encrypt` で書いた出力を qpdf で再オープン検証、パスワード付き
   フィクスチャを使用）を必須テストとして追加する。加えて各ステップ
   実行前後で `scripts/patch-coverage.sh` を回し、変更行 100%
   カバレッジを維持する
