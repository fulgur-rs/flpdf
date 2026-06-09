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

/// Build a best-effort fixture whose only `/Type /ObjStm` object carries the
/// given `dict_body` (between `<<` and `>>`) and `stream_payload`. The xref
/// keyword is corrupted so strict parsing fails and best-effort falls into the
/// linear scan, which detects the ObjStm and calls
/// `recover_compressed_offsets_from_objstm`. A plain catalog object (number 1)
/// is included so recovery always yields at least one entry and returns `Ok`.
///
/// Returns the assembled bytes plus the ObjStm object number (5) so callers can
/// assert that NO compressed entry was produced (each error arm of
/// `recover_compressed_offsets_from_objstm` returns early without inserting).
fn objstm_recovery_fixture(dict_body: &str, stream_payload: &[u8]) -> (Vec<u8>, u32) {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let objstm_obj_number: u32 = 5;
    let objstm_obj = format!(
        "{objstm_obj_number} 0 obj\n<< /Type /ObjStm {dict_body} /Length {} >>\nstream\n",
        stream_payload.len()
    )
    .into_bytes();
    bytes.extend_from_slice(&objstm_obj);
    bytes.extend_from_slice(stream_payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let start_xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );
    // Corrupt the `xref` keyword (xref -> xrez) so strict parsing fails.
    let pos = bytes
        .windows(4)
        .rposition(|window| window == b"xref")
        .expect("fixture should contain xref token");
    bytes[pos + 2] = b'z';

    (bytes, objstm_obj_number)
}

/// Assert the fixture recovers via best-effort (Ok) but produced NO compressed
/// entry referencing the ObjStm: every error arm of
/// `recover_compressed_offsets_from_objstm` returns early before inserting.
fn assert_no_compressed_entry(bytes: Vec<u8>, objstm_obj_number: u32) {
    // Strict mode must reject the corrupt xref first.
    load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("corrupt xref should fail in strict mode");

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    // Recovery still produced the catalog as a normal offset entry.
    assert!(
        loaded.entries.contains_key(&ObjectRef::new(1, 0)),
        "recovery should still yield the catalog object"
    );
    // But no compressed entry points at the (malformed) ObjStm.
    assert!(
        !loaded.entries.values().any(|entry| matches!(
            entry,
            XrefOffset::Compressed { stream, .. } if *stream == objstm_obj_number
        )),
        "malformed ObjStm must not yield a compressed entry"
    );
}

/// `recover_compressed_offsets_from_objstm` decode-failure arm: an ObjStm whose
/// `/Filter` cannot be decoded makes `decode_stream_data` return `Err`, so the
/// routine returns early and emits no compressed entry.
#[test]
fn best_effort_objstm_undecodable_filter_yields_no_compressed_entry() {
    // A bogus filter name `apply_single_filter_decode` does not recognize.
    let (bytes, objstm) = objstm_recovery_fixture("/N 1 /First 4 /Filter /NoSuchFilter", b"7 0 x");
    assert_no_compressed_entry(bytes, objstm);
}

/// `recover_compressed_offsets_from_objstm` `/N` parse arm: a negative `/N`
/// makes `parse_non_negative_u64` return `Err`, so the routine returns early.
#[test]
fn best_effort_objstm_negative_n_yields_no_compressed_entry() {
    let (bytes, objstm) = objstm_recovery_fixture("/N -1 /First 4", b"7 0 <</Foo 1>>");
    assert_no_compressed_entry(bytes, objstm);
}

/// `recover_compressed_offsets_from_objstm` object-number `parse_non_negative_i64`
/// arm: a decoded object number that is negative makes the routine return early.
#[test]
fn best_effort_objstm_negative_object_number_yields_no_compressed_entry() {
    // Decoded pairs lead with `-1 0`: the object number is negative.
    let (bytes, objstm) = objstm_recovery_fixture("/N 1 /First 6", b"-1 0 <</Foo 1>>");
    assert_no_compressed_entry(bytes, objstm);
}

