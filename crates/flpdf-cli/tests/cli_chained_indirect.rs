use assert_cmd::Command;
use std::path::PathBuf;

fn fixture() -> PathBuf {
    PathBuf::from("../../tests/fixtures/compat/chained-indirect-contents.pdf")
}

fn expected_warning(input: &std::path::Path) -> String {
    format!(
        "WARNING: {}: (object 5 0, offset 232): expected endobj",
        input.display()
    )
}

#[test]
fn qdf_matches_qpdf_11_9_and_finishes_output_before_exit_3() {
    let input = fixture();
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--qdf",
            "--static-id",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(
        stderr.matches(&expected_warning(&input)).count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("operation succeeded with warnings").count(),
        1
    );

    let actual = std::fs::read(&output).expect("warning-bearing QDF output is complete");
    let expected = include_bytes!(
        "../../../tests/golden/references/chained-indirect-contents/qdf-static-id.pdf"
    );
    assert_eq!(actual, expected);
    assert!(actual
        .windows(b"4 0 obj\n6\nendobj".len())
        .any(|w| w == b"4 0 obj\n6\nendobj"));
    assert!(!actual
        .windows(b"Original object ID: 6 0".len())
        .any(|w| w == b"Original object ID: 6 0"));
    assert!(!actual
        .windows(b"Original object ID: 7 0".len())
        .any(|w| w == b"Original object ID: 7 0"));
    assert!(!actual
        .windows(b"%% Contents for page 1".len())
        .any(|w| w == b"%% Contents for page 1"));
}

#[test]
fn normal_rewrite_recovers_bare_reference_and_exits_3_after_writing() {
    let input = fixture();
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", input.to_str().unwrap(), output.to_str().unwrap()])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(
        stderr.matches(&expected_warning(&input)).count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("operation succeeded with warnings").count(),
        1
    );

    let bytes = std::fs::read(&output).expect("warning-bearing rewrite output is complete");
    let mut pdf = flpdf::Pdf::open_mem_owned(bytes).expect("rewrite output opens");
    let page = pdf.resolve(flpdf::ObjectRef::new(3, 0)).unwrap();
    assert_eq!(
        page.as_dict().unwrap().get_ref("Contents"),
        Some(flpdf::ObjectRef::new(4, 0))
    );
    assert_eq!(
        pdf.resolve(flpdf::ObjectRef::new(4, 0)).unwrap(),
        flpdf::Object::Integer(6)
    );
    assert_eq!(
        pdf.resolve(flpdf::ObjectRef::new(5, 0)).unwrap(),
        flpdf::Object::Null
    );
}

#[test]
fn qdf_subcommand_exits_3_after_writing_complete_output() {
    let input = fixture();
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let result = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["qdf", input.to_str().unwrap(), output.to_str().unwrap()])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(
        stderr.matches(&expected_warning(&input)).count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("operation succeeded with warnings").count(),
        1
    );

    let bytes = std::fs::read(&output).expect("warning-bearing QDF output is complete");
    let mut pdf = flpdf::Pdf::open_mem_owned(bytes).expect("QDF output opens");
    assert_eq!(
        pdf.resolve(flpdf::ObjectRef::new(4, 0)).unwrap(),
        flpdf::Object::Integer(6)
    );
}
