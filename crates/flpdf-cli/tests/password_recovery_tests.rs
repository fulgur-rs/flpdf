use assert_cmd::Command;
use flpdf::{EncryptParams, Pdf, PdfWriter};
use std::fs;
use std::io::Cursor;
use std::path::Path;

fn encrypted_fixture_with_raw_password(password: &[u8]) -> Vec<u8> {
    let input_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let input = fs::read(input_path).unwrap();
    let mut pdf = Pdf::open(Cursor::new(input)).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(
        password.to_vec(),
        b"owner".to_vec(),
    ));
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    writer.get_buffer().unwrap()
}

fn run_check(input: &Path, password_file: &Path, suppress_recovery: bool) -> Command {
    let mut command = Command::cargo_bin("flpdf").unwrap();
    command.args([
        "check",
        &format!("--password-file={}", password_file.display()),
    ]);
    if suppress_recovery {
        command.arg("--suppress-password-recovery");
    }
    command.arg(input);
    command
}

#[test]
fn check_recovers_pdfdoc_password_and_suppression_disables_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("encrypted.pdf");
    let password_file = temp.path().join("password.txt");
    fs::write(&input, encrypted_fixture_with_raw_password(b"caf\xe9")).unwrap();
    fs::write(&password_file, "café").unwrap();

    run_check(&input, &password_file, false).assert().success();
    run_check(&input, &password_file, true)
        .assert()
        .failure()
        .stderr(predicates::str::contains("incorrect password"));
}
