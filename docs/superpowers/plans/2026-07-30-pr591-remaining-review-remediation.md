# PR #591 Remaining Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the three selected unresolved PR #591 findings while preserving qpdf 11.9.0 diagnostics, fallback bounds, and strict decode semantics.

**Architecture:** Share the existing bounded object-read budget with stream-offset lookup, attach repair diagnostics to a new crate error variant when recovery ultimately fails, and parameterize the shared decode engine so only recovering callers retain `Data` events. Track DCTDecode parity separately because it adds a lossy image decoder rather than repairing these existing boundaries.

**Tech Stack:** Rust workspace, `thiserror`, flpdf parser/xref/filter pipeline, pinned qpdf 11.9.0 `test_driver`, Beads, GitHub GraphQL review threads.

## Global Constraints

- Pinned qpdf 11.9.0 source and live output are authoritative.
- Use RED → GREEN → REFACTOR for every production change.
- Keep the shared `resolution_fallbacks_remaining` cap; do not add an unbounded read-to-EOF path.
- Preserve warning/error/data order and all existing recovering decode events.
- Public backward compatibility is subordinate to qpdf parity for this pre-1.0 crate.
- Changed executable-line coverage must be 100%.
- Do not resolve GitHub review threads unless the user explicitly requests resolution.
- Do not vendor qpdf-owned qtest fixtures.

---

### Task 1: Retry the full object for stream-data offset lookup

**Files:**
- Modify: `crates/flpdf/src/reader.rs:884-909`
- Test: `crates/flpdf/src/reader.rs` reader unit tests

**Interfaces:**
- Preserves: `Pdf::source_stream_data_offset(&mut self, ObjectRef) -> Result<Option<u64>>`
- Consumes: `Pdf::next_object_offset`, `Pdf::resolution_fallbacks_remaining`
- Produces: bounded parse followed by one budgeted EOF retry

- [ ] **Step 1: Add a malformed-xref fixture helper and failing test**

Add a unit-test helper that writes object 1 as a valid stream, but records
object 2's uncompressed xref offset inside object 1 before its dictionary and
stream framing are complete:

```rust
fn stream_with_false_next_xref_offset() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let stream_offset = bytes.len();
    bytes.extend_from_slice(
        b"1 0 obj\n<< /Filter [ /FlateDecode /FlateDecode ] \
          /DecodeParms [ null ] /Length 3 >>\nstream\nabc\nendstream\nendobj\n",
    );
    let false_next = stream_offset + b"1 0 obj\n<< /Filter".len();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 3\n\
             0000000000 65535 f \n\
             {stream_offset:010} 00000 n \n\
             {false_next:010} 00000 n \n\
             trailer\n<< /Size 3 /Root 1 0 R /QTest 1 0 R >>\n\
             startxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    bytes
}
```

Add:

```rust
#[test]
fn source_stream_data_offset_retries_after_false_next_object_offset() {
    let bytes = stream_with_false_next_xref_offset();
    let expected = bytes
        .windows(b"\nstream\nabc".len())
        .position(|window| window == b"\nstream\nabc")
        .expect("stream marker")
        + b"\nstream\n".len();
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open false-next-offset PDF");

    assert!(matches!(
        pdf.resolve(ObjectRef::new(1, 0)).expect("ordinary bounded fallback"),
        Object::Stream(_)
    ));
    assert_eq!(
        pdf.source_stream_data_offset(ObjectRef::new(1, 0))
            .expect("offset lookup uses the same fallback"),
        Some(expected as u64)
    );
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p flpdf reader::tests::source_stream_data_offset_retries_after_false_next_object_offset -- --exact
```

Expected: FAIL from `parse_file_object_syntax` because the false object-2
offset truncates object 1's bounded window.

- [ ] **Step 3: Implement the budgeted full retry**

Extract a private helper used by `source_stream_data_offset`:

