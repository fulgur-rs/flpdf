//! Tests for [`flpdf::fix_qdf`].
//!
//! The committed fixtures under `tests/fixtures/qdf-fix/` make these tests
//! deterministic without requiring `qpdf`/`fix-qdf` at run time:
//!
//! * `*-clean.qdf`        — a pristine `qpdf --qdf` output (the QDF form).
//! * `corrupt-*.qdf`      — a hand-corrupted copy (stale length / shifted
//!   offsets / wrong `/Size` / wrong `startxref`).
//! * `corrupt-*.golden.qdf` — the byte-exact output of the system
//!   `fix-qdf < corrupt-*.qdf` oracle (qpdf 11.9.0).
//!
//! `flpdf::fix_qdf` must reproduce the oracle golden byte-for-byte.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("qdf-fix")
}

fn read(name: &str) -> Vec<u8> {
    fs::read(fixtures_dir().join(name)).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

/// Cases involving an object stream (`/Type /ObjStm`) and/or a
/// cross-reference stream (`/Type /XRef`) — qpdf `fix-qdf` accepts both
/// (`qpdf/fix-qdf.cc`'s `st_in_ostream_*` / `st_in_xref_stream_dict`
/// states), so these are part of the same byte-identical contract as the
/// classic-xref-table cases above.
const OBJSTM_CASES: &[&str] = &[
    "corrupt-objstm",
    "corrupt-objstm-multi",
    "corrupt-objstm-big",
];

/// Edge cases in the object-stream/cross-reference-stream scanner, each
/// confirmed against the live `fix-qdf` 11.9.0 oracle
/// (`fix-qdf <input> > <golden>`) before being committed:
///
/// * `corrupt-xref-truncated` — a cross-reference-stream object truncated
///   right after its `stream` keyword, with no `endstream` anywhere in the
///   file. The oracle classifies `/Type /XRef` from the dictionary alone
///   (`qpdf/fix-qdf.cc`'s `st_in_obj`, `line.find("/Type /XRef")`) and
///   completes on the `stream\n` line itself (`st_in_xref_stream_dict`
///   writing its binary payload and tail the moment that line is seen) —
///   `endstream` is never looked for.
/// * `corrupt-trailing-garbage` — a well-formed classic-tail QDF with a
///   syntactically valid `N G obj ... endobj` block appended *after* the
///   original `%%EOF`. The oracle's `st_top`/`st_at_xref`/.../`st_done`
///   state machine can never re-enter object recognition once the tail
///   `xref` line is seen, so this trailing block is discarded entirely
///   (not copied through, not treated as an object).
/// * `corrupt-nested-type-xref` — a regular stream whose dictionary has a
///   nested sub-dictionary containing `/Type /XRef`. The oracle's
///   `line.find("/Type /XRef") != line.npos` check is a plain per-line
///   substring search with no `<<`/`>>` nesting awareness, so it
///   misclassifies this object exactly like flpdf does — this fixture
///   pins that (buggy but byte-identical) shared behavior.
/// * `corrupt-comment-type-xref` — a regular stream whose dictionary has a
///   `%` comment line whose TEXT contains the literal substring
///   `/Type /XRef`. Since the oracle's check is a raw per-line substring
///   search with no comment awareness at all, it misclassifies this
///   comment line as the real `/Type /XRef` marker (confirmed against the
///   live `fix-qdf` binary) — this fixture pins that shared behavior.
/// * `corrupt-string-type-xref` — the same misclassification, but the
///   decoy `/Type /XRef` text sits inside a `(...)` literal string value
///   instead of a comment; the oracle's substring search has no string-
///   literal awareness either, so the result is identical (confirmed
///   against the live oracle).
/// * `corrupt-decoy-xref-line` — a `xref stream decoy, not the real tail`
///   line sitting between two top-level objects (not the file's real
///   tail `xref` keyword). The oracle's `st_top` transition requires the
///   ENTIRE line to equal the literal `"xref\n"` (`line.compare("xref\n")
///   == 0`), so a merely `xref`-prefixed line is just echoed as ordinary
///   text and object recognition continues past it (confirmed against the
///   live oracle: the second object is still found and included in the
///   regenerated table).
/// * `corrupt-decoy-xref-after-last-object` — the same unrecognized line
///   after the last object; qpdf preserves it and locates only the exact
///   `xref\n` line that enters `st_at_xref`.
const SCANNER_EDGE_CASES: &[&str] = &[
    "corrupt-xref-truncated",
    "corrupt-trailing-garbage",
    "corrupt-nested-type-xref",
    "corrupt-comment-type-xref",
    "corrupt-string-type-xref",
    "corrupt-decoy-xref-line",
    "corrupt-decoy-xref-after-last-object",
];

/// Ordinary-stream holder cases for qpdf's positional state transition.
const POSITIONAL_LENGTH_CASES: &[&str] = &[
    "corrupt-length-position",
    "corrupt-length-position-markers",
    "corrupt-length-position-no-successor",
    "corrupt-length-marker-before-endobj",
];

/// A QDF with an empty indirect-object table and a direct trailer root.
const EMPTY_QDF_CASES: &[&str] = &["empty-qdf-direct-root", "empty-qdf-decoy-xref-line"];

/// Each corrupted fixture, fixed by `flpdf::fix_qdf`, must equal the committed
/// oracle golden byte-for-byte.
#[test]
fn matches_oracle_golden_byte_for_byte() {
    for case in [
        "corrupt-length",
        "corrupt-shift",
        "corrupt-size",
        "corrupt-startxref",
        "corrupt-combo",
    ]
    .into_iter()
    .chain(OBJSTM_CASES.iter().copied())
    .chain(SCANNER_EDGE_CASES.iter().copied())
    .chain(POSITIONAL_LENGTH_CASES.iter().copied())
    .chain(EMPTY_QDF_CASES.iter().copied())
    {
        let input = read(&format!("{case}.qdf"));
        let golden = read(&format!("{case}.golden.qdf"));
        let got = flpdf::fix_qdf(&input).unwrap_or_else(|e| panic!("{case}: fix_qdf: {e}"));
        assert_eq!(
            got,
            golden,
            "{case}: flpdf::fix_qdf output does not match the system fix-qdf golden\n\
             got {} bytes, golden {} bytes\nfirst diff at {:?}",
            got.len(),
            golden.len(),
            got.iter().zip(golden.iter()).position(|(a, b)| a != b)
        );
    }
}

/// Running `fix_qdf` on an already-valid QDF file is a no-op (true for both a
/// file with streams and one without).
#[test]
fn no_op_on_clean_qdf() {
    for clean in [
        "one-page-clean.qdf",
        "minimal-clean.qdf",
        "objstm-clean.qdf",
        "empty-qdf-direct-root.qdf",
    ] {
        let data = read(clean);
        let got = flpdf::fix_qdf(&data).unwrap();
        assert_eq!(got, data, "{clean}: fix_qdf should be a no-op on clean QDF");
    }
}

#[test]
fn empty_qdf_direct_root_matches_qpdf_golden_and_is_a_no_op() {
    let input = read("empty-qdf-direct-root.qdf");
    let golden = read("empty-qdf-direct-root.golden.qdf");
    let fixed = flpdf::fix_qdf(&input).expect("qpdf accepts a QDF with no indirect objects");

    assert_eq!(fixed, golden, "empty QDF output must match qpdf fix-qdf");
    assert_eq!(fixed, input, "qpdf fix-qdf is a no-op on this empty QDF");
}

#[test]
fn empty_qdf_uses_the_recognized_xref_after_a_decoy_line() {
    let input = read("empty-qdf-decoy-xref-line.qdf");
    let golden = read("empty-qdf-decoy-xref-line.golden.qdf");
    let fixed = flpdf::fix_qdf(&input).expect("qpdf accepts the xref decoy as top-level text");

    assert_eq!(
        fixed, golden,
        "the empty-table prefix must end at qpdf's exact xref line"
    );
}

/// `fix_qdf(fix_qdf(x)) == fix_qdf(x)` for every corrupted input.
#[test]
fn idempotent() {
    for case in [
        "corrupt-length",
        "corrupt-shift",
        "corrupt-size",
        "corrupt-startxref",
        "corrupt-combo",
    ]
    .into_iter()
    .chain(OBJSTM_CASES.iter().copied())
    .chain(SCANNER_EDGE_CASES.iter().copied())
    .chain(POSITIONAL_LENGTH_CASES.iter().copied())
    .chain(EMPTY_QDF_CASES.iter().copied())
    {
        let input = read(&format!("{case}.qdf"));
        let once = flpdf::fix_qdf(&input).unwrap();
        let twice = flpdf::fix_qdf(&once).unwrap();
        assert_eq!(once, twice, "{case}: fix_qdf is not idempotent");
    }
}

/// The repaired output must be a valid PDF accepted by `qpdf --check`.
/// Gated on `qpdf` availability so the suite still runs without it.
#[test]
fn repaired_output_passes_qpdf_check() {
    if Command::new("qpdf").arg("--version").output().is_err() {
        eprintln!("qpdf not available; skipping qpdf --check verification");
        return;
    }
    // Per-invocation unique temp dir: a fixed shared path races under
    // parallel `cargo test` / concurrent CI jobs.
    let dir = tempfile::tempdir().expect("temp dir");
    let tmp = dir.path().join("fix-check.pdf");
    // `corrupt-objstm-big` is excluded: it is a synthetic 300-member ObjStm
    // fixture (forcing the xref stream's 2-byte object-index field width)
    // whose `/Root` deliberately does not resolve to a real `/Catalog` —
    // it exists only to exercise `fix_qdf`'s own byte output, not to be a
    // structurally valid PDF `qpdf --check` would accept.
    for case in [
        "corrupt-length",
        "corrupt-shift",
        "corrupt-size",
        "corrupt-startxref",
        "corrupt-combo",
        "corrupt-objstm",
        "corrupt-objstm-multi",
    ] {
        let input = read(&format!("{case}.qdf"));
        let fixed = flpdf::fix_qdf(&input).unwrap();
        fs::write(&tmp, &fixed).unwrap();
        let out = Command::new("qpdf")
            .arg("--check")
            .arg(&tmp)
            .output()
            .expect("run qpdf --check");
        assert!(
            out.status.success(),
            "{case}: qpdf --check failed on repaired output:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    // `dir` (TempDir) is removed on drop — no manual cleanup needed.
}

/// If the live `fix-qdf` oracle is present, confirm our committed goldens still
/// match it (guards against fixture drift). Skipped when the tool is absent.
#[test]
fn committed_goldens_still_match_live_oracle() {
    if Command::new("fix-qdf").arg("--version").output().is_err() {
        eprintln!("fix-qdf not available; skipping live oracle re-check");
        return;
    }
    for case in [
        "corrupt-length",
        "corrupt-shift",
        "corrupt-size",
        "corrupt-startxref",
        "corrupt-combo",
        "corrupt-decoy-xref-after-last-object",
    ]
    .into_iter()
    .chain(OBJSTM_CASES.iter().copied())
    .chain(POSITIONAL_LENGTH_CASES.iter().copied())
    .chain(EMPTY_QDF_CASES.iter().copied())
    {
        use std::io::Write;
        let input = read(&format!("{case}.qdf"));
        let golden = read(&format!("{case}.golden.qdf"));
        let mut child = Command::new("fix-qdf")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn fix-qdf");
        child.stdin.take().unwrap().write_all(&input).unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            out.stdout, golden,
            "{case}: committed golden no longer matches live fix-qdf"
        );
    }
}

/// Member object numbers inside an ObjStm continue the SAME sequential
/// counter as top-level objects (qpdf `fix-qdf.cc`'s `checkObjId` increments
/// one `last_obj` across both `st_top`'s `N 0 obj` matches and
/// `st_in_ostream_offsets`/`st_in_ostream_obj`'s `%% Object stream: object N`
/// matches) — a gap inside a stream's members is rejected exactly like a gap
/// between top-level objects.
#[test]
fn objstm_member_gap_is_rejected() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"1 0 obj\n<<\n  /Type /ObjStm\n  /Length 0\n  /N 0\n  /First 0\n>>\n");
    // Container is object 1, so the first member must be object 2 — skip to
    // 3 instead.
    pdf.extend_from_slice(
        b"stream\n0 0\n%% Object stream: object 3, index 0\n<<\n  /Type /Catalog\n>>\n\
          endstream\nendobj\n\n",
    );
    // A real `qpdf --qdf` always pairs object streams with a
    // cross-reference-stream tail (a classic table has no compressed-object
    // entry type), so the terminating object must be one too.
    pdf.extend_from_slice(
        b"4 0 obj\n<<\n  /Type /XRef\n  /Length 0\n  /W [ 0 0 0 ]\n  /Root 2 0 R\n  /Size 0\n>>\n\
          stream\nXXXXXXXXXX\nendstream\nendobj\n\nstartxref\n0\n%%EOF\n",
    );
    let err = flpdf::fix_qdf(&pdf).unwrap_err();
    assert!(
        format!("{err}").contains("non-sequential object numbering"),
        "a numbering gap spanning into an ObjStm's members must be rejected \
         the same way as a top-level gap, got: {err}"
    );
}

