# PR #591 Driver Parity Follow-up Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve OS-native qpdf test-driver paths and let only the qpdf-compatibility driver decode supported filter chains longer than the library's default 16-stage hardening limit.

**Architecture:** Add filter-chain policy to the existing `DecodeLimits` value and expose a public recovering-with-limits boundary; every ordinary flpdf caller keeps `Some(16)`, while `flpdf-test-driver` explicitly selects `None`. Independently, carry `OsString`/`OsStr` through the driver filesystem boundary and convert only diagnostic rendering to bytes, using exact Unix bytes and native Windows wide paths.

**Tech Stack:** Rust 2021 workspace, `std::ffi::{OsStr, OsString}`, platform `OsStrExt`, flpdf stream filters and ordered `StreamDecodeEvent`, `assert_cmd`, Bash/Python fixture generation, pinned qpdf 11.9.0, Beads, GitHub GraphQL review threads.

## Global Constraints

- Pinned qpdf 11.9.0 source and measured test-driver output/status are the behavioral oracle.
- Ordinary `decode_stream_data`, `decode_stream_data_recovering`, `decode_stream_data_with_limits`, reader crypt handling, and check paths retain a default maximum filter-chain length of exactly 16.
- Only the qpdf test-driver route sets `max_filter_chain: None`; do not remove the hardened library default and do not decode a chain in chunks.
- `DecodeLimits::default()` is exactly `max_output: None` and `max_filter_chain: Some(16)`.
- Unix argv and filename diagnostics preserve invalid UTF-8 bytes exactly; Windows filesystem and CRT probing use the native wide path.
- Existing stdout-before-stderr flush ordering and merged-output ordering remain unchanged.
- DCTDecode remains owned by `flpdf-n9t0.9`; TIFF Predictor 2 is deferred to new child `flpdf-n9t0.10`.
- The new long-chain PDF is flpdf-authored, deterministic, and its `.out` is generated only by the pinned qpdf 11.9.0 driver.
- Finish with fresh 100% changed executable-line coverage for `crates/flpdf/src` and report-only changed lines, with no new coverage exclusions.
- Reply in-thread only after the pushed commit and CI evidence exist; do not resolve any of the four review threads without separate authorization.

---

### Task 1: Make the filter-chain limit configurable without weakening defaults

**Files:**
- Modify: `crates/flpdf/src/filters.rs:15-32, 143-176, 222-271, 313-329, 2343-2538`
- Modify: `crates/flpdf/src/check.rs:1007-1009, 1189-1191, 1213-1215`
- Modify: `crates/flpdf-cli/src/main.rs:1618-1620, 2051-2053`
- Modify: `crates/flpdf/tests/stream_decode_recovery_public_api.rs`
- Modify: `docs/threat-model.md:114,200`

**Interfaces:**
- Consumes: existing `DecodeLimits { max_output: Option<usize> }`, `decode_stream_data_recovering`, `decode_stream_data_recovering_with_limits_and_mode`, and ordered `StreamDecodeOutcome`.
- Produces: `DecodeLimits { pub max_output: Option<usize>, pub max_filter_chain: Option<usize> }`.
- Produces: `pub fn decode_stream_data_recovering_with_limits(dict: &Dictionary, stream_data: &[u8], limits: DecodeLimits) -> Result<StreamDecodeOutcome>`.
- Preserves: `pub(crate) fn validate_filter_chain_len(filters: &[Object]) -> Result<()>` as the default-16 wrapper used by `reader.rs` explicit-Crypt handling.

- [ ] **Step 1: Add public RED coverage for the default cap and explicit unlimited recovery**

In `crates/flpdf/tests/stream_decode_recovery_public_api.rs`, extend the import and add:

```rust
use flpdf::filters::{
    decode_stream_data, decode_stream_data_recovering,
    decode_stream_data_recovering_with_limits, encode_stream_data, DecodeLimits,
    StreamDecodeEvent,
};

fn asciihex_dictionary(stages: usize) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert(
        "Filter",
        Object::Array(vec![Object::Name(b"ASCIIHexDecode".to_vec()); stages]),
    );
    dictionary
}

#[test]
fn recovering_limits_keep_default_chain_cap_but_allow_explicit_unlimited_chain() {
    let one_stage = asciihex_dictionary(1);
    let dictionary = asciihex_dictionary(17);
    let original = b"A";
    let mut encoded = original.to_vec();
    for _ in 0..17 {
        encoded = encode_stream_data(&one_stage, &encoded).unwrap();
    }

    assert_eq!(
        decode_stream_data_recovering(&dictionary, &encoded)
            .unwrap_err()
            .to_string(),
        "unsupported PDF feature: filter chain length 17 exceeds maximum of 16"
    );

    let outcome = decode_stream_data_recovering_with_limits(
        &dictionary,
        &encoded,
        DecodeLimits {
            max_output: None,
            max_filter_chain: None,
        },
    )
    .unwrap();

    assert_eq!(outcome.data, original);
    assert!(matches!(
        &outcome.events[..],
        [StreamDecodeEvent::Data(data)] if data == original
    ));
}
```

- [ ] **Step 2: Run the public test and verify RED**

Run:

```bash
cargo test -p flpdf --test stream_decode_recovery_public_api recovering_limits_keep_default_chain_cap_but_allow_explicit_unlimited_chain -- --exact
```

Expected: compilation fails because `decode_stream_data_recovering_with_limits` and `DecodeLimits::max_filter_chain` do not exist.

- [ ] **Step 3: Add unit RED coverage for exact default policy and validation precedence**

In `crates/flpdf/src/filters.rs` tests, add:

```rust
#[test]
fn decode_limits_default_to_unbounded_output_and_sixteen_filters() {
    assert_eq!(
        DecodeLimits::default(),
        DecodeLimits {
            max_output: None,
            max_filter_chain: Some(16),
        }
    );
}

#[test]
fn unlimited_chain_policy_reaches_filter_item_validation() {
    let mut filters = vec![Object::Name(b"ASCIIHexDecode".to_vec()); 16];
    filters.push(Object::Integer(1));
    let mut dictionary = Dictionary::new();
    dictionary.insert("Filter", Object::Array(filters));

    let error = decode_stream_data_recovering_with_limits(
        &dictionary,
        b">",
        DecodeLimits {
            max_output: None,
            max_filter_chain: None,
        },
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "unsupported PDF feature: stream filter type is not name or array"
    );
}
```

This pins both the ordinary 16-stage precedence and the `None` route reaching the next qpdf-compatible validation boundary.

- [ ] **Step 4: Implement the minimal limits API**

Replace the fixed validator internals in `crates/flpdf/src/filters.rs` with:

```rust
const MAX_FILTER_CHAIN_LEN: usize = 16;

pub(crate) fn validate_filter_chain_len(filters: &[Object]) -> Result<()> {
    validate_filter_chain_count(filters.len(), Some(MAX_FILTER_CHAIN_LEN))
}

fn validate_filter_chain_count(count: usize, maximum: Option<usize>) -> Result<()> {
    if let Some(maximum) = maximum.filter(|maximum| count > *maximum) {
        return Err(Error::Unsupported(format!(
            "filter chain length {count} exceeds maximum of {maximum}"
        )));
    }
    Ok(())
}
```

