//! CLI coverage for qpdf's `--optimize-images` transformation.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::path::Path;

fn build_raw_grayscale_image_pdf(width: usize, height: usize) -> Vec<u8> {
    build_raw_image_pdf(width, height, "DeviceGray", 1)
}

fn build_raw_image_pdf(
    width: usize,
    height: usize,
    colorspace: &str,
    components: usize,
) -> Vec<u8> {
    let pixels = vec![128u8; width * height * components];
    let content = b"q 200 0 0 200 0 0 cm /Im1 Do Q\n";
    let image_dictionary = format!(
        "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /{colorspace} /BitsPerComponent 8 /Length {} >>",
        pixels.len()
    );
    let content_dictionary = format!("<< /Length {} >>", content.len());
    let objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_object(content_dictionary.as_bytes(), content)),
        (5, stream_object(image_dictionary.as_bytes(), &pixels)),
    ];

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (number, body) in objects {
        offsets.push((number, pdf.len()));
        pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        pdf.extend_from_slice(&body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for (_, offset) in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn raw_stream(path: &Path, object: u32) -> Vec<u8> {
    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([&format!("--show-object={object}"), "--raw-stream-data"])
        .arg(path)
        .output()
        .expect("run flpdf raw stream");
    assert!(output.status.success(), "raw stream failed: {output:?}");
    output.stdout
}

fn stream_object(dictionary: &[u8], data: &[u8]) -> Vec<u8> {
    let mut object = dictionary.to_vec();
    object.extend_from_slice(b"\nstream\n");
    object.extend_from_slice(data);
    object.extend_from_slice(b"\nendstream");
    object
}

fn first_image(path: &Path) -> Value {
    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--json=2", "--json-key=pages"])
        .arg(path)
        .output()
        .expect("run flpdf JSON");
    assert!(output.status.success(), "JSON failed: {output:?}");
    let json: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    json["pages"][0]["images"][0].clone()
}

fn image_filters(path: &Path) -> Vec<String> {
    first_image(path)["filter"]
        .as_array()
        .expect("image filter array")
        .iter()
        .map(|filter| filter.as_str().expect("filter name").to_owned())
        .collect()
}

fn image_object(path: &Path) -> u32 {
    first_image(path)["object"]
        .as_str()
        .and_then(|object| object.split_whitespace().next())
        .and_then(|number| number.parse().ok())
        .expect("image object reference")
}

#[test]
fn rewrite_subcommand_preserves_image_dictionary_and_clears_decode_parms() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("output.pdf");
    std::fs::write(&input, build_raw_grayscale_image_pdf(200, 200)).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["rewrite", "--static-id", "--optimize-images"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let image = first_image(&output);
    assert_eq!(image["filter"], serde_json::json!(["/DCTDecode"]));
    assert_eq!(image["decodeparms"], serde_json::json!([null]));
    assert_eq!(image["width"], 200);
    assert_eq!(image["height"], 200);
    assert_eq!(image["colorspace"], "/DeviceGray");
}

#[test]
fn optimize_images_honors_inclusive_minimum_width() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("output.pdf");
    std::fs::write(&input, build_raw_grayscale_image_pdf(200, 200)).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--static-id",
            "--optimize-images",
            "--oi-min-width=200",
            "--oi-min-height=0",
            "--oi-min-area=0",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(!image_filters(&output).contains(&"/DCTDecode".to_owned()));
}

#[test]
fn optimize_images_uses_one_by_one_sampling_for_cmyk() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("output.pdf");
    std::fs::write(&input, build_raw_image_pdf(200, 200, "DeviceCMYK", 4)).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--static-id",
            "--optimize-images",
            "--oi-min-width=0",
            "--oi-min-height=0",
            "--oi-min-area=0",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    let jpeg = raw_stream(&output, image_object(&output));
    let expected_sof = [
        0xff, 0xc0, 0x00, 0x14, 0x08, 0x00, 0xc8, 0x00, 0xc8, 0x04, 0x43, 0x11, 0x00, 0x4d, 0x11,
        0x00, 0x59, 0x11, 0x00, 0x4b, 0x11, 0x00,
    ];
    assert!(
        jpeg.windows(expected_sof.len())
            .any(|window| window == expected_sof),
        "CMYK JPEG should retain qpdf's 1x1 sampling factors"
    );
}

#[test]
fn optimize_images_runs_after_page_selection() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("output.pdf");
    std::fs::write(&input, build_raw_grayscale_image_pdf(200, 200)).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--static-id", "--optimize-images"])
        .arg(&input)
        .args(["--pages", ".", "--"])
        .arg(&output)
        .assert()
        .success();

    assert_eq!(image_filters(&output), vec!["/DCTDecode"]);
}

#[test]
fn optimize_images_is_applied_before_json_pages_output() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    std::fs::write(&input, build_raw_grayscale_image_pdf(200, 200)).expect("write input");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--optimize-images", "--json=2", "--json-key=pages"])
        .arg(&input)
        .output()
        .expect("run JSON");
    assert!(output.status.success(), "JSON failed: {output:?}");
    let json: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(
        json["pages"][0]["images"][0]["filter"],
        serde_json::json!(["/DCTDecode"])
    );
}

#[test]
fn optimize_images_recompresses_an_eligible_raw_image() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("output.pdf");
    std::fs::write(&input, build_raw_grayscale_image_pdf(200, 200)).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--static-id", "--optimize-images", "--verbose"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "optimizing image reduces size from",
        ));

    assert_eq!(image_filters(&output), vec!["/DCTDecode"]);
}

#[test]
fn rewrite_subcommand_applies_optimize_images_with_page_selection() {
    // The rewrite subcommand's page-operation guard used to reject
    // --optimize-images unconditionally, even though run_page_extraction
    // (called for `--pages`) already threads image options through via
    // `cmd.optimize_images.then_some(image_options)` — the same way the
    // top-level `--pages` route already does (see
    // optimize_images_runs_after_page_selection above).
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("output.pdf");
    std::fs::write(&input, build_raw_grayscale_image_pdf(200, 200)).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg("rewrite")
        .arg(&input)
        .arg(&output)
        .args(["--static-id", "--optimize-images", "--pages"])
        .arg(&input)
        .args(["1", "--"])
        .assert()
        .success();

    assert_eq!(image_filters(&output), vec!["/DCTDecode"]);
}

#[test]
fn optimize_images_conflicts_with_check() {
    // --check's inspection dispatch never reaches a rewrite path that
    // consumes the computed image options, so without this clap-level
    // conflict the flag would be silently accepted and dropped.
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    std::fs::write(&input, build_raw_grayscale_image_pdf(200, 200)).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--optimize-images", "--check"])
        .arg(&input)
        .assert()
        .failure()
        .code(2);
}

#[test]
fn optimize_images_conflicts_with_remove_attachment() {
    // run_remove_attachment (and the other attachment-mutation dispatch
    // branches) call their dedicated writers without ever consuming
    // top_level_image_options, so the same silent-drop risk applies here.
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("output.pdf");
    std::fs::write(&input, build_raw_grayscale_image_pdf(200, 200)).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--optimize-images", "--remove-attachment=missing"])
        .arg(&input)
        .arg(&output)
        .assert()
        .failure()
        .code(2);
}
