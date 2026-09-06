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
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    let mut roundtrip = Pdf::open_mem_owned(writer.get_buffer().unwrap()).unwrap();
    assert_eq!(
        roundtrip
            .trailer()
            .try_get_key(b"/Promoted")
            .unwrap()
            .unparse_resolved(),
        b"[ 1 42 ]"
    );
}
