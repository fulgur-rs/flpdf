# Test Driver Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address all three actionable PR #591 review threads with qpdf 11.9.0-compatible reference provenance, lazy array indirectness, and filter-aware `/DecodeParms` resolution.

**Architecture:** Keep the changes inside `flpdf-qtest-tools`. Extend `Handle` with terminal-reference provenance, replace eager array child handles with raw indirectness metadata, and resolve only the decode-parameter keys consumed by each resolved filter. Preserve the first reference for unparse identity and use the terminal reference only for source warning locations.

**Tech Stack:** Rust 2021, `flpdf` object model, qpdf 11.9.0 pinned oracle, Cargo integration tests, Bash/Python fixture generation.

## Global Constraints

- qpdf 11.9.0 behavior and generated output are authoritative.
- Keep the public `flpdf` API unchanged.
- Preserve current unparse bytes, warning text, warning ordering, and exit status.
- Keep all production changes internal to `crates/flpdf-qtest-tools`.
- Use RED-GREEN-REFACTOR for every review thread.
- CI patch coverage must remain 100% for changed executable lines.
- Do not reply to or resolve GitHub review threads without separate authorization.

## File Responsibilities

- `crates/flpdf-qtest-tools/src/driver/handle.rs`: qpdf-shaped object-handle provenance, array metadata, and stream dictionary preparation.
- `crates/flpdf-qtest-tools/src/driver/test_0_1.rs`: `test_driver 1` output behavior and focused regression tests.
- `tests/fixtures/test_driver/generate.sh`: deterministic PDF inputs for the pinned differential corpus.
- `scripts/qpdf-test-driver-diff.sh`: trusted fixture inventory and qpdf/Rust byte comparison.
- `tests/fixtures/test_driver/*.pdf`: committed generated inputs.
- `tests/fixtures/test_driver/*.out`: committed qpdf 11.9.0 merged-output goldens.

---

### Task 1: Preserve the terminal stream reference

**Files:**
- Modify: `crates/flpdf-qtest-tools/src/driver/handle.rs:7-18`
- Modify: `crates/flpdf-qtest-tools/src/driver/handle.rs:174-197`
- Test: `crates/flpdf-qtest-tools/src/driver/test_0_1.rs:314-354`

**Interfaces:**
- Consumes: `Pdf::source_stream_data_offset(ObjectRef) -> Result<Option<u64>>`.
- Produces: `Handle::terminal_indirect_ref(&self) -> Option<ObjectRef>`.
- Preserves: `Handle::indirect_ref()` continues to return the first reference.

- [ ] **Step 1: Write the failing chained-stream warning test**

Add a test beside `decode_parms_length_mismatch_reports_qpdf_warning`:

```rust
#[test]
fn chained_stream_warning_uses_terminal_stream_offset() {
    let stream = b"<< /Filter [ /FlateDecode /FlateDecode ] \
                   /DecodeParms [ null ] /Length 3 >>\n\
                   stream\nabc\nendstream"
        .to_vec();
    let bytes = pdf_with_qtest(
        b"6 0 R",
        &[(6, b"7 0 R".to_vec()), (7, stream)],
    );
    let options = PdfOpenOptions {
        repair: true,
        ..PdfOpenOptions::default()
    };
    let mut pdf = Pdf::open_mem_owned_with_options(bytes, options)
        .expect("open chained stream fixture");
    let terminal_offset = pdf
        .source_stream_data_offset(flpdf::ObjectRef::new(7, 0))
        .expect("locate terminal stream")
        .expect("terminal stream offset");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut diagnostics_written = pdf.repair_diagnostics().entries().len();

    run_test_0_1(
        &mut pdf,
        "fixture.pdf",
        &mut stdout,
        &mut stderr,
        &mut diagnostics_written,
    )
    .expect("run chained stream fixture");

    assert_eq!(
        stderr,
        format!(
            "WARNING: fixture.pdf (offset {terminal_offset}): \
             stream /DecodeParms length is inconsistent with filters\n"
        )
        .into_bytes()
    );
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p flpdf-qtest-tools \
  driver::test_0_1::tests::chained_stream_warning_uses_terminal_stream_offset \
  -- --exact
```

Expected: FAIL because the warning has no `(offset N)` segment; the current
`indirect_ref()` returns object `6`, whose body is not a stream.

