use assert_cmd::Command;
use flpdf::{PageDocumentHelper, PageObjectHelper, Pdf};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

fn expected_usage(message: &str) -> String {
    format!(
        "\nflpdf: {message}\n\nFor help:\n  flpdf --help=usage       usage information\n  \
flpdf --help=topic       help on a topic\n  flpdf --help=--option    help on an option\n  \
flpdf --help             general help and a topic list\n\n"
    )
}

fn qpdf_available() -> bool {
    ProcessCommand::new("/usr/bin/qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn page_count(path: &std::path::Path) -> usize {
    let mut pdf = Pdf::open(Cursor::new(fs::read(path).unwrap())).unwrap();
    PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .unwrap()
        .len()
}

fn one_page_with_image_pdf() -> Vec<u8> {
    let objects = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n".as_slice(),
        b"4 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 0 >>\nstream\n\nendstream\nendobj\n".as_slice(),
        b"5 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n".as_slice(),
    ];
    let mut bytes = b"%PDF-1.3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

#[test]
fn job_json_file_runs_through_the_production_qpdf_job() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("minimal.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"minimal.pdf","outputFile":"output.pdf","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .code(0)
        .stdout("");

    assert!(directory.path().join("output.pdf").is_file());
}

#[test]
fn job_json_file_show_npages_matches_qpdf_without_output_file() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showNpages":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert_eq!(flpdf.status.code(), Some(0));
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_show_xref_matches_qpdf_without_output_file() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showXref":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert_eq!(flpdf.status.code(), Some(0));
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_show_object_matches_qpdf_without_output_file() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showObject":"trailer"}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert_eq!(flpdf.status.code(), Some(0));
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_show_object_stream_modes_match_qpdf() {
    if !qpdf_available() {
        return;
    }
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");

    for mode in ["rawStreamData", "filteredStreamData"] {
        let directory = tempfile::tempdir().unwrap();
        fs::copy(&fixture, directory.path().join("input.pdf")).unwrap();
        fs::write(
            directory.path().join("job.json"),
            format!(r#"{{"inputFile":"input.pdf","showObject":"7","{mode}":""}}"#),
        )
        .unwrap();

        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .output()
            .unwrap();

        assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
        assert_eq!(flpdf.status.code(), Some(0));
        assert_eq!(flpdf.stdout, qpdf.stdout);
        assert_eq!(flpdf.stderr, qpdf.stderr);
    }
}

#[test]
fn job_json_file_list_attachments_matches_qpdf_without_output_file() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","listAttachments":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert_eq!(flpdf.status.code(), Some(0));
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_show_attachment_matches_qpdf_without_output_file() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showAttachment":"attachment.txt"}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert_eq!(flpdf.status.code(), Some(0));
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_compression_level_reaches_the_writer() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/lone-flate-l9.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    let mut outputs = Vec::new();
    for level in ["1", "9"] {
        let output_name = format!("out-{level}.pdf");
        let output = directory.path().join(&output_name);
        fs::write(
            directory.path().join(format!("job-{level}.json")),
            format!(
                r#"{{"inputFile":"input.pdf","outputFile":"{output_name}","staticId":"","recompressFlate":"","objectStreams":"disable","compressionLevel":"{level}"}}"#
            ),
        )
        .unwrap();
        Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .arg(format!("--job-json-file=job-{level}.json"))
            .assert()
            .success();
        outputs.push(fs::read(output).unwrap());
    }
    assert_ne!(
        outputs[0], outputs[1],
        "job JSON compressionLevel must reach the Flate writer"
    );
}

#[test]
fn job_json_file_copy_encryption_reaches_the_writer() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.pdf");
    let donor = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    let output = directory.path().join("output.pdf");
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        &input,
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        format!(
            r#"{{"inputFile":"{}","outputFile":"{}","copyEncryption":"{}","encryptionFilePassword":"user-v4-aes","staticId":""}}"#,
            input.display(),
            output.display(),
            donor.display()
        ),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .success();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .arg("--password=user-v4-aes")
        .arg("--check")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf must authenticate copied encryption: {qpdf:?}"
    );
}

#[test]
fn job_json_file_password_mode_reaches_encryption_writer() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.pdf");
    let output = directory.path().join("output.pdf");
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        &input,
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        format!(
            r#"{{"inputFile":"{}","outputFile":"{}","passwordMode":"hex-bytes","encrypt":{{"userPassword":"75736572","ownerPassword":"6f776e","128bit":{{"useAes":"y"}}}}}}"#,
            input.display(),
            output.display()
        ),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .success();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .arg("--password=user")
        .arg("--check")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "passwordMode=hex-bytes must decode encryption passwords: {qpdf:?}"
    );
}

#[test]
fn job_json_file_auto_password_warning_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        r#"{"inputFile":"input.pdf","outputFile":"output.pdf","passwordMode":"auto","encrypt":{"userPassword":"😀","ownerPassword":"owner","128bit":{"useAes":"y"}}}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert!(flpdf.status.success(), "flpdf job JSON failed: {flpdf:?}");
    let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr).replace("qpdf:", "flpdf:");
    assert_eq!(flpdf.stderr, qpdf_stderr.as_bytes());
}

#[test]
fn job_json_file_unicode_password_error_is_deferred_to_write() {
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        r#"{"inputFile":"input.pdf","outputFile":"output.pdf","passwordMode":"unicode","encrypt":{"userPassword":"😀","ownerPassword":"owner","128bit":{"useAes":"y"}}}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "supplied password cannot be encoded for 40-bit or 128-bit encryption formats",
        ));
}

