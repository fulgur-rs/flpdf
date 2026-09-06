use assert_cmd::Command;
use flpdf::{PageDocumentHelper, Pdf};
use std::fs::{self, File};

fn assert_usage(binary: &str, expected: &str) {
    Command::cargo_bin(binary)
        .expect("document-construction helper binary")
        .assert()
        .failure()
        .code(2)
        .stdout("")
        .stderr(predicates::str::diff(expected.to_owned()));
}

#[test]
fn from_scratch_rejects_wrong_arity_with_qpdf_usage() {
    assert_usage("pdf_from_scratch", "Usage: pdf_from_scratch n\n");
}

#[test]
fn from_scratch_rejects_unknown_test_number() {
    Command::cargo_bin("pdf_from_scratch")
        .expect("document-construction helper binary")
        .arg("1")
        .assert()
        .failure()
        .code(2)
        .stdout("")
        .stderr("invalid test 1\n");
}

#[test]
fn from_scratch_builds_one_page_document() {
    let directory = tempfile::tempdir().expect("temporary directory");
    Command::cargo_bin("pdf_from_scratch")
        .expect("document-construction helper binary")
        .current_dir(directory.path())
        .arg("0")
        .assert()
        .success()
        .stdout("test 0 done\n")
        .stderr("");

    let output = directory.path().join("a.pdf");
    assert!(output.is_file());
    let mut pdf =
        Pdf::open(File::open(&output).expect("open helper output")).expect("parse helper output");
    let pages = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("enumerate helper output pages");
    assert_eq!(pages.len(), 1);
}

#[test]
fn from_scratch_reports_output_write_failure() {
    let directory = tempfile::tempdir().expect("temporary directory");
    fs::create_dir(directory.path().join("a.pdf")).expect("create output directory");

    Command::cargo_bin("pdf_from_scratch")
        .expect("document-construction helper binary")
        .current_dir(directory.path())
        .arg("0")
        .assert()
        .failure()
        .code(2)
        .stdout("")
        .stderr(predicates::str::contains("open a.pdf"));
}

#[test]
fn many_nulls_rejects_wrong_arity_with_qpdf_usage() {
    assert_usage("test_many_nulls", "Usage: test_many_nulls outfile.pdf\n");
}

#[test]
#[ignore = "the full 400,000-null generator runs in the release qtest survey"]
fn many_nulls_builds_a_deterministic_sparse_graph() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let first = directory.path().join("first.pdf");
    let second = directory.path().join("second.pdf");

    for output in [&first, &second] {
        Command::cargo_bin("test_many_nulls")
            .expect("document-construction helper binary")
            .arg(output)
            .assert()
            .success()
            .stdout("")
            .stderr("");
    }

    assert_eq!(
        fs::read(&first).expect("read first output"),
        fs::read(&second).expect("read second output")
    );

    let mut pdf =
        Pdf::open(File::open(&first).expect("open helper output")).expect("parse helper output");
    let nulls = pdf.trailer().try_get_key(b"/Nulls").expect("read /Nulls");
    let outer = nulls
        .try_get_array_as_vector()
        .expect("read outer null array");
    assert_eq!(outer.len(), 20);
    for inner in outer {
        assert_eq!(
            inner
                .try_get_array_as_vector()
                .expect("read inner null array")
                .len(),
            20_000
        );
    }
    let pages = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("enumerate helper output pages");
    assert_eq!(pages.len(), 1);
}

#[test]
#[ignore = "the full 400,000-null generator runs in the release qtest survey"]
fn many_nulls_reports_output_write_failure() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = directory.path().join("a.pdf");
    fs::create_dir(&output).expect("create output directory");

    Command::cargo_bin("test_many_nulls")
        .expect("document-construction helper binary")
        .arg(&output)
        .assert()
        .failure()
        .code(2)
        .stdout("")
        .stderr(predicates::str::contains("open a.pdf"));
}
