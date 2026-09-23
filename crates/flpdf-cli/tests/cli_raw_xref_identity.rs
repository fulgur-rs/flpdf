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

fn recovery_huge_object_header_pdf() -> Vec<u8> {
    b"%PDF-1.4\n\
1 0 obj\n\
<< /Type /Catalog >>\n\
endobj\n\
3000000000 0 obj\n\
45\n\
endobj\n\
trailer\n\
<< /Size 2 /Root 1 0 R >>\n\
startxref\n\
0\n\
%%EOF\n"
        .to_vec()
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
fn show_xref_recovery_range_error_matches_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("recovery-huge-object-header.pdf");
    std::fs::write(&input, recovery_huge_object_header_pdf()).expect("write fixture");

    let qpdf = run_qpdf(&input);
    let flpdf = run_flpdf(&input);

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
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

fn reconstructed_size_missing_object_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let object_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
    // This stale row sends the indirect /Size lookup to object 1. Recovery
    // finds no object 2, so qpdf's hasKey resolves the child to null and
    // reports that the trailer lacks /Size at the post-reconstruction EOF.
    bytes.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 2 0 R /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
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
fn check_reports_missing_indirect_size_after_reconstruction_like_qpdf() {
    if !qpdf_available() {
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("missing-indirect-size.pdf");
    std::fs::write(&path, reconstructed_size_missing_object_pdf()).expect("write fixture");

    let qpdf = run_qpdf_check(&path);
    let flpdf = run_flpdf_check(&path);
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

/// Both identity maps name objects in one output number space, so the
/// allocation counter has to span them. Numbering the ordinary map alone hands
/// out a number a raw-identity object already holds; the later xref entry then
/// overwrites the earlier one and the file silently loses an object. `qpdf
/// --check` does not catch that, because it only follows the xref.
#[test]
fn raw_and_ordinary_objects_get_distinct_output_numbers() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("raw-and-ordinary-orphans.pdf");
    std::fs::write(&input, raw_and_ordinary_orphans_pdf()).expect("write fixture");

    for mode in ["disable", "preserve", "generate"] {
        let output = temp.path().join(format!("out-{mode}.pdf"));
        Command::cargo_bin("flpdf")
            .expect("flpdf should build")
            .env("FLPDF_PROGNAME", "qpdf")
            .args([
                "rewrite",
                "--static-id",
                "--preserve-unreferenced",
                &format!("--object-streams={mode}"),
            ])
            .arg(&input)
            .arg(&output)
            .assert()
            .success();

        let written = std::fs::read(&output).expect("read output");
        let mut numbers = Vec::new();
        let mut rest = written.as_slice();
        while let Some(at) = rest.windows(6).position(|w| w == b" 0 obj") {
            let head = &rest[..at];
            let start = head
                .iter()
                .rposition(|b| !b.is_ascii_digit())
                .map_or(0, |i| i + 1);
            if start < head.len() {
                numbers.push(String::from_utf8_lossy(&head[start..]).into_owned());
            }
            rest = &rest[at + 6..];
        }
        let mut sorted = numbers.clone();
        sorted.sort();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            before,
            "object-streams={mode} reused an output object number: {numbers:?}"
        );
    }
}

/// A catalog, a page tree, an unreferenced object whose header generation is
/// outside the `N G R` range, and an ordinary unreferenced object numbered
/// above it.
fn raw_and_ordinary_orphans_pdf() -> Vec<u8> {
    build_fixture(&[
        (1, 0, b"<< /Type /Catalog /Pages 2 0 R >>".as_slice()),
        (
            2,
            0,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".as_slice(),
        ),
        (
            3,
            0,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".as_slice(),
        ),
        (5, 65_536, b"45".as_slice()),
        (6, 0, b"(ordinary orphan)".as_slice()),
    ])
}

fn build_fixture(objects: &[(u32, u32, &[u8])]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (number, generation, body) in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{number} {generation} obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    for (index, (number, generation, _)) in objects.iter().enumerate() {
        bytes.extend_from_slice(
            format!("{number} 1\n{:010} {generation:05} n \n", offsets[index]).as_bytes(),
        );
    }
    let size = objects.iter().map(|(number, _, _)| number).max().unwrap() + 1;
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}

/// A classic xref table whose `/XRefStm` payload holds a free row for object 4
/// and then an unknown entry type.
///
/// `QPDF::processXRefStream` commits each free row inline while it walks the
/// payload, so object 4's tombstone is already in `m->deleted_objects` when
/// the type-3 row throws. `read_xrefTable` does not catch that, so `read_xref`
/// hands to `reconstruct_xref`, which keeps the tombstone for the whole line
/// scan -- `insertReconstructedXrefEntry` therefore refuses object 4 even
/// though `4 0 obj` is in the file. A classic table's own `f` rows cannot do
/// this: both implementations defer them until after the `/XRefStm` read, so a
/// failure inside the section discards them.
///
/// Object 6 exists so that the line scan overwrites the default type-0 row
/// that the throwing entry left behind; without it `--show-xref` aborts on
/// that unrenderable row instead of printing the reconstructed table.
fn xref_stm_free_row_then_unknown_type_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.5\n".to_vec();
    let mut offsets = [0usize; 7];
    for (number, body) in [
        (1usize, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (4, "42"),
        (6, "43"),
    ] {
        offsets[number] = bytes.len();
        bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }

    // /W [1 2 1]: object 0 free, object 4 free, object 6 unknown type 3.
    let payload: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0];
    offsets[5] = bytes.len();
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /W [1 2 1] /Index [0 1 4 1 6 1] /Size 7 /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[1]).as_bytes());
    bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[2]).as_bytes());
    bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[3]).as_bytes());
    // Object 4 is free here too: a live classic row would keep
    // `insertFreeXrefEntry` from recording the tombstone at all.
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[5]).as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 6 /Root 1 0 R /XRefStm {} >>\nstartxref\n{xref}\n%%EOF\n",
            offsets[5]
        )
        .as_bytes(),
    );
    bytes
}

#[test]
fn show_xref_suppresses_a_committed_free_row_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("xref-stm-free-row-then-unknown-type.pdf");
    std::fs::write(&input, xref_stm_free_row_then_unknown_type_pdf()).expect("write fixture");

    let qpdf = run_qpdf(&input);
    let flpdf = run_flpdf(&input);

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        String::from_utf8_lossy(&flpdf.stdout),
        String::from_utf8_lossy(&qpdf.stdout)
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    // Guard the fixture itself: the tombstone only exists because the
    // `/XRefStm` read fails after the free row, and object 4 is only missing
    // because that tombstone survived into reconstruction.
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains("unknown xref stream entry type 3"),
        "the fixture must fail the /XRefStm read: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let shown = String::from_utf8_lossy(&qpdf.stdout);
    assert!(
        shown.contains("6/0: uncompressed"),
        "reconstruction must have run: {shown}"
    );
    assert!(
        !shown.contains("4/0:"),
        "the committed free row must suppress object 4: {shown}"
    );
}