#[test]
fn job_json_file_encryption_status_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    for (fixture, json, expected_code) in [
        (
            "minimal.pdf",
            r#"{"inputFile":"input.pdf","isEncrypted":""}"#,
            2,
        ),
        (
            "encrypted/v4-aes-128-r4.pdf",
            r#"{"inputFile":"input.pdf","password":"user-v4-aes","requiresPassword":""}"#,
            3,
        ),
        (
            "encrypted/v4-aes-128-r4.pdf",
            r#"{"inputFile":"input.pdf","password":"wrong","requiresPassword":""}"#,
            0,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        fs::copy(
            fixture_root.join(fixture),
            directory.path().join("input.pdf"),
        )
        .unwrap();
        fs::write(directory.path().join("job.json"), json).unwrap();

        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .output()
            .unwrap();

        assert_eq!(
            qpdf.status.code(),
            Some(expected_code),
            "qpdf probe: {qpdf:?}"
        );
        assert_eq!(
            flpdf.status.code(),
            Some(expected_code),
            "flpdf probe: {flpdf:?}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout);
        assert_eq!(flpdf.stderr, qpdf.stderr);
    }
}

#[test]
fn job_json_file_show_encryption_honors_raw_key_and_key_output() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/encrypted/v5-aes-256-r6.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","password":"fc459408a5282b7c59daa5162f860e82315679cc04942ef57993bfd287f30290","passwordIsHexKey":"","showEncryption":"","showEncryptionKey":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert!(flpdf.status.success(), "flpdf job JSON failed: {flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr).replace("qpdf:", "flpdf:");
    assert_eq!(flpdf.stderr, qpdf_stderr.as_bytes());
}

#[test]
fn job_json_file_report_memory_usage_matches_qpdf_shape() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"/dev/null","reportMemoryUsage":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert!(flpdf.status.success(), "flpdf job JSON failed: {flpdf:?}");
    let qpdf_line = String::from_utf8_lossy(&qpdf.stderr);
    let flpdf_line = String::from_utf8_lossy(&flpdf.stderr);
    assert!(
        qpdf_line.starts_with("qpdf-max-memory-usage "),
        "{qpdf_line}"
    );
    assert!(
        flpdf_line.starts_with("qpdf-max-memory-usage "),
        "{flpdf_line}"
    );
    assert!(
        flpdf_line
            .trim_end()
            .strip_prefix("qpdf-max-memory-usage ")
            .is_some_and(|value| value.parse::<usize>().is_ok()),
        "{flpdf_line}"
    );
}

#[test]
fn job_json_file_nested_job_json_file_is_applied() {
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("nested.json"),
        br#"{"inputFile":"input.pdf","outputFile":"output.pdf","staticId":""}"#,
    )
    .unwrap();
    fs::write(
        directory.path().join("outer.json"),
        br#"{"jobJsonFile":"nested.json"}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=outer.json")
        .assert()
        .success();
    assert!(directory.path().join("output.pdf").is_file());
}

#[test]
fn job_json_file_show_npages_preserves_qpdf_inspection_order() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","check":"","showNpages":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert!(flpdf.status.success(), "flpdf job JSON failed: {flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(flpdf.stdout.ends_with(b"0\n"));
}

#[test]
fn job_json_file_show_npages_preserves_qpdf_malformed_count_fallback() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let mut input = fs::read(fixture).unwrap();
    let marker = b"/Count 1";
    let start = input
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    input[start..start + marker.len()].copy_from_slice(b"/Count  ");
    fs::write(directory.path().join("input.pdf"), input).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showNpages":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert_eq!(
        qpdf.status.code(),
        Some(3),
        "qpdf job JSON failed: {qpdf:?}"
    );
    assert_eq!(
        flpdf.status.code(),
        Some(3),
        "flpdf job JSON failed: {flpdf:?}"
    );
    assert_eq!(flpdf.stdout, b"0\n");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr).replace("qpdf:", "flpdf:");
    assert_eq!(flpdf.stderr, qpdf_stderr.as_bytes());
}

#[test]
fn job_json_file_show_npages_rejects_invalid_values_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();

    for (value, expected) in [
        (serde_json::json!("yes"), "value must be the empty string"),
        (
            serde_json::json!(42),
            "JSON handler: value at .showNpages is not of expected type",
        ),
        (
            serde_json::json!(false),
            "JSON handler: value at .showNpages is not of expected type",
        ),
    ] {
        fs::write(
            directory.path().join("job.json"),
            serde_json::to_vec(&serde_json::json!({
                "inputFile": "input.pdf",
                "showNpages": value,
            }))
            .unwrap(),
        )
        .unwrap();
        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .output()
            .unwrap();

        assert_eq!(
            qpdf.status.code(),
            Some(2),
            "qpdf unexpectedly passed: {qpdf:?}"
        );
        assert_eq!(
            flpdf.status.code(),
            Some(2),
            "flpdf unexpectedly passed: {flpdf:?}"
        );
        assert!(String::from_utf8_lossy(&flpdf.stderr).contains(expected));
    }
}

#[test]
fn job_json_file_show_npages_rejects_an_output_file_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"output.pdf","showNpages":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert_eq!(qpdf.status.code(), Some(2));
    assert_eq!(flpdf.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&qpdf.stderr).contains("no output file may be given"));
    assert!(String::from_utf8_lossy(&flpdf.stderr).contains("no output file may be given"));
    assert!(!directory.path().join("output.pdf").exists());
}

