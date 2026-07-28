# Stream Filter Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve truncated-Flate diagnostics and remove the eager encoded-input copy identified by the two actionable review threads on PR #576.

**Architecture:** Thread the existing `PlFlate` warning callback through the crate-private filter-chain driver. Public whole-buffer decode APIs reject warnings because they cannot return diagnostics, while `check` collects the same warnings into its existing `Diagnostics` channel. Represent the current filter input as `Cow<[u8]>` so the first stage borrows the caller's encoded bytes and later stages own their results.

**Tech Stack:** Rust 2021; existing `StreamFilter`, `PlFlate`, `PipelineResult`, `Diagnostics`, and Cargo test/Clippy/llvm-cov tooling.

## Global Constraints

- Preserve the signatures and visibility of `decode_stream_data`, `decode_stream_data_with_limits`, and `DecodeLimits`.
- Add no public warning-aware API.
- Preserve qpdf 11.9.0's warning text: `input stream is complete but output may still be valid`.
- Keep real codec failures as errors in every path.
- Keep output-limit failures as public decode errors and `--check` warnings.
- Keep filter ordering, `/DecodeParms`, predictor, Crypt, and maximum chain-length behavior unchanged.
- Do not refactor predictor allocation or later filter-stage buffers.
- Do not reply to or resolve GitHub review threads.
- Follow RED-GREEN-REFACTOR for each behavior and retain fresh 100% patch coverage.

---

### Task 1: Reject truncated-Flate warnings from public decode APIs

**Files:**
- Modify: `crates/flpdf/src/filters.rs:53-220`
- Test: `crates/flpdf/src/filters.rs:780-850`

**Interfaces:**
- Consumes: `crate::pipeline::{PipelineError, PipelineResult}` and the existing `StreamFilter::pipe_decode` warning callback.
- Produces: `pub(crate) fn decode_stream_data_with_limits_and_warnings(dict: &Dictionary, stream_data: &[u8], limits: DecodeLimits, warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>) -> Result<Vec<u8>>`.
- Produces: private `fn reject_decode_warning(message: &str, code: i32) -> PipelineResult<()>`.

- [ ] **Step 1: Write the failing public decode regression test**

Add beside the existing malformed-header test:

```rust
#[test]
fn decode_stream_data_rejects_truncated_flate_warning() {
    let error = decode_stream_data(&flate_dict(), b"\x78").unwrap_err();

    assert_eq!(
        error.to_string(),
        "unsupported PDF feature: stream inflate: \
         input stream is complete but output may still be valid (zlib error -5)"
    );
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p flpdf --lib filters::tests::decode_stream_data_rejects_truncated_flate_warning -- --exact
```

Expected: FAIL because `decode_stream_data` returns `Ok([])`.

- [ ] **Step 3: Add the internal warning-aware entry point**

Import `PipelineError` and `PipelineResult`, then add:

```rust
fn reject_decode_warning(message: &str, code: i32) -> PipelineResult<()> {
    Err(PipelineError::runtime(format!(
        "stream inflate: {message} (zlib error {code})"
    )))
}

pub(crate) fn decode_stream_data_with_limits_and_warnings(
    dict: &Dictionary,
    stream_data: &[u8],
    limits: DecodeLimits,
    warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
) -> Result<Vec<u8>> {
    decode_stream_data_with_filters(
        dict.get("Filter"),
        dict.get("DecodeParms"),
        stream_data,
        limits,
        warn,
    )
}
```

Add a `warn` parameter of the same callback type to
`decode_stream_data_with_filters` and
`decode_stream_data_with_filters_and_crypt`. Pass it unchanged to
`filter.pipe_decode`.

Make `decode_stream_data` and `decode_stream_data_with_limits` call their
existing internal route with `&mut reject_decode_warning`:

```rust
pub fn decode_stream_data(dict: &Dictionary, stream_data: &[u8]) -> Result<Vec<u8>> {
    decode_stream_data_with_limits_and_warnings(
        dict,
        stream_data,
        DecodeLimits::default(),
        &mut reject_decode_warning,
    )
}

pub fn decode_stream_data_with_limits(
    dict: &Dictionary,
    stream_data: &[u8],
    limits: DecodeLimits,
) -> Result<Vec<u8>> {
    decode_stream_data_with_limits_and_warnings(
        dict,
        stream_data,
        limits,
        &mut reject_decode_warning,
    )
}
```

Keep the Crypt-only closure unchanged and pass `warn` after it when calling
`decode_stream_data_with_filters_and_crypt`.

- [ ] **Step 4: Run focused filter tests and verify GREEN**

Run:

```bash
cargo test -p flpdf --lib filters::tests::decode_stream_data_rejects_truncated_flate_warning -- --exact
cargo test -p flpdf --lib filters::tests
```

