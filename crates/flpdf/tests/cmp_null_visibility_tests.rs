//! Byte-identity coverage for qpdf's null-aware dictionary visibility.

#![cfg(feature = "qpdf-zlib-compat")]

use flpdf::{
    write_pdf_with_options, NewlineBeforeEndstream, Object, ObjectRef, ObjectStreamMode, Pdf,
    WriteOptions,
};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::Path;

fn append_xref_entry(entries: &mut Vec<u8>, kind: u8, field1: u32, field2: u16) {
    entries.push(kind);
    entries.extend_from_slice(&field1.to_be_bytes());
    entries.extend_from_slice(&field2.to_be_bytes());
}

fn single_member_objstm_fixture(member: &[u8], trailer_extras: &str) -> Vec<u8> {
    let mut bytes = b"%PDF-1.5\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Held 5 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let objstm_offset = bytes.len();
    let mut body = b"5 0 ".to_vec();
    body.extend_from_slice(member);
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
            body.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    let mut xref = Vec::new();
    append_xref_entry(&mut xref, 0, 0, u16::MAX);
    append_xref_entry(&mut xref, 1, catalog_offset as u32, 0);
    append_xref_entry(&mut xref, 1, pages_offset as u32, 0);
    append_xref_entry(&mut xref, 0, 0, 0);
    append_xref_entry(&mut xref, 1, objstm_offset as u32, 0);
    append_xref_entry(&mut xref, 2, 4, 0);
    append_xref_entry(&mut xref, 1, xref_offset as u32, 0);
    bytes.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /XRef /Size 7 /Root 1 0 R {trailer_extras}\
             /W [1 4 2] /Length {} >>\nstream\n",
            xref.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

fn preserve_fixture(
    fixture: &[u8],
    configure: impl FnOnce(&mut WriteOptions),
) -> flpdf::Result<Vec<u8>> {
    let mut pdf = Pdf::open(Cursor::new(fixture))?;
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = ObjectStreamMode::Preserve;
    options.static_id = true;
    options.newline_before_endstream = NewlineBeforeEndstream::Never;
    configure(&mut options);
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options)?;
    Ok(out)
}

fn rewrite_mode(fixture: &str, mode: ObjectStreamMode) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = mode;
    options.static_id = true;
    options.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options).unwrap();
    out
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a == b {
        return None;
    }
    let common = a.len().min(b.len());
    for i in 0..common {
        if a[i] != b[i] {
            return Some(i);
        }
    }
    Some(common)
}

fn assert_golden(actual: &[u8], golden_name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(golden_name);
    let expected =
        std::fs::read(&path).unwrap_or_else(|error| panic!("read golden {path:?}: {error}"));
    if let Some(offset) = first_diff(actual, &expected) {
        let start = offset.saturating_sub(16);
        panic!(
            "{golden_name}: not byte-identical to qpdf golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {offset})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[start..(offset + 16).min(actual.len())],
            &expected[start..(offset + 16).min(expected.len())],
        );
    }
}

#[test]
fn disable_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-matrix.pdf", ObjectStreamMode::Disable),
        "null-visible-matrix/disable.pdf",
    );
}

#[test]
fn preserve_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-matrix-objstm.pdf", ObjectStreamMode::Preserve),
        "null-visible-matrix-objstm/preserve.pdf",
    );
}

#[test]
fn disable_null_visibility_cycle_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-cycle.pdf", ObjectStreamMode::Disable),
        "null-visible-cycle/disable.pdf",
    );
}

#[test]
fn preserve_filters_unreachable_sibling_from_source_container() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-mixed.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-mixed/preserve.pdf",
    );
}

#[test]
fn preserve_drops_fully_unreachable_source_container() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-unreachable.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-unreachable/preserve.pdf",
    );
}

#[test]
fn preserve_keeps_single_source_container_over_100_members() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-over-100.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-over-100/preserve.pdf",
    );
}

#[test]
fn preserve_emits_reachable_signature_dictionary_plain() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-signature.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-signature/preserve.pdf",
    );
}

#[test]
fn preserve_keeps_signatures_with_null_fields_compressed_and_hides_fields() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-signature-null-fields.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-signature-null-fields/preserve.pdf",
    );
}

#[test]
fn preserve_empty_qpdf_plan_does_not_repack_signature() {
    let fixture =
        single_member_objstm_fixture(b"<< /Type /Sig /ByteRange [0 1 2 3] /Contents <00> >>", "");
    let output = preserve_fixture(&fixture, |_| {}).unwrap();
    assert!(
        output.windows(b"\nxref\n".len()).any(|w| w == b"\nxref\n")
            && output
                .windows(b"\ntrailer ".len())
                .any(|w| w == b"\ntrailer "),
        "an empty Preserve plan must fall back to a classic xref table and trailer"
    );
    assert!(
        !output
            .windows(b"/Type /XRef".len())
            .any(|w| w == b"/Type /XRef"),
        "an empty Preserve plan must not emit an xref stream"
    );
    let mut reopened = Pdf::open(Cursor::new(output.as_slice())).unwrap();

    assert!(
        reopened
            .object_refs()
            .into_iter()
            .all(|object_ref| !matches!(
                reopened.resolve(object_ref).unwrap(),
                Object::Stream(ref stream)
                    if matches!(
                        stream.dict.get("Type"),
                        Some(Object::Name(name)) if name.as_slice() == b"ObjStm"
                    )
            )),
        "an empty qpdf Preserve plan is authoritative; the legacy planner must not repack /Sig"
    );
    assert!(
        reopened
            .object_refs()
            .into_iter()
            .any(|object_ref| matches!(
                reopened.resolve(object_ref).unwrap(),
                Object::Dictionary(ref dict)
                    if matches!(
                        dict.get("Type"),
                        Some(Object::Name(name)) if name.as_slice() == b"Sig"
                    )
            )),
        "the reachable signature dictionary must be emitted as a plain object"
    );
}

