use assert_cmd::Command as CargoCommand;
use std::fs;
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
    ProcessCommand::new("qpdf")
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
    ProcessCommand::new("qpdf")
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

/// Run one rewrite both ways and compare the produced file, not only the
/// console. `--static-id` keeps `/ID` out of the comparison, and callers pass
/// `--stream-data=uncompress` so the default miniz_oxide backend's DEFLATE
/// bytes -- CLAUDE.md's one sanctioned difference -- stay out of it too.
fn assert_rewrite_matches_qpdf(args: &[String], qpdf_output: &Path, flpdf_output: &Path) {
    let expected = run_qpdf(args);
    let actual = run_flpdf(&{
        let mut args = args.to_vec();
        *args.last_mut().expect("output path") = flpdf_output.display().to_string();
        args
    });
    assert_eq!(expected.status.code(), Some(0), "qpdf failed: {expected:?}");
    assert_eq!(actual.status.code(), expected.status.code());
    assert_eq!(actual.stdout, expected.stdout);
    assert_eq!(actual.stderr, expected.stderr);
    assert_eq!(
        fs::read(flpdf_output).expect("flpdf output"),
        fs::read(qpdf_output).expect("qpdf output"),
        "rewrite output must match qpdf for {args:?}"
    );
}

/// An AcroForm that actually needs appearance generation. Every compat
/// fixture leaves `/NeedAppearances` unset, so `--generate-appearances` is a
/// no-op against them and cannot show that the flag reached the document.
fn need_appearances_pdf() -> Vec<u8> {
    let bodies: [&str; 5] = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /NeedAppearances true \
/DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv 5 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (field1) /V (hello) \
/Rect [100 100 300 130] /P 3 0 R >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    ];

    let mut bytes = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes(),
    );
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            bodies.len() + 1
        )
        .as_bytes(),
    );
    bytes
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
        "--static-id".to_owned(),
        "--stream-data=uncompress".to_owned(),
        "--generate-appearances".to_owned(),
        "--add-attachment".to_owned(),
        payload.display().to_string(),
        "--key=added".to_owned(),
        "--".to_owned(),
        input.display().to_string(),
        add_qpdf.display().to_string(),
    ];
    assert_rewrite_matches_qpdf(&add_args, &add_qpdf, &add_flpdf);

    let copy_qpdf = tempdir.path().join("copy-qpdf.pdf");
    let copy_flpdf = tempdir.path().join("copy-flpdf.pdf");
    let copy_args = vec![
        "--static-id".to_owned(),
        "--stream-data=uncompress".to_owned(),
        "--generate-appearances".to_owned(),
        "--copy-attachments-from".to_owned(),
        donor.display().to_string(),
        "--".to_owned(),
        input.display().to_string(),
        copy_qpdf.display().to_string(),
    ];
    assert_rewrite_matches_qpdf(&copy_args, &copy_qpdf, &copy_flpdf);
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
    let args = vec![
        "--static-id".to_owned(),
        "--stream-data=uncompress".to_owned(),
        "--flatten-annotations=all".to_owned(),
        "--remove-attachment=attachment.txt".to_owned(),
        input.display().to_string(),
        qpdf_output.display().to_string(),
    ];
    assert_rewrite_matches_qpdf(&args, &qpdf_output, &flpdf_output);
}

/// The flag has to reach the document, not merely be accepted. Against a form
/// that needs appearances, qpdf's own output differs with and without it, so
/// a route that silently dropped the flag would fail this comparison.
#[test]
fn generate_appearances_reaches_the_attachment_rewrite_route() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let tempdir = tempfile::tempdir().expect("temporary directory");
    let input = tempdir.path().join("need-appearances.pdf");
    fs::write(&input, need_appearances_pdf()).expect("write the fixture");
    let payload = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/fixture-names.txt");

    let build = |generate: bool, output: &Path| {
        let mut args = vec![
            "--static-id".to_owned(),
            "--stream-data=uncompress".to_owned(),
        ];
        if generate {
            args.push("--generate-appearances".to_owned());
        }
        args.extend([
            "--add-attachment".to_owned(),
            payload.display().to_string(),
            "--key=added".to_owned(),
            "--".to_owned(),
            input.display().to_string(),
            output.display().to_string(),
        ]);
        args
    };

    // qpdf itself must produce different bytes here, or the comparison below
    // would pass even for a route that ignores the flag.
    let plain = tempdir.path().join("plain-qpdf.pdf");
    let generated = tempdir.path().join("generated-qpdf.pdf");
    let plain_run = run_qpdf(&build(false, &plain));
    assert_eq!(plain_run.status.code(), Some(0), "{plain_run:?}");
    let generated_run = run_qpdf(&build(true, &generated));
    assert_eq!(generated_run.status.code(), Some(0), "{generated_run:?}");
    assert_ne!(
        fs::read(&plain).expect("plain qpdf output"),
        fs::read(&generated).expect("generated qpdf output"),
        "the fixture must make --generate-appearances observable"
    );

    let actual = tempdir.path().join("generated-flpdf.pdf");
    assert_rewrite_matches_qpdf(&build(true, &generated), &generated, &actual);
}
