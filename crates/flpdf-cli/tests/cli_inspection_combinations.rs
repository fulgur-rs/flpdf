//! qpdf 11.9.0 inspection combinations must share one ordered job route.

use std::path::Path;
use std::process::{Command, Output};
use std::{fs, path::PathBuf};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";
const FXO_RED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/fxo-red.pdf"
);
const ATTACHMENT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/attachment-two-page.pdf"
);
const LINEARIZED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/linearized-one-page.pdf"
);

fn qpdf_available() -> bool {
    let output = match Command::new("qpdf").arg("--version").output() {
        Ok(output) => output,
        Err(error) => {
            if std::env::var_os("CI").is_some() {
                panic!("qpdf 11.9.0 is required on CI: {error}");
            }
            eprintln!("skipping qpdf combination comparison: {error}");
            return false;
        }
    };
    let version = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && version.lines().next() == Some(EXPECTED_QPDF_VERSION) {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!(
            "qpdf 11.9.0 is required on CI; found {:?}",
            version.lines().next()
        );
    }
    eprintln!(
        "skipping qpdf combination comparison; found {:?}",
        version.lines().next()
    );
    false
}

fn run_qpdf(args: &[&str], input: &str) -> Output {
    Command::new("qpdf")
        .args(args)
        .arg(input)
        .output()
        .expect("qpdf should start")
}

fn run_flpdf(args: &[&str], input: &str) -> Output {
    Command::new(assert_cmd::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .arg(input)
        .output()
        .expect("flpdf should start")
}

fn run_qpdf_exact(args: &[String]) -> Output {
    Command::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should start")
}

fn run_flpdf_exact(args: &[String]) -> Output {
    Command::new(assert_cmd::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf should start")
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

fn normalize_stdout(args: &[&str], bytes: &[u8]) -> Vec<u8> {
    if !cfg!(windows) {
        return bytes.to_vec();
    }
    if args.iter().any(|arg| arg.starts_with("--show-attachment=")) {
        // qpdf's info line uses text-mode stdout on Windows, while the
        // extracted attachment payload is a binary save stream. Normalize
        // only the info prefix so payload CRLF bytes remain authoritative.
        let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') else {
            return bytes.to_vec();
        };
        let mut normalized = normalize_text_newlines(&bytes[..=newline]);
        normalized.extend_from_slice(&bytes[newline + 1..]);
        normalized
    } else {
        normalize_text_newlines(bytes)
    }
}

fn one_page_with_image_pdf() -> Vec<u8> {
    let objects = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n".as_slice(),
        b"4 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 0 >>\nstream\n\nendstream\nendobj\n".as_slice(),
        b"5 0 obj\n<< /Length 12 >>\nstream\nq\n/Im1 Do\nQ\nendstream\nendobj\n".as_slice(),
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

fn assert_matches_qpdf_path(args: &[&str], input: &Path) {
    let qpdf = Command::new("qpdf")
        .args(args)
        .arg(input)
        .output()
        .expect("qpdf should start");
    let flpdf = Command::new(assert_cmd::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .arg(input)
        .output()
        .expect("flpdf should start");
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "{args:?} status");
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "{args:?} stdout"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "{args:?} stderr"
    );
}

fn assert_matches_qpdf(args: &[&str], input: &str) {
    assert!(Path::new(input).exists(), "fixture must exist: {input}");
    let qpdf = run_qpdf(args, input);
    let flpdf = run_flpdf(args, input);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "exit code mismatch for {args:?}"
    );
    assert_eq!(
        normalize_stdout(args, &flpdf.stdout),
        normalize_stdout(args, &qpdf.stdout),
        "stdout mismatch for {args:?}"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "stderr mismatch for {args:?}"
    );
}

fn assert_matches_qpdf_exact(args: &[String]) {
    let qpdf = run_qpdf_exact(args);
    let flpdf = run_flpdf_exact(args);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "exit code mismatch for {args:?}"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "stdout mismatch for {args:?}"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "stderr mismatch for {args:?}"
    );
}

#[test]
fn check_linearization_accepts_and_runs_all_qpdf_compatible_combinations() {
    if !qpdf_available() {
        return;
    }

    let cases: &[(&[&str], &str)] = &[
        (&["--check-linearization", "--list-attachments"], ATTACHMENT),
        (
            &["--check-linearization", "--show-attachment=attachment.txt"],
            ATTACHMENT,
        ),
        (&["--check-linearization", "--show-xref"], FXO_RED),
        (
            &["--check-linearization", "--show-linearization"],
            LINEARIZED,
        ),
        (&["--check-linearization", "--show-encryption"], FXO_RED),
        (&["--check-linearization", "--static-id"], FXO_RED),
        (&["--check-linearization", "--deterministic-id"], FXO_RED),
        (&["--check-linearization", "--static-aes-iv"], FXO_RED),
        (
            &["--check-linearization", "--preserve-unreferenced"],
            FXO_RED,
        ),
        (&["--check-linearization", "--decrypt"], FXO_RED),
        (&["--check-linearization", "--qdf"], FXO_RED),
        (&["--check-linearization", "--coalesce-contents"], FXO_RED),
        (&["--check-linearization", "--remove-restrictions"], FXO_RED),
        (&["--check-linearization", "--linearize"], FXO_RED),
        (&["--check-linearization", "--collate"], FXO_RED),
    ];

    for (args, input) in cases {
        assert_matches_qpdf(args, input);
    }
}

#[test]
fn inspection_flags_run_in_qpdf_do_inspection_order() {
    if !qpdf_available() {
        return;
    }

    for args in [
        &["--check", "--show-npages"][..],
        &["--check", "--show-pages"][..],
        &["--check", "--list-attachments"][..],
        &["--check", "--show-encryption"][..],
        &["--show-npages", "--show-xref"][..],
        &["--show-pages", "--show-xref"][..],
        &["--show-npages", "--show-pages"][..],
        &["--check-linearization", "--show-object=19"][..],
        &[
            "--check-linearization",
            "--show-object=19",
            "--raw-stream-data",
        ][..],
        &[
            "--check-linearization",
            "--show-object=19",
            "--filtered-stream-data",
        ][..],
    ] {
        assert_matches_qpdf(
            args,
            if args.contains(&"--list-attachments") {
                ATTACHMENT
            } else {
                FXO_RED
            },
        );
    }
}

#[test]
fn page_selection_precedes_single_page_inspection() {
    if !qpdf_available() {
        return;
    }

    let minimal =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let three_page =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/three-page.pdf");
    let cases = [
        vec![
            "--empty".to_owned(),
            "--pages".to_owned(),
            minimal.display().to_string(),
            "--".to_owned(),
            "--show-pages".to_owned(),
        ],
        vec![
            three_page.display().to_string(),
            "--pages".to_owned(),
            ".".to_owned(),
            "2".to_owned(),
            "--".to_owned(),
            "--show-pages".to_owned(),
        ],
        vec![
            "--collate=0".to_owned(),
            three_page.display().to_string(),
            "--pages".to_owned(),
            ".".to_owned(),
            "1".to_owned(),
            ".".to_owned(),
            "2".to_owned(),
            "--".to_owned(),
            "--show-pages".to_owned(),
        ],
    ];

    for args in cases {
        assert_matches_qpdf_exact(&args);
    }
}

#[test]
fn combined_show_pages_preserves_qpdf_with_images_output() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("temporary fixture directory");
    let input = PathBuf::from(directory.path()).join("with-image.pdf");
    fs::write(&input, one_page_with_image_pdf()).expect("write image fixture");

    assert_matches_qpdf_path(&["--check", "--show-pages", "--with-images"], &input);
}