/// An `/Type /ObjStm` object whose stream body has zero
/// `%% Object stream: object N` marker lines is malformed — real qpdf
/// `--qdf --object-streams=generate` never emits an empty object stream —
/// and must be a `Parse` error rather than silently producing an empty
/// member table.
#[test]
fn objstm_with_no_members_is_rejected() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(
        b"1 0 obj\n<<\n  /Type /ObjStm\n  /Length 0\n  /N 0\n  /First 0\n>>\n\
          stream\nno markers here\nendstream\nendobj\n\n",
    );
    pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 1 0 R\n  /Size 2\n>>\nstartxref\n0\n%%EOF\n");
    let err = flpdf::fix_qdf(&pdf).unwrap_err();
    assert!(
        matches!(err, flpdf::Error::Parse { .. }),
        "an ObjStm with no members must be a Parse error, got: {err:?}"
    );
}

/// Object recognition still stops correctly when the file ends immediately
/// after a bare `xref` with no trailing newline at all. `is_xref_keyword_line`
/// requires an EXACT `"xref\n"` match (mirroring qpdf's
/// `line.compare("xref\n"sv) == 0`, `qpdf/fix-qdf.cc:130`) and so does NOT
/// itself recognize this truncated, newline-less line — but `find_next_obj`'s
/// scan still halts there via its own end-of-input boundary (the current
/// line's end reaches `input.len()`), so no bogus extra object swallows the
/// dangling `xref` text either way. The overall result is still a `Parse`
/// error: the SEPARATE tail-locating scan (`find_line_keyword_from`, used
/// only after classic-tail mode is established) tolerates end-of-input right
/// after the keyword and does find this `xref`, but no `trailer` follows it.
#[test]
fn xref_keyword_at_true_eof_with_no_trailing_byte_stops_object_scan() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"1 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n");
    pdf.extend_from_slice(b"xref");
    let err = flpdf::fix_qdf(&pdf).unwrap_err();
    assert!(
        format!("{err}").contains("no `trailer` keyword"),
        "the dangling `xref` at true EOF must be recognized as the tail \
         keyword (stopping object recognition there), not consumed as an \
         object; got: {err}"
    );
}

/// A top-level (non-stream) object with no `endobj` anywhere in the
/// remainder of the file — e.g. a QDF truncated mid-object — is rejected.
/// Unlike the `/Type /XRef` truncation case (`corrupt-xref-truncated`
/// fixture), a non-`/Type /XRef` object has no oracle-defined way to
/// complete without its `endobj`, so this must stay a hard `Parse` error.
#[test]
fn object_without_endobj_is_rejected() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"1 0 obj\n<<\n  /Type /Catalog\n>>\n");
    let err = flpdf::fix_qdf(&pdf).unwrap_err();
    assert!(
        matches!(err, flpdf::Error::Parse { .. }),
        "a truncated object with no `endobj` must be a Parse error, got: {err:?}"
    );
    assert!(
        format!("{err}").contains("object without matching `endobj`"),
        "unexpected error: {err}"
    );
}

