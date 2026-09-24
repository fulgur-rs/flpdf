# QDF Positional Length Holder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `flpdf::fix_qdf` choose and rewrite ordinary stream length values exactly as qpdf 11.9.0 does: from the next top-level object in file order.

**Architecture:** Keep the byte-oriented `ObjectSpan` scanner and sequential numbering validation. Remove `/Length` dictionary interpretation from holder selection; associate each ordinary stream with its immediate positional successor, count all exact newline-marker lines between them, validate the successor's first body line as qpdf's bare integer line, and rewrite that line. Leave ObjStm and XRef stream state paths independent.

**Tech Stack:** Rust workspace, `crates/flpdf/src/qdf_fix.rs`, `crates/flpdf/tests/qdf_fix_tests.rs`, pinned qpdf 11.9.0 `qpdf/fix-qdf.cc`.

---

## Files and responsibilities

- `crates/flpdf/tests/qdf_fix_tests.rs`: qpdf-derived RED regressions, fixture/golden comparisons, and corrected old M-keyed expectations.
- `tests/fixtures/qdf-fix/`: committed qpdf 11.9.0 input/golden pairs for wrong declared target and repeated markers.
- `crates/flpdf/src/qdf_fix.rs`: positional selection, exact marker counts and integer-line validation; byte-preserving rewrite.
- `docs/qpdf-correspondence.md`: document the positional qdf-fix contract and qpdf source range.
- `crates/flpdf-cli/tests/cli_qdf_roundtrip_matrix.rs` and `tests/fixtures/qdf-roundtrip/`: refresh the old grouped-holder test data from the current QDF writer, which already emits qpdf's adjacent-holder layout.

## Task 1: Add qpdf-derived RED tests

**Files:** `crates/flpdf/tests/qdf_fix_tests.rs`, `tests/fixtures/qdf-fix/`.

- [ ] Add this minimal byte fixture builder. The stream payload is four bytes (`abc\n`); the output length should be 4, or 2 when there are two marker lines.

```rust
fn positional_length_qdf(
    length_entry: Option<&[u8]>,
    marker_count: usize,
    next_body: &[u8],
) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n1 0 obj\n<<\n".to_vec();
    if let Some(entry) = length_entry {
        pdf.extend_from_slice(entry);
        pdf.push(b'\n');
    }
    pdf.extend_from_slice(b">>\nstream\nabc\nendstream\nendobj\n");
    for _ in 0..marker_count {
        pdf.extend_from_slice(b"%QDF: ignore_newline\n");
    }
    pdf.extend_from_slice(b"2 0 obj\n");
    pdf.extend_from_slice(next_body);
    if !next_body.ends_with(b"\n") {
        pdf.push(b'\n');
    }
    pdf.extend_from_slice(b"endobj\n\n3 0 obj\n<<\n/Type /Catalog\n>>\nendobj\n\n");
    pdf.extend_from_slice(
        b"xref\n0 4\n0000000000 65535 f \n0000000000 00000 n \n\
          0000000000 00000 n \n0000000000 00000 n \n",
    );
    pdf.extend_from_slice(
        b"trailer <<\n  /Root 3 0 R\n  /Size 4\n>>\nstartxref\n0\n%%EOF\n",
    );
    pdf
}
```

- [ ] Add this test for all `/Length` forms. It must preserve any supplied dictionary entry while rewriting positional object 2 to 4.

```rust
#[test]
fn positional_holder_is_ignored_by_length_reference_shape() {
    let entries: [Option<&[u8]>; 4] = [
        Some(&b"  /Length 99 0 R"[..]),
        Some(&b"  /Length 44"[..]),
        None,
        Some(&b"  /Length 2 1 R"[..]),
    ];
    for entry in entries {
        let input = positional_length_qdf(entry, 0, b"0\n");
        let fixed = flpdf::fix_qdf(&input).expect("repair positional holder");
        assert!(
            fixed.windows(b"\n2 0 obj\n4\nendobj".len())
                .any(|window| window == b"\n2 0 obj\n4\nendobj"),
            "the next object, not /Length M, holds the measured length"
        );
        if let Some(entry) = entry {
            assert!(
                fixed.windows(entry.len()).any(|window| window == entry),
                "fix-qdf must preserve the /Length dictionary bytes"
            );
        }
    }
}
```

