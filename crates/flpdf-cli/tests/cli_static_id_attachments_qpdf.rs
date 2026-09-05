//! Attachment mutation routes must honor qpdf's `--static-id` writer option.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

const QPDF: &str = "qpdf";
const STATIC_ID: &str = "31415926535897932384626433832795";

fn qpdf_available() -> bool {
    ShellCommand::new(QPDF)
        .arg("--version")
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .is_some_and(|line| line.trim() == "qpdf version 11.9.0")
        })
        .unwrap_or(false)
}

fn run_qpdf(args: &[&Path], output: &Path) {
    let status = ShellCommand::new(QPDF)
        .args(["--static-id"])
        .args(args.iter().map(|path| path.as_os_str()))
        .arg(output)
        .status()
        .expect("qpdf should spawn");
    assert!(status.success(), "qpdf command failed for {output:?}");
}

fn run_flpdf(args: &[&str], output: &Path) {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(args)
        .arg(output)
        .assert()
        .success();
}

fn id_words(path: &Path) -> (String, String) {
    let bytes = std::fs::read(path).expect("read PDF output");
    let marker = b"/ID [<";
    let start = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("output trailer must contain /ID")
        + marker.len();
    let first_end = bytes[start..]
        .iter()
        .position(|&byte| byte == b'>')
        .expect("first ID word must be closed")
        + start;
    let second_start = first_end + 2;
    let second_end = bytes[second_start..]
        .iter()
        .position(|&byte| byte == b'>')
        .expect("second ID word must be closed")
        + second_start;
    (
        String::from_utf8_lossy(&bytes[start..first_end]).into_owned(),
        String::from_utf8_lossy(&bytes[second_start..second_end]).into_owned(),
    )
}

fn assert_static_id_pair(qpdf_output: &Path, flpdf_output: &Path) {
    let qpdf_ids = id_words(qpdf_output);
    let flpdf_ids = id_words(flpdf_output);
    assert_eq!(qpdf_ids.0, STATIC_ID);
    assert_eq!(qpdf_ids.1, STATIC_ID);
    assert_eq!(flpdf_ids, qpdf_ids, "flpdf /ID must match qpdf");
}

#[test]
fn attachment_mutation_routes_honor_static_id_like_qpdf() {
    if !qpdf_available() {
        eprintln!("[skip] qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let attachment = temp.path().join("payload.txt");
    std::fs::write(&attachment, b"static-id attachment payload").expect("write attachment");

    let qpdf_add = temp.path().join("qpdf-add.pdf");
    let flpdf_add = temp.path().join("flpdf-add.pdf");
    run_qpdf(
        &[
            Path::new("--add-attachment"),
            &attachment,
            Path::new("--key=static-key"),
            Path::new("--"),
            &input,
        ],
        &qpdf_add,
    );
    run_flpdf(
        &[
            input.to_str().expect("input path"),
            "--static-id",
            "--add-attachment",
            attachment.to_str().expect("attachment path"),
            "--key=static-key",
            "--",
        ],
        &flpdf_add,
    );
    assert_static_id_pair(&qpdf_add, &flpdf_add);

    let qpdf_copy = temp.path().join("qpdf-copy.pdf");
    let flpdf_copy = temp.path().join("flpdf-copy.pdf");
    run_qpdf(
        &[
            Path::new("--copy-attachments-from"),
            &qpdf_add,
            Path::new("--"),
            &input,
        ],
        &qpdf_copy,
    );
    run_flpdf(
        &[
            input.to_str().expect("input path"),
            "--static-id",
            "--copy-attachments-from",
            qpdf_add.to_str().expect("source path"),
            "--",
        ],
        &flpdf_copy,
    );
    assert_static_id_pair(&qpdf_copy, &flpdf_copy);

    let qpdf_remove = temp.path().join("qpdf-remove.pdf");
    let flpdf_remove = temp.path().join("flpdf-remove.pdf");
    run_qpdf(
        &[Path::new("--remove-attachment=static-key"), &qpdf_add],
        &qpdf_remove,
    );
    run_flpdf(
        &[
            qpdf_add.to_str().expect("source path"),
            "--static-id",
            "--remove-attachment=static-key",
        ],
        &flpdf_remove,
    );
    assert_static_id_pair(&qpdf_remove, &flpdf_remove);
}

#[test]
fn copy_attachments_applies_stream_data_to_every_stream_like_qpdf() {
    if !qpdf_available() {
        eprintln!("[skip] qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat");
    let input = fixtures.join("one-page.pdf");
    let donor = fixtures.join("attachment-two-page.pdf");
    let qpdf_output = temp.path().join("qpdf-copy-uncompress.pdf");
    let flpdf_output = temp.path().join("flpdf-copy-uncompress.pdf");

    let qpdf = ShellCommand::new(QPDF)
        .args([
            "--static-id",
            "--stream-data=uncompress",
            "--newline-before-endstream=y",
            "--copy-attachments-from",
        ])
        .arg(&donor)
        .arg("--")
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf copy should spawn");
    assert!(
        qpdf.status.success(),
        "qpdf copy failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--static-id",
            "--stream-data=uncompress",
            "--newline-before-endstream=y",
            "--copy-attachments-from",
        ])
        .arg(&donor)
        .arg("--")
        .arg(&input)
        .arg(&flpdf_output)
        .assert()
        .success();

    let qpdf_bytes = std::fs::read(&qpdf_output).expect("read qpdf output");
    let flpdf_bytes = std::fs::read(&flpdf_output).expect("read flpdf output");
    assert_eq!(
        flpdf_bytes, qpdf_bytes,
        "copy-attachments-from with --stream-data=uncompress must match qpdf byte-for-byte"
    );
}
