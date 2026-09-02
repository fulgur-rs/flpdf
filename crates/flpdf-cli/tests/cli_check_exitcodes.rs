//! CLI integration tests for qpdf-compatible `--check` / `check` exit codes.
//!
//! # Exit-code semantics (flpdf-9hc.23.2)
//!
//! Source: qpdf manual §"Exit Status"
//!   <https://qpdf.readthedocs.io/en/stable/cli.html#exit-status>
//! Confirmed by `qpdf/include/qpdf/Constants.h`:
//!   qpdf_exit_success = 0  (no errors or warnings)
//!   qpdf_exit_error   = 2  (errors found)
//!   qpdf_exit_warning = 3  (warnings found, no errors)
//!
//! Three fixture classes are exercised:
//!   1. clean PDF            → exit 0
//!   2. warnings-only PDF    → exit 3
//!   3. corrupt/error PDF    → exit 2

use assert_cmd::Command;
use flpdf::ObjectHandle;
use predicates::prelude::*;
use std::io::Write;

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

/// Minimal but fully valid single-page PDF — produces exit 0.
fn clean_pdf_bytes() -> Vec<u8> {
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
    pdf.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// PDF with a deliberately corrupt xref table.  When opened with `--repair`
/// the parser recovers via linear scan and emits a "xref repaired" warning
/// (no errors) → exit 3.
fn warnings_only_corrupt_xref_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let offsets: Vec<usize> = {
        let objects: &[&[u8]] = &[
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        ];
        let mut offs = Vec::new();
        for obj in objects {
            offs.push(pdf.len());
            pdf.extend_from_slice(obj);
        }
        offs
    };
    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );

    // Corrupt the xref keyword so the parser must repair.
    let xref_pos = pdf.windows(4).position(|w| w == b"xref").unwrap();
    pdf[xref_pos + 2] = b'z'; // "xref" → "xrzf"

    pdf
}

/// A repaired classic xref whose indirect trailer /Size points at an object
/// whose stale xref offset names a different object. qpdf reconstructs the
/// table, then continues its post-reconstruction /Size validation and emits
/// the object-count mismatch warning.
fn recovered_indirect_size_header_mismatch_pdf_bytes() -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let catalog_offset = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let wrong_offset = pdf.len() + b"3 0 obj\n3\nendobj\n".len();
    pdf.extend_from_slice(b"3 0 obj\n3\nendobj\n");
    pdf.extend_from_slice(b"4 0 obj\n<< /Foo true >>\nendobj\n");
    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{catalog_offset:010} 00000 n \n0000000000 65535 f \n{wrong_offset:010} 00000 n \n{wrong_offset:010} 00000 n \ntrailer\n<< /Size 3 0 R /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

/// PDF that is irrecoverably corrupt — no valid objects reachable, causing
/// the check to report errors → exit 2.
fn corrupt_pdf_bytes() -> Vec<u8> {
    b"%PDF-1.4\nthis is not a valid pdf at all\n%%EOF\n".to_vec()
}

/// A structurally valid single-page PDF whose page `/Contents 4 0 R` is a
/// `/FlateDecode` stream whose body is not valid zlib. The document opens
/// cleanly (correct xref/trailer), so the only error is the decode failure of
/// the content stream — `--check` reports it as an error → exit 2.
fn corrupt_content_stream_pdf_bytes() -> Vec<u8> {
    // The literal payload is deliberately not a zlib stream; its byte length is
    // written verbatim into `/Length` so the object parses but fails to decode.
    let payload: &[u8] = b"this is not valid zlib data at all";

    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );

    let off4 = pdf.len();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(payload);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// A valid page whose content stream has a stale direct `/Length`. Opening the
/// document is clean; resolving `/Contents` during `--check` performs the
/// recovery and must surface those lazy diagnostics as warnings.
fn recovered_content_stream_pdf_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(b"4 0 obj\n<< /Length 99 >>\nstream\nq\nQ\nendstream\nendobj\n");

    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// A structurally valid single-page PDF whose page `/Contents 4 0 R` is a
