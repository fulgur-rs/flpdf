# qpdf Page Document Helper Boundary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `PageDocumentHelper` a qpdf 11.9.0-aligned live-document facade while preserving `page_extract.rs` as the fresh-document page-extraction path.

**Architecture:** `PageDocumentHelper` will derive the current page list from qpdf-compatible repair before operating on it.  New qpdf-named facade methods delegate to the existing page-tree rebuild, inherited-attribute, resource-pruning, and annotation-flattening primitives; existing ergonomic methods remain as forwarding compatibility conveniences.  `page_extract.rs` remains separate because it creates a new PDF rather than altering a live document.

**Tech Stack:** Rust workspace, `flpdf`, qpdf 11.9.0 source oracle, Cargo test/clippy/fmt.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are the oracle.
- Do not create a second page-tree traversal, resource-pruning implementation, or annotation-flattening implementation.
- `page_extract.rs` owns the `emptyPDF() + addPage()` fresh-document route; `pages.rs` owns traversal.
- Keep a page list uncached: callers must enumerate again after mutations.
- Preserve existing public convenience methods unless an exact qpdf behavior requires changing their semantics.

---

### Task 1: Repair-aware page enumeration and inherited attributes

**Files:**
- Modify: `crates/flpdf/src/page_document_helper.rs`
- Modify: `crates/flpdf/tests/page_document_helper_tests.rs`

**Interfaces:**
- Consumes: `crate::pages::repair::prepare_for_optimization(&mut Pdf<R>) -> Result<Option<PreparedPages>>`
- Consumes: `crate::optimization::inherited_attrs::push(&mut Pdf<R>, &PreparedPages, bool, bool) -> Result<()>`
- Produces: `PageDocumentHelper::get_all_pages(&mut self) -> Result<Vec<ObjectRef>>`
- Produces: `PageDocumentHelper::push_inherited_attributes_to_pages(&mut self) -> Result<()>`

- [ ] **Step 1: Write the failing repair-aware enumeration test**

  Add a fixture whose catalog `/Pages` points at a leaf whose `/Parent` points to the actual `/Pages` root, then assert that `get_all_pages` returns the leaf and rewrites catalog `/Pages` to the true root.  This distinguishes it from the current raw `page_refs` route.

  ```rust
  #[test]
  fn get_all_pages_repairs_catalog_pages_pointer() {
      let mut pdf = open(pdf_with_catalog_pages_pointing_to_leaf());
      let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
      assert_eq!(pages, vec![ObjectRef::new(3, 0)]);
      let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else { panic!() };
      assert_eq!(catalog.get("Pages"), Some(&Object::Reference(ObjectRef::new(2, 0))));
  }
  ```

- [ ] **Step 2: Run the test to verify it fails**

  Run: `cargo test -p flpdf --test page_document_helper_tests get_all_pages_repairs_catalog_pages_pointer`

  Expected: compilation failure because `get_all_pages` does not exist.

- [ ] **Step 3: Implement repair-aware enumeration and inherited push**

  Add a private helper and the two public methods below.  Make existing `pages`, `iter`, `get`, and `rotate` obtain their list through `get_all_pages` rather than `pages::page_refs`.

  ```rust
  fn prepared_pages(&mut self) -> Result<Option<crate::pages::repair::PreparedPages>> {
      crate::pages::repair::prepare_for_optimization(self.pdf)
  }

  pub fn get_all_pages(&mut self) -> Result<Vec<ObjectRef>> {
      Ok(self.prepared_pages()?.map_or_else(Vec::new, |prepared| prepared.pages))
  }

  pub fn push_inherited_attributes_to_pages(&mut self) -> Result<()> {
      if let Some(prepared) = self.prepared_pages()? {
          crate::optimization::inherited_attrs::push(self.pdf, &prepared, true, false)?;
      }
      Ok(())
  }
  ```

- [ ] **Step 4: Add and run inherited-attribute regression coverage**

  Build a root `/Pages` with `/Rotate 90` and a child `/Page` without `/Rotate`; call `push_inherited_attributes_to_pages`; assert the leaf owns `Object::Integer(90)`.  Run:

  `cargo test -p flpdf --test page_document_helper_tests`

  Expected: all existing and new helper tests pass.

- [ ] **Step 5: Commit the independently working facade traversal**

  ```bash
  git add crates/flpdf/src/page_document_helper.rs crates/flpdf/tests/page_document_helper_tests.rs
  git commit -m "feat(flpdf): add qpdf page helper traversal facade"
  ```

