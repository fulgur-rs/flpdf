//! Page-operation observable-behavior matrix vs qpdf 11.9.0 (flpdf-9hc.8.13).
//!
//! Truth source: `/usr/bin/qpdf` 11.9.0. Every cell runs the *same* inputs and
//! flags through both `qpdf` and the `flpdf` binary, then compares the
//! observable result (resulting page count, per-page `/Rotate`, split file
//! names, collate ordering). Equality is asserted for parity cells.
//! Intentional, documented divergences are asserted as EXPECTED (flpdf's value
//! plus a comment explaining why it differs from qpdf). Genuinely unknown
//! divergences discovered while writing this matrix are marked `#[ignore]`
//! with a descriptive reason and reported for a follow-up at the originating
//! layer (this subtask only adds tests; it does not patch lower layers).
//!
//! Matrix axes:
//!   --pages     { single range, multi-input (repeat), :odd, :even,
//!                 reverse z-1, '.' shorthand, password }
//!   --rotate    { +N delta, -N delta, no-sign assign, :range, repeated,
//!                 output-page numbering under --pages }
//!   --split-pages { 1, 2, N>=npages, leading-dot template }
//!   --collate   { default, N>1 }
//!   combinations  { pages+rotate, pages+split, pages+collate }
//!
//! qpdf observation basis is recorded inline per cell (commands were run
//! against qpdf 11.9.0 on the same fixtures during authoring; the tests
//! re-derive qpdf's answer at runtime so they cannot silently rot).

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as Shell;

// NOTE: qpdf's machine-readable inspection is `--json=2`, but this crate has no
// JSON dependency and the test-only subtask must not add one. Instead we read
// qpdf's *output PDF* with flpdf's own `--show-pages` / `--show-npages` — the
// SAME reader is then applied to both tools' outputs, so any divergence is in
// the page-op transform, not in how the observable property is measured. The
// qpdf-produced files are independently structurally valid (asserted via
// `flpdf --check` is implicitly exercised by --show-pages succeeding).

const THREE_PAGE: &str = "../../tests/fixtures/compat/three-page.pdf";
const TWO_PAGE: &str = "../../tests/fixtures/compat/two-page.pdf";

/// Absolute path to a fixture (so a per-cell `cwd` change is unnecessary).
fn fixture_abs(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// `qpdf` binary path (the project's pinned truth source).
const QPDF: &str = "/usr/bin/qpdf";

/// The qpdf release this matrix's expected values were derived from. If the
/// installed qpdf differs, the observable behaviour may differ too, so the
/// affected cells skip rather than silently validate against a different
/// truth source.
const EXPECTED_QPDF_VERSION: &str = "11.9.0";

fn qpdf_available() -> bool {
    if !Path::new(QPDF).exists() {
        return false;
    }
    // `qpdf --version` first stdout line is exactly "qpdf version <v>".
    // Require an *exact* first-line match so a patched/suffixed build
    // (e.g. "11.9.0-ubuntu2") or an unrelated bundled-library version line
    // is not mistaken for the pinned oracle.
    match Shell::new(QPDF).arg("--version").output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines().next().map(str::trim)
                == Some(&format!("qpdf version {EXPECTED_QPDF_VERSION}"))
        }
        Err(_) => false,
    }
}

/// Run qpdf with `args`; return (success, stdout) — stderr is folded into the
/// failure path only.
fn run_qpdf(args: &[&str]) -> (bool, String) {
    let out = Shell::new(QPDF)
        .args(args)
        .output()
        .expect("qpdf should spawn");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Run the flpdf binary with `args`; assert success and return stdout.
fn flpdf_ok(args: &[&str]) -> String {
    let out = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .assert()
        .success();
    String::from_utf8_lossy(&out.get_output().stdout).into_owned()
}

/// Page count read from `path` via flpdf's `--show-npages`. Applied uniformly
/// to qpdf-produced and flpdf-produced files (common reader).
fn npages_of(path: &Path) -> usize {
    let out = flpdf_ok(&["--show-npages", path.to_str().unwrap()]);
    out.trim()
        .lines()
        .next()
        .unwrap()
        .trim()
        .parse()
        .expect("npages integer")
}

/// Per-page `/Rotate` values read from `path` via flpdf's `--show-pages`
/// (`  rotate: <n>` lines), in page order. Common reader for both tools'
/// outputs.
fn rotates_of(path: &Path) -> Vec<i64> {
    let out = flpdf_ok(&["--show-pages", path.to_str().unwrap()]);
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("rotate: "))
        .map(|n| n.trim().parse().expect("rotate integer"))
        .collect()
}

