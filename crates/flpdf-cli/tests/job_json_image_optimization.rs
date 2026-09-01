//! CLI coverage for qpdf job JSON image transformations.

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use std::process::Command as ProcessCommand;

fn stream_object(dictionary: &[u8], data: &[u8]) -> Vec<u8> {
    let mut object = dictionary.to_vec();
    object.extend_from_slice(b"\nstream\n");
    object.extend_from_slice(data);
    object.extend_from_slice(b"\nendstream");
    object
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

fn raw_image_pdf(width: usize, height: usize) -> Vec<u8> {
    let pixels = vec![128u8; width * height];
    let content = b"q 200 0 0 200 0 0 cm /Im1 Do Q\n";
    assemble_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        ),
        (
            4,
            stream_object(
                format!("<< /Length {} >>", content.len()).as_bytes(),
                content,
            ),
        ),
        (
            5,
            stream_object(
                format!(
                    "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {} >>",
                    pixels.len()
                )
                .as_bytes(),
                &pixels,
            ),
        ),
    ])
}

fn inline_image_pdf(width: usize, height: usize) -> Vec<u8> {
    let pixels = vec![128u8; width * height];
    let mut content =
        format!("q 200 0 0 200 0 0 cm BI /W {width} /H {height} /CS /G /BPC 8 ID\n").into_bytes();
    content.extend_from_slice(&pixels);
    content.extend_from_slice(b"\nEI Q\n");
    assemble_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>".to_vec(),
        ),
        (
            4,
            stream_object(
                format!("<< /Length {} >>", content.len()).as_bytes(),
                &content,
            ),
        ),
    ])
}

fn pages_json(path: &Path) -> Value {
    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--json=2", "--json-key=pages"])
        .arg(path)
        .output()
        .expect("run flpdf JSON");
    assert!(output.status.success(), "JSON failed: {output:?}");
    serde_json::from_slice(&output.stdout).expect("valid JSON")
}

fn image_filters(path: &Path) -> Vec<String> {
    pages_json(path)["pages"][0]["images"]
        .as_array()
        .expect("image array")
        .iter()
        .flat_map(|image| {
            image["filter"]
                .as_array()
                .expect("filter array")
                .iter()
                .map(|filter| filter.as_str().expect("filter name").to_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn qpdf_available() -> bool {
    ProcessCommand::new("/usr/bin/qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn qpdf_pages_json(path: &Path) -> Value {
    let output = ProcessCommand::new("/usr/bin/qpdf")
        .args(["--json", "--json-key=pages"])
        .arg(path)
        .output()
        .expect("run qpdf JSON");
    assert!(output.status.success(), "qpdf JSON failed: {output:?}");
    serde_json::from_slice(&output.stdout).expect("valid qpdf JSON")
}

#[test]
fn job_json_optimize_images_recompresses_an_eligible_image() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(directory.path().join("input.pdf"), raw_image_pdf(200, 200))
        .expect("write input");
    std::fs::write(
        directory.path().join("job.json"),
        serde_json::json!({
            "inputFile": "input.pdf",
            "outputFile": "output.pdf",
            "optimizeImages": "",
            "oiMinWidth": "0",
            "oiMinHeight": "0",
            "oiMinArea": "0",
            "staticId": ""
        })
        .to_string(),
    )
    .expect("write job");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .success();

    assert_eq!(
        image_filters(&directory.path().join("output.pdf")),
        ["/DCTDecode"]
    );
}

#[test]
fn job_json_externalize_inline_images_uses_the_canonical_externalizer() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        directory.path().join("input.pdf"),
        inline_image_pdf(200, 200),
    )
    .expect("write input");
    std::fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"output.pdf","externalizeInlineImages":"","iiMinBytes":"0","staticId":""}"#,
    )
    .expect("write job");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .success();

    assert_eq!(
        image_filters(&directory.path().join("output.pdf")),
        ["/FlateDecode"]
    );
    assert_eq!(
        pages_json(&directory.path().join("output.pdf"))["pages"][0]["images"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn job_json_optimize_images_honors_keep_inline_images() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        directory.path().join("input.pdf"),
        inline_image_pdf(200, 200),
    )
    .expect("write input");
    std::fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"output.pdf","optimizeImages":"","keepInlineImages":"","iiMinBytes":"0","oiMinWidth":"0","oiMinHeight":"0","oiMinArea":"0","staticId":""}"#,
    )
    .expect("write job");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .success();

    assert!(
        pages_json(&directory.path().join("output.pdf"))["pages"][0]["images"]
            .as_array()
            .expect("image array")
            .is_empty()
    );
}

