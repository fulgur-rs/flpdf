use assert_cmd::Command;
use flpdf::{
    acroform_sig_flags, filespec_helper::encode_utf16be, AnnotationObjectHelper, Object, Pdf,
};
use predicates::prelude::*;
use std::fs::File;
use std::io::BufReader;
use std::io::Write;

mod common;
use common::first_widget_ref;

#[test]
fn check_valid_fixture_exits_successfully() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("File is not encrypted\n"));
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
    .stdout(predicate::str::contains("File is encrypted\n"));
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
    .stderr(predicate::str::contains("incorrect password"))
    .stderr(predicate::str::contains("--password"));
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
        .stdout(predicate::str::contains("File is encrypted\n"))
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
        .stdout(predicate::str::contains("File is encrypted\n"))
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
    .stderr(predicate::str::contains("incorrect password"))
    .stderr(predicate::str::contains("--password"));
}

#[test]
fn rewrite_encrypted_fixture_writes_plaintext_output() {
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
        .stdout(predicate::str::contains("File is not encrypted\n"));
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
    .stdout(predicate::str::contains("File is encrypted\n"));
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
        .stdout(predicate::str::contains("File is encrypted\n"));
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
fn rewrite_remove_restrictions_strips_signatures_and_warns() {
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
        .stderr(predicate::str::contains("signatures are now invalidated"));

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
        !output_bytes.windows(3).any(|window| window == b"/V "),
        "signature field /V entries must be removed"
    );
    assert!(
        !output_bytes.windows(3).any(|window| window == b"/FT"),
        "signature field /FT entries must be removed (field object GC'd)"
    );
    assert!(
        !output_bytes
            .windows(b"/ByteRange".len())
            .any(|window| window == b"/ByteRange"),
        "orphaned signature dictionaries must be removed"
    );
}

