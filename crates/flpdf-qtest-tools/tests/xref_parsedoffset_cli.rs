use assert_cmd::Command;
use std::fs;
use std::path::Path;
use std::process::Command as ProcessCommand;
#[cfg(unix)]
use std::process::Stdio;

fn fixture_path(name: &str) -> String {
    format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn qpdf_error_message(path: &Path) -> Option<Vec<u8>> {
    let qpdf = std::env::var_os("QPDF").unwrap_or_else(|| "qpdf".into());
    let version = match ProcessCommand::new(&qpdf).arg("--version").output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::env::var_os("CI").is_some() {
                panic!("qpdf 11.9.0 is required for metadata differential tests");
            }
            eprintln!("skipping metadata differential test: qpdf is not available");
            return None;
        }
        Err(error) => panic!("run qpdf --version: {error}"),
    };
    assert!(version.status.success(), "qpdf --version failed");
    if !version.stdout.starts_with(b"qpdf version 11.9.0\n")
        && !version.stdout.starts_with(b"qpdf version 11.9.0\r\n")
    {
        assert!(
            std::env::var_os("CI").is_none(),
            "qpdf 11.9.0 is required for metadata differential tests, got {:?}",
            String::from_utf8_lossy(&version.stdout)
        );
        eprintln!(
            "skipping metadata differential test: expected qpdf 11.9.0, got {:?}",
            String::from_utf8_lossy(&version.stdout)
        );
        return None;
    }

    let output = ProcessCommand::new(qpdf)
        .arg("--check")
        .arg(path)
        .output()
        .expect("run qpdf --check");
    assert_eq!(
        output.status.code(),
        Some(2),
        "qpdf --check must reject input"
    );
    assert!(
        output.stdout.is_empty(),
        "qpdf --check must not write stdout"
    );

    let open_marker = b"open ";
    let start = output
        .stderr
        .windows(open_marker.len())
        .position(|window| window == open_marker)
        .expect("qpdf open failure must contain the open operation");
    let qpdf_stderr = &output.stderr[start..];
    let mut normalized = Vec::with_capacity(qpdf_stderr.len());
    let mut index = 0;
    while index < qpdf_stderr.len() {
        if qpdf_stderr[index] == b'\r' && qpdf_stderr.get(index + 1) == Some(&b'\n') {
            index += 1;
        }
        normalized.push(qpdf_stderr[index]);
        index += 1;
    }
    Some(normalized)
}

fn assert_metadata_helpers_match_qpdf(path: &Path, expected_stderr: &[u8]) {
    for binary in ["test_xref", "test_parsedoffset"] {
        let output = Command::cargo_bin(binary)
            .expect("metadata helper binary")
            .arg(path)
            .output()
            .expect("run metadata helper");
        assert_eq!(output.status.code(), Some(2), "{binary} must reject input");
        assert!(output.stdout.is_empty(), "{binary} must not write stdout");
        assert_eq!(
            output.stderr, expected_stderr,
            "{binary} must match qpdf's open diagnostic"
        );
    }
}

#[test]
fn test_xref_formats_the_effective_source_table_in_object_order() {
    Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .arg(fixture_path("minimal.pdf"))
        .assert()
        .success()
        .stdout(
            "1/0, uncompressed, offset = 9 (0x9)\n\
             2/0, uncompressed, offset = 58 (0x3a)\n",
        )
        .stderr("");
}

#[test]
fn test_xref_preserves_compressed_stream_number_and_index() {
    Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .arg(fixture_path("compat/three-page-objstm.pdf"))
        .assert()
        .success()
        .stdout(concat!(
            "1/0, uncompressed, offset = 15 (0xf)\n",
            "2/0, compressed, stream number = 1, stream index = 0\n",
            "3/0, compressed, stream number = 1, stream index = 1\n",
            "4/0, compressed, stream number = 1, stream index = 2\n",
            "5/0, compressed, stream number = 1, stream index = 3\n",
            "6/0, compressed, stream number = 1, stream index = 4\n",
            "7/0, compressed, stream number = 1, stream index = 5\n",
            "8/0, compressed, stream number = 1, stream index = 6\n",
            "9/0, compressed, stream number = 1, stream index = 7\n",
            "10/0, uncompressed, offset = 532 (0x214)\n",
            "11/0, uncompressed, offset = 685 (0x2ad)\n",
            "12/0, uncompressed, offset = 838 (0x346)\n",
            "13/0, uncompressed, offset = 991 (0x3df)\n",
        ))
        .stderr("");
}

