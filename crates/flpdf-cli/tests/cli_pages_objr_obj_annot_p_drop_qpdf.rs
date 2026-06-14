//! OBJR `/Obj` annotation `/P` drop parity vs qpdf 11.9.0.
//!
//! Truth source: `/usr/bin/qpdf` 11.9.0. When `--pages` removes a page whose
//! only surviving inbound reference (besides the page tree) is an annotation
//! reached through a structure-tree object reference (`/Type /OBJR` `/Obj`),
//! qpdf keeps that annotation alive (the OBJR still points at it) but drops the
//! annotation's now-dangling `/P` back-reference to the removed page. With the
//! `/P` gone, the removed page has no inbound reference left and is
//! garbage-collected: it is absent from the output, NOT emitted as a `null`.
//! An annotation on a surviving page keeps its `/P`, and both OBJRs keep
//! `/Obj`. This isolates the OBJR-`/Obj`-survived annotation `/P`-drop →
//! page-GC behaviour.
//!
//! This test runs the SAME fixture and flags through both `qpdf` and the
//! `flpdf` binary, then asserts the observable parity:
//!   1. both outputs have exactly 2 pages and exactly 2 `/Type /Page` objects
//!      (the removed page is gone entirely — no null placeholder),
//!   2. the annotation that pointed at the removed page has NO `/P`,
//!   3. the annotation that pointed at a surviving page keeps `/P`, and it
//!      resolves to a `/Type /Page` object,
//!   4. both OBJR objects survive and each keeps an `/Obj`.
//!
//! Both outputs are normalized with `qpdf --qdf --object-streams=disable
//! --no-original-object-ids` first, so the assertions read a stable textual
//! form. Object numbering differs between tools, so objects are located by
//! their content (the `(rm)` / `(kp)` text strings, the `/Type /Page` and
//! `/Type /Annot` / `/Type /OBJR` lines), never by hardcoded object numbers.

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
    match Shell::new(QPDF).arg("--version").output() {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines().next().map(str::trim)
                == Some(&format!("qpdf version {EXPECTED_QPDF_VERSION}"))
        }
        Ok(_) => false,
        Err(_) => false,
    }
}

fn run_qpdf(args: &[&str]) -> std::process::Output {
    Shell::new(QPDF)
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

/// Normalize a PDF with qpdf's QDF mode into a stable, parseable text form.
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
    let bytes = std::fs::read(output).expect("qdf output should be readable");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Count the `%% Page` markers qpdf's QDF writer emits, one per page.
fn qdf_page_count(qdf: &str) -> usize {
    qdf.lines()
        .filter(|l| l.trim_start().starts_with("%% Page"))
        .count()
}

/// Split normalized QDF text into `(object number, body)` pairs.
fn qdf_objects(qdf: &str) -> Vec<(u32, String)> {
    let mut objects = Vec::new();
    let mut current: Option<(u32, Vec<&str>)> = None;
    for line in qdf.lines() {
        let trimmed = line.trim();
        if let Some(num) = trimmed
            .strip_suffix(" 0 obj")
            .and_then(|n| n.parse::<u32>().ok())
        {
            current = Some((num, Vec::new()));
        } else if trimmed == "endobj" {
            if let Some((num, body)) = current.take() {
                objects.push((num, body.join("\n")));
            }
        } else if let Some((_, body)) = current.as_mut() {
            body.push(line);
        }
    }
    objects
}

/// Build a 3-page PDF whose page 2 (object 4) is reachable, besides the page
/// tree, only through a structure-tree OBJR `/Obj` pointing at its annotation.
///
/// Object layout: 1 catalog (/StructTreeRoot 10), 2 pages, 3 page1 (/Annots
/// [31]), 4 page2 removed (/Annots [30]), 5 page3, 10 struct tree root (/K 20),
/// 20 document elem (/K [21 22]), 21 OBJR (/Pg 4 /Obj 30) reaching the
/// removed-page annot, 22 OBJR (/Pg 3 /Obj 31) reaching a surviving-page annot,
/// 30 removed-page annot (/NM (rm) /P 4), 31 surviving-page annot (/NM (kp)
/// /P 3).
fn objr_obj_annot_fixture() -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut buf: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let mut offsets: Vec<(u32, usize)> = Vec::new();
    let mut add = |buf: &mut Vec<u8>, num: u32, body: &str| {
        offsets.push((num, buf.len()));
        buf.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
    };

    add(
        &mut buf,
        1,
        "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>",
    );
    add(
        &mut buf,
        2,
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>",
    );
    add(
        &mut buf,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [31 0 R] >>",
    );
    add(
        &mut buf,
        4,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [30 0 R] >>",
    );
    add(
        &mut buf,
        5,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    add(&mut buf, 10, "<< /Type /StructTreeRoot /K 20 0 R >>");
    add(
        &mut buf,
        20,
        "<< /Type /StructElem /S /Document /K [21 0 R 22 0 R] >>",
    );
    add(&mut buf, 21, "<< /Type /OBJR /Pg 4 0 R /Obj 30 0 R >>");
    add(&mut buf, 22, "<< /Type /OBJR /Pg 3 0 R /Obj 31 0 R >>");
    add(
        &mut buf,
        30,
        "<< /Type /Annot /Subtype /Text /NM (rm) /P 4 0 R /Rect [0 0 10 10] >>",
    );
    add(
        &mut buf,
        31,
        "<< /Type /Annot /Subtype /Text /NM (kp) /P 3 0 R /Rect [0 0 10 10] >>",
    );

    let max_num = offsets.iter().map(|&(n, _)| n).max().unwrap();
    let xref_pos = buf.len();
    buf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
    for i in 1..=max_num {
        match offsets.iter().find(|&&(n, _)| n == i) {
            Some(&(_, off)) => {
                buf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            None => buf.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    buf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            max_num + 1
        )
        .as_bytes(),
    );

    let mut f = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    f.write_all(&buf).unwrap();
    f.flush().unwrap();
    f
}

/// Find the single annotation object whose body contains `marker`
/// (a content string such as `(rm)` or `(kp)`).
fn find_annot<'a>(objects: &'a [(u32, String)], marker: &str, tool: &str) -> &'a str {
    let mut found = objects.iter().filter(|(_, body)| {
        body.contains(marker) && body.lines().any(|l| l.trim() == "/Type /Annot")
    });
    let (_, body) = found
        .next()
        .unwrap_or_else(|| panic!("{tool}: no /Type /Annot object containing `{marker}` found"));
    assert!(
        found.next().is_none(),
        "{tool}: more than one /Type /Annot object containing `{marker}`"
    );
    body
}