/// A `/Type` value split across a comment (`/Type %comment\n/ObjStm`,
/// each half on its own line) is NOT recognized as an object stream.
/// qpdf's `/Type /ObjStm` check is a raw, per-LINE substring search
/// (`line.find("/Type /ObjStm"sv)`, `qpdf/fix-qdf.cc:142`), evaluated
/// once per line while streaming the dict; the literal 13-byte text never
/// appears within a single line here, so it never matches on either line
/// — confirmed against the live `fix-qdf` binary: this exact fixture is
/// rejected with `"...: expected object 2"` (the un-classified object's
/// would-be sole member, object 2, is never counted into the sequential
/// `1..N` counter, so the next real top-level object — numbered 3 — is
/// non-sequential). The raw per-line scan is authoritative for this fixture;
/// a comment-aware `/Type` scan would produce a different result.
#[test]
fn objstm_type_split_across_comment_line_is_not_recognized() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(
        b"1 0 obj\n<<\n  /Type %an inline comment\n  /ObjStm\n  /Length 0\n  /N 0\n  /First 0\n>>\n\
          stream\n0 0\n%% Object stream: object 2, index 0\n<<\n  /Type /Catalog\n>>\n\
          endstream\nendobj\n\n",
    );
    pdf.extend_from_slice(
        b"3 0 obj\n<<\n  /Type /XRef\n  /Length 0\n  /W [ 0 0 0 ]\n  /Root 2 0 R\n  /Size 0\n>>\n\
          stream\nXXXXXXXXXX\nendstream\nendobj\n\nstartxref\n0\n%%EOF\n",
    );
    let err = flpdf::fix_qdf(&pdf).unwrap_err();
    assert!(
        matches!(err, flpdf::Error::Parse { .. }),
        "comment-split /Type /ObjStm must not classify, leaving object 2's member \
         uncounted and object 3 non-sequential; got: {err:?}"
    );
    assert!(
        format!("{err}").contains("non-sequential"),
        "unexpected error: {err}"
    );
}

/// A line starting with the literal `%% Object stream: object ` prefix but
/// with no digit immediately after is not a member marker (mirrors qpdf's
/// `re_ostream_obj`, which requires `(\d+)` right there) — it stays part of
/// the current member's body, copied verbatim, rather than starting a new
/// member.
#[test]
fn objstm_marker_prefix_without_digit_is_not_a_marker() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(
        b"1 0 obj\n<<\n  /Type /ObjStm\n  /Length 0\n  /N 0\n  /First 0\n>>\n\
          stream\n0 0\n%% Object stream: object 2, index 0\n<<\n\
          %% Object stream: object X\n  /Type /Catalog\n>>\n\
          endstream\nendobj\n\n",
    );
    pdf.extend_from_slice(
        b"3 0 obj\n<<\n  /Type /XRef\n  /Length 0\n  /W [ 0 0 0 ]\n  /Root 2 0 R\n  /Size 0\n>>\n\
          stream\nXXXXXXXXXX\nendstream\nendobj\n\nstartxref\n0\n%%EOF\n",
    );
    let fixed = flpdf::fix_qdf(&pdf).expect("a decoy marker-prefix line must not break scanning");
    assert!(
        find(&fixed, b"/N 1").is_some(),
        "the decoy line must not be counted as a second member;\ngot:\n{}",
        String::from_utf8_lossy(&fixed)
    );
    assert!(
        find(&fixed, b"%% Object stream: object X").is_some(),
        "the decoy line's text must survive verbatim as member 2's body content;\ngot:\n{}",
        String::from_utf8_lossy(&fixed)
    );
    assert_eq!(flpdf::fix_qdf(&fixed).unwrap(), fixed, "idempotent");
}

/// An `/Extends` entry with no digit following is not a match (mirrors
/// qpdf's `re_extends = "/Extends (\d+ 0 R)"`) — the object stream is
/// processed with no `/Extends` in its regenerated dict.
#[test]
fn objstm_extends_without_digit_is_ignored() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(
        b"1 0 obj\n<<\n  /Type /ObjStm\n  /Extends none\n  /Length 0\n  /N 0\n  /First 0\n>>\n\
          stream\n0 0\n%% Object stream: object 2, index 0\n<<\n  /Type /Catalog\n>>\n\
          endstream\nendobj\n\n",
    );
    pdf.extend_from_slice(
        b"3 0 obj\n<<\n  /Type /XRef\n  /Length 0\n  /W [ 0 0 0 ]\n  /Root 2 0 R\n  /Size 0\n>>\n\
          stream\nXXXXXXXXXX\nendstream\nendobj\n\nstartxref\n0\n%%EOF\n",
    );
    let fixed = flpdf::fix_qdf(&pdf).expect("a non-matching /Extends must not be an error");
    assert!(
        find(&fixed, b"/Extends").is_none(),
        "no /Extends N 0 R match means none is emitted;\ngot:\n{}",
        String::from_utf8_lossy(&fixed)
    );
    assert_eq!(flpdf::fix_qdf(&fixed).unwrap(), fixed, "idempotent");
}

/// An `/Extends` entry whose digits are NOT followed by exactly ` 0 R`
/// (e.g. a non-zero generation) does not match either — same qpdf regex,
/// same "no /Extends emitted" outcome.
#[test]
fn objstm_extends_wrong_generation_is_ignored() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(
        b"1 0 obj\n<<\n  /Type /ObjStm\n  /Extends 9 1 R\n  /Length 0\n  /N 0\n  /First 0\n>>\n\
          stream\n0 0\n%% Object stream: object 2, index 0\n<<\n  /Type /Catalog\n>>\n\
          endstream\nendobj\n\n",
    );
    pdf.extend_from_slice(
        b"3 0 obj\n<<\n  /Type /XRef\n  /Length 0\n  /W [ 0 0 0 ]\n  /Root 2 0 R\n  /Size 0\n>>\n\
          stream\nXXXXXXXXXX\nendstream\nendobj\n\nstartxref\n0\n%%EOF\n",
    );
    let fixed = flpdf::fix_qdf(&pdf).expect("a non-zero-generation /Extends must not be an error");
    assert!(
        find(&fixed, b"/Extends").is_none(),
        "a non-zero-generation /Extends N G R does not match qpdf's regex;\ngot:\n{}",
        String::from_utf8_lossy(&fixed)
    );
    assert_eq!(flpdf::fix_qdf(&fixed).unwrap(), fixed, "idempotent");
}

/// Real `qpdf --qdf` always pairs an object stream with a
/// cross-reference-stream tail (a classic `xref` table has no entry type
/// for a compressed object), so this combination cannot arise from genuine
/// QDF input — an object stream in a file whose tail is a classic table is
/// `Unsupported`.
#[test]
fn objstm_in_classic_xref_form_is_unsupported() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(
        b"1 0 obj\n<<\n  /Type /ObjStm\n  /Length 0\n  /N 0\n  /First 0\n>>\n\
          stream\n0 0\n%% Object stream: object 2, index 0\n<<\n  /Type /Catalog\n>>\n\
          endstream\nendobj\n\n",
    );
    pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 2 0 R\n  /Size 2\n>>\nstartxref\n0\n%%EOF\n");
    let err = flpdf::fix_qdf(&pdf).unwrap_err();
    assert!(
        matches!(err, flpdf::Error::Unsupported(_)),
        "an ObjStm with a classic xref tail must be Unsupported, got: {err:?}"
    );
    assert!(
        format!("{err}").contains("classic"),
        "error should explain the classic-xref-tail restriction, got: {err}"
    );
}

