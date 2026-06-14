//! Article-thread bead `/P` drop parity vs qpdf 11.9.0.
//!
//! Truth source: `/usr/bin/qpdf` 11.9.0. When `--pages` drops a page that is
//! reached ONLY from an article bead's `/P`, qpdf removes the `/P` key from that
//! bead and the now-unreferenced page is garbage-collected: it is absent from
//! the output, NOT emitted as a `null` object. The bead itself stays in the
//! thread ring (its `/N`/`/V`/`/T`/`/R` intact) and the thread is retained. A
//! `/P` pointing at a surviving page is kept (remapped to the surviving page
//! object). This is the structural-reference *drop* family — the opposite of
//! the link-annot / outline / named-dest null-out family.
//!
//! This test runs the SAME fixture and flags through both `qpdf` and the
//! `flpdf` binary, then asserts the observable parity:
//!   1. both outputs have exactly 2 pages and exactly 2 `/Type /Page` objects
//!      (the removed page is gone entirely — no null placeholder),
//!   2. the thread ring is intact: exactly 3 `/Type /Bead` objects and exactly
//!      1 `/Type /Thread`, whose `/F` resolves to a bead,
//!   3. exactly one bead has NO `/P` (the one that pointed at the removed page),
//!   4. every other bead keeps a `/P` resolving to a `/Type /Page` object.
//!
//! Both outputs are normalized with `qpdf --qdf --object-streams=disable
//! --no-original-object-ids` first, so the assertions read a stable textual
//! form. Object numbering differs between tools, so beads are classified by
//! whether they carry a `/P`, never by hardcoded object numbers.

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

/// Whether the object body has a top-level `/P ` key (a bead's page back-ref).
fn has_p_key(body: &str) -> bool {
    body.lines().any(|l| l.trim().starts_with("/P "))
}

/// Parse the object number from a `/<key> N 0 R` line within `body`.
fn ref_target(body: &str, key: &str) -> Option<u32> {
    let prefix = format!("/{key} ");
    body.lines()
        .map(str::trim)
        .find(|l| l.starts_with(&prefix))?
        .strip_prefix(&prefix)?
        .strip_suffix(" 0 R")?
        .parse()
        .ok()
}

fn is_page(objects: &[(u32, String)], num: u32) -> bool {
    objects
        .iter()
        .find(|&&(n, _)| n == num)
        .is_some_and(|(_, body)| body.lines().any(|l| l.trim() == "/Type /Page"))
}

/// Build a 3-page PDF whose page 2 is referenced ONLY by an article bead's `/P`
/// (besides the page tree itself).
///
/// Object layout: 1 catalog (/Threads [10]), 2 pages, 3 page1, 4 page2,
/// 5 page3 (each with a /B bead array), 10 thread (/F 11), 11/12/13 a 3-bead
/// ring 11→12→13 with bead 11 on page1, bead 12 on page2, bead 13 on page3.
fn thread_fixture() -> tempfile::NamedTempFile {
    thread_fixture_inner(true)
}

/// Same layout as [`thread_fixture`] but with no catalog `/Threads`, so the bead
/// ring is reachable only through the surviving pages' `/B` arrays.
fn thread_fixture_b_only() -> tempfile::NamedTempFile {
    thread_fixture_inner(false)
}

