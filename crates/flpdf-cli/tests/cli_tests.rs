use assert_cmd::Command;
use flpdf::{
    acroform_sig_flags, filespec_helper::encode_utf16be, pages, AnnotationObjectHelper,
    PageObjectHelper, Pdf,
};
use predicates::prelude::*;
use std::fs::File;
use std::io::BufReader;
use std::io::Write;
use std::process::Command as ProcessCommand;

mod common;
use common::PdfCanonicalTestExt;
use common::{first_widget_ref, page_annotation_handles};

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;

/// `true` when `needle` appears as a contiguous byte subslice of `hay`.
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

fn normalized_os_message(error: &std::io::Error) -> String {
    let message = error.to_string();
    error
        .raw_os_error()
        .and_then(|code| message.strip_suffix(&format!(" (os error {code})")))
        .unwrap_or(&message)
        .to_owned()
}

fn json_qpdf_metadata(json: &serde_json::Value) -> &serde_json::Value {
    &json["qpdf"][0]
}

#[test]
fn check_valid_fixture_exits_successfully() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )));
}

#[test]
fn check_accepts_ignore_xref_streams_on_a_clean_pdf() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--ignore-xref-streams",
            "--check",
            "../../tests/fixtures/minimal.pdf",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )));
}

#[test]
fn top_level_object_streams_modes_reach_the_writer() {
    let temp = tempfile::tempdir().unwrap();
    let input = "../../tests/fixtures/compat/one-page.pdf";

    for mode in ["preserve", "disable", "generate"] {
        let output = temp.path().join(format!("object-streams-{mode}.pdf"));
        let flag = format!("--object-streams={mode}");
        Command::cargo_bin("flpdf")
            .unwrap()
            .args([
                "--static-id",
                flag.as_str(),
                input,
                output.to_str().unwrap(),
            ])
            .assert()
            .success();

        let rendered = std::fs::read(&output).unwrap();
        let has_objstm = contains(&rendered, b"/Type /ObjStm");
        assert_eq!(
            has_objstm,
            mode == "generate",
            "top-level --object-streams={mode} produced unexpected ObjStm shape"
        );
    }

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "-object-streams=generate",
            "--static-id",
            input,
            temp.path()
                .join("object-streams-single-dash.pdf")
                .to_str()
                .unwrap(),
        ])
        .assert()
        .success();
}

#[test]
fn top_level_writer_mode_help_matches_qpdf_terms() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--object-streams"))
        .stdout(predicate::str::contains("preserve"))
        .stdout(predicate::str::contains("generate"))
        .stdout(predicate::str::contains("--stream-data"))
        .stdout(predicate::str::contains("uncompress"));
}

#[test]
fn suppress_recovery_matches_qpdf_on_a_recoverable_xref_error() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--suppress-recovery",
            "--check",
            "../../tests/fixtures/test_driver/repairable_input.pdf",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parse error"))
        .stderr(predicate::str::contains("Attempting to reconstruct").not());
}

#[test]
fn repair_conflicts_with_suppress_recovery() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--repair",
            "--suppress-recovery",
            "--check",
            "../../tests/fixtures/minimal.pdf",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn suppress_recovery_applies_to_an_overlay_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("overlay-corrupt.pdf");
    let output = temp.path().join("overlay.pdf");
    std::fs::write(&source, corrupt_xref_with_info_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--suppress-recovery",
            "--overlay",
            source.to_str().unwrap(),
            "--",
            "../../tests/fixtures/compat/one-page.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parse error"))
        .stderr(predicate::str::contains("Attempting to reconstruct").not());
}

#[test]
fn ignore_xref_streams_applies_to_an_overlay_source() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("overlay.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--ignore-xref-streams",
            "--overlay",
            "../../tests/fixtures/compat/three-page-objstm.pdf",
            "--",
            "../../tests/fixtures/compat/one-page.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parse error"));
}

#[test]
fn ignore_xref_streams_applies_to_a_copy_attachments_donor() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("copy-attachments.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--ignore-xref-streams",
            "../../tests/fixtures/minimal.pdf",
            "--copy-attachments-from",
            "../../tests/fixtures/compat/three-page-objstm.pdf",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parse error"));
}

#[test]
fn ignore_xref_streams_applies_to_a_copy_encryption_donor() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("copy-encryption.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--ignore-xref-streams",
            "--copy-encryption=../../tests/fixtures/compat/three-page-objstm.pdf",
            "--encryption-file-password=",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parse error"));
}

#[test]
fn suppress_recovery_applies_to_a_copy_attachments_donor() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("copy-attachments.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--suppress-recovery",
            "../../tests/fixtures/minimal.pdf",
            "--copy-attachments-from",
            "../../tests/fixtures/test_driver/missing_startxref.pdf",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parse error"))
        .stderr(predicate::str::contains("Attempting to reconstruct").not());
}

#[test]
fn suppress_recovery_applies_to_a_copy_encryption_donor() {
    let temp = tempfile::tempdir().unwrap();
    let donor = temp.path().join("encrypted-corrupt.pdf");
    let output = temp.path().join("copy-encryption.pdf");
    std::fs::write(
        &donor,
        corrupt_startxref(include_bytes!(
            "../../../tests/fixtures/compat/encrypted-r4-three-page.pdf"
        )),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--suppress-recovery",
            &format!("--copy-encryption={}", donor.display()),
            "--encryption-file-password=",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parse error"))
        .stderr(predicate::str::contains("Attempting to reconstruct").not());
}

#[test]
fn is_encrypted_applies_suppressed_recovery() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "is-encrypted",
            "--suppress-recovery",
            "../../tests/fixtures/test_driver/missing_startxref.pdf",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parse error"))
        .stderr(predicate::str::contains("Attempting to reconstruct").not());
}

#[test]
fn is_encrypted_applies_ignore_xref_streams() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "is-encrypted",
            "--ignore-xref-streams",
            "../../tests/fixtures/compat/three-page-objstm.pdf",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parse error"));
}

#[test]
fn recovery_help_text_matches_qpdf_wording() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ignore any cross-reference streams in the file",
        ))
        .stdout(predicate::str::contains(
            "Avoid attempting to recover when errors are found",
        ));
}

#[test]
fn check_accepts_qpdf_bare_flag_with_discarded_equals_value() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check=ignored", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )));
}

#[test]
fn top_level_single_dash_qdf_reaches_qdf_writer() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("single-dash-qdf.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "-qdf",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(std::fs::read(&output).unwrap().starts_with(b"%PDF-"));
    assert!(std::fs::read(&output)
        .unwrap()
        .windows(b"%QDF-1.0".len())
        .any(|window| window == b"%QDF-1.0"));
}

#[test]
fn overlay_segment_preserves_trailing_top_level_flag() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("overlay-trailing-flag.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "../../tests/fixtures/compat/one-page.pdf",
            output.to_str().unwrap(),
            "--overlay",
            "../../tests/fixtures/compat/one-page.pdf",
            "--",
            "--static-id",
        ])
        .assert()
        .success();

    assert!(output.exists());
}

#[test]
fn check_encrypted_fixture_accepts_correct_empty_password_flag() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--check",
        "--password=",
        "../../tests/fixtures/compat/encrypted-r4-three-page.pdf",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains(format!("R = 4{EOL}")));
}

#[test]
fn check_encrypted_fixture_rejects_wrong_password() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--check",
        "--password=wrong",
        "../../tests/fixtures/compat/encrypted-r4-three-page.pdf",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("invalid password"));
}

#[test]
fn check_inspects_rc4_encrypted_input_by_default() {
    // qpdf treats `--check` as a read-only inspection: an RC4 (weak-crypto) file
    // opened with the correct password is checked and exits 0 WITHOUT
    // `--allow-weak-crypto` and with no weak-crypto warning (verified qpdf
    // 11.9.0). flpdf previously hit the weak-crypto gate and exited 2 here.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("rc4.pdf");
    std::fs::write(&input, encrypted_v1_owner_password_fixture()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--password=owner"])
        .arg(&input)
        .assert()
        .code(0)
        .stdout(predicate::str::contains(format!("R = 2{EOL}")))
        .stderr(predicate::str::contains("weak crypto").not());
}

#[test]
fn check_rc4_with_allow_weak_crypto_still_clean_no_warning() {
    // `--allow-weak-crypto` makes no difference to `--check`: qpdf emits no
    // weak-crypto warning for the inspection regardless of the flag (verified
    // qpdf 11.9.0, exit 0 with and without it). So the flag neither downgrades
    // to exit 3 nor adds a warning.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("rc4.pdf");
    std::fs::write(&input, encrypted_v1_owner_password_fixture()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--allow-weak-crypto", "--password=owner"])
        .arg(&input)
        .assert()
        .code(0)
        .stdout(predicate::str::contains(format!("R = 2{EOL}")))
        .stderr(predicate::str::contains("weak crypto").not());
}

#[test]
fn check_repair_encrypted_fixture_rejects_wrong_password_actionably() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--check",
        "--repair",
        "--password=wrong",
        "../../tests/fixtures/compat/encrypted-r4-three-page.pdf",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("invalid password"));
}

#[test]
fn rewrite_encrypted_fixture_preserves_encryption_by_default() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("--password=")
        .arg("../../tests/fixtures/compat/encrypted-r4-three-page.pdf")
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    let mut check = Command::cargo_bin("flpdf").unwrap();
    check
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("R = 4{EOL}")));
}

#[test]
fn check_encrypted_fixture_uses_empty_default_password() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--check",
        "../../tests/fixtures/compat/encrypted-r4-three-page.pdf",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains(format!("R = 4{EOL}")));
}

#[test]
fn check_encrypted_fixture_reads_password_file_and_strips_newline() {
    let temp = tempfile::tempdir().unwrap();
    let password_file = temp.path().join("password.txt");
    std::fs::write(&password_file, b"\r\n").unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check"])
        .arg(format!("--password-file={}", password_file.display()))
        .arg("../../tests/fixtures/compat/encrypted-r4-three-page.pdf")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("R = 4{EOL}")));
}

#[test]
fn password_and_password_file_are_mutually_exclusive() {
    let temp = tempfile::tempdir().unwrap();
    let password_file = temp.path().join("password.txt");
    std::fs::write(&password_file, b"").unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--password="])
        .arg(format!("--password-file={}", password_file.display()))
        .arg("../../tests/fixtures/compat/encrypted-r4-three-page.pdf")
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn rewrite_fixture_creates_output() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("../../tests/fixtures/minimal.pdf")
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

#[test]
fn rewrite_remove_restrictions_strips_signatures_without_warning() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("signed.pdf");
    let output = temp.path().join("unsigned.pdf");
    std::fs::write(&input, signed_acroform_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--remove-restrictions"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicate::str::contains("signatures are now invalidated").not());

    let file = File::open(&output).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
    assert!(
        pdf.signatures().unwrap().is_empty(),
        "--remove-restrictions output must not report signed fields"
    );
    // qpdf disableDigitalSignatures erases the /Sig field from the top-level
    // /Fields array; the field-only object here is then garbage-collected.
    assert_eq!(
        acroform_fields_len(&mut pdf),
        Some(0),
        "AcroForm must survive with an emptied /Fields array"
    );

    let output_bytes = std::fs::read(&output).unwrap();
    assert!(
        !contains(&output_bytes, b"/V "),
        "signature field /V entries must be removed"
    );
    assert!(
        !contains(&output_bytes, b"/FT"),
        "signature field /FT entries must be removed (field object GC'd)"
    );
    assert!(
        !contains(&output_bytes, b"/ByteRange"),
        "orphaned signature dictionaries must be removed"
    );
}

#[test]
fn copy_attachments_remove_restrictions_is_silent_for_signatures() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("signed.pdf");
    let output = temp.path().join("copied.pdf");
    let donor = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    std::fs::write(&input, signed_acroform_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--remove-restrictions", "--copy-attachments-from"])
        .arg(&donor)
        .arg("--")
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicate::str::contains("removed restrictions").not())
        .stderr(predicate::str::contains("removed signatures").not());

    assert!(output.exists());
    let file = File::open(&output).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    assert!(
        pdf.signatures().unwrap().is_empty(),
        "copy route must strip signatures under --remove-restrictions"
    );
}

#[test]
fn rewrite_linearize_remove_restrictions_strips_signatures_without_warning() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("signed.pdf");
    let output = temp.path().join("unsigned-linearized.pdf");
    std::fs::write(&input, signed_acroform_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--linearize", "--remove-restrictions"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicate::str::contains("signatures are now invalidated").not());

    let file = File::open(&output).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
    assert!(
        pdf.signatures().unwrap().is_empty(),
        "linearized --remove-restrictions output must not report signed fields"
    );

    let output_bytes = std::fs::read(&output).unwrap();
    assert!(
        !contains(&output_bytes, b"/V "),
        "linearized signature field /V entries must be removed"
    );
    assert!(
        !contains(&output_bytes, b"/ByteRange"),
        "linearized output must remove orphaned signature dictionaries"
    );
}

#[test]
fn rewrite_linearize_remove_restrictions_strips_docmdp_perms() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("docmdp.pdf");
    let output = temp.path().join("out-linearized.pdf");
    std::fs::write(&input, signed_perms_docmdp_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--linearize", "--remove-restrictions"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicate::str::contains("signatures are now invalidated").not());

    let output_bytes = std::fs::read(&output).unwrap();
    for tok in [&b"/Perms"[..], &b"/DocMDP"[..], &b"/ByteRange"[..]] {
        assert!(
            !contains(&output_bytes, tok),
            "linearized --remove-restrictions must drop {:?}",
            std::str::from_utf8(tok).unwrap()
        );
    }
    let file = File::open(&output).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    assert!(pdf.signatures().unwrap().is_empty());
}

#[test]
fn rewrite_linearize_remove_restrictions_keeps_widget_annotation() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("widget.pdf");
    let output = temp.path().join("out-linearized.pdf");
    std::fs::write(&input, signed_widget_acroform_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--linearize", "--remove-restrictions"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicate::str::contains("signatures are now invalidated").not());

    let output_bytes = std::fs::read(&output).unwrap();
    // Widget annotation survives (reachable from page /Annots) ...
    assert!(contains(&output_bytes, b"/Widget"));
    // ... but its signature keys are gone.
    assert!(!contains(&output_bytes, b"/ByteRange"));

    let file = File::open(&output).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
    assert_eq!(
        acroform_fields_len(&mut pdf),
        Some(0),
        "top-level /Fields must be emptied"
    );
    assert!(pdf.signatures().unwrap().is_empty());
}

#[test]
fn rewrite_remove_restrictions_strips_docmdp_perms_without_warning() {
    // A certification (DocMDP) signature can live only in the catalog /Perms
    // dictionary, with no /AcroForm. qpdf --remove-restrictions drops /Perms
    // unconditionally (QPDF::removeSecurityRestrictions), which orphans the
    // signature dictionary. This exercises the catalog-/Perms detection branch.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("certified.pdf");
    let output = temp.path().join("uncertified.pdf");
    std::fs::write(&input, signed_perms_docmdp_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--remove-restrictions"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicate::str::contains("signatures are now invalidated").not());

    let file = File::open(&output).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    assert!(
        pdf.signatures().unwrap().is_empty(),
        "DocMDP --remove-restrictions output must not report signed fields"
    );

    let output_bytes = std::fs::read(&output).unwrap();
    assert!(
        !contains(&output_bytes, b"/Perms"),
        "catalog /Perms must be removed"
    );
    assert!(
        !contains(&output_bytes, b"/DocMDP"),
        "the /DocMDP dictionary must be removed with /Perms"
    );
    assert!(
        !contains(&output_bytes, b"/ByteRange"),
        "the orphaned DocMDP signature dictionary must be garbage-collected"
    );
}

#[test]
fn rewrite_remove_restrictions_keeps_widget_annotation() {
    // When the signature field doubles as a page /Widget annotation, qpdf
    // disableDigitalSignatures erases it from /AcroForm /Fields and strips
    // /FT /V, but the object survives because the page /Annots still references
    // it. The now-orphaned signature dictionary is garbage-collected.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("signed-widget.pdf");
    let output = temp.path().join("unsigned-widget.pdf");
    std::fs::write(&input, signed_widget_acroform_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--remove-restrictions"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicate::str::contains("signatures are now invalidated").not());

    let file = File::open(&output).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
    assert!(
        pdf.signatures().unwrap().is_empty(),
        "widget --remove-restrictions output must not report signed fields"
    );
    assert_eq!(
        acroform_fields_len(&mut pdf),
        Some(0),
        "AcroForm must survive with an emptied /Fields array"
    );

    let output_bytes = std::fs::read(&output).unwrap();
    // The widget annotation survives (still referenced from the page /Annots)...
    assert!(
        contains(&output_bytes, b"/Subtype /Widget"),
        "the widget annotation must survive as a page annotation"
    );
    assert!(
        contains(&output_bytes, b"/T (Approval)"),
        "the surviving widget must keep its /T field name"
    );
    // ...but its signature identity is stripped.
    assert!(
        !contains(&output_bytes, b"/FT"),
        "the surviving widget must lose its /FT (signature field type)"
    );
    assert!(
        !contains(&output_bytes, b"/ByteRange"),
        "the orphaned signature dictionary must be garbage-collected"
    );
}

#[test]
fn rewrite_default_is_qpdf_equivalent_full_rewrite() {
    // A plain `flpdf rewrite IN OUT` (no flags)
    // must match qpdf's documented defaults — qpdf full-rewrites and applies
    // --compress-streams=y by default. This asserts that the deliberate
    // default behavior (fresh rewrite + FlateDecode compression) holds, so a
    // regression to a verbatim no-op default would be caught.
    let temp = tempfile::tempdir().unwrap();
    let default_out = temp.path().join("default.pdf");
    let nocomp_out = temp.path().join("nocomp.pdf");
    let input = "../../tests/fixtures/compat/one-page.pdf";

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", input, default_out.to_str().unwrap()])
        .assert()
        .success();
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--compress-streams=n",
            input,
            nocomp_out.to_str().unwrap(),
        ])
        .assert()
        .success();

    let default_bytes = std::fs::read(&default_out).unwrap();
    let nocomp_bytes = std::fs::read(&nocomp_out).unwrap();
    let input_bytes = std::fs::read(input).unwrap();

    // Default output is a real full rewrite (not a verbatim copy of input).
    assert_ne!(
        default_bytes, input_bytes,
        "default rewrite must full-rewrite, not copy the source verbatim"
    );
    // Default applies FlateDecode compression (qpdf default = compress=y),
    // whereas --compress-streams=n does not.
    let has_flate = |b: &[u8]| b.windows(11).any(|w| w == b"FlateDecode");
    assert!(
        has_flate(&default_bytes),
        "default rewrite must FlateDecode-compress streams (qpdf-equivalent default)"
    );
    // The default (compress=y) and explicit --compress-streams=n outputs
    // must differ: this proves the qpdf-equivalent compression default is
    // actually applied, not silently ignored. (A byte-size comparison is
    // unreliable on tiny fixtures where the zlib/header overhead can exceed
    // the savings, so we assert on behavior, not size.)
    assert_ne!(
        default_bytes, nocomp_bytes,
        "default rewrite (compress=y) must differ from --compress-streams=n output"
    );
}

#[test]
fn invalid_compression_level_retries_each_stream_like_qpdf() {
    let temp = tempfile::tempdir().unwrap();
    let input = "../../tests/fixtures/compat/one-page.pdf";
    let qpdf_output = temp.path().join("qpdf-invalid-level.pdf");
    let flpdf_output = temp.path().join("flpdf-invalid-level.pdf");

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--recompress-flate", "--compression-level=10", input])
        .arg(&qpdf_output)
        .output()
        .expect("qpdf 11.9.0 must be available");
    assert_eq!(qpdf.status.code(), Some(3));

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--recompress-flate", "--compression-level=10", input])
        .arg(&flpdf_output)
        .output()
        .unwrap();
    assert_eq!(flpdf.status.code(), Some(3));
    assert!(
        contains(
            &qpdf.stderr,
            b"stream will be re-processed without filtering"
        ) && contains(
            &flpdf.stderr,
            b"stream will be re-processed without filtering"
        ),
        "both writers must report the per-stream raw retry"
    );
    assert!(
        flpdf_output.exists(),
        "the recovered output must be retained"
    );

    let check = ProcessCommand::new("qpdf")
        .args(["--check"])
        .arg(&flpdf_output)
        .output()
        .expect("qpdf --check");
    assert!(
        check.status.success(),
        "the per-stream fallback must leave a valid PDF: {}",
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn rewrite_repaired_fixture_with_repair_flag() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_pdf()).unwrap();

    let output = temp.path().join("out.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--repair",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ])
    .assert()
    .code(3)
    .stderr(predicate::str::contains(
        "flpdf: operation succeeded with warnings; resulting file may have some problems",
    ));

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

#[test]
fn check_subcommand_succeeds() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["check", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )));
}

#[test]
fn pages_subcommand_prints_each_page() {
    let fixture = fixture_with_nested_pages();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["pages", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("page 1: 3 0 R"))
        .stdout(predicate::str::contains("page 2: 6 0 R"));
}

#[test]
fn pages_subcommand_prints_count() {
    let fixture = fixture_with_nested_pages();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["pages", "--show-npages", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("2"));
}

#[test]
fn dump_object_subcommand_accepts_ref() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["dump-object", "1 0", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/Type /Catalog"));
}

#[test]
fn qdf_subcommand_rewrites_output() {
    // The `qdf` subcommand is an alias of `rewrite --qdf`: it must
    // emit canonical QDF, not the
    // legacy raw-dump route.
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "qdf",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    assert!(std::fs::metadata(&output).unwrap().len() > 0);

    let rendered = std::fs::read(&output).unwrap();
    let has = |needle: &[u8]| rendered.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"%QDF-1.0"), "expected %QDF-1.0 header marker");
    assert!(
        has(b"%% Original object ID:"),
        "expected %% Original object ID: comments"
    );
    assert!(has(b"\nxref\n"), "expected a classic `xref` table");
    assert!(!has(b"/Type /XRef"), "QDF must not use an xref stream");
    assert!(!has(b"/Type /ObjStm"), "QDF must not use object streams");
}

#[test]
fn qdf_repaired_input_keeps_output_and_exits_three_with_output_summary() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("repaired.qdf.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "qdf",
            "--repair",
            "../../tests/fixtures/test_driver/repairable_input.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings; resulting file may have some problems",
        ));

    assert!(output.exists(), "warning exit must retain qdf output");
}

#[test]
fn qdf_adjacent_endstream_with_indirect_length_is_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("good14-shaped-qdf.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--qdf",
            "../../tests/fixtures/compat/good14-shaped-indirect-length-adjacent-endstream.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .code(0)
        .stderr(predicate::str::is_empty());
    assert!(output.exists());
}

#[test]
fn qdf_subcommand_dumps_all_reachable_objects() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fixture_with_orphan_object();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "qdf",
        fixture.path().to_str().unwrap(),
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    // The QDF output contains a binary header marker (non-UTF-8 bytes), so we
    // read raw bytes and search for target substrings as bytes. Canonical QDF
    // (the new `qdf` == `rewrite --qdf` behavior) renumbers objects Catalog-first
    // and emits only those reachable from the trailer `/Root`+`/Info` seed.
    // The fixture's `/Type /Orphan` object is unreferenced, so it is dropped —
    // matching qpdf's default (`qpdf --qdf` drops unreferenced objects unless
    // `--preserve-unreferenced` is given). We therefore assert by object *type*
    // (object numbers are unstable under renumber), not by hardcoded `N 0 obj`.
    let rendered = std::fs::read(&output).unwrap();
    let has = |needle: &[u8]| rendered.windows(needle.len()).any(|w| w == needle);
    // Every object reachable from /Root is dumped.
    assert!(
        has(b"/Type /Catalog"),
        "expected the reachable Catalog object"
    );
    assert!(has(b"/Type /Pages"), "expected the reachable Pages object");
    assert!(has(b"/Type /Page"), "expected the reachable Page object");
    // The unreferenced orphan is dropped (qpdf-consistent, no
    // --preserve-unreferenced). This assertion is the test's teeth: it fails if
    // the renumber stops dropping unreachable objects.
    assert!(
        !has(b"/Type /Orphan"),
        "unreferenced orphan object must be dropped (qpdf-consistent)"
    );
    assert!(has(b"%QDF-1.0"), "expected %QDF-1.0 header marker");
    assert!(
        has(b"%% Original object ID:"),
        "expected %% Original object ID: comments"
    );
    assert!(has(b"\nxref\n"), "expected a classic `xref` table");
    assert!(!has(b"/Type /XRef"), "QDF must not use an xref stream");
    assert!(!has(b"/Type /ObjStm"), "QDF must not use object streams");
}

#[test]
fn preserve_unreferenced_retains_orphan_across_writer_cli_surfaces() {
    let fixture = fixture_with_orphan_object();
    let temp = tempfile::tempdir().unwrap();
    let input = fixture.path();

    for (name, args) in [
        ("rewrite", vec!["rewrite", "--preserve-unreferenced"]),
        (
            "rewrite-disable",
            vec![
                "rewrite",
                "--preserve-unreferenced",
                "--object-streams=disable",
            ],
        ),
        (
            "rewrite-generate",
            vec![
                "rewrite",
                "--preserve-unreferenced",
                "--object-streams=generate",
            ],
        ),
        (
            "rewrite-qdf-generate",
            vec![
                "rewrite",
                "--qdf",
                "--preserve-unreferenced",
                "--object-streams=generate",
            ],
        ),
        ("qdf", vec!["qdf", "--preserve-unreferenced"]),
        ("top-level", vec!["--preserve-unreferenced"]),
    ] {
        let output = temp.path().join(format!("{name}.pdf"));
        Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(input)
            .arg(&output)
            .assert()
            .success();
        let rendered = std::fs::read(&output).unwrap();
        assert!(
            contains(&rendered, b"/Type /Orphan"),
            "{name} preserve-unreferenced route must retain the orphan object"
        );
    }

    let qpdf_output = temp.path().join("qpdf-qdf.pdf");
    let qpdf_status = std::process::Command::new("qpdf")
        .args(["--qdf", "--preserve-unreferenced", "--static-id"])
        .arg(input)
        .arg(&qpdf_output)
        .status()
        .expect("qpdf 11.9.0 must be available for this differential test");
    assert!(
        qpdf_status.success(),
        "qpdf preserve-unreferenced probe failed"
    );
    let qpdf_rendered = std::fs::read(qpdf_output).unwrap();
    assert_eq!(
        contains(&qpdf_rendered, b"/Type /Orphan"),
        contains(
            &std::fs::read(temp.path().join("qdf.pdf")).unwrap(),
            b"/Type /Orphan"
        ),
        "flpdf qdf preserve-unreferenced must match qpdf's orphan reachability"
    );
}

