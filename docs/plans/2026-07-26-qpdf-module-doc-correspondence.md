# qpdf Module Doc Correspondence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Require every `crates/flpdf/src/**/*.rs` module to declare its qpdf
correspondence, generate a deterministic module index, and enforce both the
annotations and generated file in CI.

**Architecture:** A Python-standard-library checker owns parsing, validation,
index rendering, and `--write`/`--check` behavior. Source files remain the
machine-readable input; the generated index is committed output, while
`docs/qpdf-correspondence.md` remains the human-authored architectural map.
Python fixture tests cover checker behavior, and the existing Rust CI workflow
contract suite pins the new CI commands.

**Tech Stack:** Python 3 standard library (`argparse`, `dataclasses`, `pathlib`,
`tempfile`, `unittest`), Rust module inner docs, GitHub Actions YAML, Cargo.

## Global Constraints

- Cover every `*.rs` file below `crates/flpdf/src`, including `lib.rs`, nested
  modules, and `mod.rs`; exclude `crates/flpdf-cli`.
- Require exactly one leading classification:
  `//! Mirrors qpdf 11.9.0 libqpdf/X.cc.` or
  `//! qpdf correspondence: <non-empty reason>.`
- Allow a comma-separated list of `libqpdf/*.cc` files on a `Mirrors` line.
- Do not label smeared, incomplete, substituted, or flpdf-only modules as
  completed mirrors.
- Keep `docs/qpdf-correspondence.md` human-authored; generate
  `docs/qpdf-module-doc-index.md` separately.
- Use only Python's standard library.
- Do not change Rust implementation, visibility, module names, or runtime
  behavior.

---

### Task 1: Build the annotation parser and deterministic index generator

**Files:**
- Create: `scripts/qpdf-module-docs.py`
- Create: `scripts/tests/test_qpdf_module_docs.py`

**Interfaces:**
- Produces:
  - `Classification(kind: str, text: str)`
  - `classify_source(path: Path, source: str) -> Classification`
  - `scan_modules(source_root: Path, repo_root: Path) -> list[tuple[Path, Classification]]`
  - `render_index(entries: list[tuple[Path, Classification]]) -> str`
  - CLI flags `--root`, `--source-root`, `--index`, and mutually exclusive
    `--write`/`--check`
- Consumes: only Python standard-library APIs and Rust source text.

- [ ] **Step 1: Write fixture tests before the checker exists**

Create `scripts/tests/test_qpdf_module_docs.py`. Load
`scripts/qpdf-module-docs.py` with `importlib.util.spec_from_file_location` so
the hyphenated executable filename remains usable. Cover these exact cases:

```python
def test_accepts_single_mirror(self):
    result = self.module.classify_source(
        Path("crates/flpdf/src/pdf_version.rs"),
        "//! Mirrors qpdf 11.9.0 libqpdf/PDFVersion.cc.\n\npub struct V;\n",
    )
    self.assertEqual(("mirror", "libqpdf/PDFVersion.cc"), (result.kind, result.text))

def test_accepts_multiple_mirror_files(self):
    result = self.module.classify_source(
        Path("crates/flpdf/src/json.rs"),
        "//! Mirrors qpdf 11.9.0 libqpdf/JSON.cc, libqpdf/JSONHandler.cc.\n",
    )
    self.assertEqual(
        ("mirror", "libqpdf/JSON.cc, libqpdf/JSONHandler.cc"),
        (result.kind, result.text),
    )

def test_accepts_non_mirror_reason(self):
    result = self.module.classify_source(
        Path("crates/flpdf/src/fonts.rs"),
        "//! qpdf correspondence: flpdf-only font inspection.\n",
    )
    self.assertEqual(
        ("correspondence", "flpdf-only font inspection"),
        (result.kind, result.text),
    )
```

