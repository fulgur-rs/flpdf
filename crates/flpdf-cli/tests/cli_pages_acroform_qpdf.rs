//! AcroForm copy-order parity for qpdf 11.9.0 `--pages`.
//!
//! qpdf fixes copied annotations at each final page-selection event
//! (`libqpdf/QPDFJob.cc:2517-2584`), so a repeated page gets a fresh widget and
//! field tree and is renamed against the fields already added in that order
//! (`libqpdf/QPDFAcroFormDocumentHelper.cc:62-110`).

use assert_cmd::assert::OutputAssertExt;
use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use std::process::Command as Shell;

const QPDF: &str = "/usr/bin/qpdf";
const EXPECTED_QPDF_VERSION: &str = "11.9.0";
const FIXTURE: &str = "../../tests/fixtures/compat/acroform-sig-widget.pdf";
const NO_ACROFORM_FIXTURE: &str = "../../tests/fixtures/compat/link-annot-no-acroform.pdf";

fn qpdf_available() -> bool {
    if !Path::new(QPDF).exists() {
        return false;
    }
    match Shell::new(QPDF).arg("--version").output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.lines().next().map(str::trim)
                == Some(&format!("qpdf version {EXPECTED_QPDF_VERSION}"))
        }
        Err(_) => false,
    }
}

fn acroform_json(path: &Path) -> Value {
    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=acroform"])
        .arg(path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).expect("flpdf acroform JSON")
}

fn qpdf_acroform_json(path: &Path) -> Value {
    let output = Shell::new(QPDF)
        .args(["--json=2", "--json-key=acroform"])
        .arg(path)
        .output()
        .expect("qpdf should spawn");
    assert!(
        output.status.success(),
        "qpdf acroform JSON failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("qpdf acroform JSON")
}

fn qdf(path: &Path) -> String {
    let output = Shell::new(QPDF)
        .args(["--qdf", "--object-streams=disable"])
        .arg(path)
        .arg("-")
        .output()
        .expect("qpdf should spawn");
    assert!(
        output.status.success(),
        "qpdf QDF failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn acroform_has_dr(path: &Path) -> bool {
    let text = qdf(path);
    let start = text.find("/AcroForm").expect("catalog AcroForm");
    let mut tokens = text[start + "/AcroForm".len()..].split_whitespace();
    let first = tokens.next().expect("AcroForm value");
    let object_start = if first == "<<" {
        start
    } else {
        let generation = tokens.next().expect("AcroForm generation");
        let header = format!("\n{first} {generation} obj");
        text.find(&header).expect("AcroForm object")
    };
    let end = text[object_start..]
        .find("endobj")
        .map(|offset| object_start + offset)
        .expect("AcroForm object end");
    text[object_start..end].contains("/DR")
}

fn page_object_refs(path: &Path) -> Vec<String> {
    let output = Shell::new(QPDF)
        .arg("--show-pages")
        .arg(path)
        .output()
        .expect("qpdf should spawn");
    assert!(
        output.status.success(),
        "qpdf --show-pages failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            (parts.next() == Some("page")).then(|| {
                parts.next().expect("page number");
                format!(
                    "{} {} {}",
                    parts.next().expect("page object number"),
                    parts.next().expect("page object generation"),
                    parts.next().expect("page object marker")
                )
            })
        })
        .collect()
}

fn widget_page_position(path: &Path, partial_name: &str) -> usize {
    let text = qdf(path);
    let marker = format!("/T ({partial_name})");
    let marker_at = text.find(&marker).expect("field partial name");
    let object_at = text[..marker_at]
        .rfind(" 0 obj")
        .expect("field object header");
    let end = text[marker_at..]
        .find("endobj")
        .map(|offset| marker_at + offset)
        .expect("field object end");
    let object = &text[object_at..end];
    let mut page_ref_parts = object
        .split("/P ")
        .nth(1)
        .expect("widget /P")
        .split_whitespace();
    let page_ref = format!(
        "{} {} {}",
        page_ref_parts.next().expect("page object number"),
        page_ref_parts.next().expect("page object generation"),
        page_ref_parts.next().expect("page object marker")
    );
    page_object_refs(path)
        .iter()
        .position(|candidate| candidate.ends_with(&page_ref))
        .map(|index| index + 1)
        .expect("widget /P page")
}

fn observable_fields(json: &Value) -> Vec<Value> {
    json["acroform"]["fields"]
        .as_array()
        .expect("acroform fields array")
        .iter()
        .map(|field| {
            serde_json::json!({
                "partialname": field["partialname"],
                "pageposfrom1": field["pageposfrom1"],
            })
        })
        .collect()
}

fn field_object_ids(json: &Value, key: &str) -> Vec<String> {
    json["acroform"]["fields"]
        .as_array()
        .expect("acroform fields array")
        .iter()
        .map(|field| {
            field[key]
                .as_str()
                .expect("object reference string")
                .to_owned()
        })
        .collect()
}

fn annotation_object_ids(json: &Value) -> Vec<String> {
    json["acroform"]["fields"]
        .as_array()
        .expect("acroform fields array")
        .iter()
        .map(|field| {
            field["annotation"]["object"]
                .as_str()
                .expect("annotation object reference")
                .to_owned()
        })
        .collect()
}

fn assert_distinct_ids(ids: &[String], description: &str) {
    assert_distinct_ids_count(ids, 3, description);
}

fn assert_distinct_ids_count(ids: &[String], expected: usize, description: &str) {
    assert_eq!(ids.len(), expected, "{description} count");
    assert_eq!(
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        expected,
        "qpdf copies each final-page {description} independently"
    );
}

