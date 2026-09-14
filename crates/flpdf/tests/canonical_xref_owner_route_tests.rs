//! Route contracts for the canonical-owner xref handoff.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use flpdf::{ObjectRef, Pdf, PdfOpenOptions, XrefEntry};

fn production_source(path: &str) -> String {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join(path),
    )
    .unwrap_or_else(|error| panic!("unable to read {path}: {error}"));
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

fn qpdf_available() -> bool {
    Command::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == "qpdf version 11.9.0")
        })
}

/// Build a damaged document whose xref-stream candidate contains 65
/// line-start false object headers. qpdf's `reconstruct_xref` resolves the
/// candidate to EOF (`libqpdf/QPDF.cc:577-608`), while the owner-less test
/// scaffolding's bounded retry would stop after 64 offset positions.
fn candidate_with_more_than_64_false_headers() -> (Vec<u8>, usize, usize) {
    const CANDIDATE: u32 = 1_000;
    const FALSE_HEADER_COUNT: u32 = 65;

    let mut bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    let candidate_offset = bytes.len();

    // The first two xref-stream entries are object 0 and the candidate. The
    // remaining bytes are deliberately extra stream data: qpdf warns about
    // the size mismatch but still accepts the candidate and ignores the tail.
    let mut payload = vec![0, 0, 0, 0, 255];
    let candidate_offset = u32::try_from(candidate_offset).expect("fixture offset fits u32");
    let offset = candidate_offset.to_be_bytes();
    payload.extend_from_slice(&[1, offset[1], offset[2], offset[3], 0]);
    payload.push(b'\n');
    for number in CANDIDATE + 1..=CANDIDATE + FALSE_HEADER_COUNT {
        payload.extend_from_slice(format!("{number} 0 obj\nnull\nendobj\n").as_bytes());
    }

    bytes.extend_from_slice(
        format!(
            "{CANDIDATE} 0 obj\n<< /Type /XRef /Size 1001 /Root 1 0 R /Index [0 1 {CANDIDATE} 1] /W [1 3 1] /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\nstartxref\n0\n%%EOF\n");

    (bytes, candidate_offset as usize, payload.len())
}

#[test]
fn canonical_open_does_not_keep_a_bootstrap_owner_or_rebind_handoff() {
    let engine = production_source("engine.rs");
    for forbidden in [
        "let bootstrap_cache = loaded_state.bootstrap_cache",
        "drop(bootstrap_cache)",
        "rebind_handle_value(",
    ] {
        assert!(
            !engine.contains(forbidden),
            "canonical Pdf::open retains bootstrap handoff {forbidden}"
        );
    }
    let reader = production_source("reader.rs");
    assert!(
        !reader.contains("rebind_handle_value("),
        "canonical parsed xref streams must already belong to ResolverHandle"
    );
}

#[test]
fn pdf_teardown_has_one_canonical_disconnect_owner() {
    let pdf = production_source("pdf.rs");
    assert_eq!(
        pdf.matches("self.resolver.disconnect_all()").count(),
        1,
        "Pdf teardown must use exactly one ResolverHandle disconnect walk"
    );
    let resolver = production_source("reader/resolver.rs");
    let teardown = resolver
        .split_once("pub(crate) fn disconnect_all")
        .and_then(|(_, rest)| rest.split_once("    pub(crate) fn "))
        .map_or(resolver.as_str(), |(function, _)| function);
    assert_eq!(
        teardown.matches("raw_source_xref_entries.clear()").count(),
        1,
        "the raw xref-table clear must stay a single production site"
    );
    assert_eq!(
        teardown.matches("object_cache").count(),
        1,
        "the object-cache walk must stay a single production site"
    );
    let clear = teardown
        .find("raw_source_xref_entries.clear()")
        .expect("teardown clears the canonical xref table");
    let walk = teardown
        .find("object_cache")
        .expect("teardown walks the canonical object cache");
    assert!(
        clear < walk,
        "qpdf teardown clears xref_table before disconnecting obj_cache"
    );
}

#[test]
fn resolver_has_one_raw_source_xref_owner() {
    let resolver = production_source("reader/resolver.rs");
    assert!(
        !resolver.contains("source_xref_entries: BTreeMap<ObjectRef, XrefEntry>"),
        "production resolver must not retain a second ObjectRef-keyed source xref map"
    );
    assert_eq!(
        resolver
            .matches("raw_source_xref_entries: BTreeMap<QpdfObjGen, XrefEntry>")
            .count(),
        1,
        "the resolver must have exactly one raw QPDFObjGen source table"
    );
}

#[test]
fn production_open_always_supplies_the_canonical_xref_owner() {
    let engine = production_source("engine.rs");
    let load_call = engine
        .split_once("let loaded_state = match load_xref_state_from_bytes(")
        .and_then(|(_, rest)| rest.split_once("        ) {"))
        .map_or_else(
            || panic!("Pdf::open must call load_xref_state_from_bytes"),
            |(call, _)| call,
        );
    assert!(
        load_call.contains("Some(resolver.as_ref())"),
        "Pdf::open must pass ResolverHandle as the xref owner: {load_call}"
    );

    let xref = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("xref.rs"),
    )
    .expect("read xref source");
    assert!(
        xref.contains("#[cfg(test)]\npub(crate) fn load_xref_state_with_options"),
        "the ownerless standalone xref loader must remain test-only"
    );
}

#[test]
fn canonical_open_reads_a_candidate_past_64_false_headers_like_qpdf() {
    let (fixture, candidate_offset, payload_len) = candidate_with_more_than_64_false_headers();
    let pdf = Pdf::open_with_options(
        Cursor::new(fixture.clone()),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("canonical open must recover the candidate xref stream");
    let candidate = pdf
        .get_xref_table()
        .get(&ObjectRef::new(1_000, 0))
        .copied()
        .expect("canonical xref table must retain the candidate entry");
    assert_eq!(
        candidate,
        XrefEntry::Uncompressed {
            offset: candidate_offset as u64
        }
    );
    assert!(
        pdf.repair_diagnostics().entries().iter().any(|diagnostic| {
            String::from_utf8_lossy(diagnostic.get_message_detail())
                == format!(
                    "Cross-reference stream data has the wrong size; expected = 10; actual = {payload_len}"
                )
        }),
        "canonical open must keep qpdf's candidate size warning"
    );

    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
        return;
    }

    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("candidate-over-64-false-headers.pdf");
    fs::write(&input, fixture).expect("write qpdf fixture");
    let qpdf = Command::new("qpdf")
        .args(["--warning-exit-0", "--show-xref"])
        .arg(&input)
        .output()
        .expect("qpdf should spawn");
    assert!(
        qpdf.status.success(),
        "qpdf --show-xref failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains(&format!(
            "Cross-reference stream data has the wrong size; expected = 10; actual = {payload_len}"
        )),
        "qpdf must report the fixture's intentionally extra xref-stream data: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let shown = String::from_utf8_lossy(&qpdf.stdout);
    assert!(
        shown.contains(&format!(
            "1000/0: uncompressed; offset = {candidate_offset}"
        )),
        "qpdf must retain the candidate entry: {shown}"
    );
}
