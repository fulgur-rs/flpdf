//! qpdf 11.9.0 acceptance for global input policies on JSON attachment donors.
//!
//! `QPDFJob::copyAttachments` opens each donor through the job configuration,
//! so the JSON route must carry the same password and recovery policy that the
//! primary input route receives.

use assert_cmd::Command;
use flpdf::{EncryptParams, Pdf, PdfWriter};
use std::ffi::OsString;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

#[path = "support/text_newlines.rs"]
mod text_newlines;
use text_newlines::normalize_text_newlines;

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
        panic!("{EXPECTED_QPDF_VERSION} is required for JSON donor policy tests on CI");
    }
    eprintln!("skipping JSON donor policy tests: {EXPECTED_QPDF_VERSION} is unavailable");
    true
}
fn run_qpdf_and_flpdf(args: &[OsString]) -> (Output, Output) {
    let qpdf = ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf should spawn");
    (qpdf, flpdf)
}

fn assert_pair(label: &str, args: &[OsString]) -> (Output, Output) {
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
    (qpdf, flpdf)
}

fn json_copy_args(
    global_options: &[OsString],
    donor: &Path,
    donor_options: &[OsString],
) -> Vec<OsString> {
    let input = fixture("three-page.pdf");
    let mut args = vec![
        OsString::from("--json=2"),
        OsString::from("--json-key=attachments"),
    ];
    args.extend_from_slice(global_options);
    args.push(OsString::from("--copy-attachments-from"));
    args.push(donor.as_os_str().to_owned());
    args.extend_from_slice(donor_options);
    args.push(OsString::from("--"));
    args.push(input.into());
    args.push(OsString::from("-"));
    args
}

fn corrupt_startxref(input: &Path, output: &Path) {
    let mut bytes = fs::read(input).unwrap();
    let marker = b"startxref\n";
    let marker_start = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
        .expect("attachment fixture must have a startxref marker");
    let value_start = marker_start + marker.len();
    let value_len = bytes[value_start..]
        .iter()
        .position(|&byte| byte == b'\n')
        .expect("startxref must have a newline-terminated value");
    bytes[value_start..value_start + value_len].fill(b'9');
    fs::write(output, bytes).unwrap();
}

fn make_xref_stream_donor(input: &Path, output: &Path) {
    let result = ShellCommand::new("qpdf")
        .args(["--object-streams=generate", "--static-id"])
        .arg(input)
        .arg(output)
        .output()
        .expect("qpdf should generate the xref-stream donor");
    assert!(
        result.status.success(),
        "qpdf donor generation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = fs::read(output).unwrap();
    assert!(
        bytes
            .windows(b"/Type /XRef".len())
            .any(|window| window == b"/Type /XRef"),
        "generated donor must contain an xref stream"
    );
}

fn encrypted_attachment_fixture_with_raw_password(password: &[u8]) -> Vec<u8> {
    let input = fs::read(fixture("attachment-two-page.pdf")).unwrap();
    let mut pdf = Pdf::open(Cursor::new(input)).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(
        password.to_vec(),
        b"owner".to_vec(),
    ));
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    writer.get_buffer().unwrap()
}

fn qpdf_encryption_key(input: &Path) -> String {
    let output = ShellCommand::new("qpdf")
        .args([
            "--show-encryption",
            "--show-encryption-key",
            "--password=123",
        ])
        .arg(input)
        .output()
        .expect("qpdf should show the donor encryption key");
    assert!(
        output.status.success(),
        "qpdf key inspection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("Encryption key = "))
        .map(str::to_owned)
        .expect("qpdf encryption report must contain the file key")
}

#[test]
fn json_copy_suppress_recovery_matches_qpdf() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let donor = temp.path().join("damaged-donor.pdf");
    corrupt_startxref(&fixture("attachment-two-page.pdf"), &donor);
    let args = json_copy_args(&[OsString::from("--suppress-recovery")], &donor, &[]);

    let (qpdf, _) = assert_pair("JSON donor --suppress-recovery", &args);
    assert_eq!(qpdf.status.code(), Some(2));
}

