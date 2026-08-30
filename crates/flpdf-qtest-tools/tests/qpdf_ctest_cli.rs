use assert_cmd::Command;
use flpdf::Pdf;
use std::fs;
use std::io::Cursor;

fn minimal_pdf() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/minimal.pdf")
}

fn encrypted_fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/encrypted")
        .join(name)
}

#[test]
fn qpdf_ctest_2_reports_invalid_password_through_the_c_api_error_surface() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = encrypted_fixture("v1-rc4-40-r2.pdf");
    let output = directory.path().join("unused.pdf");
    let input_name = input.to_str().expect("input path is UTF-8");
    let expected = format!(
        "error: {input_name}: invalid password\n  code: 4\n  file: {input_name}\n  pos: 0\n  text: invalid password\nC test 2 done\n"
    );

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args(["2", input_name, "wrong", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(expected)
        .stderr("");
    assert!(
        !output.exists(),
        "test02 must not initialize a writer after auth failure"
    );
}

#[test]
fn qpdf_ctest_encryption_writer_cases_cover_r2_through_r6() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = minimal_pdf();
    let cases = [
        ("11", "user1"),
        ("12", "user2"),
        ("15", "user2"),
        ("17", "user3"),
        ("18", "user4"),
    ];

    for (test_number, user_password) in cases {
        let output = directory.path().join(format!("test-{test_number}.pdf"));
        Command::cargo_bin("qpdf-ctest")
            .expect("qpdf-ctest binary")
            .args([
                test_number,
                input.to_str().unwrap(),
                "",
                output.to_str().unwrap(),
            ])
            .assert()
            .success()
            .stdout(format!("C test {test_number} done\n"))
            .stderr("");

        let pdf = Pdf::open_with_options(
            std::fs::File::open(&output).expect("open ctest output"),
            flpdf::PdfOpenOptions {
                password: user_password.as_bytes().to_vec(),
                ..flpdf::PdfOpenOptions::default()
            },
        )
        .expect("ctest encryption output must authenticate");
        assert!(pdf.is_encrypted());
    }
}

#[test]
fn qpdf_ctest_13_recovers_the_user_password_and_decrypts() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = encrypted_fixture("v2-rc4-128-r3.pdf");
    let output = directory.path().join("decrypted.pdf");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "13",
            input.to_str().unwrap(),
            "user-v2",
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout("user password: user-v2\nC test 13 done\n")
        .stderr("");

    let pdf = Pdf::open(std::io::Cursor::new(std::fs::read(output).unwrap()))
        .expect("test13 output must be plaintext");
    assert!(!pdf.is_encrypted());
}

#[test]
fn qpdf_ctest_19_writes_the_same_deterministic_pdf_twice() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("input.pdf");
    let first = directory.path().join("first.pdf");
    let second = directory.path().join("second.pdf");
    fs::copy(minimal_pdf(), &input).expect("copy input PDF");

    for output in [&first, &second] {
        Command::cargo_bin("qpdf-ctest")
            .expect("qpdf-ctest binary")
            .args([
                "19",
                input.to_str().expect("input path is UTF-8"),
                "",
                output.to_str().expect("output path is UTF-8"),
            ])
            .assert()
            .success()
            .stdout("C test 19 done\n")
            .stderr("");
    }

    assert_eq!(
        fs::read(first).expect("read first output"),
        fs::read(second).expect("read second output"),
        "qpdf-ctest test19 must preserve qpdf deterministic-ID repeatability"
    );
}

#[test]
fn qpdf_ctest_version_reports_the_pinned_qpdf_version() {
    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .arg("--version")
        .assert()
        .success()
        .stdout("qpdf-ctest version 11.9.0\n")
        .stderr("");
}

#[test]
fn qpdf_ctest_1_reports_plaintext_metadata_and_ignores_outfile() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = directory.path().join("unused-output.pdf");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "1",
            minimal_pdf().to_str().expect("input path is UTF-8"),
            "",
            output.to_str().expect("output path is UTF-8"),
        ])
        .assert()
        .success()
        .stdout("version: 1.7\nlinearized: 0\nencrypted: 0\nC test 1 done\n")
        .stderr("");

    assert!(
        !output.exists(),
        "test01 must not write its outfile argument"
    );
}

#[test]
fn qpdf_ctest_1_reports_linearized_metadata() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = directory.path().join("unused-output.pdf");
    let input = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/compat/linearized-one-page.pdf");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "1",
            input.to_str().expect("input path is UTF-8"),
            "",
            output.to_str().expect("output path is UTF-8"),
        ])
        .assert()
        .success()
        .stdout("version: 1.3\nlinearized: 1\nencrypted: 0\nC test 1 done\n")
        .stderr("");

    assert!(
        !output.exists(),
        "test01 must not write its outfile argument"
    );
}

#[test]
fn qpdf_ctest_1_reports_encryption_metadata_and_permissions() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = directory.path().join("unused-output.pdf");
    let input = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/encrypted/v2-rc4-128-r3.pdf");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "1",
            input.to_str().expect("input path is UTF-8"),
            "user-v2",
            output.to_str().expect("output path is UTF-8"),
        ])
        .assert()
        .success()
        .stdout(concat!(
            "version: 1.7\n",
            "linearized: 0\n",
            "encrypted: 1\n",
            "user password: user-v2\n",
            "extract for accessibility: 1\n",
            "extract for any purpose: 1\n",
            "print low resolution: 1\n",
            "print high resolution: 1\n",
            "modify document assembly: 1\n",
            "modify forms: 1\n",
            "modify annotations: 1\n",
            "modify other: 1\n",
            "modify anything: 1\n",
            "C test 1 done\n",
        ))
        .stderr("");

    assert!(
        !output.exists(),
        "test01 must not write its outfile argument"
    );
}

#[test]
fn qpdf_ctest_20_writes_with_specialized_decode_level() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = directory.path().join("output.pdf");
    let input = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/test_driver/stream_dct.pdf");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "20",
            input.to_str().expect("input path is UTF-8"),
            "",
            output.to_str().expect("output path is UTF-8"),
        ])
        .assert()
        .success()
        .stdout("C test 20 done\n")
        .stderr("");

    assert!(output.is_file(), "test20 must write its output PDF");
    let mut pdf = Pdf::open(Cursor::new(fs::read(&output).expect("read output PDF")))
        .expect("open test20 output PDF");
    let has_dct_stream = pdf.object_refs().into_iter().any(|object_ref| {
        let object = pdf.get_object_handle(object_ref);
        pdf.resolve(&object).is_ok()
            && object
                .as_stream_dict()
                .and_then(|dict| dict.get_key(b"/Filter").as_name())
                .is_some_and(|name| name == b"DCTDecode")
    });
    assert!(
        has_dct_stream,
        "specialized decode level must preserve the lossy DCT filter"
    );
}
