//! qpdf 11.9.0 acceptance for writer/attachment options combined with
//! inspection and JSON output.

use assert_cmd::Command;
use regex::Regex;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

#[path = "support/text_newlines.rs"]
mod text_newlines;
use text_newlines::normalize_text_newlines;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

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

fn skip_without_qpdf() -> bool {
    if qpdf_available() {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("{EXPECTED_QPDF_VERSION} is required for this qpdf conflict matrix on CI");
    }
    eprintln!("skipping qpdf conflict matrix: {EXPECTED_QPDF_VERSION} is unavailable");
    true
}
fn run_qpdf_and_flpdf(args: &[OsString]) -> (Output, Output) {
    let qpdf = ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf should spawn");
    (qpdf, flpdf)
}

fn assert_pair(label: &str, args: &[OsString]) {
    let (qpdf, flpdf) = run_qpdf_and_flpdf(args);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "{label}: exit status differs; qpdf stderr={} flpdf stderr={}",
        String::from_utf8_lossy(&qpdf.stderr),
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "{label}: stdout differs"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "{label}: stderr differs"
    );
    assert!(
        qpdf.status.success(),
        "{label}: qpdf must accept the combination; stderr={}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
}

fn assert_status_output_pair(label: &str, args: &[OsString]) {
    let (qpdf, flpdf) = run_qpdf_and_flpdf(args);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "{label}: exit status differs; qpdf stderr={} flpdf stderr={}",
        String::from_utf8_lossy(&qpdf.stderr),
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "{label}: stdout differs"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "{label}: stderr differs"
    );
}

fn json_args(extra: &[OsString], input: &Path) -> Vec<OsString> {
    let mut args = vec![OsString::from("--json=2")];
    args.extend_from_slice(extra);
    args.push(input.as_os_str().to_owned());
    args.push(OsString::from("-"));
    args
}

fn json_mode_args(mode: &str, extra: &[OsString], input: &Path) -> Vec<OsString> {
    let mut args = vec![OsString::from(mode)];
    args.extend_from_slice(extra);
    args.push(input.as_os_str().to_owned());
    args.push(OsString::from("-"));
    args
}

fn assert_json_output_pair(label: &str, extra: &[OsString], input: &Path) {
    let temp = tempfile::tempdir().expect("temporary JSON output directory");
    let qpdf_output = temp.path().join("qpdf.json");
    let flpdf_output = temp.path().join("flpdf.json");

    let mut qpdf_args = vec![OsString::from("--json-output=2")];
    qpdf_args.extend_from_slice(extra);
    qpdf_args.push(input.as_os_str().to_owned());
    qpdf_args.push(qpdf_output.as_os_str().to_owned());
    let qpdf = ShellCommand::new("qpdf")
        .args(&qpdf_args)
        .output()
        .expect("qpdf should spawn");

    let mut flpdf_args = vec![OsString::from("--json-output=2")];
    flpdf_args.extend_from_slice(extra);
    flpdf_args.push(input.as_os_str().to_owned());
    flpdf_args.push(flpdf_output.as_os_str().to_owned());
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(&flpdf_args)
        .output()
        .expect("flpdf should spawn");

    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "{label}: exit status differs; qpdf stderr={} flpdf stderr={}",
        String::from_utf8_lossy(&qpdf.stderr),
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "{label}: stdout differs"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "{label}: stderr differs"
    );
    assert!(
        qpdf.status.success(),
        "{label}: qpdf must accept the combination; stderr={}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    assert_eq!(
        normalize_text_newlines(&fs::read(&flpdf_output).expect("flpdf JSON output")),
        normalize_text_newlines(&fs::read(&qpdf_output).expect("qpdf JSON output")),
        "{label}: JSON output differs"
    );
}

