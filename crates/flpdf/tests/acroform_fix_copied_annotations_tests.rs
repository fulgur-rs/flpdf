use std::collections::BTreeSet;
use std::io::Cursor;

use flpdf::{AcroFormDocumentHelper, ObjectHandle, PageDocumentHelper, Pdf, QpdfObjGen};

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

fn two_page_widget_pdf() -> Vec<u8> {
    classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots 7 0 R >>\nendobj\n",
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots 7 0 R >>\nendobj\n",
        b"5 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (Field) /Rect [0 0 10 10] /P 3 0 R >>\nendobj\n",
        b"6 0 obj\n<< /Fields [5 0 R] >>\nendobj\n",
        b"7 0 obj\n[5 0 R]\nendobj\n",
    ])
}

fn two_page_plain_pdf() -> Vec<u8> {
    classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
    ])
}

fn two_page_pdf_with_source_annots(source_annots: Option<&str>) -> Vec<u8> {
    let annots_entry = source_annots.map_or_else(String::new, |value| format!("/Annots {value} "));
    let source_page = format!(
        "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] {annots_entry}>>\nendobj\n"
    );
    let objects: Vec<Vec<u8>> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n".to_vec(),
        source_page.into_bytes(),
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots 5 0 R >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n[6 0 R]\nendobj\n".to_vec(),
        b"6 0 obj\n<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>\nendobj\n".to_vec(),
        b"7 0 obj\n[]\nendobj\n".to_vec(),
    ];
    let object_slices: Vec<_> = objects.iter().map(Vec::as_slice).collect();
    classic_pdf(&object_slices)
}

