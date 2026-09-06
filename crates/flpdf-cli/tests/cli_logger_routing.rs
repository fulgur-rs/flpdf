//! QPDFLogger routing coverage for qpdf-equivalent CLI output.

use assert_cmd::Command;
use std::path::Path;
use std::process::{Command as ProcessCommand, Output};

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;

const MINIMAL: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/minimal.pdf"
);
const MULTI_STREAM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/multi-stream-one-page.pdf"
);
const ONE_PAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/one-page.pdf"
);
const WARNING_PDF: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/test_driver/missing_startxref.pdf"
);
const ATTACHMENT_PDF: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/attachment-two-page.pdf"
);
const LARGE_LINEARIZED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/objstm-lin-outlines-80-200.pdf"
);
const EXPECTED_QPDF_VERSION: &str = "11.9.0";

fn flpdf() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("flpdf"))
}

fn qpdf_version_is_expected(stdout: &[u8]) -> bool {
    String::from_utf8_lossy(stdout)
        .lines()
        .next()
        .map(str::trim)
        == Some(&format!("qpdf version {EXPECTED_QPDF_VERSION}"))
}

fn qpdf_available() -> bool {
    let observation = match ProcessCommand::new("qpdf").arg("--version").output() {
        Ok(output) if output.status.success() && qpdf_version_is_expected(&output.stdout) => {
            return true;
        }
        Ok(output) => {
            let first_line = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("<empty stdout>")
                .to_owned();
            format!("found {first_line:?} (status {})", output.status)
        }
        Err(error) => format!("unable to run qpdf --version: {error}"),
    };

    if std::env::var_os("CI").is_some() {
        panic!(
            "qpdf {EXPECTED_QPDF_VERSION} is required for cli_logger_routing differential tests on CI; {observation}"
        );
    }
    eprintln!(
        "skipping logger routing differential: qpdf {EXPECTED_QPDF_VERSION} is required; {observation}"
    );
    false
}

fn run_qpdf(args: &[&str]) -> Output {
    ProcessCommand::new("qpdf").args(args).output().unwrap()
}

fn run_flpdf(args: &[&str]) -> Output {
    ProcessCommand::new(assert_cmd::cargo::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .unwrap()
}

#[cfg(unix)]
fn run_merged_check(program: impl AsRef<std::ffi::OsStr>, args: &[&str], input: &Path) -> Output {
    ProcessCommand::new("/bin/sh")
        .args([
            "-c",
            r#"program="$1"; shift; exec "$program" "$@" 2>&1"#,
            "qpdf-check",
        ])
        .arg(program)
        .args(args)
        .arg(input)
        .env("FLPDF_PROGNAME", "qpdf")
        .output()
        .unwrap()
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

fn assert_observables_equal(label: &str, qpdf: &Output, flpdf: &Output, text_output: bool) {
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "{label}: status");
    if cfg!(windows) && text_output {
        assert_eq!(
            normalize_text_newlines(&flpdf.stdout),
            normalize_text_newlines(&qpdf.stdout),
            "{label}: stdout"
        );
        assert_eq!(
            normalize_text_newlines(&flpdf.stderr),
            normalize_text_newlines(&qpdf.stderr),
            "{label}: stderr"
        );
    } else {
        assert_eq!(flpdf.stdout, qpdf.stdout, "{label}: stdout");
        assert_eq!(flpdf.stderr, qpdf.stderr, "{label}: stderr");
    }
}

#[test]
fn text_newline_normalization_only_collapses_crlf_pairs() {
    assert_eq!(
        normalize_text_newlines(b"first\r\nsecond\nthird\rfourth"),
        b"first\nsecond\nthird\rfourth"
    );
}

#[test]
fn qpdf_logger_oracle_version_gate_accepts_only_11_9_0() {
    assert!(qpdf_version_is_expected(
        b"qpdf version 11.9.0\nRun qpdf --copyright for details\n"
    ));
    assert!(!qpdf_version_is_expected(b"qpdf version 12.0.0\n"));
    assert!(!qpdf_version_is_expected(b"qpdf version 11.9.0-custom\n"));
}

#[test]
fn text_logger_uses_qpdf_platform_line_endings() {
    let version = flpdf().args(["--version"]).output().unwrap();
    assert!(version.status.success());
    assert_eq!(
        version.stdout,
        format!("qpdf version 11.9.0{EOL}Run qpdf --copyright to see copyright and license information.{EOL}")
        .into_bytes()
    );

    let check = flpdf()
        .args(["--repair", "--check", WARNING_PDF])
        .output()
        .unwrap();
    assert_eq!(check.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&check.stdout).contains(&format!("checking {WARNING_PDF}{EOL}"))
    );
    assert!(String::from_utf8_lossy(&check.stderr)
        .ends_with(&format!("flpdf: operation succeeded with warnings{}", EOL)));
}

