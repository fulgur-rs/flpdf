//! qpdf parity coverage for `flpdf rewrite --linearize` (and the top-level
//! `flpdf --linearize INPUT OUTPUT` shorthand) on encrypted inputs. The
//! encrypted fixtures in this file intentionally have no pages; qpdf 11.9.0
//! reports the same "no pages found while calculating linearization data"
//! diagnostic before it can emit an output.

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
fn rewrite_linearize_encrypted_no_page_input_matches_qpdf_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out.pdf");
    rewrite_linearize_into(&out, "v5-aes-256-r6.pdf", "user-v5-r6")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "no pages found while calculating linearization data",
        ));
    assert_eq!(
        std::fs::metadata(&out).unwrap().len(),
        0,
        "qpdf leaves a zero-byte output placeholder on this planning error"
    );
}

#[test]
fn rewrite_linearize_encrypted_fixture_errors_match_qpdf() {
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
                "no pages found while calculating linearization data",
            ));
        assert_eq!(
            std::fs::metadata(&out).unwrap().len(),
            0,
            "{file_name}: qpdf leaves a zero-byte output placeholder on this planning error"
        );
    }
}

#[test]
fn top_level_linearize_encrypted_no_page_input_matches_qpdf_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out.pdf");
    // `flpdf --linearize INPUT OUTPUT` (no `rewrite` subcommand) is the qpdf-
    // style top-level alias and must surface the same qpdf error.
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("--linearize")
        .arg("--password=user-v5-r6")
        .arg(fixture("v5-aes-256-r6.pdf"))
        .arg(&out);
    cmd.assert().failure().stderr(predicates::str::contains(
        "no pages found while calculating linearization data",
    ));
    assert_eq!(
        std::fs::metadata(&out).unwrap().len(),
        0,
        "qpdf leaves a zero-byte output placeholder on this planning error"
    );
}
