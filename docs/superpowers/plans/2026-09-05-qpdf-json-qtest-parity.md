# qpdf-json qtest parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make all 145 subtests in qpdf 11.9.0's `qpdf-json.test` pass through canonical flpdf Rust paths and the portable qpdf-ctest process adapter.

**Architecture:** First connect the existing QPDFJob page-selection consumer to the JSON output route and add qpdf's monotonic inherited-page observation to Pdf. In a separate stacked consumer slice, extend the existing Rust qpdf-ctest binary for tests 42–47 using the existing JSON input/output, writer, and Pipeline APIs. Finish with an isolated qtest run and evidence-only parity-ledger reconciliation.

**Tech Stack:** Rust workspace (`flpdf`, `flpdf-cli`, `flpdf-qtest-tools`), qpdf 11.9.0 source/oracle, qtest Perl driver, Bash, JSON Lines, `jq`, and `serde_json` test assertions.

---

## File map

- Modify `crates/flpdf/src/pdf.rs`: store and expose the monotonic qpdf inherited-page push observation.
- Modify `crates/flpdf/src/engine.rs`: initialize that document state for uninitialized and parsed Pdf values.
- Modify `crates/flpdf/src/page_document_helper.rs`: set that observation at the canonical push boundary.
- Modify `crates/flpdf/src/document_json.rs`: serialize the stored observation instead of a constant.
- Modify `crates/flpdf-cli/src/main.rs`: apply parsed single-source `--pages` selection before JSON serialization using `QPDFJob::handle_page_specs`.
- Test `crates/flpdf-cli/tests/cli_json.rs`: add a synthetic malformed/inherited page-tree regression for qpdf-json 120/122.
- Modify `crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs`: add dispatch and portable implementations for qpdf-ctest tests 42–47.
- Test `crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs`: cover the new process boundary and failure paths with real temporary files.
- Modify `/tmp/flpdf-qtest-25kg-15-qtest/parity/qtest-11.9.0.jsonl` only after the exact qtest run proves the changed outcomes; never modify `vendor/qpdf-qtest/qpdf-json.test`.

## qpdf responsibility map

| qpdf 11.9.0 responsibility | Rust owner | Verification |
| --- | --- | --- |
| `createQPDF` applies update, `handlePageSpecs`, then output | `run_json` plus `QPDFJob::handle_page_specs` | qpdf-json 120/122 |
| `everPushedInheritedAttributesToPages` | `Pdf` + `PageDocumentHelper` | JSON metadata assertion |
| `createFromJSON` / `updateFromJSON` | `Pdf::create_from_json` / `Pdf::update_from_json` | qpdf-ctest 42–45 |
| `writeJSON` and `writeStreamJSON` | `document_json::write_json` and `ObjectHandle::write_stream_json` | qpdf-ctest 46/47 |
| C test process observations | `qpdf-ctest` Rust binary | qpdf-json 126–138 |