Also assert `ValueError` diagnostics for missing, duplicate, empty-reason,
wrong-version, absolute-path, non-`libqpdf` path, and non-`.cc` path inputs.
Create temporary nested module trees to assert repository-relative lexical
ordering, Markdown escaping for `|` and backticks, and `--check` failure when
the committed index is stale.

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
```

Expected: failure while loading `scripts/qpdf-module-docs.py` because the
checker does not exist.

- [ ] **Step 3: Implement the minimal parser**

Create `scripts/qpdf-module-docs.py` with:

```python
MIRROR_PREFIX = "//! Mirrors qpdf 11.9.0 "
CORRESPONDENCE_PREFIX = "//! qpdf correspondence: "
QPDF_PATH_RE = re.compile(r"libqpdf/[A-Za-z0-9_+-]+\.cc")

@dataclass(frozen=True)
class Classification:
    kind: str
    text: str
```

`classify_source` examines only the leading inner-doc/inner-attribute region
before the first Rust item. It collects both supported prefixes, rejects counts
other than one, strips the final period, validates every comma-separated mirror
path with `fullmatch`, and rejects an empty correspondence reason.

- [ ] **Step 4: Implement scanning, rendering, and CLI modes**

`scan_modules` recursively selects `*.rs`, sorts paths relative to the
repository root, and aggregates every validation error before exiting.

`render_index` returns:

```markdown
# qpdf Module Doc Index

<!-- Generated by `python3 scripts/qpdf-module-docs.py --write`; do not edit. -->

| flpdf module | classification | qpdf correspondence |
|---|---|---|
| `crates/flpdf/src/pdf_version.rs` | mirror | `libqpdf/PDFVersion.cc` |
```

Escape backslashes, `|`, and backticks in cell content. Ensure one terminal
newline. `--write` creates the parent directory and writes UTF-8. `--check`
compares exact bytes and emits a command to regenerate on mismatch.

- [ ] **Step 5: Run the focused test and verify GREEN**

Run:

```bash
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
```

Expected: all checker tests pass.

- [ ] **Step 6: Commit the checker cycle**

```bash
git add scripts/qpdf-module-docs.py scripts/tests/test_qpdf_module_docs.py
git commit -m "test: define qpdf module correspondence checker"
```

### Task 2: Classify every flpdf core module and generate the index

**Files:**
- Modify: every `crates/flpdf/src/**/*.rs` missing a classification
- Create: `docs/qpdf-module-doc-index.md`
- Modify: `docs/qpdf-correspondence.md`

**Interfaces:**
- Consumes: `scripts/qpdf-module-docs.py --write`
- Produces: a complete source annotation inventory and committed generated
  index accepted by `--check`

- [ ] **Step 1: Run the repository checker and verify RED**

Run:

```bash
python3 scripts/qpdf-module-docs.py --check
```

Expected: failure listing existing modules whose leading docs have no
classification, plus the missing generated index.

- [ ] **Step 2: Classify modules from current evidence**

For each `crates/flpdf/src/**/*.rs` file, read its leading docs, its row in
`docs/qpdf-correspondence.md`, and relevant implementation names before
editing.

Use `Mirrors` only for a completed component boundary already supported by the
correspondence map, such as:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/PDFVersion.cc.
//! Mirrors qpdf 11.9.0 libqpdf/QPDFOutlineDocumentHelper.cc, libqpdf/QPDFOutlineObjectHelper.cc.
```

Use specific non-mirror reasons elsewhere:

```rust
//! qpdf correspondence: QPDFWriter.cc responsibilities shared across writer modules.
//! qpdf correspondence: QPDFJob.cc page-operation responsibility; not a standalone component mirror.
//! qpdf correspondence: Rust crate substitution for qpdf crypto primitives.
//! qpdf correspondence: flpdf-only standard font metrics.
```

Do not use a generic line that omits the known qpdf component. Preserve
existing module documentation and place the classification in its leading
inner-doc block.

