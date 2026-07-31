# qtest Character-Encoding Helpers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port qpdf 11.9.0 `test_pdf_doc_encoding` and `test_pdf_unicode` to Rust, wire their historical command names into flpdf-qtest, and make all three owned `character-encoding.test` invocations execute the Rust production path.

**Architecture:** The `flpdf` crate keeps one canonical `pdf_string` implementation of qpdf PDF-string decoding, Unicode-string construction, and forced-binary unparsing. The two dedicated binaries call that domain API directly; `character_encoding` owns the qtest input/output contract without a delegation-only string adapter. A pinned-qpdf differential script proves the helper boundary, while a separate flpdf-qtest branch owns PATH shims, release-build resolution, ledger transitions, and the before/after full survey.

**Tech Stack:** Rust 2021, `flpdf`, `flpdf-qtest-tools`, Bash, Python 3 `unittest`, Cargo, CMake, pinned qpdf 11.9.0 commit `3b97c9bd266b7c32ea36d3536e22dab77412886d`.

## Global Constraints

- qpdf 11.9.0 source and observed output are authoritative.
- Own exactly the three helper invocations in `character-encoding.test`; the fourth `qpdf --list-attachments` invocation is out of scope.
- Do not copy qpdf-qtest inputs or goldens into the flpdf repository.
- Keep PDF string semantics in the ordinary `flpdf::pdf_string` domain API; do not add a delegation-only qtest string adapter.
- Preserve qpdf line reading: split at LF, strip one immediately preceding CR, keep a final unterminated line, and do not synthesize a line after a terminal LF.
- Preserve qpdf `newUnicodeString`: PDFDocEncoding only for a lossless non-BOM-looking input; otherwise UTF-16BE with BOM and qpdf-compatible malformed-UTF-8 replacement/consumption.
- Preserve qpdf usage output and Linux failure behavior, including SIGABRT after an uncaught input-open/read exception.
- Reach fresh 100% changed executable-line coverage for each flpdf PR diff.
- Record full qtest survey snapshots before and after wiring with zero allowlist regression.
- Keep the dirty `/home/ubuntu/flpdf-qtest` main checkout untouched; use an isolated `/tmp` worktree for that repository.

---

### Task 1: Canonical PDF string domain

**Files:**
- Create: `crates/flpdf/src/pdf_string.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf/src/object.rs`

**Interfaces:**
- Produces: `pdf_string::utf8_value(stored: &[u8]) -> Vec<u8>`
- Produces: `pdf_string::new_unicode_string(utf8: &[u8]) -> Vec<u8>`
- Produces: `pdf_string::unparse_binary(stored: &[u8]) -> Vec<u8>`
- Consumes: the existing `object` hex writer; all PDF string conversion remains in this module.

- [ ] **Step 1: Write failing adapter tests**

Create `pdf_string.rs` with tests first. Use hand-derived literals for:

```rust
assert_eq!(utf8_value(&[0x80]), "•".as_bytes());
assert_eq!(new_unicode_string("ASCII".as_bytes()), b"ASCII");
assert_eq!(
    new_unicode_string("🥔".as_bytes()),
    b"\xfe\xff\xd8\x3e\xdd\x54"
);
assert_eq!(
    new_unicode_string("þÿ".as_bytes()),
    b"\xfe\xff\x00\xfe\x00\xff"
);
assert_eq!(
    new_unicode_string(b"\xfeafter"),
    b"\xfe\xff\xff\xfd\x00a\x00f\x00t\x00e\x00r"
);
assert_eq!(unparse_binary(b"A\n\x80"), b"<410a80>");
```

The `þÿ` case catches the existing false PDFDocEncoding choice that would create a stored string beginning with the UTF-16BE BOM. The malformed sequence catches replacement grouping drift.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p flpdf pdf_string
```

Expected: compilation fails because `pdf_string` and its functions do not exist. After adding only wrappers, the `þÿ` assertion must still fail against the current encoder.

- [ ] **Step 3: Implement the minimal domain module**

In `lib.rs` add:

```rust
pub mod pdf_string;
```

The domain module owns the PDFDocEncoding table, qpdf UTF-8 traversal,
Unicode-string construction, and forced binary serialization. It must contain
the only implementation of those operations.

- [ ] **Step 4: Correct canonical Unicode construction**

Refactor qpdf UTF-8 traversal to return both normalized UTF-8 and whether any decoder error occurred. `new_unicode_string` must:

1. normalize with the qpdf traversal;
2. reject PDFDocEncoding when traversal reported an error;
3. reject PDFDocEncoding when the original input would encode to `fe ff`, `ff fe`, or `ef bb bf`;
4. reject it when any normalized scalar lacks a PDFDocEncoding byte;
5. otherwise return PDFDocEncoding;
6. on rejection, UTF-16BE-encode the normalized value with BOM.

Do not change JSON behavior except where the same canonical construction function is already consumed.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf pdf_string
cargo test -p flpdf nntree::tests::name_codec_matches_qpdf_utf8_value_and_new_unicode_string
```

