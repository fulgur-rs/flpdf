use assert_cmd::Command;
use std::path::Path;

#[test]
fn pages_source_acroform_analysis_warns_once_like_qpdf() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("merged.pdf");
    let primary = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/acroform-sig-orphan-widget.pdf");

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite"])
        .arg(&primary)
        .arg(&output)
        .args(["--pages"])
        .arg(&source)
        .args(["1", "--"])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(
        stderr
            .matches(
                "this widget annotation is not reachable from /AcroForm in the document catalog"
            )
            .count(),
        1,
        "qpdf caches one AcroFormDocumentHelper per source document: {stderr}"
    );
    assert!(
        output.is_file(),
        "warning-bearing qpdf-compatible output exists"
    );
}
