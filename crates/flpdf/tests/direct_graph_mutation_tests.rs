use flpdf::{ObjectHandle, Pdf};

#[test]
fn direct_self_dictionary_can_be_promoted_to_an_indirect_self_reference() {
    let dictionary = ObjectHandle::dictionary(Vec::new());
    dictionary
        .replace_key(b"/Self", dictionary.clone())
        .unwrap();
    assert!(dictionary
        .try_get_key(b"/Self")
        .unwrap()
        .is_same_object_as(&dictionary));

    let mut pdf = Pdf::empty().unwrap();
    let indirect = pdf.make_indirect_object_handle(dictionary.clone()).unwrap();
    let expected = format!("<< /Self {} >>", indirect.object_ref().unwrap());
    assert_eq!(indirect.unparse_resolved().unwrap(), expected.as_bytes());
    let mut direct = indirect.clone();
    let error = direct.make_direct(false).unwrap_err();
    assert!(matches!(error, flpdf::Error::System(message)
        if message == "loop detected while converting object from indirect to direct"));
}

#[test]
fn all_public_array_mutators_accept_direct_self_references() {
    for operation in 0..6 {
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        let index = match operation {
            0 => {
                array.set_array_item(0, array.clone()).unwrap();
                0
            }
            1 => {
                array.set_array_items(vec![array.clone()]).unwrap();
                0
            }
            2 => {
                array.insert_array_item(0, array.clone()).unwrap();
                0
            }
            3 => {
                let returned = array
                    .insert_array_item_and_get_new(0, array.clone())
                    .unwrap();
                assert!(returned.is_same_object_as(&array));
                0
            }
            4 => {
                array.append_array_item(array.clone()).unwrap();
                1
            }
            _ => {
                let returned = array.append_array_item_and_get_new(array.clone()).unwrap();
                assert!(returned.is_same_object_as(&array));
                1
            }
        };
        assert!(array
            .try_get_array_item(index)
            .unwrap()
            .is_same_object_as(&array));
        array.set_array_items(Vec::new()).unwrap();
        assert_eq!(array.try_get_array_n_items().unwrap(), 0);
    }
}

#[test]
fn bulk_array_replacement_keeps_the_self_prefix_before_a_foreign_owner_error() {
    let target = Pdf::empty().unwrap();
    let mut source = Pdf::empty().unwrap();
    let foreign = source
        .make_indirect_object_handle(ObjectHandle::integer(17))
        .unwrap();
    let array = ObjectHandle::array(vec![ObjectHandle::integer(9)]);
    array.set_object_description(&target, "array").unwrap();

    let error = array
        .set_array_items(vec![array.clone(), foreign])
        .unwrap_err();

    assert!(matches!(error, flpdf::Error::Internal(message)
        if message == "Attempting to add an object from a different QPDF. Use QPDF::copyForeignObject to add objects from another file."));
    assert_eq!(array.try_get_array_n_items().unwrap(), 1);
    assert!(array
        .try_get_array_item(0)
        .unwrap()
        .is_same_object_as(&array));
    array.set_array_items(Vec::new()).unwrap();
}

#[test]
fn dictionary_replacement_resolves_an_indirect_value_before_inserting_it() {
    let data = include_bytes!("../../../tests/fixtures/compat/dictionary-lazy-value.pdf").to_vec();
    let mut pdf = Pdf::open_mem_owned(data).unwrap();
    pdf.set_suppress_warnings(true);
    let value = pdf.get_object_handle(flpdf::ObjectRef::new(3, 0));
    let dictionary = ObjectHandle::dictionary(Vec::new());
    assert!(!pdf.any_warnings());

    dictionary.replace_key(b"/Value", value.clone()).unwrap();

    assert_eq!(pdf.num_warnings(), 1);
    assert!(dictionary
        .try_get_key(b"/Value")
        .unwrap()
        .is_same_object_as(&value));
}