- [ ] **Step 3: Track first and terminal references separately**

Change `Handle` and `resolve_chain` to:

```rust
pub(crate) struct Handle {
    resolved: Object,
    indirect: Option<ObjectRef>,
    terminal_indirect: Option<ObjectRef>,
}

pub(crate) fn from_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Object,
) -> flpdf::Result<Self> {
    let (resolved, indirect, terminal_indirect) = resolve_chain(pdf, value)?;
    Ok(Self {
        resolved,
        indirect,
        terminal_indirect,
    })
}

pub(crate) fn terminal_indirect_ref(&self) -> Option<ObjectRef> {
    self.terminal_indirect
}

fn resolve_chain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mut value: Object,
) -> flpdf::Result<(Object, Option<ObjectRef>, Option<ObjectRef>)> {
    let mut indirect = None;
    let mut terminal_indirect = None;
    for _ in 0..MAX_REF_CHAIN_DEPTH {
        let Object::Reference(reference) = value else {
            return Ok((value, indirect, terminal_indirect));
        };
        indirect.get_or_insert(reference);
        terminal_indirect = Some(reference);
        value = pdf.resolve_borrowed(reference)?.clone();
    }
    if matches!(value, Object::Reference(_)) {
        Err(Error::parse(
            0,
            format!("object reference chain exceeds {MAX_REF_CHAIN_DEPTH} hops"),
        ))
    } else {
        Ok((value, indirect, terminal_indirect))
    }
}
```

Update the test-only manual `Handle` construction in
`qpdf_type_codes_and_names_are_explicit` with
`terminal_indirect: None`.

Change the warning-offset lookup in `test_0_1.rs` from `indirect_ref()` to
`terminal_indirect_ref()`.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-qtest-tools \
  driver::test_0_1::tests::chained_stream_warning_uses_terminal_stream_offset \
  -- --exact
cargo test -p flpdf-qtest-tools driver::handle::tests --lib
cargo test -p flpdf-qtest-tools driver::test_0_1::tests --lib
```

Expected: all commands PASS. Existing chained-reference unparse tests must
still emit the first reference.

- [ ] **Step 5: Commit the provenance fix**

```bash
git add crates/flpdf-qtest-tools/src/driver/handle.rs \
  crates/flpdf-qtest-tools/src/driver/test_0_1.rs
