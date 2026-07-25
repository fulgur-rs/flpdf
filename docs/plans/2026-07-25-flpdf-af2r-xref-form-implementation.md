# qpdf-compatible plain xref form selection implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make full rewrites emit an xref stream exactly when the final plain-write placement contains an ObjStm container.

**Architecture:** `PlainWritePlan` remains the single owner of logical object placement and trailer form. Its existing `has_object_stream` result will directly select `XrefForm`, so the serializer receives a self-consistent plan without consulting the source xref form.

**Tech Stack:** Rust workspace, qpdf 11.9.0 oracle, `qpdf-zlib-compat`, cargo-llvm-cov.

## Global constraints

- qpdf 11.9.0 source and observed output are the compatibility oracle.
- Do not change incremental, linearized, encrypted, or structural stream-compression behavior.
- The final committed diff must have 100% patch coverage.
- Preserve existing user changes and keep `main` clean.

---

### Task 1: Add xref-form regression coverage

**Files:**
- Create: `tests/golden/references/null-visible-matrix-objstm/disable.pdf`
- Modify: `crates/flpdf/tests/cmp_diff_zero_tests.rs`
- Modify: `crates/flpdf/tests/object_streams_writer_tests.rs`
- Modify: `crates/flpdf/src/writer/plain/plan.rs` (test module only)

**Interfaces:**
- Consumes: `rewrite_qpdf_equivalent_mode(fixture, ObjectStreamMode)` and `build_xref_stream_pdf_no_objstm()`.
- Produces: executable expectations that no final ObjStm means `XrefForm::Table`.

- [ ] **Step 1: Generate the qpdf Disable golden**

Run:

```bash
qpdf --static-id --object-streams=disable \
  tests/fixtures/compat/null-visible-matrix-objstm.pdf \
  tests/golden/references/null-visible-matrix-objstm/disable.pdf
```

Expected: qpdf exits 3 because the authored fixture intentionally has a
non-canonical `/Size`, but creates a valid output containing a classic `xref`
table and no `/Type /XRef`.

- [ ] **Step 2: Add the failing byte-parity test**

Add to `crates/flpdf/tests/cmp_diff_zero_tests.rs`:

```rust
#[test]
fn disable_xref_stream_source_downgrades_to_classic_table_byte_identical_to_qpdf() {
    assert_cmp_diff_zero_mode_named(
        "null-visible-matrix-objstm.pdf",
        ObjectStreamMode::Disable,
        "null-visible-matrix-objstm",
        "disable.pdf",
    );
}
```

- [ ] **Step 3: Add the failing Preserve-without-ObjStm behavior test**

Add to `crates/flpdf/tests/object_streams_writer_tests.rs`:

```rust
#[test]
fn preserve_xref_stream_without_objstm_downgrades_to_classic_table() {
    let source = build_xref_stream_pdf_no_objstm();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = ObjectStreamMode::Preserve;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    assert!(output.windows(b"\nxref\n".len()).any(|w| w == b"\nxref\n"));
    assert!(!output
        .windows(b"/Type /XRef".len())
        .any(|w| w == b"/Type /XRef"));
}
```

- [ ] **Step 4: Correct plan-level expected behavior before production code**

In `crates/flpdf/src/writer/plain/plan.rs`, replace the two Disable tests that
expect inherited xref-stream form:

```rust
#[test]
fn disable_xref_stream_source_keeps_parseable_source_version_and_uses_table() {
    // Read three-page-objstm.pdf, change its header from 1.5 to 1.4.
    // Build a Disable plan.
    assert_eq!(plan.version, "1.4");
    assert_eq!(plan.trailer.form, XrefForm::Table);
}

#[test]
fn disable_xref_stream_source_does_not_apply_stream_version_repair() {
    // Read three-page-objstm.pdf, change its header to x.y.
    // Build a Disable plan.
    assert_eq!(plan.version, "x.y");
    assert_eq!(plan.trailer.form, XrefForm::Table);
    plan.validate().unwrap();
}
```

- [ ] **Step 5: Run the new tests and verify RED**

Run:

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests \
  disable_xref_stream_source_downgrades_to_classic_table_byte_identical_to_qpdf -- --exact
cargo test -p flpdf --test object_streams_writer_tests \
  preserve_xref_stream_without_objstm_downgrades_to_classic_table -- --exact
cargo test -p flpdf writer::plain::plan::tests::disable_xref_stream_source -- --nocapture
```

Expected: each test fails because current production code chooses
`pdf.last_xref_form()` when no output ObjStm exists.

### Task 2: Select xref form from final placement

**Files:**
- Modify: `crates/flpdf/src/writer/plain/plan.rs:106-123`

**Interfaces:**
- Consumes: `has_object_stream: bool`, already computed from final `placement.objects`.
- Produces: `TrailerPlan.form`, with `Stream` iff an `ObjectStream` placement exists.

- [ ] **Step 1: Implement the minimal production change**

Replace the mode/source-form decision with:

```rust
let form = if has_object_stream {
    XrefForm::Stream
} else {
    XrefForm::Table
};
```

Do not change `structural_filtered`, body emission, or xref serialization.

- [ ] **Step 2: Run the RED tests and verify GREEN**

Run the three commands from Task 1 Step 5.

Expected: all new tests pass.

- [ ] **Step 3: Run focused regression suites**

Run:

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --test object_streams_writer_tests
cargo test -p flpdf writer::plain::plan::tests
```

Expected: all tests pass, including existing Preserve/Generate ObjStm cases.

- [ ] **Step 4: Format and inspect the diff**

Run:

```bash
cargo fmt --all
cargo fmt --all -- --check
git diff --check
git diff --stat
```

Expected: formatting and whitespace checks pass; the production diff is limited
to xref-form selection.

- [ ] **Step 5: Commit the implementation**

```bash
git add crates/flpdf/src/writer/plain/plan.rs \
  crates/flpdf/tests/cmp_diff_zero_tests.rs \
  crates/flpdf/tests/object_streams_writer_tests.rs \
  tests/golden/references/null-visible-matrix-objstm/disable.pdf
git commit -m "fix(writer): derive xref form from final object placement"
```

### Task 3: Verify and publish

**Files:**
- No new files.

**Interfaces:**
- Consumes: committed implementation from Task 2.
- Produces: verified commit, closed Bead, and pushed Git/Beads state.

- [ ] **Step 1: Run repository quality gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
cargo test
```

Expected: every command exits 0.

- [ ] **Step 2: Measure committed-HEAD patch coverage**

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: 100% changed-line coverage.

- [ ] **Step 3: Close and synchronize the Bead**

```bash
bd close flpdf-af2r --reason="qpdf-compatible xref form now derives from final ObjStm placement"
bd dolt push
```

- [ ] **Step 4: Push Git and confirm remote state**

```bash
git push -u origin fix/flpdf-af2r-xref-form
git status --short --branch
```

Expected: push succeeds and the worktree is clean.
