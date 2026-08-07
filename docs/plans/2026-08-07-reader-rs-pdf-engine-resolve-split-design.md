# reader.rs 分割設計: pdf.rs / engine.rs / resolve.rs / obj_cache.rs

> **For Claude:** this is a durable-decision document under `AGENTS.md` §7. The
> qpdf oracle facts below (QPDF.cc method inventory, `QPDF::Members` field
> layout, the `QPDF_*.cc` file split, the `qpdf-rs` crate structure) were
> verified against pinned qpdf 11.9.0 source and live lookups; review them for
> correctness like any other claim. `flpdf-0b12` (issue filed, implemented in
> `refactor/flpdf-0b12-pdf-module`, stacked on this PR) covers the `pdf.rs`
> slice; file bd issues for the remaining slices (`obj_cache.rs`/
> `resolve.rs`/`engine.rs` extraction, encryption-entry/foreign-object-map/
> JSON-prep/`linearized_hint_ref` relocation) before implementing them.

**Status:** design, with the `pdf.rs` slice already implemented
(`flpdf-0b12`, see モジュール階層節). The rest is design only; see
「次のステップ」. Produced via `superpowers:brainstorming` after the
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
2. **`reader.rs` 自体の肥大化**（本設計の分析着手時点で8700行超。
   `flpdf-0b12` が8メソッドを `pdf.rs` へ抽出した現状は8475行）が
   レビュー・ナビゲーションを
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

**発見**: `reader.rs` の `impl<R: Read + Seek> Pdf<R>` ブロック（分析当時は
607-3326行・99メソッド。`flpdf-0b12` が8メソッドを `pdf.rs` へ抽出した後の
現状は444-3051行・91メソッド）を実際に読むと、上記5ファイルのうち
**`QPDF_encryption.cc` に対応するエントリポイントメソッド**
（`is_encrypted`/`authenticate_if_encrypted`/`permissions` 等10個。
`signatures` は下記の通り QPDF_encryption.cc とは無関係なので除く）が
**reader.rs 側にも重複して存在する**。これは「reader.rs が大きすぎる」
問題の一部が、実は「本来別の場所にあるべきコードが reader.rs に
漏れ出している」問題であることを示す。

同様に `linearized_hint_ref`（`reader.rs:1541-1575`、実装コメントが
`QPDF_linearization.cc:139-141` を明記）は、上記5ファイルのうち
`QPDF_linearization.cc`（既存の `linearization/` が受け皿）の責務が
reader.rs 側に漏れ出している別の一例。

**行番号の注記**: 以下、本設計文書内の `reader.rs:NNNN` 引用は
`flpdf-0b12`（commit `56a01c2b`）適用後、つまり `pdf.rs` 抽出済みの
現行 `reader.rs` を指す（`refactor/flpdf-0b12-pdf-module` ブランチで
再確認済み）。

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

### モジュール階層: `pub(crate)` フィールドで解決済み（実装済み）

`struct Pdf<R>` の全フィールドは元々 module private（`reader.rs:78-` 参照）
だった。`pdf.rs` を `lib.rs` 直下の並列モジュールとして追加すると、
`reader.rs` 側に残る他の `impl Pdf<R>` ブロック（大半のメソッドはまだ
そこにある）が private フィールドへアクセスできなくなる、という懸念が
あった。

**この懸念は `flpdf-0b12`（`refactor/flpdf-0b12-pdf-module`、
commit `56a01c2b`）で実装され、解決済み**: `Pdf<R>` の全フィールドを
`pub(crate)` にし（`crates/flpdf/src/pdf.rs:51-`）、`reader.rs` 側は
並列モジュールのまま変更しない。`reader.rs` に残る `lift`/
`lift_to_handle_bounded`/`EncryptionState`/
`ResolverHandle::encryption_parameters` も同様に可視性だけ `pub(crate)`
へ広げ（実装自体は移動しない）、`pdf.rs` 側から呼べるようにした。
**以前の版にあった「`engine.rs`/`resolve.rs`/`obj_cache.rs` は `pdf` の
子モジュールとして宣言する」という案は不採用**（`pub(crate)` の方が
遥かに小さい変更で同じ問題を解く）。