#[test]
fn rewrite_linearize_remove_restrictions_strips_signatures_and_warns() {
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
        .stderr(predicate::str::contains("signatures are now invalidated"));

    let file = File::open(&output).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
    assert!(
        pdf.signatures().unwrap().is_empty(),
        "linearized --remove-restrictions output must not report signed fields"
    );

    let output_bytes = std::fs::read(&output).unwrap();
    assert!(
        !output_bytes.windows(3).any(|window| window == b"/V "),
        "linearized signature field /V entries must be removed"
    );
    assert!(
        !output_bytes
            .windows(b"/ByteRange".len())
            .any(|window| window == b"/ByteRange"),
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
        .stderr(predicate::str::contains("signatures are now invalidated"));

    let output_bytes = std::fs::read(&output).unwrap();
    for tok in [&b"/Perms"[..], &b"/DocMDP"[..], &b"/ByteRange"[..]] {
        assert!(
            !output_bytes.windows(tok.len()).any(|w| w == tok),
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
        .stderr(predicate::str::contains("signatures are now invalidated"));

    let output_bytes = std::fs::read(&output).unwrap();
    // Widget annotation survives (reachable from page /Annots) ...
    assert!(output_bytes
        .windows(b"/Widget".len())
        .any(|w| w == b"/Widget"));
    // ... but its signature keys are gone.
    assert!(!output_bytes
        .windows(b"/ByteRange".len())
        .any(|w| w == b"/ByteRange"));

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
fn rewrite_remove_restrictions_strips_docmdp_perms_and_warns() {
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
        .stderr(predicate::str::contains("signatures are now invalidated"));

    let file = File::open(&output).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    assert!(
        pdf.signatures().unwrap().is_empty(),
        "DocMDP --remove-restrictions output must not report signed fields"
    );

    let output_bytes = std::fs::read(&output).unwrap();
    assert!(
        !output_bytes
            .windows(b"/Perms".len())
            .any(|window| window == b"/Perms"),
        "catalog /Perms must be removed"
    );
    assert!(
        !output_bytes
            .windows(b"/DocMDP".len())
            .any(|window| window == b"/DocMDP"),
        "the /DocMDP dictionary must be removed with /Perms"
    );
    assert!(
        !output_bytes
            .windows(b"/ByteRange".len())
            .any(|window| window == b"/ByteRange"),
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
        .stderr(predicate::str::contains("signatures are now invalidated"));

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
        output_bytes
            .windows(b"/Subtype /Widget".len())
            .any(|window| window == b"/Subtype /Widget"),
        "the widget annotation must survive as a page annotation"
    );
    assert!(
        output_bytes
            .windows(b"/T (Approval)".len())
            .any(|window| window == b"/T (Approval)"),
        "the surviving widget must keep its /T field name"
    );
    // ...but its signature identity is stripped.
    assert!(
        !output_bytes.windows(3).any(|window| window == b"/FT"),
        "the surviving widget must lose its /FT (signature field type)"
    );
    assert!(
        !output_bytes
            .windows(b"/ByteRange".len())
            .any(|window| window == b"/ByteRange"),
        "the orphaned signature dictionary must be garbage-collected"
    );
}

#[test]
fn rewrite_default_is_qpdf_equivalent_full_rewrite() {
    // flpdf-9hc.12.7 acceptance: a plain `flpdf rewrite IN OUT` (no flags)
    // must match qpdf's documented defaults — qpdf full-rewrites and applies
    // --compress-streams=y by default. This asserts that the deliberate
    // default behavior (full rewrite + FlateDecode compression) holds, so a
    // regression to a verbatim/incremental no-op default would be caught.
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
    .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

#[test]
fn check_subcommand_succeeds() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["check", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("File is not encrypted\n"));
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
    cmd.args(["pages", "--count", fixture.path().to_str().unwrap()])
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
    // The `qdf` subcommand is now an alias of `rewrite --qdf` (epic
    // flpdf-9hc.6 architecture decision): it must emit canonical QDF, not the
    // old standalone `write_qdf` raw dump.
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
// These exist so the qpdf qtest acceptance harness (which PATH-shims
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
    // (flpdf-9hc.13.4). This test mirrors qpdf qtest's "no stdout/stderr"
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

    assert!(output.exists());
    assert!(pass1.exists());
}

// ---------------------------------------------------------------------------
// Version validation tests
// ---------------------------------------------------------------------------

#[test]
fn rewrite_force_version_invalid_abc_exits_nonzero() {
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
    .failure()
    .stderr(predicate::str::contains("invalid --force-version"));
}

#[test]
fn rewrite_force_version_with_newline_exits_nonzero() {
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
    .failure()
    .stderr(predicate::str::contains("invalid --force-version"));
}

#[test]
fn rewrite_min_version_invalid_abc_exits_nonzero() {
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
    .failure()
    .stderr(predicate::str::contains("invalid --min-version"));
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
    // line down to exactly 1.4 (acceptance: "Output header line matches the
    // chosen version"). flpdf-9hc.13.1.
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
    // it must be a no-op (header stays 1.7). flpdf-9hc.13.1.
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
    // must be raised to exactly 1.7. flpdf-9hc.13.1.
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
fn rewrite_force_version_honored_on_incremental_path() {
    // Regression for flpdf-9hc.13.1: `--remove-unreferenced-resources=no`
    // with no other mutation flag would otherwise take the incremental-update
    // write path, which copies the source header verbatim and silently drops
    // --force-version. The CLI must promote to full_rewrite so the version
    // setter is honored (qpdf always full-rewrites and always honors it).
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
        "force-version must be honored even on the would-be incremental path; \
         got {:?}",
        std::str::from_utf8(&bytes[..bytes.len().min(9)]).unwrap_or("<bad>")
    );
}

#[test]
fn show_info_with_repair_flag_handles_corrupt_xref() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_with_info_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--repair", "--show-info", input.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Title = (Corrupt fixture)"));
}

#[test]
fn show_info_without_repair_rejects_corrupt_xref() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_with_info_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-info", input.to_str().unwrap()])
        .assert()
        .failure();
}

#[test]
fn check_without_repair_rejects_corrupt_xref() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_with_info_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", input.to_str().unwrap()])
        .assert()
        .failure();
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
        .stdout(predicate::str::contains("File is not encrypted\n"));
}

#[test]
fn dump_object_accepts_ref_without_suffix() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--dump-object", "1 0", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/Type /Catalog"));
}

#[test]
fn dump_object_accepts_ref_with_r_suffix() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--dump-object", "1 0 R", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/Type /Catalog"));
}

#[test]
fn dump_object_rejects_invalid_ref() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--dump-object", "bad", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .failure();
}

#[test]
fn show_info_prints_document_info() {
    let fixture = fixture_with_metadata_outline_and_fonts();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-info", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Title = (Fixture PDF)"))
        .stdout(predicate::str::contains("Creator = (flpdf)"));
}

#[test]
fn show_catalog_prints_root_dictionary() {
    let fixture = fixture_with_metadata_outline_and_fonts();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-catalog", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Catalog: <<"))
        .stdout(predicate::str::contains("/Type /Catalog"));
}

