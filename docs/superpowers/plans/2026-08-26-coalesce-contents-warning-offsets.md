# qtest coalesce-contents Warning-Offset Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make qtest `coalesce-contents.test` pass all 8 cases by preserving parsed content-stream offsets in qpdf-compatible normalization warnings.

**Architecture:** Keep the existing qpdf-shaped page normalization before writer planning. Replace the CLI normalization path's lossy `Vec<bool>` warning result with a private structured value containing the source stream `parsed_offset` and terminal-bad-token state, then render those values through the existing diagnostic-location formatter. The canonical stream pipeline and coalesce provider remain unchanged; qtest remains the external acceptance oracle.

**Tech Stack:** Rust workspace, `flpdf-cli` integration tests, qpdf 11.9.0 pinned source, Perl qtest harness, Git worktree, Beads/Dolt.

---

### Task 1: Add the CLI RED regression and update parsed-stream warning expectations

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_tests.rs:1335-1427, 4930-5220`

- [ ] **Step 1: Add a helper that derives a fixture stream-data offset from bytes**

Add this test-only helper next to `one_page_pdf_with_content` so expectations
are derived from the authored PDF bytes rather than hard-coded to an object
number:

```rust
fn stream_data_offset(bytes: &[u8]) -> u64 {
    let marker = b"stream\n";
    let marker_pos = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("fixture must contain a newline-terminated stream marker");
    u64::try_from(marker_pos + marker.len()).expect("fixture offset fits u64")
}
```

- [ ] **Step 2: Make the existing linearized warning test assert the qpdf offset**

Change `top_level_linearize_normalize_content_preserves_warning_exit` to keep
the generated input bytes and compute `let offset = stream_data_offset(&bytes);`
before writing them. Change each of its three warning prefixes from
`input.display()` to `format!("{} (offset {offset})", input.display())` while
keeping the message text and exit status unchanged. This is the first RED
regression: current flpdf emits the same warnings without `(offset N)`.

- [ ] **Step 3: Run the focused RED test**

Run:

```bash
cargo test -p flpdf-cli --test cli_tests top_level_linearize_normalize_content_preserves_warning_exit
```

Expected result: FAIL only because the actual warning location is the bare
input path while the assertion requires the parsed stream offset; the output
file and exit status remain correct.

- [ ] **Step 4: Update the other exact warning assertions for parsed streams**

For the existing exact-output tests
`rewrite_normalize_content_follows_indirect_contents_array`,
`rewrite_normalize_content_duplicate_array_stream_warns_once`, and
`rewrite_normalize_content_deduplicates_terminal_stream_aliases`, retain the
fixture bytes in a local variable, compute the relevant stream-data offset
with `stream_data_offset`, and change all three expected warning locations to
the corresponding `input (offset N)` form. Tests that only assert message
presence or warning ordering remain unchanged; generated/contextless streams
must continue to use a bare input path.

- [ ] **Step 5: Run the focused CLI normalization tests before production code**

Run:

```bash
cargo test -p flpdf-cli --test cli_tests
```

Expected result: the new offset assertions fail against the unmodified CLI,
confirming the regression is real and not a test typo.

- [ ] **Step 6: Commit the RED test changes**

```bash
git add crates/flpdf-cli/tests/cli_tests.rs
git commit -m "test: require parsed offsets in normalization warnings"
```

### Task 2: Carry stream offsets through the canonical CLI normalization result

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:3310-3385, 3545-3580, 6065-6110`

- [ ] **Step 1: Define the private structured warning value**