Expected: both PASS.

- [ ] **Step 5: Commit Task 1**

```bash
git add crates/flpdf/src/filters.rs
git commit -m "fix: reject unreported flate warnings"
```

---

### Task 2: Preserve Flate warnings in `--check`

**Files:**
- Modify: `crates/flpdf/src/check.rs:8-10`
- Modify: `crates/flpdf/src/check.rs:283-360`
- Test: `crates/flpdf/src/check.rs:620-705`
- Test: `crates/flpdf/src/check.rs:920-1020`

**Interfaces:**
- Consumes: `decode_stream_data_with_limits_and_warnings` from Task 1.
- Produces: warning diagnostics formatted as `page {page_number}: {location}: {message}`.

- [ ] **Step 1: Write the failing checker regression test**

Add this fixture helper:

```rust
fn truncated_flate_content_pdf() -> Vec<u8> {
    content_pdf(
        "4 0 R",
        &[(4, corrupt_filtered_object(4, "FlateDecode", b"\x78"))],
    )
}
```

Add this test near the output-limit warning tests:

```rust
#[test]
fn truncated_flate_content_is_a_warning_not_an_error() {
    let report = check_reader_with_options(
        Cursor::new(truncated_flate_content_pdf()),
        PdfOpenOptions {
            repair: false,
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(report.valid);
    assert!(report.diagnostics.entries().iter().any(|diagnostic| {
        diagnostic.severity == Severity::Warning
            && diagnostic.message.contains("content stream object 4 0")
            && diagnostic
                .message
                .contains("input stream is complete but output may still be valid")
    }));
    assert!(!report
        .diagnostics
        .entries()
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error));
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p flpdf --lib check::tests::truncated_flate_content_is_a_warning_not_an_error -- --exact
```

Expected: FAIL because Task 1's public warning policy makes `check` report an
error.

- [ ] **Step 3: Route checker warnings into `Diagnostics`**

Replace the `decode_stream_data_with_limits` import with
`decode_stream_data_with_limits_and_warnings`.

In the content-stream loop, compute `location` before decoding, collect warning
messages, then record them before classifying the decode result:

```rust
let location = match stream_ref {
    Some(r) => format!("content stream object {} {}", r.number, r.generation),
    None => "inline content stream".to_string(),
};
let mut decode_warnings = Vec::new();
let result = decode_stream_data_with_limits_and_warnings(
    &stream.dict,
    &stream.data,
    limits,
    &mut |message, _code| {
        decode_warnings.push(message.to_string());
        Ok(())
    },
);
for warning in decode_warnings {
    diagnostics.push(Diagnostic::warning(
        format!("page {page_number}: {location}: {warning}"),
        None,
    ));
}
if let Err(error) = result {
    if is_decode_output_limit_error(&error) {
        let limit = limits.max_output.unwrap_or_default();
        diagnostics.push(Diagnostic::warning(
            format!(
                "page {page_number}: {location}: decoded output exceeds the \
                 configured limit of {limit} bytes; skipped (decode-bomb guard)"
            ),
            None,
        ));
    } else {
        diagnostics.push(Diagnostic::error(
            format!(
                "page {page_number}: {location}: \
                 errors while decoding content stream"
            ),
            None,
        ));
    }
}
```

Do not suppress collected warnings when a later error also occurs.

- [ ] **Step 4: Run focused checker tests and verify GREEN**

Run:

```bash
cargo test -p flpdf --lib check::tests::truncated_flate_content_is_a_warning_not_an_error -- --exact
cargo test -p flpdf --lib check::tests
```

Expected: both PASS.

- [ ] **Step 5: Commit Task 2**

```bash
git add crates/flpdf/src/check.rs crates/flpdf/src/filters.rs
git commit -m "fix: surface flate warnings during checks"
```

---

### Task 3: Borrow encoded input for the first filter stage

**Files:**
- Modify: `crates/flpdf/src/filters.rs:1-10`
- Modify: `crates/flpdf/src/filters.rs:175-220`
- Test support: `crates/flpdf/src/stream_filter.rs:180-210`
- Test: `crates/flpdf/src/filters.rs:780-870`

**Interfaces:**
- Consumes: existing `StreamFilter::pipe_decode(&mut self, data: &[u8], ...)`.
- Produces: a `Cow<'_, [u8]>` filter-chain accumulator initially set to `Cow::Borrowed(stream_data)`.
- Produces under `#[cfg(test)]`: `pub(crate) fn expect_first_filter_input(data: &[u8])`.
- Produces under `#[cfg(test)]`: registered filter name `TestBorrowedInput`.

