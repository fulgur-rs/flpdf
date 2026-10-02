use std::process::Command;

#[test]
fn flatten_rotation_rounds_generated_real_values_like_qpdf() {
    let Ok(version) = Command::new("qpdf").arg("--version").output() else {
        return;
    };
    if !String::from_utf8_lossy(&version.stdout).contains("qpdf version 11.9.0") {
        return;
    }
    let input = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/compat/flatten-rotation-real-precision.pdf"
    );
    let temporary = tempfile::tempdir().unwrap();
    let expected = temporary.path().join("qpdf.pdf");
    let actual = temporary.path().join("flpdf.pdf");
    for (program, output) in [("qpdf", &expected), (env!("CARGO_BIN_EXE_flpdf"), &actual)] {
        let result = Command::new(program)
            .args([
                "--static-id",
                "--qdf",
                "--object-streams=disable",
                "--flatten-rotation",
                input,
            ])
            .arg(output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert_eq!(
        std::fs::read(actual).unwrap(),
        std::fs::read(expected).unwrap()
    );
}