Add near the normalization helpers:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContentNormalizationWarning {
    parsed_offset: Option<u64>,
    last_token_was_bad: bool,
}
```

- [ ] **Step 2: Propagate the value from the stream handle**

Change the return types of `normalize_page_contents`,
`apply_normalize_content`, and `normalize_and_store_stream_handle` from
`Vec<bool>` / `Option<bool>` to the structured warning type. In
`normalize_and_store_stream_handle`, build the warning before consuming the
normalizer result:

```rust
let warning = normalized.any_bad_tokens().then(|| {
    ContentNormalizationWarning {
        parsed_offset: (stream.get_parsed_offset() >= 0)
            .then_some(stream.get_parsed_offset() as u64),
        last_token_was_bad: normalized.last_token_was_bad(),
    }
});
```

Do not alter the `seen` deduplication, stream decode, provider-backed
coalesce path, mutation order, or writer settings.

- [ ] **Step 3: Render the structured offset at the existing warning boundary**

Change `emit_content_normalization_warnings` to accept
`ContentNormalizationWarning` and calculate its location once:

```rust
let location = diagnostic_location(input, warning.parsed_offset);
```

Use `warning.last_token_was_bad` for the conditional second warning. Change
`finish_rewrite_warnings` to iterate over `&ContentNormalizationWarning`
values. Keep the existing repair-warning ordering and `finish_warning_state`
call exactly as-is.

- [ ] **Step 4: Run the RED test to verify GREEN**

Run:

```bash
cargo test -p flpdf-cli --test cli_tests
```

Expected result: all selected tests pass, including the new parsed-offset
assertions and the existing exact warning-order assertions.

- [ ] **Step 5: Run the full local normalization/coalesce unit surfaces**

```bash
cargo test -p flpdf --test coalesce_tests
cargo test -p flpdf-cli --test cli_tests
```

Expected result: zero failures; existing no-offset tests for generated streams
and warning deduplication remain green.

- [ ] **Step 6: Commit the canonical implementation**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_tests.rs
git commit -m "fix: preserve parsed offsets in normalization warnings"
```

### Task 3: Verify the complete qtest oracle and quality gates

**Files:**
- No qtest fixture or expected-output edits.
- Read-only evidence: `/home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/coalesce-contents.test` and its same-run `harness.log`/`qtest-results.xml` artifacts.

- [ ] **Step 1: Rebuild the exact helper set from this worktree**

```bash
cargo build --release --bin flpdf --bin flpdf-test-compare --bin flpdf-test-driver
```

- [ ] **Step 2: Run the focused qtest suite with all required helper variables**

Run the qtest driver from a temporary copy of `vendor/qpdf-qtest` with the
worktree's release binaries and `/home/ubuntu/flpdf-qtest/shim` first in
`PATH`, setting `FLPDF_QPDF_COMPAT=1`,
`FLPDF_QTEST_NORMALIZE=/home/ubuntu/flpdf-qtest/normalize/stderr-rules.sed`,
`FLPDF_CLI_BIN`, `FLPDF_TEST_COMPARE_BIN`, and `FLPDF_TEST_DRIVER_BIN`.
Use `TESTS=coalesce-contents` and retain the resulting `harness.log` and
`qtest-results.xml` paths. Expected result: total 8, passes 8, failures 0;
row 1 includes offsets 671, 823, 962, and 1338 and rows 2--8 remain passing.

- [ ] **Step 3: Compare row 1 directly against qpdf**

Run qpdf 11.9.0 and the worktree flpdf on `split-tokens.pdf` with
`--qdf --static-id`, compare stderr after only the normal qtest path
substitution, verify both exit status 3, and verify the output file remains
byte-identical to the already passing qdf golden.

- [ ] **Step 4: Run repository quality gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo test -p flpdf
cargo test -p flpdf-cli
```

Expected result: every command exits 0. The qtest suite remains the
authoritative proof for the named objective; Rust and CLI suites prove that
the structured warning result did not regress other normalization or coalesce
consumers.

- [ ] **Step 5: Record evidence in Beads, push, and create the PR**

Append the commit, focused qtest totals, direct qpdf comparison, and quality
gate results to `flpdf-25kg.6.23`, run `bd dep cycles`, then `bd dolt push` and
push the feature branch. Create a PR against `main`; do not close the issue
until the PR is merged and live state is re-queried.
