use assert_cmd::Command;
use flpdf::{PageDocumentHelper, PageObjectHelper, Pdf};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;

fn expected_usage(message: &str) -> String {
    format!(
        "{EOL}flpdf: {message}{EOL}{EOL}For help:{EOL}  flpdf --help=usage       usage information{EOL}  \
flpdf --help=topic       help on a topic{EOL}  flpdf --help=--option    help on an option{EOL}  \
flpdf --help             general help and a topic list{EOL}{EOL}"
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
fn job_json_file_password_and_password_file_follow_argv_order() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    let password_file = directory.path().join("password.txt");
    fs::write(&password_file, b"user-v4-aes\n").unwrap();
    let password_file_arg = format!("--password-file={}", password_file.display());

    for (name, options, expected_status) in [
        (
            "file-wins",
            vec!["--password=wrong".to_owned(), password_file_arg.clone()],
            Some(0),
        ),
        (
            "password-wins",
            vec![password_file_arg.clone(), "--password=wrong".to_owned()],
            Some(2),
        ),
    ] {
        let qpdf_output = format!("qpdf-{name}.pdf");
        let flpdf_output = format!("flpdf-{name}.pdf");
        let qpdf_job = format!("qpdf-{name}.json");
        let flpdf_job = format!("flpdf-{name}.json");
        let job_json = |output: &str| {
            format!(
                r#"{{"inputFile":"input.pdf","outputFile":"{output}","staticId":"","decrypt":""}}"#
            )
        };
        fs::write(directory.path().join(&qpdf_job), job_json(&qpdf_output)).unwrap();
        fs::write(directory.path().join(&flpdf_job), job_json(&flpdf_output)).unwrap();

        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .args(&options)
            .arg(format!("--job-json-file={qpdf_job}"))
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .env("FLPDF_PROGNAME", "qpdf")
            .args(&options)
            .arg(format!("--job-json-file={flpdf_job}"))
            .output()
            .unwrap();

        assert_eq!(qpdf.status.code(), expected_status, "qpdf {name}: {qpdf:?}");
        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "flpdf {name}: {flpdf:?}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout, "stdout differs for {name}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "stderr differs for {name}");
    }
}

#[test]
fn job_json_file_password_follows_argv_order() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","password":"user-v4-aes","staticId":"","decrypt":""}"#,
    )
    .unwrap();

    for (name, args) in [
        (
            "cli-before-json",
            vec![
                "--password=wrong".to_owned(),
                "--job-json-file=job.json".to_owned(),
            ],
        ),
        (
            "json-before-cli",
            vec![
                "--job-json-file=job.json".to_owned(),
                "--password=wrong".to_owned(),
            ],
        ),
    ] {
        let output = directory.path().join("out.pdf");
        let _ = fs::remove_file(&output);
        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .args(&args)
            .output()
            .unwrap();
        let _ = fs::remove_file(&output);
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .env("FLPDF_PROGNAME", "qpdf")
            .args(&args)
            .output()
            .unwrap();

        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "status differs for {name}: qpdf={qpdf:?}, flpdf={flpdf:?}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout, "stdout differs for {name}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "stderr differs for {name}");
    }
}

#[test]
fn job_json_file_password_file_follows_argv_order() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(directory.path().join("password.txt"), b"user-v4-aes\n").unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","password":"wrong","staticId":"","decrypt":""}"#,
    )
    .unwrap();

    for (name, args) in [
        (
            "file-before-json",
            vec![
                "--password-file=password.txt".to_owned(),
                "--job-json-file=job.json".to_owned(),
            ],
        ),
        (
            "json-before-file",
            vec![
                "--job-json-file=job.json".to_owned(),
                "--password-file=password.txt".to_owned(),
            ],
        ),
    ] {
        let output = directory.path().join("out.pdf");
        let _ = fs::remove_file(&output);
        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .args(&args)
            .output()
            .unwrap();
        let _ = fs::remove_file(&output);
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .env("FLPDF_PROGNAME", "qpdf")
            .args(&args)
            .output()
            .unwrap();

        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "status differs for {name}: qpdf={qpdf:?}, flpdf={flpdf:?}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout, "stdout differs for {name}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "stderr differs for {name}");
    }
}

#[test]
fn job_json_file_password_mode_follows_argv_order() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","password":"757365722d76342d616573","staticId":"","decrypt":""}"#,
    )
    .unwrap();

    for (name, args) in [
        (
            "mode-before-json",
            ["--password-mode=hex-bytes", "--job-json-file=job.json"],
        ),
        (
            "json-before-mode",
            ["--job-json-file=job.json", "--password-mode=hex-bytes"],
        ),
    ] {
        let output = directory.path().join("out.pdf");
        let _ = fs::remove_file(&output);
        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            qpdf.status.success(),
            "qpdf job JSON failed for {name}: {qpdf:?}"
        );
        assert!(
            output.is_file(),
            "qpdf should create the decrypted output for {name}"
        );
        fs::remove_file(&output).unwrap();

        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .env("FLPDF_PROGNAME", "qpdf")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "status differs for {name}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout, "stdout differs for {name}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "stderr differs for {name}");
    }
}