/// *valid* FlateDecode stream that inflates to `decoded_len` bytes (small
/// compressed, large inflated). The stream is intact, so `--check` reports
/// it clean regardless of size (default unlimited, matching qpdf).
fn bomb_content_stream_pdf_bytes(decoded_len: usize) -> Vec<u8> {
    let flate_dict = ObjectHandle::dictionary(vec![(
        b"/Filter".to_vec(),
        ObjectHandle::name(b"FlateDecode".to_vec()),
    )]);
    let encoded = flpdf::filters::encode_stream_data(&flate_dict, &vec![0u8; decoded_len]).unwrap();

    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
            encoded.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&encoded);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// Valid xref but the trailer lacks /Root — opens fine, check reports an
/// error-severity diagnostic → exit 2.
fn missing_root_pdf_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 3\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n")
            .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 3 >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// Clean PDF whose catalog declares an Adobe extension level
/// (`/Extensions /ADBE /ExtensionLevel 8`). qpdf appends `extension level 8` to
/// its `PDF Version:` banner.
fn extension_level_pdf_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n");
    let off1 = pdf.len();
    pdf.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions << /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >> >>\nendobj\n",
    );
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// The Catalog is valid at open time, but its indirect `/Extensions` value
/// emits a lazy recovery warning when the check summary reads the Adobe
/// extension level. The warning must be included before the final check
/// snapshot and therefore select qpdf's warning exit status 3.
fn late_extension_warning_pdf_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n");
    let off1 = pdf.len();
    pdf.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 4 0 R >>\nendobj\n",
    );
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    let off4 = pdf.len();
    // Deliberately omit `endobj`; resolving this object is the late warning.
    pdf.extend_from_slice(b"4 0 obj\n<< /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >>\n");
    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn linearized_with_parameter_replacement(marker: &[u8], replacement: &[u8]) -> Vec<u8> {
    assert_eq!(marker.len(), replacement.len());
    let mut bytes =
        include_bytes!("../../../tests/fixtures/compat/linearized-one-page.pdf").to_vec();
    let start = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("linearization parameter marker should exist");
    bytes[start..start + marker.len()].copy_from_slice(replacement);
    bytes
}

// ---------------------------------------------------------------------------
// Tests: exit 0 — clean PDF
// ---------------------------------------------------------------------------

#[test]
fn check_clean_pdf_exits_0() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&clean_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", f.path().to_str().unwrap()])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )))
        .stdout(predicate::str::contains("PDF check succeeded").not())
        .stderr(predicate::str::is_empty());
}

#[test]
fn check_subcommand_clean_pdf_exits_0() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&clean_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["check", f.path().to_str().unwrap()])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )))
        .stdout(predicate::str::contains("PDF check succeeded").not())
        .stderr(predicate::str::is_empty());
}

// ---------------------------------------------------------------------------
// Tests: qpdf-compatible stdout "checking" block
// ---------------------------------------------------------------------------

/// A clean plaintext PDF prints qpdf's full check block: the `checking <file>`
/// banner, header version, encryption + linearization status, and the trailing
/// reassurance note. qpdf hard-codes the program name in this note.
#[test]
fn check_clean_pdf_emits_qpdf_block() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&clean_pdf_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", &path])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(format!("checking {path}{EOL}")))
        .stdout(predicate::str::contains(format!("PDF Version: 1.4{EOL}")))
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )))
        .stdout(predicate::str::contains(format!(
            "File is not linearized{EOL}"
        )))
        .stdout(predicate::str::contains(format!(
            "No syntax or stream encoding errors found; the file may still contain{EOL}\
                 errors that qpdf cannot detect{EOL}"
        )))
        .stdout(predicate::str::contains("PDF check succeeded").not());
}

