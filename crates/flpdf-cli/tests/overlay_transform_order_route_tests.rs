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

fn production_function_body<'a>(source: &'a str, signature: &str, next_signature: &str) -> &'a str {
    source
        .split_once(signature)
        .expect("named production function")
        .1
        .split_once(next_signature)
        .expect("following production function")
        .0
}

fn item_body_end(body: &str) -> Option<usize> {
    #[derive(PartialEq)]
    enum State {
        Code,
        LineComment,
        StringLiteral,
    }

    let mut state = State::Code;
    let mut depth = 0usize;
    let mut chars = body.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        match state {
            State::Code => match character {
                '/' if chars.peek().map(|&(_, next)| next) == Some('/') => {
                    state = State::LineComment;
                }
                '"' => state = State::StringLiteral,
                '{' => depth += 1,
                '}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(index + character.len_utf8());
                    }
                }
                _ => {}
            },
            State::LineComment => {
                if character == '\n' {
                    state = State::Code;
                }
            }
            State::StringLiteral => match character {
                '\\' => {
                    chars.next();
                }
                '"' => state = State::Code,
                _ => {}
            },
        }
    }
    None
}

fn production_function_body_exact<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("production function {signature:?} must exist"));
    let after_signature = &source[start..];
    let brace = after_signature
        .find('{')
        .unwrap_or_else(|| panic!("production function {signature:?} must have a body"));
    let body = &after_signature[brace..];
    let end = item_body_end(body)
        .unwrap_or_else(|| panic!("production function {signature:?} must balance braces"));
    &body[..end]
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
    let ordinary_rewrite = production_function_body(
        &source,
        "fn run_rewrite_with_qpdf_job(",
        "\nfn configure_rewrite_job(",
    );

    assert!(
        ordinary_rewrite.contains("finish_job_exit_status(job.run()?)"),
        "ordinary rewrite must complete through one QPDFJob::run()"
    );
    for staged_route in [
        "job.create_qpdf()",
        "job.write_qpdf(",
        "job.get_exit_code()",
    ] {
        assert!(
            !ordinary_rewrite.contains(staged_route),
            "ordinary rewrite must not split its Job lifecycle at {staged_route}"
        );
    }
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
fn standalone_check_uses_the_canonical_job_lifecycle() {
    let source = production_main_source();
    let check_route = source
        .split_once("fn run_check(")
        .and_then(|(_, tail)| {
            tail.split_once("fn run_check_linearization")
                .map(|(body, _)| body)
        })
        .expect("standalone check route");

    assert!(
        check_route.contains("job.run()"),
        "standalone check must use QPDFJob's create/write inspection lifecycle"
    );
    for forbidden in [
        "File::open(",
        "open_with_description(",
        "job.check(",
        "finish_check_job(",
    ] {
        assert!(
            !check_route.contains(forbidden),
            "standalone check retains a CLI-local route: {forbidden}"
        );
    }
}

#[test]
fn check_linearization_subcommand_uses_job_run() {
    let source = production_main_source();
    let route = source
        .split_once("fn run_check_linearization(")
        .and_then(|(_, tail)| tail.split_once("struct EncryptionCliOptions"))
        .map(|(body, _)| body)
        .expect("check-linearization subcommand route");

    assert!(
        route.contains("configuration.check_linearization()"),
        "check-linearization subcommand must configure the canonical Job flag"
    );
    assert!(
        route.contains("job.run()"),
        "check-linearization subcommand must use QPDFJob::run"
    );
    for forbidden in [
        "job.check_linearization(",
        "job.show_linearization(",
        "open_with_description(",
        "File::open(",
    ] {
        assert!(
            !route.contains(forbidden),
            "check-linearization subcommand retains a direct route: {forbidden}"
        );
    }
}

#[test]
fn pages_subcommands_use_job_run() {
    let source = production_main_source();
    let after_npages = source
        .split_once("fn run_show_npages(")
        .and_then(|(_, tail)| tail.split_once("fn run_show_pages("))
        .map(|(body, _)| body)
        .expect("show-npages subcommand route");
    let show_pages = source
        .split_once("fn run_show_pages(")
        .and_then(|(_, tail)| tail.split_once("// Encryption inspection subcommands"))
        .map(|(body, _)| body)
        .expect("show-pages subcommand route");

    assert!(
        after_npages.contains("configuration.show_npages()"),
        "pages --show-npages must configure the canonical Job flag"
    );
    assert!(
        after_npages.contains("job.run()"),
        "pages --show-npages must use QPDFJob::run"
    );
    assert!(
        show_pages.contains("configuration.show_pages()"),
        "default pages must configure the canonical Job flag"
    );
    assert!(
        show_pages.contains("job.run()"),
        "default pages must use QPDFJob::run"
    );

    for forbidden in [
        "job.show_npages(",
        "job.show_pages(",
        "open_pdf_with_suppression(",
        "complete_report(",
    ] {
        assert!(
            !after_npages.contains(forbidden) && !show_pages.contains(forbidden),
            "pages subcommand retains a direct route: {forbidden}"
        );
    }
}