/// A stream body may contain a line-anchored `xref` without being the
/// cross-reference table, and a dictionary string may contain `stream` without
/// being the stream keyword:
///   1. A decompressed stream body that contains a line-anchored `xref` must
///      NOT be mistaken for the cross-reference table (use the LAST one).
///   2. A `stream` byte sequence inside a dictionary string value must NOT be
///      mistaken for the `stream` keyword (match it line-anchored).
#[test]
fn ignores_xref_and_stream_inside_object_body() {
    // obj 1: stream whose dict has a string containing the word "stream" and
    // whose decompressed content contains a line `xref`. /Length points to
    // obj 3, but qpdf's positional integer holder is obj 2. Initial xref offsets are intentionally bogus zeros —
    // fix_qdf must regenerate them and still pick the real table at the tail.
    // Object numbering is contiguous 1..3 (qpdf's fix-qdf rejects gaps).
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    pdf.extend_from_slice(b"<<\n  /Length 3 0 R\n  /Note (the word stream appears here)\n>>\n");
    pdf.extend_from_slice(b"stream\nline one\nxref\nendstream\nendobj\n\n");
    pdf.extend_from_slice(b"2 0 obj\n0\nendobj\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 2 0\n3 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    // Real (tail) xref table with deliberately wrong offsets.
    pdf.extend_from_slice(b"xref\n0 4\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 3 0 R\n  /Size 4\n>>\nstartxref\n0\n%%EOF\n");

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must succeed");
    let s = &fixed;

    // The `xref` line inside obj 1's stream body is preserved verbatim.
    assert!(
        find(s, b"stream\nline one\nxref\nendstream").is_some(),
        "stream body (incl. its inner `xref` line) must be preserved verbatim"
    );

    // Exactly ONE regenerated cross-reference table: a line-anchored `xref`
    // immediately followed by the `0 4` subsection header.
    assert!(
        find(s, b"\nxref\n0 4\n").is_some(),
        "real xref table must be regenerated at the tail"
    );

    // Positional holder (obj 2) recomputed to the verbatim content byte count:
    // "line one\nxref\n" == 14 bytes (after `stream`+EOL, up to line `endstream`).
    assert!(
        find(s, b"2 0 obj\n14\nendobj").is_some(),
        "positional integer holder must be recomputed to 14, got:\n{}",
        String::from_utf8_lossy(s)
    );

    // Idempotent.
    let again = flpdf::fix_qdf(&fixed).expect("fix_qdf idempotent");
    assert_eq!(again, fixed, "fix_qdf must be idempotent");
}

/// Tiny substring search helper (tests only).
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A stream body may contain line-anchored `endobj` and `xref` text:
///   A decompressed QDF stream body may contain a line-anchored `endobj` (and
///   `xref`). The naive "first line-anchored endobj after N G obj" would
///   truncate the object span there, corrupting subsequent xref/length repair.
///   fix_qdf must anchor the endobj search AFTER `endstream`.
#[test]
fn stream_body_endobj_and_xref_not_mistaken_for_object_terminator() {
    // obj 1: stream whose decompressed body contains BOTH a line `endobj` and
    // a line `xref`; the object terminator must still be found after
    // `endstream`.
    // /Length points at obj 3, but qpdf's positional integer holder is obj 2.
    // Xref offsets are bogus zeros.
    // Object numbering is contiguous 1..3 (qpdf's fix-qdf rejects gaps).
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    pdf.extend_from_slice(b"<<\n  /Length 3 0 R\n>>\n");
    // Stream body contains both `endobj` and `xref` on their own lines.
    pdf.extend_from_slice(
        b"stream\nsome content\nendobj\nmore content\nxref\nfinal line\nendstream\nendobj\n\n",
    );
    pdf.extend_from_slice(b"2 0 obj\n0\nendobj\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 2 0\n3 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    // Real (tail) xref table with deliberately wrong offsets.
    pdf.extend_from_slice(b"xref\n0 4\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 3 0 R\n  /Size 4\n>>\nstartxref\n0\n%%EOF\n");

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must succeed on stream-body-endobj input");

    // The entire stream body (including the inner `endobj` and `xref` lines)
    // must be preserved verbatim between `stream\n` and `endstream`.
    assert!(
        find(
            &fixed,
            b"stream\nsome content\nendobj\nmore content\nxref\nfinal line\nendstream"
        )
        .is_some(),
        "stream body (incl. inner `endobj` and `xref`) must be preserved verbatim;\ngot:\n{}",
        String::from_utf8_lossy(&fixed)
    );

    // Exactly one regenerated xref table at the tail (the one we emitted).
    assert!(
        find(&fixed, b"\nxref\n0 4\n").is_some(),
        "real xref table must be regenerated at the tail"
    );

    // Positional holder (obj 2) recomputed to the verbatim content byte count:
    // "some content\nendobj\nmore content\nxref\nfinal line\n" = 50 bytes.
    let expected_len = b"some content\nendobj\nmore content\nxref\nfinal line\n".len();
    let expected_holder = format!("2 0 obj\n{expected_len}\nendobj");
    assert!(
        find(&fixed, expected_holder.as_bytes()).is_some(),
        "positional integer holder must be recomputed to {expected_len};\ngot:\n{}",
        String::from_utf8_lossy(&fixed)
    );

    // Idempotent.
    let again = flpdf::fix_qdf(&fixed).expect("fix_qdf must be idempotent");
    assert_eq!(
        again, fixed,
        "fix_qdf must be idempotent on stream-body-endobj output"
    );
}

/// `/Length1` and `/Length` dictionary values do not select qpdf's holder.
#[test]
fn length1_and_wrong_length_target_do_not_control_positional_holder() {
    // Object 2 is the positional holder even though /Length points to object 3.
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    pdf.extend_from_slice(b"<<\n  /Length1 999\n  /Length 3 0 R\n>>\n");
    pdf.extend_from_slice(b"stream\nhello world\nendstream\nendobj\n\n");
    pdf.extend_from_slice(b"2 0 obj\n0\nendobj\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 2 0\n3 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    pdf.extend_from_slice(b"xref\n0 4\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 3 0 R\n  /Size 4\n>>\nstartxref\n0\n%%EOF\n");

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf uses the positional object");

    let expected_len = b"hello world\n".len();
    let expected_holder = format!("2 0 obj\n{expected_len}\nendobj");
    assert!(
        find(&fixed, expected_holder.as_bytes()).is_some(),
        "positional holder must be recomputed to {expected_len};\ngot:\n{}",
        String::from_utf8_lossy(&fixed)
    );
    assert!(
        find(&fixed, b"/Length1 999\n  /Length 3 0 R").is_some(),
        "length-like dictionary bytes must remain verbatim"
    );
    assert_eq!(flpdf::fix_qdf(&fixed).unwrap(), fixed, "idempotent");
}

/// A direct `/Length` value is preserved while the positional successor is updated.
#[test]
fn direct_length_with_length1_preserves_dict_and_rewrites_successor() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    pdf.extend_from_slice(b"<<\n  /Length1 999\n  /Length 11\n>>\n");
    pdf.extend_from_slice(b"stream\nhello world\nendstream\nendobj\n\n");
    pdf.extend_from_slice(b"2 0 obj\n0\nendobj\n\n");
    pdf.extend_from_slice(b"3 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n");
    pdf.extend_from_slice(b"xref\n0 4\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 3 0 R\n  /Size 4\n>>\nstartxref\n0\n%%EOF\n");

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must repair the positional integer");

    assert!(find(&fixed, b"/Length1 999\n  /Length 11\n").is_some());
    assert_positional_length(&fixed, b"hello world\n".len());
}

/// Closed loop for the flpdf QDF writer and flpdf::fix_qdf
/// must mesh. Produce a real QDF via the writer (it now emits indirect
/// `/Length H 0 R` + a bare-integer holder), hand-edit a stream's decoded
/// payload (the canonical "human edits the QDF" use case), run flpdf::fix_qdf,
/// and verify it repairs the indirect length-holder body — then `qpdf --check`
/// accepts the result. This is the lighter version; the full round-trip
/// A broader round-trip matrix is covered by the CLI integration tests.
#[test]
fn writer_qdf_then_edit_then_fix_qdf_closed_loop() {
    use flpdf::Pdf;
    use std::io::Cursor;

    let source = read("../compat/three-page.pdf");
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
    let opts = WriterTestSettings {
        qdf: true,
        static_id: true,
        ..WriterTestSettings::default()
    };
    let mut qdf = Vec::new();
    write_with_settings(&mut pdf, &mut qdf, &opts).unwrap();

    // Sanity: writer produced an indirect-length stream + holder.
    let lp = find(&qdf, b"/Length ").expect("indirect /Length entry");
    let tail = std::str::from_utf8(&qdf[lp + b"/Length ".len()..lp + b"/Length ".len() + 16])
        .expect("ascii");
    let mut it = tail.split_whitespace();
    let holder: u32 = it.next().unwrap().parse().unwrap();
    assert_eq!(it.next(), Some("0"));
    assert_eq!(it.next(), Some("R"), "writer must emit indirect /Length");

    // Hand-edit: inject extra bytes into the first stream's decoded payload,
    // simulating a human editing the QDF content. The indirect holder body
    // is now STALE — exactly the failure flpdf::fix_qdf exists to repair.
    let s_kw = find(&qdf, b"\nstream\n").expect("stream kw");
    let payload_start = s_kw + b"\nstream\n".len();
    let inject = b"% injected by a human editor\n";
    let mut edited = qdf.clone();
    edited.splice(payload_start..payload_start, inject.iter().copied());

    // The clean writer holder body — read it from the unedited QDF so the
    // test does not hardcode the fixture's raw payload length. fix_qdf must
    // reproduce it exactly, accounting for `%QDF: ignore_newline` whenever
    // stream framing adds an LF.
    assert_eq!(
        flpdf::fix_qdf(&qdf).expect("fix_qdf on clean writer QDF"),
        qdf,
        "fix_qdf must be a no-op on unedited flpdf QDF (writer/fix_qdf mesh)"
    );
    let clean_holder_hdr = format!("\n{holder} 0 obj\n");
    let chp = find(&qdf, clean_holder_hdr.as_bytes()).expect("clean holder");
    let crest = &qdf[chp + clean_holder_hdr.len()..];
    let cend = find(crest, b"\nendobj").expect("clean holder endobj");
    let clean_len: usize = std::str::from_utf8(&crest[..cend])
        .unwrap()
        .trim()
        .parse()
        .expect("clean holder body integer");

    // fix_qdf must repair the indirect holder, xref, /Size, startxref.
    let fixed = flpdf::fix_qdf(&edited).expect("fix_qdf on edited writer QDF");

    // The holder body for `holder` must now reflect the LENGTHENED payload.
    let stale_holder = format!("\n{holder} 0 obj\n{clean_len}\nendobj");
    assert!(
        find(&fixed, stale_holder.as_bytes()).is_none(),
        "stale holder value {clean_len} must have been recomputed"
    );
    let new_len = clean_len + inject.len();
    let fixed_holder = format!("\n{holder} 0 obj\n{new_len}\nendobj");
    assert!(
        find(&fixed, fixed_holder.as_bytes()).is_some(),
        "indirect length-holder {holder} must be repaired to {new_len}"
    );

    // qpdf must accept the closed-loop result.
    if Command::new("qpdf").arg("--version").output().is_ok() {
        // Per-invocation unique temp dir.
        let dir = tempfile::tempdir().expect("temp dir");
        let tmp = dir.path().join("closed-loop.pdf");
        fs::write(&tmp, &fixed).unwrap();
        let out = Command::new("qpdf")
            .arg("--check")
            .arg(&tmp)
            .output()
            .expect("run qpdf --check");
        assert!(
            out.status.success(),
            "qpdf --check failed on closed-loop output:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        // `dir` (TempDir) is removed on drop.
    } else {
        eprintln!("qpdf not available; skipping qpdf --check in closed-loop test");
    }

    // fix_qdf must be idempotent on its own output.
    let again = flpdf::fix_qdf(&fixed).expect("fix_qdf idempotent");
    assert_eq!(
        again, fixed,
        "fix_qdf must be idempotent on repaired writer QDF"
    );
}

/// qpdf 11.9.0 writes the raw payload length to the indirect holder and uses
/// this marker to tell fix-qdf that its byte scan includes one framing LF.
#[test]
fn ignore_newline_marker_repairs_raw_length_and_is_idempotent() {
    let qdf = fs::read("../../tests/golden/references/qdf-ignore-newline/qdf-static-id.pdf")
        .expect("read qpdf non-EOL QDF golden");

    assert_eq!(
        flpdf::fix_qdf(&qdf).expect("fix_qdf on clean qpdf QDF"),
        qdf,
        "fix_qdf must exclude the framing LF named by %QDF: ignore_newline"
    );

    // Simulate a human changing the raw payload from `A` to `ABC`. The
    // framing LF remains on disk but is still excluded from the repaired
    // holder, so qpdf's holder object 3 must become 3 rather than 4.
    let payload_start = find(&qdf, b"\nstream\n").expect("stream keyword") + b"\nstream\n".len();
    let mut edited = qdf.clone();
    edited.splice(payload_start + 1..payload_start + 1, b"BC".iter().copied());
    let fixed = flpdf::fix_qdf(&edited).expect("fix_qdf on edited qpdf QDF");
    assert!(
        find(&fixed, b"\n3 0 obj\n3\nendobj").is_some(),
        "holder must contain the edited raw payload length, excluding framing LF"
    );
    assert!(
        find(&fixed, b"%QDF: ignore_newline\n3 0 obj").is_some(),
        "marker must remain immediately before the holder"
    );
    assert_eq!(
        flpdf::fix_qdf(&fixed).expect("fix_qdf idempotence"),
        fixed,
        "marker-aware fix_qdf output must be idempotent"
    );
}

/// Length-like text in strings, comments, and keys stays byte-preserved;
/// none of it selects qpdf's positional holder.
#[test]
fn length_like_string_and_comment_do_not_control_positional_holder() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    // Decoy `/Length` inside a literal string and a comment, before a wrong
    // declared target. qpdf never examines any of these dictionary tokens.
    pdf.extend_from_slice(b"<<\n  /Note (a /Length 999 decoy)\n");
    pdf.extend_from_slice(b"  %% /Length 888 in a comment\n");
    pdf.extend_from_slice(b"  /Length 3 0 R\n>>\n");
    pdf.extend_from_slice(b"stream\nABCDEFGHIJ\nendstream\nendobj\n\n");
    pdf.extend_from_slice(b"2 0 obj\n0\nendobj\n\n");
    pdf.extend_from_slice(b"3 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n");
    pdf.extend_from_slice(b"xref\n0 4\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 3 0 R\n  /Size 4\n>>\nstartxref\n0\n%%EOF\n");

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must use positional object 2");

    assert_positional_length(&fixed, b"ABCDEFGHIJ\n".len());
    // The decoy string and comment are preserved verbatim.
    assert!(find(&fixed, b"/Note (a /Length 999 decoy)").is_some());
    assert!(find(&fixed, b"%% /Length 888 in a comment").is_some());

    // Idempotent.
    assert_eq!(
        flpdf::fix_qdf(&fixed).expect("idempotent"),
        fixed,
        "fix_qdf must be idempotent"
    );
}

/// A `/ObjStm` inside a string/comment must
/// NOT trigger the Unsupported(ObjStm) rejection. A valid QDF with no real
/// object stream but text mentioning /ObjStm must repair normally.
#[test]
fn objstm_substring_in_string_not_rejected() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    pdf.extend_from_slice(
        b"<<\n  /Type /Catalog\n  /Note (this mentions /ObjStm but is not one)\n",
    );
    pdf.extend_from_slice(b"  %% /ObjStm in a comment too\n>>\nendobj\n\n");
    pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 1 0 R\n  /Size 2\n>>\nstartxref\n0\n%%EOF\n");

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must not reject string-only /ObjStm");
    assert!(find(&fixed, b"/Note (this mentions /ObjStm but is not one)").is_some());
    assert!(find(&fixed, b"\nxref\n0 2\n").is_some());
}

/// A trailer key like `/SizeExtra` before
/// the real `/Size` must not absorb the recomputed size.
#[test]
fn sizeextra_not_mistaken_for_size() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 1 0\n1 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000000 00000 n \n");
    // /SizeExtra (decoy) and a /Note string containing /Size, before real /Size.
    pdf.extend_from_slice(
        b"trailer <<\n  /SizeExtra 7\n  /Note (/Size 999)\n  /Root 1 0 R\n  /Size 4242\n>>\nstartxref\n0\n%%EOF\n",
    );

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must succeed");
    // Real /Size recomputed to 2 (max obj number 1 + 1); decoys untouched.
    assert!(
        find(&fixed, b"/Size 2\n").is_some(),
        "real /Size must be rewritten to 2:\n{}",
        String::from_utf8_lossy(&fixed)
    );
    assert!(
        find(&fixed, b"/SizeExtra 7\n").is_some(),
        "/SizeExtra must be untouched"
    );
    assert!(
        find(&fixed, b"/Note (/Size 999)").is_some(),
        "string decoy must be verbatim"
    );
    assert!(
        find(&fixed, b"/Size 4242").is_none(),
        "stale real /Size must be gone"
    );
}