/// On exit 3 (warnings, no errors) the block is still printed, but qpdf omits
/// the trailing "No syntax ..." reassurance note (warnings go to stderr).
#[test]
fn check_warnings_emit_block_without_trailing_line() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", "--repair", f.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )))
        .stdout(predicate::str::contains(format!(
            "File is not linearized{EOL}"
        )))
        .stdout(predicate::str::contains("No syntax or stream encoding errors found").not())
        .stdout(predicate::str::contains("PDF check succeeded").not());
}

/// A catalog `/Extensions /ADBE /ExtensionLevel` is appended to the version
/// banner, matching qpdf (`PDF Version: 1.7 extension level 8`).
#[test]
fn check_appends_adobe_extension_level_to_version() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&extension_level_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", f.path().to_str().unwrap()])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(format!(
            "PDF Version: 1.7 extension level 8{EOL}"
        )));
}

#[test]
fn check_reports_late_extension_warning_and_exits_3() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&late_extension_warning_pdf_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", &path])
        .assert()
        .code(3)
        .stdout(predicate::str::contains(format!(
            "PDF Version: 1.7 extension level 8{EOL}"
        )))
        .stderr(predicate::str::contains("expected endobj"));
}

/// qpdf's repository fixture is a valid linearized document. `--check` reports
/// it as linearized without treating detection itself as a warning, so the run
/// exits 0 and includes the trailing reassurance note.
#[test]
fn check_linearized_pdf_reports_linearized_line() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(include_bytes!(
        "../../../tests/fixtures/compat/linearized-one-page.pdf"
    ))
    .unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", f.path().to_str().unwrap()])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(format!("File is linearized{EOL}")))
        .stdout(predicate::str::contains("File is not linearized").not())
        .stdout(predicate::str::contains(format!(
            "No syntax or stream encoding errors found; the file may still contain{EOL}\
                 errors that qpdf cannot detect{EOL}"
        )))
        .stderr(predicate::str::is_empty());
}

#[test]
fn check_linearized_o_mismatch_uses_qpdf_warning() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&linearized_with_parameter_replacement(
        b"/O 6 /E", b"/O 7 /E",
    ))
    .unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let output = cmd
        .env_remove("FLPDF_PROGNAME")
        .args(["--check", &path])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains(&format!("File is linearized{EOL}")));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "WARNING: {path}: first page object (/O) mismatch{EOL}\
             flpdf: operation succeeded with warnings{EOL}"
        )
    );
}

#[test]
fn check_linearized_n_mismatch_uses_qpdf_warning() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&linearized_with_parameter_replacement(
        b"/N 1 /T", b"/N 2 /T",
    ))
    .unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let output = cmd
        .env_remove("FLPDF_PROGNAME")
        .args(["--check", &path])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains(&format!("File is linearized{EOL}")));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "WARNING: {path}: error encountered while checking linearization data: \
             {path} (linearization hint table, offset 908): /N does not match number of pages{EOL}\
             flpdf: operation succeeded with warnings{EOL}"
        )
    );
}

#[test]
fn check_linearized_p_wrong_type_uses_qpdf_warning() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    // Keep the mutation length-preserving: replacing /E with a malformed /P
    // leaves the xref table valid while exercising qpdf's all-keys type gate.
    f.write_all(&linearized_with_parameter_replacement(
        b"/E 1198", b"/P /Bad",
    ))
    .unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let output = cmd
        .env_remove("FLPDF_PROGNAME")
        .args(["--check", &path])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains(&format!("File is linearized{EOL}")));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "WARNING: {path}: error encountered while checking linearization data: \
             {path} (linearization dictionary, offset 23): some keys in linearization dictionary are of the wrong type{EOL}\
             flpdf: operation succeeded with warnings{EOL}"
        )
    );
}

// ---------------------------------------------------------------------------
// Tests: exit 3 — warnings without errors (corrupt xref, repaired)
// ---------------------------------------------------------------------------