/// Per-page `/MediaBox` values read from `path` via flpdf's `--show-pages`
/// (`  media-box: <arr>` lines), in page order. Used as a stable per-page
/// identity marker so order-sensitive ops (reverse, collate) can assert the
/// *sequence* matches qpdf, not merely the count.
fn media_boxes_of(path: &Path) -> Vec<String> {
    let out = flpdf_ok(&["--show-pages", path.to_str().unwrap()]);
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("media-box: "))
        .map(|s| s.trim().to_string())
        .collect()
}

/// Write a structurally valid `n`-page PDF whose pages have *distinct*
/// MediaBox widths (page `i` → `[0 0 (i*100) 200]`). The width uniquely
/// identifies each source page, so a reordering op's output page sequence
/// can be compared element-by-element against qpdf's.
fn distinct_pages_pdf(n: usize) -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut buf: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let kids: String = (0..n)
        .map(|i| format!("{} 0 R", 3 + i))
        .collect::<Vec<_>>()
        .join(" ");

    offsets.push(buf.len());
    buf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(buf.len());
    buf.extend_from_slice(
        format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {n} >>\nendobj\n").as_bytes(),
    );
    for i in 0..n {
        offsets.push(buf.len());
        let w = (i + 1) * 100;
        // `/Resources` is a required (inheritable) Page attribute; qpdf 12.x
        // warns ("Resources is missing or invalid") and bumps `qpdf --check`
        // to exit 3 without it, where qpdf 11.x stayed silent.
        buf.extend_from_slice(
            format!(
                "{} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} 200] \
                 /Resources << >> >>\nendobj\n",
                3 + i
            )
            .as_bytes(),
        );
    }
    let xref_pos = buf.len();
    let total = n + 3; // objs 0..=(n+2)
    buf.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
    for &off in &offsets {
        buf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    buf.extend_from_slice(
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n")
            .as_bytes(),
    );
    let mut f = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    f.write_all(&buf).unwrap();
    f.flush().unwrap();
    f
}

/// Sorted list of split output basenames matching `<stem>-*.pdf` in `dir`.
fn split_outputs(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".pdf") && n.contains('-'))
        .collect();
    names.sort();
    names
}

// ===========================================================================
// --pages : page-selection parity
// ===========================================================================

#[test]
fn pages_single_range_matches_qpdf_count() {
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "2-3",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "2-3",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 2, "qpdf observed: 3p --pages . 2-3 -> 2");
    assert_eq!(npages_of(&f), npages_of(&q), "flpdf must match qpdf");
}

#[test]
fn pages_odd_parity_is_position_based_like_qpdf() {
    // Documented divergence #1 (EXPECTED, but qpdf-CORRECT): `:odd` selects by
    // POSITION within the resulting set, not by page number. qpdf 11.9.0:
    // `3p --pages . 1-3:odd -> 2 pages` (positions 1,3 of [1,2,3] => pages
    // 1,3). flpdf matches qpdf here, so this is asserted as PARITY.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3:odd",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3:odd",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 2);
    assert_eq!(npages_of(&f), npages_of(&q));
}

#[test]
fn pages_even_parity_is_position_based_like_qpdf() {
    // qpdf 11.9.0: `3p --pages . 1-3:even -> 1 page` (position 2 of [1,2,3]).
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3:even",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3:even",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 1);
    assert_eq!(npages_of(&f), npages_of(&q));
}

