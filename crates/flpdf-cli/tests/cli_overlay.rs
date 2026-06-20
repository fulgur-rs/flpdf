//! End-to-end CLI wiring tests for `--overlay` / `--underlay` (not byte-level).
//!
//! These run in the default Pure-Rust build (no `qpdf-zlib-compat`): they
//! exercise the full CLI path — raw-argv pre-split (`extract_overlay_groups`),
//! source open + `flpdf::OverlaySpec` build (`build_overlay_specs`), the
//! `run_rewrite` overlay-stacking step, and the write — and assert observable
//! outcomes (exit status, page count, presence of the overlay XObject markers).
//!
//! Byte-identity to qpdf is proven at the library layer (the `overlay::byte_gate`
//! tests in `crates/flpdf/src/overlay.rs`, run with `qpdf-zlib-compat`). The CLI
//! binary's default output is intentionally not byte-identical to qpdf — the CLI
//! emits `NewlineBeforeEndstream::Yes` framing whereas qpdf's default is `Never`,
//! a pre-existing project-wide divergence documented in
//! `tests/golden/compat-matrix.md` (CLI byte-equal "diverge for every row").
//! These tests therefore assert wiring behavior, not output bytes.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .to_path_buf()
}

fn fixture(name: &str) -> String {
    fixtures_dir().join(name).to_str().unwrap().to_string()
}

/// Run `flpdf rewrite <dest> <overlay_args...> <OUT>` and return the output
/// bytes, asserting exit 0.
fn run_overlay_ok(dest: &str, overlay_args: &[&str]) -> Vec<u8> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("overlay-out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("rewrite").arg(fixture(dest));
    for a in overlay_args {
        cmd.arg(a);
    }
    cmd.arg(&out);
    cmd.assert().success();

    std::fs::read(&out).expect("output file present after success")
}

#[test]
fn overlay_succeeds_and_output_parses_with_same_page_count() {
    let one = fixture("one-page.pdf");
    let bytes = run_overlay_ok("three-page.pdf", &["--overlay", &one, "--"]);

    // The output must be a valid PDF flpdf can re-open with the dest page count
    // unchanged (overlay stacks content, it does not add/remove pages).
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reparse.pdf");
    std::fs::write(&path, &bytes).unwrap();
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--show-npages")
        .arg(&path)
        .assert()
        .success()
        .stdout(predicate::str::contains("3"));

    // The applied overlay rebuilds page 1's resources as /Fx0 (the page) + /Fx1
    // (the source); their presence proves the stacking step actually ran.
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("/Fx0"),
        "overlay must convert the page to /Fx0"
    );
    assert!(
        text.contains("/Fx1"),
        "overlay must import the source as /Fx1"
    );
}

#[test]
fn two_overlays_compose_successfully() {
    let one = fixture("one-page.pdf");
    let two = fixture("two-page.pdf");
    let bytes = run_overlay_ok(
        "three-page.pdf",
        &["--overlay", &one, "--", "--overlay", &two, "--"],
    );
    let text = String::from_utf8_lossy(&bytes);
    // Page 1 receives both sources: /Fx0 (page) + /Fx1 + /Fx2.
    assert!(
        text.contains("/Fx2"),
        "two overlays must produce /Fx2: composed"
    );
}

#[test]
fn overlay_and_underlay_compose_successfully() {
    let one = fixture("one-page.pdf");
    let two = fixture("two-page.pdf");
    let bytes = run_overlay_ok(
        "three-page.pdf",
        &["--overlay", &one, "--", "--underlay", &two, "--"],
    );
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("/Fx2"),
        "mixed overlay+underlay must produce /Fx2"
    );
}

#[test]
fn overlay_with_from_range_succeeds() {
    // `--from=2` selects source page 2 first; the CLI must thread the range
    // through `build_overlay_specs` so the overlay still stacks (p1 <- s2).
    let two = fixture("two-page.pdf");
    let bytes = run_overlay_ok("three-page.pdf", &["--overlay", &two, "--from=2", "--"]);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Fx0"), "page must become /Fx0");
    assert!(
        text.contains("/Fx1"),
        "--from-selected source must import as /Fx1"
    );
}

#[test]
fn empty_from_differs_from_absent_from() {
    // qpdf parity: an explicit empty `--from=` is an empty source set, so
    // `--repeat` cycles from the first dest page (every dest page <- s2). This is
    // observably different from an absent `--from` (p1<-s1, p2<-s2, then repeat),
    // so the two invocations must NOT produce identical output.
    let two = fixture("two-page.pdf");
    let absent = run_overlay_ok("three-page.pdf", &["--overlay", &two, "--repeat=2", "--"]);
    let empty = run_overlay_ok(
        "three-page.pdf",
        &["--overlay", &two, "--from=", "--repeat=2", "--"],
    );
    assert_ne!(
        absent, empty,
        "explicit empty --from= must be distinguished from an absent --from"
    );
}

