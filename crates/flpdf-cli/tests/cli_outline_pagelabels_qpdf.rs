//! Outline / named-destination / `/PageLabels` parity vs qpdf 11.9.0 for
//! `--pages`, plus a current-behaviour lock for `flpdf --json`'s `outlines`
//! and `pagelabels` sections.
//!
//! Truth source: `/usr/bin/qpdf` 11.9.0, same convention as
//! `cli_pages_pagelabels_qpdf.rs` (which this file complements — that one
//! covers `/PageLabels` alone; this one adds the outline `/Dest` and modern
//! `/Names /Dests` destinations that survive a `--pages` subset selection).
//!
//! The comparison is done through flpdf's own reader API (page index of the
//! resolved destination target), not a raw byte or JSON diff: `qpdf --pages`
//! and `flpdf --pages` are free to renumber objects differently while still
//! being semantically correct, so comparing "which page (by position) does
//! this destination point at" is the right level of abstraction — exactly
//! the pattern `cli_pages_pagelabels_qpdf.rs` already uses for `/PageLabels`.

use assert_cmd::Command;
use flpdf::{pages, Object, ObjectRef, Pdf};
use std::path::Path;
use std::process::Command as Shell;

/// The qpdf release this parity test's expected behaviour was derived from.
const EXPECTED_QPDF_VERSION: &str = "11.9.0";

/// Resolves the qpdf binary: `QPDF` env var if set, else plain `qpdf` so it
/// picks up `$PATH` (macOS Homebrew, non-Debian layouts, custom builds).
fn qpdf_command() -> String {
    std::env::var("QPDF").unwrap_or_else(|_| "qpdf".to_string())
}

fn qpdf_available() -> bool {
    match Shell::new(qpdf_command()).arg("--version").output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines().next().map(str::trim)
                == Some(&format!("qpdf version {EXPECTED_QPDF_VERSION}"))
        }
        Err(_) => false,
    }
}

