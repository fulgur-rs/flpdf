//! Integration tests for `--object-streams` modes using qpdf as an external oracle.
//!
//! Covers flpdf-9hc.5.11 acceptance criteria (tests 1–4):
//!
//! 1. **preserve mode keeps qpdf membership** — after a qpdf-generated multi-ObjStm
//!    input is rewritten with `--object-streams=preserve`, every member-group present
//!    in the input reappears in the output (possibly under a renumbered container).
//!
//! 2. **disable mode emits no ObjStm** — `--object-streams=disable` on a multi-ObjStm
//!    input produces zero `compressed;` xref entries and no `/Type /ObjStm` streams.
//!
//! 3. **generate mode produces compressed xref entries on non-ObjStm input** — `one-page.pdf`
//!    (no source ObjStm) gets at least 2 compressed entries, and the Catalog / Pages
//!    objects resolve correctly from the output.
//!
//! 4. **generate eligibility matches qpdf semantically** — both qpdf and flpdf produce
//!    the same *count* of compressed entries when applying `--object-streams=generate`
//!    to `one-page.pdf`, confirming parity in the eligibility predicate and planner.
//!
//! When qpdf is not installed:
//! - **Linux CI** (`CI` env var set, non-Windows): the test panics — qpdf is a hard
//!   requirement on CI.
//! - **Local / Windows CI**: prints a diagnostic and returns early (tests skip).

use assert_cmd::Command as CargoCommand;
use flpdf::{Object, Pdf};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Fixture path
// ---------------------------------------------------------------------------

const COMPAT_FIXTURE_DIR: &str = "../../tests/fixtures/compat";

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT_FIXTURE_DIR)
        .join(name)
}

// ---------------------------------------------------------------------------
// qpdf guards — same policy as cli_linearize_qpdf.rs
// ---------------------------------------------------------------------------

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Returns `true` when the caller should return early (qpdf missing and skip allowed).
#[must_use]
fn skip_if_qpdf_missing() -> bool {
    if qpdf_available() {
        return false;
    }
    let on_ci = std::env::var_os("CI").is_some();
    let on_windows = cfg!(target_os = "windows");
    if on_ci && !on_windows {
        panic!(
            "qpdf is required for cli_object_streams_qpdf_parity tests on CI (Linux); \
             install qpdf in the workflow before running this test suite"
        );
    }
    eprintln!(
        "skipping: qpdf not available (target_os={}, CI={})",
        std::env::consts::OS,
        on_ci
    );
    true
}

// ---------------------------------------------------------------------------
// qpdf helpers
// ---------------------------------------------------------------------------

