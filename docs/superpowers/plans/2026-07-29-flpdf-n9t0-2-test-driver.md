# flpdf `test_driver test_0_1` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Rust `flpdf-test-driver` binary that reproduces qpdf 11.9.0
`test_driver` id 1 (`test_0_1`) for the 20 `basic-parsing.test` cases other
than good14.

**Architecture:** Extend the existing `flpdf-qtest-tools` crate with a second
binary and a `driver` library module. A crate-local `Handle` adapts flpdf's
`Option`/`Object` API to qpdf's auto-dereferencing `QPDFObjectHandle`
semantics without changing the public `flpdf` API. Flpdf-authored PDF fixtures
and committed oracle output exercise the binary boundary; a developer-only
differential script rebuilds qpdf's pinned test driver to regenerate/check the
oracle output.

**Tech Stack:** Rust 2021 workspace, `flpdf`, `assert_cmd`, shell, Python 3
standard library, pinned qpdf 11.9.0 source.

## Global Constraints

- qpdf 11.9.0 source and observed output are authoritative.
- Implement id 1 only; id 0, id 3, remaining test functions, and the
  flpdf-qtest shim are out of scope.
- Read the PDF before dispatch lookup, so malformed-input errors beat
  `invalid test N`.
- Open id 1 from memory with `PdfOpenOptions { repair: true, ..Default::default() }`.
- Resolve reference chains through at most 64 hops and retain the first
  indirect reference for `isIndirect()`/`unparse()`.
- Do not copy any qpdf-qtest corpus file into this repository.
- Keep changed executable-line coverage at 100%.

---

### Task 1: Shared CLI helpers

**Files:**
- Create: `crates/flpdf-qtest-tools/src/common.rs`
- Modify: `crates/flpdf-qtest-tools/src/lib.rs`
- Modify: `crates/flpdf-qtest-tools/src/main.rs`
- Modify: `crates/flpdf-qtest-tools/src/output.rs`

**Interfaces:**
- Produces: `common::program_name(argv0: &str) -> &str`
- Produces: `output::write_bytes(out: &mut dyn Write, bytes: &[u8]) -> io::Result<()>`

- [ ] **Step 1: Write failing shared-helper tests**

Create `common.rs` with only the tests below, add `pub mod common;` to
`lib.rs`, and do not define `program_name` yet:

```rust
#[test]
fn program_name_strips_unix_and_windows_paths_and_exe() {
    assert_eq!(program_name("/tmp/flpdf-test-driver"), "flpdf-test-driver");
    assert_eq!(program_name(r"C:\tmp\flpdf-test-driver.exe"), "flpdf-test-driver");
}
```

Add an `output.rs` test with a short-writing sink and assert that
`write_bytes` uses `write_all` semantics.

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test -p flpdf-qtest-tools common::tests
cargo test -p flpdf-qtest-tools output::tests
```

Expected: compilation fails because `common`/`write_bytes` do not exist.

- [ ] **Step 3: Implement the shared helpers**

Move the existing `program_name` implementation out of `main.rs` unchanged:

```rust
pub fn program_name(argv0: &str) -> &str {
    let stem = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0);
    stem.strip_suffix(".exe").unwrap_or(stem)
}
```

Implement:

```rust
pub fn write_bytes(out: &mut dyn Write, bytes: &[u8]) -> io::Result<()> {
    out.write_all(bytes)
}
```

Export `pub mod common;` and import `program_name` from the library in the
existing compare binary. Binary registration waits for Task 3, after its
failing CLI tests exist.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-qtest-tools common::tests output::tests
cargo test -p flpdf-qtest-tools --test cli_usage
```

Expected: all pass; existing compare CLI output remains unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf-qtest-tools
git commit -m "refactor(qtest): share helper binary conventions"
```

### Task 2: QPDFObjectHandle adapter

**Files:**
- Create: `crates/flpdf-qtest-tools/src/driver/mod.rs`
- Create: `crates/flpdf-qtest-tools/src/driver/handle.rs`
- Modify: `crates/flpdf-qtest-tools/src/lib.rs`

**Interfaces:**
- Produces:
  `Handle::from_value<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> flpdf::Result<Handle>`
- Produces:
  `Handle::get_key<R: Read + Seek>(pdf: &mut Pdf<R>, dictionary: &Dictionary, key: &[u8]) -> flpdf::Result<Handle>`
- Produces:
  `Handle::has_key<R: Read + Seek>(pdf: &mut Pdf<R>, dictionary: &Dictionary, key: &[u8]) -> flpdf::Result<bool>`
- Produces:
  `Handle::array_items<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> flpdf::Result<Vec<Handle>>`
- Produces:
  `Handle::dictionary_items<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> flpdf::Result<Vec<(Vec<u8>, Handle)>>`
- Produces: `type_code`, `type_name`, `unparse`, and `unparse_resolved`
- Produces:
  `resolve_stream_dictionary<R: Read + Seek>(pdf: &mut Pdf<R>, source: &Dictionary) -> flpdf::Result<Dictionary>`

- [ ] **Step 1: Write Handle contract tests**

Construct small flpdf-authored PDFs in test helpers and cover:

```rust
assert_eq!(missing.type_code(), 2);
assert_eq!(missing.type_name(), "null");
assert!(!missing.is_indirect());