Change `DecodeLimits` and its default to:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeLimits {
    pub max_output: Option<usize>,
    pub max_filter_chain: Option<usize>,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_output: None,
            max_filter_chain: Some(MAX_FILTER_CHAIN_LEN),
        }
    }
}
```

Add the public recovering boundary next to `decode_stream_data_recovering`:

```rust
pub fn decode_stream_data_recovering_with_limits(
    dict: &Dictionary,
    stream_data: &[u8],
    limits: DecodeLimits,
) -> Result<StreamDecodeOutcome> {
    decode_stream_data_recovering_with_limits_and_mode(
        dict,
        stream_data,
        limits,
        DataEventMode::Record,
    )
}
```

Make `decode_stream_data_recovering` delegate to the new public function with `DecodeLimits::default()`. In `decode_stream_data_with_filters_and_crypt`, change both checks to:

```rust
if let Some(Object::Array(filters)) = filter {
    validate_filter_chain_count(filters.len(), limits.max_filter_chain)?;
}
let specs = decode_filter_specs(filter, decode_params)?;
validate_filter_chain_count(specs.len(), limits.max_filter_chain)?;
```

Keep `reader.rs` calling `validate_filter_chain_len(filters)` so encrypted-stream opening retains the default cap.

- [ ] **Step 5: Update every existing limits literal to retain the 16-stage default**

For every existing `DecodeLimits { max_output: ... }` literal in
`crates/flpdf/src/filters.rs`, `crates/flpdf/src/check.rs`, and
`crates/flpdf-cli/src/main.rs`, use:

```rust
DecodeLimits {
    max_output: Some(expected_limit),
    ..DecodeLimits::default()
}
```

Do not set `max_filter_chain: None` in these existing callers. Confirm no incomplete literal remains:

```bash
rg -n -U 'DecodeLimits \{\n\\s*max_output: [^\\n]+,\n\\s*\}' crates/flpdf/src crates/flpdf/tests crates/flpdf-cli/src
```

Expected: no matches.

- [ ] **Step 6: Update API docs and the threat-model statement**

Document `max_filter_chain` as a count checked before malformed filter items, with `None` meaning no count limit. Update the `DecodeLimits` type documentation so “default unlimited” applies to output only, not to the filter count. Document `decode_stream_data_recovering_with_limits` errors and ordered-event behavior.

In `docs/threat-model.md`, replace the claim that the cap is unconditional with the exact posture:

```markdown
The ordinary decode APIs cap `/Filter` chains at 16 stages by default through
`DecodeLimits::default()`. Callers must explicitly set
`DecodeLimits::max_filter_chain` to `None` to opt out; the qpdf compatibility
test driver does so to reproduce qpdf's uncapped `qpdf_dl_all` behavior.
```

- [ ] **Step 7: Run focused GREEN checks**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test stream_decode_recovery_public_api
cargo test -p flpdf filters::tests
cargo test -p flpdf --test check_tests
cargo check -p flpdf-cli --all-targets
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc -p flpdf --no-deps --document-private-items
```

Expected: all commands pass; the public test proves default rejection plus explicit unlimited ordered output.

- [ ] **Step 8: Commit the core policy boundary**

```bash
git add crates/flpdf/src/filters.rs crates/flpdf/src/check.rs crates/flpdf-cli/src/main.rs crates/flpdf/tests/stream_decode_recovery_public_api.rs docs/threat-model.md
git commit -m "feat(filters): make decode chain limit configurable"
```

---

### Task 2: Preserve OS-native driver argv, paths, and diagnostic bytes

**Files:**
- Modify: `crates/flpdf-qtest-tools/src/bin/driver.rs`
- Modify: `crates/flpdf-qtest-tools/src/common.rs`
- Modify: `crates/flpdf-qtest-tools/src/driver/mod.rs`
- Modify: `crates/flpdf-qtest-tools/src/driver/test_0_1.rs`
- Modify: `crates/flpdf-qtest-tools/tests/driver_cli.rs`

**Interfaces:**
- Consumes: `std::env::args_os`, `std::fs::read<P: AsRef<Path>>`, existing raw stderr writer, native CRT open probes, and qpdf-style signed-decimal-prefix parsing.
- Produces: `pub fn test_driver_program_name_bytes(argv0: &[u8]) -> &[u8]`,
  using qpdf's slash-only, suffix-preserving rule.
- Produces: `pub fn run(args: &[OsString], stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8`.
- Produces: internal `fn os_str_diagnostic_bytes(value: &OsStr) -> Cow<'_, [u8]>`.
- Changes: driver-only filename diagnostic parameters in `run_test_0_1`, `emit_new_diagnostics`, and `write_warning` from `&str` to `&[u8]`.
- Preserves: the existing string `program_name(&str) -> &str` used by `flpdf-test-compare`.

- [ ] **Step 1: Write Unix integration RED tests for valid and missing non-UTF-8 paths**

In `crates/flpdf-qtest-tools/tests/driver_cli.rs`, add Unix imports:

```rust
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
```

Add:

```rust
#[cfg(unix)]
#[test]
fn non_utf8_pdf_path_opens_without_panicking() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut filename = b"valid-".to_vec();
    filename.push(0xff);
    filename.extend_from_slice(b".pdf");
    let path = directory.path().join(std::ffi::OsString::from_vec(filename));
    fs::copy(minimal_pdf(), &path).expect("copy minimal PDF");

    driver()
        .arg("1")
        .arg(&path)
        .assert()
        .code(0)
        .stdout(TEST_1_OUTPUT)
        .stderr("");
}

#[cfg(unix)]
#[test]
fn missing_non_utf8_pdf_path_reports_raw_bytes_and_exit_two() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut filename = b"missing-".to_vec();
    filename.push(0xff);
    filename.extend_from_slice(b".pdf");
    let path = directory
        .path()
        .join(std::ffi::OsString::from_vec(filename.clone()));

    let assertion = driver().arg("1").arg(&path).assert().code(2).stdout("");
    let stderr = assertion.get_output().stderr.as_slice();
    assert!(stderr.starts_with(b"open "));
    assert!(stderr.windows(filename.len()).any(|window| window == filename));
    assert!(!stderr.windows(b"panicked at".len()).any(|window| window == b"panicked at"));
}
```

