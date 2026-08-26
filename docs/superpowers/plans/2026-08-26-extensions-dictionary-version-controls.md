# extensions-dictionary Version Controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement qpdf 11.9.0's top-level `--min-version` and `--force-version`
version-spec behavior so `extensions-dictionary.test` reaches all 156 rows with
matching headers, Adobe extension dictionaries, and `PDFVersion` observations.

**Architecture:** Keep header/source parsing (`M.m`) separate from qpdf's job
version specification (`M.m[.E]`). Parse the optional extension level once at
the CLI/configuration boundary, store the base version and extension level in
the existing canonical `WriterConfiguration`/`PdfWriter` settings, and leave
final pair comparison plus Catalog `/Extensions` mutation in the writer.
Top-level and `rewrite` routes share the same parsed option object; page,
linearize, and attachment writer paths receive the same pair rather than
silently dropping it.

**Tech Stack:** Rust workspace, clap CLI, `PdfVersion`/`WriterConfiguration`,
qpdf 11.9.0 source and binary oracle, vendored qtest 156-row suite.

---

### Task 1: Add the qpdf version-spec parser contract

**Files:**
- Modify: `crates/flpdf/tests/pdf_version_tests.rs`
- Modify: `crates/flpdf/src/pdf_version.rs`

- [ ] **Step 1: Write the failing parser tests**

Add tests for the exact qtest forms and rejection boundaries:

```rust
#[test]
fn parses_qpdf_version_spec_into_base_version_and_extension_level() {
    assert_eq!(parse_pdf_version_spec("1.3"), Some(("1.3".into(), 0)));
    assert_eq!(parse_pdf_version_spec("1.7.1"), Some(("1.7".into(), 1)));
    assert_eq!(parse_pdf_version_spec("1.8.0"), Some(("1.8".into(), 0)));
    assert_eq!(parse_pdf_version_spec("1.8.5"), Some(("1.8".into(), 5)));
}

#[test]
fn rejects_version_specs_without_major_minor_or_with_extra_components() {
    for value in ["1", "1.", ".7", "1.7.", "1.7.1.2", "not-a-version"] {
        assert_eq!(parse_pdf_version_spec(value), None, "{value:?}");
    }
}
```

Import `parse_pdf_version_spec` from the public crate API beside the existing
`parse_pdf_version` import.

- [ ] **Step 2: Run the parser tests and verify the expected RED failure**

Run:

```bash
cargo test -p flpdf --test pdf_version_tests parses_qpdf_version_spec
```

Expected: compilation fails because `parse_pdf_version_spec` does not yet
exist. If the test compiles and passes, the test is not exercising the missing
contract and must be corrected before implementation.

- [ ] **Step 3: Implement the parser at the version boundary**

In `crates/flpdf/src/pdf_version.rs`, add:

```rust
/// Parses qpdf's job version syntax `M.m[.E]` into the header version and
/// optional extension level. The returned version is always the two-component
/// string that qpdf passes to `QPDFWriter`; the third component is never a PDF
/// header version.
pub fn parse_pdf_version_spec(value: &str) -> Option<(String, i64)> {
    let mut parts = value.split('.');
    let major = parts.next()?.parse::<u8>().ok()?;
    let minor = parts.next()?.parse::<u8>().ok()?;
    let extension_level = match parts.next() {
        None => 0,
        Some(level) if !level.is_empty() => level.parse::<i64>().ok()?,
        Some(_) => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((format!("{major}.{minor}"), extension_level))
}
```

Re-export it from `crates/flpdf/src/lib.rs` beside `parse_pdf_version`.
Keep `parse_pdf_version` as the `M.m` source/header parser used by the writer;
do not make the emitter accept `M.m.E`.

- [ ] **Step 4: Run the parser tests and verify GREEN**

Run:

```bash
cargo test -p flpdf --test pdf_version_tests
```

Expected: all parser tests pass, including the existing `M.m` API tests.

- [ ] **Step 5: Commit the parser contract**

```bash
git add crates/flpdf/src/pdf_version.rs crates/flpdf/src/lib.rs crates/flpdf/tests/pdf_version_tests.rs
git commit -m "feat: parse qpdf version extension specs"
```

### Task 2: Add one CLI version-option conversion and regression tests

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf-cli/tests/cli_tests.rs`

- [ ] **Step 1: Write failing CLI tests for top-level and rewrite forms**

Add tests that use real `minimal.pdf` and inspect the output header and Catalog
through the existing `flpdf-test-driver`-independent CLI assertions:

```rust
#[test]
fn top_level_min_version_with_extension_level_is_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--min-version=1.7.1",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
    let bytes = std::fs::read(&output).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.7\n"));
    assert!(bytes.windows(b"/BaseVersion /1.7".len()).any(|w| w == b"/BaseVersion /1.7"));
    assert!(bytes.windows(b"/ExtensionLevel 1".len()).any(|w| w == b"/ExtensionLevel 1"));
}