#[test]
fn single_underlay_succeeds() {
    // A lone `--underlay` (no overlay) must stack the source beneath the page.
    let two = fixture("two-page.pdf");
    let bytes = run_overlay_ok("three-page.pdf", &["--underlay", &two, "--"]);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Fx0"), "page must become /Fx0");
    assert!(text.contains("/Fx1"), "underlay source must import as /Fx1");
}

#[test]
fn overlay_with_pages_is_rejected() {
    let one = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("o.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg(fixture("three-page.pdf"))
        .args(["--overlay", &one, "--"])
        .args(["--pages", ".", "1-2", "--"])
        .arg(out.to_str().unwrap())
        .assert()
        .failure()
        .stderr(predicate::str::contains("overlay/--underlay"));
}

#[test]
fn overlay_with_linearize_is_rejected() {
    let one = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("o.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg("--linearize")
        .arg(fixture("three-page.pdf"))
        .args(["--overlay", &one, "--"])
        .arg(out.to_str().unwrap())
        .assert()
        .failure()
        .stderr(predicate::str::contains("--linearize"));
}

#[test]
fn unterminated_overlay_group_is_rejected() {
    let one = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("o.pdf");
    // No bare `--` after the source: qpdf requires the terminator.
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg(fixture("three-page.pdf"))
        .args(["--overlay", &one])
        .arg(out.to_str().unwrap())
        .assert()
        .failure()
        .stderr(predicate::str::contains("terminated by a `--`"));
}

#[test]
fn top_level_overlay_with_pages_is_rejected() {
    // The top-level (subcommand-less) page-op path also rejects overlay.
    let one = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("o.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(fixture("three-page.pdf"))
        .args(["--overlay", &one, "--"])
        .args(["--pages", ".", "1-2", "--"])
        .arg(out.to_str().unwrap())
        .assert()
        .failure()
        .stderr(predicate::str::contains("overlay/--underlay"));
}

#[test]
fn unterminated_underlay_group_is_rejected() {
    // The --underlay flag-name arm of the unterminated-group error.
    let two = fixture("two-page.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("o.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg(fixture("three-page.pdf"))
        .args(["--underlay", &two])
        .arg(out.to_str().unwrap())
        .assert()
        .failure()
        .stderr(predicate::str::contains("terminated by a `--`"));
}

#[test]
fn top_level_overlay_alias_succeeds() {
    // qpdf-shaped top-level form: `flpdf in --overlay f -- out`.
    let one = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("o.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(fixture("three-page.pdf"))
        .args(["--overlay", &one, "--"])
        .arg(out.to_str().unwrap())
        .assert()
        .success();
    let bytes = std::fs::read(&out).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("/Fx0"));
}

#[test]
fn overlay_encrypted_source_with_correct_password_succeeds() {
    // The source is AES-256 encrypted (user password "u"). The segment's
    // --password= must be threaded to the source open so the page can be
    // imported as the overlay XObject. (Output is not byte-compared here — see
    // the module doc; byte-identity for a higher-version source is tracked
    // separately as version-floor propagation.)
    let enc = fixture("one-page-enc-u.pdf");
    let bytes = run_overlay_ok("three-page.pdf", &["--overlay", &enc, "--password=u", "--"]);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Fx0"), "page must become /Fx0");
    assert!(
        text.contains("/Fx1"),
        "decrypted source must import as /Fx1"
    );
}

#[test]
fn overlay_encrypted_source_with_wrong_password_fails() {
    let enc = fixture("one-page-enc-u.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("o.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg(fixture("three-page.pdf"))
        .args(["--overlay", &enc, "--password=wrong", "--"])
        .arg(out.to_str().unwrap())
        .assert()
        .failure();
}

#[test]
fn overlay_encrypted_source_without_password_fails() {
    let enc = fixture("one-page-enc-u.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("o.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg(fixture("three-page.pdf"))
        .args(["--overlay", &enc, "--"])
        .arg(out.to_str().unwrap())
        .assert()
        .failure();
}

#[test]
fn overlay_on_non_rewrite_subcommand_is_rejected() {
    // An overlay group is stripped from argv before clap; on a non-rewrite
    // command it must fail loudly, not be silently ignored.
    let one = fixture("one-page.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("check")
        .arg(fixture("three-page.pdf"))
        .args(["--overlay", &one, "--"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "can only be used with rewrite output",
        ));
}

#[test]
fn overlay_equals_form_is_rejected() {
    // qpdf rejects `--overlay=FILE` (the file must be a separate token). flpdf
    // must too, so the equals form is never a silent no-op via clap.
    let one = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("o.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg(fixture("three-page.pdf"))
        .arg(format!("--overlay={one}"))
        .arg("--")
        .arg(out.to_str().unwrap())
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not supported"));
}

#[test]
fn overlay_on_top_level_inspection_is_rejected() {
    // Top-level inspection mode (--show-npages) with an overlay group must also
    // fail rather than drop the overlay.
    let one = fixture("one-page.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--show-npages")
        .arg(fixture("three-page.pdf"))
        .args(["--overlay", &one, "--"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "can only be used with rewrite output",
        ));
}
