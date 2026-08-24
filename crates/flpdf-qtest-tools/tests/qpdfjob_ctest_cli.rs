use assert_cmd::Command;
use std::fs;

fn minimal_pdf() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/minimal.pdf")
}

#[test]
fn qpdfjob_ctest_wide_dispatch_uses_the_production_job_route() {
    let directory = tempfile::tempdir().expect("temporary directory");
    fs::copy(minimal_pdf(), directory.path().join("minimal.pdf")).expect("copy minimal PDF");

    Command::cargo_bin("qpdfjob-ctest")
        .expect("qpdfjob-ctest binary")
        .current_dir(directory.path())
        .arg("wide")
        .assert()
        .code(0)
        .stdout("wide test passed\n")
        .stderr("");

    assert!(directory.path().join("a.pdf").is_file());
}