#[test]
fn test_parsedoffset_walks_direct_children_and_formats_qpdf_offsets() {
    Command::cargo_bin("test_parsedoffset")
        .expect("test_parsedoffset binary")
        .arg(fixture_path("minimal.pdf"))
        .assert()
        .success()
        .stdout(
            "--- objects not in streams ---\n\
             offset = 17 (0x11), indirect 1/0, dictionary\n\
             offset = 26 (0x1a), direct, name\n\
             offset = 66 (0x42), indirect 2/0, dictionary\n\
             offset = 75 (0x4b), direct, name\n\
             offset = 89 (0x59), direct, integer\n\
             offset = 97 (0x61), direct, array\n\
             succeeded\n",
        )
        .stderr("");
}

#[test]
fn test_parsedoffset_attributes_empty_object_warnings_to_the_object() {
    let mut input_bytes = b"%PDF-1.7\n".to_vec();
    let object1_offset = input_bytes.len();
    input_bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let object2_offset = input_bytes.len();
    input_bytes.extend_from_slice(b"2 0 obj\nendobj\n");
    let empty_offset = object2_offset + b"2 0 obj\n".len();
    let xref_offset = input_bytes.len();
    input_bytes.extend_from_slice(
        format!(
            "xref\n0 3\n0000000000 65535 f \n{object1_offset:010} 00000 n \n{object2_offset:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("empty-object.pdf");
    fs::write(&input, input_bytes).expect("write empty-object fixture");
    let path = input.display();
    let expected_warning = format!(
        "WARNING: {path} (object 2 0, offset {empty_offset}): empty object treated as null\n"
    );

    Command::cargo_bin("test_parsedoffset")
        .expect("test_parsedoffset binary")
        .arg(&input)
        .assert()
        .success()
        .stderr(predicates::str::contains(expected_warning));
}

#[test]
fn test_parsedoffset_groups_objects_in_object_streams() {
    Command::cargo_bin("test_parsedoffset")
        .expect("test_parsedoffset binary")
        .arg(fixture_path("compat/three-page-objstm.pdf"))
        .assert()
        .success()
        .stdout(predicates::str::contains("--- objects in stream 1 ---"))
        .stdout(predicates::str::contains(
            "offset = 45 (0x2d), indirect 2/0, dictionary",
        ))
        .stdout(predicates::str::contains(
            "offset = 61 (0x3d), indirect 3/0, dictionary",
        ))
        .stdout(predicates::str::contains("succeeded\n"))
        .stderr("");
}

#[test]
fn metadata_helpers_match_qpdf_usage_contracts() {
    Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .assert()
        .code(2)
        .stdout("")
        .stderr("usage: test_xref INPUT.pdf\n");

    Command::cargo_bin("test_parsedoffset")
        .expect("test_parsedoffset binary")
        .assert()
        .code(2)
        .stdout("")
        .stderr("Usage: test_parsedoffset INPUT.pdf\n");
}

#[cfg(unix)]
#[test]
fn metadata_usage_write_failures_do_not_panic() {
    let dev_full = Path::new("/dev/full");
    if !dev_full.exists() {
        eprintln!("skipping metadata /dev/full write-failure test: device is unavailable");
        return;
    }

    for binary in [
        env!("CARGO_BIN_EXE_test_xref"),
        env!("CARGO_BIN_EXE_test_parsedoffset"),
    ] {
        let stderr = fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("/dev/full");
        let status = ProcessCommand::new(binary)
            .stderr(Stdio::from(stderr))
            .status()
            .expect("run metadata helper");
        assert_eq!(status.code(), Some(2), "helper must return usage status");
    }
}

#[test]
fn metadata_helpers_report_missing_input_with_qpdf_open_wording() {
    let path = Path::new("/definitely/missing/flpdf-metadata.pdf");
    let Some(expected_stderr) = qpdf_error_message(path) else {
        return;
    };
    assert_metadata_helpers_match_qpdf(path, &expected_stderr);
}

#[cfg(unix)]
#[test]
fn metadata_helpers_report_read_failures_with_qpdf_file_input_wording() {
    let directory = tempfile::tempdir().expect("temporary directory");
    for binary in ["test_xref", "test_parsedoffset"] {
        Command::cargo_bin(binary)
            .expect("metadata helper binary")
            .arg(directory.path())
            .assert()
            .code(2)
            .stdout("")
            .stderr(format!("{}: read 1024 bytes\n", directory.path().display()));
    }
}

#[cfg(windows)]
#[test]
fn metadata_helpers_report_directory_open_failures_with_native_wording() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let Some(expected_stderr) = qpdf_error_message(directory.path()) else {
        return;
    };
    assert_metadata_helpers_match_qpdf(directory.path(), &expected_stderr);
}

#[test]
fn test_xref_preserves_qpdf_recovery_diagnostics_before_terminal_failure() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("malformed.pdf");
    fs::write(&input, b"garbage").expect("write malformed input");
    let path = input.display();
    let expected = format!(
        "WARNING: {path}: can't find PDF header\n\
         WARNING: {path}: file is damaged\n\
         WARNING: {path}: can't find startxref\n\
         WARNING: {path}: Attempting to reconstruct cross-reference table\n\
         {path}: unable to find trailer dictionary while recovering damaged file\n"
    );

    Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .arg(&input)
        .assert()
        .code(2)
        .stdout("")
        .stderr(expected);
}