### Task 2: qpdf-style page insertion and removal facade

**Files:**
- Modify: `crates/flpdf/src/page_document_helper.rs`
- Modify: `crates/flpdf/tests/page_document_helper_tests.rs`

**Interfaces:**
- Consumes: `PageDocumentHelper::get_all_pages(&mut self) -> Result<Vec<ObjectRef>>`
- Consumes: `PageDocumentHelper::insert(&mut self, usize, ObjectRef) -> Result<RebuildResult>`
- Consumes: `PageDocumentHelper::remove(&mut self, usize) -> Result<RebuildResult>`
- Produces: `add_page(&mut self, ObjectRef, bool) -> Result<RebuildResult>`
- Produces: `add_page_at(&mut self, ObjectRef, bool, ObjectRef) -> Result<RebuildResult>`
- Produces: `remove_page(&mut self, ObjectRef) -> Result<RebuildResult>`

- [ ] **Step 1: Write failing insertion/removal behavior tests**

  Add tests that call `add_page(page, true)` and `add_page(page, false)` and assert the selected page is first and last respectively.  Add tests for `add_page_at(page, true, reference)` and `add_page_at(page, false, reference)`.  Add a `remove_page` test that removes by page reference and a one-page removal test that expects an empty `/Kids` array and `/Count 0`, matching qpdf `QPDF::removePage`.

  ```rust
  #[test]
  fn remove_page_allows_an_empty_document() {
      let mut pdf = open(build_n_page_pdf(1));
      PageDocumentHelper::new(&mut pdf)
          .remove_page(ObjectRef::new(3, 0))
          .unwrap();
      assert!(PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap().is_empty());
  }
  ```

- [ ] **Step 2: Run the selected tests to verify they fail**

  Run: `cargo test -p flpdf --test page_document_helper_tests add_page`

  Expected: compilation failure because the qpdf-named methods do not exist.

- [ ] **Step 3: Implement the qpdf-named mutation methods**

  Use the repaired current list for positions and delegate the actual update to `insert`/`remove`.  `add_page_at` must reject a `ref_page` absent from the current list before any mutation.  Remove the current last-page guard so `rebuild_page_tree(self.pdf, &[])` writes qpdf's valid empty `/Pages` tree.

  ```rust
  pub fn add_page(&mut self, page: ObjectRef, first: bool) -> Result<RebuildResult> {
      let position = if first { 0 } else { self.get_all_pages()?.len() };
      self.insert(position, page)
  }

  pub fn add_page_at(&mut self, page: ObjectRef, before: bool, ref_page: ObjectRef) -> Result<RebuildResult> {
      let pages = self.get_all_pages()?;
      let position = pages.iter().position(|&candidate| candidate == ref_page)
          .ok_or(Error::Missing("reference page is not in the document"))?;
      self.insert(position + usize::from(!before), page)
  }

  pub fn remove_page(&mut self, page: ObjectRef) -> Result<RebuildResult> {
      let position = self.get_all_pages()?.iter().position(|&candidate| candidate == page)
          .ok_or(Error::Missing("page is not in the document"))?;
      self.remove(position)
  }
  ```

- [ ] **Step 4: Run mutation regression tests**

  Run: `cargo test -p flpdf --test page_document_helper_tests`

  Expected: all insertion, removal, ordering, and round-trip tests pass, including the empty-document case.

- [ ] **Step 5: Commit the independently working mutation facade**

  ```bash
  git add crates/flpdf/src/page_document_helper.rs crates/flpdf/tests/page_document_helper_tests.rs
  git commit -m "feat(flpdf): align page helper mutations with qpdf"
  ```

### Task 3: Resource pruning, annotation flattening, and extraction-boundary documentation

**Files:**
- Modify: `crates/flpdf/src/page_document_helper.rs`
- Modify: `crates/flpdf/src/resources.rs`
- Modify: `crates/flpdf/src/page_extract.rs`
- Modify: `crates/flpdf/tests/page_document_helper_tests.rs`

**Interfaces:**
- Consumes: a page-scoped resource-pruning primitive extracted from `resources.rs`
- Consumes: `crate::page_annotation_flatten::flatten_annotations(&mut Pdf<R>, FlattenMode) -> Result<usize>`
- Produces: `remove_unreferenced_resources(&mut self) -> Result<()>`
- Produces: `flatten_annotations(&mut self, FlattenMode) -> Result<usize>`

