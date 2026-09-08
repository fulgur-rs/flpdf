//! CLI coverage for qpdf's `--optimize-images` transformation.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::path::Path;
use std::process::Command as ProcessCommand;

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
    build_raw_image_pdf_with_pixels(width, height, colorspace, components, pixels)
}

fn build_raw_image_pdf_with_pixels(
    width: usize,
    height: usize,
    colorspace: &str,
    components: usize,
    pixels: Vec<u8>,
) -> Vec<u8> {
    assert_eq!(pixels.len(), width * height * components);
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

fn qpdf_11_9_available() -> bool {
    let output = ProcessCommand::new("/usr/bin/qpdf")
        .arg("--version")
        .output();
    output.is_ok_and(|output| {
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("11.9.0")
    })
}

fn qpdf_first_image(path: &Path) -> Value {
    let output = ProcessCommand::new("/usr/bin/qpdf")
        .args(["--json", "--json-key=pages"])
        .arg(path)
        .output()
        .expect("run qpdf JSON");
    assert!(output.status.success(), "qpdf JSON failed: {output:?}");
    let json: Value = serde_json::from_slice(&output.stdout).expect("valid qpdf JSON");
    json["pages"][0]["images"][0].clone()
}

fn qpdf_raw_stream(path: &Path, object: u32) -> Vec<u8> {
    let output = ProcessCommand::new("/usr/bin/qpdf")
        .args([&format!("--show-object={object}"), "--raw-stream-data"])
        .arg(path)
        .output()
        .expect("run qpdf raw stream");
    assert!(
        output.status.success(),
        "qpdf raw stream failed: {output:?}"
    );
    output.stdout
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
fn optimize_images_emits_qpdf_identical_jpeg_bytes_for_gray_rgb_and_cmyk() {
    if !qpdf_11_9_available() {
        return;
    }

    for (colorspace, components) in [("DeviceGray", 1), ("DeviceRGB", 3), ("DeviceCMYK", 4)] {
        for (pattern, pixels) in [
            ("uniform", vec![128u8; 200 * 200 * components]),
            (
                "structured",
                (0..200 * 200 * components)
                    .map(|index| ((index * 37 + index / 200 * 11) % 256) as u8)
                    .collect(),
            ),
        ] {
            let tempdir = tempfile::tempdir().expect("tempdir");
            let input = tempdir.path().join("input.pdf");
            let qpdf_output = tempdir.path().join("qpdf.pdf");
            let flpdf_output = tempdir.path().join("flpdf.pdf");
            std::fs::write(
                &input,
                build_raw_image_pdf_with_pixels(200, 200, colorspace, components, pixels),
            )
            .expect("write input");

            let qpdf = ProcessCommand::new("/usr/bin/qpdf")
                .args([
                    "--static-id",
                    "--optimize-images",
                    "--oi-min-width=0",
                    "--oi-min-height=0",
                    "--oi-min-area=0",
                ])
                .arg(&input)
                .arg(&qpdf_output)
                .output()
                .expect("run qpdf optimize-images");
            assert!(
                qpdf.status.success(),
                "qpdf optimize-images failed for {colorspace}/{pattern}: {qpdf:?}"
            );

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
                .arg(&flpdf_output)
                .assert()
                .success();

            let qpdf_image = qpdf_first_image(&qpdf_output);
            assert_eq!(
                qpdf_image["filter"],
                serde_json::json!(["/DCTDecode"]),
                "fixture was not optimized by qpdf for {colorspace}/{pattern}"
            );
            assert_eq!(
                image_filters(&flpdf_output),
                vec!["/DCTDecode"],
                "fixture was not optimized by flpdf for {colorspace}/{pattern}"
            );

            let qpdf_object = qpdf_image["object"]
                .as_str()
                .and_then(|object| object.split_whitespace().next())
                .and_then(|number| number.parse().ok())
                .expect("qpdf image object reference");
            let flpdf_object = image_object(&flpdf_output);
            assert_eq!(
                qpdf_raw_stream(&qpdf_output, qpdf_object),
                raw_stream(&flpdf_output, flpdf_object),
                "JPEG bytes differ from qpdf for {colorspace}/{pattern}"
            );
        }
    }
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
fn optimize_images_is_accepted_with_check_like_qpdf() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    std::fs::write(&input, build_raw_grayscale_image_pdf(200, 200)).expect("write input");

    let with_option = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--optimize-images", "--check"])
        .arg(&input)
        .assert()
        .success();
    let plain = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg("--check")
        .arg(&input)
        .assert()
        .success();

    // flpdf currently accepts the flag on this route and drops it, so the
    // check report is byte-for-byte the plain one. qpdf instead runs
    // `handleTransformations` before `writeQPDF` picks the inspection branch
    // (`QPDFJob.cc:474,484-491`), so its report reflects the transformed
    // document. Pin the accept-and-drop behaviour here; `flpdf-w2fk` tracks
    // wiring the transformation into the inspection routes.
    assert_eq!(
        with_option.get_output().stdout,
        plain.get_output().stdout,
        "the image option is currently dropped on the --check route"
    );
}

#[test]
fn externalize_inline_images_is_accepted_with_check_like_qpdf() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    std::fs::write(&input, build_raw_grayscale_image_pdf(200, 200)).expect("write input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--externalize-inline-images", "--check"])
        .arg(&input)
        .assert()
        .success();
}