- [ ] **Step 3: Generate the index**

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
```

Add a short “Machine-readable module index” paragraph near the introduction of
`docs/qpdf-correspondence.md` linking to `qpdf-module-doc-index.md` and stating
that the generated index records source annotations but does not replace the
manual responsibility/status map.

- [ ] **Step 4: Run the repository checker and verify GREEN**

Run:

```bash
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
```

Expected: both commands exit zero.

- [ ] **Step 5: Verify the bulk edit is documentation-only**

Run:

```bash
git diff --word-diff=porcelain HEAD -- crates/flpdf/src
git diff --check
```

Inspect every non-context addition under `crates/flpdf/src`; each addition must
begin with `//!`, and there must be no removed Rust code or changed item
signatures.

- [ ] **Step 6: Commit the source inventory**

```bash
git add crates/flpdf/src docs/qpdf-module-doc-index.md docs/qpdf-correspondence.md
git commit -m "docs: classify flpdf modules by qpdf correspondence"
```

### Task 3: Enforce the checker in CI

**Files:**
- Modify: `crates/flpdf-cli/tests/ci_workflow_contract.rs`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Produces: a CI contract requiring both checker unit tests and repository drift
  validation in the Quality job
- Consumes: the exact commands from the approved design.

- [ ] **Step 1: Add the failing CI contract test**

Add:

```rust
#[test]
fn quality_checks_qpdf_module_correspondence() {
    let workflow = include_str!("../../../.github/workflows/ci.yml");
    assert!(
        workflow.contains("python3 -m unittest scripts/tests/test_qpdf_module_docs.py"),
        "quality job must run qpdf module checker tests"
    );
    assert!(
        workflow.contains("python3 scripts/qpdf-module-docs.py --check"),
        "quality job must reject missing annotations and stale generated output"
    );
}
```

- [ ] **Step 2: Run the focused Rust test and verify RED**

Run:

```bash
cargo test -p flpdf-cli --test ci_workflow_contract quality_checks_qpdf_module_correspondence
```

Expected: failure because the workflow does not contain the checker commands.

- [ ] **Step 3: Add the Quality-job step**

Immediately after checkout in `.github/workflows/ci.yml`, add:

```yaml
      - name: Check qpdf module correspondence
        run: |
          python3 -m unittest scripts/tests/test_qpdf_module_docs.py
          python3 scripts/qpdf-module-docs.py --check
```

- [ ] **Step 4: Run focused CI tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-cli --test ci_workflow_contract
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
```

Expected: all commands exit zero.

- [ ] **Step 5: Commit the CI gate**

```bash
git add .github/workflows/ci.yml crates/flpdf-cli/tests/ci_workflow_contract.rs
git commit -m "ci: check qpdf module correspondence"
```

### Task 4: Verify all repository gates and finish the bead

**Files:**
- Modify through Beads: `flpdf-qxba.3`
- No source changes expected.

**Interfaces:**
- Consumes: final committed branch state
- Produces: fresh evidence for every required quality gate and persisted Beads
  state

- [ ] **Step 1: Run focused checker gates**

```bash
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
cargo test -p flpdf-cli --test ci_workflow_contract
```

- [ ] **Step 2: Run formatting and documentation gates**

```bash
cargo fmt --all -- --check
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
```

- [ ] **Step 3: Run lint and workspace tests**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
```

- [ ] **Step 4: Measure changed-line coverage from final HEAD**

Commit any final corrections first, then run:

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: patch coverage gate reports 100% for changed executable Rust lines.

- [ ] **Step 5: Verify branch scope and cleanliness**

```bash
git status --short
git diff --check origin/main...HEAD
git diff --stat origin/main...HEAD
git log --oneline origin/main..HEAD
```

Confirm the diff contains only the design/plan, checker/tests, module docs,
generated index, correspondence-doc link, CI contract, and workflow gate.

- [ ] **Step 6: Close and publish tracker state**

```bash
bd close flpdf-qxba.3 --reason="Implemented source annotations, generated module index, and CI drift checks"
bd dolt push
```

- [ ] **Step 7: Rebase and push the implementation branch**

```bash
git pull --rebase
git push -u origin feature/flpdf-qxba-3-qpdf-module-docs
```

Do not report completion until both pushes succeed.

---

## Review Follow-up

