//! Multi-source `--pages` preserve-unreferenced parity against qpdf 11.9.0.
//!
//! qpdf keeps the primary document as the output/base QPDF while it copies
//! foreign pages (`libqpdf/QPDFJob.cc:2360-2632`). Therefore its writer-level
//! `--preserve-unreferenced` option still sees genuinely unreferenced objects
//! from the primary input. The CLI's fresh merged-document route must retain
//! those objects as well.

use assert_cmd::Command;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn write_pdf_fixture(path: &Path, objects: &[(u32, &str)], root: u32) {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = BTreeMap::new();
    let max = objects.iter().map(|(number, _)| *number).max().unwrap_or(0);
    for (number, body) in objects {
        offsets.insert(*number, bytes.len() as u64);
        bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_offset = bytes.len();
    let size = max + 1;
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..=max {
        match offsets.get(&number) {
            Some(offset) => bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    std::fs::write(path, bytes).expect("synthetic PDF fixture should be writable");
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

fn run_qpdf(args: &[&str]) -> std::process::Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn normalize_qdf(input: &Path, output: &Path) -> Vec<u8> {
    let result = run_qpdf(&[
        "--qdf",
        "--object-streams=disable",
        "--no-original-object-ids",
        "--preserve-unreferenced",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "qpdf QDF normalization failed for {}: {}",
        input.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    std::fs::read(output).expect("normalized QDF should be readable")
}

fn has_primary_orphan_marker(qdf: &[u8]) -> bool {
    qdf.windows(b"unreachable root".len())
        .any(|window| window == b"unreachable root")
}

#[test]
fn multi_source_pages_preserve_primary_unreferenced_objects_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let primary = fixture("null-visible-preserve-unreachable.pdf");
    let foreign = fixture("one-page.pdf");
    let qpdf_default_output = temp.path().join("qpdf-default.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_default_output = temp.path().join("flpdf-default.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let qpdf_default_qdf = temp.path().join("qpdf-default-qdf.pdf");
    let qpdf_qdf = temp.path().join("qpdf-qdf.pdf");
    let flpdf_default_qdf = temp.path().join("flpdf-default-qdf.pdf");
    let flpdf_qdf = temp.path().join("flpdf-qdf.pdf");

    let qpdf_default_result = run_qpdf(&[
        primary.to_str().unwrap(),
        "--pages",
        foreign.to_str().unwrap(),
        "1",
        "--",
        qpdf_default_output.to_str().unwrap(),
    ]);
    assert!(
        qpdf_default_result.status.success(),
        "qpdf default multi-source --pages failed: {}",
        String::from_utf8_lossy(&qpdf_default_result.stderr)
    );

    let qpdf_result = run_qpdf(&[
        "--preserve-unreferenced",
        primary.to_str().unwrap(),
        "--pages",
        foreign.to_str().unwrap(),
        "1",
        "--",
        qpdf_output.to_str().unwrap(),
    ]);
    assert!(
        qpdf_result.status.success(),
        "qpdf multi-source --pages failed: {}",
        String::from_utf8_lossy(&qpdf_result.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--pages"])
        .arg(&foreign)
        .arg("1")
        .arg("--")
        .arg(&primary)
        .arg(&flpdf_default_output)
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--preserve-unreferenced", "--pages"])
        .arg(&foreign)
        .arg("1")
        .arg("--")
        .arg(&primary)
        .arg(&flpdf_output)
        .assert()
        .success();

    let qpdf_default_qdf = normalize_qdf(&qpdf_default_output, &qpdf_default_qdf);
    let qpdf_qdf = normalize_qdf(&qpdf_output, &qpdf_qdf);
    let flpdf_default_qdf = normalize_qdf(&flpdf_default_output, &flpdf_default_qdf);
    let flpdf_qdf = normalize_qdf(&flpdf_output, &flpdf_qdf);
    assert!(
        !has_primary_orphan_marker(&qpdf_default_qdf),
        "qpdf default output must drop the primary orphan marker"
    );
    assert!(
        !has_primary_orphan_marker(&flpdf_default_qdf),
        "flpdf default output must keep dropping the primary orphan marker"
    );
    assert!(
        has_primary_orphan_marker(&qpdf_qdf),
        "qpdf preserve output must retain the primary orphan marker"
    );
    assert!(
        has_primary_orphan_marker(&flpdf_qdf),
        "flpdf preserve output must retain the primary orphan marker"
    );
}

/// A preserved orphan may reference the primary's own structural roots
/// directly (here, the Catalog via `/Owner`). qpdf keeps the primary `QPDF`
/// in place, so that reference always resolves to the document's one true
/// Catalog. flpdf's merge instead builds a fresh Catalog/`/Pages` pair and
/// copies everything else into it, so the canonical foreign-object map must
/// seed the primary's original Catalog/`/Pages` refs onto their target
/// equivalents -- otherwise the orphan's reference either becomes
/// `Object::Null` (unseeded, dropped by the canonical copier) or a needless
/// duplicate Catalog copy.
///
/// A distinct external source is required (not just `.`): with only `.` in
/// the page-selection segment, `has_external_source` is false and the CLI
/// takes `run_page_extraction_from_repeated_pdf`, which operates on the
/// primary in place and never calls the preserve-aware page-spec handler's
/// canonical foreign-object map setup -- that route would pass even if the
/// seeding fix were removed.
///
/// This asserts semantic structure (marker survives, exactly one Catalog,
/// `/Owner` resolves to that Catalog's own object number) rather than full
/// output byte-identity: flpdf's foreign-object copy assigns target numbers
/// by sorted source `ObjectRef` rather than qpdf's `copyForeignObject`
/// discovery-order traversal, an unrelated, pre-existing numbering
/// difference (reproducible even with plain, non-preserve `--pages`
/// merges of a primary and this same foreign source, with no orphan
/// involved at all) that is out of scope here.
#[test]
fn multi_source_pages_preserve_orphan_reference_to_primary_catalog_resolves_to_target_catalog() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let primary = fixture("primary-orphan-references-catalog.pdf");
    let foreign = fixture("one-page.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf_result = run_qpdf(&[
        "--preserve-unreferenced",
        "--pages",
        ".",
        "1",
        foreign.to_str().unwrap(),
        "1",
        "--",
        "--static-id",
        primary.to_str().unwrap(),
        qpdf_output.to_str().unwrap(),
    ]);
    assert!(
        qpdf_result.status.success(),
        "qpdf multi-source --pages with --preserve-unreferenced failed: {}",
        String::from_utf8_lossy(&qpdf_result.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--preserve-unreferenced", "--pages", ".", "1"])
        .arg(&foreign)
        .arg("1")
        .arg("--")
        .arg("--static-id")
        .arg(&primary)
        .arg(&flpdf_output)
        .assert()
        .success();

    let qpdf_qdf_path = temp.path().join("qpdf-qdf.pdf");
    let flpdf_qdf_path = temp.path().join("flpdf-qdf.pdf");
    let qpdf_qdf = normalize_qdf(&qpdf_output, &qpdf_qdf_path);
    let flpdf_qdf = normalize_qdf(&flpdf_output, &flpdf_qdf_path);

    for (label, qdf) in [("qpdf", &qpdf_qdf), ("flpdf", &flpdf_qdf)] {
        assert!(
            has_primary_orphan_marker(qdf),
            "{label} output must retain the primary orphan marker"
        );
        assert_eq!(
            catalog_object_numbers(qdf).len(),
            1,
            "{label} output must contain exactly one /Type /Catalog object, not a duplicate: {}",
            String::from_utf8_lossy(qdf)
        );
    }

    let flpdf_catalog = catalog_object_numbers(&flpdf_qdf)[0];
    let flpdf_owner_target = owner_reference_target(&flpdf_qdf)
        .expect("flpdf output must retain the orphan's /Owner reference");
    assert_eq!(
        flpdf_owner_target,
        flpdf_catalog,
        "flpdf's orphan /Owner must resolve to its own output's single Catalog object, \
         not a null or a duplicate: {}",
        String::from_utf8_lossy(&flpdf_qdf)
    );
}

#[test]
fn selected_page_back_reference_to_primary_catalog_resolves_to_target_catalog() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let primary = temp.path().join("primary-selected-backref.pdf");
    write_pdf_fixture(
        &primary,
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Owner 1 0 R >>",
            ),
        ],
        1,
    );
    let foreign = fixture("one-page.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf_result = run_qpdf(&[
        "--preserve-unreferenced",
        "--pages",
        ".",
        "1",
        foreign.to_str().unwrap(),
        "1",
        "--",
        "--static-id",
        primary.to_str().unwrap(),
        qpdf_output.to_str().unwrap(),
    ]);
    assert!(
        qpdf_result.status.success(),
        "qpdf selected-page back-reference merge failed: {}",
        String::from_utf8_lossy(&qpdf_result.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--preserve-unreferenced", "--pages", ".", "1"])
        .arg(&foreign)
        .arg("1")
        .arg("--")
        .arg("--static-id")
        .arg(&primary)
        .arg(&flpdf_output)
        .assert()
        .success();

    let qpdf_qdf_path = temp.path().join("qpdf-qdf.pdf");
    let flpdf_qdf_path = temp.path().join("flpdf-qdf.pdf");
    let qpdf_qdf = normalize_qdf(&qpdf_output, &qpdf_qdf_path);
    let flpdf_qdf = normalize_qdf(&flpdf_output, &flpdf_qdf_path);

    for (label, qdf) in [("qpdf", &qpdf_qdf), ("flpdf", &flpdf_qdf)] {
        assert_eq!(
            catalog_object_numbers(qdf).len(),
            1,
            "{label} output must contain one Catalog"
        );
        assert_eq!(
            owner_reference_target(qdf),
            Some(catalog_object_numbers(qdf)[0]),
            "{label} selected page /Owner must resolve to the output Catalog"
        );
    }
}

#[test]
fn selected_page_indirect_null_is_retained_by_preserve_unreferenced() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let primary = temp.path().join("primary-selected-null.pdf");
    write_pdf_fixture(
        &primary,
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Foo 4 0 R >>",
            ),
            (4, "null"),
        ],
        1,
    );
    let foreign = fixture("one-page.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf_result = run_qpdf(&[
        "--preserve-unreferenced",
        "--pages",
        ".",
        "1",
        foreign.to_str().unwrap(),
        "1",
        "--",
        "--static-id",
        primary.to_str().unwrap(),
        qpdf_output.to_str().unwrap(),
    ]);
    assert!(
        qpdf_result.status.success(),
        "qpdf selected-page indirect-null merge failed: {}",
        String::from_utf8_lossy(&qpdf_result.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--preserve-unreferenced", "--pages", ".", "1"])
        .arg(&foreign)
        .arg("1")
        .arg("--")
        .arg("--static-id")
        .arg(&primary)
        .arg(&flpdf_output)
        .assert()
        .success();

    let qpdf_qdf_path = temp.path().join("qpdf-qdf.pdf");
    let flpdf_qdf_path = temp.path().join("flpdf-qdf.pdf");
    let qpdf_qdf = normalize_qdf(&qpdf_output, &qpdf_qdf_path);
    let flpdf_qdf = normalize_qdf(&flpdf_output, &flpdf_qdf_path);
    assert_eq!(
        null_object_numbers(&qpdf_qdf).len(),
        1,
        "qpdf must retain the selected page's indirect null object"
    );
    assert_eq!(
        null_object_numbers(&flpdf_qdf).len(),
        null_object_numbers(&qpdf_qdf).len(),
        "flpdf must retain the same indirect null object under preserve-unreferenced"
    );
}

/// Object numbers of every top-level `/Type /Catalog` object in `qdf`
/// (QDF-normalized, one object per `N 0 obj` line). A well-formed merge
/// output has exactly one.
fn catalog_object_numbers(qdf: &[u8]) -> Vec<u32> {
    let text = String::from_utf8_lossy(qdf);
    let text = text.as_ref();
    let obj_start = regex::Regex::new(r"(?m)^(\d+) 0 obj\n<<").unwrap();
    obj_start
        .captures_iter(text)
        .filter_map(|capture| {
            let start = capture.get(0).unwrap().start();
            let end = text[start..]
                .find("endobj")
                .map_or(text.len(), |i| start + i);
            text[start..end]
                .contains("/Type /Catalog")
                .then(|| capture[1].parse().unwrap())
        })
        .collect()
}

/// The object number `/Owner N 0 R` points at in `qdf`, if present.
fn owner_reference_target(qdf: &[u8]) -> Option<u32> {
    let text = String::from_utf8_lossy(qdf);
    let text = text.as_ref();
    regex::Regex::new(r"/Owner (\d+) 0 R")
        .unwrap()
        .captures(text)
        .map(|capture| capture[1].parse().unwrap())
}

fn null_object_numbers(qdf: &[u8]) -> Vec<u32> {
    let text = String::from_utf8_lossy(qdf);
    regex::Regex::new(r"(?m)^(\d+) 0 obj\nnull\nendobj")
        .unwrap()
        .captures_iter(&text)
        .map(|capture| capture[1].parse().unwrap())
        .collect()
}

/// When the primary itself already uses object streams, a compressed
/// member's own ref and its `/ObjStm` container's ref are both present in
/// `live_object_refs()`. Copying the container verbatim (rather than only
/// its still-live members, which the writer independently regenerates a
/// fresh container for) would duplicate the container's members a second
/// time as dead, dangling content nothing in the output references --
/// confirmed by a probe of `job/page_merge.rs`'s copy_seed construction
/// that showed exactly this: the `--object-streams=generate`-converted
/// primary's original container reappeared byte-for-byte in flpdf's output
/// (with source-side object numbers baked into its compressed payload,
/// meaningless in the target's renumbering) alongside the correctly copied
/// standalone member, inflating `/Size` and duplicating
/// `/Marker (ExclusivePage2Font)`.
///
/// The dangling duplicate is unreferenced from the live graph either way,
/// so a *further* qpdf normalization pass without `--preserve-unreferenced`
/// sweeps it away identically regardless of whether flpdf produced it --
/// that would launder away exactly the defect this test targets. Assert
/// directly against the raw, unprocessed CLI output instead: this multi-source
/// `--pages` route does not regenerate object streams, even without
/// `--preserve-unreferenced`, so a correct raw
/// output here has zero `/Type /ObjStm` objects and exactly one occurrence
/// of the marker string.
#[test]
fn multi_source_pages_preserve_does_not_duplicate_source_object_stream_container() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let primary = fixture("primary-objstm-exclusive-font.pdf");
    let foreign = fixture("no-stream-one-page.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf_result = run_qpdf(&[
        "--preserve-unreferenced",
        "--pages",
        ".",
        "1",
        foreign.to_str().unwrap(),
        "1",
        "--",
        "--static-id",
        primary.to_str().unwrap(),
        qpdf_output.to_str().unwrap(),
    ]);
    assert!(
        qpdf_result.status.success(),
        "qpdf multi-source --pages with --preserve-unreferenced failed: {}",
        String::from_utf8_lossy(&qpdf_result.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--preserve-unreferenced", "--pages", ".", "1"])
        .arg(&foreign)
        .arg("1")
        .arg("--")
        .arg("--static-id")
        .arg(&primary)
        .arg(&flpdf_output)
        .assert()
        .success();

    // A further qpdf normalization pass (with or without
    // --preserve-unreferenced) sweeps away the dangling duplicate either
    // way, since it is unreferenced from the live graph regardless of
    // whether flpdf produced it -- that would launder away exactly the
    // defect this test targets. Check the raw, unprocessed CLI output
    // directly instead.
    let flpdf_bytes = std::fs::read(&flpdf_output).expect("flpdf output should be readable");
    let objstm_count = flpdf_bytes
        .windows(b"/Type /ObjStm".len())
        .filter(|window| *window == b"/Type /ObjStm")
        .count();
    assert_eq!(
        objstm_count, 0,
        "flpdf does not regenerate object streams for multi-source --pages \
         output, so a correct raw output here has zero /Type /ObjStm objects; \
         a non-zero count means the source's original container was copied \
         verbatim as a dangling duplicate"
    );
    let marker_count = flpdf_bytes
        .windows(b"ExclusivePage2Font".len())
        .filter(|window| *window == b"ExclusivePage2Font")
        .count();
    assert_eq!(
        marker_count, 1,
        "the preserved font must appear exactly once in flpdf's raw output, \
         not duplicated via a dangling copy of the source object-stream container"
    );
}
