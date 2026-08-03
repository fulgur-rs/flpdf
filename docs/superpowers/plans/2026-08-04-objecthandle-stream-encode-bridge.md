# ObjectHandle Stream Encode Bridge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `filters::encode_stream_data_from_handle`, preserving the exact bytes and errors of the existing Dictionary encoder while resolving indirect stream-dictionary values through `ObjectHandle`.

**Architecture:** Keep two shape readers and one encode engine. The legacy path reduces `Object` values with `decode_filter_specs_from_object`; the new path reads the holder and children with `try_*`, reduces them with `decode_filter_specs_from_handle`, and both pass `Vec<FilterSpec>` to one private executor.

**Tech Stack:** Rust workspace (`flpdf`), pinned qpdf 11.9.0 at `scripts/fetch-qpdf-source.sh --print-path`, Cargo unit/integration tests, qpdf-zlib byte gates, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

## Global Constraints

- Pinned qpdf 11.9.0 source and observed behavior are the semantic oracle.
- Add only `pub(crate) fn encode_stream_data_from_handle(&ObjectHandle, &[u8]) -> Result<Vec<u8>>`; do not add a public API, trait, materialization bridge, or fallback resolver path.
- Do not migrate any consumer; `flpdf-egzr.3.2.5` owns writer migration.
- Keep `encode_stream_data(&Dictionary, &[u8])` byte- and error-identical.
- Production source changes may touch only `crates/flpdf/src/filters.rs` and, only if necessary for its existing shape reader, `crates/flpdf/src/stream_filter.rs`.
- Follow strict RED -> GREEN -> REFACTOR. All acceptance tests land before production code.
- Fresh changed executable-line coverage must be 100% against `origin/main`.

---

### Task 1: Add acceptance tests, verify RED, implement the bridge, and verify GREEN

**Files:**
- Modify: `crates/flpdf/src/filters.rs` (`#[cfg(test)] mod tests`)
- Test: `crates/flpdf/src/filters.rs`

**Interfaces:**
- Consumes: existing `decode_filter_specs_from_handle`, `ObjectHandle`, `ObjectValue`, `resolver_bearing_handle`, `Pdf::get_object_handle`, `encode_stream_data`, and codec test helpers.
- Produces: four acceptance tests specifying direct equivalence, absolute bytes, live indirect resolution, and dropped-resolver failure; `pub(crate) fn encode_stream_data_from_handle(&ObjectHandle, &[u8]) -> Result<Vec<u8>>`; and private `encode_stream_data_from_specs(Vec<FilterSpec>, &[u8]) -> Result<Vec<u8>>`.

- [ ] **Step 1: Add direct-shape conversion and result comparison helpers**

Inside `filters.rs`'s existing `#[cfg(test)] mod tests`, import the already-tested direct shape converter and add these helpers near the existing encode tests:

```rust
use crate::stream_filter::tests::handle_from_object;

fn native_encode_dictionary(dictionary: &Dictionary) -> ObjectHandle {
    ObjectHandle::dictionary(
        dictionary
            .iter()
            .map(|(key, value)| (key.to_vec(), handle_from_object(Some(value))))
            .collect(),
    )
}

fn comparable_encode(
    result: Result<Vec<u8>>,
) -> std::result::Result<Vec<u8>, String> {
    result.map_err(|error| error.to_string())
}

fn named_filter_dictionary(name: &[u8]) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Filter", Object::Name(name.to_vec()));
    dictionary
}
```

- [ ] **Step 2: Add the full direct filter matrix equivalence test**

Add this test. Its relative comparison catches a shape-reader or wiring drift; the absolute test in Step 3 catches shared-engine drift.