```rust
fn parse_source_file_object_at(
    &mut self,
    offset: u64,
) -> Result<PendingFileObject> {
    let next = self.next_object_offset(offset);
    self.reader.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    match next {
        Some(next) => self
            .reader
            .by_ref()
            .take(next.saturating_sub(offset))
            .read_to_end(&mut bytes)?,
        None => self.reader.read_to_end(&mut bytes)?,
    };

    match parse_file_object_syntax(&bytes) {
        Ok(pending) => Ok(pending),
        Err(window_error)
            if next.is_some() && self.resolution_fallbacks_remaining > 0 =>
        {
            self.resolution_fallbacks_remaining -= 1;
            self.reader.seek(SeekFrom::Start(offset))?;
            let mut full = Vec::new();
            self.reader.read_to_end(&mut full)?;
            parse_file_object_syntax(&full).or(Err(window_error))
        }
        Err(error) => Err(error),
    }
}
```

Replace the inline bounded read in `source_stream_data_offset` with this
helper. Keep `PendingBody::Stream { data_start, .. }` as the only accepted
body.

- [ ] **Step 4: Verify GREEN and adjacent reader behavior**

Run:

```bash
cargo test -p flpdf reader::tests::source_stream_data_offset_retries_after_false_next_object_offset -- --exact
cargo test -p flpdf reader::tests::source_stream_data_offset_comes_from_parsed_object_framing -- --exact
cargo test -p flpdf reader::tests::normal_resolution_retries_when_bounded_window_ends_inside_stream_payload -- --exact
cargo fmt --all -- --check
```

Expected: all pass.

- [ ] **Step 5: Commit Task 1**

```bash
git add crates/flpdf/src/reader.rs
git commit -m "fix(reader): retry bounded stream offset parsing"
```

---

### Task 2: Preserve repair diagnostics on terminal open failure

**Files:**
- Modify: `crates/flpdf/src/error.rs`
- Modify: `crates/flpdf/src/xref.rs`
- Modify: `crates/flpdf-qtest-tools/src/driver/mod.rs`
- Modify: `tests/fixtures/test_driver/generate.sh`
- Modify: `scripts/qpdf-test-driver-diff.sh`
- Create: `tests/fixtures/test_driver/open_repair_failure.pdf`
- Create: `tests/fixtures/test_driver/open_repair_failure.out`
- Test: `crates/flpdf/src/xref.rs`
- Test: `crates/flpdf-qtest-tools/src/driver/mod.rs`
- Test: `crates/flpdf-qtest-tools/tests/driver_goldens.rs`

**Interfaces:**
- Produces:
  `Error::OpenFailure { source: Box<Error>, diagnostics: Diagnostics }`
- Produces:
  `Error::open_failure(&self) -> Option<(&Error, &Diagnostics)>`
- Preserves: existing `Error` display text by delegating to `source`
- Consumes: existing `write_warning` and `write_error` driver boundaries

- [ ] **Step 1: Add the failed-recovery fixture to the generator**

Add `open_repair_failure` to the generator and differential inventories. Its
bytes are:

```text
%PDF-1.7
startxref
0
%%EOF
```

Generate the PDF deterministically in `generate.sh`. Generate its `.out` only
from pinned qpdf 11.9.0:

```bash
bash tests/fixtures/test_driver/generate.sh --generate
bash scripts/qpdf-test-driver-diff.sh --regenerate
```

Inspect the oracle output and record the exact three warnings, terminal error,
and exit status in the test expectation. Do not edit the `.out` manually.

- [ ] **Step 2: Add failing xref and driver tests**

In `xref.rs`, add:

```rust
#[test]
fn failed_repair_retains_qpdf_warning_sequence() {
    let mut input = Cursor::new(b"%PDF-1.7\nstartxref\n0\n%%EOF\n");
    let error =
        load_xref_and_trailer_with_repair(&mut input, true).expect_err("repair must fail");
    let (source, diagnostics) = error
        .open_failure()
        .expect("repair failure carries diagnostics");

    assert_eq!(
        diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>(),
        vec![
            "file is damaged",
            "can't find startxref",
            "Attempting to reconstruct cross-reference table",
        ]
    );
    assert!(source.to_string().contains("indirect object"));
}
```

Add a driver CLI test that invokes test 1 on `open_repair_failure.pdf` and
asserts the committed qpdf merged output and exit 2.

- [ ] **Step 3: Run the focused tests and verify RED**

Run:

