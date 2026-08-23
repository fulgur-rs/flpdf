//! qpdf correspondence: test-only coverage for QPDFWriter trailer reference renumbering.

use super::*;
use crate::object::{Dictionary, Object};
use crate::writer::rewrite_renumber::CatalogFirstRenumber;
use crate::ObjectRef;

#[test]
fn remap_trailer_refs_rewrites_nested_direct_container() {
    let map = CatalogFirstRenumber::from_pairs_for_test(&[
        (ObjectRef::new(1, 0), ObjectRef::new(1, 0)),
        (ObjectRef::new(10, 0), ObjectRef::new(3, 0)),
    ]);
    let mut nested = Dictionary::new();
    nested.insert("Child", Object::Reference(ObjectRef::new(10, 0)));
    let mut trailer = Dictionary::new();
    trailer.insert("Extra", Object::Dictionary(nested));

    remap_trailer_refs(&mut trailer, &map, &[]).expect("nested direct container must remap");

    let extra = trailer
        .get("Extra")
        .expect("nested direct container must exist")
        .as_dict()
        .expect("nested direct container must remain a dictionary");
    assert_eq!(
        extra.get("Child"),
        Some(&Object::Reference(ObjectRef::new(3, 0)))
    );
}