#[test]
fn preserve_pages_retains_qpdf_promoted_deselected_inheritable_object() {
    let fixture = fixture_with_nested_pages();
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf-pages-preserve.qdf.pdf");
    let flpdf_output = temp.path().join("flpdf-pages-preserve.qdf.pdf");

    let qpdf_status = std::process::Command::new("qpdf")
        .args([
            "--qdf",
            "--object-streams=disable",
            "--preserve-unreferenced",
            "--static-id",
        ])
        .arg(fixture.path())
        .args(["--pages", fixture.path().to_str().unwrap(), "2", "--"])
        .arg(&qpdf_output)
        .status()
        .expect("qpdf 11.9.0 must be available for this differential test");
    assert!(qpdf_status.success(), "qpdf page-selection probe failed");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--qdf",
            "--object-streams=disable",
            "--preserve-unreferenced",
        ])
        .arg(fixture.path())
        .args(["--pages", ".", "2", "--"])
        .arg(&flpdf_output)
        .assert()
        .success();

    // The input root carries a direct [0 0 595.28 841.89] /MediaBox while the
    // selected page has its own /MediaBox. qpdf promotes the root value before
    // flattening removes it; preserve-unreferenced then retains that orphan as
    // a standalone array object.
    let promoted_media_box = b"[\n  0\n  0\n  595.28\n  841.89\n]";
    let qpdf_rendered = std::fs::read(&qpdf_output).unwrap();
    let qpdf_has_promoted = contains(&qpdf_rendered, promoted_media_box);
    assert!(
        qpdf_has_promoted,
        "qpdf 11.9.0 probe must retain the promoted orphan inheritable object"
    );
    assert_eq!(
        contains(&std::fs::read(&flpdf_output).unwrap(), promoted_media_box),
        qpdf_has_promoted,
        "preserve page selection must retain qpdf's promoted orphan inheritable object"
    );
}

#[test]
fn preserve_unreferenced_remove_attachment_matches_qpdf() {
    let input = "../../tests/fixtures/compat/attachment-two-page.pdf";
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf-remove-preserve.pdf");
    let flpdf_output = temp.path().join("flpdf-remove-preserve.pdf");

    let qpdf_status = std::process::Command::new("qpdf")
        .args([
            "--remove-attachment=attachment.txt",
            "--preserve-unreferenced",
            "--static-id",
        ])
        .arg(input)
        .arg(&qpdf_output)
        .status()
        .expect("qpdf 11.9.0 must be available for this differential test");
    assert!(qpdf_status.success(), "qpdf attachment removal failed");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input,
            "--remove-attachment=attachment.txt",
            "--preserve-unreferenced",
        ])
        .arg(&flpdf_output)
        .assert()
        .success();

    let xref_count = |path: &std::path::Path| {
        let output = std::process::Command::new("qpdf")
            .args(["--show-xref"])
            .arg(path)
            .output()
            .expect("qpdf --show-xref");
        assert!(output.status.success(), "qpdf --show-xref failed");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    };

    assert_eq!(
        xref_count(&flpdf_output),
        xref_count(&qpdf_output),
        "preserve-unreferenced attachment removal must retain qpdf's detached objects"
    );
}

#[test]
fn preserve_unreferenced_is_accepted_for_inspection_only_check() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--preserve-unreferenced",
            "--check",
            "../../tests/fixtures/minimal.pdf",
        ])
        .assert()
        .success();
}

#[test]
fn preserve_unreferenced_linearize_matches_qpdf_reachability() {
    let fixture = fixture_with_orphan_object();
    let input = fixture.path();
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf-linearized.pdf");
    let flpdf_output = temp.path().join("flpdf-linearized.pdf");

    let qpdf_status = std::process::Command::new("qpdf")
        .args(["--linearize", "--preserve-unreferenced", "--static-id"])
        .arg(input)
        .arg(&qpdf_output)
        .status()
        .expect("qpdf 11.9.0 must be available for this differential test");
    assert!(qpdf_status.success(), "qpdf linearize probe failed");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--preserve-unreferenced", "--linearize", "--static-id"])
        .arg(input)
        .arg(&flpdf_output)
        .assert()
        .success();

    let xref_count = |path: &std::path::Path| {
        let output = std::process::Command::new("qpdf")
            .args(["--show-xref"])
            .arg(path)
            .output()
            .expect("qpdf --show-xref");
        assert!(output.status.success(), "qpdf --show-xref failed");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    };

    assert!(
        !contains(&std::fs::read(&qpdf_output).unwrap(), b"/Type /Orphan"),
        "qpdf linearize does not preserve unreachable objects"
    );
    assert!(
        !contains(&std::fs::read(&flpdf_output).unwrap(), b"/Type /Orphan"),
        "flpdf linearize must match qpdf's reachability policy"
    );
    assert_eq!(
        xref_count(&flpdf_output),
        xref_count(&qpdf_output),
        "linearized preserve-unreferenced must match qpdf's object set"
    );
}

#[test]
fn rewrite_subcommand_rewrites_output() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

// ---------------------------------------------------------------------------
// qpdf-style top-level flat flags
//
// These exist so the qpdf qtest harness (which PATH-shims
// `qpdf` → `flpdf` with no arg translation) can drive flpdf with the
// commands its `.test` files already use. The behaviour mirrors the
// equivalent `flpdf rewrite ...` subcommand invocation.
// ---------------------------------------------------------------------------

/// Build a single-page PDF in memory.  Same shape as the helper in
/// cli_linearize.rs; duplicated here to keep this test self-contained
/// without re-exporting test helpers between integration test crates.
fn one_page_pdf_bytes() -> Vec<u8> {
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
        "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
    );
    pdf.extend_from_slice(xref.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// Build a one-page tree with an intermediate `/Pages` node carrying an
/// unrecognized key. qpdf's flatten path warns before discarding that node;
/// the retained root's own unknown keys are intentionally not part of this
/// fixture.
fn one_page_pdf_with_unknown_intermediate_pages_key() -> Vec<u8> {
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 /UserUnit 2 >>\nendobj\n",
        b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    ])
}

#[test]
fn top_level_linearize_rewrites_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--linearize", "--static-id"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

fn first_page_content(path: &std::path::Path) -> Vec<u8> {
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let page = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
    flpdf::pages::page_content_bytes(&mut pdf, page).unwrap()
}

fn first_page_content_filter(path: &std::path::Path) -> Option<Vec<u8>> {
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let page_ref = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
    let page = pdf.resolve_canonical_object(page_ref).unwrap().clone();
    let contents_ref = page.try_get_key(b"/Contents").ok()?.object_ref()?;
    let stream = pdf.resolve_canonical_object(contents_ref).ok()?;
    stream
        .as_stream_dict()?
        .try_get_key(b"/Filter")
        .ok()?
        .as_name()
}

#[test]
fn top_level_normalize_content_y_routes_to_content_normalizer() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("crlf-content.pdf");
    let output = temp.path().join("normalized.pdf");
    std::fs::write(&input, one_page_pdf_with_content(b"q\rQ")).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--normalize-content=y")
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert_eq!(first_page_content(&output), b"q\nQ");
    assert_eq!(first_page_content_filter(&output), None);
}

#[test]
fn top_level_linearize_normalize_content_y_mutates_before_planning() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("crlf-content.pdf");
    let output = temp.path().join("normalized-linearized.pdf");
    // CRLF collapses to LF, changing the stream length. If only the write
    // graph is normalized while the planning graph is left untouched, the
    // linearization offsets/hints are computed for the wrong object size.
    std::fs::write(&input, one_page_pdf_with_content(b"q\r\nQ")).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--linearize", "--normalize-content=y"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert_eq!(first_page_content(&output), b"q\nQ");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("check-linearization")
        .arg(&output)
        .assert()
        .success();
}

#[test]
fn top_level_linearize_normalize_content_preserves_warning_exit() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("bad-content.pdf");
    let output = temp.path().join("normalized-linearized.pdf");
    let input_bytes = one_page_pdf_with_content(b"\r<0g");
    let offset = stream_data_offset(&input_bytes);
    std::fs::write(&input, &input_bytes).unwrap();

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--linearize", "--normalize-content=y"])
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    assert!(output.exists(), "qpdf warning exit must retain output");
    assert_eq!(first_page_content(&output), b"\n<0g");
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        format!(
            "WARNING: {} (offset {offset}): content normalization encountered bad tokens{EOL}\
             WARNING: {} (offset {offset}): normalized content ended with a bad token; you may be able to resolve this by coalescing content streams in combination with normalizing content. From the command line, specify --coalesce-contents{EOL}\
             WARNING: {} (offset {offset}): Resulting stream data may be corrupted but is may still useful for manual inspection. For more information on this warning, search for content normalization in the manual.{EOL}\
             flpdf: operation succeeded with warnings; resulting file may have some problems{EOL}",
            input.display(),
            input.display(),
            input.display(),
        )
    );
}

#[test]
fn top_level_linearize_normalize_content_warning_writes_independent_pass1() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("bad-content.pdf");
    let output = temp.path().join("normalized-linearized.pdf");
    let pass1 = temp.path().join("pass1.pdf");
    std::fs::write(&input, one_page_pdf_with_content(b"\r<0g")).unwrap();
    std::fs::write(&pass1, b"stale pass1").unwrap();

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--linearize", "--normalize-content=y"])
        .arg(format!("--linearize-pass1={}", pass1.display()))
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    assert!(output.exists(), "warning exit must retain the final output");
    let pass1_bytes = std::fs::read(&pass1).unwrap();
    let output_bytes = std::fs::read(&output).unwrap();
    assert!(pass1_bytes.starts_with(b"%PDF-"));
    assert!(pass1_bytes
        .windows(b"% hint_offset=".len())
        .any(|window| window == b"% hint_offset="));
    assert_ne!(pass1_bytes, output_bytes);
}

#[test]
fn top_level_qdf_defaults_to_content_normalization() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("crlf-content.pdf");
    let output = temp.path().join("default-qdf.pdf");
    std::fs::write(&input, one_page_pdf_with_content(b"q\rQ")).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--qdf")
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert_eq!(first_page_content(&output), b"q\nQ");
}

#[test]
fn top_level_qdf_explicit_normalize_content_n_overrides_default() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("crlf-content.pdf");
    let output = temp.path().join("not-normalized-qdf.pdf");
    std::fs::write(&input, one_page_pdf_with_content(b"q\rQ")).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--qdf", "--normalize-content=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert_eq!(first_page_content(&output), b"q\rQ");
}

#[test]
fn top_level_normalize_content_y_applies_after_page_selection() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("crlf-content.pdf");
    let output = temp.path().join("normalized-pages.pdf");
    std::fs::write(&input, one_page_pdf_with_content(b"q\rQ")).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--normalize-content=y",
            input.to_str().unwrap(),
            "--pages",
            ".",
            "1",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(first_page_content(&output), b"q\nQ");
}

#[test]
fn top_level_decode_level_none_is_accepted_with_mutating_attachment_paths() {
    // decode-level=none matches the attachment serializers' existing
    // behavior (no filter decoding at all), so it must not be rejected the
    // way a non-none level is.
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("attachment.bin");
    std::fs::write(&attachment, b"payload").unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--decode-level=none",
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
}

#[test]
fn top_level_add_attachment_normalization_preserves_warning_completion() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("bad-content.pdf");
    let attachment = temp.path().join("attachment.bin");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_with_content(b"\r<0g")).unwrap();
    std::fs::write(&attachment, b"payload").unwrap();

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--normalize-content=y", "--compress-streams=n"])
        .arg("--add-attachment")
        .arg(&attachment)
        .arg("--")
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    assert!(
        output.exists(),
        "warning exit must retain attachment output"
    );
    let stderr = String::from_utf8(result.stderr).unwrap();
    assert!(stderr.contains("content normalization encountered bad tokens"));
    assert!(
        stderr.contains("operation succeeded with warnings; resulting file may have some problems")
    );
}

#[test]
fn top_level_add_attachment_no_warn_suppresses_normalization_text() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("bad-content.pdf");
    let attachment = temp.path().join("attachment.bin");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_with_content(b"\r<0g")).unwrap();
    std::fs::write(&attachment, b"payload").unwrap();

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--no-warn", "--normalize-content=y", "--compress-streams=n"])
        .arg("--add-attachment")
        .arg(&attachment)
        .arg("--")
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    assert!(
        output.exists(),
        "warning exit must retain attachment output"
    );
    assert!(
        result.stderr.is_empty(),
        "--no-warn must suppress warning text"
    );
}

#[test]
fn top_level_linearize_accepts_compress_streams_and_pass1() {
    // Mirrors the COMMAND from upstream qpdf's linearize-pass1.test:
    //   qpdf --linearize --static-id --compress-streams=n \
    //        --linearize-pass1=b.pdf in.pdf a.pdf
    // We do not assert byte-equality with qpdf's golden output here —
    // that is a separate, much larger gate. We assert only that the CLI
    // parses, runs to completion, writes both files, and emits no
    // stdout/stderr (qpdf qtest's subtest 1 condition).
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("a.pdf");
    let pass1 = temp.path().join("b.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    // --static-id normally emits a "testing only" stderr warning
    //. This test mirrors qpdf qtest's "no stdout/stderr"
    // condition, so suppress the diagnostic via the documented opt-out env
    // var; the empty-stderr assertion below still pins the parity guarantee.
    cmd.env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--linearize", "--static-id", "--compress-streams=n"])
        .arg(format!("--linearize-pass1={}", pass1.display()))
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());

    let output_bytes = std::fs::read(&output).unwrap();
    let pass1_bytes = std::fs::read(&pass1).unwrap();
    assert!(pass1_bytes.starts_with(b"%PDF-"));
    assert!(pass1_bytes
        .windows(b"% hint_offset=".len())
        .any(|window| window == b"% hint_offset="));
    assert_ne!(pass1_bytes, output_bytes);
}

// ---------------------------------------------------------------------------
// Version validation tests
// ---------------------------------------------------------------------------

#[test]
fn rewrite_force_version_accepts_raw_abc() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--force-version=abc",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(std::fs::read(&output).unwrap().starts_with(b"%PDF-abc\n"));
}

#[test]
fn rewrite_force_version_accepts_newline_in_raw_value() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .arg("--force-version=1.4\n")
    .assert()
    .success();

    assert!(std::fs::read(&output).unwrap().starts_with(b"%PDF-1.4\n\n"));
}

#[test]
fn rewrite_min_version_accepts_raw_abc_as_a_noop() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--min-version=abc",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(std::fs::read(&output).unwrap().starts_with(b"%PDF-1.7\n"));
}

#[test]
fn rewrite_valid_force_version_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--force-version=1.4",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    // minimal.pdf has header 1.7; --force-version=1.4 must rewrite the header
    // line down to exactly 1.4 ("Output header line matches the chosen
    // version").
    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-1.4\n"),
        "expected forced header %PDF-1.4; got {:?}",
        std::str::from_utf8(&bytes[..bytes.len().min(9)]).unwrap_or("<bad>")
    );
}

#[test]
fn rewrite_valid_min_version_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--min-version=1.3",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    // minimal.pdf is already 1.7; --min-version=1.3 is below the source, so
    // it must be a no-op (header stays 1.7).
    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-1.7\n"),
        "min-version below source must be a no-op (header stays 1.7); got {:?}",
        std::str::from_utf8(&bytes[..bytes.len().min(9)]).unwrap_or("<bad>")
    );
}

#[test]
fn rewrite_min_version_raises_header_on_low_source() {
    // Build a header-1.3 PDF and request --min-version=1.7: the header line
    // must be raised to exactly 1.7.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("v13.pdf");
    let output = temp.path().join("out.pdf");

    let mut pdf = b"%PDF-1.3\n".to_vec();
    let o1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let o2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let startxref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    pdf.extend_from_slice(format!("{o1:010} 00000 n \n{o2:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n").as_bytes(),
    );
    std::fs::write(&input, &pdf).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--min-version=1.7",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-1.7\n"),
        "min-version 1.7 must raise header 1.3 -> 1.7; got {:?}",
        std::str::from_utf8(&bytes[..bytes.len().min(9)]).unwrap_or("<bad>")
    );
}

#[test]
fn top_level_min_version_with_extension_level_is_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--min-version=1.7.1",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.7\n"));
    assert!(contains(&bytes, b"/BaseVersion /1.7"));
    assert!(contains(&bytes, b"/ExtensionLevel 1"));
}

#[test]
fn rewrite_force_version_with_extension_level_emits_base_header_and_adbe_pair() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--static-id",
            "--force-version=1.8.5",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.8\n"));
    assert!(contains(&bytes, b"/BaseVersion /1.8"));
    assert!(contains(&bytes, b"/ExtensionLevel 5"));
}

#[test]
fn rewrite_force_version_preserves_qpdf_raw_version_header() {
    let temp = tempfile::tempdir().unwrap();
    let input = "../../tests/fixtures/compat/one-page.pdf";
    let qpdf_output = temp.path().join("qpdf-force-trailing-dot.pdf");
    let flpdf_output = temp.path().join("flpdf-force-trailing-dot.pdf");

    let qpdf_status = ProcessCommand::new("qpdf")
        .args(["--static-id", "--force-version=1.7.", input])
        .arg(&qpdf_output)
        .status()
        .expect("qpdf 11.9.0 must be available for this differential test");
    assert!(qpdf_status.success(), "qpdf force-version probe failed");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--static-id",
            "--force-version=1.7.",
            input,
            flpdf_output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let qpdf_bytes = std::fs::read(&qpdf_output).unwrap();
    let flpdf_bytes = std::fs::read(&flpdf_output).unwrap();
    let expected_header = b"%PDF-1.7.\n";
    assert!(qpdf_bytes.starts_with(expected_header));
    assert_eq!(
        &flpdf_bytes[..expected_header.len().min(flpdf_bytes.len())],
        expected_header,
        "flpdf must preserve qpdf's raw forced version header"
    );
}

#[test]
fn top_level_min_version_preserves_qpdf_raw_version_header() {
    let temp = tempfile::tempdir().unwrap();
    let input = "../../tests/fixtures/compat/one-page.pdf";
    let qpdf_output = temp.path().join("qpdf-min-trailing-dot.pdf");
    let flpdf_output = temp.path().join("flpdf-min-trailing-dot.pdf");

    let qpdf_status = ProcessCommand::new("qpdf")
        .args(["--static-id", "--min-version=1.7.", input])
        .arg(&qpdf_output)
        .status()
        .expect("qpdf 11.9.0 must be available for this differential test");
    assert!(qpdf_status.success(), "qpdf min-version probe failed");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--min-version=1.7.",
            input,
            flpdf_output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let qpdf_bytes = std::fs::read(&qpdf_output).unwrap();
    let flpdf_bytes = std::fs::read(&flpdf_output).unwrap();
    let expected_header = b"%PDF-1.7.\n";
    assert!(qpdf_bytes.starts_with(expected_header));
    assert_eq!(
        &flpdf_bytes[..expected_header.len().min(flpdf_bytes.len())],
        expected_header,
        "top-level min-version must preserve qpdf's raw version header"
    );
}

#[test]
fn top_level_min_version_outright_win_takes_the_raw_winning_version() {
    // 1.7x > 1.3 numerically, so this is an outright win, not a tie: qpdf's
    // compare > 0 branch (QPDFWriter.cc:217-247) takes both the raw string
    // and the extension level from the winning --min-version candidate.
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("min-version-extension-tie.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--min-version=1.7x.2",
            "../../tests/fixtures/compat/one-page.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.7x\n"));
    assert!(contains(&bytes, b"/BaseVersion /1.7x"));
    assert!(contains(&bytes, b"/ExtensionLevel 2"));
}

#[test]
fn top_level_min_version_numeric_tie_keeps_the_source_s_raw_version() {
    // A genuine numeric tie (source forced to exactly 1.7, --min-version
    // ties at 1.7 but spells it "1.7x"): qpdf's setMinimumPDFVersion never
    // sets set_version on compare == 0, only set_extension_level. Verified
    // against live qpdf 11.9.0: the source's raw "1.7" spelling survives,
    // not the tying --min-version candidate's "1.7x".
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source-1.7.pdf");
    let output = temp.path().join("min-version-numeric-tie.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--force-version=1.7",
            "../../tests/fixtures/compat/one-page.pdf",
            source.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--min-version=1.7x.2",
            source.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.7\n"));
    assert!(contains(&bytes, b"/BaseVersion /1.7 "));
    assert!(contains(&bytes, b"/ExtensionLevel 2"));
}

#[test]
fn top_level_min_version_numeric_tie_keeps_the_source_s_higher_extension() {
    // The source's own extension level (8) numerically ties with
    // --min-version's version (1.7x) but is higher than its extension
    // level (2): the source wins the tie outright (qpdf's compare == 0,
    // extension_level(2) > m->min_extension_level(8) is false, so neither
    // set_version nor set_extension_level fire), and the header and
    // Catalog /Extensions /ADBE entry must agree. Verified byte-identical
    // against live qpdf 11.9.0.
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("min-version-source-wins-extension.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--min-version=1.7x.2",
            "../../tests/fixtures/compat/direct-root-adbe.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.7\n"));
    assert!(contains(&bytes, b"/BaseVersion /1.7 "));
    assert!(contains(&bytes, b"/ExtensionLevel 8"));
}

#[test]
fn empty_force_version_is_a_noop_like_qpdf() {
    let temp = tempfile::tempdir().unwrap();
    let input = "../../tests/fixtures/compat/one-page.pdf";
    let qpdf_output = temp.path().join("qpdf-force-empty.pdf");
    let flpdf_output = temp.path().join("flpdf-force-empty.pdf");

    let qpdf_status = ProcessCommand::new("qpdf")
        .args(["--static-id", "--force-version=", input])
        .arg(&qpdf_output)
        .status()
        .expect("qpdf 11.9.0 must be available for this differential test");
    assert!(
        qpdf_status.success(),
        "qpdf empty force-version probe failed"
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--static-id",
            "--force-version=",
            input,
            flpdf_output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let qpdf_bytes = std::fs::read(&qpdf_output).unwrap();
    let flpdf_bytes = std::fs::read(&flpdf_output).unwrap();
    assert!(qpdf_bytes.starts_with(b"%PDF-1.3\n"));
    assert_eq!(
        &flpdf_bytes[..b"%PDF-1.3\n".len().min(flpdf_bytes.len())],
        b"%PDF-1.3\n",
        "an empty force-version value must not replace qpdf's source version"
    );
}

#[test]
fn top_level_force_version_accepts_raw_value() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--force-version=not-a-version",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(std::fs::read(&output)
        .unwrap()
        .starts_with(b"%PDF-not-a-version\n"));
}

#[test]
fn rewrite_force_version_honored_without_mutation() {
    // Regression: `--remove-unreferenced-resources=no`
    // must not change the canonical writer route or silently drop
    // --force-version. qpdf always emits a fresh rewrite and honors the
    // version setter.
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--remove-unreferenced-resources=no",
        "--force-version=1.4",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-1.4\n"),
        "force-version must be honored on the canonical rewrite path; \
         got {:?}",
        std::str::from_utf8(&bytes[..bytes.len().min(9)]).unwrap_or("<bad>")
    );
}

#[test]
fn check_repairs_corrupt_xref_by_default() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_with_info_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", input.to_str().unwrap()])
        .assert()
        .code(3)
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )));
}

#[test]
fn check_with_repair_accepts_corrupt_xref() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_with_info_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    // Repair produces a "xref repaired" warning → exit 3 (qpdf-compatible:
    // warnings found, no errors).
    cmd.args(["--repair", "--check", input.to_str().unwrap()])
        .assert()
        .code(3)
        .stdout(predicate::str::contains(format!(
            "File is not encrypted{EOL}"
        )));
}

#[test]
fn dump_object_accepts_ref_without_suffix() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-object=1 0", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/Type /Catalog"));
}

#[test]
fn dump_object_accepts_ref_with_r_suffix() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-object=1 0 R", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/Type /Catalog"));
}

#[test]
fn show_object_invalid_selector_matches_qpdf_no_output() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-object=bad", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn json_outlines_short_first_name_tree_pair_exits_two_without_complete_json() {
    let fixture = fixture_with_short_first_name_tree_pair();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let assert = cmd
        .args(["--json=2", "--json-key=outlines"])
        .arg(fixture.path())
        .assert()
        .code(2);
    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let repair = stderr
        .find("attempting to repair after error:")
        .unwrap_or_else(|| panic!("missing repair warning in {stderr}"));
    let fatal = stderr
        .rfind("update ivalue: items array is too short")
        .unwrap_or_else(|| panic!("missing fatal error in {stderr}"));
    assert!(repair < fatal, "{stderr}");
    assert_eq!(
        stderr.matches("attempting to repair after error:").count(),
        1
    );
    assert_eq!(
        stderr
            .matches("update ivalue: items array is too short")
            .count(),
        2
    );
    assert!(output.stdout.starts_with(b"{\n  \"version\": 2,"));
    assert!(!output.stdout.ends_with(b"}\n"));
    assert!(serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err());
}

#[test]
fn json_successful_name_tree_repair_emits_warning_summary_and_exits_three() {
    let fixture = fixture_with_repaired_name_tree_and_stream();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let assert = cmd
        .args(["--json=2", "--json-key=outlines"])
        .arg(fixture.path())
        .assert()
        .code(3);
    let output = assert.get_output();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["outlines"][0]["dest"][0], "3 0 R");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let warning = stderr
        .find("attempting to repair after error:")
        .unwrap_or_else(|| panic!("missing repair warning in {stderr}"));
    let summary = stderr
        .find("flpdf: operation succeeded with warnings")
        .unwrap_or_else(|| panic!("missing warning summary in {stderr}"));
    assert!(warning < summary, "{stderr}");
    assert_eq!(
        stderr.matches("attempting to repair after error:").count(),
        1
    );
    assert_eq!(
        stderr
            .matches("flpdf: operation succeeded with warnings")
            .count(),
        1
    );
}

