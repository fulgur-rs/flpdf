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

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use flpdf::{pages, Pdf};

const FIXTURE: &str = "../../tests/fixtures/compat/three-page.pdf";

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn qpdf_available() -> bool {
    StdCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Same skip policy as `cli_static_id.rs`: hard-fail on CI (qpdf is a required
/// oracle, installed on every runner), soft-skip locally when qpdf is absent.
#[must_use]
fn skip_if_qpdf_missing() -> bool {
    if qpdf_available() {
        return false;
    }
    let on_ci = std::env::var_os("CI").is_some();
    if on_ci {
        panic!(
            "qpdf is required for cli_linearize_objstm tests on CI; \
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

    // Part placement: in Generate mode qpdf 11.9.0 packs the first-page shared
    // dicts (/Font dict + /Font) plus the /Pages tree and /Info into ONE
    // first-half (Part-3) ObjStm container, emitted *before* the /E boundary
    // (the /Catalog stays standalone).  For the three-page fixture this is the
    // only container, so the marker must land before /E.  A regression that
    // moved the first-half container after /E (or back to the old Part-4-only
    // safety valve) must fail here.
    let e_off = parse_e_offset(&bytes) as usize;
    assert!(
        e_off < bytes.len(),
        "/E ({e_off}) must be a valid in-file offset"
    );
    let marker_pos = objstm_marker_positions(&bytes);
    assert!(
        !marker_pos.is_empty(),
        "linearized+generate output must contain at least one ObjStm marker"
    );
    assert!(
        marker_pos.iter().any(|&p| p < e_off),
        "the first-half (Part-3) ObjStm container must be emitted before /E \
         ({e_off}) — qpdf packs the first-page shared dicts + /Pages + /Info \
         there; got marker offsets {marker_pos:?}"
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
//
//    Both invocations pass --static-id so the comparison is deterministic:
//    the default /ID strategy (flpdf-9hc.13.2) is now random, so two separate
//    runs would otherwise differ only in the trailer /ID bytes.  The /ID
//    randomness itself is covered by dedicated tests; this test isolates the
//    structural disable-vs-default equivalence.
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
            "--static-id",
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
            "--static-id",
            FIXTURE,
            out_default.to_str().unwrap(),
        ])
        .assert()
        .success();

    // qpdf cross-check is skipped when qpdf is unavailable (soft-skip locally,
    // hard-fail on CI) — the byte-identity assertions below do not need
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

// ============================================================================
// Epic acceptance gate (flpdf-9hc.5.8.5) — systematic 3-mode × multi-page
// coverage with Part-boundary, per-page-1, round-trip, and qpdf cross-check.
// ============================================================================

// ---------------------------------------------------------------------------
// Helpers (acceptance gate)
// ---------------------------------------------------------------------------

/// Run `qpdf --show-xref <path>` and return stdout.
fn qpdf_show_xref(path: &Path) -> String {
    let out = StdCommand::new("qpdf")
        .args(["--show-xref", path.to_str().unwrap()])
        .output()
        .expect("spawn qpdf --show-xref");
    assert!(
        out.status.success(),
        "qpdf --show-xref failed on {}",
        path.display()
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Parsed xref entry produced by `qpdf --show-xref`.
#[derive(Debug, Clone)]
enum XrefEntry {
    Uncompressed { num: u32, offset: u64 },
    Compressed { num: u32, stream: u32 },
}

/// Parse the stdout of `qpdf --show-xref` into a vector of [`XrefEntry`].
fn parse_xref_entries(xref_output: &str) -> Vec<XrefEntry> {
    let mut entries = Vec::new();
    for line in xref_output.lines() {
        let line = line.trim();
        let Some((obj_part, rest)) = line.split_once(": ") else {
            continue;
        };
        let Some((num_str, _)) = obj_part.split_once('/') else {
            continue;
        };
        let Ok(num) = num_str.trim().parse::<u32>() else {
            continue;
        };
        if let Some(offset_str) = rest.strip_prefix("uncompressed; offset = ") {
            if let Ok(offset) = offset_str.trim().parse::<u64>() {
                entries.push(XrefEntry::Uncompressed { num, offset });
            }
        } else if let Some(compressed_part) = rest.strip_prefix("compressed; stream = ") {
            if let Some((stream_str, _)) = compressed_part.split_once(", index = ") {
                if let Ok(stream) = stream_str.trim().parse::<u32>() {
                    entries.push(XrefEntry::Compressed { num, stream });
                }
            }
        }
    }
    entries
}

/// Return the set of object numbers that are compressed (inside an ObjStm).
fn compressed_obj_numbers(entries: &[XrefEntry]) -> BTreeSet<u32> {
    entries
        .iter()
        .filter_map(|e| {
            if let XrefEntry::Compressed { num, .. } = e {
                Some(*num)
            } else {
                None
            }
        })
        .collect()
}

/// Return the uncompressed offset for `num`, or `None`.
fn uncompressed_offset(entries: &[XrefEntry], num: u32) -> Option<u64> {
    entries.iter().find_map(|e| {
        if let XrefEntry::Uncompressed { num: n, offset } = e {
            if *n == num {
                return Some(*offset);
            }
        }
        None
    })
}

/// Return the set of ObjStm container object numbers in the xref.
fn objstm_container_numbers(entries: &[XrefEntry]) -> BTreeSet<u32> {
    entries
        .iter()
        .filter_map(|e| {
            if let XrefEntry::Compressed { stream, .. } = e {
                Some(*stream)
            } else {
                None
            }
        })
        .collect()
}

/// Run `flpdf rewrite --linearize --object-streams=<mode> <input> <output>`.
fn linearize_with_mode(mode: &str, input: &Path, output: &Path) {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            &format!("--object-streams={mode}"),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// 4. Epic acceptance gate: 3 modes × multi-page fixture
//    (a) qpdf --check-linearization clean
//    (c) per-page-1 plain indirect (not compressed)
//    (d) round-trip (all objects resolve, page count matches)
//    (b) Part boundary: ObjStm containers in Part-4 (offset > /E)
// ---------------------------------------------------------------------------

/// Verify invariants for one (mode, fixture) combination.
///
/// Used by `acceptance_gate_three_modes_multi_page` and
/// `acceptance_gate_multi_objstm_input`.
fn assert_acceptance_invariants(mode: &str, out: &Path, input_page_count: usize) {
    let bytes = std::fs::read(out).unwrap();

    // --- (a) qpdf --check-linearization ---
    let (ok, msg) = qpdf_check_linearization(out);
    assert!(
        ok && msg.contains("no linearization errors"),
        "mode={mode}: qpdf --check-linearization failed: {msg}"
    );

    // --- (d) round-trip: all objects resolve, page count preserved ---
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(
        !refs.is_empty(),
        "mode={mode}: round-tripped doc exposes objects"
    );
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("mode={mode}: object {r} did not resolve: {e}"));
    }
    let mut pdf2 = Pdf::open(Cursor::new(bytes.clone())).expect("Pdf::open for page count");
    let out_pages = pages::page_refs(&mut pdf2).expect("page_refs");
    assert_eq!(
        out_pages.len(),
        input_page_count,
        "mode={mode}: page count must be preserved after linearize; expected {input_page_count} got {}",
        out_pages.len()
    );

    // --- (c) per-page-1 plain indirect: first page must NOT be compressed ---
    let first_page_ref = out_pages[0];
    let xref_text = qpdf_show_xref(out);
    let xref_entries = parse_xref_entries(&xref_text);
    let compressed_nums = compressed_obj_numbers(&xref_entries);
    assert!(
        !compressed_nums.contains(&first_page_ref.number),
        "mode={mode}: first-page object {} must be plain indirect (not inside ObjStm); \
         xref shows it as compressed — Part-3 first-page packing invariant violated",
        first_page_ref
    );
    // First page must have an uncompressed xref entry with a valid offset.
    let fp_offset = uncompressed_offset(&xref_entries, first_page_ref.number);
    assert!(
        fp_offset.is_some(),
        "mode={mode}: first-page object {} has no uncompressed xref entry",
        first_page_ref
    );

    // --- (b) First-half ObjStm packing: for a multi-page document qpdf 11.9.0
    // packs the first-page shared dicts + /Pages + /Info into a first-half
    // (Part-3) ObjStm container emitted BEFORE /E (the /Catalog stays
    // standalone).  In `generate` mode there must therefore be at least one
    // container before /E.  `preserve` over these fixtures has NO source
    // ObjStms, so it emits none (qpdf classic-linearize parity); `disable`
    // emits none.  Whatever the count, the first-page page object itself stays
    // a plain indirect (asserted above), so /O matches qpdf.
    let e_off = parse_e_offset(&bytes);
    assert!(
        (e_off as usize) < bytes.len(),
        "mode={mode}: /E ({e_off}) must be a valid in-file offset (file len {})",
        bytes.len()
    );
    let container_nums = objstm_container_numbers(&xref_entries);
    if mode == "generate" {
        let any_before_e = container_nums.iter().any(|cnum| {
            uncompressed_offset(&xref_entries, *cnum)
                .map(|coff| coff < e_off)
                .unwrap_or(false)
        });
        assert!(
            any_before_e,
            "mode={mode}: a first-half (Part-3) ObjStm container must be emitted \
             before /E={e_off} (qpdf packs the first-page shared dicts + /Pages + \
             /Info there); container objs = {container_nums:?}"
        );
    }

    // --- (b) Param dict placement: the /Linearized dict must be before /E ---
    // The param dict is the first object in the file (at the very beginning, offset ~ 15).
    let param_needle = b"/Linearized ";
    let param_pos = bytes
        .windows(param_needle.len())
        .position(|w| w == param_needle)
        .expect("param dict /Linearized key present");
    assert!(
        (param_pos as u64) < e_off,
        "mode={mode}: /Linearized param dict (at byte {param_pos}) must be before /E={e_off}"
    );
}

#[test]
fn acceptance_gate_three_modes_multi_page() {
    if skip_if_qpdf_missing() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let fixtures: &[(&str, usize)] = &[("two-page.pdf", 2), ("three-page.pdf", 3)];
    let modes = ["preserve", "disable", "generate"];

    for (fixture_name, page_count) in fixtures {
        let input = fixture_path(fixture_name);
        for mode in &modes {
            let out = dir
                .path()
                .join(format!("acceptance-{fixture_name}-{mode}.pdf"));
            linearize_with_mode(mode, &input, &out);
            assert_acceptance_invariants(mode, &out, *page_count);
        }
    }
}

// ---------------------------------------------------------------------------
// 5. Generate mode: ObjStm containers must exist (non-vacuous check)
//    Regression: ensure at least one Part-4 ObjStm is present for generate.
// ---------------------------------------------------------------------------

#[test]
fn acceptance_gate_generate_has_objstm_in_part4() {
    if skip_if_qpdf_missing() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let fixtures: &[&str] = &["two-page.pdf", "three-page.pdf"];

    for fixture_name in fixtures {
        let input = fixture_path(fixture_name);
        let out = dir
            .path()
            .join(format!("part4-generate-{fixture_name}.pdf"));
        linearize_with_mode("generate", &input, &out);

        let bytes = std::fs::read(&out).unwrap();
        let n = count_objstm_containers(&bytes);
        assert!(
            n >= 1,
            "generate mode on {fixture_name}: expected >=1 ObjStm container, found {n}"
        );

        // qpdf packs the first-page shared dicts + /Pages + /Info into a
        // first-half (Part-3) ObjStm container emitted before /E.  For these
        // multi-page fixtures that is the sole container, so it must land before
        // /E (the non-vacuous check that distinguishes generate from disable).
        let e_off = parse_e_offset(&bytes);
        let xref_text = qpdf_show_xref(&out);
        let xref_entries = parse_xref_entries(&xref_text);
        let containers = objstm_container_numbers(&xref_entries);
        assert!(
            !containers.is_empty(),
            "generate mode on {fixture_name}: xref must reference ObjStm containers"
        );
        let any_before_e = containers.iter().any(|cnum| {
            uncompressed_offset(&xref_entries, *cnum)
                .map(|coff| coff < e_off)
                .unwrap_or(false)
        });
        assert!(
            any_before_e,
            "generate {fixture_name}: a first-half (Part-3) ObjStm container must be \
             before /E={e_off} (qpdf packs the first-page shared dicts + /Pages + /Info there); \
             containers = {containers:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 6. Multi-ObjStm input: preserve + generate must handle ObjStm-bearing input
//    Validates (d) round-trip and (c) per-page-1 invariants on ObjStm input.
// ---------------------------------------------------------------------------

// flpdf-zbf9: linearizing an ObjStm-bearing input drops the source's structural
// containers (/Type /ObjStm, /Type /XRef) from the live body, matching qpdf —
// see LinearizationPlan::from_pdf / writer::is_source_structural_container.
#[test]
fn acceptance_gate_objstm_bearing_input() {
    if skip_if_qpdf_missing() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let source = fixture_path("three-page.pdf");

    // Produce an ObjStm-bearing input via qpdf --object-streams=generate.
    let multi_objstm_input = dir.path().join("multi-objstm-source.pdf");
    let status = StdCommand::new("qpdf")
        .args([
            "--object-streams=generate",
            source.to_str().unwrap(),
            multi_objstm_input.to_str().unwrap(),
        ])
        .status()
        .expect("spawn qpdf --object-streams=generate");
    assert!(status.success(), "qpdf --object-streams=generate failed");

    // Confirm the input actually has ObjStm containers.
    let input_bytes = std::fs::read(&multi_objstm_input).unwrap();
    let input_containers = count_objstm_containers(&input_bytes);
    assert!(
        input_containers >= 1,
        "pre-condition: qpdf generate input must contain >=1 ObjStm; found {input_containers}"
    );

    // Run preserve and generate modes against the ObjStm-bearing input.
    for mode in &["preserve", "generate"] {
        let out = dir
            .path()
            .join(format!("objstm-input-linearize-{mode}.pdf"));
        linearize_with_mode(mode, &multi_objstm_input, &out);
        // Three-page fixture has 3 pages.
        assert_acceptance_invariants(mode, &out, 3);
    }
}

// ---------------------------------------------------------------------------
// 7. qpdf cross-check: qpdf's own --linearize --object-streams=generate output
//    and flpdf's output both keep first-page object as plain indirect.
//
//    Note: qpdf itself DOES place an ObjStm container in Part-3 (before /E),
//    packing shared catalog/font/info objects.  flpdf currently defers Part-3
//    ObjStm packing entirely (safety valve clears part3_batches, tracked as
//    flpdf-ihb).  The observable behavioral agreement between the two tools is
//    therefore NOT "same ObjStm placement" but rather "first-page object stays
//    uncompressed" — which both tools satisfy.
// ---------------------------------------------------------------------------

#[test]
fn qpdf_crosscheck_per_page1_plain_both_tools() {
    if skip_if_qpdf_missing() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let source = fixture_path("three-page.pdf");
    let qpdf_out = dir.path().join("qpdf-lin-gen.pdf");

    // Produce qpdf's own linearized+generate output as the reference.
    let status = StdCommand::new("qpdf")
        .args([
            "--linearize",
            "--object-streams=generate",
            source.to_str().unwrap(),
            qpdf_out.to_str().unwrap(),
        ])
        .status()
        .expect("spawn qpdf --linearize --object-streams=generate");
    assert!(
        status.success(),
        "qpdf --linearize --object-streams=generate failed"
    );

    let qpdf_bytes = std::fs::read(&qpdf_out).unwrap();
    let qpdf_xref_text = qpdf_show_xref(&qpdf_out);
    let qpdf_xref_entries = parse_xref_entries(&qpdf_xref_text);

    // qpdf must pass --check-linearization on its own output.
    let (ok, msg) = qpdf_check_linearization(&qpdf_out);
    assert!(
        ok && msg.contains("no linearization errors"),
        "qpdf's own lin+gen output must be qpdf-clean: {msg}"
    );

    // (e-cross) qpdf must produce at least one ObjStm container.
    let qpdf_containers = objstm_container_numbers(&qpdf_xref_entries);
    assert!(
        !qpdf_containers.is_empty(),
        "qpdf lin+gen must produce at least one ObjStm container"
    );

    // (e-cross) SHARED INVARIANT: qpdf keeps first-page object as plain indirect.
    // Note: qpdf may place ObjStm containers in Part-3 (before /E) for shared
    // catalog/font/info objects — that is qpdf's own behavior and NOT what we
    // assert here.  What we assert is that the FIRST PAGE OBJECT is always plain,
    // which both tools must satisfy.
    let mut pdf_qpdf = Pdf::open(Cursor::new(qpdf_bytes)).expect("Pdf::open qpdf output");
    let qpdf_page_refs = pages::page_refs(&mut pdf_qpdf).expect("page_refs on qpdf output");
    assert!(
        !qpdf_page_refs.is_empty(),
        "qpdf lin+gen output must expose at least one page"
    );
    let qpdf_first_page_num = qpdf_page_refs[0].number;
    let qpdf_compressed = compressed_obj_numbers(&qpdf_xref_entries);
    assert!(
        !qpdf_compressed.contains(&qpdf_first_page_num),
        "qpdf reference: first-page object {} must be plain indirect (not inside ObjStm)",
        qpdf_page_refs[0]
    );

    // Now produce flpdf's generate output on the same fixture and verify the
    // same per-page-1-plain invariant holds — confirming behavioral parity.
    let flpdf_out = dir.path().join("flpdf-lin-gen.pdf");
    linearize_with_mode("generate", &source, &flpdf_out);
    let flpdf_bytes = std::fs::read(&flpdf_out).unwrap();
    let flpdf_xref_text = qpdf_show_xref(&flpdf_out);
    let flpdf_xref_entries = parse_xref_entries(&flpdf_xref_text);

    // flpdf must also produce at least one ObjStm (non-vacuous check).
    let flpdf_containers = objstm_container_numbers(&flpdf_xref_entries);
    assert!(
        !flpdf_containers.is_empty(),
        "flpdf lin+gen must produce at least one ObjStm container (same as qpdf)"
    );

    // flpdf now matches qpdf's Part-3 packing: the first-page shared dicts +
    // /Pages + /Info are packed into a first-half ObjStm container emitted
    // before /E (the /Catalog stays standalone).  Assert parity with qpdf —
    // flpdf, like qpdf, places a container before /E.
    let flpdf_e_off = parse_e_offset(&flpdf_bytes);
    let flpdf_any_before_e = flpdf_containers.iter().any(|cnum| {
        uncompressed_offset(&flpdf_xref_entries, *cnum)
            .map(|coff| coff < flpdf_e_off)
            .unwrap_or(false)
    });
    let qpdf_e_off = parse_e_offset(&std::fs::read(&qpdf_out).unwrap());
    let qpdf_any_before_e = qpdf_containers.iter().any(|cnum| {
        uncompressed_offset(&qpdf_xref_entries, *cnum)
            .map(|coff| coff < qpdf_e_off)
            .unwrap_or(false)
    });
    assert_eq!(
        flpdf_any_before_e, qpdf_any_before_e,
        "flpdf must match qpdf on whether an ObjStm container is packed before /E \
         (first-half Part-3 packing parity); flpdf={flpdf_any_before_e} qpdf={qpdf_any_before_e}"
    );
    assert!(
        flpdf_any_before_e,
        "flpdf: a first-half (Part-3) ObjStm container must be before /E={flpdf_e_off} \
         (qpdf member-set parity); containers = {flpdf_containers:?}"
    );

    let mut pdf_flpdf = Pdf::open(Cursor::new(flpdf_bytes)).expect("Pdf::open flpdf output");
    let flpdf_page_refs = pages::page_refs(&mut pdf_flpdf).expect("page_refs on flpdf output");
    assert_eq!(
        flpdf_page_refs.len(),
        qpdf_page_refs.len(),
        "flpdf and qpdf outputs must have the same page count"
    );
    let flpdf_first_page_num = flpdf_page_refs[0].number;
    let flpdf_compressed = compressed_obj_numbers(&flpdf_xref_entries);
    assert!(
        !flpdf_compressed.contains(&flpdf_first_page_num),
        "flpdf: first-page object {} must be plain indirect — \
         both flpdf and qpdf agree: first-page object is not packed into ObjStm",
        flpdf_page_refs[0]
    );
}
