//! qpdf 11.9.0 differential coverage for generic optional-content graphs
//! through ordinary rewrite, page extraction, and multi-source page copy.
//!
//! qpdf has no OCG-specific model. The assertions here pin the reachable
//! object graph and stream data that its generic writer/page-copy paths emit.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";
const PRIMARY_OCG_NAMES: &[&str] = &["u:Primary used", "u:Primary unused"];
const SECONDARY_CONTENT_BASE64: &str = "L09DIC9PQ19CIEJEQwovRm0gRG8KRU1DCg==";
const SECONDARY_FORM_BASE64: &str = "cSAwIDAgMSByZyAwIDAgMjUgMjUgcmUgZiBRCg==";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
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

fn require_qpdf() -> bool {
    if qpdf_available() {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for optional-content page-operation parity tests");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available");
    false
}

fn output_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed with {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn qpdf_check(path: &Path) {
    let output = ShellCommand::new("qpdf")
        .arg("--check")
        .arg(path)
        .output()
        .expect("qpdf should spawn");
    output_success("qpdf --check", &output);
}

fn writer_args(input: &Path, output: &Path) -> Vec<String> {
    vec![
        "--static-id".to_owned(),
        "--object-streams=disable".to_owned(),
        "--compress-streams=n".to_owned(),
        input.display().to_string(),
        output.display().to_string(),
    ]
}

fn content_transform_args(input: &Path, option: &str, output: &Path) -> Vec<String> {
    let mut args = writer_args(input, output);
    args.insert(3, option.to_owned());
    args
}

fn page_operation_args(input: &Path, specs: &[String], output: &Path) -> Vec<String> {
    let mut args = vec![
        "--static-id".to_owned(),
        "--object-streams=disable".to_owned(),
        "--compress-streams=n".to_owned(),
        input.display().to_string(),
        "--pages".to_owned(),
    ];
    args.extend(specs.iter().cloned());
    args.push("--".to_owned());
    args.push(output.display().to_string());
    args
}

fn run_qpdf(args: &[String]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[String]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assert_command_parity(label: &str, qpdf: &Output, flpdf: &Output) {
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "{label}: exit status"
    );
    assert_eq!(flpdf.stdout, qpdf.stdout, "{label}: stdout");
    assert_eq!(flpdf.stderr, qpdf.stderr, "{label}: stderr");
    output_success(&format!("qpdf {label}"), qpdf);
    output_success(&format!("flpdf {label}"), flpdf);
}

fn json_output(path: &Path, flpdf: bool) -> Value {
    let args = [
        "--json=2",
        "--json-key=qpdf",
        "--json-stream-data=inline",
        path.to_str().expect("UTF-8 test path"),
    ]
    .map(str::to_owned);
    let output = if flpdf {
        run_flpdf(&args)
    } else {
        run_qpdf(&args)
    };
    output_success(if flpdf { "flpdf JSON" } else { "qpdf JSON" }, &output);
    assert!(output.stderr.is_empty(), "JSON stderr: {:?}", output.stderr);
    serde_json::from_slice(&output.stdout).expect("parse qpdf JSON")
}

fn assert_json_map_matches_qpdf(label: &str, qpdf_path: &Path, flpdf_path: &Path) -> Value {
    let qpdf_json = json_output(qpdf_path, false);
    let flpdf_json = json_output(flpdf_path, true);
    assert_eq!(
        flpdf_json["qpdf"][0], qpdf_json["qpdf"][0],
        "{label}: qpdf document metadata must match"
    );
    assert_eq!(
        flpdf_json["qpdf"][1], qpdf_json["qpdf"][1],
        "{label}: the complete qpdf object map and inline stream data must match"
    );
    qpdf_json
}

fn value_for_reference<'a>(objects: &'a Value, value: &'a Value) -> &'a Value {
    if let Some(reference) = value.as_str().filter(|value| value.ends_with(" R")) {
        let key = format!("obj:{reference}");
        let entry = objects
            .get(&key)
            .unwrap_or_else(|| panic!("qpdf JSON object map is missing {key}"));
        entry
            .get("value")
            .or_else(|| entry.get("stream").and_then(|stream| stream.get("dict")))
            .unwrap_or_else(|| panic!("qpdf JSON object {key} has no value or stream dictionary"))
    } else {
        value
    }
}

fn object_entry<'a>(objects: &'a Value, value: &Value) -> &'a Value {
    let reference = value
        .as_str()
        .filter(|value| value.ends_with(" R"))
        .expect("indirect object reference");
    let key = format!("obj:{reference}");
    objects
        .get(&key)
        .unwrap_or_else(|| panic!("qpdf JSON object map is missing {key}"))
}

fn catalog_ocg_names(json: &Value) -> Vec<String> {
    let objects = &json["qpdf"][1];
    let root = value_for_reference(objects, &objects["trailer"]["value"]["/Root"]);
    let ocproperties = value_for_reference(objects, &root["/OCProperties"]);
    ocproperties["/OCGs"]
        .as_array()
        .expect("Catalog /OCProperties /OCGs array")
        .iter()
        .map(|reference| {
            value_for_reference(objects, reference)["/Name"]
                .as_str()
                .expect("OCG /Name string")
                .to_owned()
        })
        .collect()
}