`pdf.rs` へ実際に移動したのは8メソッド + `Drop` impl のみ（`crates/flpdf/src/
pdf.rs:179-` で確認済み）: `version`, `trailer`, `trailer_handle`,
`trailer_key_handle`, `root_ref`, `adobe_extension_level`,
`ever_called_get_all_pages`, `mark_get_all_pages_called`。**`unique_id()`
アクセサ（フィールド自体は `struct Pdf` と共に `pdf.rs` にあるが、
`pub(crate) fn unique_id(&self) -> u64 { self.unique_id }` という
アクセサメソッドは reader.rs:1703 に残ったまま**——以前の版の「`unique_id`
も移動済み」という記載は誤りだったため訂正する。qpdf の `getTrailer()`
と同じ trivial 基準（下記参照）で `pdf.rs` へ移すのが妥当だが、
`flpdf-0b12` の実装範囲には含まれていない。**以前の版では
`adobe_extension_level`/
`trailer_handle`/`trailer_key_handle` を「内部で resolve()/lift() を呼ぶ
から」という理由で `resolve.rs` へ再分類する提案をしていたが、実装は
この3つも他5メソッドと同じ「direct document-state accessor」グループとして
`pdf.rs` にまとめて置いた**（呼び出し先の `lift`/`lift_to_handle_bounded`
自体は reader.rs に残したまま可視性だけ広げているので、物理的な移動は
発生していない）。本設計もこれに合わせて `resolve.rs` の対象リストから
この3メソッドを外す。

**`pub(crate)` 化により、`engine.rs`/将来の `resolve.rs`/`obj_cache.rs`/
`security/*`/`object_copy.rs` はどれも lib.rs 直下の並列モジュールの
ままでよい**。フィールド・helper の可視性だけ `pub(crate)` に広げれば、
モジュール階層をどう組んでも private 境界の問題は起きない。

**既存 `crates/flpdf/src/engine.rs`（PR #657 でマージ済み）への実際の
変更**（`flpdf-0b12` で確認済み）: `use crate::reader::Pdf;` →
`use crate::Pdf;` の1行のみ。ファイルの再配置は発生しない。

**lib.rs の配線変更**（`flpdf-0b12` で実施済み）: `pub mod pdf;` を追加、
`pub use pdf::Pdf;` を追加、既存の
`pub use reader::{EncryptionInfo, Pdf, PdfOpenOptions, Permissions};` から
`Pdf` を除去（`EncryptionInfo`/`PdfOpenOptions`/`Permissions` は
reader.rs に残ったまま）。

**`security/standard.rs`/`encrypt_setup.rs`/`permissions.rs`/
`object_copy.rs` も同じ解決法をそのまま使える**: `is_encrypted`
（`self.encryption.borrow()`。フィールド定義は `pdf.rs:154`、メソッド本体は
`reader.rs:558-560`）や `take_foreign_object_map`（`self.foreign_object_maps`。
フィールド定義は `pdf.rs:81`、メソッド本体は `reader.rs:1707-`）のような
エントリポイントを移す際は、`flpdf-0b12`
と同じ手法（該当フィールド／呼び出し先ヘルパーを `pub(crate)` にする）を
適用する。`is_dirty`/`live_object_refs` 等が返す値の型は既に外部公開
されているため、これは新規の公開面拡大ではない。

### `pdf.rs`（実装済み — `flpdf-0b12`）
- `struct Pdf<R>` 定義（全フィールド `pub(crate)`）、`Drop` impl
- `version`, `trailer`, `trailer_handle`, `trailer_key_handle`, `root_ref`,
  `adobe_extension_level`, `ever_called_get_all_pages`,
  `mark_get_all_pages_called`（direct document-state accessor として
  1グループにまとめる。`adobe_extension_level` が内部で `resolve()`/
  `lift()` を呼ぶ点は上記モジュール階層節を参照）