```rust
#[test]
fn handle_encode_matches_dictionary_encode_for_the_full_filter_matrix() {
    let plain = b"ObjectHandle encode matrix: AAABBBCCCDDDEEE".to_vec();
    let mut rows: Vec<(String, Dictionary, Vec<u8>)> = Vec::new();

    rows.push(("missing /Filter".to_string(), Dictionary::new(), plain.clone()));

    for name in [
        b"FlateDecode".as_slice(),
        b"Fl",
        b"ASCII85Decode",
        b"A85",
        b"ASCIIHexDecode",
        b"AHx",
        b"RunLengthDecode",
        b"RL",
        b"LZWDecode",
        b"LZW",
        b"DCTDecode",
        b"DCT",
        b"CCITTFaxDecode",
        b"CCF",
        b"JBIG2Decode",
        b"JPXDecode",
        b"NoSuchDecode",
    ] {
        rows.push((
            String::from_utf8_lossy(name).into_owned(),
            named_filter_dictionary(name),
            plain.clone(),
        ));
    }

    rows.push((
        "ASCII85 then Flate chain".to_string(),
        array_filter_dict(&[b"ASCII85Decode", b"FlateDecode"]),
        plain.clone(),
    ));

    for predictor in 10..=15 {
        rows.push((
            format!("PNG predictor {predictor}"),
            png_predictor_dict(predictor, 4),
            sample_raw_4x2(),
        ));
    }

    let mut malformed_filter = Dictionary::new();
    malformed_filter.insert("Filter", Object::Integer(1));
    rows.push((
        "malformed /Filter".to_string(),
        malformed_filter,
        plain.clone(),
    ));

    let mut malformed_parms = named_filter_dictionary(b"FlateDecode");
    malformed_parms.insert("DecodeParms", Object::Array(vec![Object::Null, Object::Null]));
    rows.push((
        "misaligned /DecodeParms".to_string(),
        malformed_parms,
        plain.clone(),
    ));

    for (label, legacy, input) in rows {
        let native = native_encode_dictionary(&legacy);
        assert_eq!(
            comparable_encode(encode_stream_data(&legacy, &input)),
            comparable_encode(encode_stream_data_from_handle(&native, &input)),
            "encode paths diverged for {label}"
        );
    }
}
```

- [ ] **Step 3: Add independent literal and round-trip assertions**

Add this test. Removing the shared encode loop or changing reverse filter order must fail independently of Dictionary/handle agreement.

```rust
#[test]
fn handle_encode_has_absolute_missing_run_length_and_chain_outputs() {
    let plain = b"AA";
    assert_eq!(
        encode_stream_data_from_handle(&ObjectHandle::dictionary(vec![]), plain).unwrap(),
        plain
    );

    let run_length = ObjectHandle::dictionary(vec![(
        b"Filter".to_vec(),
        ObjectHandle::name(b"RunLengthDecode".to_vec()),
    )]);
    assert_eq!(
        encode_stream_data_from_handle(&run_length, plain).unwrap(),
        [0xff, b'A', 0x80]
    );

    let chain = array_filter_dict(&[b"ASCII85Decode", b"FlateDecode"]);
    let native_chain = native_encode_dictionary(&chain);
    let payload = b"reverse-order chain payload";
    let encoded = encode_stream_data_from_handle(&native_chain, payload).unwrap();
    assert_eq!(decode_stream_data(&chain, &encoded).unwrap(), payload);
}
```

- [ ] **Step 4: Add a real-PDF fixture whose holder, filter, parameters, and parameter value are indirect**

Add the fixture builder and test below. The production change that makes this fail is replacing `try_get_key`/`decode_filter_specs_from_handle` with non-resolving access or Dictionary materialization.

```rust
fn pdf_with_indirect_encode_dictionary() -> Vec<u8> {
    let bodies: [&[u8]; 5] = [
        b"<< /Type /Catalog >>",
        b"/FlateDecode",
        b"<< /Predictor 12 /Columns 5 0 R >>",
        b"<< /Filter 2 0 R /DecodeParms 3 0 R >>",
        b"4",
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_start = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn handle_encode_resolves_the_holder_filter_decode_parms_and_parameter_value() {
    let mut pdf = crate::Pdf::open(std::io::Cursor::new(
        pdf_with_indirect_encode_dictionary(),
    ))
    .expect("open indirect encode fixture");
    let stream_dictionary = pdf.get_object_handle(crate::ObjectRef::new(4, 0));
    assert!(!stream_dictionary.is_resolved());

    let raw = sample_raw_4x2();
    let actual = encode_stream_data_from_handle(&stream_dictionary, &raw).unwrap();
    let expected = encode_stream_data(&png_predictor_dict(12, 4), &raw).unwrap();

    assert!(stream_dictionary.is_resolved());
    assert_eq!(actual, expected);
}
```

- [ ] **Step 5: Add the dropped-resolver error test**