- [ ] **Step 2: Run the integration tests and verify RED**

Run:

```bash
cargo test -p flpdf-qtest-tools --test driver_cli non_utf8 -- --nocapture
```

Expected on Unix: both tests fail because `env::args()` panics before `driver::run`; the child stderr contains Rust's non-Unicode argument panic rather than qpdf-shaped output.

- [ ] **Step 3: Add unit RED coverage for raw argv0 basename and parser input**

In `crates/flpdf-qtest-tools/src/common.rs`, change the test import to
`use super::{program_name, test_driver_program_name_bytes};` and add:

```rust
#[test]
fn test_driver_program_name_preserves_backslash_suffix_and_non_utf8() {
    assert_eq!(
        test_driver_program_name_bytes(b"/tmp/test-\xff\\driver.exe"),
        b"test-\xff\\driver.exe"
    );
}
```

In `crates/flpdf-qtest-tools/src/driver/mod.rs` tests, add:

```rust
#[cfg(unix)]
#[test]
fn usage_preserves_non_utf8_backslash_and_exe_suffix() {
    use std::os::unix::ffi::OsStringExt;

    let args = vec![std::ffi::OsString::from_vec(
        b"/tmp/test-\xff\\driver.exe".to_vec(),
    )];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
    assert!(stdout.is_empty());
    assert_eq!(stderr, b"Usage: test-\xff\\driver.exe n filename1 [arg2]\n");
}
```

- [ ] **Step 4: Implement byte-oriented basename and OS diagnostic conversion**

Keep the compare-only `program_name` unchanged and add to `common.rs`:

```rust
pub fn test_driver_program_name_bytes(argv0: &[u8]) -> &[u8] {
    argv0
        .rsplit(|byte| *byte == b'/')
        .next()
        .unwrap_or(argv0)
}
```

In `driver/mod.rs`, import `Cow`, `OsStr`, and `OsString`. Add:

```rust
#[cfg(unix)]
fn os_str_diagnostic_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(value.as_bytes())
}

#[cfg(not(unix))]
fn os_str_diagnostic_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    Cow::Owned(value.to_string_lossy().into_owned().into_bytes())
}
```

Change the binary boundary to:

```rust
fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().collect();
    // lock stdout/stderr exactly as before
    ExitCode::from(flpdf_qtest_tools::driver::run(&args, &mut out, &mut err))
}
```

In `run`, derive `whoami` and the number from `os_str_diagnostic_bytes`, keep `filename: &OsStr` for `std::fs::read`, and keep a `Cow<[u8]>` alive for every filename diagnostic. Build usage and parse errors as `Vec<u8>` and send them through `write_error_bytes`; never call `to_str().expect(...)` or format the filename.

- [ ] **Step 5: Make native CRT probes accept `&OsStr`**

On Unix:

```rust
#[cfg(unix)]
fn crt_open_error_message(filename: &OsStr) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let filename = CString::new(filename.as_bytes()).ok()?;
    // retain the existing fopen/errno/strerror/close behavior
}
```

On Windows:

```rust
#[cfg(windows)]
fn crt_open_error_message(filename: &OsStr) -> Option<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;
    let filename: Vec<libc::wchar_t> =
        filename.encode_wide().chain(std::iter::once(0)).collect();
    if filename[..filename.len() - 1].contains(&0) {
        return None;
    }
    // retain the existing _wfopen_s error handling
}
```

Use the platform-native `OsStr` NUL check in tests. The non-Unix diagnostic fallback remains documented as lossy only for unpaired wide values; ordinary valid-Unicode Windows output remains byte-identical.

- [ ] **Step 6: Convert filename-bearing diagnostics to byte assembly**

Change `open_error_bytes` to accept `filename: &[u8]`. Replace `open_pdf_error` with:

```rust
fn open_pdf_error_bytes(n: i32, filename: &[u8], error: &Error) -> Vec<u8> {
    let suffix = match error {
        Error::Parse { message, .. } if n == 0 && message == "xref not found" => {
            Some(b": can't find startxref".as_slice())
        }
        Error::Parse { message, .. }
            if n != 0 && message == "trailer dictionary not found" =>
        {
            Some(b": unable to find trailer dictionary while recovering damaged file".as_slice())
        }
        _ => None,
    };
    if let Some(suffix) = suffix {
        let mut output = filename.to_vec();
        output.extend_from_slice(suffix);
        output
    } else {
        error.to_string().into_bytes()
    }
}
```

Change `write_warning(filename, ...)` to assemble a `Vec<u8>` from:

```text
"WARNING: " + filename + optional offset framing + UTF-8 diagnostic.message
```

and call `write_stderr_bytes` once. Change `emit_new_diagnostics` and every filename parameter in `driver/test_0_1.rs` to `&[u8]`. Retain the existing stdout flush before the first stderr write.

- [ ] **Step 7: Parse the test number from bytes without path conversion**

Change:

```rust
fn parse_test_number(input: &[u8]) -> Result<i32, Vec<u8>>
```

Preserve the existing whitespace/sign/digit-prefix arithmetic. For overflow, concatenate the raw input between fixed ASCII fragments:

```rust
fn decimal_error(prefix: &[u8], input: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut message = prefix.to_vec();
    message.extend_from_slice(input);
    message.extend_from_slice(suffix);
    message
}
```

Use the parsed numeric value's ASCII `to_string()` only for the i64-to-i32 range message. Non-ASCII bytes before digits produce test number 0, matching the existing no-digits grammar, without panic.

- [ ] **Step 8: Update existing driver unit arguments to `OsString`**

In `driver/mod.rs` tests, replace each `Vec<String>` argument setup with:

```rust
let args = vec![
    OsString::from("flpdf-test-driver"),
    OsString::from("1"),
    OsString::from(fixture("direct_null")),
];
```

Update CRT/open-error helper tests to pass `OsStr::new(...)` and raw filename diagnostic slices separately. Preserve all writer-failure assertions and exact existing ASCII outputs.
Update the three direct `run_test_0_1` test calls in `driver/test_0_1.rs` to
pass byte filename literals such as `b"input.pdf"`.

