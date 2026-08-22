# Filespec Helper Boundaries Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split the Filespec and EmbeddedFile qpdf helper implementations and move high-level attachment file I/O to the QPDFJob attachment boundary without changing observable behavior.

**Architecture:** `filespec_helper/mod.rs` becomes a thin module root that exposes `filespec.rs`, `embedded_file_stream.rs`, and shared low-level helpers. `job/attachments.rs` owns path-based attachment orchestration and extraction. Existing live ObjectHandle/provider paths remain the single implementation.

**Tech Stack:** Rust 2021, `ObjectHandle`, `Pdf`, qpdf 11.9.0 pinned source, qpdf JSON/live probes, Cargo tests, strict rustdoc, Clippy, llvm-cov patch coverage.

---

### Task 1: Add the RED module-boundary contract

**Files:**
- Create: `crates/flpdf/tests/filespec_helper_route_cutover_tests.rs`

- [ ] **Step 1: Write the failing contract test**

```rust
use std::fs;
use std::path::Path;

#[test]
fn filespec_helpers_have_qpdf_owner_modules_and_no_old_facade() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!root.join("src/filespec_helper.rs").exists());
    let module = fs::read_to_string(root.join("src/filespec_helper/mod.rs")).unwrap();
    assert!(module.contains("mod filespec;"));
    assert!(module.contains("mod embedded_file_stream;"));
    assert!(root.join("src/filespec_helper/filespec.rs").exists());
    assert!(root
        .join("src/filespec_helper/embedded_file_stream.rs")
        .exists());

    let filespec = fs::read_to_string(root.join("src/filespec_helper/filespec.rs")).unwrap();
    let ef_stream =
        fs::read_to_string(root.join("src/filespec_helper/embedded_file_stream.rs")).unwrap();
    let job = fs::read_to_string(root.join("src/job/attachments.rs")).unwrap();
    assert!(filespec.contains("pub struct FileSpec"));
    assert!(filespec.contains("pub struct FileSpecBuilder"));
    assert!(ef_stream.contains("pub struct EmbeddedFileStream"));
    for function in [
        "pub fn add_attachment_from_path",
        "pub fn extract_attachment",
        "pub fn write_attachment",
        "pub fn extract_attachment_to_path",
    ] {
        assert!(!module.contains(function));
        assert!(!filespec.contains(function));
        assert!(job.contains(function));
    }
}
```

- [ ] **Step 2: Run the contract and verify RED**

Run: `cargo test -p flpdf --test filespec_helper_route_cutover_tests`

Expected: FAIL because the current monolithic `src/filespec_helper.rs` still
exists and the owner modules do not yet exist.

- [ ] **Step 3: Commit the RED test**

Run: `git add crates/flpdf/tests/filespec_helper_route_cutover_tests.rs && git commit -m "test: require split filespec helper boundaries"`

### Task 2: Create the qpdf-shaped module tree

**Files:**
- Rename: `crates/flpdf/src/filespec_helper.rs` -> `crates/flpdf/src/filespec_helper/mod.rs`
- Create: `crates/flpdf/src/filespec_helper/filespec.rs`
- Create: `crates/flpdf/src/filespec_helper/embedded_file_stream.rs`
- Create: `crates/flpdf/src/filespec_helper/shared.rs`
- Modify: `crates/flpdf/src/lib.rs`

- [ ] **Step 1: Move production sections without changing code behavior**

Move the existing `EmbeddedFileStream` implementation (currently beginning at
line 136) into `embedded_file_stream.rs`; move `FileSpec` and
`FileSpecBuilder` (currently beginning at lines 602 and 1032) into
`filespec.rs`; move `qpdf_style_open_error`, `encode_utf16be`,
`format_pdf_date`, and `md5_checksum` into `shared.rs`. Keep the existing unit
tests in the module root until the production owners compile, then update their
imports explicitly.

- [ ] **Step 2: Add the thin module root**

`filespec_helper/mod.rs` must contain only the qpdf correspondence module docs,
module declarations, shared re-exports, and low-level public re-exports:

```rust
mod embedded_file_stream;
mod filespec;
mod shared;

pub use embedded_file_stream::EmbeddedFileStream;
pub use filespec::{FileParamDates, FileSpec, FileSpecBuilder};
pub use shared::{encode_utf16be, format_pdf_date, md5_checksum};
pub(crate) use shared::qpdf_style_open_error;
```

- [ ] **Step 3: Run compilation-focused tests**

Run: `cargo test -p flpdf --test filespec_helper_tests --no-default-features`

Expected: the low-level Filespec and EmbeddedFile tests compile and pass;
high-level imports may still fail until Task 3 moves their owner functions.

### Task 3: Move the high-level attachment facade to `job/attachments.rs`

**Files:**
- Modify: `crates/flpdf/src/job/attachments.rs`
- Modify: `crates/flpdf/src/job/mod.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `crates/flpdf/src/filespec_helper/mod.rs`
- Modify: `crates/flpdf/src/json/input.rs`
- Modify: `crates/flpdf/src/pdf_string.rs`
- Modify: `crates/flpdf/src/embedded_files.rs`
- Modify: `crates/flpdf/src/job/attachment_list.rs`
- Modify: `crates/flpdf/tests/filespec_helper_tests.rs`
- Modify: `crates/flpdf/tests/embedded_files_tests.rs`
- Modify: `crates/flpdf/tests/helper_api_tests.rs`

- [ ] **Step 1: Move the four file-I/O functions and fallback**

Move `add_attachment_from_path`, `ascii_filename_fallback`,
`extract_attachment`, `write_attachment`, and `extract_attachment_to_path`
into `job/attachments.rs`. Keep their bodies and error strings unchanged while
changing only the low-level imports to the new module owners.

- [ ] **Step 2: Publish the new owner and remove the old facade**

Add the moved functions to `job/mod.rs`'s public exports and re-export them at
the crate root from `job`. Remove their `filespec_helper` exports and update
all in-tree callers to `crate::job::...` or the canonical root/job import.
Keep `qpdf_style_open_error` crate-private through `filespec_helper::shared`;
it is a parser diagnostic utility, not an attachment facade.

- [ ] **Step 3: Run the existing attachment and JSON suites**

Run: `cargo test -p flpdf --test filespec_helper_tests --test embedded_files_tests --test helper_api_tests`

Expected: PASS with the same qpdf-shaped attachment bytes, names, metadata,
provider behavior, and error text.

### Task 4: Complete RED→GREEN verification and correspondence

**Files:**
- Modify: `docs/qpdf-correspondence.md`
- Modify: `docs/qpdf-module-doc-index.md` only if the module-doc checker requires regenerated entries

- [ ] **Step 1: Run the boundary contract and focused qpdf tests**

Run: `cargo test -p flpdf --test filespec_helper_route_cutover_tests --test filespec_helper_tests --test embedded_files_tests --test helper_api_tests --test cli_tests`

Expected: PASS, including the new route contract.

- [ ] **Step 2: Run the live qpdf attachment comparison**

Run:

```bash
qpdf --json=2 --json-key=attachments tests/fixtures/compat/attachment-two-page.pdf /tmp/filespec-qpdf.json
cargo run --quiet --bin flpdf -- --json=2 --json-key=attachments tests/fixtures/compat/attachment-two-page.pdf >/tmp/filespec-flpdf.json
diff -u /tmp/filespec-qpdf.json /tmp/filespec-flpdf.json
```

Expected: no diff; qpdf version must be 11.9.0.

- [ ] **Step 3: Update correspondence and run repository gates**

Record the two helper source ranges and the job ownership boundary in
`docs/qpdf-correspondence.md`, then run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
cargo test --workspace --all-features
```

- [ ] **Step 4: Run fresh patch coverage and inspect the diff**

Run: `scripts/patch-coverage.sh --base origin/main`

Expected: `flpdf ... uncovered 0 -> PASS (100%)`; then run `git diff --check`
and confirm no compatibility wrapper or duplicate high-level attachment route
remains.

- [ ] **Step 5: Commit the implementation**

Run: `git add crates/flpdf/src crates/flpdf/tests docs && git commit -m "refactor: split filespec and embedded file helpers"`
