//! qpdf parity tests for the V=5 reader/writer password boundary.

use flpdf::{EncryptParams, PasswordMode, Pdf, PdfOpenOptions, PdfWriter};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The qpdf release whose behavior these parity assertions cover.
/// Update this single value when the oracle release moves.
const EXPECTED_QPDF_VERSION: &str = "11.9.0";

fn minimal_fixture() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

#[test]
fn qpdf_version_gate_matches_the_requested_version() {
    assert!(qpdf_version_matches(b"qpdf version 11.9.0\n", "11.9.0"));
    assert!(qpdf_version_matches(
        b"qpdf version 12.0.0\nextra details\n",
        "12.0.0"
    ));
    assert!(!qpdf_version_matches(b"qpdf version 12.0.0\n", "11.9.0"));
    assert!(!qpdf_version_matches(
        b"qpdf version 11.9.0-dev\n",
        "11.9.0"
    ));
    assert!(!qpdf_version_matches(b"", "11.9.0"));
}

fn qpdf_version_matches(stdout: &[u8], expected_version: &str) -> bool {
    let expected_line = format!("qpdf version {expected_version}");
    let output = String::from_utf8_lossy(stdout);
    output.lines().next().map(str::trim) == Some(expected_line.as_str())
}

fn qpdf_available() -> bool {
    Command::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success() && qpdf_version_matches(&output.stdout, EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false)
}

fn flpdf_encrypted(input: &[u8], user_password: &[u8], r5: bool) -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(input.to_vec())).expect("open plaintext fixture");
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_encryption_parameters(if r5 {
        EncryptParams::v5_r5(user_password.to_vec(), b"owner".to_vec())
    } else {
        EncryptParams::v5_r6(user_password.to_vec(), b"owner".to_vec())
    });
    writer.set_output_memory().expect("configure memory output");
    writer.write().expect("write V=5 fixture");
    writer.get_buffer().expect("take V=5 fixture output")
}

fn qpdf_check(path: &Path, password: &[u8]) -> Output {
    let password = String::from_utf8(password.to_vec()).expect("test password is ASCII");
    Command::new("qpdf")
        .arg(format!("--password={password}"))
        .arg("--check")
        .arg(path)
        .output()
        .expect("run qpdf --check")
}

fn write_qpdf_encrypted(input: &Path, output: &Path, user_password: &[u8], r5: bool) {
    let user_password = String::from_utf8(user_password.to_vec()).expect("test password is ASCII");
    write_qpdf_encrypted_with_passwords(input, output, &user_password, "owner", r5);
}

fn write_qpdf_encrypted_with_passwords(
    input: &Path,
    output: &Path,
    user_password: &str,
    owner_password: &str,
    r5: bool,
) {
    let mut command = Command::new("qpdf");
    command
        .arg("--static-id")
        .arg("--encrypt")
        .arg(user_password)
        .arg(owner_password)
        .arg("256");
    if r5 {
        command.arg("--force-R5");
    }
    let output_result = command
        .arg("--")
        .arg(input)
        .arg(output)
        .output()
        .expect("run qpdf encrypted writer");
    assert!(
        output_result.status.success(),
        "qpdf encrypted write failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output_result.stdout),
        String::from_utf8_lossy(&output_result.stderr)
    );
}

fn zero_hex_dictionary_string(bytes: &mut [u8], key: &str) {
    let marker = format!("/{key} <");
    let marker = marker.as_bytes();
    let start = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap_or_else(|| panic!("PDF is missing {key} hex string"));
    let hex_start = start + marker.len();
    let hex_end = hex_start
        + bytes[hex_start..]
            .iter()
            .position(|byte| *byte == b'>')
            .unwrap_or_else(|| panic!("PDF has unterminated {key} hex string"));
    let mut digits = 0;
    for byte in &mut bytes[hex_start..hex_end] {
        if byte.is_ascii_hexdigit() {
            *byte = b'0';
            digits += 1;
        } else {
            assert!(byte.is_ascii_whitespace(), "unexpected byte in /{key}");
        }
    }
    assert_eq!(digits, 64, "/{key} should contain 32 bytes");
}

fn qpdf_file_key_and_encryption_report(path: &Path, password: &[u8]) -> (Vec<u8>, String) {
    let password = String::from_utf8(password.to_vec()).expect("test password is ASCII");
    let output = Command::new("qpdf")
        .arg(format!("--password={password}"))
        .arg("--show-encryption")
        .arg("--show-encryption-key")
        .arg(path)
        .output()
        .expect("run qpdf encryption inspection");
    assert!(
        output.status.success(),
        "qpdf encryption inspection failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8_lossy(&output.stdout).into_owned();
    let key_hex = report
        .lines()
        .find_map(|line| line.strip_prefix("Encryption key = "))
        .unwrap_or_else(|| panic!("qpdf encryption report should include the file key:\n{report}"));
    assert_eq!(key_hex.len(), 64);
    let file_key = key_hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII key hex"), 16)
                .expect("valid key hex")
        })
        .collect();
    (file_key, report)
}