- [ ] **Step 9: Run focused GREEN checks on every supported host contract**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf-qtest-tools common::tests
cargo test -p flpdf-qtest-tools driver::tests
cargo test -p flpdf-qtest-tools --test driver_cli
cargo test -p flpdf-qtest-tools --test driver_goldens
cargo check -p flpdf-qtest-tools --all-targets
```

Expected: all tests pass; Unix tests prove byte-exact invalid UTF-8 behavior, and existing ASCII/Windows-compatible fixtures retain their output.

- [ ] **Step 10: Commit the OS-native boundary**

```bash
git add crates/flpdf-qtest-tools/src/bin/driver.rs crates/flpdf-qtest-tools/src/common.rs crates/flpdf-qtest-tools/src/driver/mod.rs crates/flpdf-qtest-tools/src/driver/test_0_1.rs crates/flpdf-qtest-tools/tests/driver_cli.rs
git commit -m "fix(qtest): preserve native driver paths"
```

---

### Task 3: Wire uncapped recovery into the driver and add the qpdf differential fixture

**Files:**
- Modify: `crates/flpdf-qtest-tools/src/driver/test_0_1.rs:3,121`
- Modify: `tests/fixtures/test_driver/generate.sh:17-55,164-194`
- Modify: `tests/fixtures/test_driver/README.md`
- Modify: `scripts/qpdf-test-driver-diff.sh:21-59`
- Create: `tests/fixtures/test_driver/stream_filter_chain_17.pdf`
- Create: `tests/fixtures/test_driver/stream_filter_chain_17.out`

**Interfaces:**
- Consumes: Task 1's `decode_stream_data_recovering_with_limits` and `DecodeLimits::max_filter_chain`.
- Produces: driver policy `DecodeLimits { max_output: None, max_filter_chain: None }`.
- Produces: deterministic `stream_filter_chain_17` fixture and pinned qpdf merged-output golden.
- Preserves: unsupported codec behavior, DecodeParms semantics, event order, warning offsets, exit status, and every existing 37 fixture golden.

- [ ] **Step 1: Add the fixture generator entry before wiring the driver**

Add `stream_filter_chain_17` to both fixture-name arrays. In the Python generator, add:

```python
write(
    "stream_filter_chain_17",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter [ "
                + b" ".join([b"/ASCIIHexDecode"] * 17)
                + b" ]",
                b">",
            ),
        },
    ),
)
```

The one-byte raw stream forces qpdf to construct and finish all 17 supported stages without creating an exponentially large committed fixture.

- [ ] **Step 2: Generate only flpdf-authored input and verify driver RED**

Run:

```bash
bash tests/fixtures/test_driver/generate.sh --generate
cargo test -p flpdf-qtest-tools --test driver_goldens test_0_1_fixtures_match_committed_qpdf_merged_output -- --exact
```

Expected: the golden test fails first because `stream_filter_chain_17.out` does not exist. Do not synthesize the `.out` with Rust.

- [ ] **Step 3: Generate the oracle golden from pinned qpdf**

Run:

```bash
bash scripts/qpdf-test-driver-diff.sh --regenerate
```

Expected before driver wiring: pinned qpdf exits 0 and writes the new `.out`; flpdf differs because it reports the stream as not filterable under the default 16-stage cap. The script exits non-zero on that mismatch after creating the qpdf oracle output.

- [ ] **Step 4: Wire only test 0/1 stream recovery to the unlimited chain policy**

In `driver/test_0_1.rs`, import `DecodeLimits` and replace the recovery call with:

```rust
match flpdf::filters::decode_stream_data_recovering_with_limits(
    &decode_dictionary,
    &stream.data,
    DecodeLimits {
        max_output: None,
        max_filter_chain: None,
    },
) {
```

Do not change `ResolvedStreamDictionary::is_filterable`, codec support, warning recovery, or output ordering.

- [ ] **Step 5: Document the fixture and verify no old generated files drifted**

Add to `tests/fixtures/test_driver/README.md`:

```markdown
`stream_filter_chain_17` declares 17 supported `ASCIIHexDecode` stages. It
pins qpdf test 1's uncapped `qpdf_dl_all` filter construction while the ordinary
flpdf decode API remains capped at 16 by default.
```

Inspect:

```bash
git status --short tests/fixtures/test_driver
git diff --stat -- tests/fixtures/test_driver
```

Expected fixture changes: the generator, README, new `stream_filter_chain_17.pdf`, and new qpdf-derived `stream_filter_chain_17.out`; existing `.pdf` and `.out` files remain unchanged.

- [ ] **Step 6: Run fixture GREEN and exact differential checks**

Run:

```bash
bash tests/fixtures/test_driver/generate.sh --check
cargo test -p flpdf-qtest-tools --test driver_goldens
bash scripts/qpdf-test-driver-diff.sh --check
```

Expected final differential line:

```text
qpdf and flpdf test_driver outputs match 46 fixtures and 11 CLI probes
```

- [ ] **Step 7: Commit production wiring and oracle fixture**

```bash
git add crates/flpdf-qtest-tools/src/driver/test_0_1.rs tests/fixtures/test_driver/generate.sh tests/fixtures/test_driver/README.md tests/fixtures/test_driver/stream_filter_chain_17.pdf tests/fixtures/test_driver/stream_filter_chain_17.out scripts/qpdf-test-driver-diff.sh
git commit -m "fix(qtest): match qpdf filter chain depth"
```

---

### Task 4: Persist the TIFF Predictor 2 follow-up

**Files:**
- External state: Beads issue `flpdf-n9t0.10`
- External state: Beads issue `flpdf-n9t0.2` notes

**Interfaces:**
- Consumes: parent `flpdf-n9t0`, blocker `flpdf-n9t0.2`, and review comment `discussion_r3678686116`.
- Produces: open feature `flpdf-n9t0.10` depending on `.2`, with qpdf-faithful Flate/LZW TIFF predictor acceptance.
- Preserves: no Predictor 2 implementation or fixed claim in PR #591.

- [ ] **Step 1: Prove the target ID is unused**

Run:

```bash
bd show flpdf-n9t0.10 --json
```

Expected: Beads reports that `flpdf-n9t0.10` does not exist. If it exists with the exact scope below, update it rather than creating a duplicate.

- [ ] **Step 2: Create the exact follow-up**

Run:

```bash
bd create \
  --parent flpdf-n9t0 \
  --deps flpdf-n9t0.2 \
  --type feature \
  --priority 2 \
  --external-ref "gh-591#discussion_r3678686116" \
  --title "flpdf: qpdf-compatible TIFF Predictor 2 for Flate and LZW" \
  --description "PR #591 review follow-up. Pinned qpdf 11.9.0 routes Predictor 2 through Pl_TIFFPredictor for both FlateDecode and LZWDecode. Implement that component faithfully rather than adding a test-driver exception." \
  --acceptance "Cover 1/2/4/8/16 BitsPerComponent, multiple Colors, row reset, partial-row zero padding at finish, invalid Columns/Colors/BitsPerComponent geometry, Flate and LZW composition, pinned qpdf test-driver merged output/status, and 100% changed executable-line coverage." \
  --json
```

Expected: the creation JSON returns the parent-derived ID `flpdf-n9t0.10`.

- [ ] **Step 3: Read back hierarchy and dependency**

Run:

```bash
bd show flpdf-n9t0.10 --json
bd dep list flpdf-n9t0.10
```

Expected: status `open`, parent `flpdf-n9t0`, and dependency on `flpdf-n9t0.2`.

- [ ] **Step 4: Persist tracker state**

Run:

```bash
bd dolt push
```

Expected: the Dolt push succeeds. Benign `.beads/issues.jsonl` auto-export warnings do not override the authoritative Dolt result.

---

### Task 5: Run whole-branch quality, coverage, and regression gates

**Files:**
- Verify: all files changed since `origin/main`
- Modify only if a gate exposes a real defect: the smallest owning source/test file

**Interfaces:**
- Consumes: Tasks 1-4 completed and committed, clean Git worktree, pinned qpdf tree.
- Produces: fresh evidence for formatting, focused behavior, all-feature workspace behavior, lint, rustdoc, module docs, differential parity, and 100% patch coverage.

- [ ] **Step 1: Confirm committed scope and clean tree**

Run:

```bash
git status --short
git diff --check origin/main...HEAD
git log --oneline origin/feat/flpdf-n9t0-2-test-driver..HEAD
```

Expected: no uncommitted Git files, no whitespace errors, and only the approved follow-up commits after the former remote head.

- [ ] **Step 2: Run formatting and focused tests**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test stream_decode_recovery_public_api
cargo test -p flpdf filters::tests
cargo test -p flpdf --test check_tests
cargo test -p flpdf-qtest-tools --test driver_cli
cargo test -p flpdf-qtest-tools --test driver_goldens
cargo test -p flpdf-qtest-tools
bash tests/fixtures/test_driver/generate.sh --check
bash scripts/qpdf-test-driver-diff.sh --check
```

Expected: all pass and the differential reports 46 fixtures plus 11 CLI probes.

- [ ] **Step 3: Run workspace lint and all-feature tests**

Run:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Expected: both pass with no warnings promoted to errors and no test failures.

- [ ] **Step 4: Run strict docs gates**

Run:

```bash
python3 scripts/qpdf-module-docs.py --check
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
```

Expected: generated correspondence docs are current and strict private rustdoc succeeds.

- [ ] **Step 5: Prove no new coverage exclusion was added**

Run:

```bash
git diff --unified=0 origin/main...HEAD -- crates/flpdf/src crates/flpdf-qtest-tools/src | rg '^\\+.*(cov:ignore|coverage off|coverage:ignore)' || true
```

Expected: no matches attributable to this branch.

- [ ] **Step 6: Run fresh authoritative patch coverage**

Run:

```bash
scripts/patch-coverage.sh --base origin/main
```

Expected: 100% for changed executable lines under `crates/flpdf/src` and 100% for report-only changed executable lines, with zero uncovered lines.

If a line is uncovered, add a behaviorally meaningful focused test, run its RED mutation check when feasible, rerun the focused suite, commit the smallest test-only fix, and rerun this fresh coverage command.

- [ ] **Step 7: Record final evidence in Beads and persist it**

Append a note to `flpdf-n9t0.2` naming:

```text
the final commit, 46 fixtures + 11 probes, non-UTF-8 Unix valid/missing path tests,
workspace fmt/clippy/tests, strict rustdoc, module docs, exact fresh coverage
numerators, and flpdf-n9t0.10 as the TIFF follow-up
```

Then run:

```bash
bd dolt push
```

Expected: Beads state is durable; keep `flpdf-n9t0.2` in progress until push, GitHub CI, thread replies, and readback complete.

---

### Task 6: Push, verify CI, and reply to the two fixed review threads

**Files:**
- External state: remote branch `feat/flpdf-n9t0-2-test-driver`
- External state: PR #591 checks
- External state: GitHub review threads `PRRT_kwDOSYPosM6U7uik` and `PRRT_kwDOSYPosM6U7uiu`

**Interfaces:**
- Consumes: clean locally verified branch and persisted Beads state.
- Produces: pushed PR head, all-green CI, one technical in-thread reply for non-UTF-8 paths, one for the overlong filter chain, and GraphQL readback proving replies exist.
- Preserves: DCT thread `PRRT_kwDOSYPosM6U5gwc`, TIFF thread `PRRT_kwDOSYPosM6U7uip`, and all four thread resolution states as unresolved.

- [ ] **Step 1: Reconcile and push the branch**

Run:

```bash
git status --short --branch
git fetch --prune origin
git rebase origin/feat/flpdf-n9t0-2-test-driver
git push origin feat/flpdf-n9t0-2-test-driver
```

Before push, state that this updates PR #591 with the verified follow-up commits. Expected: push succeeds without force.

- [ ] **Step 2: Prove local, remote, and PR head identity**

Run:

```bash
git rev-parse HEAD
git rev-parse origin/feat/flpdf-n9t0-2-test-driver
gh pr view 591 --json headRefOid
```

Expected: all three OIDs are identical.

- [ ] **Step 3: Wait for all PR checks and inspect failures if any**

Run:

```bash
gh pr checks 591 --watch --interval 15
gh pr checks 591
```

Expected: Ubuntu, ARM, macOS, Windows, Quality, Coverage, Fuzz, CodeQL, and codecov checks are successful. If a check fails, inspect its GitHub Actions log, reproduce locally, fix via a new RED→GREEN commit, rerun the relevant local and coverage gates, push, and wait again.

- [ ] **Step 4: Reply in the non-UTF-8 path thread**

Reply to `PRRT_kwDOSYPosM6U7uik` in-thread with the pushed commit OID and exact evidence:

```text
Fixed by carrying args as OsString/OsStr through the filesystem boundary and
writing Unix filename/argv0 diagnostics from raw OsStrExt bytes. The new Unix
tests cover both a valid PDF at an invalid-UTF-8 path and a missing invalid-
UTF-8 path (raw bytes, exit 2, no panic); existing cross-platform driver tests
and Windows CI remain green.
```

Do not resolve the thread.

- [ ] **Step 5: Reply in the overlong-chain thread**

Reply to `PRRT_kwDOSYPosM6U7uiu` in-thread with the pushed commit OID and exact evidence:

```text
Fixed with an explicit driver policy rather than removing library hardening:
DecodeLimits now defaults to max_filter_chain=Some(16), while test_driver test
0/1 alone passes None to the ordered recovering API. The new flpdf-authored
17-stage ASCIIHex fixture matches pinned qpdf 11.9.0; the full differential is
46 fixtures + 11 CLI probes, and the default public API still rejects 17.
```

Do not resolve the thread.

- [ ] **Step 6: Read back all four review threads**

Use GitHub GraphQL to read:

```text
PRRT_kwDOSYPosM6U5gwc  DCTDecode
PRRT_kwDOSYPosM6U7uik  non-UTF-8 path
PRRT_kwDOSYPosM6U7uip  TIFF Predictor 2
PRRT_kwDOSYPosM6U7uiu  overlong filter chain
```

Expected:

- the non-UTF-8 and overlong-chain threads contain the new replies;
- DCT has no fixed claim and remains tracked by `flpdf-n9t0.9`;
- TIFF has no fixed claim and remains tracked by `flpdf-n9t0.10`;
- all four have `isResolved: false`.

- [ ] **Step 7: Final tracker persistence and handoff**

Append PR head, GitHub run/check result, reply URLs or IDs, and unresolved readback to `flpdf-n9t0.2`; run:

```bash
bd dolt push
git status --short --branch
```

Expected: Beads push succeeds, the Git worktree is clean and synchronized, and `flpdf-n9t0.2` remains open/in-progress pending merge.