#[test]
fn rewrite_force_version_with_extension_level_emits_base_header_and_adbe_pair() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--static-id",
            "--force-version=1.8.5",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
    let bytes = std::fs::read(&output).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.8\n"));
    assert!(bytes.windows(b"/BaseVersion /1.8".len()).any(|w| w == b"/BaseVersion /1.8"));
    assert!(bytes.windows(b"/ExtensionLevel 5".len()).any(|w| w == b"/ExtensionLevel 5"));
}
```

Use the repository's existing `Command` import and fixture conventions. These
tests must be added before the CLI production changes.

- [ ] **Step 2: Run the new CLI tests and verify the expected RED failure**

Run:

```bash
cargo test -p flpdf-cli --test cli_tests top_level_min_version_with_extension_level
cargo test -p flpdf-cli --test cli_tests rewrite_force_version_with_extension_level
```

Expected: the top-level invocation fails with an unrecognized argument, and
the rewrite invocation rejects `1.8.5` as invalid. Capture both failures as the
baseline for the routing gap.

- [ ] **Step 3: Implement the shared CLI conversion object**

In `crates/flpdf-cli/src/main.rs`, add a small internal value object and helper
near the CLI `WriterOptions` definition:

```rust
#[derive(Debug, Clone, Default)]
struct CliVersionOptions {
    min: Option<(String, i64)>,
    force: Option<(String, i64)>,
}

fn parse_cli_version_options(
    min: Option<&str>,
    force: Option<&str>,
) -> CliResult<CliVersionOptions> {
    let parse = |name: &str, value: Option<&str>| {
        value
            .map(|value| {
                flpdf::parse_pdf_version_spec(value)
                    .ok_or_else(|| format!("invalid {name} value: {value:?}").into())
            })
            .transpose()
    };
    Ok(CliVersionOptions {
        min: parse("--min-version", min)?,
        force: parse("--force-version", force)?,
    })
}

fn apply_cli_version_options(options: &mut WriterOptions, versions: &CliVersionOptions) {
    if let Some((version, extension_level)) = &versions.min {
        options.min_version = Some(version.clone());
        options.min_extension_level = Some(*extension_level);
    }
    if let Some((version, extension_level)) = &versions.force {
        options.force_version = Some(version.clone());
        options.force_extension_level = Some(*extension_level);
    }
}
```

The helper must be the only CLI conversion from full version text to writer
fields. Preserve the existing process exit code and diagnostic shape at the
call sites by printing its error and exiting with the current usage code.

- [ ] **Step 4: Apply the helper to `rewrite` and verify GREEN**

Replace the current raw `parse_pdf_version` validation in the
`Commands::Rewrite` arm with `parse_cli_version_options`, then call
`apply_cli_version_options` on the constructed `WriterOptions`. This stores
`1.8.5` as `min_version=Some("1.8")` plus
`min_extension_level=Some(5)`, never as a header string.

Run:

```bash
cargo test -p flpdf-cli --test cli_tests rewrite_force_version_with_extension_level
```

Expected: the rewrite regression passes; the top-level test remains RED until
Task 3 adds the fields and route.

- [ ] **Step 5: Commit the shared conversion and rewrite support**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_tests.rs
git commit -m "feat(cli): accept qpdf extension-level version specs"
```

### Task 3: Wire top-level version options through every writer-producing route

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf-cli/tests/cli_tests.rs`

- [ ] **Step 1: Add top-level clap fields and route tests**

Add `min_version: Option<String>` and `force_version: Option<String>` to
`Cli`, with the same `#[arg(long = "min-version")]` and
`#[arg(long = "force-version")]` declarations as the rewrite fields. Extend
the CLI tests with a top-level `--force-version=1.8.5` invocation and an
invalid top-level value assertion. The production fields are intentionally
added before wiring so the route test first fails at output construction if
the options are not applied.

- [ ] **Step 2: Run the top-level tests and record the RED failure**

Run:

```bash
cargo test -p flpdf-cli --test cli_tests top_level_min_version_with_extension_level
```

Expected: clap accepts the option after the field addition, but the output
still lacks the requested ADBE pair until the shared helper is applied.

- [ ] **Step 3: Apply version options to top-level `WriterOptions` construction**

In `main()`, parse `args.min_version.as_deref()` and
`args.force_version.as_deref()` once after clap parsing. For each top-level
`WriterOptions` construction (normal rewrite, linearize, and page-operation),
call:

```rust
apply_cli_version_options(&mut options, &top_level_versions);
```

Do the same for top-level attachment write functions by passing a cloned
`CliVersionOptions` into `run_add_attachment`, `run_remove_attachment`, and
`run_copy_attachments_from`; apply it immediately before their
`write_with_pdf_writer` call. Read-only attachment list/show paths do not
create a writer and do not consume these options.

Validate the parsed options before dispatch so invalid values cannot open or
truncate an input. Keep the existing qpdf-shaped precedence: force is stored
as an exact pair and the writer applies it after input/minimum floors.

- [ ] **Step 4: Run focused route tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-cli --test cli_tests top_level_min_version_with_extension_level
cargo test -p flpdf-cli --test cli_tests rewrite_force_version_with_extension_level
cargo test -p flpdf-cli --test cli_tests rewrite_force_version_invalid_abc_exits_nonzero
```

Expected: all pass, including invalid-value handling. Confirm emitted headers
are `1.7`/`1.8`, never `1.7.1`/`1.8.5`.

- [ ] **Step 5: Commit the top-level route**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_tests.rs
git commit -m "feat(cli): wire top-level qpdf version controls"
```

