# flpdf-9hc.22.8 — Signed-PDF preservation matrix tests

## ゴール
署名済みPDFの保全挙動を table-driven の preservation matrix で系統的に検証する。
戦略A（合成フィクスチャ + 外部検証手順の文書化）を採用。暗号検証は行わず、
構造的不変条件（byte-range の byte-identity 保持・検出件数/メタデータ・拒否/strip 挙動）
を全セルで検証する。

## マトリクス
フィクスチャ変種（行）×操作（列）。

変種:
- `single_detached` — 1署名 / SubFilter `adbe.pkcs7.detached`
- `single_sha1` — 1署名 / SubFilter `adbe.pkcs7.sha1`
- `multi_sig` — 2署名フィールド（detached + sha1）

操作×期待:
1. detection: `signatures()` が件数・field_name・byte_range・sub_filter を期待通り返す
2. full-rewrite 既定: `write_pdf_with_options(full_rewrite)` が `Error::Signed` で拒否
3. --remove-restrictions 相当: `clear_sig_flags` + `strip_signature_values` が成功し、
   以後 `signatures()` が空・SigFlags の SignaturesExist がクリア
4. incremental（非署名オブジェクト変更）: byte-range byte-identity 保持・`out.starts_with(input)`・書き込み成功
5. incremental（AcroForm 変更）: `invalidates_signatures=true` / 理由 `IncrementalTouchesAcroForm`
6. incremental（署名フィールド変更）: `invalidates_signatures=true` / 理由 `IncrementalTouchesSignedObject`

## 実装
- 新規: `crates/flpdf/tests/signed_preservation_matrix.rs`
  - `build_pdf` / `open` ヘルパーを本ファイル内に複製（integration test は各々別クレート）
  - `Variant` 記述子（bytes・期待検出値・acroform_ref・page_ref・sig_field_refs）と `variants()`
  - 各操作を `#[test]` として全変種でループ（データ駆動）。失敗時は変種名を含むメッセージ。
- byte_range はダミー値だが各変種内で文書長に収まるよう設定（preservation 比較が有効）。
- multi-sig は `/AcroForm /Fields [.. ..]` に2つの widget+sig dict ペア、署名ごとに異なる byte_range。

## 外部検証ツールの文書化
- `docs/` に将来CI拡張手順を追記（または専用md）:
  - 実署名フィクスチャ生成は pyhanko/endesive 等が必要（qpdf/pdfsig は検証専用・署名作成不可）
  - pdfsig 検証コマンド例と期待出力
  - 証明書有効期限による validity 検証の経年劣化リスクと、構造検証を主軸にする根拠
  - CI 組み込み時の前提（フィクスチャ追加・ツールインストール）

## 検証
- `cargo test -p flpdf --test signed_preservation_matrix`
- `cargo test`（全体回帰）/ `cargo clippy` / `cargo fmt --check`

## スコープ外
- 実暗号署名の生成・検証（将来CI拡張として文書化のみ）
- 新規 public API の追加（既存 API のカバレッジ拡充に限定）
