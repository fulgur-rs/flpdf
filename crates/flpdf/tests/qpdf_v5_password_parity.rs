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

fn shorten_hex_dictionary_string(bytes: &mut [u8], key: &str, byte_length: usize) {
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
            digits += 1;
            if digits > byte_length * 2 {
                *byte = b' ';
            }
        } else {
            assert!(byte.is_ascii_whitespace(), "unexpected byte in /{key}");
        }
    }
    assert!(byte_length * 2 < digits, "/{key} must be shortened");
    assert_eq!(digits % 2, 0, "/{key} must contain whole bytes");
}

fn flip_first_hex_dictionary_nibble(bytes: &mut [u8], key: &str) {
    let marker = format!("/{key} <");
    let start = bytes
        .windows(marker.len())
        .position(|window| window == marker.as_bytes())
        .unwrap_or_else(|| panic!("PDF is missing {key} hex string"))
        + marker.len();
    let first = bytes[start];
    assert!(
        first.is_ascii_hexdigit(),
        "/{key} must begin with hex digits"
    );
    bytes[start] = if first == b'0' { b'1' } else { b'0' };
}

fn append_hex_dictionary_bytes_and_reindex_xref(
    bytes: &[u8],
    key: &str,
    appended_hex: &[u8],
) -> Vec<u8> {
    assert!(!appended_hex.is_empty() && appended_hex.len().is_multiple_of(2));
    assert!(appended_hex.iter().all(u8::is_ascii_hexdigit));
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

    let startxref_marker = b"startxref\n";
    let startxref_start = bytes
        .windows(startxref_marker.len())
        .rposition(|window| window == startxref_marker)
        .unwrap_or_else(|| panic!("PDF is missing startxref"))
        + startxref_marker.len();
    let startxref_end = startxref_start
        + bytes[startxref_start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or_else(|| panic!("PDF has unterminated startxref"));
    let old_xref_offset = std::str::from_utf8(&bytes[startxref_start..startxref_end])
        .expect("startxref is ASCII")
        .parse::<usize>()
        .expect("startxref is an offset");
    assert!(bytes[old_xref_offset..].starts_with(b"xref\n"));

    let insertion_offset = hex_end;
    let delta = appended_hex.len();
    let mut output = Vec::with_capacity(bytes.len() + delta);
    output.extend_from_slice(&bytes[..hex_end]);
    output.extend_from_slice(appended_hex);
    output.extend_from_slice(&bytes[hex_end..]);

    let xref = &bytes[old_xref_offset..];
    let mut line_start = 0;
    let mut remaining_entries = 0;
    let mut saw_xref = false;
    for line in xref.split_inclusive(|byte| *byte == b'\n') {
        let trimmed = line.trim_ascii();
        if !saw_xref {
            assert_eq!(trimmed, b"xref");
            saw_xref = true;
        } else if trimmed.starts_with(b"trailer") {
            break;
        } else if remaining_entries == 0 {
            let fields: Vec<_> = trimmed
                .split(|byte| byte.is_ascii_whitespace())
                .filter(|field| !field.is_empty())
                .collect();
            assert_eq!(fields.len(), 2, "expected an xref subsection header");
            remaining_entries = std::str::from_utf8(fields[1])
                .expect("xref count is ASCII")
                .parse::<usize>()
                .expect("xref count is an integer");
        } else {
            let fields: Vec<_> = trimmed
                .split(|byte| byte.is_ascii_whitespace())
                .filter(|field| !field.is_empty())
                .collect();
            assert_eq!(fields.len(), 3, "expected an xref entry");
            if fields[2] == b"n" {
                let old_offset = std::str::from_utf8(fields[0])
                    .expect("xref offset is ASCII")
                    .parse::<usize>()
                    .expect("xref offset is an integer");
                if old_offset > insertion_offset {
                    let new_offset = old_offset + delta;
                    let encoded = format!("{new_offset:010}");
                    assert_eq!(encoded.len(), 10, "xref offset must fit its field");
                    let output_entry_start = old_xref_offset + delta + line_start;
                    output[output_entry_start..output_entry_start + 10]
                        .copy_from_slice(encoded.as_bytes());
                }
            }
            remaining_entries -= 1;
        }
        line_start += line.len();
    }
    assert!(saw_xref && remaining_entries == 0);

    let adjusted_xref_offset = old_xref_offset + delta;
    let encoded_xref_offset = adjusted_xref_offset.to_string();
    assert_eq!(
        encoded_xref_offset.len(),
        startxref_end - startxref_start,
        "startxref offset must fit its existing field"
    );
    output[startxref_start + delta..startxref_end + delta]
        .copy_from_slice(encoded_xref_offset.as_bytes());
    output
}

fn qpdf_file_key_and_encryption_report(path: &Path, password: &[u8]) -> (Vec<u8>, String) {
    qpdf_file_key_and_encryption_report_with_exit_codes(path, password, &[0])
}

fn qpdf_file_key_and_encryption_report_with_exit_codes(
    path: &Path,
    password: &[u8],
    exit_codes: &[i32],
) -> (Vec<u8>, String) {
    let output = qpdf_encryption_output(path, password);
    assert!(
        output
            .status
            .code()
            .is_some_and(|code| exit_codes.contains(&code)),
        "qpdf encryption inspection failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    qpdf_file_key_and_report_from_output(&output)
}

fn qpdf_encryption_output(path: &Path, password: &[u8]) -> Output {
    let password = String::from_utf8(password.to_vec()).expect("test password is ASCII");
    Command::new("qpdf")
        .arg(format!("--password={password}"))
        .arg("--show-encryption")
        .arg("--show-encryption-key")
        .arg(path)
        .output()
        .expect("run qpdf encryption inspection")
}

fn qpdf_file_key_and_report_from_output(output: &Output) -> (Vec<u8>, String) {
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

fn qpdf_perms_warning_offset(stderr: &[u8]) -> i64 {
    let stderr = String::from_utf8_lossy(stderr);
    let marker = "(encryption dictionary, offset ";
    let start = stderr
        .find(marker)
        .unwrap_or_else(|| panic!("qpdf warning has no encryption-dictionary context: {stderr}"))
        + marker.len();
    let end = start
        + stderr[start..]
            .find(')')
            .unwrap_or_else(|| panic!("qpdf warning has no offset terminator: {stderr}"));
    stderr[start..end]
        .parse::<i64>()
        .unwrap_or_else(|error| panic!("invalid qpdf warning offset: {error}"))
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
fn v5_encryption_parameter_lengths_match_qpdf_padding_and_prefix_use() {
    if !qpdf_available() {
        eprintln!(
            "qpdf {EXPECTED_QPDF_VERSION} not available; skipping V=5 parameter length parity test"
        );
        return;
    }

    let input = minimal_fixture();
    let directory = tempfile::tempdir().expect("create parameter-length directory");
    let input_path = write_bytes(directory.path(), "input.pdf", &input);

    for r5 in [true, false] {
        let suffix = if r5 { "r5" } else { "r6" };
        let original_path = directory.path().join(format!("{suffix}-original.pdf"));
        write_qpdf_encrypted_with_passwords(&input_path, &original_path, "user", "owner", r5);
        let original = fs::read(&original_path).expect("read qpdf encrypted fixture");

        if r5 {
            for (key, password, target_length) in [
                ("U", b"user".as_slice(), 40),
                ("O", b"owner".as_slice(), 40),
                ("UE", b"user".as_slice(), 31),
                ("OE", b"owner".as_slice(), 31),
            ] {
                let mut candidate = original.clone();
                shorten_hex_dictionary_string(&mut candidate, key, target_length);
                let candidate_path = write_bytes(
                    directory.path(),
                    &format!("{suffix}-short-{key}.pdf"),
                    &candidate,
                );
                let (expected_key, qpdf_report) =
                    qpdf_file_key_and_encryption_report_with_exit_codes(
                        &candidate_path,
                        password,
                        &[0, 3],
                    );
                let role = if key == "O" || key == "OE" {
                    "owner password"
                } else {
                    "user password"
                };
                assert!(
                    qpdf_report.contains(&format!("Supplied password is {role}")),
                    "qpdf must authenticate the {role} for short /{key}:\n{qpdf_report}"
                );
                let pdf = Pdf::open_with_options(
                    Cursor::new(candidate),
                    PdfOpenOptions {
                        password: password.to_vec(),
                        ..PdfOpenOptions::default()
                    },
                )
                .unwrap_or_else(|error| {
                    panic!("flpdf should accept qpdf's NUL-padded short /{key}: {error}")
                });
                assert_eq!(
                    pdf.encryption_file_key().as_deref(),
                    Some(expected_key.as_slice()),
                    "{suffix} short /{key} must use the same padded material as qpdf"
                );
            }
        }

        for (key, password, appended_hex) in [
            ("UE", b"user".as_slice(), b"a5".as_slice()),
            ("OE", b"owner".as_slice(), b"5a".as_slice()),
        ] {
            let candidate =
                append_hex_dictionary_bytes_and_reindex_xref(&original, key, appended_hex);
            let candidate_path = write_bytes(
                directory.path(),
                &format!("{suffix}-long-{key}.pdf"),
                &candidate,
            );
            let (expected_key, qpdf_report) =
                qpdf_file_key_and_encryption_report(&candidate_path, password);
            let role = if key == "OE" {
                "owner password"
            } else {
                "user password"
            };
            assert!(
                qpdf_report.contains(&format!("Supplied password is {role}")),
                "qpdf must authenticate the {role} for long /{key}:\n{qpdf_report}"
            );
            let pdf = Pdf::open_with_options(
                Cursor::new(candidate),
                PdfOpenOptions {
                    password: password.to_vec(),
                    ..PdfOpenOptions::default()
                },
            )
            .unwrap_or_else(|error| panic!("flpdf should use the qpdf /{key} prefix: {error}"));
            assert_eq!(
                pdf.encryption_file_key().as_deref(),
                Some(expected_key.as_slice()),
                "{suffix} long /{key} must use the same prefix as qpdf"
            );
        }
    }
}

#[test]
fn v5_perms_validation_and_warning_contract_match_qpdf() {
    if !qpdf_available() {
        eprintln!("qpdf {EXPECTED_QPDF_VERSION} not available; skipping V=5 /Perms parity test");
        return;
    }

    let input = minimal_fixture();
    let directory = tempfile::tempdir().expect("create /Perms parity directory");
    let input_path = write_bytes(directory.path(), "input.pdf", &input);

    for r5 in [true, false] {
        let suffix = if r5 { "r5" } else { "r6" };
        let original_path = directory.path().join(format!("{suffix}-original.pdf"));
        write_qpdf_encrypted_with_passwords(&input_path, &original_path, "user", "owner", r5);
        let original = fs::read(&original_path).expect("read qpdf encrypted fixture");

        for (key, password) in [
            ("UE", b"user".as_slice()),
            ("OE", b"owner".as_slice()),
            ("Perms", b"user".as_slice()),
        ] {
            let mut candidate = original.clone();
            flip_first_hex_dictionary_nibble(&mut candidate, key);
            let candidate_path = write_bytes(
                directory.path(),
                &format!("{suffix}-bad-{key}.pdf"),
                &candidate,
            );
            assert_qpdf_perms_warning_matches_flpdf(&candidate_path, candidate, password);
        }

        let mut short_perms = original.clone();
        shorten_hex_dictionary_string(&mut short_perms, "Perms", 12);
        let short_perms_path = write_bytes(
            directory.path(),
            &format!("{suffix}-short-perms.pdf"),
            &short_perms,
        );
        assert_qpdf_perms_warning_matches_flpdf(&short_perms_path, short_perms, b"user");

        let long_perms = append_hex_dictionary_bytes_and_reindex_xref(&original, "Perms", b"a5");
        let long_perms_path = write_bytes(
            directory.path(),
            &format!("{suffix}-long-perms.pdf"),
            &long_perms,
        );
        let qpdf_output = qpdf_encryption_output(&long_perms_path, b"user");
        assert_eq!(qpdf_output.status.code(), Some(0));
        let (expected_key, qpdf_report) = qpdf_file_key_and_report_from_output(&qpdf_output);
        assert!(qpdf_report.contains("Supplied password is user password"));
        let pdf = Pdf::open_with_options(
            Cursor::new(long_perms),
            PdfOpenOptions {
                password: b"user".to_vec(),
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("qpdf accepts /Perms values longer than the first 16 bytes");
        assert!(pdf.repair_diagnostics().entries().is_empty());
        assert_eq!(
            pdf.encryption_file_key().as_deref(),
            Some(expected_key.as_slice()),
            "{suffix} long /Perms must use the same first-block key as qpdf"
        );
    }
}

fn assert_qpdf_perms_warning_matches_flpdf(path: &Path, bytes: Vec<u8>, password: &[u8]) {
    let qpdf_output = qpdf_encryption_output(path, password);
    assert_eq!(
        qpdf_output.status.code(),
        Some(3),
        "qpdf should report a non-fatal /Perms warning:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&qpdf_output.stdout),
        String::from_utf8_lossy(&qpdf_output.stderr)
    );
    let expected_message = b"/Perms field in encryption dictionary doesn't match expected value";
    let qpdf_stderr = String::from_utf8_lossy(&qpdf_output.stderr);
    assert!(
        qpdf_stderr.contains("(encryption dictionary, offset "),
        "qpdf should identify the encryption dictionary in its warning:\n{qpdf_stderr}"
    );
    assert!(
        qpdf_output
            .stderr
            .windows(expected_message.len())
            .any(|window| window == expected_message),
        "qpdf should emit its fixed /Perms warning:\n{qpdf_stderr}"
    );
    let (expected_key, qpdf_report) = qpdf_file_key_and_report_from_output(&qpdf_output);
    let role = if password == b"owner" {
        "Supplied password is owner password"
    } else {
        "Supplied password is user password"
    };
    assert!(qpdf_report.contains(role));

    let description = path.display().to_string();
    let pdf = Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            description: description.as_bytes().to_vec(),
            password: password.to_vec(),
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    )
    .unwrap_or_else(|error| panic!("flpdf should accept the authenticated PDF: {error}"));
    let diagnostics = pdf.repair_diagnostics();
    assert_eq!(diagnostics.entries().len(), 1);
    let warning = &diagnostics.entries()[0];
    assert_eq!(warning.get_error_code(), flpdf::QpdfErrorCode::DamagedPdf);
    assert_eq!(warning.get_filename(), description.as_bytes());
    assert_eq!(warning.get_object(), b"encryption dictionary");
    assert_eq!(
        warning.get_file_position(),
        qpdf_perms_warning_offset(&qpdf_output.stderr)
    );
    assert_eq!(warning.get_message_detail(), expected_message);
    assert_eq!(
        pdf.encryption_file_key().as_deref(),
        Some(expected_key.as_slice())
    );
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
