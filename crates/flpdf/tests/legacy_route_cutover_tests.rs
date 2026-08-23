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

#[test]
fn qpdf_named_trailer_surface_returns_a_live_handle() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/minimal.pdf").as_slice(),
    ))
    .unwrap();

    assert!(pdf.trailer().is_direct());
    assert!(pdf.trailer().get_key(b"/Root").is_indirect());
}

#[test]
fn qpdf_named_resolve_surface_resolves_a_handle_in_place() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/minimal.pdf").as_slice(),
    ))
    .unwrap();
    let root = pdf.root_handle().unwrap();

    pdf.resolve(&root).unwrap();
    assert!(root.get_key(b"/Pages").is_indirect());
}

#[test]
fn qpdf_cutover_has_no_public_raw_object_route() {
    let sources = [
        ("lib.rs", include_str!("../src/lib.rs")),
        ("object.rs", include_str!("../src/object.rs")),
        ("pdf.rs", include_str!("../src/pdf.rs")),
        ("reader.rs", include_str!("../src/reader.rs")),
        ("object_handle.rs", include_str!("../src/object_handle.rs")),
    ];
    let forbidden = [
        "pub enum Object",
        "pub fn trailer_dictionary",
        "pub fn resolve_borrowed",
        "pub fn resolve_object(",
        "pub fn resolve(",
        "pub fn resolve_to_terminal(",
        "pub fn resolve_to_terminal_ref(",
        "fn resolve_to_cache(",
        "legacy_materialized_memo",
        "pub(crate) fn lift_object_to_handle",
        "pub fn materialize(&self) -> Result<Object>",
    ];

    for (name, source) in sources {
        for needle in forbidden {
            assert!(
                !source.contains(needle),
                "legacy raw-object route marker {needle:?} remains in {name}"
            );
        }
    }
}
