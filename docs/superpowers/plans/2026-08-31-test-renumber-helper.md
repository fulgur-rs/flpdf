# qtest `test_renumber` Helper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement qpdf 11.9.0's portable `test_renumber` behavior in
`flpdf-qtest-tools`, wire it into `flpdf-qtest`, and make all eight
`renumber-objects.test` cases pass against the canonical flpdf writer.

**Architecture:** The helper remains a qtest-only consumer. It opens a PDF,
configures the existing public `PdfWriter`, reloads its memory output, and
compares canonical `ObjectHandle` values plus writer/reloaded xref snapshots.
The separate qtest repository supplies only the executable shim/build wiring
and promotes the eight manifest rows after one paired full-survey run.

**Tech Stack:** Rust workspace (`flpdf`, `flpdf-qtest-tools`), qpdf 11.9.0
source and binaries, Python/Perl qtest harness, JSONL parity manifest,
GitHub Actions, and `cargo llvm-cov` patch coverage.

---

## Files and responsibility map

flpdf worktree:

- Create `crates/flpdf-qtest-tools/src/renumber.rs`: qpdf-shaped argument
  option model, writer execution, recursive object comparison, xref
  comparison, and exact stdout/error formatting.
- Create `crates/flpdf-qtest-tools/src/bin/test_renumber.rs`: OS-argument
  boundary, usage/status-2 handling, input open, and helper invocation.
- Modify `crates/flpdf-qtest-tools/src/lib.rs`: register the `renumber` module
  without exposing a new core compatibility API.
- Modify `crates/flpdf-qtest-tools/Cargo.toml`: register the `test_renumber`
  binary target.
- Create `crates/flpdf-qtest-tools/tests/renumber.rs`: real-binary tests for
  usage, bad options, missing input, success, and failure status.
- Modify `docs/qpdf-correspondence.md`: add the `test_renumber.cc` mapping and
  final differential evidence.

separate qtest worktree:

- Modify `scripts/run.sh`, `.github/workflows/ci.yml`, and `README.md` to
  build/export/use `FLPDF_TEST_RENUMBER_BIN`.
- Create `shim/test_renumber` that executes that absolute binary and preserves
  argv/status/stdout/stderr.
- Modify `parity/qtest-11.9.0.jsonl` for exactly the eight
  `renumber-objects` rows after the paired run proves `pass`.
- Modify qtest tests for the new env/build/shim contract when the existing
  contract tests require an explicit binary list.

## Task 1: Add failing helper and binary contract tests

**Files:**

- Create: `crates/flpdf-qtest-tools/tests/renumber.rs`
- Modify: `crates/flpdf-qtest-tools/Cargo.toml` only if the test needs an
  existing dev dependency already used by neighboring tests.

- [ ] **Step 1: Write the failing integration tests**

Use the real `minimal.pdf` fixture and the Cargo-provided binary path. The
test contract must be expressed before the binary target exists:

```rust
use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn test_renumber_usage_matches_qpdf_status_two() {
    let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
        .output()
        .expect("test_renumber must be runnable");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Usage: test_renumber [OPTION] INPUT.pdf\nOption:\n  --object-streams=preserve|disable|generate\n  --linearize\n  --preserve-unreferenced\n"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn test_renumber_rejects_unknown_options_and_missing_input() {
    for args in [
        vec!["--not-an-option", fixture("minimal.pdf").to_str().unwrap()],
        vec!["--object-streams=bad", fixture("minimal.pdf").to_str().unwrap()],
        vec!["/path/that/does/not/exist.pdf"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
            .args(args)
            .output()
            .expect("test_renumber must be runnable");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn test_renumber_minimal_pdf_reports_success() {
    let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
        .arg(fixture("minimal.pdf"))
        .output()
        .expect("test_renumber must be runnable");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--- compare between input and renumbered objects ---"));
    assert!(stdout.contains("--- compare between written and reloaded xref tables ---"));
    assert!(stdout.ends_with("succeeded\n"));
    assert!(output.stderr.is_empty());
}
```

- [ ] **Step 2: Run the tests to verify RED**

Run:

```bash
cargo test -p flpdf-qtest-tools --test renumber
```

Expected: compilation fails because the `test_renumber` binary target does
not exist. Do not add a stub binary merely to turn this into a false green.

## Task 2: Implement the canonical qpdf helper in Rust

**Files:**