/// `/ObjStm` as a non-/Type name value (or
/// after a custom key) must NOT trigger the Unsupported(ObjStm) rejection —
/// only a real `/Type /ObjStm` object stream is unsupported.
#[test]
fn objstm_as_plain_name_value_not_rejected() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    pdf.extend_from_slice(b"<<\n  /SomeKey /ObjStm\n  /Type /Catalog\n>>\nendobj\n\n");
    pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 1 0 R\n  /Size 2\n>>\nstartxref\n0\n%%EOF\n");

    let fixed =
        flpdf::fix_qdf(&pdf).expect("fix_qdf must accept /ObjStm as a non-/Type name value");
    assert!(find(&fixed, b"/SomeKey /ObjStm").is_some());
    assert!(find(&fixed, b"\nxref\n0 2\n").is_some());
}

/// `>>` inside a trailer string/comment must
/// not be taken as the dict close, so the real /Size is still rewritten.
#[test]
fn trailer_close_ignores_brackets_in_string() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 1 0\n1 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000000 00000 n \n");
    // Decoy `>>` inside a literal string AND a comment, before the real
    // closing `>>`; the real /Size sits after the decoys.
    pdf.extend_from_slice(
        b"trailer <<\n  /Note (closing >> here)\n  %% another >> in a comment\n  /Root 1 0 R\n  /Size 4242\n>>\nstartxref\n0\n%%EOF\n",
    );

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must succeed");
    assert!(
        find(&fixed, b"/Size 2\n").is_some(),
        "real /Size must be rewritten despite >> in string/comment:\n{}",
        String::from_utf8_lossy(&fixed)
    );
    assert!(
        find(&fixed, b"/Size 4242").is_none(),
        "stale /Size must be gone"
    );
    assert!(
        find(&fixed, b"/Note (closing >> here)").is_some(),
        "string decoy verbatim"
    );
    assert_eq!(flpdf::fix_qdf(&fixed).unwrap(), fixed, "idempotent");
}

