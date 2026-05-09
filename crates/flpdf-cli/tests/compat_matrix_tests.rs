use assert_cmd::Command;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;
use tempfile::tempdir;

const COMPAT_FIXTURE_DIR: &str = "../../tests/fixtures/compat";

#[test]
fn qpdf_inspect_pages_matrix_matches_golden() {
    if !is_qpdf_available() {
        return;
    }

    let expected = load_name_value_golden("inspect-npages.txt");
    for (name, expected_pages) in expected {
        let fixture = fixture_path(&name);
        let actual_pages = run_qpdf(&["--show-npages", fixture.to_str().unwrap()]);
        assert_eq!(actual_pages, expected_pages);

        let tmp = tempdir().unwrap();
        let rewritten = tmp.path().join(format!("rewrite-{name}"));

        let mut cmd = Command::cargo_bin("flpdf").unwrap();
        cmd.args([fixture.to_str().unwrap(), rewritten.to_str().unwrap()])
            .assert()
            .success();

        let rewritten_pages = run_qpdf(&["--show-npages", rewritten.to_str().unwrap()]);
        assert_eq!(rewritten_pages, expected_pages);
    }
}

#[test]
fn qpdf_pages_range_roundtrip_matches_selected_output() {
    if !is_qpdf_available() {
        return;
    }

    let tmp = tempdir().unwrap();
    let extracted = tmp.path().join("selected-page-1.pdf");
    let fixture = fixture_path("two-page.pdf");

    run_qpdf_with_args(&[
        fixture.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        "--",
        extracted.to_str().unwrap(),
    ]);

    let actual_pages = run_qpdf(&["--show-pages", extracted.to_str().unwrap()]);
    assert!(actual_pages.starts_with("page 1:"));
    assert!(actual_pages.contains("content:"));
    assert_eq!(
        run_qpdf(&["--show-npages", extracted.to_str().unwrap()]),
        "1"
    );

    let rewritten = tmp.path().join("selected-page-1-rewrite.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([extracted.to_str().unwrap(), rewritten.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(
        run_qpdf(&["--show-npages", rewritten.to_str().unwrap()]),
        "1"
    );
}

#[test]
fn qpdf_split_and_merge_matrix_roundtrip_still_parsable() {
    if !is_qpdf_available() {
        return;
    }

    let tmp = tempdir().unwrap();
    let split_dir = tmp.path().join("split");
    fs::create_dir(&split_dir).unwrap();

    let split_fixture = fixture_path("three-page.pdf");
    let split_pattern = split_dir.join("split-%d.pdf");
    run_qpdf_with_args(&[
        "--split-pages=1",
        split_fixture.to_str().unwrap(),
        split_pattern.to_str().unwrap(),
    ]);

    let expected = load_name_value_golden("split-three-page.txt");
    let mut split_outputs: Vec<PathBuf> = expected
        .keys()
        .map(|file_name| split_dir.join(file_name))
        .collect();
    split_outputs.sort();

    for path in split_outputs {
        let expected_pages = expected
            .get(path.file_name().unwrap().to_str().unwrap())
            .unwrap();
        assert_eq!(
            run_qpdf(&["--show-npages", path.to_str().unwrap()]),
            *expected_pages
        );

        let rewritten = tmp.path().join(format!(
            "rewritten-{}",
            path.file_name().unwrap().to_str().unwrap()
        ));
        let mut cmd = Command::cargo_bin("flpdf").unwrap();
        cmd.args([path.to_str().unwrap(), rewritten.to_str().unwrap()])
            .assert()
            .success();
        assert_eq!(
            run_qpdf(&["--show-npages", rewritten.to_str().unwrap()]),
            *expected_pages
        );
    }

    let merged = tmp.path().join("merged.pdf");
    run_qpdf_with_args(&[
        "--empty",
        "--pages",
        fixture_path("one-page.pdf").to_str().unwrap(),
        fixture_path("two-page.pdf").to_str().unwrap(),
        "--",
        merged.to_str().unwrap(),
    ]);

    let expected_merged = load_lines("merge-one-plus-two-pages.txt")
        .first()
        .cloned()
        .expect("golden file merge-one-plus-two-pages.txt should not be empty");
    assert_eq!(
        run_qpdf(&["--show-npages", merged.to_str().unwrap()]),
        expected_merged
    );
    let rewrite = tmp.path().join("merged-rewrite.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([merged.to_str().unwrap(), rewrite.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(
        run_qpdf(&["--show-npages", rewrite.to_str().unwrap()]),
        expected_merged
    );
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT_FIXTURE_DIR)
        .join(name)
}

fn is_qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_qpdf(args: &[&str]) -> String {
    let output = ShellCommand::new("qpdf")
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to invoke qpdf: {err}"));
    assert!(
        output.status.success(),
        "qpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap_or_else(|err| panic!("qpdf output was not utf8: {err}"))
        .trim()
        .to_string()
}

fn run_qpdf_with_args(args: &[&str]) {
    let output = ShellCommand::new("qpdf")
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to invoke qpdf: {err}"));
    assert!(
        output.status.success(),
        "qpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn load_lines(file_name: &str) -> Vec<String> {
    fs::read_to_string(golden_path(file_name))
        .unwrap_or_else(|err| panic!("failed to read golden file {file_name}: {err}"))
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn load_name_value_golden(file_name: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in load_lines(file_name) {
        let mut split = line.splitn(2, ' ');
        let name = split.next().unwrap_or_default().to_string();
        let value = split.next().unwrap_or_default().to_string();
        values.insert(name, value);
    }
    values
}

fn golden_path(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/golden")
        .join(file)
}