#[test]
fn pages_reverse_range_matches_qpdf() {
    // `z-1` = last..first. qpdf 11.9.0: 3p -> 3 pages (reversed order).
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Distinct MediaBox widths (100,200,300) make page identity observable,
    // so we assert the *reversed order*, not just the count.
    let src_file = distinct_pages_pdf(3);
    let src = src_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "z-1",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "z-1",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 3);
    assert_eq!(npages_of(&f), npages_of(&q));
    // z-1 = last..first → widths reversed.
    let q_boxes = media_boxes_of(&q);
    assert_eq!(
        q_boxes,
        vec![
            "[ 0 0 300 200 ]".to_string(),
            "[ 0 0 200 200 ]".to_string(),
            "[ 0 0 100 200 ]".to_string(),
        ],
        "qpdf z-1 should reverse page order"
    );
    assert_eq!(
        media_boxes_of(&f),
        q_boxes,
        "flpdf z-1 page order must match qpdf"
    );
}

#[test]
fn pages_dot_shorthand_single_page_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 1);
    assert_eq!(npages_of(&f), npages_of(&q));
}

#[test]
fn pages_multi_input_same_file_repeated_matches_qpdf() {
    // `. 1 . 3` repeats the primary input → single-document case in flpdf,
    // 2 pages out. qpdf 11.9.0 produces the same count.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Distinct widths so `. 1 . 3` order (page1 then page3) is asserted.
    let src_file = distinct_pages_pdf(3);
    let src = src_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        ".",
        "3",
        "--",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        ".",
        "3",
        "--",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 2);
    assert_eq!(npages_of(&f), npages_of(&q));
    // Selected pages 1 then 3 → widths 100 then 300, in that order.
    let q_boxes = media_boxes_of(&q);
    assert_eq!(
        q_boxes,
        vec!["[ 0 0 100 200 ]".to_string(), "[ 0 0 300 200 ]".to_string()],
        "qpdf `. 1 . 3` should yield pages 1,3 in order"
    );
    assert_eq!(
        media_boxes_of(&f),
        q_boxes,
        "flpdf repeated-same-file selection order must match qpdf"
    );
}