#[test]
fn job_json_file_show_pages_matches_qpdf_without_output_file() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("input.pdf"),
        one_page_with_image_pdf(),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showPages":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert_eq!(
        flpdf.status.code(),
        Some(0),
        "flpdf job JSON failed: {flpdf:?}"
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_show_pages_with_images_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("input.pdf"),
        one_page_with_image_pdf(),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showPages":"","withImages":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert_eq!(
        flpdf.status.code(),
        Some(0),
        "flpdf job JSON failed: {flpdf:?}"
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_show_pages_reports_malformed_contents_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/chained-indirect-contents.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showPages":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert_eq!(qpdf.status.code(), Some(3));
    assert_eq!(flpdf.status.code(), Some(3));
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        flpdf.stderr,
        String::from_utf8_lossy(&qpdf.stderr)
            .replace("qpdf", "flpdf")
            .as_bytes()
    );
}

#[test]
fn job_json_file_with_images_alone_keeps_the_output_requirement() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","withImages":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert_eq!(qpdf.status.code(), Some(2));
    assert_eq!(flpdf.status.code(), Some(2));
    assert_eq!(
        flpdf.stderr,
        String::from_utf8_lossy(&qpdf.stderr)
            .replace("qpdf", "flpdf")
            .as_bytes()
    );
}

#[test]
fn job_json_file_show_pages_preserves_qpdf_inspection_order() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showNpages":"","showPages":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert_eq!(
        flpdf.status.code(),
        Some(0),
        "flpdf job JSON failed: {flpdf:?}"
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_show_pages_rejects_json_output_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showPages":"","jsonOutput":"2"}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert_eq!(qpdf.status.code(), Some(2));
    assert_eq!(flpdf.status.code(), Some(2));
    assert_eq!(
        flpdf.stderr,
        String::from_utf8_lossy(&qpdf.stderr)
            .replace("qpdf", "flpdf")
            .as_bytes()
    );
}

#[test]
fn job_json_file_show_npages_rejects_json_output_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();

    for json_option in ["json", "jsonOutput"] {
        fs::write(
            directory.path().join("job.json"),
            format!(r#"{{"inputFile":"input.pdf","showNpages":"","{json_option}":"2"}}"#),
        )
        .unwrap();

        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .output()
            .unwrap();

        assert_eq!(
            qpdf.status.code(),
            Some(2),
            "qpdf unexpectedly passed: {qpdf:?}"
        );
        assert_eq!(
            flpdf.status.code(),
            Some(2),
            "flpdf unexpectedly passed: {flpdf:?}"
        );
        assert_eq!(
            flpdf.stderr,
            String::from_utf8_lossy(&qpdf.stderr)
                .replace("qpdf", "flpdf")
                .as_bytes()
        );
        assert!(!directory.path().join("output.pdf").exists());
    }
}

#[test]
fn job_json_file_check_mode_repairs_catalog_type_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");

    for job_json in [
        r#"{"inputFile":"input.pdf","check":""}"#,
        r#"{"inputFile":"input.pdf","check":"","showNpages":""}"#,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut input = fs::read(&fixture).unwrap();
        let marker = b"/Type /Catalog";
        let start = input
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap();
        input[start..start + marker.len()].copy_from_slice(b"/Type /Catxxxx");
        fs::write(directory.path().join("input.pdf"), input).unwrap();
        fs::write(directory.path().join("job.json"), job_json).unwrap();

        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .output()
            .unwrap();

        assert_eq!(
            qpdf.status.code(),
            Some(3),
            "qpdf unexpectedly passed: {qpdf:?}"
        );
        assert_eq!(
            flpdf.status.code(),
            Some(3),
            "flpdf unexpectedly passed: {flpdf:?}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout);
        assert_eq!(
            flpdf.stderr,
            String::from_utf8_lossy(&qpdf.stderr)
                .replace("qpdf", "flpdf")
                .as_bytes()
        );
        assert_eq!(
            String::from_utf8_lossy(&flpdf.stderr)
                .matches("catalog /Type entry missing or invalid")
                .count(),
            1
        );
    }
}

#[test]
fn job_json_file_check_does_not_duplicate_repair_warnings_before_show_npages() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let mut input = fs::read(fixture).unwrap();
    let marker = b"/Count 1";
    let start = input
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    input[start..start + marker.len()].copy_from_slice(b"/Count  ");
    fs::write(directory.path().join("input.pdf"), input).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","check":"","showNpages":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert_eq!(
        qpdf.status.code(),
        Some(3),
        "qpdf unexpectedly passed: {qpdf:?}"
    );
    assert_eq!(
        flpdf.status.code(),
        Some(3),
        "flpdf unexpectedly passed: {flpdf:?}"
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        flpdf.stderr,
        String::from_utf8_lossy(&qpdf.stderr)
            .replace("qpdf", "flpdf")
            .as_bytes()
    );
    assert_eq!(
        String::from_utf8_lossy(&flpdf.stderr)
            .matches("expected dictionary key but found non-name object")
            .count(),
        1
    );
}

#[test]
fn job_json_file_check_linearization_matches_qpdf_without_output_file() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearized-one-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","checkLinearization":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert_eq!(
        flpdf.status.code(),
        Some(0),
        "flpdf job JSON failed: {flpdf:?}"
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_show_linearization_matches_qpdf_without_output_file() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearized-one-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showLinearization":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert_eq!(flpdf.status.code(), Some(0));
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_places_show_linearization_before_show_xref() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearized-one-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showLinearization":"","showXref":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert_eq!(flpdf.status.code(), Some(0));
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_check_linearization_reports_non_linearized_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","checkLinearization":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");
    assert!(flpdf.status.success(), "flpdf job JSON failed: {flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_file_check_linearization_preserves_warning_status_and_text() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut input = fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/linearized-one-page.pdf"),
    )
    .unwrap();
    let marker = b"/O 6 /E";
    let start = input
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    input[start..start + marker.len()].copy_from_slice(b"/O 7 /E");
    fs::write(directory.path().join("input.pdf"), input).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","checkLinearization":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert_eq!(
        qpdf.status.code(),
        Some(3),
        "qpdf job JSON failed: {qpdf:?}"
    );
    assert_eq!(
        flpdf.status.code(),
        Some(3),
        "flpdf job JSON failed: {flpdf:?}"
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr).replace("qpdf:", "flpdf:");
    assert_eq!(flpdf.stderr, qpdf_stderr.as_bytes());
}