### Task 5: Make character-literal scanning independent of Python XID tables

**Files:**
- Modify: `scripts/qpdf-module-docs.py`
- Test: `scripts/tests/test_qpdf_module_docs.py`

**Interfaces:**
- Consumes: an apostrophe offset inside `_advance_inner_attribute`
- Produces:
  - `_character_literal_end(line: str, quote_offset: int) -> int | None`
  - an exclusive end offset only for one Rust character payload followed
    immediately by the closing apostrophe

- [ ] **Step 1: Add failing scanner regressions**

Replace the existing Unicode lifetime fixture with the Rust-accepted U+088F
case and add a positive character-literal case:

```python
def test_accepts_classification_after_inner_attribute_with_newer_xid_lifetime(self):
    result = self.module.classify_source(
        Path("crates/flpdf/src/lib.rs"),
        "#![doc = stringify!('࢏)] // lifetime's spelling\n"
        "//! qpdf correspondence: crate root.\n\n"
        "pub mod x;\n",
    )

    self.assertEqual(
        ("correspondence", "crate root"),
        (result.kind, result.text),
    )

def test_accepts_classification_after_inner_attribute_with_bracket_char_literal(self):
    result = self.module.classify_source(
        Path("crates/flpdf/src/lib.rs"),
        "#![doc = stringify!(']')]\n"
        "//! qpdf correspondence: crate root.\n\n"
        "pub mod x;\n",
    )

    self.assertEqual(
        ("correspondence", "crate root"),
        (result.kind, result.text),
    )
```

- [ ] **Step 2: Run the lifetime regression and verify RED**

Run:

```bash
python3 -m unittest \
  scripts.tests.test_qpdf_module_docs.ClassificationTests.test_accepts_classification_after_inner_attribute_with_newer_xid_lifetime
```

Expected: failure with the missing-classification diagnostic because Python
3.12 does not recognize U+088F as an identifier start and the scanner consumes
the apostrophe in `lifetime's` as a false closing quote.

- [ ] **Step 3: Recognize only complete character literals**

Remove `_is_xid_start` and `_is_xid_continue`. Add
`_character_literal_end`, which accepts one source character or one supported
Rust escape and requires the closing apostrophe immediately after it:

```python
def _character_literal_end(line: str, quote_offset: int) -> int | None:
    payload_offset = quote_offset + 1
    if payload_offset >= len(line):
        return None

    char = line[payload_offset]
    if char in {"'", "\n", "\r"}:
        return None
    if char != "\\":
        closing_offset = payload_offset + 1
    elif payload_offset + 1 >= len(line):
        return None
    elif line[payload_offset + 1] in {"n", "r", "t", "\\", "0", "'", '"'}:
        closing_offset = payload_offset + 2
    elif line[payload_offset + 1] == "x":
        digits = line[payload_offset + 2 : payload_offset + 4]
        if len(digits) != 2 or any(digit not in "0123456789abcdefABCDEF" for digit in digits):
            return None
        closing_offset = payload_offset + 4
    elif line.startswith("\\u{", payload_offset):
        brace_offset = line.find("}", payload_offset + 3)
        if brace_offset < 0:
            return None
        digits = line[payload_offset + 3 : brace_offset].replace("_", "")
        if not 1 <= len(digits) <= 6 or any(
            digit not in "0123456789abcdefABCDEF" for digit in digits
        ):
            return None
        closing_offset = brace_offset + 1
    else:
        return None

    if closing_offset >= len(line) or line[closing_offset] != "'":
        return None
    return closing_offset + 1
```

In `_advance_inner_attribute`, retain byte-character support by deriving
`quote_offset` from `b'`, call this helper, and advance only when it returns an
offset:

```python
quote_offset = offset + 1 if line.startswith("b'", offset) else offset
if quote_offset < len(line) and line[quote_offset] == "'":
    literal_end = _character_literal_end(line, quote_offset)
    if literal_end is not None:
        offset = literal_end
        continue
```

- [ ] **Step 4: Run focused and complete Python tests**

