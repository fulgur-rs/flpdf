# PdfVersion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** qpdf 11.9.0 の `PDFVersion` を `PdfVersion` として全域移植し、flpdf 内の PDF バージョン値表現をこの型へ一本化する。

**Architecture:** `pdf_version.rs` は比較・更新・文字列化だけを担う依存ゼロの値型とし、writer の最小バージョン、暗号方式、object/xref stream、linearization、Adobe extension の各ポリシーは既存の場所に残す。既存の公開関数 `parse_pdf_version` は同モジュールへ移し、戻り値を `Option<PdfVersion>` にして、writer、plain plan、overlay、CLI の生タプルを段階的に置換する。

**Tech Stack:** Rust 2021 workspace、qpdf 11.9.0 `include/qpdf/PDFVersion.hh` / `libqpdf/PDFVersion.cc`、Cargo tests、cargo-llvm-cov

## Global Constraints

- qpdf 11.9.0 の `PDFVersion.hh` にある公開メンバを例外なく移植する。
- 値型以外の writer ポリシーは `writer.rs` と `writer/plain/plan.rs` に残す。
- `pdf_version.rs` の先頭を `//! Mirrors qpdf 11.9.0 libqpdf/PDFVersion.cc.` とし、ヘッダとの対応も次行に記録する。
- `(u8, u8)` を PDF バージョンの意味で使う実装を `writer.rs`、`writer/plain/plan.rs`、`overlay.rs`、`flpdf-cli/src/main.rs` に残さない。
- qpdf byte baselineを変えず、最終コミットの patch coverage を100%にする。

---

### Task 1: qpdf-compatible `PdfVersion` value type

**Files:**
- Create: `crates/flpdf/src/pdf_version.rs`
- Create: `crates/flpdf/tests/pdf_version_tests.rs`
- Modify: `crates/flpdf/src/lib.rs`

**Interfaces:**
- Consumes: qpdf `PDFVersion(int major, int minor, int extension = 0)` と公開メンバ全て
- Produces: `PdfVersion::new(u8, u8, i64)`, `PdfVersion::parse(&str)`, `update_if_greater`, `get_version`, `major`, `minor`, `extension_level`

- [ ] **Step 1: 公開APIの失敗テストを書く**

```rust
use flpdf::PdfVersion;

#[test]
fn exposes_the_complete_qpdf_pdfversion_value_api() {
    let mut version = PdfVersion::default();
    assert_eq!(version.get_version(), ("0.0".to_string(), 0));

    version.update_if_greater(PdfVersion::new(1, 7, 3));
    assert_eq!(version.major(), 1);
    assert_eq!(version.minor(), 7);
    assert_eq!(version.extension_level(), 3);
    assert_eq!(version.get_version(), ("1.7".to_string(), 3));

    version.update_if_greater(PdfVersion::new(1, 7, 2));
    assert_eq!(version, PdfVersion::new(1, 7, 3));
    assert!(PdfVersion::new(1, 7, 2) < PdfVersion::new(1, 7, 3));
    assert!(PdfVersion::new(1, 6, 99) < PdfVersion::new(1, 7, 0));
}

#[test]
fn parses_only_existing_flpdf_major_minor_syntax() {
    assert_eq!(PdfVersion::parse("1.7"), Some(PdfVersion::new(1, 7, 0)));
    assert_eq!(PdfVersion::parse("1.10"), Some(PdfVersion::new(1, 10, 0)));
    assert_eq!(PdfVersion::parse("invalid"), None);
    assert_eq!(PdfVersion::parse("1.7.3"), None);
    assert_eq!(PdfVersion::parse("256.0"), None);
}
```

- [ ] **Step 2: 未定義APIにより失敗することを確認する**

Run: `cargo test -p flpdf --test pdf_version_tests`

Expected: FAIL with unresolved import `flpdf::PdfVersion`.

- [ ] **Step 3: 値型と公開exportを最小実装する**

