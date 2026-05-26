//! Integration tests for `--linearize` and `check-linearization` CLI surface.
//!
//! Happy-path: linearize a known fixture, then verify with check-linearization.
//! Malformed-path: tamper with /L in the output; check-linearization must exit 1.
//! Regression: plain `rewrite` (no --linearize) still works unchanged.

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write as _;

// ---------------------------------------------------------------------------
// Helper: build a minimal single-page PDF fixture in memory
// ---------------------------------------------------------------------------

fn minimal_pdf_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    let xref_start = pdf.len();
    let xref = format!(
        "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
        off1, off2, off3,
    );
    pdf.extend_from_slice(xref.as_bytes());
    let trailer = format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    pdf.extend_from_slice(trailer.as_bytes());
    pdf
}

fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(bytes).unwrap();
    f
}

// ---------------------------------------------------------------------------
// 1. Happy path: rewrite --linearize produces a file, check-linearization passes
// ---------------------------------------------------------------------------

#[test]
fn rewrite_linearize_then_check_passes() {
    let input = write_temp(&minimal_pdf_bytes());
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("linearized.pdf");

    // --linearize
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(output.exists(), "linearized output file must be created");
    assert!(
        std::fs::metadata(&output).unwrap().len() > 0,
        "linearized output must not be empty"
    );

    // check-linearization must exit 0
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["check-linearization", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("linearization OK"));
}

// ---------------------------------------------------------------------------
// 2. check-linearization on a malformed file exits 1 with actionable stderr
// ---------------------------------------------------------------------------

#[test]
fn check_linearization_tampered_l_exits_1() {
    // First produce a valid linearized file.
    let input = write_temp(&minimal_pdf_bytes());
    let outdir = tempfile::tempdir().unwrap();
    let linearized_path = outdir.path().join("linearized.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            input.path().to_str().unwrap(),
            linearized_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Read and tamper: find "/L " followed by ASCII digits (variable-width
    // post flpdf-9hc.20.25) and bump the last digit so /L != file_length.
    let mut bytes = std::fs::read(&linearized_path).unwrap();
    let needle = b"/L ";
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("could not find '/L ' in linearized output for tampering");
    let val_start = pos + needle.len();
    let val_end = val_start
        + bytes[val_start..]
            .iter()
            .position(|&b| !b.is_ascii_digit())
            .expect("/L value must terminate at a non-digit");
    assert!(val_end > val_start, "/L value must have at least one digit");
    let last = val_end - 1;
    bytes[last] = if bytes[last] == b'9' { b'0' } else { bytes[last] + 1 };

    let tampered_path = outdir.path().join("tampered.pdf");
    std::fs::write(&tampered_path, &bytes).unwrap();

    // check-linearization must exit 1 with an actionable message.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["check-linearization", tampered_path.to_str().unwrap()])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("linearization check failed"));
}

// ---------------------------------------------------------------------------
// 3. check-linearization on a non-linearized PDF exits 1
// ---------------------------------------------------------------------------

#[test]
fn check_linearization_non_linearized_exits_1() {
    let input = write_temp(&minimal_pdf_bytes());

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["check-linearization", input.path().to_str().unwrap()])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not a linearized PDF"));
}

// ---------------------------------------------------------------------------
// 4. Regression: plain `rewrite` (no --linearize) still works
// ---------------------------------------------------------------------------

#[test]
fn rewrite_without_linearize_flag_still_works() {
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(output.exists());
    assert!(std::fs::metadata(&output).unwrap().len() > 0);
}

// ---------------------------------------------------------------------------
// 5. check-linearization on a missing file exits 2 (I/O error)
// ---------------------------------------------------------------------------

#[test]
fn check_linearization_missing_file_exits_2() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "check-linearization",
            "/tmp/this_file_does_not_exist_flpdf_test.pdf",
        ])
        .assert()
        .failure()
        .code(2);
}

// ---------------------------------------------------------------------------
// 6. Version selection: --linearize inherits source version in the header
//
// Fixture: tests/fixtures/compat/one-page.pdf starts with %PDF-1.3
// qpdf --linearize one-page.pdf → %PDF-1.3 (source version inherited)
// flpdf rewrite --linearize should produce the same header.
//
// Note: qpdf may downgrade the version based on feature analysis (e.g.
// two-page.pdf 1.4 → 1.3).  We do not replicate that subsystem; only
// "source >= 1.2" docs where qpdf also preserves the version are tested.
// ---------------------------------------------------------------------------

#[test]
fn linearize_inherits_source_version() {
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("lin.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "../../tests/fixtures/compat/one-page.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-1.3\n"),
        "linearized header must be %PDF-1.3 (inherited from source); got: {}",
        String::from_utf8_lossy(&bytes[..bytes.len().min(20)])
    );
}