#[test]
fn test_parsedoffset_rejects_an_enumerated_object_missing_from_xref() {
    Command::cargo_bin("test_parsedoffset")
        .expect("test_parsedoffset binary")
        .arg(fixture_path("compat/dangling-body-one-page.pdf"))
        .assert()
        .code(2)
        .stdout("")
        .stderr("99/0 is not found in xref table\n");
}

#[test]
fn test_parsedoffset_preserves_repair_warnings_before_post_enumeration_failure() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut input_bytes =
        fs::read(fixture_path("test_driver/repairable_input.pdf")).expect("read repairable input");
    let marker = b"/QTest true >>";
    let marker_start = input_bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("repairable trailer marker");
    let insert_at = marker_start + b"/QTest true".len();
    input_bytes.splice(insert_at..insert_at, b" /Info 99 0 R".iter().copied());

    let input = directory.path().join("repairable-with-dangling-info.pdf");
    fs::write(&input, input_bytes).expect("write repaired-input variant");
    let path = input.display();

    let output = Command::cargo_bin("test_parsedoffset")
        .expect("test_parsedoffset binary")
        .arg(&input)
        .output()
        .expect("run test_parsedoffset");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8_lossy(&output.stderr);
    let warning_start = format!(
        "WARNING: {path}: file is damaged\n\
         WARNING: {path}: can't find startxref\n\
         WARNING: {path}: Attempting to reconstruct cross-reference table\n"
    );
    assert!(
        stderr.starts_with(&warning_start),
        "repair warnings must precede the terminal error: {stderr}"
    );
    assert!(
        stderr.ends_with("99/0 is not found in xref table\n"),
        "the post-enumeration error must remain visible: {stderr}"
    );
    assert!(
        !stderr.contains(&format!("{path}: 99/0 is not found in xref table")),
        "post-enumeration errors must not be reclassified as open failures: {stderr}"
    );
}

#[test]
fn encrypted_authentication_failure_preserves_repair_warnings() {
    let mut input_bytes =
        fs::read(fixture_path("encrypted/v5-aes-256-r6.pdf")).expect("read encrypted fixture");
    let xref_header = input_bytes
        .windows(b"xref\n0 4\n".len())
        .position(|window| window == b"xref\n0 4\n")
        .expect("xref header");
    input_bytes[xref_header + b"xref\n0 ".len()] = b'X';

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("damaged-encrypted.pdf");
    fs::write(&input, input_bytes).expect("write damaged encrypted input");
    let path = input.display();

    let output = Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .arg(&input)
        .output()
        .expect("run test_xref");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("WARNING: {path}:")),
        "authentication failure must retain repair warnings: {stderr}"
    );
    assert!(
        stderr.ends_with(&format!("{path}: invalid password\n")),
        "terminal authentication error must retain qpdf path wording: {stderr}"
    );
}

#[test]
fn test_xref_matches_qpdf_recovered_xref_and_warning_order() {
    let path = fixture_path("test_driver/repairable_input.pdf");
    Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .arg(path)
        .assert()
        .success()
        .stdout(concat!(
            "1/0, uncompressed, offset = 9 (0x9)\n",
            "2/0, uncompressed, offset = 58 (0x3a)\n",
        ))
        .stderr(concat!(
            "WARNING: ",
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/test_driver/repairable_input.pdf: file is damaged\n",
            "WARNING: ",
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/test_driver/repairable_input.pdf: can't find startxref\n",
            "WARNING: ",
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/test_driver/repairable_input.pdf: Attempting to reconstruct cross-reference table\n",
        ));
}