fn thread_fixture_inner(with_threads: bool) -> tempfile::NamedTempFile {
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
        if with_threads {
            "<< /Type /Catalog /Pages 2 0 R /Threads [10 0 R] >>"
        } else {
            "<< /Type /Catalog /Pages 2 0 R >>"
        },
    );
    add(
        &mut buf,
        2,
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>",
    );
    add(
        &mut buf,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /B [11 0 R] >>",
    );
    add(
        &mut buf,
        4,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /B [12 0 R] >>",
    );
    add(
        &mut buf,
        5,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /B [13 0 R] >>",
    );
    add(&mut buf, 10, "<< /Type /Thread /F 11 0 R >>");
    add(
        &mut buf,
        11,
        "<< /Type /Bead /T 10 0 R /N 12 0 R /V 13 0 R /P 3 0 R /R [0 0 100 100] >>",
    );
    add(
        &mut buf,
        12,
        "<< /Type /Bead /T 10 0 R /N 13 0 R /V 11 0 R /P 4 0 R /R [0 0 100 100] >>",
    );
    add(
        &mut buf,
        13,
        "<< /Type /Bead /T 10 0 R /N 11 0 R /V 12 0 R /P 5 0 R /R [0 0 100 100] >>",
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

/// Assert the bead `/P` drop-family facts on one tool's normalized output.
fn assert_bead_p_drop_facts(qdf: &str, tool: &str) {
    // 1. Exactly 2 pages and exactly 2 /Type /Page objects: the removed page is
    //    absent entirely (drop family), not kept as a null object.
    assert_eq!(qdf_page_count(qdf), 2, "{tool}: should drop to 2 pages");
    let objects = qdf_objects(qdf);
    let page_objs = objects
        .iter()
        .filter(|(_, body)| body.lines().any(|l| l.trim() == "/Type /Page"))
        .count();
    assert_eq!(
        page_objs, 2,
        "{tool}: removed page must be garbage-collected, not kept/nulled"
    );

    // 2. The thread ring is intact: all 3 beads survive, and exactly one thread
    //    remains with its /F pointing at a bead.
    let beads: Vec<&(u32, String)> = objects
        .iter()
        .filter(|(_, body)| body.lines().any(|l| l.trim() == "/Type /Bead"))
        .collect();
    assert_eq!(
        beads.len(),
        3,
        "{tool}: every bead must be kept in the ring, found {}",
        beads.len()
    );
    let threads: Vec<&(u32, String)> = objects
        .iter()
        .filter(|(_, body)| body.lines().any(|l| l.trim() == "/Type /Thread"))
        .collect();
    assert_eq!(
        threads.len(),
        1,
        "{tool}: the thread must be retained, found {}",
        threads.len()
    );
    let f_target = ref_target(&threads[0].1, "F").unwrap_or_else(|| {
        panic!(
            "{tool}: thread /F must be a reference, got:\n{}",
            threads[0].1
        )
    });
    assert!(
        objects
            .iter()
            .any(|&(n, ref body)| n == f_target && body.lines().any(|l| l.trim() == "/Type /Bead")),
        "{tool}: thread /F must resolve to a bead object"
    );

    // 3. Exactly one bead has no /P (the one that pointed at the removed page).
    let beads_without_p = beads.iter().filter(|(_, b)| !has_p_key(b)).count();
    assert_eq!(
        beads_without_p, 1,
        "{tool}: exactly one bead (removed-page) must have its /P dropped, found {beads_without_p}"
    );

    // 4. Every other bead keeps a /P resolving to a /Type /Page object.
    for (num, body) in &beads {
        if !has_p_key(body) {
            continue;
        }
        let target = ref_target(body, "P")
            .unwrap_or_else(|| panic!("{tool}: bead {num} /P must be a reference, got:\n{body}"));
        assert!(
            is_page(&objects, target),
            "{tool}: surviving bead {num} /P must point at a page object (target {target})"
        );
    }
}

/// Run the same `--pages . 1,3` through qpdf and flpdf over `src_file`, then
/// assert both outputs satisfy the bead `/P` drop-family facts.
fn assert_parity(src_file: &tempfile::NamedTempFile) {
    let tmp = tempfile::tempdir().unwrap();
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
        "qpdf --pages should succeed on the fixture (exit {:?}): {}",
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

    assert_bead_p_drop_facts(&q_qdf, "qpdf");
    assert_bead_p_drop_facts(&f_qdf, "flpdf");
}

#[test]
fn bead_p_to_removed_page_dropped_and_page_gced_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    eprintln!(
        "qpdf {EXPECTED_QPDF_VERSION} present; running real article-thread bead /P assertions"
    );
    assert_parity(&thread_fixture());
}

#[test]
fn bead_p_drop_via_b_array_without_threads_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    eprintln!("qpdf {EXPECTED_QPDF_VERSION} present; running real /B-seeded bead /P assertions");
    // No catalog /Threads: qpdf still reaches the ring through the surviving
    // pages' /B arrays, drops the removed-page bead's /P, and GCs the page.
    assert_parity(&thread_fixture_b_only());
}

/// Build a 3-page PDF whose removed page 2 (object 4) is referenced by BOTH a
/// surviving outline `/Dest` AND a single article-thread bead's `/P`.
///
/// The surviving destination forces the earlier null-out pass
/// ([`outline_dest_remap`]) to keep the removed page as `null` (rather than
/// garbage-collect it), so the bead's `/P` resolves to `null` when the bead
/// `/P`-drop pass runs. The thread (object 10) and its single self-ring bead
/// (object 11, on the removed page) are kept alive by the catalog `/Threads`.
///
/// Object layout: 1 catalog (/Threads [10] /Outlines 40), 2 pages, 3 page1
/// (survives), 4 page2 (removed, /B [11]), 5 page3 (survives), 10 thread
/// (/F 11), 11 bead (/N 11 /V 11 self-ring, /P 4 to removed page), 40 outlines,
/// 41 outline item (/Dest [4 0 R /Fit] to the removed page).
fn combined_outline_dest_thread_fixture() -> tempfile::NamedTempFile {
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
        "<< /Type /Catalog /Pages 2 0 R /Threads [10 0 R] /Outlines 40 0 R >>",
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
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [11 0 R] >>",
    );
    add(
        &mut buf,
        5,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    add(&mut buf, 10, "<< /Type /Thread /F 11 0 R >>");
    add(
        &mut buf,
        11,
        "<< /Type /Bead /T 10 0 R /N 11 0 R /V 11 0 R /P 4 0 R /R [0 0 100 100] >>",
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
fn bead_p_dropped_even_when_page_nulled_by_surviving_dest_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    eprintln!(
        "qpdf {EXPECTED_QPDF_VERSION} present; running combined dest+bead /P-drop assertions"
    );

    let tmp = tempfile::tempdir().unwrap();
    let src_file = combined_outline_dest_thread_fixture();
    let src = src_file.path();

    let q_out = tmp.path().join("q.pdf");
    let f_out = tmp.path().join("f.pdf");

    // `--object-streams=disable` (NOT `--qdf`) keeps the bead object greppable in
    // the raw output WITHOUT running qpdf's QDF normaliser — which would delete
    // flpdf's dangling /P and mask the regression. The assertions read this raw
    // output, never a re-normalized form.
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
    // page is kept as `null` because the surviving outline /Dest still references
    // it, but the bead's dangling /P to it must still be dropped — exactly what
    // qpdf does. The fixture has a single bead, so `/Type /Bead` locates it.
    for (path, tool) in [(&q_out, "qpdf"), (&f_out, "flpdf")] {
        let raw = String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
        let bead_obj = raw_object_with(&raw, "/Type /Bead", tool);
        assert!(
            !bead_obj.contains("/P "),
            "{tool}: bead /P to the nulled removed page must be dropped, got:\n{bead_obj}"
        );
    }
}