#[test]
fn image_transform_options_are_accepted_with_inspection_modes_like_qpdf() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = tempdir.path().join("input.pdf");
    std::fs::write(&input, build_raw_grayscale_image_pdf(200, 200)).expect("write input");
    let inspection_modes = [
        vec!["--show-npages"],
        vec!["--show-pages"],
        vec!["--show-xref"],
        vec!["--show-linearization"],
        vec!["--show-encryption"],
        vec!["--show-object=trailer"],
    ];

    for image_option in ["--optimize-images", "--externalize-inline-images"] {
        for mode in &inspection_modes {
            let with_option = Command::cargo_bin("flpdf")
                .expect("flpdf binary")
                .arg(image_option)
                .args(mode)
                .arg(&input)
                .assert()
                .success();
            let plain = Command::cargo_bin("flpdf")
                .expect("flpdf binary")
                .args(mode)
                .arg(&input)
                .assert()
                .success();
            // Accept-and-drop: identical inspection output with and without
            // the image option. qpdf's own report differs because it runs the
            // transformation first (`QPDFJob.cc:474`); `flpdf-w2fk` tracks
            // closing that gap.
            assert_eq!(
                with_option.get_output().stdout,
                plain.get_output().stdout,
                "{image_option} is currently dropped on {mode:?}"
            );
        }
    }
}