#[test]
fn pages_cross_document_merge_is_expected_scope_error() {
    // EXPECTED divergence (documented #4 / single-document scope): qpdf
    // happily merges pages from two distinct files; flpdf intentionally
    // refuses cross-document merges with an actionable error (cross-doc merge
    // + AcroForm collision handling tracked separately). Assert flpdf's
    // boundary behavior; qpdf-divergent by design.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let three = fixture_abs(THREE_PAGE);
    let two = fixture_abs(TWO_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    // qpdf accepts the cross-document merge (3 pages: p1 of three + 2 of two).
    let (ok, _) = run_qpdf(&[
        three.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        two.to_str().unwrap(),
        "1-2",
        "--",
        q.to_str().unwrap(),
    ]);
    assert!(ok, "qpdf is expected to accept cross-document merge");

    // flpdf refuses, actionably (exit != 0, message names the boundary).
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            three.to_str().unwrap(),
            "--pages",
            ".",
            "1",
            two.to_str().unwrap(),
            "1-2",
            "--",
            f.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cross-document"))
        .stderr(predicates::str::contains("not supported"));
}

// ===========================================================================
// --rotate : rotation parity + documented sign-semantics divergence
// ===========================================================================

#[test]
fn rotate_plus_delta_matches_qpdf() {
    // `+90` is a relative (delta) rotation in both tools. From /Rotate 0 the
    // result is 90 for every page. qpdf 11.9.0 verified.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[src.to_str().unwrap(), "--rotate=+90", q.to_str().unwrap()]);
    flpdf_ok(&[src.to_str().unwrap(), f.to_str().unwrap(), "--rotate=+90"]);

    assert_eq!(rotates_of(&q), vec![90, 90, 90]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
}

#[test]
fn rotate_minus_delta_matches_qpdf() {
    // `-90` relative → 270 from a base of 0. qpdf 11.9.0 verified.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[src.to_str().unwrap(), "--rotate=-90", q.to_str().unwrap()]);
    flpdf_ok(&[src.to_str().unwrap(), f.to_str().unwrap(), "--rotate=-90"]);

    assert_eq!(rotates_of(&q), vec![270, 270, 270]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
}

#[test]
fn rotate_plus_delta_accumulates_on_nonzero_base_like_qpdf() {
    // Two-step: base /Rotate 90 (via +90), then +90 again → 180. The relative
    // (+) form composes with the existing /Rotate identically in both tools.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let base = tmp.path().join("base90.pdf");
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    // Build the rotated base with qpdf (truth source) so both tools start
    // from byte-identical input.
    run_qpdf(&[
        src.to_str().unwrap(),
        "--rotate=+90",
        base.to_str().unwrap(),
    ]);
    assert_eq!(rotates_of(&base), vec![90, 90, 90]);

    run_qpdf(&[base.to_str().unwrap(), "--rotate=+90", q.to_str().unwrap()]);
    flpdf_ok(&[base.to_str().unwrap(), f.to_str().unwrap(), "--rotate=+90"]);

    assert_eq!(rotates_of(&q), vec![180, 180, 180]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
}

#[test]
fn rotate_no_sign_is_delta_in_flpdf_expected_divergence() {
    // EXPECTED divergence (intentional, layer-documented in
    // crates/flpdf/src/rotate_spec.rs lines ~14-19): qpdf's no-sign `angle`
    // is an ABSOLUTE assignment ("--help=--rotate": "you almost always want
    // +angle or -angle rather than just angle"). flpdf-9hc.8.5 deliberately
    // treats no-sign `angle` as additive (RotateMode::Add) and reserves
    // RotateMode::Assign for a future issue. So on a base of /Rotate 90:
    //   qpdf  `--rotate=90` → 90  (assign)
    //   flpdf `--rotate=90` → 180 (delta: 90 + 90)
    // This is asserted as the EXPECTED, documented difference (NOT a bug, NOT
    // ignored): flpdf == delta, qpdf == assign.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let base = tmp.path().join("base90.pdf");
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--rotate=+90",
        base.to_str().unwrap(),
    ]);
    assert_eq!(rotates_of(&base), vec![90, 90, 90]);

    run_qpdf(&[base.to_str().unwrap(), "--rotate=90", q.to_str().unwrap()]);
    flpdf_ok(&[base.to_str().unwrap(), f.to_str().unwrap(), "--rotate=90"]);

    // qpdf: no-sign = assign → stays 90.
    assert_eq!(
        rotates_of(&q),
        vec![90, 90, 90],
        "qpdf no-sign rotate is absolute assignment"
    );
    // flpdf: no-sign = additive (8.5 intentional) → 90 + 90 = 180.
    assert_eq!(
        rotates_of(&f),
        vec![180, 180, 180],
        "flpdf 8.5 treats no-sign rotate as delta; Assign mode deferred (EXPECTED divergence)"
    );
}

#[test]
fn rotate_with_range_matches_qpdf() {
    // `+90:2` rotates only page 2. qpdf 11.9.0: [0, 90, 0].
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[src.to_str().unwrap(), "--rotate=+90:2", q.to_str().unwrap()]);
    flpdf_ok(&[src.to_str().unwrap(), f.to_str().unwrap(), "--rotate=+90:2"]);

    assert_eq!(rotates_of(&q), vec![0, 90, 0]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
}

#[test]
fn rotate_repeated_specs_apply_in_order_like_qpdf() {
    // `--rotate=+90:1 --rotate=180:3` → page1=90, page2=0, page3=180.
    // qpdf 11.9.0 verified.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--rotate=+90:1",
        "--rotate=180:3",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        f.to_str().unwrap(),
        "--rotate=+90:1",
        "--rotate=180:3",
    ]);

    // page3 uses no-sign 180 on a base of 0: assign(180) == delta(+180) here,
    // so this cell is parity (the sign-semantics divergence is invisible at
    // base 0; it is isolated in rotate_no_sign_is_delta_*).
    assert_eq!(rotates_of(&q), vec![90, 0, 180]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
}

// ===========================================================================
// --split-pages : chunking + filename parity / divergence
// ===========================================================================

#[test]
fn split_pages_one_filename_matches_qpdf() {
    // qpdf 11.9.0: `3p --split-pages=1` → q-1.pdf, q-2.pdf, q-3.pdf.
    // flpdf now matches: chunk_size==1 uses the single-number suffix
    // (page_split::split_output_path). Regression guard for flpdf-s5e.
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);

    // Cross-check the qpdf baseline only where qpdf is present (skipped on
    // Windows CI / hosts without /usr/bin/qpdf); the flpdf assertion below
    // always runs against the qpdf-11.9.0-observed expected names.
    if qpdf_available() {
        run_qpdf(&[
            src.to_str().unwrap(),
            "--split-pages=1",
            qdir.path().join("o.pdf").to_str().unwrap(),
        ]);
        assert_eq!(
            split_outputs(qdir.path()),
            vec!["o-1.pdf", "o-2.pdf", "o-3.pdf"],
            "qpdf observed baseline"
        );
    }
    flpdf_ok(&[
        src.to_str().unwrap(),
        fdir.path().join("o.pdf").to_str().unwrap(),
        "--split-pages=1",
    ]);
    assert_eq!(
        split_outputs(fdir.path()),
        vec!["o-1.pdf", "o-2.pdf", "o-3.pdf"],
        "flpdf split-pages=1 naming must match qpdf"
    );
}