Expected: all adapter and existing JSON string tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/pdf_string.rs crates/flpdf/src/json_inspect.rs crates/flpdf/src/nntree.rs crates/flpdf/src/outline_document_helper.rs
git commit -m "refactor(flpdf): own PDF string semantics in domain module"
```

### Task 2: Dedicated Rust helper binaries

**Files:**
- Create: `crates/flpdf-qtest-tools/src/character_encoding.rs`
- Create: `crates/flpdf-qtest-tools/src/bin/pdf_doc_encoding.rs`
- Create: `crates/flpdf-qtest-tools/src/bin/pdf_unicode.rs`
- Modify: `crates/flpdf-qtest-tools/src/lib.rs`
- Modify: `crates/flpdf-qtest-tools/Cargo.toml`
- Test: `crates/flpdf-qtest-tools/tests/character_encoding_cli.rs`

**Interfaces:**
- Produces:
  `character_encoding::run_pdf_doc_encoding(args, stdout, stderr) -> RunOutcome`
- Produces:
  `character_encoding::run_pdf_unicode(args, stdout, stderr) -> RunOutcome`
- Produces binaries `flpdf-test-pdf-doc-encoding` and `flpdf-test-pdf-unicode`.
- Consumes Task 1 `flpdf::pdf_string` directly; `character_encoding` owns only the helper process contract.

- [ ] **Step 1: Write failing binary-boundary tests**

Create integration tests that write flpdf-authored byte fixtures into a temp directory. Assert:

- PDFDoc input `b"plain\r\n\x80 bullet\rbare\nlast"` emits
  `b"plain\n\xe2\x80\xa2 bullet\rbare\nlast\n"`;
- Unicode input containing ASCII, Euro, potato, `þÿ`, and malformed UTF-8 emits exact `<lowercase-hex>` binary unparses;
- a trailing LF does not create an extra output line;
- empty and blank-line files remain distinct;
- too few/many arguments print `Usage: <argv0> infile\n` to stderr and exit 2;
- a missing path and a directory input emit the two measured qpdf exception diagnostics and terminate with SIGABRT on Linux;
- non-UTF-8 argv0/input path diagnostics remain raw bytes.

Use `std::os::unix::process::ExitStatusExt::signal()` for SIGABRT assertions instead of converting the signal to a fabricated exit code.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p flpdf-qtest-tools --test character_encoding_cli
```

Expected: tests fail because neither Cargo binary is registered.

- [ ] **Step 3: Implement shared line and CLI behavior**

`character_encoding.rs` owns one byte-oriented LF/CR line iterator and one argv validator. Both runners:

1. derive whoami using qpdf's forward-slash-only basename rule;
2. require exactly one input argument;
3. read bytes without UTF-8 conversion;
4. transform each line through Task 1;
5. write one LF after every transformed line.

Represent fatal reads as:

```rust
pub enum RunOutcome {
    Exit(u8),
    Abort(Vec<u8>),
}
```

The binary writes `Abort` bytes to stderr, flushes, then calls
`std::process::abort()`. Missing-open diagnostics name `QPDFSystemError` and
include the native Linux strerror text; post-open read errors name
`std::runtime_error` and use `failure reading character from file`.

- [ ] **Step 4: Register and implement both binaries**

Add:

```toml
[[bin]]
name = "flpdf-test-pdf-doc-encoding"
path = "src/bin/pdf_doc_encoding.rs"

[[bin]]
name = "flpdf-test-pdf-unicode"
path = "src/bin/pdf_unicode.rs"
```

Each `main` only gathers `args_os`, locks stdout/stderr, delegates, and maps
`RunOutcome`.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-qtest-tools --test character_encoding_cli
cargo test -p flpdf-qtest-tools character_encoding::
```

Expected: exact byte/status/side-effect assertions pass.

- [ ] **Step 6: Refactor and re-run**

Remove duplicated argv, input-read, output-write, abort, and line-loop code.
Keep the transform itself as the only mode-specific operation, then repeat
Step 5.

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf-qtest-tools
git commit -m "feat(qtest): port character encoding helpers"
```

### Task 3: Pinned-qpdf differential

**Files:**
- Create: `scripts/qpdf-character-encoding-diff.sh`
- Create: `scripts/tests/qpdf-character-encoding-diff-contract.sh`

