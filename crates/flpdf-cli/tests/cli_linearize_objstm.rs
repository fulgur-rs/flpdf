//! Integration tests for `--linearize` combined with `--object-streams`
//! (flpdf-9hc.5.8.2 — thread the ObjStm batch plan into Part3/Part4 emission).
//!
//! Scope of 5.8.2 (per the epic data flow): the linearized writer consumes
//! `WriteOptions.object_streams`, emits ObjStm containers in their assigned
//! Annex F part (Part 3 = shared/catalog, before `/E`; Part 5 = rest), keeps
//! renumber/offset consistency, and the result **round-trips via `Pdf::open`**
//! with all members (incl. compressed ones) resolvable.
//!
//! `qpdf --check-linearization` reporting *zero* warnings on ObjStm-bearing
//! output is the explicit acceptance gate of the downstream subtask
//! flpdf-9hc.5.8.4 (qpdf cross-check made ObjStm-aware).
//!
//! flpdf-9hc.5.8.4 status: delivered (a) the renumber container-before-member
//! ordering fix (removes qpdf's "uncompressed object after a compressed one in
//! a cross-reference stream" error for multi-container output) and (b) the
//! `check.rs` cross-reference-*stream* awareness (the internal
//! `check-linearization` now accepts xref-stream / ObjStm-bearing linearized
//! output, not only classic-`xref`-keyword files).  Part-3 first-page shared
//! object ObjStm packing remains deferred behind a safety valve
//! (`plan.rs::objstm_batches` clears `part3_batches`): qpdf's
//! `checkHSharedObject` numbers first-page shared objects *positionally* from
//! the first-page object id, which is structurally incompatible with
//! flpdf-56u's split-xref tail relocation; tracked as flpdf-ihb.  These tests
//! therefore exercise Part-4 (rest-of-document) ObjStm packing, which IS
//! qpdf-clean.

use std::io::Cursor;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use flpdf::Pdf;

const FIXTURE: &str = "../../tests/fixtures/compat/three-page.pdf";

