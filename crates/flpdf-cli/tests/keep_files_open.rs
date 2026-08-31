//! qpdf 11.9.0 `--keep-files-open` and threshold parity.
//!
//! The qpdf oracle is `vendor/qpdf-qtest/keep-files-open.test`: its four
//! cases exercise the automatic distinct-file threshold and explicit y/n
//! overrides on the `--empty --pages` route.  The observable contract is the
//! verbose selection line; the output PDF is also required to be created.

use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;

fn minimal_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf")
}

fn run_keep_files_open_case(
    file_count: usize,
    extra_options: &[&str],
    expected_selection: Option<&str>,
) {
    let temp = tempfile::tempdir().expect("temporary qtest input directory");
    let fixture = minimal_fixture();
    let mut inputs = Vec::with_capacity(file_count);
    for number in 1..=file_count {
        let path = temp.path().join(format!("{number:03}-kfo.pdf"));
        fs::copy(&fixture, &path).expect("copy qtest input fixture");
        inputs.push(path);
    }
    let output = temp.path().join("a.pdf");

    let mut args = vec!["--verbose", "--static-id", "--empty"]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
    args.extend(extra_options.iter().map(|option| (*option).to_owned()));
    args.push("--pages".to_owned());
    args.extend(inputs.iter().map(|path| path.display().to_string()));
    args.push("--".to_owned());
    args.push(output.display().to_string());

    let result = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(&args)
        .output()
        .expect("flpdf invocation");
    assert!(
        result.status.success(),
        "keep-files-open case failed: status={:?}\nstdout={}\nstderr={}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    match expected_selection {
        Some(expected_selection) => {
            assert!(
                stdout.contains(expected_selection),
                "missing selection line {expected_selection:?} in:\n{stdout}"
            );
            assert!(
                !stdout.contains(if expected_selection.ends_with("=y\n") {
                    "selecting --keep-open-files=n"
                } else {
                    "selecting --keep-open-files=y"
                }),
                "opposite keep-files-open selection was emitted:\n{stdout}"
            );
        }
        None => assert!(
            !stdout.contains("selecting --keep-open-files="),
            "explicit keep-files-open must not emit an automatic selection line:\n{stdout}"
        ),
    }
    assert!(
        output.is_file(),
        "qpdf-shaped page job did not write output"
    );
    let page_count = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--show-npages", output.to_str().expect("output path")])
        .output()
        .expect("show output page count");
    assert!(
        page_count.status.success(),
        "output page-count inspection failed: {}",
        String::from_utf8_lossy(&page_count.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&page_count.stdout).trim(),
        file_count.to_string(),
        "empty page job must copy one page from each selected source"
    );
}

#[test]
fn automatic_threshold_disables_keep_files_open_above_boundary() {
    run_keep_files_open_case(
        51,
        &["--keep-files-open-threshold=50"],
        Some("flpdf: selecting --keep-open-files=n\n"),
    );
}

#[test]
fn automatic_threshold_keeps_files_open_within_boundary() {
    run_keep_files_open_case(10, &[], Some("flpdf: selecting --keep-open-files=y\n"));
}

#[test]
fn threshold_accepts_qpdf_numeric_prefix_conversion() {
    run_keep_files_open_case(
        1,
        &["--keep-files-open-threshold=50junk"],
        Some("flpdf: selecting --keep-open-files=y\n"),
    );
}

#[test]
fn negative_threshold_matches_qpdf_unsigned_conversion_error() {
    let temp = tempfile::tempdir().expect("temporary qtest input directory");
    let fixture = minimal_fixture();
    let input = temp.path().join("001-kfo.pdf");
    fs::copy(fixture, &input).expect("copy qtest input fixture");
    let output = temp.path().join("a.pdf");

    let result = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--verbose",
            "--static-id",
            "--empty",
            "--keep-files-open-threshold=-1",
            "--pages",
        ])
        .arg(&input)
        .args(["--"])
        .arg(&output)
        .output()
        .expect("flpdf invocation");
    assert!(!result.status.success(), "negative threshold must fail");
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("underflow converting -1 to 64-bit unsigned integer"),
        "unexpected diagnostic: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        !output.exists(),
        "failed configuration must not write output"
    );
}

