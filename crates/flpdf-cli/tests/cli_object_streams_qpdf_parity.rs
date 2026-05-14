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
//! 4. **generate eligibility is complete for both tools** — both qpdf and flpdf compress
//!    *all* eligible non-stream objects when applying `--object-streams=generate` to
//!    `one-page.pdf` (no eligible object is left uncompressed in either output).
//!
//! When qpdf is not installed:
//! - **Linux CI** (`CI` env var set, non-Windows): the test panics — qpdf is a hard
//!   requirement on CI.
//! - **Local / Windows CI**: prints a diagnostic and returns early (tests skip).

use assert_cmd::Command as CargoCommand;
use flpdf::{Object, ObjectRef, Pdf};
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
            // Skip lines we cannot parse rather than silently substituting 0,
            // which would make a future qpdf format change show up as bogus
            // "object 0 lives at offset 0" entries instead of a test diagnostic.
            let Ok(offset) = offset_part.trim().parse::<u64>() else {
                continue;
            };
            records.push(XrefRecord::Uncompressed { num, offset });
        } else if let Some(compressed_part) = rest.strip_prefix("compressed; stream = ") {
            // "S, index = I"
            if let Some((stream_str, index_part)) = compressed_part.split_once(", index = ") {
                let Ok(stream) = stream_str.trim().parse::<u32>() else {
                    continue;
                };
                let Ok(index) = index_part.trim().parse::<u32>() else {
                    continue;
                };
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
// Test 4: generate eligibility is complete for both tools (SET parity)
// ---------------------------------------------------------------------------

/// Returns the set of object numbers that are uncompressed but would be
/// eligible for ObjStm placement.
///
/// An object is considered eligible iff (on `one-page.pdf`, which has no
/// Encryption dict or Linearization parameter dict):
///   - generation == 0 (enforced by the xref — all objects here are gen-0)
///   - it is NOT a stream
///   - it is NOT a /Type /ObjStm or /Type /XRef dictionary
///
/// This mirrors the `is_eligible_for_objstm` predicate implemented in flpdf,
/// applied here to a PDF opened with the flpdf public API.
fn uncovered_eligible_objects(path: &Path, xref: &[XrefRecord]) -> BTreeSet<u32> {
    let bytes = std::fs::read(path).unwrap();
    let mut pdf = Pdf::open(Cursor::new(&bytes)).unwrap();

    // Defensive guard for the assumption baked into the eligibility check below:
    // we treat every uncompressed gen-0 non-stream non-ObjStm/XRef object as
    // eligible.  That is only correct when the fixture has no Encryption dict
    // and no Linearization parameter dict.  If the fixture grows either, the
    // caller must broaden this function (or pick a different fixture).
    assert!(
        !pdf.is_encrypted(),
        "uncovered_eligible_objects assumes the fixture is not encrypted; \
         broaden the eligibility check before reusing it on encrypted inputs"
    );
    assert!(
        pdf.linearized_hint_ref().ok().flatten().is_none(),
        "uncovered_eligible_objects assumes the fixture is not linearized; \
         broaden the eligibility check before reusing it on linearized inputs"
    );

    // Build set of compressed object numbers from xref.
    let compressed: BTreeSet<u32> = compressed_object_set(xref);

    // Resolve every (number, generation) the document actually advertises so we
    // can look up the real generation per number rather than hard-coding 0.
    // Conforming xref-stream PDFs assign generation 0 to all in-use entries,
    // but reading it from the source keeps this honest if that ever changes.
    let mut gen_by_number: std::collections::HashMap<u32, u16> = std::collections::HashMap::new();
    for r in pdf.object_refs() {
        gen_by_number.insert(r.number, r.generation);
    }

    let mut uncovered = BTreeSet::new();
    for record in xref {
        let num = match record {
            XrefRecord::Uncompressed { num, .. } => *num,
            // Already compressed — not our target.
            _ => continue,
        };
        // Resolve the object at its real generation (0 for conforming fixtures).
        // Non-zero generation is ineligible per ObjStm rules — skip.
        let generation = match gen_by_number.get(&num) {
            Some(g) => *g,
            None => continue,
        };
        if generation != 0 {
            continue;
        }
        let obj_ref = ObjectRef::new(num, generation);
        let obj = match pdf.resolve(obj_ref) {
            Ok(o) => o,
            // If we cannot resolve it, skip (e.g. object 0 free).
            Err(_) => continue,
        };
        // Streams are always ineligible.
        if matches!(obj, Object::Stream(_)) {
            continue;
        }
        // Check dict /Type for ObjStm / XRef.
        if let Object::Dictionary(ref d) = obj {
            let type_bytes = match d.get("Type") {
                Some(Object::Name(n)) => Some(n.as_slice()),
                _ => None,
            };
            if let Some(t) = type_bytes {
                if t == b"ObjStm" || t == b"XRef" {
                    continue;
                }
            }
        }
        // This object is eligible but uncompressed.
        if !compressed.contains(&num) {
            uncovered.insert(num);
        }
    }
    uncovered
}

/// Test 4: both qpdf and flpdf achieve **complete** coverage — no eligible
/// object is left uncompressed.
///
/// `one-page.pdf` has no Encryption dict or Linearization parameter dict,
/// so the only ineligible objects are stream objects (page content, ObjStm,
/// XRef stream) — which are also uncompressed.  Every non-stream
/// gen-0 object must end up in an ObjStm.
///
/// Object numbers differ because qpdf renumbers on generate (ObjStm gets
/// number 1, members start at 2) while flpdf retains original numbering.
/// Asserting that both sets of uncovered-eligible objects are empty is
/// therefore equivalent to the full SET parity requirement in the spec.
#[test]
fn generate_eligibility_complete_for_both_tools() {
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

    // Both outputs must have at least some compressed entries (basic sanity).
    let qpdf_compressed = compressed_object_set(&qpdf_xref);
    let flpdf_compressed = compressed_object_set(&flpdf_xref);
    assert!(
        !qpdf_compressed.is_empty(),
        "qpdf generate must compress at least one object"
    );
    assert!(
        !flpdf_compressed.is_empty(),
        "flpdf generate must compress at least one object"
    );

    // Neither tool may leave an eligible non-stream object uncompressed.
    let qpdf_uncovered = uncovered_eligible_objects(&qpdf_out, &qpdf_xref);
    let flpdf_uncovered = uncovered_eligible_objects(&flpdf_out, &flpdf_xref);

    assert!(
        qpdf_uncovered.is_empty(),
        "qpdf --object-streams=generate left eligible objects uncompressed: {:?}",
        qpdf_uncovered
    );
    assert!(
        flpdf_uncovered.is_empty(),
        "flpdf --object-streams=generate left eligible objects uncompressed: {:?}",
        flpdf_uncovered
    );

    // Also verify counts are equal (belt-and-suspenders — the completeness
    // assertions above already imply this for this fixture).
    assert_eq!(
        qpdf_compressed.len(),
        flpdf_compressed.len(),
        "generate mode: compressed-object count must agree;\n\
         qpdf ({} objects): {:?}\n\
         flpdf ({} objects): {:?}",
        qpdf_compressed.len(),
        qpdf_compressed,
        flpdf_compressed.len(),
        flpdf_compressed
    );
}
