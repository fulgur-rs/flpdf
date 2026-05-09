use flpdf::{fonts, outline, pages, ObjectRef, Pdf};
use std::io::Cursor;
use std::io::Write;

#[test]
fn page_refs_returns_pages_in_document_order() {
    let pdf = nested_pages_pdf();
    let mut pdf = Pdf::open(Cursor::new(pdf)).unwrap();
    let pages = pages::page_refs(&mut pdf).unwrap();
    assert_eq!(pages, vec![ObjectRef::new(3, 0), ObjectRef::new(6, 0)]);
}

#[test]
fn page_refs_with_max_depth_rejects_too_deep_trees() {
    let pdf = nested_pages_pdf();
    let mut pdf = Pdf::open(Cursor::new(pdf)).unwrap();
    let error = pages::page_refs_with_max_depth(&mut pdf, 1).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("depth exceeds maximum of 1"),
        "expected depth error, got {message}"
    );
}

#[test]
fn outline_items_returns_titles_in_pre_order() {
    let pdf = pdf_with_metadata_outline_and_fonts();
    let mut pdf = Pdf::open(Cursor::new(pdf)).unwrap();
    let items = outline::outline_items(&mut pdf).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].depth, 0);
    assert_eq!(items[0].title, "Chapter One");
    assert_eq!(items[0].object_ref, ObjectRef::new(10, 0));
}

#[test]
fn outline_items_returns_empty_when_outline_missing() {
    let pdf = nested_pages_pdf();
    let mut pdf = Pdf::open(Cursor::new(pdf)).unwrap();
    let items = outline::outline_items(&mut pdf).unwrap();
    assert!(items.is_empty());
}

#[test]
fn font_entries_collects_indirect_and_named_fonts() {
    let pdf = pdf_with_metadata_outline_and_fonts();
    let mut pdf = Pdf::open(Cursor::new(pdf)).unwrap();
    let fonts = fonts::font_entries(&mut pdf).unwrap();
    assert_eq!(fonts.len(), 2);
    assert!(fonts.contains_key(b"F1".as_slice()));
    assert!(fonts.contains_key(b"F2".as_slice()));
}

#[test]
fn object_ref_parse_accepts_with_and_without_r() {
    assert_eq!(ObjectRef::parse("12 0").unwrap(), ObjectRef::new(12, 0));
    assert_eq!(ObjectRef::parse("12 0 R").unwrap(), ObjectRef::new(12, 0));
    assert_eq!("4 1 R".parse::<ObjectRef>().unwrap(), ObjectRef::new(4, 1));
}

#[test]
fn object_ref_parse_rejects_garbage() {
    assert!(ObjectRef::parse("bad").is_err());
    assert!(ObjectRef::parse("1").is_err());
    assert!(ObjectRef::parse("1 0 X").is_err());
    assert!(ObjectRef::parse("1 -1").is_err());
}

fn nested_pages_pdf() -> Vec<u8> {
    let object1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
    let object2 = b"2 0 obj\n<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] >>\nendobj\n";
    let object3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 842] /Contents 5 0 R >>\nendobj\n";
    let object4 = b"4 0 obj\n<< /Type /Pages /Count 1 /Kids [6 0 R] >>\nendobj\n";
    let object5 = b"5 0 obj\n<< /Length 14 >>\nstream\nBT (one) Tj ET\nendstream\nendobj\n";
    let object6 =
        b"6 0 obj\n<< /Type /Page /Parent 4 0 R /MediaBox [0 0 200 100] /Contents 7 0 R >>\nendobj\n";
    let object7 = b"7 0 obj\n<< /Length 15 >>\nstream\nBT (two) Tj ET\nendstream\nendobj\n";

    finalize_pdf(&[
        object1.to_vec(),
        object2.to_vec(),
        object3.to_vec(),
        object4.to_vec(),
        object5.to_vec(),
        object6.to_vec(),
        object7.to_vec(),
    ])
}

fn pdf_with_metadata_outline_and_fonts() -> Vec<u8> {
    let object1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R /Metadata 4 0 R /Info 5 0 R >>\nendobj\n";
    let object2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [6 0 R] >>\nendobj\n";
    let object3 = b"3 0 obj\n<< /Type /Outlines /First 10 0 R /Last 10 0 R /Count 1 >>\nendobj\n";
    let metadata_data = b"<xmpmeta>fixture</xmpmeta>";
    let object4 = format!(
        "4 0 obj\n<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        metadata_data.len(),
        std::str::from_utf8(metadata_data).unwrap()
    )
    .into_bytes();
    let object5 = b"5 0 obj\n<< /Title (Fixture PDF) /Creator (flpdf) >>\nendobj\n";
    let object6 = b"6 0 obj\n<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R /F2 8 0 R >> >> /MediaBox [0 0 612 792] /Contents 9 0 R >>\nendobj\n";
    let object7 = b"7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>\nendobj\n";
    let object8 = b"8 0 obj\n<< /Type /Font /Subtype /Type0 /BaseFont /Courier >>\nendobj\n";
    let content_data = b"BT /F1 12 Tf (Hello) Tj ET";
    let object9 = format!(
        "9 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        content_data.len(),
        std::str::from_utf8(content_data).unwrap()
    )
    .into_bytes();
    let object10 =
        b"10 0 obj\n<< /Title (Chapter One) /Parent 3 0 R /Dest [6 0 R /Fit] >>\nendobj\n";

    finalize_pdf(&[
        object1.to_vec(),
        object2.to_vec(),
        object3.to_vec(),
        object4,
        object5.to_vec(),
        object6.to_vec(),
        object7.to_vec(),
        object8.to_vec(),
        object9,
        object10.to_vec(),
    ])
}

fn finalize_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    write!(&mut bytes, "xref\n0 {}\n", objects.len() + 1).unwrap();
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for &offset in &offsets {
        writeln!(&mut bytes, "{offset:010} 00000 n ").unwrap();
    }
    write!(
        &mut bytes,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
        objects.len() + 1
    )
    .unwrap();
    bytes
}