#[test]
fn split_pages_two_filenames_match_qpdf() {
    // qpdf 11.9.0: `3p --split-pages=2` → o-1-2.pdf, o-3-3.pdf (range form,
    // trailing single-page chunk still keeps lo-hi). flpdf matches exactly.
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);

    if qpdf_available() {
        run_qpdf(&[
            src.to_str().unwrap(),
            "--split-pages=2",
            qdir.path().join("o.pdf").to_str().unwrap(),
        ]);
        assert_eq!(
            split_outputs(qdir.path()),
            vec!["o-1-2.pdf", "o-3-3.pdf"],
            "qpdf observed baseline"
        );
    }
    flpdf_ok(&[
        src.to_str().unwrap(),
        fdir.path().join("o.pdf").to_str().unwrap(),
        "--split-pages=2",
    ]);
    assert_eq!(split_outputs(fdir.path()), vec!["o-1-2.pdf", "o-3-3.pdf"]);
}

#[test]
fn split_pages_n_ge_npages_single_file_matches_qpdf() {
    // qpdf 11.9.0: `3p --split-pages=5` → one file o-1-3.pdf. flpdf matches.
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);

    if qpdf_available() {
        run_qpdf(&[
            src.to_str().unwrap(),
            "--split-pages=5",
            qdir.path().join("o.pdf").to_str().unwrap(),
        ]);
        assert_eq!(split_outputs(qdir.path()), vec!["o-1-3.pdf"]);
    }
    flpdf_ok(&[
        src.to_str().unwrap(),
        fdir.path().join("o.pdf").to_str().unwrap(),
        "--split-pages=5",
    ]);
    assert_eq!(split_outputs(fdir.path()), vec!["o-1-3.pdf"]);
}

#[test]
fn split_pages_leading_dot_template_matches_qpdf() {
    // Documented divergence #2 (actually PARITY): leading-dot template `.pdf`
    // → empty stem, ".pdf" treated as extension → `-1-2.pdf`, `-3-3.pdf`.
    // qpdf 11.9.0 produces the same; assert exact parity.
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);

    if qpdf_available() {
        run_qpdf(&[
            src.to_str().unwrap(),
            "--split-pages=2",
            qdir.path().join(".pdf").to_str().unwrap(),
        ]);
        assert_eq!(
            split_outputs(qdir.path()),
            vec!["-1-2.pdf", "-3-3.pdf"],
            "qpdf leading-dot baseline"
        );
    }
    flpdf_ok(&[
        src.to_str().unwrap(),
        fdir.path().join(".pdf").to_str().unwrap(),
        "--split-pages=2",
    ]);
    assert_eq!(split_outputs(fdir.path()), vec!["-1-2.pdf", "-3-3.pdf"]);
}

