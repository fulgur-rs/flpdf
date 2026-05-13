use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

const ENCRYPTED_FIXTURES: &[(&str, &str, bool)] = &[
    ("v1-rc4-40-r2.pdf", "user-v1", true),
    ("v2-rc4-128-r3.pdf", "user-v2", true),
    ("v4-rc4-128-r4.pdf", "user-v4-rc4", true),
    ("v4-aes-128-r4.pdf", "user-v4-aes", false),
    ("v5-aes-256-r5.pdf", "user-v5-r5", true),
    ("v5-aes-256-r6.pdf", "user-v5-r6", false),
];

#[test]
fn encrypted_fixtures_rewrite_to_plaintext_matching_qpdf_decrypt_objects() {
    skip_if_qpdf_missing();

    let tmp = tempfile::tempdir().unwrap();
    for (file_name, password, allow_weak_crypto) in ENCRYPTED_FIXTURES {
        let input = encrypted_fixture(file_name);
        let qpdf_plaintext = tmp.path().join(format!("qpdf-{file_name}"));
        let flpdf_plaintext = tmp.path().join(format!("flpdf-{file_name}"));

        run_qpdf_decrypt(&input, password, *allow_weak_crypto, &qpdf_plaintext);
        run_flpdf_rewrite(&input, password, *allow_weak_crypto, &flpdf_plaintext);

        assert_plaintext_pdf_is_readable(&flpdf_plaintext, file_name);

        // Byte equality is intentionally not required: flpdf rewrites plaintext
        // with its own incidental serialization. Compare qpdf's object JSON
        // instead, with static IDs to remove trailer-ID churn from the oracle.
        let qpdf_objects = qpdf_objects_json(&qpdf_plaintext);
        let flpdf_objects = qpdf_objects_json(&flpdf_plaintext);
        assert_eq!(
            flpdf_objects, qpdf_objects,
            "{file_name}: plaintext rewrite differs from qpdf --decrypt under qpdf --json=1 --json-key=objects"
        );
    }
}

fn encrypted_fixture(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted")
        .join(file_name)
}

fn run_flpdf_rewrite(input: &Path, password: &str, allow_weak_crypto: bool, output: &Path) {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("rewrite")
        .arg("--static-id")
        .arg(format!("--password={password}"));
    if allow_weak_crypto {
        cmd.arg("--allow-weak-crypto");
    }
    cmd.arg(input).arg(output).assert().success();
}

fn run_qpdf_decrypt(input: &Path, password: &str, allow_weak_crypto: bool, output: &Path) {
    let mut cmd = ShellCommand::new("qpdf");
    cmd.arg(format!("--password={password}"));
    if allow_weak_crypto {
        cmd.arg("--allow-weak-crypto");
    }
    cmd.arg("--static-id");
    let result = cmd
        .arg("--decrypt")
        .arg(input)
        .arg(output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "qpdf --decrypt failed for {}: {}",
        input.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn assert_plaintext_pdf_is_readable(output: &Path, file_name: &str) {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("check")
        .arg(output)
        .assert()
        .success()
        .stdout(predicates::str::contains("PDF check succeeded"));

    let bytes = std::fs::read(output).unwrap();
    assert!(
        !bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "{file_name}: plaintext rewrite must not contain /Encrypt"
    );
}

fn qpdf_objects_json(path: &Path) -> Vec<u8> {
    let result = ShellCommand::new("qpdf")
        .args(["--json=1", "--json-key=objects"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "qpdf --json=1 --json-key=objects failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

fn skip_if_qpdf_missing() {
    let available = ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if available {
        return;
    }

    let on_ci = std::env::var_os("CI").is_some();
    let on_windows = cfg!(target_os = "windows");
    if on_ci && !on_windows {
        panic!(
            "qpdf is required for encrypted plaintext rewrite tests on CI (Linux); install qpdf before running this test suite"
        );
    }
    eprintln!(
        "skipping: qpdf not available (target_os={}, CI={})",
        std::env::consts::OS,
        on_ci
    );
}
