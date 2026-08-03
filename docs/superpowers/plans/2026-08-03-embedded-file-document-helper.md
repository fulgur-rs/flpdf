# EmbeddedFileDocumentHelper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the five qpdf 11.9.0 `QPDFEmbeddedFileDocumentHelper` operations without changing the later JSON Job-layer scope.

**Architecture:** Add a document helper that owns `&mut Pdf`, matching current document helpers. Read APIs return a sorted `ObjectHandle` map: indirect values preserve canonical identity and direct Filespecs are lifted to direct handles. Match qpdf object topology and repair behavior: retain direct `/Names`, retain an empty `/EmbeddedFiles` tree after final removal, and use no artificial numeric depth cap. Helper removal nulls only the removed indirect Filespec and never invokes the stronger `remove_attachment` cleanup/GC path.

**Tech Stack:** Rust, `Pdf`, `ObjectHandle`, `NameTree`, qpdf 11.9.0 source, Cargo.

---

## File structure

- Modify `crates/flpdf/src/embedded_files.rs`: helper, private tree-root/value helpers, docs.
- Modify `crates/flpdf/src/lib.rs`: public re-export.
- Modify `crates/flpdf/tests/embedded_files_tests.rs`: public API integration tests.

### Task 1: Read API (TDD)

**Files:**
- Modify: `crates/flpdf/src/embedded_files.rs:1-80,371-435`
- Modify: `crates/flpdf/src/lib.rs:200-205`
- Test: `crates/flpdf/tests/embedded_files_tests.rs`

- [ ] **Step 1: Write failing read tests**

```rust
#[test]
fn helper_reads_named_filespecs_as_handles() {
    let mut pdf = open(build_single_level_pdf());
    let files = EmbeddedFileDocumentHelper::new(&mut pdf).get_embedded_files().unwrap();
    assert_eq!(files.keys().cloned().collect::<Vec<_>>(), vec![b"alpha".to_vec(), b"beta".to_vec()]);
    let alpha = files.get(b"alpha".as_slice()).unwrap().clone();
    drop(files);
    assert_eq!(FileSpec::new(alpha, &mut pdf).unwrap().get_filename().unwrap(), b"alpha.txt");
}
```

Add an absent-tree test asserting `has_embedded_files() == false`, an empty map, and `None` lookup.

Add this shared test helper before the new tests; it creates an indirect
Filespec using the already-public factory:

```rust
fn make_filespec(pdf: &mut Pdf<Cursor<Vec<u8>>>, filename: &[u8]) -> ObjectHandle {
    let ef = EmbeddedFileStream::create_ef_stream(pdf, b"payload").unwrap();
    FileSpec::create_file_spec(pdf, filename, ef).unwrap()
}
```

- [ ] **Step 2: Verify RED**

Run: `cargo test -p flpdf --test embedded_files_tests helper_`

Expected: compilation fails because `EmbeddedFileDocumentHelper` does not exist.

- [ ] **Step 3: Implement the minimum read boundary**

```rust
pub struct EmbeddedFileDocumentHelper<'a, R: Read + Seek> { pdf: &'a mut Pdf<R> }
impl<'a, R: Read + Seek> EmbeddedFileDocumentHelper<'a, R> {
    pub fn new(pdf: &'a mut Pdf<R>) -> Self { Self { pdf } }
    pub fn has_embedded_files(&mut self) -> Result<bool> { self.embedded_files_root().map(|root| root.is_some()) }
    pub fn get_embedded_files(&mut self) -> Result<BTreeMap<Vec<u8>, ObjectHandle>> { self.read_all_handles() }
    pub fn get_embedded_file(&mut self, key: &[u8]) -> Result<Option<ObjectHandle>> { self.read_one_handle(key) }
}
impl<R: Read + Seek> Pdf<R> {
    pub fn embedded_files(&mut self) -> EmbeddedFileDocumentHelper<'_, R> { EmbeddedFileDocumentHelper::new(self) }
}
```

Resolve `/Root /Names /EmbeddedFiles` through existing ref-chain helpers. Lift raw values with crate-private `Pdf::lift_object_to_handle`; add the helper to the existing `lib.rs` re-export.

- [ ] **Step 4: Verify GREEN and commit**

Run: `cargo test -p flpdf --test embedded_files_tests helper_`

Expected: both tests pass.

```bash
git add crates/flpdf/src/embedded_files.rs crates/flpdf/src/lib.rs crates/flpdf/tests/embedded_files_tests.rs
git commit -m "feat: add embedded-file document helper reads"
```

### Task 2: Replace API (TDD)

**Files:**
- Modify: `crates/flpdf/src/embedded_files.rs:483-561`
- Test: `crates/flpdf/tests/embedded_files_tests.rs`

- [ ] **Step 1: Write failing replacement tests**

