# QPDFAnnotationObjectHelper 境界設計

## 目的

`flpdf-9ng9` として、qpdf 11.9.0 の
`QPDFAnnotationObjectHelper` を `flpdf` の単一責務モジュールとして再配置する。
現在の raw `Object` / `Pdf::resolve_borrowed` ベースの
`AnnotationObjectHelper` と `page_annotation_enum` の重複した annotation
読取り経路を、ObjectHandle-native API に置換する。

## Oracle と責務境界

oracle は固定済み qpdf 11.9.0 の
`include/qpdf/QPDFAnnotationObjectHelper.hh` および
`libqpdf/QPDFAnnotationObjectHelper.cc` である。

- `QPDFAnnotationObjectHelper` が所有する annotation object の読取りは
  `annotation_helper.rs` に集約する。
- `QPDFFormFieldObjectHelper` の継承フィールド属性は既存の Tier A1
  `FormFieldObjectHelper` に残す。
- ページの `/Annots` 配列を列挙する責務は
  `QPDFPageObjectHelper` 側に残す。annotation helper は与えられた
  ObjectHandle の `/Subtype`、`/Rect`、`/AP`、`/F` を扱う。
- `page_annotation_enum.rs` の widget-to-field 分類はこの helper が返す
  ObjectHandle API を使う consumer 側へ移し、旧 module を削除する。

## 公開 API

`AnnotationObjectHelper` は `ObjectHandle` を値として保持し、qpdf の公開面を
snake_case で提供する。`ObjectRef + &mut Pdf` を受け取る constructor、raw
`Dictionary` / `Object` を返す accessor、compatibility wrapper は残さない。

最低限、以下を提供する。

- `new(ObjectHandle)`
- `object_handle(&self) -> &ObjectHandle`
- `get_rect(&self) -> Option<PageBox>`
- `get_appearance_dictionary(&self) -> ObjectHandle`
- `get_flags(&self) -> i64`
- `get_appearance_stream(&self, which: &[u8], state: Option<&[u8]>) -> ObjectHandle`

qpdf の null-object 規約に合わせ、欠落・null・型不一致の optional read は null
ObjectHandle 又は既定値に正規化する。壊れた間接参照のエラーは ObjectHandle の
解決経路からそのまま返す。`get_appearance_stream` は `/AP/which` が stream の場合
その stream を、state dictionary の場合は明示 state 又は `/AS` を使って選ぶ。

## 移行

1. qpdf の API ごとの integration test を先に追加し、現行 API に存在しない
   ObjectHandle constructor/accessor をコンパイル失敗として確認する。
2. `annotation_helper.rs` を ObjectHandle-only に置換する。Form field helper は
   Tier A1 の module に移して依存を一方向にする。
3. `page_annotation_enum.rs` の page traversal / widget linkage を消費側へ移し、
   public re-export を削除する。旧 module の実装は削除する。
4. `rg` で対象 helper と移行した consumer に `Object::`、`resolve_borrowed`、
   raw `Pdf` constructor が残っていないことを確認する。

## テストと完了条件

- `get_rect` は absent、numeric array、malformed array を検証する。
- appearance dictionary/stream は direct・indirect、単一 stream、state dictionary、
  absent/null を検証する。
- flags は absent・integer・non-integer の qpdf 既定値を検証する。
- fixture ベースの page annotation consumer は既存の順序・widget classification を
  保持する。
- pinned qpdf 11.9.0 に対する最小 probe と focused Rust integration test を記録する。
- `cargo fmt -- --check`、対象 tests、workspace clippy、changed-line coverage を通す。
