//! qpdf 11.9.0 acceptance for writer/attachment options combined with
//! inspection and JSON output.

use assert_cmd::Command;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false)
}

fn skip_without_qpdf() -> bool {
    if qpdf_available() {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("{EXPECTED_QPDF_VERSION} is required for this qpdf conflict matrix on CI");
    }
    eprintln!("skipping qpdf conflict matrix: {EXPECTED_QPDF_VERSION} is unavailable");
    true
}

fn normalize_text_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;
    while let Some((&byte, rest)) = remaining.split_first() {
        if byte == b'\r' && rest.first() == Some(&b'\n') {
            normalized.push(b'\n');
            remaining = &rest[1..];
        } else {
            normalized.push(byte);
            remaining = rest;
        }
    }
    normalized
}

fn run_qpdf_and_flpdf(args: &[OsString]) -> (Output, Output) {
    let qpdf = ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .args(args)
        .output()
        .expect("flpdf should spawn");
    (qpdf, flpdf)
}

fn assert_pair(label: &str, args: &[OsString]) {
    let (qpdf, flpdf) = run_qpdf_and_flpdf(args);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "{label}: exit status differs; qpdf stderr={} flpdf stderr={}",
        String::from_utf8_lossy(&qpdf.stderr),
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "{label}: stdout differs"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "{label}: stderr differs"
    );
    assert!(
        qpdf.status.success(),
        "{label}: qpdf must accept the combination; stderr={}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
}

fn json_args(extra: &[OsString], input: &Path) -> Vec<OsString> {
    let mut args = vec![OsString::from("--json=2")];
    args.extend_from_slice(extra);
    args.push(input.as_os_str().to_owned());
    args.push(OsString::from("-"));
    args
}

#[test]
fn qpdf_writer_and_attachment_conflicts_match_qpdf() {
    if skip_without_qpdf() {
        return;
    }

    let three_page = fixture("three-page.pdf");
    let attachment_input = fixture("attachment-two-page.pdf");
    let add_file = fixture("golden/inspect-npages.txt");

    let remove = json_args(
        &[OsString::from("--remove-attachment=attachment.txt")],
        &attachment_input,
    );
    assert_pair("remove-attachment + json", &remove);

    let add = json_args(
        &[
            OsString::from("--json-key=attachments"),
            OsString::from("--add-attachment"),
            add_file.as_os_str().to_owned(),
            OsString::from("--key=k9"),
            OsString::from("--creationdate=D:20200101000000Z"),
            OsString::from("--moddate=D:20200101000000Z"),
            OsString::from("--"),
        ],
        &three_page,
    );
    assert_pair("add-attachment + json", &add);

    let copy = json_args(
        &[
            OsString::from("--json-key=attachments"),
            OsString::from("--copy-attachments-from"),
            attachment_input.as_os_str().to_owned(),
            OsString::from("--"),
        ],
        &three_page,
    );
    assert_pair("copy-attachments-from + json", &copy);

    let encrypt = [
        OsString::from("--encrypt"),
        OsString::from(""),
        OsString::from(""),
        OsString::from("128"),
        OsString::from("--use-aes=y"),
        OsString::from("--"),
    ];
    assert_pair("encrypt + json", &json_args(&encrypt, &three_page));
    let mut encrypt_check = vec![OsString::from("--check")];
    encrypt_check.extend(encrypt.clone());
    encrypt_check.push(three_page.as_os_str().to_owned());
    assert_pair("encrypt + check", &encrypt_check);
    let mut encrypt_npages = vec![OsString::from("--show-npages")];
    encrypt_npages.extend(encrypt);
    encrypt_npages.push(three_page.as_os_str().to_owned());
    assert_pair("encrypt + show-npages", &encrypt_npages);

    assert_pair(
        "linearize + json",
        &json_args(&[OsString::from("--linearize")], &three_page),
    );

    assert_pair(
        "decrypt + json",
        &json_args(&[OsString::from("--decrypt")], &three_page),
    );
    assert_pair(
        "decrypt + check",
        &[
            OsString::from("--check"),
            OsString::from("--decrypt"),
            three_page.as_os_str().to_owned(),
        ],
    );
    assert_pair(
        "decrypt + show-npages",
        &[
            OsString::from("--show-npages"),
            OsString::from("--decrypt"),
            three_page.as_os_str().to_owned(),
        ],
    );
}
