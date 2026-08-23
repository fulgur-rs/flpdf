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
    // `root_handle()` already resolves its own candidate, so a fresh,
    // still-unresolved handle from `get_object_handle` is what actually
    // exercises `resolve()`'s own effect rather than one it inherits.
    let root_ref = pdf.root_ref().unwrap();
    let root = pdf.get_object_handle(root_ref);
    assert!(
        !root.is_resolved(),
        "a fresh indirect handle starts unresolved"
    );

    pdf.resolve(&root).unwrap();

    assert!(
        root.is_resolved(),
        "resolve() must resolve the handle in place"
    );
    assert!(root.get_key(b"/Pages").is_indirect());
}

#[test]
fn qpdf_cutover_has_no_legacy_handle_aliases() {
    let sources = [("reader.rs", include_str!("../src/reader.rs"))];
    let forbidden = [
        "pub fn resolve_object_handle(",
        "pub fn resolve_object_handle_to_terminal(",
        "pub fn resolve_object_handle_to_terminal_ref(",
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