- [ ] **Step 1: Add the test-only borrowed-input probe and failing test**

In `stream_filter.rs`, add a thread-local expected pointer and a test filter:

```rust
#[cfg(test)]
thread_local! {
    static EXPECTED_FIRST_INPUT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn expect_first_filter_input(data: &[u8]) {
    EXPECTED_FIRST_INPUT.set(data.as_ptr() as usize);
}

#[cfg(test)]
struct BorrowedInputProbe;

#[cfg(test)]
impl StreamFilter for BorrowedInputProbe {
    fn pipe_decode(
        &mut self,
        data: &[u8],
        _: Option<usize>,
        _: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
    ) -> Result<Vec<u8>> {
        EXPECTED_FIRST_INPUT.with(|expected| {
            assert_eq!(data.as_ptr() as usize, expected.get());
        });
        Ok(data.to_vec())
    }
}
```

Register `b"TestBorrowedInput"` to return `BorrowedInputProbe`.

In `filters.rs`, import `expect_first_filter_input` under `#[cfg(test)]` and add:

```rust
#[test]
fn first_filter_borrows_the_callers_encoded_input() {
    let input = b"borrowed first-stage input";
    expect_first_filter_input(input);
    let mut dict = Dictionary::new();
    dict.insert(
        "Filter",
        Object::Name(b"TestBorrowedInput".to_vec()),
    );

    assert_eq!(decode_stream_data(&dict, input).unwrap(), input);
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p flpdf --lib filters::tests::first_filter_borrows_the_callers_encoded_input -- --exact
```

Expected: FAIL because the probe receives the eager `stream_data.to_vec()`
allocation rather than the original slice.

- [ ] **Step 3: Replace the eager copy with `Cow`**

Import `std::borrow::Cow` and change the accumulator:

```rust
let mut decoded = Cow::Borrowed(stream_data);
for spec in specs {
    let filter_name = spec.normalized_name();
    let next = if filter_name == b"Crypt" {
        decrypt_crypt(spec.decode_params, decoded.as_ref())?
    } else {
        let stage = if let Some(mut filter) = stream_filter_for(filter_name) {
            if !filter.set_decode_params(spec.decode_params) {
                return Err(Error::Unsupported(format!(
                    "stream filter {} does not support supplied /DecodeParms",
                    String::from_utf8_lossy(filter_name)
                )));
            }
            extract_predictor_params(spec.decode_params)?;
            filter.pipe_decode(decoded.as_ref(), limits.max_output, warn)?
        } else {
            apply_single_filter_decode(
                filter_name,
                decoded.as_ref(),
                spec.decode_params,
                limits.max_output,
            )
            .map_err(Error::Unsupported)?
        };
        apply_decode_params(spec.decode_params, &stage)?
    };
    decoded = Cow::Owned(next);
}
Ok(decoded.into_owned())
```

The code above retains the existing registered-filter validation order before
writing bytes into the codec pipeline.

- [ ] **Step 4: Run focused filter tests and verify GREEN**

Run:

```bash
cargo test -p flpdf --lib filters::tests::first_filter_borrows_the_callers_encoded_input -- --exact
cargo test -p flpdf --lib filters::tests
```

Expected: both PASS.

- [ ] **Step 5: Commit Task 3**

```bash
git add crates/flpdf/src/filters.rs crates/flpdf/src/stream_filter.rs
git commit -m "perf: borrow first stream filter input"
```

---

### Task 4: Final verification and publication

**Files:**
- Modify only if formatting requires it: files changed in Tasks 1-3.

**Interfaces:**
- Consumes: all completed review-remediation changes.
- Produces: pushed PR #576 branch with clean quality gates.

- [ ] **Step 1: Run formatting and static analysis**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
python3 scripts/qpdf-module-docs.py --check
```

Expected: all PASS.

- [ ] **Step 2: Run the workspace suite**

```bash
cargo test --workspace --all-features --quiet
```

Expected: PASS with no failed tests.

- [ ] **Step 3: Run fresh patch coverage**

```bash
cargo llvm-cov clean --workspace
scripts/patch-coverage.sh --base origin/main
```

Expected: 100% of changed executable lines covered.

- [ ] **Step 4: Inspect the final branch**

```bash
git diff --check origin/main...HEAD
git status -sb
git log --oneline origin/main..HEAD
```

Expected: no whitespace errors and no uncommitted files.

- [ ] **Step 5: Push and read back the PR**

```bash
bd dolt push
git push
gh pr view 576 --repo fulgur-rs/flpdf \
  --json number,url,state,isDraft,headRefOid,statusCheckRollup
```

Expected: push succeeds and PR #576 points at the new local `HEAD`. Do not
reply to or resolve review threads.