#[test]
fn repeated_and_foreign_pages_fix_acroform_in_final_order() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_pages_acroform_qpdf] qpdf 11.9.0 is unavailable");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    // Distinct literal paths intentionally make qpdf treat the first and
    // second occurrence as separate page-spec sources, while the third
    // occurrence repeats the primary source page.
    let first = temp.path().join("first.pdf");
    let second = temp.path().join("second.pdf");
    std::fs::copy(&fixture, &first).expect("copy first source");
    std::fs::copy(&fixture, &second).expect("copy second source");

    let qpdf_output = temp.path().join("qpdf.pdf");
    let qpdf_status = Shell::new(QPDF)
        .arg(&first)
        .args(["--pages"])
        .arg(&first)
        .arg("1")
        .arg(&second)
        .arg("1")
        .arg(&first)
        .arg("1")
        .args(["--"])
        .arg(&qpdf_output)
        .status()
        .expect("qpdf should spawn");
    assert!(qpdf_status.success(), "qpdf --pages should succeed");

    let flpdf_output = temp.path().join("flpdf.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&first)
        .args(["--pages"])
        .arg(&first)
        .arg("1")
        .arg(&second)
        .arg("1")
        .arg(&first)
        .arg("1")
        .args(["--"])
        .arg(&flpdf_output)
        .assert()
        .success();

    let qpdf_json = qpdf_acroform_json(&qpdf_output);
    let flpdf_json = acroform_json(&flpdf_output);
    assert_eq!(
        observable_fields(&flpdf_json),
        observable_fields(&qpdf_json),
        "AcroForm field order/names must follow qpdf's final page order"
    );
    assert_eq!(
        observable_fields(&qpdf_json),
        vec![
            serde_json::json!({"partialname": "Approval", "pageposfrom1": 1}),
            serde_json::json!({"partialname": "Approval+1", "pageposfrom1": 2}),
            serde_json::json!({"partialname": "Approval+2", "pageposfrom1": 3}),
        ]
    );

    for json in [&qpdf_json, &flpdf_json] {
        assert_distinct_ids(&field_object_ids(json, "object"), "field objects");
        assert_distinct_ids(&annotation_object_ids(json), "annotation objects");
    }

    assert_eq!(
        widget_page_position(&qpdf_output, "Approval+2"),
        1,
        "qpdf keeps the repeated primary widget's /P on its first page"
    );
    assert_eq!(
        widget_page_position(&flpdf_output, "Approval+2"),
        1,
        "flpdf must keep the repeated primary widget's /P on its first page"
    );
}

#[test]
fn repeated_single_source_acroform_copies_each_page_occurrence() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_pages_acroform_qpdf] qpdf 11.9.0 is unavailable");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let source = temp.path().join("source.pdf");
    std::fs::copy(&fixture, &source).expect("copy source");

    let qpdf_output = temp.path().join("qpdf.pdf");
    Shell::new(QPDF)
        .arg(&source)
        .args(["--pages"])
        .arg(&source)
        .arg("1")
        .arg(&source)
        .arg("1")
        .args(["--"])
        .arg(&qpdf_output)
        .assert()
        .success();

    let flpdf_output = temp.path().join("flpdf.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&source)
        .args(["--pages"])
        .arg(&source)
        .arg("1")
        .arg(&source)
        .arg("1")
        .args(["--"])
        .arg(&flpdf_output)
        .assert()
        .success();

    let qpdf_json = qpdf_acroform_json(&qpdf_output);
    let flpdf_json = acroform_json(&flpdf_output);
    assert_eq!(
        observable_fields(&flpdf_json),
        observable_fields(&qpdf_json),
        "same-source repeated pages must preserve qpdf field order and names"
    );
    assert_distinct_ids_count(
        &field_object_ids(&flpdf_json, "object"),
        2,
        "same-source field objects",
    );
    assert_distinct_ids_count(
        &annotation_object_ids(&flpdf_json),
        2,
        "same-source annotation objects",
    );
    assert_eq!(
        widget_page_position(&flpdf_output, "Approval+1"),
        1,
        "same-source repeated widget /P must point to the first page"
    );
}

#[test]
fn foreign_page_without_acroform_does_not_create_destination_dr() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_pages_acroform_qpdf] qpdf 11.9.0 is unavailable");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let source_fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let no_acroform_fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(NO_ACROFORM_FIXTURE);
    let source = temp.path().join("source.pdf");
    let no_acroform = temp.path().join("no-acroform.pdf");
    std::fs::copy(&source_fixture, &source).expect("copy source");
    std::fs::copy(&no_acroform_fixture, &no_acroform).expect("copy no-AcroForm source");

    let qpdf_output = temp.path().join("qpdf.pdf");
    Shell::new(QPDF)
        .arg(&source)
        .args(["--pages"])
        .arg(&source)
        .arg("1")
        .arg(&source)
        .arg("1")
        .arg(&no_acroform)
        .arg("1")
        .args(["--"])
        .arg(&qpdf_output)
        .assert()
        .success();

    let flpdf_output = temp.path().join("flpdf.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&source)
        .args(["--pages"])
        .arg(&source)
        .arg("1")
        .arg(&source)
        .arg("1")
        .arg(&no_acroform)
        .arg("1")
        .args(["--"])
        .arg(&flpdf_output)
        .assert()
        .success();

    assert_eq!(
        observable_fields(&acroform_json(&flpdf_output)),
        observable_fields(&qpdf_acroform_json(&qpdf_output)),
        "a foreign page without AcroForm must not alter field selection"
    );
    assert!(
        !acroform_has_dr(&qpdf_output),
        "qpdf has no destination /DR"
    );
    assert!(
        !acroform_has_dr(&flpdf_output),
        "flpdf must not create destination /DR for a no-AcroForm source"
    );
}