#[test]
fn show_encryption_subcommand_uses_job_run() {
    let source = production_main_source();
    let route = source
        .split_once("fn run_show_encryption(")
        .and_then(|(_, tail)| tail.split_once("fn hex_lower("))
        .map(|(body, _)| body)
        .expect("show-encryption subcommand route");

    assert!(
        route.contains("configuration.show_encryption()"),
        "show-encryption must configure the canonical Job flag"
    );
    assert!(
        route.contains("job.set_input_file("),
        "show-encryption input must be owned by QPDFJob"
    );
    assert!(
        route.contains("job.run()"),
        "show-encryption must use QPDFJob::run"
    );
    for forbidden in [
        "job.show_encryption(",
        "job.complete_report(",
        "finish_show_encryption(",
        "open_for_encryption_inspection_with_description(",
    ] {
        assert!(
            !route.contains(forbidden),
            "show-encryption subcommand retains a direct route: {forbidden}"
        );
    }
}

#[test]
fn page_selection_overlay_uses_the_canonical_job_owner() {
    let source = production_main_source();
    let page_route = production_function_body(
        &source,
        "fn run_page_extraction(",
        "\nfn run_empty_page_extraction",
    );

    for forbidden in [
        "flpdf::handle_under_overlay(",
        "flpdf::overlay_verbose_report(",
        "build_overlay_specs_with_suppression(",
    ] {
        assert!(
            !page_route.contains(forbidden),
            "rewrite page route retains a direct overlay helper: {forbidden}"
        );
    }
    assert!(
        page_route.contains("configure_rewrite_job(")
            && page_route.contains("run_page_operation_job(job, page_ops, remove_unref)"),
        "rewrite page route must configure overlays and finish on its one QPDFJob"
    );
    let rewrite_configuration =
        production_function_body_exact(&source, "fn configure_rewrite_job(");
    assert!(
        rewrite_configuration.contains("configuration.overlay(")
            && rewrite_configuration.contains("configuration.underlay("),
        "the canonical rewrite Job must own overlay and underlay sources"
    );
}

#[test]
fn page_selection_post_plan_rotation_and_images_use_the_canonical_job_owner() {
    let source = production_main_source();
    let page_route = production_function_body(
        &source,
        "fn run_page_extraction(",
        "\nfn run_empty_page_extraction",
    );

    for forbidden in ["apply_rotate_specs(", "apply_image_transformations("] {
        assert!(
            !page_route.contains(forbidden),
            "rewrite page route retains a direct transform helper: {forbidden}"
        );
    }
    let page_configuration = production_function_body(
        &source,
        "fn configure_page_selection_job(",
        "\nfn run_page_operation_job",
    );
    assert!(
        page_configuration.contains("configuration.rotate("),
        "page-selection settings must queue rotations on QPDFJob"
    );
    let rewrite_configuration =
        production_function_body_exact(&source, "fn configure_rewrite_job(");
    assert!(
        rewrite_configuration.contains("configuration.optimize_images(")
            || rewrite_configuration.contains("configuration.externalize_inline_images("),
        "rewrite Job configuration must queue image transformations on QPDFJob"
    );
}