- **未移動（今後の対象）**: `unique_id()` アクセサ（`reader.rs:1703`、
  `self.unique_id` を返すのみ）は `flpdf-0b12` の移動対象に含まれて
  おらず、まだ reader.rs に残っている。**`obj_cache.rs` には含めない**
  （後述）: 消費者（`filespec_helper.rs:115`/`embedded_files.rs:492`/
  `object_copy.rs:126`）はいずれも document identity の照合（handle の
  所属確認・foreign-copy state のキー）に使っており、object cache の
  CRUD ではない。`getTrailer()` と同じ trivial 基準（上記モジュール
  階層節）により、移動する際は `pdf.rs` の直接アクセサ群に加える
- 「reader.rs にあるのがおかしい」の根本原因（crate全体で使う中心型が
  narrow-purpose に見えるファイル名の下にある）をここで解消

### `engine.rs`（PR #657 で既に存在、拡張対象。orchestration 層）
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
- **`decrypt_resolved_object` の private 依存閉包もここに含める**:
  `decrypt_object_strings`(`reader.rs:3236-`), `decrypt_stream_bytes`
  (`reader.rs:3508-`), `apply_explicit_crypt_filters`(`reader.rs:3527-`),
  `stream_has_explicit_crypt_filter`(`reader.rs:3660-`),
  `is_metadata_stream`(`reader.rs:3671-`),
  `warn_unknown_crypt_filters`（`reader.rs:2923-`、`&self` メソッド）。
  含めないと resolve 時復号ロジックの本体が reader.rs に残る
- **`source_xref_offsets`, `source_xref_entries`, `source_header_offset`,
  `previous_xref_offset`, `last_xref_form`, `compressed_parent` もここに
  含める**（後述の「未決定」から格上げ）。これらは qtest 専用ではなく
  production consumer が存在する: `writer.rs:1004-1005,1027,1151,1469`
  （xref stream 生成時の source offset 参照）と
  `subset_prune.rs:196`（`compressed_parent`）。ソース document の
  xref 由来構造情報を返す resolve/seek 隣接の状態なので、`resolve.rs`
  が正しい置き場所
- **`source_bytes`, `lift_object_to_handle`, `source_stream_data_offset`
  もここに含める**。`source_bytes`（`reader.rs:1380-1382`、
  `self.resolver.read_physical_input()` に委譲）の production caller は
  `writer.rs:983`。`lift_object_to_handle`（`reader.rs:1056-1058`、
  `self.lift_to_handle_bounded` へ委譲 — 上記の `lift*` 系そのもの）の
  production caller は `embedded_files.rs`/`form_field_object_helper.rs`/
  `json_inspect.rs`。`source_stream_data_offset`（`reader.rs:1093-`、
  `self.resolver.xref_entry(...)` を読む）は `pub fn`（クレート外公開API）
  で、resolve/seek の一次プリミティブに直接依存する
- `adobe_extension_level`/`trailer_handle`/`trailer_key_handle` は
  ここに**含めない**（`pdf.rs` 参照）。qpdf の `QPDF::getExtensionLevel()`
  （`QPDF.cc:2329-2346`）は多段の間接参照 chain walk を行う実処理で
  `getTrailer()` のような単純な field 返却ではないが、`flpdf-0b12` の
  実装は「direct document-state accessor」という括りでこの3つを他の
  trivial アクセサと同じ `pdf.rs` にまとめて置いた（呼び出し先の
  `lift`/`lift_to_handle_bounded` 自体は `pub(crate)` 化のみで
  reader.rs に残る）。本設計もこれに合わせる
- 既存 `reader/resolver.rs` の `ResolverCore<R>`（`pub(crate)`,
  `object_cache: BTreeMap<ObjectRef, ObjectHandle>` 等）が既にこの領域の
  一部を担っているため、実装時に統合対象を精査する

### `obj_cache.rs`（新規）
- object cache への直接操作（qpdf `Members::obj_cache` フィールドに対応、
  resolve を経由しない document 自身の CRUD）: `set_object`, `delete_object`,
  `is_dirty`, `dirty_object_refs`, `clear_dirty`, `object_refs`,
  `live_object_refs`, `is_canonical_object_handle`,
  `next_available_object_ref`, `object_number_is_available`,
  `mark_object_dirty`, `mark_object_handle_mutated`,
  `mark_object_handle_dirty`, `make_indirect_object_handle`,
  `get_all_object_handles`, `resolved_count`, `deleted_object_refs`
  （`resolved_count`/`deleted_object_refs` は `self.cache` への直接委譲、
  `reader.rs:1386-1392` で確認済み）