// ---------------------------------------------------------------------------
// 7. Version selection: --min-version raises the header version
// ---------------------------------------------------------------------------

#[test]
fn linearize_min_version_raises_header() {
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("lin-min17.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--min-version=1.7",
            "../../tests/fixtures/compat/one-page.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-1.7\n"),
        "with --min-version=1.7 the header must be %PDF-1.7; got: {}",
        String::from_utf8_lossy(&bytes[..bytes.len().min(20)])
    );
}

// ---------------------------------------------------------------------------
// 8. Version selection: --force-version overrides source and linearize floor
// ---------------------------------------------------------------------------

#[test]
fn linearize_force_version_overrides() {
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("lin-force14.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--force-version=1.4",
            "../../tests/fixtures/compat/one-page.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-1.4\n"),
        "with --force-version=1.4 the header must be %PDF-1.4; got: {}",
        String::from_utf8_lossy(&bytes[..bytes.len().min(20)])
    );
}

// ---------------------------------------------------------------------------
// 9. Version selection: --force-version can go below the linearize 1.2 floor
// ---------------------------------------------------------------------------

#[test]
fn linearize_force_version_overrides_linearize_floor() {
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("lin-force10.pdf");

    // %PDF-1.0 is unusual but the --force flag must honour the request.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--force-version=1.0",
            "../../tests/fixtures/compat/one-page.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-1.0\n"),
        "with --force-version=1.0 the header must be %PDF-1.0; got: {}",
        String::from_utf8_lossy(&bytes[..bytes.len().min(20)])
    );
}

// ---------------------------------------------------------------------------
// 10. Default /ID strategy (flpdf-9hc.13.2): when source has no /ID and
//     --static-id is not set, the linearized output must still emit a fresh
//     random two-element /ID that differs between runs and is not the qpdf
//     static-id (π) constant.  Matches qpdf's default observable behaviour
//     (ISO 32000-1 §14.4).
// ---------------------------------------------------------------------------

/// Extract the two hex byte-strings of the *first* `/ID [<..><..>]` array.
fn first_id_pair(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let pos = bytes
        .windows(3)
        .position(|w| w == b"/ID")
        .expect("/ID key present");
    let lb = bytes[pos..]
        .iter()
        .position(|&b| b == b'[')
        .map(|i| pos + i)
        .expect("/ID array open bracket");
    let rb = bytes[lb..]
        .iter()
        .position(|&b| b == b']')
        .map(|i| lb + i)
        .expect("/ID array close bracket");
    let slice = &bytes[lb + 1..rb];
    let mut hexes = slice
        .split(|&b| b == b'<' || b == b'>')
        .filter(|s| !s.is_empty() && s.iter().all(|&c| c.is_ascii_hexdigit()))
        .map(|s| s.to_vec());
    let a = hexes.next().expect("/ID element 1");
    let b = hexes.next().expect("/ID element 2");
    (a, b)
}

#[test]
fn linearize_no_source_id_emits_fresh_random_id() {
    // minimal_pdf_bytes() builds a trailer with no /ID entry.
    let input = write_temp(&minimal_pdf_bytes());

    let run = || {
        let outdir = tempfile::tempdir().unwrap();
        let output = outdir.path().join("linearized.pdf");
        Command::cargo_bin("flpdf")
            .unwrap()
            .args([
                "rewrite",
                "--linearize",
                input.path().to_str().unwrap(),
                output.to_str().unwrap(),
            ])
            .assert()
            .success();
        std::fs::read(&output).unwrap()
    };

    let bytes1 = run();
    let bytes2 = run();

    // /ID must be present despite the source having none.
    let id_needle = b"/ID";
    assert!(
        bytes1.windows(id_needle.len()).any(|w| w == id_needle),
        "default strategy must emit /ID even when source has none"
    );

    let (a1, b1) = first_id_pair(&bytes1);
    let (a2, b2) = first_id_pair(&bytes2);

    // Each element is a 16-byte (32 hex digit) string.
    assert_eq!(a1.len(), 32, "/ID element 1 must be 16 bytes (32 hex)");
    assert_eq!(b1.len(), 32, "/ID element 2 must be 16 bytes (32 hex)");

    // Not the qpdf static-id (π) constant.
    let pi = b"31415926535897932384626433832795";
    assert_ne!(a1.as_slice(), pi, "/ID[0] must not be the π constant");
    assert_ne!(b1.as_slice(), pi, "/ID[1] must not be the π constant");

    // Not all zeros.
    assert!(a1.iter().any(|&c| c != b'0'), "/ID[0] must not be all-zero");
    assert!(b1.iter().any(|&c| c != b'0'), "/ID[1] must not be all-zero");

    // Random: two independent runs of a no-/ID source produce different IDs.
    assert!(
        (a1, b1) != (a2, b2),
        "default /ID must differ between independent runs"
    );
}

