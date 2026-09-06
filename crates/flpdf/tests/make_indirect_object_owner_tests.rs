//! qpdf makeIndirectObject promotes the existing QObject allocation.

use flpdf::{ObjectHandle, Pdf};

#[test]
fn public_factory_promotes_the_retained_direct_alias_in_place() {
    let mut pdf = Pdf::empty().unwrap();
    let source = ObjectHandle::dictionary(vec![(b"/Value".to_vec(), ObjectHandle::integer(42))]);
    let alias = source.clone();
    let result = pdf.make_indirect_object_handle(source).unwrap();

    assert!(alias.is_same_object_as(&result));
    assert!(alias.is_indirect());
    assert_eq!(alias.object_ref(), result.object_ref());
    alias
        .replace_key(b"/Value", ObjectHandle::integer(7))
        .unwrap();
    assert!(result
        .try_get_key(b"/Value")
        .unwrap()
        .try_is_integer()
        .unwrap());
    assert_eq!(result.try_get_key(b"/Value").unwrap().unparse(), b"7");
}

#[test]
fn indirect_and_reserved_inputs_are_registered_again_with_the_same_identity() {
    let mut pdf = Pdf::empty().unwrap();
    let first = pdf
        .make_indirect_object_handle(ObjectHandle::integer(42))
        .unwrap();
    let first_ref = first.object_ref().unwrap();
    let second = pdf.make_indirect_object_handle(first.clone()).unwrap();
    let second_ref = second.object_ref().unwrap();
    assert_eq!(second_ref.number, first_ref.number + 1);
    assert!(first.is_same_object_as(&second));
    assert!(pdf.get_object_handle(first_ref).is_same_object_as(&first));
    assert_eq!(second.object_ref(), Some(first_ref));
    pdf.get_object_handle(second_ref);
    assert_eq!(first.object_ref(), Some(second_ref));

    let reserved = pdf.new_reserved().unwrap();
    let reserved_ref = reserved.object_ref().unwrap();
    let promoted = pdf.make_indirect_object_handle(reserved.clone()).unwrap();
    assert!(promoted.is_same_object_as(&reserved));
    assert!(promoted.is_reserved());
    assert_eq!(
        promoted.object_ref().unwrap().number,
        reserved_ref.number + 1
    );
}

#[test]
fn uninitialized_is_rejected_before_allocation_and_maximum_id_leaves_input_direct() {
    let mut pdf = Pdf::empty().unwrap();
    pdf.get_object_handle(flpdf::ObjectRef::new(i32::MAX as u32, 0));
    let error = pdf
        .make_indirect_object_handle(ObjectHandle::uninitialized())
        .unwrap_err();
    assert!(
        matches!(error, flpdf::Error::Internal(ref message) if message == "attempted to make an uninitialized QPDFObjectHandle indirect")
    );
    let source = ObjectHandle::integer(9);
    let error = pdf.make_indirect_object_handle(source.clone()).unwrap_err();
    assert!(
        matches!(error, flpdf::Error::Unsupported(ref message) if message == "max object id is too high to create new objects")
    );
    assert!(source.is_direct());
}

#[test]
fn promotion_updates_distinct_handles_sharing_the_replacement_value() {
    let mut pdf = Pdf::empty().unwrap();
    let target = pdf
        .make_indirect_object_handle(ObjectHandle::integer(1))
        .unwrap();
    let target_ref = target.object_ref().unwrap();
    let replacement = ObjectHandle::integer(99);
    pdf.replace_object(target_ref, replacement.clone()).unwrap();
    assert!(!target.is_same_object_as(&replacement));
    let promoted = pdf
        .make_indirect_object_handle(replacement.clone())
        .unwrap();
    let new_ref = promoted.object_ref();
    assert!(promoted.is_same_object_as(&replacement));
    assert_eq!(target.object_ref(), new_ref);
    assert_eq!(target.unparse_resolved(), b"99");
    pdf.get_object_handle(target_ref);
    assert_eq!(replacement.object_ref(), Some(target_ref));
}

#[test]
fn promotion_can_change_the_owning_document_and_lookup_restores_it() {
    let mut first_pdf = Pdf::empty().unwrap();
    let mut second_pdf = Pdf::empty().unwrap();
    let source = first_pdf
        .make_indirect_object_handle(ObjectHandle::integer(8))
        .unwrap();
    let first_ref = source.object_ref().unwrap();
    let second = second_pdf
        .make_indirect_object_handle(source.clone())
        .unwrap();
    assert!(source.is_same_object_as(&second));
    assert!(second_pdf
        .get_object_handle(second.object_ref().unwrap())
        .is_same_object_as(&source));
    assert!(first_pdf
        .get_object_handle(first_ref)
        .is_same_object_as(&source));
    assert_eq!(source.unparse_resolved(), b"8");
}

