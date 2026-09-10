use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

fn production_main_source() -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("main.rs"),
    )
    .expect("read flpdf-cli main source")
}

fn assemble_pdf(objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0usize; objects.len() + 1];
    for (number, body) in objects {
        offsets[*number as usize] = bytes.len();
        bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.into_iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

fn inline_image_pdf(width: usize, height: usize) -> Vec<u8> {
    // qpdf's inline-image reader consumes the separator byte before EI as part
    // of the raw buffer, so leave one byte for that delimiter.
    let pixels = vec![128u8; width * height - 1];
    let mut content =
        format!("q 200 0 0 200 0 0 cm BI /W {width} /H {height} /CS /G /BPC 8 ID\n").into_bytes();
    content.extend_from_slice(&pixels);
    content.extend_from_slice(b" EI Q\n");
    assemble_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>".to_vec(),
        ),
        (
            4,
            {
                let mut object = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
                object.extend_from_slice(&content);
                object.extend_from_slice(b"endstream");
                object
            },
        ),
    ])
}

fn qpdf_available() -> bool {
    ProcessCommand::new("/usr/bin/qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn image_count(path: &Path) -> usize {
    let output = ProcessCommand::new("/usr/bin/qpdf")
        .args(["--json=2"])
        .arg(path)
        .output()
        .expect("run qpdf JSON");
    assert!(output.status.success(), "qpdf JSON failed: {output:?}");
    let json: Value = serde_json::from_slice(&output.stdout).expect("valid qpdf JSON");
    fn count(value: &Value) -> usize {
        match value {
            Value::Object(entries) => entries
                .iter()
                .map(|(key, value)| {
                    usize::from(key == "/Subtype" && value.as_str() == Some("/Image"))
                        + count(value)
                })
                .sum(),
            Value::Array(values) => values.iter().map(count).sum(),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 0,
        }
    }
    count(&json)
}

#[test]
fn ordinary_rewrite_uses_the_canonical_job_transform_and_output_routes() {
    let source = production_main_source();
    let ordinary_rewrite = source
        .split_once("fn run_rewrite_with_qpdf_job")
        .and_then(|(_, tail)| {
            tail.split_once("// Page operations: ")
                .map(|(body, _)| body)
        })
        .expect("ordinary rewrite route");

    assert!(
        ordinary_rewrite.contains("job.create_qpdf()"),
        "ordinary rewrite input creation and transformations must be owned by QPDFJob"
    );
    assert!(
        ordinary_rewrite.contains("job.write_qpdf("),
        "ordinary rewrite output must be owned by QPDFJob"
    );
    for forbidden in [
        "apply_image_transformations(",
        "flpdf::handle_under_overlay(",
        "AcroFormDocumentHelper::new",
        "PageDocumentHelper::new",
        "PageObjectHelper::new",
        "write_with_pdf_writer(",
    ] {
        assert!(
            !ordinary_rewrite.contains(forbidden),
            "ordinary rewrite retains a direct route: {forbidden}"
        );
    }
}

#[test]
fn direct_rewrite_overlay_images_match_qpdf_after_externalization() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("temporary directory");
    let primary = directory.path().join("primary.pdf");
    let overlay = directory.path().join("overlay.pdf");
    let flpdf_output = directory.path().join("flpdf-output.pdf");
    let qpdf_output = directory.path().join("qpdf-output.pdf");
    let input = inline_image_pdf(200, 200);
    fs::write(&primary, &input).expect("write primary");
    fs::write(&overlay, &input).expect("write overlay");

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .args([
            "--static-id",
            "--optimize-images",
            "--ii-min-bytes=0",
            "--overlay",
        ])
        .arg(&overlay)
        .arg("--")
        .arg(&primary)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf overlay image transformation");
    assert!(qpdf.status.success(), "qpdf failed: {qpdf:?}");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--static-id",
            "--optimize-images",
            "--ii-min-bytes=0",
            "--overlay",
        ])
        .arg(&overlay)
        .arg("--")
        .arg(&primary)
        .arg(&flpdf_output)
        .assert()
        .success();

    let qpdf_images = image_count(&qpdf_output);
    let flpdf_images = image_count(&flpdf_output);
    assert!(qpdf_images > 0, "probe must contain externalized images");
    assert_eq!(
        flpdf_images, qpdf_images,
        "overlay image traversal diverged"
    );

    #[cfg(feature = "qpdf-zlib-compat")]
    assert_eq!(
        fs::read(&flpdf_output).expect("read flpdf output"),
        fs::read(&qpdf_output).expect("read qpdf output"),
        "qpdf-zlib-compat overlay image rewrite must be byte-identical"
    );
}
