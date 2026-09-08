//! Top-level qpdf-compatible inline-image transformation coverage.

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

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
    let content = inline_image_content(b"/G", &[0, 64, 128, 255], true);
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

fn inline_image_content(colorspace: &[u8], payload: &[u8], with_ei: bool) -> Vec<u8> {
    let mut content = b"q 200 0 0 200 0 0 cm BI /W 2 /H 2 /CS ".to_vec();
    content.extend_from_slice(colorspace);
    content.extend_from_slice(b" /BPC 8 ID\n");
    content.extend_from_slice(payload);
    if with_ei {
        content.extend_from_slice(b"\nEI Q\n");
    } else {
        content.extend_from_slice(b"\nQ\n");
    }
    content
}

fn two_page_inline_image_pdf() -> Vec<u8> {
    let first = inline_image_content(b"/G", &[0, 64, 128, 255], true);
    let second = inline_image_content(b"/G", &[255, 128, 64, 0], true);
    assemble_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>".to_vec(),
        ),
        (
            4,
            stream_object(format!("<< /Length {} >>", first.len()).as_bytes(), &first),
        ),
        (
            5,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 6 0 R >>".to_vec(),
        ),
        (
            6,
            stream_object(format!("<< /Length {} >>", second.len()).as_bytes(), &second),
        ),
    ])
}

fn nested_form_inline_image_pdf() -> Vec<u8> {
    let page_content = b"q /Fm1 Do Q\n".to_vec();
    let form_content = inline_image_content(b"/G", &[0, 64, 128, 255], true);
    assemble_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Fm1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        ),
        (
            4,
            stream_object(
                format!("<< /Length {} >>", page_content.len()).as_bytes(),
                &page_content,
            ),
        ),
        (
            5,
            stream_object(
                format!(
                    "<< /Type /XObject /Subtype /Form /FormType 1 /BBox [0 0 200 200] /Resources << >> /Length {} >>",
                    form_content.len()
                )
                .as_bytes(),
                &form_content,
            ),
        ),
    ])
}

fn named_colorspace_inline_image_pdf() -> Vec<u8> {
    let content = inline_image_content(b"/CS1", &[0, 64, 128, 255], true);
    assemble_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /ColorSpace << /CS1 /DeviceGray >> >> /Contents 4 0 R >>".to_vec(),
        ),
        (
            4,
            stream_object(format!("<< /Length {} >>", content.len()).as_bytes(), &content),
        ),
    ])
}

fn no_inline_image_pdf() -> Vec<u8> {
    let content = b"q Q\n";
    assemble_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>".to_vec(),
        ),
        (
            4,
            stream_object(format!("<< /Length {} >>", content.len()).as_bytes(), content),
        ),
    ])
}

fn damaged_inline_image_pdf() -> Vec<u8> {
    let content = inline_image_content(b"/G", &[0, 64, 128, 255], false);
    assemble_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>".to_vec(),
        ),
        (
            4,
            stream_object(format!("<< /Length {} >>", content.len()).as_bytes(), &content),
        ),
    ])
}

fn qpdf_or_skip() -> bool {
    let result = ProcessCommand::new("qpdf").arg("--version").output();
    let version = result.ok().and_then(|output| {
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    });
    let exact = version
        .as_deref()
        .and_then(|version| version.lines().next())
        == Some(EXPECTED_QPDF_VERSION);
    if exact {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!(
            "qpdf 11.9.0 is required for inline-image oracle tests; found {:?}",
            version
        );
    }
    eprintln!(
        "[SKIP cli_inline_images_transform] qpdf 11.9.0 is unavailable; found {:?}",
        version
    );
    false
}

fn run_qpdf_rewrite(flags: &[String], input: &Path, output: &Path) -> Output {
    ProcessCommand::new("qpdf")
        .args(flags)
        .arg(input)
        .arg(output)
        .output()
        .expect("qpdf 11.9.0 is available")
}

fn run_flpdf_rewrite(flags: &[String], input: &Path, output: &Path) -> Output {
    let mut command = Command::cargo_bin("flpdf").expect("flpdf binary");
    command
        .args(flags)
        .arg(input)
        .arg(output)
        .output()
        .expect("run flpdf")
}

/// Run qpdf when the pinned 11.9.0 oracle is present.
///
/// The flpdf-only assertions in the pre-existing tests below do not need the
/// oracle, so gate only the comparison rather than returning early and
/// skipping the whole test on a machine without qpdf.
fn run_qpdf_rewrite_if_available(flags: &[String], input: &Path, output: &Path) -> Option<Output> {
    qpdf_or_skip().then(|| run_qpdf_rewrite(flags, input, output))
}

