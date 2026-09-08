//! Top-level qpdf-compatible inline-image transformation coverage.

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;

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

fn stream_object(dictionary: &[u8], data: &[u8]) -> Vec<u8> {
    let mut object = dictionary.to_vec();
    object.extend_from_slice(b"\nstream\n");
    object.extend_from_slice(data);
    object.extend_from_slice(b"\nendstream");
    object
}

fn inline_image_pdf() -> Vec<u8> {
    let mut content = b"q 200 0 0 200 0 0 cm BI /W 2 /H 2 /CS /G /BPC 8 ID\n".to_vec();
    content.extend_from_slice(&[0, 64, 128, 255]);
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

#[test]
fn top_level_externalize_inline_images_converts_inline_image_to_xobject() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("input.pdf");
    let output = directory.path().join("output.pdf");
    std::fs::write(&input, inline_image_pdf()).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--externalize-inline-images",
            "--ii-min-bytes=0",
            "--static-id",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let image = &pages_json(&output)["pages"][0]["images"][0];
    assert_eq!(image["name"], "/IIm1");
    assert_eq!(image["width"], 2);
    assert_eq!(image["height"], 2);
}

#[test]
fn top_level_externalize_inline_images_honors_inclusive_payload_threshold() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("input.pdf");
    let at_limit = directory.path().join("at-limit.pdf");
    let above_limit = directory.path().join("above-limit.pdf");
    std::fs::write(&input, inline_image_pdf()).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--externalize-inline-images",
            "--ii-min-bytes=5",
            "--static-id",
        ])
        .arg(&input)
        .arg(&at_limit)
        .assert()
        .success();
    assert_eq!(
        pages_json(&at_limit)["pages"][0]["images"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--externalize-inline-images",
            "--ii-min-bytes=6",
            "--static-id",
        ])
        .arg(&input)
        .arg(&above_limit)
        .assert()
        .success();
    assert!(pages_json(&above_limit)["pages"][0]["images"]
        .as_array()
        .expect("image array")
        .is_empty());
}

#[test]
fn explicit_externalization_overrides_keep_inline_images() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("input.pdf");
    let output = directory.path().join("output.pdf");
    std::fs::write(&input, inline_image_pdf()).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--externalize-inline-images",
            "--keep-inline-images",
            "--ii-min-bytes=0",
            "--static-id",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert_eq!(
        pages_json(&output)["pages"][0]["images"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn rewrite_subcommand_accepts_externalize_inline_images() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("input.pdf");
    let output = directory.path().join("output.pdf");
    std::fs::write(&input, inline_image_pdf()).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "rewrite",
            "--externalize-inline-images",
            "--ii-min-bytes=0",
            "--static-id",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert_eq!(
        pages_json(&output)["pages"][0]["images"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