#[test]
fn show_metadata_prints_stream_summary() {
    let fixture = fixture_with_metadata_outline_and_fonts();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-metadata", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Metadata: stream"))
        .stdout(predicate::str::contains("/XML"));
}

#[test]
fn show_outline_prints_titles() {
    let fixture = fixture_with_metadata_outline_and_fonts();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-outline", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1: Chapter One"));
}

#[test]
fn show_fonts_prints_summary() {
    let fixture = fixture_with_metadata_outline_and_fonts();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-fonts", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("F1"))
        .stdout(predicate::str::contains("F2"));
}

#[test]
fn show_fonts_prints_inline_dictionary_fonts() {
    let fixture = fixture_with_inline_font_dictionary();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-fonts", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("FDirect"))
        .stdout(predicate::str::contains("type: /Font"));
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
        .stdout(predicate::str::contains("media-box: [ 0 0 595.28 842 ]"))
        .stdout(predicate::str::contains("media-box: [ 0 0 200 100 ]"));
}

fn fixture_with_metadata_outline_and_fonts() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();

    let object1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R /Metadata 4 0 R /Info 5 0 R >>\nendobj\n";
    let object2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [6 0 R] >>\nendobj\n";
    let object3 = b"3 0 obj\n<< /Type /Outlines /First 10 0 R /Last 10 0 R /Count 1 >>\nendobj\n";
    let metadata_data = b"<xmpmeta>Fixture metadata</xmpmeta>";
    let object4 = format!(
        "4 0 obj\n<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        metadata_data.len(),
        String::from_utf8_lossy(metadata_data)
    )
    .into_bytes();
    let object5 = b"5 0 obj\n<< /Title (Fixture PDF) /Creator (flpdf) >>\nendobj\n";
    let object6 = b"6 0 obj\n<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R /F2 8 0 R >> >> /MediaBox [0 0 612 792] /Contents 9 0 R >>\nendobj\n";
    let object7 = b"7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>\nendobj\n";
    let object8 = b"8 0 obj\n<< /Type /Font /Subtype /Type0 /BaseFont /Courier >>\nendobj\n";
    let content_data = b"BT /F1 12 Tf (Hello) Tj ET";
    let object9 = format!(
        "9 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        content_data.len(),
        String::from_utf8_lossy(content_data)
    )
    .into_bytes();
    let object10 =
        b"10 0 obj\n<< /Title (Chapter One) /Parent 3 0 R /Dest [6 0 R /Fit] >>\nendobj\n";

    let objects = vec![
        object1.to_vec(),
        object2.to_vec(),
        object3.to_vec(),
        object4,
        object5.to_vec(),
        object6.to_vec(),
        object7.to_vec(),
        object8.to_vec(),
        object9,
        object10.to_vec(),
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
            "trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );

    let file = fixture.as_file_mut();
    file.write_all(&bytes).unwrap();

    fixture
}