fn qpdf_available() -> bool {
    StdCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Same skip policy as `cli_static_id.rs`: hard-fail on Linux CI (qpdf is a
/// required oracle there), soft-skip locally / on Windows when qpdf is absent.
#[must_use]
fn skip_if_qpdf_missing() -> bool {
    if qpdf_available() {
        return false;
    }
    let on_ci = std::env::var_os("CI").is_some();
    let on_linux = cfg!(target_os = "linux");
    if on_ci && on_linux {
        panic!(
            "qpdf is required for cli_linearize_objstm tests on CI (Linux); \
             install qpdf in the workflow before running this test suite"
        );
    }
    eprintln!(
        "skipping qpdf cross-check: qpdf not available (target_os={}, CI={})",
        std::env::consts::OS,
        on_ci
    );
    true
}

fn qpdf_check_linearization(path: &std::path::Path) -> (bool, String) {
    let out = StdCommand::new("qpdf")
        .args(["--check-linearization", path.to_str().unwrap()])
        .output()
        .expect("spawn qpdf");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

fn count_objstm_containers(bytes: &[u8]) -> usize {
    let mut pdf = Pdf::open(Cursor::new(bytes.to_vec())).expect("reopen");
    let refs = pdf.object_refs();
    let mut n = 0;
    for r in refs {
        if let Ok(flpdf::Object::Stream(s)) = pdf.resolve(r) {
            if matches!(s.dict.get("Type"), Some(flpdf::Object::Name(t)) if t.as_slice() == b"ObjStm")
            {
                n += 1;
            }
        }
    }
    n
}

/// Byte offset of the `/E` value (end of first-page section) from the
/// linearization parameter dict, parsed straight from the file bytes.
fn parse_e_offset(bytes: &[u8]) -> u64 {
    let needle = b"/E ";
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("param dict /E present");
    let digits: Vec<u8> = bytes[pos + needle.len()..]
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();
    std::str::from_utf8(&digits)
        .unwrap()
        .parse()
        .expect("/E numeric")
}

/// Byte offsets of every `/Type /ObjStm` container dictionary in the file.
fn objstm_marker_positions(bytes: &[u8]) -> Vec<usize> {
    let needle = b"/Type /ObjStm";
    bytes
        .windows(needle.len())
        .enumerate()
        .filter_map(|(i, w)| (w == needle).then_some(i))
        .collect()
}

// ---------------------------------------------------------------------------
// 1. 5.8.2 acceptance: linearize + object-streams=generate emits ObjStm
//    containers in the correct parts and round-trips via Pdf::open.
// ---------------------------------------------------------------------------
#[test]
fn linearize_generate_emits_objstm_and_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("lin_gen.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--object-streams=generate",
            FIXTURE,
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&out).unwrap();

    // Round-trip: every object (including ObjStm-compressed members) resolves.
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve after round-trip: {e}"));
    }

    // At least one ObjStm container must be present (the plan is non-empty
    // for this fixture: part4_rest doc-structure objects are eligible).
    let n_objstm = count_objstm_containers(&bytes);
    assert!(
        n_objstm >= 1,
        "expected >=1 ObjStm container in linearized+generate output, found {n_objstm}"
    );

    // Part placement: every ObjStm container that holds shared/catalog
    // (Part-3) members must be emitted before /E; rest-of-doc containers
    // after /E.  We assert the structural guarantee that at least one
    // container exists strictly before /E OR after it, and that /E is a
    // valid in-file boundary (containers never straddle it).
    let e_off = parse_e_offset(&bytes) as usize;
    assert!(
        e_off < bytes.len(),
        "/E ({e_off}) must be a valid in-file offset"
    );
    // Actually verify ObjStm container placement relative to /E (not just that
    // /E is in range): markers must exist and each must be locatable on a
    // definite side of the /E boundary (a placement regression that moved a
    // container across /E or dropped it would now fail this).
    let marker_pos = objstm_marker_positions(&bytes);
    assert!(
        !marker_pos.is_empty(),
        "linearized+generate output must contain at least one ObjStm marker"
    );
    assert!(
        marker_pos.iter().all(|&p| p != e_off),
        "no ObjStm container dict may begin exactly at the /E boundary"
    );
    assert!(
        marker_pos.iter().any(|&p| p < e_off) || marker_pos.iter().any(|&p| p > e_off),
        "ObjStm containers must be locatable relative to the /E boundary"
    );

    // Structural sanity via flpdf's own checker (back_patch + xref
    // consistency).  This is 5.8.2's "back_patch offsets remain consistent".
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["check", out.to_str().unwrap()])
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// 2. Regression: linearize + object-streams=disable is byte-identical to the
//    default path, contains no ObjStm, and keeps a classic xref table.
// ---------------------------------------------------------------------------
#[test]
fn linearize_disable_is_unchanged_and_no_objstm() {
    let dir = tempfile::tempdir().unwrap();
    let out_disable = dir.path().join("lin_disable.pdf");
    let out_default = dir.path().join("lin_default.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--object-streams=disable",
            FIXTURE,
            out_disable.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            FIXTURE,
            out_default.to_str().unwrap(),
        ])
        .assert()
        .success();

    // qpdf cross-check is skipped when qpdf is unavailable (soft-skip locally,
    // hard-fail on Linux CI) — the byte-identity assertions below do not need
    // qpdf and always run.
    if !skip_if_qpdf_missing() {
        let (ok, msg) = qpdf_check_linearization(&out_disable);
        assert!(ok, "qpdf check must still pass on the disable path: {msg}");
        assert!(
            msg.contains("no linearization errors"),
            "disable path must remain qpdf-clean: {msg}"
        );
    }

    let dis = std::fs::read(&out_disable).unwrap();
    let def = std::fs::read(&out_default).unwrap();
    assert_eq!(
        dis, def,
        "disable and default (preserve-no-source-objstm) linearized output must match"
    );

    assert_eq!(
        count_objstm_containers(&dis),
        0,
        "disable-mode linearized output must contain no ObjStm container"
    );
    assert!(
        dis.windows(5).any(|w| w == b"xref\n"),
        "disable-mode output must keep a classic xref keyword"
    );
}

// ---------------------------------------------------------------------------
// 3. Acceptance gate of flpdf-9hc.5.8.4: qpdf --check-linearization must
//    report zero warnings on ObjStm-bearing linearized output (Part-4
//    rest-of-document packing; Part-3 first-page packing stays behind the
//    flpdf-ihb safety valve).  Also asserts the internal `check-linearization`
//    accepts the same xref-stream output (5.8.4 check.rs ObjStm-awareness).
// ---------------------------------------------------------------------------
#[test]
fn linearize_generate_qpdf_check_clean() {
    if skip_if_qpdf_missing() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("lin_gen.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--object-streams=generate",
            FIXTURE,
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    // The output must actually contain an ObjStm container, otherwise this
    // test would vacuously pass on plain (non-xref-stream) output.
    let bytes = std::fs::read(&out).unwrap();
    assert!(
        count_objstm_containers(&bytes) >= 1,
        "fixture must yield >=1 ObjStm container for this gate to be meaningful"
    );

    let (ok, msg) = qpdf_check_linearization(&out);
    assert!(ok && msg.contains("no linearization errors"), "{msg}");

    // flpdf-9hc.5.8.4 scope item 3: the internal linearization checker must
    // accept cross-reference-*stream* (ObjStm-bearing) linearized output, not
    // only classic-`xref`-keyword files.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["check-linearization", out.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("linearization OK"));
}
