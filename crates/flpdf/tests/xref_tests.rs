use flpdf::{
    load_xref_and_trailer, load_xref_and_trailer_best_effort, load_xref_and_trailer_with_repair,
    Error, ObjectRef, XrefForm, XrefOffset,
};
use std::fs::File;
use std::io::BufReader;
use std::io::Cursor;

#[test]
fn loads_xref_table_and_trailer() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut reader = BufReader::new(file);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(loaded.version, "1.7");
    assert_eq!(loaded.startxref, 110);
    assert_eq!(loaded.last_xref_form, XrefForm::Table);
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(9))
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefOffset::Offset(58))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

#[test]
fn loads_xref_stream_and_trailer() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    bytes.extend_from_slice(&obj1);

    let xref_entries = [0u8, 0, 0, 0, 0, 1, 0, 0, 0x0A, 0, 1, 0, 0, 0x14, 0];

    let xref_stream_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 3 /Root 1 0 R /W [1 3 1] /Index [0 3] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();

    let xref_object_offset = bytes.len();
    bytes.extend_from_slice(&xref_stream_obj);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let startxref = xref_object_offset;
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut reader = std::io::Cursor::new(bytes);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(loaded.version, "1.7");
    assert_eq!(loaded.startxref, u64::try_from(startxref).unwrap());
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(10))
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefOffset::Offset(20))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
    assert_eq!(startxref, loaded.startxref as usize);
    assert_eq!(startxref, xref_object_offset);
}

#[test]
fn loads_xref_stream_without_index_uses_size_range() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    bytes.extend_from_slice(&obj1);

    let xref_entries = [
        0, 0, 0, 0, 0, // object 0 free
        1, 0, 0, 0x0A, 0, // object 1 at offset 10
        1, 0, 0, 0x14, 0, // object 2 at offset 20
    ];

    let xref_stream_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 3 /Root 1 0 R /W [1 3 1] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_obj);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut reader = std::io::Cursor::new(bytes);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(10))
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefOffset::Offset(20))
    );
}

#[test]
fn rejects_xref_stream_when_range_exceeds_size() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    bytes.extend_from_slice(&obj1);

    let xref_entries = [
        0, 0, 0, 0, 0, // object 0 free
        1, 0, 0, 0x0A, 0, // object 1 at offset 10
        1, 0, 0, 0x14, 0, // object 2 at offset 20
    ];

    let xref_stream_obj =
        format!("3 0 obj\n<< /Type /XRef /Size 2 /Root 1 0 R /W [1 3 1] /Index [0 3] /Length {} >>\nstream\n", xref_entries.len()).into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_obj);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut reader = std::io::Cursor::new(bytes);
    let err = load_xref_and_trailer(&mut reader).expect_err("stream range exceeds /Size");
    let message = format!("{err}");
    assert!(message.contains("xref range exceeds /Size"));
    assert!(matches!(err, Error::Parse { .. }));
}

#[test]
fn parses_xref_stream_with_compressed_entries() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    bytes.extend_from_slice(&obj1);

    let xref_entries = [
        0, 0, 0, 0, 0, // object 0 free
        2, 0, 0, 0x02, 0, // object 1 compressed (type 2)
    ];

    let xref_stream_obj =
        format!("3 0 obj\n<< /Type /XRef /Size 2 /Root 1 0 R /W [1 3 1] /Index [0 2] /Length {} >>\nstream\n", xref_entries.len()).into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_obj);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut reader = std::io::Cursor::new(bytes);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Compressed {
            stream: 2,
            index: 0
        })
    );
}