git commit -m "fix(qtest): locate warnings at terminal streams"
```

---

### Task 2: Report array indirectness without child resolution

**Files:**
- Modify: `crates/flpdf-qtest-tools/src/driver/handle.rs:56-70`
- Modify: `crates/flpdf-qtest-tools/src/driver/test_0_1.rs:80-88`
- Test: `crates/flpdf-qtest-tools/src/driver/test_0_1.rs:260-285`
- Test: `crates/flpdf-qtest-tools/src/driver/handle.rs:381-407`

**Interfaces:**
- Produces: `Handle::array_item_indirectness(&self) -> flpdf::Result<Vec<bool>>`.
- Removes: `Handle::array_items(&self, pdf)`.
- Does not resolve: any raw `Object::Reference` stored in the array.

- [ ] **Step 1: Write the failing over-limit child-chain test**

Add this test beside the existing array output test:

```rust
#[test]
fn array_reports_indirect_child_without_resolving_its_target() {
    let mut extras = Vec::new();
    for number in 100..=164 {
        let value = if number == 164 {
            b"true".to_vec()
        } else {
            format!("{} 0 R", number + 1).into_bytes()
        };
        extras.push((number, value));
    }

    let actual = output(b"[ 100 0 R ]", &extras);
    assert!(actual.windows(b"  item 0 is indirect\n".len()).any(|line| {
        line == b"  item 0 is indirect\n"
    }));
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p flpdf-qtest-tools \
  driver::test_0_1::tests::array_reports_indirect_child_without_resolving_its_target \
  -- --exact
```

Expected: FAIL at `run test_0_1` with
`object reference chain exceeds 64 hops`, proving `array_items` eagerly
resolves the child.

- [ ] **Step 3: Replace child handles with raw indirectness metadata**

Replace `array_items` with:

```rust
pub(crate) fn array_item_indirectness(&self) -> flpdf::Result<Vec<bool>> {
    let values = self
        .resolved
        .as_array()
        .ok_or_else(|| Error::System("array access on non-array object".to_string()))?;
    Ok(values
        .iter()
        .map(|value| matches!(value, Object::Reference(_)))
        .collect())
}
```

Change the `test_0_1` array branch to:

```rust
for (index, is_indirect) in qtest.array_item_indirectness()?.into_iter().enumerate() {
    let direct_prefix = if is_indirect { "in" } else { "" };
    writeln!(stdout, "  item {index} is {direct_prefix}direct")?;
}
```

Update `array_and_dictionary_items_preserve_child_indirectness` so the array
half asserts:

```rust
assert_eq!(
    array.array_item_indirectness().expect("array item metadata"),
    vec![false, true]
);
```

Keep the dictionary half unchanged.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-qtest-tools \
  driver::test_0_1::tests::array_reports_indirect_child_without_resolving_its_target \
  -- --exact
cargo test -p flpdf-qtest-tools driver::handle::tests --lib
cargo test -p flpdf-qtest-tools driver::test_0_1::tests --lib
```

Expected: all commands PASS, including the existing exact array output test.

- [ ] **Step 5: Commit the lazy array fix**

```bash
git add crates/flpdf-qtest-tools/src/driver/handle.rs \
  crates/flpdf-qtest-tools/src/driver/test_0_1.rs
git commit -m "fix(qtest): inspect array indirectness lazily"
```

---

### Task 3: Resolve only filter-consumed decode parameters

**Files:**
- Modify: `crates/flpdf-qtest-tools/src/driver/handle.rs:206-260`
- Test: `crates/flpdf-qtest-tools/src/driver/handle.rs:421-491`
- Test: `crates/flpdf-qtest-tools/src/driver/test_0_1.rs:286-360`

**Interfaces:**
- Preserves: `resolve_stream_dictionary(pdf, source) -> flpdf::Result<Dictionary>`.
- Produces internal helpers:
  - `resolved_filter_names(&Object) -> Option<Vec<&[u8]>>`
  - `resolve_decode_params(pdf, &[&[u8]], Object) -> flpdf::Result<Object>`
  - `resolve_decode_param_dict(pdf, &[&[u8]], Dictionary) -> flpdf::Result<Dictionary>`
- Consumes recognized keys only:
  `Predictor`, `Columns`, `Colors`, `BitsPerComponent`, and LZW-only
  `EarlyChange`.

- [ ] **Step 1: Change the nesting regression to require ignored-key success**

Replace `stream_parameter_nesting_rejects_depth_64` with:

```rust
#[test]
fn stream_parameter_resolution_ignores_deep_unknown_values() {
    let mut pdf = handle_pdf(b"");
    let metadata = (0..64).fold(Object::Integer(1), |value, _| {
        Object::Array(vec![value])
    });
    let mut params = Dictionary::new();
    params.insert(b"Predictor", Object::Reference(ObjectRef::new(13, 0)));
    params.insert(b"Metadata", metadata.clone());
    let mut dictionary = Dictionary::new();
    dictionary.insert(b"Filter", Object::Name(b"FlateDecode".to_vec()));
    dictionary.insert(b"DecodeParms", Object::Dictionary(params));

    let resolved = resolve_stream_dictionary(&mut pdf, &dictionary)
        .expect("resolve consumed stream parameters");
    let resolved_params = resolved
        .get(b"DecodeParms")
        .and_then(Object::as_dict)
        .expect("resolved DecodeParms");
    assert_eq!(
        resolved_params.get(b"Predictor"),
        Some(&Object::Integer(15))
    );
    assert_eq!(resolved_params.get(b"Metadata"), Some(&metadata));
}
```

Add an end-to-end `test_0_1` test using zlib bytes already present in
`flate_stream_emits_raw_and_decoded_bytes_and_indirect_unparse`:

```rust
#[test]
fn flate_ignores_deep_unknown_decode_parameter_values() {
    let metadata = (0..64).fold(b"1".to_vec(), |value, _| {
        let mut nested = b"[ ".to_vec();
        nested.extend_from_slice(&value);
        nested.extend_from_slice(b" ]");
        nested
    });
    let compressed = b"\x78\x9c\x4b\x4c\x4a\x06\x00\x02\x4d\x01\x27";
    let mut stream =
        b"<< /Filter /FlateDecode /DecodeParms << /Metadata ".to_vec();
    stream.extend_from_slice(&metadata);
    stream.extend_from_slice(b" >> /Length 11 >>\nstream\n");
    stream.extend_from_slice(compressed);
    stream.extend_from_slice(b"\nendstream");

    let actual = output(b"7 0 R", &[(7, stream)]);
    assert!(actual.windows(b"\nabc\nEnd of stream data\n".len()).any(|line| {
        line == b"\nabc\nEnd of stream data\n"
    }));
}
```

- [ ] **Step 2: Run both tests and verify RED**

Run:

```bash
cargo test -p flpdf-qtest-tools \
  driver::handle::tests::stream_parameter_resolution_ignores_deep_unknown_values \
  -- --exact
cargo test -p flpdf-qtest-tools \
  driver::test_0_1::tests::flate_ignores_deep_unknown_decode_parameter_values \
  -- --exact
```

Expected: both FAIL with
`stream parameter nesting exceeds 64 levels`, emitted by the current recursive
`resolve_nested`.

- [ ] **Step 3: Add filter-name and recognized-key helpers**

Add:

```rust
fn resolved_filter_names(filter: &Object) -> Option<Vec<&[u8]>> {
    match filter {
        Object::Null => Some(Vec::new()),
        Object::Name(name) => Some(vec![name]),
        Object::Array(values) => values
            .iter()
            .map(Object::as_name)
            .collect::<Option<Vec<_>>>(),
        _ => None,
    }
}

fn normalized_filter_name(name: &[u8]) -> &[u8] {
    match name {
        b"Fl" => b"FlateDecode",
        b"LZW" => b"LZWDecode",
        name => name,
    }
}

fn filter_consumes_decode_key(filter: &[u8], key: &[u8]) -> bool {
    match normalized_filter_name(filter) {
        b"FlateDecode" => matches!(
            key,
            b"Predictor" | b"Columns" | b"Colors" | b"BitsPerComponent"
        ),
        b"LZWDecode" => matches!(
            key,
            b"Predictor"
                | b"Columns"
                | b"Colors"
                | b"BitsPerComponent"
                | b"EarlyChange"
        ),
        _ => false,
    }
}
```

- [ ] **Step 4: Implement selective decode-parameter resolution**

Replace the `resolve_nested` call for `/DecodeParms` with helpers that:

1. resolve the top-level parameter reference;
2. return an array unchanged when its length differs from the filter count;
3. resolve each matching array slot's reference;
4. resolve only dictionary values consumed by the paired filter; and
5. for a singleton parameter dictionary, resolve the union of keys consumed by
   all filters.

Use this implementation:

```rust
fn resolve_decode_param_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filters: &[&[u8]],
    dictionary: Dictionary,
) -> flpdf::Result<Object> {
    let entries: Vec<(Vec<u8>, Object)> = dictionary
        .iter()
        .map(|(key, value)| (key.to_vec(), value.clone()))
        .collect();
    let mut resolved = Dictionary::new();
    for (key, value) in entries {
        let value = if filters
            .iter()
            .any(|filter| filter_consumes_decode_key(filter, &key))
        {
            resolve_chain(pdf, value)?.0
        } else {
            value
        };
        resolved.insert(key, value);
    }
    Ok(Object::Dictionary(resolved))
}

fn resolve_decode_param_for_filters<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filters: &[&[u8]],
    value: Object,
) -> flpdf::Result<Object> {
    let value = resolve_chain(pdf, value)?.0;
    match value {
        Object::Dictionary(dictionary) => {
            resolve_decode_param_dict(pdf, filters, dictionary)
        }
        other => Ok(other),
    }
}

fn resolve_decode_params<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filters: &[&[u8]],
    value: Object,
) -> flpdf::Result<Object> {
    let value = resolve_chain(pdf, value)?.0;
    match value {
        Object::Array(values) if values.len() == filters.len() => {
            let values = values
                .into_iter()
                .zip(filters.iter().copied())
                .map(|(value, filter)| {
                    resolve_decode_param_for_filters(pdf, &[filter], value)
                })
                .collect::<flpdf::Result<Vec<_>>>()?;
            Ok(Object::Array(values))
        }
        Object::Array(values) => Ok(Object::Array(values)),
        other => resolve_decode_param_for_filters(pdf, filters, other),
    }
}
```

Update `resolve_stream_dictionary` to resolve `/Filter` first, derive names,
then selectively resolve `/DecodeParms`:

```rust
pub(crate) fn resolve_stream_dictionary<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    source: &Dictionary,
) -> flpdf::Result<Dictionary> {
    let filter = source
        .get(b"Filter")
        .cloned()
        .map(|value| resolve_filter_structure(pdf, value, 0))
        .transpose()?;
    let filter_names = filter
        .as_ref()
        .and_then(resolved_filter_names);

    let mut resolved = Dictionary::new();
    for (key, value) in source.iter() {
        let value = if key == b"Filter" {
            filter.clone().unwrap_or_else(|| value.clone())
        } else if key == b"DecodeParms" {
            match filter_names.as_deref() {
                Some(names) => resolve_decode_params(pdf, names, value.clone())?,
                None => value.clone(),
            }
        } else {
            value.clone()
        };
        resolved.insert(key, value);
    }
    Ok(resolved)
}
```

Rename `resolve_nested` to `resolve_filter_structure`, keep its existing
recursion and nesting bound for `/Filter`, and update its recursive calls to
the new name.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-qtest-tools \
  driver::handle::tests::stream_parameter_resolution_ignores_deep_unknown_values \
  -- --exact
cargo test -p flpdf-qtest-tools \
  driver::test_0_1::tests::flate_ignores_deep_unknown_decode_parameter_values \
  -- --exact
cargo test -p flpdf-qtest-tools driver::handle::tests --lib
cargo test -p flpdf-qtest-tools driver::test_0_1::tests --lib
```

Expected: all commands PASS. In particular,
`stream_filter_and_decode_params_reference_chains_are_fully_resolved` must
continue to prove recognized indirect values resolve.

- [ ] **Step 6: Commit the selective parameter fix**

```bash
git add crates/flpdf-qtest-tools/src/driver/handle.rs \
  crates/flpdf-qtest-tools/src/driver/test_0_1.rs
git commit -m "fix(qtest): resolve only consumed decode parameters"
```

---

### Task 4: Extend the pinned qpdf differential corpus

**Files:**
- Modify: `tests/fixtures/test_driver/generate.sh`
- Modify: `scripts/qpdf-test-driver-diff.sh`
- Create: `tests/fixtures/test_driver/stream_chained_warning_offset.pdf`
- Create: `tests/fixtures/test_driver/stream_chained_warning_offset.out`
- Create: `tests/fixtures/test_driver/array_deep_reference.pdf`
- Create: `tests/fixtures/test_driver/array_deep_reference.out`
- Create: `tests/fixtures/test_driver/stream_unknown_decode_param.pdf`
- Create: `tests/fixtures/test_driver/stream_unknown_decode_param.out`

**Interfaces:**
- Consumes: the pinned qpdf commit enforced by
  `scripts/qpdf-test-driver-diff.sh`.
- Produces: three deterministic qpdf/Rust merged-output fixture pairs.

- [ ] **Step 1: Add all three fixture names to both inventories**

Add these names to `fixture_names` in both scripts:

```bash
array_deep_reference
stream_chained_warning_offset
stream_unknown_decode_param
```

Keep the arrays lexically grouped with the existing array and stream fixtures.

- [ ] **Step 2: Add deterministic fixture generation**

In the Python block in `generate.sh`, add:

```python
write(
    "array_deep_reference",
    build_pdf(
        b"[ 100 0 R ]",
        {
            **{
                number: f"{number + 1} 0 R".encode("ascii")
                for number in range(100, 164)
            },
            164: b"true",
        },
    ),
)

write(
    "stream_chained_warning_offset",
    build_pdf(
        b"6 0 R",
        {
            6: b"7 0 R",
            7: stream(
                b"/Filter [ /FlateDecode /FlateDecode ] "
                b"/DecodeParms [ null ]",
                b"abc",
            ),
        },
    ),
)

metadata = b"1"
for _ in range(64):
    metadata = b"[ " + metadata + b" ]"
write(
    "stream_unknown_decode_param",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter /FlateDecode /DecodeParms << /Metadata "
                + metadata
                + b" >>",
                flate_abc,
            ),
        },
    ),
)
```

- [ ] **Step 3: Generate inputs and verify they are accepted by qpdf**

Run:

```bash
bash tests/fixtures/test_driver/generate.sh --generate
```

Expected: exit 0 and all three new `.pdf` files exist. The pre-fix Rust driver
exits nonzero on `array_deep_reference.pdf` at its 64-hop eager-resolution
limit.

- [ ] **Step 4: Regenerate trusted qpdf goldens and compare Rust**

Run:

```bash
bash scripts/qpdf-test-driver-diff.sh --regenerate
bash scripts/qpdf-test-driver-diff.sh --check
```

Expected: both commands exit 0 and report that qpdf and flpdf outputs match all
28 fixtures. Inspect the chained warning golden to confirm its offset points
inside object 7's stream body.

- [ ] **Step 5: Run committed golden tests**

Run:

```bash
cargo test -p flpdf-qtest-tools --test driver_goldens
```

Expected: PASS with the three new fixtures included automatically.

- [ ] **Step 6: Commit the oracle corpus**

```bash
git add scripts/qpdf-test-driver-diff.sh \
  tests/fixtures/test_driver/generate.sh \
  tests/fixtures/test_driver/array_deep_reference.pdf \
  tests/fixtures/test_driver/array_deep_reference.out \
  tests/fixtures/test_driver/stream_chained_warning_offset.pdf \
  tests/fixtures/test_driver/stream_chained_warning_offset.out \
  tests/fixtures/test_driver/stream_unknown_decode_param.pdf \
  tests/fixtures/test_driver/stream_unknown_decode_param.out
