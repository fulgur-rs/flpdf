# Filespec ObjectHandle Ownership Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** qpdf 11.9.0 の `createFileSpec`/`createEFStream` と `checkOwnership` に一致する Filespec factory 境界を実装する。

**Architecture:** Factory は `ObjectHandle` の直接挿入境界だけで所有権を検査し、foreign indirect handle を destination のhandle registryへ登録せず拒否する。新規 indirect object は qpdf のobject-cache契約で採番し、factory固有の圧縮処理は削除する。

**Tech Stack:** Rust workspace, `flpdf`, qpdf 11.9.0 source oracle, cargo test/llvm-cov.

## Global Constraints

- qpdf 11.9.0 source と live probe が唯一の挙動oracle。
- 既存のraw `Object` 互換経路をfactory内に追加しない。
- REDを確認してから最小実装へ進む。
- fmt、workspace all-feature clippy、関連テスト、変更行coverage 100%を通す。

---

### Task 1: Factory ownership and allocation regression tests

**Files:**
- Modify: `crates/flpdf/tests/filespec_helper_tests.rs`
- Modify: `crates/flpdf/src/filespec_helper.rs` のunit test module（必要な直接handle子孫ケース）

**Interfaces:**
- Consumes: `FileSpec::create_file_spec`, `EmbeddedFileStream::create_ef_stream`, `Pdf::get_object_handle`
- Produces: qpdf ownership境界と採番を固定する公開API回帰テスト

- [ ] **Step 1: Write failing tests**

```rust
assert!(FileSpec::create_file_spec(&mut destination, b"foreign", foreign).is_err());
assert_eq!(next_factory_ref(&mut destination), ObjectRef::new(4, 0));
```

加えて、foreign indirect handleを含むdirect dictionaryを渡しても、直接挿入値自体がdirectなら拒否しないこと、およびxref外のdangling referenceが次のfactory採番を押し上げないことをpublic factory経由で検証する。

- [ ] **Step 2: Verify RED**

Run: `cargo test -p flpdf --test filespec_helper_tests -- foreign`

Expected: qpdfと異なるregistry登録、子孫walk、またはdangling予約を示すFAIL。

- [ ] **Step 3: Implement minimal factory boundary**

`FileSpec::create_file_spec` と `EmbeddedFileStream::create_ef_stream` を、`ObjectHandle`の直接挿入とqpdf object-cache採番に合わせる。foreign handleを判定するためにcanonical lookupを副作用付きで呼ばない。

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p flpdf --test filespec_helper_tests`

Expected: PASS。

### Task 2: Remove the non-qpdf compression builder path

**Files:**
- Modify: `crates/flpdf/src/filespec_helper.rs`
- Modify: `crates/flpdf/tests/filespec_helper_tests.rs`

**Interfaces:**
- Consumes: `FileSpecBuilder::build`, `EmbeddedFileStream::create_ef_stream`
- Produces: qpdf factoryに限定されたFilespec construction API

- [ ] **Step 1: Write the failing API/behavior test**

```rust
let filespec = FileSpecBuilder::new("data.bin", b"payload".as_ref())
    .build(&mut pdf)?;
assert!(embedded_stream(&mut pdf, filespec).dict.get("Filter").is_none());
```

既存の`.compress(true)`テストを削除し、qpdf factoryがraw dataをそのままnewStreamへ渡すことを示すtestへ置換する。

- [ ] **Step 2: Verify RED**

Run: `cargo test -p flpdf --test filespec_helper_tests -- builder`

Expected: 削除対象の圧縮経路が残っているため失敗、または旧APIを参照してコンパイル失敗。

- [ ] **Step 3: Implement minimal deletion**

`FileSpecBuilder::compress`、圧縮state、`encode_stream_data` import、factory内のdecode済みpayload再圧縮を削除する。attachment helperはqpdf primitive外の圧縮要求を持たないfactory経路を使う。

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p flpdf --test filespec_helper_tests`

Expected: PASS。

### Task 3: Oracle evidence and quality gates

**Files:**
- Modify: `crates/flpdf/src/filespec_helper.rs` module documentation only if citations require correction

- [ ] **Step 1: Run source and behavior probes**

Run: `scripts/fetch-qpdf-source.sh --print-path` and use its `QPDFFileSpecObjectHelper.cc`, `QPDFEFStreamObjectHelper.cc`, `QPDFObjectHandle.cc`, `QPDF.cc` citations to verify direct insertion, ownership, and object allocation.

- [ ] **Step 2: Run focused and workspace gates**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test -p flpdf --test filespec_helper_tests && cargo test -p flpdf`

Expected: all PASS.

- [ ] **Step 3: Measure changed-line coverage**

Run: `cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path /tmp/flpdf-3yn9-2.lcov && scripts/patch-coverage.sh --base origin/main --lcov /tmp/flpdf-3yn9-2.lcov`

Expected: 100% changed-line coverage.
