//! Regression coverage for flpdf-4ea: `flpdf rewrite --linearize` (and the
//! top-level `flpdf --linearize INPUT OUTPUT` shorthand) must refuse to run
//! against an encrypted input until the decrypt → linearize → back-patch
//! pipeline is exercised end-to-end. The reject was added in d8d665e because
//! the linearization writer historically packaged the still-encrypted streams
//! verbatim, producing a structurally-valid-looking PDF that silently broke
//! every content stream.

use assert_cmd::Command;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted")
        .join(name)
}

fn rewrite_linearize_into(out: &Path, fixture_name: &str, password: &str) -> Command {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("rewrite")
        .arg("--linearize")
        .arg(format!("--password={password}"))
        .arg("--allow-weak-crypto")
        .arg(fixture(fixture_name))
        .arg(out);
    cmd
}

#[test]
fn rewrite_linearize_rejects_encrypted_input_with_actionable_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out.pdf");
    rewrite_linearize_into(&out, "v5-aes-256-r6.pdf", "user-v5-r6")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "encrypted PDF output is not supported",
        ))
        .stderr(predicates::str::contains(
            "use plain rewrite to produce decrypted plaintext",
        ));
    assert!(
        !out.exists(),
        "rewrite --linearize must not leave a partial output behind"
    );
}

#[test]
fn rewrite_linearize_rejects_all_encrypted_fixture_variants() {
    let cases = [
        ("v1-rc4-40-r2.pdf", "user-v1"),
        ("v2-rc4-128-r3.pdf", "user-v2"),
        ("v4-rc4-128-r4.pdf", "user-v4-rc4"),
        ("v4-aes-128-r4.pdf", "user-v4-aes"),
        ("v5-aes-256-r5.pdf", "user-v5-r5"),
        ("v5-aes-256-r6.pdf", "user-v5-r6"),
    ];
    for (file_name, password) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join(format!("out-{file_name}"));
        rewrite_linearize_into(&out, file_name, password)
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "encrypted PDF output is not supported",
            ))
            .stderr(predicates::str::contains(
                "use plain rewrite to produce decrypted plaintext",
            ));
        assert!(
            !out.exists(),
            "{file_name}: --linearize must not leave a partial output behind"
        );
    }
}

#[test]
fn top_level_linearize_shorthand_also_rejects_encrypted_input() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out.pdf");
    // `flpdf --linearize INPUT OUTPUT` (no `rewrite` subcommand) is the qpdf-
    // style top-level alias; it must reject too.
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("--linearize")
        .arg("--password=user-v5-r6")
        .arg(fixture("v5-aes-256-r6.pdf"))
        .arg(&out);
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains(
            "encrypted PDF output is not supported",
        ))
        .stderr(predicates::str::contains(
            "use plain rewrite to produce decrypted plaintext",
        ));
    assert!(!out.exists());
}