#[test]
fn job_json_file_password_is_hex_key_follows_argv_order() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","password":"5042ec4efa389ea32a149ab2a34e84fc","staticId":"","decrypt":""}"#,
    )
    .unwrap();

    for (name, args) in [
        (
            "hex-key-before-json",
            ["--password-is-hex-key", "--job-json-file=job.json"],
        ),
        (
            "json-before-hex-key",
            ["--job-json-file=job.json", "--password-is-hex-key"],
        ),
    ] {
        let output = directory.path().join("out.pdf");
        let _ = fs::remove_file(&output);
        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            qpdf.status.success(),
            "qpdf job JSON failed for {name}: {qpdf:?}"
        );
        assert!(
            output.is_file(),
            "qpdf should create the decrypted output for {name}"
        );
        fs::remove_file(&output).unwrap();

        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .env("FLPDF_PROGNAME", "qpdf")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "status differs for {name}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout, "stdout differs for {name}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "stderr differs for {name}");
    }
}

#[test]
fn job_json_file_empty_input_selector_follows_argv_order() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"outputFile":"out.pdf","staticId":""}"#,
    )
    .unwrap();

    for (name, args) in [
        (
            "empty-before-json",
            vec!["--empty".to_owned(), "--job-json-file=job.json".to_owned()],
        ),
        (
            "json-before-empty",
            vec!["--job-json-file=job.json".to_owned(), "--empty".to_owned()],
        ),
    ] {
        let output = directory.path().join("out.pdf");
        let _ = fs::remove_file(&output);
        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .args(&args)
            .output()
            .unwrap();
        let qpdf_bytes = fs::read(&output).expect("qpdf should write an empty PDF");
        fs::remove_file(&output).unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .env("FLPDF_PROGNAME", "qpdf")
            .args(&args)
            .output()
            .unwrap();

        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "status differs for {name}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout, "stdout differs for {name}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "stderr differs for {name}");
        assert_eq!(
            fs::read(&output).unwrap(),
            qpdf_bytes,
            "output differs for {name}"
        );
    }
}

#[test]
fn job_json_file_selector_errors_follow_argv_order() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();

    let cases: &[(&str, &[u8], &[&str])] = &[
        (
            "output-json-before-positional",
            br#"{"outputFile":"json-output.pdf"}"#,
            &[
                "--job-json-file=job.json",
                "input.pdf",
                "positional-output.pdf",
            ],
        ),
        (
            "output-positional-before-json",
            br#"{"outputFile":"json-output.pdf"}"#,
            &[
                "input.pdf",
                "positional-output.pdf",
                "--job-json-file=job.json",
            ],
        ),
        (
            "input-json-before-positional",
            br#"{"inputFile":"json-input.pdf"}"#,
            &["--job-json-file=job.json", "input.pdf"],
        ),
        (
            "input-positional-before-json",
            br#"{"inputFile":"json-input.pdf"}"#,
            &["input.pdf", "--job-json-file=job.json"],
        ),
    ];

    for (name, json, args) in cases {
        fs::write(directory.path().join("job.json"), json).unwrap();
        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .args(*args)
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .env("FLPDF_PROGNAME", "qpdf")
            .args(*args)
            .output()
            .unwrap();

        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "status differs for {name}: qpdf={qpdf:?}, flpdf={flpdf:?}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout, "stdout differs for {name}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "stderr differs for {name}");
    }
}

