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
    let source = source.replace("\r\n", "\n");
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
        .split_once("let loaded_state = match load_xref_state_from_source(")
        .and_then(|(_, rest)| rest.split_once("        ) {"))
        .map_or_else(
            || panic!("Pdf::open must call load_xref_state_from_source"),
            |(call, _)| call,
        );
    assert!(
        load_call.contains("resolver.as_ref()"),
        "Pdf::open must pass ResolverHandle as the xref owner: {load_call}"
    );

    let xref = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("xref.rs"),
    )
    .expect("read xref source")
    .replace("\r\n", "\n");
    for dead in [
        "fn load_xref_state_with_options",
        "fn load_xref_state_from_bytes",
        "Option<&dyn CanonicalTrailerOwner>",
    ] {
        assert!(
            !xref.contains(dead),
            "the owner-less standalone xref route is removed; {dead} must not return"
        );
    }
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

/// A classic table whose trailer has no `/Size`: qpdf's `read_xref` validates
/// the trailer before it accepts the section (`libqpdf/QPDF.cc:906-930`), so
/// the missing key is a `damagedPDF` warning attributed to the trailer that
/// is followed by reconstruction.
fn classic_trailer_without_size() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \ntrailer\n");
    bytes.extend_from_slice(b"<< /Root 1 0 R >>");
    bytes.extend_from_slice(format!("\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    bytes
}

/// An xref stream whose `/W` widths do not describe its payload. qpdf warns
/// about the size mismatch, fails on the unknown entry type, reconstructs,
/// and re-reads the same candidate while reconstructing
/// (`libqpdf/QPDF.cc:1046-1075,577-608`).
fn xref_stream_with_wrong_payload_size() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj\n<< /Type /XRef /W [1 0 1] /Size 1 /Length 4 >>\nstream\nabcd\nendstream\nendobj\nstartxref\n9\n%%EOF\n".to_vec()
}

/// Collect the warnings the canonical `Pdf::open` route accumulates, whether
/// the open ultimately succeeds or fails, rendered exactly as qpdf renders
/// the text after its own `WARNING: ` prefix (`QPDFExc::createWhat`,
/// `libqpdf/QPDFExc.cc:18-49`).
fn canonical_open_warnings(bytes: Vec<u8>, description: &str) -> Vec<String> {
    let options = PdfOpenOptions {
        repair: true,
        suppress_warnings: true,
        description: description.as_bytes().to_vec(),
        ..PdfOpenOptions::default()
    };
    match Pdf::open_with_options(Cursor::new(bytes), options) {
        Ok(pdf) => pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|diagnostic| String::from_utf8_lossy(diagnostic.what_bytes()).into_owned())
            .collect(),
        Err(error) => {
            let (_, diagnostics) = error
                .open_failure()
                .expect("a permissive open failure carries its accumulated warnings");
            diagnostics
                .entries()
                .iter()
                .map(|diagnostic| String::from_utf8_lossy(diagnostic.what_bytes()).into_owned())
                .collect()
        }
    }
}

fn qpdf_warnings(path: &PathBuf) -> Vec<String> {
    let output = Command::new("qpdf")
        .args(["--warning-exit-0", "--show-xref"])
        .arg(path)
        .output()
        .expect("qpdf should spawn");
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("WARNING: "))
        .map(str::to_owned)
        .collect()
}

/// The canonical owner route must reproduce qpdf's repair-warning sequence
/// -- text, order and offsets -- for the damaged shapes the migrated xref
/// unit tests assert. Those unit tests compare flpdf against its own
/// expectations; this one pins the same sequences to the qpdf 11.9.0 binary
/// so the migration cannot drift both sides together.
#[test]
fn canonical_route_repair_warnings_match_qpdf() {
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    for (name, fixture, expected) in [
        (
            "classic-trailer-without-size.pdf",
            classic_trailer_without_size(),
            vec![
                "file is damaged".to_owned(),
                "(trailer, offset 45): trailer dictionary lacks /Size key".to_owned(),
                "Attempting to reconstruct cross-reference table".to_owned(),
            ],
        ),
        (
            "xref-stream-wrong-payload-size.pdf",
            xref_stream_with_wrong_payload_size(),
            vec![
                "(xref stream, offset 9): Cross-reference stream data has the wrong size; expected = 2; actual = 4".to_owned(),
                "file is damaged".to_owned(),
                "(xref stream, offset 71): unknown xref stream entry type 97".to_owned(),
                "Attempting to reconstruct cross-reference table".to_owned(),
                "(xref stream, offset 9): Cross-reference stream data has the wrong size; expected = 2; actual = 4".to_owned(),
                "reported number of objects (1) is not one plus the highest object number (1)".to_owned(),
            ],
        ),
    ] {
        let input = directory.path().join(name);
        fs::write(&input, &fixture).expect("write qpdf fixture");
        let description = input.to_string_lossy().into_owned();
        // qpdf prefixes its own source name to every warning; rendering the
        // flpdf diagnostics with the same description makes the two lists
        // directly comparable instead of only message-detail comparable.
        let expected: Vec<String> = expected
            .into_iter()
            .map(|message| format!("{description}{}{message}", if message.starts_with('(') { " " } else { ": " }))
            .collect();

        assert_eq!(
            canonical_open_warnings(fixture, &description),
            expected,
            "canonical route warnings changed for {name}"
        );

        if !qpdf_available() {
            eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
            continue;
        }
        assert_eq!(
            qpdf_warnings(&input),
            expected,
            "qpdf 11.9.0 no longer produces the pinned sequence for {name}"
        );
    }
}