- **`get_object_handle` も engine.rs からここへ再分類**。自身の doc
  コメント（`reader.rs:1575-1576`）が「This does not perform file I/O or
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
  encrypted`（`reader.rs:782-`）は reader.rs 内で private 定義されている
  `EncryptionState`(`reader.rs:54-`)/`EncryptionMode`(`reader.rs:327-`)/
  `required_revision`/`required_version`/`required_permissions`
  (`reader.rs:3883-3957` 付近)/`interpret_cf`(`reader.rs:3980-`)/
  `crypt_filter_modes`(`reader.rs:4010-`) に依存する。**さらに**
  `decode_hex_file_key`/`standard_handler_inputs`/
  `standard_handler_r5_inputs`/`map_uo_length_to_bad_password`/
  `encrypt_metadata_flag`/`r6_perms_warning`（`reader.rs:782-` 付近が
  これらを呼ぶ）/`first_file_id`（`reader.rs:4110-`）も同じ依存閉包に
  含まれる（全て `reader.rs:3750-4149` 付近に private 定義）。**さらに**
  `standard_handler_inputs`/`standard_handler_r5_inputs` 自体が呼ぶ
  `required_integer`/`required_name`/`required_32_byte_string`/
  `required_48_byte_string`/`crypt_filter_method`（`reader.rs:4039-4149`
  付近）と、`interpret_cf` が呼ぶ `interpret_cf_name`
  （`reader.rs:3955-`）も同じ依存閉包に含まれる。これらの
  ヘルパー型・関数も同じ移動対象に含める（`docs/qpdf-correspondence.md:136` が
  `interpret_cf` 系を既に `QPDF::interpretCF`（`QPDF_encryption.cc:700-716`）
  対応として記録済み）。含めないと暗号ロジックの大半が reader.rs に
  残ったまま、10個の薄いラッパーだけを移動することになる
- `signatures`（`reader.rs:708-710`、`crate::signatures::signatures` への
  薄い委譲）は上記グループに**含めない**。qpdf の `QPDF.cc` に signature
  関連コードは0件、`QPDFAcroFormDocumentHelper.cc` にあるのも
  `disableDigitalSignatures()`（削除のみ）で検査/読み取り API は無い
  （`docs/qpdf-correspondence.md:367` の記載も同じ）。qpdf に対応物のない
  flpdf 独自機能なので、既存 `signatures.rs` へ移す
- `take_foreign_object_map`/`set_foreign_object_map`（`reader.rs:1707-1725`
  付近）は `obj_cache.rs` に**含めない**。qpdf `Members::obj_cache`
  （`QPDF.hh:1467`）とは別フィールドの `Members::object_copiers`
  （`QPDF.hh:1476`、`ObjCopier::object_map`）に対応し、production caller は
  `object_copy.rs` の `copyForeignObject` 実装のみ（grep で確認済み）。
  既存 `object_copy.rs` へ移す
- `linearized_hint_ref`（`reader.rs:1541-1575`、コメントが
  `QPDF_linearization.cc:139-141` を明記）は既存 `linearization/` へ移す
- JSON 出力準備群（`QpdfPreparedObjects`(`reader.rs:43-`),
  `prepare_qpdf_json_objects`(`reader.rs:1459-`),
  `qpdf_json_live_object_refs`(`reader.rs:1519-`),
  `resolve_qpdf_json_object`(`reader.rs:2669-`),
  `resolve_qpdf_json_object_borrowed`(`reader.rs:2702-`)）は
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

1. bd issue（epic + 残りのサブタスク）を本設計に基づいて作成する。
   `pdf.rs` 抽出（`struct Pdf<R>` + 8メソッド + `Drop`）は
   `flpdf-0b12`（`refactor/flpdf-0b12-pdf-module`、PR #658 スタック）で
   **実装済み** — フィールドを `pub(crate)` にするだけで sibling モジュール
   のまま解決した（モジュール階層節参照）。**`unique_id()` アクセサは
   この実装済み範囲に含まれない**（reader.rs に残ったまま、上記
   `pdf.rs` 節参照）。残りは `unique_id()` アクセサの `pdf.rs` への移動、
   `obj_cache.rs`/`resolve.rs`/`engine.rs` 抽出と、暗号/認証エントリ・
   `take_foreign_object_map`・JSON準備群・`linearized_hint_ref` の
   既存ファイルへの移動
2. 実装順序: `pdf.rs` の前例（`pub(crate)` フィールド化）と同じ手法を
   横展開すればよく、特別な遷移的ネストは不要（モジュール階層節参照）。
   `obj_cache.rs`/`resolve.rs` は新規の並列モジュールとしてそのまま
   追加できる。**`engine.rs` は新規ではなく PR #657 で既に存在する
   既存モジュール**（`Pdf::empty()` のみ実装済み）なので、「追加」では
   なく「拡張」する（`open`/`open_with_repair_mode` 等をこのファイルに
   足していく）。`Pdf` 側で必要なフィールド/helper は既に `pub(crate)`
   になっている前例に倣って広げる。暗号/認証エントリの `security/*` への
   移動、`take_foreign_object_map` 等の `object_copy.rs` への移動も
   同じ手法（該当フィールド・呼び出し先ヘルパーを `pub(crate)` にする）
   でよい
3. `docs/qpdf-correspondence.md` の `QPDF.cc` 行（§1、対応表側の既存
   記載時点での行数 `reader.rs`(7898) — 実際の現状行数（8475、上記
   「動機」参照）とは別の、対応表が最後に更新された時点のスナップショット
   ） + `reader/resolver.rs`(...) + `reader/file_object.rs`
   (1405) + `xref.rs`(1220) + `object_copy.rs`(342: `copyForeignObject`) +
   `cache.rs`(112) + `writer/object_streams.rs`(207-237) +
   `signatures.rs`(245-: `removeSecurityRestrictions`) +
   `page_closure.rs`(441) + `ref_chain.rs`(159)）を更新する際は、
   **`reader/resolver.rs` は本設計の4ファイルへ差し替えるが、
   `reader.rs` は残す（削除しない）**。`reader/resolver.rs` を差し替える
   際は、`QPDF.cc` 行以外にも `reader/resolver.rs` を名指ししている
   行がある（`docs/qpdf-correspondence.md:135` `QPDF.hh`
   `EncryptionParameters`、`:136` `QPDF::interpretCF`、`:137`
   `QPDF::decryptStream`、`:140` `InputSource` 系、`:199` `Pl_AES_PDF`、
   `:200` `Pl_RC4`）ので、これら6行も併せて `resolve.rs` を指すよう
   更新する（`QPDF.cc` 行だけ直して他を放置すると存在しないファイルを
   指す行が残る）。`reader.rs` には legacy な
   `Pdf::resolve`/`Pdf::resolve_borrowed`（`reader.rs:2428-2451`、
   doc コメントで `qpdf-cutover-delete(flpdf-25kg.3.3)` と明記）が
   残っており、production caller が実在する（`object_copy.rs:108,152`、
   `flpdf-cli/src/main.rs:3444` 等）。この2メソッドの削除は
   `flpdf-25kg.3.3`（呼び出し側の `ObjectHandle` 移行）が前提で、
   本設計はそれを前提にしない・実施もしないため、`reader.rs` は
   本設計の4ファイル抽出後も実体を持つモジュールとして correspondence
   行に残す。`reader/file_object.rs`/`xref.rs`/`object_copy.rs`/
   `cache.rs`/`writer/object_streams.rs`/`signatures.rs`/
   `page_closure.rs`/`ref_chain.rs` も本設計が触れない別責務なので、
   既存の記載をそのまま残す（`docs/qpdf-correspondence.md:372-373` が
   `object_copy.rs` を `QPDF.cc` 行にあえて置いている理由を明記しており、
   同じ理由で他の7ファイルも残す必要がある。`QPDF_encryption.cc`/
   `QPDF_json.cc`/`QPDF_linearization.cc`/`QPDF_optimization.cc`/
   `QPDF_pages.cc` はこれとは別に既に独立行を持つため、そちらとの
   二重帰属だけを避ける）。上記「新規ファイルを作らず既存へ委譲する
   もの」で個別に触れた `signatures.rs`/`object_copy.rs`/
   `linearization/`/`document_json.rs` への移動は、それぞれの既存行
   （§7/§8）を個別に更新する
4. 各ステップは出力バイトに影響しない「入れ物」の変更のみ
   （CLAUDE.md 分類(B)）なので、バイト差分ゼロを都度確認しながら進める。
   AGENTS.md §7 の acceptance-criteria 階層として、各ステップの検証には
   具体的なコマンドを紐付ける: `cargo test -p flpdf`（lib 全件 + 全
   integration + doctest）に加え、`--features qpdf-zlib-compat` での
   byte-identical テスト群（`cmp_linearize_tests.rs` の
   `assert_*_byte_identical`、`crates/flpdf-cli/tests/cli_byte_identical.rs`、
   `deterministic_id_qpdf_parity_tests.rs`）を移動対象メソッドが実際に
   通る経路であることを確認したうえで実行する。**`compat_matrix_tests.rs`
   はこのリストに含めない**: `qpdf-zlib-compat` gate が無く、byte 比較も
   `qdf_object_body`（部分文字列抽出）のみで、他のケースはページ数・
   parseability・xref 形式・prefix 保持等の性質チェックであり、全体
   byte-identity は検証しない（実際に読んで確認済み）。補助的な
   compatibility カバレッジとして扱い、byte-identity ゲートとしては
   数えない。
   特に resolve/認証まわりの移動は、単体テストだけでは値の materialize
   有無や byte 出力まで確認できないため、上記 byte-identical スイートを
   必ず含める。**`authenticate_if_encrypted`/暗号ヘルパーの移動には
   上記4本では不十分**（いずれも暗号化フィクスチャ・パスワード・
   `PdfOpenOptions` を使わないため認証経路を通らない）。
   `crates/flpdf-cli/tests/encrypt_cli_tests.rs` も確認したが、
   `encrypted_document_is_byte_identical_to_qpdf`（:1154-1199）は平文
   入力を暗号化して**書く**経路のみを byte 比較しており、暗号化された
   入力を**開いて**認証する経路は通らない。`copy_encryption_from_
   decrypts_with_donor_user_password_via_qpdf`（:1416-）はドナーの
   暗号化 PDF をパスワード付きで開く（`authenticate_if_encrypted` を
   通る）が、アサーションは `qpdf --show-encryption` の構造チェックで
   byte 比較ではない。**探索した範囲では、flpdf 側が暗号化入力を認証し
   つつ byte-identical 比較まで行う既存テストは無い**。したがって
   `authenticate_if_encrypted` 系の移動には、既存テストを流用するのでは
   なく新規の encrypted-input rewrite golden（パスワード付きで開く →
   何らかの操作 → 書き出し → qpdf の同等操作と byte 比較）を実装の一部
   として追加することを前提条件とする。加えて各ステップ実行前後で
   `scripts/patch-coverage.sh` を回し、変更行 100% カバレッジを維持する。
   **各ステップは `flpdf-0b12` のように前段のステップに stack する PR に
   なる**ため、`patch-coverage.sh` は必ず `--base <親ブランチ>` 付きで
   実行する（引数無しだと既定の `origin/main` と比較してしまい、前段の
   ステップで既に入った変更まで当該ステップの変更行として扱われて
   誤判定する）
5. **各抽出は CLAUDE.md 逸脱分類 (B) に該当する**（qpdf 自身の
   `QPDF.cc` 単一ファイルという足場を、flpdf 側で複数の Rust モジュールに
   置き換える「入れ物」の変更）。CLAUDE.md 条件3（該当箇所参照）により、
   `docs/qpdf-correspondence.md` の更新（上記ステップ3）に加えて
   **各新規/拡張モジュール（`pdf.rs`/`engine.rs`/`resolve.rs`/
   `obj_cache.rs`）自身にも逸脱理由を1行記録することを受け入れ基準に
   含める**（`crates/flpdf/src/engine.rs` の `Pdf::empty()` 移動時に
   既にこの形式で記録済み — モジュール内コメント + 対応表 ⚪ 行の
   両方、が前例）。correspondence table の更新だけでは条件3を満たさない
