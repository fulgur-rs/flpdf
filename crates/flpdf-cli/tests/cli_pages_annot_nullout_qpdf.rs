//! Link-annotation / `/OpenAction` null-out parity vs qpdf 11.9.0.
//!
//! Truth source: `/usr/bin/qpdf` 11.9.0. When `--pages` drops a page that is
//! reached ONLY from a surviving page's link annotation (`/Dest` or
//! `/A /GoTo /D`) or from the catalog `/OpenAction /GoTo /D`, qpdf replaces the
//! removed page object with a `null` object and keeps the destination reference
//! verbatim (the reference now points at that null object). This test runs the
//! SAME fixtures and flags through both `qpdf` and the `flpdf` binary, then
//! asserts the observable parity:
//!   1. both outputs have exactly 2 pages (the removed page is gone from the
//!      page tree),
//!   2. the destination reference still exists (NOT dropped), and
//!   3. the object it points at has a body of `null` — in BOTH tools' outputs.
//!
//! Both outputs are normalized with `qpdf --qdf --object-streams=disable
//! --no-original-object-ids` first, so the assertions read a stable textual
//! form. Object numbering differs between tools, so the reference target is
//! resolved by parsing (never hardcoded) and only the null-vs-not / page-count
//! / reference-present facts are asserted — not byte identity.

use assert_cmd::Command;
use std::path::Path;
use std::process::Command as Shell;

/// `qpdf` binary path (the project's pinned truth source).
const QPDF: &str = "/usr/bin/qpdf";

/// The qpdf release this parity test's expected behaviour was derived from.
/// A patched/suffixed build may behave differently, so non-matching builds
/// skip rather than validate against a different truth source.
const EXPECTED_QPDF_VERSION: &str = "11.9.0";

fn qpdf_available() -> bool {
    if !Path::new(QPDF).exists() {
        return false;
    }
    // `qpdf --version` first stdout line is exactly "qpdf version <v>".
    match Shell::new(QPDF).arg("--version").output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines().next().map(str::trim)
                == Some(&format!("qpdf version {EXPECTED_QPDF_VERSION}"))
        }
        Err(_) => false,
    }
}