#[test]
fn retained_alias_mutation_is_visible_in_written_output() {
    let mut pdf = Pdf::empty().unwrap();
    let source = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
    let promoted = pdf.make_indirect_object_handle(source.clone()).unwrap();
    pdf.trailer().replace_key(b"/Promoted", promoted).unwrap();
    source.append_array_item(ObjectHandle::integer(42)).unwrap();
    let mut writer = flpdf::PdfWriter::new(&mut pdf);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    let bytes = writer.get_buffer().unwrap();
    assert_eq!(
        bytes,
        include_bytes!("../../../tests/fixtures/compat/golden/make-indirect-retained-alias.pdf")
    );
    let mut roundtrip = Pdf::open_mem_owned(bytes).unwrap();
    assert_eq!(
        roundtrip
            .trailer()
            .try_get_key(b"/Promoted")
            .unwrap()
            .unparse_resolved(),
        b"[ 1 42 ]"
    );
}

#[test]
fn unresolved_input_is_not_dereferenced_by_promotion() {
    let mut pdf = Pdf::empty().unwrap();
    let source = pdf.get_object_handle(flpdf::ObjectRef::new(99, 0));
    let promoted = pdf.make_indirect_object_handle(source.clone()).unwrap();
    assert!(promoted.is_same_object_as(&source));
    assert_eq!(promoted.object_ref(), Some(flpdf::ObjectRef::new(100, 0)));
    assert!(!source.is_resolved());
    assert_eq!(promoted.unparse_resolved(), b"null");
}

#[test]
fn direct_reserved_and_destroyed_values_keep_their_initialized_state() {
    let mut pdf = Pdf::empty().unwrap();
    let reserved = pdf.new_reserved().unwrap().shallow_copy().unwrap();
    assert!(reserved.is_direct());
    let result = pdf.make_indirect_object_handle(reserved.clone()).unwrap();
    assert!(result.is_reserved());
    assert!(result.is_same_object_as(&reserved));

    let destroyed = {
        let mut other = Pdf::empty().unwrap();
        other
            .make_indirect_object_handle(ObjectHandle::integer(8))
            .unwrap()
    };
    assert_eq!(destroyed.type_code().unwrap(), 14);
    let result = pdf.make_indirect_object_handle(destroyed.clone()).unwrap();
    assert_eq!(result.type_code().unwrap(), 14);
    assert!(result.is_same_object_as(&destroyed));
}

#[test]
fn maximum_valid_object_number_can_be_allocated_once() {
    let mut pdf = Pdf::empty().unwrap();
    pdf.get_object_handle(flpdf::ObjectRef::new(i32::MAX as u32 - 1, 0));
    let source = ObjectHandle::null();
    let result = pdf.make_indirect_object_handle(source.clone()).unwrap();
    assert_eq!(
        result.object_ref(),
        Some(flpdf::ObjectRef::new(i32::MAX as u32, 0))
    );
    assert!(result.is_same_object_as(&source));
    assert!(pdf
        .make_indirect_object_handle(ObjectHandle::null())
        .is_err());
}

#[test]
fn enumeration_reapplies_each_cache_key_to_the_shared_value() {
    let mut pdf = Pdf::empty().unwrap();
    let source = pdf
        .make_indirect_object_handle(ObjectHandle::integer(1))
        .unwrap();
    let old_ref = source.object_ref().unwrap();
    pdf.make_indirect_object_handle(source.clone()).unwrap();
    let new_ref = source.object_ref().unwrap();
    pdf.get_object_handle(old_ref);
    assert_eq!(source.object_ref(), Some(old_ref));
    let objects = pdf.get_all_objects().unwrap();
    assert_eq!(
        objects
            .iter()
            .filter(|handle| handle.is_same_object_as(&source))
            .count(),
        2
    );
    assert_eq!(source.object_ref(), Some(new_ref));
}

#[test]
fn replacing_a_repromoted_value_preserves_the_departing_alias_identity() {
    let mut pdf = Pdf::empty().unwrap();
    let target = pdf
        .make_indirect_object_handle(ObjectHandle::integer(1))
        .unwrap();
    let target_ref = target.object_ref().unwrap();
    let shared = ObjectHandle::integer(2);
    pdf.replace_object(target_ref, shared.clone()).unwrap();
    pdf.make_indirect_object_handle(shared.clone()).unwrap();
    let shared_ref = shared.object_ref();
    pdf.replace_object(target_ref, ObjectHandle::integer(3))
        .unwrap();
    assert_eq!(shared.object_ref(), shared_ref);
    assert_eq!(shared.unparse_resolved(), b"2");
    assert_eq!(target.object_ref(), Some(target_ref));
    assert_eq!(target.unparse_resolved(), b"3");
}