**Interfaces:**
- Consumes both Task 2 binaries.
- Builds qpdf targets `test_pdf_doc_encoding` and `test_pdf_unicode`.
- Produces one reproducible `--check` command with byte/status comparisons.

- [ ] **Step 1: Write a failing script-contract test**

Use a private fake pinned source, fake `cmake`, and fake `cargo` like the
existing tokenizer/test-driver script contracts. Assert that the script:

- resolves qpdf only through `scripts/fetch-qpdf-source.sh --print-path`;
- verifies exact pinned HEAD and a clean tree;
- uses a mode-0700 `/tmp/flpdf-qpdf-character-encoding.*` build directory;
- requests both qpdf targets and both Rust binaries;
- runs authored normal, CRLF, blank/final-line, usage, missing-open, and
  directory-read probes;
- compares merged bytes and statuses;
- refuses a symlink/escaped cleanup target.

- [ ] **Step 2: Run contract test and verify RED**

Run:

```bash
bash scripts/tests/qpdf-character-encoding-diff-contract.sh
```

Expected: failure because the differential script is absent.

- [ ] **Step 3: Implement the differential script**

Follow `scripts/qpdf-test-driver-diff.sh` safety checks. Generate all probe
inputs inside the private build directory; do not read or copy flpdf-qtest
fixtures. Configure qpdf with its default implicit crypto provider, build the
two helper targets, build the two Rust binaries with a private
`CARGO_TARGET_DIR`, and compare merged stdout/stderr plus status/signal
semantics.

- [ ] **Step 4: Verify contract and live oracle**

Run:

```bash
bash scripts/tests/qpdf-character-encoding-diff-contract.sh
bash scripts/qpdf-character-encoding-diff.sh --check
```

Expected: the contract passes and all live qpdf/Rust probes match.

- [ ] **Step 5: Commit**

```bash
git add scripts/qpdf-character-encoding-diff.sh scripts/tests/qpdf-character-encoding-diff-contract.sh
git commit -m "test(qtest): diff character helpers against qpdf"
```

### Task 4: flpdf quality gates and publication

**Files:**
- Review all flpdf branch changes.

- [ ] **Step 1: Run focused and workspace gates**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf-qtest-tools --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features -- -D warnings
bash scripts/qpdf-character-encoding-diff.sh --check
```

- [ ] **Step 2: Run fresh changed-line coverage**

Run:

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --all-features --lcov --output-path target/coverage/flpdf-egzr-2.lcov
bash scripts/patch-coverage.sh --base origin/main --lcov target/coverage/flpdf-egzr-2.lcov
```

Expected: 100% executable changed-line coverage.

- [ ] **Step 3: Self-review and push**

Run:

```bash
git status --short
git diff --check origin/main...HEAD
git diff --stat origin/main...HEAD
git push -u origin feat/flpdf-egzr-2-character-encoding
```

Open the flpdf implementation PR against `main`. Do not close the Bead yet;
the qtest consumer route remains required.

### Task 5: flpdf-qtest isolated baseline and RED wiring tests

**Files:**
- Create an isolated worktree at `/tmp/flpdf-qtest-egzr-2`.
- Modify tests under `scripts/tests/` before production runner/shim files.

**Interfaces:**
- Consumes the built Task 2 binaries from the flpdf worktree.
- Produces failing tests for both historical command routes.

- [ ] **Step 1: Create the isolated qtest worktree**

From `/home/ubuntu/flpdf-qtest`, fetch/prune and create branch
`feat/flpdf-egzr-2-character-encoding-shims` at `origin/main` in a fresh
`/tmp/flpdf-qtest-egzr-2` worktree. Do not alter the dirty main checkout.

- [ ] **Step 2: Record the before full-survey snapshot**

Build the current required flpdf binaries and run:

```bash
QTEST_FULL=1 \
FLPDF_CLI_BIN=/home/ubuntu/flpdf/.worktrees/flpdf-egzr-2-character-encoding/target/release/flpdf \
FLPDF_TEST_COMPARE_BIN=/home/ubuntu/flpdf/.worktrees/flpdf-egzr-2-character-encoding/target/release/flpdf-test-compare \
FLPDF_TEST_DRIVER_BIN=/home/ubuntu/flpdf/.worktrees/flpdf-egzr-2-character-encoding/target/release/flpdf-test-driver \
./scripts/run.sh
```

Record qpdf/flpdf/qtest pins, total/parser counts, applicable denominator,
passing count, failure clusters, and allowlist regressions in the Bead notes.
The three owned rows must still be blocked before shim wiring.

- [ ] **Step 3: Write failing runner and shim tests**

Add execution tests that use fake binaries and assert:

- `run.sh` resolves/exports
  `FLPDF_TEST_PDF_DOC_ENCODING_BIN` and `FLPDF_TEST_PDF_UNICODE_BIN`;
- `FLPDF_DIR` triggers a release build containing both `--bin` arguments;
- either missing/non-executable helper fails closed before qtest execution;
- `shim/test_pdf_doc_encoding` and `shim/test_pdf_unicode` forward argv,
  stdout, stderr, and exit status to the correct binary;
- both shims use normalization only for stderr and reject an unset binary env.

- [ ] **Step 4: Run tests and verify RED**

Run:

```bash
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
```

Expected: new helper resolution/shim tests fail because the variables, build
arguments, and shim files are absent.

### Task 6: flpdf-qtest production wiring and survey

**Files:**
- Create: `shim/test_pdf_doc_encoding`
- Create: `shim/test_pdf_unicode`
- Modify: `scripts/run.sh`
- Modify: `README.md`
- Modify: `scripts/tests/test_run_contract.py`
- Modify: `scripts/tests/test_run_execution.py`
- Create: `scripts/tests/test_character_encoding_shims.py`
- Modify after successful survey: `parity/qtest-11.9.0.jsonl`

**Interfaces:**
- Produces historical commands `test_pdf_doc_encoding` and `test_pdf_unicode`
  at the front of qtest `PATH`.
- Consumes `FLPDF_TEST_PDF_DOC_ENCODING_BIN` and
  `FLPDF_TEST_PDF_UNICODE_BIN`.

- [ ] **Step 1: Implement runner resolution and release build**

Mirror the existing test-driver resolution order:

1. explicit environment variable;
2. `${FLPDF_DIR}/target/release/flpdf-test-pdf-doc-encoding` or
   `${FLPDF_DIR}/target/release/flpdf-test-pdf-unicode`, and mark
   `need_build=1`;
3. repository-local built binary;
4. otherwise fail with exit 2.

Add both binaries to the one release `cargo build` command, export both
variables, and include them in the executable preflight loop.

- [ ] **Step 2: Add delegating shims and documentation**

Each shim validates its specific env variable, preserves argv, delegates with
`exec`, and applies `FLPDF_QTEST_NORMALIZE` to stderr exactly like
`shim/test_driver`. Update README build/run examples and PATH-shadowing
documentation.

- [ ] **Step 3: Run qtest repository tests and verify GREEN**

Run:

```bash
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
```

Expected: all runner and shim tests pass.

- [ ] **Step 4: Build release binaries and run the three owned invocations**

Build all five required binaries from the flpdf worktree, then run the full
`character-encoding.test` through the qtest harness. Confirm subtests 1–3 pass
and subtest 4 retains its independently owned state.

- [ ] **Step 5: Run the after full survey**

Repeat Task 5 Step 2 with both new binary variables set. Require:

- total/parser/classified counts remain internally consistent;
- `character-encoding 1`, `2`, and `3` are ordinary PASS;
- allowlist regressions remain zero;
- no other `passing`, `blocked`, or `failing` row changes unexpectedly.

Only after this successful same-run evidence, change the three rows to
`state:"passing"` and clear `rationale`, `owner`, `bead`, and
`replacement_ref`.

- [ ] **Step 6: Re-run manifest verification and tests**

Run:

```bash
python3 scripts/verify-parity-manifest.py \
  harness.log qtest-results.xml parity/qtest-11.9.0.jsonl
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
git diff --check origin/main...HEAD
```

- [ ] **Step 7: Commit and push**

```bash
git add README.md scripts/run.sh scripts/tests shim/test_pdf_doc_encoding shim/test_pdf_unicode parity/qtest-11.9.0.jsonl
git commit -m "feat(qtest): wire character encoding helpers"
git push -u origin feat/flpdf-egzr-2-character-encoding-shims
```

Open the flpdf-qtest consumer PR against `main`.

### Task 7: Final issue lifecycle

**Files:**
- Review both repository diffs and PR checks.

- [ ] **Step 1: Record exact evidence**

Add Bead notes with both branch/PR URLs, qpdf pin, flpdf pin, qtest pin,
focused differential result, full-survey before/after counts, and fresh 100%
changed-line coverage.

- [ ] **Step 2: Persist tracker and git state**

Run:

```bash
bd dolt push
git status --short --branch
git push
```

- [ ] **Step 3: Close only after merge**

Do not close `flpdf-egzr.2` while either PR is unmerged. After both merge,
fetch/prune, prove both feature heads are ancestors of their respective
`main`, re-run the focused helper differential and qtest survey on merged
heads, close with the concrete evidence, and `bd dolt push`.
