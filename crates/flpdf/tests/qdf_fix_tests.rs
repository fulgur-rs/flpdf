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
    ] {
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
    for clean in ["one-page-clean.qdf", "minimal-clean.qdf"] {
        let data = read(clean);
        let got = flpdf::fix_qdf(&data).unwrap();
        assert_eq!(got, data, "{clean}: fix_qdf should be a no-op on clean QDF");
    }
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
    ] {
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
    let tmp = std::env::temp_dir().join("flpdf_qdf_fix_check.pdf");
    for case in [
        "corrupt-length",
        "corrupt-shift",
        "corrupt-size",
        "corrupt-startxref",
        "corrupt-combo",
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
    let _ = fs::remove_file(&tmp);
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
    ] {
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

/// An object stream in the input is rejected with an `Unsupported` error
/// (QDF mode disables ObjStm; this is the documented failure mode).
#[test]
fn objstm_input_is_unsupported() {
    let mut data = read("one-page-clean.qdf");
    // Inject a fake /ObjStm type into the first object's dictionary.
    let pos = data
        .windows(7)
        .position(|w| w == b"/Type /")
        .expect("a /Type entry to mutate");
    data.splice(pos..pos, b"/Type /ObjStm ".iter().copied());
    let err = flpdf::fix_qdf(&data).unwrap_err();
    assert!(
        matches!(err, flpdf::Error::Unsupported(_)),
        "expected Unsupported for ObjStm input, got {err:?}"
    );
}

/// Regression for roborev job 989 (qdf_fix.rs robustness):
///   1. A decompressed stream body that contains a line-anchored `xref` must
///      NOT be mistaken for the cross-reference table (use the LAST one).
///   2. A `stream` byte sequence inside a dictionary string value must NOT be
///      mistaken for the `stream` keyword (match it line-anchored).
#[test]
fn ignores_xref_and_stream_inside_object_body() {
    // obj 1: stream whose dict has a string containing the word "stream" and
    // whose decompressed content contains a line `xref`. /Length is indirect
    // (held by obj 4). Initial xref offsets are intentionally bogus zeros —
    // fix_qdf must regenerate them and still pick the real table at the tail.
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    pdf.extend_from_slice(b"<<\n  /Length 4 0 R\n  /Note (the word stream appears here)\n>>\n");
    pdf.extend_from_slice(b"stream\nline one\nxref\nendstream\nendobj\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 2 0\n2 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    pdf.extend_from_slice(b"%% Original object ID: 4 0\n4 0 obj\n0\nendobj\n\n");
    // Real (tail) xref table with deliberately wrong offsets.
    pdf.extend_from_slice(b"xref\n0 5\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 2 0 R\n  /Size 5\n>>\nstartxref\n0\n%%EOF\n");

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must succeed");
    let s = &fixed;

    // The `xref` line inside obj 1's stream body is preserved verbatim.
    assert!(
        find(s, b"stream\nline one\nxref\nendstream").is_some(),
        "stream body (incl. its inner `xref` line) must be preserved verbatim"
    );

    // Exactly ONE regenerated cross-reference table: a line-anchored `xref`
    // immediately followed by the `0 5` subsection header.
    assert!(
        find(s, b"\nxref\n0 5\n").is_some(),
        "real xref table must be regenerated at the tail"
    );

    // /Length holder (obj 4) recomputed to the verbatim content byte count:
    // "line one\nxref\n" == 14 bytes (after `stream`+EOL, up to line `endstream`).
    assert!(
        find(s, b"4 0 obj\n14\nendobj").is_some(),
        "indirect /Length holder must be recomputed to 14, got:\n{}",
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

/// Regression for roborev job 991 (qdf_fix.rs ~251):
///   A decompressed QDF stream body may contain a line-anchored `endobj` (and
///   `xref`). The naive "first line-anchored endobj after N G obj" would
///   truncate the object span there, corrupting subsequent xref/length repair.
///   fix_qdf must anchor the endobj search AFTER `endstream`.
#[test]
fn stream_body_endobj_and_xref_not_mistaken_for_object_terminator() {
    // obj 1: stream whose decompressed body contains BOTH a line `endobj` and
    // a line `xref` — the canonical regression case for roborev 991.
    // /Length is indirect (held by obj 4). xref offsets are bogus zeros.
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    pdf.extend_from_slice(b"<<\n  /Length 4 0 R\n>>\n");
    // Stream body contains both `endobj` and `xref` on their own lines.
    pdf.extend_from_slice(
        b"stream\nsome content\nendobj\nmore content\nxref\nfinal line\nendstream\nendobj\n\n",
    );
    pdf.extend_from_slice(
        b"%% Original object ID: 2 0\n2 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    pdf.extend_from_slice(b"%% Original object ID: 4 0\n4 0 obj\n0\nendobj\n\n");
    // Real (tail) xref table with deliberately wrong offsets.
    pdf.extend_from_slice(b"xref\n0 5\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 2 0 R\n  /Size 5\n>>\nstartxref\n0\n%%EOF\n");

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
        find(&fixed, b"\nxref\n0 5\n").is_some(),
        "real xref table must be regenerated at the tail"
    );

    // /Length holder (obj 4) recomputed to the verbatim content byte count:
    // "some content\nendobj\nmore content\nxref\nfinal line\n" = 50 bytes.
    let expected_len = b"some content\nendobj\nmore content\nxref\nfinal line\n".len();
    let expected_holder = format!("4 0 obj\n{expected_len}\nendobj");
    assert!(
        find(&fixed, expected_holder.as_bytes()).is_some(),
        "indirect /Length holder must be recomputed to {expected_len};\ngot:\n{}",
        String::from_utf8_lossy(&fixed)
    );

    // Idempotent.
    let again = flpdf::fix_qdf(&fixed).expect("fix_qdf must be idempotent");
    assert_eq!(
        again, fixed,
        "fix_qdf must be idempotent on stream-body-endobj output"
    );
}

/// Regression for roborev job 992 (qdf_fix.rs ~189 classify_length):
///   A stream dict containing `/Length1 999` before the real `/Length H 0 R`
///   must not fool classify_length into treating `/Length1` as `/Length`.
///   fix_qdf must locate and recompute the REAL indirect length holder H.
#[test]
fn length1_not_mistaken_for_indirect_length() {
    // obj 1: stream with `/Length1 999` before the real `/Length 4 0 R`.
    // obj 4 is the length holder. Bogus xref offsets.
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    // /Length1 appears BEFORE /Length — the false-match scenario.
    pdf.extend_from_slice(b"<<\n  /Length1 999\n  /Length 4 0 R\n>>\n");
    pdf.extend_from_slice(b"stream\nhello world\nendstream\nendobj\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 2 0\n2 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    pdf.extend_from_slice(b"%% Original object ID: 4 0\n4 0 obj\n0\nendobj\n\n");
    pdf.extend_from_slice(b"xref\n0 5\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 2 0 R\n  /Size 5\n>>\nstartxref\n0\n%%EOF\n");

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must succeed with /Length1 in dict");

    // /Length holder obj 4 must be recomputed to the stream body byte count:
    // "hello world\n" = 12 bytes.
    let expected_len = b"hello world\n".len();
    let expected_holder = format!("4 0 obj\n{expected_len}\nendobj");
    assert!(
        find(&fixed, expected_holder.as_bytes()).is_some(),
        "indirect /Length holder must be recomputed to {expected_len} (not fooled by /Length1);\ngot:\n{}",
        String::from_utf8_lossy(&fixed)
    );

    // /Length1 must NOT have been misread as the holder reference — obj 999
    // does not exist, so if classify_length had parsed 999 as the holder num
    // the function would return an error above. The fact that we got Ok(fixed)
    // already proves we didn't pick /Length1. Assert the stale holder (0) is
    // gone and the correct value is present.
    assert!(
        find(&fixed, b"4 0 obj\n0\nendobj").is_none(),
        "stale holder value 0 must have been replaced"
    );

    // Idempotent.
    let again = flpdf::fix_qdf(&fixed).expect("fix_qdf must be idempotent");
    assert_eq!(
        again, fixed,
        "fix_qdf must be idempotent on /Length1 output"
    );
}

/// Negative control for roborev job 992: a dict with ONLY `/Length1` and a
/// direct `/Length` integer has no indirect holder; fix_qdf must leave the
/// direct length verbatim (the oracle/design: direct lengths are out of scope).
#[test]
fn direct_length_with_length1_left_verbatim() {
    // obj 1: stream with `/Length1 999` and a DIRECT `/Length 11`.
    // No length-holder object exists.
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    pdf.extend_from_slice(b"<<\n  /Length1 999\n  /Length 11\n>>\n");
    pdf.extend_from_slice(b"stream\nhello world\nendstream\nendobj\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 2 0\n2 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    pdf.extend_from_slice(b"xref\n0 3\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 2 0 R\n  /Size 3\n>>\nstartxref\n0\n%%EOF\n");

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must succeed on direct-length input");

    // The direct /Length 11 must be preserved verbatim (fix_qdf does not
    // rewrite direct lengths — that is intentionally out of scope).
    assert!(
        find(&fixed, b"/Length 11\n").is_some(),
        "direct /Length must be preserved verbatim;\ngot:\n{}",
        String::from_utf8_lossy(&fixed)
    );

    // No spurious holder rewrite: /Length1 must not be touched.
    assert!(
        find(&fixed, b"/Length1 999\n").is_some(),
        "/Length1 must be preserved verbatim"
    );
}

/// Closed loop for flpdf-9hc.6.12: the flpdf QDF writer and flpdf::fix_qdf
/// must mesh. Produce a real QDF via the writer (it now emits indirect
/// `/Length H 0 R` + a bare-integer holder), hand-edit a stream's decoded
/// payload (the canonical "human edits the QDF" use case), run flpdf::fix_qdf,
/// and verify it repairs the indirect length-holder body — then `qpdf --check`
/// accepts the result. This is the lighter version; the full round-trip
/// matrix is flpdf-9hc.6.9.
#[test]
fn writer_qdf_then_edit_then_fix_qdf_closed_loop() {
    use flpdf::{write_pdf_with_options, Pdf, WriteOptions};
    use std::io::Cursor;

    let source = read("../compat/three-page.pdf");
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.qdf = true;
    opts.static_id = true;
    let mut qdf = Vec::new();
    write_pdf_with_options(&mut pdf, &mut qdf, &opts).unwrap();

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
    // test does not hardcode the fixture's content length. It is the on-disk
    // byte count (payload + the single EOL the Yes framing adds), which is
    // exactly what flpdf::fix_qdf recomputes — so on the UNEDITED file
    // fix_qdf must be a perfect no-op (writer↔fix_qdf mesh).
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
        let tmp = std::env::temp_dir().join("flpdf_qdf_closed_loop.pdf");
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
        let _ = fs::remove_file(&tmp);
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

/// Regression for roborev job 993: `/Length` appearing inside a string value
/// or a comment in the stream dict must NOT be mistaken for the real key.
/// fix_qdf must still locate the genuine indirect `/Length H 0 R` and
/// recompute holder H after the stream content is edited.
#[test]
fn length_inside_string_or_comment_not_mistaken_for_key() {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    // Decoy `/Length` inside a literal string AND a comment, before the real
    // indirect /Length key.
    pdf.extend_from_slice(b"<<\n  /Note (a /Length 999 decoy)\n");
    pdf.extend_from_slice(b"  %% /Length 888 in a comment\n");
    pdf.extend_from_slice(b"  /Length 3 0 R\n>>\n");
    pdf.extend_from_slice(b"stream\nABCDEFGHIJ\nendstream\nendobj\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 2 0\n2 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    pdf.extend_from_slice(b"3 0 obj\n0\nendobj\n\n");
    pdf.extend_from_slice(b"xref\n0 4\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 2 0 R\n  /Size 4\n>>\nstartxref\n0\n%%EOF\n");

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must succeed");

    // Holder object 3 must be recomputed to the on-disk content length of the
    // stream payload ("ABCDEFGHIJ\n" = 11 bytes, payload + framing EOL).
    assert!(
        find(&fixed, b"\n3 0 obj\n11\nendobj").is_some(),
        "indirect /Length holder 3 must be recomputed (decoy /Length in string/comment ignored):\n{}",
        String::from_utf8_lossy(&fixed)
    );
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