/// Run `qpdf --object-streams=generate <input> <output>`.
fn qpdf_generate(input: &Path, output: &Path) {
    let status = ShellCommand::new("qpdf")
        .args([
            "--object-streams=generate",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .status()
        .expect("failed to spawn qpdf --object-streams=generate");
    assert!(status.success(), "qpdf --object-streams=generate failed");
}

/// Run `qpdf --show-xref <path>` and return the raw stdout.
fn run_qpdf_show_xref(path: &Path) -> String {
    let out = ShellCommand::new("qpdf")
        .args(["--show-xref", path.to_str().unwrap()])
        .output()
        .expect("failed to spawn qpdf --show-xref");
    assert!(
        out.status.success(),
        "qpdf --show-xref failed on {}",
        path.display()
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// xref parser
// ---------------------------------------------------------------------------

/// A single xref entry as reported by `qpdf --show-xref`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum XrefRecord {
    Free,
    Uncompressed { num: u32, offset: u64 },
    Compressed { num: u32, stream: u32, index: u32 },
}

/// Parse `qpdf --show-xref` output into a list of `XrefRecord`s.
///
/// Line formats (qpdf 11.x):
/// - `N/G: free`
/// - `N/G: uncompressed; offset = O`
/// - `N/G: compressed; stream = S, index = I`
fn parse_qpdf_show_xref(output: &str) -> Vec<XrefRecord> {
    let mut records = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Split "N/G: ..." at the first ": "
        let Some((obj_part, rest)) = line.split_once(": ") else {
            continue;
        };
        // Parse N from "N/G"
        let Some((num_str, _gen_str)) = obj_part.split_once('/') else {
            continue;
        };
        let Ok(num) = num_str.trim().parse::<u32>() else {
            continue;
        };

        if rest.starts_with("free") {
            records.push(XrefRecord::Free);
        } else if let Some(offset_part) = rest.strip_prefix("uncompressed; offset = ") {
            let offset: u64 = offset_part.trim().parse().unwrap_or(0);
            records.push(XrefRecord::Uncompressed { num, offset });
        } else if let Some(compressed_part) = rest.strip_prefix("compressed; stream = ") {
            // "S, index = I"
            if let Some((stream_str, index_part)) = compressed_part.split_once(", index = ") {
                let stream: u32 = stream_str.trim().parse().unwrap_or(0);
                let index: u32 = index_part.trim().parse().unwrap_or(0);
                records.push(XrefRecord::Compressed { num, stream, index });
            }
        }
    }
    records
}

/// Extract the set of object numbers that have `Compressed` xref entries.
fn compressed_object_set(records: &[XrefRecord]) -> BTreeSet<u32> {
    records
        .iter()
        .filter_map(|r| {
            if let XrefRecord::Compressed { num, .. } = r {
                Some(*num)
            } else {
                None
            }
        })
        .collect()
}

/// Extract a membership map: container_object_number → BTreeSet<member_object_number>.
///
/// Used to compare preserve-mode groupings across rewrite.
fn membership_map(records: &[XrefRecord]) -> BTreeMap<u32, BTreeSet<u32>> {
    let mut map: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for record in records {
        if let XrefRecord::Compressed { num, stream, .. } = record {
            map.entry(*stream).or_default().insert(*num);
        }
    }
    map
}

// ---------------------------------------------------------------------------
// flpdf helpers
// ---------------------------------------------------------------------------

/// Run `flpdf rewrite --full-rewrite --object-streams=<mode> <input> <output>`.
fn rewrite_with_mode(mode: &str, input: &Path, output: &Path) {
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--full-rewrite",
            &format!("--object-streams={mode}"),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// Test 1: preserve mode keeps qpdf membership
// ---------------------------------------------------------------------------

#[test]
fn preserve_mode_keeps_qpdf_membership() {
    if skip_if_qpdf_missing() {
        return;
    }

    let tmp = tempdir().unwrap();
    let source = fixture_path("one-page.pdf");

    // Step 1: produce a qpdf-generated multi-ObjStm input.
    let multi_objstm = tmp.path().join("multi-objstm.pdf");
    qpdf_generate(&source, &multi_objstm);

    // Step 2: rewrite with flpdf --object-streams=preserve.
    let preserved = tmp.path().join("preserved.pdf");
    rewrite_with_mode("preserve", &multi_objstm, &preserved);

    // Step 3: parse xref of both files.
    let input_xref = parse_qpdf_show_xref(&run_qpdf_show_xref(&multi_objstm));
    let output_xref = parse_qpdf_show_xref(&run_qpdf_show_xref(&preserved));

    // Build member-sets for each container, then compare as a SET of member-sets
    // (container numbers may differ after renaming).
    let input_map = membership_map(&input_xref);
    let output_map = membership_map(&output_xref);

    // Collect the VALUE sets (ignoring container keys).
    let input_groups: BTreeSet<Vec<u32>> = input_map
        .values()
        .map(|s| s.iter().copied().collect::<Vec<_>>())
        .collect();
    let output_groups: BTreeSet<Vec<u32>> = output_map
        .values()
        .map(|s| s.iter().copied().collect::<Vec<_>>())
        .collect();

    assert_eq!(
        input_groups, output_groups,
        "preserve mode must keep the same ObjStm member groups (container numbers may differ);\n\
         input groups: {:?}\n\
         output groups: {:?}",
        input_groups, output_groups
    );
}

// ---------------------------------------------------------------------------
// Test 2: disable mode emits no ObjStm
// ---------------------------------------------------------------------------

#[test]
fn disable_mode_emits_no_objstm() {
    if skip_if_qpdf_missing() {
        return;
    }

    let tmp = tempdir().unwrap();
    let source = fixture_path("one-page.pdf");

    // Produce multi-ObjStm input.
    let multi_objstm = tmp.path().join("multi-objstm.pdf");
    qpdf_generate(&source, &multi_objstm);

    // Rewrite with disable.
    let disabled = tmp.path().join("disabled.pdf");
    rewrite_with_mode("disable", &multi_objstm, &disabled);

    // Check 1: qpdf --show-xref must have zero `compressed;` entries.
    let xref_output = run_qpdf_show_xref(&disabled);
    let records = parse_qpdf_show_xref(&xref_output);
    let compressed_count = records
        .iter()
        .filter(|r| matches!(r, XrefRecord::Compressed { .. }))
        .count();
    assert_eq!(
        compressed_count, 0,
        "disable mode must produce zero compressed xref entries; got {compressed_count}:\n{xref_output}"
    );

    // Check 2: no /Type /ObjStm stream in the output.
    let bytes = std::fs::read(&disabled).unwrap();
    let mut pdf = Pdf::open(Cursor::new(&bytes)).unwrap();
    let has_objstm = pdf.object_refs().into_iter().any(|r| {
        if let Ok(Object::Stream(s)) = pdf.resolve(r) {
            matches!(s.dict.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"ObjStm")
        } else {
            false
        }
    });
    assert!(
        !has_objstm,
        "disable mode must not emit any /Type /ObjStm stream in the output"
    );
}

// ---------------------------------------------------------------------------
// Test 3: generate mode produces compressed xref entries on non-ObjStm input
// ---------------------------------------------------------------------------

#[test]
fn generate_mode_produces_compressed_entries_on_plain_input() {
    if skip_if_qpdf_missing() {
        return;
    }

    let tmp = tempdir().unwrap();
    let source = fixture_path("one-page.pdf");

    // Rewrite with generate (source has no ObjStm).
    let generated = tmp.path().join("generated.pdf");
    rewrite_with_mode("generate", &source, &generated);

    // Check 1: at least 2 compressed xref entries.
    let xref_output = run_qpdf_show_xref(&generated);
    let records = parse_qpdf_show_xref(&xref_output);
    let compressed: BTreeSet<u32> = compressed_object_set(&records);
    assert!(
        compressed.len() >= 2,
        "generate mode on one-page.pdf must produce >= 2 compressed xref entries; got {}:\n{xref_output}",
        compressed.len()
    );

    // Check 2: Catalog and Pages objects resolve correctly via the trailer chain.
    let bytes = std::fs::read(&generated).unwrap();
    let mut pdf = Pdf::open(Cursor::new(&bytes)).unwrap();

    // Find Catalog via trailer /Root.
    let root_ref = pdf
        .root_ref()
        .expect("output PDF must have a /Root reference in trailer");
    let catalog = pdf.resolve(root_ref).expect("failed to resolve /Root");
    let catalog_dict = match &catalog {
        Object::Dictionary(d) => d.clone(),
        other => panic!("expected /Root to be a Dictionary, got {other:?}"),
    };
    assert!(
        matches!(catalog_dict.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"Catalog"),
        "/Root must resolve to a /Type /Catalog dictionary; dict = {catalog_dict:?}"
    );

    // Find Pages via /Root /Pages.
    let pages_ref = catalog_dict
        .get_ref("Pages")
        .expect("/Catalog must contain a /Pages reference");
    let pages = pdf.resolve(pages_ref).expect("failed to resolve /Pages");
    let pages_dict = match pages {
        Object::Dictionary(d) => d,
        other => panic!("expected /Pages to be a Dictionary, got {other:?}"),
    };
    assert!(
        matches!(pages_dict.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"Pages"),
        "/Pages must resolve to a /Type /Pages dictionary; dict = {pages_dict:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: generate eligibility matches qpdf semantically (count parity)
// ---------------------------------------------------------------------------

#[test]
fn generate_eligibility_count_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }

    let tmp = tempdir().unwrap();
    let source = fixture_path("one-page.pdf");

    // Reference: qpdf --object-streams=generate.
    let qpdf_out = tmp.path().join("qpdf-generated.pdf");
    qpdf_generate(&source, &qpdf_out);

    // flpdf: --object-streams=generate.
    let flpdf_out = tmp.path().join("flpdf-generated.pdf");
    rewrite_with_mode("generate", &source, &flpdf_out);

    // Parse xref of both outputs.
    let qpdf_xref = parse_qpdf_show_xref(&run_qpdf_show_xref(&qpdf_out));
    let flpdf_xref = parse_qpdf_show_xref(&run_qpdf_show_xref(&flpdf_out));

    let qpdf_compressed = compressed_object_set(&qpdf_xref);
    let flpdf_compressed = compressed_object_set(&flpdf_xref);

    // Both tools must compress the same number of objects.
    // Object numbers differ because qpdf renumbers (ObjStm gets number 1, members start at 2)
    // while flpdf retains original numbering.  Count equality is the semantic assertion.
    assert_eq!(
        qpdf_compressed.len(),
        flpdf_compressed.len(),
        "generate mode: qpdf and flpdf must compress the same NUMBER of objects;\n\
         qpdf compressed set (count {}): {:?}\n\
         flpdf compressed set (count {}): {:?}",
        qpdf_compressed.len(),
        qpdf_compressed,
        flpdf_compressed.len(),
        flpdf_compressed
    );
    assert!(
        !qpdf_compressed.is_empty(),
        "generate mode must compress at least one object"
    );
}