### Task 1: Add the page-selection RED test

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_json.rs`

- [ ] **Step 1: Add a fixture with an intermediate `/Pages` resource and a duplicate leaf.**

Use the existing `build_pdf` helper to create objects 1–8: catalog 1 points at pages 2; pages 2 has `/Kids [3 0 R]`, `/Count 2`, and `/Resources 8 0 R`; pages node 3 has `/Kids [4 0 R 4 0 R]`, `/Parent 2 0 R`, and `/Count 2`; page 4 has `/MediaBox`, `/Contents 5 0 R`, and `/Parent 3 0 R`; object 5 is the content stream; object 6 is the Helvetica font; object 8 is the inherited resource dictionary. Keep the fixture authored in the flpdf test file rather than copying qpdf-qtest data.

- [ ] **Step 2: Add the failing CLI regression.**

Run the command shape used by qpdf-json.test:

```rust
#[test]
fn json_output_pages_selection_rebuilds_the_live_page_tree() {
    let input = write_temp_pdf(&duplicate_page_inherited_pdf());
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("pages.json");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--json-output",
            input.path().to_str().unwrap(),
            "--json-key=pages",
            "--pages",
            ".",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .code(3);

    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(json["qpdf"][0]["pushedinheritedpageresources"], true);
    assert_eq!(json["qpdf"][0]["calledgetallpages"], true);
    assert_eq!(json["qpdf"][0]["maxobjectid"], 8);
    assert_eq!(json["qpdf"][1]["obj:2 0 R"]["value"]["/Kids"],
               serde_json::json!(["4 0 R", "7 0 R"]));
    assert!(json["qpdf"][1]["obj:4 0 R"]["value"].get("/Resources").is_some());
    assert!(json["qpdf"][1]["obj:7 0 R"]["value"].get("/Resources").is_some());
}
```

- [ ] **Step 3: Run the focused test and verify the expected RED result.**

Run `cargo test -p flpdf-cli --test cli_json json_output_pages_selection_rebuilds_the_live_page_tree -- --exact --nocapture`.

Expected: the command exits with qpdf's warning status 3 (the fixture's duplicate page leaf triggers a repair warning) but the assertion for `pushedinheritedpageresources == true` or the flattened `/Kids` array fails, proving that the test observes the missing production behavior rather than a setup error.

### Task 2: Implement qpdf's inherited-page observation and JSON page route

**Files:**
- Modify: `crates/flpdf/src/pdf.rs`
- Modify: `crates/flpdf/src/page_document_helper.rs`
- Modify: `crates/flpdf/src/document_json.rs`
- Modify: `crates/flpdf-cli/src/main.rs`

- [ ] **Step 1: Add the monotonic document state and accessor.**

Add a `pub(crate) ever_pushed_inherited_attributes_to_pages: bool` field adjacent to `ever_called_get_all_pages`, initialize it to `false` in every `Pdf` constructor, and expose:

```rust
pub fn ever_pushed_inherited_attributes_to_pages(&self) -> bool {
    self.ever_pushed_inherited_attributes_to_pages
}
```

Keep the value monotonic. Do not use page contents, an empty collection, or cache presence as a sentinel.

- [ ] **Step 2: Set the state only at the canonical push boundary.**

In `PageDocumentHelper::push_inherited_attributes_to_pages`, retain the existing repair and `optimization::inherited_attrs::push` calls. After a successful push operation, set `self.pdf.ever_pushed_inherited_attributes_to_pages = true`. Do not set it from `get_all_pages`, `tree_rebuild`, JSON serialization, or a CLI branch.

- [ ] **Step 3: Serialize the live state.**

Replace the constant `Json::make_bool(false)` for `pushedinheritedpageresources` in `document_json::write_json_key` with `Json::make_bool(pdf.ever_pushed_inherited_attributes_to_pages())`. Preserve the existing qpdf key order and object-map writer.

- [ ] **Step 4: Add one CLI helper for the qpdf page-spec boundary.**

Implement a helper in `main.rs` with this signature:

```rust
fn apply_json_page_specs<R: Read + Seek + 'static>(
    job: &mut QPDFJob,
    pdf: &mut Pdf<R>,
    primary_input: &Path,
    page_ops: &PageOpArgs,
) -> CliResult<()>
```

The helper returns immediately when `page_ops.pages` is empty. Otherwise it parses the segment with the existing `parse_pages_segment`, resolves `.` with `resolve_page_specs`, rejects a source path other than the primary input with the existing qpdf-shaped page-spec error, constructs `PageSpecInput` values with source index zero, and calls `job.handle_page_specs` with `RemoveUnreferencedResources::Auto` and the existing preserve policy used by the single-source page route. Drop the returned `PageSpecJobOutput` only after the in-place page mutation has completed. Do not add a second page walker.

- [ ] **Step 5: Invoke the helper before JSON serialization.**

Call `apply_json_page_specs` after PDF creation/update and before `run_json_document` in the file-backed branch of `run_json`. Use the same `QPDFJob` that owns the JSON logger. Keep JSON-created input and multi-source page jobs on their existing explicit paths unless the current type boundary can pass them through the same canonical API without a new special case.

- [ ] **Step 6: Re-run the RED test and the focused JSON/page suites.**

Run the exact focused test from Task 1, then:

```text
cargo test -p flpdf-cli --test cli_json
cargo test -p flpdf --test page_document_helper_qpdf_tests
cargo test -p flpdf --test document_json_tests
cargo test -p flpdf --test json_document_tests
```

Expected: the new regression and all existing focused suites pass; the JSON route uses the existing page selection and inherited-attribute mutation code.

### Task 3 and Task 4: qpdf-ctest 42–47 (completed by the merged prerequisite)

**Status:** Already implemented and merged in flpdf PR #1506 (`qpdf_ctest.rs` dispatches
tests 42–47 through the canonical `Pdf::create_from_json_file`/`create_from_json`/
`update_from_json_file`/`update_from_json` and JSON-writer routes; `qpdf_ctest_cli.rs`
already contains `qpdf_ctest_json_cases_42_through_47_match_qpdf`, a live-qpdf-11.9.0
byte-identical differential covering all six cases including the `auto-4` file-backed
stream side-file). Do not re-add an "unsupported test" RED phase for these test numbers —
the dispatch already accepts them. If further work on qpdf-ctest 42–47 is needed, treat
this merged state as the baseline and write new RED coverage only for behavior that is
still actually missing.

### Task 5: Verify qpdf-json.test and reconcile evidence

**Files:**
- Modify: `/tmp/flpdf-qtest-25kg-15-qtest/parity/qtest-11.9.0.jsonl` only when justified by the same run

- [ ] **Step 1: Build exact release binaries from the committed implementation state.**

From the flpdf implementation worktree, run the qtest release build command selecting `flpdf`, `flpdf-test-compare`, `flpdf-test-driver`, `qpdfjob-ctest`, and `qpdf-ctest` with `--features qpdf-zlib-compat`. Record the commit SHA used for the binaries.

- [ ] **Step 2: Run qpdf-json.test in an isolated copied datadir.**

Use a disposable qtest datadir, a separate `harness.log` (never `qtest.log`), the copied shim directory, and `TESTS=qpdf-json` with the exact release binaries. Preserve the generated `qtest-results.xml` beside the harness log.

- [ ] **Step 3: Verify all 145 XML outcomes.**

Parse the same-run XML and require `total=145`, every testcase `outcome=pass`, zero failures, zero unexpected passes, zero missing tests. Inspect any failure from the XML and qtest log before changing code or ledger state.

- [ ] **Step 4: Run the complete qtest/manifest gate.**

Run `QTEST_FULL=1 ./scripts/run.sh` from the qtest worktree with all fourteen exact binary paths, then run `verify-allowlist.py` and `verify-parity-manifest.py` against the paired `survey/latest/harness.log` and `survey/latest/qtest-results.xml`. Keep unrelated stale ledger rows unchanged.

- [ ] **Step 5: Update only proven qpdf-json ledger rows.**

If the current manifest still marks qpdf-json 120, 122, 127, 129, 131, 133, 135, 137, or 138 as blocked/failing, change only the exact rows whose state and ownership are resolved by the successful same-run result. Retain represented rows with their existing Rust oracle references when that is still the approved ownership model. Validate every changed JSONL line with `jq -e .` and rerun both validators.

- [ ] **Step 6: Record final evidence and run the full implementation gates.**

Run the focused suites, `cargo test --workspace --all-features`, strict private-item rustdoc, all-feature clippy, qpdf route/deviation checks, and fresh changed-line patch coverage from the exact committed worktree. Read back Beads issue states and close only the completed child/parent issues after all evidence is present.

## Final handoff checks

- [ ] `qpdf-json.test` has 145/145 pass in a same-run artifact pair.
- [ ] `qtest-results.xml` and `harness.log` are from the same isolated run.
- [ ] qtest vendor sources are unchanged.
- [ ] No C ABI, shell-out, sentinel, compatibility bridge, or test-only output fabrication was added.
- [ ] Beads dependencies are acyclic, completed issues are read back, `bd dolt push` reports `Push complete.`, and the implementation/qtest git branches are pushed only after the quality gates pass.