#[test]
fn qpdf_writer_and_attachment_conflicts_match_qpdf() {
    if skip_without_qpdf() {
        return;
    }

    let three_page = fixture("three-page.pdf");
    let attachment_input = fixture("attachment-two-page.pdf");
    let add_file = fixture("golden/inspect-npages.txt");

    let remove = json_args(
        &[OsString::from("--remove-attachment=attachment.txt")],
        &attachment_input,
    );
    assert_pair("remove-attachment + json", &remove);

    let add = json_args(
        &[
            OsString::from("--json-key=attachments"),
            OsString::from("--add-attachment"),
            add_file.as_os_str().to_owned(),
            OsString::from("--key=k9"),
            OsString::from("--creationdate=D:20200101000000Z"),
            OsString::from("--moddate=D:20200101000000Z"),
            OsString::from("--"),
        ],
        &three_page,
    );
    assert_pair("add-attachment + json", &add);

    let copy = json_args(
        &[
            OsString::from("--json-key=attachments"),
            OsString::from("--copy-attachments-from"),
            attachment_input.as_os_str().to_owned(),
            OsString::from("--"),
        ],
        &three_page,
    );
    assert_pair("copy-attachments-from + json", &copy);

    let encrypt = [
        OsString::from("--encrypt"),
        OsString::from(""),
        OsString::from(""),
        OsString::from("128"),
        OsString::from("--use-aes=y"),
        OsString::from("--"),
    ];
    assert_pair("encrypt + json", &json_args(&encrypt, &three_page));
    let mut encrypt_check = vec![OsString::from("--check")];
    encrypt_check.extend(encrypt.clone());
    encrypt_check.push(three_page.as_os_str().to_owned());
    assert_pair("encrypt + check", &encrypt_check);
    let mut encrypt_npages = vec![OsString::from("--show-npages")];
    encrypt_npages.extend(encrypt.clone());
    encrypt_npages.push(three_page.as_os_str().to_owned());
    assert_pair("encrypt + show-npages", &encrypt_npages);

    let mut encrypt_check_linearization = vec![OsString::from("--check-linearization")];
    encrypt_check_linearization.extend(encrypt.clone());
    encrypt_check_linearization.push(three_page.as_os_str().to_owned());
    assert_pair(
        "encrypt + check-linearization",
        &encrypt_check_linearization,
    );

    let mut encrypt_show_encryption = vec![OsString::from("--show-encryption")];
    encrypt_show_encryption.extend(encrypt.clone());
    encrypt_show_encryption.push(three_page.as_os_str().to_owned());
    assert_pair("encrypt + show-encryption", &encrypt_show_encryption);

    let mut encrypt_show_pages = vec![OsString::from("--show-pages")];
    encrypt_show_pages.extend(encrypt);
    encrypt_show_pages.push(three_page.as_os_str().to_owned());
    assert_pair("encrypt + show-pages", &encrypt_show_pages);

    assert_pair(
        "linearize + json",
        &json_args(&[OsString::from("--linearize")], &three_page),
    );

    assert_pair(
        "decrypt + json",
        &json_args(&[OsString::from("--decrypt")], &three_page),
    );
    assert_pair(
        "decrypt + check",
        &[
            OsString::from("--check"),
            OsString::from("--decrypt"),
            three_page.as_os_str().to_owned(),
        ],
    );
    assert_pair(
        "decrypt + show-npages",
        &[
            OsString::from("--show-npages"),
            OsString::from("--decrypt"),
            three_page.as_os_str().to_owned(),
        ],
    );
    assert_pair(
        "decrypt + show-encryption",
        &[
            OsString::from("--show-encryption"),
            OsString::from("--decrypt"),
            three_page.as_os_str().to_owned(),
        ],
    );
    assert_pair(
        "decrypt + show-pages",
        &[
            OsString::from("--show-pages"),
            OsString::from("--decrypt"),
            three_page.as_os_str().to_owned(),
        ],
    );

    assert_pair(
        "compress-streams=n + json",
        &json_args(&[OsString::from("--compress-streams=n")], &three_page),
    );
    assert_pair(
        "qdf + json",
        &json_args(&[OsString::from("--qdf")], &three_page),
    );
    assert_pair(
        "rotate + check-linearization",
        &[
            OsString::from("--check-linearization"),
            OsString::from("--rotate=90"),
            three_page.as_os_str().to_owned(),
        ],
    );
    assert_pair(
        "pages + check-linearization",
        &[
            three_page.as_os_str().to_owned(),
            OsString::from("--pages"),
            OsString::from("."),
            OsString::from("1"),
            OsString::from("--"),
            OsString::from("--check-linearization"),
        ],
    );

    assert_json_output_pair(
        "compress-streams=n + json-output",
        &[OsString::from("--compress-streams=n")],
        &three_page,
    );
    assert_json_output_pair("qdf + json-output", &[OsString::from("--qdf")], &three_page);
}