fn fixture_with_inline_font_dictionary() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();

    let object1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
    let object2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n";
    let object3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Resources << /Font << /FDirect << /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >> >> >> /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n";
    let content_data = b"HelloPDF\n";
    let object4 = format!(
        "4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        content_data.len(),
        String::from_utf8_lossy(content_data)
    )
    .into_bytes();

    let mut offsets = Vec::new();
    let objects: Vec<Vec<u8>> = vec![
        object1.to_vec(),
        object2.to_vec(),
        object3.to_vec(),
        object4.to_vec(),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    for object in &objects {
        offsets.push(bytes.len());
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
/// dictionary — there is NO `/AcroForm`. This exercises the catalog-`/Perms`
/// detection branch of `pdf_has_signature_evidence`. The DocMDP signature is
/// obj 4 (referenced only via `/Perms`), so dropping `/Perms` orphans it and it
/// is garbage-collected on the full rewrite.
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

/// Walk Catalog → `/AcroForm` → `/Fields` (resolving an indirect reference at
/// each hop) and return the number of field entries. Returns `None` when the
/// document has no `/AcroForm` dictionary or no `/Fields` array — distinct from
/// `Some(0)` (an AcroForm that survives with an emptied `/Fields`).
fn acroform_fields_len(pdf: &mut Pdf<BufReader<File>>) -> Option<usize> {
    let root_ref = pdf.root_ref()?;
    let Object::Dictionary(catalog) = pdf.resolve(root_ref).ok()? else {
        return None;
    };
    let acroform = resolve_maybe_indirect(pdf, catalog.get("AcroForm")?.clone())?;
    let fields = resolve_maybe_indirect(pdf, acroform.as_dict()?.get("Fields")?.clone())?;
    Some(fields.as_array()?.len())
}

/// Resolve `object` if it is an indirect reference, otherwise return it as-is.
fn resolve_maybe_indirect(pdf: &mut Pdf<BufReader<File>>, object: Object) -> Option<Object> {
    match object {
        Object::Reference(object_ref) => pdf.resolve(object_ref).ok(),
        direct => Some(direct),
    }
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
// flpdf-9hc.12.7: CLI flags --compress-streams / --normalize-content /
//                 --coalesce-contents / --remove-unreferenced-resources /
//                 --newline-before-endstream
// ---------------------------------------------------------------------------

/// Minimal single-page PDF with a content stream and a font resource entry.
/// `/F2` is NOT referenced in the content stream. On a plain `rewrite` flpdf
/// keeps it — matching qpdf, which only prunes `/Resources` entries during page
/// operations (`--pages`), not on a plain rewrite (flpdf-79ef). The tests using
/// this fixture assert flag acceptance + structurally valid output, not pruning;
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
        .args(["rewrite", "--compress-streams=y", "--full-rewrite"])
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
        .args(["rewrite", "--compress-streams=n", "--full-rewrite"])
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
        .args(["rewrite", "--newline-before-endstream=y", "--full-rewrite"])
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
        .args(["rewrite", "--newline-before-endstream=n", "--full-rewrite"])
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

// ── help text contains qpdf-compatible defaults ───────────────────────────────

#[test]
fn rewrite_help_shows_compress_streams_default_y() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("compress-streams"))
        .stdout(predicate::str::contains("default: y"));
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
fn rewrite_help_shows_newline_before_endstream_default_y() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("newline-before-endstream"))
        .stdout(predicate::str::contains("default: y"));
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
            "--full-rewrite",
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
// (flpdf-9hc.8.12).
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

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-pages", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("rotate: 180"));
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

    let show = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-pages", output.to_str().unwrap()])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&show.get_output().stdout).into_owned();
    // Two output pages; first rotated 90, second 0.
    let p1 = stdout.find("page 1:").unwrap();
    let p2 = stdout.find("page 2:").unwrap();
    assert!(
        stdout[p1..p2].contains("rotate: 90"),
        "page 1 should be rotated 90: {stdout}"
    );
    assert!(
        stdout[p2..].contains("rotate: 0"),
        "page 2 should stay 0: {stdout}"
    );
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
        .args(["--show-outline", output.to_str().unwrap()])
        .assert()
        .success();
    let outline_txt = String::from_utf8_lossy(&outline.get_output().stdout).into_owned();
    assert!(
        outline_txt.contains("Item1")
            && outline_txt.contains("Item2")
            && outline_txt.contains("Item3"),
        "all outline items must be kept (null-out): {outline_txt}"
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
        .args(["--show-outline", output.to_str().unwrap()])
        .assert()
        .success();
    let txt = String::from_utf8_lossy(&outline.get_output().stdout).into_owned();
    assert!(txt.contains("Item1") && txt.contains("Item2") && txt.contains("Item3"));
}

// ── Scope-boundary errors (actionable, not swallowed) ─────────────────────

#[test]
fn pages_cross_document_merge_is_rejected_actionably() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(THREE_PAGE)
        .args(["--pages", ".", "1", TWO_PAGE, "2", "--"])
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicate::str::contains("cross-document"))
        .stderr(predicate::str::contains("not supported"));
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

// ── Attachment tests (flpdf-9hc.10.9) ────────────────────────────────────────

/// Write a minimal valid PDF to a tempfile and return the path.
fn minimal_pdf_temp() -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(include_bytes!("../../../tests/fixtures/minimal.pdf"))
        .unwrap();
    f
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
fn add_attachment_non_ascii_basename_uses_ascii_fallback_and_unicode_uf() {
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
    let fs_obj = pdf.resolve(*filespec_ref).unwrap();
    let Object::Dictionary(fs_dict) = fs_obj else {
        panic!("expected filespec dictionary");
    };

    assert_eq!(
        fs_dict.get("F"),
        Some(&Object::String(b"____.pdf".to_vec())),
        "/F must be ASCII-safe fallback"
    );
    assert_eq!(
        fs_dict.get("UF"),
        Some(&Object::String(encode_utf16be("レポート.pdf"))),
        "/UF must preserve the Unicode basename"
    );
}