#[test]
fn preserve_fast_path_rejects_conflicting_id_modes() {
    let fixture = single_member_objstm_fixture(b"<< /Kind /Ordinary >>", "");
    let error = preserve_fixture(&fixture, |options| options.deterministic_id = true).unwrap_err();
    assert!(
        matches!(error, flpdf::Error::Unsupported(ref message)
            if message.contains("mutually exclusive")),
        "got {error:?}"
    );
}

#[test]
fn preserve_empty_qpdf_plan_supports_deterministic_id() {
    let fixture =
        single_member_objstm_fixture(b"<< /Type /Sig /ByteRange [0 1 2 3] /Contents <00> >>", "");
    let write = || {
        preserve_fixture(&fixture, |options| {
            options.static_id = false;
            options.deterministic_id = true;
        })
        .unwrap()
    };
    let first = write();
    let second = write();

    assert_eq!(
        first, second,
        "empty-plan Preserve output must honor deterministic ID"
    );
    assert!(
        first.windows(b"\nxref\n".len()).any(|w| w == b"\nxref\n"),
        "deterministic ID must retain the empty-plan classic xref form"
    );
}

#[test]
fn preserve_fast_path_retains_direct_trailer_extras() {
    let fixture = single_member_objstm_fixture(
        b"<< /Kind /Ordinary >>",
        "/Foo << /Held 5 0 R >> /Info << /Producer (direct-info) >> ",
    );
    let output = preserve_fixture(&fixture, |_| {}).unwrap();
    let mut reopened = Pdf::open(Cursor::new(output.as_slice())).unwrap();

    let held = match reopened.trailer().get("Foo") {
        Some(Object::Dictionary(dict)) => match dict.get("Held") {
            Some(Object::Reference(reference)) => *reference,
            other => panic!("direct /Foo /Held must remain an indirect reference, got {other:?}"),
        },
        other => panic!("the direct /Foo trailer dictionary must survive, got {other:?}"),
    };
    assert_ne!(
        held,
        ObjectRef::new(5, 0),
        "nested trailer refs must be rewritten from their source number"
    );
    assert!(
        matches!(
            reopened.resolve(held).unwrap(),
            Object::Dictionary(ref dict)
                if matches!(
                    dict.get("Kind"),
                    Some(Object::Name(name)) if name.as_slice() == b"Ordinary"
                )
        ),
        "the remapped /Foo /Held reference must resolve to the original member object"
    );
    assert!(
        matches!(
            reopened.trailer().get("Info"),
            Some(Object::Dictionary(dict))
                if matches!(
                    dict.get("Producer"),
                    Some(Object::String(value)) if value == b"direct-info"
                )
        ),
        "a direct /Info trailer dictionary must survive"
    );

    let position = |token: &[u8]| {
        output
            .windows(token.len())
            .position(|window| window == token)
            .unwrap_or_else(|| panic!("missing trailer token {:?}", String::from_utf8_lossy(token)))
    };
    let positions = [
        position(b" /Foo "),
        position(b" /Info "),
        position(b" /Root "),
        position(b" /Size "),
        position(b" /ID "),
    ];
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "trimmed trailer entries and generated keys must follow qpdf writeTrailer order"
    );
}

#[test]
fn legacy_preserve_fallback_splits_source_container_over_100_members() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/null-visible-preserve-over-100.pdf");
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();

    // The source xref stream is structural and unreachable from the document
    // graph, so deleting it is a behavior-neutral routing sentinel. It makes
    // `deleted_object_refs` non-empty, bypassing the dedicated plain qpdf
    // Preserve writer and exercising the shared legacy planner without
    // changing the 104 reachable source ObjStm members.
    pdf.delete_object(ObjectRef::new(107, 0));

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = ObjectStreamMode::Preserve;
    options.static_id = true;
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options).unwrap();

    let mut reopened = Pdf::open(Cursor::new(out)).unwrap();
    let mut member_counts = Vec::new();
    for object_ref in reopened.object_refs() {
        if let Object::Stream(stream) = reopened.resolve(object_ref).unwrap() {
            if matches!(
                stream.dict.get("Type"),
                Some(Object::Name(name)) if name.as_slice() == b"ObjStm"
            ) {
                let Some(Object::Integer(count)) = stream.dict.get("N") else {
                    panic!("ObjStm must carry an integer /N");
                };
                member_counts.push(*count);
            }
        }
    }
    member_counts.sort_unstable();
    assert_eq!(
        member_counts,
        vec![4, 100],
        "legacy Preserve must retain its pre-Task-2 100-member cap"
    );
}