// ---------------------------------------------------------------------------
// 11. /ID present: when --static-id is set, the linearized output must
//     contain /ID (regression guard for the static-id path).
// ---------------------------------------------------------------------------

#[test]
fn linearize_static_id_emits_id_key() {
    let input = write_temp(&minimal_pdf_bytes());
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("linearized-static.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--static-id",
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();

    // The trailer must contain /ID when --static-id is set.
    let id_needle = b"/ID";
    assert!(
        bytes.windows(id_needle.len()).any(|w| w == id_needle),
        "linearized output with --static-id must contain /ID"
    );
}

// ---------------------------------------------------------------------------
// 12. parse_pdf_version / effective_pdf_version unit tests
// ---------------------------------------------------------------------------

#[test]
fn parse_pdf_version_valid() {
    use flpdf::parse_pdf_version;
    assert_eq!(parse_pdf_version("1.3"), Some((1, 3)));
    assert_eq!(parse_pdf_version("1.7"), Some((1, 7)));
    assert_eq!(parse_pdf_version("2.0"), Some((2, 0)));
    assert_eq!(parse_pdf_version("1.10"), Some((1, 10)));
    assert_eq!(parse_pdf_version("invalid"), None);
    assert_eq!(parse_pdf_version(""), None);
}

#[test]
fn effective_version_source_inherit() {
    use flpdf::{effective_pdf_version, WriteOptions};
    let opts = WriteOptions::default();
    assert_eq!(effective_pdf_version("1.3", &opts, false), "1.3");
    assert_eq!(effective_pdf_version("1.7", &opts, false), "1.7");
}

#[test]
fn effective_version_linearize_floor() {
    use flpdf::{effective_pdf_version, WriteOptions};
    let opts = WriteOptions::default();
    // Source 1.0 + linearize → should be bumped to 1.2.
    assert_eq!(effective_pdf_version("1.0", &opts, true), "1.2");
    // Source 1.3 + linearize → stays 1.3.
    assert_eq!(effective_pdf_version("1.3", &opts, true), "1.3");
}

#[test]
fn effective_version_min_version() {
    use flpdf::{effective_pdf_version, WriteOptions};
    let mut opts = WriteOptions::default();
    opts.min_version = Some("1.7".to_string());
    // Source 1.3, min 1.7 → 1.7
    assert_eq!(effective_pdf_version("1.3", &opts, false), "1.7");
    // Source 1.7, min 1.3 → stays 1.7
    opts.min_version = Some("1.3".to_string());
    assert_eq!(effective_pdf_version("1.7", &opts, false), "1.7");
}

#[test]
fn effective_version_force_version() {
    use flpdf::{effective_pdf_version, WriteOptions};
    let mut opts = WriteOptions::default();
    opts.force_version = Some("1.4".to_string());
    // force overrides everything, even source 1.7
    assert_eq!(effective_pdf_version("1.7", &opts, false), "1.4");
    // force overrides linearize floor
    opts.force_version = Some("1.0".to_string());
    assert_eq!(effective_pdf_version("1.3", &opts, true), "1.0");
}

// ---------------------------------------------------------------------------
// 13. Part 1 trailer startxref must be 0; Part 6 startxref must be the real
//     main xref offset (qpdf linearized PDF convention, ISO 32000-1 Annex F).
// ---------------------------------------------------------------------------

#[test]
fn linearize_part1_startxref_is_zero_main_startxref_is_real() {
    let input = write_temp(&minimal_pdf_bytes());
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("linearized.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--static-id",
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();

    let needle = b"startxref\n";

    // Helper: parse the decimal value immediately after "startxref\n" at pos.
    let parse_val = |pos: usize| -> usize {
        let val_start = pos + needle.len();
        let val_end = bytes[val_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| val_start + p)
            .expect("startxref value must be newline-terminated");
        std::str::from_utf8(&bytes[val_start..val_end])
            .expect("UTF-8")
            .trim()
            .parse()
            .expect("decimal")
    };

    // First startxref → Part 1 first trailer: must be 0.
    let first_pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("must have at least one startxref");
    let part1_val: usize = parse_val(first_pos);
    assert_eq!(
        part1_val, 0,
        "Part 1 first trailer startxref must be 0 (qpdf linearized convention)"
    );

    // Last startxref → Part 6 main trailer: must point at the last standalone
    // `xref` keyword (not the `xref` that appears inside `startxref`).
    let last_pos = bytes
        .windows(needle.len())
        .rposition(|w| w == needle)
        .expect("must have at least two startxref");
    let main_val: usize = parse_val(last_pos);
    let last_xref_pos = (0..bytes.len().saturating_sub(3))
        .rev()
        .find(|&i| {
            &bytes[i..i + 4] == b"xref"
                && (i == 0 || bytes[i - 1].is_ascii_whitespace())
                && (i + 4 >= bytes.len() || bytes[i + 4].is_ascii_whitespace())
        })
        .expect("must have at least one standalone xref keyword");
    assert_eq!(
        main_val, last_xref_pos,
        "Part 6 main startxref ({main_val}) must equal last xref keyword offset ({last_xref_pos})"
    );
}

// ---------------------------------------------------------------------------
// 14. Param dict integers are variable-width decimal (no zero-padding),
//     matching qpdf byte format (flpdf-9hc.20.25).
//
//     qpdf emits e.g. `/L 1701 /H [ 601 118 ] /O 6 /E 1198 /N 1 /T 1523`,
//     not flpdf's earlier `/L 0000001701 /H [ 0000000601 0000000118 ] ...`
//     fixed-width form.  Each integer field's decimal text matches the
//     minimal representation of its value, with no leading zeros (except
//     for value 0 itself, which is the single byte `0`).
// ---------------------------------------------------------------------------

/// Extract the bytes of the value following `key` (e.g. `/L `) up to the next
/// non-digit byte. Returns the digit slice including its position.
fn extract_int_field(bytes: &[u8], key: &[u8]) -> (usize, Vec<u8>) {
    let pos = bytes
        .windows(key.len())
        .position(|w| w == key)
        .unwrap_or_else(|| panic!("param dict key {:?} not found", String::from_utf8_lossy(key)));
    let val_start = pos + key.len();
    let val_end = bytes[val_start..]
        .iter()
        .position(|&b| !b.is_ascii_digit())
        .map(|p| val_start + p)
        .expect("integer field must terminate");
    (val_start, bytes[val_start..val_end].to_vec())
}

#[test]
fn linearize_param_dict_integers_are_variable_width_decimal() {
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("lin.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--static-id",
            "../../tests/fixtures/compat/one-page.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();

    // For every integer field, the digit-text must be the minimal decimal
    // representation of its numeric value (no leading zeros).
    let check_no_leading_zero = |label: &str, digits: &[u8]| {
        let s = std::str::from_utf8(digits).expect("digits utf-8");
        let parsed: u64 = s.parse().unwrap_or_else(|e| {
            panic!("{label} value '{s}' must be a valid decimal integer: {e}")
        });
        let canonical = parsed.to_string();
        assert_eq!(
            s, canonical,
            "{label} must be variable-width decimal with no zero-padding; \
             got '{s}', expected '{canonical}' (= {parsed})"
        );
    };

    let (_, l_digits) = extract_int_field(&bytes, b"/L ");
    check_no_leading_zero("/L", &l_digits);

    // /H [ X Y ]
    let h_pos = bytes
        .windows(b"/H [ ".len())
        .position(|w| w == b"/H [ ")
        .expect("/H array");
    let h_inner_start = h_pos + b"/H [ ".len();
    let space1 = bytes[h_inner_start..]
        .iter()
        .position(|&b| b == b' ')
        .map(|p| h_inner_start + p)
        .expect("/H[0] terminator");
    check_no_leading_zero("/H[0]", &bytes[h_inner_start..space1]);
    let h1_start = space1 + 1;
    let space2 = bytes[h1_start..]
        .iter()
        .position(|&b| b == b' ')
        .map(|p| h1_start + p)
        .expect("/H[1] terminator");
    check_no_leading_zero("/H[1]", &bytes[h1_start..space2]);

    let (_, o_digits) = extract_int_field(&bytes, b"/O ");
    check_no_leading_zero("/O", &o_digits);
    let (_, e_digits) = extract_int_field(&bytes, b"/E ");
    check_no_leading_zero("/E", &e_digits);
    let (_, n_digits) = extract_int_field(&bytes, b"/N ");
    check_no_leading_zero("/N", &n_digits);
    let (_, t_digits) = extract_int_field(&bytes, b"/T ");
    check_no_leading_zero("/T", &t_digits);
}

// ---------------------------------------------------------------------------
// 15. Param dict /L value equals total file length after variable-width
//     compaction (flpdf-9hc.20.25 — value semantics regression guard).
// ---------------------------------------------------------------------------
#[test]
fn linearize_l_value_equals_file_length_post_compact() {
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("lin.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--static-id",
            "../../tests/fixtures/compat/one-page.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    let (_, l_digits) = extract_int_field(&bytes, b"/L ");
    let l_val: usize = std::str::from_utf8(&l_digits)
        .unwrap()
        .parse()
        .expect("/L numeric");
    assert_eq!(
        l_val,
        bytes.len(),
        "/L ({l_val}) must equal the total file length ({})",
        bytes.len()
    );
}