```rust
//! Mirrors qpdf 11.9.0 libqpdf/PDFVersion.cc.
//! Public API: qpdf 11.9.0 include/qpdf/PDFVersion.hh.

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct PdfVersion {
    major: u8,
    minor: u8,
    extension_level: i64,
}

impl PdfVersion {
    pub const fn new(major: u8, minor: u8, extension_level: i64) -> Self {
        Self { major, minor, extension_level }
    }

    pub fn parse(value: &str) -> Option<Self> {
        let (major, minor) = value.split_once('.')?;
        Some(Self::new(major.parse().ok()?, minor.parse().ok()?, 0))
    }

    pub fn update_if_greater(&mut self, other: Self) {
        if *self < other {
            *self = other;
        }
    }

    pub fn get_version(self) -> (String, i64) {
        (format!("{}.{}", self.major, self.minor), self.extension_level)
    }

    pub const fn major(self) -> u8 { self.major }
    pub const fn minor(self) -> u8 { self.minor }
    pub const fn extension_level(self) -> i64 { self.extension_level }
}

```

`lib.rs` に `pub mod pdf_version;` と `pub use pdf_version::PdfVersion;` を追加する。既存の `writer::parse_pdf_version` はTask 2まで維持し、このTaskを単独でcompile可能にする。

- [ ] **Step 4: 値型テストを通す**

Run: `cargo test -p flpdf --test pdf_version_tests`

Expected: PASS, 2 tests.

- [ ] **Step 5: commitする**

```bash
git add crates/flpdf/src/pdf_version.rs crates/flpdf/src/lib.rs crates/flpdf/tests/pdf_version_tests.rs
git commit -m "feat(pdf-version): add qpdf-compatible value type"
```

### Task 2: writerと全consumerのバージョン表現を `PdfVersion` に統一

**Files:**
- Modify: `crates/flpdf/src/pdf_version.rs`
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/writer/plain/plan.rs`
- Modify: `crates/flpdf/src/overlay.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf-cli/tests/cli_linearize.rs`

**Interfaces:**
- Consumes: Task 1 の `PdfVersion`
- Produces: `parse_pdf_version(&str) -> Option<PdfVersion>`、`PdfVersion::static_version_str() -> Option<&'static str>`、既存の `effective_pdf_version` / `effective_pdf_version_and_ext` の外部シグネチャと出力を維持したwriterと全consumer

- [ ] **Step 1: 公開parserの戻り値と標準版文字列の失敗テストを追加する**

`pdf_version_tests.rs` に `parse_pdf_version` が `PdfVersion` を返す期待値を追加し、`pdf_version.rs` のunit testsに以下を追加する。

```rust
#[test]
fn public_parser_returns_the_value_type() {
    assert_eq!(
        flpdf::parse_pdf_version("1.7"),
        Some(PdfVersion::new(1, 7, 0))
    );
}

#[test]
fn standard_version_strings_cover_writer_encryption_floors() {
    assert_eq!(PdfVersion::new(1, 3, 0).static_version_str(), Some("1.3"));
    assert_eq!(PdfVersion::new(1, 4, 0).static_version_str(), Some("1.4"));
    assert_eq!(PdfVersion::new(1, 5, 0).static_version_str(), Some("1.5"));
    assert_eq!(PdfVersion::new(1, 6, 0).static_version_str(), Some("1.6"));
    assert_eq!(PdfVersion::new(1, 7, 0).static_version_str(), Some("1.7"));
    assert_eq!(PdfVersion::new(2, 0, 0).static_version_str(), None);
}
```

- [ ] **Step 2: helper未実装で失敗することを確認する**

Run: `cargo test -p flpdf standard_version_strings_cover_writer_encryption_floors`

Expected: FAIL because `static_version_str` is not defined.

- [ ] **Step 3: 変更前のD2重複箇所を記録する**

Run:

```bash
rg -n '\(u8, u8\)|< \(1, 5\)|unwrap_or\(\(1, 0\)\)' \
  crates/flpdf/src/writer.rs \
  crates/flpdf/src/writer/plain/plan.rs \
  crates/flpdf/src/overlay.rs \
  crates/flpdf-cli/src/main.rs
```

Expected: writer、plain plan、overlay、CLIの既存tuple箇所を列挙する。

- [ ] **Step 4: writerと全consumerを一度に値型へ移行する**

`PdfVersion::static_version_str` を `pub(crate)` で実装する。`writer.rs` では次を行う。

```rust
use crate::pdf_version::{parse_pdf_version, PdfVersion};

const PDF_1_2: PdfVersion = PdfVersion::new(1, 2, 0);
const PDF_1_5: PdfVersion = PdfVersion::new(1, 5, 0);
```