#[test]
fn check_warnings_only_pdf_exits_3() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    // --repair triggers the recovery heuristic; the resulting "xref repaired"
    // diagnostic is a warning (no error) → exit 3.
    cmd.args(["--check", "--repair", f.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )))
        .stderr(predicate::str::contains("WARNING: "));
}

#[test]
fn check_no_warn_suppresses_warning_delivery_but_keeps_exit_3() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--no-warn",
        "--check",
        "--repair",
        f.path().to_str().unwrap(),
    ])
    .assert()
    .code(3)
    .stdout(predicate::str::contains("checking "))
    .stderr(predicate::str::contains("WARNING:").not());
}

#[test]
fn check_subcommand_warnings_only_pdf_exits_3() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["check", "--repair", f.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )))
        .stderr(predicate::str::contains("WARNING: "));
}

#[test]
fn check_warnings_use_qpdf_stderr_format() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", "--repair", &path])
        .assert()
        .code(3)
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )))
        // qpdf shape: WARNING: <file>: <msg>, surrounding warnings without
        // offset, then the trailing summary line.
        .stderr(predicate::str::contains(format!(
            "WARNING: {path}: file is damaged{EOL}"
        )))
        .stderr(predicate::str::contains(format!(
            "Attempting to reconstruct cross-reference table{EOL}"
        )))
        .stderr(predicate::str::contains(format!(
            "flpdf: operation succeeded with warnings{EOL}"
        )))
        // The old lowercase `warning: <msg>` prefix must be gone.
        .stderr(predicate::str::contains("warning: ").not());
}

#[test]
fn check_with_repair_reports_size_mismatch_after_xref_reconstruction() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&recovered_indirect_size_header_mismatch_pdf_bytes())
        .unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--repair", f.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "reported number of objects (3) is not one plus the highest object number (4)",
        ));
}

#[test]
fn check_object_warning_uses_qpdf_space_before_object_context() {
    let input = "../../tests/fixtures/compat/chained-indirect-contents.pdf";

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", input])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(format!(
            "WARNING: {input} (object 5 0, offset 232): expected endobj{EOL}"
        )))
        .stderr(predicate::str::contains(
            format!(
                "WARNING: page object 3 0:  object is supposed to be a stream or an array of streams but is neither{EOL}"
            ),
        ))
        .stderr(
            predicate::str::contains(format!(
                "WARNING: {input}: page object 3 0: object is supposed to be a stream"
            ))
            .not(),
        )
        .stderr(
            predicate::str::contains(format!(
                "WARNING: {input}: (object 5 0, offset 232): expected endobj"
            ))
            .not(),
        )
        .stderr(
            predicate::str::contains(format!(
                "WARNING: {input} (offset 232) (object 5 0, offset 232): expected endobj"
            ))
            .not(),
        );
}

/// The trigger warning (and only the trigger warning) carries `(offset N)`.
#[test]
fn check_trigger_warning_carries_offset() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--repair", &path])
        .assert()
        .code(3)
        .stderr(
            predicate::str::is_match(format!(
                "WARNING: {} \\(offset \\d+\\): ",
                regex::escape(&path)
            ))
            .unwrap(),
        )
        .stderr(predicate::str::contains(format!("WARNING: {path} (offset")).count(1));
}

// ---------------------------------------------------------------------------
// Tests: exit 2 — errors / corrupt PDF
// ---------------------------------------------------------------------------

#[test]
fn check_corrupt_pdf_exits_2() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&corrupt_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", f.path().to_str().unwrap()])
        .assert()
        .code(2);
}

#[test]
fn check_terminal_open_failure_prints_repair_warnings_before_error_once() {
    let input = "../../tests/fixtures/test_driver/open_repair_failure.pdf";
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let output = cmd
        .env_remove("FLPDF_PROGNAME")
        .args(["--check", "--repair", input])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "WARNING: {input}: file is damaged{EOL}\
             WARNING: {input}: can't find startxref{EOL}\
             WARNING: {input}: Attempting to reconstruct cross-reference table{EOL}\
             flpdf: {input}: parse error at byte 0: unable to find trailer dictionary while recovering damaged file{EOL}"
        )
    );
}

