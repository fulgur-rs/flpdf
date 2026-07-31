# qtest String Adapter Boundary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the qtest string adapter out of `flpdf` while keeping qpdf 11.9.0 PDF-string semantics in one normal `flpdf::pdf_string` domain module.

**Architecture:** Extract the existing canonical PDFDocEncoding, UTF-8 normalization, Unicode-string construction, and forced-binary serialization into `crates/flpdf/src/pdf_string.rs`. `character_encoding` calls that ordinary domain API directly and owns the qpdf test-binary input/output contract; do not add a delegation-only qtest string module. Update all core consumers and correspondence documentation to reflect the qpdf `libqpdf` versus `qpdf` split.

**Tech Stack:** Rust 2021 workspace, `flpdf`, `flpdf-qtest-tools`, qpdf 11.9.0 pinned source and helper binaries, Cargo tests, Clippy, `scripts/qpdf-character-encoding-diff.sh`, `scripts/qpdf-module-docs.py`, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

---

### Task 1: Extract the canonical PDF string domain module

**Files:**
- Create: `crates/flpdf/src/pdf_string.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf/src/nntree.rs`
- Modify: `crates/flpdf/src/outline_document_helper.rs`
- Test: `crates/flpdf/src/pdf_string.rs`

The new module corresponds to qpdf's `libqpdf/QPDF_String.cc`, not to the
qpdf test programs. It must expose these ordinary domain functions:

```rust
pub fn utf8_value(stored: &[u8]) -> Vec<u8>;
pub fn new_unicode_string(utf8: &[u8]) -> Vec<u8>;
pub fn unparse_binary(stored: &[u8]) -> Vec<u8>;
```

Keep the normalized-value helper needed by name-tree code crate-visible inside
`pdf_string`; do not expose a qtest-specific module or feature-gated API.

- [ ] **Step 1: Add the failing core-domain tests first**

Create `pdf_string.rs` with the test module and the public function signatures
absent. Move the existing semantic tests from
`crates/flpdf/src/qtest_string.rs` and `json_inspect` into this test module.
The minimum assertions are:

```rust
assert_eq!(utf8_value(&[0x80]), "•".as_bytes());
assert_eq!(utf8_value(&[0xfe, 0xff, 0x54, 0x0d, 0x52, 0x4d]), "名前".as_bytes());
assert_eq!(utf8_value(&[0xef, 0xbb, 0xbf, 0xff]), &[0xff]);
assert_eq!(new_unicode_string(b"ASCII"), b"ASCII");
assert_eq!(new_unicode_string("🥔".as_bytes()), b"\xfe\xff\xd8\x3e\xdd\x54");
assert_eq!(new_unicode_string("þÿ".as_bytes()), b"\xfe\xff\x00\xfe\x00\xff");
assert_eq!(new_unicode_string(b"\xfeafter"), b"\xfe\xff\xff\xfd\x00a\x00f\x00t\x00e\x00r");
assert_eq!(unparse_binary(b"A\n\x80"), b"<410a80>");
```

Also move the malformed UTF-8 traversal assertions currently in
`nntree.rs` so the normalization behavior is tested by the owning module:
`[0xc2, b'A'] -> "�A"`, `[0xc0, 0x80] -> "�"`, `[0xc2] -> "�"`,
`[0x80] -> "�"`, and the five-byte form
`[0xf8, 0x88, 0x80, 0x80, 0x80] -> "�"`.

- [ ] **Step 2: Run the focused test and verify the intended RED state**

Run:

```bash
cargo test -p flpdf pdf_string
```

Expected: compilation fails because `pdf_string` has tests referring to the
three not-yet-defined functions. Do not copy implementation code before this
failure is observed.

- [ ] **Step 3: Move the canonical implementation without changing semantics**

Move the implementation currently in `json_inspect.rs` as one unit:

1. Move `PDFDOC_ENCODING` and `build_pdfdoc_table`.
2. Rename `qpdf_utf8_value` to `utf8_value`.
3. Move `qpdf_new_unicode_utf8_value` and its private normalization helper
   under the crate-visible name `normalized_utf8_value`.