### Task 4: Verify qpdf Catalog extension behavior and harden canonical writer coverage

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/tests/writer_tests.rs`
- Modify: `crates/flpdf-cli/tests/cli_tests.rs`

- [ ] **Step 1: Add canonical writer regression tests before any writer correction**

Add tests around `effective_pdf_version_and_ext` and the existing extension
helpers for these qpdf cases:

```rust
#[test]
fn version_spec_extension_level_is_used_only_when_version_wins_or_ties() {
    let mut options = WriterOptions::default();
    options.min_version = Some("1.8".into());
    options.min_extension_level = Some(5);
    assert_eq!(effective_pdf_version_and_ext("1.3", 0, &options, false, false), ("1.8", 5));
}

#[test]
fn forced_version_pair_drops_source_extension_level() {
    let mut options = WriterOptions::default();
    options.force_version = Some("1.3".into());
    options.force_extension_level = Some(0);
    assert_eq!(effective_pdf_version_and_ext("1.7", 2, &options, false, false), ("1.3", 0));
}
```

Use the existing writer test helper/imports and preserve the current pairwise
tests; these are regression locks for the new parser output.

- [ ] **Step 2: Run the canonical tests and verify RED if a writer gap exists**

Run:

```bash
cargo test -p flpdf version_spec_extension_level
cargo test -p flpdf forced_version_pair_drops_source_extension_level
```

If either fails, trace the failure to the canonical writer function and correct
only the qpdf-derived branch. Do not add CLI-side Catalog mutation.

- [ ] **Step 3: Run qpdf differential probes for all four source shapes**

Build the release CLI and, in a disposable directory, run qpdf and flpdf with
the same `--static-id` plus each `--min-version`/`--force-version` value. Use
the existing `flpdf-test-driver 34` helper to compare version/header,
extension-level, Catalog `/Extensions`, and `PDFVersion`. Include:

```bash
qpdf --static-id --force-version=1.8.5 INPUT q.pdf
target/release/flpdf --static-id --force-version=1.8.5 INPUT f.pdf
target/release/flpdf-test-driver 34 q.pdf
target/release/flpdf-test-driver 34 f.pdf
```

Repeat for `minimal.pdf`, `extensions-adbe.pdf`, `extensions-other.pdf`, and
`extensions-adbe-other.pdf`, then compare the QDF and non-QDF forced-1.8.5
bytes with `cmp` after rebuilding qpdf/flpdf outputs from the same inputs.

- [ ] **Step 4: Commit any canonical writer correction and tests**

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/tests/writer_tests.rs crates/flpdf-cli/tests/cli_tests.rs
git commit -m "test(writer): lock qpdf extension version precedence"
```

### Task 5: Run the authoritative qtest suite and quality gates

**Files:**
- No qtest vendor edits in this repository.
- Evidence: disposable qtest run directory plus paired artifacts.

- [ ] **Step 1: Build all qtest binaries from the implementation worktree**

Run:

```bash
cargo build --release --bin flpdf --bin flpdf-test-compare --bin flpdf-test-driver \
  --bin qpdfjob-ctest --bin qpdf-ctest \
  --bin flpdf-test-pdf-doc-encoding --bin flpdf-test-pdf-unicode \
  --bin flpdf-test-unicode-filenames --bin test_xref --bin test_parsedoffset
```

- [ ] **Step 2: Run `extensions-dictionary.test` from a disposable qtest datadir**

Use `qtest-driver` with `TESTS=extensions-dictionary`, the implementation
worktree's release binaries, and the qtest shims. Keep the resulting
`harness.log`, `qtest-results.xml`, and `qtest.log` together under a unique
`/tmp` directory. Expected result: 156 total, 0 failures, and no unrecognized
version options.

- [ ] **Step 3: Add same-run evidence to the Beads issue**

Read the paired artifacts and record the exact counts and any remaining
non-target failures in `flpdf-25kg.7.2.1` without overwriting the audit note.
The separate `flpdf-qtest` manifest remains a follow-up and is not edited from
this flpdf worktree.

- [ ] **Step 4: Run all local quality gates**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf
cargo test -p flpdf-cli
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' \
  cargo doc --workspace --no-deps --document-private-items
python3 scripts/check-qpdf-deviation-markers.py --check
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
git diff --check
```

- [ ] **Step 5: Review the final diff and close the implementation issue**

Check `git status --short --branch`, inspect the complete diff against
`origin/main`, run `bd dep cycles`, then close with the exact commit and qtest
evidence:

```bash
bd close flpdf-25kg.7.2.1 --reason="Implemented qpdf M.m.E version parsing and top-level/rewrite writer routing; extensions-dictionary.test 156-row evidence and quality gates recorded."
bd dolt push
```

Do not mark the parent `flpdf-25kg.7.2` complete until its other scope rows
are independently classified.