#[test]
fn json_key_qpdf_skips_unselected_outline_repair() {
    let fixture = fixture_with_repaired_name_tree_and_stream();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let assert = cmd
        .args(["--json=2", "--json-key=qpdf"])
        .arg(fixture.path())
        .assert()
        .code(0);
    let output = assert.get_output();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json.get("outlines").is_none());
    let dests = &json["qpdf"][1]["obj:1 0 R"]["value"]["/Names"]["/Dests"];
    assert_eq!(dests["/Kids"][0], "8 0 R");
    assert!(dests.get("/Names").is_none());
}

#[test]
fn json_key_outlines_and_qpdf_repairs_before_raw_object_projection() {
    let fixture = fixture_with_repaired_name_tree_and_stream();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let assert = cmd
        .args(["--json=2", "--json-key=outlines", "--json-key=qpdf"])
        .arg(fixture.path())
        .assert()
        .code(3);
    let output = assert.get_output();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["outlines"][0]["dest"][0], "3 0 R");
    let dests = &json["qpdf"][1]["obj:1 0 R"]["value"]["/Names"]["/Dests"];
    assert_eq!(dests["/Names"][0], "u:shape");
    assert_eq!(dests["/Names"][1][0], "3 0 R");
    assert!(dests.get("/Kids").is_none());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("attempting to repair after error:").count(),
        1
    );
    assert_eq!(
        stderr
            .matches("flpdf: operation succeeded with warnings")
            .count(),
        1
    );
}

#[test]
fn json_key_selection_order_is_qpdf_fixed() {
    let fixture = fixture_with_repaired_name_tree_and_stream();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let assert = cmd
        .args(["--json=2", "--json-key=qpdf", "--json-key=outlines"])
        .arg(fixture.path())
        .assert()
        .code(3);
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let outlines = stdout
        .find("\"outlines\":")
        .unwrap_or_else(|| panic!("missing outlines section in {stdout}"));
    let qpdf = stdout
        .find("\"qpdf\":")
        .unwrap_or_else(|| panic!("missing qpdf section in {stdout}"));
    assert!(outlines < qpdf, "{stdout}");
}

#[test]
fn json_metadata_tracks_page_enumeration_without_pushing_inherited_resources() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/inherited-resources-one-page.pdf");
    let cases = [
        ("qpdf only", &["--json=2", "--json-key=qpdf"][..], false),
        (
            "pages then qpdf",
            &["--json=2", "--json-key=pages", "--json-key=qpdf"][..],
            true,
        ),
    ];

    for (label, args, called_get_all_pages) in cases {
        let output = Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(&fixture)
            .output()
            .unwrap();
        assert!(output.status.success(), "{label}");
        assert!(output.stderr.is_empty(), "{label}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            json_qpdf_metadata(&json),
            &serde_json::json!({
                "jsonversion": 2,
                "pdfversion": "1.4",
                "pushedinheritedpageresources": false,
                "calledgetallpages": called_get_all_pages,
                "maxobjectid": 5
            }),
            "{label}"
        );
        assert!(
            json["qpdf"][1]["obj:3 0 R"]["value"]
                .get("/Resources")
                .is_none(),
            "{label}: page dictionary must remain unmodified"
        );
    }
}

#[test]
fn json_missing_catalog_pages_matches_qpdf_warning_contract() {
    if !qpdf_11_9_available() {
        return;
    }

    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture.write_all(&catalog_without_pages_pdf()).unwrap();

    let cases: [(&[&str], usize); 6] = [
        (&["--json=2", "--json-key=pages"], 1),
        (&["--json=2", "--json-key=pagelabels"], 1),
        (&["--json=2", "--json-key=outlines"], 1),
        (&["--json=2", "--json-key=acroform"], 1),
        (&["--json=2"], 4),
        (&["--json=2", "--json-key=qpdf"], 0),
    ];

    for (args, expected_warning_count) in cases {
        let qpdf = std::process::Command::new("qpdf")
            .args(args)
            .arg(fixture.path())
            .output()
            .unwrap();
        assert_eq!(
            qpdf.status.code(),
            Some(if expected_warning_count == 0 { 0 } else { 3 }),
            "qpdf status for {args:?}: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout)
            .unwrap_or_else(|error| panic!("qpdf JSON for {args:?} was invalid: {error}"));

        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(fixture.path())
            .output()
            .unwrap();
        assert_eq!(
            flpdf.status.code(),
            Some(if expected_warning_count == 0 { 0 } else { 3 }),
            "flpdf status for {args:?}: {}",
            String::from_utf8_lossy(&flpdf.stderr)
        );
        let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout)
            .unwrap_or_else(|error| panic!("flpdf JSON for {args:?} was invalid: {error}"));

        assert_eq!(flpdf_json, qpdf_json, "JSON mismatch for {args:?}");
        let warning = "operation for dictionary attempted on object of type null: \
                       returning false for a key containment request";
        assert_eq!(
            String::from_utf8_lossy(&flpdf.stderr)
                .matches(warning)
                .count(),
            expected_warning_count,
            "warning count for {args:?}: {}",
            String::from_utf8_lossy(&flpdf.stderr)
        );
        assert!(!flpdf
            .stderr
            .windows(b"missing required PDF entry: /Pages".len())
            .any(|window| window == b"missing required PDF entry: /Pages"));
    }
}

#[test]
fn pages_external_source_matches_qpdf_resource_copy_modes() {
    // qpdf 11.9.0's --pages path uses a page-local copy for inherited or
    // shared /Resources when auto fires or yes is explicit. `no` leaves the
    // source reference intact. This fixture is the smallest inherited-
    // /Resources case that distinguishes those three modes.
    let primary =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/inherited-resources-one-page.pdf");
    let temp = tempfile::tempdir().unwrap();

    for (mode, expected_direct) in [("auto", true), ("yes", true), ("no", false)] {
        let output = temp.path().join(format!("pages-{mode}.pdf"));
        Command::cargo_bin("flpdf")
            .unwrap()
            .args(["rewrite"])
            .arg(&primary)
            .arg(&output)
            .arg(format!("--remove-unreferenced-resources={mode}"))
            .args(["--pages"])
            .arg(&source)
            .args(["1", "--"])
            .assert()
            .success();

        let mut pdf = Pdf::open(std::io::BufReader::new(File::open(&output).unwrap())).unwrap();
        let page_ref = pages::page_refs(&mut pdf).unwrap()[0];
        let page = pdf.resolve_canonical_object(page_ref).unwrap();
        let resources = page.try_get_key(b"/Resources").unwrap();
        assert_eq!(
            resources.is_direct(),
            expected_direct,
            "--remove-unreferenced-resources={mode}: {page:?}"
        );
    }
}

#[test]
fn json_metadata_includes_outline_repair_allocations_in_maxobjectid() {
    let fixture = fixture_with_name_tree_repair_allocations();
    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=outlines", "--json-key=qpdf"])
        .arg(fixture.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json_qpdf_metadata(&json),
        &serde_json::json!({
            "jsonversion": 2,
            "pdfversion": "1.7",
            "pushedinheritedpageresources": false,
            "calledgetallpages": true,
            "maxobjectid": 10
        })
    );
    assert!(json["qpdf"][1].get("obj:9 0 R").is_some());
    assert!(json["qpdf"][1].get("obj:10 0 R").is_some());
}

#[test]
fn json_qpdf_preparation_treats_referenced_empty_object_as_null_with_warning() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture.write_all(&empty_object_json_pdf()).unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=qpdf", "--json-object=7"])
        .arg(fixture.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["qpdf"][1]["obj:7 0 R"],
        serde_json::json!({"value": null})
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("empty object treated as null").count(), 1);
    assert_eq!(
        stderr.matches("operation succeeded with warnings").count(),
        1
    );
    assert!(
        stderr.find("empty object treated as null").unwrap()
            < stderr.find("operation succeeded with warnings").unwrap(),
        "{stderr}"
    );
}

#[test]
fn json_qpdf_preparation_treats_top_level_bare_reference_as_integer() {
    let bytes = top_level_bare_reference_json_pdf();
    let expected_warning_offset = bytes
        .windows(b"4 0 obj\n3 0 R".len())
        .position(|window| window == b"4 0 obj\n3 0 R")
        .expect("fixture must contain malformed object")
        + b"4 0 obj\n3 ".len();
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture.write_all(&bytes).unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=qpdf", "--json-object=4"])
        .arg(fixture.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["qpdf"][1]["obj:4 0 R"],
        serde_json::json!({"value": 3})
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let warning = format!("(object 4 0, offset {expected_warning_offset}): expected endobj");
    assert_eq!(stderr.matches(&warning).count(), 1, "{stderr}");
    assert_eq!(
        stderr.matches("operation succeeded with warnings").count(),
        1
    );
    assert!(
        stderr.find(&warning).unwrap() < stderr.find("operation succeeded with warnings").unwrap(),
        "{stderr}"
    );
}

#[test]
fn json_unselected_qpdf_does_not_resolve_unrelated_empty_object() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture.write_all(&empty_object_json_pdf()).unwrap();

    for section in ["pages", "outlines"] {
        let output = Command::cargo_bin("flpdf")
            .unwrap()
            .args(["--json=2", &format!("--json-key={section}")])
            .arg(fixture.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{section}");
        assert!(output.stderr.is_empty(), "{section}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(json.get(section).is_some(), "{section}");
        assert!(json.get("qpdf").is_none(), "{section}");
    }
}

#[test]
fn json_qpdf_preparation_walks_nested_stream_holder_and_cycle_objects() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture.write_all(&dangling_container_json_pdf()).unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=qpdf"])
        .arg(fixture.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json_qpdf_metadata(&json)["maxobjectid"], 99);
    assert_eq!(
        json["qpdf"][1]["obj:1 0 R"]["value"]["/Nested"],
        serde_json::json!({"/Array": ["99 2 R", null]})
    );
    assert_eq!(
        json["qpdf"][1]["obj:4 0 R"]["stream"]["dict"],
        serde_json::json!({"/Length": 0})
    );
    assert_eq!(
        json["qpdf"][1]["obj:5 0 R"],
        serde_json::json!({"value": {}})
    );
    assert_eq!(
        json["qpdf"][1]["obj:99 2 R"],
        serde_json::json!({"value": null})
    );
    assert_eq!(
        json["qpdf"][1]["obj:6 0 R"],
        serde_json::json!({"value": {"/Next": "7 0 R"}})
    );
    assert_eq!(
        json["qpdf"][1]["obj:7 0 R"],
        serde_json::json!({"value": {"/Next": "6 0 R"}})
    );
    assert!(output.stderr.is_empty());
}

#[test]
#[ignore = "live qpdf 11.9.0 dangling container JSON oracle"]
fn live_qpdf_dangling_container_json_matches() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture.write_all(&dangling_container_json_pdf()).unwrap();
    let args = ["--json=2", "--json-key=qpdf"];
    let qpdf = std::process::Command::new("qpdf")
        .args(args)
        .arg(fixture.path())
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(fixture.path())
        .output()
        .unwrap();

    let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr);
    let flpdf_stderr = String::from_utf8_lossy(&flpdf.stderr);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "qpdf stderr: {qpdf_stderr}\nflpdf stderr: {flpdf_stderr}"
    );
    let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
    let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
    assert_eq!(flpdf_json["qpdf"], qpdf_json["qpdf"]);
    assert_eq!(flpdf_stderr, qpdf_stderr);
}

#[test]
fn json_qpdf_preparation_resolves_lazy_objects_and_exact_free_generations() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture
        .write_all(&dangling_generation_free_json_pdf())
        .unwrap();
    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=qpdf"])
        .arg(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json_qpdf_metadata(&json)["maxobjectid"], 88);
    let objects = &json["qpdf"][1];
    assert_eq!(objects["obj:8 0 R"], serde_json::json!({"value": null}));
    assert_eq!(
        objects["obj:8 1 R"],
        serde_json::json!({"value": {"/Value": 1}})
    );
    assert_eq!(objects["obj:20 7 R"], serde_json::json!({"value": null}));
    assert_eq!(objects["obj:88 4 R"], serde_json::json!({"value": null}));
    assert!(objects.get("obj:200 7 R").is_none());
}

#[test]
#[ignore = "live qpdf 11.9.0 lazy/generation/free JSON oracle"]
fn live_qpdf_dangling_generation_free_json_matches() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture
        .write_all(&dangling_generation_free_json_pdf())
        .unwrap();
    let args = ["--json=2", "--json-key=qpdf"];
    let qpdf = std::process::Command::new("qpdf")
        .args(args)
        .arg(fixture.path())
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(fixture.path())
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stderr, qpdf.stderr);
    let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
    let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
    assert_eq!(flpdf_json["qpdf"], qpdf_json["qpdf"]);
}