- [ ] Add `each_ignore_newline_marker_subtracts_one`: call `positional_length_qdf(Some(b"  /Length 99 0 R"), 2, b"0\n")`; assert object 2 becomes `2` and the result is idempotent.
- [ ] Add `holder_integer_line_must_match_qpdf_exactly`: call `positional_length_qdf(Some(b"  /Length 2 0 R"), 0, b" 0\n")`; require `Error::Parse`. The pinned qpdf probe exits 2 with `expected integer` for this input shape.
- [ ] Commit input/golden pairs from the pinned qpdf qtest `fix1.qdf`: change its first `/Length 5 0 R` to `/Length 99 0 R`, and separately insert two exact marker lines before object 5. Generate each golden with `/usr/bin/fix-qdf` 11.9.0, register both fixtures in the qdf tests, and compare flpdf output byte-for-byte.
- [ ] Add a no-successor input/golden pair by removing the final integer holder from qpdf `fix1.qdf`. The live oracle rewrites the earlier holder but leaves the xref/trailer tail byte-for-byte unchanged because it remains in `st_after_stream`.
- [ ] Add a malformed-holder-header regression by changing `5 0 obj\n` to `5  0 obj\n` in the wrong-target fixture. qpdf ignores that header and exits 2 with `expected object 5` at the next recognized object.
- [ ] Run `cargo test -p flpdf --test qdf_fix_tests positional_holder_is_ignored_by_length_reference_shape -- --exact`; it must fail because flpdf currently follows object M or skips a holder when `/Length` is absent/direct.
- [ ] Run the marker and integer-line tests individually and confirm each fails for its intended reason.

## Task 2: Replace M-keyed holder resolution

**Files:** `crates/flpdf/src/qdf_fix.rs`.

- [ ] Remove `length_holder` from `ObjectBody::Plain` and stop calling `classify_length` when parsing an ordinary stream; preserve dictionary bytes unchanged.
- [ ] Make `parse_obj_header` recognize only qpdf's LF-terminated single-space `N 0 obj` form, and call it only for lines ending in LF.
- [ ] Replace `has_ignore_newline_marker` with a count of exact LF-terminated marker lines in the separator between the stream span and the next top-level span.
- [ ] Build rewrite lengths from adjacent `objects.windows(2)` entries. For each ordinary `Plain` stream, subtract the separator marker count with saturation and associate the result with the immediately following top-level object, regardless of its parsed body variant. qpdf enters `st_in_length` before it would classify that successor body.
- [ ] Require the successor header to be `N 0 obj\n`, then require the line immediately after that header to be nonempty ASCII digits followed by LF. Return `Error::Parse` otherwise, matching qpdf's `st_in_length` rule; do not skip an ObjStm/XRef object to find a later integer.
- [ ] Replace only the validated integer line with the measured value. Remove declared-M missing-object, generation, conflict, and reuse checks because qpdf does not consult M/G at all.
- [ ] If the last parsed object is an ordinary stream with no recognized successor, finish earlier holder rewrites and copy the remaining input tail verbatim; do not regenerate xref/trailer because qpdf never leaves `st_after_stream`.
- [ ] Run every new qpdf-derived RED regression and `cargo test -p flpdf --test qdf_fix_tests`; verify all positional, no-successor, header-recognition and integer-line cases pass and inspect any legacy M-keyed assertion that remains.

The positional assignment stores the length and validated integer-line range at the successor object's index:

```rust
let mut new_len_body: Vec<Option<(usize, usize, usize)>> = vec![None; objects.len()];
for (i, object) in objects.iter().enumerate() {
    let ObjectBody::Plain {
        stream_len: Some(measured_len),
        ignore_newline_count,
    } = &object.body
    else {
        continue;
    };
    let Some(successor) = objects.get(i + 1) else {
        continue;
    };
    let Some((integer_start, integer_end)) = qpdf_bare_integer_line_range(input, successor) else {
        return Err(Error::parse(successor.body_start, "fix_qdf: expected integer"));
    };
    new_len_body[i + 1] = Some((
        measured_len.saturating_sub(*ignore_newline_count),
        integer_start,
        integer_end,
    ));
}
```

The object-header recognizer is applied only to LF-terminated lines and matches qpdf's generation-zero single-space expression:

```rust
fn parse_obj_header(line: &[u8]) -> Option<(u32, u32, usize)> {
    let number = line.strip_suffix(b" 0 obj")?;
    if number.is_empty() || !number.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let number = std::str::from_utf8(number).ok()?.parse::<u32>().ok()?;
    Some((number, 0, line.len()))
}
```

After the object-emission loop, preserve the unprocessed tail if qpdf would still be in `st_after_stream`:

```rust
if matches!(
    objects.last().map(|object| &object.body),
    Some(ObjectBody::Plain {
        stream_len: Some(_),
        ..
    })
) {
    let last_end = objects.last().expect("nonempty object list").end;
    out.extend_from_slice(&input[last_end..]);
    return Ok(out);
}
```

## Task 3: Reconcile old tests and documentation

**Files:** `crates/flpdf/tests/qdf_fix_tests.rs`, `crates/flpdf/src/qdf_fix.rs`, `docs/qpdf-correspondence.md`.

- [ ] Replace `direct_length_with_length1_left_verbatim` and the indirect-holder-generation tests with positional assertions; `/Length1`, comments and strings remain verbatim but do not select a holder.
- [ ] Change the missing-M and ObjStm-successor tests to assert `expected integer`; consolidate the old conflicting/same-length M-reuse tests into `following_stream_is_not_a_bare_integer_holder`; remove the redundant declared-generation rejection test because the new nonzero-reference test covers qpdf's ignored `/Length` generation.
- [ ] Keep the no-successor partial-tail fixture and malformed-header sequencing test in both focused and live-oracle coverage.
- [ ] Keep the writer QDF closed-loop, clean-QDF no-op, ObjStm/XRef tests, oracle goldens, sequential-number checks and xref-regeneration tests passing.
- [ ] Regenerate `three-page-clean.qdf` with `flpdf rewrite --qdf --static-id`, then regenerate its stale-length, edited-payload and shifted-offset variants from that output. Confirm live qpdf `fix-qdf` is a byte no-op on clean output, returns nonzero for each corrupted input, and accepts each repaired output.
- [ ] Update module docs and local comments to state that qpdf never reads `/Length` for holder selection and that each exact marker line subtracts once.
- [ ] Extend the `qdf_fix.rs` correspondence note with `fix-qdf.cc:240-264` and the live-probed positional cases; do not change the QDF writer route.
- [ ] Run `cargo test -p flpdf --test qdf_fix_tests` and compare committed fixtures byte-for-byte against `/usr/bin/fix-qdf` 11.9.0.

## Task 4: Full verification and PR handoff

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run strict Rustdoc: `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items`.
- [ ] Run `cargo clippy --workspace --all-targets --all-features -- -D warnings` and `cargo test --workspace`.
- [ ] Run qpdf module-doc, deviation-marker, and route-matrix checks; verify no new route mismatch was introduced.
- [ ] Re-fetch and rebase onto current `origin/main`, run committed `scripts/patch-coverage.sh --base origin/main`, and require zero uncovered changed lines.
- [ ] Commit and push the implementation branch, create a Draft PR, wait for every required check on the exact head, then mark it Ready.
- [ ] Read back PR status and all checks, update `.cvby` with commit/PR/verification evidence, run `bd dep cycles`, and confirm `bd dolt push` prints `Push complete.`.