#[test]
fn qpdf_id_and_coalesce_json_conflicts_match_qpdf() {
    if skip_without_qpdf() {
        return;
    }

    let three_page = fixture("three-page.pdf");
    let multiple_contents = fixture("qdf-contents-ref-array.pdf");
    let transforms = [
        OsString::from("--static-id"),
        OsString::from("--deterministic-id"),
        OsString::from("--coalesce-contents"),
    ];
    let json_modes = ["--json-output=2", "--json", "--json=2"];

    for transform in &transforms {
        let input = if transform == &transforms[2] {
            &multiple_contents
        } else {
            &three_page
        };
        for mode in json_modes {
            let label = format!("{transform:?} + {mode}");
            if mode == "--json-output=2" {
                assert_json_output_pair(&label, std::slice::from_ref(transform), input);
            } else {
                assert_pair(
                    &label,
                    &json_mode_args(mode, std::slice::from_ref(transform), input),
                );
            }
        }
    }
}

#[test]
fn top_level_conflict_definitions_are_qpdf_shaped() {
    let source = include_str!("../src/main.rs");
    let top_level = source
        .split_once("struct RewriteCommand")
        .map(|(prefix, _)| prefix)
        .expect("source contains the native rewrite command");
    let conflict = Regex::new(r#"(?s)conflicts_with(?:_all)?\s*=\s*(?:"[^"]*"|\[[^\]]*\])"#)
        .expect("conflict declaration regex");
    let forbidden = [
        "job_json_file",
        "json_input",
        "update_from_json",
        "json_key",
        "json_object",
        "json_stream_data",
        "json_stream_prefix",
        "split_pages",
        "overlay",
        "underlay",
        "copy_encryption",
        "encryption_file_password",
        "static_aes_iv",
        "recompress_flate",
        "compression_level",
        "linearize_pass1",
        "remove_restrictions",
        "no_original_object_ids",
        "preserve_unreferenced",
    ];

    assert!(
        !top_level.contains("ArgGroup::new(\"attachment_op\")"),
        "attachment mutations must not be guarded by a clap ArgGroup"
    );
    assert_eq!(
        source.matches("conflicts_with = \"repair\"").count(),
        1,
        "the flpdf-only repair/recovery safety guard must remain explicit"
    );
    for declaration in conflict.find_iter(top_level) {
        let declaration = declaration.as_str();
        for name in forbidden {
            assert!(
                !declaration.contains(name),
                "qpdf-incompatible {name:?} remains in {declaration:?}"
            );
        }
    }
}

#[test]
fn qpdf_writer_inspection_matrix_has_no_false_conflicts() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("attachment-two-page.pdf");
    let payload = fixture("golden/inspect-npages.txt");
    let overlay = fixture("one-page.pdf");
    let writers: Vec<Vec<OsString>> = vec![
        vec![
            OsString::from("--encrypt"),
            OsString::from(""),
            OsString::from(""),
            OsString::from("128"),
            OsString::from("--use-aes=y"),
            OsString::from("--"),
        ],
        vec![OsString::from("--decrypt")],
        vec![OsString::from("--linearize")],
        vec![OsString::from("--recompress-flate")],
        vec![OsString::from("--compression-level=9")],
        vec![OsString::from("--linearize-pass1=/dev/null")],
        vec![OsString::from("--remove-restrictions")],
        vec![OsString::from("--coalesce-contents")],
        vec![OsString::from("--flatten-annotations=all")],
        vec![OsString::from("--generate-appearances")],
        vec![OsString::from("--optimize-images")],
        vec![OsString::from("--externalize-inline-images")],
        vec![OsString::from("--preserve-unreferenced")],
        vec![OsString::from("--no-original-object-ids")],
        vec![OsString::from("--static-id")],
        vec![OsString::from("--static-aes-iv")],
        vec![OsString::from("--qdf")],
        vec![OsString::from("--object-streams=preserve")],
        vec![OsString::from("--compress-streams=n")],
        vec![OsString::from("--decode-level=none")],
        vec![OsString::from("--stream-data=preserve")],
        vec![OsString::from("--remove-page-labels")],
    ];
    let inspections: Vec<Vec<OsString>> = vec![
        vec![OsString::from("--check")],
        vec![OsString::from("--check-linearization")],
        vec![OsString::from("--show-object=trailer")],
        vec![OsString::from("--show-npages")],
        vec![OsString::from("--show-pages")],
        vec![OsString::from("--show-xref")],
        vec![OsString::from("--show-linearization")],
        vec![OsString::from("--show-encryption")],
        vec![OsString::from("--is-encrypted")],
        vec![OsString::from("--requires-password")],
        vec![OsString::from("--list-attachments")],
        vec![OsString::from("--show-attachment=attachment.txt")],
    ];
    assert_eq!(writers.len(), 22);
    assert_eq!(inspections.len(), 12);

    let mut cases = 0;
    for writer in writers {
        for inspection in &inspections {
            let mut args = writer.clone();
            args.extend(inspection.iter().cloned());
            args.push(input.as_os_str().to_owned());
            assert_status_output_pair(&format!("writer {writer:?} + {inspection:?}"), &args);
            cases += 1;
        }
    }
    assert_eq!(cases, 264);

    let mut attachment_args = vec![
        OsString::from("--add-attachment"),
        payload.as_os_str().to_owned(),
        OsString::from("--key=matrix-added"),
        OsString::from("--creationdate=D:20200101000000Z"),
        OsString::from("--moddate=D:20200101000000Z"),
        OsString::from("--"),
        OsString::from("--overlay"),
        overlay.as_os_str().to_owned(),
        OsString::from("--"),
        OsString::from("--list-attachments"),
        input.as_os_str().to_owned(),
    ];
    assert_status_output_pair("attachment + overlay + list", &attachment_args);
    attachment_args.splice(
        0..,
        [
            OsString::from("--remove-attachment=attachment.txt"),
            OsString::from("--add-attachment"),
            payload.as_os_str().to_owned(),
            OsString::from("--key=matrix-replaced"),
            OsString::from("--creationdate=D:20200101000000Z"),
            OsString::from("--moddate=D:20200101000000Z"),
            OsString::from("--"),
            OsString::from("--show-attachment=matrix-replaced"),
            input.as_os_str().to_owned(),
        ],
    );
    assert_status_output_pair("remove + add + show", &attachment_args);
}