#[test]
fn job_json_file_check_linearization_rejects_non_empty_values() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearized-one-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","checkLinearization":"yes"}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            ".checkLinearization: value must be the empty string",
        ));
}

#[test]
fn job_json_file_check_linearization_rejects_non_string_values() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearized-one-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();

    for (value, message) in [
        (
            "42",
            "JSON handler: value at .checkLinearization is not of expected type",
        ),
        (
            "false",
            "JSON handler: value at .checkLinearization is not of expected type",
        ),
    ] {
        fs::write(
            directory.path().join("job.json"),
            format!(r#"{{"inputFile":"input.pdf","checkLinearization":{value}}}"#),
        )
        .unwrap();

        Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .assert()
            .code(2)
            .stderr(predicates::str::contains(message));
    }
}

#[test]
fn job_json_file_check_linearization_rejects_an_output_file_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearized-one-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"output.pdf","checkLinearization":""}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert_eq!(
        qpdf.status.code(),
        Some(2),
        "qpdf job JSON unexpectedly passed: {qpdf:?}"
    );
    assert_eq!(
        flpdf.status.code(),
        Some(2),
        "flpdf job JSON unexpectedly passed: {flpdf:?}"
    );
    assert!(String::from_utf8_lossy(&qpdf.stderr)
        .contains("no output file may be given for this option"));
    assert!(String::from_utf8_lossy(&flpdf.stderr)
        .contains("no output file may be given for this option"));
    assert!(!directory.path().join("output.pdf").exists());
}

#[test]
fn job_json_file_collate_values_match_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();

    let q_job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "q.pdf",
        "pages": [
            {"file": "input.pdf", "range": "1-3"},
            {"file": "input.pdf", "range": "1-3"}
        ],
        "collate": "2,1",
        "staticId": ""
    });
    fs::write(
        directory.path().join("q.json"),
        serde_json::to_vec(&q_job).unwrap(),
    )
    .unwrap();
    let f_job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "f.pdf",
        "pages": [
            {"file": "input.pdf", "range": "1-3"},
            {"file": "input.pdf", "range": "1-3"}
        ],
        "collate": "2,1",
        "staticId": ""
    });
    fs::write(
        directory.path().join("f.json"),
        serde_json::to_vec(&f_job).unwrap(),
    )
    .unwrap();

    let q_output = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=q.json")
        .output()
        .unwrap();
    assert!(
        q_output.status.success(),
        "qpdf job JSON failed: {}",
        String::from_utf8_lossy(&q_output.stderr)
    );
    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=f.json")
        .assert()
        .code(0)
        .stdout("");

    assert_eq!(page_count(&directory.path().join("q.pdf")), 6);
    assert_eq!(
        page_count(&directory.path().join("f.pdf")),
        page_count(&directory.path().join("q.pdf"))
    );

    let q_zero_job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "q-zero.pdf",
        "pages": [
            {"file": "input.pdf", "range": "1-3"},
            {"file": "input.pdf", "range": "1-3"}
        ],
        "collate": "0,1",
        "staticId": ""
    });
    fs::write(
        directory.path().join("q-zero.json"),
        serde_json::to_vec(&q_zero_job).unwrap(),
    )
    .unwrap();
    let f_zero_job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "f-zero.pdf",
        "pages": [
            {"file": "input.pdf", "range": "1-3"},
            {"file": "input.pdf", "range": "1-3"}
        ],
        "collate": "0,1",
        "staticId": ""
    });
    fs::write(
        directory.path().join("f-zero.json"),
        serde_json::to_vec(&f_zero_job).unwrap(),
    )
    .unwrap();
    let q_zero_output = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=q-zero.json")
        .output()
        .unwrap();
    assert!(q_zero_output.status.success());
    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=f-zero.json")
        .assert()
        .code(0)
        .stdout("");

    assert_eq!(page_count(&directory.path().join("q-zero.pdf")), 3);
    assert_eq!(
        page_count(&directory.path().join("f-zero.pdf")),
        page_count(&directory.path().join("q-zero.pdf"))
    );
}

#[test]
fn job_json_file_coalesce_contents_replaces_a_page_contents_array() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/multi-contents-one-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("coalesce.json"),
        br#"{"inputFile":"input.pdf","outputFile":"coalesced.pdf","coalesceContents":"","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=coalesce.json")
        .assert()
        .code(0)
        .stdout("");

    let mut pdf = Pdf::open(Cursor::new(
        fs::read(directory.path().join("coalesced.pdf")).unwrap(),
    ))
    .unwrap();
    let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page).unwrap();
    let contents = page.try_get_key(b"/Contents").unwrap();
    pdf.resolve(&contents).unwrap();
    assert!(
        contents.as_stream_dict().is_some(),
        "coalesceContents must replace an array with one stream"
    );
    assert_eq!(
        flpdf::pages::page_content_bytes(&mut pdf, page_ref).unwrap(),
        b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET\nBT /F1 12 Tf 100 680 Td (World) Tj ET\n"
    );
}