/// Run qpdf with `args`; panic if it cannot spawn. Returns the process output
/// so callers can surface the exit code and stderr in failure diagnostics.
fn run_qpdf(args: &[&str]) -> std::process::Output {
    Shell::new(QPDF)
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

/// Normalize a PDF with qpdf's QDF mode into a stable, parseable text form and
/// return its contents. Disables object streams and original object ids so the
/// dictionaries and `N 0 obj` bodies appear as plain text.
fn normalize_qdf(input: &Path, output: &Path) -> String {
    let out = run_qpdf(&[
        "--qdf",
        "--object-streams=disable",
        "--no-original-object-ids",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "qpdf --qdf normalization should succeed for {input:?} (exit {:?}): {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    // QDF files start with a binary marker comment (`%`+high bytes), so the
    // file is not valid UTF-8 as a whole. We only ever parse ASCII lines
    // (`N 0 obj`, `/Dest [`, `null`, `%% Page`), so a lossy decode is safe.
    let bytes = std::fs::read(output).expect("qdf output should be readable");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Count the `%% Page` markers qpdf's QDF writer emits, one per page in the
/// page tree. This is the observable page count of the normalized output.
fn qdf_page_count(qdf: &str) -> usize {
    qdf.lines()
        .filter(|l| l.trim_start().starts_with("%% Page"))
        .count()
}

/// Given normalized QDF text and a destination key (`/Dest`, `/D`), find the
/// `key [` line, read the first reference `N 0 R` from the following line, then
/// assert object `N 0 obj` has a body of exactly `null`. Panics with a clear
/// message if the key is absent (which also proves the reference was NOT
/// dropped) or if the target body is not `null`.
fn assert_dest_points_at_null(qdf: &str, key: &str, tool: &str) {
    let lines: Vec<&str> = qdf.lines().collect();
    // Find the destination array opener: `<key> [` (bracket kept so `/Dest`
    // is not confused with `/D`, and `/D` is matched exactly).
    let opener = format!("{key} [");
    let key_idx = lines
        .iter()
        .position(|l| l.trim() == opener)
        .unwrap_or_else(|| {
            panic!("{tool}: destination key `{key} [` not found (reference must be preserved)")
        });
    // The first array element line, e.g. `    6 0 R`.
    let ref_line = lines
        .get(key_idx + 1)
        .unwrap_or_else(|| panic!("{tool}: missing array element after `{key} [`"))
        .trim();
    let obj_num: u32 = ref_line
        .strip_suffix(" 0 R")
        .unwrap_or_else(|| panic!("{tool}: expected `N 0 R` after `{key} [`, got `{ref_line}`"))
        .parse()
        .unwrap_or_else(|_| panic!("{tool}: could not parse object number from `{ref_line}`"));
    // Find `N 0 obj` and assert its body is exactly `null`.
    let obj_header = format!("{obj_num} 0 obj");
    let obj_idx = lines
        .iter()
        .position(|l| l.trim() == obj_header)
        .unwrap_or_else(|| panic!("{tool}: target object `{obj_header}` not found"));
    // First non-empty line after the header is the object body.
    let body = lines[obj_idx + 1..]
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or_else(|| panic!("{tool}: target object `{obj_header}` has no body"));
    assert_eq!(
        body, "null",
        "{tool}: removed page (obj {obj_num}) reached via {key} should be null, got `{body}`"
    );
}

/// Build a structurally valid 3-page PDF with distinct MediaBox widths
/// (page1=100, page2=200, page3=300). Page 2 (width 200) is the destination
/// target reached only via the given mechanism; pages 1 and 3 survive
/// `--pages . 1,3`.
///
/// `variant` selects how page 2 is referenced:
///   - `"dest"`   : page1 `/Annots [annot]`, annot `/Dest [page2 /Fit]`
///   - `"action"` : page1 `/Annots [annot]`, annot `/A /GoTo /D [page2 /Fit]`
///   - `"openaction"` : catalog `/OpenAction /GoTo /D [page2 /Fit]`, no annot
///   - `"inline"` : page1 `/Annots [ << ...inline link... /Dest [page2 /Fit] >> ]`
///     (a direct-dict annotation, no indirect annot object)
fn nullout_fixture(variant: &str) -> tempfile::NamedTempFile {
    use std::io::Write;
    // Object layout (page2 is always obj 4 = the removed dest target):
    //   1 catalog, 2 pages, 3 page1, 4 page2, 5 page3, [6 annot for dest/action]
    let mut buf: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let mut add = |buf: &mut Vec<u8>, obj: &str| {
        offsets.push(buf.len());
        buf.extend_from_slice(obj.as_bytes());
    };

    let catalog = if variant == "openaction" {
        "1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
         /OpenAction << /Type /Action /S /GoTo /D [4 0 R /Fit] >> >>\nendobj\n"
            .to_string()
    } else {
        "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string()
    };
    add(&mut buf, &catalog);
    add(
        &mut buf,
        "2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>\nendobj\n",
    );
    let page1 = match variant {
        "openaction" => "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] \
             /Resources << >> >>\nendobj\n"
            .to_string(),
        // Inline (direct-dict) annotation embedded directly in /Annots.
        "inline" => "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] \
             /Resources << >> /Annots [ << /Type /Annot /Subtype /Link \
             /Rect [0 0 50 50] /Dest [4 0 R /Fit] >> ] >>\nendobj\n"
            .to_string(),
        _ => "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] \
             /Resources << >> /Annots [6 0 R] >>\nendobj\n"
            .to_string(),
    };
    add(&mut buf, &page1);
    add(
        &mut buf,
        "4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> >>\nendobj\n",
    );
    add(
        &mut buf,
        "5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << >> >>\nendobj\n",
    );
    match variant {
        "dest" => add(
            &mut buf,
            "6 0 obj\n<< /Type /Annot /Subtype /Link /Rect [0 0 50 50] \
             /Dest [4 0 R /Fit] >>\nendobj\n",
        ),
        "action" => add(
            &mut buf,
            "6 0 obj\n<< /Type /Annot /Subtype /Link /Rect [0 0 50 50] \
             /A << /Type /Action /S /GoTo /D [4 0 R /Fit] >> >>\nendobj\n",
        ),
        "openaction" | "inline" => {}
        other => panic!("unknown fixture variant: {other}"),
    }

    let xref_pos = buf.len();
    let total = offsets.len() + 1; // objects + the free object 0
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

/// Run the parity scenario for one fixture variant + destination key.
/// `--pages . 1,3` drops page 2; both tools must null-out the page-2 object
/// while preserving the `key` reference that pointed at it.
fn assert_nullout_parity(variant: &str, key: &str) {
    if !qpdf_available() {
        return;
    }
    eprintln!(
        "qpdf {EXPECTED_QPDF_VERSION} present; running real null-out assertions ({variant} / {key})"
    );

    let tmp = tempfile::tempdir().unwrap();
    let src_file = nullout_fixture(variant);
    let src = src_file.path();

    let q_out = tmp.path().join("q.pdf");
    let f_out = tmp.path().join("f.pdf");

    // qpdf is the oracle; if it rejects our fixture the offset bookkeeping is
    // wrong, so fail loudly rather than skip.
    let q_run = run_qpdf(&[
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1,3",
        "--",
        q_out.to_str().unwrap(),
    ]);
    assert!(
        q_run.status.success(),
        "qpdf --pages should succeed on the {variant} fixture (exit {:?}): {}",
        q_run.status.code(),
        String::from_utf8_lossy(&q_run.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            src.to_str().unwrap(),
            "--pages",
            ".",
            "1,3",
            "--",
            f_out.to_str().unwrap(),
        ])
        .assert()
        .success();

    let q_qdf_path = tmp.path().join("q.qdf");
    let f_qdf_path = tmp.path().join("f.qdf");
    let q_qdf = normalize_qdf(&q_out, &q_qdf_path);
    let f_qdf = normalize_qdf(&f_out, &f_qdf_path);

    // 1. Both outputs have exactly 2 pages (page 2 is gone from the page tree).
    assert_eq!(
        qdf_page_count(&q_qdf),
        2,
        "qpdf: {variant} fixture should drop to 2 pages"
    );
    assert_eq!(
        qdf_page_count(&f_qdf),
        2,
        "flpdf: {variant} fixture should drop to 2 pages, matching qpdf"
    );

    // 2 + 3. The destination reference is preserved AND points at a null object,
    // in qpdf's output (the oracle) and in flpdf's output (the tool under test).
    assert_dest_points_at_null(&q_qdf, key, "qpdf");
    assert_dest_points_at_null(&f_qdf, key, "flpdf");
}

#[test]
fn link_annot_dest_removed_page_becomes_null_like_qpdf() {
    // page1 link annot `/Dest [page2 /Fit]`; deleting page2 → page2 obj is
    // null, `/Dest` reference preserved. (qpdf 11.9.0 verified.)
    assert_nullout_parity("dest", "/Dest");
}

#[test]
fn link_annot_goto_action_removed_page_becomes_null_like_qpdf() {
    // page1 link annot `/A /GoTo /D [page2 /Fit]`; deleting page2 → page2 obj
    // is null, `/D` reference preserved. (qpdf 11.9.0 verified.)
    assert_nullout_parity("action", "/D");
}

#[test]
fn open_action_goto_removed_page_becomes_null_like_qpdf() {
    // catalog `/OpenAction /GoTo /D [page2 /Fit]`; deleting page2 → page2 obj
    // is null, `/D` reference preserved. (qpdf 11.9.0 verified.)
    assert_nullout_parity("openaction", "/D");
}

#[test]
fn inline_link_annot_dest_removed_page_becomes_null_like_qpdf() {
    // page1 INLINE (direct-dict) link annot `/Dest [page2 /Fit]`; deleting page2
    // → page2 obj is null, `/Dest` reference preserved. (qpdf 11.9.0 verified.)
    assert_nullout_parity("inline", "/Dest");
}
