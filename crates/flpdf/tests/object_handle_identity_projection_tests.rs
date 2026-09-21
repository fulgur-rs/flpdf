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
//!
//! Object number zero is the second such identity, and it fails the gate for a
//! different reason: it is not an indirect reference at all, because
//! `QPDFObjGen::isIndirect()` is `obj != 0`
//! (`include/qpdf/QPDFObjGen.hh:77-81`) and `QPDFObjectHandle::isIndirect()`
//! repeats that test (`include/qpdf/QPDFObjectHandle.hh:1629-1633`). qpdf's
//! cache does not consult it, so the identity still receives a canonical slot
//! (`QPDF::getObject`, `libqpdf/QPDF.cc:1952-1959`) which — having no
//! cross-reference row of its own — settles on the unknown-object null
//! fallback (`QPDF::resolve`, `libqpdf/QPDF.cc:1743-1747`).

use flpdf::{ObjectHandle, ObjectRef, Pdf};

fn open() -> Pdf<std::io::Cursor<Vec<u8>>> {
    let bytes = std::fs::read("../../tests/fixtures/minimal.pdf").expect("fixture");
    Pdf::open(std::io::Cursor::new(bytes)).expect("open")
}

/// The object-number-zero identity these tests carry through the resolver.
fn object_zero() -> ObjectRef {
    ObjectRef::new(0, 17)
}

/// Identities outside the `N G R` parser range, which the public `ObjectRef`
/// factory admits but `QpdfObjGen::to_object_ref` rejects.
const UNPROJECTABLE: [(u32, u16); 1] = [(9, 65535)];

/// Identities the parser range does cover, as the control.
const PROJECTABLE: [(u32, u16); 2] = [(7, 0), (5, 65534)];

