use assert_cmd::Command as CargoCommand;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn root_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn qpdf_available() -> bool {
    ProcessCommand::new("/usr/bin/qpdf")
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

fn run_qpdf(args: &[String]) -> Output {
    ProcessCommand::new("/usr/bin/qpdf")
        .args(args)
        .output()
        .expect("qpdf 11.9.0 should spawn")
}

fn run_flpdf(args: &[String]) -> Output {
    CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assert_matches_flpdf(args: &[String]) {
    let expected = run_qpdf(args);
    let actual = run_flpdf(args);
    assert_eq!(expected.status.code(), Some(0), "qpdf failed: {expected:?}");
    assert_eq!(actual.status.code(), expected.status.code());
    assert_eq!(actual.stdout, expected.stdout);
    assert_eq!(actual.stderr, expected.stderr);
}

#[test]
fn attachment_inspection_accepts_generate_appearances_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let input = fixture("attachment-two-page.pdf");
    let args = vec![
        "--generate-appearances".to_owned(),
        "--list-attachments".to_owned(),
        input.display().to_string(),
    ];
    assert_matches_flpdf(&args);
}

#[test]
fn attachment_show_accepts_generate_appearances_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let input = fixture("attachment-two-page.pdf");
    let args = vec![
        "--generate-appearances".to_owned(),
        "--show-attachment=attachment.txt".to_owned(),
        input.display().to_string(),
    ];
    assert_matches_flpdf(&args);
}

#[test]
fn attachment_inspection_accepts_flatten_annotations_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let input = fixture("attachment-two-page.pdf");
    let args = vec![
        "--flatten-annotations=all".to_owned(),
        "--list-attachments".to_owned(),
        input.display().to_string(),
    ];
    assert_matches_flpdf(&args);
}

#[test]
fn check_linearization_accepts_flatten_annotations_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let input = fixture("form-fields-and-annotations.pdf");
    let args = vec![
        "--check-linearization".to_owned(),
        "--flatten-annotations=all".to_owned(),
        input.display().to_string(),
    ];
    assert_matches_flpdf(&args);
}

#[test]
fn attachment_rewrite_routes_accept_generate_appearances_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let tempdir = tempfile::tempdir().expect("temporary directory");
    let input = root_fixture("minimal.pdf");
    let donor = fixture("attachment-two-page.pdf");
    let payload = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/fixture-names.txt");

    let add_qpdf = tempdir.path().join("add-qpdf.pdf");
    let add_flpdf = tempdir.path().join("add-flpdf.pdf");
    let add_args = vec![
        "--generate-appearances".to_owned(),
        "--add-attachment".to_owned(),
        payload.display().to_string(),
        "--key=added".to_owned(),
        "--".to_owned(),
        input.display().to_string(),
        add_qpdf.display().to_string(),
    ];
    let expected = run_qpdf(&add_args);
    let actual = run_flpdf(&{
        let mut args = add_args.clone();
        *args.last_mut().expect("qpdf output path") = add_flpdf.display().to_string();
        args
    });
    assert_eq!(expected.status.code(), Some(0), "qpdf failed: {expected:?}");
    assert_eq!(actual.status.code(), expected.status.code());
    assert_eq!(actual.stdout, expected.stdout);
    assert_eq!(actual.stderr, expected.stderr);

    let copy_qpdf = tempdir.path().join("copy-qpdf.pdf");
    let copy_flpdf = tempdir.path().join("copy-flpdf.pdf");
    let copy_args = vec![
        "--generate-appearances".to_owned(),
        "--copy-attachments-from".to_owned(),
        donor.display().to_string(),
        "--".to_owned(),
        input.display().to_string(),
        copy_qpdf.display().to_string(),
    ];
    let expected = run_qpdf(&copy_args);
    let actual = run_flpdf(&{
        let mut args = copy_args.clone();
        *args.last_mut().expect("qpdf output path") = copy_flpdf.display().to_string();
        args
    });
    assert_eq!(expected.status.code(), Some(0), "qpdf failed: {expected:?}");
    assert_eq!(actual.status.code(), expected.status.code());
    assert_eq!(actual.stdout, expected.stdout);
    assert_eq!(actual.stderr, expected.stderr);
}

#[test]
fn attachment_rewrite_routes_accept_flatten_annotations_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let tempdir = tempfile::tempdir().expect("temporary directory");
    let input = fixture("attachment-two-page.pdf");
    let qpdf_output = tempdir.path().join("remove-qpdf.pdf");
    let flpdf_output = tempdir.path().join("remove-flpdf.pdf");
    let mut args = vec![
        "--flatten-annotations=all".to_owned(),
        "--remove-attachment=attachment.txt".to_owned(),
        input.display().to_string(),
        qpdf_output.display().to_string(),
    ];
    let expected = run_qpdf(&args);
    *args.last_mut().expect("qpdf output path") = flpdf_output.display().to_string();
    let actual = run_flpdf(&args);
    assert_eq!(expected.status.code(), Some(0), "qpdf failed: {expected:?}");
    assert_eq!(actual.status.code(), expected.status.code());
    assert_eq!(actual.stdout, expected.stdout);
    assert_eq!(actual.stderr, expected.stderr);
}
