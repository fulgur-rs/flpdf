use flpdf::{ObjectHandle, ObjectRef, Pdf};
use std::collections::BTreeSet;
use std::rc::Rc;

fn stream(subtype: &[u8]) -> ObjectHandle {
    ObjectHandle::stream(
        ObjectHandle::dictionary(vec![(
            b"/Subtype".to_vec(),
            ObjectHandle::name(subtype.to_vec()),
        )]),
        Rc::new(Vec::new()),
    )
}

fn indirect_content_shape_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let bodies = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 6 0 R >>\nendobj\n".to_vec(),
        b"4 0 obj\n<< /Subtype /Form /Length 0 >>\nstream\nendstream\nendobj\n".to_vec(),
        b"5 0 obj\n<< /Subtype /Image /ImageMask true /Length 0 >>\nstream\nendstream\nendobj\n"
            .to_vec(),
        b"6 0 obj\n[4 0 R]\nendobj\n".to_vec(),
    ];
    let mut offsets = Vec::new();
    for body in &bodies {
        offsets.push(pdf.len());
        pdf.extend_from_slice(body);
    }
    let xref_start = pdf.len();
    pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

#[test]
fn form_and_image_classification_matches_qpdf() {
    assert!(stream(b"Form").is_form_xobject().unwrap());
    assert!(!stream(b"Image").is_form_xobject().unwrap());
    assert!(!ObjectHandle::integer(1).is_form_xobject().unwrap());

    let image = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Image".to_vec())),
            (b"/ImageMask".to_vec(), ObjectHandle::boolean(true)),
        ]),
        Rc::new(Vec::new()),
    );
    assert!(image.is_image(false).unwrap());
    assert!(!image.is_image(true).unwrap());
    assert!(!stream(b"Form").is_image(true).unwrap());
    assert!(!ObjectHandle::integer(1).is_image(true).unwrap());
}

#[test]
fn indirect_form_image_and_page_contents_use_canonical_handles() {
    let mut pdf = Pdf::open_mem_owned(indirect_content_shape_pdf()).unwrap();
    let form = pdf.get_object_handle(ObjectRef::new(4, 0));
    let image = pdf.get_object_handle(ObjectRef::new(5, 0));
    assert!(form.is_form_xobject().unwrap());
    assert!(image.is_image(false).unwrap());
    assert!(!image.is_image(true).unwrap());

    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    let contents = page.get_page_contents().unwrap();
    assert_eq!(contents.len(), 1);
    assert!(contents[0].is_same_object_as(&form));
}

#[test]
fn unique_resource_name_uses_the_supplied_prefix_and_suffix_cursor() {
    let resources = ObjectHandle::dictionary(vec![(
        b"/Font".to_vec(),
        ObjectHandle::dictionary(vec![
            (b"/F0".to_vec(), ObjectHandle::integer(1)),
            (b"/F2".to_vec(), ObjectHandle::integer(2)),
        ]),
    )]);
    let mut min_suffix = 0;

    assert_eq!(
        resources
            .get_unique_resource_name(b"/F", &mut min_suffix, None)
            .unwrap(),
        b"/F1"
    );
    assert_eq!(min_suffix, 1);
}

#[test]
fn page_contents_normalizes_absent_null_single_and_array_shapes() {
    let content_a = stream(b"Form");
    let content_b = stream(b"Form");

    let absent = ObjectHandle::dictionary(Vec::new());
    assert!(absent.get_page_contents().unwrap().is_empty());

    let null = ObjectHandle::dictionary(vec![(b"/Contents".to_vec(), ObjectHandle::null())]);
    assert!(null.get_page_contents().unwrap().is_empty());

    let single = ObjectHandle::dictionary(vec![(b"/Contents".to_vec(), content_a.clone())]);
    let single_contents = single.get_page_contents().unwrap();
    assert_eq!(single_contents.len(), 1);
    assert!(single_contents[0].is_same_object_as(&content_a));

    let array = ObjectHandle::dictionary(vec![(
        b"/Contents".to_vec(),
        ObjectHandle::array(vec![content_a.clone(), content_b.clone()]),
    )]);
    let array_contents = array.get_page_contents().unwrap();
    assert_eq!(array_contents.len(), 2);
    assert!(array_contents[0].is_same_object_as(&content_a));
    assert!(array_contents[1].is_same_object_as(&content_b));
}

#[test]
fn page_contents_rejects_a_contextless_non_stream_array_member_like_qpdf_warning() {
    let page = ObjectHandle::dictionary(vec![(
        b"/Contents".to_vec(),
        ObjectHandle::array(vec![ObjectHandle::integer(1)]),
    )]);

    let error = page
        .get_page_contents()
        .expect_err("qpdf warning has no document context and becomes an error");
    assert!(error.to_string().contains("non-stream"));
}

#[test]
fn page_contents_rejects_a_contextless_non_stream_outer_value_like_qpdf_warning() {
    let page = ObjectHandle::dictionary(vec![(b"/Contents".to_vec(), ObjectHandle::integer(1))]);

    let error = page
        .get_page_contents()
        .expect_err("qpdf warning has no document context and becomes an error");
    assert!(error.to_string().contains("supposed to be a stream"));
}

#[test]
fn unique_resource_name_accepts_a_precomputed_name_set() {
    let resources = ObjectHandle::dictionary(Vec::new());
    let names = BTreeSet::from([b"/Im0".to_vec(), b"/Im1".to_vec()]);
    let mut min_suffix = 0;

    assert_eq!(
        resources
            .get_unique_resource_name(b"/Im", &mut min_suffix, Some(&names))
            .unwrap(),
        b"/Im2"
    );
    assert_eq!(min_suffix, 2);
}

#[test]
fn unique_resource_name_resolves_nested_indirect_resource_dictionaries() {
    let mut pdf = Pdf::open_mem_owned(indirect_content_shape_pdf()).unwrap();
    let font = pdf
        .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
            b"/F0".to_vec(),
            ObjectHandle::integer(1),
        )]))
        .unwrap();
    let resources = ObjectHandle::dictionary(vec![(b"/Font".to_vec(), font)]);
    let mut min_suffix = 0;

    assert_eq!(
        resources
            .get_unique_resource_name(b"/F", &mut min_suffix, None)
            .unwrap(),
        b"/F1"
    );
}

#[test]
fn unique_resource_name_on_a_non_dictionary_uses_an_empty_name_set() {
    let mut min_suffix = 0;
    assert_eq!(
        ObjectHandle::integer(1)
            .get_unique_resource_name(b"/F", &mut min_suffix, None)
            .unwrap(),
        b"/F0"
    );
}