assert!(first_hop.is_indirect());
assert_eq!(first_hop.as_bool(), Some(true));
assert_eq!(first_hop.unparse(), b"6 0 R");
assert_eq!(first_hop.unparse_resolved(), b"true");
```

Add separate tests for:

- direct null, dangling ref, and real-null ref all making `has_key` false;
- a two-hop reference chain resolving to boolean while `unparse()` retains
  the first hop;
- all supported qpdf type codes/names;
- array/dictionary child indirectness;
- stream `unparse()` and `unparse_resolved()` both returning its object ref;
- `/Filter` and `/DecodeParms` resolution at top-level, array-element, and
  decode-parameter-dictionary-entry positions.

- [ ] **Step 2: Run Handle tests and verify RED**

Run:

```bash
cargo test -p flpdf-qtest-tools driver::handle::tests
```

Expected: compilation fails because `driver::handle` is absent.

- [ ] **Step 3: Implement minimal Handle semantics**

Use owned `Object` clones to avoid holding a `Pdf` borrow while following the
next reference:

```rust
const MAX_REF_CHAIN_DEPTH: usize = 64;

pub(crate) struct Handle {
    resolved: Object,
    indirect: Option<ObjectRef>,
}
```

For each `Object::Reference`, remember only the first ref, call
`pdf.resolve_borrowed(reference)?.clone()`, and continue until the terminal
object. Return an error after 64 hops. Implement qpdf's explicit type table:
null=2, boolean=3, integer=4, real=5, string=6, name=7, array=8,
dictionary=9, stream=10, operator=11, inline-image=12.

For stream dictionaries, clone and recursively resolve:

- the `/Filter` value and every array element;
- the `/DecodeParms` value, every array element, and each dictionary entry
  value;
- every encountered reference chain through the same 64-hop resolver.

- [ ] **Step 4: Run Handle tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-qtest-tools driver::handle::tests
```

Expected: all Handle tests pass with no warnings.

- [ ] **Step 5: Refactor and re-run**

Share a single `resolve_chain` implementation between Handle creation and
stream-dictionary normalization. Re-run the Task 2 command.

- [ ] **Step 6: Commit**

```bash
git add crates/flpdf-qtest-tools/src/driver crates/flpdf-qtest-tools/src/lib.rs
git commit -m "feat(qtest): add qpdf object handle adapter"
```

### Task 3: `test_0_1` dispatch and output

**Files:**
- Create: `crates/flpdf-qtest-tools/src/driver/test_0_1.rs`
- Create: `crates/flpdf-qtest-tools/src/bin/driver.rs`
- Modify: `crates/flpdf-qtest-tools/src/driver/mod.rs`
- Modify: `crates/flpdf-qtest-tools/Cargo.toml`
- Test: `crates/flpdf-qtest-tools/tests/driver_cli.rs`

**Interfaces:**
- Produces: `driver::run(args, stdout, stderr) -> u8`
- Consumes: Task 2 `Handle`
- Binary contract: `test_driver <n> <filename1> [arg2]`

- [ ] **Step 1: Write failing CLI contract tests**

Using `assert_cmd::Command::cargo_bin("flpdf-test-driver")`, assert exact
stdout/stderr and status for:

- too few and too many args: exact `Usage: flpdf-test-driver n filename1 [arg2]\n`, exit 2;
- valid PDF + id 50: exact `invalid test 50\n`, empty stdout, exit 2;
- malformed PDF + id 50: exact parse error only, no `invalid test 50`;
- four-argument id 50: accepted syntax and `invalid test 50`, not Usage;
- id 0: `invalid test 0` (the documented scope boundary).

- [ ] **Step 2: Run CLI tests and verify RED**

Run:

```bash
cargo test -p flpdf-qtest-tools --test driver_cli
```

Expected: compilation fails because the `flpdf-test-driver` binary is not
registered.

- [ ] **Step 3: Implement pre-dispatch open and errors**

Register the binary only after observing RED:

```toml
[[bin]]
name = "flpdf-test-driver"
path = "src/bin/driver.rs"
```

Then implement `run`, which must:

1. validate only argc 3/4;
2. parse `n`;
3. read `argv[2]`;
4. open from memory with repair enabled;
5. emit each repair diagnostic as `WARNING: <argv[2]>: <message>\n`;
6. dispatch id 1 or return `invalid test N`;
7. append `test N done\n` only after successful dispatch.

Flush stdout before every stderr write.

- [ ] **Step 4: Write failing `test_0_1` unit tests**

Drive `test_0_1` through in-memory writers and assert exact bytes for one
fixture per Object branch: null, boolean true/false, integer, real, name,
string, array, dictionary, stream, and unfilterable stream.

