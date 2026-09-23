//! Route contracts for the canonical-owner xref handoff.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use flpdf::{ObjectRef, Pdf, PdfOpenOptions, XrefEntry};

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
