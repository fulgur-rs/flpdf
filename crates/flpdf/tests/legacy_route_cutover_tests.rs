use flpdf::Pdf;
use std::io::Cursor;

#[test]
fn qpdf_named_handle_enumeration_has_no_legacy_alias() {
    let production = include_str!("../src/reader.rs");
    assert!(!production.contains("pub fn get_all_object_handles"));

    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/minimal.pdf").as_slice(),
    ))
    .unwrap();

    assert!(!pdf.get_all_objects().unwrap().is_empty());
}