#[test]
fn json_qpdf_preparation_discovers_trailer_only_dangling_generations() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture.write_all(&trailer_dangling_json_pdf()).unwrap();
    let cases: [(&str, &[&str]); 4] = [
        (
            "trailer",
            &["--json=2", "--json-key=qpdf", "--json-object=trailer"],
        ),
        (
            "object99",
            &["--json=2", "--json-key=qpdf", "--json-object=99"],
        ),
        (
            "generation",
            &["--json=2", "--json-key=qpdf", "--json-object=88,4"],
        ),
        ("all", &["--json=2", "--json-key=qpdf"]),
    ];

    for (label, args) in cases {
        let output = Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(fixture.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{label}");
        assert!(output.stderr.is_empty(), "{label}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json_qpdf_metadata(&json)["maxobjectid"], 99, "{label}");
        let map = json["qpdf"][1].as_object().unwrap();
        match label {
            "trailer" => assert_eq!(
                map.keys().map(String::as_str).collect::<Vec<_>>(),
                vec!["trailer"]
            ),
            "object99" => assert_eq!(
                map,
                &serde_json::Map::from_iter([(
                    "obj:99 0 R".to_string(),
                    serde_json::json!({"value": null}),
                )])
            ),
            "generation" => assert_eq!(
                map,
                &serde_json::Map::from_iter([(
                    "obj:88 4 R".to_string(),
                    serde_json::json!({"value": null}),
                )])
            ),
            "all" => {
                assert_eq!(map["obj:99 0 R"], serde_json::json!({"value": null}));
                assert_eq!(map["obj:88 4 R"], serde_json::json!({"value": null}));
                assert!(map.get("obj:0 0 R").is_none());
                assert!(map.get("obj:77 65535 R").is_none());
                assert!(map.get("obj:200 7 R").is_none());
            }
            _ => unreachable!(),
        }
        if let Some(trailer) = map.get("trailer") {
            let value = trailer["value"].as_object().unwrap();
            assert_eq!(value["/Root"], "1 0 R");
            assert_eq!(value["/Size"], 201);
            for omitted in ["/Info", "/Gen", "/Zero", "/BadGen"] {
                assert!(value.get(omitted).is_none(), "{label}: {omitted}");
            }
        }
    }
}

#[test]
#[ignore = "live qpdf 11.9.0 trailer-only dangling JSON oracle"]
fn live_qpdf_trailer_only_dangling_json_matches() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture.write_all(&trailer_dangling_json_pdf()).unwrap();
    let cases: [&[&str]; 4] = [
        &["--json=2", "--json-key=qpdf", "--json-object=trailer"],
        &["--json=2", "--json-key=qpdf", "--json-object=99"],
        &["--json=2", "--json-key=qpdf", "--json-object=88,4"],
        &["--json=2", "--json-key=qpdf"],
    ];

    for args in cases {
        let qpdf = std::process::Command::new("qpdf")
            .args(args)
            .arg(fixture.path())
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(fixture.path())
            .output()
            .unwrap();
        assert_eq!(flpdf.status.code(), qpdf.status.code(), "{args:?}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "{args:?}");
        let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
        let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
        assert_eq!(flpdf_json["qpdf"], qpdf_json["qpdf"], "{args:?}");
    }
}

#[test]
fn json_qpdf_preparation_discovers_all_historical_incremental_trailer_refs() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture
        .write_all(&multigeneration_historical_trailer_json_pdf())
        .unwrap();
    let cases: [(&str, &[&str]); 6] = [
        (
            "trailer",
            &["--json=2", "--json-key=qpdf", "--json-object=trailer"],
        ),
        (
            "oldest",
            &["--json=2", "--json-key=qpdf", "--json-object=99"],
        ),
        (
            "old-generation",
            &["--json=2", "--json-key=qpdf", "--json-object=88,4"],
        ),
        (
            "replaced-generation",
            &["--json=2", "--json-key=qpdf", "--json-object=60,1"],
        ),
        (
            "referenced-free",
            &["--json=2", "--json-key=qpdf", "--json-object=20,7"],
        ),
        ("all", &["--json=2", "--json-key=qpdf"]),
    ];

    for (label, args) in cases {
        let output = Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(fixture.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{label}");
        assert!(output.stderr.is_empty(), "{label}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json_qpdf_metadata(&json)["maxobjectid"], 99, "{label}");
        let map = json["qpdf"][1].as_object().unwrap();
        match label {
            "trailer" => {
                assert_eq!(
                    map.keys().map(String::as_str).collect::<Vec<_>>(),
                    vec!["trailer"]
                );
                let value = map["trailer"]["value"].as_object().unwrap();
                assert_eq!(value["/Root"], "1 0 R");
                assert_eq!(value["/Size"], 201);
                assert!(value.get("/Newest").is_none());
                for historical in ["/Info", "/OldGen", "/Freed", "/Middle"] {
                    assert!(value.get(historical).is_none(), "{historical}");
                }
            }
            "oldest" => assert_eq!(map["obj:99 0 R"], serde_json::json!({"value": null})),
            "old-generation" => {
                assert_eq!(map["obj:88 4 R"], serde_json::json!({"value": null}))
            }
            "replaced-generation" => {
                assert_eq!(map["obj:60 1 R"], serde_json::json!({"value": null}))
            }
            "referenced-free" => {
                assert_eq!(map["obj:20 7 R"], serde_json::json!({"value": null}))
            }
            "all" => {
                for key in [
                    "obj:99 0 R",
                    "obj:88 4 R",
                    "obj:20 7 R",
                    "obj:60 1 R",
                    "obj:70 3 R",
                    "obj:50 2 R",
                ] {
                    assert_eq!(map[key], serde_json::json!({"value": null}), "{key}");
                }
                assert!(map.get("obj:0 0 R").is_none());
                assert!(map.get("obj:77 65535 R").is_none());
                assert!(map.get("obj:200 7 R").is_none());
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn json_qpdf_preparation_discovers_refs_from_a_freed_historical_xref_stream() {
    for free_generation in [0, 1] {
        let mut fixture = tempfile::NamedTempFile::new().unwrap();
        fixture
            .write_all(&historical_xref_stream_json_pdf_with_free_generation(
                free_generation,
            ))
            .unwrap();
        let output = Command::cargo_bin("flpdf")
            .unwrap()
            .args(["--json=2", "--json-key=qpdf"])
            .arg(fixture.path())
            .output()
            .unwrap();

        assert!(output.status.success(), "generation {free_generation}");
        assert!(output.stderr.is_empty(), "generation {free_generation}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json_qpdf_metadata(&json)["maxobjectid"], 99);
        let map = json["qpdf"][1].as_object().unwrap();
        assert_eq!(map["obj:99 0 R"], serde_json::json!({"value": null}));
        assert_eq!(map["obj:88 4 R"], serde_json::json!({"value": null}));
        assert_eq!(
            map["obj:4 0 R"]["stream"]["dict"]["/Type"],
            serde_json::json!("/XRef")
        );
        assert!(map["obj:4 0 R"]["stream"]["dict"].get("/Info").is_none());
        assert!(map["obj:4 0 R"]["stream"]["dict"].get("/Gen").is_none());
        let trailer = map["trailer"]["value"].as_object().unwrap();
        assert!(trailer.get("/Info").is_none());
        assert!(trailer.get("/Gen").is_none());
    }
}

#[test]
fn json_file_mode_writes_selected_historical_xref_stream_bytes_only() {
    for free_generation in [0, 1] {
        let mut fixture = tempfile::NamedTempFile::new().unwrap();
        fixture
            .write_all(&historical_xref_stream_json_pdf_with_free_generation(
                free_generation,
            ))
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join(format!("stream-{free_generation}"));
        let prefix_arg = format!("--json-stream-prefix={}", prefix.display());
        let output = Command::cargo_bin("flpdf")
            .unwrap()
            .args([
                "--json=2",
                "--json-key=qpdf",
                "--json-object=4,0",
                "--json-stream-data=file",
            ])
            .arg(&prefix_arg)
            .arg(fixture.path())
            .output()
            .unwrap();

        assert!(output.status.success(), "generation {free_generation}");
        assert!(output.stderr.is_empty(), "generation {free_generation}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let side_path = format!("{}-4", prefix.display());
        assert_eq!(
            json["qpdf"][1]["obj:4 0 R"]["stream"]["datafile"],
            side_path
        );
        assert_eq!(std::fs::read(&side_path).unwrap().len(), 35);

        let unselected_prefix = temp.path().join(format!("unselected-{free_generation}"));
        let unselected_prefix_arg = format!("--json-stream-prefix={}", unselected_prefix.display());
        let unselected = Command::cargo_bin("flpdf")
            .unwrap()
            .args(["--json=2", "--json-key=pages", "--json-stream-data=file"])
            .arg(&unselected_prefix_arg)
            .arg(fixture.path())
            .output()
            .unwrap();
        assert!(unselected.status.success());
        assert!(!std::path::Path::new(&format!("{}-4", unselected_prefix.display())).exists());
    }
}

#[test]
#[ignore = "live qpdf 11.9.0 historical xref stream datafile oracle"]
fn live_qpdf_historical_xref_stream_file_payload_matches() {
    for free_generation in [0, 1] {
        let mut fixture = tempfile::NamedTempFile::new().unwrap();
        fixture
            .write_all(&historical_xref_stream_json_pdf_with_free_generation(
                free_generation,
            ))
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("stream");
        let args = [
            "--json=2",
            "--json-key=qpdf",
            "--json-object=4,0",
            "--json-stream-data=file",
        ];
        let prefix_arg = format!("--json-stream-prefix={}", prefix.display());
        let qpdf = std::process::Command::new("qpdf")
            .args(args)
            .arg(&prefix_arg)
            .arg(fixture.path())
            .output()
            .unwrap();
        let side_path = format!("{}-4", prefix.display());
        let qpdf_payload = std::fs::read(&side_path).unwrap();
        std::fs::remove_file(&side_path).unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(&prefix_arg)
            .arg(fixture.path())
            .env("FLPDF_PROGNAME", "qpdf")
            .output()
            .unwrap();
        let flpdf_payload = std::fs::read(&side_path).unwrap();

        assert_eq!(flpdf.status.code(), qpdf.status.code());
        assert_eq!(flpdf.stderr, qpdf.stderr);
        assert_eq!(flpdf_payload, qpdf_payload);
        let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
        let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
        assert_eq!(
            flpdf_json["qpdf"][1]["obj:4 0 R"]["stream"]["datafile"],
            qpdf_json["qpdf"][1]["obj:4 0 R"]["stream"]["datafile"]
        );
    }
}

#[test]
fn json_qpdf_preparation_prefers_a_live_reuse_of_an_xref_stream_ref() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture
        .write_all(&reused_historical_xref_stream_json_pdf())
        .unwrap();
    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=qpdf", "--json-object=4,0"])
        .arg(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json_qpdf_metadata(&json)["maxobjectid"], 99);
    assert_eq!(
        json["qpdf"][1]["obj:4 0 R"],
        serde_json::json!({"value": {"/Marker": "/New"}})
    );
}

#[test]
fn json_qpdf_preparation_prefers_the_nearest_repeated_xref_stream_generation() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture
        .write_all(&repeated_historical_xref_stream_json_pdf())
        .unwrap();
    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=qpdf", "--json-object=4,0"])
        .arg(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json_qpdf_metadata(&json)["maxobjectid"], 92);
    let dict = &json["qpdf"][1]["obj:4 0 R"]["stream"]["dict"];
    assert_eq!(dict["/Marker"], serde_json::json!("/Near"));
    assert!(dict.get("/Info").is_none());
}

#[test]
fn json_qpdf_preparation_keeps_historical_refs_when_repair_stops_a_prev_cycle() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture
        .write_all(&circular_historical_trailer_json_pdf())
        .unwrap();
    let strict = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=qpdf"])
        .arg(fixture.path())
        .output()
        .unwrap();
    assert!(!strict.status.success());

    let repaired = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--repair", "--json=2", "--json-key=qpdf"])
        .arg(fixture.path())
        .output()
        .unwrap();
    assert_eq!(repaired.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&repaired.stderr);
    assert_eq!(stderr.matches("WARNING:").count(), 3, "{stderr}");
    assert!(stderr.ends_with(&format!("flpdf: operation succeeded with warnings{EOL}")));
    let json: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(json_qpdf_metadata(&json)["maxobjectid"], 99);
    assert_eq!(
        json["qpdf"][1]["obj:99 0 R"],
        serde_json::json!({"value": null})
    );
    assert!(json["qpdf"][1]["trailer"]["value"].get("/Info").is_none());
}

#[test]
fn json_repair_preserves_refs_and_xref_stream_after_a_late_malformed_prev() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture
        .write_all(&malformed_late_prev_after_valid_sections_json_pdf())
        .unwrap();
    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--repair", "--json=2", "--json-key=qpdf"])
        .arg(fixture.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("WARNING:").count(), 3, "{stderr}");
    assert!(stderr.ends_with(&format!("flpdf: operation succeeded with warnings{EOL}")));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let objects = json["qpdf"][1].as_object().unwrap();
    for key in ["obj:99 0 R", "obj:88 4 R", "obj:70 3 R", "obj:50 2 R"] {
        assert!(objects.contains_key(key), "missing {key}: {json}");
    }
    assert_eq!(objects["obj:4 0 R"]["stream"]["dict"]["/Type"], "/XRef");
}

#[test]
fn json_open_warnings_precede_fatal_output_error_without_success_summary() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture
        .write_all(&malformed_late_prev_after_valid_sections_json_pdf())
        .unwrap();
    let output_directory = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--repair", "--json=2", "--json-key=qpdf", "--json-output=2"])
        .arg(fixture.path())
        .arg(output_directory.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("WARNING:").count(), 3, "{stderr}");
    let last_warning = stderr.rfind("WARNING:").unwrap();
    let fatal = stderr.rfind("flpdf:").unwrap();
    assert!(last_warning < fatal, "{stderr}");
    assert!(!stderr.contains("operation succeeded with warnings"));
}

#[test]
#[ignore = "live qpdf 11.9.0 historical incremental trailer JSON oracle"]
fn live_qpdf_historical_incremental_trailer_json_matches() {
    for bytes in [
        historical_xref_stream_json_pdf_with_free_generation(0),
        historical_xref_stream_json_pdf_with_free_generation(1),
        reused_historical_xref_stream_json_pdf(),
        multigeneration_historical_trailer_json_pdf(),
    ] {
        let mut fixture = tempfile::NamedTempFile::new().unwrap();
        fixture.write_all(&bytes).unwrap();
        for args in [
            &["--json=2", "--json-key=qpdf", "--json-object=trailer"][..],
            &["--json=2", "--json-key=qpdf", "--json-object=99"][..],
            &["--json=2", "--json-key=qpdf", "--json-object=88,4"][..],
            &["--json=2", "--json-key=qpdf"][..],
        ] {
            let qpdf = std::process::Command::new("qpdf")
                .args(args)
                .arg(fixture.path())
                .output()
                .unwrap();
            let flpdf = Command::cargo_bin("flpdf")
                .unwrap()
                .args(args)
                .arg(fixture.path())
                .output()
                .unwrap();
            assert_eq!(flpdf.status.code(), qpdf.status.code(), "{args:?}");
            assert_eq!(flpdf.stderr, qpdf.stderr, "{args:?}");
            let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
            let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
            assert_eq!(flpdf_json["qpdf"], qpdf_json["qpdf"], "{args:?}");
        }
    }
}

#[test]
#[ignore = "live qpdf 11.9.0 repeated historical xref stream cache oracle"]
fn live_qpdf_repeated_historical_xref_stream_json_matches() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture
        .write_all(&repeated_historical_xref_stream_json_pdf())
        .unwrap();
    for args in [
        &["--json=2", "--json-key=qpdf", "--json-object=4,0"][..],
        &["--json=2", "--json-key=qpdf"][..],
    ] {
        let qpdf = std::process::Command::new("qpdf")
            .args(args)
            .arg(fixture.path())
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(fixture.path())
            .output()
            .unwrap();
        assert_eq!(flpdf.status.code(), qpdf.status.code(), "{args:?}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "{args:?}");
        let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
        let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
        assert_eq!(flpdf_json["qpdf"], qpdf_json["qpdf"], "{args:?}");
    }
}

#[test]
#[ignore = "live qpdf 11.9.0 historical trailer through /Prev cycle oracle"]
fn live_qpdf_prev_cycle_historical_trailer_json_matches() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture
        .write_all(&circular_historical_trailer_json_pdf())
        .unwrap();
    let qpdf = std::process::Command::new("qpdf")
        .args(["--json=2", "--json-key=qpdf"])
        .arg(fixture.path())
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--repair", "--json=2", "--json-key=qpdf"])
        .arg(fixture.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .output()
        .unwrap();
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stderr, qpdf.stderr);
    let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
    let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
    assert_eq!(flpdf_json["qpdf"], qpdf_json["qpdf"]);
}

#[test]
#[ignore = "live qpdf 11.9.0 malformed late /Prev repair oracle"]
fn live_qpdf_malformed_late_prev_json_matches() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture
        .write_all(&malformed_late_prev_after_valid_sections_json_pdf())
        .unwrap();
    let args = ["--json=2", "--json-key=qpdf"];
    let qpdf = std::process::Command::new("qpdf")
        .args(args)
        .arg(fixture.path())
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--repair", "--json=2", "--json-key=qpdf"])
        .arg(fixture.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stderr, qpdf.stderr);
    let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
    let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
    assert_eq!(flpdf_json["qpdf"], qpdf_json["qpdf"]);
}

#[test]
#[ignore = "live qpdf 11.9.0 empty-object diagnostic oracle"]
fn live_qpdf_empty_object_json_matches() {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    fixture.write_all(&empty_object_json_pdf()).unwrap();
    let args = ["--json=2", "--json-key=qpdf", "--json-object=7"];
    let qpdf = std::process::Command::new("qpdf")
        .args(args)
        .arg(fixture.path())
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(fixture.path())
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
    let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
    assert_eq!(flpdf_json["qpdf"], qpdf_json["qpdf"]);
    for stderr in [&qpdf.stderr, &flpdf.stderr] {
        let stderr = String::from_utf8_lossy(stderr);
        assert_eq!(stderr.matches("empty object treated as null").count(), 1);
        assert_eq!(
            stderr.matches("operation succeeded with warnings").count(),
            1
        );
    }
    let warning_suffix = |stderr: &[u8]| {
        String::from_utf8_lossy(stderr)
            .lines()
            .find(|line| line.contains("empty object treated as null"))
            .and_then(|line| line.split_once("(object"))
            .map(|(_, suffix)| suffix.to_string())
            .unwrap()
    };
    assert_eq!(warning_suffix(&flpdf.stderr), warning_suffix(&qpdf.stderr));
}

#[test]
fn json_qpdf_dangling_fixture_selectors_and_all_match_raw_rules() {
    let input = "../../tests/fixtures/compat/dangling-body-one-page.pdf";
    let cases: [(&str, &[&str], &[&str]); 3] = [
        (
            "trailer",
            &["--json=2", "--json-key=qpdf", "--json-object=trailer"],
            &["trailer"],
        ),
        (
            "dangling",
            &["--json=2", "--json-key=qpdf", "--json-object=99"],
            &["obj:99 0 R"],
        ),
        (
            "all",
            &["--json=2", "--json-key=qpdf"],
            &["obj:4 0 R", "obj:99 0 R", "trailer"],
        ),
    ];

    for (label, args, expected_keys) in cases {
        let output = Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(input)
            .output()
            .unwrap();
        assert!(output.status.success(), "{label}");
        assert!(output.stderr.is_empty(), "{label}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json_qpdf_metadata(&json)["maxobjectid"], 99, "{label}");
        let map = json["qpdf"][1].as_object().unwrap();
        if label == "all" {
            assert!(expected_keys.iter().all(|key| map.contains_key(*key)));
        } else {
            assert_eq!(
                map.keys().map(String::as_str).collect::<Vec<_>>(),
                expected_keys,
                "{label}"
            );
        }
        if label != "trailer" {
            assert_eq!(map["obj:99 0 R"], serde_json::json!({"value": null}));
        }
        if label == "all" {
            let catalog = &map["obj:4 0 R"]["value"];
            assert!(catalog.get("/Bad").is_none());
            assert!(catalog.get("/Junk").is_none());
            assert_eq!(catalog["/Nested"], serde_json::json!({}));
            assert_eq!(catalog["/ArrZero"], serde_json::json!([null]));
        }
    }
}

#[test]
#[ignore = "live qpdf 11.9.0 dangling selectors/all oracle"]
fn live_qpdf_dangling_fixture_selectors_and_all_match() {
    let input = "../../tests/fixtures/compat/dangling-body-one-page.pdf";
    let cases: [&[&str]; 3] = [
        &["--json=2", "--json-key=qpdf", "--json-object=trailer"],
        &["--json=2", "--json-key=qpdf", "--json-object=99"],
        &["--json=2", "--json-key=qpdf"],
    ];
    for args in cases {
        let qpdf = std::process::Command::new("qpdf")
            .args(args)
            .arg(input)
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(input)
            .output()
            .unwrap();
        assert_eq!(flpdf.status.code(), qpdf.status.code(), "{args:?}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "{args:?}");
        let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
        let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
        assert_eq!(flpdf_json["qpdf"], qpdf_json["qpdf"], "{args:?}");
    }
}

#[test]
#[ignore = "live qpdf 11.9.0 compressed dangling JSON oracle"]
fn live_qpdf_compressed_dangling_json_matches() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.pdf");
    let compressed = temp.path().join("compressed.pdf");
    std::fs::write(&input, dangling_container_json_pdf()).unwrap();
    let generate = std::process::Command::new("qpdf")
        .args(["--object-streams=generate"])
        .arg(&input)
        .arg(&compressed)
        .output()
        .unwrap();
    assert!(
        generate.status.success(),
        "{}",
        String::from_utf8_lossy(&generate.stderr)
    );
    assert!(contains(&std::fs::read(&compressed).unwrap(), b"/ObjStm"));

    let args = ["--json=2", "--json-key=qpdf"];
    let qpdf = std::process::Command::new("qpdf")
        .args(args)
        .arg(&compressed)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(&compressed)
        .output()
        .unwrap();
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stderr, qpdf.stderr);
    let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
    let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
    assert_eq!(flpdf_json["qpdf"], qpdf_json["qpdf"]);
}

#[test]
#[ignore = "live qpdf 11.9.0 selected JSON section oracle"]
fn live_qpdf_selected_json_sections_match_construction_side_effects() {
    struct Case {
        label: &'static str,
        args: &'static [&'static str],
        has_outlines: bool,
        has_qpdf: bool,
        warned: bool,
    }

    let cases = [
        Case {
            label: "qpdf only",
            args: &["--json=2", "--json-key=qpdf"],
            has_outlines: false,
            has_qpdf: true,
            warned: false,
        },
        Case {
            label: "outlines only",
            args: &["--json=2", "--json-key=outlines"],
            has_outlines: true,
            has_qpdf: false,
            warned: true,
        },
        Case {
            label: "outlines plus qpdf",
            args: &["--json=2", "--json-key=outlines", "--json-key=qpdf"],
            has_outlines: true,
            has_qpdf: true,
            warned: true,
        },
        Case {
            label: "full",
            args: &["--json=2"],
            has_outlines: true,
            has_qpdf: true,
            warned: true,
        },
    ];

    for case in cases {
        let fixture = fixture_with_repaired_name_tree_and_stream();
        let qpdf = std::process::Command::new("qpdf")
            .args(case.args)
            .arg(fixture.path())
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .args(case.args)
            .arg(fixture.path())
            .output()
            .unwrap();

        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "{}: qpdf stderr={} flpdf stderr={}",
            case.label,
            String::from_utf8_lossy(&qpdf.stderr),
            String::from_utf8_lossy(&flpdf.stderr)
        );

        let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
        let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
        let qpdf_keys = qpdf_json.as_object().unwrap().keys().collect::<Vec<_>>();
        let flpdf_keys = flpdf_json.as_object().unwrap().keys().collect::<Vec<_>>();
        assert_eq!(flpdf_keys, qpdf_keys, "{}", case.label);
        assert_eq!(
            flpdf_json.get("outlines").is_some(),
            case.has_outlines,
            "{}",
            case.label
        );
        assert_eq!(
            flpdf_json.get("qpdf").is_some(),
            case.has_qpdf,
            "{}",
            case.label
        );

        if case.has_outlines {
            assert_eq!(
                flpdf_json["outlines"][0]["dest"], qpdf_json["outlines"][0]["dest"],
                "{}",
                case.label
            );
        }
        if case.has_qpdf {
            assert_eq!(
                json_qpdf_metadata(&flpdf_json),
                json_qpdf_metadata(&qpdf_json),
                "{}: complete qpdf metadata",
                case.label
            );
            for object in ["obj:1 0 R", "obj:8 0 R"] {
                assert_eq!(
                    flpdf_json["qpdf"][1][object], qpdf_json["qpdf"][1][object],
                    "{}: {object}",
                    case.label
                );
            }
        }

        let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr);
        let flpdf_stderr = String::from_utf8_lossy(&flpdf.stderr);
        let expected_count = usize::from(case.warned);
        assert_eq!(
            qpdf_stderr
                .matches("attempting to repair after error:")
                .count(),
            expected_count,
            "{}: {qpdf_stderr}",
            case.label
        );
        assert_eq!(
            flpdf_stderr
                .matches("attempting to repair after error:")
                .count(),
            expected_count,
            "{}: {flpdf_stderr}",
            case.label
        );
        assert_eq!(
            qpdf_stderr
                .matches("operation succeeded with warnings")
                .count(),
            expected_count,
            "{}: {qpdf_stderr}",
            case.label
        );
        assert_eq!(
            flpdf_stderr
                .matches("operation succeeded with warnings")
                .count(),
            expected_count,
            "{}: {flpdf_stderr}",
            case.label
        );
        if case.warned {
            assert!(
                qpdf_stderr
                    .find("attempting to repair after error:")
                    .unwrap()
                    < qpdf_stderr
                        .find("operation succeeded with warnings")
                        .unwrap(),
                "{}: {qpdf_stderr}",
                case.label
            );
            assert!(
                flpdf_stderr
                    .find("attempting to repair after error:")
                    .unwrap()
                    < flpdf_stderr
                        .find("operation succeeded with warnings")
                        .unwrap(),
                "{}: {flpdf_stderr}",
                case.label
            );
        }
    }
}

#[test]
#[ignore = "live qpdf 11.9.0 JSON metadata oracle"]
fn live_qpdf_json_metadata_matches_inherited_resources_and_outline_allocation() {
    let inherited = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/inherited-resources-one-page.pdf");
    let allocation = fixture_with_name_tree_repair_allocations();
    let cases = [
        (
            "inherited qpdf only",
            inherited.as_path(),
            &["--json=2", "--json-key=qpdf"][..],
        ),
        (
            "inherited pages then qpdf",
            inherited.as_path(),
            &["--json=2", "--json-key=pages", "--json-key=qpdf"][..],
        ),
        (
            "inherited pagelabels then qpdf",
            inherited.as_path(),
            &["--json=2", "--json-key=pagelabels", "--json-key=qpdf"][..],
        ),
        (
            "inherited acroform then qpdf",
            inherited.as_path(),
            &["--json=2", "--json-key=acroform", "--json-key=qpdf"][..],
        ),
        (
            "inherited attachments then qpdf",
            inherited.as_path(),
            &["--json=2", "--json-key=attachments", "--json-key=qpdf"][..],
        ),
        (
            "inherited encrypt then qpdf",
            inherited.as_path(),
            &["--json=2", "--json-key=encrypt", "--json-key=qpdf"][..],
        ),
        (
            "inherited outlines then qpdf",
            inherited.as_path(),
            &["--json=2", "--json-key=outlines", "--json-key=qpdf"][..],
        ),
        (
            "inherited non-page combination",
            inherited.as_path(),
            &[
                "--json=2",
                "--json-key=attachments",
                "--json-key=encrypt",
                "--json-key=qpdf",
            ][..],
        ),
        (
            "inherited mixed combination",
            inherited.as_path(),
            &[
                "--json=2",
                "--json-key=qpdf",
                "--json-key=attachments",
                "--json-key=outlines",
            ][..],
        ),
        (
            "inherited full document",
            inherited.as_path(),
            &["--json=2"][..],
        ),
        (
            "outline repair allocation",
            allocation.path(),
            &["--json=2", "--json-key=outlines", "--json-key=qpdf"][..],
        ),
    ];

    for (label, fixture, args) in cases {
        let qpdf = std::process::Command::new("qpdf")
            .args(args)
            .arg(fixture)
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(fixture)
            .output()
            .unwrap();
        assert_eq!(flpdf.status.code(), qpdf.status.code(), "{label}");
        let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
        let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
        assert_eq!(
            json_qpdf_metadata(&flpdf_json),
            json_qpdf_metadata(&qpdf_json),
            "{label}"
        );
        assert_eq!(
            flpdf_json["qpdf"][1]["obj:3 0 R"]["value"].get("/Resources"),
            qpdf_json["qpdf"][1]["obj:3 0 R"]["value"].get("/Resources"),
            "{label}"
        );
    }
}

#[test]
fn json_processing_warnings_do_not_repeat_open_time_warnings() {
    let fixture = corrupt_xref_repaired_name_tree_fixture();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let assert = cmd
        .args(["--repair", "--json=2", "--json-key=outlines"])
        .arg(fixture.path())
        .assert()
        .code(3);
    let output = assert.get_output();
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    let open_warning = stderr
        .lines()
        .find(|line| line.starts_with("WARNING:") && !line.contains("attempting to repair"))
        .unwrap_or_else(|| panic!("missing open-time warning in {stderr}"));
    assert_eq!(
        stderr.lines().filter(|line| *line == open_warning).count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("attempting to repair after error:").count(),
        1
    );
    assert_eq!(
        stderr
            .matches("flpdf: operation succeeded with warnings")
            .count(),
        1
    );
}

#[test]
fn json_output_open_error_happens_before_json_processing() {
    let fixture = fixture_with_repaired_name_tree_and_stream();
    let output_directory = tempfile::tempdir().unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let assert = cmd
        .args(["--json=2", "--json-key=outlines", "--json-output=2"])
        .arg(fixture.path())
        .arg(output_directory.path())
        .assert()
        .code(2);
    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("attempting to repair after error:").count(),
        0,
        "{stderr}"
    );
    assert!(stderr.contains("flpdf:"), "{stderr}");
    assert!(!stderr.contains("operation succeeded with warnings"));
    assert!(output.stdout.is_empty());
}

#[test]
fn json_output_missing_input_preserves_existing_output_and_reports_input_open() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("missing.pdf");
    let output_path = temp.path().join("out.json");
    let original_output = b"existing JSON output must remain unchanged\n";
    std::fs::write(&output_path, original_output).unwrap();

    let expected_error = format!(
        "flpdf: open {}: {}",
        input.display(),
        normalized_os_message(&File::open(&input).unwrap_err())
    );
    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json-output=2"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(&output_path).unwrap(), original_output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.lines().last(),
        Some(expected_error.as_str()),
        "{stderr}"
    );
    assert!(
        !stderr.contains("unable to inspect --json-output file"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn json_output_writes_to_dev_null_without_stdout() {
    // Regression for unconditional set_len/seek after the output identity
    // check: /dev/null is writable but rejects truncation. Other non-regular
    // sinks, such as FIFOs, may also reject seek. The CLI must still treat it
    // as a JSON sink.
    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--json-output=2",
            "../../tests/fixtures/minimal.pdf",
            "/dev/null",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}

#[test]
fn json_side_file_error_emits_recorded_warning_after_partial_json_before_fatal_error() {
    let fixture = fixture_with_repaired_name_tree_and_stream();
    let temp = tempfile::tempdir().unwrap();
    let missing_prefix = temp.path().join("missing").join("stream");
    let missing_prefix_arg = format!("--json-stream-prefix={}", missing_prefix.display());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let assert = cmd
        .args(["--json=2", "--json-stream-data=file"])
        .arg(&missing_prefix_arg)
        .arg(fixture.path())
        .assert()
        .code(2);
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("{\n"), "{stdout}");
    assert!(
        stdout.contains("\"dest\": [\n        \"3 0 R\""),
        "{stdout}"
    );
    assert!(stdout.contains(r#""obj:6 0 R": {"#), "{stdout}");
    assert!(
        stdout.ends_with("\"stream\": "),
        "qpdf opens the final side file immediately after the stream key: {stdout}"
    );
    assert!(
        !stdout[stdout.rfind("\"stream\": ").unwrap()..].contains('{'),
        "the stream value must not start before the side-file open succeeds: {stdout}"
    );
    assert!(
        !stdout[stdout.rfind("\"stream\": ").unwrap()..].contains("datafile"),
        "the datafile member must not start before the side-file open succeeds: {stdout}"
    );
    let json_error = serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap_err();
    assert!(json_error.is_eof(), "{json_error}: {stdout}");
    assert!(!stdout.ends_with("\n}\n"), "{stdout}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let warning = stderr
        .find("attempting to repair after error:")
        .unwrap_or_else(|| panic!("missing repair warning in {stderr}"));
    let fatal = stderr
        .rfind("flpdf:")
        .unwrap_or_else(|| panic!("missing fatal error in {stderr}"));
    assert!(warning < fatal, "{stderr}");
    let side_path = format!("{}-6", missing_prefix.display());
    let open_error = std::fs::File::create(&side_path).unwrap_err();
    let expected_fatal = format!(
        "flpdf: open {side_path}: {}",
        normalized_os_message(&open_error)
    );
    assert_eq!(
        stderr.lines().last(),
        Some(expected_fatal.as_str()),
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("attempting to repair after error:").count(),
        1
    );
    assert!(!stderr.contains("operation succeeded with warnings"));
}

#[test]
fn json_and_side_files_complete_before_warning_exit_three() {
    let fixture = fixture_with_repaired_name_tree_and_stream();
    let temp = tempfile::tempdir().unwrap();
    let json_path = temp.path().join("out.json");
    let prefix = temp.path().join("stream");
    let prefix_arg = format!("--json-stream-prefix={}", prefix.display());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let assert = cmd
        .args(["--json=2", "--json-stream-data=file"])
        .arg(&prefix_arg)
        .arg(fixture.path())
        .arg(&json_path)
        .assert()
        .code(3);
    let output = assert.get_output();
    assert!(output.stdout.is_empty());
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
    assert_eq!(json["outlines"][0]["dest"][0], "3 0 R");
    assert!(std::path::Path::new(&format!("{}-6", prefix.display())).is_file());

    let stderr = String::from_utf8_lossy(&output.stderr);
    let warning = stderr.find("attempting to repair after error:").unwrap();
    let summary = stderr
        .find("flpdf: operation succeeded with warnings")
        .unwrap();
    assert!(warning < summary, "{stderr}");
    assert!(
        stderr.contains(
            "flpdf: operation succeeded with warnings; resulting file may have some problems"
        ),
        "JSON written to a path is an output-producing operation: {stderr}"
    );
}

#[test]
fn show_npages_prints_total_pages() {
    let fixture = fixture_with_nested_pages();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-npages", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("2"));
}

#[test]
fn show_pages_lists_each_page() {
    let fixture = fixture_with_nested_pages();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-pages", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("page 1: 3 0 R"))
        .stdout(predicate::str::contains("page 2: 6 0 R"))
        .stdout(predicate::str::contains(format!("  content:{EOL}")))
        .stdout(predicate::str::contains("    5 0 R"))
        .stdout(predicate::str::contains("    7 0 R"));
}

#[test]
fn show_xref_prints_qpdf_effective_table() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-xref", "../../tests/fixtures/compat/one-page.pdf"])
        .assert()
        .success()
        .stdout(format!(
            "1/0: uncompressed; offset = 61{EOL}\
             2/0: uncompressed; offset = 92{EOL}\
             3/0: uncompressed; offset = 199{EOL}\
             4/0: uncompressed; offset = 392{EOL}\
             5/0: uncompressed; offset = 460{EOL}\
             6/0: uncompressed; offset = 721{EOL}\
             7/0: uncompressed; offset = 780{EOL}"
        ));

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--show-xref",
            "../../tests/fixtures/compat/three-page-objstm.pdf",
        ])
        .assert()
        .success()
        .stdout(format!(
            "1/0: uncompressed; offset = 15{EOL}\
             2/0: compressed; stream = 1, index = 0{EOL}\
             3/0: compressed; stream = 1, index = 1{EOL}\
             4/0: compressed; stream = 1, index = 2{EOL}\
             5/0: compressed; stream = 1, index = 3{EOL}\
             6/0: compressed; stream = 1, index = 4{EOL}\
             7/0: compressed; stream = 1, index = 5{EOL}\
             8/0: compressed; stream = 1, index = 6{EOL}\
             9/0: compressed; stream = 1, index = 7{EOL}\
             10/0: uncompressed; offset = 532{EOL}\
             11/0: uncompressed; offset = 685{EOL}\
             12/0: uncompressed; offset = 838{EOL}\
             13/0: uncompressed; offset = 991{EOL}"
        ));
}

fn fixture_with_short_first_name_tree_pair() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    let objects = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names << /Dests << /Names [(m)] >> >> >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".as_slice(),
        b"4 0 obj\n<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>\nendobj\n".as_slice(),
        b"5 0 obj\n<< /Title (One) /Parent 4 0 R /Dest (m) >>\nendobj\n".as_slice(),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );
    fixture.as_file_mut().write_all(&bytes).unwrap();
    fixture
}

fn fixture_with_name_tree_repair_allocations() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    let pairs = (0..33)
        .map(|index| format!("(k{index:02}) [3 0 R /Fit]"))
        .collect::<Vec<_>>()
        .join(" ");
    let objects = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names << /Dests << /Kids [8 0 R] >> >> >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".to_vec(),
        b"4 0 obj\n<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>\nendobj\n".to_vec(),
        b"5 0 obj\n<< /Title (One) /Parent 4 0 R /Dest (k17) >>\nendobj\n".to_vec(),
        b"6 0 obj\nnull\nendobj\n".to_vec(),
        b"7 0 obj\nnull\nendobj\n".to_vec(),
        format!("8 0 obj\n<< /Names [{pairs}] >>\nendobj\n").into_bytes(),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in &objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    fixture.as_file_mut().write_all(&bytes).unwrap();
    fixture
}

fn fixture_with_repaired_name_tree_and_stream() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();
    let objects = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names << /Dests << /Kids [8 0 R] >> >> >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>\nendobj\n".as_slice(),
        b"4 0 obj\n<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>\nendobj\n".as_slice(),
        b"5 0 obj\n<< /Title (One) /Parent 4 0 R /Dest (shape) >>\nendobj\n".as_slice(),
        b"6 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n".as_slice(),
        b"7 0 obj\nnull\nendobj\n".as_slice(),
        b"8 0 obj\n<< /Names [(shape) [3 0 R /Fit]] >>\nendobj\n".as_slice(),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    fixture.as_file_mut().write_all(&bytes).unwrap();
    fixture
}

