//! Identity projections must survive resolver operations.
//!
//! `ObjectRef` and qpdf's `QPDFObjGen` do not describe the same set of
//! identities. `QPDFObjGen` is qpdf's raw object/generation pair, read verbatim
//! from an object header (`libqpdf/QPDF.cc:1699-1753`), while `ObjectRef` is the
//! narrower surface a `N G R` indirect reference can name — qpdf's parser turns
//! anything with `gen >= 65535` into a null instead
//! (`libqpdf/QPDFParser.cc:157-178`).
//!
//! Converting from `ObjectRef` to the raw identity is therefore lossless, but
//! converting back applies the parser gate and is not its inverse. These tests
//! pin the identities that only the Rust `ObjectRef` factory can name, so that
//! carrying the raw identity through the resolver does not silently drop them.

use flpdf::{ObjectRef, Pdf};

fn open() -> Pdf<std::io::Cursor<Vec<u8>>> {
    let bytes = std::fs::read("../../tests/fixtures/minimal.pdf").expect("fixture");
    Pdf::open(std::io::Cursor::new(bytes)).expect("open")
}

/// Identities outside the `N G R` parser range, plus object number 0, which the
/// public `ObjectRef` factory admits but `QpdfObjGen::to_object_ref` rejects.
const UNPROJECTABLE: [(u32, u16); 2] = [(9, 65535), (0, 0)];

/// Identities the parser range does cover, as the control.
const PROJECTABLE: [(u32, u16); 2] = [(7, 0), (5, 65534)];

#[test]
fn get_all_objects_preserves_handle_identities() {
    for (number, generation) in UNPROJECTABLE.into_iter().chain(PROJECTABLE) {
        let mut pdf = open();
        let object_ref = ObjectRef::new(number, generation);
        let handle = pdf.get_object_handle(object_ref);
        assert_eq!(handle.object_ref(), Some(object_ref));
        assert!(handle.is_indirect());

        pdf.get_all_objects().expect("get_all_objects");

        assert_eq!(
            handle.object_ref(),
            Some(object_ref),
            "enumeration dropped the projection for {number} {generation}"
        );
        assert!(
            handle.is_indirect(),
            "enumeration made {number} {generation} direct"
        );
    }
}

#[test]
fn swapping_objects_preserves_handle_identities() {
    for (number, generation) in UNPROJECTABLE.into_iter().chain(PROJECTABLE) {
        let mut pdf = open();
        let object_ref = ObjectRef::new(number, generation);
        let other_ref = ObjectRef::new(3, 0);
        let handle = pdf.get_object_handle(object_ref);
        let other = pdf.get_object_handle(other_ref);

        pdf.swap_objects(object_ref, other_ref).expect("swap");

        assert_eq!(
            handle.object_ref(),
            Some(object_ref),
            "swap dropped the projection for {number} {generation}"
        );
        assert!(
            handle.is_indirect(),
            "swap made {number} {generation} direct"
        );
        assert_eq!(other.object_ref(), Some(other_ref));
        assert!(other.is_indirect());
    }
}