#[test]
fn binary_json_uses_stdout_without_stderr() {
    let output = flpdf().args(["--json=2", MINIMAL]).output().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["version"], serde_json::json!(2));
}

#[test]
fn binary_raw_stream_preserves_exact_bytes() {
    let output = flpdf()
        .args(["show-stream", "4 0 R", MULTI_STREAM, "--raw-stream-data"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        [
            0x78, 0x9c, 0x2b, 0x54, 0x30, 0x54, 0x30, 0x00, 0x42, 0x08, 0x99, 0x9c, 0x0b, 0x00,
            0x1a, 0x69, 0x03, 0x44,
        ]
    );
}

#[test]
fn binary_pdf_dash_writes_stdout_without_creating_a_dash_file() {
    let directory = tempfile::tempdir().unwrap();
    let output = flpdf()
        .current_dir(directory.path())
        .args([MINIMAL, "-"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(!Path::new(directory.path()).join("-").exists());
}

#[test]
fn binary_linearized_pdf_dash_uses_the_same_save_route() {
    let output = flpdf()
        .args(["--linearize", ONE_PAGE, "-"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_linearized_pdf_dash_writes_pass1_independently() {
    let directory = tempfile::tempdir().unwrap();
    let pass1 = directory.path().join("pass1.pdf");
    let pass1_arg = format!("--linearize-pass1={}", pass1.display());
    let output = flpdf()
        .args(["--linearize", &pass1_arg, ONE_PAGE, "-"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stderr.is_empty());
    let pass1_bytes = std::fs::read(pass1).unwrap();
    assert!(pass1_bytes.starts_with(b"%PDF-"));
    assert_ne!(pass1_bytes, output.stdout);
    assert!(pass1_bytes
        .windows(b"% hint_offset=".len())
        .any(|window| window == b"% hint_offset="));
}

#[test]
fn binary_linearized_pass1_open_failure_names_path_before_final_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let pass1 = directory.path().join("missing-parent").join("pass1.pdf");
    let pass1_arg = format!("--linearize-pass1={}", pass1.display());
    let output = flpdf()
        .args(["--linearize", &pass1_arg, ONE_PAGE, "-"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "final PDF must not be emitted after pass-1 open failure"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(&format!("open {}: ", pass1.display())));
    assert!(stderr.contains("No such file") || stderr.contains("cannot find"));
}

#[cfg(unix)]
#[test]
fn qpdf_differential_matches_small_and_large_pass1_dev_full_boundaries() {
    if !Path::new("/dev/full").exists() || !qpdf_available() {
        eprintln!("skipping pass-1 /dev/full differential");
        return;
    }

    let small_args = ["--linearize", "--linearize-pass1=/dev/full", ONE_PAGE, "-"];
    let qpdf_small = run_qpdf(&small_args);
    let flpdf_small = run_flpdf(&small_args);
    assert_eq!(qpdf_small.status.code(), Some(0), "qpdf small status");
    assert_eq!(flpdf_small.status.code(), Some(0), "flpdf small status");
    assert!(qpdf_small.stdout.starts_with(b"%PDF-"));
    assert!(flpdf_small.stdout.starts_with(b"%PDF-"));
    assert!(qpdf_small.stderr.is_empty());
    assert!(flpdf_small.stderr.is_empty());

    let large_args = [
        "--linearize",
        "--linearize-pass1=/dev/full",
        LARGE_LINEARIZED,
        "-",
    ];
    let qpdf_large = run_qpdf(&large_args);
    let flpdf_large = run_flpdf(&large_args);
    assert_eq!(qpdf_large.status.code(), Some(2), "qpdf large status");
    assert_eq!(flpdf_large.status.code(), Some(2), "flpdf large status");
    assert!(qpdf_large.stdout.is_empty());
    assert!(flpdf_large.stdout.is_empty());
    let qpdf_stderr = String::from_utf8_lossy(&qpdf_large.stderr);
    let flpdf_stderr = String::from_utf8_lossy(&flpdf_large.stderr);
    assert!(qpdf_stderr.contains("linearization pass1: Pl_StdioFile::write"));
    assert!(flpdf_stderr.contains("write /dev/full:"));
    assert!(qpdf_stderr.contains("No space left on device"));
    assert!(flpdf_stderr.contains("No space left on device"));
}

#[cfg(unix)]
#[test]
fn qpdf_differential_matches_file_write_error_swallowing() {
    if !Path::new("/dev/full").exists() || !qpdf_available() {
        eprintln!("skipping final file-write /dev/full differential");
        return;
    }

    let plain_args = ["--static-id", ONE_PAGE, "/dev/full"];
    assert_observables_equal(
        "plain file output",
        &run_qpdf(&plain_args),
        &run_flpdf(&plain_args),
        true,
    );

    let page_operation_args = [
        "--progress",
        "--static-id",
        "--rotate=90:1",
        ONE_PAGE,
        "/dev/full",
    ];
    assert_observables_equal(
        "page-operation file output",
        &run_qpdf(&page_operation_args),
        &run_flpdf(&page_operation_args),
        true,
    );

    let page_extraction_args = [
        "--progress",
        "--static-id",
        ONE_PAGE,
        "--pages",
        ".",
        "1",
        "--",
        "/dev/full",
    ];
    assert_observables_equal(
        "page-extraction file output",
        &run_qpdf(&page_extraction_args),
        &run_flpdf(&page_extraction_args),
        true,
    );
}

#[test]
fn binary_qdf_dash_uses_the_same_save_route() {
    let output = flpdf().args(["qdf", MINIMAL, "-"]).output().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stdout.windows(5).any(|window| window == b"%QDF-"));
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_page_extraction_dash_uses_the_save_route_without_creating_a_dash_file() {
    let directory = tempfile::tempdir().unwrap();
    let output = flpdf()
        .current_dir(directory.path())
        .args([ONE_PAGE, "--pages", ".", "1", "--", "-"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stderr.is_empty());
    assert!(!directory.path().join("-").exists());
}

#[test]
fn binary_rotate_dash_uses_the_save_route_without_creating_a_dash_file() {
    let directory = tempfile::tempdir().unwrap();
    let output = flpdf()
        .current_dir(directory.path())
        .args([ONE_PAGE, "-", "--rotate=+90:1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stderr.is_empty());
    assert!(!directory.path().join("-").exists());
}

#[test]
fn split_pages_dash_is_rejected_before_output_is_created() {
    let directory = tempfile::tempdir().unwrap();
    let output = flpdf()
        .current_dir(directory.path())
        .args([ONE_PAGE, "-", "--split-pages=1"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("--split-pages may not be used when writing to standard output"));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn verbose_page_extraction_dash_keeps_pdf_and_info_on_separate_routes() {
    let output = flpdf()
        .args(["--verbose", ONE_PAGE, "--pages", ".", "1", "--", "-"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(!output
        .stdout
        .windows(b"flpdf:".len())
        .any(|window| window == b"flpdf:"));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("flpdf: selecting --keep-open-files=y")
    );
}

#[test]
fn text_rewrite_verbose_uses_info_route_for_file_output() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("out.pdf");
    let output = flpdf()
        .args(["--verbose", MINIMAL, output_path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("flpdf: wrote file {}{}", output_path.display(), EOL)
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn text_rewrite_verbose_does_not_announce_standard_output() {
    let output = flpdf().args(["--verbose", MINIMAL, "-"]).output().unwrap();

    assert!(output.status.success());
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stderr.is_empty());
}

#[test]
fn text_check_success_stays_on_info_route() {
    let output = flpdf().args(["--check", MINIMAL]).output().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with(&format!("checking {MINIMAL}{EOL}")));
    assert!(stdout.ends_with(&format!("errors that qpdf cannot detect{}", EOL)));
}

#[test]
fn qpdf_differential_matches_routed_output_matrix() {
    if !qpdf_available() {
        eprintln!("qpdf not available; skipping logger routing differential");
        return;
    }

    let cases: &[(&str, &[&str], &[&str], bool)] = &[
        (
            "clean check",
            &["--check", MINIMAL],
            &["--check", MINIMAL],
            true,
        ),
        (
            "warning check",
            &["--check", WARNING_PDF],
            &["--repair", "--check", WARNING_PDF],
            true,
        ),
        (
            "JSON stdout",
            &["--json=2", MINIMAL],
            &["--json=2", MINIMAL],
            false,
        ),
        (
            "raw stream",
            &["--show-object=4", "--raw-stream-data", MULTI_STREAM],
            &["show-stream", "4 0 R", MULTI_STREAM, "--raw-stream-data"],
            false,
        ),
        (
            "filtered stream",
            &["--show-object=4", "--filtered-stream-data", MULTI_STREAM],
            &["show-stream", "4 0 R", MULTI_STREAM],
            false,
        ),
        (
            "attachment",
            &["--show-attachment=attachment.txt", ATTACHMENT_PDF],
            &["--show-attachment=attachment.txt", ATTACHMENT_PDF],
            false,
        ),
    ];

    for (label, qpdf_args, flpdf_args, text_output) in cases {
        assert_observables_equal(
            label,
            &run_qpdf(qpdf_args),
            &run_flpdf(flpdf_args),
            *text_output,
        );
    }
}

#[cfg(unix)]
#[test]
fn qpdf_differential_preserves_open_warning_order_before_check_banner() {
    if !qpdf_available() {
        eprintln!("qpdf not available; skipping merged check-order differential");
        return;
    }

    let input = Path::new(WARNING_PDF);
    let qpdf = run_merged_check("qpdf", &["--check"], input);
    let flpdf = run_merged_check(
        assert_cmd::cargo::cargo_bin!("flpdf"),
        &["--repair", "--check"],
        input,
    );

    assert_eq!(qpdf.status.code(), Some(3), "qpdf status");
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "status");
    assert_eq!(flpdf.stdout, qpdf.stdout, "merged stdout/stderr output");
    assert!(
        qpdf.stderr.is_empty(),
        "qpdf shell stderr: {:?}",
        qpdf.stderr
    );
    assert!(
        flpdf.stderr.is_empty(),
        "flpdf shell stderr: {:?}",
        flpdf.stderr
    );

    let qpdf_no_warn = run_merged_check("qpdf", &["--no-warn", "--check"], input);
    let flpdf_no_warn = run_merged_check(
        assert_cmd::cargo::cargo_bin!("flpdf"),
        &["--no-warn", "--repair", "--check"],
        input,
    );
    assert_eq!(qpdf_no_warn.status.code(), Some(3), "qpdf --no-warn status");
    assert_eq!(
        flpdf_no_warn.status.code(),
        qpdf_no_warn.status.code(),
        "--no-warn status"
    );
    assert_eq!(
        flpdf_no_warn.stdout, qpdf_no_warn.stdout,
        "--no-warn merged stdout/stderr output"
    );
    assert!(
        qpdf_no_warn.stderr.is_empty(),
        "qpdf --no-warn shell stderr: {:?}",
        qpdf_no_warn.stderr
    );
    assert!(
        flpdf_no_warn.stderr.is_empty(),
        "flpdf --no-warn shell stderr: {:?}",
        flpdf_no_warn.stderr
    );
}

#[test]
fn qpdf_differential_matches_missing_input_open_error_on_all_input_routes() {
    if !qpdf_available() {
        eprintln!("qpdf not available; skipping logger routing differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let missing = directory.path().join("missing.pdf");
    let rewrite_output = directory.path().join("rewrite-output.pdf");
    let pages_output = directory.path().join("pages-output.pdf");
    let missing = missing.to_str().expect("UTF-8 temporary path");
    let rewrite_output = rewrite_output.to_str().expect("UTF-8 temporary path");
    let pages_output = pages_output.to_str().expect("UTF-8 temporary path");

    let cases = [
        ("plain rewrite", vec![missing, rewrite_output]),
        ("--check", vec!["--check", missing]),
        (
            "--empty --pages",
            vec!["--empty", "--pages", missing, "--", pages_output],
        ),
        (
            "--copy-attachments-from",
            vec![
                "--copy-attachments-from",
                missing,
                "--",
                ONE_PAGE,
                rewrite_output,
            ],
        ),
    ];
    for (label, args) in cases {
        let qpdf = run_qpdf(&args);
        let flpdf = run_flpdf(&args);
        assert_observables_equal(label, &qpdf, &flpdf, true);
    }

    let encrypted = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/encrypted/v4-aes-128-r4.pdf"
    );
    let qpdf = run_qpdf(&[
        ONE_PAGE,
        "--pages",
        encrypted,
        "--password=wrong",
        "1",
        "--",
        pages_output,
    ]);
    let flpdf = run_flpdf(&[
        ONE_PAGE,
        "--pages",
        encrypted,
        "--password=wrong",
        "1",
        "--",
        pages_output,
    ]);
    assert_observables_equal("secondary source authentication", &qpdf, &flpdf, true);
}

#[test]
fn qpdf_differential_matches_remaining_file_error_boundaries() {
    if !qpdf_available() {
        eprintln!("qpdf not available; skipping file error-boundary differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input_directory = directory.path().join("input-directory");
    std::fs::create_dir(&input_directory).expect("create directory input");
    let input_directory = input_directory.to_str().expect("UTF-8 temporary path");
    let output_path = directory.path().join("output.pdf");
    let output = output_path.to_str().expect("UTF-8 temporary path");
    let missing_parent_output_path = directory.path().join("missing-parent").join("output.pdf");
    let missing_parent_output = missing_parent_output_path
        .to_str()
        .expect("UTF-8 temporary path");
    let recovery_input = directory.path().join("bad.pdf");
    std::fs::write(&recovery_input, b"not a PDF\n").expect("write malformed input");
    let recovery_input = recovery_input.to_str().expect("UTF-8 temporary path");

    let missing_parent_json_path = directory.path().join("missing-parent").join("output.json");
    let missing_parent_json = missing_parent_json_path
        .to_str()
        .expect("UTF-8 temporary path");

    let cases = [
        ("directory input", vec![input_directory, output]),
        (
            "missing output parent",
            vec![MINIMAL, missing_parent_output],
        ),
        (
            "missing JSON output parent",
            vec!["--json=2", MINIMAL, "--json-output", missing_parent_json],
        ),
        ("recovery failure", vec![recovery_input, output]),
    ];
    for (label, args) in cases {
        let qpdf = run_qpdf(&args);
        let flpdf = run_flpdf(&args);
        assert_observables_equal(label, &qpdf, &flpdf, true);
    }

    assert!(!output_path.exists(), "failed input must not create output");
    assert!(
        !missing_parent_output_path.exists(),
        "failed output open must not create output"
    );
}

#[cfg(unix)]
#[test]
fn qpdf_differential_matches_permission_denied_output_open() {
    if !qpdf_available() {
        eprintln!("qpdf not available; skipping permission-denied differential");
        return;
    }

    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary directory");
    let readonly = directory.path().join("readonly");
    std::fs::create_dir(&readonly).expect("create read-only output directory");
    std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o555))
        .expect("make output directory read-only");
    let output_path = readonly.join("output.pdf");
    let output = output_path.to_str().expect("UTF-8 temporary path");

    let qpdf = run_qpdf(&[MINIMAL, output]);
    let flpdf = run_flpdf(&[MINIMAL, output]);
    if qpdf.status.code() != Some(2) {
        eprintln!(
            "skipping permission-denied differential: qpdf could write in the read-only directory"
        );
        return;
    }
    assert_eq!(
        flpdf.status.code(),
        Some(2),
        "flpdf must fail when qpdf observes permission denied: {:?}",
        flpdf.stderr
    );
    assert_observables_equal("permission-denied output", &qpdf, &flpdf, true);
    assert!(
        !output_path.exists(),
        "permission failure must not create output"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cli_accepts_a_non_utf8_input_path_without_panicking() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let directory = tempfile::tempdir().expect("temporary source directory");
    let input = directory
        .path()
        .join(std::ffi::OsString::from_vec(b"input-\xff.pdf".to_vec()));
    std::fs::copy(MINIMAL, &input).expect("copy fixture to non-UTF-8 path");

    let output = ProcessCommand::new(assert_cmd::cargo::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .arg("--check")
        .arg(&input)
        .output()
        .expect("run flpdf");

    assert_eq!(
        output.status.code(),
        Some(0),
        "non-UTF-8 argv must not panic: {:?}",
        output.stderr
    );
    let input_bytes = input.as_os_str().as_bytes();
    assert!(
        output
            .stdout
            .windows(input_bytes.len())
            .any(|window| window == input_bytes),
        "check output must preserve the raw input path: {:?}",
        output.stdout
    );
    assert!(
        !output
            .stdout
            .windows(3)
            .any(|window| window == b"\xef\xbf\xbd"),
        "check output must not replace the raw input path with U+FFFD: {:?}",
        output.stdout
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cli_reports_a_non_utf8_missing_path_without_panicking() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let directory = tempfile::tempdir().expect("temporary source directory");
    let input = directory
        .path()
        .join(std::ffi::OsString::from_vec(b"missing-\xff.pdf".to_vec()));

    let output = ProcessCommand::new(assert_cmd::cargo::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .arg("--check")
        .arg(&input)
        .output()
        .expect("run flpdf");

    assert_eq!(
        output.status.code(),
        Some(2),
        "missing input must be a CLI error, not a panic: {:?}",
        output.stderr
    );
    let input_bytes = input.as_os_str().as_bytes();
    assert!(
        output
            .stderr
            .windows(input_bytes.len())
            .any(|window| window == input_bytes),
        "open errors must preserve the raw input path: {:?}",
        output.stderr
    );
    assert!(
        !output
            .stderr
            .windows(3)
            .any(|window| window == b"\xef\xbf\xbd"),
        "open errors must not replace the raw input path with U+FFFD: {:?}",
        output.stderr
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cli_attachment_segment_open_error_matches_qpdf_for_non_utf8_path() {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    if !qpdf_available() {
        eprintln!("qpdf not available; skipping attachment-segment differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary attachment directory");
    let input = directory.path().join("input.pdf");
    let qpdf_output = directory.path().join("qpdf-output.pdf");
    let flpdf_output = directory.path().join("flpdf-output.pdf");
    let attachment = directory
        .path()
        .join(OsString::from_vec(b"attach-\xff.bin".to_vec()));
    std::fs::copy(MINIMAL, &input).expect("copy input fixture");

    let args = |output: &Path| {
        vec![
            input.as_os_str().to_os_string(),
            output.as_os_str().to_os_string(),
            OsString::from("--add-attachment"),
            attachment.as_os_str().to_os_string(),
            OsString::from("--"),
        ]
    };
    let qpdf_args = args(&qpdf_output);
    let qpdf = ProcessCommand::new("qpdf")
        .args(&qpdf_args)
        .output()
        .expect("run qpdf");
    let flpdf_args = args(&flpdf_output);
    let flpdf = ProcessCommand::new(assert_cmd::cargo::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .args(&flpdf_args)
        .output()
        .expect("run flpdf");

    assert_eq!(
        qpdf.status.code(),
        Some(2),
        "qpdf stderr: {:?}",
        qpdf.stderr
    );
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "status");
    assert_eq!(flpdf.stdout, qpdf.stdout, "stdout");
    assert_eq!(flpdf.stderr, qpdf.stderr, "stderr");
    assert!(
        qpdf.stderr
            .windows(attachment.as_os_str().as_bytes().len())
            .any(|window| window == attachment.as_os_str().as_bytes()),
        "qpdf must preserve the raw attachment path: {:?}",
        qpdf.stderr
    );
    assert!(!qpdf
        .stderr
        .windows(3)
        .any(|window| window == b"\xef\xbf\xbd"));
}

#[cfg(target_os = "linux")]
#[test]
fn cli_preserves_a_non_utf8_page_source_path_through_the_qpdf_segment_parser() {
    use std::os::unix::ffi::OsStringExt;

    let directory = tempfile::tempdir().expect("temporary source directory");
    let primary = directory.path().join("primary.pdf");
    let secondary = directory
        .path()
        .join(std::ffi::OsString::from_vec(b"secondary-\xff.pdf".to_vec()));
    let output_path = directory.path().join("output.pdf");
    std::fs::copy(ONE_PAGE, &primary).expect("copy primary fixture");
    std::fs::copy(ONE_PAGE, &secondary).expect("copy secondary fixture");

    let output = ProcessCommand::new(assert_cmd::cargo::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .arg(&primary)
        .arg("--pages")
        .arg(".")
        .arg(&secondary)
        .arg("--")
        .arg(&output_path)
        .output()
        .expect("run flpdf");

    assert_eq!(
        output.status.code(),
        Some(0),
        "non-UTF-8 page-source argv must not panic: {:?}",
        output.stderr
    );
    assert!(
        output_path.is_file(),
        "page operation must create its output"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cli_preserves_a_non_utf8_overlay_path_through_the_qpdf_segment_parser() {
    use std::os::unix::ffi::OsStringExt;

    let directory = tempfile::tempdir().expect("temporary source directory");
    let primary = directory.path().join("primary.pdf");
    let overlay = directory
        .path()
        .join(std::ffi::OsString::from_vec(b"overlay-\xff.pdf".to_vec()));
    let output_path = directory.path().join("output.pdf");
    std::fs::copy(ONE_PAGE, &primary).expect("copy primary fixture");
    std::fs::copy(ONE_PAGE, &overlay).expect("copy overlay fixture");

    let output = ProcessCommand::new(assert_cmd::cargo::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .arg(&primary)
        .arg("--overlay")
        .arg(&overlay)
        .arg("--")
        .arg(&output_path)
        .output()
        .expect("run flpdf");

    assert_eq!(
        output.status.code(),
        Some(0),
        "non-UTF-8 overlay argv must not panic: {:?}",
        output.stderr
    );
    assert!(
        output_path.is_file(),
        "overlay operation must create its output"
    );
}