#[test]
fn empty_page_selection_uses_the_shared_job_run() {
    let source = production_main_source();
    let empty_pages = production_function_body(
        &source,
        "fn run_empty_page_extraction(",
        "\nfn split_pages_active",
    );

    assert!(
        empty_pages.contains("configuration.empty_input()"),
        "empty-primary page selection must configure QPDFJob's empty input"
    );
    assert!(
        empty_pages.contains("run_page_operation_job(job, page_ops, remove_unref)"),
        "empty-primary page selection must use the shared QPDFJob runner"
    );
    for forbidden in [
        "open_page_source(",
        "job.handle_page_specs(",
        "job.create_qpdf()",
        "job.write_qpdf(",
    ] {
        assert!(
            !empty_pages.contains(forbidden),
            "empty-primary page selection retains a CLI-owned page/lifecycle route: {forbidden}"
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

#[test]
fn rewrite_page_selection_overlay_images_match_qpdf_after_externalization() {
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
            "--qdf",
            "--no-original-object-ids",
            "--static-id",
            "--optimize-images",
            "--ii-min-bytes=0",
            "--overlay",
        ])
        .arg(&overlay)
        .arg("--")
        .arg(&primary)
        .args(["--pages", ".", "1", "--"])
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf page-selection overlay image transformation");
    assert!(qpdf.status.success(), "qpdf failed: {qpdf:?}");

    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args([
            "rewrite",
            "--qdf",
            "--no-original-object-ids",
            "--static-id",
            "--optimize-images",
            "--ii-min-bytes=0",
        ])
        .arg(&primary)
        .args(["--overlay"])
        .arg(&overlay)
        .arg("--")
        .args(["--pages", ".", "1", "--"])
        .arg(&flpdf_output)
        .output()
        .expect("run flpdf page-selection overlay image transformation");
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(flpdf.status.success(), "flpdf failed: {flpdf:?}");
    let qpdf_images = image_count(&qpdf_output);
    let flpdf_images = image_count(&flpdf_output);
    assert!(qpdf_images > 0, "probe must contain images");
    assert_eq!(
        flpdf_images, qpdf_images,
        "page-selection overlay image traversal diverged"
    );
    assert_eq!(
        fs::read(&flpdf_output).expect("read flpdf output"),
        fs::read(&qpdf_output).expect("read qpdf output"),
        "page-selection overlay image rewrite must be byte-identical"
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn linearized_overlay_and_underlay_rewrites_match_qpdf() {
    if !qpdf_available() {
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let primary =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/three-page.pdf");
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");

    let cases = [
        ("overlay", "--overlay"),
        ("underlay", "--underlay"),
        ("linearize-before-overlay", "--overlay"),
    ];
    for (name, placement) in cases {
        let qpdf_output = directory.path().join(format!("{name}-qpdf.pdf"));
        let flpdf_output = directory.path().join(format!("{name}-flpdf.pdf"));
        let args = if name == "linearize-before-overlay" {
            vec![
                "--static-id".to_owned(),
                "--linearize".to_owned(),
                placement.to_owned(),
                source.display().to_string(),
                "--".to_owned(),
                primary.display().to_string(),
                qpdf_output.display().to_string(),
            ]
        } else {
            vec![
                "--static-id".to_owned(),
                placement.to_owned(),
                source.display().to_string(),
                "--".to_owned(),
                "--linearize".to_owned(),
                primary.display().to_string(),
                qpdf_output.display().to_string(),
            ]
        };

        let mut flpdf_args = args.clone();
        *flpdf_args.last_mut().expect("flpdf output argument") = flpdf_output.display().to_string();

        let qpdf = ProcessCommand::new("/usr/bin/qpdf")
            .args(&args)
            .output()
            .expect("run qpdf linearized overlay oracle");
        let flpdf = Command::cargo_bin("flpdf")
            .expect("flpdf binary")
            .env("FLPDF_STATIC_ID_QUIET", "1")
            .args(flpdf_args.iter().map(String::as_str))
            .output()
            .expect("run flpdf linearized overlay route");

        assert_eq!(flpdf.status.code(), qpdf.status.code(), "{name} status");
        assert_eq!(flpdf.stdout, qpdf.stdout, "{name} stdout");
        assert_eq!(flpdf.stderr, qpdf.stderr, "{name} stderr");
        assert!(qpdf.status.success(), "qpdf {name} failed: {qpdf:?}");
        assert!(flpdf.status.success(), "flpdf {name} failed: {flpdf:?}");
        assert_eq!(
            fs::read(&flpdf_output).expect("read flpdf output"),
            fs::read(&qpdf_output).expect("read qpdf output"),
            "{name} output must be byte-identical"
        );

        let check = ProcessCommand::new("/usr/bin/qpdf")
            .args(["--check"])
            .arg(&flpdf_output)
            .output()
            .expect("check linearized output");
        assert!(check.status.success(), "{name} output must pass qpdf check");
        assert!(
            String::from_utf8_lossy(&check.stdout).contains("File is linearized"),
            "{name} output must be linearized"
        );
    }
}