/// `recover_compressed_offsets_from_objstm` object-number read arm: the decoded
/// data does not begin with an integer where the object number is expected, so
/// `integer_for_indirect` fails and the routine returns early.
#[test]
fn best_effort_objstm_non_integer_object_number_yields_no_compressed_entry() {
    // `/N 1` but the payload is a name, not the expected leading integer.
    let (bytes, objstm) = objstm_recovery_fixture("/N 1 /First 4", b"/Foo 0 0");
    assert_no_compressed_entry(bytes, objstm);
}

/// `recover_compressed_offsets_from_objstm` `u32::try_from` arm: a decoded
/// object number larger than `u32::MAX` overflows the `u32` conversion, so the
/// routine returns early.
#[test]
fn best_effort_objstm_object_number_overflows_u32_yields_no_compressed_entry() {
    // 5_000_000_000 > u32::MAX (4_294_967_295).
    let (bytes, objstm) = objstm_recovery_fixture("/N 1 /First 16", b"5000000000 0 <<>>");
    assert_no_compressed_entry(bytes, objstm);
}

/// `recover_compressed_offsets_from_objstm` offset `parse_non_negative_i64` arm:
/// a negative intra-stream offset makes the routine return early.
#[test]
fn best_effort_objstm_negative_offset_yields_no_compressed_entry() {
    // Object number 7 is valid, but its offset `-1` is negative.
    let (bytes, objstm) = objstm_recovery_fixture("/N 1 /First 6", b"7 -1 <</Foo 1>>");
    assert_no_compressed_entry(bytes, objstm);
}