fn pages(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Vec<ObjectHandle> {
    PageDocumentHelper::new(pdf)
        .get_all_pages()
        .expect("page tree should resolve")
}

fn page_annotations(page: &ObjectHandle) -> Vec<ObjectHandle> {
    page.try_get_key(b"/Annots")
        .expect("read page /Annots")
        .try_get_array_as_vector()
        .expect("resolve page /Annots array")
}

fn field_object_gens(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Vec<QpdfObjGen> {
    let root = pdf.get_object_handle(pdf.root_ref().expect("document should have a root"));
    let acroform = root
        .try_get_key(b"/AcroForm")
        .expect("read catalog /AcroForm");
    let fields = acroform
        .try_get_key(b"/Fields")
        .expect("read /AcroForm /Fields")
        .try_get_array_as_vector()
        .expect("resolve /Fields array");
    fields.iter().map(ObjectHandle::get_obj_gen).collect()
}

#[test]
fn same_document_fix_copied_annotations_replaces_annots_and_adds_fields() {
    let mut pdf = Pdf::open_mem_owned(two_page_widget_pdf()).expect("open fixture");
    let pages = pages(&mut pdf);
    let source_page = pages[0].clone();
    let destination_page = pages[1].clone();
    let source_annots = source_page
        .try_get_key(b"/Annots")
        .expect("read source /Annots");

    let mut helper = AcroFormDocumentHelper::new(&mut pdf).expect("analyze source form");
    helper
        .fix_copied_annotations(destination_page.clone(), source_page.clone())
        .expect("fix same-document copied annotations");
    drop(helper);

    let destination_annots = destination_page
        .try_get_key(b"/Annots")
        .expect("read destination /Annots");
    assert!(destination_annots.is_direct());
    let copied_widget = page_annotations(&destination_page)
        .into_iter()
        .next()
        .expect("destination should have one copied widget");
    assert_ne!(copied_widget.get_obj_gen(), QpdfObjGen::new(5, 0));
    assert_eq!(
        source_annots.object_ref(),
        Some(flpdf::ObjectRef::new(7, 0)),
        "source page must retain its shared indirect annotation array"
    );
    assert_eq!(
        field_object_gens(&mut pdf),
        vec![QpdfObjGen::new(5, 0), copied_widget.get_obj_gen()],
        "the new field is appended after replacing destination /Annots"
    );
}

#[test]
fn same_document_fix_copied_annotations_can_report_new_field_objgens() {
    let mut pdf = Pdf::open_mem_owned(two_page_widget_pdf()).expect("open fixture");
    let pages = pages(&mut pdf);
    let source_page = pages[0].clone();
    let destination_page = pages[1].clone();
    let sentinel = QpdfObjGen::new(500, 0);
    let mut added_fields = BTreeSet::from([sentinel]);

    let mut helper = AcroFormDocumentHelper::new(&mut pdf).expect("analyze source form");
    helper
        .fix_copied_annotations_with_new_fields(
            destination_page.clone(),
            source_page,
            &mut added_fields,
        )
        .expect("fix same-document copied annotations and report fields");

    let copied_widget = page_annotations(&destination_page)
        .into_iter()
        .next()
        .expect("destination should have one copied widget");
    assert_eq!(added_fields.len(), 2);
    assert!(added_fields.contains(&sentinel));
    assert!(added_fields.contains(&copied_widget.get_obj_gen()));
}

#[test]
fn foreign_fix_copied_annotations_replaces_annots_and_adds_fields() {
    let mut source = Pdf::open_mem_owned(two_page_widget_pdf()).expect("open source fixture");
    let mut destination = Pdf::open_mem_owned(two_page_plain_pdf()).expect("open target fixture");
    let source_page = pages(&mut source)[0].clone();
    let destination_page = pages(&mut destination)[1].clone();

    let mut helper = AcroFormDocumentHelper::new(&mut destination).expect("analyze target form");
    helper
        .fix_copied_annotations_from(destination_page.clone(), source_page.clone(), &mut source)
        .expect("fix foreign copied annotations");
    drop(helper);

    assert!(destination_page
        .try_get_key(b"/Annots")
        .expect("read destination /Annots")
        .is_direct());
    let copied_widget = page_annotations(&destination_page)
        .into_iter()
        .next()
        .expect("destination should have one copied widget");
    assert_eq!(
        field_object_gens(&mut destination),
        vec![copied_widget.get_obj_gen()]
    );
}

#[test]
fn foreign_fix_copied_annotations_can_report_new_field_objgens() {
    let mut source = Pdf::open_mem_owned(two_page_widget_pdf()).expect("open source fixture");
    let mut destination = Pdf::open_mem_owned(two_page_plain_pdf()).expect("open target fixture");
    let source_page = pages(&mut source)[0].clone();
    let destination_page = pages(&mut destination)[1].clone();
    let sentinel = QpdfObjGen::new(500, 0);
    let mut added_fields = BTreeSet::from([sentinel]);

    let mut helper = AcroFormDocumentHelper::new(&mut destination).expect("analyze target form");
    helper
        .fix_copied_annotations_from_with_new_fields(
            destination_page.clone(),
            source_page,
            &mut source,
            &mut added_fields,
        )
        .expect("fix foreign copied annotations and report fields");

    let copied_widget = page_annotations(&destination_page)
        .into_iter()
        .next()
        .expect("destination should have one copied widget");
    assert_eq!(added_fields.len(), 2);
    assert!(added_fields.contains(&sentinel));
    assert!(added_fields.contains(&copied_widget.get_obj_gen()));
}

#[test]
fn foreign_fix_copied_annotations_returns_for_missing_non_array_and_empty_annots() {
    let sentinel = QpdfObjGen::new(500, 0);
    for source_annots in [None, Some("42"), Some("7 0 R")] {
        let mut source = Pdf::open_mem_owned(two_page_pdf_with_source_annots(source_annots))
            .expect("open source fixture");
        let mut destination = Pdf::open_mem_owned(two_page_pdf_with_source_annots(None))
            .expect("open target fixture");
        let source_page = pages(&mut source)[0].clone();
        let destination_page = pages(&mut destination)[1].clone();
        let mut added_fields = BTreeSet::from([sentinel]);

        let mut helper =
            AcroFormDocumentHelper::new(&mut destination).expect("analyze target form");
        helper
            .fix_copied_annotations_from_with_new_fields(
                destination_page.clone(),
                source_page,
                &mut source,
                &mut added_fields,
            )
            .expect("empty or malformed source /Annots returns without mutation");
        drop(helper);

        assert_eq!(added_fields, BTreeSet::from([sentinel]));
        let destination_annots = destination_page
            .try_get_key(b"/Annots")
            .expect("read destination /Annots");
        assert_eq!(
            destination_annots.object_ref(),
            Some(flpdf::ObjectRef::new(5, 0))
        );
        let destination_items = destination_annots
            .try_get_array_as_vector()
            .expect("resolve destination /Annots");
        assert_eq!(destination_items.len(), 1);
        assert_eq!(
            destination_items[0].object_ref(),
            Some(flpdf::ObjectRef::new(6, 0))
        );
    }
}