/// Assert the OBJR-`/Obj` annotation `/P`-drop facts on one tool's normalized
/// output.
fn assert_facts(qdf: &str, tool: &str) {
    let objects = qdf_objects(qdf);

    // 1. Exactly 2 pages and exactly 2 /Type /Page objects: the removed page is
    //    absent entirely (garbage-collected), not kept as a null object. Match
    //    the exact line so `/Type /Pages` is not miscounted as a page.
    assert_eq!(qdf_page_count(qdf), 2, "{tool}: should drop to 2 pages");
    let page_objs = objects
        .iter()
        .filter(|(_, body)| body.lines().any(|l| l.trim() == "/Type /Page"))
        .count();
    assert_eq!(
        page_objs, 2,
        "{tool}: removed page must be garbage-collected, not kept/nulled"
    );

    // 2. The annotation that pointed at the removed page (carries `(rm)`)
    //    survives — reached through the OBJR's /Obj — but its dangling /P to the
    //    removed page is dropped.
    let rm_annot = find_annot(&objects, "(rm)", tool);
    assert!(
        !rm_annot.lines().any(|l| l.trim_start().starts_with("/P ")),
        "{tool}: removed-page annotation's dangling /P must be dropped, got:\n{rm_annot}"
    );

    // 3. The annotation that pointed at a surviving page (carries `(kp)`) keeps
    //    its /P, and that /P resolves to a /Type /Page object.
    let kp_annot = find_annot(&objects, "(kp)", tool);
    let p_line = kp_annot
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("/P "))
        .unwrap_or_else(|| {
            panic!("{tool}: surviving-page annotation must keep its /P, got:\n{kp_annot}")
        });
    let target: u32 = p_line
        .strip_prefix("/P ")
        .and_then(|r| r.strip_suffix(" 0 R"))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{tool}: malformed /P line `{p_line}`"));
    let (_, target_body) = objects
        .iter()
        .find(|&&(num, _)| num == target)
        .unwrap_or_else(|| panic!("{tool}: surviving /P target object {target} not found"));
    assert!(
        target_body.lines().any(|l| l.trim() == "/Type /Page"),
        "{tool}: surviving /P must point at a page object, got:\n{target_body}"
    );

    // 4. Both OBJR objects survive and each keeps its /Obj (the structure-tree
    //    reference that kept the annotations alive is preserved).
    let objr_count = objects
        .iter()
        .filter(|(_, body)| body.lines().any(|l| l.trim() == "/Type /OBJR"))
        .count();
    assert_eq!(objr_count, 2, "{tool}: both OBJR objects must survive");
    for (num, body) in objects
        .iter()
        .filter(|(_, body)| body.lines().any(|l| l.trim() == "/Type /OBJR"))
    {
        assert!(
            body.lines().any(|l| l.trim_start().starts_with("/Obj ")),
            "{tool}: OBJR object {num} must keep its /Obj, got:\n{body}"
        );
    }
}

#[test]
fn objr_obj_annot_p_to_removed_page_dropped_and_page_gced_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    eprintln!(
        "qpdf {EXPECTED_QPDF_VERSION} present; running real OBJR /Obj annotation /P assertions"
    );

    let tmp = tempfile::tempdir().unwrap();
    let src_file = objr_obj_annot_fixture();
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
        "qpdf --pages should succeed on the OBJR /Obj fixture (exit {:?}): {}",
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

    let q_qdf = normalize_qdf(&q_out, &tmp.path().join("q.qdf"));
    let f_qdf = normalize_qdf(&f_out, &tmp.path().join("f.qdf"));

    assert_facts(&q_qdf, "qpdf");
    assert_facts(&f_qdf, "flpdf");
}