fn expected_primary_ocg_names() -> Vec<String> {
    PRIMARY_OCG_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

fn assert_secondary_page_graph(json: &Value) {
    let objects = &json["qpdf"][1];
    let root = value_for_reference(objects, &objects["trailer"]["value"]["/Root"]);
    let pages = value_for_reference(objects, &root["/Pages"]);
    let kids = pages["/Kids"]
        .as_array()
        .expect("merged /Pages /Kids array");
    assert_eq!(
        kids.len(),
        2,
        "the primary and selected secondary pages remain"
    );

    let secondary_page = value_for_reference(objects, &kids[1]);
    let resources = value_for_reference(objects, &secondary_page["/Resources"]);
    let properties = value_for_reference(objects, &resources["/Properties"]);
    let secondary_ocg_ref = &properties["/OC_B"];
    assert_eq!(
        value_for_reference(objects, secondary_ocg_ref)["/Name"],
        "u:Secondary layer",
        "the selected secondary page keeps its optional-content property reference"
    );

    let contents = object_entry(objects, &secondary_page["/Contents"]);
    assert_eq!(
        contents["stream"]["data"], SECONDARY_CONTENT_BASE64,
        "inline BDC and Form invocation bytes remain in the selected page stream"
    );

    let annots = secondary_page["/Annots"]
        .as_array()
        .expect("page /Annots array");
    let annotation = value_for_reference(objects, &annots[0]);
    let annotation_ocmd = value_for_reference(objects, &annotation["/OC"]);
    assert_eq!(annotation_ocmd["/Type"], "/OCMD");
    let annotation_ve = annotation_ocmd["/VE"].as_array().expect("annotation /VE");
    assert_eq!(annotation_ve[0], "/And");
    assert_eq!(annotation_ve[1], *secondary_ocg_ref);
    assert_eq!(annotation_ve[2][0], "/Not");
    assert_eq!(annotation_ve[2][1], *secondary_ocg_ref);
    let annotation_ve_metadata = value_for_reference(objects, &annotation_ve[3]);
    assert_eq!(
        annotation_ve_metadata["/Tag"],
        "u:opaque annotation metadata"
    );
    assert_eq!(annotation_ve_metadata["/Ref"], *secondary_ocg_ref);

    let xobjects = value_for_reference(objects, &resources["/XObject"]);
    let form_entry = object_entry(objects, &xobjects["/Fm"]);
    let form_dict = &form_entry["stream"]["dict"];
    let form_ocmd = value_for_reference(objects, &form_dict["/OC"]);
    assert_eq!(form_ocmd["/Type"], "/OCMD");
    let form_ve = value_for_reference(objects, &form_ocmd["/VE"]);
    assert_eq!(form_ve["/Operator"], "/Not");
    assert_eq!(form_ve["/Operand"], *secondary_ocg_ref);
    assert_eq!(form_ve["/Metadata"]["/Tag"], "u:opaque Form metadata");
    assert_eq!(form_ve["/Metadata"]["/Ref"], *secondary_ocg_ref);
    assert_eq!(
        form_entry["stream"]["data"], SECONDARY_FORM_BASE64,
        "Form-XObject stream bytes remain in the selected page graph"
    );
}

fn assert_content_ocg_graph_and_streams(json: &Value, expected_stream_data: &[&str]) {
    let objects = &json["qpdf"][1];
    let root = value_for_reference(objects, &objects["trailer"]["value"]["/Root"]);
    let pages = value_for_reference(objects, &root["/Pages"]);
    let page = value_for_reference(objects, &pages["/Kids"][0]);
    let resources = value_for_reference(objects, &page["/Resources"]);
    let properties = value_for_reference(objects, &resources["/Properties"]);
    let ocg = value_for_reference(objects, &properties["/LayerA"]);
    assert_eq!(ocg["/Name"], "u:Primary layer");

    let content_references: Vec<&Value> = match page["/Contents"].as_array() {
        Some(contents) => contents.iter().collect(),
        None => vec![&page["/Contents"]],
    };
    assert_eq!(
        content_references.len(),
        expected_stream_data.len(),
        "content stream shape after the page transformation"
    );
    for (reference, expected) in content_references.iter().zip(expected_stream_data) {
        let stream = object_entry(objects, reference);
        assert_eq!(
            stream["stream"]["data"].as_str(),
            Some(*expected),
            "marked-content BDC/EMC operands and stream bytes"
        );
    }
}

fn assert_content_transform_matches_qpdf(option: &str, expected_stream_data: &[&str], label: &str) {
    if !require_qpdf() {
        return;
    }
    let input = fixture("ocproperties-content-transform.pdf");
    assert!(input.is_file(), "missing OCG content fixture: {input:?}");
    let directory = tempfile::tempdir().expect("temporary output directory");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    qpdf_check(&input);

    let qpdf_args = content_transform_args(&input, option, &qpdf_output);
    let flpdf_args = content_transform_args(&input, option, &flpdf_output);
    let qpdf = run_qpdf(&qpdf_args);
    let flpdf = run_flpdf(&flpdf_args);
    assert_command_parity(label, &qpdf, &flpdf);
    let json = assert_output_pair(label, &qpdf_output, &flpdf_output);
    assert_content_ocg_graph_and_streams(&json, expected_stream_data);
}

fn assert_output_pair(label: &str, qpdf_output: &Path, flpdf_output: &Path) -> Value {
    qpdf_check(qpdf_output);
    qpdf_check(flpdf_output);
    assert_eq!(
        std::fs::read(flpdf_output).expect("read flpdf output"),
        std::fs::read(qpdf_output).expect("read qpdf output"),
        "{label}: output bytes must match with compression disabled"
    );
    assert_json_map_matches_qpdf(label, qpdf_output, flpdf_output)
}

#[test]
fn ordinary_rewrite_keeps_primary_used_and_unused_optional_content() {
    if !require_qpdf() {
        return;
    }
    let input = fixture("ocproperties-primary-used-unused.pdf");
    assert!(input.is_file(), "missing OCG fixture: {input:?}");
    let directory = tempfile::tempdir().expect("temporary output directory");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");

    qpdf_check(&input);
    let qpdf_args = writer_args(&input, &qpdf_output);
    let flpdf_args = writer_args(&input, &flpdf_output);
    let qpdf = run_qpdf(&qpdf_args);
    let flpdf = run_flpdf(&flpdf_args);
    assert_command_parity("ordinary rewrite", &qpdf, &flpdf);
    let qpdf_json = assert_output_pair("ordinary rewrite", &qpdf_output, &flpdf_output);
    assert_eq!(
        catalog_ocg_names(&qpdf_json),
        expected_primary_ocg_names(),
        "an OCG unused by page content remains while reachable from /OCProperties"
    );
}

#[test]
fn primary_page_extraction_keeps_used_and_unused_catalog_ocgs() {
    if !require_qpdf() {
        return;
    }
    let input = fixture("ocproperties-primary-used-unused.pdf");
    assert!(input.is_file(), "missing OCG fixture: {input:?}");
    let directory = tempfile::tempdir().expect("temporary output directory");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    qpdf_check(&input);

    let specs = [".".to_owned(), "1".to_owned()];
    let qpdf_args = page_operation_args(&input, &specs, &qpdf_output);
    let flpdf_args = page_operation_args(&input, &specs, &flpdf_output);
    let qpdf = run_qpdf(&qpdf_args);
    let flpdf = run_flpdf(&flpdf_args);
    assert_command_parity("primary page extraction", &qpdf, &flpdf);
    let qpdf_json = assert_output_pair("primary page extraction", &qpdf_output, &flpdf_output);
    assert_eq!(
        catalog_ocg_names(&qpdf_json),
        expected_primary_ocg_names(),
        "page selection does not prune /OCGs unused by selected content but reachable from the primary Catalog"
    );
}

#[test]
fn multi_source_pages_copy_secondary_oc_graph_without_catalog_union() {
    if !require_qpdf() {
        return;
    }
    let primary = fixture("ocproperties-primary-used-unused.pdf");
    let secondary = fixture("ocproperties-secondary-page-graph.pdf");
    assert!(
        primary.is_file(),
        "missing primary OCG fixture: {primary:?}"
    );
    assert!(
        secondary.is_file(),
        "missing secondary OCG fixture: {secondary:?}"
    );
    let directory = tempfile::tempdir().expect("temporary output directory");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    qpdf_check(&primary);
    qpdf_check(&secondary);

    let specs = [
        ".".to_owned(),
        secondary.to_str().expect("UTF-8 fixture path").to_owned(),
        "1".to_owned(),
    ];
    let qpdf_args = page_operation_args(&primary, &specs, &qpdf_output);
    let flpdf_args = page_operation_args(&primary, &specs, &flpdf_output);
    let qpdf = run_qpdf(&qpdf_args);
    let flpdf = run_flpdf(&flpdf_args);
    assert_command_parity("A-primary/B-secondary page merge", &qpdf, &flpdf);
    let qpdf_json = assert_output_pair(
        "A-primary/B-secondary page merge",
        &qpdf_output,
        &flpdf_output,
    );
    assert_eq!(
        catalog_ocg_names(&qpdf_json),
        expected_primary_ocg_names(),
        "this qpdf command retains the primary Catalog optional-content graph and does not union the secondary Catalog"
    );
    assert_secondary_page_graph(&qpdf_json);
}

#[test]
fn normalize_content_preserves_marked_content_and_properties() {
    assert_content_transform_matches_qpdf(
        "--normalize-content=y",
        &["L09DIC9MYXllckEgQkRDCjAgMCAyNSAyNSByZSBmCg==", "RU1DCg=="],
        "normalize content with optional-content BDC",
    );
}

#[test]
fn coalesce_contents_preserves_marked_content_and_properties() {
    assert_content_transform_matches_qpdf(
        "--coalesce-contents",
        &["L09DIC9MYXllckEgQkRDDTAgMCAyNSAyNSByZSBmDQpFTUMN"],
        "coalesce contents with optional-content BDC",
    );
}