/// A comment-split-`/ObjStm`
/// fixture. This fixture's FIRST top-level object is numbered `2` (not
/// `1`) — a construction quirk independent of the comment split itself.
/// Confirmed against the live `fix-qdf` binary that this exact fixture is
/// rejected with `"...: expected object 1"`: the oracle fatals purely on
/// non-sequential numbering, before any `/Type /ObjStm` classification
/// question is ever reached. flpdf must reject it too, for the same
/// reason — this supersedes the prior assertion that the error mentions
/// "ObjStm" (that reasoning predates `find_raw_type_line`'s oracle-raw
/// per-line scan; per `objstm_type_split_across_comment_line_is_not_recognized`,
/// a comment-split `/Type`/`/ObjStm` is never classified as an object
/// stream in the first place, so no "ObjStm"-specific error path is ever
/// reached here either).
#[test]
fn objstm_with_comment_between_type_and_objstm_is_rejected() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 2 0\n2 0 obj\n");
    pdf.extend_from_slice(
        b"<<\n  /Type %an inline comment\n  /ObjStm\n  /N 1\n>>\nstream\nx\nendstream\nendobj\n\n",
    );
    pdf.extend_from_slice(
        b"xref\n0 3\n0000000000 65535 f \n0000000000 00000 n \n0000000000 00000 n \n",
    );
    pdf.extend_from_slice(b"trailer <<\n  /Root 1 0 R\n  /Size 3\n>>\nstartxref\n0\n%%EOF\n");
    let err = flpdf::fix_qdf(&pdf).unwrap_err();
    assert!(
        matches!(err, flpdf::Error::Parse { .. }),
        "non-sequential top-level numbering (starts at object 2) must be rejected, \
         got: {err:?}"
    );
    assert!(
        format!("{err}").contains("non-sequential"),
        "unexpected error: {err}"
    );
}

/// A NON-stream object whose literal string
/// value contains line-anchored `stream`/`endstream`/`endobj` byte sequences
/// must not be mis-detected as a stream / mis-spanned. The string lives inside
/// `<<...>>`, before the dict close, so the dict-close-anchored stream scan
/// must ignore it; the object's real `endobj` is the terminator.
#[test]
fn stream_keywords_inside_dict_string_not_mistaken_for_stream() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    // /Note's string contains lines `stream`, `endstream`, `endobj`.
    pdf.extend_from_slice(b"<<\n  /Type /Catalog\n  /Note (line\nstream\nfake body\nendstream\nendobj\n)\n>>\nendobj\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 2 0\n2 0 obj\n<<\n  /Type /Pages\n  /Count 0\n  /Kids [\n  ]\n>>\nendobj\n\n");
    pdf.extend_from_slice(
        b"xref\n0 3\n0000000000 65535 f \n0000000000 00000 n \n0000000000 00000 n \n",
    );
    pdf.extend_from_slice(b"trailer <<\n  /Root 1 0 R\n  /Size 3\n>>\nstartxref\n0\n%%EOF\n");
    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must succeed (string keywords ignored)");
    // Both objects preserved; the decoy string kept verbatim; one regenerated xref.
    assert!(
        find(
            &fixed,
            b"/Note (line\nstream\nfake body\nendstream\nendobj\n)"
        )
        .is_some(),
        "decoy string must be preserved verbatim:\n{}",
        String::from_utf8_lossy(&fixed)
    );
    assert!(
        find(&fixed, b"\n2 0 obj\n").is_some(),
        "object 2 must not be lost to mis-span"
    );
    assert!(
        find(&fixed, b"\nxref\n0 3\n").is_some(),
        "exactly one regenerated xref"
    );
    assert_eq!(flpdf::fix_qdf(&fixed).unwrap(), fixed, "idempotent");
}

// ── qpdf's positional stream-length holder state ────────────────────────────

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
    if !next_body.is_empty() && !next_body.ends_with(b"\n") {
        pdf.push(b'\n');
    }
    pdf.extend_from_slice(b"endobj\n\n3 0 obj\n<<\n/Type /Catalog\n>>\nendobj\n\n");
    pdf.extend_from_slice(
        b"xref\n0 4\n0000000000 65535 f \n0000000000 00000 n \n\
          0000000000 00000 n \n0000000000 00000 n \n",
    );
    pdf.extend_from_slice(b"trailer <<\n  /Root 3 0 R\n  /Size 4\n>>\nstartxref\n0\n%%EOF\n");
    pdf
}

fn assert_positional_length(fixed: &[u8], length: usize) {
    let expected = format!("\n2 0 obj\n{length}\nendobj");
    assert!(
        find(fixed, expected.as_bytes()).is_some(),
        "positional object 2 must contain length {length}:\n{}",
        String::from_utf8_lossy(fixed)
    );
}

#[test]
fn wrong_length_target_still_rewrites_positionally_next_object() {
    let input = positional_length_qdf(Some(b"  /Length 99 0 R"), 0, b"0\n");
    let fixed = flpdf::fix_qdf(&input).expect("qpdf ignores the declared holder number");

    assert_positional_length(&fixed, 4);
    assert!(find(&fixed, b"/Length 99 0 R").is_some());
}

#[test]
fn direct_or_absent_length_still_rewrites_positional_object() {
    for entry in [Some(&b"  /Length 44"[..]), None] {
        let input = positional_length_qdf(entry, 0, b"0\n");
        let fixed = flpdf::fix_qdf(&input).expect("qpdf does not inspect /Length");
        assert_positional_length(&fixed, 4);
        if let Some(entry) = entry {
            assert!(fixed.windows(entry.len()).any(|window| window == entry));
        } else {
            assert!(find(&fixed, b"/Length ").is_none());
        }
    }
}

#[test]
fn nonzero_length_generation_does_not_change_positional_holder() {
    let input = positional_length_qdf(Some(b"  /Length 2 1 R"), 0, b"0\n");
    let fixed = flpdf::fix_qdf(&input).expect("qpdf ignores the declared generation");

    assert_positional_length(&fixed, 4);
    assert!(find(&fixed, b"/Length 2 1 R").is_some());
}

#[test]
fn each_ignore_newline_marker_line_subtracts_once() {
    let input = positional_length_qdf(Some(b"  /Length 99 0 R"), 2, b"0\n");
    let fixed = flpdf::fix_qdf(&input).expect("qpdf consumes both marker lines");

    assert_positional_length(&fixed, 2);
    assert_eq!(flpdf::fix_qdf(&fixed).unwrap(), fixed, "idempotent");
}