// ===========================================================================
// --collate : interleave parity
// ===========================================================================

#[test]
fn collate_default_matches_qpdf_count() {
    // `--pages . 1-2 . 3 -- --collate` interleaves the two selections. qpdf
    // 11.9.0: 3 pages out. flpdf single-document collate matches the count.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Distinct widths so the *interleave order* is observable, not just count.
    let src_file = distinct_pages_pdf(3);
    let src = src_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-2",
        ".",
        "3",
        "--",
        "--collate",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-2",
        ".",
        "3",
        "--",
        "--collate",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 3);
    assert_eq!(npages_of(&f), npages_of(&q));
    // qpdf is the oracle for the interleave order; flpdf must match it
    // page-for-page (collate ordering is the observable behaviour here).
    let q_boxes = media_boxes_of(&q);
    assert_eq!(q_boxes.len(), 3, "sanity: 3 collated pages");
    assert_eq!(
        media_boxes_of(&f),
        q_boxes,
        "flpdf --collate page order must match qpdf, not just the count"
    );
}

#[test]
fn collate_n_gt_1_matches_qpdf_count() {
    // `--pages . 1-3 -- --collate=2`. qpdf 11.9.0: 3 pages. flpdf matches.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src_file = distinct_pages_pdf(3);
    let src = src_file.path();
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        "--",
        "--collate=2",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        "--",
        "--collate=2",
        f.to_str().unwrap(),
    ]);

    assert_eq!(npages_of(&q), 3);
    assert_eq!(npages_of(&f), npages_of(&q));
    let q_boxes = media_boxes_of(&q);
    assert_eq!(q_boxes.len(), 3);
    assert_eq!(
        media_boxes_of(&f),
        q_boxes,
        "flpdf --collate=2 page order must match qpdf"
    );
}

// ===========================================================================
// Combinations
// ===========================================================================

#[test]
fn pages_then_rotate_uses_output_page_numbering_like_qpdf() {
    // `--pages . 2-3 -- --rotate=+90:1` rotates the FIRST EXTRACTED page
    // (output numbering). qpdf 11.9.0: extracted [src2, src3] → rotates
    // [90, 0]. flpdf matches.
    if !qpdf_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "2-3",
        "--",
        "--rotate=+90:1",
        q.to_str().unwrap(),
    ]);
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "2-3",
        "--",
        "--rotate=+90:1",
        f.to_str().unwrap(),
    ]);

    assert_eq!(rotates_of(&q), vec![90, 0]);
    assert_eq!(rotates_of(&f), rotates_of(&q));
}

#[test]
fn pages_then_split_pages_combined_matches_qpdf() {
    // `--pages . 1-3 -- --split-pages=2`. qpdf 11.9.0: o-1-2.pdf, o-3-3.pdf.
    // flpdf matches (split_pages=2 keeps range form — parity).
    let qdir = tempfile::tempdir().unwrap();
    let fdir = tempfile::tempdir().unwrap();
    let src = fixture_abs(THREE_PAGE);

    if qpdf_available() {
        run_qpdf(&[
            src.to_str().unwrap(),
            "--pages",
            ".",
            "1-3",
            "--",
            "--split-pages=2",
            qdir.path().join("o.pdf").to_str().unwrap(),
        ]);
        assert_eq!(split_outputs(qdir.path()), vec!["o-1-2.pdf", "o-3-3.pdf"]);
    }
    flpdf_ok(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1-3",
        "--",
        "--split-pages=2",
        fdir.path().join("o.pdf").to_str().unwrap(),
    ]);
    assert_eq!(split_outputs(fdir.path()), vec!["o-1-2.pdf", "o-3-3.pdf"]);
}

// ===========================================================================
// --pages with passwords (encrypted-source scope boundary)
// ===========================================================================