#[test]
fn get_all_objects_preserves_handle_identities() {
    for (number, generation) in UNPROJECTABLE.into_iter().chain(PROJECTABLE) {
        let mut pdf = open();
        let object_ref = ObjectRef::new(number, generation);
        let handle = pdf.get_object_handle(object_ref);
        let expected_projection = (generation < u16::MAX).then_some(object_ref);
        assert_eq!(handle.object_ref(), expected_projection);
        assert!(handle.is_indirect());

        pdf.get_all_objects().expect("get_all_objects");

        assert_eq!(
            handle.object_ref(),
            expected_projection,
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
        let expected_projection = (generation < u16::MAX).then_some(object_ref);

        pdf.swap_objects(object_ref, other_ref).expect("swap");

        assert_eq!(
            handle.object_ref(),
            expected_projection,
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

#[test]
fn object_number_zero_is_not_an_indirect_object_handle_projection() {
    let mut pdf = open();
    let handle = pdf.get_object_handle(object_zero());

    assert!(!handle.is_indirect());
    assert_eq!(handle.object_ref(), None);
}

/// Object number zero cannot ride the loop above, because it answers `None`
/// and `false` where every projectable identity answers its own reference and
/// `true`. Enumeration must still hand back the one cache slot the identity
/// already owns: qpdf's `getAllObjects` re-wraps each cached object in place
/// (`newIndirect(iter.first, iter.second.object)`,
/// `libqpdf/QPDF.cc:1286-1295`), which is the same object `QPDF::getObject`
/// returns for that identity (`libqpdf/QPDF.cc:1952-1959`).
#[test]
fn get_all_objects_preserves_the_object_number_zero_handle() {
    let mut pdf = open();
    let handle = pdf.get_object_handle(object_zero());
    assert_eq!(handle.object_ref(), None);
    assert!(!handle.is_indirect());
    assert!(handle.is_same_object_as(&pdf.get_object_handle(object_zero())));

    let enumerated = pdf.get_all_objects().expect("get_all_objects");

    assert!(
        enumerated
            .iter()
            .any(|entry| entry.is_same_object_as(&handle)),
        "enumeration dropped object number zero's cache entry"
    );
    assert_eq!(
        handle.object_ref(),
        None,
        "enumeration invented a projection for object number zero"
    );
    assert!(
        !handle.is_indirect(),
        "enumeration made object number zero indirect"
    );
    assert!(
        handle.is_same_object_as(&pdf.get_object_handle(object_zero())),
        "enumeration minted a second slot for object number zero"
    );
}

/// qpdf's `swapObjects` resolves both identities and then swaps the values the
/// two cache slots hold (`libqpdf/QPDF.cc:2284-2291`), so the slots themselves
/// — object number zero's included — outlive the swap.
#[test]
fn swapping_objects_preserves_the_object_number_zero_handle() {
    let mut pdf = open();
    let other_ref = ObjectRef::new(3, 0);
    let handle = pdf.get_object_handle(object_zero());
    let other = pdf.get_object_handle(other_ref);

    pdf.swap_objects(object_zero(), other_ref).expect("swap");

    assert_eq!(
        handle.object_ref(),
        None,
        "swap invented a projection for object number zero"
    );
    assert!(
        !handle.is_indirect(),
        "swap made object number zero indirect"
    );
    assert!(
        handle.is_same_object_as(&pdf.get_object_handle(object_zero())),
        "swap minted a second slot for object number zero"
    );
    assert_eq!(other.object_ref(), Some(other_ref));
    assert!(other.is_indirect());
}

/// Every accessor that qpdf routes through `dereference()` must settle object
/// number zero on the null fallback, against a real document rather than a
/// stand-in resolver. Each accessor opens its own `Pdf` so that none of them
/// observes resolution another one already performed.
#[test]
fn object_number_zero_resolves_to_null_through_a_real_pdf() {
    let mut pdf = open();
    let handle = pdf.get_object_handle(object_zero());
    assert!(!handle.is_resolved());
    assert!(
        handle.try_is_null().expect("null resolution"),
        "object number zero did not settle on qpdf's null fallback"
    );
    assert!(handle.is_resolved());

    // Assert serialization through the fallible twin: `unparse_resolved` maps
    // a failed serialization onto a literal `null`, so only
    // `try_unparse_resolved` distinguishes real null resolution from that
    // fallback.
    let mut pdf = open();
    let handle = pdf.get_object_handle(object_zero());
    assert_eq!(
        handle.try_unparse_resolved().expect("unparse"),
        b"null".to_vec()
    );

    // `QPDFObjectHandle::unparse` emits the `N G R` form only for an indirect
    // handle and otherwise delegates to `unparseResolved`
    // (`libqpdf/QPDFObjectHandle.cc:1574-1584`).
    let mut pdf = open();
    let handle = pdf.get_object_handle(object_zero());
    assert_eq!(handle.unparse(), b"null".to_vec());

    // `QPDFObjectHandle::writeJSON` gates only the reference form on
    // `isIndirect()` and otherwise falls through to an unconditional
    // `dereference()` (`libqpdf/QPDFObjectHandle.cc:1630-1639`,
    // `:2376-2383`), so object number zero serializes as JSON null under
    // either `dereference_indirect` setting.
    for dereference_indirect in [true, false] {
        let mut pdf = open();
        let handle = pdf.get_object_handle(object_zero());
        let json = handle
            .get_json(2, dereference_indirect)
            .expect("object number zero must serialize as JSON null");
        assert!(
            json.is_null(),
            "object number zero serialized as something other than JSON null \
             with dereference_indirect={dereference_indirect}"
        );
    }
}

/// An array element does not take the handle route out of qpdf.
/// `QPDF_Array::writeJSON` writes each element through `QPDFObject::writeJSON`
/// (`libqpdf/QPDF_Array.cc:163-169`), which performs no resolution, so an
/// unresolved element raises `QPDF_Unresolved::writeJSON`'s own error rather
/// than settling on the null fallback the handle route would reach
/// (`libqpdf/QPDF_Unresolved.cc:29-33`).
///
/// Object number zero is the only identity that can tell the two routes apart:
/// an indirect element takes the reference-form branch before either route
/// resolves anything, and a direct element is never unresolved.
///
/// Such an element is reachable only programmatically. qpdf's parser rejects
/// `id < 1` and substitutes a null (`libqpdf/QPDFParser.cc:166-176`), so
/// `0 G R` never becomes a reference in a parsed document.
#[test]
fn an_unresolved_object_number_zero_array_element_is_not_resolved_for_json() {
    let mut pdf = open();
    let zero = pdf.get_object_handle(object_zero());
    let array = ObjectHandle::array(vec![zero, ObjectHandle::integer(1)]);

    let error = array
        .get_json(2, true)
        .expect_err("the value route must not resolve an array element");

    assert_eq!(
        error.to_string(),
        "attempted to get JSON from an unresolved QPDFObjectHandle"
    );
}

/// `QPDF_Dictionary::writeJSON` filters each entry with `isNull()` before
/// emitting its key (`libqpdf/QPDF_Dictionary.cc:72-92`); `isNull()`
/// dereferences unconditionally (`libqpdf/QPDFObjectHandle.cc:353-356`,
/// `:2376-2383`), so a dangling entry's key disappears from the output
/// entirely rather than surviving with a `null` value. A dangling *indirect*
/// child already matched this before object number zero was resolved for
/// JSON (`flpdf-e7d0f`); this pins that the object-zero case matches too,
/// since it is the one identity `object_gen.is_some()` admits without also
/// being indirect.
#[test]
fn a_dictionary_value_at_object_number_zero_is_omitted_from_json_like_qpdf() {
    let mut pdf = open();
    let zero = pdf.get_object_handle(object_zero());
    let dictionary = ObjectHandle::dictionary(vec![
        (b"/Keep".to_vec(), ObjectHandle::integer(1)),
        (b"/Zero".to_vec(), zero),
    ]);

    let json = dictionary
        .get_json(2, true)
        .expect("object number zero settles to null on the handle route, not an error");

    let mut keys = Vec::new();
    json.for_each_dict_item(|key, _value| keys.push(key.to_vec()));

    assert_eq!(
        keys,
        vec![b"/Keep".to_vec()],
        "a dictionary value at object number zero resolves to null and must drop its \
         key entirely, matching qpdf, instead of surviving as \"/Zero\": null"
    );
}
