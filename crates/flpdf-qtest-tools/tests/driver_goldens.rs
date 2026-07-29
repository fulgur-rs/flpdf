use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::process::{Command as StdCommand, Stdio};

fn fixture_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/test_driver")
}

fn fixture_names() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(fixture_dir())
        .expect("read test_driver fixtures")
        .map(|entry| entry.expect("fixture entry").path())
        .filter(|path| path.extension() == Some(OsStr::new("pdf")))
        .map(|path| {
            path.file_stem()
                .expect("fixture stem")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn run_fixture(name: &str) -> (Option<i32>, Vec<u8>) {
    let merged = tempfile::tempfile().expect("merged capture file");
    let stdout = merged.try_clone().expect("clone merged capture");
    let stderr = merged.try_clone().expect("clone merged capture");
    let status = StdCommand::new(assert_cmd::cargo_bin!("flpdf-test-driver"))
        .args(["1", &format!("{name}.pdf")])
        .current_dir(fixture_dir())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .expect("run flpdf-test-driver");

    let mut merged: File = merged;
    merged.seek(SeekFrom::Start(0)).expect("rewind capture");
    let mut actual = Vec::new();
    merged.read_to_end(&mut actual).expect("read merged output");
    (status.code(), actual)
}

#[test]
fn test_0_1_fixtures_match_committed_qpdf_merged_output() {
    for name in fixture_names() {
        let expected =
            fs::read(fixture_dir().join(format!("{name}.out"))).expect("read qpdf oracle output");
        let (status, actual) = run_fixture(&name);
        let expected_status = if name == "open_repair_failure" { 2 } else { 0 };
        assert_eq!(status, Some(expected_status), "{name}: unexpected status");
        assert_eq!(actual, expected, "{name}: merged output differs");
    }
}

#[test]
fn open_repair_failure_matches_qpdf_output_and_exit_two() {
    let expected =
        fs::read(fixture_dir().join("open_repair_failure.out")).expect("read qpdf oracle output");
    let (status, actual) = run_fixture("open_repair_failure");

    assert_eq!(status, Some(2));
    assert_eq!(actual, expected);
}

#[test]
fn empty_reconstructed_xref_matches_qpdf_output_and_exit_zero() {
    let expected = fs::read(fixture_dir().join("empty_reconstructed_xref.out"))
        .expect("read qpdf oracle output");
    let (status, actual) = run_fixture("empty_reconstructed_xref");

    assert_eq!(status, Some(0));
    assert_eq!(actual, expected);
}
