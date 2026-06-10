//! Struct-tree `/Pg` drop parity vs qpdf 11.9.0.
//!
//! Truth source: `/usr/bin/qpdf` 11.9.0. When `--pages` drops a page that is
//! reached ONLY from a structure element's `/Pg`, qpdf removes the `/Pg` key
//! from that element and the now-unreferenced page is garbage-collected: it is
//! absent from the output, NOT emitted as a `null` object. This is the
//! structural-reference *drop* family — the opposite of the link-annot /
//! outline / named-dest null-out family. A `/Pg` pointing at a surviving page
//! is kept (remapped to the surviving page object).
//!
//! This test runs the SAME fixture and flags through both `qpdf` and the
//! `flpdf` binary, then asserts the observable parity:
//!   1. both outputs have exactly 2 pages and exactly 2 `/Type /Page` objects
//!      (the removed page is gone entirely — no null placeholder),
//!   2. the structure element that pointed at the removed page has NO `/Pg`,
//!   3. the structure element that pointed at a surviving page keeps `/Pg`,
//!      and it resolves to a `/Type /Page` object.
//!
//! Both outputs are normalized with `qpdf --qdf --object-streams=disable
//! --no-original-object-ids` first, so the assertions read a stable textual
//! form. Object numbering differs between tools, so elements are located by
//! their MCID kid (`/K 0` vs `/K 1`), never by hardcoded object numbers.

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
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines().next().map(str::trim)
                == Some(&format!("qpdf version {EXPECTED_QPDF_VERSION}"))
        }
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

/// Find the single structure element whose body contains `marker`
/// (an MCID kid line such as `/K 0`).
fn find_elem<'a>(objects: &'a [(u32, String)], marker: &str, tool: &str) -> &'a str {
    let mut found = objects.iter().filter(|(_, body)| {
        body.contains("/Type /StructElem") && body.lines().any(|l| l.trim() == marker)
    });
    let (_, body) = found
        .next()
        .unwrap_or_else(|| panic!("{tool}: no StructElem with `{marker}` found"));
    assert!(
        found.next().is_none(),
        "{tool}: more than one StructElem with `{marker}`"
    );
    body
}

/// Build a 3-page PDF whose page 2 is referenced ONLY by a structure
/// element's `/Pg` (besides the page tree itself).
///
/// Object layout: 1 catalog (/StructTreeRoot 10), 2 pages, 3 page1, 4 page2,
/// 5 page3, 10 struct tree root (/K 20), 20 document elem (/K [21 22]),
/// 21 elem `/Pg 4 0 R` + MCID kid 0, 22 elem `/Pg 3 0 R` + MCID kid 1.
fn structtree_fixture() -> tempfile::NamedTempFile {
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
    for n in 3..=5 {
        add(
            &mut buf,
            n,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> >>",
        );
    }
    add(&mut buf, 10, "<< /Type /StructTreeRoot /K 20 0 R >>");
    add(
        &mut buf,
        20,
        "<< /Type /StructElem /S /Document /P 10 0 R /K [21 0 R 22 0 R] >>",
    );
    add(
        &mut buf,
        21,
        "<< /Type /StructElem /S /P /P 20 0 R /Pg 4 0 R /K 0 >>",
    );
    add(
        &mut buf,
        22,
        "<< /Type /StructElem /S /P /P 20 0 R /Pg 3 0 R /K 1 >>",
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

/// Assert the drop-family facts on one tool's normalized output.
fn assert_pg_drop_facts(qdf: &str, tool: &str) {
    // 1. Exactly 2 pages and exactly 2 /Type /Page objects: the removed page
    //    is absent entirely (drop family), not kept as a null object.
    assert_eq!(qdf_page_count(qdf), 2, "{tool}: should drop to 2 pages");
    let objects = qdf_objects(qdf);
    let page_objs = objects
        .iter()
        .filter(|(_, body)| body.contains("/Type /Page\n") || body.ends_with("/Type /Page"))
        .count();
    let page_objs = if page_objs == 0 {
        // Key order differs between writers; count via line match instead.
        objects
            .iter()
            .filter(|(_, body)| body.lines().any(|l| l.trim() == "/Type /Page"))
            .count()
    } else {
        page_objs
    };
    assert_eq!(
        page_objs, 2,
        "{tool}: removed page must be garbage-collected, not kept/nulled"
    );

    // 2. The element whose /Pg pointed at the removed page (MCID kid 0) has
    //    no /Pg anymore.
    let removed_elem = find_elem(&objects, "/K 0", tool);
    assert!(
        !removed_elem.contains("/Pg"),
        "{tool}: StructElem with removed-page /Pg must have the key dropped, got:\n{removed_elem}"
    );

    // 3. The element whose /Pg pointed at a surviving page (MCID kid 1) keeps
    //    it, and it resolves to a /Type /Page object.
    let kept_elem = find_elem(&objects, "/K 1", tool);
    let pg_line = kept_elem
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("/Pg "))
        .unwrap_or_else(|| {
            panic!("{tool}: StructElem with surviving-page /Pg must keep it, got:\n{kept_elem}")
        });
    let target: u32 = pg_line
        .strip_prefix("/Pg ")
        .and_then(|r| r.strip_suffix(" 0 R"))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{tool}: malformed /Pg line `{pg_line}`"));
    let (_, target_body) = objects
        .iter()
        .find(|&&(num, _)| num == target)
        .unwrap_or_else(|| panic!("{tool}: surviving /Pg target object {target} not found"));
    assert!(
        target_body.lines().any(|l| l.trim() == "/Type /Page"),
        "{tool}: surviving /Pg must point at a page object, got:\n{target_body}"
    );
}

#[test]
fn structelem_pg_to_removed_page_dropped_and_page_gced_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    eprintln!("qpdf {EXPECTED_QPDF_VERSION} present; running real struct-tree /Pg assertions");

    let tmp = tempfile::tempdir().unwrap();
    let src_file = structtree_fixture();
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
        "qpdf --pages should succeed on the struct-tree fixture (exit {:?}): {}",
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

    assert_pg_drop_facts(&q_qdf, "qpdf");
    assert_pg_drop_facts(&f_qdf, "flpdf");
}
