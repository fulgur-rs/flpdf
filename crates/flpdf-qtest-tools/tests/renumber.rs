use std::path::PathBuf;
use std::process::Command;

use flpdf::{ObjectStreamMode, Pdf, PdfWriter};
use std::fs::File;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn test_renumber_usage_matches_qpdf_status_two() {
    let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
        .output()
        .expect("test_renumber must be runnable");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Usage: test_renumber [OPTION] INPUT.pdf\nOption:\n  --object-streams=preserve|disable|generate\n  --linearize\n  --preserve-unreferenced\n"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn test_renumber_rejects_unknown_options_and_missing_input() {
    let minimal = fixture("minimal.pdf");
    let minimal = minimal.to_str().expect("fixture path must be UTF-8");
    for args in [
        vec!["--not-an-option", minimal],
        vec!["--object-streams=bad", minimal],
        vec!["/path/that/does/not/exist.pdf"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
            .args(args)
            .output()
            .expect("test_renumber must be runnable");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }

    let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
        .arg("--linearize")
        .output()
        .expect("test_renumber must be runnable");
    assert_eq!(output.status.code(), Some(2));
    let expected_open_error = if cfg!(windows) {
        "open : The system cannot find the path specified.\n"
    } else {
        "open : No such file or directory\n"
    };
    assert_eq!(String::from_utf8_lossy(&output.stderr), expected_open_error);
}

#[test]
fn test_renumber_minimal_pdf_reports_success() {
    let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
        .arg(fixture("minimal.pdf"))
        .output()
        .expect("test_renumber must be runnable");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--- compare between input and renumbered objects ---"));
    assert!(stdout.contains("--- compare between written and reloaded xref tables ---"));
    assert!(stdout.ends_with("succeeded\n"));
    assert!(output.stderr.is_empty());
}

#[test]
fn test_renumber_signed_qpdf_fixture_preserve_linearization_reports_success() {
    let Some(input) = std::env::var_os("FLPDF_QPDF_RENUMBER_INPUT") else {
        eprintln!("skipping signed qpdf fixture regression: env is unavailable");
        return;
    };
    let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
        .args([
            "--linearize",
            input.to_str().expect("fixture path must be UTF-8"),
        ])
        .output()
        .expect("test_renumber must be runnable");
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).ends_with("succeeded\n"));
    assert!(output.stderr.is_empty());
}

#[test]
fn preserve_linearization_handles_generated_open_document_objstms() {
    let input = fixture("compat/objstm-lin-openaction-80-80.pdf");
    let mut source = Pdf::open(File::open(input).expect("fixture must be readable"))
        .expect("fixture must parse");
    let generated = {
        let mut writer = PdfWriter::new(&mut source);
        writer.set_output_memory().expect("configure memory output");
        writer.set_object_stream_mode(ObjectStreamMode::Generate);
        writer.write().expect("generate source ObjStms");
        writer.get_buffer().expect("read generated PDF")
    };

    let mut preserved = Pdf::open_mem_owned(generated).expect("generated PDF must reload");
    let mut writer = PdfWriter::new(&mut preserved);
    writer.set_output_memory().expect("configure memory output");
    writer.set_object_stream_mode(ObjectStreamMode::Preserve);
    writer.set_linearization(true);
    writer.write().expect("preserve linearization must succeed");
    assert!(!writer.get_buffer().expect("read linearized PDF").is_empty());
}