/// Build an AES-256 encrypted copy of THREE_PAGE (user==owner password) using
/// qpdf, returning its path inside `dir`. Skips (returns None) if qpdf is
/// unavailable.
fn make_encrypted_three_page(dir: &Path, pw: &str) -> Option<PathBuf> {
    if !qpdf_available() {
        return None;
    }
    let src = fixture_abs(THREE_PAGE);
    let enc = dir.join("enc3.pdf");
    let (ok, _) = run_qpdf(&[
        "--encrypt",
        pw,
        pw,
        "256",
        "--",
        src.to_str().unwrap(),
        enc.to_str().unwrap(),
    ]);
    if ok {
        Some(enc)
    } else {
        None
    }
}

#[test]
fn pages_secondary_encrypted_input_is_expected_scope_error() {
    // EXPECTED divergence (documented scope boundary, analogous to #4):
    // qpdf pulls pages from an encrypted secondary (given its password) and
    // writes a DECRYPTED output. flpdf intentionally refuses to emit
    // decrypted output in page-ops mode (reject_encrypted_write in
    // crates/flpdf-cli/src/main.rs ~line 1882): "encrypted PDF output is not
    // supported for this mode; use plain rewrite to produce decrypted
    // plaintext". Assert flpdf's actionable refusal; qpdf-divergent by
    // design.
    let tmp = tempfile::tempdir().unwrap();
    let Some(enc) = make_encrypted_three_page(tmp.path(), "secretpw") else {
        return;
    };
    let q = tmp.path().join("q.pdf");
    let f = tmp.path().join("f.pdf");

    // qpdf: per-input password on the encrypted secondary → succeeds.
    let three = fixture_abs(THREE_PAGE);
    let (ok, _) = run_qpdf(&[
        three.to_str().unwrap(),
        "--pages",
        enc.to_str().unwrap(),
        "--password=secretpw",
        "1-2",
        "--",
        q.to_str().unwrap(),
    ]);
    // qpdf produces a decrypted 2-page output (its documented behavior). We
    // only require that qpdf accepts it; the divergence is flpdf's refusal.
    assert!(ok || q.exists(), "qpdf is expected to accept the merge");

    // flpdf: refuses with the documented actionable error.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            three.to_str().unwrap(),
            "--pages",
            enc.to_str().unwrap(),
            "--password=secretpw",
            "1-2",
            "--",
            f.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "encrypted PDF output is not supported for this mode",
        ));
}

#[test]
#[ignore = "flpdf-1uh: with an encrypted PRIMARY input + top-level --password, `--pages . RANGE --` fails with 'incorrect password' because the top-level password is not forwarded to CombinedPlan::from_specs's planning open (only to the later rebuild open). Layer flpdf-9hc.8.12 (run_page_extraction in crates/flpdf-cli/src/main.rs). Proven not an auth/AES-256 gap: plain `rewrite enc3.pdf --password=secretpw out.pdf` succeeds. Tracked as bug flpdf-1uh (P2, child of epic flpdf-9hc.8); un-ignore once fixed in the 8.12 layer."]
fn pages_primary_encrypted_toplevel_password_threads_through() {
    // Repro: encrypted primary, top-level --password=secretpw, --pages . 2-3.
    // Expected (matching qpdf's auth model): the planning stage should open
    // the primary with the supplied password. flpdf currently does NOT thread
    // the top-level password into CombinedPlan::from_specs, so it reports
    // "encrypted PDF: incorrect password" even though the password is correct.
    // (After 8.12 threads the password, the EXPECTED end-state is the same
    // documented scope refusal as the secondary-input case — i.e. flpdf would
    // then reject with "encrypted PDF output is not supported for this mode".
    // This test is ignored until 8.12 forwards the password; do NOT fix here.)
    let tmp = tempfile::tempdir().unwrap();
    let Some(enc) = make_encrypted_three_page(tmp.path(), "secretpw") else {
        return;
    };
    let f = tmp.path().join("f.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            enc.to_str().unwrap(),
            "--password=secretpw",
            "--pages",
            ".",
            "2-3",
            "--",
            f.to_str().unwrap(),
        ])
        .assert()
        // Once 8.12 threads the top-level password, planning auth succeeds and
        // flpdf reaches the documented encrypted-output scope refusal.
        .failure()
        .stderr(predicates::str::contains(
            "encrypted PDF output is not supported for this mode",
        ));
}