/// Four-page document with:
///  - an outline item whose `/Dest` is an explicit array targeting page 1
///    (0-based index 0);
///  - a modern `/Names /Dests` entry `"target"` targeting page 3 (0-based
///    index 2);
///  - roman-lowercase `/PageLabels` for pages 0-1, decimal (restart at 1)
///    for pages 2-3 (same shape `cli_pages_pagelabels_qpdf.rs` uses).
///
/// Both destinations target pages that SURVIVE the `1,3` (1-based) selection
/// used below, so this exercises the "kept" path, not the null-out path
/// (already covered extensively by the raw destination assertions in
/// `page_merge_tests.rs`).
fn outline_and_dests_four_page_pdf() -> Vec<u8> {
    let objects: &[(u32, &str)] = &[
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names 8 0 R /PageLabels \
             << /Nums [0 << /S /r >> 2 << /S /D /St 1 >>] >> >>",
        ),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R 7 0 R 5 0 R 6 0 R] /Count 4 >>",
        ),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (7, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (
            4,
            "<< /Type /Outlines /First 40 0 R /Last 40 0 R /Count 1 >>",
        ),
        (40, "<< /Title (Go) /Parent 4 0 R /Dest [3 0 R /Fit] >>"),
        (8, "<< /Dests 9 0 R >>"),
        (9, "<< /Names [(target) [5 0 R /Fit]] >>"),
    ];
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in objects {
        offsets.insert(*n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    let size = objects.iter().map(|(n, _)| *n).max().unwrap() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offsets.get(&n) {
            Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// Returns the 0-based index of `target` within `pdf`'s page order. Panics
/// if `target` is not among the document's pages (a test-harness bug the
/// callers deliberately do not tolerate silently).
fn page_index_of(pdf: &mut Pdf<std::io::BufReader<std::fs::File>>, target: ObjectRef) -> usize {
    let refs = pages::page_refs(pdf).unwrap();
    refs.iter()
        .position(|&r| r == target)
        .expect("dest target must be one of the output's own pages")
}

/// 0-based index, within `pdf`'s own page order, of the page an outline
/// item's explicit `/Dest` array targets. Panics if there is no outline
/// item, or its `/Dest` does not resolve to a page in this document — a
/// test-harness bug, not something the assertions below should tolerate
/// silently.
fn outline_dest_page_index(pdf: &mut Pdf<std::io::BufReader<std::fs::File>>) -> usize {
    let roots = pdf.outline().get_root().unwrap();
    let target = roots[0]
        .dest_page()
        .as_ref_id()
        .expect("dest must resolve to a page ref");
    page_index_of(pdf, target)
}

fn terminal_object(pdf: &mut Pdf<std::io::BufReader<std::fs::File>>, mut value: Object) -> Object {
    for _ in 0..64 {
        match value {
            Object::Reference(reference) => value = pdf.resolve(reference).unwrap(),
            other => return other,
        }
    }
    panic!("test fixture contains an excessively deep reference chain");
}

/// 0-based index, within `pdf`'s own page order, of the page the raw modern
/// `/Names /Dests` entry `"target"` points at. This fixture has a single leaf,
/// so the test reads that leaf directly without a normalized destination API.
fn named_dest_page_index(pdf: &mut Pdf<std::io::BufReader<std::fs::File>>) -> usize {
    let catalog_ref = pdf.root_ref().expect("catalog ref");
    let Object::Dictionary(catalog) = pdf.resolve(catalog_ref).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    let Object::Dictionary(names) =
        terminal_object(pdf, catalog.get("Names").cloned().expect("catalog /Names"))
    else {
        panic!("catalog /Names must resolve to a dictionary");
    };
    let Object::Dictionary(dests) = terminal_object(
        pdf,
        names.get("Dests").cloned().expect("catalog /Names /Dests"),
    ) else {
        panic!("catalog /Names /Dests must resolve to a dictionary");
    };
    let Object::Array(entries) = dests.get("Names").expect("destination leaf /Names") else {
        panic!("destination leaf /Names must be an array");
    };
    let target_index = entries
        .chunks_exact(2)
        .position(|pair| pair[0] == Object::String(b"target".to_vec()))
        .expect("\"target\" entry must survive");
    let Object::Array(dest) = terminal_object(pdf, entries[target_index * 2 + 1].clone()) else {
        panic!("target destination must remain a raw array");
    };
    let target = dest[0].as_ref_id().expect("destination page reference");
    page_index_of(pdf, target)
}

fn open(path: &Path) -> Pdf<std::io::BufReader<std::fs::File>> {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    Pdf::open(std::io::BufReader::new(file)).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

#[test]
fn cli_pages_subset_outline_and_named_dest_page_positions_match_qpdf() {
    if !qpdf_available() {
        eprintln!(
            "[SKIP cli_outline_pagelabels_qpdf] qpdf {EXPECTED_QPDF_VERSION} not on PATH — set QPDF env or install to run"
        );
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("in.pdf");
    std::fs::write(&src, outline_and_dests_four_page_pdf()).unwrap();

    // Select 1-based pages 1 and 3 (0-based 0, 2) — both destination targets
    // survive, landing at output indices 0 and 1 respectively.
    let qpdf_out = tmp.path().join("qpdf_out.pdf");
    let status = Shell::new(qpdf_command())
        .args([
            src.to_str().unwrap(),
            "--pages",
            ".",
            "1,3",
            "--",
            qpdf_out.to_str().unwrap(),
        ])
        .status()
        .expect("qpdf should spawn");
    assert!(status.success(), "qpdf --pages should succeed");

    let flpdf_out = tmp.path().join("flpdf_out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&src)
        .args(["--pages", ".", "1,3", "--"])
        .arg(&flpdf_out)
        .assert()
        .success();

    let mut qpdf_doc = open(&qpdf_out);
    let mut flpdf_doc = open(&flpdf_out);

    assert_eq!(pages::page_refs(&mut qpdf_doc).unwrap().len(), 2);
    assert_eq!(pages::page_refs(&mut flpdf_doc).unwrap().len(), 2);

    assert_eq!(
        outline_dest_page_index(&mut flpdf_doc),
        outline_dest_page_index(&mut qpdf_doc),
        "outline /Dest must resolve to the same OUTPUT page position as qpdf's"
    );
    assert_eq!(
        named_dest_page_index(&mut flpdf_doc),
        named_dest_page_index(&mut qpdf_doc),
        "/Names /Dests entry must resolve to the same OUTPUT page position as qpdf's"
    );
    // Pin the expected positions explicitly, so a future fixture/behaviour
    // change surfaces as a clear diff rather than a silent no-op comparison.
    assert_eq!(outline_dest_page_index(&mut qpdf_doc), 0);
    assert_eq!(named_dest_page_index(&mut qpdf_doc), 1);
}

// ---------------------------------------------------------------------------
// JSON current-behaviour lock (see beads flpdf-q28i for the tracked schema
// divergence vs qpdf — out of scope to fix here; this test only guards
// against a further, silent regression in flpdf's own JSON output).
// ---------------------------------------------------------------------------

#[test]
fn cli_json_outlines_and_pagelabels_sections_are_populated() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("in.pdf");
    std::fs::write(&src, outline_and_dests_four_page_pdf()).unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    // "outlines": one root item, with today's key set (see flpdf-q28i for the
    // tracked divergence from qpdf's actual `dest`/`destpageposfrom1`/`kids`/
    // `object`/`open`/`title` shape — this only locks flpdf's own current
    // output against a further regression).
    let outlines = json.get("outlines").and_then(|v| v.as_array()).unwrap();
    assert_eq!(outlines.len(), 1, "one root outline item");
    let item = &outlines[0];
    assert_eq!(item.get("title").and_then(|v| v.as_str()), Some("Go"));
    assert!(item.get("dest").is_some_and(|v| v.is_array()));
    assert!(item
        .get("kids")
        .and_then(|v| v.as_array())
        .is_some_and(|a| a.is_empty()));

    // "pagelabels": two ranges (roman from 0, decimal from 2), today's
    // {index, label: {first, prefix, style}} shape.
    let pagelabels = json.get("pagelabels").and_then(|v| v.as_array()).unwrap();
    assert_eq!(pagelabels.len(), 2, "two label ranges");
    assert_eq!(pagelabels[0].get("index").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(
        pagelabels[0]
            .get("label")
            .and_then(|l| l.get("style"))
            .and_then(|v| v.as_str()),
        Some("r")
    );
    assert_eq!(pagelabels[1].get("index").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(
        pagelabels[1]
            .get("label")
            .and_then(|l| l.get("style"))
            .and_then(|v| v.as_str()),
        Some("D")
    );
}