fn run_qpdf_page_selection(input: &Path, output: &Path) -> Output {
    ProcessCommand::new("qpdf")
        .args([
            "--externalize-inline-images",
            "--ii-min-bytes=0",
            "--static-id",
        ])
        .arg(input)
        .args(["--pages"])
        .arg(input)
        .args(["1", "--"])
        .arg(output)
        .output()
        .expect("qpdf 11.9.0 is available")
}

fn run_flpdf_page_selection(input: &Path, output: &Path) -> Output {
    let mut command = Command::cargo_bin("flpdf").expect("flpdf binary");
    command
        .args([
            "--externalize-inline-images",
            "--ii-min-bytes=0",
            "--static-id",
        ])
        .arg(input)
        .args(["--pages"])
        .arg(input)
        .args(["1", "--"])
        .arg(output)
        .output()
        .expect("run flpdf page selection")
}

fn qpdf_pages_json(path: &Path) -> Value {
    let output = ProcessCommand::new("qpdf")
        .args(["--json=2", "--json-key=pages"])
        .arg(path)
        .output()
        .expect("run qpdf JSON");
    assert!(
        output.status.success(),
        "qpdf JSON failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("valid qpdf JSON")
}

fn flpdf_pages_json(path: &Path) -> Value {
    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--json=2", "--json-key=pages"])
        .arg(path)
        .output()
        .expect("run flpdf JSON");
    assert!(output.status.success(), "JSON failed: {output:?}");
    serde_json::from_slice(&output.stdout).expect("valid JSON")
}

fn normalized_page_images(value: &Value) -> Vec<Vec<Value>> {
    value["pages"]
        .as_array()
        .expect("pages array")
        .iter()
        .map(|page| {
            page["images"]
                .as_array()
                .expect("images array")
                .iter()
                .map(|image| {
                    let mut image = image.clone();
                    image
                        .as_object_mut()
                        .expect("image object")
                        .remove("object");
                    image
                })
                .collect()
        })
        .collect()
}

fn assert_page_images_match(qpdf_output: &Path, flpdf_output: &Path) {
    let oracle = normalized_page_images(&qpdf_pages_json(qpdf_output));
    assert_eq!(
        oracle,
        normalized_page_images(&flpdf_pages_json(flpdf_output)),
        "flpdf inline-image page metadata must match qpdf 11.9.0"
    );
    // Reading flpdf's own output with flpdf moves the writer and the JSON
    // reader together, so a matching pair of errors stays green. Read the same
    // file with qpdf as well to pin the written document, not the round trip.
    assert_eq!(
        oracle,
        normalized_page_images(&qpdf_pages_json(flpdf_output)),
        "qpdf 11.9.0 must see the same inline-image metadata in flpdf's output"
    );
}

/// Compare the whole `--qdf` output, plus stderr and exit status, against
/// qpdf 11.9.0's own.
///
/// qpdf's `inline-images.test` compares whole files rather than metadata
/// (`qpdf/qtest/inline-images.test:93-94`), which is what makes it catch a
/// transform that lands in the wrong place. The page-image JSON cannot: qpdf's
/// `doJSONPages` lists only the images a page owns directly
/// (`libqpdf/QPDFJob.cc:1044` -> `QPDFPageObjectHelper::getImages`,
/// `libqpdf/QPDFPageObjectHelper.cc:376-384`), while `externalizeInlineImages`
/// also recurses into Form XObject resources
/// (`libqpdf/QPDFPageObjectHelper.cc:430-434`), so a nested-form or
/// content-only difference shows up as `[] == []`.
///
/// `--qdf` disables stream compression, so the DEFLATE backend difference that
/// CLAUDE.md allows as deviation (A) never reaches the output and this
/// comparison needs no `qpdf-zlib-compat` feature.
fn assert_qdf_bytes_match(input: &Path, extra_flags: &[&str], label: &str) {
    if !qpdf_or_skip() {
        return;
    }
    let directory = tempfile::tempdir().expect("tempdir");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    let mut flags = vec!["--qdf".to_owned(), "--static-id".to_owned()];
    flags.extend(extra_flags.iter().map(|flag| (*flag).to_owned()));

    let qpdf = run_qpdf_rewrite(&flags, input, &qpdf_output);
    let flpdf = run_flpdf_rewrite(&flags, input, &flpdf_output);

    assert_streams_and_status_match(&qpdf, &flpdf, label);
    assert_eq!(
        std::fs::read(&qpdf_output).expect("qpdf output"),
        std::fs::read(&flpdf_output).expect("flpdf output"),
        "{label}: --qdf output must be byte-identical to qpdf 11.9.0"
    );
}

/// Compare exit status and both captured streams against qpdf 11.9.0.
///
/// Writing progress, diagnostics or PDF data to stdout is an observable CLI
/// regression for any caller that pipes it, and neither the exit status nor
/// the output file would show it; an unexpected warning on stderr is the same
/// kind of hole. Only the program-name prefix is normalized away.
fn assert_streams_and_status_match(qpdf: &Output, flpdf: &Output, label: &str) {
    assert_eq!(
        qpdf.status.code(),
        flpdf.status.code(),
        "{label}: exit status must match qpdf 11.9.0"
    );
    assert_eq!(
        String::from_utf8_lossy(&qpdf.stderr).replace("qpdf:", ""),
        String::from_utf8_lossy(&flpdf.stderr).replace("flpdf:", ""),
        "{label}: diagnostics must match qpdf 11.9.0 apart from the program name"
    );
    assert_eq!(
        String::from_utf8_lossy(&qpdf.stdout).replace("qpdf:", ""),
        String::from_utf8_lossy(&flpdf.stdout).replace("flpdf:", ""),
        "{label}: standard output must match qpdf 11.9.0 apart from the program name"
    );
}

/// Whole-file `--qdf` comparison for the page-selection invocation.
///
/// [`assert_qdf_bytes_match`] takes its flags as a flat list, which cannot
/// reproduce `--pages <file> 1 --` with the primary input placed before the
/// segment. Run the same shape the metadata comparison uses so both checks
/// exercise one parsing path.
fn assert_page_selection_qdf_bytes_match(input: &Path) {
    if !qpdf_or_skip() {
        return;
    }
    let directory = tempfile::tempdir().expect("tempdir");
    let qpdf_output = directory.path().join("qpdf-qdf.pdf");
    let flpdf_output = directory.path().join("flpdf-qdf.pdf");

    let qpdf = ProcessCommand::new("qpdf")
        .args([
            "--qdf",
            "--externalize-inline-images",
            "--ii-min-bytes=0",
            "--static-id",
        ])
        .arg(input)
        .args(["--pages"])
        .arg(input)
        .args(["1", "--"])
        .arg(&qpdf_output)
        .output()
        .expect("qpdf 11.9.0 is available");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--qdf",
            "--externalize-inline-images",
            "--ii-min-bytes=0",
            "--static-id",
        ])
        .arg(input)
        .args(["--pages"])
        .arg(input)
        .args(["1", "--"])
        .arg(&flpdf_output)
        .output()
        .expect("run flpdf page selection");

    assert_streams_and_status_match(&qpdf, &flpdf, "page selection");
    assert_eq!(
        std::fs::read(&qpdf_output).expect("qpdf output"),
        std::fs::read(&flpdf_output).expect("flpdf output"),
        "page selection: --qdf output must be byte-identical to qpdf 11.9.0"
    );
}