- `pdf_version.rs` に `pub fn parse_pdf_version(&str) -> Option<PdfVersion>` を追加する。
- `writer.rs` 内の旧 `parse_pdf_version` と `static_version_string` を削除する。
- `force_version_below_1_5`、`effective_pdf_version`、`effective_pdf_version_and_ext` の比較値を `PdfVersion` にする。
- `encryption_version_floor` を `Option<PdfVersion>` にし、extension levelも値型へ格納する。
- `effective_pdf_version` の戻り値 `&str` は維持し、暗号floor文字列だけ `static_version_str().unwrap_or("1.7")` で選択する。
- `effective_pdf_version_and_ext` は `PdfVersion::extension_level()` で暗号floorのextensionを読む。
- `writer/plain/plan.rs` のxref stream floorとvalidateを `PdfVersion` にする。
- `overlay.rs::accumulate_max` の戻り値を `(PdfVersion, i64)` にする。
- CLIのoverlay/underlay version accumulator 2経路を `PdfVersion` にする。
- major/minor文字列の生成は `max_ver.get_version().0` を用いる。
- `lib.rs` のwriter re-exportから `parse_pdf_version` を削除し、pdf_version moduleからre-exportする。
- `cli_linearize.rs` のtuple期待値を `PdfVersion::new` へ置換する。

- [ ] **Step 5: focused testsを通す**

Run: `cargo test -p flpdf --test pdf_version_tests`

Expected: PASS.

Run: `cargo test -p flpdf writer::plain::plan::tests`

Expected: PASS.

Run: `cargo test -p flpdf overlay`

Expected: PASS.

Run: `cargo test -p flpdf-cli --test cli_linearize parse_pdf_version`

Expected: PASS.

Run: `cargo test -p flpdf-cli --test cli_tests`

Expected: PASS.

- [ ] **Step 6: D2 grep gateを通す**

Task 2 Step 3と同じ `rg` を実行する。

Expected: PDFバージョンの意味を持つtupleは0件。無関係なtupleが見つかった場合は、用途を確認して対象外であることを記録する。

- [ ] **Step 7: commitする**

```bash
git add crates/flpdf/src/pdf_version.rs crates/flpdf/src/writer.rs crates/flpdf/src/writer/plain/plan.rs crates/flpdf/src/overlay.rs crates/flpdf/src/lib.rs crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_linearize.rs
git commit -m "refactor: route PDF versions through PdfVersion"
```

### Task 3: qpdf parityと品質ゲート

**Files:**
- Modify if required by formatting/lints: Task 1-2の変更ファイルのみ

**Interfaces:**
- Consumes: Task 1-2の完成実装
- Produces: D1-D5の検証証拠

- [ ] **Step 1: qpdf公開APIとの全域対応を再確認する**

Run:

```bash
sed -n '1,220p' "$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/PDFVersion.hh"
sed -n '1,220p' "$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/PDFVersion.cc"
```

`origin/main` には取得スクリプト更新が未マージなので、スクリプトが古い場合は既存の `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` を同じpin済みoracleとして使う。constructor/default、copy相当のderive、`<`、`==`、update、version+extension出力、3 getterが全てテスト済みであることを確認する。

- [ ] **Step 2: format・clippy・workspace testsを通す**

Run: `cargo fmt --all -- --check`

Expected: PASS.

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

Run: `cargo test`

Expected: PASS with only pre-existing ignored tests.

- [ ] **Step 3: byte-sensitive focused gatesを通す**

Run: `cargo test -p flpdf --test writer_tests`

Run: `cargo test -p flpdf-cli --test compat_matrix_tests`

Expected: PASS; compat matrixはqpdfが利用できない環境では既存仕様によりskip.

- [ ] **Step 4: 最終HEADのpatch coverageを100%にする**

Run:

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path /tmp/flpdf-qxba-4.lcov
scripts/patch-coverage.sh --base origin/main --lcov /tmp/flpdf-qxba-4.lcov
```

Expected: changed lines 100%. 未到達行があれば最小テストを追加し再実行する。

- [ ] **Step 5: issue完了とpush**

`bd close flpdf-qxba.4` は全ゲート通過後に実行する。続けて `bd dolt push`、`git pull --rebase origin feature/flpdf-qxba-4-pdf-version`、`git push -u origin feature/flpdf-qxba-4-pdf-version` を行い、remote push成功を確認する。