- Create: `crates/flpdf-qtest-tools/src/renumber.rs`
- Create: `crates/flpdf-qtest-tools/src/bin/test_renumber.rs`
- Modify: `crates/flpdf-qtest-tools/src/lib.rs`
- Modify: `crates/flpdf-qtest-tools/Cargo.toml`
- Test: `crates/flpdf-qtest-tools/tests/renumber.rs`

- [ ] **Step 1: Register the module and binary target**

Add the existing qtest-tools pattern to `Cargo.toml`:

```toml
[[bin]]
name = "test_renumber"
path = "src/bin/test_renumber.rs"
```

Register `pub mod renumber;` in `src/lib.rs`. Do not add a new public API to
the `flpdf` core crate.

- [ ] **Step 2: Implement the qpdf option model and usage boundary**

Use an internal copyable option struct with these defaults:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RenumberOptions {
    pub(crate) object_streams: flpdf::ObjectStreamMode,
    pub(crate) linearize: bool,
    pub(crate) preserve_unreferenced: bool,
}
```

The parser must accept exactly the three qpdf options from
`test_renumber.cc:168-205`, one final input path, and no extra positional
argument. Usage goes to stderr and returns status 2; no `unwrap`, sentinel
object, or panic may replace an operation error.

- [ ] **Step 3: Implement recursive object comparison**

Implement a private function with this shape:

```rust
fn compare_objects(
    source: &flpdf::ObjectHandle,
    emitted: &flpdf::ObjectHandle,
    visited: &mut std::collections::BTreeSet<flpdf::ObjectRef>,
    stdout: &mut dyn std::io::Write,
    stderr: &mut dyn std::io::Write,
) -> flpdf::Result<bool>
```

Match qpdf's `test_renumber.cc:24-117`: stop revisiting an indirect source
reference, compare boolean/integer/real/string/name values, recurse through
arrays and dictionaries in key order, and print `stream objects are not
compared` while treating stream objects as equal. Type/value/key/size
mismatches print qpdf-shaped diagnostics and return `false` so the caller can
return status 2.

Write unit tests for scalar mismatch, array mismatch, dictionary mismatch,
cycle termination, and stream skipping before moving to the writer path.

- [ ] **Step 4: Run comparison tests to verify the next GREEN boundary**

Run:

```bash
cargo test -p flpdf-qtest-tools renumber
```

Expected: the parser/usage tests pass; success still fails until writer
execution is implemented. If a public accessor is missing, stop and record
the exact qpdf primitive gap instead of introducing a raw-object bridge.

- [ ] **Step 5: Implement writer execution and xref comparison**

Implement the helper run against the existing public APIs:

```rust
let mut pdf = flpdf::Pdf::open(std::fs::File::open(input)?)?;
let source_objects = pdf.get_all_objects()?;
let mut writer = flpdf::PdfWriter::new(&mut pdf);
writer.set_output_memory()?;
writer.set_object_stream_mode(options.object_streams);
writer.set_linearization(options.linearize);
writer.set_preserve_unreferenced_objects(options.preserve_unreferenced);
writer.write()?;
let output_bytes = writer.get_buffer()?;
let written_xref = writer.get_written_xref_table()?;
let mut reloaded = flpdf::Pdf::open_mem_owned(output_bytes)?;
```

For each source object returned by `get_all_objects`, obtain its indirect
`ObjectRef`, call `get_renumbered_obj_gen`, print the mapping, print `deleted`
for an absent mapping, and compare the source handle with the reloaded emitted
handle. Then compare `written_xref` to
`reloaded.get_xref_table()` using `XrefEntry` variants and the same fields
observed by qpdf, including qpdf's upstream self-comparison behavior at
`test_renumber.cc:147,153-154`. Print both `complete` markers and
`succeeded` only after every check succeeds.

- [ ] **Step 6: Implement the binary error boundary**

`src/bin/test_renumber.rs` must call the parser/helper with `std::env::args_os`
and map every `flpdf::Error` to its raw display on stderr plus process status
2. Successful helper execution returns 0. Keep binary I/O separate from the
helper's comparison output so qtest can capture channels independently.

- [ ] **Step 7: Run the Rust helper tests GREEN**

Run:

```bash
cargo fmt --all
cargo test -p flpdf-qtest-tools --test renumber
cargo test -p flpdf-qtest-tools
```

Expected: all helper tests pass, including usage, bad option, missing input,
success, comparison mismatch, cycle, stream, and xref cases.

- [ ] **Step 8: Fix the canonical Preserve linearization boundary exposed by
  the signed fixture**

The first live differential must remain a RED test until the writer is fixed.
qpdf's `preserveObjectStreams` keeps the source member-to-container assignment
(`QPDFWriter.cc:1939-1966`), and `writeLinearized` emits each preserved
container once while `enqueueObject` routes a compressed member through its
container (`QPDFWriter.cc:1072-1125,1621-1757`). The current flpdf path can
leave those same Preserve members in `part4_open_document_plain`, causing a
plain duplicate, and the writer result omits the source-container mapping that
qpdf exposes.

Add a regression assertion using the qpdf qtest signed fixture when the pinned
qpdf test tree is available, then correct the shared linearization writer data
flow: suppress a member from every plain emission list when the resolved
ObjStm layout owns it, and carry source-container identity through the
canonical writer result for Preserve mode. Do not skip the fixture, weaken the
xref comparison, or add a helper-only mapping shim. Re-run the focused helper
test and the eight-case differential before wiring qtest.

## Task 3: Differentially verify all qpdf helper modes

**Files:**

- Test: `crates/flpdf-qtest-tools/tests/renumber.rs`
- Modify: `docs/qpdf-correspondence.md`

- [ ] **Step 1: Run qpdf and flpdf for the same eight cases**

Use the pinned qpdf helper and the fresh Rust binary from the same worktree:

```bash
qpdf_root=/home/ubuntu/.cache/flpdf/qpdf-11.9.0/qpdf/qtest/qpdf
qpdf_helper=/tmp/qpdf-11.9.0-build/qpdf/test_renumber
flpdf_helper=target/debug/test_renumber
probe_dir=$(mktemp -d -p /tmp flpdf-egzr7-diff.XXXXXX)
while IFS=$'\t' read -r name args; do
  read -r -a argv <<< "$args"
  (cd "$qpdf_root" && "$qpdf_helper" "${argv[@]}") \
    > "$probe_dir/$name.qpdf.out" 2> "$probe_dir/$name.qpdf.err"
  qpdf_status=$?
  (cd "$qpdf_root" && "$flpdf_helper" "${argv[@]}") \
    > "$probe_dir/$name.flpdf.out" 2> "$probe_dir/$name.flpdf.err"
  flpdf_status=$?
  test "$qpdf_status" -eq "$flpdf_status"
  diff -u "$probe_dir/$name.qpdf.out" "$probe_dir/$name.flpdf.out"
  diff -u "$probe_dir/$name.qpdf.err" "$probe_dir/$name.flpdf.err"