#[test]
fn loads_latest_xref_stream_free_entries_over_previous_live_entries() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n";
    let obj1_offset = bytes.len() as u64;
    bytes.extend_from_slice(obj1);

    let previous_xref_offset = bytes.len() as u64;
    let previous_xref_entries =
        build_encoded_xref_stream_entries(&[(0, 0, 0), (1, obj1_offset, 0), (2, 12, 0), (0, 0, 0)]);

    let previous_xref_object = make_xref_stream_object(2, 4, None, 1, &previous_xref_entries);
    bytes.extend_from_slice(&previous_xref_object);

    let latest_xref_offset = bytes.len() as u64;
    let latest_xref_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 0),
        (1, obj1_offset, 0),
        (0, 0, 0),
        (1, latest_xref_offset, 0),
    ]);

    let latest_xref_object =
        make_xref_stream_object(3, 4, Some(previous_xref_offset), 1, &latest_xref_entries);
    bytes.extend_from_slice(&latest_xref_object);

    bytes.extend_from_slice(format!("startxref\n{latest_xref_offset}\n%%EOF\n").as_bytes());

    let mut reader = Cursor::new(bytes);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefOffset::Free { next: 0 })
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(obj1_offset))
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(3, 0)),
        Some(&XrefOffset::Offset(latest_xref_offset))
    );
}

fn build_encoded_xref_stream_entries(entries: &[(u8, u64, u64)]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(entries.len() * 7);
    for &(entry_type, field1, field2) in entries {
        encoded.push(entry_type);
        encoded.extend_from_slice(&field1.to_be_bytes()[4..]);
        encoded.extend_from_slice(&field2.to_be_bytes()[6..]);
    }
    encoded
}

fn make_xref_stream_object(
    object_number: u32,
    size: u32,
    prev_offset: Option<u64>,
    root_ref_number: u32,
    entries: &[u8],
) -> Vec<u8> {
    let prev = prev_offset
        .map(|offset| format!(" /Prev {offset}"))
        .unwrap_or_default();

    let mut object = format!(
        "{} 0 obj\n<< /Type /XRef /Size {size} /Root {root_ref_number} 0 R /W [1 4 2] /Index [0 {size}] /Length {}{} >>\nstream\n",
        object_number,
        entries.len(),
        prev
    )
    .into_bytes();
    object.extend_from_slice(entries);

    // Keep stream data trivially decodable with no postprocessing.
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

#[test]
fn best_effort_recovers_from_corrupt_xref_data() {
    let bytes = corrupt_xref_pdf();

    let err = load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("corrupt xref should fail in strict mode");
    let message = format!("{err}");
    assert!(!message.is_empty());

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(loaded.version, "1.7");
    assert_eq!(loaded.repair_diagnostics.entries().len(), 1);
    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|entry| entry.message.contains("repaired by linear object scan")));
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(9))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

fn corrupt_xref_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
    let obj2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec();
    let obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R >>\nendobj\n".to_vec();
    let obj4 = b"4 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n".to_vec();

    let mut offsets = Vec::new();
    for object in &[obj1, obj2, obj3, obj4] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f\n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }

    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );

    let mut corrupted = bytes.clone();
    let Some(pos) = corrupted.windows(4).position(|window| window == b"xref") else {
        unreachable!("fixture should contain xref token")
    };
    if let Some(byte) = corrupted.get_mut(pos + 2) {
        *byte = b'z';
    }
    corrupted
}