#[test]
fn json_input_and_update_inspection_reserve_stdout_before_attachment() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary JSON inspection directory");
    let input_pdf = fixture("attachment-two-page.pdf");
    let json = temp.path().join("attachment.json");
    let generated = ShellCommand::new("qpdf")
        .args([
            OsString::from("--json-output=2"),
            input_pdf.as_os_str().to_owned(),
        ])
        .arg(&json)
        .output()
        .expect("qpdf JSON input fixture should spawn");
    assert!(
        generated.status.success(),
        "qpdf JSON fixture failed: {generated:?}"
    );

    let json_input_args = [
        OsString::from("--json-input"),
        OsString::from("--show-npages"),
        OsString::from("--show-attachment=attachment.txt"),
        json.as_os_str().to_owned(),
    ];
    assert_status_output_pair("json-input show-npages + show-attachment", &json_input_args);

    let update_args = [
        OsString::from(format!("--update-from-json={}", json.display())),
        OsString::from("--show-npages"),
        OsString::from("--show-attachment=attachment.txt"),
        input_pdf.as_os_str().to_owned(),
    ];
    assert_status_output_pair(
        "update-from-json show-npages + show-attachment",
        &update_args,
    );
}

#[test]
fn json_input_and_update_inspection_apply_overlay_before_show_object() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary JSON overlay directory");
    let json_input = fixture("json-input/complete.json");
    let update = fixture("json-input/update.json");
    let overlay = fixture("three-page.pdf");
    let generated_pdf = temp.path().join("complete.pdf");
    let generated = ShellCommand::new("qpdf")
        .args([
            OsString::from("--json-input"),
            json_input.as_os_str().to_owned(),
        ])
        .arg(&generated_pdf)
        .output()
        .expect("qpdf JSON document should spawn");
    assert!(
        generated.status.success(),
        "qpdf JSON document failed: {generated:?}"
    );

    for overlay_flag in ["--overlay", "--underlay"] {
        let json_args = [
            OsString::from("--json-input"),
            OsString::from(overlay_flag),
            overlay.as_os_str().to_owned(),
            OsString::from("--"),
            OsString::from("--show-object=3,0"),
            json_input.as_os_str().to_owned(),
        ];
        assert_pair(
            &format!("json-input {overlay_flag} + show-object"),
            &json_args,
        );

        let update_args = [
            OsString::from(format!("--update-from-json={}", update.display())),
            OsString::from(overlay_flag),
            overlay.as_os_str().to_owned(),
            OsString::from("--"),
            OsString::from("--show-object=3,0"),
            generated_pdf.as_os_str().to_owned(),
        ];
        assert_pair(
            &format!("update-from-json {overlay_flag} + show-object"),
            &update_args,
        );
    }
}