```rust
#[test]
fn handle_encode_surfaces_a_dropped_document_from_the_dictionary_holder() {
    let (stream_dictionary, resolver) = resolver_bearing_handle(
        ObjectValue::Dictionary(
            [(
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            )]
            .into_iter()
            .collect(),
        ),
    );
    drop(resolver);

    let error = encode_stream_data_from_handle(&stream_dictionary, b"payload")
        .expect_err("a dropped document must not read as an empty filter chain");

    assert!(matches!(error, Error::Internal(_)));
    assert_eq!(error.to_string(), "object 20 0 belongs to a dropped PDF");
}
```

- [ ] **Step 6: Run the focused tests and verify RED**

Run:

```bash
cargo test -p flpdf --lib handle_encode -- --nocapture
```

Expected: compilation fails because `encode_stream_data_from_handle` is not defined. Confirm there are no fixture, type, or assertion errors before proceeding.

---

- [ ] **Step 7: Add the crate-private handle entry point with qpdf citations**

Place this immediately after the public Dictionary entry point:

```rust
/// Encode `stream_data` using `/Filter` and `/DecodeParms` read from an
/// `ObjectHandle` stream dictionary.
///
/// qpdf reads both keys through the resolving `stream_dict.getKey` accessor
/// (`libqpdf/QPDF_Stream.cc:386`, `:441`) and reads array children through
/// `getArrayItem` (`:400`, `:448`). `try_get_key` plus
/// `decode_filter_specs_from_handle` preserves that indirect-object behavior.
/// The encode pipeline remains the same one used by [`encode_stream_data`];
/// qpdf builds stream pipelines in reverse order and installs Flate deflate at
/// `libqpdf/QPDF_Stream.cc:529-568`. Predictor encoding remains qpdf's fixed
/// Up-row algorithm (`libqpdf/Pl_PNGFilter.cc:215-228`), and RunLength packet
/// plus EOD emission remains `libqpdf/Pl_RunLength.cc:105-145`.
///
/// # Errors
///
/// Returns the same filter and predictor errors as [`encode_stream_data`],
/// plus [`Error::Internal`] if an indirect holder or child still needs a
/// document resolver after its document has been dropped.
#[allow(dead_code)] // promoted when flpdf-egzr.3.2.5 migrates writer consumers
pub(crate) fn encode_stream_data_from_handle(
    stream_dict: &ObjectHandle,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    let filter = stream_dict.try_get_key(b"Filter")?;
    let decode_params = stream_dict.try_get_key(b"DecodeParms")?;
    let specs = decode_filter_specs_from_handle(&filter, &decode_params, None)?;
    encode_stream_data_from_specs(specs, stream_data)
}
```

- [ ] **Step 8: Extract the shape-neutral encode executor**

Replace the body below the existing Object reader with this exact split:

```rust
fn encode_stream_data_with_filters(
    filter: Option<&Object>,
    decode_params: Option<&Object>,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    // The encode path is writer output rather than untrusted input, so it is
    // uncapped — see `MAX_FILTER_CHAIN_LEN`'s own doc.
    let specs = decode_filter_specs_from_object(filter, decode_params, None)?;
    encode_stream_data_from_specs(specs, stream_data)
}

fn encode_stream_data_from_specs(
    specs: Vec<FilterSpec>,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    // ISO 32000-1 §7.4.2: the /Filter array names filters in *decode*
    // order, so encoding must apply them in reverse for round-tripping.
    let mut encoded = stream_data.to_vec();
    for spec in specs.into_iter().rev() {
        let after_predictor =
            apply_encode_params(spec.normalized_name(), &spec.decode_params, &encoded)?;
        encoded = if spec.normalized_name() == b"FlateDecode" {
            encode_flate(&after_predictor)?
        } else {
            apply_single_filter_encode(spec.normalized_name(), &after_predictor)
                .map_err(Error::Unsupported)?
        };
    }
    Ok(encoded)
}
```