4. Rename `qpdf_unicode_string_bytes` to `new_unicode_string`.
5. Keep the existing malformed UTF-8 replacement rules, BOM-looking input
   checks, PDFDocEncoding representability search, and UTF-16BE fallback
   unchanged.
6. Implement `unparse_binary` by calling the existing
   `crate::object::write_hex_string` into a `Vec<u8>`; do not duplicate hex
   digit logic in qtest-tools.

Declare the module in `lib.rs` with ordinary visibility:

```rust
pub mod pdf_string;
```

Remove the `qtest-driver`-gated `qtest_string` declaration. Delete the old
`crates/flpdf/src/qtest_string.rs` only after its tests and implementation
have been moved.

- [ ] **Step 4: Route existing core consumers through `pdf_string`**

Update call sites exactly as follows:

```rust
// nntree.rs
use crate::pdf_string::{normalized_utf8_value, new_unicode_string};
// NameKey::from_object
crate::pdf_string::utf8_value(value)
// NameKey::to_object
let normalized = normalized_utf8_value(key);
Object::String(new_unicode_string(&normalized))
```

In `outline_document_helper.rs`, replace every
`crate::json_inspect::qpdf_utf8_value` with `crate::pdf_string::utf8_value` and
every `crate::json_inspect::qpdf_new_unicode_utf8_value` with
`crate::pdf_string::normalized_utf8_value`.

Remove the moved functions and their now-owned tests from `json_inspect.rs`.
No JSON output behavior or unrelated `json_inspect` helper should change.

- [ ] **Step 5: Run focused core tests and verify GREEN**

Run:

```bash
cargo test -p flpdf pdf_string
cargo test -p flpdf nntree::tests::name_codec_matches_qpdf_utf8_value_and_new_unicode_string
cargo test -p flpdf --test reader_tests
```

Expected: all pass, with no remaining `json_inspect::qpdf_utf8_value`,
`json_inspect::qpdf_unicode_string_bytes`, or
`json_inspect::qpdf_new_unicode_utf8_value` references.

- [ ] **Step 6: Commit the core extraction**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/pdf_string.rs \
  crates/flpdf/src/json_inspect.rs crates/flpdf/src/nntree.rs \
  crates/flpdf/src/outline_document_helper.rs crates/flpdf/src/qtest_string.rs
git commit -m "refactor(flpdf): own PDF string semantics in domain module"
```

### Task 2: Route the qtest helper directly through the domain API

**Files:**
- Modify: `crates/flpdf-qtest-tools/src/character_encoding.rs`
- Modify: `crates/flpdf-qtest-tools/src/lib.rs`
- Test: `crates/flpdf-qtest-tools/tests/character_encoding_cli.rs`

Call `flpdf::pdf_string::{utf8_value,new_unicode_string,unparse_binary}`
directly from `character_encoding.rs`. There is no qtest-specific string
transformation, so a private delegation module would only obscure the actual
ownership boundary. Keep line splitting, argv, stderr, exit-code, and SIGABRT
behavior unchanged.

- [ ] **Step 1: Remove the delegation-only module**

Delete `crates/flpdf-qtest-tools/src/qtest_string.rs`, remove its crate-root
declaration, and replace its call sites with direct `flpdf::pdf_string` calls.

- [ ] **Step 2: Run qtest-tools focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-qtest-tools --test character_encoding_cli
cargo test -p flpdf-qtest-tools character_encoding::
```

Expected: all tests pass and `rg -n "qtest_string|crates/flpdf/src/qtest_string"`
finds no production reference.

- [ ] **Step 3: Commit the direct boundary**

```bash
git add crates/flpdf-qtest-tools/src/lib.rs \
  crates/flpdf-qtest-tools/src/character_encoding.rs
git commit -m "refactor(qtest-tools): call PDF string domain directly"
```

### Task 3: Synchronize correspondence and historical plan documentation

