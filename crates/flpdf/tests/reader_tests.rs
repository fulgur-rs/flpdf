use flate2::write::ZlibEncoder;
use flate2::Compression;
use flpdf::{parse_object, EncryptedError, Error, Object, ObjectRef, Pdf, PdfOpenOptions};
use std::fs::File;
use std::io::BufReader;
use std::io::Write;

#[test]
fn opens_pdf_without_resolving_all_objects() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let pdf = Pdf::open(BufReader::new(file)).unwrap();

    assert_eq!(pdf.version(), "1.7");
    assert_eq!(pdf.resolved_count(), 0);
    assert_eq!(pdf.trailer().get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

#[test]
fn open_with_options_uses_empty_password_by_default() {
    let file = File::open("../../tests/fixtures/compat/encrypted-r4-three-page.pdf").unwrap();
    let pdf = Pdf::open_with_options(BufReader::new(file), PdfOpenOptions::default()).unwrap();

    assert_eq!(pdf.version(), "1.6");
}

#[test]
fn open_with_options_rejects_wrong_password() {
    let file = File::open("../../tests/fixtures/compat/encrypted-r4-three-page.pdf").unwrap();
    let options = PdfOpenOptions {
        password: b"wrong".to_vec(),
        ..PdfOpenOptions::default()
    };
    let err = match Pdf::open_with_options(BufReader::new(file), options) {
        Ok(_) => panic!("wrong password should be rejected"),
        Err(err) => err,
    };

    assert!(matches!(err, Error::Encrypted(EncryptedError::BadPassword)));
}

#[test]
fn resolves_indirect_object_on_access() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    let root = pdf.resolve(ObjectRef::new(1, 0)).unwrap();
    let Object::Dictionary(dict) = root else {
        panic!("expected catalog dictionary")
    };

    assert_eq!(dict.get_ref("Pages"), Some(ObjectRef::new(2, 0)));
    assert_eq!(pdf.resolved_count(), 1);
}

#[test]
fn missing_reference_resolves_to_null() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    assert_eq!(pdf.resolve(ObjectRef::new(99, 0)).unwrap(), Object::Null);
}

#[test]
fn resolves_compressed_entry_from_xref_stream() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let catalog = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(&catalog);

    let obj3_offset = bytes.len();
    let obj_stream_body = b"2 0 42";
    let obj3 = format!(
        "3 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
        obj_stream_body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(&obj3);
    bytes.extend_from_slice(obj_stream_body);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 3, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj3_offset as u32, 0);

    let xref_stream_object = format!(
        "4 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_object);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).unwrap();
    assert_eq!(
        pdf.resolve(ObjectRef::new(2, 0)).unwrap(),
        Object::Integer(42)
    );
}

#[test]
fn resolves_compressed_entry_with_flate_decode_from_xref_stream() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>| {
        bytes.extend_from_slice(object);
    };

    add_object(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n", &mut bytes);

    let member1 = format!("<< /Type /Packed /Payload ({}) >>", "A".repeat(400),).into_bytes();
    let member2 = format!("<< /Type /Packed /Payload ({}) >>", "B".repeat(420),).into_bytes();

    let (stream_data, first) = encode_flate_objstm(&[(2, &member1[..]), (3, &member2[..])]);
    let obj_stream_offset = bytes.len();
    let obj_stream = format!(
        "4 0 obj\n<< /Type /ObjStm /N 2 /First {} /Length {} /Filter /FlateDecode >>\nstream\n",
        first,
        stream_data.len(),
    )
    .into_bytes();
    bytes.extend_from_slice(&obj_stream);
    bytes.extend_from_slice(&stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 1);
    append_xref_stream_entry(&mut xref_entries, 1, obj_stream_offset as u32, 0);

    let xref_stream_object = format!(
        "5 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 3 1] /Index [0 5] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_object);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).unwrap();
    assert_eq!(
        pdf.resolve(ObjectRef::new(2, 0)).unwrap(),
        parse_object(&member1).unwrap()
    );
    assert_eq!(
        pdf.resolve(ObjectRef::new(3, 0)).unwrap(),
        parse_object(&member2).unwrap()
    );
}

#[test]
fn resolves_compressed_entry_declared_in_extended_object_stream() {
    let mut pdf = Pdf::open(std::io::Cursor::new(objstm_extends_chain_pdf())).unwrap();

    assert_eq!(
        pdf.resolve(ObjectRef::new(2, 0)).unwrap(),
        Object::Integer(42)
    );
    assert_eq!(
        pdf.resolve(ObjectRef::new(3, 0)).unwrap(),
        Object::Integer(99)
    );
}

fn objstm_extends_chain_pdf() -> Vec<u8> {
    decode_hex_fixture(include_str!(
        "../../../tests/fixtures/compat/objstm-extends-chain.pdf.hex"
    ))
}

fn decode_hex_fixture(hex: &str) -> Vec<u8> {
    let digits: Vec<u8> = hex
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    assert!(digits.len().is_multiple_of(2));

    digits
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(pair, 16).unwrap()
        })
        .collect()
}

fn append_u24_be(bytes: &mut Vec<u8>, value: u32) {
    let bytes_u24 = value.to_be_bytes();
    bytes.extend_from_slice(&bytes_u24[1..]);
}

fn encode_flate_objstm(members: &[(u32, &[u8])]) -> (Vec<u8>, usize) {
    let mut header = String::new();
    let mut body = Vec::new();

    for (index, (number, object_data)) in members.iter().enumerate() {
        let offset = body.len();
        header.push_str(&format!("{} {} ", number, offset));
        body.extend_from_slice(object_data);
        if index + 1 < members.len() {
            body.push(b'\n');
        }
    }

    let mut decoded = Vec::new();
    decoded.extend_from_slice(header.as_bytes());
    decoded.extend_from_slice(&body);

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&decoded).unwrap();
    let encoded = encoder.finish().unwrap();

    (encoded, header.len())
}

fn append_xref_stream_entry(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u8) {
    entries.push(entry_type);
    append_u24_be(entries, field1);
    entries.push(field2);
}
