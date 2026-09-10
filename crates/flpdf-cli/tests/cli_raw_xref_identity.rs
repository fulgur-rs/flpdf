use assert_cmd::Command;
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
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

fn in_use_generation_65536_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let objects = [
        b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".as_slice(),
        b"2 0 obj\n42\nendobj\n".as_slice(),
        b"3 0 obj\n43\nendobj\n".as_slice(),
        b"4 0 obj\n44\nendobj\n".as_slice(),
        b"5 0 obj\n45\nendobj\n".as_slice(),
    ];
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for (index, offset) in offsets.iter().enumerate() {
        let generation = if index == 4 { 65_536 } else { 0 };
        bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn matching_in_use_generation_65536_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let objects = [
        b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".as_slice(),
        b"2 0 obj\n42\nendobj\n".as_slice(),
        b"3 0 obj\n43\nendobj\n".as_slice(),
        b"4 0 obj\n44\nendobj\n".as_slice(),
        b"5 65536 obj\n45\nendobj\n".as_slice(),
    ];
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for (index, offset) in offsets.iter().enumerate() {
        let generation = if index == 4 { 65_536 } else { 0 };
        bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn previous_generation_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let objects = [
        b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".as_slice(),
        b"2 0 obj\n42\nendobj\n".as_slice(),
        b"3 0 obj\n43\nendobj\n".as_slice(),
        b"4 0 obj\n44\nendobj\n".as_slice(),
        b"5 0 obj\n45\nendobj\n".as_slice(),
    ];
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }

    let previous_xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\n");

    let latest_xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for (index, offset) in offsets.iter().enumerate() {
        let generation = if index == 4 { 65_535 } else { 0 };
        bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 6 /Root 1 0 R /Prev {previous_xref_offset} >>\nstartxref\n{latest_xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    bytes
}

fn run_qpdf(path: &std::path::Path) -> Output {
    ProcessCommand::new("qpdf")
        .args(["--show-xref", path.to_str().unwrap()])
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(path: &std::path::Path) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf should build")
        // qpdf prefixes its diagnostics with the program name, so compare
        // under the same name rather than excusing the difference.
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--show-xref", path.to_str().unwrap()])
        .output()
        .expect("flpdf should spawn")
}

fn run_qpdf_json(path: &std::path::Path) -> Output {
    ProcessCommand::new("qpdf")
        .args(["--json=2", path.to_str().unwrap()])
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf_json(path: &std::path::Path) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--json=2", path.to_str().unwrap()])
        .output()
        .expect("flpdf should spawn")
}

fn run_qpdf_json_object(path: &std::path::Path, selector: &str) -> Output {
    ProcessCommand::new("qpdf")
        .args([
            "--json=2",
            &format!("--json-object={selector}"),
            path.to_str().unwrap(),
        ])
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf_json_object(path: &std::path::Path, selector: &str) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            "--json=2",
            &format!("--json-object={selector}"),
            path.to_str().unwrap(),
        ])
        .output()
        .expect("flpdf should spawn")
}

fn run_qpdf_check(path: &std::path::Path) -> Output {
    ProcessCommand::new("qpdf")
        .args(["--check", path.to_str().unwrap()])
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf_check(path: &std::path::Path) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--check", path.to_str().unwrap()])
        .output()
        .expect("flpdf should spawn")
}

fn run_qpdf_show_object(path: &std::path::Path, selector: &str) -> Output {
    ProcessCommand::new("qpdf")
        .args([&format!("--show-object={selector}"), path.to_str().unwrap()])
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf_show_object(path: &std::path::Path, selector: &str) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args([&format!("--show-object={selector}"), path.to_str().unwrap()])
        .output()
        .expect("flpdf should spawn")
}

#[test]
fn show_xref_preserves_in_use_generation_outside_object_ref_range() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("in-use-generation-65536.pdf");
    std::fs::write(&input, in_use_generation_65536_pdf()).expect("write fixture");

    let qpdf = run_qpdf(&input);
    let flpdf = run_flpdf(&input);

    assert!(qpdf.status.success(), "qpdf failed: {:?}", qpdf);
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(qpdf
        .stdout
        .windows(b"5/65536: uncompressed".len())
        .any(|window| { window == b"5/65536: uncompressed" }));
}