fn corrupt_xref_repaired_name_tree_fixture() -> tempfile::NamedTempFile {
    let fixture = fixture_with_repaired_name_tree_and_stream();
    let mut bytes = std::fs::read(fixture.path()).unwrap();
    let xref = bytes
        .windows(4)
        .position(|window| window == b"xref")
        .expect("fixture must contain xref");
    bytes[xref + 2] = b'z';
    std::fs::write(fixture.path(), bytes).unwrap();
    fixture
}

fn fixture_with_orphan_object() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();

    let object1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
    let object2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n";
    let object3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R >>\nendobj\n";
    let content_data = b"Hello PDF";
    let object4 = format!(
        "4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        content_data.len(),
        String::from_utf8_lossy(content_data)
    )
    .into_bytes();
    let object5 = b"5 0 obj\n<< /Type /Orphan >>\nendobj\n";

    let objects = vec![
        object1.to_vec(),
        object2.to_vec(),
        object3.to_vec(),
        object4,
        object5.to_vec(),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len() + 1);
    for object in &objects {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(format!("{:010} 65535 f\n", 0).as_bytes());
    for &offset in &offsets {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );

    fixture.as_file_mut().write_all(&bytes).unwrap();

    fixture
}

fn fixture_with_nested_pages() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();

    let object1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
    let object2 = b"2 0 obj\n<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] /MediaBox [0 0 595.28 841.89] >>\nendobj\n";
    let object3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 842] /Contents 5 0 R >>\nendobj\n";
    let object4 = b"4 0 obj\n<< /Type /Pages /Count 1 /Kids [6 0 R] /Rotate 90 >>\nendobj\n";
    let object5 = b"5 0 obj\n<< /Length 14 >>\nstream\nBT (one) Tj ET\nendstream\nendobj\n";
    let object6 = b"6 0 obj\n<< /Type /Page /Parent 4 0 R /Rotate 90 /MediaBox [0 0 200 100] /Contents 7 0 R >>\nendobj\n";
    let object7 = b"7 0 obj\n<< /Length 15 >>\nstream\nBT (two) Tj ET\nendstream\nendobj\n";
    let objects = vec![
        object1.to_vec(),
        object2.to_vec(),
        object3.to_vec(),
        object4.to_vec(),
        object5.to_vec(),
        object6.to_vec(),
        object7.to_vec(),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len() + 1);
    for object in &objects {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(format!("{:010} 65535 f\n", 0).as_bytes());
    for &offset in &offsets {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );

    fixture.write_all(&bytes).unwrap();

    fixture
}

fn corrupt_xref_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
    let obj2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec();
    let obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R >>\nendobj\n".to_vec();
    let obj4 = b"4 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n".to_vec();

    let mut offsets = Vec::new();
    for object in &[obj1, obj2, obj3, obj4] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f\n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }

    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );

    let mut corrupted = bytes;
    let Some(pos) = corrupted.windows(4).position(|window| window == b"xref") else {
        unreachable!("fixture should contain xref token")
    };
    if let Some(byte) = corrupted.get_mut(pos + 2) {
        *byte = b'z';
    }

    corrupted
}

fn signed_acroform_pdf() -> Vec<u8> {
    let objects: Vec<&[u8]> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        b"4 0 obj\n<< /Fields [5 0 R] /SigFlags 3 >>\nendobj\n",
        b"5 0 obj\n<< /FT /Sig /T (Approval) /V 6 0 R /Rect [0 0 0 0] >>\nendobj\n",
        b"6 0 obj\n<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>\nendobj\n",
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }

    let xref_start = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    bytes
}

/// A certified PDF whose only signature lives in the catalog `/Perms /DocMDP`
/// dictionary — there is NO `/AcroForm`. `--remove-restrictions` drops `/Perms`
/// unconditionally (matching qpdf's `removeSecurityRestrictions`), orphaning the
/// DocMDP signature dict (obj 4, referenced only via `/Perms`) so it is
/// garbage-collected on the full rewrite.
fn signed_perms_docmdp_pdf() -> Vec<u8> {
    let objects: Vec<&[u8]> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Perms << /DocMDP 4 0 R >> >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        b"4 0 obj\n<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>\nendobj\n",
    ];
    build_classic_pdf(&objects)
}

/// Like [`signed_acroform_pdf`], but the signature field is a `/Widget`
/// annotation that is ALSO referenced from the page's `/Annots`. After
/// `--remove-restrictions` the field is erased from `/AcroForm /Fields` and its
/// `/FT` `/V` are stripped, but the object survives (as a plain widget) because
/// the page still references it. The orphaned signature dictionary (obj 6) is
/// garbage-collected.
fn signed_widget_acroform_pdf() -> Vec<u8> {
    let objects: Vec<&[u8]> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>\nendobj\n",
        b"4 0 obj\n<< /Fields [5 0 R] /SigFlags 3 >>\nendobj\n",
        b"5 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Sig /T (Approval) /V 6 0 R /Rect [10 20 30 40] /P 3 0 R >>\nendobj\n",
        b"6 0 obj\n<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>\nendobj\n",
    ];
    build_classic_pdf(&objects)
}

/// Assemble a classic (uncompressed) PDF from pre-serialized `N 0 obj … endobj`
/// bodies, emitting a contiguous xref table (obj 0 free + one `n` entry per
/// body) and a trailer with `/Size` = bodies + 1. Mirrors the xref-building
/// style of [`signed_acroform_pdf`].
fn build_classic_pdf(objects: &[&[u8]]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let size = offsets.len() + 1;
    let xref_start = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}

fn one_page_pdf_with_content(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>\nendobj\n",
        stream.as_slice(),
    ])
}

fn stream_data_offset(bytes: &[u8]) -> u64 {
    let marker = b"stream\n";
    let marker_pos = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("fixture must contain a newline-terminated stream marker");
    u64::try_from(marker_pos + marker.len()).expect("fixture offset fits u64")
}

/// One page whose effective `/Resources` is an indirect dictionary inherited
/// from the `/Pages` node. The `--pages` resource-prune route must shallow-copy
/// that dictionary onto the selected leaf before pruning, matching qpdf's
/// `QPDFPageObjectHelper::getAttribute("/Resources", true)` boundary.
fn one_page_pdf_with_inherited_indirect_resources() -> Vec<u8> {
    let content = b"q Q";
    let stream = [
        format!("5 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Contents 5 0 R >>\nendobj\n",
        b"4 0 obj\n<< >>\nendobj\n",
        stream.as_slice(),
    ])
}

/// One page with a page-local indirect `/Resources` dictionary. qpdf's
/// `--pages` Auto heuristic leaves this reference alone when the source has
/// no shared resource dictionary, shared XObject dictionary, or inherited
/// resources to trigger pruning.
fn one_page_pdf_with_page_local_indirect_resources() -> Vec<u8> {
    let content = b"q Q";
    let stream = [
        format!("5 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Resources 4 0 R /Contents 5 0 R >>\nendobj\n",
        b"4 0 obj\n<< /Font << /F1 6 0 R >> >>\nendobj\n",
        stream.as_slice(),
        b"6 0 obj\n<< /BaseFont /Helvetica /Subtype /Type1 /Type /Font >>\nendobj\n",
    ])
}

fn qpdf_11_9_available() -> bool {
    std::process::Command::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .map(str::trim)
                    == Some("qpdf version 11.9.0")
        })
        .unwrap_or(false)
}

fn image_xobject(num: u32) -> Vec<u8> {
    let mut object = format!(
        "{num} 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
         /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n"
    )
    .into_bytes();
    object.push(0);
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

fn two_page_pdf_with_shared_xobject_category() -> Vec<u8> {
    let content1 = [
        b"7 0 obj\n<< /Length 10 >>\nstream\n".as_slice(),
        b"q /X1 Do Q",
        b"\nendstream\nendobj\n",
    ]
    .concat();
    let content2 = [
        b"8 0 obj\n<< /Length 10 >>\nstream\n".as_slice(),
        b"q /X2 Do Q",
        b"\nendstream\nendobj\n",
    ]
    .concat();
    let image1 = image_xobject(10);
    let image2 = image_xobject(11);
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Contents 7 0 R /Resources 5 0 R >>\nendobj\n",
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Contents 8 0 R /Resources 6 0 R >>\nendobj\n",
        b"5 0 obj\n<< /XObject 9 0 R >>\nendobj\n",
        b"6 0 obj\n<< /XObject 9 0 R >>\nendobj\n",
        content1.as_slice(),
        content2.as_slice(),
        b"9 0 obj\n<< /X1 10 0 R /X2 11 0 R >>\nendobj\n",
        image1.as_slice(),
        image2.as_slice(),
    ])
}

fn one_page_pdf_with_inherited_other_resource_categories() -> Vec<u8> {
    let content = b"BT /F1 12 Tf ET";
    let stream = [
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    let image = image_xobject(6);
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 5 0 R >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Contents 4 0 R >>\nendobj\n",
        stream.as_slice(),
        b"5 0 obj\n<< /Font << /F1 << /Type /Font >> /UnusedFont << /Type /Font >> >> /XObject << /UnusedXObject 6 0 R >> /ColorSpace << /UnusedColorSpace /DeviceRGB >> /Pattern << /UnusedPattern << >> >> /Shading << /UnusedShading << >> >> /ExtGState << /UnusedExtGState << >> >> /Properties << /UnusedProperties << >> >> >>\nendobj\n",
        image.as_slice(),
    ])
}

fn resource_category_keys_from_path(
    path: &std::path::Path,
    page_index: usize,
    category: &str,
) -> Vec<String> {
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let page_ref = pages::page_refs(&mut pdf).unwrap()[page_index];
    let page = pdf.resolve_canonical_object(page_ref).unwrap();
    let resources = page
        .try_get_key(b"/Resources")
        .expect("page resources should exist");
    let category_key = format!("/{category}");
    let category = resources
        .try_get_key(category_key.as_bytes())
        .expect("resource category should be readable");
    pdf.resolve(&category).expect("resolve resource category");
    category
        .as_dictionary()
        .unwrap_or_default()
        .keys()
        .map(|name| String::from_utf8(name.strip_prefix(b"/").unwrap_or(name).to_vec()).unwrap())
        .collect()
}

fn one_page_pdf_with_indirect_contents_array(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!("5 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>\nendobj\n",
        b"4 0 obj\n[5 0 R]\nendobj\n",
        stream.as_slice(),
    ])
}

fn one_page_pdf_with_indirect_filtered_contents_array(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!(
            "5 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
            content.len()
        )
        .into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>\nendobj\n",
        b"4 0 obj\n[5 0 R]\nendobj\n",
        stream.as_slice(),
    ])
}

fn one_page_pdf_with_duplicate_content_array(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents [4 0 R 4 0 R] >>\nendobj\n",
        stream.as_slice(),
    ])
}

fn one_page_pdf_with_mixed_content_array(content: &[u8], extra: &[u8]) -> Vec<u8> {
    let stream = [
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    let page = [
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents [4 0 R ".as_slice(),
        extra,
        b"] >>\nendobj\n".as_slice(),
    ]
    .concat();
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        page.as_slice(),
        stream.as_slice(),
    ])
}

fn one_page_pdf_with_stale_length_and_content(content: &[u8]) -> Vec<u8> {
    let stream = [
        b"4 0 obj\n<< /Length 99 >>\nstream\n".to_vec(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>\nendobj\n",
        stream.as_slice(),
    ])
}

fn two_page_pdf_with_shared_content(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!("5 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    let objects: [&[u8]; 5] = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 5 0 R >>\nendobj\n",
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 5 0 R >>\nendobj\n",
        stream.as_slice(),
    ];
    build_classic_pdf(&objects)
}

fn two_page_pdf_with_indirect_content_aliases(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!("7 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 5 0 R >>\nendobj\n",
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 6 0 R >>\nendobj\n",
        b"5 0 obj\n[7 0 R]\nendobj\n",
        b"6 0 obj\n[7 0 R]\nendobj\n",
        stream.as_slice(),
    ])
}

fn empty_object_json_pdf() -> Vec<u8> {
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Probe 7 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
        b"4 0 obj\nnull\nendobj\n",
        b"5 0 obj\nnull\nendobj\n",
        b"6 0 obj\nnull\nendobj\n",
        b"7 0 obj\nendobj\n",
    ])
}

fn catalog_without_pages_pdf() -> Vec<u8> {
    build_classic_pdf(&[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n"])
}

fn top_level_bare_reference_json_pdf() -> Vec<u8> {
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
        b"3 0 obj\n<< /Answer 42 >>\nendobj\n",
        b"4 0 obj\n3 0 R\nendobj\n",
    ])
}

fn dangling_container_json_pdf() -> Vec<u8> {
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Nested << /Drop 99 2 R /Array [99 2 R 0 0 R] >> >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
        b"4 0 obj\n<< /Length 0 /Drop 99 2 R >>\nstream\nendstream\nendobj\n",
        b"5 0 obj\n<< /Held 99 2 R >>\nendobj\n",
        b"6 0 obj\n<< /Next 7 0 R >>\nendobj\n",
        b"7 0 obj\n<< /Next 6 0 R >>\nendobj\n",
    ])
}

fn dangling_generation_free_json_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let objects: [(u32, u16, &[u8]); 5] = [
        (
            1,
            0,
            b"<< /Type /Catalog /Pages 2 0 R /Live 8 1 R /Stale 8 0 R /Freed 20 7 R >>",
        ),
        (2, 0, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            0,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>",
        ),
        (8, 1, b"<< /Value 1 >>"),
        (9, 0, b"<< /Lazy 88 4 R >>"),
    ];
    let mut offsets = Vec::new();
    for (number, generation, body) in objects {
        offsets.push((number, generation, bytes.len()));
        bytes.extend_from_slice(format!("{number} {generation} obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    for (number, generation, offset) in offsets {
        bytes
            .extend_from_slice(format!("{number} 1\n{offset:010} {generation:05} n \n").as_bytes());
    }
    bytes.extend_from_slice(b"20 1\n0000000000 00007 f \n");
    bytes.extend_from_slice(b"200 1\n0000000000 00007 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 201 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn trailer_dangling_json_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let objects: [(u32, u16, &[u8]); 3] = [
        (1, 0, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, 0, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            0,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>",
        ),
    ];
    let mut offsets = Vec::new();
    for (number, generation, body) in objects {
        offsets.push((number, generation, bytes.len()));
        bytes.extend_from_slice(format!("{number} {generation} obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    for (number, generation, offset) in offsets {
        bytes
            .extend_from_slice(format!("{number} 1\n{offset:010} {generation:05} n \n").as_bytes());
    }
    bytes.extend_from_slice(b"200 1\n0000000000 00007 f \n");
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 201 /Root 1 0 R /Info 99 0 R /Gen 88 4 R /Zero 0 0 R /BadGen 77 65535 R >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    bytes
}

fn incremental_last_startxref(bytes: &[u8]) -> u64 {
    let marker = b"startxref\n";
    let start = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
        .unwrap()
        + marker.len();
    let end = bytes[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|length| start + length)
        .unwrap();
    std::str::from_utf8(&bytes[start..end])
        .unwrap()
        .parse()
        .unwrap()
}

fn append_json_classic_increment(
    bytes: &mut Vec<u8>,
    objects: &[(u32, u16, &str)],
    free_entries: &[(u32, u16)],
    size: u32,
    trailer_extra: &str,
    previous_xref: u64,
) -> u64 {
    let mut offsets = Vec::new();
    for (number, generation, body) in objects {
        offsets.push((*number, *generation, bytes.len()));
        bytes.extend_from_slice(format!("{number} {generation} obj\n{body}\nendobj\n").as_bytes());
    }
    let xref = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n");
    for (number, generation, offset) in offsets {
        bytes
            .extend_from_slice(format!("{number} 1\n{offset:010} {generation:05} n \n").as_bytes());
    }
    for (number, generation) in free_entries {
        bytes.extend_from_slice(format!("{number} 1\n0000000000 {generation:05} f \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root 1 0 R /Prev {previous_xref} {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    xref
}

fn incremental_json_base(trailer_extra: &str) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let objects: [(u32, &[u8]); 3] = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
    ];
    let mut offsets = Vec::new();
    for (number, body) in objects {
        offsets.push((number, bytes.len()));
        bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    for (number, offset) in offsets {
        bytes.extend_from_slice(format!("{number} 1\n{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}

fn multigeneration_historical_trailer_json_pdf() -> Vec<u8> {
    let mut bytes = incremental_json_base(
        "/Info 99 0 R /OldGen 88 4 R /Freed 20 7 R /Zero 0 0 R /BadGen 77 65535 R",
    );
    let oldest = incremental_last_startxref(&bytes);
    let middle = append_json_classic_increment(
        &mut bytes,
        &[(4, 0, "null")],
        &[],
        5,
        "/Info 60 1 R /Middle 70 3 R",
        oldest,
    );
    append_json_classic_increment(
        &mut bytes,
        &[(5, 0, "null")],
        &[(20, 7), (200, 7)],
        201,
        "/Newest 50 2 R",
        middle,
    );
    bytes
}

fn historical_xref_stream_json_pdf_with_options(
    free_generation: u16,
    previous: Option<u64>,
    latest_trailer_extra: &str,
) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in [
        (1u32, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
    ] {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_stream_offset = bytes.len() as u32;
    let mut entries = Vec::new();
    for (kind, offset, generation) in [
        (0u8, 0u32, u16::MAX),
        (1, offsets[0], 0),
        (1, offsets[1], 0),
        (1, offsets[2], 0),
        (1, xref_stream_offset, 0),
    ] {
        entries.push(kind);
        entries.extend_from_slice(&offset.to_be_bytes());
        entries.extend_from_slice(&generation.to_be_bytes());
    }
    let prev = previous.map_or_else(String::new, |offset| format!("/Prev {offset} "));
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /Info 99 0 R /Gen 88 4 R {prev}/W [1 4 2] /Index [0 5] /Length {} >>\nstream\n",
            entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&entries);
    bytes.extend_from_slice(
        format!("\nendstream\nendobj\nstartxref\n{xref_stream_offset}\n%%EOF\n").as_bytes(),
    );
    append_json_classic_increment(
        &mut bytes,
        &[(5, 0, "null")],
        &[(4, free_generation)],
        6,
        latest_trailer_extra,
        u64::from(xref_stream_offset),
    );
    bytes
}

fn historical_xref_stream_json_pdf_with_free_generation(free_generation: u16) -> Vec<u8> {
    historical_xref_stream_json_pdf_with_options(free_generation, None, "")
}

fn malformed_late_prev_after_valid_sections_json_pdf() -> Vec<u8> {
    let mut bytes = historical_xref_stream_json_pdf_with_options(1, Some(9), "/Middle 70 3 R");
    let middle = incremental_last_startxref(&bytes);
    append_json_classic_increment(
        &mut bytes,
        &[(6, 0, "null")],
        &[],
        7,
        "/Newest 50 2 R",
        middle,
    );
    bytes
}

fn historical_xref_stream_json_pdf() -> Vec<u8> {
    historical_xref_stream_json_pdf_with_free_generation(1)
}

fn latest_xref_stream_json_pdf() -> Vec<u8> {
    let bytes = historical_xref_stream_json_pdf();
    let eof = bytes
        .windows(b"%%EOF\n".len())
        .position(|window| window == b"%%EOF\n")
        .unwrap()
        + b"%%EOF\n".len();
    bytes[..eof].to_vec()
}

fn reused_historical_xref_stream_json_pdf() -> Vec<u8> {
    let mut bytes = latest_xref_stream_json_pdf();
    let previous = incremental_last_startxref(&bytes);
    append_json_classic_increment(
        &mut bytes,
        &[(4, 0, "<< /Marker /New >>"), (5, 0, "null")],
        &[],
        6,
        "",
        previous,
    );
    bytes
}

fn repeated_historical_xref_stream_json_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in [
        (1u32, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
    ] {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }

    let append_xref_stream = |bytes: &mut Vec<u8>,
                              marker: &str,
                              info: u32,
                              previous: Option<u64>| {
        let xref_stream_offset = bytes.len() as u32;
        let mut entries = Vec::new();
        for (kind, offset, generation) in [
            (0u8, 0u32, u16::MAX),
            (1, offsets[0], 0),
            (1, offsets[1], 0),
            (1, offsets[2], 0),
            (1, xref_stream_offset, 0),
        ] {
            entries.push(kind);
            entries.extend_from_slice(&offset.to_be_bytes());
            entries.extend_from_slice(&generation.to_be_bytes());
        }
        let prev = previous.map_or_else(String::new, |value| format!("/Prev {value} "));
        bytes.extend_from_slice(
                format!(
                    "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /Marker /{marker} /Info {info} 0 R {prev}/W [1 4 2] /Index [0 5] /Length {} >>\nstream\n",
                    entries.len()
                )
                .as_bytes(),
            );
        bytes.extend_from_slice(&entries);
        bytes.extend_from_slice(
            format!("\nendstream\nendobj\nstartxref\n{xref_stream_offset}\n%%EOF\n").as_bytes(),
        );
        u64::from(xref_stream_offset)
    };

    let oldest = append_xref_stream(&mut bytes, "Old", 91, None);
    let nearest = append_xref_stream(&mut bytes, "Near", 92, Some(oldest));
    append_json_classic_increment(&mut bytes, &[(5, 0, "null")], &[(4, 1)], 6, "", nearest);
    bytes
}

fn circular_historical_trailer_json_pdf() -> Vec<u8> {
    let mut bytes = incremental_json_base("/Info 99 0 R /Prev 0000000000");
    let old_xref = incremental_last_startxref(&bytes);
    let latest_xref =
        append_json_classic_increment(&mut bytes, &[(4, 0, "null")], &[], 5, "", old_xref);
    let marker = b"/Prev 0000000000";
    let start = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap()
        + b"/Prev ".len();
    bytes[start..start + 10].copy_from_slice(format!("{latest_xref:010}").as_bytes());
    bytes
}

/// Walk Catalog → `/AcroForm` → `/Fields` (resolving an indirect reference at
/// each hop) and return the number of field entries. Returns `None` when the
/// document has no `/AcroForm` dictionary or no `/Fields` array — distinct from
/// `Some(0)` (an AcroForm that survives with an emptied `/Fields`).
fn acroform_fields_len(pdf: &mut Pdf<BufReader<File>>) -> Option<usize> {
    let root_ref = pdf.root_ref()?;
    let catalog = pdf.resolve_canonical_object(root_ref).ok()?;
    let acroform = catalog.try_get_key(b"/AcroForm").ok()?;
    pdf.resolve(&acroform).ok()?;
    let fields = acroform.try_get_key(b"/Fields").ok()?;
    pdf.resolve(&fields).ok()?;
    Some(fields.as_array()?.len())
}

fn encrypted_v1_owner_password_fixture() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");
    let xref_offset = bytes.len();
    let trailer = b"trailer\n<< /Size 3 /Root 1 0 R /Encrypt << /Filter /Standard /V 1 /R 2 /Length 40 /P -3904 /O <94e8094419662a774442fb072e3d9f19e9d130ec09a4d0061e78fe920f7ab62f> /U <13f520c882d052bf57b416b747c13979bded7ea31240fe41928852aca3894c49> >> /ID [<000102030405060708090a0b0c0d0e0f><000102030405060708090a0b0c0d0e0f>] >>\nstartxref\n";
    bytes.extend_from_slice(format!("xref\n0 3\n0000000000 65535 f \n{obj1_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(trailer);
    bytes.extend_from_slice(xref_offset.to_string().as_bytes());
    bytes.extend_from_slice(b"\n%%EOF\n");
    bytes
}

// ---------------------------------------------------------------------------
// CLI flags --compress-streams / --normalize-content /
//                 --coalesce-contents / --remove-unreferenced-resources /
//                 --newline-before-endstream
// ---------------------------------------------------------------------------

/// Minimal single-page PDF with a content stream and a font resource entry.
/// `/F2` is NOT referenced in the content stream. On a plain `rewrite` flpdf
/// keeps it — matching qpdf, which only prunes `/Resources` entries during page
/// operations (`--pages`), not on a plain rewrite. The tests using
/// this fixture assert that each flag is accepted and the output is structurally
/// valid, not that resources are pruned;
/// the behavioral retention assertions live in `cli_optimization_matrix.rs`.
fn one_page_pdf_with_unused_resource() -> Vec<u8> {
    let content_data = b"BT /F1 12 Tf (Hello) Tj ET";
    let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
    let obj2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n";
    // F1 is referenced, F2 is NOT referenced in the content stream.
    let obj3_bytes = b"3 0 obj\n<< /Type /Page /Parent 2 0 R \
        /Resources << /Font << /F1 4 0 R /F2 5 0 R >> >> \
        /MediaBox [0 0 612 792] /Contents 6 0 R >>\nendobj\n";
    let obj4 = b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>\nendobj\n";
    let obj5 = b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n";
    let obj6 = format!(
        "6 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        content_data.len(),
        String::from_utf8_lossy(content_data)
    );
    let objects: Vec<&[u8]> = vec![obj1, obj2, obj3_bytes, obj4, obj5, obj6.as_bytes()];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for obj in &objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(obj);
    }
    let xref_start = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for &off in &offsets {
        bytes.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

/// A two-page PDF where each page has multiple /Contents streams.
fn two_page_pdf_with_multi_contents() -> Vec<u8> {
    // Object numbers are consecutive (1..=7) so the positionally-built
    // xref table below stays consistent with the object numbers.
    let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
    let obj2 = b"2 0 obj\n<< /Type /Pages /Count 2 /Kids [3 0 R 6 0 R] >>\nendobj\n";
    // Page 1: two /Contents streams (4 0 R and 5 0 R).
    let obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [4 0 R 5 0 R] >>\nendobj\n";
    let c1 = b"q Q";
    let obj4 = format!(
        "4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        c1.len(),
        String::from_utf8_lossy(c1)
    );
    let c2 = b"q Q";
    let obj5 = format!(
        "5 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        c2.len(),
        String::from_utf8_lossy(c2)
    );
    // Page 2: single /Contents (7 0 R).
    let obj6 = b"6 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R >>\nendobj\n";
    let c3 = b"q Q";
    let obj7 = format!(
        "7 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        c3.len(),
        String::from_utf8_lossy(c3)
    );
    let objects: Vec<Vec<u8>> = vec![
        obj1.to_vec(),
        obj2.to_vec(),
        obj3.to_vec(),
        obj4.into_bytes(),
        obj5.into_bytes(),
        obj6.to_vec(),
        obj7.into_bytes(),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for obj in &objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(obj);
    }
    let xref_start = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for &off in &offsets {
        bytes.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

// ── compress-streams ──────────────────────────────────────────────────────────

#[test]
fn rewrite_compress_streams_y_accepted_and_produces_valid_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--compress-streams=y"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn rewrite_compress_streams_n_accepted_and_produces_valid_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

// ── normalize-content ─────────────────────────────────────────────────────────

#[test]
fn rewrite_normalize_content_y_accepted_and_produces_valid_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn rewrite_normalize_content_n_accepted_and_produces_valid_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    // The produced PDF must be structurally valid.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn rewrite_normalize_content_bad_token_writes_output_warns_and_exits_three() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("bad-content.pdf");
    let output = temp.path().join("normalized.pdf");
    let input_bytes = one_page_pdf_with_content(b"\r<0g");
    let offset = stream_data_offset(&input_bytes);
    std::fs::write(&input, &input_bytes).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .code(3)
        .stderr(predicate::str::contains(format!(
            "WARNING: {} (offset {offset}): content normalization encountered bad tokens",
            input.display(),
        )))
        .stderr(predicate::str::contains(
            "normalized content ended with a bad token",
        ))
        .stderr(predicate::str::contains(
            "Resulting stream data may be corrupted but is may still useful",
        ))
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings; resulting file may have some problems",
        ));

    assert!(output.exists(), "qpdf warning exit must retain output");
    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
    assert_eq!(
        flpdf::pages::page_content_bytes(&mut pdf, page).unwrap(),
        b"\n<0g"
    );
}

#[test]
fn rewrite_normalize_content_follows_indirect_contents_array() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("indirect-array-bad-content.pdf");
    let output = temp.path().join("normalized.pdf");
    let input_bytes = one_page_pdf_with_indirect_contents_array(b"\r<0g");
    let offset = stream_data_offset(&input_bytes);
    std::fs::write(&input, &input_bytes).unwrap();

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    assert!(output.exists(), "qpdf warning exit must retain output");
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        format!(
            "WARNING: {} (offset {offset}): content normalization encountered bad tokens{EOL}\
             WARNING: {} (offset {offset}): normalized content ended with a bad token; you may be able to resolve this by coalescing content streams in combination with normalizing content. From the command line, specify --coalesce-contents{EOL}\
             WARNING: {} (offset {offset}): Resulting stream data may be corrupted but is may still useful for manual inspection. For more information on this warning, search for content normalization in the manual.{EOL}\
             flpdf: operation succeeded with warnings; resulting file may have some problems{EOL}",
            input.display(),
            input.display(),
            input.display(),
        )
    );

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
    assert_eq!(
        flpdf::pages::page_content_bytes(&mut pdf, page).unwrap(),
        b"\n<0g"
    );
}

#[test]
fn rewrite_normalize_content_skips_null_array_entries_like_qpdf() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("mixed-content-array.pdf");
    let output = temp.path().join("normalized.pdf");
    std::fs::write(
        &input,
        one_page_pdf_with_mixed_content_array(b"q\rQ", b"null"),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_ref = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
    let page = pdf.resolve_canonical_object(page_ref).unwrap();
    let contents = page
        .try_get_key(b"/Contents")
        .expect("rewritten page must have /Contents");
    pdf.resolve(&contents).unwrap();
    let contents = contents
        .as_array()
        .expect("rewritten page must retain its /Contents array");
    let stream_ref = contents[0]
        .object_ref()
        .expect("first /Contents element must remain a stream reference");
    let stream = pdf.resolve_canonical_object(stream_ref).unwrap();
    assert_eq!(
        stream
            .get_stream_data(flpdf::DecodeLevel::All)
            .unwrap()
            .as_slice(),
        b"q\nQ"
    );
}

#[test]
fn rewrite_normalize_content_propagates_indirect_stream_decode_error() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("indirect-array-corrupt-flate.pdf");
    let output = temp.path().join("normalized.pdf");
    std::fs::write(
        &input,
        one_page_pdf_with_indirect_filtered_contents_array(b"not a zlib stream"),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("inflate"));

    assert!(
        !output.exists(),
        "decode failure must abort before creating output"
    );
}

#[test]
fn rewrite_normalize_content_recovered_bad_token_omits_terminal_warning() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("recovered-content.pdf");
    let output = temp.path().join("normalized.pdf");
    std::fs::write(&input, one_page_pdf_with_content(b"<0g> q")).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "content normalization encountered bad tokens",
        ))
        .stderr(predicate::str::contains("normalized content ended").not())
        .stderr(predicate::str::contains(
            "Resulting stream data may be corrupted but is may still useful",
        ));

    assert!(output.exists());
}

#[test]
fn rewrite_normalize_content_shared_bad_stream_warns_once() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("shared-bad-content.pdf");
    let output = temp.path().join("normalized.pdf");
    std::fs::write(&input, two_page_pdf_with_shared_content(b"<0g")).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .code(3)
        .stderr(predicate::str::contains("content normalization encountered bad tokens").count(1))
        .stderr(
            predicate::str::contains(
                "Resulting stream data may be corrupted but is may still useful",
            )
            .count(1),
        );

    assert!(output.exists());
}

#[test]
fn rewrite_normalize_content_duplicate_array_stream_warns_once() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("duplicate-array-bad-content.pdf");
    let output = temp.path().join("normalized.pdf");
    let input_bytes = one_page_pdf_with_duplicate_content_array(b"<0g");
    let offset = stream_data_offset(&input_bytes);
    std::fs::write(&input, &input_bytes).unwrap();

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    assert!(output.exists(), "qpdf warning exit must retain output");
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        format!(
            "WARNING: {} (offset {offset}): content normalization encountered bad tokens{EOL}\
             WARNING: {} (offset {offset}): normalized content ended with a bad token; you may be able to resolve this by coalescing content streams in combination with normalizing content. From the command line, specify --coalesce-contents{EOL}\
             WARNING: {} (offset {offset}): Resulting stream data may be corrupted but is may still useful for manual inspection. For more information on this warning, search for content normalization in the manual.{EOL}\
             flpdf: operation succeeded with warnings; resulting file may have some problems{EOL}",
            input.display(),
            input.display(),
            input.display(),
        )
    );
}

#[test]
fn rewrite_normalize_content_deduplicates_terminal_stream_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("aliased-bad-content.pdf");
    let output = temp.path().join("normalized.pdf");
    let input_bytes = two_page_pdf_with_indirect_content_aliases(b"\r<0g");
    let offset = stream_data_offset(&input_bytes);
    std::fs::write(&input, &input_bytes).unwrap();

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    assert!(output.exists(), "qpdf warning exit must retain output");
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        format!(
            "WARNING: {} (offset {offset}): content normalization encountered bad tokens{EOL}\
             WARNING: {} (offset {offset}): normalized content ended with a bad token; you may be able to resolve this by coalescing content streams in combination with normalizing content. From the command line, specify --coalesce-contents{EOL}\
             WARNING: {} (offset {offset}): Resulting stream data may be corrupted but is may still useful for manual inspection. For more information on this warning, search for content normalization in the manual.{EOL}\
             flpdf: operation succeeded with warnings; resulting file may have some problems{EOL}",
            input.display(),
            input.display(),
            input.display(),
        )
    );

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    for page in flpdf::pages::page_refs(&mut pdf).unwrap() {
        assert_eq!(
            flpdf::pages::page_content_bytes(&mut pdf, page).unwrap(),
            b"\n<0g"
        );
    }
}

