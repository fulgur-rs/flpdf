use assert_cmd::Command;
use std::path::Path;
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

#[derive(Clone, Copy)]
enum EncryptionOrder {
    DecryptThenEncrypt,
    EncryptThenDecrypt,
    DecryptThenCopy,
}

impl EncryptionOrder {
    const ALL: [Self; 3] = [
        Self::DecryptThenEncrypt,
        Self::EncryptThenDecrypt,
        Self::DecryptThenCopy,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::DecryptThenEncrypt => "decrypt then encrypt",
            Self::EncryptThenDecrypt => "encrypt then decrypt",
            Self::DecryptThenCopy => "decrypt then copy-encryption",
        }
    }

    fn show_password(self) -> &'static str {
        match self {
            Self::DecryptThenEncrypt => "new",
            Self::EncryptThenDecrypt | Self::DecryptThenCopy => "",
        }
    }
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

fn args_for(order: EncryptionOrder, input: &Path, output: &Path) -> Vec<String> {
    let input = input.to_str().expect("fixture path is UTF-8");
    let output = output.to_str().expect("output path is UTF-8");
    let donor = format!("--copy-encryption={input}");
    match order {
        EncryptionOrder::DecryptThenEncrypt => vec![
            "--password=".into(),
            "--static-id".into(),
            "--decrypt".into(),
            input.into(),
            "--encrypt".into(),
            "--user-password=new".into(),
            "--owner-password=n2".into(),
            "--bits=256".into(),
            "--".into(),
            output.into(),
        ],
        EncryptionOrder::EncryptThenDecrypt => vec![
            "--password=".into(),
            "--static-id".into(),
            "--encrypt".into(),
            "--user-password=new".into(),
            "--owner-password=n2".into(),
            "--bits=256".into(),
            "--".into(),
            input.into(),
            "--decrypt".into(),
            output.into(),
        ],
        EncryptionOrder::DecryptThenCopy => vec![
            "--password=".into(),
            "--static-id".into(),
            "--decrypt".into(),
            input.into(),
            donor,
            "--encryption-file-password=".into(),
            output.into(),
        ],
    }
}

fn run_qpdf(args: &[String]) -> Output {
    ProcessCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn show_encryption(path: &Path, password: &str) -> Output {
    ProcessCommand::new("qpdf")
        .args(["--show-encryption", &format!("--password={password}")])
        .arg(path)
        .output()
        .expect("qpdf --show-encryption should spawn")
}

#[test]
fn encryption_mode_options_follow_qpdf_last_occurrence_order() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_encryption_precedence] qpdf 11.9.0 is unavailable");
        return;
    }

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/encrypted-r4-three-page.pdf");
    let temp = tempfile::tempdir().expect("temporary output directory");

    for order in EncryptionOrder::ALL {
        let qpdf_output = temp.path().join(format!("qpdf-{}.pdf", order.name()));
        let flpdf_output = temp.path().join(format!("flpdf-{}.pdf", order.name()));
        let args = args_for(order, &fixture, &qpdf_output);
        let qpdf = run_qpdf(&args);

        let flpdf = Command::cargo_bin("flpdf")
            .expect("flpdf binary")
            .args(args_for(order, &fixture, &flpdf_output))
            .output()
            .expect("flpdf should spawn");
        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "{}: qpdf stderr={} flpdf stderr={}",
            order.name(),
            String::from_utf8_lossy(&qpdf.stderr),
            String::from_utf8_lossy(&flpdf.stderr)
        );
        assert!(qpdf.status.success(), "{}: qpdf must succeed", order.name());

        let qpdf_encryption = show_encryption(&qpdf_output, order.show_password());
        let flpdf_encryption = show_encryption(&flpdf_output, order.show_password());
        assert_eq!(
            flpdf_encryption.status.code(),
            qpdf_encryption.status.code(),
            "{}: encryption inspection status differs",
            order.name()
        );
        assert_eq!(
            flpdf_encryption.stdout,
            qpdf_encryption.stdout,
            "{}: encryption inspection differs",
            order.name()
        );
    }
}

#[test]
fn overridden_encrypt_still_validates_its_key_length() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_encryption_precedence] qpdf 11.9.0 is unavailable");
        return;
    }

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let temp = tempfile::tempdir().expect("temporary output directory");
    let qpdf_output = temp.path().join("qpdf-invalid.pdf");
    let flpdf_output = temp.path().join("flpdf-invalid.pdf");
    let args = vec![
        "--encrypt".to_owned(),
        "u".to_owned(),
        "o".to_owned(),
        "999".to_owned(),
        "--".to_owned(),
        fixture.to_str().unwrap().to_owned(),
        "--decrypt".to_owned(),
        qpdf_output.to_str().unwrap().to_owned(),
    ];
    let qpdf = run_qpdf(&args);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--encrypt",
            "u",
            "o",
            "999",
            "--",
            fixture.to_str().unwrap(),
            "--decrypt",
            flpdf_output.to_str().unwrap(),
        ])
        .output()
        .expect("flpdf should spawn");
    assert_eq!(qpdf.status.code(), Some(2));
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert!(
        String::from_utf8_lossy(&flpdf.stderr).contains("KEY-LEN"),
        "flpdf should validate the overridden encryption occurrence: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
}