git commit -m "test(qtest): cover review regression corpus"
```

---

### Task 5: Verify, record, and publish the remediation

**Files:**
- Verify: all files changed by Tasks 1-4
- Tracker: Bead `flpdf-n9t0.2`

**Interfaces:**
- Produces: a clean pushed branch with fresh local gates and updated Beads
  state.
- Leaves unchanged: GitHub review thread reply and resolution state.

- [ ] **Step 1: Run formatting and focused package checks**

```bash
cargo fmt --all -- --check
cargo test -p flpdf-qtest-tools
```

Expected: both commands exit 0.

- [ ] **Step 2: Run workspace lint and tests**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Expected: both commands exit 0 with no failures.

- [ ] **Step 3: Re-run the pinned differential oracle**

```bash
bash scripts/qpdf-test-driver-diff.sh --check
```

Expected: exit 0 with all 28 fixtures byte-identical and status-identical.

- [ ] **Step 4: Run fresh patch coverage**

```bash
bash scripts/patch-coverage.sh --base origin/main
```

Expected: `flpdf` and the complete changed executable-line report both show
100%, with zero uncovered changed lines.

- [ ] **Step 5: Verify repository state**

```bash
git diff --check
git status --short --branch
git log -6 --oneline --decorate
```

Expected: no working-tree changes and the remediation commits are ahead of or
equal to the remote branch.

- [ ] **Step 6: Record Beads evidence**

```bash
bd update flpdf-n9t0.2 --append-notes \
  "PR #591 review remediation: addressed all 3 actionable threads locally. \
Terminal stream warning provenance, lazy array indirectness, and filter-aware \
DecodeParms resolution are covered by focused tests and pinned qpdf differential fixtures. \
GitHub replies/resolution intentionally not performed."
bd dolt push
```

Expected: issue update succeeds and Dolt reports `Push complete`.

- [ ] **Step 7: Push the branch**

```bash
git push origin feat/flpdf-n9t0-2-test-driver
```

Expected: remote branch advances to the local HEAD.

- [ ] **Step 8: Monitor PR #591 checks**

```bash
gh pr checks 591 --watch --interval 10
```

Expected: every check reaches success, including Windows, Coverage, Quality,
Fuzz, and CodeQL.

- [ ] **Step 9: Read back thread state without mutating GitHub**

```bash
python3 \
  /home/ubuntu/.codex/plugins/cache/openai-curated-remote/github/0.1.8-2841cf9749ae/skills/gh-address-comments/scripts/fetch_comments.py
```

Expected: the three addressed threads may be outdated after the pushed diff but
remain unresolved because reply/resolve was not authorized.
