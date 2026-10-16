//! Route contracts for the canonical-owner xref handoff.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use flpdf::{Error, ObjectRef, Pdf, PdfOpenOptions, XrefEntry};

fn source_file(path: &str) -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join(path),
    )
    .unwrap_or_else(|error| panic!("unable to read {path}: {error}"))
    .replace("\r\n", "\n")
}

fn production_source(path: &str) -> String {
    let source = source_file(path);
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
        teardown
            .matches("xref_registration.clear_raw_entries()")
            .count(),
        1,
        "the raw xref-table clear must stay a single production site"
    );
    assert_eq!(
        teardown.matches("object_cache").count(),
        1,
        "the object-cache walk must stay a single production site"
    );
    let clear = teardown
        .find("xref_registration.clear_raw_entries()")
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
fn resolver_has_one_owner_held_xref_registration() {
    let resolver = production_source("reader/resolver.rs");
    assert!(
        !resolver.contains("source_xref_entries: BTreeMap<ObjectRef, XrefEntry>"),
        "production resolver must not retain a second ObjectRef-keyed source xref map"
    );
    assert_eq!(
        resolver
            .matches("\n    xref_registration: XrefRegistration,\n")
            .count(),
        1,
        "the resolver must own exactly one canonical xref registration"
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
fn open_and_resolve_recovery_share_one_owner_operation() {
    fn function_region<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        source
            .split_once(start)
            .and_then(|(_, rest)| rest.split_once(end))
            .map_or_else(
                || panic!("production source is missing function boundary {start} .. {end}"),
                |(function, _)| function,
            )
    }

    let xref = source_file("xref.rs");
    let open = function_region(
        &xref,
        "fn load_xref_state_from_window(",
        "#[allow(clippy::too_many_arguments)]\nfn parse_xref_from_start_with_owner(",
    );
    let shared = function_region(
        &xref,
        "fn reconstruct_xref_on_owner(",
        "fn merge_recovered_qpdf_state(",
    );
    let resolver = production_source("reader/resolver.rs");
    let delayed = function_region(
        &resolver,
        "fn reconstruct_xref_and_retry(",
        "    /// Sever every canonical handle's value",
    );

    assert!(
        open.contains("reconstruct_xref_on_owner("),
        "parse recovery must delegate to the shared owner operation"
    );
    assert!(
        delayed.contains("reconstruct_xref_on_owner("),
        "delayed recovery must delegate to the shared owner operation"
    );
    assert!(
        delayed.contains("XrefReconstructionRequest::DelayedResolution"),
        "delayed recovery must supply its typed trigger context"
    );
    for placeholder in ["&[]", "String::new()"] {
        assert!(
            !delayed.contains(placeholder),
            "delayed recovery must not pass open-only placeholder {placeholder}"
        );
    }
    for shared_step in [
        "begin_xref_reconstruction()",
        "push_repair_diagnostics(",
        "remove_uncompressed_entries()",
        "recover_xref_entries_from_source(",
        "clear_deleted_objects()",
        "recover_trailer_from_xref_stream_candidate(",
    ] {
        assert!(
            shared.contains(shared_step),
            "the shared owner operation must own {shared_step}"
        );
    }
    assert!(
        delayed.contains("XrefEntry::Uncompressed { offset: new_offset }"),
        "the resolver caller must retain qpdf's type-1-only retry decision"
    );
    for duplicated_step in [
        "begin_xref_reconstruction()",
        "push_warning_at(0, \"file is damaged\")",
        "recover_xref_entries(",
        "remove_uncompressed_entries()",
    ] {
        assert!(
            !delayed.contains(duplicated_step),
            "the resolver caller must not duplicate shared step {duplicated_step}"
        );
    }
}

#[test]
fn hybrid_xref_key_presence_uses_the_resolving_has_key_route() {
    let xref = source_file("xref.rs");
    let hybrid = xref
        .split_once("fn merge_xref_stream_from_classic_trailer_with_build_diagnostics(")
        .and_then(|(_, rest)| rest.split_once("\n/// `error_diagnostics_sink`"))
        .map_or_else(
            || panic!("production hybrid xref merge function must remain identifiable"),
            |(function, _)| function,
        );

    assert!(
        hybrid.contains("loaded.loaded.trailer.try_has_key(b\"/XRefStm\")?"),
        "hybrid xref key presence must resolve the trailer value like QPDFObjectHandle::hasKey"
    );
    let key_check = hybrid
        .find("loaded.loaded.trailer.try_has_key(b\"/XRefStm\")?")
        .expect("the hybrid trailer key is checked with the resolving accessor");
    let ignore_gate = hybrid
        .find("if options.ignore_xref_streams")
        .expect("the existing ignore-xref-streams gate must remain");
    assert!(
        key_check < ignore_gate,
        "qpdf checks trailer key presence before the ignore-xref-streams gate"
    );
    assert!(
        !hybrid.contains(".as_dictionary()") && !hybrid.contains("contains_key(b\"/XRefStm\""),
        "hybrid xref key presence must not inspect the unresolved raw map"
    );
}

#[test]
fn candidate_recovery_reentry_without_a_trailer_matches_qpdf_failure() {
    let fixture = include_bytes!("fixtures/xref-reconstruction-reentrant-before-trailer.pdf");
    assert_eq!(
        fixture,
        include_bytes!(
            "../../../fuzz/seeds/roundtrip/xref-reconstruction-reentrant-before-trailer.pdf"
        ),
        "the regression fixture is also kept in the short fuzz corpus"
    );
    let open = std::panic::catch_unwind(|| {
        Pdf::open_with_options(
            Cursor::new(fixture),
            PdfOpenOptions {
                repair: true,
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
    });
    let result = open.expect("qpdf rejects this recovery candidate without panicking");
    let error = match result {
        Ok(_) => panic!("the fixture has no recoverable trailer dictionary"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("unable to find trailer dictionary while recovering damaged file"),
        "expected qpdf's terminal recovery error, got {error}"
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

/// A classic xref table that inserts type-1 rows for objects 1 and 2 before
/// its invalid object-3 row fails. Both rows point to object 1; the only text
/// resembling object 2 is inside a comment, so qpdf reconstruction must drop
/// the stale loader row for 2 and recover only the real object 1.
fn classic_xref_with_stale_type1_loader_row() -> (Vec<u8>, usize, usize) {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let object_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    bytes.extend_from_slice(b"% 2 0 obj\n<< /Type /Pages >>\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
    let invalid_row_offset = bytes.len();
    bytes.extend_from_slice(format!("{object_offset:010} 00000 x \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    (bytes, object_offset, invalid_row_offset)
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

fn classic_xref_with_warning_before_bad_entry() -> (Vec<u8>, usize, usize) {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 3\n");
    // The extra field separator is accepted but warned by qpdf's
    // parse_xrefEntry (`QPDF.cc:790-802`).
    bytes.extend_from_slice(b"0000000000  65535 f \n");
    // The next row is fatal, so warnings already emitted for row zero must
    // remain in QPDF::warn's document-owned collection during reconstruction.
    bytes.extend_from_slice(b"not an xref entry\n");
    bytes.extend_from_slice(b"0000000000 00000 n \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    (bytes, catalog_offset, pages_offset)
}

#[test]
fn failed_classic_xref_parse_keeps_prior_entry_warnings_like_qpdf() {
    let (fixture, catalog_offset, pages_offset) = classic_xref_with_warning_before_bad_entry();
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory
        .path()
        .join("classic-xref-warning-before-error.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();
    let mut pdf = Pdf::open_with_options(
        Cursor::new(fixture),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("canonical recovery should recover objects after the broken xref row");
    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));
    pdf.root_handle()
        .expect("reconstruction should retain the Catalog object");
    let flpdf_warnings: Vec<String> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
        .collect();
    let rendered = render_xref_table(&pdf.get_xref_table());
    assert!(rendered.contains(&format!("1/0: uncompressed; offset = {catalog_offset}")));
    assert!(rendered.contains(&format!("2/0: uncompressed; offset = {pages_offset}")));
    assert!(
        flpdf_warnings.first().is_some_and(|warning| {
            warning.contains("(xref table, offset")
                && warning.ends_with("accepting invalid xref table entry")
        }),
        "the canonical owner must retain the accepted row warning before recovery: {flpdf_warnings:?}"
    );

    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
        return;
    }
    let qpdf = Command::new("qpdf")
        .args(["--warning-exit-0", "--show-xref"])
        .arg(&input)
        .output()
        .expect("qpdf should spawn");
    assert!(
        qpdf.status.success(),
        "qpdf should recover this classic table: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("WARNING: "))
        .map(str::to_owned)
        .collect();
    assert!(qpdf_warnings
        .first()
        .is_some_and(|warning| warning.contains("accepting invalid xref table entry")));
    let qpdf_rows: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
        .lines()
        .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
        .map(str::to_owned)
        .collect();
    assert_eq!(flpdf_warnings, qpdf_warnings);
    assert_eq!(rendered, qpdf_rows);
}

fn classic_xref_integer_overflow_fixture(section: &[u8]) -> (Vec<u8>, usize, usize, usize) {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n");
    let section_offset = bytes.len();
    bytes.extend_from_slice(section);
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    (bytes, catalog_offset, pages_offset, section_offset)
}

/// Exercise both sides of QPDF::parse's xref-read catch boundary:
/// parse_xrefFirst narrows subsection values before reading a row, while
/// parse_xrefEntry warns about accepted whitespace before narrowing its
/// signed 64-bit offset and signed 32-bit generation (`QPDF.cc:450-464,722-767,770-842`;
/// `QUtil.cc:373-393`).
fn assert_classic_xref_integer_overflow_matches_qpdf(
    filename: &str,
    section: &[u8],
    accepted_row_offset: Option<usize>,
    conversion_error: &str,
) {
    let error_detail = format!("error reading xref: {conversion_error}");

    let (fixture, catalog_offset, pages_offset, section_offset) =
        classic_xref_integer_overflow_fixture(section);
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join(filename);
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();
    let accepted_warning = accepted_row_offset.map(|offset| {
        format!(
            "{description} (xref table, offset {}): accepting invalid xref table entry",
            section_offset + offset
        )
    });

    let strict_error = match Pdf::open_with_options(
        Cursor::new(fixture.clone()),
        PdfOpenOptions {
            repair: false,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("strict open must reject the overflowing xref integer"),
        Err(error) => error,
    };
    let mut strict_warnings = Vec::new();
    let terminal_error = if let Some((source, diagnostics)) = strict_error.open_failure() {
        strict_warnings = diagnostics
            .entries()
            .iter()
            .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
            .collect();
        source
    } else {
        &strict_error
    };
    let Error::QpdfExc(qpdf_error) = terminal_error else {
        panic!("qpdf catches range_error as DamagedPdf, got {terminal_error:?}");
    };
    assert_eq!(
        qpdf_error.get_error_code(),
        flpdf::QpdfErrorCode::DamagedPdf
    );
    assert_eq!(qpdf_error.get_filename(), description.as_bytes());
    assert_eq!(qpdf_error.get_object(), b"");
    assert_eq!(qpdf_error.get_file_position(), 0);
    assert_eq!(qpdf_error.get_message_detail(), error_detail.as_bytes());
    assert_eq!(
        qpdf_error.what_bytes(),
        format!("{description}: {error_detail}").as_bytes()
    );
    assert_eq!(
        strict_warnings,
        accepted_warning.clone().into_iter().collect::<Vec<_>>()
    );

    let pdf = Pdf::open_with_options(
        Cursor::new(fixture.clone()),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("repair mode should reconstruct the two catalog objects");
    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));
    let flpdf_warnings: Vec<String> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
        .collect();
    let mut expected_warnings = accepted_warning.into_iter().collect::<Vec<_>>();
    expected_warnings.extend([
        format!("{description}: file is damaged"),
        format!("{description}: {error_detail}"),
        format!("{description}: Attempting to reconstruct cross-reference table"),
    ]);
    assert_eq!(flpdf_warnings, expected_warnings);
    let rendered = render_xref_table(&pdf.get_xref_table());
    assert!(rendered.contains(&format!("1/0: uncompressed; offset = {catalog_offset}")));
    assert!(rendered.contains(&format!("2/0: uncompressed; offset = {pages_offset}")));

    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
        return;
    }
    let strict_qpdf = Command::new("qpdf")
        .args(["--suppress-recovery", "--check"])
        .arg(&input)
        .output()
        .expect("qpdf should spawn");
    assert_eq!(strict_qpdf.status.code(), Some(2));
    let strict_stderr = String::from_utf8_lossy(&strict_qpdf.stderr);
    let strict_qpdf_warnings: Vec<String> = strict_stderr
        .lines()
        .filter_map(|line| line.strip_prefix("WARNING: "))
        .map(str::to_owned)
        .collect();
    assert_eq!(strict_warnings, strict_qpdf_warnings);
    assert!(strict_stderr
        .lines()
        .any(|line| line == format!("qpdf: {description}: {error_detail}")));

    let qpdf = Command::new("qpdf")
        .args(["--warning-exit-0", "--show-xref"])
        .arg(&input)
        .output()
        .expect("qpdf should spawn");
    assert!(
        qpdf.status.success(),
        "qpdf should recover this classic table: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("WARNING: "))
        .map(str::to_owned)
        .collect();
    assert_eq!(flpdf_warnings, qpdf_warnings);
    let qpdf_rows: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
        .lines()
        .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
        .map(str::to_owned)
        .collect();
    assert_eq!(rendered, qpdf_rows);
}

#[test]
fn classic_xref_subsection_integer_overflow_matches_qpdf() {
    assert_classic_xref_integer_overflow_matches_qpdf(
        "classic-xref-subsection-object-overflow.pdf",
        b"2147483648 1\n0000000000 00000 n \n",
        None,
        "integer out of range converting 2147483648 from a 8-byte signed type to a 4-byte signed type",
    );
}

#[test]
fn classic_xref_subsection_count_overflow_matches_qpdf() {
    assert_classic_xref_integer_overflow_matches_qpdf(
        "classic-xref-subsection-count-overflow.pdf",
        b"0 2147483648\n",
        None,
        "integer out of range converting 2147483648 from a 8-byte signed type to a 4-byte signed type",
    );
}

#[test]
fn classic_xref_generation_overflow_warns_before_the_qpdf_range_error() {
    assert_classic_xref_integer_overflow_matches_qpdf(
        "classic-xref-generation-overflow.pdf",
        b"0 1\n0000000000  2147483648 n \n",
        Some(b"0 1\n".len()),
        "integer out of range converting 2147483648 from a 8-byte signed type to a 4-byte signed type",
    );
}

#[test]
fn classic_xref_object_number_narrowing_preserves_row_warning_order() {
    assert_classic_xref_integer_overflow_matches_qpdf(
        "classic-xref-object-number-overflow.pdf",
        b"2147483647 2\n0000000000 00000 f \n0000000000  00000 n \n",
        Some(b"2147483647 2\n0000000000 00000 f \n".len()),
        "integer out of range converting 2147483648 from a 8-byte signed type to a 4-byte signed type",
    );
}

#[test]
fn classic_xref_entry_offset_overflow_uses_qpdf_signed_long_range() {
    assert_classic_xref_integer_overflow_matches_qpdf(
        "classic-xref-offset-overflow.pdf",
        b"0 1\n9223372036854775808 00000 f \n",
        Some(b"0 1\n".len()),
        "overflow/underflow converting 9223372036854775808 to 64-bit integer",
    );
}

#[test]
fn open_reconstruction_does_not_restore_stale_partially_parsed_xref_rows() {
    let (fixture, object_offset, invalid_row_offset) = classic_xref_with_stale_type1_loader_row();
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("stale-type1-loader-row.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();
    let pdf = Pdf::open_with_options(
        Cursor::new(fixture),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("open-time xref reconstruction should recover the catalog");

    let rendered = render_xref_table(&pdf.get_xref_table());
    assert_eq!(
        rendered,
        vec![format!("1/0: uncompressed; offset = {object_offset}")],
        "the final owner table must not restore stale 2/0 from the failed loader snapshot"
    );
    let flpdf_warnings: Vec<String> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
        .collect();
    let expected_warnings = vec![
        format!("{description}: file is damaged"),
        format!(
            "{description} (xref table, offset {invalid_row_offset}): invalid xref entry (obj=3)"
        ),
        format!("{description}: Attempting to reconstruct cross-reference table"),
    ];
    assert_eq!(
        flpdf_warnings, expected_warnings,
        "the failed partial table must enter qpdf's ordered reconstruction warning path"
    );

    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
        return;
    }
    let qpdf = Command::new("qpdf")
        .arg("--show-xref")
        .arg(&input)
        .output()
        .expect("qpdf should spawn");
    assert_eq!(
        qpdf.status.code(),
        Some(3),
        "qpdf reports recovery warnings as exit 3"
    );
    let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("WARNING: "))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        qpdf_warnings, expected_warnings,
        "qpdf 11.9.0 must retain the fixture's three ordered warnings"
    );
    let qpdf_rows: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
        .lines()
        .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        rendered, qpdf_rows,
        "the effective flpdf table must equal qpdf 11.9.0 after reconstruction"
    );
}

/// A classic xref table whose `/XRefStm` commits a free row and then fails.
///
/// qpdf's `processXRefStream` calls `insertFreeXrefEntry` inline while it
/// walks the payload (`libqpdf/QPDF.cc:1124-1127`), so the tombstone for
/// object 4 is already in `m->deleted_objects` when the following type-3 row
/// makes `insertXrefEntry` throw `unknown xref stream entry type 3`
/// (`libqpdf/QPDF.cc:1180-1183`). `read_xrefTable` does not catch that
/// exception, so `read_xref` hands straight to `reconstruct_xref`, which keeps
/// the filter for the whole line scan and clears it only afterwards
/// (`libqpdf/QPDF.cc:575`): `insertReconstructedXrefEntry` therefore refuses
/// object 4 (`libqpdf/QPDF.cc:1197-1210`) even though `4 0 obj` is present in
/// the file and the scan finds it.
///
/// A classic table's own `f` rows cannot reach this state. Both qpdf
/// (`libqpdf/QPDF.cc:880-931`: `deleted_items` is collected during the entry
/// loop but only handed to `insertFreeXrefEntry` after the `/XRefStm` read)
/// and flpdf defer them, so a failure inside the section discards them
/// instead of committing them. An xref stream's inline free rows are the only
/// way a tombstone survives into reconstruction.
///
/// Object 6 exists so that reconstruction overwrites the default type-0 row
/// `try_emplace` leaves behind for the throwing entry; without it
/// `qpdf --show-xref` aborts on that unrenderable row and shows no table at
/// all. The `/XRefStm` entry for object 4 must not be preceded by a live
/// classic row for the same object-generation slot, because
/// `insertFreeXrefEntry` only records a tombstone for an object that is not
/// already in the table.
fn classic_table_whose_xref_stm_commits_a_free_row_then_fails() -> (Vec<u8>, [usize; 7]) {
    let mut bytes = b"%PDF-1.5\n".to_vec();
    let mut offsets = [0usize; 7];
    let object = |bytes: &mut Vec<u8>, offsets: &mut [usize; 7], number: usize, body: &[u8]| {
        offsets[number] = bytes.len();
        bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    };
    object(
        &mut bytes,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    );
    object(
        &mut bytes,
        &mut offsets,
        2,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    object(
        &mut bytes,
        &mut offsets,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    object(&mut bytes, &mut offsets, 4, b"42");
    object(&mut bytes, &mut offsets, 6, b"43");

    // /W [1 2 1]: object 0 free, object 4 free, object 6 unknown type 3.
    let payload: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0];
    offsets[5] = bytes.len();
    bytes.extend_from_slice(b"5 0 obj\n");
    bytes.extend_from_slice(
        format!(
            "<< /Type /XRef /W [1 2 1] /Index [0 1 4 1 6 1] /Size 7 /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[1]).as_bytes());
    bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[2]).as_bytes());
    bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[3]).as_bytes());
    // Object 4 is free here too, so the deferred classic row cannot register
    // it before the `/XRefStm` tombstone is recorded.
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[5]).as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 6 /Root 1 0 R /XRefStm {} >>\nstartxref\n{xref}\n%%EOF\n",
            offsets[5]
        )
        .as_bytes(),
    );
    (bytes, offsets)
}

/// Render an xref table the way `qpdf --show-xref` renders it
/// (`QPDF::showXRefTable`, `libqpdf/QPDF.cc:1212-1240`).
fn render_xref_table(table: &std::collections::BTreeMap<ObjectRef, XrefEntry>) -> Vec<String> {
    table
        .iter()
        .map(|(object_ref, entry)| match entry {
            XrefEntry::Uncompressed { offset } => format!(
                "{}/{}: uncompressed; offset = {offset}",
                object_ref.number, object_ref.generation
            ),
            XrefEntry::Compressed { stream, index } => format!(
                "{}/{}: compressed; stream = {stream}, index = {index}",
                object_ref.number, object_ref.generation
            ),
            XrefEntry::Free { next } => format!(
                "{}/{}: free; next = {next}",
                object_ref.number, object_ref.generation
            ),
        })
        .collect()
}

/// A free row that an xref stream committed before failing must still suppress
/// its object number during reconstruction, on the handoff that reaches
/// reconstruction straight from the initial section's parse failure.
///
/// This is the one recovery handoff that does not pass through
/// `merge_recovered_qpdf_state`, and qpdf applies the suppression inside
/// `insertReconstructedXrefEntry` rather than after the scan, so it cannot
/// depend on the handoff taken.
#[test]
fn reconstruction_after_a_committed_free_row_suppresses_it_like_qpdf() {
    let (fixture, offsets) = classic_table_whose_xref_stm_commits_a_free_row_then_fails();
    assert_eq!(
        fixture.get(offsets[4]..offsets[4] + b"4 0 obj".len()),
        Some(b"4 0 obj".as_slice()),
        "the freed object must really be in the file, or the scan has nothing to suppress"
    );

    let pdf = Pdf::open_with_options(
        Cursor::new(fixture.clone()),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("the reconstructed table keeps the classic trailer, so the open succeeds");
    let table = pdf.get_xref_table();
    assert!(
        !table.contains_key(&ObjectRef::new(4, 0)),
        "the /XRefStm free row must suppress object 4 during reconstruction: {:?}",
        render_xref_table(&table)
    );
    let rendered = render_xref_table(&table);
    assert_eq!(
        rendered,
        vec![
            format!("1/0: uncompressed; offset = {}", offsets[1]),
            format!("2/0: uncompressed; offset = {}", offsets[2]),
            format!("3/0: uncompressed; offset = {}", offsets[3]),
            format!("5/0: uncompressed; offset = {}", offsets[5]),
            format!("6/0: uncompressed; offset = {}", offsets[6]),
        ],
        "the surviving rows are the ones the line scan found outside the tombstone"
    );

    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
        return;
    }
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("xref-stm-free-row-then-failure.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let qpdf = Command::new("qpdf")
        .args(["--warning-exit-0", "--show-xref"])
        .arg(&input)
        .output()
        .expect("qpdf should spawn");
    let stderr = String::from_utf8_lossy(&qpdf.stderr);
    assert!(
        qpdf.status.success(),
        "qpdf --show-xref must render the reconstructed table: {stderr}"
    );
    assert!(
        stderr.contains("unknown xref stream entry type 3"),
        "the fixture must actually fail the /XRefStm read after the free row: {stderr}"
    );
    let shown: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
        .lines()
        .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
        .map(str::to_owned)
        .collect();
    assert!(
        !shown.is_empty(),
        "qpdf must show a reconstructed table, not an empty one: {stderr}"
    );
    assert_eq!(
        rendered, shown,
        "flpdf's reconstructed xref table must equal qpdf 11.9.0's"
    );
}

/// A classic xref table with a broken keyword (forcing reconstruction) plus
/// one candidate whose "generation" token is 150 digits -- well past qpdf's
/// `readToken(m->file, MAX_LEN)` cap of 100 (`libqpdf/QPDF.cc:548,553,558,559`).
/// qpdf's reconstruction line scan reads that field as a bad token rather
/// than an (enormous) integer, so `t2.isInteger()` is false and the whole
/// candidate is never registered (`QPDF.cc:557-561`).
fn reconstruction_candidate_with_overlong_generation_token() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    for object in [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".as_slice(),
    ] {
        bytes.extend_from_slice(object);
    }
    bytes.extend_from_slice(b"9 ");
    bytes.extend(std::iter::repeat_n(b'9', 150));
    bytes.extend_from_slice(b" obj\n<< /Bogus true >>\nendobj\n");
    let xref = bytes.len();
    bytes.extend_from_slice(
        b"xreff\n0 4\n0000000000 65535 f \ntrailer\n<< /Size 4 /Root 1 0 R >>\n",
    );
    bytes.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    bytes
}

/// `Tokenizer::read_qpdf_token`'s `max_len` slot is genuinely load-bearing
/// for the xref-reconstruction line scan, not just for the classic
/// subsection lookahead the other tests here pin: dropping it (treating
/// `max_len` as unbounded) would let the 150-digit "generation" token parse
/// as one huge integer, register object 9, and diverge from qpdf.
#[test]
fn reconstruction_skips_a_candidate_whose_generation_token_exceeds_max_len_like_qpdf() {
    let fixture = reconstruction_candidate_with_overlong_generation_token();

    let pdf = Pdf::open_with_options(
        Cursor::new(fixture.clone()),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("the reconstructed table keeps the classic trailer, so the open succeeds");
    let table = pdf.get_xref_table();
    assert!(
        !table.contains_key(&ObjectRef::new(9, 0)),
        "an over-length generation token must not register a reconstruction candidate: {:?}",
        render_xref_table(&table)
    );
    let rendered = render_xref_table(&table);
    assert_eq!(
        rendered,
        vec![
            "1/0: uncompressed; offset = 9".to_owned(),
            "2/0: uncompressed; offset = 58".to_owned(),
            "3/0: uncompressed; offset = 115".to_owned(),
        ],
        "only the three well-formed objects survive reconstruction"
    );

    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
        return;
    }
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("overlong-generation-token.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let qpdf = Command::new("qpdf")
        .args(["--warning-exit-0", "--show-xref"])
        .arg(&input)
        .output()
        .expect("qpdf should spawn");
    assert!(
        qpdf.status.success(),
        "qpdf --show-xref must render the reconstructed table: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let shown: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
        .lines()
        .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        rendered, shown,
        "flpdf's reconstructed xref table must equal qpdf 11.9.0's"
    );
}

/// qpdf limits each recovery token to 100 bytes, but its tokenizer skips
/// whitespace between the object number, generation and `obj` keyword without
/// counting those separators (`QPDF.cc:547-559`, `QPDFTokenizer.cc:921-953`).
fn recovery_header_with_unbounded_separator_whitespace() -> (Vec<u8>, usize, usize) {
    const SEPARATOR_WHITESPACE: usize = 16_384;
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1\n");
    bytes.extend(std::iter::repeat_n(b' ', SEPARATOR_WHITESPACE));
    bytes.push(b'%');
    bytes.extend(std::iter::repeat_n(b'x', 400));
    bytes.push(b'\n');
    bytes.extend_from_slice(b"0\n");
    bytes.extend(std::iter::repeat_n(b' ', SEPARATOR_WHITESPACE));
    bytes.push(b'%');
    bytes.extend(std::iter::repeat_n(b'x', 400));
    bytes.push(b'\n');
    bytes.extend_from_slice(b"obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    bytes.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n");

    (bytes, catalog_offset, pages_offset)
}

#[test]
fn reconstruction_header_lookahead_skips_unbounded_separator_whitespace() {
    let (fixture, catalog_offset, pages_offset) =
        recovery_header_with_unbounded_separator_whitespace();
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("unbounded-header-whitespace.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();

    let mut pdf = Pdf::open_with_options(
        Cursor::new(fixture),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("canonical recovery must accept whitespace between header tokens");

    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));
    pdf.root_handle()
        .expect("recovered /Root must resolve to the Catalog dictionary");
    let rendered = render_xref_table(&pdf.get_xref_table());
    assert_eq!(
        rendered,
        vec![
            format!("1/0: uncompressed; offset = {catalog_offset}"),
            format!("2/0: uncompressed; offset = {pages_offset}"),
        ],
        "the line scan must register the object whose header spans long separators"
    );

    let warnings: Vec<String> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
        .collect();
    let expected_warnings = vec![
        format!("{description}: file is damaged"),
        format!("{description}: can't find startxref"),
        format!("{description}: Attempting to reconstruct cross-reference table"),
    ];
    assert_eq!(warnings, expected_warnings);

    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
        return;
    }
    let qpdf = Command::new("qpdf")
        .arg("--show-xref")
        .arg(&input)
        .output()
        .expect("qpdf should spawn");
    assert_eq!(
        qpdf.status.code(),
        Some(3),
        "qpdf reports reconstructed damaged files with exit 3"
    );
    let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("WARNING: "))
        .map(str::to_owned)
        .collect();
    assert_eq!(qpdf_warnings, expected_warnings);
    let qpdf_rows: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
        .lines()
        .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
        .map(str::to_owned)
        .collect();
    assert_eq!(rendered, qpdf_rows);
}

/// Build a valid classic-xref file whose `%PDF-` candidate starts at the last
/// four-byte position in qpdf's first-1024-byte header search window. qpdf
/// tests the match from its candidate position, then reads the header line
/// from that same live source position (`QPDF.cc:388-406,430-437`).
fn pdf_with_header_at_search_boundary() -> (Vec<u8>, usize, usize) {
    const HEADER_OFFSET: usize = 1020;
    // qpdf rejects this first candidate's version, then keeps searching and
    // accepts the valid candidate whose marker straddles the 1024-byte block.
    let mut bytes = b"%PDF-x\n".to_vec();
    bytes.resize(HEADER_OFFSET, b'x');
    bytes.extend_from_slice(b"%PDF-1.7\r\n");

    let catalog_offset = bytes.len() - HEADER_OFFSET;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len() - HEADER_OFFSET;
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref_offset = bytes.len() - HEADER_OFFSET;

    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    (bytes, catalog_offset, pages_offset)
}

#[test]
fn canonical_header_search_reads_the_full_candidate_line_at_the_window_edge() {
    let (fixture, catalog_offset, pages_offset) = pdf_with_header_at_search_boundary();
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("header-at-search-boundary.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();

    let qpdf = Command::new("qpdf")
        .arg("--show-xref")
        .arg(&input)
        .output()
        .expect("qpdf 11.9.0 should spawn");
    assert_eq!(
        qpdf.status.code(),
        Some(0),
        "qpdf must accept a header beginning at byte 1020: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("WARNING: "))
        .map(str::to_owned)
        .collect();
    assert!(
        qpdf_warnings.is_empty(),
        "valid prefixed PDF: {qpdf_warnings:?}"
    );
    let qpdf_rows: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
        .lines()
        .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
        .map(str::to_owned)
        .collect();

    let mut pdf = Pdf::open_with_options(
        Cursor::new(fixture),
        PdfOpenOptions {
            repair: false,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("canonical open must find the complete header past the search-window edge");
    assert_eq!(pdf.version(), "1.7");
    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));
    pdf.root_handle()
        .expect("the rebased /Root must resolve to the Catalog dictionary");
    let rendered = render_xref_table(&pdf.get_xref_table());
    assert!(rendered.contains(&format!("1/0: uncompressed; offset = {catalog_offset}")));
    assert!(rendered.contains(&format!("2/0: uncompressed; offset = {pages_offset}")));
    assert_eq!(rendered, qpdf_rows);
    assert!(pdf.repair_diagnostics().entries().is_empty());
}

fn pdf_with_header_outside_search_window() -> (Vec<u8>, usize, usize) {
    const HEADER_OFFSET: usize = 1024;
    let mut bytes = vec![b'x'; HEADER_OFFSET];
    bytes.extend_from_slice(b"%PDF-1.7\n");

    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref_offset = bytes.len();

    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    (bytes, catalog_offset, pages_offset)
}

#[test]
fn canonical_header_search_rejects_a_candidate_starting_at_byte_1024() {
    let (fixture, catalog_offset, pages_offset) = pdf_with_header_outside_search_window();
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("header-outside-search-window.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();

    let qpdf = Command::new("qpdf")
        .args(["--warning-exit-0", "--show-xref"])
        .arg(&input)
        .output()
        .expect("qpdf 11.9.0 should spawn");
    assert!(
        qpdf.status.success(),
        "qpdf should continue after warning about the out-of-window header: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("WARNING: "))
        .map(str::to_owned)
        .collect();
    assert!(qpdf_warnings
        .iter()
        .any(|warning| { warning == &format!("{description}: can't find PDF header") }));
    let qpdf_rows: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
        .lines()
        .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
        .map(str::to_owned)
        .collect();

    let mut pdf = Pdf::open_with_options(
        Cursor::new(fixture),
        PdfOpenOptions {
            repair: false,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("a valid xref remains readable after qpdf's missing-header warning");
    assert_eq!(pdf.version(), "1.2");
    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));
    pdf.root_handle()
        .expect("the physical /Root must resolve when the header is out of range");
    let warnings: Vec<String> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
        .collect();
    assert_eq!(warnings, qpdf_warnings);

    let rendered = render_xref_table(&pdf.get_xref_table());
    assert!(rendered.contains(&format!("1/0: uncompressed; offset = {catalog_offset}")));
    assert!(rendered.contains(&format!("2/0: uncompressed; offset = {pages_offset}")));
    assert_eq!(rendered, qpdf_rows);
}

fn pdf_with_a_long_header_line() -> (Vec<u8>, usize, usize) {
    const COMMENT_BYTES: usize = 12 * 1024;
    let mut bytes = b"%PDF-1.7".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', COMMENT_BYTES));

    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref_offset = bytes.len();

    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    (bytes, catalog_offset, pages_offset)
}

#[test]
fn canonical_header_reader_skips_a_line_longer_than_10k_like_qpdf() {
    let (fixture, catalog_offset, pages_offset) = pdf_with_a_long_header_line();
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("long-pdf-header-line.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();

    let qpdf = Command::new("qpdf")
        .arg("--show-xref")
        .arg(&input)
        .output()
        .expect("qpdf 11.9.0 should spawn");
    assert_eq!(
        qpdf.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let qpdf_rows: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
        .lines()
        .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
        .map(str::to_owned)
        .collect();

    let mut pdf = Pdf::open_with_options(
        Cursor::new(fixture),
        PdfOpenOptions {
            repair: false,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("the live header reader must scan to the line ending");
    assert_eq!(pdf.version(), "1.7");
    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));
    pdf.root_handle().expect("the /Root remains resolvable");
    let rendered = render_xref_table(&pdf.get_xref_table());
    assert!(rendered.contains(&format!("1/0: uncompressed; offset = {catalog_offset}")));
    assert!(rendered.contains(&format!("2/0: uncompressed; offset = {pages_offset}")));
    assert_eq!(rendered, qpdf_rows);
    assert!(pdf.repair_diagnostics().entries().is_empty());
}

#[test]
fn canonical_header_reader_consumes_eol_and_eof_like_qpdf() {
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    for (name, fixture) in [
        ("header-line-crlf-at-eof.pdf", b"%PDF-1.7\r\n".as_slice()),
        (
            "header-line-unterminated-at-eof.pdf",
            b"%PDF-1.7".as_slice(),
        ),
    ] {
        let input = directory.path().join(name);
        fs::write(&input, fixture).expect("write qpdf fixture");
        let description = input.to_string_lossy().into_owned();

        let qpdf = Command::new("qpdf")
            .arg("--show-xref")
            .arg(&input)
            .output()
            .expect("qpdf 11.9.0 should spawn");
        assert_eq!(qpdf.status.code(), Some(2));
        let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
            .lines()
            .filter_map(|line| line.strip_prefix("WARNING: "))
            .map(str::to_owned)
            .collect();

        let result = Pdf::open_with_options(
            Cursor::new(fixture),
            PdfOpenOptions {
                repair: true,
                suppress_warnings: true,
                description: description.as_bytes().to_vec(),
                ..PdfOpenOptions::default()
            },
        );
        let error = match result {
            Ok(_) => panic!("qpdf cannot recover a header-only source"),
            Err(error) => error,
        };
        let (_, diagnostics) = error
            .open_failure()
            .expect("recovery failure must retain qpdf's preceding warnings");
        let warnings: Vec<String> = diagnostics
            .entries()
            .iter()
            .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
            .collect();
        assert_eq!(warnings, qpdf_warnings);
        assert!(qpdf_warnings
            .iter()
            .all(|warning| !warning.contains("can't find PDF header")));
        assert!(String::from_utf8_lossy(&qpdf.stderr)
            .contains("unable to find trailer dictionary while recovering damaged file"));
        assert!(error
            .to_string()
            .contains("unable to find trailer dictionary while recovering damaged file"));
    }
}

#[test]
fn canonical_header_search_handles_a_source_shorter_than_the_marker_like_qpdf() {
    let fixture = b"x".to_vec();
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("short-header-search-input.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();

    let qpdf = Command::new("qpdf")
        .arg("--show-xref")
        .arg(&input)
        .output()
        .expect("qpdf 11.9.0 should spawn");
    assert_eq!(qpdf.status.code(), Some(2));
    let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("WARNING: "))
        .map(str::to_owned)
        .collect();
    let warnings = canonical_open_warnings(fixture, &description);
    assert_eq!(warnings, qpdf_warnings);
    assert!(qpdf_warnings
        .iter()
        .any(|warning| warning == &format!("{description}: can't find PDF header")));
}

#[test]
fn canonical_header_search_rejects_an_incomplete_marker_at_eof_like_qpdf() {
    let mut fixture = vec![b'x'; 1019];
    fixture.extend_from_slice(b"%PDF");
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("incomplete-header-marker.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();

    let qpdf = Command::new("qpdf")
        .arg("--show-xref")
        .arg(&input)
        .output()
        .expect("qpdf 11.9.0 should spawn");
    assert_eq!(qpdf.status.code(), Some(2));
    let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("WARNING: "))
        .map(str::to_owned)
        .collect();
    let warnings = canonical_open_warnings(fixture, &description);
    assert_eq!(warnings, qpdf_warnings);
    assert!(qpdf_warnings
        .iter()
        .any(|warning| warning == &format!("{description}: can't find PDF header")));
}

struct HeaderReadFailure {
    cursor: Cursor<Vec<u8>>,
}

impl std::io::Read for HeaderReadFailure {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        Err(std::io::Error::other("synthetic header read failure"))
    }
}

impl std::io::Seek for HeaderReadFailure {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.cursor, position)
    }
}

#[test]
fn initial_header_read_error_retains_qpdf_system_exception_context() {
    let result = Pdf::open_with_options(
        HeaderReadFailure {
            cursor: Cursor::new(Vec::new()),
        },
        PdfOpenOptions {
            description: b"header-read.pdf".to_vec(),
            repair: false,
            ..PdfOpenOptions::default()
        },
    );
    let error = match result {
        Ok(_) => panic!("the source read failure must propagate"),
        Err(error) => error,
    };
    let flpdf::Error::QpdfExc(error) = error else {
        panic!("FileInputSource read failures are QPDFExc system errors: {error:?}");
    };
    assert_eq!(error.get_error_code(), flpdf::QpdfErrorCode::System);
    assert_eq!(error.get_filename(), b"header-read.pdf");
    assert_eq!(error.get_object(), b"");
    assert_eq!(error.get_file_position(), 0);
    assert_eq!(error.get_message_detail(), b"read 1024 bytes");
}

/// qpdf's `QPDF::readTrailer` parses the trailer from the live source with no
/// 64 KiB window (`QPDF.cc:565-570,1312-1328`).
fn recovery_trailer_with_large_padding() -> (Vec<u8>, usize, usize) {
    const PADDING_LENGTH: usize = 66_000;
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    bytes.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R /Padding (");
    bytes.extend(std::iter::repeat_n(b'x', PADDING_LENGTH));
    bytes.extend_from_slice(b") >>\nstream\nstartxref\n0\n%%EOF\n");
    (bytes, catalog_offset, pages_offset)
}

#[test]
fn reconstruction_trailer_parser_reads_beyond_the_64k_window() {
    let (fixture, catalog_offset, pages_offset) = recovery_trailer_with_large_padding();
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("large-recovery-trailer.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();
    let stream_offset = fixture
        .windows(b"stream".len())
        .position(|window| window == b"stream")
        .expect("fixture has the post-trailer stream token")
        + b"stream".len();
    let expected_warnings = vec![
        format!("{description}: file is damaged"),
        format!("{description}: can't find startxref"),
        format!("{description}: Attempting to reconstruct cross-reference table"),
        format!("{description} (trailer, offset {stream_offset}): stream keyword found in trailer"),
    ];

    let oracle_rows = if qpdf_available() {
        let qpdf = Command::new("qpdf")
            .arg("--show-xref")
            .arg(&input)
            .output()
            .expect("qpdf should spawn");
        assert_eq!(
            qpdf.status.code(),
            Some(3),
            "qpdf accepts the long trailer and reports recovery warnings"
        );
        let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
            .lines()
            .filter_map(|line| line.strip_prefix("WARNING: "))
            .map(str::to_owned)
            .collect();
        assert_eq!(qpdf_warnings, expected_warnings);
        Some(
            String::from_utf8_lossy(&qpdf.stdout)
                .lines()
                .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        )
    } else {
        eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
        None
    };

    let mut pdf = Pdf::open_with_options(
        Cursor::new(fixture),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("canonical reconstruction must parse trailer strings beyond 64 KiB");

    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));
    pdf.root_handle()
        .expect("recovered /Root must resolve to the Catalog dictionary");
    let rendered = render_xref_table(&pdf.get_xref_table());
    assert_eq!(
        rendered,
        vec![
            format!("1/0: uncompressed; offset = {catalog_offset}"),
            format!("2/0: uncompressed; offset = {pages_offset}"),
        ],
        "the long trailer must not prevent reconstructed object rows"
    );
    assert_eq!(
        pdf.repair_diagnostics()
            .entries()
            .iter()
            .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
            .collect::<Vec<_>>(),
        expected_warnings
    );
    if let Some(oracle_rows) = oracle_rows {
        assert_eq!(rendered, oracle_rows);
    }
}

#[test]
fn reconstruction_live_trailer_parser_preserves_token_warning_offsets() {
    let fixture = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\ntrailer\n<< /Size 3 /Root 1 0 R /Invalid <g> >>\nstartxref\n0\n%%EOF\n".to_vec();
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join("invalid-trailer-hex.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();
    let expected_warnings = vec![
        format!("{description}: file is damaged"),
        format!("{description}: can't find startxref"),
        format!("{description}: Attempting to reconstruct cross-reference table"),
        format!("{description} (trailer, offset 150): invalid character (g) in hexstring"),
        format!("{description} (trailer, offset 152): unexpected >"),
        format!("{description} (trailer, offset 120): expected dictionary key but found non-name object; inserting key /QPDFFake1"),
    ];

    let qpdf_rows = if qpdf_available() {
        let qpdf = Command::new("qpdf")
            .arg("--show-xref")
            .arg(&input)
            .output()
            .expect("qpdf should spawn");
        assert_eq!(qpdf.status.code(), Some(3));
        let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
            .lines()
            .filter_map(|line| line.strip_prefix("WARNING: "))
            .map(str::to_owned)
            .collect();
        assert_eq!(qpdf_warnings, expected_warnings);
        Some(
            String::from_utf8_lossy(&qpdf.stdout)
                .lines()
                .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        )
    } else {
        eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
        None
    };

    let mut pdf = Pdf::open_with_options(
        Cursor::new(fixture),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("canonical recovery must keep parser warnings from a trailer candidate");
    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));
    pdf.root_handle()
        .expect("recovered /Root must resolve to the Catalog dictionary");
    let flpdf_warnings: Vec<String> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
        .collect();
    assert_eq!(flpdf_warnings, expected_warnings);

    let rendered = render_xref_table(&pdf.get_xref_table());
    assert_eq!(
        rendered,
        vec![
            "1/0: uncompressed; offset = 9".to_owned(),
            "2/0: uncompressed; offset = 58".to_owned(),
        ]
    );
    if let Some(qpdf_rows) = qpdf_rows {
        assert_eq!(rendered, qpdf_rows);
    }
}

#[test]
fn reconstruction_live_trailer_parser_skips_non_dictionary_candidates() {
    let mut fixture = b"%PDF-1.4\n".to_vec();
    let catalog_offset = fixture.len();
    fixture.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = fixture.len();
    fixture.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    fixture.extend_from_slice(
        b"trailer\n42\ntrailer\nendobj\ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n",
    );
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory
        .path()
        .join("non-dictionary-trailer-candidates.pdf");
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();

    let oracle = if qpdf_available() {
        let qpdf = Command::new("qpdf")
            .arg("--show-xref")
            .arg(&input)
            .output()
            .expect("qpdf should spawn");
        assert_eq!(qpdf.status.code(), Some(3));
        let warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
            .lines()
            .filter_map(|line| line.strip_prefix("WARNING: "))
            .map(str::to_owned)
            .collect();
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("empty object treated as null")));
        let rows: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
            .lines()
            .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
            .map(str::to_owned)
            .collect();
        Some((warnings, rows))
    } else {
        eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
        None
    };

    let mut pdf = Pdf::open_with_options(
        Cursor::new(fixture),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("recovery must continue past non-dictionary trailer candidates");
    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));
    pdf.root_handle()
        .expect("the last dictionary trailer candidate must provide /Root");
    let warnings: Vec<String> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
        .collect();
    let rendered = render_xref_table(&pdf.get_xref_table());
    assert_eq!(
        rendered,
        vec![
            format!("1/0: uncompressed; offset = {catalog_offset}"),
            format!("2/0: uncompressed; offset = {pages_offset}"),
        ]
    );
    if let Some((oracle_warnings, oracle_rows)) = oracle {
        assert_eq!(warnings, oracle_warnings);
        assert_eq!(rendered, oracle_rows);
    }
}

/// Build a single-page document whose cross-reference section is a classic
/// table, inserting `between` after the last subsection entry and writing
/// `startxref_value` (default: the table offset) after the `startxref`
/// keyword.
///
/// `between` lands exactly where qpdf's subsection loop performs its
/// `readToken(m->file).isWord("trailer")` lookahead (`libqpdf/QPDF.cc:886-891`)
/// and `startxref_value` lands exactly where `QPDF::findStartxref` reads its
/// second token (`libqpdf/QPDF.cc:413-421`) -- the two places `QPDF::readToken`
/// governs on the classic route.
fn classic_xref_document(between: &[u8], startxref_value: Option<&[u8]>) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for object in [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".as_slice(),
    ] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(between);
    bytes.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n");
    match startxref_value {
        Some(value) => bytes.extend_from_slice(value),
        None => bytes.extend_from_slice(format!("{xref}\n").as_bytes()),
    }
    bytes.extend_from_slice(b"%%EOF\n");
    bytes
}

#[test]
fn startxref_candidate_search_falls_back_by_qpdf_token_predicate() {
    let base = classic_xref_document(b"", None);
    let xref_offset = base
        .windows(b"xref\n0 4\n".len())
        .position(|window| window == b"xref\n0 4\n")
        .expect("the base fixture has a classic xref table");
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let open = |bytes: Vec<u8>, description: &str| {
        Pdf::open_with_options(
            Cursor::new(bytes),
            PdfOpenOptions {
                repair: true,
                suppress_warnings: true,
                description: description.as_bytes().to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("the canonical loader must use qpdf's startxref candidate predicate")
    };
    // Every case below must resolve to this table without recovery: the
    // `startxref` that qpdf's `findLast` + `findStartxref` accept points at
    // the real section.
    let expected_rows = {
        let pdf = open(base.clone(), "base.pdf");
        assert!(pdf.repair_diagnostics().entries().is_empty());
        render_xref_table(&pdf.get_xref_table())
    };
    // qpdf's findLast searches the byte substring, then applies
    // findStartxref at that byte. It has no separate left-boundary test, so
    // the `startxref` inside `notstartxref` is the last accepted candidate.
    // The earlier, whole-word marker in this fixture points at a bogus
    // offset: an implementation that rejected the embedded substring would
    // fall back to it and have to reconstruct the table.
    let bogus_then_suffix = {
        let mut bytes = classic_xref_document(b"", Some(b"1\n"));
        bytes.extend_from_slice(format!("notstartxref\n{xref_offset}\n").as_bytes());
        bytes
    };
    let cases = [
        (
            "later-noninteger-startxref.pdf",
            [base.as_slice(), b"startxref\nnot-an-integer\n"].concat(),
        ),
        (
            "startxref-with-attached-digits.pdf",
            [base.as_slice(), b"startxref123\n9\n"].concat(),
        ),
        ("startxref-suffix-of-word.pdf", bogus_then_suffix),
    ];

    for (name, fixture) in cases {
        let input = directory.path().join(name);
        fs::write(&input, &fixture).expect("write qpdf fixture");
        let description = input.to_string_lossy().into_owned();

        let pdf = open(fixture, &description);
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "the accepted startxref candidate must not trigger recovery for {name}"
        );
        let rows = render_xref_table(&pdf.get_xref_table());
        assert_eq!(rows, expected_rows, "{name}");

        if !qpdf_available() {
            eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
            continue;
        }
        let qpdf = Command::new("qpdf")
            .arg("--show-xref")
            .arg(&input)
            .output()
            .expect("qpdf should spawn");
        assert_eq!(
            qpdf.status.code(),
            Some(0),
            "qpdf must use the last candidate accepted by findStartxref for {name}"
        );
        let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
            .lines()
            .filter_map(|line| line.strip_prefix("WARNING: "))
            .map(str::to_owned)
            .collect();
        assert!(
            qpdf_warnings.is_empty(),
            "qpdf warnings for {name}: {qpdf_warnings:?}"
        );
        let qpdf_rows: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
            .lines()
            .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
            .map(str::to_owned)
            .collect();
        assert_eq!(rows, qpdf_rows, "{name}");
    }
}

fn classic_xref_document_with_header_prefix(prefix: &[u8], startxref: &[u8]) -> Vec<u8> {
    let mut bytes = prefix.to_vec();
    let header_offset = bytes.len();
    bytes.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    for object in [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".as_slice(),
    ] {
        offsets.push(bytes.len() - header_offset);
        bytes.extend_from_slice(object);
    }
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n");
    bytes.extend_from_slice(startxref);
    bytes.extend_from_slice(b"%%EOF\n");
    bytes
}

#[derive(Clone, Copy)]
enum NegativeStartxrefSourceFailure {
    FileInputSource,
    OffsetInputSource,
}

fn assert_negative_startxref_matches_qpdf(
    filename: &str,
    fixture: Vec<u8>,
    source_failure: NegativeStartxrefSourceFailure,
    offset: &str,
) {
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    let input = directory.path().join(filename);
    fs::write(&input, &fixture).expect("write qpdf fixture");
    let description = input.to_string_lossy().into_owned();
    let error_detail = match source_failure {
        NegativeStartxrefSourceFailure::FileInputSource => {
            format!("seek to {description}, offset {offset} (0): Invalid argument")
        }
        NegativeStartxrefSourceFailure::OffsetInputSource => {
            "offset input source: seek before beginning of file".to_owned()
        }
    };

    let strict_error = match Pdf::open_with_options(
        fs::File::open(&input).expect("open PDF fixture for strict mode"),
        PdfOpenOptions {
            repair: false,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("strict open must attempt qpdf's signed seek for startxref -1"),
        Err(error) => error,
    };
    assert!(strict_error.open_failure().is_none());
    let Error::QpdfExc(qpdf_error) = &strict_error else {
        panic!("qpdf's read_xref seek exception must cross QPDF::parse as DamagedPdf: {strict_error:?}");
    };
    assert_eq!(
        qpdf_error.get_error_code(),
        flpdf::QpdfErrorCode::DamagedPdf
    );
    assert_eq!(qpdf_error.get_filename(), description.as_bytes());
    assert_eq!(qpdf_error.get_object(), b"");
    assert_eq!(qpdf_error.get_file_position(), 0);
    assert_eq!(
        qpdf_error.get_message_detail(),
        format!("error reading xref: {error_detail}").as_bytes()
    );

    let pdf = Pdf::open_with_options(
        fs::File::open(&input).expect("open PDF fixture for repair mode"),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            description: description.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("qpdf repairs after the signed startxref seek fails");
    let flpdf_warnings: Vec<String> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
        .collect();
    assert_eq!(
        flpdf_warnings,
        vec![
            format!("{description}: file is damaged"),
            format!("{description}: error reading xref: {error_detail}"),
            format!("{description}: Attempting to reconstruct cross-reference table"),
        ]
    );

    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
        return;
    }
    let strict_qpdf = Command::new("qpdf")
        .args(["--suppress-recovery", "--check"])
        .arg(&input)
        .output()
        .expect("qpdf should spawn");
    assert_eq!(strict_qpdf.status.code(), Some(2));
    let strict_stderr = String::from_utf8_lossy(&strict_qpdf.stderr);
    assert_eq!(
        strict_stderr.trim(),
        format!("qpdf: {description}: error reading xref: {error_detail}")
    );

    let qpdf = Command::new("qpdf")
        .args(["--warning-exit-0", "--show-xref"])
        .arg(&input)
        .output()
        .expect("qpdf should spawn");
    assert!(qpdf.status.success());
    let qpdf_warnings: Vec<String> = String::from_utf8_lossy(&qpdf.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("WARNING: "))
        .map(str::to_owned)
        .collect();
    assert_eq!(flpdf_warnings, qpdf_warnings);
    let qpdf_rows: Vec<String> = String::from_utf8_lossy(&qpdf.stdout)
        .lines()
        .filter(|line| line.contains(": uncompressed;") || line.contains(": compressed;"))
        .map(str::to_owned)
        .collect();
    assert_eq!(render_xref_table(&pdf.get_xref_table()), qpdf_rows);
}

#[test]
fn negative_startxref_matches_qpdf_file_seek_error() {
    let fixture = classic_xref_document(b"", Some(b"-1\n"));
    assert_negative_startxref_matches_qpdf(
        "negative-startxref.pdf",
        fixture,
        NegativeStartxrefSourceFailure::FileInputSource,
        "-1",
    );
}

#[test]
fn negative_startxref_matches_qpdf_buffer_seek_error() {
    let description = b"memory.pdf";
    let error = match Pdf::open_with_options(
        Cursor::new(classic_xref_document(b"", Some(b"-1\n"))),
        PdfOpenOptions {
            repair: false,
            suppress_warnings: true,
            description: description.to_vec(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("strict memory input must attempt qpdf's signed seek"),
        Err(error) => error,
    };
    assert!(error.open_failure().is_none());
    let Error::QpdfExc(qpdf_error) = error else {
        panic!("qpdf's BufferInputSource seek failure is wrapped by QPDF::parse: {error:?}");
    };
    assert_eq!(
        qpdf_error.get_error_code(),
        flpdf::QpdfErrorCode::DamagedPdf
    );
    assert_eq!(qpdf_error.get_filename(), description);
    assert_eq!(qpdf_error.get_object(), b"");
    assert_eq!(qpdf_error.get_file_position(), 0);
    assert_eq!(
        qpdf_error.get_message_detail(),
        b"error reading xref: memory.pdf: seek before beginning of buffer"
    );
}

#[test]
fn negative_startxref_after_header_offset_matches_qpdf_offset_source_error() {
    let fixture = classic_xref_document_with_header_prefix(b"leading material\n", b"-1\n");
    assert_negative_startxref_matches_qpdf(
        "negative-startxref-after-header.pdf",
        fixture,
        NegativeStartxrefSourceFailure::OffsetInputSource,
        "-1",
    );
}

#[test]
fn minimum_signed_startxref_matches_qpdf_file_seek_error() {
    let offset = "-9223372036854775808";
    let fixture = classic_xref_document(b"", Some(format!("{offset}\n").as_bytes()));
    assert_negative_startxref_matches_qpdf(
        "minimum-signed-startxref.pdf",
        fixture,
        NegativeStartxrefSourceFailure::FileInputSource,
        offset,
    );
}

#[test]
fn startxref_signed_long_overflow_matches_uncaught_qpdf_conversion() {
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    for (index, value) in ["9223372036854775808", "-9223372036854775809"]
        .into_iter()
        .enumerate()
    {
        let fixture = classic_xref_document(b"", Some(format!("{value}\n").as_bytes()));
        let input = directory
            .path()
            .join(format!("startxref-signed-long-overflow-{index}.pdf"));
        fs::write(&input, &fixture).expect("write qpdf fixture");
        let expected =
            format!("overflow/underflow converting {value} to 64-bit integer").into_bytes();

        for repair in [false, true] {
            let error = match Pdf::open_with_options(
                fs::File::open(&input).expect("open PDF fixture"),
                PdfOpenOptions {
                    repair,
                    suppress_warnings: true,
                    description: input.to_string_lossy().as_bytes().to_vec(),
                    ..PdfOpenOptions::default()
                },
            ) {
                Ok(_) => {
                    panic!("QUtil::string_to_ll overflow occurs before qpdf's recovery catch")
                }
                Err(error) => error,
            };
            assert!(error.open_failure().is_none());
            assert_eq!(error.raw_message(), Some(expected.as_slice()));
        }

        if !qpdf_available() {
            eprintln!("qpdf 11.9.0 is not available; skipping only the oracle comparison");
            return;
        }
        for args in [vec!["--suppress-recovery", "--check"], vec!["--check"]] {
            let qpdf = Command::new("qpdf")
                .args(&args)
                .arg(&input)
                .output()
                .expect("qpdf should spawn");
            assert_eq!(qpdf.status.code(), Some(2));
            assert_eq!(
                String::from_utf8_lossy(&qpdf.stderr).trim(),
                format!("qpdf: {}", String::from_utf8_lossy(&expected))
            );
        }
    }
}

/// A classic section that jumps straight from the `xref` keyword to
/// `trailer`. qpdf's `while (!done)` loop always opens by reading a
/// subsection header and only recognises `trailer` through the lookahead that
/// closes a subsection (`libqpdf/QPDF.cc:851-890`), so a table with no
/// subsection is `xref syntax invalid` rather than an empty table.
fn classic_xref_without_subsection() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    for object in [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".as_slice(),
    ] {
        bytes.extend_from_slice(object);
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\ntrailer\n<< /Size 4 /Root 1 0 R >>\n");
    bytes.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    bytes
}

/// `QPDF::readToken` is fixed at `allow_bad = true`
/// (`libqpdf/QPDF.cc:1535-1539`), so on the classic route a `tt_bad` token is
/// a value the caller inspects and rejects, never a raised error. Both places
/// that token governs are pinned here against qpdf 11.9.0: the subsection
/// loop's `trailer` lookahead, which rewinds and re-reads the bytes as the
/// next subsection header, and `findStartxref`, whose non-integer second
/// token degrades to `can't find startxref` rather than surfacing the
/// tokenizer's own diagnostic.
#[test]
fn classic_xref_read_token_lookahead_matches_qpdf() {
    let directory = tempfile::tempdir().expect("create qpdf fixture directory");
    for (name, fixture, expected) in [
        (
            "classic-xref-clean.pdf",
            classic_xref_document(b"", None),
            Vec::new(),
        ),
        // A `)` where the lookahead runs: `tt_bad`, so not the `trailer`
        // keyword, so re-read as the next subsection header, which fails.
        (
            "classic-xref-bad-lookahead-token.pdf",
            classic_xref_document(b")\n", None),
            vec![
                "file is damaged".to_owned(),
                "(xref table, offset 275): xref syntax invalid".to_owned(),
                "Attempting to reconstruct cross-reference table".to_owned(),
            ],
        ),
        // A comment is ignorable to the tokenizer, so the lookahead reads
        // past it and finds `trailer`: the table parses without repair.
        (
            "classic-xref-comment-before-trailer.pdf",
            classic_xref_document(b"%comment\n", None),
            Vec::new(),
        ),
        // The rewind target is the position the lookahead started from, not
        // the first non-space byte, so the reported offset is the whitespace.
        (
            "classic-xref-bad-lookahead-after-space.pdf",
            classic_xref_document(b"\n   )\n", None),
            vec![
                "file is damaged".to_owned(),
                "(xref table, offset 276): xref syntax invalid".to_owned(),
                "Attempting to reconstruct cross-reference table".to_owned(),
            ],
        ),
        (
            "classic-xref-without-subsection.pdf",
            classic_xref_without_subsection(),
            vec![
                "file is damaged".to_owned(),
                "(xref table, offset 191): xref syntax invalid".to_owned(),
                "Attempting to reconstruct cross-reference table".to_owned(),
            ],
        ),
        (
            "classic-xref-bad-startxref-token.pdf",
            classic_xref_document(b"", Some(b")\n")),
            vec![
                "file is damaged".to_owned(),
                "can't find startxref".to_owned(),
                "Attempting to reconstruct cross-reference table".to_owned(),
            ],
        ),
    ] {
        let input = directory.path().join(name);
        fs::write(&input, &fixture).expect("write qpdf fixture");
        let description = input.to_string_lossy().into_owned();
        let expected: Vec<String> = expected
            .into_iter()
            .map(|message| {
                format!(
                    "{description}{}{message}",
                    if message.starts_with('(') { " " } else { ": " }
                )
            })
            .collect();

        assert_eq!(
            canonical_open_warnings(fixture.clone(), &description),
            expected,
            "canonical route warnings changed for {name}"
        );

        // Every fixture resolves to the same three uncompressed entries,
        // whether it was read from the table or reconstructed, so the
        // warning list above is what separates the two outcomes. Pinning the
        // entries as well keeps a fixture that stopped parsing for an
        // unrelated reason from passing as agreement.
        let pdf = Pdf::open_with_options(
            Cursor::new(fixture),
            PdfOpenOptions {
                repair: true,
                suppress_warnings: true,
                description: description.as_bytes().to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("{name} must open: {error}"));
        let table = pdf.get_xref_table();
        for (number, offset) in [(1u32, 9u64), (2, 58), (3, 115)] {
            assert_eq!(
                table.get(&ObjectRef::new(number, 0)).copied(),
                Some(XrefEntry::Uncompressed { offset }),
                "{name} lost object {number}"
            );
        }

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