/// A document that opens cleanly but whose page content stream fails to decode
/// is an error, not a warning: `--check` exits 2 and, because the run is not
/// valid, suppresses the trailing "No syntax or stream encoding errors found"
/// reassurance note (which is only printed on a clean exit-0 run). The decode
/// failure is reported on stderr, which also proves the exit-2 comes from the
/// content stream rather than a structural parse failure.
#[test]
fn check_corrupt_content_stream_exits_2_without_clean_note() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&corrupt_content_stream_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", f.path().to_str().unwrap()])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("No syntax or stream encoding errors found").not())
        .stderr(predicate::str::contains(
            "errors while decoding content stream",
        ));
}

#[test]
fn check_surfaces_lazy_content_stream_recovery_warnings() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&recovered_content_stream_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", f.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("No syntax or stream encoding errors found").not())
        .stderr(predicate::str::contains("expected endstream"))
        .stderr(predicate::str::contains(
            "attempting to recover stream length",
        ))
        .stderr(predicate::str::contains("recovered stream length"))
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings",
        ));
}

/// qpdf prints check errors as a single `<progname>: <file>: <msg>` line and
/// no extra "check failed" summary (observed with qpdf 11.9.0 on the same
/// fixture: `qpdf: noroot.pdf: unable to find /Root dictionary`).
#[test]
fn check_error_diagnostics_use_qpdf_stderr_format() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&missing_root_pdf_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", &path])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "flpdf: {path}: unable to find /Root dictionary{EOL}"
        )))
        .stderr(predicate::str::contains("PDF check failed").not())
        .stderr(predicate::str::contains("error: ").not())
        // exit 2 emits no stdout block at all: qpdf throws during document init
        // (missing /Root) before printing the `checking` banner, and flpdf
        // matches by gating the block on a valid report.
        .stdout(predicate::str::is_empty());
}

#[test]
fn rewrite_rejects_a_missing_root_before_creating_output() {
    let mut input = tempfile::NamedTempFile::new().unwrap();
    input.write_all(&missing_root_pdf_bytes()).unwrap();
    let output = tempfile::NamedTempFile::new().unwrap();
    let output_path = output.path().to_path_buf();
    drop(output);
    let input_path = input.path().to_str().unwrap().to_string();
    let output_path_string = output_path.to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args([&input_path, &output_path_string])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "flpdf: {input_path}: unable to find /Root dictionary"
        )))
        .stdout(predicate::str::is_empty());

    assert!(!output_path.exists());
}

/// Fatal open errors carry the input path: `<progname>: <file>: <msg>`
/// (observed qpdf shape: `qpdf: notpdf.pdf: unable to find trailer
/// dictionary while recovering damaged file`).
#[test]
fn fatal_open_error_includes_filename() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&corrupt_pdf_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", &path])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!("flpdf: {path}: ")));
}

/// FLPDF_PROGNAME swaps the program-name prefix (the qpdf qtest harness shim
/// sets FLPDF_PROGNAME=qpdf); diagnostics are otherwise identical.
#[test]
fn flpdf_progname_env_swaps_prefix() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env("FLPDF_PROGNAME", "qpdf")
        .args(["--check", "--repair", &path])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(format!(
            "qpdf: operation succeeded with warnings{EOL}"
        )))
        .stderr(predicate::str::contains("flpdf:").not());
}

/// An empty FLPDF_PROGNAME falls back to the default prefix instead of
/// rendering a broken `: <message>` line.
#[test]
fn flpdf_progname_empty_env_falls_back_to_default() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env("FLPDF_PROGNAME", "")
        .args(["--check", "--repair", &path])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(format!(
            "flpdf: operation succeeded with warnings{EOL}"
        )));
}