#[test]
fn optimize_images_runs_with_remove_attachment_like_qpdf() {
    if !qpdf_11_9_available() {
        return;
    }
    let tempdir = tempfile::tempdir().expect("tempdir");
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    let qpdf_output = tempdir.path().join("qpdf.pdf");
    let flpdf_output = tempdir.path().join("flpdf.pdf");

    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .args(["--optimize-images", "--remove-attachment=attachment.txt"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf attachment/image combination");
    assert!(qpdf.status.success(), "qpdf combination failed: {qpdf:?}");

    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--optimize-images", "--remove-attachment=attachment.txt"])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .expect("run flpdf attachment/image combination");
    assert_eq!(flpdf.status.code(), qpdf.status.code());

    let qpdf_list = ProcessCommand::new("/usr/bin/qpdf")
        .args(["--list-attachments"])
        .arg(&qpdf_output)
        .output()
        .expect("list qpdf attachments");
    let flpdf_list = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--list-attachments"])
        .arg(&flpdf_output)
        .output()
        .expect("list flpdf attachments");
    assert_eq!(flpdf_list.status.code(), qpdf_list.status.code());
    assert_eq!(flpdf_list.stdout, qpdf_list.stdout);
    assert_eq!(flpdf_list.stderr, qpdf_list.stderr);
}

#[test]
fn optimize_images_is_accepted_with_attachment_inspection_like_qpdf() {
    if !qpdf_11_9_available() {
        return;
    }
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");

    let qpdf_list = ProcessCommand::new("/usr/bin/qpdf")
        .args(["--optimize-images", "--list-attachments"])
        .arg(&input)
        .output()
        .expect("run qpdf attachment listing");
    let flpdf_list = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--optimize-images", "--list-attachments"])
        .arg(&input)
        .output()
        .expect("run flpdf attachment listing");
    assert_eq!(flpdf_list.status.code(), qpdf_list.status.code());
    assert_eq!(flpdf_list.stdout, qpdf_list.stdout);
    assert_eq!(flpdf_list.stderr, qpdf_list.stderr);

    let qpdf_show = ProcessCommand::new("/usr/bin/qpdf")
        .args(["--optimize-images", "--show-attachment=attachment.txt"])
        .arg(&input)
        .output()
        .expect("run qpdf attachment show");
    let flpdf_show = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--optimize-images", "--show-attachment=attachment.txt"])
        .arg(&input)
        .output()
        .expect("run flpdf attachment show");
    assert_eq!(flpdf_show.status.code(), qpdf_show.status.code());
    assert_eq!(flpdf_show.stdout, qpdf_show.stdout);
    assert_eq!(flpdf_show.stderr, qpdf_show.stderr);
}

#[test]
fn optimize_images_is_accepted_with_empty_attachment_inspection_like_qpdf() {
    if !qpdf_11_9_available() {
        return;
    }
    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .args(["--empty", "--optimize-images", "--show-attachment=missing"])
        .output()
        .expect("run qpdf empty attachment inspection");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--empty", "--optimize-images", "--show-attachment=missing"])
        .output()
        .expect("run flpdf empty attachment inspection");
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn optimize_images_is_accepted_with_add_and_copy_attachment_like_qpdf() {
    if !qpdf_11_9_available() {
        return;
    }
    let tempdir = tempfile::tempdir().expect("tempdir");
    let payload = tempdir.path().join("payload.txt");
    std::fs::write(&payload, b"payload").expect("write payload");
    let base =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let donor = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");

    let qpdf_add = tempdir.path().join("qpdf-add.pdf");
    let flpdf_add = tempdir.path().join("flpdf-add.pdf");
    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .args(["--optimize-images", "--add-attachment"])
        .arg(&payload)
        .args(["--key=newkey", "--"])
        .arg(&base)
        .arg(&qpdf_add)
        .output()
        .expect("run qpdf add attachment");
    assert!(qpdf.status.success(), "qpdf add failed: {qpdf:?}");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg("--optimize-images")
        .arg(&base)
        .args(["--add-attachment"])
        .arg(&payload)
        .args(["--key=newkey", "--"])
        .arg(&flpdf_add)
        .output()
        .expect("run flpdf add attachment");
    assert!(flpdf.status.success(), "flpdf add failed: {flpdf:?}");

    let qpdf_copy = tempdir.path().join("qpdf-copy.pdf");
    let flpdf_copy = tempdir.path().join("flpdf-copy.pdf");
    let qpdf = ProcessCommand::new("/usr/bin/qpdf")
        .args(["--optimize-images", "--copy-attachments-from"])
        .arg(&donor)
        .args(["--"])
        .arg(&base)
        .arg(&qpdf_copy)
        .output()
        .expect("run qpdf copy attachments");
    assert!(qpdf.status.success(), "qpdf copy failed: {qpdf:?}");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg("--optimize-images")
        .arg(&base)
        .args(["--copy-attachments-from"])
        .arg(&donor)
        .args(["--"])
        .arg(&flpdf_copy)
        .output()
        .expect("run flpdf copy attachments");
    assert!(flpdf.status.success(), "flpdf copy failed: {flpdf:?}");

    for (qpdf_output, flpdf_output, key) in [
        (&qpdf_add, &flpdf_add, "newkey"),
        (&qpdf_copy, &flpdf_copy, "attachment.txt"),
    ] {
        let qpdf_list = ProcessCommand::new("/usr/bin/qpdf")
            .arg("--list-attachments")
            .arg(qpdf_output)
            .output()
            .expect("list qpdf attachment output");
        let flpdf_list = Command::cargo_bin("flpdf")
            .expect("flpdf binary")
            .arg("--list-attachments")
            .arg(flpdf_output)
            .output()
            .expect("list flpdf attachment output");
        assert!(String::from_utf8_lossy(&qpdf_list.stdout).contains(key));
        assert!(String::from_utf8_lossy(&flpdf_list.stdout).contains(key));
    }
}