#[test]
fn job_json_file_flatten_rotation_bakes_rotate_into_page_content() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page-r90.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("flatten.json"),
        br#"{"inputFile":"input.pdf","outputFile":"flattened.pdf","flattenRotation":"","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=flatten.json")
        .assert()
        .code(0)
        .stdout("");

    let mut pdf = Pdf::open(Cursor::new(
        fs::read(directory.path().join("flattened.pdf")).unwrap(),
    ))
    .unwrap();
    let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page).unwrap();
    assert!(
        !page.has_key(b"/Rotate"),
        "flattenRotation must remove /Rotate"
    );

    let media_box = page.try_get_key(b"/MediaBox").unwrap();
    pdf.resolve(&media_box).unwrap();
    let media_box = media_box.as_array().unwrap();
    assert_eq!(
        media_box
            .iter()
            .map(|value| value.as_integer())
            .collect::<Vec<_>>(),
        vec![Some(0), Some(0), Some(792), Some(612)]
    );

    assert!(
        flpdf::pages::page_content_bytes(&mut pdf, page_ref)
            .unwrap()
            .starts_with(b"q\n0 -1 1 0 0 612 cm\n"),
        "flattenRotation must prepend qpdf's 90-degree matrix"
    );
}

#[test]
fn job_json_file_flatten_rotation_preserves_orphan_widget_warning_status() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/acroform-sig-orphan-widget.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("flatten-warning.json"),
        br#"{"inputFile":"input.pdf","outputFile":"flattened.pdf","flattenRotation":"","staticId":""}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=flatten-warning.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("this widget annotation is not reachable from /AcroForm"),
        "flattenRotation must preserve qpdf's orphan-widget warning"
    );
    assert!(directory.path().join("flattened.pdf").is_file());
}

#[test]
fn job_json_file_generate_appearances_clears_need_marker_and_adds_ap() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("input.pdf"),
        job_json_appearance_fixture(),
    )
    .unwrap();
    fs::write(
        directory.path().join("appearances.json"),
        br#"{"inputFile":"input.pdf","outputFile":"generated.pdf","generateAppearances":"","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=appearances.json")
        .assert()
        .code(0)
        .stdout("");

    let mut pdf = Pdf::open(Cursor::new(
        fs::read(directory.path().join("generated.pdf")).unwrap(),
    ))
    .unwrap();
    let root = pdf.root_handle().unwrap();
    let acroform = root.try_get_key(b"/AcroForm").unwrap();
    pdf.resolve(&acroform).unwrap();
    let need_appearances = acroform.try_get_key(b"/NeedAppearances").unwrap();
    pdf.resolve(&need_appearances).unwrap();
    assert_ne!(
        need_appearances.as_boolean(),
        Some(true),
        "generateAppearances must clear qpdf's NeedAppearances marker"
    );

    let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page).unwrap();
    let annots = page.try_get_key(b"/Annots").unwrap();
    pdf.resolve(&annots).unwrap();
    let widget = annots.as_array().unwrap()[0].clone();
    pdf.resolve(&widget).unwrap();
    let appearance = widget.try_get_key(b"/AP").unwrap();
    pdf.resolve(&appearance).unwrap();
    let normal = appearance.try_get_key(b"/N").unwrap();
    pdf.resolve(&normal).unwrap();
    assert!(
        normal.as_stream_dict().is_some(),
        "generateAppearances must install a normal widget appearance"
    );
}

#[test]
fn job_json_file_flatten_annotations_all_removes_widget_from_annots() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("input.pdf"),
        job_json_flatten_annotation_fixture(),
    )
    .unwrap();
    fs::write(
        directory.path().join("flatten.json"),
        br#"{"inputFile":"input.pdf","outputFile":"flattened.pdf","flattenAnnotations":"all","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=flatten.json")
        .assert()
        .code(0)
        .stdout("");

    let mut pdf = Pdf::open(Cursor::new(
        fs::read(directory.path().join("flattened.pdf")).unwrap(),
    ))
    .unwrap();
    let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
    assert!(
        PageObjectHelper::new(page_ref, &mut pdf)
            .get_annotations_filtered(None)
            .unwrap()
            .is_empty(),
        "flattenAnnotations=all must remove the Widget from /Annots"
    );
    assert!(
        pdf.root_handle()
            .unwrap()
            .try_get_key(b"/AcroForm")
            .unwrap()
            .is_null(),
        "flattenAnnotations=all must remove the now-empty /AcroForm"
    );
}