```bash
cargo test -p flpdf xref::tests::failed_repair_retains_qpdf_warning_sequence -- --exact
cargo test -p flpdf-qtest-tools open_repair_failure -- --nocapture
```

Expected: the first test cannot find `Error::open_failure`; the driver emits
only the terminal error instead of the qpdf warning prefix.

- [ ] **Step 4: Add the open-failure error variant**

In `error.rs`, add:

```rust
#[error("{source}")]
OpenFailure {
    #[source]
    source: Box<Error>,
    diagnostics: crate::Diagnostics,
},
```

Add:

```rust
pub fn open_failure(&self) -> Option<(&Error, &crate::Diagnostics)> {
    match self {
        Self::OpenFailure {
            source,
            diagnostics,
        } => Some((source.as_ref(), diagnostics)),
        _ => None,
    }
}

pub(crate) fn with_open_diagnostics(
    source: Error,
    diagnostics: crate::Diagnostics,
) -> Error {
    if diagnostics.entries().is_empty() {
        source
    } else {
        Self::OpenFailure {
            source: Box::new(source),
            diagnostics,
        }
    }
}
```

Document that the wrapper is created only after repair warnings exist.

- [ ] **Step 5: Wrap terminal linear-scan failures**

In `recover_xref_from_linear_scan`, build `repair_diagnostics` before
`recover_xref_entries` and `recover_trailer`. Wrap either terminal failure with
a clone of the already-created diagnostics:

```rust
let mut repair_diagnostics = Diagnostics::default();
push_repair_diagnostics(&mut repair_diagnostics, &trigger_error, startxref);

let entries = recover_xref_entries(bytes).map_err(|error| {
    Error::with_open_diagnostics(error, repair_diagnostics.clone())
})?;
let trailer = match (recover_trailer(bytes), fallback_trailer) {
    (Ok(trailer), _) => trailer,
    (Err(_), Some(trailer)) => trailer.clone(),
    (Err(error), None) => {
        return Err(Error::with_open_diagnostics(
            error,
            repair_diagnostics,
        ));
    }
};
```

Do not wrap header failures or strict-open failures that occur before repair.

- [ ] **Step 6: Emit failed-open diagnostics in the driver**

Replace the open error arm with a helper that:

```rust
fn write_open_failure(
    n: i32,
    filename: &str,
    error: &Error,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let source = if let Some((source, diagnostics)) = error.open_failure() {
        for diagnostic in diagnostics.entries() {
            if write_warning(filename, diagnostic, stdout, stderr).is_err() {
                return 2;
            }
        }
        source
    } else {
        error
    };
    write_error(stdout, stderr, &open_pdf_error(n, filename, source))
}
```

Use it from `run`. Add a write-failure regression proving the terminal error is
not attempted after warning output fails.

- [ ] **Step 7: Verify GREEN and the pinned differential**

Run:

```bash
cargo test -p flpdf xref::tests::failed_repair_retains_qpdf_warning_sequence -- --exact
cargo test -p flpdf-qtest-tools open_repair_failure -- --nocapture
cargo test -p flpdf-qtest-tools --test driver_goldens
bash tests/fixtures/test_driver/generate.sh --check
bash scripts/qpdf-test-driver-diff.sh --check
cargo fmt --all -- --check
```

Expected: all pass and the differential count increases by one fixture.

- [ ] **Step 8: Commit Task 2**

```bash
git add crates/flpdf/src/error.rs crates/flpdf/src/xref.rs \
  crates/flpdf-qtest-tools/src/driver/mod.rs \
  tests/fixtures/test_driver/generate.sh \
  scripts/qpdf-test-driver-diff.sh \
  tests/fixtures/test_driver/open_repair_failure.pdf \
  tests/fixtures/test_driver/open_repair_failure.out
git commit -m "fix(qtest): preserve failed repair diagnostics"
```

---

### Task 3: Suppress duplicate data events on strict decode

**Files:**
- Modify: `crates/flpdf/src/filters.rs`
- Test: `crates/flpdf/src/filters.rs`
- Test: `crates/flpdf/tests/stream_decode_recovery_public_api.rs`