#[test]
fn rewrite_normalize_content_keeps_lazy_repair_warnings_before_normalization_warnings() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("lazy-and-bad-content.pdf");
    let output = temp.path().join("normalized.pdf");
    let input_bytes = one_page_pdf_with_stale_length_and_content(b"<0g");
    let offset = stream_data_offset(&input_bytes);
    std::fs::write(&input, &input_bytes).unwrap();

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    assert!(output.exists(), "qpdf warning exit must retain output");
    let stderr = String::from_utf8(result.stderr).unwrap();
    let lines: Vec<_> = stderr.lines().collect();
    let normalization_start = lines
        .iter()
        .position(|line| line.ends_with("content normalization encountered bad tokens"))
        .unwrap();
    for suffix in [
        "expected endstream",
        "attempting to recover stream length",
        "recovered stream length: 4",
    ] {
        assert!(
            lines[..normalization_start]
                .iter()
                .any(|line| line.ends_with(suffix)),
            "missing lazy warning ending with {suffix:?} before normalization warnings: {stderr}"
        );
        assert_eq!(
            lines[..normalization_start]
                .iter()
                .filter(|line| line.ends_with(suffix))
                .count(),
            1,
            "lazy warning ending with {suffix:?} was emitted more than once: {stderr}"
        );
    }
    assert_eq!(
        &lines[normalization_start..],
        [
            format!(
                "WARNING: {} (offset {offset}): content normalization encountered bad tokens",
                input.display()
            ),
            format!(
                "WARNING: {} (offset {offset}): Resulting stream data may be corrupted but is may still useful for manual inspection. For more information on this warning, search for content normalization in the manual.",
                input.display()
            ),
            "flpdf: operation succeeded with warnings; resulting file may have some problems"
                .to_string(),
        ]
    );
}

// ── coalesce-contents ─────────────────────────────────────────────────────────

#[test]
fn rewrite_coalesce_contents_accepted_and_produces_valid_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, two_page_pdf_with_multi_contents()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--coalesce-contents"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn top_level_coalesce_contents_accepted_and_produces_valid_output() {
    // Top-level alias of `flpdf rewrite --coalesce-contents` (qpdf-shape).
    // Mirrors rewrite_coalesce_contents_accepted_and_produces_valid_output,
    // dropping only the "rewrite" argv token.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, two_page_pdf_with_multi_contents()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--coalesce-contents"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn top_level_coalesce_contents_conflicts_with_check() {
    // A silent-ignore combination (--check would win the dispatch chain over
    // any rewrite modifier) would produce wrong output. clap must surface it
    // as a usage error, exit 2 (qpdf convention). Mirrors how --decrypt /
    // --remove-restrictions are gated.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", "--coalesce-contents", "in.pdf"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_coalesce_contents_conflicts_with_add_attachment() {
    // Silent-shadow guard: without a clap conflict, the add-attachment
    // branch wins the dispatch chain and --coalesce-contents is dropped
    // without diagnostic. Reject the combination at usage-error level
    // (exit 2, qpdf convention).
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--coalesce-contents",
            "--add-attachment",
            "attach.pdf",
            "--",
            "in.pdf",
            "out.pdf",
        ])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_coalesce_contents_conflicts_with_copy_attachments_from() {
    // Silent-shadow guard, sibling of the add-attachment case.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--coalesce-contents",
            "--copy-attachments-from",
            "donor.pdf",
            "in.pdf",
            "out.pdf",
        ])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_coalesce_contents_conflicts_with_linearize() {
    // Silent-shadow guard: --linearize is threaded to `run_rewrite`, whose
    // linearize branch never reads `coalesce_contents`. Rejecting the
    // combination up-front at clap level prevents the caller from getting a
    // linearized output whose /Contents arrays are still unmerged.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--linearize", "--coalesce-contents", "in.pdf", "out.pdf"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_coalesce_contents_conflicts_with_pages() {
    // Silent-shadow guard: the page-op dispatch branch owns the write via
    // run_page_extraction / run_rewrite_with_page_ops, neither of which reads
    // `args.coalesce_contents`. The `--encrypt` / `--overlay` combinations
    // are already rejected inside that branch; mirror the same treatment for
    // --coalesce-contents at the clap level.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--coalesce-contents",
            "in.pdf",
            "--pages",
            ".",
            "--",
            "out.pdf",
        ])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_coalesce_contents_conflicts_with_rotate() {
    // Sibling of the --pages case.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--coalesce-contents", "--rotate=+90:1", "in.pdf", "out.pdf"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_coalesce_contents_conflicts_with_split_pages() {
    // Sibling of the --pages case.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--coalesce-contents",
            "--split-pages",
            "1",
            "in.pdf",
            "out.pdf",
        ])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_coalesce_contents_accepts_collate_alone() {
    // `--collate` alone (no `--pages`) is a documented no-op that does NOT
    // activate `page_ops_active`; the default rewrite branch runs and honors
    // `--coalesce-contents`. Regression net: keep `--collate` OUT of the
    // conflicts_with_all list so qpdf-shaped callers that pass `--collate`
    // unconditionally (with or without `--pages`) still coalesce correctly.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, two_page_pdf_with_multi_contents()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--coalesce-contents", "--collate=1"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
}

#[test]
fn top_level_coalesce_contents_conflicts_with_empty() {
    // Sibling of the --pages case.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--coalesce-contents", "--empty", "in.pdf", "out.pdf"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_generate_appearances_conflicts_with_check() {
    // Silent-shadow guard: --check's inspection dispatch never reaches the
    // rewrite path that reads `args.generate_appearances`.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--generate-appearances", "--check", "in.pdf"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_generate_appearances_conflicts_with_pages() {
    // Silent-shadow guard: the page-op dispatch branch owns the write via
    // run_page_extraction / run_rewrite_with_page_ops, neither of which
    // reads `args.generate_appearances` (mirrors the `--coalesce-contents`
    // / `--encrypt` treatment in the same branch).
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--generate-appearances",
            "in.pdf",
            "--pages",
            ".",
            "--",
            "out.pdf",
        ])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_generate_appearances_conflicts_with_rotate() {
    // Sibling of the --pages case.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--generate-appearances",
            "--rotate=+90:1",
            "in.pdf",
            "out.pdf",
        ])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_generate_appearances_conflicts_with_split_pages() {
    // Sibling of the --pages case.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--generate-appearances",
            "--split-pages",
            "in.pdf",
            "out-%d.pdf",
        ])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_generate_appearances_conflicts_with_json_output() {
    // Without this conflict, `--generate-appearances --json-output=2 in`
    // would exit 0 while run_json dumps the unmodified input, silently
    // dropping the requested appearance generation (same class of bug
    // documented on `--flatten-annotations` above).
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--generate-appearances", "--json-output=2", "in.pdf"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn top_level_coalesce_contents_with_overlay_underlay_trailing_position() {
    // The exact shape qtest form-xobject uo-3 emits (via the PATH-shim
    // qpdf→flpdf): --coalesce-contents at the very end of argv, after
    // TWO overlay/underlay groups each terminated by `--`. The parser
    // must let the trailing top-level flag through to clap, and clap
    // must accept it. We only assert exit 0 —
    // byte-parity of the output is a separate concern.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let overlay = temp.path().join("over.pdf");
    let underlay = temp.path().join("under.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, two_page_pdf_with_multi_contents()).unwrap();
    std::fs::write(&overlay, two_page_pdf_with_multi_contents()).unwrap();
    std::fs::write(&underlay, two_page_pdf_with_multi_contents()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args([
            "--static-id",
            "--qdf",
            "--no-original-object-ids",
            "--verbose",
        ])
        .arg(&input)
        .arg(&output)
        .arg("--overlay")
        .arg(&overlay)
        .args(["--from=", "--repeat=r2,r1", "--"])
        .arg("--underlay")
        .arg(&underlay)
        .args(["--from=z-1", "--", "--coalesce-contents"])
        .assert()
        .success();

    assert!(output.exists());
}

// ── remove-unreferenced-resources ─────────────────────────────────────────────

#[test]
fn rewrite_remove_unreferenced_resources_auto_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_with_unused_resource()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--remove-unreferenced-resources=auto"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn rewrite_remove_unreferenced_resources_yes_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_with_unused_resource()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--remove-unreferenced-resources=yes"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn rewrite_remove_unreferenced_resources_no_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--remove-unreferenced-resources=no"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    // The produced PDF must be structurally valid.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

// ── newline-before-endstream ──────────────────────────────────────────────────

#[test]
fn rewrite_newline_before_endstream_y_accepted_and_produces_valid_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--newline-before-endstream=y"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn rewrite_newline_before_endstream_n_accepted_and_produces_valid_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--newline-before-endstream=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn rewrite_newline_before_endstream_garbage_uses_qpdf_bare_flag_behavior() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_with_content(b"q Q")).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--compress-streams=n",
            "--newline-before-endstream=garbage",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let output_bytes = std::fs::read(&output).unwrap();
    assert!(contains(&output_bytes, b"q Q\nendstream"));

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

// ── help text contains qpdf-compatible defaults ───────────────────────────────

#[test]
fn rewrite_help_shows_compress_streams_default_y() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("compress-streams"))
        .stdout(predicate::str::contains("`y` (default)"));
}

#[test]
fn rewrite_help_shows_normalize_content_default_n() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("normalize-content"))
        .stdout(predicate::str::contains("default: n"));
}

#[test]
fn rewrite_help_shows_remove_unreferenced_resources_default_auto() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("remove-unreferenced-resources"))
        .stdout(predicate::str::contains("default: auto"));
}

#[test]
fn rewrite_help_shows_newline_before_endstream_default_never() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("newline-before-endstream"))
        .stdout(predicate::str::contains("default: never"));
}

#[test]
fn rewrite_help_shows_preserve_unreferenced() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("preserve-unreferenced"));
}

// ── combination tests ─────────────────────────────────────────────────────────

#[test]
fn rewrite_full_rewrite_with_compress_n_and_newline_n() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--compress-streams=n",
            "--newline-before-endstream=n",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn rewrite_coalesce_and_normalize_content_combination() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, two_page_pdf_with_multi_contents()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--coalesce-contents", "--normalize-content=y"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn rewrite_normalize_and_remove_unreferenced_combination() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_with_unused_resource()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--normalize-content=y",
            "--remove-unreferenced-resources=yes",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

// ===========================================================================
// Page operations: --pages / --rotate / --split-pages / --collate
//
//
// qpdf observation basis (/usr/bin/qpdf 11.9.0): see the comment block at the
// top of the page-ops section in main.rs. Key facts encoded in these tests:
//   - `qpdf in --pages . 2-3 -- --rotate=+90:1 out` rotates the first
//     EXTRACTED page (output page numbering).
//   - `qpdf --split-pages=2 in out.pdf` → out-1-2.pdf, out-3-3.pdf.
//   - `--collate`/`--rotate`/`--split-pages` without `--pages` exit 0.
// ===========================================================================

const THREE_PAGE: &str = "../../tests/fixtures/compat/three-page.pdf";
const TWO_PAGE: &str = "../../tests/fixtures/compat/two-page.pdf";

/// Build a 3-page PDF where:
///   - each page carries its own `/Resources /Font` with a DISTINCT font
///     entry (F1/F2/F3 → fonts 30/31/32),
///   - an `/Outlines` tree has one item per page (Item1→p1, Item2→p2,
///     Item3→p3),
///   - a `/Names /Dests` name-tree maps "d1"/"d2"/"d3" to the three pages.
///
/// Used to assert, via the CLI, that after `--pages` extraction the
/// post-rebuild passes actually run: dropped pages' outline items and named
/// dests are gone, surviving ones repoint, and dropped pages' font resources
/// are pruned out of the output.
///
/// Object layout (numbers are stable; ObjectRef gen 0):
///   1  Catalog (/Pages 2 /Outlines 20 /Names 25)
///   2  Pages root (/Kids [3 6 9])
///   3  Page 1 (/Contents 4 /Resources << /Font 5 >>)
///   4  content p1   5  /Font << /F1 30 >>
///   6  Page 2 (/Contents 7 /Resources << /Font 8 >>)
///   7  content p2   8  /Font << /F2 31 >>
///   9  Page 3 (/Contents 10 /Resources << /Font 11 >>)
///  10  content p3  11  /Font << /F3 32 >>
///  20  Outlines root (/First 21 /Last 23 /Count 3)
///  21  Item1 (/Dest [3 /Fit] /Next 22)
///  22  Item2 (/Dest [6 /Fit] /Prev 21 /Next 23)
///  23  Item3 (/Dest [9 /Fit] /Prev 22)
///  25  Names (/Dests 26)
///  26  Dests name-tree leaf (/Names [(d1) [3 /Fit] (d2) [6 /Fit] (d3) [9 /Fit]])
///  30  Font F1   31  Font F2   32  Font F3
fn outline_dests_three_page_pdf() -> Vec<u8> {
    let c1 = b"BT /F1 12 Tf 1 1 Td (P1) Tj ET";
    let c2 = b"BT /F2 12 Tf 1 1 Td (P2) Tj ET";
    let c3 = b"BT /F3 12 Tf 1 1 Td (P3) Tj ET";

    let mut out: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let mut offs: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();

    let emit =
        |out: &mut Vec<u8>, offs: &mut std::collections::BTreeMap<u32, u64>, n: u32, body: &str| {
            offs.insert(n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        };
    let emit_stream = |out: &mut Vec<u8>,
                       offs: &mut std::collections::BTreeMap<u32, u64>,
                       n: u32,
                       data: &[u8]| {
        offs.insert(n, out.len() as u64);
        out.extend_from_slice(
            format!("{n} 0 obj\n<< /Length {} >>\nstream\n", data.len()).as_bytes(),
        );
        out.extend_from_slice(data);
        out.extend_from_slice(b"\nendstream\nendobj\n");
    };

    emit(
        &mut out,
        &mut offs,
        1,
        "<< /Type /Catalog /Pages 2 0 R /Outlines 20 0 R /Names 25 0 R >>",
    );
    emit(
        &mut out,
        &mut offs,
        2,
        "<< /Type /Pages /Kids [3 0 R 6 0 R 9 0 R] /Count 3 >>",
    );
    emit(&mut out, &mut offs, 3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font 5 0 R >> >>");
    emit_stream(&mut out, &mut offs, 4, c1);
    emit(&mut out, &mut offs, 5, "<< /F1 30 0 R >>");
    emit(&mut out, &mut offs, 6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 7 0 R /Resources << /Font 8 0 R >> >>");
    emit_stream(&mut out, &mut offs, 7, c2);
    emit(&mut out, &mut offs, 8, "<< /F2 31 0 R >>");
    emit(&mut out, &mut offs, 9, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 10 0 R /Resources << /Font 11 0 R >> >>");
    emit_stream(&mut out, &mut offs, 10, c3);
    emit(&mut out, &mut offs, 11, "<< /F3 32 0 R >>");
    emit(
        &mut out,
        &mut offs,
        20,
        "<< /Type /Outlines /First 21 0 R /Last 23 0 R /Count 3 >>",
    );
    emit(
        &mut out,
        &mut offs,
        21,
        "<< /Title (Item1) /Parent 20 0 R /Dest [3 0 R /Fit] /Next 22 0 R >>",
    );
    emit(
        &mut out,
        &mut offs,
        22,
        "<< /Title (Item2) /Parent 20 0 R /Dest [6 0 R /Fit] /Prev 21 0 R /Next 23 0 R >>",
    );
    emit(
        &mut out,
        &mut offs,
        23,
        "<< /Title (Item3) /Parent 20 0 R /Dest [9 0 R /Fit] /Prev 22 0 R >>",
    );
    emit(&mut out, &mut offs, 25, "<< /Dests 26 0 R >>");
    emit(
        &mut out,
        &mut offs,
        26,
        "<< /Names [(d1) [3 0 R /Fit] (d2) [6 0 R /Fit] (d3) [9 0 R /Fit]] >>",
    );
    emit(
        &mut out,
        &mut offs,
        30,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    );
    emit(
        &mut out,
        &mut offs,
        31,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>",
    );
    emit(
        &mut out,
        &mut offs,
        32,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>",
    );

    let max_obj = 32u32;
    let xref_start = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_obj + 1).as_bytes());
    for i in 1..=max_obj {
        match offs.get(&i) {
            Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            max_obj + 1
        )
        .as_bytes(),
    );
    out
}

// ── Individual flags ──────────────────────────────────────────────────────

#[test]
fn pages_extracts_subset_top_level_syntax() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .args(["--pages", ".", "2-3", "--"])
        .arg(&output)
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-npages", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("2"));
}

#[test]
fn pages_repaired_input_keeps_output_and_exits_three_with_output_summary() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("damaged.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(
        &input,
        include_bytes!("../../../tests/fixtures/compat/missing-mediabox-leaf.pdf"),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--repair"])
        .arg(&input)
        .args(["--pages", ".", "1", "--"])
        .arg(&output)
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings; resulting file may have some problems",
        ));

    assert!(output.exists(), "warning exit must retain extracted output");
}

#[test]
fn pages_unknown_intermediate_key_matches_qpdf_warning_exit() {
    if !qpdf_11_9_available() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("unknown-intermediate-pages-key.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    std::fs::write(&input, one_page_pdf_with_unknown_intermediate_pages_key()).unwrap();

    let qpdf = std::process::Command::new("qpdf")
        .arg(&input)
        .args(["--pages", input.to_str().unwrap(), "1", "--"])
        .arg(&qpdf_output)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&input)
        .args(["--pages", ".", "1", "--"])
        .arg(&flpdf_output)
        .output()
        .unwrap();

    let warning =
        "Unknown key /UserUnit in /Pages object is being discarded as a result of flattening the /Pages tree";
    let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr);
    let flpdf_stderr = String::from_utf8_lossy(&flpdf.stderr);
    let qpdf_warning_line = qpdf_stderr
        .lines()
        .find(|line| line.contains("Unknown key /UserUnit"))
        .expect("qpdf must emit the unknown intermediate key warning");
    let flpdf_warning_line = flpdf_stderr
        .lines()
        .find(|line| line.contains("Unknown key /UserUnit"))
        .expect("flpdf must emit the unknown intermediate key warning");
    assert_eq!(qpdf.status.code(), Some(3), "qpdf stderr: {qpdf_stderr}");
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "flpdf stderr: {flpdf_stderr}"
    );
    assert_eq!(qpdf_warning_line, flpdf_warning_line);
    assert!(qpdf_warning_line.contains(warning));
    assert_eq!(qpdf_stderr.matches("Unknown key /UserUnit").count(), 1);
    assert_eq!(flpdf_stderr.matches("Unknown key /UserUnit").count(), 1);
    assert!(qpdf_output.exists());
    assert!(flpdf_output.exists());
}