- [ ] **Step 1: Write failing facade-routing tests**

  Use an existing minimal content-stream fixture with `/Resources /Font` entries `F1` (used) and `F2` (unused).  Invoke the helper method and assert `F2` is absent.  Use a one-page annotation fixture with a normal appearance and assert `helper.flatten_annotations(FlattenMode::All)` returns one and removes the annotation from the page.

  ```rust
  #[test]
  fn helper_prunes_unused_resources() {
      let mut pdf = open(pdf_with_used_and_unused_font_resources());
      PageDocumentHelper::new(&mut pdf).remove_unreferenced_resources().unwrap();
      assert_eq!(font_keys(&mut pdf), [b"F1".as_slice()]);
  }
  ```

- [ ] **Step 2: Run the selected tests to verify they fail**

  Run: `cargo test -p flpdf --test page_document_helper_tests helper_`

  Expected: compilation failure because the facade methods do not exist.

- [ ] **Step 3: Add facade delegates and document the separate extraction route**

  Factor the current resource scan into a page-scoped primitive and have the helper enumerate repaired pages and invoke it once per page.  This is required because qpdf's `QPDFPageDocumentHelper::removeUnreferencedResources` loops over pages and `QPDFPageObjectHelper::removeUnreferencedResources` shallow-copies each page's `/Font` and `/XObject` dictionaries before pruning; the document-wide `Yes` mode instead unions names for a shared dictionary.  Reuse the existing content parsing, Form recursion, unresolved-name, and dictionary-update code; do not fork it.  Extend `page_extract.rs` module documentation to state that its `minimal_target_bytes` plus copied selected pages is the qpdf `emptyPDF() + addPage()` extraction route, not the live-document `PageDocumentHelper::add_page` route.

  ```rust
  pub fn remove_unreferenced_resources(&mut self) -> Result<()> {
      for page in self.get_all_pages()? {
          crate::resources::remove_unreferenced_resources_on_page(self.pdf, page)?;
      }
      Ok(())
  }

  pub fn flatten_annotations(&mut self, mode: crate::FlattenMode) -> Result<usize> {
      crate::flatten_annotations(self.pdf, mode)
  }
  ```

- [ ] **Step 4: Run focused facade and existing primitive tests**

  Run: `cargo test -p flpdf --test page_document_helper_tests`

  Run: `cargo test -p flpdf --test resource_pruning_tests`

  Expected: all tests pass; the facade adds no second pruning or flattening path.

- [ ] **Step 5: Commit the completed responsibility boundary**

  ```bash
  git add crates/flpdf/src/page_document_helper.rs crates/flpdf/src/page_extract.rs crates/flpdf/tests/page_document_helper_tests.rs
  git commit -m "feat(flpdf): complete page document helper boundary"
  ```

### Task 4: qpdf comparison and repository quality gates

**Files:**
- Modify if required by verified output: files from Tasks 1-3 only
- Verify: `crates/flpdf/tests/page_document_helper_tests.rs`

**Interfaces:**
- Consumes: all `PageDocumentHelper` methods added above
- Produces: source-backed behavior confirmation and a clean quality-gate result

- [ ] **Step 1: Add a focused qpdf 11.9.0 probe for ambiguous mutation behavior**

  Compile a temporary C++ probe against the installed qpdf development headers only if a Rust assertion disagrees with `QPDF_pages.cc:210-295`.  Exercise one-page `removePage`, before/after insertion, and non-member reference-page failure; discard the probe after recording the observed result in a test comment.

- [ ] **Step 2: Run the focused qpdf-facing test suite**

  Run: `cargo test -p flpdf --test page_document_helper_tests`

  Run: `cargo test -p flpdf --test resource_pruning_tests`

  Expected: all tests pass.

- [ ] **Step 3: Run workspace formatting and lint gates**

  Run: `cargo fmt --all -- --check`

  Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

  Expected: both commands exit 0.

- [ ] **Step 4: Run the workspace regression suite and changed-line coverage**

  Run: `cargo test`

  Run: `scripts/patch-coverage.sh --base origin/main`

  Expected: all workspace tests pass and every changed executable line is covered.

- [ ] **Step 5: Commit only verified fixes, then inspect the final diff**

  ```bash
  git status --short
  git diff origin/main...HEAD --check
  git log --oneline origin/main..HEAD
  ```
