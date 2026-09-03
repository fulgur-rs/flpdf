//! Differential coverage for generic writer preservation of optional-content data.
//!
//! qpdf 11.9.0 has no `/OCProperties`-specific writer code. These tests pin the
//! behavior that matters here: a multi-config optional-content graph is reached
//! through the Catalog, and every indirect reference is remapped by the normal
//! plain-writer traversal.

use flpdf::{ObjectHandle, ObjectRef, ObjectStreamMode, Pdf};
use serde_json::{json, Value};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::{write_with_settings, PdfCanonicalTestExt, WriterTestSettings};

const EXPECTED_QPDF_VERSION: &str = "11.9.0";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn qpdf_available() -> bool {
    Command::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| {
                        line.trim() == format!("qpdf version {EXPECTED_QPDF_VERSION}")
                    })
        })
        .unwrap_or(false)
}

fn qpdf_rewrite(input: &Path, output: &Path) {
    let result = Command::new("qpdf")
        .args(["--static-id", "--object-streams=preserve"])
        .arg(input)
        .arg(output)
        .output()
        .expect("run qpdf standard rewrite");
    assert!(
        result.status.success(),
        "qpdf rewrite failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn qpdf_check(path: &Path) {
    let result = Command::new("qpdf")
        .arg("--check")
        .arg(path)
        .output()
        .expect("run qpdf --check");
    assert!(
        result.status.success(),
        "qpdf --check failed for {path:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn qpdf_json(path: &Path) -> Value {
    let result = Command::new("qpdf")
        .args(["--json-output=2", "--json-stream-data=none"])
        .arg(path)
        .arg("-")
        .output()
        .expect("run qpdf JSON output");
    assert!(
        result.status.success(),
        "qpdf JSON output failed for {path:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).expect("parse qpdf JSON output")
}

fn flpdf_rewrite(input: &Path) -> flpdf::Result<Vec<u8>> {
    let mut pdf = Pdf::open(BufReader::new(File::open(input)?))?;
    let settings = WriterTestSettings {
        static_id: true,
        object_streams: ObjectStreamMode::Preserve,
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &settings)?;
    Ok(output)
}

fn object_refs(handles: &[ObjectHandle]) -> Vec<Option<ObjectRef>> {
    handles.iter().map(ObjectHandle::object_ref).collect()
}

fn assert_flpdf_structure(output: &[u8]) -> flpdf::Result<()> {
    let mut pdf = Pdf::open(BufReader::new(Cursor::new(output.to_vec())))?;
    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));

    let catalog = pdf.resolve_canonical_object(ObjectRef::new(1, 0))?;
    assert_eq!(
        catalog.try_get_key(b"/OCProperties")?.object_ref(),
        Some(ObjectRef::new(2, 0))
    );

    let ocproperties = pdf.resolve_canonical_object(ObjectRef::new(2, 0))?;
    assert_eq!(
        object_refs(
            &ocproperties
                .try_get_key(b"/OCGs")?
                .try_get_array_as_vector()?
        ),
        vec![Some(ObjectRef::new(7, 0)), Some(ObjectRef::new(8, 0))]
    );
    assert_eq!(
        ocproperties.try_get_key(b"/D")?.object_ref(),
        Some(ObjectRef::new(6, 0))
    );
    assert_eq!(
        object_refs(
            &ocproperties
                .try_get_key(b"/Configs")?
                .try_get_array_as_vector()?
        ),
        vec![Some(ObjectRef::new(4, 0)), Some(ObjectRef::new(5, 0))]
    );

    let default_config = pdf.resolve_canonical_object(ObjectRef::new(6, 0))?;
    let order = default_config
        .try_get_key(b"/Order")?
        .try_get_array_as_vector()?;
    assert_eq!(order[0].object_ref(), Some(ObjectRef::new(7, 0)));
    assert_eq!(
        object_refs(&order[1].try_get_array_as_vector()?),
        vec![Some(ObjectRef::new(8, 0))]
    );
    Ok(())
}

fn rewritten_objects(json: &Value) -> &Value {
    &json["qpdf"][1]
}

#[test]
fn plain_rewrite_preserves_multi_config_ocproperties_and_remaps_all_references() -> flpdf::Result<()>
{
    let input = fixture("ocproperties-multiconfig.pdf");
    assert!(
        input.is_file(),
        "OCProperties fixture must exist: {input:?}"
    );
    let temporary = tempfile::tempdir()?;
    let flpdf_output = temporary.path().join("flpdf-output.pdf");

    let actual = flpdf_rewrite(&input)?;
    std::fs::write(&flpdf_output, &actual)?;
    assert_flpdf_structure(&actual)?;

    if !qpdf_available() {
        eprintln!(
            "qpdf {EXPECTED_QPDF_VERSION} is unavailable; skipping only the differential oracle"
        );
        return Ok(());
    }

    qpdf_check(&input);
    let qpdf_output = temporary.path().join("qpdf-output.pdf");
    qpdf_rewrite(&input, &qpdf_output);
    qpdf_check(&qpdf_output);
    qpdf_check(&flpdf_output);

    let qpdf_output_json = qpdf_json(&qpdf_output);
    let objects = rewritten_objects(&qpdf_output_json);
    assert_eq!(
        objects["obj:1 0 R"]["value"]["/OCProperties"], "2 0 R",
        "Catalog /OCProperties reference must follow Catalog-first renumbering"
    );
    assert_eq!(
        objects["obj:2 0 R"]["value"]["/OCGs"],
        json!(["7 0 R", "8 0 R"]),
        "/OCGs references must remap to the rewritten OCG objects"
    );
    assert_eq!(
        objects["obj:2 0 R"]["value"]["/D"], "6 0 R",
        "default /D config reference must be preserved and remapped"
    );
    assert_eq!(
        objects["obj:2 0 R"]["value"]["/Configs"],
        json!(["4 0 R", "5 0 R"]),
        "all alternate /Configs references must be preserved and remapped"
    );
    assert_eq!(
        objects["obj:6 0 R"]["value"]["/Order"],
        json!(["7 0 R", ["8 0 R"]]),
        "nested default /Order references must retain their structure"
    );

    assert_eq!(
        qpdf_json(&flpdf_output),
        qpdf_output_json,
        "flpdf's optional-content graph must match qpdf's rewritten graph"
    );
    assert_eq!(
        actual,
        std::fs::read(&qpdf_output)?,
        "plain rewrite must match qpdf --static-id for this stream-free fixture"
    );
    Ok(())
}