```rust
#[test]
fn helper_replace_creates_and_replaces_name_tree_entry() {
    let mut pdf = open(build_no_names_pdf());
    let first = make_filespec(&mut pdf, b"first.txt");
    let second = make_filespec(&mut pdf, b"second.txt");
    let mut helper = pdf.embedded_files();
    helper.replace_embedded_file(b"entry", first).unwrap();
    helper.replace_embedded_file(b"entry", second.clone()).unwrap();
    assert!(helper.has_embedded_files().unwrap());
    assert!(helper.get_embedded_file(b"entry").unwrap().unwrap().is_same_object_as(&second));
}
```

Add a foreign-indirect-handle test expecting `Error::Unsupported` with identical pre/post attachment lists, and a direct Filespec replacement test.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p flpdf --test embedded_files_tests helper_replace_`

Expected: compilation fails because `replace_embedded_file` is absent.

- [ ] **Step 3: Implement minimal replacement**

```rust
pub fn replace_embedded_file(&mut self, key: &[u8], filespec: ObjectHandle) -> Result<()> {
    let value = match filespec.object_ref() {
        Some(r) if self.pdf.is_canonical_object_handle(&filespec) => Object::Reference(r),
        Some(_) => return Err(Error::Unsupported("filespec handle belongs to another Pdf".into())),
        None => filespec.materialize(),
    };
    self.insert_raw_embedded_file(key, value)
}
```

Extract the body of `insert_embedded_file` into a private value-accepting routine if necessary; retain its ref-only public behavior.

- [ ] **Step 4: Verify GREEN and commit**

Run: `cargo test -p flpdf --test embedded_files_tests helper_replace_`

Expected: all replacement tests pass.

```bash
git add crates/flpdf/src/embedded_files.rs crates/flpdf/tests/embedded_files_tests.rs
git commit -m "feat: add embedded-file helper replacement"
```

### Task 3: Remove API (TDD)

**Files:**
- Modify: `crates/flpdf/src/embedded_files.rs:563-625`
- Test: `crates/flpdf/tests/embedded_files_tests.rs`

- [ ] **Step 1: Write failing removal tests**

```rust
#[test]
fn helper_remove_nulls_indirect_filespec_without_attachment_gc() {
    let mut pdf = open(build_no_names_pdf());
    let filespec = make_filespec(&mut pdf, b"remove.txt");
    let filespec_ref = filespec.object_ref().unwrap();
    pdf.embedded_files().replace_embedded_file(b"remove", filespec).unwrap();
    assert!(pdf.embedded_files().remove_embedded_file(b"remove").unwrap());
    assert!(pdf.resolve_borrowed(filespec_ref).unwrap().is_null());
}
```

Add absent tree/key tests returning false and a direct Filespec removal test proving no unrelated indirect object is nulled.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p flpdf --test embedded_files_tests helper_remove_`

Expected: compilation fails because `remove_embedded_file` is absent.

- [ ] **Step 3: Implement minimal removal**

```rust
pub fn remove_embedded_file(&mut self, key: &[u8]) -> Result<bool> {
    let prior = self.find_raw_embedded_file(key)?;
    if !self.delete_raw_embedded_file(key)? { return Ok(false); }
    if let Some(Object::Reference(r)) = prior { self.pdf.set_object(r, Object::Null); }
    Ok(true)
}
```

Read the old raw value through `NameTree::find_object` before deletion. Do not call `remove_attachment`, clear `/AF`, delete EF streams, or run GC.

- [ ] **Step 4: Verify GREEN and commit**

Run: `cargo test -p flpdf --test embedded_files_tests helper_remove_`

Expected: all removal tests pass.

```bash
git add crates/flpdf/src/embedded_files.rs crates/flpdf/tests/embedded_files_tests.rs
git commit -m "feat: add embedded-file helper removal"
```

### Task 4: Oracle and quality verification

**Files:**
- Modify only if source-derived checks find a correction: `crates/flpdf/src/embedded_files.rs`, `crates/flpdf/tests/embedded_files_tests.rs`.

- [ ] **Step 1: Verify qpdf behavior**

```bash
qpdf_source=$(scripts/fetch-qpdf-source.sh --print-path)
sed -n '45,65p' "$qpdf_source/include/qpdf/QPDFEmbeddedFileDocumentHelper.hh"
sed -n '48,121p' "$qpdf_source/libqpdf/QPDFEmbeddedFileDocumentHelper.cc"
```

Expected: five public methods; replacement inserts the Filespec; absence returns false; indirect removal becomes null.

- [ ] **Step 2: Run complete checks**

```bash
cargo test -p flpdf --test embedded_files_tests
cargo test -p flpdf --test filespec_helper_tests
cargo fmt -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
scripts/patch-coverage.sh --base main
```

Expected: every command exits 0 and changed-line coverage is complete.

- [ ] **Step 3: Commit only a verification-driven correction**

```bash
git status --short
git add crates/flpdf/src/embedded_files.rs crates/flpdf/src/lib.rs crates/flpdf/tests/embedded_files_tests.rs
git commit -m "test: cover embedded-file helper API"
```

Skip the commit when the status is empty.