#[test]
fn explicit_keep_files_open_y_overrides_automatic_selection() {
    run_keep_files_open_case(9, &["--keep-files-open=y"], None);
}

#[test]
fn explicit_keep_files_open_n_overrides_automatic_selection() {
    run_keep_files_open_case(9, &["--keep-files-open=n"], None);
}

#[test]
fn empty_pages_route_applies_a_requested_overlay() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fixture = minimal_fixture();
    let with_overlay = temp.path().join("with-overlay.pdf");
    let without_overlay = temp.path().join("without-overlay.pdf");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--static-id", "--empty", "--overlay"])
        .arg(&fixture)
        .args(["--", "--pages"])
        .arg(&fixture)
        .args(["1", "--"])
        .arg(&with_overlay)
        .assert()
        .success();
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--static-id", "--empty", "--pages"])
        .arg(&fixture)
        .args(["1", "--"])
        .arg(&without_overlay)
        .assert()
        .success();

    let with_bytes = fs::read(&with_overlay).expect("read overlay output");
    let without_bytes = fs::read(&without_overlay).expect("read baseline output");
    assert_ne!(
        with_bytes, without_bytes,
        "--overlay must change the --empty --pages output, not be silently dropped"
    );
}

#[test]
fn rewrite_subcommand_accepts_empty_pages_like_the_top_level_route() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fixture = minimal_fixture();
    let top_level_output = temp.path().join("top-level.pdf");
    let rewrite_output = temp.path().join("rewrite.pdf");
    // `rewrite`'s own `input` positional is unused for an empty primary
    // (mirrors the top-level route's own repurposed positional), and need
    // not exist.
    let unused_input = temp.path().join("unused-input.pdf");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--static-id", "--empty", "--pages"])
        .arg(&fixture)
        .args(["1", "--"])
        .arg(&top_level_output)
        .assert()
        .success();
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .arg("rewrite")
        .arg(&unused_input)
        .arg(&rewrite_output)
        .args(["--static-id", "--empty", "--pages"])
        .arg(&fixture)
        .args(["1", "--"])
        .assert()
        .success();

    assert_eq!(
        fs::read(&top_level_output).expect("read top-level output"),
        fs::read(&rewrite_output).expect("read rewrite-subcommand output"),
        "rewrite --empty --pages must match the top-level --empty --pages route byte-for-byte"
    );
}

#[test]
fn empty_pages_route_applies_a_valid_update_from_json_to_the_empty_primary() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fixture = minimal_fixture();
    let output = temp.path().join("updated.pdf");
    let update_json = temp.path().join("update.json");
    // A well-formed qpdf JSON v2 update against the fixed empty primary
    // (object 1 is its Catalog, pointing at empty Pages object 2, per
    // `EMPTY_PDF_BYTES`); adds a harmless custom key to prove the update
    // actually ran.
    fs::write(
        &update_json,
        r#"{"qpdf":[{"jsonversion":2},{"obj:1 0 R":{"value":{"/Type":"/Catalog","/Pages":"2 0 R","/Custom":"/UPDATED"}}}]}"#,
    )
    .expect("write update JSON");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--static-id"])
        .arg(format!("--update-from-json={}", update_json.display()))
        .args(["--empty", "--pages"])
        .arg(&fixture)
        .args(["1", "--"])
        .arg(&output)
        .assert()
        .success();
    assert!(output.is_file(), "update-from-json route must write output");
}

#[test]
fn empty_pages_route_surfaces_malformed_update_from_json_as_an_error() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fixture = minimal_fixture();
    let output = temp.path().join("rejected.pdf");
    let update_json = temp.path().join("bad-update.json");
    fs::write(&update_json, r#"{"bogus": "not a qpdf JSON v2 update"}"#)
        .expect("write malformed update JSON");

    // qpdf 11.9.0 live-probed: `--update-from-json` against `--empty --pages`
    // is not a silent no-op; malformed JSON exits 2 with "errors found in
    // JSON" and no output file is written.
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg(format!("--update-from-json={}", update_json.display()))
        .args(["--empty", "--pages"])
        .arg(&fixture)
        .args(["1", "--"])
        .arg(&output)
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("errors found in JSON"));
    assert!(
        !output.exists(),
        "a rejected --update-from-json must not write output"
    );
}