fn image_transform_flags(externalize: bool, optimize: bool, keep_inline: bool) -> Vec<String> {
    let mut flags = vec!["--static-id".to_owned()];
    if externalize {
        flags.push("--externalize-inline-images".to_owned());
    }
    if externalize || optimize {
        flags.push("--ii-min-bytes=0".to_owned());
    }
    if optimize {
        flags.push("--optimize-images".to_owned());
    }
    if keep_inline {
        flags.push("--keep-inline-images".to_owned());
    }
    flags
}

#[test]
fn top_level_externalize_inline_images_converts_inline_image_to_xobject() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("input.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let output = directory.path().join("output.pdf");
    std::fs::write(&input, inline_image_pdf()).expect("write input");

    let qpdf = run_qpdf_rewrite_if_available(
        &[
            "--externalize-inline-images".to_owned(),
            "--ii-min-bytes=0".to_owned(),
            "--static-id".to_owned(),
        ],
        &input,
        &qpdf_output,
    );
    if let Some(qpdf) = &qpdf {
        assert!(
            qpdf.status.success(),
            "qpdf externalization failed: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
    }

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

    if qpdf.is_some() {
        assert_page_images_match(&qpdf_output, &output);
    }
    let image = &flpdf_pages_json(&output)["pages"][0]["images"][0];
    assert_eq!(image["name"], "/IIm1");
    assert_eq!(image["width"], 2);
    assert_eq!(image["height"], 2);
}

#[test]
fn top_level_externalize_inline_images_honors_inclusive_payload_threshold() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("input.pdf");
    let qpdf_at_limit = directory.path().join("qpdf-at-limit.pdf");
    let qpdf_above_limit = directory.path().join("qpdf-above-limit.pdf");
    let at_limit = directory.path().join("at-limit.pdf");
    let above_limit = directory.path().join("above-limit.pdf");
    std::fs::write(&input, inline_image_pdf()).expect("write input");

    let qpdf_at_limit_result = run_qpdf_rewrite_if_available(
        &[
            "--externalize-inline-images".to_owned(),
            "--ii-min-bytes=5".to_owned(),
            "--static-id".to_owned(),
        ],
        &input,
        &qpdf_at_limit,
    );
    if let Some(result) = &qpdf_at_limit_result {
        assert!(result.status.success());
    }
    let qpdf_above_limit_result = run_qpdf_rewrite_if_available(
        &[
            "--externalize-inline-images".to_owned(),
            "--ii-min-bytes=6".to_owned(),
            "--static-id".to_owned(),
        ],
        &input,
        &qpdf_above_limit,
    );
    if let Some(result) = &qpdf_above_limit_result {
        assert!(result.status.success());
    }

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
        flpdf_pages_json(&at_limit)["pages"][0]["images"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    if qpdf_at_limit_result.is_some() {
        assert_page_images_match(&qpdf_at_limit, &at_limit);
    }

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
    assert!(flpdf_pages_json(&above_limit)["pages"][0]["images"]
        .as_array()
        .expect("image array")
        .is_empty());
    if qpdf_above_limit_result.is_some() {
        assert_page_images_match(&qpdf_above_limit, &above_limit);
    }
    // At `--ii-min-bytes=6` the five-byte image stays inline, so no XObject is
    // produced and both page-image arrays are empty. Compare the whole file so
    // the retained `BI ... EI` content itself is checked.
    assert_qdf_bytes_match(
        &input,
        &["--externalize-inline-images", "--ii-min-bytes=6"],
        "above the inclusive threshold",
    );
    assert_qdf_bytes_match(
        &input,
        &["--externalize-inline-images", "--ii-min-bytes=5"],
        "at the inclusive threshold",
    );
}

#[test]
fn explicit_externalization_overrides_keep_inline_images() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("input.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let output = directory.path().join("output.pdf");
    std::fs::write(&input, inline_image_pdf()).expect("write input");

    let qpdf = run_qpdf_rewrite_if_available(
        &[
            "--externalize-inline-images".to_owned(),
            "--keep-inline-images".to_owned(),
            "--ii-min-bytes=0".to_owned(),
            "--static-id".to_owned(),
        ],
        &input,
        &qpdf_output,
    );
    if let Some(qpdf) = &qpdf {
        assert!(qpdf.status.success());
    }

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
        flpdf_pages_json(&output)["pages"][0]["images"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    if qpdf.is_some() {
        assert_page_images_match(&qpdf_output, &output);
    }
}

#[test]
fn rewrite_subcommand_accepts_externalize_inline_images() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("input.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let output = directory.path().join("output.pdf");
    std::fs::write(&input, inline_image_pdf()).expect("write input");

    let qpdf = run_qpdf_rewrite_if_available(
        &[
            "--externalize-inline-images".to_owned(),
            "--ii-min-bytes=0".to_owned(),
            "--static-id".to_owned(),
        ],
        &input,
        &qpdf_output,
    );
    if let Some(qpdf) = &qpdf {
        assert!(qpdf.status.success());
    }

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
        flpdf_pages_json(&output)["pages"][0]["images"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    if qpdf.is_some() {
        assert_page_images_match(&qpdf_output, &output);
    }
}

#[test]
fn externalize_and_optimize_flags_match_qpdf_truth_table() {
    if !qpdf_or_skip() {
        return;
    }
    let cases = [
        (false, false, false),
        (false, false, true),
        (false, true, false),
        (false, true, true),
        (true, false, false),
        (true, false, true),
        (true, true, false),
        (true, true, true),
    ];
    for (externalize, optimize, keep_inline) in cases {
        let directory = tempfile::tempdir().expect("tempdir");
        let input = directory.path().join("input.pdf");
        let qpdf_output = directory.path().join("qpdf.pdf");
        let flpdf_output = directory.path().join("flpdf.pdf");
        std::fs::write(&input, inline_image_pdf()).expect("write input");
        let flags = image_transform_flags(externalize, optimize, keep_inline);

        let qpdf = run_qpdf_rewrite(&flags, &input, &qpdf_output);
        assert!(
            qpdf.status.success(),
            "qpdf truth-table case ({externalize}, {optimize}, {keep_inline}) failed: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        let flpdf = run_flpdf_rewrite(&flags, &input, &flpdf_output);
        assert!(
            flpdf.status.success(),
            "flpdf truth-table case ({externalize}, {optimize}, {keep_inline}) failed: {}",
            String::from_utf8_lossy(&flpdf.stderr)
        );
        assert_page_images_match(&qpdf_output, &flpdf_output);
        // Rows where qpdf keeps the image inline produce no XObject at all, so
        // the page-image JSON is `[] == []` for them. The whole-file
        // comparison sees the retained `BI ... EI` content itself.
        let extra: Vec<&str> = flags
            .iter()
            .filter(|flag| flag.as_str() != "--static-id")
            .map(String::as_str)
            .collect();
        assert_qdf_bytes_match(
            &input,
            &extra,
            &format!("truth table ({externalize}, {optimize}, {keep_inline})"),
        );
    }
}

#[test]
fn page_selection_with_externalization_matches_qpdf() {
    if !qpdf_or_skip() {
        return;
    }
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("input.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    std::fs::write(&input, two_page_inline_image_pdf()).expect("write input");

    let qpdf = run_qpdf_page_selection(&input, &qpdf_output);
    assert!(
        qpdf.status.success(),
        "qpdf --pages externalization failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let flpdf = run_flpdf_page_selection(&input, &flpdf_output);
    assert!(
        flpdf.status.success(),
        "flpdf --pages externalization failed: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_page_images_match(&qpdf_output, &flpdf_output);
    // The page-image JSON is identical for either selected page, so it cannot
    // tell them apart; the whole-file comparison can.
    assert_page_selection_qdf_bytes_match(&input);
}

#[test]
fn nested_forms_named_colorspaces_and_empty_pages_match_qpdf() {
    if !qpdf_or_skip() {
        return;
    }
    let cases = [
        ("nested-form", nested_form_inline_image_pdf()),
        ("named-colorspace", named_colorspace_inline_image_pdf()),
        ("no-inline-image", no_inline_image_pdf()),
    ];
    for (name, bytes) in cases {
        let directory = tempfile::tempdir().expect("tempdir");
        let input = directory.path().join(format!("{name}-input.pdf"));
        let qpdf_output = directory.path().join(format!("{name}-qpdf.pdf"));
        let flpdf_output = directory.path().join(format!("{name}-flpdf.pdf"));
        std::fs::write(&input, bytes).expect("write input");
        let flags = vec![
            "--externalize-inline-images".to_owned(),
            "--ii-min-bytes=0".to_owned(),
            "--static-id".to_owned(),
        ];

        let qpdf = run_qpdf_rewrite(&flags, &input, &qpdf_output);
        assert!(
            qpdf.status.success(),
            "qpdf {name} externalization failed: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        let flpdf = run_flpdf_rewrite(&flags, &input, &flpdf_output);
        assert!(
            flpdf.status.success(),
            "flpdf {name} externalization failed: {}",
            String::from_utf8_lossy(&flpdf.stderr)
        );
        assert_page_images_match(&qpdf_output, &flpdf_output);
        assert_qdf_bytes_match(
            &input,
            &["--externalize-inline-images", "--ii-min-bytes=0"],
            name,
        );
    }
}

#[test]
fn damaged_inline_image_recovery_matches_qpdf() {
    if !qpdf_or_skip() {
        return;
    }
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("damaged-input.pdf");
    let qpdf_output = directory.path().join("damaged-qpdf.pdf");
    let flpdf_output = directory.path().join("damaged-flpdf.pdf");
    std::fs::write(&input, damaged_inline_image_pdf()).expect("write input");
    let flags = vec![
        "--externalize-inline-images".to_owned(),
        "--ii-min-bytes=0".to_owned(),
        "--static-id".to_owned(),
    ];

    let qpdf = run_qpdf_rewrite(&flags, &input, &qpdf_output);
    assert!(
        qpdf.status.success(),
        "qpdf damaged inline-image recovery failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let flpdf = run_flpdf_rewrite(&flags, &input, &flpdf_output);
    assert!(
        flpdf.status.success(),
        "flpdf damaged inline-image recovery failed: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_page_images_match(&qpdf_output, &flpdf_output);
    assert_qdf_bytes_match(
        &input,
        &["--externalize-inline-images", "--ii-min-bytes=0"],
        "damaged inline image",
    );
}