#[test]
fn ignore_newline_between_endstream_and_endobj_matches_qpdf_golden() {
    let input = read("corrupt-length-marker-before-endobj.qdf");
    let golden = read("corrupt-length-marker-before-endobj.golden.qdf");
    let fixed = flpdf::fix_qdf(&input).expect("qpdf counts the marker before endobj");

    assert_eq!(
        fixed, golden,
        "marker before endobj must match qpdf fix-qdf"
    );
    assert_eq!(flpdf::fix_qdf(&fixed).unwrap(), fixed, "idempotent");
}

#[test]
fn no_positional_successor_leaves_qpdf_tail_processing_in_st_after_stream() {
    let input = read("corrupt-length-position-no-successor.qdf");
    let golden = read("corrupt-length-position-no-successor.golden.qdf");
    let fixed = flpdf::fix_qdf(&input).expect("qpdf returns success without a successor");

    assert_eq!(
        fixed, golden,
        "partial st_after_stream output must match qpdf"
    );
    assert_eq!(flpdf::fix_qdf(&fixed).unwrap(), fixed, "idempotent");
}

#[test]
fn unrecognized_positional_header_does_not_advance_object_numbering() {
    let header = b"5 0 obj\n";
    for replacement in [b"5  0 obj\n".as_slice(), b" 0 obj\n", b"x 0 obj\n"] {
        let mut input = read("corrupt-length-position.qdf");
        let position = find(&input, header).expect("holder 5 header");
        input.splice(
            position..position + header.len(),
            replacement.iter().copied(),
        );
        let err = flpdf::fix_qdf(&input).unwrap_err();

        assert!(
            format!("{err}").contains("non-sequential object numbering"),
            "qpdf ignores header {replacement:?}, so the next recognized object must fail its ID check: {err}"
        );
    }
}

#[test]
fn positional_holder_expected_integer_precedes_successor_body_validation() {
    let pdf = read("error-order-holder-before-objstm.qdf");
    let error =
        flpdf::fix_qdf(&pdf).expect_err("qpdf checks the holder line before the ObjStm body");

    assert!(matches!(
        error,
        flpdf::Error::Parse { message, .. } if message == "fix_qdf: expected integer"
    ));
}

#[test]
fn positional_holder_requires_an_exact_bare_integer_line() {
    let invalid_bodies: [&[u8]; 4] = [b" 0\n", b"0 \n", b"0\r\n", b""];
    for body in invalid_bodies {
        let input = positional_length_qdf(Some(b"  /Length 2 0 R"), 0, body);
        let err = match flpdf::fix_qdf(&input) {
            Err(err) => err,
            Ok(output) => panic!(
                "qpdf rejects holder body {body:?}, but fix_qdf returned:\n{}",
                String::from_utf8_lossy(&output)
            ),
        };
        assert!(
            matches!(err, flpdf::Error::Parse { .. }),
            "expected parse error: {err:?}"
        );
    }
}

// ── Positional successor validation ────────────────────────────────────────

/// qpdf rejects a non-integer positional successor even when `/Length` points
/// to an unrelated missing object.
#[test]
fn noninteger_positional_successor_is_rejected_with_missing_length_target() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    pdf.extend_from_slice(b"<<\n  /Length 9 0 R\n>>\nstream\nhello\nendstream\nendobj\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 2 0\n2 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    // Object 2 follows the stream but begins with a dictionary, not an integer.
    pdf.extend_from_slice(
        b"xref\n0 3\n0000000000 65535 f \n0000000000 00000 n \n0000000000 00000 n \n",
    );
    pdf.extend_from_slice(b"trailer <<\n  /Root 2 0 R\n  /Size 3\n>>\nstartxref\n0\n%%EOF\n");
    let err = flpdf::fix_qdf(&pdf).unwrap_err();
    assert!(
        format!("{err}").contains("integer"),
        "expected integer error: {err}"
    );
}

/// The successor's body line must be an integer even when the successor is
/// an ObjStm dictionary; qpdf fails before it reaches ObjStm processing.
#[test]
fn objstm_successor_is_rejected_by_integer_line_rule() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(
        b"1 0 obj\n<<\n  /Length 2 0 R\n>>\nstream\nhello\nendstream\nendobj\n\n",
    );
    pdf.extend_from_slice(
        b"2 0 obj\n<<\n  /Type /ObjStm\n  /N 1\n  /First 0\n>>\n\
          stream\n0\n%% Object stream: object 3\n<<\n  /Type /Catalog\n>>\n\
          endstream\nendobj\n\n",
    );
    pdf.extend_from_slice(
        b"4 0 obj\n<<\n  /Type /XRef\n  /Root 3 0 R\n  /Size 0\n>>\n\
          stream\nXXXXXXXXXX\nendstream\nendobj\n\nstartxref\n0\n%%EOF\n",
    );
    let err = flpdf::fix_qdf(&pdf).unwrap_err();
    assert!(
        matches!(err, flpdf::Error::Parse { .. }),
        "an ObjStm successor must fail qpdf's integer-line check, got: {err:?}"
    );
    assert!(
        format!("{err}").contains("integer"),
        "expected integer error: {err}"
    );
}

/// A second stream immediately after the first cannot act as a bare integer
/// holder, regardless of whether both `/Length` keys declare object 4.
#[test]
fn following_stream_is_not_a_bare_integer_holder() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n<<\n  /Length 4 0 R\n>>\nstream\nABC\nendstream\nendobj\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 2 0\n2 0 obj\n<<\n  /Length 4 0 R\n>>\nstream\nABCDEFGHI\nendstream\nendobj\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 3 0\n3 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    pdf.extend_from_slice(b"4 0 obj\n0\nendobj\n\n");
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n0000000000 00000 n \n0000000000 00000 n \n0000000000 00000 n \n0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 3 0 R\n  /Size 5\n>>\nstartxref\n0\n%%EOF\n");
    let err = flpdf::fix_qdf(&pdf).unwrap_err();
    assert!(
        format!("{err}").contains("integer"),
        "expected integer error: {err}"
    );
}

// ── Object numbers must be 1..N in file order ──────────────────────────────
// fix_qdf sizes the regenerated xref from the object COUNT, never the maximum
// object number, so a sparse/huge number cannot amplify the table. It requires
// objects numbered exactly 1..N in ascending file order (qpdf's
// QdfFixer::checkObjId), which closes the dense-xref amplification DoS and the
// `max_num + 1` overflow AND matches qpdf byte-for-byte. flpdf's own writer
// emits objects in ascending file order, so this rejects
// nothing the writer produces — see `writer_indirect_length_qdf_round_trips`.

/// A sparse high object number (the second object is `1_000_000`, not `2`) is
/// rejected — it would otherwise drive a multi-megabyte dense xref table.
#[test]
fn sparse_high_object_number_is_rejected() {
    let pdf = two_object_qdf_with_second_number(1_000_000);
    let err = flpdf::fix_qdf(&pdf).expect_err("sparse high object number must be rejected");
    assert!(
        matches!(err, flpdf::Error::Parse { .. }),
        "expected Parse error for sparse high object number, got {err:?}"
    );
    assert!(
        format!("{err}").contains("non-sequential object numbering"),
        "unexpected error: {err}"
    );
}

/// `u32::MAX` as an object number is rejected with a normal error, never
/// overflowing `max_num + 1` (debug panic) or wrapping `/Size` (release build).
#[test]
fn max_u32_object_number_is_rejected_without_overflow() {
    let pdf = two_object_qdf_with_second_number(u32::MAX);
    let err = flpdf::fix_qdf(&pdf).expect_err("u32::MAX object number must be rejected");
    assert!(
        format!("{err}").contains("non-sequential object numbering"),
        "unexpected error: {err}"
    );
}

/// A duplicate object number (two `1 0 obj`) is rejected: the second object's
/// number is 1 where 2 is expected, so it fails the sequential 1..N check.
#[test]
fn duplicate_object_number_is_rejected() {
    let pdf = two_object_qdf_with_second_number(1);
    let err = flpdf::fix_qdf(&pdf).expect_err("duplicate object number must be rejected");
    assert!(
        format!("{err}").contains("non-sequential object numbering"),
        "unexpected error: {err}"
    );
}