#[test]
fn rejects_startxref_offset_beyond_eof_without_panic() {
    // Regression test for GitHub issue #304: a `startxref` offset pointing past
    // the end of the file must yield a descriptive parse error, not panic when
    // the xref stream branch slices `bytes[xref_pos..]`.
    let mut bytes = b"%PDF-1.4\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    // Point startxref well beyond the end of the buffer.
    let beyond_eof = bytes.len() + 4096;
    bytes.extend_from_slice(format!("startxref\n{beyond_eof}\n%%EOF\n").as_bytes());

    let mut reader = Cursor::new(bytes);
    let err =
        load_xref_and_trailer(&mut reader).expect_err("startxref past EOF should error, not panic");
    let message = format!("{err}");
    assert!(
        message.contains("xref stream offset is beyond end of file"),
        "expected descriptive offset error, got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

#[test]
fn xref_stream_parse_error_offset_is_absolute() {
    // When `startxref` points to an in-bounds but malformed location, the error
    // from parsing the indirect object must be reported in absolute file
    // coordinates (`xref_pos + relative_offset`), not relative to the sliced
    // tail. Here the tail starts with a non-integer token, so the parse fails at
    // relative offset 0, which must surface as the absolute `garbage_pos`.
    let mut bytes = b"%PDF-1.4\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let garbage_pos = bytes.len();
    bytes.extend_from_slice(b"not-an-indirect-object\n");
    bytes.extend_from_slice(format!("startxref\n{garbage_pos}\n%%EOF\n").as_bytes());

    let mut reader = Cursor::new(bytes);
    let err =
        load_xref_and_trailer(&mut reader).expect_err("malformed xref stream object should error");
    let Error::Parse { offset, .. } = err else {
        panic!("expected Error::Parse, got {err:?}");
    };
    assert_eq!(
        offset, garbage_pos,
        "parse error offset must be absolute (xref_pos + relative)"
    );
}

#[test]
fn rejects_startxref_offset_exactly_at_eof_without_panic() {
    // Boundary companion to the test above: when `startxref` equals the file
    // length exactly, `bytes.get(xref_pos..)` yields an empty slice rather than
    // `None`. That empty tail must still produce the descriptive
    // "beyond end of file" error instead of slipping into a generic parse
    // failure at offset 0.
    let mut bytes = b"%PDF-1.4\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    // Choose a target offset past the current trailer, then pad the file so its
    // total length equals that offset exactly (the empty-slice boundary).
    let target = bytes.len() + 256;
    bytes.extend_from_slice(format!("startxref\n{target}\n%%EOF\n").as_bytes());
    while bytes.len() < target {
        bytes.push(b' ');
    }
    assert_eq!(
        bytes.len(),
        target,
        "file length must equal startxref offset"
    );

    let mut reader = Cursor::new(bytes);
    let err =
        load_xref_and_trailer(&mut reader).expect_err("startxref at EOF should error, not panic");
    let message = format!("{err}");
    assert!(
        message.contains("xref stream offset is beyond end of file"),
        "expected descriptive offset error, got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// Best-effort recovery must detect a `/Type /ObjStm` object stream during the
/// linear scan and emit `XrefOffset::Compressed` entries for the objects it
/// packs (`recover_xref_entries` ObjStm branch + `recover_compressed_offsets_from_objstm`).
///
/// The ObjStm carries no `/Filter`, so `decode_stream_data` is a passthrough and
/// its raw bytes are the cross-reference pairs header `objnum offset ...` that
/// the recovery routine walks. We pack a single compressed object (number 7).
#[test]
fn best_effort_recovers_objstm_compressed_entries() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    // A plain catalog object so the linear scan also yields a normal entry.
    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    bytes.extend_from_slice(&obj1);

    // Object stream object number 5. Its payload begins with the pairs header
    // `7 0` (compressed object 7 at intra-stream offset 0) followed by the
    // object body that lives at `/First`. `recover_compressed_offsets_from_objstm`
    // only reads the leading `/N` pairs, so the body is incidental here.
    let objstm_obj_number: u32 = 5;
    let compressed_obj_number: u32 = 7;
    let objstm_data = b"7 0 <</Foo 1>>".to_vec();
    let objstm_obj = format!(
        "{objstm_obj_number} 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
        objstm_data.len()
    )
    .into_bytes();
    let objstm_offset = bytes.len() as u64;
    bytes.extend_from_slice(&objstm_obj);
    bytes.extend_from_slice(&objstm_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    // A valid xref + trailer, then corrupt the `xref` keyword so strict parsing
    // fails and best-effort falls into the linear-scan recovery path.
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );
    let pos = bytes
        .windows(4)
        .rposition(|window| window == b"xref")
        .expect("fixture should contain xref token");
    bytes[pos + 2] = b'z';

    // Strict mode must reject the corrupt xref.
    load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("corrupt xref should fail in strict mode");

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();

    // The ObjStm object itself recovers as a normal offset entry.
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(objstm_obj_number, 0)),
        Some(&XrefOffset::Offset(objstm_offset))
    );
    // The packed object recovers as a compressed entry pointing at the ObjStm.
    assert_eq!(
        loaded
            .entries
            .get(&ObjectRef::new(compressed_obj_number, 0)),
        Some(&XrefOffset::Compressed {
            stream: objstm_obj_number,
            index: 0,
        })
    );
    assert!(
        loaded
            .entries
            .values()
            .any(|entry| matches!(entry, XrefOffset::Compressed { stream, .. } if *stream == objstm_obj_number)),
        "expected at least one compressed entry referencing the ObjStm"
    );
}

/// When the linear scan finds no indirect objects at all, recovery must fail
/// with the "unable to recover xref entries" error (`recover_xref_entries`
/// empty-map branch).
#[test]
fn best_effort_errors_when_no_objects_to_recover() {
    // Header + corrupt xref + trailer, but zero indirect objects to scan.
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 1 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );

    let err = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect_err("no recoverable objects should fail");
    let message = format!("{err}");
    assert!(
        message.contains("unable to recover xref entries"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// When recovery finds objects but no `trailer` keyword exists, `recover_trailer`
/// must fail with "trailer dictionary not found".
#[test]
fn best_effort_errors_when_trailer_missing() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    // A recoverable indirect object so `recover_xref_entries` succeeds.
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let start_xref = bytes.len();
    // Corrupt xref keyword and rename the `trailer` keyword to `traile_` so the
    // literal marker is absent.
    bytes.extend_from_slice(b"zref\n0 2\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("traile_\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );

    let err = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect_err("missing trailer keyword should fail");
    let message = format!("{err}");
    assert!(
        message.contains("trailer dictionary not found"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// When the `trailer` keyword is present but followed by a non-dictionary token,
/// `recover_trailer` must fail with "trailer dictionary is not a dictionary".
#[test]
fn best_effort_errors_when_trailer_not_dictionary() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    // A recoverable indirect object so `recover_xref_entries` succeeds.
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 2\n0000000000 65535 f \n");
    // `trailer` followed by a bare integer rather than a `<<...>>` dictionary.
    bytes.extend_from_slice(format!("trailer\n42\nstartxref\n{start_xref}\n%%EOF\n").as_bytes());

    let err = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect_err("non-dictionary trailer should fail");
    let message = format!("{err}");
    assert!(
        message.contains("trailer dictionary is not a dictionary"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// When `startxref` is absent, repair pushes a "missing startxref" error and
/// retries `parse_xref_from_start` at offset 0, which fails at the header and
/// pushes a second error. `format_repair_diagnostic` then takes its multi-error
/// (`_ =>`) arm, joining both clauses with "; " into a single diagnostic.
#[test]
fn repair_diagnostic_aggregates_multiple_errors() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    // A recoverable object and a valid trailer so recovery itself succeeds.
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    // Note: NO `startxref` keyword at all.
    bytes.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R >>\n%%EOF\n");

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();

    // A single Diagnostic carries the combined message.
    assert_eq!(loaded.repair_diagnostics.entries().len(), 1);
    let message = &loaded.repair_diagnostics.entries()[0].message;
    assert!(
        message.starts_with("xref parsing failed and was repaired by linear object scan: "),
        "expected multi-error diagnostic prefix, got {message}"
    );
    assert!(
        message.contains("; "),
        "expected joined clauses, got {message}"
    );
    assert!(
        message.contains("missing startxref"),
        "expected first parse error, got {message}"
    );
    // Recovery still produced usable entries and a trailer.
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(9))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

/// When `startxref` is absent but the FIRST indirect object in the file is
/// itself a valid xref stream with no `/Prev`, repair pushes a single "missing
/// startxref" error and resets the retry offset to 0. `parse_xref_from_start`
/// then skips the `%PDF-` header comment and parses that xref stream
/// successfully, so `merge_previous_xref_sections` is a no-op and the
/// accumulated-error warning arm runs (the "succeeded but with parse errors"
/// path), emitting exactly one diagnostic via `format_repair_diagnostic`'s
/// single-error (`1 =>`) form. This is distinct from the linear-scan recovery
/// path: the stream parse keeps `XrefForm::Stream`, whereas a linear scan would
/// force `XrefForm::Table`.
#[test]
fn with_repair_appends_diagnostic_when_stream_parse_succeeds() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    // The xref stream is the FIRST indirect object (object number 1) so that,
    // after `skip_ws` skips the `%PDF-` comment line, `parse_xref_from_start`
    // parses it directly. It carries no `/Prev`.
    let xref_offset = bytes.len() as u64;
    let xref_entries = build_encoded_xref_stream_entries(&[(0, 0, 0), (1, xref_offset, 0)]);
    let xref_object = make_xref_stream_object(1, 2, None, 1, &xref_entries);
    bytes.extend_from_slice(&xref_object);

    // Deliberately NO `startxref` keyword: only an `%%EOF` marker follows.
    bytes.extend_from_slice(b"%%EOF\n");
    assert!(
        !bytes.windows(b"startxref".len()).any(|w| w == b"startxref"),
        "fixture must not contain a startxref keyword"
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();

    // The xref STREAM parse succeeded (not a linear scan, which sets Table).
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);

    // Exactly one diagnostic, built from the single "missing startxref" error.
    assert_eq!(loaded.repair_diagnostics.entries().len(), 1);
    let message = &loaded.repair_diagnostics.entries()[0].message;
    assert!(
        message.contains("missing startxref"),
        "expected the missing-startxref clause, got {message}"
    );
    assert!(
        message.contains("repaired by linear object scan"),
        "expected the repair clause, got {message}"
    );

    // The stream's own entries are present (e.g. object 1 at its offset).
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(xref_offset))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

/// A `/Prev` chain that points back at itself is a circular reference: strict
/// mode must reject it with "xref /Prev is circular", while best-effort must
/// stop following the chain and return `Ok` with the entries seen so far.
#[test]
fn circular_prev_recovers_with_repair_and_rejected_strict() {
    // Build a single valid xref table whose own offset we then feed into its
    // trailer `/Prev`, so the chain revisits the same offset (a 1-node cycle).
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 2\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{obj1_offset:010} 00000 n \n").as_bytes());
    // `/Prev` points back at this same xref section.
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 2 /Root 1 0 R /Prev {xref_offset} >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    // Strict mode rejects the cycle.
    let err = load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("circular /Prev should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("xref /Prev is circular"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");

    // Best-effort stops following the cycle and returns the entries it has.
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(obj1_offset as u64))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

/// A `/Prev` offset pointing at a malformed (non-circular) location makes
/// `merge_previous_xref_sections` error. Strict mode propagates that error;
/// best-effort records it as a diagnostic and falls back to the linear scan.
#[test]
fn merge_failure_falls_back_to_linear_scan() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    // A bogus location that is neither `xref` nor a valid xref stream object.
    let bad_prev = bytes.len();
    bytes.extend_from_slice(b"not-an-xref-section\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 2\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{obj1_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 2 /Root 1 0 R /Prev {bad_prev} >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    // Strict mode propagates the merge-error from `merge_previous_xref_sections`,
    // surfaced as the failure to parse the malformed `/Prev` target as an xref
    // stream object.
    let err = load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("malformed /Prev target should fail strict parse");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
    assert!(format!("{err}").contains("expected integer"), "got {err}");

    // Best-effort records the error and recovers via the linear object scan.
    let loaded = load_xref_and_trailer_with_repair(&mut Cursor::new(bytes), true).unwrap();
    assert!(
        !loaded.repair_diagnostics.entries().is_empty(),
        "expected a repair diagnostic from the merge fallback"
    );
    assert!(
        loaded
            .repair_diagnostics
            .entries()
            .iter()
            .any(|entry| entry.message.contains("repaired by linear object scan")),
        "expected the linear-scan repair diagnostic"
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(obj1_offset as u64))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

// The "succeeded but with accumulated parse errors" warning path in
// `load_xref_and_trailer_with_repair` is exercised by
// `with_repair_appends_diagnostic_when_stream_parse_succeeds`.
//
// Unreachable arms via the public API (documented, not tested):
//
// * The empty-`parse_errors` (`0 =>`) arm of `format_repair_diagnostic`: every
//   call site passes a non-empty `parse_errors`. Each call is either preceded by
//   a push onto `parse_errors`, or guarded by `!parse_errors.is_empty()`, so the
//   slice is never empty at a call site.
//
// * The `startxref` `usize::try_from` overflow arm of
//   `load_xref_and_trailer_with_repair` (both the repair and strict variants) is
//   unreachable on 64-bit targets, where `usize::try_from(u64)` cannot overflow.
//
// * The `/Prev` `usize::try_from` overflow arm of
//   `merge_previous_xref_sections` is unreachable on 64-bit targets, where
//   `usize::try_from(u64)` cannot overflow.
