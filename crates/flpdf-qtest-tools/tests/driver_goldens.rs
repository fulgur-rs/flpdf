use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::process::{Command as StdCommand, Stdio};

fn fixture_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/test_driver")
}

fn fixture_names() -> Vec<String> {
    include_str!("../../../tests/fixtures/test_driver/fixture-names.txt")
        .lines()
        .map(str::to_string)
        .collect()
}

fn fixture_stems(extension: &str) -> BTreeSet<String> {
    fs::read_dir(fixture_dir())
        .expect("read test_driver fixtures")
        .map(|entry| entry.expect("fixture entry").path())
        .filter(|path| path.extension() == Some(OsStr::new(extension)))
        .map(|path| {
            path.file_stem()
                .expect("fixture stem")
                .to_string_lossy()
                .into_owned()
        })
        .collect()
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
fn fixture_inventories_match_manifest_exactly() {
    let names = fixture_names();
    assert_eq!(names.len(), 53, "unexpected manifest fixture count");
    assert!(names.iter().all(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    }));
    let manifest: BTreeSet<String> = names.iter().cloned().collect();
    assert_eq!(manifest.len(), names.len(), "duplicate manifest fixture");
    assert_eq!(fixture_stems("pdf"), manifest, "PDF inventory differs");
    assert_eq!(fixture_stems("out"), manifest, "oracle inventory differs");
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