#[test]
fn job_json_explicit_externalization_overrides_keep_inline_images() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        directory.path().join("input.pdf"),
        inline_image_pdf(200, 200),
    )
    .expect("write input");
    std::fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"output.pdf","externalizeInlineImages":"","optimizeImages":"","keepInlineImages":"","iiMinBytes":"0","oiMinWidth":"0","oiMinHeight":"0","oiMinArea":"0","staticId":""}"#,
    )
    .expect("write job");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .success();

    assert_eq!(
        image_filters(&directory.path().join("output.pdf")),
        ["/DCTDecode"]
    );
}

#[test]
fn job_json_externalization_honors_the_inclusive_minimum_payload() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        directory.path().join("input.pdf"),
        inline_image_pdf(200, 200),
    )
    .expect("write input");
    std::fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"output.pdf","externalizeInlineImages":"","iiMinBytes":"40000","staticId":""}"#,
    )
    .expect("write job");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .success();

    assert_eq!(
        image_filters(&directory.path().join("output.pdf")),
        ["/FlateDecode"]
    );
}

#[test]
fn job_json_optimize_images_honors_inclusive_minimum_width() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(directory.path().join("input.pdf"), raw_image_pdf(200, 200))
        .expect("write input");
    std::fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"input.pdf","outputFile":"output.pdf","optimizeImages":"","oiMinWidth":"200","oiMinHeight":"0","oiMinArea":"0","staticId":""}"#,
    )
    .expect("write job");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .success();

    assert_eq!(
        image_filters(&directory.path().join("output.pdf")),
        ["/FlateDecode"]
    );
}

#[test]
fn job_json_image_options_match_qpdf_when_available() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(directory.path().join("input.pdf"), raw_image_pdf(200, 200))
        .expect("write input");
    let job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "qpdf.pdf",
        "optimizeImages": "",
        "oiMinWidth": "0",
        "oiMinHeight": "0",
        "oiMinArea": "0",
        "staticId": ""
    });
    std::fs::write(directory.path().join("qpdf.json"), job.to_string()).expect("write qpdf job");
    let flpdf_job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "flpdf.pdf",
        "optimizeImages": "",
        "oiMinWidth": "0",
        "oiMinHeight": "0",
        "oiMinArea": "0",
        "staticId": ""
    });
    std::fs::write(directory.path().join("flpdf.json"), flpdf_job.to_string())
        .expect("write flpdf job");

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=qpdf.json")
        .output()
        .expect("run qpdf");
    assert!(qpdf.status.success(), "qpdf job failed: {qpdf:?}");
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .arg("--job-json-file=flpdf.json")
        .assert()
        .success();

    assert_eq!(
        image_filters(&directory.path().join("flpdf.pdf")),
        ["/DCTDecode"]
    );
}

#[test]
fn job_json_inline_image_options_match_qpdf_when_available() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        directory.path().join("input.pdf"),
        inline_image_pdf(200, 200),
    )
    .expect("write input");
    let job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "qpdf.pdf",
        "externalizeInlineImages": "",
        "iiMinBytes": "0",
        "staticId": ""
    });
    std::fs::write(directory.path().join("qpdf.json"), job.to_string()).expect("write qpdf job");
    let flpdf_job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "flpdf.pdf",
        "externalizeInlineImages": "",
        "iiMinBytes": "0",
        "staticId": ""
    });
    std::fs::write(directory.path().join("flpdf.json"), flpdf_job.to_string())
        .expect("write flpdf job");

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=qpdf.json")
        .output()
        .expect("run qpdf");
    assert!(qpdf.status.success(), "qpdf job failed: {qpdf:?}");
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .arg("--job-json-file=flpdf.json")
        .assert()
        .success();

    assert_eq!(
        pages_json(&directory.path().join("flpdf.pdf"))["pages"][0]["images"]
            .as_array()
            .expect("flpdf image array")
            .len(),
        qpdf_pages_json(&directory.path().join("qpdf.pdf"))["pages"][0]["images"]
            .as_array()
            .expect("qpdf image array")
            .len()
    );
}

#[test]
fn job_json_image_options_reject_wrong_types_at_the_json_boundary() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(directory.path().join("input.pdf"), raw_image_pdf(200, 200))
        .expect("write input");
    for (name, option) in [("optimizeImages", "42"), ("keepInlineImages", "true")] {
        let job =
            format!(r#"{{"inputFile":"input.pdf","outputFile":"output.pdf","{name}":{option}}}"#);
        std::fs::write(directory.path().join("job.json"), job).expect("write job");
        Command::cargo_bin("flpdf")
            .expect("flpdf binary")
            .current_dir(directory.path())
            .arg("--job-json-file=job.json")
            .assert()
            .code(2)
            .stderr(predicates::str::contains(format!(
                "JSON handler: value at .{name} is not of expected type"
            )));
    }
}