#[test]
fn json_copy_ignore_xref_streams_matches_qpdf() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let donor = temp.path().join("xref-stream-donor.pdf");
    make_xref_stream_donor(&fixture("attachment-two-page.pdf"), &donor);
    let args = json_copy_args(&[OsString::from("--ignore-xref-streams")], &donor, &[]);

    let (qpdf, _) = assert_pair("JSON donor --ignore-xref-streams", &args);
    assert_eq!(qpdf.status.code(), Some(2));
}

#[test]
fn json_copy_hex_bytes_password_mode_matches_qpdf() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let donor = temp.path().join("hex-bytes-donor.pdf");
    let result = ShellCommand::new("qpdf")
        .args([
            "--static-id",
            "--encrypt",
            "123",
            "123",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture("attachment-two-page.pdf"))
        .arg(&donor)
        .output()
        .expect("qpdf should generate the encrypted donor");
    assert!(
        result.status.success(),
        "qpdf donor generation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let args = json_copy_args(
        &[OsString::from("--password-mode=hex-bytes")],
        &donor,
        &[OsString::from("--password=313233")],
    );

    let (qpdf, _) = assert_pair("JSON donor --password-mode=hex-bytes", &args);
    assert!(qpdf.status.success());
}

#[test]
fn json_copy_suppress_password_recovery_matches_qpdf() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let donor = temp.path().join("password-recovery-donor.pdf");
    fs::write(
        &donor,
        encrypted_attachment_fixture_with_raw_password(b"caf\xe9"),
    )
    .unwrap();
    let args = json_copy_args(
        &[OsString::from("--suppress-password-recovery")],
        &donor,
        &[OsString::from("--password=café")],
    );

    let (qpdf, _) = assert_pair("JSON donor --suppress-password-recovery", &args);
    assert_eq!(qpdf.status.code(), Some(2));
}

#[test]
fn json_copy_password_is_hex_key_matches_qpdf() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let donor = temp.path().join("hex-key-donor.pdf");
    let result = ShellCommand::new("qpdf")
        .args([
            "--static-id",
            "--encrypt",
            "123",
            "123",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture("attachment-two-page.pdf"))
        .arg(&donor)
        .output()
        .expect("qpdf should generate the encrypted donor");
    assert!(
        result.status.success(),
        "qpdf donor generation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let key = qpdf_encryption_key(&donor);
    let args = json_copy_args(
        &[OsString::from("--password-is-hex-key")],
        &donor,
        &[OsString::from(format!("--password={key}"))],
    );

    let (qpdf, _) = assert_pair("JSON donor --password-is-hex-key", &args);
    assert!(qpdf.status.success());
}

#[test]
fn copy_donor_wrong_password_matches_qpdf_for_normal_and_json_routes() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let donor = temp.path().join("wrong-password-donor.pdf");
    fs::write(
        &donor,
        encrypted_attachment_fixture_with_raw_password(b"correct-password"),
    )
    .unwrap();
    let input = fixture("three-page.pdf");

    let normal_output = temp.path().join("normal-output.pdf");
    let normal_args = vec![
        OsString::from("--copy-attachments-from"),
        donor.as_os_str().to_owned(),
        OsString::from("--password=wrong-password"),
        OsString::from("--"),
        input.as_os_str().to_owned(),
        normal_output.as_os_str().to_owned(),
    ];
    let (qpdf, _) = assert_pair("wrong donor password + normal output", &normal_args);
    assert_eq!(qpdf.status.code(), Some(2));

    let json_args = json_copy_args(&[], &donor, &[OsString::from("--password=wrong-password")]);
    let (qpdf, _) = assert_pair("wrong donor password + JSON output", &json_args);
    assert_eq!(qpdf.status.code(), Some(2));
}
