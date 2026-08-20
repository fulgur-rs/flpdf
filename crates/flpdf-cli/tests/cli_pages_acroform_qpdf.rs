//! AcroForm copy-order parity for qpdf 11.9.0 `--pages`.
//!
//! qpdf fixes copied annotations at each final page-selection event
//! (`libqpdf/QPDFJob.cc:2517-2584`), so a repeated page gets a fresh widget and
//! field tree and is renamed against the fields already added in that order
//! (`libqpdf/QPDFAcroFormDocumentHelper.cc:62-110`).

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use std::process::Command as Shell;

const QPDF: &str = "/usr/bin/qpdf";
const EXPECTED_QPDF_VERSION: &str = "11.9.0";
const FIXTURE: &str = "../../tests/fixtures/compat/acroform-sig-widget.pdf";

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
    assert_eq!(ids.len(), 3, "{description} count");
    assert_eq!(
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        3,
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
}