Run:

```bash
python3 -m unittest \
  scripts.tests.test_qpdf_module_docs.ClassificationTests.test_accepts_classification_after_inner_attribute_with_newer_xid_lifetime \
  scripts.tests.test_qpdf_module_docs.ClassificationTests.test_accepts_classification_after_inner_attribute_with_bracket_char_literal
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
```

Expected: every command exits zero.

- [ ] **Step 5: Commit the scanner fix**

```bash
git add scripts/qpdf-module-docs.py scripts/tests/test_qpdf_module_docs.py
git commit -m "fix: parse Rust character literals without Python XID"
```

### Task 6: Reject workflow-level default shells in the Quality contract

**Files:**
- Modify and test: `crates/flpdf-cli/tests/ci_workflow_contract.rs`

**Interfaces:**
- Consumes:
  - the complete workflow YAML
  - an exact Quality-job command
- Produces:
  - `quality_workflow_contains_exact_command(workflow: &str, command: &str) -> bool`
  - `false` when workflow-level `defaults.run.shell` changes inherited shell
    execution

- [ ] **Step 1: Add the failing workflow regression**

Add:

```rust
#[test]
fn quality_command_contract_rejects_workflow_default_shell() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
defaults:
  run:
    shell: echo {{0}}
jobs:
  quality:
    steps:
      - run: {command}
"
    );

    assert!(!quality_workflow_contains_exact_command(
        &workflow, command
    ));
}
```

- [ ] **Step 2: Run the regression and verify RED**

Run:

```bash
cargo test -p flpdf-cli --test ci_workflow_contract \
  quality_command_contract_rejects_workflow_default_shell
```

Expected: compile failure because
`quality_workflow_contains_exact_command` does not exist.

- [ ] **Step 3: Add the workflow-to-job contract boundary**

Add the wrapper beside `quality_job_body`:

```rust
fn quality_workflow_contains_exact_command(workflow: &str, command: &str) -> bool {
    !job_has_default_run_shell(workflow, 0)
        && workflow_contains_exact_command(quality_job_body(workflow), command)
}
```

Update both assertions in `quality_checks_qpdf_module_correspondence` to pass
the complete `CI_WORKFLOW` through this wrapper:

```rust
assert!(quality_workflow_contains_exact_command(
    CI_WORKFLOW,
    "python3 -m unittest scripts/tests/test_qpdf_module_docs.py"
));
assert!(quality_workflow_contains_exact_command(
    CI_WORKFLOW,
    "python3 scripts/qpdf-module-docs.py --check"
));
```

- [ ] **Step 4: Run focused and complete Rust contract tests**

Run:

```bash
cargo test -p flpdf-cli --test ci_workflow_contract \
  quality_command_contract_rejects_workflow_default_shell
cargo test -p flpdf-cli --test ci_workflow_contract
```

Expected: both commands exit zero.

- [ ] **Step 5: Commit the workflow contract fix**

```bash
git add crates/flpdf-cli/tests/ci_workflow_contract.rs
git commit -m "fix: include workflow defaults in module docs contract"
```

### Task 7: Verify and publish the review fixes

**Files:**
- No source changes expected.

**Interfaces:**
- Consumes: the final committed branch
- Produces: fresh local verification plus pushed Git and Beads state

- [ ] **Step 1: Run focused behavior and repository checks**

```bash
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
cargo test -p flpdf-cli --test ci_workflow_contract
cargo fmt --all -- --check
```

- [ ] **Step 2: Run workspace quality gates**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
```

- [ ] **Step 3: Measure final changed-line coverage**

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: patch coverage reports 100% for executable Rust lines changed by the
pull request.

- [ ] **Step 4: Verify scope and working-tree cleanliness**

```bash
git diff --check origin/main...HEAD
git diff --stat origin/main...HEAD
git status --short
```

- [ ] **Step 5: Publish Beads and Git state**

```bash
bd dolt push
git pull --rebase
git push
```

Do not reply to or resolve GitHub review threads in this execution phase.
