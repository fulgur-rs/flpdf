use std::collections::BTreeSet;

use flpdf::{AcroFormDocumentHelper, ObjectRef, PageDocumentHelper, Pdf, QpdfObjGen};

fn classic_pdf(objects: &[&[u8]]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let size = offsets.len() + 1;
    let xref_start = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}

fn two_widget_form_pdf() -> Vec<u8> {
    classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm 7 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots 8 0 R >>\nendobj\n",
        b"4 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (Remove) /Rect [0 0 10 10] /P 3 0 R >>\nendobj\n",
        b"5 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (Keep) /Rect [20 20 30 30] /P 3 0 R >>\nendobj\n",
        b"6 0 obj\nnull\nendobj\n",
        b"7 0 obj\n<< /Fields 9 0 R >>\nendobj\n",
        b"8 0 obj\n[4 0 R 5 0 R]\nendobj\n",
        b"9 0 obj\n[4 0 R 5 0 R]\nendobj\n",
    ])
}

#[test]
fn remove_form_fields_is_public_and_keeps_fields_array_and_cache_in_sync() {
    let mut pdf = Pdf::open_mem_owned(two_widget_form_pdf()).expect("open AcroForm fixture");
    let page = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("read page tree")
        .into_iter()
        .next()
        .expect("fixture page");
    let annotations = page
        .try_get_key(b"/Annots")
        .expect("read page /Annots")
        .try_get_array_as_vector()
        .expect("resolve /Annots");
    assert_eq!(annotations.len(), 2);
    let removed = ObjectRef::new(4, 0);
    let kept = ObjectRef::new(5, 0);
    let root = pdf.get_object_handle(pdf.root_ref().expect("document root"));
    let fields = root
        .try_get_key(b"/AcroForm")
        .expect("read /AcroForm")
        .try_get_key(b"/Fields")
        .expect("read /Fields");
    assert_eq!(fields.object_ref(), Some(ObjectRef::new(9, 0)));

    let mut helper = AcroFormDocumentHelper::new(&mut pdf).expect("analyze form fields");
    assert!(helper
        .get_field_for_annotation(removed)
        .expect("lookup removed field before mutation")
        .is_some());
    assert!(helper
        .get_field_for_annotation(kept)
        .expect("lookup kept field before mutation")
        .is_some());

    helper
        .remove_form_fields(&BTreeSet::from([QpdfObjGen::new(4, 0)]))
        .expect("remove one top-level field");

    assert_eq!(fields.object_ref(), Some(ObjectRef::new(9, 0)));
    let remaining_fields = fields
        .try_get_array_as_vector()
        .expect("resolve live /Fields");
    assert_eq!(remaining_fields.len(), 1);
    assert_eq!(remaining_fields[0].object_ref(), Some(ObjectRef::new(5, 0)));
    assert!(helper
        .get_field_for_annotation(removed)
        .expect("query removed annotation cache")
        .is_none());
    assert!(helper
        .get_field_for_annotation(kept)
        .expect("query retained annotation cache")
        .is_some());
}
