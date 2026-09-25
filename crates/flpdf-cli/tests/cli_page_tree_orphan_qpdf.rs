//! qpdf 11.9.0 page enumeration follows the Catalog `/Pages` graph and does
//! not promote `/Page` objects that are merely present elsewhere in the body.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

#[path = "support/eol.rs"]
mod eol;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";
const ORPHAN_MARKER: &[u8] = b"ORPHAN-PAGE";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn require_qpdf() {
    let output = ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .expect("qpdf 11.9.0 must be installed for page-tree parity tests");
    assert!(output.status.success(), "qpdf --version failed");
    let version = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    assert_eq!(version, EXPECTED_QPDF_VERSION);
}

fn run_qpdf(args: &[String]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[String]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assert_command_pair(label: &str, qpdf: &Output, flpdf: &Output) {
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "{label}: exit status"
    );
    assert_eq!(flpdf.stdout, qpdf.stdout, "{label}: stdout");
    assert_eq!(flpdf.stderr, qpdf.stderr, "{label}: stderr");
    assert!(qpdf.status.success(), "qpdf {label}: {:?}", qpdf.status);
    assert!(flpdf.status.success(), "flpdf {label}: {:?}", flpdf.status);
}

fn qpdf_check(path: &Path) {
    let output = ShellCommand::new("qpdf")
        .arg("--check")
        .arg(path)
        .output()
        .expect("qpdf should spawn");
    assert!(
        output.status.success(),
        "qpdf --check failed for {path:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn show_npages_args(input: &Path) -> Vec<String> {
    vec![
        "--show-npages".to_owned(),
        input.to_str().expect("UTF-8 test path").to_owned(),
    ]
}

fn json_pages_args(input: &Path) -> Vec<String> {
    vec![
        "--json=2".to_owned(),
        "--json-key=pages".to_owned(),
        input.to_str().expect("UTF-8 test path").to_owned(),
    ]
}

fn rewrite_args(input: &Path, output: &Path, preserve_unreferenced: bool) -> Vec<String> {
    let mut args = vec![
        "--static-id".to_owned(),
        "--object-streams=disable".to_owned(),
        "--compress-streams=n".to_owned(),
    ];
    if preserve_unreferenced {
        args.push("--preserve-unreferenced".to_owned());
    }
    args.push(input.to_str().expect("UTF-8 input path").to_owned());
    args.push(output.to_str().expect("UTF-8 output path").to_owned());
    args
}

fn selected_page_args(input: &Path, output: &Path) -> Vec<String> {
    vec![
        "--static-id".to_owned(),
        "--object-streams=disable".to_owned(),
        "--compress-streams=n".to_owned(),
        input.to_str().expect("UTF-8 input path").to_owned(),
        "--pages".to_owned(),
        ".".to_owned(),
        "1".to_owned(),
        "--".to_owned(),
        output.to_str().expect("UTF-8 output path").to_owned(),
    ]
}

fn assert_page_list_has_one_reachable_page(path: &Path, expected_object: Option<&str>) {
    let args = json_pages_args(path);
    let qpdf = run_qpdf(&args);
    let flpdf = run_flpdf(&args);
    assert_command_pair("JSON page enumeration", &qpdf, &flpdf);

    let json: Value = serde_json::from_slice(&qpdf.stdout).expect("parse qpdf pages JSON");
    let pages = json["pages"].as_array().expect("JSON pages array");
    assert_eq!(pages.len(), 1);
    if let Some(expected_object) = expected_object {
        assert_eq!(pages[0]["object"], expected_object);
    }
}

fn assert_output_pair(label: &str, qpdf_path: &Path, flpdf_path: &Path) {
    qpdf_check(qpdf_path);
    qpdf_check(flpdf_path);
    assert_eq!(
        std::fs::read(flpdf_path).expect("read flpdf output"),
        std::fs::read(qpdf_path).expect("read qpdf output"),
        "{label}: full output bytes must match"
    );
}

#[test]
fn orphan_body_page_is_not_promoted_into_the_catalog_page_tree() {
    require_qpdf();
    let input = fixture("page-tree-orphan.pdf");
    assert!(input.is_file(), "missing page-tree fixture: {input:?}");
    qpdf_check(&input);

    let qpdf_npages = run_qpdf(&show_npages_args(&input));
    let flpdf_npages = run_flpdf(&show_npages_args(&input));
    assert_command_pair("show-npages", &qpdf_npages, &flpdf_npages);
    let expected_npages = format!("1{}", eol::EOL);
    assert_eq!(qpdf_npages.stdout, expected_npages.as_bytes());
    assert_page_list_has_one_reachable_page(&input, Some("3 0 R"));

    let directory = tempfile::tempdir().expect("temporary output directory");
    for (label, preserve_unreferenced, expect_orphan_object) in [
        ("ordinary rewrite", false, false),
        ("preserve-unreferenced rewrite", true, true),
    ] {
        let qpdf_output = directory.path().join(format!("{label}-qpdf.pdf"));
        let flpdf_output = directory.path().join(format!("{label}-flpdf.pdf"));
        let qpdf_args = rewrite_args(&input, &qpdf_output, preserve_unreferenced);
        let flpdf_args = rewrite_args(&input, &flpdf_output, preserve_unreferenced);
        let qpdf = run_qpdf(&qpdf_args);
        let flpdf = run_flpdf(&flpdf_args);
        assert_command_pair(label, &qpdf, &flpdf);
        assert_output_pair(label, &qpdf_output, &flpdf_output);
        assert_page_list_has_one_reachable_page(&qpdf_output, None);
        assert_eq!(
            std::fs::read(&qpdf_output)
                .expect("read qpdf output")
                .windows(ORPHAN_MARKER.len())
                .any(|window| window == ORPHAN_MARKER),
            expect_orphan_object,
            "{label}: preserve an orphan object only when requested, without adding a page"
        );
    }

    let qpdf_output = directory.path().join("selected-qpdf.pdf");
    let flpdf_output = directory.path().join("selected-flpdf.pdf");
    let qpdf = run_qpdf(&selected_page_args(&input, &qpdf_output));
    let flpdf = run_flpdf(&selected_page_args(&input, &flpdf_output));
    assert_command_pair("single reachable page selection", &qpdf, &flpdf);
    assert_output_pair(
        "single reachable page selection",
        &qpdf_output,
        &flpdf_output,
    );
    assert_page_list_has_one_reachable_page(&qpdf_output, None);
    assert!(!std::fs::read(qpdf_output)
        .expect("read page-selection output")
        .windows(ORPHAN_MARKER.len())
        .any(|window| window == ORPHAN_MARKER));
}
