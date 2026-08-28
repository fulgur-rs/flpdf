use assert_cmd::Command;
use std::fs;

fn minimal_pdf() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/minimal.pdf")
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