#[test]
fn job_json_file_flatten_annotations_modes_follow_qpdf_flag_masks() {
    // qpdf's QPDFJob::Config uses (required, forbidden) masks of
    // (0, 0x3), (0, 0x23), and (0x4, 0x3) for all, screen, and print
    // respectively (`libqpdf/QPDFJob_config.cc:190-200`).
    for (mode, expected_drawn) in [("all", 3), ("screen", 2), ("print", 1)] {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("input.pdf"),
            job_json_flagged_annotations_fixture(),
        )
        .unwrap();
        fs::write(
            directory.path().join("flatten.json"),
            format!(
                r#"{{"inputFile":"input.pdf","outputFile":"flattened.pdf","flattenAnnotations":"{mode}","staticId":""}}"#
            ),
        )
        .unwrap();
        fs::write(
            directory.path().join("qpdf.json"),
            format!(
                r#"{{"inputFile":"input.pdf","outputFile":"qpdf.pdf","flattenAnnotations":"{mode}","staticId":""}}"#
            ),
        )
        .unwrap();

        if qpdf_available() {
            let qpdf_output = ProcessCommand::new("/usr/bin/qpdf")
                .current_dir(directory.path())
                .arg("--job-json-file=qpdf.json")
                .output()
                .unwrap();
            assert!(
                qpdf_output.status.success(),
                "qpdf job JSON failed for flattenAnnotations={mode}: {}",
                String::from_utf8_lossy(&qpdf_output.stderr)
            );
        }

        Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .arg("--job-json-file=flatten.json")
            .assert()
            .code(0)
            .stdout("");

        let mut pdf = Pdf::open(Cursor::new(
            fs::read(directory.path().join("flattened.pdf")).unwrap(),
        ))
        .unwrap();
        let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
        let remaining = PageObjectHelper::new(page_ref, &mut pdf)
            .get_annotations_filtered(None)
            .unwrap()
            .len();
        assert_eq!(
            remaining, 0,
            "flattenAnnotations={mode} must remove annotations that have an appearance"
        );
        let content = flpdf::pages::page_content_bytes(&mut pdf, page_ref).unwrap();
        let drawn = content
            .windows(b" Do\n".len())
            .filter(|window| *window == b" Do\n")
            .count();
        assert_eq!(
            drawn, expected_drawn,
            "flattenAnnotations={mode} must apply qpdf's required/forbidden masks"
        );

        if qpdf_available() {
            let mut qpdf = Pdf::open(Cursor::new(
                fs::read(directory.path().join("qpdf.pdf")).unwrap(),
            ))
            .unwrap();
            let qpdf_page_ref = PageDocumentHelper::new(&mut qpdf).get_all_pages().unwrap()[0];
            let qpdf_content = flpdf::pages::page_content_bytes(&mut qpdf, qpdf_page_ref).unwrap();
            let qpdf_drawn = qpdf_content
                .windows(b" Do\n".len())
                .filter(|window| *window == b" Do\n")
                .count();
            assert_eq!(
                qpdf_drawn, expected_drawn,
                "qpdf 11.9.0 must draw the expected annotations for flattenAnnotations={mode}"
            );
            assert_eq!(
                qpdf_drawn, drawn,
                "flpdf and qpdf must agree on flattenAnnotations={mode}"
            );
        }
    }
}

#[test]
fn job_json_file_generate_appearances_runs_before_flatten_annotations() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("input.pdf"),
        job_json_appearance_fixture(),
    )
    .unwrap();
    fs::write(
        directory.path().join("flatten.json"),
        br#"{"inputFile":"input.pdf","outputFile":"flattened.pdf","generateAppearances":"","flattenAnnotations":"all","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=flatten.json")
        .assert()
        .code(0)
        .stdout("");

    let mut pdf = Pdf::open(Cursor::new(
        fs::read(directory.path().join("flattened.pdf")).unwrap(),
    ))
    .unwrap();
    let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
    assert!(
        PageObjectHelper::new(page_ref, &mut pdf)
            .get_annotations_filtered(None)
            .unwrap()
            .is_empty(),
        "generateAppearances must run before flattenAnnotations"
    );
}

#[test]
fn job_json_file_flatten_annotations_rejects_invalid_values() {
    let cases = [
        ("\"bogus\"", "unexpected value"),
        ("42", "value must be a string"),
        ("\"\"", "unexpected value"),
    ];
    for (value, expected_message) in cases {
        let directory = tempfile::tempdir().unwrap();
        fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
            directory.path().join("input.pdf"),
        )
        .unwrap();
        fs::write(
            directory.path().join("invalid.json"),
            format!(
                r#"{{"inputFile":"input.pdf","outputFile":"output.pdf","flattenAnnotations":{value}}}"#
            ),
        )
        .unwrap();

        let output = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .arg("--job-json-file=invalid.json")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "value={value}");
        assert!(stderr.contains(".flattenAnnotations"), "stderr={stderr}");
        assert!(
            stderr.contains(expected_message),
            "value={value} stderr={stderr}"
        );
        assert!(!directory.path().join("output.pdf").exists());
    }
}

#[test]
fn job_json_file_usage_errors_use_the_qpdf_job_file_boundary() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("bad.json"),
        br#"{"objectStreams":"potato"}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=bad.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "error with job-json file bad.json",
        ));
}

#[test]
fn job_json_file_missing_output_reports_one_diagnostic() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("missing-output.json"),
        br#"{"inputFile":"input.pdf"}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=missing-output.json")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr,
        expected_usage("an output file name is required; use - for standard output")
    );
}

#[test]
fn job_json_file_progress_reports_qpdf_write_progress_to_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("progress.json"),
        br#"{"inputFile":"input.pdf","outputFile":"output.pdf","progress":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=progress.json")
        .assert()
        .code(0)
        .stdout(predicates::str::contains("write progress: 0%"))
        .stdout(predicates::str::contains("write progress: 100%"));
}

#[test]
fn job_json_file_preserves_qpdf_warning_status() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    fs::copy(fixture, directory.path().join("repairable.pdf")).unwrap();
    fs::write(
        directory.path().join("warning.json"),
        br#"{"inputFile":"repairable.pdf","outputFile":"output.pdf","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=warning.json")
        .assert()
        .code(3)
        .stderr(predicates::str::contains(
            "operation succeeded with warnings",
        ));

    assert!(directory.path().join("output.pdf").is_file());
}

#[test]
fn job_json_file_rejects_same_input_and_output_without_truncating_input() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let input = directory.path().join("input.pdf");
    let output = directory.path().join("output.pdf");
    fs::copy(fixture, &input).unwrap();
    fs::hard_link(&input, &output).unwrap();
    let before = fs::read(&input).unwrap();
    fs::write(
        directory.path().join("same.json"),
        serde_json::json!({
            "inputFile": input,
            "outputFile": output,
        })
        .to_string(),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=same.json")
        .assert()
        .code(2)
        .stderr(predicates::str::diff(expected_usage(
            "input file and output file are the same; use --replace-input to intentionally overwrite the input",
        )));

    assert_eq!(fs::read(&input).unwrap(), before);
}

