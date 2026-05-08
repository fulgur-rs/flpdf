use flpdf::{Object, ObjectRef, Pdf};
use std::fs::File;
use std::io::BufReader;

#[test]
fn opens_pdf_without_resolving_all_objects() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let pdf = Pdf::open(BufReader::new(file)).unwrap();

    assert_eq!(pdf.version(), "1.7");
    assert_eq!(pdf.resolved_count(), 0);
    assert_eq!(pdf.trailer().get_ref("Root"), Some(ObjectRef::new(1, 0)));
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