**Files:**
- Modify: `docs/qpdf-module-doc-index.md` (generated)
- Modify: `docs/superpowers/plans/2026-07-30-qtest-character-encoding-helpers.md`
- Modify: `crates/flpdf/src/pdf_string.rs` module documentation

- [ ] **Step 1: Update module correspondence annotations**

Document `pdf_string.rs` with a `libqpdf/QPDF_String.cc` correspondence line.
Do not add a qtest-tools string adapter to the flpdf module index; the helper
crate calls the domain API directly.

- [ ] **Step 2: Update the existing character-encoding plan**

Replace its feature-gated `flpdf::qtest_string` architecture with:

- ordinary `flpdf::pdf_string` for core semantics;
- direct `flpdf::pdf_string` calls from `character_encoding` for the
  test-binary's string operations;
- no `qtest-driver` feature gate for this boundary.

Update the Task 1 file list and interfaces to point at `pdf_string.rs`, and
update Task 2 to consume `flpdf::pdf_string` directly.
Preserve the plan's qpdf output, signal, differential, and coverage criteria.

- [ ] **Step 3: Regenerate and inspect the module index**

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
git diff --check
rg -n "qtest_string|pdf_string" docs/qpdf-module-doc-index.md \
  docs/superpowers/plans/2026-07-30-qtest-character-encoding-helpers.md
```

Expected: the generated index contains `crates/flpdf/src/pdf_string.rs` and
does not contain `crates/flpdf/src/qtest_string.rs`; the historical plan names
the direct `character_encoding` boundary without a delegation-only adapter.

- [ ] **Step 4: Commit documentation synchronization**

```bash
git add crates/flpdf/src/pdf_string.rs docs/qpdf-module-doc-index.md \
  docs/superpowers/plans/2026-07-30-qtest-character-encoding-helpers.md
git commit -m "docs: align qtest string ownership with qpdf"
```

### Task 4: Run oracle, workspace, and coverage gates

**Files:**
- Verify the committed implementation and documentation diff.
- Record the results in `bd comment flpdf-egzr.9` after all gates pass.

- [ ] **Step 1: Run formatting and focused compatibility checks**

```bash
cargo fmt --all -- --check
bash scripts/qpdf-character-encoding-diff.sh --check
cargo test -p flpdf --all-targets
cargo test -p flpdf-qtest-tools --locked
```

Expected: formatting, the pinned qpdf 11.9.0 character-encoding differential,
and both affected crates pass with unchanged stdout/stderr/status/signal
behavior.

- [ ] **Step 2: Run workspace quality gates**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --locked
```

Expected: both commands exit successfully with no new warnings or failures.

- [ ] **Step 3: Run fresh changed-line coverage**

Commit all implementation changes first, then run:

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/patch-cov.lcov
bash scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: `patch-coverage: OK` and 100% coverage for every changed
executable line under `crates/flpdf/src`.

- [ ] **Step 4: Record qtest survey and finalize tracker evidence**

From `/home/ubuntu/flpdf-qtest`, run the full survey against the binaries built
from this worktree:

```bash
QTEST_FULL=1 \
FLPDF_CLI_BIN=/home/ubuntu/flpdf/.worktrees/flpdf-egzr-9-pdf-string-boundary/target/release/flpdf \
FLPDF_TEST_COMPARE_BIN=/home/ubuntu/flpdf/.worktrees/flpdf-egzr-9-pdf-string-boundary/target/release/flpdf-test-compare \
FLPDF_TEST_DRIVER_BIN=/home/ubuntu/flpdf/.worktrees/flpdf-egzr-9-pdf-string-boundary/target/release/flpdf-test-driver \
./scripts/run.sh
```

Compare the same-run result with the baseline recorded for the character
encoding helper slice. Record qpdf/flpdf/qtest pins, applicable denominator,
pass counts, and zero allowlist regressions in the `flpdf-egzr.9` Bead comment.
Then persist the tracker and inspect the branch:

```bash
bd dolt push
git status --short --branch
```

Do not close the Bead until the current-main/PR evidence and manifest checks
support every acceptance criterion.
