# qpdf Local PDF Date Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make default attachment PDF dates match qpdf 11.9.0 local-time and timezone-offset behavior on Unix and Windows.

**Architecture:** Add a private qpdf-shaped `qpdf_time` primitive that owns platform acquisition, formatting, and one-time default capture. Keep `job::attachments` responsible only for applying explicit or cached default dates to the embedded-file stream.

**Tech Stack:** Rust 1.89 workspace, `libc` Unix time APIs, `windows-sys` Windows time APIs, `OnceLock`, qpdf 11.9.0, cargo/qpdf differential probes.

---

### Task 1: Add the qpdf time design and dependencies

**Files:**
- Create: `docs/superpowers/specs/2026-08-20-qpdf-local-pdf-date-design.md`
- Create: `docs/superpowers/plans/2026-08-20-qpdf-local-pdf-date.md`
- Modify: `Cargo.toml`
- Modify: `crates/flpdf/Cargo.toml`

- [ ] **Step 1: Declare platform dependencies**

Add `windows-sys = "0.61"` to workspace dependencies, add
`libc.workspace = true` to `crates/flpdf` dependencies, and add a Windows-only
`windows-sys` dependency with `Win32_System_SystemInformation` and
`Win32_System_Time` features.

- [ ] **Step 2: Commit the design/dependency baseline**

Run `cargo check -p flpdf`, then commit only the design and manifest changes.

### Task 2: Write the formatter RED tests

**Files:**
- Create: `crates/flpdf/src/qpdf_time.rs`
- Modify: `crates/flpdf/src/lib.rs`

- [ ] **Step 1: Write pure formatter tests first**

Add tests for the qpdf sign contract:

```rust
assert_eq!(format_qpdf_time(QpdfTime::new(2026, 8, 20, 22, 47, 33, 0)), b"D:20260820224733Z");
assert_eq!(format_qpdf_time(QpdfTime::new(2026, 8, 20, 22, 47, 33, -540)), b"D:20260820224733+09'00'");
assert_eq!(format_qpdf_time(QpdfTime::new(2026, 8, 20, 22, 47, 33, 60)), b"D:20260820224733-01'00'");
```

Register the private module in `lib.rs` but do not add the production
formatter/acquisition implementation before running the test.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf --lib qpdf_time
```

Expected result: compilation/test failure because the qpdf time primitive is
not implemented.

### Task 3: Implement and wire the qpdf time primitive

**Files:**
- Modify: `crates/flpdf/src/qpdf_time.rs`
- Modify: `crates/flpdf/src/job/attachments.rs`

- [ ] **Step 1: Implement pure formatting and stable capture**

Implement the `QpdfTime` value, `format_qpdf_time`, and a `OnceLock<Vec<u8>>`
default-date accessor. Keep the formatter independent of the system clock so
all offset branches are directly testable.

- [ ] **Step 2: Implement the Unix source route**

Use `time`, `tzset`, `localtime_r`, and `tm_gmtoff` behind the isolated module
unsafe boundary. Convert seconds-after-UTC to qpdf's minutes-before-UTC with
the exact negation used by qpdf.

- [ ] **Step 3: Implement the Windows source route**

Use `GetLocalTime` and `GetTimeZoneInformation`, taking `Bias` exactly as
qpdf does. Keep the FFI calls and safety comments inside `qpdf_time.rs`.

- [ ] **Step 4: Wire attachments to the cached default**

Remove `current_pdf_date` and `civil_from_days` from `job/attachments.rs`.
Use the qpdf-time cached bytes for both omitted date fields and retain the
existing explicit-date branches unchanged.

- [ ] **Step 5: Run GREEN**

Run:

```bash
cargo test -p flpdf --lib qpdf_time
cargo test -p flpdf --lib job::attachments
cargo test -p flpdf --test filespec_helper_tests
```

Expected result: all pass, including formatter offsets and stable default
capture.

### Task 4: Validate the live qpdf boundary and full quality gates

**Files:**
- Inspect: `crates/flpdf/src/qpdf_time.rs`, `crates/flpdf/src/job/attachments.rs`, and qpdf probe outputs

- [ ] **Step 1: Run the timezone differential probe**

Run these commands from the repository root, using the same input and
attachment in separate output files:

```bash
probe_dir=$(mktemp -d /tmp/flpdf-s5cw-3-live.XXXXXX)
env TZ=Asia/Tokyo qpdf --add-attachment tests/fixtures/minimal.pdf -- tests/fixtures/minimal.pdf "$probe_dir/qpdf.pdf"
env TZ=Asia/Tokyo cargo run -q --bin flpdf -- tests/fixtures/minimal.pdf --add-attachment tests/fixtures/minimal.pdf -- "$probe_dir/flpdf.pdf"
qpdf --json=2 --json-key=attachments "$probe_dir/qpdf.pdf" - | jq -r '.attachments | to_entries[] | .value.streams["/F"].creationdate, .value.streams["/F"].modificationdate'
qpdf --json=2 --json-key=attachments "$probe_dir/flpdf.pdf" - | jq -r '.attachments | to_entries[] | .value.streams["/F"].creationdate, .value.streams["/F"].modificationdate'
```

Expected suffix: `+09'00'` in both outputs; both fields must be equal within
each output.

- [ ] **Step 2: Run repository gates**

```bash
cargo fmt --all -- --check
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
```

- [ ] **Step 3: Run fresh patch coverage**

After committing the tested implementation and confirming a clean tree:

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected result: zero uncovered changed executable lines under `crates/flpdf/src`.

### Task 5: Rebase, Draft PR, CI, and Beads handoff

**Files:**
- Inspect: Git diff, PR checks, and Beads readback

- [ ] **Step 1: Rebase and push**

Fetch the latest `origin/main`, rebase the feature branch, rerun focused tests
and patch coverage, then push the rebased branch.

- [ ] **Step 2: Create Draft PR**

Create one Draft PR documenting qpdf source lines, the TZ live probe, TDD
results, local gates, and patch coverage. Do not merge.

- [ ] **Step 3: Mark ready only after every CI check is green**

Read back Coverage, Fuzz, Quality, Analyze, all OS tests, and codecov/patch.
Run `gh pr ready` only after every required check passes.

- [ ] **Step 4: Persist tracker evidence**

Append implementation, PR, CI, and verification evidence to `flpdf-s5cw.3`,
run `bd dep cycles`, run `bd dolt push`, and confirm `Push complete.`. Keep the
issue open because the PR must not be merged in this workflow.