- [ ] **Step 9: Run the focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf --lib handle_encode -- --nocapture
```

Expected: 4 tests pass, 0 fail.

- [ ] **Step 10: Run all filter unit tests**

Run:

```bash
cargo test -p flpdf --lib filters::tests -- --nocapture
```

Expected: all filter tests pass with no new warnings.

- [ ] **Step 11: Format, inspect the source-only diff, and commit**

```bash
cargo fmt --all
cargo fmt --all -- --check
git diff --check
git diff --name-only -- crates/flpdf/src
git add crates/flpdf/src/filters.rs
git commit -m "feat: add ObjectHandle stream encode bridge"
```

Expected source-only diff: `crates/flpdf/src/filters.rs`. Do not stage a consumer file.

---

### Task 2: Verify byte stability, workspace quality, and scope

**Files:**
- Modify: none expected
- Test: workspace and qpdf byte-comparison suites

**Interfaces:**
- Consumes: committed implementation from Task 1.
- Produces: exact test, byte-stability, lint, documentation, and scope evidence for the Bead.

- [ ] **Step 1: Re-read the pinned source locations recorded by the new API**

```bash
qpdf_source="$(scripts/fetch-qpdf-source.sh --print-path)"
nl -ba "$qpdf_source/libqpdf/QPDF_Stream.cc" | sed -n '379,568p'
nl -ba "$qpdf_source/libqpdf/Pl_PNGFilter.cc" | sed -n '45,87p;214,228p'
nl -ba "$qpdf_source/libqpdf/Pl_RunLength.cc" | sed -n '24,64p;105,145p'
```

Expected: citations still identify resolving dictionary reads, reverse pipeline construction/Flate deflate, Up predictor encoding, and RunLength packet/EOD behavior. If the pinned tree is dirty or refuses resolution, stop rather than cite it.

- [ ] **Step 2: Run the before/after byte gate**

The pre-code baseline recorded on commit `adc3b9f7` was `12 passed; 0 failed`:

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
```

Expected after implementation: the same 12 tests pass byte-for-byte against qpdf, with zero changed consumer output.

- [ ] **Step 3: Run formatting, Clippy, strict rustdoc, and library tests**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo test -p flpdf --lib
```

Expected: all clean; the library baseline was 3181 passed, 0 failed, 9 ignored before code changes, and the after count increases only by the four new tests.

- [ ] **Step 4: Run the full workspace test suite**

```bash
cargo test
```

Expected: every workspace test passes.

- [ ] **Step 5: Verify source and consumer boundaries**

```bash
git diff --name-only origin/main...HEAD -- crates/flpdf/src
git diff --name-only origin/main...HEAD | rg -v '^docs/superpowers/(specs|plans)/'
rg -n "encode_stream_data_from_handle" crates/flpdf/src \
  | rg -v '^crates/flpdf/src/filters.rs:'
```

Expected: the first two commands list only `crates/flpdf/src/filters.rs`; the final command has no output, proving zero consumer migration.

---

### Task 3: Regenerate changed-line coverage and publish the verified branch

**Files:**
- Modify: tests only if a genuine uncovered behavior is found
- Test: fresh LCOV plus committed-tree patch coverage

**Interfaces:**
- Consumes: the Task 1 implementation after Task 2's full verification.
- Produces: 100% changed executable-line coverage, pushed Git branch, and persisted Beads evidence.

- [ ] **Step 1: Generate fresh LCOV and run the patch gate**

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/flpdf-egzr-3-2-14.lcov
bash scripts/patch-coverage.sh --base origin/main \
  --lcov target/flpdf-egzr-3-2-14.lcov
```

Expected: `patch-coverage: OK` and 100% coverage on every changed executable line. If a line is uncovered, add a behavior test that would fail under a realistic mutation, watch it fail against that mutation, restore the implementation, and rerun all focused gates; do not add a coverage-only assertion or default to `cov:ignore`.

- [ ] **Step 2: Record verification in the Bead without closing it before integration**

Use `bd update flpdf-egzr.3.2.14 --notes` to append the exact focused/full commands and results, qpdf source locations, 12/12 before/after byte snapshot, source-only diff, and 100% coverage result. Keep status `in_progress` until the branch is integrated.

- [ ] **Step 3: Commit any behavior test required by coverage**

Only if Step 1 required a real test:

```bash
git add crates/flpdf/src/filters.rs crates/flpdf/src/stream_filter.rs
git commit -m "test: cover ObjectHandle stream encode errors"
```

If no coverage repair was required, skip this commit.

- [ ] **Step 4: Run final clean-tree checks and push Git plus Beads**

```bash
git diff --check origin/main...HEAD
git status --short
git push
bd dolt push
```

Expected: clean worktree, branch push succeeds without force, and Dolt reports `Push complete`.