/// A top-level object header with a non-zero generation (`1 1 obj`) is
/// rejected. Confirmed against the live oracle: `fix-qdf.cc`'s object-header
/// regex is `re_n_0_obj = "^(\d+) 0 obj\n$"` (`qpdf/fix-qdf.cc:87`), which
/// hard-codes generation `0` — a `1 1 obj` line simply does not match, so
/// `st_top` falls through to its default `std::cout << line;` and echoes it
/// as plain text WITHOUT calling `checkObjId`/incrementing `last_obj`
/// (`qpdf/fix-qdf.cc:126-134`). The real `fix-qdf 1 1 obj ... 2 0 obj ...`
/// binary exits 2 with `expected object 1` at the `2 0 obj` line — the
/// generation is neither preserved nor threaded through the regenerated
/// xref (whose type-1 entries have no generation field at all;
/// `qpdf/fix-qdf.cc:222-233`), it is simply never recognized as an object.
#[test]
fn nonzero_generation_top_level_object_is_rejected() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"1 1 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n");
    pdf.extend_from_slice(b"2 0 obj\n<<\n  /Type /Pages\n  /Kids [ ]\n  /Count 0\n>>\nendobj\n\n");
    pdf.extend_from_slice(
        b"xref\n0 3\n0000000000 65535 f \n0000000000 00001 n \n0000000000 00000 n \n",
    );
    pdf.extend_from_slice(b"trailer <<\n  /Root 1 1 R\n  /Size 3\n>>\nstartxref\n0\n%%EOF\n");
    let err = flpdf::fix_qdf(&pdf).expect_err("non-zero top-level generation must be rejected");
    assert!(
        matches!(err, flpdf::Error::Parse { .. }),
        "expected a Parse error, got {err:?}"
    );
    assert!(
        format!("{err}").contains("non-sequential object numbering"),
        "a `1 1 obj` header must not be recognized as object 1, so the next \
         real object (`2 0 obj`) must fail the sequential-numbering check \
         (matching the oracle's `expected object 1` fatal at that same \
         point), got: {err}"
    );
}

/// Build a minimal QDF whose first object is `1 0 obj` and whose second object
/// is `{second} 0 obj`. `second == 2` is the only valid numbering; a huge value
/// is the sparse/overflow attack shape and `1` makes a duplicate — both
/// non-sequential.
fn two_object_qdf_with_second_number(second: u32) -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"1 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n");
    pdf.extend_from_slice(format!("{second} 0 obj\n<<\n>>\nendobj\n\n").as_bytes());
    pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 1 0 R\n  /Size 2\n>>\nstartxref\n0\n%%EOF\n");
    pdf
}

/// flpdf's QDF writer emits each indirect `/Length` holder inline after its
/// stream, so even an indirect-length source — which used
/// to produce out-of-order numbering (`1 2 3 4 5 7 6 8`) — now emits objects in
/// ascending file order `1..N`. Because fix_qdf requires that order, its
/// this test *proves* the writer output is qpdf-canonical; and on the
/// writer's already-correct output it must be a strict no-op. This is the
/// writer↔fix_qdf mesh guard for an indirect-length source (the test would
/// regress to a hard error if the writer ever re-emitted holders out of order).
#[test]
fn writer_indirect_length_qdf_round_trips() {
    use flpdf::Pdf;
    use std::io::Cursor;

    // Streams are clean Flate so `qpdf --check` stays warning-free.
    let source = read("../compat/objstm-lin-od-indirect-length-flate.pdf");
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
    let opts = WriterTestSettings {
        qdf: true,
        static_id: true,
        ..WriterTestSettings::default()
    };
    let mut qdf = Vec::new();
    write_with_settings(&mut pdf, &mut qdf, &opts).unwrap();

    // fix_qdf accepts only ascending 1..N file order, so a successful repair
    // proves the writer emitted this indirect-length source qpdf-canonically;
    // and on already-correct writer output it must be a strict no-op.
    let fixed = flpdf::fix_qdf(&qdf).expect("fix_qdf must repair flpdf's own indirect-length QDF");
    assert_eq!(
        fixed, qdf,
        "fix_qdf must be a no-op on flpdf's own (ascending) indirect-length QDF"
    );

    // qpdf must accept the result.
    if Command::new("qpdf").arg("--version").output().is_ok() {
        let dir = tempfile::tempdir().expect("temp dir");
        let tmp = dir.path().join("indirect-length.pdf");
        fs::write(&tmp, &fixed).unwrap();
        let out = Command::new("qpdf")
            .arg("--check")
            .arg(&tmp)
            .output()
            .expect("run qpdf --check");
        assert!(
            out.status.success(),
            "qpdf --check failed on repaired indirect-length QDF:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    } else {
        eprintln!("qpdf not available; skipping qpdf --check in indirect-length round-trip test");
    }

    // Idempotent on its own repaired output.
    let again = flpdf::fix_qdf(&fixed).expect("fix_qdf idempotent");
    assert_eq!(
        again, fixed,
        "fix_qdf must be idempotent on repaired indirect-length QDF"
    );
}

/// The QDF trailer serializer (writer.rs) branches on
/// `options.deterministic_id` inside the `options.qdf` arm; that combination
/// (unlike `qdf` alone or `deterministic_id` alone, each covered elsewhere)
/// had no covering test. Pins the QDF trailer layout, a well-formed
/// content-derived `/ID`, self-stability across repeat writes, and
/// `qpdf --check` acceptance.
#[test]
fn writer_qdf_and_deterministic_id_combine() {
    use flpdf::Pdf;
    use std::io::Cursor;

    let source = read("../compat/three-page.pdf");
    let opts = WriterTestSettings {
        qdf: true,
        deterministic_id: true,
        ..WriterTestSettings::default()
    };

    let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();
    let mut first = Vec::new();
    write_with_settings(&mut pdf, &mut first, &opts).unwrap();

    // QDF trailer shape: "trailer <<" on its own line (not the classic
    // single-space "trailer <<...>>" form).
    assert!(
        find(&first, b"\ntrailer <<\n").is_some(),
        "qdf output must use the QDF trailer layout"
    );

    // The deterministic /ID is content-derived: two 32-hex-digit strings,
    // neither of which is the all-zero static-id constant.
    let id_kw = find(&first, b"/ID").expect("/ID entry in QDF trailer");
    let after = &first[id_kw + 3..];
    let open = after
        .iter()
        .position(|&b| b == b'[')
        .expect("/ID array open");
    let close = after[open..]
        .iter()
        .position(|&b| b == b']')
        .map(|i| open + i)
        .expect("/ID array close");
    let body = std::str::from_utf8(&after[open + 1..close]).expect("ascii /ID body");
    let hex_pairs: Vec<&str> = body
        .split('<')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('>'))
        .collect();
    assert_eq!(hex_pairs.len(), 2, "/ID must carry two hex strings: {body}");
    for id in &hex_pairs {
        assert_eq!(id.len(), 32, "/ID element must be 32 hex digits: {id}");
        assert_ne!(
            *id,
            "0".repeat(32),
            "deterministic /ID must not be the static-id constant"
        );
    }

    // Self-stable: a second write of the same source produces byte-identical
    // output, matching --deterministic-id's contract in the qdf branch too.
    let mut pdf2 = Pdf::open(Cursor::new(source)).unwrap();
    let mut second = Vec::new();
    write_with_settings(&mut pdf2, &mut second, &opts).unwrap();
    assert_eq!(
        first, second,
        "deterministic /ID must be self-stable across identical writes"
    );

    if Command::new("qpdf").arg("--version").output().is_ok() {
        let dir = tempfile::tempdir().expect("temp dir");
        let tmp = dir.path().join("qdf-deterministic-id.pdf");
        fs::write(&tmp, &first).unwrap();
        let out = Command::new("qpdf")
            .arg("--check")
            .arg(&tmp)
            .output()
            .expect("run qpdf --check");
        assert!(
            out.status.success(),
            "qpdf --check failed on qdf+deterministic-id output:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    } else {
        eprintln!("qpdf not available; skipping qpdf --check in qdf+deterministic-id test");
    }
}

mod common;
#[allow(unused_imports)]
use common::{write_default, write_with_settings, WriterTestSettings};