done <<'EOF'
minimal	minimal.pdf
signed	digitally-signed.pdf
generate-min	--object-streams=generate minimal.pdf
generate-signed	--object-streams=generate digitally-signed.pdf
linearize-min	--linearize minimal.pdf
linearize-signed	--linearize digitally-signed.pdf
preserve-min	--preserve-unreferenced minimal.pdf
preserve-signed	--preserve-unreferenced digitally-signed.pdf
EOF
```

Run each helper from the qpdf fixture directory so relative fixture lookup is
identical. Record each status and the exact output diff. qpdf's helper must
report status 0 and `succeeded`; flpdf must do the same for all eight. The
signed linearize case is the regression gate for the Preserve writer fix above.

- [ ] **Step 2: Add focused Rust assertions for both fixtures and modes**

Extend the integration test with a table of the eight argument vectors and
assert status 0, empty stderr, two `complete` markers, and `succeeded`. The
tests must use the real `minimal.pdf` and `digitally-signed.pdf` fixtures and
must not vendor qpdf files.

- [ ] **Step 3: Record the source mapping**

Add a `test_renumber.cc` row to `docs/qpdf-correspondence.md` with qpdf source
lines `14-22,24-117,119-166,168-259`, the flpdf helper paths, the qpdf-head
self-comparison note, and the final eight-case differential result.

## Task 4: Wire the separate flpdf-qtest repository

**Files:**

- Modify: `flpdf-qtest/scripts/run.sh`
- Modify: `flpdf-qtest/.github/workflows/ci.yml`
- Modify: `flpdf-qtest/README.md`
- Create: `flpdf-qtest/shim/test_renumber`
- Modify: `flpdf-qtest/parity/qtest-11.9.0.jsonl`
- Test: `flpdf-qtest/scripts/tests/test_run_contract.py` and relevant shim
  tests

- [ ] **Step 1: Create an isolated qtest worktree**

Create it outside the source checkout's active worktree, based on the latest
qtest branch that contains the current manifest runner:

```bash
git -C /home/ubuntu/flpdf-qtest worktree add \
  /tmp/flpdf-qtest-egzr.7 -b flpdf-egzr-7-renumber \
  origin/flpdf-25kg-7-10-progress
