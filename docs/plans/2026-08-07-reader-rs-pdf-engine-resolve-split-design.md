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
メソッド（`is_encrypted`/`authenticate_if_encrypted`/`permissions`/
`signatures` 等11個）が **reader.rs 側にも重複して存在する**。これは
「reader.rs が大きすぎる」問題の一部が、実は「本来別の場所にあるべき
コードが reader.rs に漏れ出している」問題であることを示す。

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
用語ではなく、今回の分析のために持ち出した分析軸であり、flpdf 側の
最終ファイル名としては採用しない（後述）。

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

### `pdf.rs`（新規）
- `struct Pdf<R>` 定義、`Drop` impl
- trailer/version 等の直接フィールドアクセサ: `version`, `trailer`,
  `trailer_handle`, `trailer_key_handle`, `root_ref`, `adobe_extension_level`,
  `ever_called_get_all_pages`, `mark_get_all_pages_called`
- 「reader.rs にあるのがおかしい」の根本原因（crate全体で使う中心型が
  narrow-purpose に見えるファイル名の下にある）をここで解消

### `engine.rs`（新規、orchestration 層）
- `Pdf` を返す factory 全部: `open`, `open_with_repair`, `open_best_effort`,
  `open_with_options`, `empty`, `open_mem`, `open_mem_owned`,
  `open_mem_with_options`, `open_mem_owned_with_options`
  （qpdf の `processFile`/`processMemoryFile`/`emptyPDF` に対応）
- 解決のエントリポイント: `get_object_handle`, `resolve_object_handle`,
  `resolve_object_handle_to_terminal(_ref)` 等、呼び出し側から見える
  「解決してくれ」という要求の受け口
- 上記が内部で `resolve.rs` の一次プリミティブを呼ぶ、という二層構造

### `resolve.rs`（新規、`reader/resolver.rs` の `ResolverCore` を統合・拡張）
- resolve（参照→値の解決処理全体）と seek（InputSource内オフセット移動・
  バイト読み取り）の一次プリミティブ。qpdf の `resolve()` 実メソッド名に
  対応する、最も qpdf 語彙に忠実な名前
- 対象: `lift`/`lift_bounded`/`lift_dictionary*`, `read_object_at*`,
  `resolve_compressed_entry`, `decrypt_resolved_object`,
  `collect_object_stream_chain`, header/startxref 探索, xref 読み取り
  （`xref.rs` と要調整）, `resolve_to_cache`, `native_parse_uncompressed_value`
  等
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
  `get_all_object_handles`, `take_foreign_object_map`,
  `set_foreign_object_map`

### 新規ファイルを作らず既存へ委譲するもの
- 暗号/認証エントリ（`is_encrypted`, `authenticate_if_encrypted`,
  `encrypt_dictionary`, `encryption_ref`, `uses_weak_crypto`,
  `encryption_info`, `permissions`, `user_password_matched`,
  `owner_password_matched`, `encryption_file_key`, `signatures`）は
  `QPDF_encryption.cc` の既存受け皿（`security/standard.rs` /
  `encrypt_setup.rs` / `permissions.rs`）へ移す。reader.rs 側の実装は
  重複であり、削除対象

### 未決定（実装時に判断してよい細部）
- qtest 用の source-offset introspection（`qtest_*`, `source_xref_offsets`,
  `compressed_parent` 等15個）: qpdf に対応物が無い flpdf 独自のテスト基盤。
  `resolve.rs` に同居させるか独立ファイルにするかは実装時に決める
- `warnings`（`repair_diagnostics`, `push_warning`, `recovered_stream_eol`）
  の最終置き場所（`engine.rs` か `pdf.rs` か、小規模なので実装時判断）
- `xref.rs`（既存）と `resolve.rs`（新設）の境界線の精査

## 非目標

- `struct Pdf<R>` の型レベル分割（`Document`/`Engine` を別型にする設計）は
  今回採用しない
- 公開 API 名 `Pdf::open()` / `Pdf::empty()` 等は変更しない
  （後方互換自体は考慮しないが、この命名自体は維持する）
- `QPDFWriter.cc` 等、他の肥大 qpdf ファイルへの同種分解は今回の
  スコープ外（将来別途判断）
- `xref.rs` / `object_copy.rs` / `pages.rs` / `security/*` など、既に
  qpdf 対応が取れているファイルの変更は無い

## 次のステップ

このセッションでは設計のみ。以下は別セッションで:

1. bd issue（epic + サブタスク）を本設計に基づいて作成する
2. 実装順序の決定（`pdf.rs` 抽出 → `obj_cache.rs` 抽出 →
   `resolve.rs`/`reader/resolver.rs` 統合 → `engine.rs` 抽出 →
   暗号/認証エントリの既存ファイルへの移動、が依存の少ない順と思われるが
   要検証）
3. `docs/qpdf-correspondence.md` の `QPDF.cc` 行（§1、現状1行に約10ファイル
   詰め込み）を、本設計の4ファイル + 既存5ファイルへ分割更新する
4. 各ステップは出力バイトに影響しない「入れ物」の変更のみ
   （CLAUDE.md 分類(B)）なので、バイト差分ゼロを都度確認しながら進める