**Interfaces:**
- Produces internal enum: `DataEventMode::{Record, Suppress}`
- Preserves: `decode_stream_data_recovering` public event output
- Preserves: `decode_stream_data` and warning callback error ordering

- [ ] **Step 1: Add a failing strict-mode test**

Introduce the wished-for internal call in a unit test:

```rust
#[test]
fn strict_decode_retains_data_without_recording_a_duplicate_data_event() {
    let encoded = encode_stream_data(&flate_dict(), b"strict payload").unwrap();
    let outcome = decode_stream_data_recovering_with_limits_and_mode(
        &flate_dict(),
        &encoded,
        DecodeLimits::default(),
        DataEventMode::Suppress,
    )
    .unwrap();

    assert_eq!(outcome.data, b"strict payload");
    assert!(!outcome
        .events
        .iter()
        .any(|event| matches!(event, StreamDecodeEvent::Data(_))));
}
```

The production change that makes this pass is the new internal mode threaded
through all final-stage data-event sites.

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p flpdf filters::tests::strict_decode_retains_data_without_recording_a_duplicate_data_event -- --exact
```

Expected: compile failure because the mode-aware function and enum do not yet
exist.

- [ ] **Step 3: Add the internal event mode**

Add:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DataEventMode {
    Record,
    Suppress,
}

impl DataEventMode {
    fn push(self, events: &mut Vec<StreamDecodeEvent>, data: &[u8]) {
        if self == Self::Record && !data.is_empty() {
            events.push(StreamDecodeEvent::Data(data.to_vec()));
        }
    }
}
```

Rename the existing internal recovering function to:

```rust
fn decode_stream_data_recovering_with_limits_and_mode(
    dict: &Dictionary,
    stream_data: &[u8],
    limits: DecodeLimits,
    data_events: DataEventMode,
) -> Result<StreamDecodeOutcome>
```

The public recovering entry point passes `Record`. The strict warning wrapper
passes `Suppress`.

- [ ] **Step 4: Thread the mode through the shared engine**

Add `data_events: DataEventMode` to `decode_stream_data_with_filters` and
`decode_stream_data_with_filters_and_crypt`. Replace every:

```rust
events.push(StreamDecodeEvent::Data(data.clone()));
events.push(StreamDecodeEvent::Data(slice.to_vec()));
```

with:

```rust
data_events.push(&mut events, data_or_slice);
```

Do not guard warning or error events. Update direct internal test callers to
pass `DataEventMode::Record` unless the test explicitly verifies strict mode.

- [ ] **Step 5: Verify strict and recovering GREEN**

Run:

```bash
cargo test -p flpdf filters::tests::strict_decode_retains_data_without_recording_a_duplicate_data_event -- --exact
cargo test -p flpdf filters::tests::recovering_decode_retains_partial_bytes_after_codec_error -- --exact
cargo test -p flpdf filters::tests::strict_replay_delivers_warning_after_error_and_keeps_the_runtime_error -- --exact
cargo test -p flpdf --test stream_decode_recovery_public_api
cargo fmt --all -- --check
```

Expected: strict mode has no `Data` events, recovering output and event ordering
remain unchanged.

- [ ] **Step 6: Commit Task 3**

```bash
git add crates/flpdf/src/filters.rs \
  crates/flpdf/tests/stream_decode_recovery_public_api.rs
git commit -m "fix(filters): avoid strict decode data cloning"
```

---

### Task 4: Track DCTDecode parity as a follow-up Bead

**Files:**
- Tracker only: Beads

**Interfaces:**
- Parent: `flpdf-n9t0`
- Depends on: `flpdf-n9t0.2`
- Does not modify: Git or GitHub

- [ ] **Step 1: Check for an existing duplicate**

Run:

```bash
bd search DCTDecode
bd search "test_driver DCT"
```

If an existing open issue has the same qpdf `qpdf_dl_all` scope, update it
instead of creating a duplicate.

- [ ] **Step 2: Create the follow-up**

If no duplicate exists:

```bash
bd create \
  --id=flpdf-n9t0.9 \
  --title="flpdf: test_driver qpdf_dl_all DCTDecode parity" \
  --type=feature \
  --priority=2 \
  --parent=flpdf-n9t0 \
  --deps=flpdf-n9t0.2 \
  --description="PR #591 review follow-up. Pinned qpdf 11.9.0 test_driver uses qpdf_dl_all and decodes /DCTDecode and /DCT through SF_DCTDecode/Pl_DCT. Implement compatible DCT decoding for the test-driver path without regressing writer passthrough behavior." \
  --acceptance="Use a valid flpdf-authored JPEG fixture; reproduce pinned qpdf test_driver merged output and status for /DCTDecode and /DCT; settle raw component bytes, /ColorTransform, malformed JPEG diagnostics, and decode limits against qpdf 11.9.0 source/output; keep writer passthrough unless oracle evidence requires otherwise; add focused tests and 100% changed-line coverage."
```

- [ ] **Step 3: Verify and persist the issue**

```bash
bd show flpdf-n9t0.9
bd dolt push
```

Expected: the new issue is open under `flpdf-n9t0`, depends on
`flpdf-n9t0.2`, and Dolt push succeeds.

---

### Task 5: Run complete verification, publish, and reply in-thread

**Files:**
- Verify: all branch changes
- Update: Bead `flpdf-n9t0.2` notes
- GitHub: three selected review threads

**Interfaces:**
- Thread IDs:
  - bounded offset: `PRRT_kwDOSYPosM6UxVXH`
  - failed-open diagnostics: `PRRT_kwDOSYPosM6UxVXQ`
  - strict decode clone: `PRRT_kwDOSYPosM6U5gwe`
- Leaves unresolved: DCT thread `PRRT_kwDOSYPosM6U5gwc`

- [ ] **Step 1: Run focused package gates**

```bash
cargo fmt --all -- --check
cargo test -p flpdf-qtest-tools
bash tests/fixtures/test_driver/generate.sh --check
bash scripts/qpdf-test-driver-diff.sh --check
```

- [ ] **Step 2: Run workspace quality gates**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
python3 scripts/qpdf-module-docs.py --check
```

- [ ] **Step 3: Run fresh 100% patch coverage**

```bash
bash scripts/patch-coverage.sh --base origin/main
```

Expected: both flpdf and report-only changed executable-line totals are 100%.
Do not reuse an earlier LCOV report.

- [ ] **Step 4: Run branch hygiene checks**

```bash
git diff --check origin/main...HEAD
git status --short
```

Expected: no whitespace errors and a clean worktree.

- [ ] **Step 5: Update and persist Beads**

Append the three commit subjects, the 36-fixture/10-probe differential result,
the measured 100% coverage totals printed in Step 3, and follow-up
`flpdf-n9t0.9` to `flpdf-n9t0.2` notes:

```bash
bd update flpdf-n9t0.2 --append-notes="PR #591 remaining review remediation: bounded stream-offset fallback, failed-open repair diagnostics, and strict decode no-clone fixes committed; pinned qpdf differential 36 fixtures + 10 CLI probes; fresh origin/main patch coverage 100%; DCTDecode parity tracked by flpdf-n9t0.9."
bd dolt push
```

Keep `flpdf-n9t0.2` in progress until PR #591 is merged.

- [ ] **Step 6: Push the branch**

```bash
git push origin feat/flpdf-n9t0-2-test-driver
```

Confirm local, remote, and PR head OIDs match.

- [ ] **Step 7: Monitor GitHub CI**

```bash
gh pr checks 591 --watch --interval 10
```

If a check fails, use `github:gh-fix-ci` before changing code.

- [ ] **Step 8: Reply to the three selected inline threads**

After CI is green, use `addPullRequestReviewThreadReply` for each selected
thread. Each reply must name the commit, root-cause fix, focused regression,
pinned qpdf differential evidence where applicable, and final coverage.

Do not reply to the DCT thread as fixed. Post a concise follow-up-tracking reply
there only if the user separately authorizes that GitHub write.

- [ ] **Step 9: Read back thread state**

Run the bundled `fetch_comments.py` and verify:

- the three selected threads contain the new replies;
- no selected thread was accidentally resolved;
- the DCT thread remains unresolved and has no false fixed claim.

Report PR URL, pushed head, gates, follow-up Bead ID, reply state, and retained
worktree.