/// Same prefix swap on the fatal-open-error path, which is rendered by
/// main()'s result handler rather than run_check itself.
#[test]
fn flpdf_progname_env_swaps_prefix_on_fatal_error() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&corrupt_pdf_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env("FLPDF_PROGNAME", "qpdf")
        .args(["--check", &path])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!("qpdf: {path}: ")))
        .stderr(predicate::str::contains("flpdf:").not());
}

#[test]
fn check_subcommand_corrupt_pdf_exits_2() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&corrupt_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["check", f.path().to_str().unwrap()])
        .assert()
        .code(2);
}

/// Repair warnings emitted while opening for any subcommand (here: rewrite)
/// use the same qpdf shape as check.
#[test]
fn rewrite_repair_warnings_use_qpdf_stderr_format() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();
    let out = tempfile::NamedTempFile::new().unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["rewrite", "--repair", &path, out.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(format!(
            "WARNING: {path}: file is damaged{EOL}"
        )))
        .stderr(predicate::str::contains("warning: ").not())
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings; resulting file may have some problems",
        ));
}

// ---------------------------------------------------------------------------
// Tests: large-but-intact content streams are clean under `--check`
// ---------------------------------------------------------------------------

/// A large-but-intact content stream decodes fine: clean exit 0 (default
/// unlimited, matching qpdf; qpdf has no CLI decompression-bomb guard).
#[test]
fn check_no_limit_large_stream_exits_0() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&bomb_content_stream_pdf_bytes(64 * 1024))
        .unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", f.path().to_str().unwrap()])
        .assert()
        .code(0);
}

// ---------------------------------------------------------------------------
// Tests: weak-crypto files are inspectable by `--check` (read-only parity)
//
// qpdf treats `--check` as a read-only inspection, like `--show-encryption`,
// `--requires-password`, and `--is-encrypted`: an RC4 / R=5 file opened with the
// CORRECT password and NO `--allow-weak-crypto` is checked and exits 0 with no
// weak-crypto warning. Verified with qpdf 11.9.0:
//   qpdf --check --password=user-v2 tests/fixtures/encrypted/v2-rc4-128-r3.pdf
//   → exit 0, prints the check block + "No syntax or stream encoding errors
//     found", and emits NO weak-crypto warning on stderr.
// flpdf previously hit the weak-crypto gate here and exited 2.
// ---------------------------------------------------------------------------

/// RC4 (weak crypto) fixture inspectable with the correct user password.
const WEAK_RC4_FIXTURE: &str = "../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf";

#[test]
fn check_weak_rc4_correct_password_exits_0_as_inspection() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", "--password=user-v2", WEAK_RC4_FIXTURE])
        .assert()
        // exit 0: opened as a read-only inspection, no errors, no warnings.
        .code(0)
        // The trailing reassurance note is printed only on a clean exit-0 run,
        // so its presence proves both that the file opened AND that the
        // weak-crypto warning was suppressed (otherwise it would be exit 3).
        .stdout(predicate::str::contains(
            "No syntax or stream encoding errors found",
        ))
        // No weak-crypto error/warning surfaced anywhere — qpdf emits none.
        .stderr(predicate::str::contains("weak crypto").not());
}

#[test]
fn check_subcommand_weak_rc4_correct_password_exits_0() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["check", "--password=user-v2", WEAK_RC4_FIXTURE])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(
            "No syntax or stream encoding errors found",
        ))
        .stderr(predicate::str::contains("weak crypto").not());
}

/// Forcing the weak-crypto gate open for the inspection must NOT bypass
/// authentication: a wrong password still fails (exit 2), exactly as before.
#[test]
fn check_weak_rc4_wrong_password_still_exits_2() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", "--password=wrong", WEAK_RC4_FIXTURE])
        .assert()
        .code(2);
}