/// Build a 3-page PDF whose removed page 2 (object 4) is referenced by BOTH a
/// surviving outline `/Dest` AND a structure-tree OBJR `/Obj` annotation's `/P`.
///
/// The surviving destination forces the earlier null-out pass to keep the
/// removed page as `null` (rather than garbage-collect it), so the OBJR-survived
/// annotation's `/P` resolves to `null` when the `/P`-drop pass runs.
fn combined_outline_dest_objr_fixture() -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut buf: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let mut offsets: Vec<(u32, usize)> = Vec::new();
    let mut add = |buf: &mut Vec<u8>, num: u32, body: &str| {
        offsets.push((num, buf.len()));
        buf.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
    };

    add(
        &mut buf,
        1,
        "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R /Outlines 40 0 R >>",
    );
    add(
        &mut buf,
        2,
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>",
    );
    add(
        &mut buf,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    add(
        &mut buf,
        4,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [30 0 R] >>",
    );
    add(
        &mut buf,
        5,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    add(&mut buf, 10, "<< /Type /StructTreeRoot /K 20 0 R >>");
    add(
        &mut buf,
        20,
        "<< /Type /StructElem /S /Document /K 21 0 R >>",
    );
    add(&mut buf, 21, "<< /Type /OBJR /Pg 4 0 R /Obj 30 0 R >>");
    add(
        &mut buf,
        30,
        "<< /Type /Annot /Subtype /Text /NM (rm) /P 4 0 R /Rect [0 0 10 10] >>",
    );
    add(
        &mut buf,
        40,
        "<< /Type /Outlines /First 41 0 R /Last 41 0 R /Count 1 >>",
    );
    add(
        &mut buf,
        41,
        "<< /Title (to removed) /Parent 40 0 R /Dest [4 0 R /Fit] >>",
    );

    let max_num = offsets.iter().map(|&(n, _)| n).max().unwrap();
    let xref_pos = buf.len();
    buf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
    for i in 1..=max_num {
        match offsets.iter().find(|&&(n, _)| n == i) {
            Some(&(_, off)) => {
                buf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            None => buf.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    buf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            max_num + 1
        )
        .as_bytes(),
    );

    let mut f = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    f.write_all(&buf).unwrap();
    f.flush().unwrap();
    f
}

/// Return the body of the raw (un-normalized) `N 0 obj … endobj` span that
/// contains `marker`. Panics if no such object exists — an object hidden inside
/// an object stream would otherwise let a regression pass silently.
fn raw_object_with<'a>(raw: &'a str, marker: &str, tool: &str) -> &'a str {
    raw.split("endobj")
        .find(|chunk| chunk.contains(marker))
        .unwrap_or_else(|| {
            panic!("{tool}: no raw object containing `{marker}` (object-stream compressed?)")
        })
}

#[test]
fn objr_obj_annot_p_dropped_even_when_page_nulled_by_surviving_dest_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    eprintln!(
        "qpdf {EXPECTED_QPDF_VERSION} present; running combined dest+OBJR /P-drop assertions"
    );

    let tmp = tempfile::tempdir().unwrap();
    let src_file = combined_outline_dest_objr_fixture();
    let src = src_file.path();

    let q_out = tmp.path().join("q.pdf");
    let f_out = tmp.path().join("f.pdf");

    // `--object-streams=disable` (NOT `--qdf`) keeps the annotation object
    // greppable in the raw output WITHOUT running qpdf's QDF normaliser — which
    // would delete flpdf's dangling /P and mask the regression. The assertions
    // read this raw output, never a re-normalized form.
    let q_run = run_qpdf(&[
        "--object-streams=disable",
        src.to_str().unwrap(),
        "--pages",
        ".",
        "1,3",
        "--",
        q_out.to_str().unwrap(),
    ]);
    assert!(
        q_run.status.success(),
        "qpdf --pages should succeed on the combined fixture (exit {:?}): {}",
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

    // Assert on the RAW output of each tool (not --qdf-normalized): the removed
    // page is kept as `null` because the surviving outline /Dest still
    // references it, but the OBJR-survived annotation's dangling /P to it must
    // still be dropped — exactly what qpdf does.
    for (path, tool) in [(&q_out, "qpdf"), (&f_out, "flpdf")] {
        let raw = String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
        let rm_obj = raw_object_with(&raw, "(rm)", tool);
        assert!(
            rm_obj.contains("/Type /Annot"),
            "{tool}: the (rm) object should be the surviving annotation, got:\n{rm_obj}"
        );
        assert!(
            !rm_obj.contains("/P "),
            "{tool}: annotation /P to the nulled removed page must be dropped, got:\n{rm_obj}"
        );
    }
}