```

Verify the new worktree is clean before editing it. The flpdf binary paths
must point at the implementation worktree's release artifacts.

- [ ] **Step 2: Add binary environment/build/shim wiring**

Follow the existing `FLPDF_TEST_*_BIN` pattern exactly. Add
`FLPDF_TEST_RENUMBER_BIN`, include `--bin test_renumber` in the release build,
export and executable-preflight it, and create an executable shim that runs:

```bash
#!/usr/bin/env bash
set -euo pipefail
: "${FLPDF_TEST_RENUMBER_BIN:?FLPDF_TEST_RENUMBER_BIN is not set}"
exec "${FLPDF_TEST_RENUMBER_BIN}" "$@"
```

Update the CI and README binary lists together so the contract tests and CI
cannot silently use a host `test_renumber`.

- [ ] **Step 3: Run the focused qtest suite RED, then GREEN**

With the new shim absent, preserve the recorded 8/8 helper-unavailable RED
result. After wiring and building the Rust helper, run the same disposable
qtest-driver invocation with `TESTS=renumber-objects`; require:

```text
Total tests: 8
Passes: 8
Failures: 0
Unexpected Passes: 0
```

Keep `harness.log` and `qtest-results.xml` from this same run together.

- [ ] **Step 4: Update exactly the eight manifest rows**

Change only `renumber-objects 1` through `8` from `blocked` to `passing` after
the focused run proves them. Remove the stale `flpdf-25kg.6` ownership only
as part of that promotion; do not alter unrelated rows or the denominator.

- [ ] **Step 5: Run the full survey and manifest validators**

Run from the qtest worktree:

```bash
QTEST_FULL=1 \
FLPDF_DIR=/home/ubuntu/flpdf/.worktrees/flpdf-egzr.7 \
./scripts/run.sh
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
```

Expected: no allowlist regressions, manifest validation errors 0, and exactly
the eight renumber rows observed as passing. Inspect both the summary and the
paired XML/log artifacts; do not count suite-level `overall-outcome` strings
as testcase results.

- [ ] **Step 6: Commit and push the qtest wiring as a dependent PR**

Use a focused commit and a Draft PR based on the qtest branch used above. The
PR body must cite the flpdf implementation PR and the paired qtest evidence,
but must not claim any merge. Keep this PR Draft until its own CI is green.

## Task 5: Final verification, rebase, PR, and Beads close

**Files:** All changed files from Tasks 2-4.

- [ ] **Step 1: Run flpdf quality gates from a clean tree**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf-qtest-tools --test renumber
cargo test -p flpdf-qtest-tools
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' \
  cargo doc --workspace --no-deps --document-private-items
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
git diff --check
```

- [ ] **Step 2: Run fresh patch coverage against the actual PR base**

After committing all flpdf changes, run:

```bash
bash scripts/patch-coverage.sh --base origin/main
```

Require `flpdf changed ..., uncovered 0 -> PASS`. Do not reuse an LCOV file
from another commit as the authoritative result.

- [ ] **Step 3: Re-run qpdf differential from the final commit**

Run the eight helper comparisons and record status/stdout/stderr. Confirm the
pinned qpdf source worktree remains clean and the qpdf version is 11.9.0.

- [ ] **Step 4: Rebase before publishing the flpdf PR**

Fetch latest origin and rebase the implementation branch onto the current
`origin/main` without committing on `main`:

```bash
git fetch origin --prune
GIT_EDITOR=true git rebase origin/main
```

If rebase changes the tree, rerun the relevant tests and patch coverage.

- [ ] **Step 5: Push and create a Draft flpdf PR**

Push the dedicated branch and create a Draft PR with qpdf source references,
the eight-case differential, qtest artifact paths, and all local gate results.
Do not use `gh pr ready` until every reported CI check, including coverage,
is successful.

- [ ] **Step 6: Wait for CI, mark Ready, and leave merge to integration**

Watch all checks. After all are green, run `gh pr ready <number>`. Do not
merge the PR in this session.

- [ ] **Step 7: Record Beads evidence and close**

Append the final implementation/PR/test evidence to `flpdf-egzr.7`, close the
issue only after readback, run `bd dep cycles`, then `bd dolt push` and confirm
the literal `Push complete.` output. Read back the closed issue, PR state, git
status, and remote head.