#[test]
fn check_resolves_raw_in_use_generation_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("in-use-generation-65536.pdf");
    std::fs::write(&input, in_use_generation_65536_pdf()).expect("write fixture");

    let qpdf = run_qpdf_check(&input);
    let flpdf = run_flpdf_check(&input);

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn check_accepts_a_matching_out_of_range_object_header_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("matching-in-use-generation-65536.pdf");
    std::fs::write(&input, matching_in_use_generation_65536_pdf()).expect("write fixture");

    let qpdf = run_qpdf_check(&input);
    let flpdf = run_flpdf_check(&input);

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn show_xref_discards_lower_raw_generation_after_prev_chain() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("prev-generation.pdf");
    std::fs::write(&input, previous_generation_pdf()).expect("write fixture");

    let qpdf = run_qpdf(&input);
    let flpdf = run_flpdf(&input);

    assert!(qpdf.status.success(), "qpdf failed: {:?}", qpdf);
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    let stdout = String::from_utf8_lossy(&qpdf.stdout);
    assert!(
        stdout.contains("5/65535: uncompressed"),
        "qpdf stdout: {stdout}"
    );
    assert!(
        !stdout.contains("5/0: uncompressed"),
        "qpdf stdout: {stdout}"
    );
}

#[test]
fn show_xref_preserves_generations_found_during_reconstruction() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let input =
        std::path::Path::new("../../tests/fixtures/compat/recovered-catalog-pagelabels.pdf");
    let qpdf = run_qpdf(input);
    let flpdf = run_flpdf(input);

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(qpdf
        .stdout
        .windows(b"5/0: uncompressed".len())
        .any(|window| window == b"5/0: uncompressed"));
    assert!(qpdf
        .stdout
        .windows(b"5/1: uncompressed".len())
        .any(|window| window == b"5/1: uncompressed"));
}

/// A document that repairs its own table while the loader is still resolving
/// the trailer keeps the repaired offsets in both views. qpdf rewrites
/// `m->xref_table` in situ and resumes against it (`QPDF.cc:518-620`), so the
/// loader's stale table must not be reinstated over either one.
fn stale_indirect_size_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in [
        (1u32, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (4, "6"),
    ] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let recovered_only = bytes.len();
    bytes.extend_from_slice(b"5 0 obj\n<< /Note (only found by reconstruction) >>\nendobj\n");

    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in &offsets[..3] {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    // Object 4 carries the trailer's indirect /Size, and its row points past
    // the object header, so resolving the trailer forces reconstruction.
    bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[3] + 40).as_bytes());
    bytes.extend_from_slice(format!("{recovered_only:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 4 0 R /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

#[test]
fn show_xref_reports_offsets_repaired_during_trailer_resolution() {
    if !qpdf_available() {
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("stale-indirect-size.pdf");
    std::fs::write(&path, stale_indirect_size_pdf()).expect("write fixture");

    let qpdf = run_qpdf(&path);
    let flpdf = run_flpdf(&path);
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn json_object_map_keys_a_matching_out_of_range_header_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("matching-in-use-generation-65536.pdf");
    std::fs::write(&input, matching_in_use_generation_65536_pdf()).expect("write fixture");

    let qpdf = run_qpdf_json(&input);
    let flpdf = run_flpdf_json(&input);

    // qpdf writes the object map from the raw header identity, so the
    // out-of-range generation appears verbatim as `obj:5 65536 R` rather than
    // being dropped or projected onto the `N G R` parser range.
    assert!(
        String::from_utf8_lossy(&qpdf.stdout).contains("\"obj:5 65536 R\""),
        "qpdf 11.9.0 is expected to key the object map by the raw identity"
    );
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn json_object_selection_is_unchanged_for_projectable_identities() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("matching-in-use-generation-65536.pdf");
    std::fs::write(&input, matching_in_use_generation_65536_pdf()).expect("write fixture");

    // Selecting by an identity that projects onto `ObjectRef` must keep
    // matching, and the out-of-range sibling in the same file must not
    // perturb it.
    for selector in ["1,0", "2,0", "4,0", "trailer"] {
        let qpdf = run_qpdf_json_object(&input, selector);
        let flpdf = run_flpdf_json_object(&input, selector);

        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "exit status for selector {selector}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout, "stdout for selector {selector}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "stderr for selector {selector}");
    }
}

#[test]
fn json_object_selection_accepts_a_raw_generation_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("matching-in-use-generation-65536.pdf");
    std::fs::write(&input, matching_in_use_generation_65536_pdf()).expect("write fixture");

    let qpdf = run_qpdf_json_object(&input, "5,65536");
    let flpdf = run_flpdf_json_object(&input, "5,65536");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn show_object_accepts_a_raw_generation_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("matching-in-use-generation-65536.pdf");
    std::fs::write(&input, matching_in_use_generation_65536_pdf()).expect("write fixture");

    let qpdf = run_qpdf_show_object(&input, "5,65536");
    let flpdf = run_flpdf_show_object(&input, "5,65536");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn dump_object_accepts_a_raw_generation_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("matching-in-use-generation-65536.pdf");
    std::fs::write(&input, matching_in_use_generation_65536_pdf()).expect("write fixture");
    let qpdf = ProcessCommand::new("qpdf")
        .args(["--show-object=5,65536", input.to_str().unwrap()])
        .output()
        .expect("qpdf should spawn");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["dump-object", "5 65536", input.to_str().unwrap()])
        .output()
        .expect("flpdf should spawn");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}