#[test]
fn replacement_of_an_absent_cache_entry_registers_the_existing_object() {
    let mut pdf = Pdf::empty().unwrap();
    let source = ObjectHandle::integer(9);
    let result = pdf
        .replace_object(flpdf::ObjectRef::new(100, 0), source.clone())
        .unwrap();
    assert!(result.is_same_object_as(&source));
    let promoted = pdf.make_indirect_object_handle(source.clone()).unwrap();
    assert_eq!(promoted.object_ref(), Some(flpdf::ObjectRef::new(101, 0)));
    assert!(result.is_same_object_as(&promoted));
}

#[test]
fn swapping_repromoted_objects_keeps_active_numbers_and_moves_the_value_owner() {
    let mut pdf = Pdf::empty().unwrap();
    let first = pdf
        .make_indirect_object_handle(ObjectHandle::integer(1))
        .unwrap();
    let old_ref = first.object_ref().unwrap();
    let second = pdf
        .make_indirect_object_handle(ObjectHandle::integer(2))
        .unwrap();
    let second_ref = second.object_ref().unwrap();
    let second_owner = second.owning_pdf_unique_id();
    let mut foreign = Pdf::empty().unwrap();
    foreign.get_object_handle(flpdf::ObjectRef::new(9, 0));
    foreign.make_indirect_object_handle(first.clone()).unwrap();
    let new_ref = first.object_ref();
    let first_owner = first.owning_pdf_unique_id();
    pdf.swap_objects(old_ref, second_ref).unwrap();
    assert_eq!(first.object_ref(), new_ref);
    assert_eq!(second.object_ref(), Some(second_ref));
    assert_eq!(first.owning_pdf_unique_id(), second_owner);
    assert_eq!(second.owning_pdf_unique_id(), first_owner);
    assert_eq!(first.unparse_resolved(), b"2");
    assert_eq!(second.unparse_resolved(), b"1");
}

#[test]
fn document_drop_clears_shared_value_identity_before_detaching_cached_objects() {
    let source = ObjectHandle::integer(99);
    {
        let mut pdf = Pdf::empty().unwrap();
        let target = pdf
            .make_indirect_object_handle(ObjectHandle::integer(1))
            .unwrap();
        pdf.replace_object(target.object_ref().unwrap(), source.clone())
            .unwrap();
        assert!(source.is_indirect());
    }
    assert!(source.is_direct());
    assert_eq!(source.owning_pdf_unique_id(), None);
    assert_eq!(source.unparse_resolved(), b"99");
}

#[test]
fn swapping_an_unresolved_repromoted_object_uses_the_requested_resolution_identity() {
    let mut pdf = Pdf::empty().unwrap();
    let old_ref = flpdf::ObjectRef::new(99, 0);
    let source = pdf.get_object_handle(old_ref);
    pdf.make_indirect_object_handle(source.clone()).unwrap();
    let second = pdf
        .make_indirect_object_handle(ObjectHandle::integer(7))
        .unwrap();
    let second_ref = second.object_ref().unwrap();
    assert!(!source.is_resolved());
    pdf.swap_objects(old_ref, second_ref).unwrap();
    assert_eq!(source.object_ref(), Some(old_ref));
    assert_eq!(source.unparse_resolved(), b"7");
    assert_eq!(second.object_ref(), Some(second_ref));
    assert_eq!(second.unparse_resolved(), b"null");
}

#[test]
fn factory_counts_historical_trailer_references_before_any_enumeration() {
    let mut pdf = Pdf::open_mem_owned(
        include_bytes!("../../../tests/fixtures/compat/make-indirect-historical-trailer.pdf")
            .to_vec(),
    )
    .unwrap();
    let result = pdf
        .make_indirect_object_handle(ObjectHandle::integer(7))
        .unwrap();
    assert_eq!(result.object_ref(), Some(flpdf::ObjectRef::new(100, 0)));
    let historical = pdf.get_object_handle(flpdf::ObjectRef::new(99, 0));
    assert!(!historical.is_resolved());
    pdf.get_all_objects().unwrap();
    assert!(
        !historical.is_resolved(),
        "qpdf getAllObjects does not resolve cache-only dangling references"
    );
}
