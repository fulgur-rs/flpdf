# --check Content-Stream Decode Validation Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `flpdf --check` actually decode every page's content stream(s) and flag a decode failure as an error, so the trailing "No syntax or stream encoding errors found" line is no longer an over-claim for the most important class of streams (qpdf-parity exit 2).

**Architecture:** Add a content-stream decode pass to `check.rs::check_reader_inner_with_options`, running after the existing invariant checks. It enumerates pages (`pages::page_refs`), collects each page's `/Contents` streams *with their terminal object refs* (new `pages::page_content_stream_entries`, holder-chain aware via `resolve_ref_chain`), and attempts `filters::decode_stream_data` on each. A decode `Err` becomes an error `Diagnostic` **only** when the stream's `/Filter` chain is entirely generalized (Flate/LZW/ASCII85/ASCIIHex/RunLength); chains containing a passthrough/unknown codec (DCTDecode etc.) are skipped to avoid false positives. Any error diagnostic flips `report.valid = false`, which the CLI already maps to exit 2 and which already suppresses the trailing note.

**Tech Stack:** Rust, flpdf crate (`crates/flpdf/src/check.rs`, `crates/flpdf/src/pages.rs`), flpdf-cli integration test (`crates/flpdf-cli/tests/cli_check_exitcodes.rs`). qpdf 11.9.0 used as ground-truth oracle.

**Ground truth (qpdf 11.9.0, measured this session):**
- corrupt FlateDecode / ASCII85 / LZW **content stream** → `ERROR: page N: content stream (content stream object X 0): errors while decoding content stream` → **exit 2**.
- corrupt Flate **image XObject** / unreferenced **orphan** stream → silent → **exit 0**.
- corrupt Flate **Metadata** stream → WARNING → exit 3 (out of scope: flpdf cannot match qpdf's full reachable-stream set; image codecs differ).

**Scope decisions (locked):** message wording mirrors qpdf (`page N: content stream object X 0: errors while decoding content stream`); decode is **unbounded** (`DecodeLimits::default()`), matching the existing `page_content_bytes` path.

**Reference before coding:** `.claude/rules/pdf-rust-review-patterns.md` (esp. #1 no needless clone, #2 resolve indirect refs, #4 traversal bounds — note the `--check` whole-document exception) and `.claude/rules/pdf-rust-doc-review-patterns.md` (public-doc hygiene: no issue IDs, English-only, `# Errors`).

---

## Task 1: `pages::page_content_stream_entries` — content streams paired with terminal object refs

**Why:** The check pass needs each content stream *and* its terminal object ref (for the qpdf-style message). Existing `collect_content_streams` (pages.rs:152) drops the ref. Add a ref-returning sibling and refactor `collect_content_streams` to delegate (DRY — one holder-chain implementation).

**Files:**
- Modify: `crates/flpdf/src/pages.rs` (add `page_content_stream_entries`; refactor `collect_content_streams` to delegate)
- Test: `crates/flpdf/src/pages.rs` (`#[cfg(test)] mod tests`)

**Step 1: Write the failing test**

Add to the `tests` module in `pages.rs` (reuse existing test-PDF helpers there; if a single-page PDF builder with a content stream is not present, build inline bytes like the helpers in `check.rs::tests`). The test asserts the new function returns one entry whose ref is the content stream's object ref and whose stream decodes:

```rust
#[test]
fn content_stream_entries_yield_terminal_ref() {
    // Single page whose /Contents is `4 0 R` (a FlateDecode stream).
    let bytes = single_page_with_flate_content(); // helper: builds %PDF with obj 4 = content
    let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).unwrap();
    let page = page_refs(&mut pdf).unwrap()[0];
    let entries = page_content_stream_entries(&mut pdf, page).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, Some(ObjectRef::new(4, 0)));
    // The stream is decodable (clean flate).
    assert!(crate::filters::decode_stream_data(&entries[0].1.dict, &entries[0].1.data).is_ok());
}
```

> If no suitable helper exists, write `fn single_page_with_flate_content() -> Vec<u8>` using `flate2` to compress `b"BT /F1 12 Tf (hi) Tj ET"` and emit a 4-object PDF (catalog/pages/page/content) with a correct `xref`, mirroring `check.rs::tests::minimal_pdf_bytes`.

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib pages::tests::content_stream_entries_yield_terminal_ref`
Expected: FAIL — `cannot find function page_content_stream_entries`.

**Step 3: Implement**

Add to `pages.rs` (place near `collect_content_streams`). The function mirrors `collect_content_streams` but keeps the terminal ref from `resolve_ref_chain`:

```rust
/// Resolve a `Page`'s `/Contents` into its content streams, each paired with the
/// terminal [`ObjectRef`] of the indirect chain that produced it (`None` for a
/// direct inline stream). Holder chains (`ref → ref → stream`) are followed via
/// [`resolve_ref_chain`], so a doubly-indirect `/Contents` is not dropped.
///
/// # Errors
///
/// - [`Error::Unsupported`] when `page_ref` is not a `/Type /Page` dictionary, or
///   when a `/Contents` element does not resolve to a stream.
/// - Any [`Error`] propagated from [`Pdf::resolve`].
pub(crate) fn page_content_stream_entries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<(Option<ObjectRef>, Stream)>> {
    let page_obj = pdf.resolve_borrowed(page_ref)?;
    let Some(page_dict) = page_obj.as_dict() else {
        return Err(Error::Unsupported(format!(
            "object {page_ref} is not a dictionary, cannot extract /Contents"
        )));
    };
    // /Type must be /Page (mirror page_content_bytes' check, kept terse here).
    match page_dict.get("Type") {
        Some(Object::Name(name)) if name.as_slice() == b"Page" => {}
        _ => {
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a /Type /Page dictionary"
            )));
        }
    }
    let contents = match page_dict.get("Contents").cloned() {
        None => return Ok(Vec::new()),
        Some(c) => c,
    };
    collect_content_stream_entries(pdf, &contents, page_ref)
}