#[test]
fn job_json_file_dash_output_is_written_to_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("stdout.json"),
        br#"{"inputFile":"input.pdf","outputFile":"-"}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=stdout.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(!directory.path().join("-").exists());
}

#[test]
fn job_json_file_split_pages_writes_qpdf_named_chunks() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split.json"),
        br#"{"inputFile":"input.pdf","outputFile":"split.pdf","splitPages":"1","staticId":""}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    for page in 1..=3 {
        assert!(
            directory.path().join(format!("split-{page}.pdf")).is_file(),
            "qpdf split-pages output split-{page}.pdf is missing"
        );
    }
    assert!(
        !directory.path().join("split.pdf").exists(),
        "splitPages must not fall through to one unsplit output"
    );
}

#[test]
fn job_json_file_split_pages_preserves_a_positive_chunk_size() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-two.json"),
        br#"{"inputFile":"input.pdf","outputFile":"split-two.pdf","splitPages":"2","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-two.json")
        .assert()
        .code(0);

    assert_eq!(page_count(&directory.path().join("split-two-1-2.pdf")), 2);
    assert_eq!(page_count(&directory.path().join("split-two-3-3.pdf")), 1);
    assert!(!directory.path().join("split-two-1-1.pdf").exists());
}

#[test]
fn job_json_file_split_pages_reports_each_chunk_filename_when_verbose() {
    // qpdf reports one "wrote file" line per real chunk from inside the
    // per-chunk split loop (`libqpdf/QPDFJob.cc:3019-3021`), never the
    // requested output template. Confirmed live against qpdf 11.9.0.
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-verbose.json"),
        br#"{"inputFile":"input.pdf","outputFile":"split.pdf","splitPages":"1","verbose":"","staticId":""}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-verbose.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    for page in 1..=3 {
        assert!(
            stdout.contains(&format!("wrote file split-{page}.pdf")),
            "stdout missing chunk report for split-{page}.pdf: {stdout}"
        );
    }
    assert!(
        !stdout.contains("wrote file split.pdf\n"),
        "verbose report must not name the unsplit output template: {stdout}"
    );
}

#[test]
fn job_json_file_split_pages_reports_earlier_chunks_after_a_later_chunk_fails() {
    // qpdf reports each chunk from inside the per-chunk split loop
    // (`libqpdf/QPDFJob.cc:3019-3021`), immediately after that chunk's write
    // succeeds, so a later chunk's failure still leaves the reports for
    // every chunk written before it. Confirmed live: `qpdf --verbose
    // --split-pages=1` with out-2.pdf pre-occupied by a directory still
    // prints "wrote file out-1.pdf" before failing.
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::create_dir(directory.path().join("out-2.pdf")).unwrap();
    fs::write(
        directory.path().join("split-partial.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","splitPages":"1","verbose":""}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-partial.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("wrote file out-1.pdf"),
        "stdout missing the earlier, successfully written chunk's report: {stdout}"
    );
    assert!(directory.path().join("out-1.pdf").is_file());
}

#[test]
fn job_json_file_split_pages_allows_the_same_input_and_output_path() {
    // qpdf only runs the same-file rejection when `!m->split_pages`
    // (`libqpdf/QPDFJob.cc:627`): a splitting write never truncates the
    // original input in place, so aliasing input and output is not
    // destructive when splitting. Confirmed live against qpdf 11.9.0.
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    let input = directory.path().join("input.pdf");
    fs::copy(fixture, &input).unwrap();
    let before = fs::read(&input).unwrap();
    fs::write(
        directory.path().join("split-same.json"),
        serde_json::json!({
            "inputFile": &input,
            "outputFile": &input,
            "splitPages": "1",
        })
        .to_string(),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-same.json")
        .assert()
        .code(0);

    assert_eq!(fs::read(&input).unwrap(), before, "input must be untouched");
    for page in 1..=3 {
        assert!(directory.path().join(format!("input-{page}.pdf")).is_file());
    }
}

#[test]
fn job_json_file_split_pages_empty_value_defaults_to_one() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-empty.json"),
        br#"{"inputFile":"input.pdf","outputFile":"empty-split.pdf","splitPages":"","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-empty.json")
        .assert()
        .code(0);

    for page in 1..=3 {
        assert!(directory
            .path()
            .join(format!("empty-split-{page}.pdf"))
            .is_file());
    }
    assert!(!directory.path().join("empty-split.pdf").exists());
}

#[test]
fn job_json_file_split_pages_rejects_standard_output_like_qpdf() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-stdout.json"),
        br#"{"inputFile":"input.pdf","outputFile":"-","splitPages":"1"}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-stdout.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--split-pages may not be used when writing to standard output",
        ));
}

#[test]
fn job_json_file_rotate_applies_to_the_selected_page() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("rotate.json"),
        br#"{"inputFile":"input.pdf","outputFile":"rotated.pdf","rotate":"90:2","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=rotate.json")
        .assert()
        .code(0);

    assert_eq!(
        page_rotations(&fs::read(directory.path().join("rotated.pdf")).unwrap()),
        vec![Some(0), Some(90), Some(0)],
        "qpdf rotate=90:2 targets only output page 2"
    );
}