#[test]
fn qpdf_accepts_decrypt_with_show_xref() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("three-page.pdf");
    assert_pair(
        "decrypt + show-xref",
        &[
            OsString::from("--decrypt"),
            OsString::from("--show-xref"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_accepts_coalesce_contents_with_show_encryption() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("three-page.pdf");
    assert_pair(
        "coalesce-contents + show-encryption",
        &[
            OsString::from("--coalesce-contents"),
            OsString::from("--show-encryption"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_accepts_flatten_annotations_with_show_encryption() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("form-fields-and-annotations.pdf");
    assert_pair(
        "flatten-annotations + show-encryption",
        &[
            OsString::from("--flatten-annotations=all"),
            OsString::from("--show-encryption"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_accepts_preserve_unreferenced_with_json() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("three-page.pdf");
    assert_pair(
        "preserve-unreferenced + json",
        &[
            OsString::from("--preserve-unreferenced"),
            OsString::from("--json=2"),
            input.as_os_str().to_owned(),
            OsString::from("-"),
        ],
    );
}

#[test]
fn qpdf_accepts_preserve_unreferenced_with_json_output() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("three-page.pdf");
    assert_json_output_pair(
        "preserve-unreferenced + json-output",
        &[OsString::from("--preserve-unreferenced")],
        &input,
    );
}

#[test]
fn qpdf_accepts_json_input_with_show_object() {
    if skip_without_qpdf() {
        return;
    }

    let json = fixture("json-input/complete.json");

    assert_pair(
        "json-input + show-object",
        &[
            OsString::from("--json-input"),
            OsString::from("--show-object=trailer"),
            json.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_accepts_json_input_update_with_show_object() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("json-input/complete.json");
    let update = fixture("json-input/update.json");
    assert_pair(
        "json-input + update-from-json + show-object",
        &[
            OsString::from("--json-input"),
            OsString::from(format!("--update-from-json={}", update.display())),
            OsString::from("--show-object=trailer"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_accepts_json_input_with_attachment_mutation() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary JSON attachment output directory");
    let input = fixture("json-input/complete.json");
    let payload = fixture("golden/inspect-npages.txt");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let args = [
        OsString::from("--static-id"),
        OsString::from("--qdf"),
        OsString::from("--json-input"),
        OsString::from("--add-attachment"),
        payload.as_os_str().to_owned(),
        OsString::from("--key=json-input-attachment"),
        OsString::from("--creationdate=D:20200101000000Z"),
        OsString::from("--moddate=D:20200101000000Z"),
        OsString::from("--"),
        input.as_os_str().to_owned(),
    ];
    let qpdf = ShellCommand::new("qpdf")
        .args(&args)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf JSON attachment mutation should spawn");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(&args)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf JSON attachment mutation should spawn");

    assert!(
        qpdf.status.success(),
        "qpdf JSON attachment mutation failed: {qpdf:?}"
    );
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert_eq!(
        fs::read(flpdf_output).expect("flpdf JSON attachment output"),
        fs::read(qpdf_output).expect("qpdf JSON attachment output")
    );
}

#[test]
fn qpdf_accepts_json_input_update_with_attachment_mutation() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary JSON attachment output directory");
    let input = fixture("json-input/complete.json");
    let update = fixture("json-input/update.json");
    let payload = fixture("golden/inspect-npages.txt");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let args = [
        OsString::from("--static-id"),
        OsString::from("--qdf"),
        OsString::from("--json-input"),
        OsString::from(format!("--update-from-json={}", update.display())),
        OsString::from("--add-attachment"),
        payload.as_os_str().to_owned(),
        OsString::from("--key=updated-attachment"),
        OsString::from("--creationdate=D:20200101000000Z"),
        OsString::from("--moddate=D:20200101000000Z"),
        OsString::from("--"),
        input.as_os_str().to_owned(),
    ];
    let qpdf = ShellCommand::new("qpdf")
        .args(&args)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf JSON update attachment mutation should spawn");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(&args)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf JSON update attachment mutation should spawn");

    assert!(
        qpdf.status.success(),
        "qpdf JSON update attachment mutation failed: {qpdf:?}"
    );
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert_eq!(
        fs::read(flpdf_output).expect("flpdf updated JSON attachment output"),
        fs::read(qpdf_output).expect("qpdf updated JSON attachment output")
    );
}

#[test]
fn qpdf_accepts_json_input_with_list_attachments() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("json-input/complete.json");
    assert_pair(
        "json-input + list-attachments",
        &[
            OsString::from("--json-input"),
            OsString::from("--list-attachments"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_json_input_show_attachment_reports_missing_key() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("json-input/complete.json");
    assert_status_output_pair(
        "json-input + show-attachment",
        &[
            OsString::from("--json-input"),
            OsString::from("--show-attachment=missing"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_json_input_applies_attachment_mutation_before_listing() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("json-input/complete.json");
    let payload = fixture("golden/inspect-npages.txt");
    assert_pair(
        "json-input + add-attachment + list-attachments",
        &[
            OsString::from("--json-input"),
            OsString::from("--add-attachment"),
            payload.as_os_str().to_owned(),
            OsString::from("--key=listed"),
            OsString::from("--creationdate=D:20200101000000Z"),
            OsString::from("--moddate=D:20200101000000Z"),
            OsString::from("--"),
            OsString::from("--list-attachments"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_json_input_applies_attachment_before_showing_it() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("json-input/complete.json");
    let payload = fixture("golden/inspect-npages.txt");
    assert_pair(
        "json-input + add-attachment + show-attachment",
        &[
            OsString::from("--json-input"),
            OsString::from("--add-attachment"),
            payload.as_os_str().to_owned(),
            OsString::from("--key=shown"),
            OsString::from("--"),
            OsString::from("--show-attachment=shown"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_accepts_check_linearization_with_json_input() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("json-input/complete.json");
    assert_pair(
        "check-linearization + json-input",
        &[
            OsString::from("--check-linearization"),
            OsString::from("--json-input"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_accepts_check_linearization_with_job_json_file() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary job JSON directory");
    let job_json = temp.path().join("job.json");
    let input = fixture("three-page.pdf");
    fs::write(&job_json, b"{}").expect("job JSON file");
    let args = [
        OsString::from("--check-linearization"),
        OsString::from(format!("--job-json-file={}", job_json.display())),
        input.as_os_str().to_owned(),
    ];
    assert_pair("check-linearization + job-json-file", &args);
}

#[test]
fn qpdf_accepts_remove_restrictions_with_show_object() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("three-page.pdf");
    assert_pair(
        "remove-restrictions + show-object",
        &[
            OsString::from("--remove-restrictions"),
            OsString::from("--show-object=trailer"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_accepts_writer_only_settings_with_json() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("three-page.pdf");
    let settings = [
        ("static-aes-iv", OsString::from("--static-aes-iv")),
        ("recompress-flate", OsString::from("--recompress-flate")),
        ("compression-level", OsString::from("--compression-level=9")),
        (
            "linearize-pass1",
            OsString::from("--linearize-pass1=/dev/null"),
        ),
        (
            "remove-restrictions",
            OsString::from("--remove-restrictions"),
        ),
        (
            "no-original-object-ids",
            OsString::from("--no-original-object-ids"),
        ),
        (
            "preserve-unreferenced",
            OsString::from("--preserve-unreferenced"),
        ),
        (
            "copy-encryption",
            OsString::from(format!("--copy-encryption={}", input.display())),
        ),
    ];

    for (label, setting) in settings {
        assert_pair(
            &format!("{label} + json"),
            &[
                setting,
                OsString::from("--json=2"),
                input.as_os_str().to_owned(),
                OsString::from("-"),
            ],
        );
    }
}

#[test]
fn qpdf_accepts_encrypt_with_show_object() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("three-page.pdf");
    assert_pair(
        "encrypt + show-object",
        &[
            OsString::from("--encrypt"),
            OsString::from(""),
            OsString::from(""),
            OsString::from("128"),
            OsString::from("--use-aes=y"),
            OsString::from("--"),
            OsString::from("--show-object=trailer"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_accepts_copy_encryption_with_show_npages() {
    if skip_without_qpdf() {
        return;
    }

    let input = fixture("three-page.pdf");
    assert_pair(
        "copy-encryption + show-npages",
        &[
            OsString::from(format!("--copy-encryption={}", input.display())),
            OsString::from("--show-npages"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_password_file_overrides_password_in_argv_order() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary password directory");
    let password_file = temp.path().join("password.txt");
    fs::write(&password_file, b"user-v4-aes\n").expect("password file");
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    assert_pair(
        "password-file overrides password",
        &[
            OsString::from("--password=wrong"),
            OsString::from(format!("--password-file={}", password_file.display())),
            OsString::from("--check"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn qpdf_password_overrides_password_file_when_it_comes_last() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary password directory");
    let password_file = temp.path().join("password.txt");
    fs::write(&password_file, b"user-v4-aes\n").expect("password file");
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    assert_status_output_pair(
        "password overrides password-file",
        &[
            OsString::from(format!("--password-file={}", password_file.display())),
            OsString::from("--password=wrong"),
            OsString::from("--check"),
            input.as_os_str().to_owned(),
        ],
    );
}

#[test]
fn attachment_mutations_follow_qpdf_remove_then_add_order() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary attachment output directory");
    let input = fixture("attachment-two-page.pdf");
    let payload = fixture("golden/inspect-npages.txt");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let args = [
        OsString::from("--static-id"),
        OsString::from("--qdf"),
        OsString::from("--remove-attachment=attachment.txt"),
        OsString::from("--add-attachment"),
        payload.as_os_str().to_owned(),
        OsString::from("--key=replaced"),
        OsString::from("--creationdate=D:20200101000000Z"),
        OsString::from("--moddate=D:20200101000000Z"),
        OsString::from("--"),
        input.as_os_str().to_owned(),
    ];

    let qpdf = ShellCommand::new("qpdf")
        .args(&args)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf attachment mutation should spawn");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(&args)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf attachment mutation should spawn");

    assert!(
        qpdf.status.success(),
        "qpdf attachment mutation failed: {qpdf:?}"
    );
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(qpdf_output.is_file());
    assert!(flpdf_output.is_file());

    let list_qpdf = ShellCommand::new("qpdf")
        .args(["--list-attachments"])
        .arg(&qpdf_output)
        .output()
        .expect("qpdf attachment listing should spawn");
    let list_flpdf = ShellCommand::new("qpdf")
        .args(["--list-attachments"])
        .arg(&flpdf_output)
        .output()
        .expect("qpdf should read flpdf attachment output");
    assert_eq!(list_qpdf.status.code(), Some(0));
    assert_eq!(list_flpdf.status.code(), Some(0));
    assert_eq!(list_flpdf.stdout, list_qpdf.stdout);
}

#[test]
fn attachment_mutation_can_share_qpdf_overlay_lifecycle() {
    if skip_without_qpdf() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary overlay output directory");
    let input = fixture("two-page.pdf");
    let overlay = fixture("one-page.pdf");
    let payload = fixture("golden/inspect-npages.txt");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let args = [
        OsString::from("--static-id"),
        OsString::from("--qdf"),
        OsString::from("--add-attachment"),
        payload.as_os_str().to_owned(),
        OsString::from("--key=overlay-attachment"),
        OsString::from("--creationdate=D:20200101000000Z"),
        OsString::from("--moddate=D:20200101000000Z"),
        OsString::from("--"),
        OsString::from("--overlay"),
        overlay.as_os_str().to_owned(),
        OsString::from("--"),
        input.as_os_str().to_owned(),
    ];

    let qpdf = ShellCommand::new("qpdf")
        .args(&args)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf attachment overlay should spawn");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(&args)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf attachment overlay should spawn");

    assert!(
        qpdf.status.success(),
        "qpdf attachment overlay failed: {qpdf:?}"
    );
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert_eq!(
        fs::read(flpdf_output).expect("flpdf overlay output"),
        fs::read(qpdf_output).expect("qpdf overlay output")
    );
}