fn assert_qpdf_accepts(path: &Path, password: &[u8]) {
    let output = qpdf_check(path, password);
    assert!(
        output.status.success(),
        "qpdf should accept the password (exit {:?}):\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_qpdf_rejects(path: &Path, password: &[u8]) {
    let output = qpdf_check(path, password);
    assert_eq!(
        output.status.code(),
        Some(2),
        "qpdf should report invalid password:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid password"),
        "qpdf rejection should identify the invalid password:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_flpdf_accepts(bytes: &[u8], password: &[u8]) {
    Pdf::open_with_options(
        Cursor::new(bytes.to_vec()),
        PdfOpenOptions {
            password: password.to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap_or_else(|err| panic!("flpdf should accept the password: {err}"));
}

fn write_bytes(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
    path
}

#[test]
fn v5_password_truncation_matches_qpdf_reader_writer_split() {
    if !qpdf_available() {
        eprintln!("qpdf {EXPECTED_QPDF_VERSION} not available; skipping V=5 password parity test");
        return;
    }

    let input = minimal_fixture();
    let directory = tempfile::tempdir().expect("create parity directory");
    let input_path = write_bytes(directory.path(), "input.pdf", &input);
    let prefix = vec![b'p'; 127];
    let mut longer = prefix.clone();
    longer.push(b'x');
    let full = vec![b'p'; 128];

    for r5 in [true, false] {
        let flpdf_127_path = write_bytes(
            directory.path(),
            if r5 {
                "flpdf-r5-127.pdf"
            } else {
                "flpdf-r6-127.pdf"
            },
            &flpdf_encrypted(&input, &prefix, r5),
        );
        assert_qpdf_accepts(&flpdf_127_path, &longer);
        assert_flpdf_accepts(&fs::read(&flpdf_127_path).unwrap(), &longer);

        let flpdf_128_path = write_bytes(
            directory.path(),
            if r5 {
                "flpdf-r5-128.pdf"
            } else {
                "flpdf-r6-128.pdf"
            },
            &flpdf_encrypted(&input, &full, r5),
        );
        assert_qpdf_rejects(&flpdf_128_path, &full);
        assert_qpdf_rejects(&flpdf_128_path, &prefix);

        let qpdf_127_path = directory.path().join(if r5 {
            "qpdf-r5-127.pdf"
        } else {
            "qpdf-r6-127.pdf"
        });
        write_qpdf_encrypted(&input_path, &qpdf_127_path, &prefix, r5);
        assert_flpdf_accepts(&fs::read(&qpdf_127_path).unwrap(), &longer);
    }
}

#[test]
fn v5_owner_key_precedes_user_key_when_both_passwords_match() {
    if !qpdf_available() {
        eprintln!("qpdf {EXPECTED_QPDF_VERSION} not available; skipping V=5 key-priority test");
        return;
    }

    let input = minimal_fixture();
    let directory = tempfile::tempdir().expect("create key-priority directory");
    let input_path = write_bytes(directory.path(), "input.pdf", &input);
    let password = b"shared-password";

    for r5 in [true, false] {
        let suffix = if r5 { "r5" } else { "r6" };
        let valid_path = directory.path().join(format!("{suffix}-valid.pdf"));
        write_qpdf_encrypted_with_passwords(
            &input_path,
            &valid_path,
            "shared-password",
            "shared-password",
            r5,
        );

        let mut corrupted = fs::read(&valid_path).expect("read qpdf encrypted fixture");
        zero_hex_dictionary_string(&mut corrupted, "UE");
        let corrupted_path = write_bytes(
            directory.path(),
            &format!("{suffix}-bad-ue.pdf"),
            &corrupted,
        );
        let (expected_key, qpdf_report) =
            qpdf_file_key_and_encryption_report(&corrupted_path, password);
        assert!(qpdf_report.contains("Supplied password is owner password"));
        assert!(qpdf_report.contains("Supplied password is user password"));

        let pdf = Pdf::open_with_options(
            Cursor::new(corrupted),
            PdfOpenOptions {
                password: password.to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("flpdf should accept both matching passwords: {error}"));
        assert!(pdf.owner_password_matched());
        assert!(pdf.user_password_matched());
        assert_eq!(
            pdf.encryption_file_key().as_deref(),
            Some(expected_key.as_slice()),
            "{suffix} must recover /OE before /UE when the same password validates both entries"
        );
    }
}

#[test]
fn reader_password_mode_does_not_validate_raw_password_bytes() {
    let password = b"\xff\xfeA".to_vec();
    let encrypted = flpdf_encrypted(&minimal_fixture(), &password, false);

    let result = Pdf::open_with_options(
        Cursor::new(encrypted),
        PdfOpenOptions {
            password: password.clone(),
            password_mode: PasswordMode::Unicode,
            ..PdfOpenOptions::default()
        },
    );

    if let Err(error) = result {
        panic!("reader-side password-mode must not reject raw password bytes: {error}");
    }
}