#[test]
fn job_json_file_split_pages_reports_qpdf_late_negative_conversion_error() {
    // Unlike a genuinely non-numeric value (which qpdf's strtoll silently
    // treats as 0, falling through to an unsplit write -- see
    // job_json_file_split_pages_non_numeric_value_falls_through_to_one_unsplit_output),
    // a negative value parses successfully in qpdf and is truthy in its
    // `if (m->split_pages)` checks, only failing later during the actual
    // split loop's unsigned narrowing conversion (libqpdf/QPDFJob.cc:2970).
    // The qpdf-shaped conversion error must be reported from the split path,
    // not from the JSON parser.
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-negative.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","splitPages":"-5"}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-negative.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "integer out of range converting -5 from a 4-byte signed type to a 8-byte unsigned type",
        ));
    assert!(!directory.path().join("out.pdf").exists());
}

#[test]
fn job_json_file_split_pages_rejects_replace_input_like_qpdf() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-replace.json"),
        br#"{"inputFile":"input.pdf","replaceInput":"","splitPages":"1"}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-replace.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--split-pages may not be used with --replace-input",
        ));
}

#[test]
fn job_json_file_rotate_trailing_colon_means_all_pages_like_qpdf() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("rotate-all.json"),
        br#"{"inputFile":"input.pdf","outputFile":"rotated-all.pdf","rotate":"90:","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=rotate-all.json")
        .assert()
        .code(0);

    assert_eq!(
        page_rotations(&fs::read(directory.path().join("rotated-all.pdf")).unwrap()),
        vec![Some(90), Some(90), Some(90)],
        "qpdf treats an empty range after rotate's colon as all pages"
    );
}

#[test]
fn job_json_file_split_pages_non_numeric_value_falls_through_to_one_unsplit_output() {
    // qpdf converts the parameter with `QUtil::string_to_int`, whose
    // `strtoll` stage performs no conversion and returns 0 for a string with
    // no leading digit run; 0 is falsy in qpdf's `if (m->split_pages)`
    // checks, so a malformed value behaves exactly like an explicit "0" and
    // silently falls through to an ordinary, unsplit write rather than being
    // rejected. Confirmed live against qpdf 11.9.0.
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-invalid.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","splitPages":"not-a-number","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-invalid.json")
        .assert()
        .code(0);

    assert!(directory.path().join("out.pdf").is_file());
    assert!(!directory.path().join("out-1.pdf").exists());
}

#[test]
fn job_json_file_remove_restrictions_disables_signature_fields() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/acroform-sig-widget.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("remove.json"),
        br#"{"inputFile":"input.pdf","outputFile":"removed.pdf","removeRestrictions":"","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=remove.json")
        .assert()
        .code(0);

    let mut pdf = Pdf::open(Cursor::new(
        fs::read(directory.path().join("removed.pdf")).unwrap(),
    ))
    .unwrap();
    assert!(
        pdf.signatures().unwrap().is_empty(),
        "qpdf removeRestrictions removes signature fields"
    );
    let root = pdf.root_handle().unwrap();
    let acroform = root.try_get_key(b"/AcroForm").unwrap();
    pdf.resolve(&acroform).unwrap();
    let fields = acroform.try_get_key(b"/Fields").unwrap();
    pdf.resolve(&fields).unwrap();
    assert!(fields.as_array().is_some_and(|items| items.is_empty()));
    let sig_flags = acroform.try_get_key(b"/SigFlags").unwrap();
    pdf.resolve(&sig_flags).unwrap();
    assert_eq!(sig_flags.as_integer(), Some(0));
}

fn page_rotations(bytes: &[u8]) -> Vec<Option<i64>> {
    let mut pdf = Pdf::open(Cursor::new(bytes.to_vec())).unwrap();
    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    pages
        .into_iter()
        .map(|page_ref| {
            let page = pdf.get_object_handle(page_ref);
            pdf.resolve(&page).unwrap();
            let rotate = page.try_get_key(b"/Rotate").unwrap();
            pdf.resolve(&rotate).unwrap();
            rotate.as_integer()
        })
        .collect()
}

fn job_json_appearance_fixture() -> Vec<u8> {
    assemble_pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /NeedAppearances true /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\n",
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>\n",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Annots [4 0 R] >>\n",
        b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name1) /V (Hello) /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720] /P 3 0 R >>\n",
        b"<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\n",
    ])
}

fn job_json_flatten_annotation_fixture() -> Vec<u8> {
    assemble_pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\n",
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>\n",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Annots [4 0 R] >>\n",
        b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (field1) /V (Hello) /Rect [100 700 300 720] /P 3 0 R /AP << /N 6 0 R >> >>\n",
        b"<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\n",
        b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 20] /Length 17 >>\nstream\nBT (Hello) Tj ET\nendstream\n",
    ])
}

fn job_json_flagged_annotations_fixture() -> Vec<u8> {
    assemble_pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R >>\n",
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>\n",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Annots [4 0 R 5 0 R 6 0 R] >>\n",
        b"<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /F 0 /AP << /N 8 0 R >> >>\n",
        b"<< /Type /Annot /Subtype /Text /Rect [40 10 60 30] /F 4 /AP << /N 8 0 R >> >>\n",
        b"<< /Type /Annot /Subtype /Text /Rect [70 10 90 30] /F 32 /AP << /N 8 0 R >> >>\n",
        b"<< /Length 0 >>\nstream\n\nendstream\n",
        b"<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 17 >>\nstream\nBT (Hello) Tj ET\nendstream\n",
    ])
}

fn assemble_pdf(objects: &[&[u8]]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0u64; objects.len() + 1];
    for (index, body) in objects.iter().enumerate() {
        let object_number = index + 1;
        offsets[object_number] = bytes.len() as u64;
        bytes.extend_from_slice(format!("{object_number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"endobj\n");
    }
    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.into_iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}