#[test]
fn add_attachment_subflag_mimetype_description_afrelationship() {
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
            "--afrelationship=Data",
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

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", input.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
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
fn show_attachment_writes_to_file_with_o_flag() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let payload = b"payload bytes for file test";
    let attachment = temp.path().join("tofile.txt");
    std::fs::write(&attachment, payload).unwrap();
    let out_pdf = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--key=tofile",
            "--",
            out_pdf.to_str().unwrap(),
        ])
        .assert()
        .success();

    let extracted = temp.path().join("extracted.txt");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--show-attachment=tofile",
            "--show-attachment-to",
            extracted.to_str().unwrap(),
            out_pdf.to_str().unwrap(),
        ])
        .assert()
        .success();

    let read_back = std::fs::read(&extracted).unwrap();
    assert_eq!(read_back, payload);
}

#[test]
fn show_attachment_errors_on_missing_key() {
    let input = minimal_pdf_temp();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--show-attachment=nosuchkey",
            input.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nosuchkey"));
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
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--copy-attachments-from",
            source_with_two.to_str().unwrap(),
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
fn attachment_round_trip_add_list_show_remove_copy() {
    // Full end-to-end round-trip as described in the subtask acceptance criteria.
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
        .stderr(predicate::str::contains("invalid PDF date"))
        .stderr(predicate::str::contains("panicked").not());
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

// ── --no-original-object-ids (flpdf-9hc.13.5) ────────────────────────────────
//
// qpdf `--no-original-object-ids` omits the `%% Original object ID: N M`
// comments QDF output carries. Observed against qpdf 11.9.0: the flag changes
// ONLY QDF output (`qpdf --qdf` vs `qpdf --qdf --no-original-object-ids`);
// qpdf JSON v1/v2 is byte-identical with or without it. fulgur-qtest fails 52
// cases purely because the flag was "unrecognized"; the load-bearing fix is
// clap acceptance on both the top-level and `rewrite` surfaces.
//
// flpdf's QDF writer does not yet emit those comments (the comment body is
// epic flpdf-9hc.6), so today the flag is a byte-level no-op: default output
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
    // This guards the "default behavior unchanged" acceptance criterion and
    // will keep holding once flpdf-9hc.6 adds the comment body (the comment
    // is absent by default; the flag only suppresses an opt-in QDF annotation).
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
         (no QDF-comment emission point exists yet; flpdf-9hc.6)"
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
// flpdf-9hc.9.10: --flatten-annotations / --generate-appearances /
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

/// Single-page AcroForm PDF with one Tx widget that carries `/V` but no `/AP`.
fn tx_form_pdf_without_ap() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm \
          << /Fields [4 0 R] /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
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
fn tx_form_pdf_with_null_ap_n() -> Vec<u8> {
    assemble_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm \
          << /Fields [4 0 R] /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\nendobj\n"
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
    let ap = helper
        .appearance()
        .unwrap()
        .expect("widget should still have an /AP");
    let n = ap.get("N").cloned();
    // /N must now be a real (non-null) appearance, and resolve to a stream.
    assert!(
        matches!(n, Some(Object::Reference(_)) | Some(Object::Stream(_))),
        "null /AP/N must be replaced by a real appearance, got {n:?}"
    );
    let n_resolved = match n {
        Some(Object::Reference(r)) => pdf.resolve(r).unwrap(),
        Some(other) => other,
        None => panic!("/N missing"),
    };
    assert!(
        matches!(n_resolved, Object::Stream(_)),
        "/AP/N must resolve to a Form XObject stream"
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
    let page = pdf.resolve(page_refs[0]).unwrap();
    let Object::Dictionary(dict) = page else {
        panic!("page is not a dictionary");
    };
    // After flattening, /Rotate is either absent or normalized to 0.
    let rotate = dict.get("Rotate").and_then(|o| o.as_integer());
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
    let ap = helper
        .appearance()
        .unwrap()
        .expect("widget should have an /AP after --generate-appearances");
    assert!(
        ap.get("N").is_some(),
        "widget /AP should carry an /N normal appearance"
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
    let annots = flpdf::enumerate_page_annotations(&mut pdf, page_refs[0]).unwrap();
    assert!(
        annots.is_empty(),
        "flattened widget should be removed from /Annots, found {} annotation(s)",
        annots.len()
    );
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
    let annots = flpdf::enumerate_page_annotations(&mut pdf, page_refs[0]).unwrap();
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

/// Combining `--linearize` with the flatten/generate passes is rejected loudly
/// rather than silently dropping the requested transformation (the passes only
/// run on the non-linearize rewrite branch).
#[test]
fn rewrite_linearize_with_flatten_is_rejected() {
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
        .failure()
        .stderr(predicate::str::contains("--linearize cannot be combined"));
}