#[test]
fn pages_dot_shorthand_resolves_to_primary_input() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .args(["--pages", ".", "1", "--"])
        .arg(&output)
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-npages", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("1"));
}

#[test]
fn rotate_single_spec_rewrites_all_pages() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .arg(&output)
        .args(["--rotate=180"])
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let rotations = pages::page_refs(&mut pdf)
        .unwrap()
        .into_iter()
        .map(|page_ref| {
            PageObjectHelper::new(page_ref, &mut pdf)
                .get_attribute(b"/Rotate", false)
                .unwrap()
                .try_get_int_value()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(rotations, vec![180, 180, 180]);
}

#[test]
fn rotate_repaired_input_keeps_output_and_exits_three_with_output_summary() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("damaged.pdf");
    let output = temp.path().join("rotated.pdf");
    std::fs::write(&input, corrupt_xref_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--repair"])
        .arg(&input)
        .arg(&output)
        .arg("--rotate=90")
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings; resulting file may have some problems",
        ));

    assert!(output.exists(), "warning exit must retain rotated output");
}

#[test]
fn split_pages_produces_chunked_outputs() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .arg(&output)
        .args(["--split-pages=2"])
        .assert()
        .success();

    // qpdf 11.9.0 naming: out-1-2.pdf, out-3-3.pdf (width = digits of total).
    assert!(temp.path().join("out-1-2.pdf").exists());
    assert!(temp.path().join("out-3-3.pdf").exists());
    assert!(!output.exists(), "unsplit single file must not be written");
}

#[test]
fn split_pages_propagates_orphan_widget_warning_to_job_exit_status() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("split.pdf");
    let input = "../../tests/fixtures/compat/acroform-sig-orphan-widget.pdf";

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--split-pages=1", input])
        .arg(&output)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    let warning = "this widget annotation is not reachable from /AcroForm in the document catalog";

    assert_eq!(
        result.status.code(),
        Some(3),
        "split-pages warnings must use qpdf's warning exit status; stderr={stderr}"
    );
    assert_eq!(
        stderr.matches(warning).count(),
        1,
        "one source warning must be emitted once; stderr={stderr}"
    );
    assert!(
        stderr.contains("acroform-sig-orphan-widget.pdf"),
        "warning must retain the input description; stderr={stderr}"
    );
    assert!(
        stderr.contains("operation succeeded with warnings"),
        "split job must emit qpdf's warning summary; stderr={stderr}"
    );
}

#[test]
fn split_pages_preserves_a_repair_warning_from_the_original_input() {
    // The intermediate full-rewrite that feeds the split job can already
    // have repaired the condition that produced the original warning (here,
    // --repair's xref reconstruction), so the freshly re-opened split
    // source looks clean on its own. The original input's warning must
    // still surface in the overall exit status and summary.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_pdf()).unwrap();
    let output = temp.path().join("split.pdf");

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--repair", "--split-pages=1"])
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert_eq!(
        result.status.code(),
        Some(3),
        "a repaired original input must still exit with qpdf's warning status; stderr={stderr}"
    );
    assert!(
        stderr.contains("operation succeeded with warnings"),
        "the split job must still emit qpdf's warning summary; stderr={stderr}"
    );
    assert!(
        std::fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(
                |entry| entry.path().extension().is_some_and(|ext| ext == "pdf")
                    && entry.path() != input
            ),
        "a split output file must still be written"
    );
}

#[test]
fn pages_split_pages_preserves_a_repair_warning_from_the_original_input() {
    // Same regression as `split_pages_preserves_a_repair_warning_from_the_original_input`,
    // through the `--pages` selection branch instead of the plain-rewrite
    // branch (both call `split_rewritten_pdf` and must fold prior_warnings
    // into the split job before completing it).
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_pdf()).unwrap();
    let output = temp.path().join("split.pdf");

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--repair", "--split-pages=1"])
        .arg(&input)
        .args(["--pages", ".", "1", "--"])
        .arg(&output)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert_eq!(
        result.status.code(),
        Some(3),
        "a repaired original input must still exit with qpdf's warning status; stderr={stderr}"
    );
    assert!(
        stderr.contains("operation succeeded with warnings"),
        "the split job must still emit qpdf's warning summary; stderr={stderr}"
    );
}

#[test]
fn collate_without_pages_is_accepted_noop() {
    // qpdf 11.9.0 accepts --collate without --pages (exit 0); flpdf matches.
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .arg(&output)
        .args(["--collate=2"])
        .assert()
        .success();
    assert!(output.exists());
}

// ── Combinations matching qpdf documented examples ────────────────────────

#[test]
fn pages_then_rotate_targets_output_page_numbering() {
    // qpdf 11.9.0: `qpdf in --pages . 2-3 -- --rotate=+90:1 out` rotates the
    // FIRST EXTRACTED page only (verified: src page 2 → /Rotate 90, src page
    // 3 → /Rotate 0). The --rotate range indexes OUTPUT page numbers.
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .args(["--pages", ".", "2-3", "--"])
        .arg("--rotate=+90:1")
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let rotations = pages::page_refs(&mut pdf)
        .unwrap()
        .into_iter()
        .map(|page_ref| {
            PageObjectHelper::new(page_ref, &mut pdf)
                .get_attribute(b"/Rotate", false)
                .unwrap()
                .try_get_int_value()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(rotations, vec![90, 0]);
}

#[test]
fn pages_then_split_pages_combined() {
    // qpdf documents --split-pages as compatible with --pages.
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .args(["--pages", ".", "1-3", "--"])
        .arg("--split-pages=2")
        .arg(&output)
        .assert()
        .success();

    assert!(temp.path().join("out-1-2.pdf").exists());
    assert!(temp.path().join("out-3-3.pdf").exists());
}

#[test]
fn pages_same_file_repeated_is_single_source() {
    // `--pages . 1 . 3 --` repeats the primary input → single-document case,
    // matching qpdf's "." shorthand semantics. 2 pages out.
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .args(["--pages", ".", "1", ".", "3", "--"])
        .arg(&output)
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-npages", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("2"));
}

#[test]
fn pages_same_file_spelled_differently_is_single_source() {
    // Primary input `../../tests/fixtures/compat/three-page.pdf` and a
    // --pages segment referencing the *same* file via a different spelling
    // (extra `./` and a redundant `dir/../`) must canonicalize to one source
    // and be accepted — not rejected as a cross-document merge.
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let alt_spelling = "../../tests/fixtures/compat/./three-page.pdf";

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .args(["--pages", alt_spelling, "1", ".", "3", "--"])
        .arg(&output)
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-npages", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("2"));
}

// ── Post-rebuild integration: outline/dest remap + resource prune via CLI ──

#[test]
fn pages_extraction_remaps_outline_and_prunes_resources_via_cli() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, outline_dests_three_page_pdf()).unwrap();

    // Extract only page 2 (the middle page). After the pipeline (qpdf null-out
    // parity):
    //  - every outline item is KEPT (Item1, Item2, Item3); Item1/Item3 point at
    //    their removed pages, which become `null`.
    //  - named dests d1/d2/d3 are all KEPT.
    //  - the removed pages are nulled, so their /Resources fonts (Helvetica F1,
    //    Times-Roman F3) become unreferenced and are GC'd; only Courier (F2, the
    //    kept page's font) survives.
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&input)
        .args(["--pages", ".", "2", "--"])
        .arg(&output)
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-npages", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("1"));

    // Outline: every item is kept (null-out keeps Item1/Item2/Item3 even though
    // Item1/Item3 now point at removed, nulled pages).
    let outline = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=outlines", output.to_str().unwrap()])
        .assert()
        .success();
    let outline_json: serde_json::Value =
        serde_json::from_slice(&outline.get_output().stdout).unwrap();
    let titles: Vec<&str> = outline_json["outlines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["Item1", "Item2", "Item3"],
        "all outline items must be kept, in order (null-out parity)"
    );

    let raw = std::fs::read(&output).unwrap();
    let txt = String::from_utf8_lossy(&raw);

    // Named destinations: all three are kept (null-out keeps d1/d3 even though
    // their target pages were removed and nulled).
    assert!(
        txt.contains("(d1)") && txt.contains("(d2)") && txt.contains("(d3)"),
        "named destinations d1/d2/d3 must all be kept (null-out parity): {txt}"
    );

    // Resource prune + xref GC: dropped pages' fonts must not be in output.
    assert!(
        txt.contains("Courier"),
        "kept page's font missing from output"
    );
    assert!(
        !txt.contains("Helvetica"),
        "dropped page 1 font (Helvetica) was not pruned"
    );
    assert!(
        !txt.contains("Times-Roman"),
        "dropped page 3 font (Times-Roman) was not pruned"
    );

    // Output must be structurally valid.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn pages_extraction_materializes_inherited_indirect_resources_before_prune() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("inherited-resources.pdf");
    std::fs::write(&input, one_page_pdf_with_inherited_indirect_resources()).unwrap();

    let cases: [(&str, &[&str]); 2] = [
        ("auto", &[]),
        ("yes", &["--remove-unreferenced-resources=yes"]),
    ];
    for (label, flags) in cases {
        let output = temp.path().join(format!("out-{label}.pdf"));
        let mut command = Command::cargo_bin("flpdf").unwrap();
        if flags.is_empty() {
            command
                .arg(&input)
                .args(["--pages", ".", "1", "--"])
                .arg(&output);
        } else {
            command
                .arg("rewrite")
                .args(flags)
                .arg(&input)
                .arg(&output)
                .args(["--pages", ".", "1", "--"]);
        }
        command.assert().success();

        let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
        let page_ref = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
        let page = pdf.resolve_canonical_object(page_ref).unwrap();
        let resources = page.try_get_key(b"/Resources").unwrap();
        assert!(
            resources.try_is_dictionary().unwrap(),
            "{label}: qpdf's copy_if_shared route must materialize /Resources directly on the page; got {resources:?}"
        );
    }
}

#[test]
fn pages_extraction_resource_modes_match_qpdf_copy_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("page-local-resources.pdf");
    std::fs::write(&input, one_page_pdf_with_page_local_indirect_resources()).unwrap();

    let cases: [(&str, &[&str], bool); 3] = [
        ("auto", &[], false),
        ("no", &["--remove-unreferenced-resources=no"], false),
        ("yes", &["--remove-unreferenced-resources=yes"], true),
    ];
    for (label, flags, should_materialize) in cases {
        let output = temp.path().join(format!("out-{label}.pdf"));
        let mut command = Command::cargo_bin("flpdf").unwrap();
        if flags.is_empty() {
            command
                .arg(&input)
                .args(["--pages", ".", "1", "--"])
                .arg(&output);
        } else {
            command
                .arg("rewrite")
                .args(flags)
                .arg(&input)
                .arg(&output)
                .args(["--pages", ".", "1", "--"]);
        }
        command.assert().success();

        let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
        let page_ref = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
        let page = pdf.resolve_canonical_object(page_ref).unwrap();
        let resources = page.try_get_key(b"/Resources").unwrap();
        if should_materialize {
            assert!(
                resources.try_is_dictionary().unwrap(),
                "{label}: qpdf Yes must materialize /Resources; got {resources:?}"
            );
            let font_present = resources.try_has_key(b"/Font").unwrap();
            assert!(
                !font_present
                    || resources
                        .try_get_key(b"/Font")
                        .unwrap()
                        .try_get_keys()
                        .unwrap()
                        .is_empty(),
                "{label}: qpdf Yes must prune unused /F1 after materializing /Resources"
            );
        } else {
            assert!(
                resources.object_ref().is_some(),
                "{label}: qpdf must leave an unshared page-local indirect /Resources reference intact; got {resources:?}"
            );
        }
    }
}

#[test]
fn pages_shared_xobject_category_matches_qpdf_per_page_pruning() {
    if !qpdf_11_9_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("shared-xobject-category.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    std::fs::write(&input, two_page_pdf_with_shared_xobject_category()).unwrap();

    let qpdf = std::process::Command::new("qpdf")
        .arg(&input)
        .args(["--pages", input.to_str().unwrap(), "1-2", "--"])
        .arg(&qpdf_output)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf --pages failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&input)
        .args(["--pages", ".", "1-2", "--"])
        .arg(&flpdf_output)
        .assert()
        .success();

    for page_index in 0..2 {
        let qpdf_keys = resource_category_keys_from_path(&qpdf_output, page_index, "XObject");
        let flpdf_keys = resource_category_keys_from_path(&flpdf_output, page_index, "XObject");
        assert_eq!(
            qpdf_keys, flpdf_keys,
            "page {page_index}: flpdf shared /XObject pruning must match qpdf"
        );
    }
    assert_eq!(
        resource_category_keys_from_path(&flpdf_output, 0, "XObject"),
        vec!["X1"]
    );
    assert_eq!(
        resource_category_keys_from_path(&flpdf_output, 1, "XObject"),
        vec!["X2"]
    );
}

#[test]
fn pages_duplicate_selection_matches_qpdf_resource_copy_boundary() {
    if !qpdf_11_9_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("duplicate-shared-xobject-category.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    std::fs::write(&input, two_page_pdf_with_shared_xobject_category()).unwrap();

    let qpdf = std::process::Command::new("qpdf")
        .arg(&input)
        .args(["--pages", input.to_str().unwrap(), "1,1", "--"])
        .arg(&qpdf_output)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf --pages duplicate selection failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&input)
        .args(["--pages", ".", "1,1", "--"])
        .arg(&flpdf_output)
        .assert()
        .success();

    for page_index in 0..2 {
        assert_eq!(
            resource_category_keys_from_path(&qpdf_output, page_index, "XObject"),
            resource_category_keys_from_path(&flpdf_output, page_index, "XObject"),
            "page {page_index}: duplicate selection resource pruning must match qpdf"
        );
    }
    assert_eq!(
        resource_category_keys_from_path(&flpdf_output, 0, "XObject"),
        vec!["X1"]
    );
    assert_eq!(
        resource_category_keys_from_path(&flpdf_output, 1, "XObject"),
        vec!["X1"]
    );
}

#[test]
fn pages_inherited_resource_non_target_categories_match_qpdf() {
    if !qpdf_11_9_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("inherited-resource-categories.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    std::fs::write(
        &input,
        one_page_pdf_with_inherited_other_resource_categories(),
    )
    .unwrap();

    let qpdf = std::process::Command::new("qpdf")
        .arg(&input)
        .args(["--pages", input.to_str().unwrap(), "1", "--"])
        .arg(&qpdf_output)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf --pages failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&input)
        .args(["--pages", ".", "1", "--"])
        .arg(&flpdf_output)
        .assert()
        .success();

    for category in [
        "Font",
        "XObject",
        "ColorSpace",
        "Pattern",
        "Shading",
        "ExtGState",
        "Properties",
    ] {
        assert_eq!(
            resource_category_keys_from_path(&qpdf_output, 0, category),
            resource_category_keys_from_path(&flpdf_output, 0, category),
            "/{category} pruning must match qpdf"
        );
    }
    assert_eq!(
        resource_category_keys_from_path(&flpdf_output, 0, "Font"),
        vec!["F1"]
    );
    assert!(resource_category_keys_from_path(&flpdf_output, 0, "XObject").is_empty());
    for category in [
        "ColorSpace",
        "Pattern",
        "Shading",
        "ExtGState",
        "Properties",
    ] {
        assert_eq!(
            resource_category_keys_from_path(&flpdf_output, 0, category),
            vec![format!("Unused{category}")]
        );
    }
}

#[test]
fn pages_extraction_keeps_all_when_full_range_selected() {
    // Selecting every page keeps every outline item and every font.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, outline_dests_three_page_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&input)
        .args(["--pages", ".", "1-3", "--"])
        .arg(&output)
        .assert()
        .success();

    let outline = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=outlines", output.to_str().unwrap()])
        .assert()
        .success();
    let outline_json: serde_json::Value =
        serde_json::from_slice(&outline.get_output().stdout).unwrap();
    let titles: Vec<&str> = outline_json["outlines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Item1", "Item2", "Item3"]);
}

// ── Page-spec source lifecycle ────────────────────────────────────────────

#[test]
fn pages_cross_document_merge_is_supported() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .args(["--pages", ".", "1", TWO_PAGE, "2", "--"])
        .arg(&output)
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-npages", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("2"));
}

#[test]
fn empty_flag_is_rejected_actionably() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .arg("--empty")
        .args(["--pages", ".", "1", "--"])
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicate::str::contains("--empty"))
        .stderr(predicate::str::contains("not implemented"));
}

#[test]
fn rewrite_subcommand_supports_pages() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg(THREE_PAGE)
        .arg(&output)
        .args(["--pages", ".", "1-2", "--"])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-npages", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("2"));
}

#[test]
fn pages_help_text_mirrors_qpdf_terms() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--pages"))
        .stdout(predicate::str::contains("--rotate"))
        .stdout(predicate::str::contains("--split-pages"))
        .stdout(predicate::str::contains("--collate"));
}

// ── Attachment tests ────────────────────────────────────────

/// Write a minimal valid PDF to a tempfile and return the path.
fn minimal_pdf_temp() -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(include_bytes!("../../../tests/fixtures/minimal.pdf"))
        .unwrap();
    f
}

/// Build a valid-xref attachment fixture whose Filespec/name-tree value can
/// deliberately exercise qpdf's malformed-object paths.
fn malformed_attachment_pdf(key: &str, filespec_value: &str, stream_value: &str) -> Vec<u8> {
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R /Names 4 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_vec(),
        b"<< /EmbeddedFiles 6 0 R >>".to_vec(),
        filespec_value.as_bytes().to_vec(),
        format!("<< /Names [({key}) 5 0 R] >>").into_bytes(),
        stream_value.as_bytes().to_vec(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (number, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", number + 1).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn add_attachment_default_key_is_basename() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("hello.txt");
    std::fs::write(&attachment, b"hello world").unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    // The key should default to "hello.txt".
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello.txt"));
}

#[test]
fn add_attachment_repeated_segments_are_processed_as_one_batch() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let first = temp.path().join("first.txt");
    let second = temp.path().join("second.txt");
    let output = temp.path().join("out.pdf");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            first.to_str().unwrap(),
            "--key=one",
            "--",
            "--add-attachment",
            second.to_str().unwrap(),
            "--key=two",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("one"))
        .stdout(predicate::str::contains("two"));
}

#[test]
fn add_attachment_missing_segment_terminator_is_a_usage_error() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("one.txt");
    std::fs::write(&attachment, b"one").unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "--add-attachment: missing -- terminator",
        ));
}

#[test]
fn add_attachment_equals_form_with_no_positional_matches_qpdf_usage() {
    // qpdf's `--add-attachment` is a bare option: `--add-attachment=x --`
    // silently discards `x` and, finding no positional file token before
    // the terminator, exits 2 ("add attachment: no file specified";
    // confirmed against /usr/bin/qpdf 11.9.0). Before this fix, flpdf's
    // pre-scanner did not recognize the `=`-form token at all, so it
    // dispatched to the add-attachment path with an empty captured segment
    // list and silently wrote a successful output with nothing embedded.
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("one.txt");
    let output = temp.path().join("out.pdf");
    std::fs::write(&attachment, b"one").unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            input.path().to_str().unwrap(),
            &format!("--add-attachment={}", attachment.to_str().unwrap()),
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(format!("qpdf: add attachment: no file specified{EOL}"));

    assert!(!output.exists());
}

#[test]
fn add_attachment_without_file_matches_qpdf_usage() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
            "--add-attachment",
            "--",
        ])
        .assert()
        .code(2)
        .stderr(format!("qpdf: add attachment: no file specified{EOL}"));

    assert!(!output.exists());
}

#[test]
fn add_attachment_invalid_creation_date_matches_qpdf_usage() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("payload.txt");
    let output = temp.path().join("out.pdf");
    std::fs::write(&attachment, b"payload").unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--creationdate=potato",
            "--",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::starts_with(format!(
            "qpdf: potato is not a valid PDF timestamp{EOL}"
        )));
}

#[test]
fn add_attachment_invalid_modification_date_matches_qpdf_usage() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("payload.txt");
    let output = temp.path().join("out.pdf");
    std::fs::write(&attachment, b"payload").unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--moddate=potato",
            "--",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::starts_with(format!(
            "qpdf: potato is not a valid PDF timestamp{EOL}"
        )));
}

#[test]
fn add_attachment_equals_form_with_a_positional_embeds_it_and_drops_the_equals_value() {
    // Confirmed against /usr/bin/qpdf 11.9.0: `--add-attachment=bogus.txt
    // payload.txt --` embeds `payload.txt` (the plain positional token)
    // and silently drops `bogus.txt` (the discarded `=value`).
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let bogus = temp.path().join("bogus.txt");
    let payload = temp.path().join("payload.txt");
    let output = temp.path().join("out.pdf");
    std::fs::write(&bogus, b"bogus").unwrap();
    std::fs::write(&payload, b"payload").unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            &format!("--add-attachment={}", bogus.to_str().unwrap()),
            payload.to_str().unwrap(),
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("payload.txt"))
        .stdout(predicate::str::contains("bogus.txt").not());
}

#[test]
fn add_attachment_equals_form_positional_named_overlay_is_not_hijacked_by_overlay_scanning() {
    // The overlay/underlay group scanner runs before the attachment group
    // scanner and, before this fix, matched on the exact string
    // `--add-attachment` only -- so it did not recognize
    // `--add-attachment=...` as the start of an opaque attachment segment
    // and would misread a positional file literally named `--overlay`
    // inside that segment as the start of a new overlay group.
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let overlay_named = temp.path().join("--overlay");
    let output = temp.path().join("out.pdf");
    std::fs::write(&overlay_named, b"payload").unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(temp.path())
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment=discarded",
            "--overlay",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("--overlay"));
}

#[test]
fn add_attachment_missing_path_fails_before_writing_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let missing = temp.path().join("missing.txt");
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            missing.to_str().unwrap(),
            "--key=missing",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("missing.txt"));
    assert!(!output.exists(), "failed attachment must not create output");
}

#[test]
fn add_attachment_repaired_input_keeps_output_and_exits_three_with_output_summary() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("damaged.pdf");
    let attachment = temp.path().join("hello.txt");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, corrupt_xref_with_info_pdf()).unwrap();
    std::fs::write(&attachment, b"hello world").unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--repair"])
        .arg(&input)
        .args(["--add-attachment"])
        .arg(&attachment)
        .args(["--key=hello", "--"])
        .arg(&output)
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings; resulting file may have some problems",
        ));

    assert!(
        output.exists(),
        "warning exit must retain attachment output"
    );
}

#[test]
fn add_attachment_explicit_key_and_filename() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("data.bin");
    std::fs::write(&attachment, b"binary data").unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=mykey",
            "--filename=renamed.bin",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("mykey"));
}

#[test]
fn add_attachment_non_ascii_basename_uses_qpdf_unicode_filename_for_f_and_uf() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("レポート.pdf");
    std::fs::write(&attachment, b"unicode filename payload").unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=unicode-key",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-attachment=unicode-key", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::eq(b"unicode filename payload" as &[u8]));

    let file = File::open(&output).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let entries = flpdf::embedded_files::list_embedded_files(&mut pdf).unwrap();
    let (_, filespec_ref) = entries
        .iter()
        .find(|(key, _)| key == b"unicode-key")
        .expect("unicode attachment must be present");
    let fs_obj = pdf.resolve_canonical_object(*filespec_ref).unwrap();
    assert_eq!(
        fs_obj.try_get_key(b"/F").unwrap().as_string(),
        Some(encode_utf16be("レポート.pdf")),
        "/F must match qpdf's Unicode filename when no compatibility name is supplied"
    );
    assert_eq!(
        fs_obj.try_get_key(b"/UF").unwrap().as_string(),
        Some(encode_utf16be("レポート.pdf")),
        "/UF must preserve the Unicode basename"
    );
}

#[test]
fn add_attachment_subflag_mimetype_description() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("report.pdf");
    std::fs::write(&attachment, b"%PDF-1.4 report").unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=report",
            "--mimetype=application/pdf",
            "--description=Annual Report",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", "--verbose", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("report"));
}

#[test]
fn add_attachment_subflag_creationdate_and_moddate() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("dated.txt");
    std::fs::write(&attachment, b"dated content").unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=dated",
            "--creationdate=D:20240101120000",
            "--moddate=D:20240201130000",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", "--verbose", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("dated"));
}

#[test]
fn add_attachment_replace_flag_overwrites_existing() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("file.txt");
    std::fs::write(&attachment, b"first content").unwrap();
    let out1 = temp.path().join("out1.pdf");

    // Add first version.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=myfile",
            "--",
            out1.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Update content and add with --replace.
    std::fs::write(&attachment, b"second content").unwrap();
    let out2 = temp.path().join("out2.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            out1.to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=myfile",
            "--replace",
            "--",
            out2.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Should still have exactly one entry.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", out2.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("myfile"));
}

#[test]
fn add_attachment_without_replace_fails_on_duplicate_key() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("file.txt");
    std::fs::write(&attachment, b"content").unwrap();
    let out1 = temp.path().join("out1.pdf");

    // Add first version.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=dupkey",
            "--",
            out1.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Add again without --replace → should fail.
    let out2 = temp.path().join("out2.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            out1.to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=dupkey",
            "--",
            out2.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("dupkey"))
        .stderr(predicate::str::contains("--replace"));
}

#[test]
fn remove_attachment_removes_existing_key() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("removeme.txt");
    std::fs::write(&attachment, b"to be removed").unwrap();
    let out1 = temp.path().join("out1.pdf");

    // Add the attachment.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=removeme",
            "--",
            out1.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Verify it's there.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", out1.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("removeme"));

    // Remove it.
    let out2 = temp.path().join("out2.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            out1.to_str().unwrap(),
            "--remove-attachment=removeme",
            "--preserve-unreferenced",
            out2.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Verify it's gone.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", out2.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn remove_attachment_errors_on_missing_key() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--remove-attachment=nosuchkey",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nosuchkey"));
}