- [ ] **Step 5: Run and verify RED**

Run:

```bash
cargo test -p flpdf-qtest-tools driver::test_0_1::tests
```

Expected: failures identify missing branch output.

- [ ] **Step 6: Implement `test_0_1` minimally**

Match qpdf's five-part output exactly. Write raw stream bytes directly,
normalize the stream dictionary through Task 2, call
`flpdf::filters::decode_stream_data`, and emit either decoded bytes plus
`End of stream data` or `Stream data is not filterable.`.

- [ ] **Step 7: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-qtest-tools driver::
cargo test -p flpdf-qtest-tools --test driver_cli
```

Expected: all pass with exact byte assertions.

- [ ] **Step 8: Commit**

```bash
git add crates/flpdf-qtest-tools
git commit -m "feat(qtest): implement test driver test_0_1"
```

### Task 4: Authored fixtures, golden tests, and oracle differential

**Files:**
- Create: `tests/fixtures/test_driver/README.md`
- Create: `tests/fixtures/test_driver/generate.sh`
- Create: `tests/fixtures/test_driver/*.{pdf,out}`
- Create: `crates/flpdf-qtest-tools/tests/driver_goldens.rs`
- Create: `scripts/qpdf-test-driver-diff.sh`

**Interfaces:**
- Consumes: `flpdf-test-driver` from Task 3
- Produces: ordinary Cargo golden coverage and an opt-in pinned-qpdf oracle

- [ ] **Step 1: Add the fixture generator and README**

Generate every PDF listed in design §7 from flpdf-authored object/trailer
bytes with computed xref offsets. Use Python's standard `zlib` only for
Flate/Predictor payloads. Document licensing and the no-vendoring rule.

- [ ] **Step 2: Add failing golden tests**

Enumerate committed `.pdf/.out` pairs. For ordinary fixtures assert success,
empty stderr, and byte-exact stdout. For `repairable_input`, direct stdout and
stderr to the same temporary file descriptor, run from the fixture directory
with basename argv, then assert exit 0 and the exact merged byte stream.

- [ ] **Step 3: Run golden tests and verify RED**

Run:

```bash
cargo test -p flpdf-qtest-tools --test driver_goldens
```

Expected: failures for absent/outdated `.out` files or behavioral mismatches.

- [ ] **Step 4: Add the pinned qpdf differential script**

Follow the repository's existing oracle-script safety pattern:

- resolve the pinned source only through `fetch-qpdf-source.sh --print-path`;
- verify HEAD `3b97c9bd266b7c32ea36d3536e22dab77412886d`;
- refuse dirty source;
- build `qpdf/test_driver.cc` in a private external temporary directory;
- compare qpdf and Rust stdout/stderr/status for every authored fixture;
- leave both repositories and the pinned source unchanged.

- [ ] **Step 5: Generate oracle outputs and verify GREEN**

Run:

```bash
bash scripts/qpdf-test-driver-diff.sh --regenerate
cargo test -p flpdf-qtest-tools --test driver_goldens
bash scripts/qpdf-test-driver-diff.sh --check
```

Expected: all fixture bytes, diagnostics, and statuses match qpdf 11.9.0.

- [ ] **Step 6: Run full quality gates**

Run in order:

```bash
cargo fmt --all -- --check
cargo test -p flpdf-qtest-tools
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo llvm-cov --workspace --features qpdf-zlib-compat --lcov --output-path target/coverage/flpdf-n9t0-2.lcov
bash scripts/patch-coverage.sh --base origin/main --lcov target/coverage/flpdf-n9t0-2.lcov
```

Expected: all commands pass and patch coverage reports zero uncovered changed
executable lines.

- [ ] **Step 7: Commit**

```bash
git add tests/fixtures/test_driver crates/flpdf-qtest-tools/tests/driver_goldens.rs scripts/qpdf-test-driver-diff.sh
git commit -m "test(qtest): verify test driver against qpdf oracle"
```

### Task 5: Final review and publication

**Files:**
- Review all branch changes.

- [ ] **Step 1: Self-review against the design**

Confirm every unmarked acceptance criterion in design §§3, 5, and 7 is
covered, and confirm id 0, test_3, and the qtest shim remain untouched.

- [ ] **Step 2: Run fresh final verification**

Repeat formatting, focused tests, workspace tests, Clippy, oracle check, and
fresh patch coverage from Task 4. Record exact results in the Bead notes.

- [ ] **Step 3: Review the diff**

```bash
git status --short
git diff --check origin/main...HEAD
git diff --stat origin/main...HEAD
```

- [ ] **Step 4: Push and open the implementation PR**

```bash
git push -u origin feat/flpdf-n9t0-2-test-driver
```

Open a PR targeting `main`, wait for required reviews/checks, address
actionable findings, and do not close `flpdf-n9t0.2` until the implementation
is merged and verified on current `main`.