/// `recover_compressed_offsets_from_objstm` offset read arm: after a valid
/// object number, the offset slot is not an integer, so `integer_for_indirect`
/// fails and the routine returns early.
#[test]
fn best_effort_objstm_non_integer_offset_yields_no_compressed_entry() {
    // `7` parses as the object number, then `/Bar` is not the expected integer.
    let (bytes, objstm) = objstm_recovery_fixture("/N 1 /First 4", b"7 /Bar 0");
    assert_no_compressed_entry(bytes, objstm);
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

/// A free (`f`) xref-table entry whose 10-digit offset field exceeds
/// `u32::MAX` must be rejected in strict mode. In `parse_xref_table`, a free
/// entry's offset becomes the `Free { next }` value via `u32::try_from(offset)`;
/// when `offset` is `9999999999` (> `u32::MAX`) that conversion fails and the
/// function returns the "free xref next object does not fit u32" error (the
/// `b'f'` arm's `map_err`). Object 0's free entry (generation 65535, `next = 0`)
/// fits and is accepted; the overflow is isolated to the second entry.
#[test]
fn rejects_xref_table_free_next_overflow() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 2\n");
    // Object 0: free-list head, generation 65535, next = 0 (fits u32, accepted).
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    // Object 1: free, offset 9999999999 > u32::MAX -> overflow in the `f` arm.
    bytes.extend_from_slice(b"9999999999 00000 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("free xref next overflowing u32 should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("free xref next object does not fit u32"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// An xref-table entry whose status byte is neither `f` nor `n` must be rejected
/// in strict mode. In `parse_xref_table`, the `in_use` byte is matched against
/// `b'f'` / `b'n'`; any other byte (here `x`) takes the `_ =>` arm and returns
/// the "xref table entry status is not f or n" error.
#[test]
fn rejects_xref_table_bad_entry_status() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 2\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    // Object 1: status byte `x` is neither `f` nor `n` -> `_ =>` arm.
    bytes.extend_from_slice(b"0000000009 00000 x \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("invalid xref entry status should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("xref table entry status is not f or n"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// Well-formed xref-table entries followed by a `trailer` keyword whose value is
/// not a dictionary must be rejected in strict mode. In `parse_xref_table`, once
/// the entry loop completes and the outer loop breaks on `trailer`, the trailer
/// is parsed as an object; when that object is not `Object::Dictionary` the `_ =>`
/// arm returns the "trailer is not a dictionary" error. Here a bare integer `42`
/// follows the keyword.
#[test]
fn rejects_xref_table_trailer_not_dictionary() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    // `trailer` keyword followed by a bare integer instead of a `<<...>>` dict.
    bytes.extend_from_slice(format!("trailer\n42\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("non-dictionary trailer should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("trailer is not a dictionary"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// Build a minimal `%PDF` buffer whose `startxref` points at `xref_offset` and
/// whose `xref_obj` bytes are appended at that offset. Used by the xref-stream
/// error tests below that build a malformed stream object inline (because the
/// shared `make_xref_stream_object` helper hardcodes `/W [1 4 2]` and
/// `/Index [0 size]`, which several of these tests need to vary).
fn pdf_with_xref_object(xref_obj: &[u8]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(xref_obj);
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

/// `parse_xref_stream`: when `startxref` points at an indirect object that
/// parses as a plain dictionary rather than a `stream`, the non-`Object::Stream`
/// arm returns `Error::Unsupported("xref stream expected an indirect object
/// stream")`.
#[test]
fn rejects_xref_stream_non_stream_object() {
    // A dictionary indirect object (no `stream`/`endstream`) at the xref offset.
    let xref_obj = b"3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R >>\nendobj\n";
    let bytes = pdf_with_xref_object(xref_obj);

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("non-stream xref object should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("xref stream expected an indirect object stream"),
        "got {message}"
    );
    assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
}

/// `parse_xref_widths`: a `/W` whose value is not an array (here an integer)
/// takes the non-`Object::Array` arm and returns `Error::Parse("/W must be
/// array")`.
#[test]
fn rejects_xref_stream_w_not_array() {
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W 5 /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/W non-array should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("/W must be array"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_widths`: a `/W` array whose length is not exactly three takes the
/// `values.len() != 3` arm and returns `Error::Parse("/W must contain three
/// integers")`.
#[test]
fn rejects_xref_stream_w_wrong_length() {
    let data = [1u8, 0x0A];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [1 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/W wrong length should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("/W must contain three integers"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_index`: an `/Index` array with an odd number of integers takes
/// the `values.len() % 2 != 0` arm and returns `Error::Parse("/Index must
/// contain an even number of integers")`.
#[test]
fn rejects_xref_stream_index_odd_length() {
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [1 3 1] /Index [0] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/Index odd length should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("/Index must contain an even number of integers"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_index`: an `/Index` whose value is neither absent nor an array
/// (here an integer) takes the `_ =>` arm and returns `Error::Parse("/Index must
/// be array")`.
#[test]
fn rejects_xref_stream_index_not_array() {
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [1 3 1] /Index 5 /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/Index non-array should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("/Index must be array"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_entries`: a `/W [0 0 0]` makes `entry_width == 0`, taking the
/// zero-width guard and returning `Error::Parse("invalid cross-reference stream
/// widths")`.
#[test]
fn rejects_xref_stream_zero_widths() {
    // With all widths zero the decoded stream data is irrelevant; provide none.
    let xref_obj =
        b"3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [0 0 0] /Index [0 1] /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec();

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/W [0 0 0] should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("invalid cross-reference stream widths"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_entries`: when the entry width implied by `/W` requires more
/// bytes than the decoded stream provides, the `cursor.pos + entry_width >
/// len` guard returns `Error::Parse("xref stream data truncated")`. Here `/W
/// [1 3 1]` needs 5 bytes per entry across two declared entries but only one
/// entry's worth of data is present.
#[test]
fn rejects_xref_stream_truncated_data() {
    // /Index declares 2 entries (10 bytes) but only 5 bytes of data are present.
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 2 /Root 1 0 R /W [1 3 1] /Index [0 2] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("truncated xref stream data should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("xref stream data truncated"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_entries`: a type field (`/W[0]`) wide enough to hold a value
/// greater than 255 takes the `u8::try_from` failure arm and returns
/// `Error::Parse("xref stream object type does not fit u8")`. Here `/W [2 1 1]`
/// gives the type field two bytes and the data encodes type value `0x0100`.
#[test]
fn rejects_xref_stream_object_type_overflow() {
    // One entry: type = 0x0100 (256, > u8::MAX), field1 = 0x0A, field2 = 0.
    let data = [0x01u8, 0x00, 0x0A, 0x00];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [2 1 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("xref type > 255 should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("xref stream object type does not fit u8"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_entries`: a type-2 (compressed) entry whose field1 (the
/// containing stream's object number) exceeds `u32::MAX` takes the
/// `u32::try_from(field1)` failure arm and returns `Error::Parse("xref stream
/// object number does not fit u32")`. This needs `w1 >= 5` bytes so field1 can
/// hold a value above `u32::MAX`; `/W [1 5 1]` gives field1 five bytes encoding
/// `0x01_0000_0000` (2^32).
#[test]
fn rejects_xref_stream_type2_stream_number_overflow() {
    // type = 2, field1 = 0x01_00_00_00_00 (2^32, > u32::MAX), field2 = 0.
    let data = [2u8, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [1 5 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("type-2 stream number > u32 should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("xref stream object number does not fit u32"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_entries`: an entry whose `object_type` is 3 (neither free, in-use,
/// nor compressed) takes the `_ =>` arm and returns `Error::Unsupported(
/// "unsupported xref entry type 3")`.
#[test]
fn rejects_xref_stream_unsupported_entry_type() {
    // One entry with type byte 3.
    let data = [3u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [1 3 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("unsupported xref entry type should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("unsupported xref entry type 3"),
        "got {message}"
    );
    assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
}

/// `parse_non_negative_u64` (via the `/Size` lookup in `parse_xref_stream`):
/// a `/Size` that is not an integer (here a name) takes the non-`Object::Integer`
/// arm and returns `Error::Parse("/Size is not integer")`.
#[test]
fn rejects_xref_stream_size_not_integer() {
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size /Big /Root 1 0 R /W [1 3 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/Size non-integer should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("/Size is not integer"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_non_negative_u64` (via the `/Size` lookup in `parse_xref_stream`):
/// a negative `/Size` takes the `*integer < 0` arm and returns
/// `Error::Parse("/Size is negative")`.
#[test]
fn rejects_xref_stream_negative_size() {
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size -1 /Root 1 0 R /W [1 3 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/Size negative should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("/Size is negative"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `build_xref_ranges`: an `/Index` chunk with a zero count (`[0 0 1 1]`) takes
/// the `chunk[1] == 0` skip arm, so that chunk contributes no range. Loading
/// succeeds and only object 1 (from the `[1 1]` chunk) is present.
#[test]
fn xref_stream_index_zero_count_range_skipped() {
    // Only the second chunk `1 1` yields a range: object 1 at offset 0x14.
    let data = [1u8, 0, 0, 0x14, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 2 /Root 1 0 R /W [1 3 1] /Index [0 0 1 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let loaded = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect("zero-count index chunk should be skipped, load should succeed");
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(0x14))
    );
    // Object 0 came only from the skipped zero-count chunk, so it is absent.
    assert_eq!(loaded.entries.get(&ObjectRef::new(0, 0)), None);
}

/// `parse_xref_entries`: a `/W` with `w0 == 0` (`[0 3 1]`) takes the
/// `object_type` default-to-1 arm, so every entry is treated as a type-1
/// in-use entry yielding `XrefOffset::Offset`. Loading succeeds.
#[test]
fn loads_xref_stream_with_w0_zero_defaults_type_one() {
    // w0 == 0: no type byte; field1 = offset (3 bytes), field2 = generation (1).
    let data = [
        0, 0, 0x0A, 0, // object 0 -> offset 0x0A
        0, 0, 0x14, 0, // object 1 -> offset 0x14
    ];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 2 /Root 1 0 R /W [0 3 1] /Index [0 2] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let loaded = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect("w0 == 0 should default to type 1, load should succeed");
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(0, 0)),
        Some(&XrefOffset::Offset(0x0A))
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(0x14))
    );
}

/// `ByteCursor::read_fixed`: an xref table that declares more entries than the
/// file actually contains ends mid-entry, so reading the missing entry's
/// fixed-width offset field hits the `pos + width > len` guard and returns
/// `Error::Parse` with an "unexpected end of" message. The `startxref` keyword
/// is placed BEFORE the xref section (it is located by `rposition`, so its
/// position in the file is irrelevant) so the file can end mid-table with no
/// trailing tokens for the fixed-width reader to mistake for entry fields.
#[test]
fn rejects_xref_table_truncated_entry() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    // Emit `startxref` ahead of the xref section. Pad the offset to a fixed
    // 10-digit width (leading zeros parse fine) so the marker length does not
    // depend on the offset's decimal magnitude, making the offset a simple sum.
    let xref_offset = bytes.len() + "startxref\n0000000000\n%%EOF\n".len();
    bytes.extend_from_slice(format!("startxref\n{xref_offset:010}\n%%EOF\n").as_bytes());
    assert_eq!(
        bytes.len(),
        xref_offset,
        "xref must follow the startxref marker exactly"
    );

    // Declare 2 entries but provide only the first, then end the file: the
    // second entry's 10-digit offset field runs off the end of the buffer.
    bytes.extend_from_slice(b"xref\n0 2\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("truncated xref table entry should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("unexpected end of fixed-width field"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `ByteCursor::read_byte`: an xref table whose final entry supplies the
/// 10-digit offset and 5-digit generation but ends before the in-use status
/// byte drives `read_byte` to the `bytes.get(pos)` `None` arm, returning
/// `Error::Parse("unexpected end of input")`. As in the truncated-entry test,
/// `startxref` is placed before the xref section so the file can end mid-entry.
#[test]
fn rejects_xref_table_truncated_status_byte() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_offset = bytes.len() + "startxref\n0000000000\n%%EOF\n".len();
    bytes.extend_from_slice(format!("startxref\n{xref_offset:010}\n%%EOF\n").as_bytes());
    assert_eq!(
        bytes.len(),
        xref_offset,
        "xref must follow the startxref marker exactly"
    );

    // One declared entry: offset + generation present, but the file ends before
    // the status byte, so `read_byte` exhausts the buffer.
    bytes.extend_from_slice(b"xref\n0 1\n");
    bytes.extend_from_slice(b"0000000000 65535");

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("xref entry missing status byte should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("unexpected end of input"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `ByteCursor::read_unsigned` (via `read_u32` for the subsection count in
/// `parse_xref_table`): a subsection header that supplies the start object
/// number but no count integer makes `read_unsigned` find no digits at the
/// `trailer` keyword, taking the `start == pos` arm and returning
/// `Error::Parse("expected unsigned integer")`.
#[test]
fn rejects_xref_table_missing_object_count() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_offset = bytes.len();
    // Subsection header `0` with no count integer; `trailer` follows directly,
    // so reading the count finds no digits.
    bytes.extend_from_slice(b"xref\n0\ntrailer\n<< /Size 1 /Root 1 0 R >>\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("missing xref subsection count should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("expected unsigned integer"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

// The "succeeded but with accumulated parse errors" warning path in
// `load_xref_and_trailer_with_repair` is exercised by
// `with_repair_appends_diagnostic_when_stream_parse_succeeds`.
//
// Unreachable arms via the public API (documented, not tested):
//
// * `ByteCursor::read_be_u64`'s own `pos + width > len` end-of-stream guard is
//   shadowed in the xref-stream path: `parse_xref_entries` checks
//   `cursor.pos + entry_width > len` (full entry width) BEFORE any
//   `read_be_u64` call, and the per-field reads sum to exactly `entry_width`.
//   Truncated stream data therefore surfaces as "xref stream data truncated"
//   (see `rejects_xref_stream_truncated_data`), and `read_be_u64`'s guard is
//   never the one that fires through `load_xref_and_trailer`.
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
//
// * The `/N` `usize::try_from` overflow arm of
//   `recover_compressed_offsets_from_objstm` is likewise unreachable on 64-bit
//   targets: `/N` is parsed as a non-negative `u64`, and `usize::try_from(u64)`
//   cannot overflow when `usize` is 64-bit.
//
// * The `index == 0` true branch of `is_token_boundary` is unreachable via the
//   public API. Its sole caller in `recover_xref_entries` is guarded by
//   `bytes[cursor].is_ascii_digit() && is_token_boundary(cursor, bytes)`.
//   `load_xref_and_trailer_with_repair` calls `parse_header(&bytes)?` before any
//   path that reaches recovery, so a non-`%PDF-` header is a hard failure: by the
//   time `recover_xref_entries` runs, byte 0 is always `%` (not a digit). The
//   `is_ascii_digit` short-circuit therefore never calls `is_token_boundary` at
//   index 0.