#[test]
fn list_attachments_empty_document() {
    let input = minimal_pdf_temp();

    // qpdf names the input file when the catalog has no /Names /EmbeddedFiles
    // tree (QPDFJob::doListAttachments), rather than printing nothing.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", input.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(format!(
            "{} has no embedded files{EOL}",
            input.path().display()
        ));
}

#[test]
fn list_attachments_repaired_input_exits_three_with_inspection_summary() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--repair",
            "--list-attachments",
            "../../tests/fixtures/test_driver/repairable_input.pdf",
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(format!(
            "flpdf: operation succeeded with warnings{EOL}"
        )))
        .stderr(predicate::str::contains("resulting file may have some problems").not())
        .stdout(predicate::str::contains("has no embedded files"));
}

#[test]
fn list_attachments_shows_one_entry() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("listed.txt");
    std::fs::write(&attachment, b"listed content").unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=listed",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("listed"));
}

#[test]
fn list_attachments_verbose_shows_extra_info() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("verbose.txt");
    std::fs::write(&attachment, b"verbose content").unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=verbose",
            "--mimetype=text/plain",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    // --verbose should produce more output than plain --list-attachments.
    let plain_out = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", output.to_str().unwrap()])
        .output()
        .unwrap()
        .stdout;

    let verbose_out = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", "--verbose", output.to_str().unwrap()])
        .output()
        .unwrap()
        .stdout;

    // verbose output should be longer
    assert!(
        verbose_out.len() >= plain_out.len(),
        "verbose output should be at least as long as plain output"
    );
    // verbose output should mention the key
    assert!(
        String::from_utf8_lossy(&verbose_out).contains("verbose"),
        "verbose output should contain the key"
    );
}

#[test]
fn list_attachments_missing_ef_exits_three_with_two_type_warnings() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("missing-ef.pdf");
    std::fs::write(
        &input,
        malformed_attachment_pdf(
            "c.txt",
            "<< /Type /Filespec /F (c.txt) >>",
            "<< /Unused true >>",
        ),
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", "--verbose"])
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        output.stdout,
        format!(
            "c.txt -> 0,0{EOL}  preferred name: c.txt{EOL}  all names:{EOL}    /F -> c.txt{EOL}  all data streams:{EOL}"
        )
        .into_bytes()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr
            .matches("operation for dictionary attempted on object of type null")
            .count(),
        2,
        "qpdf ditems() emits begin/end warnings for missing /EF: {stderr}"
    );
    assert!(stderr.contains("flpdf: operation succeeded with warnings"));
}

#[test]
fn list_attachments_non_dictionary_filespec_exits_three_with_constructor_warning() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("non-dictionary-filespec.pdf");
    std::fs::write(
        &input,
        malformed_attachment_pdf("k.txt", "(not-a-filespec)", "<< /Unused true >>"),
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", "--verbose"])
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        output.stdout,
        format!(
            "k.txt -> 0,0{EOL}  preferred name: {EOL}  all names:{EOL}  all data streams:{EOL}"
        )
        .into_bytes()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Embedded file object is not a dictionary"));
    assert!(stderr.contains("operation for dictionary attempted on object of type string"));
    assert!(stderr.contains("flpdf: operation succeeded with warnings"));
}

#[test]
fn list_attachments_non_stream_ef_exits_two_with_stream_error() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("non-stream-ef.pdf");
    std::fs::write(
        &input,
        malformed_attachment_pdf(
            "g.txt",
            "<< /Type /Filespec /F (g.txt) /EF << /F 7 0 R >> >>",
            "<< /Type /EmbeddedFile >>",
        ),
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", "--verbose"])
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        output.stdout,
        format!(
            "g.txt -> 0,0{EOL}  preferred name: g.txt{EOL}  all names:{EOL}    /F -> g.txt{EOL}  all data streams:{EOL}    /F -> 7,0{EOL}      creation date: "
        )
        .into_bytes()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("operation for stream attempted on object of type dictionary"));
    assert!(!stderr.contains("operation succeeded with warnings"));
}

#[test]
fn show_attachment_writes_to_stdout() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let payload = b"payload bytes for stdout test";
    let attachment = temp.path().join("stdout.txt");
    std::fs::write(&attachment, payload).unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=showme",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let stdout_bytes = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-attachment=showme", output.to_str().unwrap()])
        .output()
        .unwrap()
        .stdout;

    assert_eq!(stdout_bytes, payload);
}

#[test]
fn show_attachment_errors_on_missing_key() {
    let input = minimal_pdf_temp();

    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            "--show-attachment=nosuchkey",
            input.path().to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(format!("qpdf: attachment nosuchkey not found{EOL}"));
}

#[test]
fn copy_attachments_from_copies_all_entries() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let source_input = minimal_pdf_temp();

    // Build a source PDF with two attachments.
    let att1 = temp.path().join("att1.txt");
    std::fs::write(&att1, b"attachment one").unwrap();
    let source_with_one = temp.path().join("src1.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            source_input.path().to_str().unwrap(),
            "--add-attachment",
            att1.to_str().unwrap(),
            "--key=att1",
            "--",
            source_with_one.to_str().unwrap(),
        ])
        .assert()
        .success();

    let att2 = temp.path().join("att2.txt");
    std::fs::write(&att2, b"attachment two").unwrap();
    let source_with_two = temp.path().join("src2.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            source_with_one.to_str().unwrap(),
            "--add-attachment",
            att2.to_str().unwrap(),
            "--key=att2",
            "--",
            source_with_two.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Copy attachments from the source into a fresh target.
    let output = temp.path().join("out.pdf");
    let copy = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--copy-attachments-from",
            source_with_two.to_str().unwrap(),
            "--",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(copy.status.success(), "copy failed: {copy:?}");
    assert!(
        copy.stderr.is_empty(),
        "qpdf-compatible copy must not emit a summary on stderr: {}",
        String::from_utf8_lossy(&copy.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("att1"))
        .stdout(predicate::str::contains("att2"));
}

#[test]
fn copy_attachments_from_with_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let source_input = minimal_pdf_temp();

    let att = temp.path().join("original.txt");
    std::fs::write(&att, b"original content").unwrap();
    let source = temp.path().join("source.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            source_input.path().to_str().unwrap(),
            "--add-attachment",
            att.to_str().unwrap(),
            "--key=original",
            "--",
            source.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = temp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--copy-attachments-from",
            source.to_str().unwrap(),
            "--prefix=pfx-",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("pfx-original"));
}

#[test]
fn bare_segment_equals_forms_discard_attached_values_like_qpdf() {
    let temp = tempfile::tempdir().unwrap();
    let one_page = "../../tests/fixtures/compat/one-page.pdf";

    let encrypted = temp.path().join("encrypted.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--encrypt=discarded",
            "user",
            "owner",
            "256",
            "--",
            one_page,
        ])
        .arg(&encrypted)
        .assert()
        .success();

    let pages = temp.path().join("pages.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([one_page, "--pages=discarded", one_page, "--"])
        .arg(&pages)
        .assert()
        .success();
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-npages", pages.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1"));

    let copied = temp.path().join("copied.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            one_page,
            "--copy-attachments-from=discarded",
            "../../tests/fixtures/compat/attachment-two-page.pdf",
            "--",
        ])
        .arg(&copied)
        .assert()
        .success();
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", copied.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("attachment.txt"));
}

/// qpdf 11.9.0 (observed): `--verbose --copy-attachments-from FILE` prints
/// `copying attachments from FILE`, `  key -> new_key` per copied entry, and
/// (once the operation writes an output file) `wrote file OUTPUT`.
#[test]
fn copy_attachments_from_verbose_prints_progress_and_wrote_file() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let source_input = minimal_pdf_temp();

    let att = temp.path().join("original.txt");
    std::fs::write(&att, b"original content").unwrap();
    let source = temp.path().join("source.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            source_input.path().to_str().unwrap(),
            "--add-attachment",
            att.to_str().unwrap(),
            "--key=original",
            "--",
            source.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = temp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--verbose",
            input.path().to_str().unwrap(),
            "--copy-attachments-from",
            source.to_str().unwrap(),
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "copying attachments from {}",
            source.display()
        )))
        .stdout(predicate::str::contains(format!(
            "  original -> original{EOL}"
        )))
        .stdout(predicate::str::contains(format!(
            "wrote file {}",
            output.display()
        )));
}

#[test]
fn attachment_round_trip_add_list_show_remove_copy() {
    // Full end-to-end round-trip for attachment lifecycle behavior.
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let payload = b"round-trip payload bytes \x00\x01\x02";
    let att = temp.path().join("rtrip.bin");
    std::fs::write(&att, payload).unwrap();

    // 1. add
    let after_add = temp.path().join("after_add.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            att.to_str().unwrap(),
            "--key=rtrip",
            "--",
            after_add.to_str().unwrap(),
        ])
        .assert()
        .success();

    // 2. list → contains "rtrip"
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", after_add.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("rtrip"));

    // 3. show → bytes match payload exactly
    let stdout_bytes = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-attachment=rtrip", after_add.to_str().unwrap()])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(stdout_bytes, payload.to_vec());

    // 4. remove
    let after_remove = temp.path().join("after_remove.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            after_add.to_str().unwrap(),
            "--remove-attachment=rtrip",
            after_remove.to_str().unwrap(),
        ])
        .assert()
        .success();

    // 5. list → empty
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", after_remove.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    // 6. copy from original (has "rtrip") into the now-empty PDF
    let after_copy = temp.path().join("after_copy.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            after_remove.to_str().unwrap(),
            "--copy-attachments-from",
            after_add.to_str().unwrap(),
            "--",
            after_copy.to_str().unwrap(),
        ])
        .assert()
        .success();

    // 7. list → "rtrip" reappears
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", after_copy.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("rtrip"));
}

#[test]
fn attachment_help_text_contains_expected_flags() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--add-attachment"))
        .stdout(predicate::str::contains("--remove-attachment"))
        .stdout(predicate::str::contains("--list-attachments"))
        .stdout(predicate::str::contains("--show-attachment"))
        .stdout(predicate::str::contains("--copy-attachments-from"));
}

/// Two attachment operations in one invocation must be a clean clap usage
/// error (mutually-exclusive ArgGroup), not silently running only the first.
#[test]
fn attachment_ops_are_mutually_exclusive() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("a.txt");
    std::fs::write(&attachment, b"a").unwrap();
    let src = minimal_pdf_temp();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=a",
            "--",
            "--copy-attachments-from",
            src.path().to_str().unwrap(),
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"))
        .stderr(predicate::str::contains("panicked").not());
}

/// A non-ASCII (e.g. fullwidth-digit) date must yield a clean CLI error,
/// never a byte-slice panic.
#[test]
fn add_attachment_non_ascii_date_is_clean_error_not_panic() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let attachment = temp.path().join("d.txt");
    std::fs::write(&attachment, b"d").unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=d",
            // Fullwidth digits: multibyte UTF-8, would panic a byte slice.
            "--creationdate=D:２０２４０１０１１２００００",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a valid PDF timestamp"))
        .stderr(predicate::str::contains("panicked").not());
}

fn corrupt_startxref(input: &[u8]) -> Vec<u8> {
    let mut bytes = input.to_vec();
    let Some(start) = bytes
        .windows(b"startxref\n".len())
        .position(|window| window == b"startxref\n")
    else {
        unreachable!("fixture should contain startxref token")
    };
    let digits_start = start + b"startxref\n".len();
    let Some(relative_end) = bytes[digits_start..].iter().position(|byte| *byte == b'\n') else {
        unreachable!("fixture should terminate startxref value")
    };
    for byte in &mut bytes[digits_start..digits_start + relative_end] {
        *byte = b'0';
    }
    bytes
}

fn corrupt_xref_with_info_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Info 5 0 R >>\nendobj\n".to_vec();
    let obj2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec();
    let obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R >>\nendobj\n".to_vec();
    let obj4 = b"4 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n".to_vec();
    let obj5 = b"5 0 obj\n<< /Title (Corrupt fixture) /Creator (flpdf) >>\nendobj\n".to_vec();

    let mut offsets = Vec::new();
    for object in [&obj1, &obj2, &obj3, &obj4, &obj5] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f\n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }

    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );

    let mut corrupted = bytes;
    let Some(pos) = corrupted.windows(4).position(|window| window == b"xref") else {
        unreachable!("fixture should contain xref token")
    };
    if let Some(byte) = corrupted.get_mut(pos + 2) {
        *byte = b'z';
    }

    corrupted
}

/// Regression: qpdf's cross-reference recovery permission is on by default
/// (`include/qpdf/QPDF.hh:1458-1462`), so a corrupt-but-recoverable
/// `--copy-attachments-from` donor must still recover and finish
/// (qpdf 11.9.0 exits 3, "operation succeeded with warnings", for this
/// exact scenario — verified empirically) even when the caller omits
/// `--repair`. `run_copy_attachments_from` used to construct the donor's
/// `PdfOpenOptions` from the raw `--repair` flag directly, bypassing the
/// `pdf_open_options` helper's default-recovery treatment — the flag's
/// absence turned recovery off for the donor specifically (the target
/// document was unaffected), diverging from qpdf, which recovers and exits
/// 3 rather than hard-failing.
#[test]
fn copy_attachments_from_corrupt_donor_recovers_without_explicit_repair_flag() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let donor = temp.path().join("corrupt-donor.pdf");
    std::fs::write(&donor, corrupt_xref_with_info_pdf()).unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--copy-attachments-from",
            donor.to_str().unwrap(),
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(format!(
            "WARNING: {}: file is damaged",
            donor.display()
        )))
        .stderr(predicate::str::contains(
            "Attempting to reconstruct cross-reference table",
        ))
        .stderr(predicate::str::contains(
            "operation succeeded with warnings",
        ));
}

// ── --no-original-object-ids ────────────────────────────────
//
// qpdf `--no-original-object-ids` omits the `%% Original object ID: N M`
// comments QDF output carries. Observed against qpdf 11.9.0: the flag changes
// ONLY QDF output (`qpdf --qdf` vs `qpdf --qdf --no-original-object-ids`);
// qpdf JSON v1/v2 is byte-identical with or without it. fulgur-qtest fails 52
// cases purely because the flag was "unrecognized"; the load-bearing fix is
// clap acceptance on both the top-level and `rewrite` surfaces.
//
// flpdf's QDF writer does not emit those comments, so the flag is a byte-level
// no-op: default output
// and `--no-original-object-ids` output must be byte-identical.

#[test]
fn top_level_no_original_object_ids_is_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let assert = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--no-original-object-ids",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
    // The whole point of .13.5: clap must NOT reject the flag as unknown.
    assert.stderr(predicate::str::contains("unrecognized").not());
    assert!(output.exists());
}

#[test]
fn rewrite_no_original_object_ids_is_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let assert = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--no-original-object-ids",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert.stderr(predicate::str::contains("unrecognized").not());
    assert!(output.exists());
}

#[test]
fn no_original_object_ids_default_behavior_unchanged() {
    // presence/absence parity: with no QDF-comment emission point yet, the
    // flag must not perturb any output byte. Compared same-surface (flag vs
    // no-flag on the SAME `rewrite` path) and made deterministic with
    // --static-id so the random trailer /ID does not cause a spurious diff.
    // This guards the default behavior: the comment is absent, and the flag
    // does not alter any output byte.
    let temp = tempfile::tempdir().unwrap();
    let baseline = temp.path().join("baseline.pdf");
    let with_flag = temp.path().join("with_flag.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args([
            "rewrite",
            "--static-id",
            "../../tests/fixtures/minimal.pdf",
            baseline.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args([
            "rewrite",
            "--static-id",
            "--no-original-object-ids",
            "../../tests/fixtures/minimal.pdf",
            with_flag.to_str().unwrap(),
        ])
        .assert()
        .success();

    let baseline_bytes = std::fs::read(&baseline).unwrap();
    let with_flag_bytes = std::fs::read(&with_flag).unwrap();
    assert_eq!(
        baseline_bytes, with_flag_bytes,
        "rewrite --no-original-object-ids must not change output bytes \
         (QDF original-object-ID comments are not emitted)"
    );
}

#[test]
fn no_original_object_ids_conflicts_with_json() {
    // Mirrors how `--static-id` conflicts with `--json`: combining a QDF/rewrite
    // modifier with --json is a usage error, not a silently-ignored flag.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--json=2",
            "--no-original-object-ids",
            "../../tests/fixtures/minimal.pdf",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"))
        .stderr(predicate::str::contains("--json"))
        .stderr(predicate::str::contains("--no-original-object-ids"));
}

// ===========================================================================
// --flatten-annotations / --generate-appearances /
// --flatten-rotation
// ===========================================================================

/// Assemble a classic cross-referenced PDF from a list of object bodies.
///
/// `objects[i]` is the full `"N 0 obj ... endobj\n"` body for object number
/// `i + 1`. The /Root is always object 1.
fn assemble_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(object);
    }
    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for &offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );
    bytes
}

/// Single-page PDF rotated 90 degrees (/Rotate on the page).
fn rotated_page_pdf() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Rotate 90 \
          /MediaBox [0 0 200 100] /Contents 4 0 R >>\nendobj\n"
            .to_vec(),
        // /Length is the exact byte count of `BT (hi) Tj ET\n` (14 bytes); the
        // full-rewrite reparse validates stream lengths strictly.
        b"4 0 obj\n<< /Length 14 >>\nstream\nBT (hi) Tj ET\nendstream\nendobj\n".to_vec(),
    ])
}

/// Single-page AcroForm PDF with one Tx widget that carries `/V` but no `/AP`
/// and requests qpdf-style appearance generation with `/NeedAppearances true`.
fn tx_form_pdf_without_ap() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm \
          << /Fields [4 0 R] /NeedAppearances true /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n"
            .to_vec(),
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (field1) \
          /V (Hello World) /Rect [100 700 300 720] /P 3 0 R >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
    ])
}

/// Single-page AcroForm PDF with one Tx widget that already has an `/AP` `/N`
/// Form XObject (so it can be flattened without first generating appearances).
fn tx_form_pdf_with_ap() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm \
          << /Fields [4 0 R] /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n"
            .to_vec(),
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (field1) \
          /V (Hello) /Rect [100 700 300 720] /P 3 0 R \
          /AP << /N 6 0 R >> >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
        b"6 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 200 20] \
          /Length 17 >>\nstream\nBT (Hello) Tj ET\nendstream\nendobj\n"
            .to_vec(),
    ])
}

/// Single-page AcroForm PDF with one Tx widget whose `/AP` `/N` is explicitly
/// `null` (must be treated as absent — a real appearance should be generated).
/// The document also sets `/NeedAppearances true`, which is qpdf's generation
/// gate.
fn tx_form_pdf_with_null_ap_n() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm \
          << /Fields [4 0 R] /NeedAppearances true /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 5 0 R /Annots [4 0 R] >>\nendobj\n"
            .to_vec(),
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (field1) \
          /V (Hello World) /Rect [100 700 300 720] /P 3 0 R \
          /AP << /N null >> >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\nendobj\n".to_vec(),
    ])
}

/// `--generate-appearances` must treat `/AP << /N null >>` as a *missing*
/// appearance and synthesize a real one — not skip it (which would leave the
/// widget value undrawable / droppable on a later flatten pass).
#[test]
fn rewrite_generate_appearances_replaces_null_ap_n() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("form.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_form_pdf_with_null_ap_n()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget = first_widget_ref(&mut pdf);
    let mut helper = AnnotationObjectHelper::new(widget, &mut pdf);
    let n = helper.get_appearance_stream(b"N", None).unwrap();
    // /N must now be a real (non-null) appearance, resolving to a Form XObject
    // stream — not the original null.
    assert!(
        n.as_stream_dict().is_some(),
        "null /AP/N must be replaced by a real appearance stream"
    );
}

/// `/Rotate` on a leaf page is removed (baked into content) by
/// `--flatten-rotation`; the command exits 0 and produces a valid PDF.
#[test]
fn rewrite_flatten_rotation_removes_rotate() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("rotated.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, rotated_page_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--flatten-rotation"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_refs = flpdf::pages::page_refs(&mut pdf).unwrap();
    let page = pdf.resolve_canonical_object(page_refs[0]).unwrap();
    // After flattening, /Rotate is either absent or normalized to 0.
    let rotate = page.try_get_key(b"/Rotate").unwrap().as_integer();
    assert!(
        rotate.is_none() || rotate == Some(0),
        "page /Rotate should be absent or 0 after --flatten-rotation, got {rotate:?}"
    );
}

/// `--generate-appearances` synthesizes an `/AP` `/N` stream for a Tx widget
/// that lacks one.
#[test]
fn rewrite_generate_appearances_adds_ap_n() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("form.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_form_pdf_without_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--generate-appearances"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let widget = first_widget_ref(&mut pdf);
    let mut helper = AnnotationObjectHelper::new(widget, &mut pdf);
    let ap = helper.get_appearance_dictionary().unwrap();
    assert!(
        !ap.get_key(b"/N").is_null(),
        "widget /AP should carry an /N normal appearance after --generate-appearances"
    );
}

/// `--flatten-annotations=all` bakes a widget that already has an `/AP` `/N`
/// into page content and drops it from `/Annots`.
#[test]
fn rewrite_flatten_annotations_all_removes_widget_from_annots() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("form.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_form_pdf_with_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--flatten-annotations=all"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_refs = flpdf::pages::page_refs(&mut pdf).unwrap();
    let annots = page_annotation_handles(&mut pdf, page_refs[0]);
    assert!(
        annots.is_empty(),
        "flattened widget should be removed from /Annots, found {} annotation(s)",
        annots.len()
    );
}

/// `--json-output` has no dispatch check of its own the way `--json` does
/// (main's dispatch chain routes to `run_json` for either flag before any
/// rewrite path that consumes `flatten_annotations`), so without this
/// conflict `--flatten-annotations=all --json-output=2 IN OUT` would exit 0
/// and silently write a JSON dump of the unmodified input while dropping the
/// requested transformation entirely.
#[test]
fn top_level_flatten_annotations_rejects_json_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("form.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_form_pdf_with_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--flatten-annotations=all", "--json-output=2"])
        .arg(&input)
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot be used with '--json-output",
        ));
}

/// The qpdf-compatible top-level form must route the same flattening
/// transformation as the native `rewrite` subcommand.
#[test]
fn top_level_flatten_annotations_all_removes_widget_from_annots() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("form.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_form_pdf_with_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--qdf",
            "--static-id",
            "--no-original-object-ids",
            "--flatten-annotations=all",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_refs = flpdf::pages::page_refs(&mut pdf).unwrap();
    let annots = page_annotation_handles(&mut pdf, page_refs[0]);
    assert!(
        annots.is_empty(),
        "top-level flattening should remove the widget from /Annots"
    );
}

/// qpdf applies annotation flattening before its linearized writer as well as
/// before the ordinary rewrite writer.
#[test]
fn top_level_flatten_annotations_linearize_removes_widget_from_annots() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("form.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_form_pdf_with_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--linearize",
            "--static-id",
            "--no-original-object-ids",
            "--flatten-annotations=all",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_refs = flpdf::pages::page_refs(&mut pdf).unwrap();
    let annots = page_annotation_handles(&mut pdf, page_refs[0]);
    assert!(
        annots.is_empty(),
        "linearized top-level flattening should remove the widget from /Annots"
    );
}

/// qpdf reports a warning and exits 3 when flattening a document whose
/// `/NeedAppearances` marker says form appearances are stale.
#[test]
fn top_level_flatten_annotations_preserves_need_appearances_warning_status() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("form.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_form_pdf_without_ap()).unwrap();

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--qdf", "--static-id", "--no-original-object-ids"])
        .arg("--flatten-annotations=all")
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("document does not have updated appearance streams, so form fields will not be flattened"),
        "top-level flattening must preserve qpdf's NeedAppearances warning"
    );
    assert!(output.is_file());
}

/// `--no-warn` suppresses the printed NeedAppearances warning text and the
/// trailing "operation succeeded with warnings" summary, while the exit
/// status still correctly reflects the recorded warning (qpdf's
/// `suppress_warnings` gates delivery, not collection: `QPDF::warn` always
/// records, only the print is conditional).
#[test]
fn top_level_flatten_annotations_no_warn_suppresses_warning_text_but_keeps_exit_status() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("form.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_form_pdf_without_ap()).unwrap();

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--qdf",
            "--static-id",
            "--no-original-object-ids",
            "--no-warn",
        ])
        .arg("--flatten-annotations=all")
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    assert!(
        result.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert!(
        result.stderr.is_empty(),
        "--no-warn must suppress both the WARNING line and the trailing \
         summary: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(output.is_file());
}

/// `--generate-appearances` followed by `--flatten-annotations=all` cooperate:
/// the missing appearance is generated first, then the widget is flattened into
/// page content and removed from `/Annots`. Without the ordering (generate
/// before flatten) the value-only widget would have no `/AP` to bake and would
/// survive in `/Annots`.
#[test]
fn rewrite_generate_then_flatten_cooperate() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("form.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, tx_form_pdf_without_ap()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--generate-appearances",
            "--flatten-annotations=all",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page_refs = flpdf::pages::page_refs(&mut pdf).unwrap();
    let annots = page_annotation_handles(&mut pdf, page_refs[0]);
    assert!(
        annots.is_empty(),
        "widget should be generated-then-flattened away, found {} annotation(s)",
        annots.len()
    );
}

/// An invalid `--flatten-annotations` value is rejected by clap with a non-zero
/// exit and a diagnostic on stderr.
#[test]
fn rewrite_flatten_annotations_rejects_invalid_mode() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--flatten-annotations=bogus",
            "../../tests/fixtures/minimal.pdf",
            "/tmp/flpdf-flatten-invalid-out.pdf",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("flatten-annotations"));
}

#[test]
fn rewrite_help_describes_screen_mode_as_including_printable_annotations() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("including printable annotations"))
        .stdout(predicate::str::contains("not when printed").not());
}

/// Combining `--linearize` with `--flatten-rotation` keeps the transformation
/// enabled on the linearized rewrite branch.
#[test]
fn rewrite_linearize_with_flatten_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("rotated.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, rotated_page_pdf()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--linearize", "--flatten-rotation"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(
        output.exists(),
        "linearized flattened output must be created"
    );
}