/// Shared holder-chain collection used by both [`page_content_stream_entries`]
/// (keeps refs) and [`collect_content_streams`] (drops refs).
fn collect_content_stream_entries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    contents: &Object,
    page_ref: ObjectRef,
) -> Result<Vec<(Option<ObjectRef>, Stream)>> {
    match contents {
        Object::Stream(s) => Ok(vec![(None, s.clone())]),
        Object::Reference(r) => {
            let (obj, last) = resolve_ref_chain(pdf, contents)?;
            match obj.into_stream() {
                Some(s) => Ok(vec![(last.or(Some(*r)), s)]),
                None => Err(Error::Unsupported(format!(
                    "/Contents reference {r} on page {page_ref} does not resolve to a stream"
                ))),
            }
        }
        Object::Array(elems) => {
            let mut out = Vec::with_capacity(elems.len());
            for elem in elems {
                match elem {
                    Object::Reference(r) => {
                        let (obj, last) = resolve_ref_chain(pdf, elem)?;
                        match obj.into_stream() {
                            Some(s) => out.push((last.or(Some(*r)), s)),
                            None => {
                                return Err(Error::Unsupported(format!(
                                    "/Contents array element {r} on page {page_ref} does not resolve to a stream"
                                )));
                            }
                        }
                    }
                    Object::Stream(s) => out.push((None, s.clone())),
                    other => {
                        return Err(Error::Unsupported(format!(
                            "/Contents array element of type {} on page {page_ref} is not a stream or reference",
                            object_type_name(other)
                        )));
                    }
                }
            }
            Ok(out)
        }
        other => Err(Error::Unsupported(format!(
            "/Contents entry on page {page_ref} has unexpected type {}",
            object_type_name(other)
        ))),
    }
}
```

Then refactor the existing `collect_content_streams` to delegate (drop the refs):

```rust
fn collect_content_streams<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    contents: &Object,
    page_ref: ObjectRef,
) -> Result<Vec<Stream>> {
    Ok(collect_content_stream_entries(pdf, contents, page_ref)?
        .into_iter()
        .map(|(_, s)| s)
        .collect())
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p flpdf --lib pages::tests`
Expected: PASS (new test + all existing `page_content_bytes` tests unchanged).

**Step 5: Commit**

```bash
git add crates/flpdf/src/pages.rs
git commit -m "feat(flpdf): page_content_stream_entries — content streams with terminal refs"
```

---

## Task 2: Filter classification helper in `check.rs`

**Why:** A decode `Err` is a *stream encoding error* only when flpdf could in principle decode the chain — i.e. every filter is generalized. Chains with passthrough/unknown codecs (DCTDecode, Crypt, …) must be skipped so valid image-bearing content (rare on content streams, but defensive) is never reported as corrupt (advisor concern #1).

**Files:**
- Modify: `crates/flpdf/src/check.rs` (add `content_filter_chain_is_generalized`)
- Test: `crates/flpdf/src/check.rs::tests`

**Step 1: Write the failing test**

```rust
#[test]
fn filter_chain_classification() {
    use crate::{Dictionary, Object};
    let mut flate = Dictionary::new();
    flate.set("Filter", Object::Name(b"FlateDecode".to_vec()));
    assert!(content_filter_chain_is_generalized(&flate));

    let mut none = Dictionary::new(); // no /Filter → trivially decodable
    assert!(content_filter_chain_is_generalized(&none));

    let mut dct = Dictionary::new();
    dct.set("Filter", Object::Name(b"DCTDecode".to_vec()));
    assert!(!content_filter_chain_is_generalized(&dct));

    let mut mixed = Dictionary::new();
    mixed.set(
        "Filter",
        Object::Array(vec![
            Object::Name(b"ASCII85Decode".to_vec()),
            Object::Name(b"DCTDecode".to_vec()),
        ]),
    );
    assert!(!content_filter_chain_is_generalized(&mixed));

    let mut indirect = Dictionary::new(); // indirect /Filter → cannot judge → skip
    indirect.set("Filter", Object::Reference(crate::ObjectRef::new(9, 0)));
    assert!(!content_filter_chain_is_generalized(&indirect));
}
```

> Confirm the actual `Dictionary` constructor/setter names (`Dictionary::new`, `set`) against `crates/flpdf/src/object.rs` before writing; adjust the test to the real API (e.g. `Dictionary::default()` / `insert`).

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib check::tests::filter_chain_classification`
Expected: FAIL — `cannot find function content_filter_chain_is_generalized`.

**Step 3: Implement**

Add to `check.rs`:

```rust
/// The generalized filters flpdf fully decodes. A decode failure on a stream
/// whose `/Filter` chain is entirely generalized is a genuine encoding error;
/// any other codec (image passthrough, `Crypt`, unknown) means flpdf cannot
/// judge corruption, so the failure must be ignored rather than reported.
const GENERALIZED_FILTERS: [&[u8]; 5] = [
    b"FlateDecode",
    b"LZWDecode",
    b"ASCII85Decode",
    b"ASCIIHexDecode",
    b"RunLengthDecode",
];

/// Return `true` when `dict`'s `/Filter` is absent (no-op decode) or names only
/// [`GENERALIZED_FILTERS`]. A `/Filter` stored as an indirect reference, a
/// non-name entry, or any non-generalized codec yields `false` (the stream is
/// not classified as decodable, so a later decode failure is not an error).
fn content_filter_chain_is_generalized(dict: &crate::Dictionary) -> bool {
    fn is_generalized(name: &[u8]) -> bool {
        GENERALIZED_FILTERS.contains(&name)
    }
    match dict.get("Filter") {
        None => true,
        Some(Object::Name(name)) => is_generalized(name),
        Some(Object::Array(elems)) => elems.iter().all(|e| match e {
            Object::Name(name) => is_generalized(name),
            _ => false,
        }),
        Some(_) => false,
    }
}
```

> Note (`//` comment, not doc): `/Filter` indirection on a content stream is essentially never seen; treating it conservatively as "skip" trades a vanishing parity gap for zero false positives. Per rule #2 we still resolve indirection where it matters — here a skip is the safe arm.

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf --lib check::tests::filter_chain_classification`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/check.rs
git commit -m "feat(flpdf): classify content-stream filter chains for --check decode"
```

---

## Task 3: `check_content_streams` pass wired into `check_reader_inner_with_options`

**Why:** This is the feature: decode every page's content streams and emit an error diagnostic per genuine decode failure.

**Files:**
- Modify: `crates/flpdf/src/check.rs` (add `check_content_streams`; call it inside `check_reader_inner_with_options`)
- Test: `crates/flpdf/src/check.rs::tests`

**Step 1: Write the failing tests**

Add tests covering each acceptance row. Build PDFs with the existing inline-bytes style (`minimal_pdf_bytes` pattern). Provide small builders: corrupt-flate content, corrupt-ascii85 content, content array `[good, corrupt]`, corrupt-flate *image XObject* (content clean), DCTDecode content stream.

```rust
#[test]
fn corrupt_flate_content_stream_is_error() {
    let report = check_reader_strict(Cursor::new(corrupt_flate_content_pdf())).unwrap();
    assert!(!report.valid);
    assert!(report.diagnostics.entries().iter().any(|d| {
        d.severity == Severity::Error
            && d.message.contains("errors while decoding content stream")
    }));
}

#[test]
fn corrupt_ascii85_content_stream_is_error() {
    let report = check_reader_strict(Cursor::new(corrupt_ascii85_content_pdf())).unwrap();
    assert!(!report.valid);
}

#[test]
fn content_array_with_one_corrupt_stream_is_error() {
    let report = check_reader_strict(Cursor::new(content_array_one_corrupt_pdf())).unwrap();
    assert!(!report.valid);
}

#[test]
fn clean_content_stream_keeps_valid() {
    let report = check_reader_strict(Cursor::new(clean_flate_content_pdf())).unwrap();
    assert!(report.valid);
    assert!(!report
        .diagnostics
        .entries()
        .iter()
        .any(|d| d.message.contains("content stream")));
}

#[test]
fn corrupt_flate_image_xobject_not_checked() {
    // Content stream clean; a corrupt FlateDecode IMAGE XObject must NOT flip valid.
    let report = check_reader_strict(Cursor::new(corrupt_flate_image_pdf())).unwrap();
    assert!(report.valid);
}

#[test]
fn dct_content_stream_skipped_no_false_error() {
    // A content stream whose /Filter is DCTDecode (abnormal) must be skipped,
    // never reported as a decode error.
    let report = check_reader_strict(Cursor::new(dct_content_stream_pdf())).unwrap();
    assert!(report.valid);
}
```

> If matching qpdf's exact message substring is brittle, assert on `Severity::Error` + `report.valid == false` as the primary signal and keep one message-substring assertion (`"content stream"`).

**Step 2: Run tests to verify they fail**

Run: `cargo test -p flpdf --lib check::tests`
Expected: the new tests FAIL (no content-stream pass yet) — e.g. `corrupt_flate_content_stream_is_error` reports `valid == true`.

**Step 3: Implement**

Add the pass and call it. Insert the call in `check_reader_inner_with_options` after the linearization block, before `let summary = CheckSummary { … }`:

```rust
    // Decode every page's content stream(s); a genuine decode failure is a
    // stream-encoding error. qpdf --check does the same and exits 2 on a broken
    // content stream. Whole-document page traversal here is deliberate: --check
    // is a full-document audit, the one place the lazy-load discipline (review
    // rule #4) is intentionally relaxed.
    check_content_streams(&mut pdf, &mut diagnostics);
```

Then the function:

```rust
/// Decode each page's content stream(s) and push an error [`Diagnostic`] for any
/// genuine decode failure (qpdf-parity exit 2). Streams whose `/Filter` chain is
/// not fully generalized are skipped — flpdf cannot decode them and so cannot
/// distinguish corruption from an unsupported codec. Structural problems that
/// prevent enumeration (e.g. a missing `/Pages`) downgrade to a warning so the
/// already-opened document still yields a report.
fn check_content_streams<R: Read + Seek>(pdf: &mut Pdf<R>, diagnostics: &mut Diagnostics) {
    let page_refs = match crate::pages::page_refs(pdf) {
        Ok(refs) => refs,
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                format!("could not enumerate pages for content-stream check: {error}"),
                None,
            ));
            return;
        }
    };
    for (index, page_ref) in page_refs.iter().enumerate() {
        let page_number = index + 1; // 1-based, matching qpdf's "page N"
        let entries = match crate::pages::page_content_stream_entries(pdf, *page_ref) {
            Ok(entries) => entries,
            Err(error) => {
                // The document opened; a structural /Contents anomaly is a
                // warning, not a hard error (mirrors the linearization-probe
                // downgrade), and must not falsely claim exit 2.
                diagnostics.push(Diagnostic::warning(
                    format!("page {page_number}: could not read content streams: {error}"),
                    None,
                ));
                continue;
            }
        };
        for (stream_ref, stream) in entries {
            if !content_filter_chain_is_generalized(&stream.dict) {
                continue; // unsupported/passthrough codec — cannot judge.
            }
            if crate::filters::decode_stream_data(&stream.dict, &stream.data).is_err() {
                let where_ = match stream_ref {
                    Some(r) => format!("content stream object {} {}", r.number, r.generation),
                    None => "inline content stream".to_string(),
                };
                diagnostics.push(Diagnostic::error(
                    format!(
                        "page {page_number}: {where_}: errors while decoding content stream"
                    ),
                    None,
                ));
            }
        }
    }
}
```

> Verify `ObjectRef` field names (`.number`, `.generation`) against `object.rs`; adjust the `format!` if they differ (e.g. use the `Display` impl: `format!("content stream object {r}")`).

**Step 4: Run tests to verify they pass**

Run: `cargo test -p flpdf --lib check::tests`
Expected: PASS (all rows).

**Step 5: Commit**

```bash
git add crates/flpdf/src/check.rs
git commit -m "feat(flpdf): --check decodes page content streams, errors on failure (flpdf-gvyz)"
```

---

## Task 4: CLI integration test — exit 2 and suppressed trailing line

**Why:** Confirm the library `valid=false` reaches the CLI exit code and the stdout note is gone, end-to-end (this is the user-visible parity behaviour and the `flpdf-tc3e` exit-code integration the issue calls out).

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_check_exitcodes.rs`

**Step 1: Write the failing test**

Mirror the existing tests in that file (they already build PDFs and assert exit codes / stdout). Add:

```rust
#[test]
fn check_corrupt_content_stream_exits_2_without_clean_note() {
    let pdf = corrupt_flate_content_pdf_bytes(); // reuse/port the lib helper
    let file = write_temp_pdf(&pdf);
    flpdf_cmd()
        .arg("--check")
        .arg(file.path())
        .assert()
        .code(2)
        .stdout(predicate::str::contains("No syntax or stream encoding errors found").not());
}
```

> Match the file's existing harness (command builder, temp-file helper, `predicates`). Look at the test near `cli_check_exitcodes.rs:232` for the established patterns.

**Step 2: Run test to verify it fails (before Task 3 is merged) / passes (after)**

Run: `cargo test -p flpdf-cli --test cli_check_exitcodes check_corrupt_content_stream`
Expected: PASS once Task 3 is in. (If written first, it FAILs with code 0.)

**Step 3: (no new impl — exercises Task 3)**

**Step 4: Run the whole check-exitcode suite**

Run: `cargo test -p flpdf-cli --test cli_check_exitcodes`
Expected: PASS (existing exit-0/2/3 cases unaffected).

**Step 5: Commit**

```bash
git add crates/flpdf-cli/tests/cli_check_exitcodes.rs
git commit -m "test(flpdf-cli): --check exits 2 on corrupt content stream, no clean note"
```

---

## Task 5: Quality gates — fmt, clippy, doc, full test, patch coverage

**Files:** none (verification only)

**Step 1: Format + lint + docs**

```bash
cargo fmt --all
cargo fmt --all --check
cargo clippy -p flpdf -p flpdf-cli --all-targets -- -D warnings
cargo test -p flpdf --doc
```
Expected: clean (fmt no diff; clippy 0 warnings; doctests pass). Fix any issue before proceeding.

**Step 2: Full test run**

```bash
cargo test -p flpdf -p flpdf-cli
```
Expected: all pass.

**Step 3: Patch coverage gate (commit first!)**

```bash
git status   # must be clean — coverage gate errors on a dirty tree
scripts/patch-coverage.sh --base flpdf-gvyz-check-content-streams^  # base = the branch point on main
```
> Use the merge-base with main as `--base` if the branch has several commits; the gate diffs HEAD against it. flpdf changed lines must be **100%** covered. Add tests for any uncovered line, or annotate a truly-untestable line with `// cov:ignore: <reason>` and note it in the PR description.

**Step 4: Qualitative coverage check**

Confirm tests exist for the *error arms and boundaries*, not just line execution: corrupt vs clean, image-not-checked, passthrough-skipped, page-enumeration-failure warning. (Acceptance rows in the issue.)

**Step 5: Final commit (if fmt/clippy touched anything)**

```bash
git add -A
git commit -m "chore(flpdf): fmt/clippy/doc fixups for --check content-stream decode"
```

---

## Notes for the implementer

- **No public-doc issue IDs / Japanese.** `check_content_streams`, the helper, and any `///` must be English and free of `flpdf-gvyz`. The internal `//` rationale comments may reference the rules informally but prefer spec/qpdf grounding.
- **No needless clones (rule #1).** `decode_stream_data` borrows the dict and data; do not clone the `Stream` beyond what the collection already owns. `resolve_ref_chain` returns owned objects — move, don't re-clone.
- **Resolve indirection (rule #2).** Content `/Contents` refs go through `resolve_ref_chain`. The one deliberate non-resolve is an indirect `/Filter`, which is conservatively skipped (documented inline).
- **Traversal bound (rule #4).** `page_refs` already bounds page-tree depth; the whole-document scan is the intentional `--check` exception — keep the inline comment that says so.
- **Encrypted input:** `resolve` returns decrypted, filter-encoded bytes, so the decode pass works unchanged; no extra handling needed.