#[test]
fn top_level_parse_errors_follow_qpdf_argv_order() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    let missing_job = directory.path().join("missing.json");
    let missing_job_argument = format!("--job-json-file={}", missing_job.display());
    let cases = vec![
        (
            "rotate-before-json-output",
            vec![
                "--rotate=91".to_owned(),
                "--json-output=1".to_owned(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "json-output-before-rotate",
            vec![
                "--json-output=1".to_owned(),
                "--rotate=91".to_owned(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "rotate-before-json",
            vec![
                "--rotate=91".to_owned(),
                "--json=0".to_owned(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "json-before-rotate",
            vec![
                "--json=0".to_owned(),
                "--rotate=91".to_owned(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "rotate-before-collate",
            vec![
                "--rotate=91".to_owned(),
                "--collate=1,".to_owned(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "collate-before-rotate",
            vec![
                "--collate=1,".to_owned(),
                "--rotate=91".to_owned(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "compression-level-before-job-json",
            vec![
                "--compression-level=999999999999999999999".to_owned(),
                missing_job_argument.clone(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "ii-min-bytes-before-job-json",
            vec![
                "--ii-min-bytes=999999999999999999999".to_owned(),
                missing_job_argument.clone(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "oi-min-area-before-job-json",
            vec![
                "--oi-min-area=999999999999999999999".to_owned(),
                missing_job_argument.clone(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "oi-min-height-before-job-json",
            vec![
                "--oi-min-height=999999999999999999999".to_owned(),
                missing_job_argument.clone(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "oi-min-width-before-job-json",
            vec![
                "--oi-min-width=999999999999999999999".to_owned(),
                missing_job_argument.clone(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "split-pages-before-job-json",
            vec![
                "--split-pages=999999999999999999999".to_owned(),
                missing_job_argument.clone(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "show-object-before-job-json",
            vec![
                "--show-object=2147483648".to_owned(),
                missing_job_argument.clone(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "job-json-before-rotate",
            vec![
                missing_job_argument.clone(),
                "--rotate=91".to_owned(),
                "input.pdf".to_owned(),
            ],
        ),
        (
            "rotate-before-job-json",
            vec![
                "--rotate=91".to_owned(),
                missing_job_argument,
                "input.pdf".to_owned(),
            ],
        ),
    ];

    for (name, args) in cases {
        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .args(&args)
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .env("FLPDF_PROGNAME", "qpdf")
            .args(&args)
            .output()
            .unwrap();

        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "status differs for {name}: qpdf={qpdf:?}, flpdf={flpdf:?}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout, "stdout differs for {name}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "stderr differs for {name}");
    }
}

#[test]
fn top_level_image_thresholds_keep_qpdf_unsigned_prefix_semantics() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        directory.path().join("input.pdf"),
    )
    .unwrap();

    for option in ["--oi-min-width=1,", "--oi-min-height=,1"] {
        let args = [option, "--check", "input.pdf"];
        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .current_dir(directory.path())
            .args(args)
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .current_dir(directory.path())
            .env("FLPDF_PROGNAME", "qpdf")
            .args(args)
            .output()
            .unwrap();

        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "status differs for {option}: qpdf={qpdf:?}, flpdf={flpdf:?}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout, "stdout differs for {option}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "stderr differs for {option}");
    }
}

#[test]
fn job_json_cli_parameter_events_reuse_the_prepared_job_state() {
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
        directory.path().join("qpdf-job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"qpdf-output.pdf","staticId":""}"#,
    )
    .unwrap();
    fs::write(
        directory.path().join("flpdf-job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"flpdf-output.pdf","staticId":""}"#,
    )
    .unwrap();

    let common = [
        "--compression-level=1",
        "--ii-min-bytes=1",
        "--keep-files-open-threshold=1",
        "--oi-min-area=1",
        "--oi-min-height=1",
        "--oi-min-width=1",
        "--split-pages=0",
    ];
    let mut qpdf_args = common.to_vec();
    qpdf_args.push("--job-json-file=qpdf-job.json");
    let mut flpdf_args = common.to_vec();
    flpdf_args.push("--job-json-file=flpdf-job.json");

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .args(&qpdf_args)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .args(&flpdf_args)
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert_eq!(
        fs::read(directory.path().join("flpdf-output.pdf")).unwrap(),
        fs::read(directory.path().join("qpdf-output.pdf")).unwrap()
    );
}

#[test]
fn job_json_file_preserves_input_encryption_when_compression_is_disabled() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    fs::copy(&fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("qpdf-job.json"),
        br#"{"inputFile":"input.pdf","password":"user-v4-aes","outputFile":"qpdf-output.pdf","staticId":"","staticAesIv":"","compressStreams":"n"}"#,
    )
    .unwrap();
    fs::write(
        directory.path().join("flpdf-job.json"),
        br#"{"inputFile":"input.pdf","password":"user-v4-aes","outputFile":"flpdf-output.pdf","staticId":"","staticAesIv":"","compressStreams":"n"}"#,
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=qpdf-job.json")
        .output()
        .unwrap();
    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");

    // qpdf's `compressStreams` setter changes only the writer compression
    // switch (`QPDFJob_config.cc:128-132`). The writer must therefore retain
    // source encryption unless an explicit decode/QDF/decrypt setting disables
    // preservation (`QPDFJob.cc:2865-2878`, `QPDFWriter.cc:2090-2101`).
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=flpdf-job.json")
        .output()
        .unwrap();
    assert!(flpdf.status.success(), "flpdf job JSON failed: {flpdf:?}");

    let qpdf_encryption = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .args(["--password=user-v4-aes", "--show-encryption"])
        .arg("qpdf-output.pdf")
        .output()
        .unwrap();
    let flpdf_encryption = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .args(["--password=user-v4-aes", "--show-encryption"])
        .arg("flpdf-output.pdf")
        .output()
        .unwrap();

    assert!(
        qpdf_encryption.status.success(),
        "qpdf output was not readable: {qpdf_encryption:?}"
    );
    assert!(
        flpdf_encryption.status.success(),
        "flpdf output was not readable: {flpdf_encryption:?}"
    );
    assert_eq!(flpdf_encryption.stdout, qpdf_encryption.stdout);
    assert_eq!(flpdf_encryption.stderr, qpdf_encryption.stderr);
}

#[test]
fn job_json_file_accepts_literal_high_bit_password_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    let mut json = br#"{"inputFile":"input.pdf","outputFile":"output.pdf","password":""}"#.to_vec();
    json.insert(json.len() - 2, 0x80);
    fs::write(directory.path().join("job.json"), json).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .code(0)
        .stdout("");

    assert!(directory.path().join("output.pdf").is_file());
}

/// A syntactically invalid job-json file fails during
/// `initialize_from_json_partial`, which carries a non-empty
/// `CliExitError::message` (unlike every other production `CliExitError`
/// site, which passes an empty message because the diagnostics were already
/// printed). This pins that the message-printing branch in `main`'s error
/// handler is reachable and formatted as documented.
#[test]
fn job_json_file_with_malformed_json_prints_error_and_exits_2() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("job.json"), b"{ not valid json").unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error with job-json file job.json:"),
        "stderr must carry the job-json error prefix, got:\n{stderr}"
    );
    assert!(
        stderr.contains("--job-json-help"),
        "stderr must point at --job-json-help, got:\n{stderr}"
    );
}

#[test]
fn job_json_file_missing_reports_job_json_context_and_usage() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.json");
    let argument = format!("--job-json-file={}", missing.display());

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg(&argument)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .arg(&argument)
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[cfg(target_os = "linux")]
#[test]
fn job_json_file_missing_preserves_non_utf8_path_bytes() {
    if !qpdf_available() {
        return;
    }

    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let directory = tempfile::tempdir().unwrap();
    let missing = directory
        .path()
        .join(OsString::from_vec(b"missing-\xff\xfe.json".to_vec()));
    let mut argument = b"--job-json-file=".to_vec();
    argument.extend_from_slice(missing.as_os_str().as_bytes());
    let argument = OsString::from_vec(argument);

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg(&argument)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .arg(&argument)
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(
        qpdf.stderr
            .windows(missing.as_os_str().as_bytes().len())
            .any(|window| window == missing.as_os_str().as_bytes()),
        "qpdf fixture must retain the raw missing path: {:?}",
        qpdf.stderr
    );
    assert!(
        !flpdf
            .stderr
            .windows(3)
            .any(|window| window == b"\xef\xbf\xbd"),
        "flpdf must not replace the raw path with U+FFFD: {:?}",
        flpdf.stderr
    );
}

#[cfg(target_os = "linux")]
#[test]
fn job_json_file_directory_keeps_the_portable_flpdf_diagnostic() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let argument = format!("--job-json-file={}", directory.path().display());

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg(&argument)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .arg(&argument)
        .output()
        .unwrap();

    assert_eq!(qpdf.status.code(), Some(2));
    assert_eq!(flpdf.status.code(), Some(2));
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_ne!(flpdf.stderr, qpdf.stderr);
    assert!(
        qpdf.stderr
            .windows(b"basic_string::_M_create".len())
            .any(|window| window == b"basic_string::_M_create"),
        "qpdf's directory diagnostic should expose the libstdc++ artifact: {:?}",
        qpdf.stderr
    );
    let flpdf_stderr = String::from_utf8(flpdf.stderr).unwrap();
    assert!(
        flpdf_stderr.contains(&format!(
            "error with job-json file {}: open {}: Is a directory",
            directory.path().display(),
            directory.path().display()
        )),
        "flpdf should retain its portable directory diagnostic: {flpdf_stderr}"
    );
}

#[cfg(target_os = "linux")]
fn run_job_json_with_single_fifo_read(
    program: &std::path::Path,
    fixture: &std::path::Path,
) -> (std::process::Output, bool) {
    use std::process::Stdio;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    let directory = tempfile::tempdir().unwrap();
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","passwordFile":"password.fifo","outputFile":"output.pdf","staticId":"","decrypt":""}"#,
    )
    .unwrap();
    let fifo = directory.path().join("password.fifo");
    assert!(ProcessCommand::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());

    let mut writer = ProcessCommand::new("sh")
        .args([
            "-c",
            "printf 'user-v4-aes\\n' > \"$1\"",
            "password-fifo-writer",
        ])
        .arg(fifo.to_str().unwrap())
        .spawn()
        .unwrap();
    let mut child = ProcessCommand::new(program)
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let timed_out = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            break true;
        }
        sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();

    let writer_deadline = Instant::now() + Duration::from_secs(2);
    let writer_completed = loop {
        if let Some(status) = writer.try_wait().unwrap() {
            break status.success();
        }
        if Instant::now() >= writer_deadline {
            let _ = writer.kill();
            let _ = writer.wait();
            break false;
        }
        sleep(Duration::from_millis(10));
    };
    assert!(
        writer_completed,
        "the FIFO writer must complete one password read"
    );
    (output, timed_out)
}

#[cfg(target_os = "linux")]
#[test]
fn job_json_password_file_is_read_once_like_qpdf() {
    if !qpdf_available() {
        return;
    }

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    let (qpdf, qpdf_timed_out) =
        run_job_json_with_single_fifo_read(std::path::Path::new("/usr/bin/qpdf"), &fixture);
    assert!(!qpdf_timed_out, "qpdf must not block on a second FIFO read");
    assert_eq!(qpdf.status.code(), Some(0), "qpdf failed: {qpdf:?}");

    let flpdf_program = PathBuf::from(assert_cmd::cargo::cargo_bin!("flpdf"));
    let (flpdf, flpdf_timed_out) = run_job_json_with_single_fifo_read(&flpdf_program, &fixture);
    assert!(
        !flpdf_timed_out,
        "flpdf must not block after consuming one password FIFO value; stderr={:?}",
        flpdf.stderr
    );
    assert_eq!(flpdf.status.code(), Some(0), "flpdf failed: {flpdf:?}");
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
fn job_json_file_show_object_normalization_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/null-length-framing-matrix.pdf");
    fs::copy(&fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","showObject":"6","filteredStreamData":"","normalizeContent":"y"}"#,
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
        .env("FLPDF_PROGNAME", "qpdf")
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
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

/// Every `--add-attachment FILE ... --` group on argv must be applied when
/// combined with `--job-json-file`, not just the first one. The
/// `--job-json-file` route feeds the full raw argv straight into qpdf's own
/// argv grammar (`QPDFJob::initializeFromArgv`), which accumulates every
/// `--add-attachment` group it sees; flpdf-cli's own preprocessing must
/// preserve every group for that route to see them all.
#[test]
fn job_json_file_applies_every_add_attachment_group() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/two-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(directory.path().join("a1.txt"), b"attachment one").unwrap();
    fs::write(directory.path().join("a2.txt"), b"attachment two").unwrap();
    fs::write(
        directory.path().join("job-qpdf.json"),
        br#"{"inputFile":"input.pdf","outputFile":"qpdf-out.pdf"}"#,
    )
    .unwrap();
    fs::write(
        directory.path().join("job-flpdf.json"),
        br#"{"inputFile":"input.pdf","outputFile":"flpdf-out.pdf"}"#,
    )
    .unwrap();

    let extra_args = |job_json: &str| {
        vec![
            format!("--job-json-file={job_json}"),
            "--static-id".to_owned(),
            "--add-attachment".to_owned(),
            "a1.txt".to_owned(),
            "--key=k1".to_owned(),
            "--creationdate=D:20240101000000Z".to_owned(),
            "--moddate=D:20240101000000Z".to_owned(),
            "--".to_owned(),
            "--add-attachment".to_owned(),
            "a2.txt".to_owned(),
            "--key=k2".to_owned(),
            "--creationdate=D:20240101000000Z".to_owned(),
            "--moddate=D:20240101000000Z".to_owned(),
            "--".to_owned(),
        ]
    };

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .args(extra_args("job-qpdf.json"))
        .output()
        .unwrap();
    assert!(qpdf.status.success(), "qpdf job JSON failed: {qpdf:?}");

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .args(extra_args("job-flpdf.json"))
        .output()
        .unwrap();
    assert_eq!(
        flpdf.status.code(),
        Some(0),
        "flpdf job JSON failed: stdout={} stderr={}",
        String::from_utf8_lossy(&flpdf.stdout),
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);

    // Compare `--list-attachments` text rather than raw output bytes: the
    // default Pure-Rust DEFLATE backend (miniz_oxide) does not produce
    // byte-identical compressed stream data to qpdf's zlib (documented
    // exception in this repo's CLAUDE.md), so a full-file byte comparison
    // would need the `qpdf-zlib-compat` feature. The attachment listing text
    // itself does not depend on stream compression and is the qpdf-parity
    // surface this regression test is about: both `--add-attachment` groups
    // being applied, not just the first.
    let qpdf_listing = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .args(["--list-attachments", "qpdf-out.pdf"])
        .output()
        .unwrap();
    let flpdf_listing = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .args(["--list-attachments", "flpdf-out.pdf"])
        .output()
        .unwrap();
    assert!(qpdf_listing.status.success());
    assert_eq!(flpdf_listing.status.code(), Some(0));
    assert_eq!(flpdf_listing.stdout, qpdf_listing.stdout);
    let listing = String::from_utf8_lossy(&flpdf_listing.stdout);
    assert!(listing.contains("k1"), "listing must contain k1: {listing}");
    assert!(listing.contains("k2"), "listing must contain k2: {listing}");
}

#[test]
fn job_json_file_with_argfile_does_not_re_expand_a_literal_at_token() {
    // Regression test for flpdf-ip497: qpdf expands `@file` exactly once,
    // via a single `QPDFArgParser::handleArgFileArguments` call
    // (`QPDFArgParser.cc:437`). flpdf-cli's own argv pre-scan
    // (`arg_parser::expand_arg_files`) must perform that one expansion, and
    // the `--job-json-file` preflight (`preflight_qpdf_cli_events`) must not
    // expand a second time. An `@marker` token that survives the first
    // expansion (because it came from inside an already-expanded file) has
    // to stay a literal argument in both qpdf and flpdf, not become the
    // `--linearize` option `marker` happens to contain.
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(directory.path().join("marker"), b"--linearize\n").unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf"}"#,
    )
    .unwrap();
    fs::write(
        directory.path().join("outer"),
        b"--job-json-file=job.json\n@marker\n",
    )
    .unwrap();

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("@outer")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .arg("@outer")
        .output()
        .unwrap();

    assert_eq!(qpdf.status.code(), Some(2), "qpdf: {qpdf:?}");
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "flpdf: {flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(!directory.path().join("out.pdf").exists());
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
fn job_json_plaintext_copy_encryption_is_a_noop_and_disables_primary_preservation() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("encrypted-input.pdf");
    let donor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let encrypted_fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    let output = directory.path().join("output.pdf");
    fs::copy(encrypted_fixture, &input).unwrap();
    fs::write(
        directory.path().join("job.json"),
        format!(
            r#"{{"inputFile":"{}","password":"user-v4-aes","outputFile":"{}","copyEncryption":"{}","staticId":""}}"#,
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

    let show = ProcessCommand::new("/usr/bin/qpdf")
        .arg("--show-encryption")
        .arg(&output)
        .output()
        .unwrap();
    assert!(show.status.success(), "qpdf must inspect output: {show:?}");
    assert_eq!(
        String::from_utf8_lossy(&show.stdout),
        format!("File is not encrypted{EOL}")
    );

    let check = ProcessCommand::new("/usr/bin/qpdf")
        .arg("--check")
        .arg(&output)
        .output()
        .unwrap();
    assert!(check.status.success(), "qpdf must check output: {check:?}");
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
fn job_json_file_verbose_auto_password_conversion_matches_qpdf() {
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
        r#"{"inputFile":"input.pdf","outputFile":"output.pdf","verbose":"","allowWeakCrypto":"","passwordMode":"auto","encrypt":{"userPassword":"café","ownerPassword":"owner","128bit":{}}}"#,
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
    let qpdf_stdout = String::from_utf8_lossy(&qpdf.stdout).replace("qpdf:", "flpdf:");
    assert_eq!(flpdf.stdout, qpdf_stdout.as_bytes());
    assert_eq!(flpdf.stderr, qpdf.stderr);
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
fn job_json_file_empty_encryption_status_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    for option in ["isEncrypted", "requiresPassword"] {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("job.json"),
            format!(r#"{{"empty":"","{option}":""}}"#),
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

        assert_eq!(qpdf.status.code(), Some(2), "qpdf probe: {qpdf:?}");
        assert_eq!(flpdf.status.code(), Some(2), "flpdf probe: {flpdf:?}");
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
    assert!(flpdf.stdout.ends_with(format!("0{EOL}").as_bytes()));
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
    assert_eq!(flpdf.stdout, format!("0{EOL}").into_bytes());
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
    page.try_is_scalar().unwrap();
    let contents = page.try_get_key(b"/Contents").unwrap();
    contents.try_is_scalar().unwrap();
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
    page.try_is_scalar().unwrap();
    assert!(
        !page.try_has_key(b"/Rotate").unwrap(),
        "flattenRotation must remove /Rotate"
    );

    let media_box = page.try_get_key(b"/MediaBox").unwrap();
    media_box.try_is_scalar().unwrap();
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
    acroform.try_is_scalar().unwrap();
    let need_appearances = acroform.try_get_key(b"/NeedAppearances").unwrap();
    need_appearances.try_is_scalar().unwrap();
    assert_ne!(
        need_appearances.as_boolean(),
        Some(true),
        "generateAppearances must clear qpdf's NeedAppearances marker"
    );

    let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
    let page = pdf.get_object_handle(page_ref);
    page.try_is_scalar().unwrap();
    let annots = page.try_get_key(b"/Annots").unwrap();
    annots.try_is_scalar().unwrap();
    let widget = annots.as_array().unwrap()[0].clone();
    widget.try_is_scalar().unwrap();
    let appearance = widget.try_get_key(b"/AP").unwrap();
    appearance.try_is_scalar().unwrap();
    let normal = appearance.try_get_key(b"/N").unwrap();
    normal.try_is_scalar().unwrap();
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
            "input file and output file are the same; use --replace-input to intentionally overwrite the input file",
        )));

    assert_eq!(fs::read(&input).unwrap(), before);
}

/// `checkConfiguration` assigns `-` as the JSON destination when no output
/// file was given (`libqpdf/QPDFJob.cc:582-586`) and then compares that name
/// with the input through `QUtil::same_file` (`:627-631`). The comparison is
/// an ordinary `stat` of both names (`libqpdf/QUtil.cc:598-607`), so a working
/// directory that really does contain a file called `-` makes the implicit
/// destination alias an input of the same name, and the job is rejected before
/// anything is written.
#[test]
fn job_json_implicit_json_destination_rejects_an_input_named_dash() {
    // Each binary gets its own working directory holding its own `-`, so
    // neither invocation can observe or disturb what the other left behind if
    // a future change moves the rejection later in the pipeline.
    fn case_directory() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
        fs::copy(fixture, directory.path().join("-")).unwrap();
        fs::write(
            directory.path().join("json.json"),
            br#"{"inputFile":"-","json":"2"}"#,
        )
        .unwrap();
        directory
    }

    let actual_directory = case_directory();
    let assertion = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(actual_directory.path())
        .arg("--job-json-file=json.json")
        .assert()
        .code(2);
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains(
            "input file and output file are the same; use --replace-input to intentionally \
             overwrite the input file"
        ),
        "unexpected diagnostic: {stderr:?}"
    );

    if !qpdf_available() {
        return;
    }
    let expected_directory = case_directory();
    let expected = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(expected_directory.path())
        .arg("--job-json-file=json.json")
        .output()
        .unwrap();
    assert_eq!(
        expected.status.code(),
        Some(2),
        "qpdf must reject the aliased implicit JSON destination too"
    );
    assert!(
        String::from_utf8_lossy(&expected.stderr)
            .contains("input file and output file are the same;"),
        "qpdf no longer reports the pinned diagnostic"
    );
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
        !stdout.contains("wrote file split.pdf"),
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
    acroform.try_is_scalar().unwrap();
    let fields = acroform.try_get_key(b"/Fields").unwrap();
    fields.try_is_scalar().unwrap();
    assert!(fields.as_array().is_some_and(|items| items.is_empty()));
    let sig_flags = acroform.try_get_key(b"/SigFlags").unwrap();
    sig_flags.try_is_scalar().unwrap();
    assert_eq!(sig_flags.as_integer(), Some(0));
}

fn page_rotations(bytes: &[u8]) -> Vec<Option<i64>> {
    let mut pdf = Pdf::open(Cursor::new(bytes.to_vec())).unwrap();
    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    pages
        .into_iter()
        .map(|page_ref| {
            let page = pdf.get_object_handle(page_ref);
            page.try_is_scalar().unwrap();
            let rotate = page.try_get_key(b"/Rotate").unwrap();
            rotate.try_is_scalar().unwrap();
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

/// qpdf's JSON handlers for these two are bare setters -- `noWarn`
/// (`auto_job_json_init.hh:287-289`) and `warningExit0` (`:469`) can only turn
/// the flag on, never clear it. The CLI must therefore not push its own
/// `false` default over a value the job JSON asked for.
#[test]
fn job_json_warning_exit_zero_survives_the_cli_default() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/chained-indirect-contents.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","staticId":"","warningExit0":""}"#,
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
        .env("FLPDF_PROGNAME", "qpdf")
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    // The input warns, so without warningExit0 this would be exit 3.
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains("WARNING"),
        "fixture must still warn for this test to mean anything: {qpdf:?}"
    );
    assert_eq!(qpdf.status.code(), Some(0), "qpdf fixture: {qpdf:?}");
    assert_eq!(flpdf.status.code(), Some(0));
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn job_json_no_warn_survives_the_cli_default() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/chained-indirect-contents.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","staticId":"","noWarn":""}"#,
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
        .env("FLPDF_PROGNAME", "qpdf")
        .arg("--job-json-file=job.json")
        .output()
        .unwrap();

    assert!(
        !String::from_utf8_lossy(&qpdf.stderr).contains("WARNING"),
        "qpdf must suppress the warnings for this test to mean anything: {qpdf:?}"
    );
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

/// A CLI-only flag inside a named segment belongs to that segment's qpdf
/// sub-parser, not to the main table.
///
/// `--repair` has no qpdf main-table entry, so the preflight drops it before
/// handing argv to `QPDFJob::initialize_from_raw_argv`. Dropping it by byte
/// comparison alone would also silence it inside an `--add-attachment ... --`
/// group, where qpdf rejects it. The job-JSON route never reparses the
/// attachment segments, so that error would be lost for good.
#[test]
fn repair_inside_an_attachment_segment_is_rejected_like_qpdf() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("payload"), b"payload").unwrap();
    std::fs::write(directory.path().join("job.json"), b"{}").unwrap();

    let args = [
        "--empty",
        "--job-json-file=job.json",
        "out.pdf",
        "--add-attachment",
        "payload",
        "--repair",
        "--",
    ];
    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .args(args)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .unwrap();

    assert_eq!(
        qpdf.status.code(),
        flpdf.status.code(),
        "exit code mismatch"
    );
    let expected = "unrecognized argument --repair (attachment options must be terminated with --)";
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains(expected),
        "qpdf stderr: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    assert!(
        String::from_utf8_lossy(&flpdf.stderr).contains(expected),
        "flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert!(
        !directory.path().join("out.pdf").exists(),
        "a rejected invocation must not write output"
    );
}

/// A top-level `--repair` still reaches the reader, since the preflight only
/// drops it while scanning the main table.
#[test]
fn top_level_repair_still_runs_with_a_job_json_file() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/three-page.pdf"
        ),
        directory.path().join("input.pdf"),
    )
    .unwrap();
    std::fs::write(directory.path().join("job.json"), b"{}").unwrap();

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .args([
            "--repair",
            "--job-json-file=job.json",
            "input.pdf",
            "out.pdf",
        ])
        .output()
        .unwrap();

    assert_eq!(
        flpdf.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert!(directory.path().join("out.pdf").exists());
}

/// qpdf's `QPDFJob::Config::passwordFile` reads the file during argument/job
/// configuration parsing (`QPDFJob_config.cc:661-679`, itself calling
/// `QUtil::read_lines_from_file` -> `QUtil::safe_fopen`), so a missing
/// `--password-file` renders as a `QPDFUsage` failure -- the portable
/// `safe_fopen` wording ("open <path>: <strerror>") plus the CLI's usage
/// "For help:" block -- not a plain runtime error. flpdf-dgei4 found this
/// diverging on two counts: the operation word ("read password file" instead
/// of qpdf's "open") and a leaked Rust `(os error N)` suffix that `strerror`
/// itself never produces. Cover both the standalone and job-json-routed
/// paths, since they go through independent flpdf-cli/flpdf-library code.
#[test]
fn password_file_missing_matches_qpdf_standalone() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing-password.txt");
    let fixture = directory.path().join("minimal.pdf");
    fs::write(
        &fixture,
        fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/minimal.pdf"),
        )
        .unwrap(),
    )
    .unwrap();
    let argument = format!("--password-file={}", missing.display());

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg(&argument)
        .arg("--check")
        .arg(&fixture)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .arg(&argument)
        .arg("--check")
        .arg(&fixture)
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn password_file_missing_matches_qpdf_via_job_json() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing-password.txt");
    let fixture = directory.path().join("minimal.pdf");
    fs::write(
        &fixture,
        fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/minimal.pdf"),
        )
        .unwrap(),
    )
    .unwrap();
    let job_json = directory.path().join("job.json");
    fs::write(&job_json, b"{}").unwrap();
    let argument = format!("--password-file={}", missing.display());
    let job_json_argument = format!("--job-json-file={}", job_json.display());

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg(&argument)
        .arg(&job_json_argument)
        .arg("--check")
        .arg(&fixture)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .arg(&argument)
        .arg(&job_json_argument)
        .arg("--check")
        .arg(&fixture)
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

/// A directory `--password-file` is a *read*-time failure, not an *open*-time
/// one: qpdf's `QUtil::read_lines_from_file(char const*)` opens with
/// `safe_fopen` (which succeeds on a directory on Unix) and only reads
/// afterward, so the failure surfaces on the first `fread` inside
/// `read_char_from_FILE`, which reports a fixed, path-less message
/// (`"failure reading character from file"`, `libqpdf/QUtil.cc:1217-1228`),
/// not the `open <path>: <strerror>` wording used for a missing file
/// (flpdf-dgei4, tests above). flpdf-lw2h0 found all three of the same
/// independent paths flpdf-dgei4 fixed for the missing-file case reusing
/// `std::fs::read`'s single open+read call for this path too, blaming the
/// read-stage failure on the open-stage wording.
#[test]
fn password_file_is_directory_matches_qpdf_standalone() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let password_dir = directory.path().join("password-dir");
    fs::create_dir(&password_dir).unwrap();
    let fixture = directory.path().join("minimal.pdf");
    fs::write(
        &fixture,
        fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/minimal.pdf"),
        )
        .unwrap(),
    )
    .unwrap();
    let argument = format!("--password-file={}", password_dir.display());

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg(&argument)
        .arg("--check")
        .arg(&fixture)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .arg(&argument)
        .arg("--check")
        .arg(&fixture)
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn password_file_is_directory_matches_qpdf_via_job_json_argument() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let password_dir = directory.path().join("password-dir");
    fs::create_dir(&password_dir).unwrap();
    let fixture = directory.path().join("minimal.pdf");
    fs::write(
        &fixture,
        fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/minimal.pdf"),
        )
        .unwrap(),
    )
    .unwrap();
    let job_json = directory.path().join("job.json");
    fs::write(&job_json, b"{}").unwrap();
    let argument = format!("--password-file={}", password_dir.display());
    let job_json_argument = format!("--job-json-file={}", job_json.display());

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg(&argument)
        .arg(&job_json_argument)
        .arg("--check")
        .arg(&fixture)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .arg(&argument)
        .arg(&job_json_argument)
        .arg("--check")
        .arg(&fixture)
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

/// The job-json document's own `passwordFile` member is a third,
/// independent read site (`crates/flpdf/src/job/lifecycle.rs`, distinct from
/// both the standalone CLI flag and the CLI-flag-combined-with-job-json
/// route above), so it needs its own directory-read coverage.
#[test]
fn password_file_is_directory_matches_qpdf_via_job_json_member() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let password_dir = directory.path().join("password-dir");
    fs::create_dir(&password_dir).unwrap();
    let fixture = directory.path().join("minimal.pdf");
    fs::write(
        &fixture,
        fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/minimal.pdf"),
        )
        .unwrap(),
    )
    .unwrap();
    let job_json = directory.path().join("job.json");
    fs::write(
        &job_json,
        format!(
            r#"{{"inputFile":{:?},"passwordFile":{:?},"check":""}}"#,
            fixture.to_string_lossy(),
            password_dir.to_string_lossy()
        ),
    )
    .unwrap();
    let job_json_argument = format!("--job-json-file={}", job_json.display());

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg(&job_json_argument)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .arg(&job_json_argument)
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}
