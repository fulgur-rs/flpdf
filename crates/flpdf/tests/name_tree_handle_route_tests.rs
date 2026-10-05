use std::{io::Read, io::Seek, path::Path};

use flpdf::{Error, NameTree, NumberTree, ObjectHandle, Pdf};

fn source_block<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source.find(start).expect("source block start");
    let remainder = &source[start..];
    let end = remainder.find(end).expect("source block end");
    &remainder[..end]
}

#[test]
fn public_name_tree_surface_is_object_handle_native() {
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("nntree.rs");
    let source = std::fs::read_to_string(source_path).expect("read nntree source");
    let name_start = source
        .find("pub struct NameTree")
        .expect("NameTree declaration");
    let number_start = source
        .find("pub struct NumberTree")
        .expect("NumberTree declaration");
    let name_surface = &source[name_start..number_start];

    assert!(
        name_surface.contains("ObjectHandle"),
        "NameTree must expose canonical ObjectHandle values"
    );
    for forbidden in [
        "pub fn new(root: Object,",
        "pub fn root(&self) -> &Object",
        "value: Object,",
        "Result<Option<Object>>",
        "Option<(Vec<u8>, Object)>",
    ] {
        assert!(
            !name_surface.contains(forbidden),
            "NameTree still exposes the raw Object route: {forbidden}"
        );
    }
}

#[test]
fn name_and_number_tree_key_reads_use_resolving_accessors() {
    let source = include_str!("../src/nntree.rs");
    let name_key = source_block(
        source,
        "impl TreeKey for NameKey",
        "pub(crate) enum NumberKey",
    );
    let number_key = source_block(
        source,
        "impl TreeKey for NumberKey",
        "#[derive(Clone, Debug, Eq, Hash, PartialEq)]",
    );
    let resolved_key = source_block(
        source,
        "fn resolved_key<K: TreeKey>",
        "/// Reject a handle-native tree value",
    );
    let increment = source_block(
        source,
        "fn increment<R: Read + Seek>",
        "fn update_current<R: Read + Seek>",
    );
    let update_current = source_block(
        source,
        "fn update_current<R: Read + Seek>",
        "fn insert_pair_with_allocator<R",
    );
    let live_dictionary_get = source_block(
        source,
        "fn get(&self, key: &str) -> Result<Option<ObjectHandle>>",
        "fn insert(&self, key: &str, value: ObjectHandle)",
    );

    assert!(name_key.contains("handle.try_is_string()"));
    assert!(name_key.contains("handle.try_get_utf8_value()"));
    assert!(!name_key.contains(".as_string()"));
    assert!(number_key.contains("handle.try_is_integer()"));
    assert!(number_key.contains("handle.try_get_int_value()"));
    assert!(!number_key.contains(".as_integer()"));
    assert!(resolved_key.contains("K::is_valid_key(value)?"));
    assert!(resolved_key.contains("K::value_from_handle(value)?"));
    assert!(!resolved_key.contains("try_dereference"));
    assert!(increment.contains("K::is_valid_key("));
    assert!(update_current.contains("K::value_from_handle(&raw_key)?"));
    assert!(live_dictionary_get.contains("value.try_is_null()?"));
    assert!(!live_dictionary_get.contains("value.is_null()"));
}

fn assert_invalid_tree_key_error(error: Error) {
    assert!(
        matches!(error, Error::QpdfExc(_)),
        "unexpected qpdf tree error: {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("item at index 0 is not the right type"),
        "unexpected qpdf tree error: {error}"
    );
}

fn maybe_make_indirect<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: ObjectHandle,
    indirect: bool,
) -> ObjectHandle {
    if indirect {
        pdf.make_indirect_from_object_handle(value)
            .expect("allocate indirect wrong-typed key")
    } else {
        value
    }
}

fn name_tree_with_wrong_first_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    wrong_type_key: ObjectHandle,
    auto_repair: bool,
) -> NameTree {
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![wrong_type_key, ObjectHandle::integer(11)]),
        )]))
        .expect("allocate name-tree root");
    if auto_repair {
        NameTree::new(root)
    } else {
        NameTree::new_with_options(root, false)
    }
}

fn number_tree_with_wrong_first_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    wrong_type_key: ObjectHandle,
    auto_repair: bool,
) -> NumberTree {
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![wrong_type_key, ObjectHandle::integer(11)]),
        )]))
        .expect("allocate number-tree root");
    if auto_repair {
        NumberTree::new(root)
    } else {
        NumberTree::new_with_options(root, false)
    }
}

fn assert_single_repair_warning<R: Read + Seek>(pdf: &Pdf<R>) {
    let diagnostics = pdf.get_warnings();
    let warnings = diagnostics.entries();
    assert_eq!(warnings.len(), 1);
    assert!(String::from_utf8_lossy(warnings[0].get_message_detail())
        .contains("attempting to repair after error"));
}

#[test]
fn name_and_number_tree_qpdf_default_call_shapes() {
    let mut pdf = Pdf::empty().expect("create empty PDF");

    let name_root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(b"a".to_vec()),
                ObjectHandle::integer(11),
                ObjectHandle::string(b"c".to_vec()),
                ObjectHandle::integer(13),
            ]),
        )]))
        .expect("allocate name-tree root");
    let mut name_tree = NameTree::new(name_root);
    let missing_name = name_tree
        .find(&mut pdf, b"b")
        .expect("default name-tree find");
    assert!(
        !missing_name.valid(),
        "default find does not return a predecessor"
    );
    let previous_name = name_tree
        .find_with_options(&mut pdf, b"b", true)
        .expect("explicit predecessor search");
    assert_eq!(previous_name.current().expect("predecessor value").0, b"a");

    let number_root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(1),
                ObjectHandle::integer(11),
                ObjectHandle::integer(3),
                ObjectHandle::integer(13),
            ]),
        )]))
        .expect("allocate number-tree root");
    let mut number_tree = NumberTree::new(number_root);
    let missing_number = number_tree
        .find(&mut pdf, 2)
        .expect("default number-tree find");
    assert!(
        !missing_number.valid(),
        "default find does not return a predecessor"
    );
    let previous_number = number_tree
        .find_with_options(&mut pdf, 2, true)
        .expect("explicit predecessor search");
    assert_eq!(previous_number.current().expect("predecessor value").0, 1);

    let empty_name_tree = NameTree::new_empty(&mut pdf).expect("default empty name tree");
    let empty_number_tree = NumberTree::new_empty(&mut pdf).expect("default empty number tree");
    let _configured_empty_name_tree = NameTree::new_empty_with_options(&mut pdf, false)
        .expect("explicit empty name-tree options");
    let _configured_empty_number_tree = NumberTree::new_empty_with_options(&mut pdf, false)
        .expect("explicit empty number-tree options");
    assert!(empty_name_tree.get_object_handle().object_ref().is_some());
    assert!(empty_number_tree.get_object_handle().object_ref().is_some());
}

#[test]
fn name_tree_find_wrong_typed_key_uses_qpdf_tree_error() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = pdf
        .make_indirect_from_object_handle(ObjectHandle::integer(7))
        .expect("allocate indirect wrong-typed name key");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![wrong_type_key, ObjectHandle::integer(11)]),
        )]))
        .expect("allocate name-tree root");
    let mut tree = NameTree::new_with_options(root, false);

    let error = match tree.find(&mut pdf, b"probe") {
        Ok(_) => panic!("qpdf rejects a wrong-typed key during find"),
        Err(error) => error,
    };

    assert_invalid_tree_key_error(error);
    assert!(pdf.get_warnings().entries().is_empty());
}

#[test]
fn number_tree_find_wrong_typed_key_uses_qpdf_tree_error() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = pdf
        .make_indirect_from_object_handle(ObjectHandle::string(b"not-an-integer".to_vec()))
        .expect("allocate indirect wrong-typed number key");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                wrong_type_key,
                ObjectHandle::string(b"value".to_vec()),
            ]),
        )]))
        .expect("allocate number-tree root");
    let mut tree = NumberTree::new_with_options(root, false);

    let error = match tree.find(&mut pdf, 1) {
        Ok(_) => panic!("qpdf rejects a wrong-typed key during find"),
        Err(error) => error,
    };

    assert_invalid_tree_key_error(error);
    assert!(pdf.get_warnings().entries().is_empty());
}

#[test]
fn number_tree_find_before_wrong_typed_first_key_uses_qpdf_tree_error() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = pdf
        .make_indirect_from_object_handle(ObjectHandle::string(b"not-an-integer".to_vec()))
        .expect("allocate indirect wrong-typed number key");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                wrong_type_key,
                ObjectHandle::string(b"invalid".to_vec()),
                ObjectHandle::integer(2),
                ObjectHandle::string(b"valid".to_vec()),
            ]),
        )]))
        .expect("allocate number-tree root");
    let mut tree = NumberTree::new_with_options(root, false);

    let error = match tree.find(&mut pdf, -1) {
        Ok(_) => panic!("qpdf rejects a wrong-typed key during find"),
        Err(error) => error,
    };

    assert_invalid_tree_key_error(error);
    assert!(pdf.get_warnings().entries().is_empty());
}

#[test]
fn name_tree_find_reports_error_when_search_reaches_direct_wrong_typed_key() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(7),
                ObjectHandle::integer(11),
                ObjectHandle::string(b"valid".to_vec()),
                ObjectHandle::integer(22),
            ]),
        )]))
        .expect("allocate name-tree root");
    let mut tree = NameTree::new_with_options(root, false);

    let error = match tree.find(&mut pdf, b"a") {
        Ok(_) => panic!("qpdf rejects a wrong-typed key during find"),
        Err(error) => error,
    };

    assert_invalid_tree_key_error(error);
    assert!(pdf.get_warnings().entries().is_empty());
}

#[test]
fn name_tree_auto_repair_preserves_direct_wrong_key_error_and_warning_order() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = ObjectHandle::integer(7);
    let mut tree = name_tree_with_wrong_first_key(&mut pdf, wrong_type_key, true);

    let error = match tree.find(&mut pdf, b"a") {
        Ok(_) => panic!("qpdf repair leaves this malformed tree unsearchable"),
        Err(error) => error,
    };

    assert_invalid_tree_key_error(error);
    assert_single_repair_warning(&pdf);
}

#[test]
fn name_tree_auto_repair_preserves_indirect_wrong_key_error_and_warning_order() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = maybe_make_indirect(&mut pdf, ObjectHandle::integer(7), true);
    let mut tree = name_tree_with_wrong_first_key(&mut pdf, wrong_type_key, true);

    let error = match tree.find(&mut pdf, b"a") {
        Ok(_) => panic!("qpdf repair leaves this malformed tree unsearchable"),
        Err(error) => error,
    };

    assert_invalid_tree_key_error(error);
    assert_single_repair_warning(&pdf);
}

#[test]
fn number_tree_auto_repair_preserves_direct_wrong_key_error_and_warning_order() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = ObjectHandle::string(b"not-an-integer".to_vec());
    let mut tree = number_tree_with_wrong_first_key(&mut pdf, wrong_type_key, true);

    let error = match tree.find(&mut pdf, -1) {
        Ok(_) => panic!("qpdf repair leaves this malformed tree unsearchable"),
        Err(error) => error,
    };

    assert_invalid_tree_key_error(error);
    assert_single_repair_warning(&pdf);
}

#[test]
fn number_tree_auto_repair_preserves_indirect_wrong_key_error_and_warning_order() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = maybe_make_indirect(
        &mut pdf,
        ObjectHandle::string(b"not-an-integer".to_vec()),
        true,
    );
    let mut tree = number_tree_with_wrong_first_key(&mut pdf, wrong_type_key, true);

    let error = match tree.find(&mut pdf, -1) {
        Ok(_) => panic!("qpdf repair leaves this malformed tree unsearchable"),
        Err(error) => error,
    };

    assert_invalid_tree_key_error(error);
    assert_single_repair_warning(&pdf);
}

#[test]
fn name_tree_resolves_an_indirect_string_key() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let key = pdf
        .make_indirect_from_object_handle(ObjectHandle::string(b"tree-key".to_vec()))
        .expect("allocate indirect name-tree key");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![key, ObjectHandle::integer(17)]),
        )]))
        .expect("allocate name-tree root");
    let mut tree = NameTree::new_with_options(root, false);

    let value = tree
        .find_object(&mut pdf, b"tree-key")
        .expect("resolve the name-tree key")
        .expect("find the entry");

    assert_eq!(value.try_get_int_value().expect("read value"), 17);
}

#[test]
fn number_tree_resolves_an_indirect_integer_key() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let key = pdf
        .make_indirect_from_object_handle(ObjectHandle::integer(42))
        .expect("allocate indirect number-tree key");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![key, ObjectHandle::string(b"label".to_vec())]),
        )]))
        .expect("allocate number-tree root");
    let mut tree = NumberTree::new_with_options(root, false);

    let value = tree
        .find_object(&mut pdf, 42)
        .expect("resolve the number-tree key")
        .expect("find the entry");

    assert_eq!(value.try_get_string_value().expect("read label"), b"label");
}

#[test]
fn name_tree_indirect_null_kids_value_is_treated_as_missing() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let null_kids = pdf
        .make_indirect_from_object_handle(ObjectHandle::null())
        .expect("allocate indirect null /Kids value");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Kids".to_vec(),
            null_kids,
        )]))
        .expect("allocate name-tree root");
    let mut tree = NameTree::new_with_options(root, false);

    let cursor = tree.begin(&mut pdf).expect("null /Kids is not an array");

    assert!(!cursor.valid());
    assert!(pdf.repair_diagnostics().entries().iter().any(|entry| {
        String::from_utf8_lossy(entry.get_message_detail())
            .contains("name/number tree node has neither non-empty /Names nor /Kids")
    }));
}

#[test]
fn name_tree_begin_reads_qpdf_default_value_for_wrong_typed_key() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = pdf
        .make_indirect_from_object_handle(ObjectHandle::integer(7))
        .expect("allocate indirect wrong-typed name key");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![
                wrong_type_key,
                ObjectHandle::integer(11),
                ObjectHandle::string(b"valid".to_vec()),
                ObjectHandle::integer(22),
            ]),
        )]))
        .expect("allocate name-tree root");
    let mut tree = NameTree::new_with_options(root, false);

    let mut cursor = tree.begin(&mut pdf).expect("match qpdf iterator begin");
    assert!(
        cursor.valid(),
        "qpdf positions the cursor at an in-range item"
    );
    let (key, value) = cursor.current().expect("current item");
    assert!(
        key.is_empty(),
        "qpdf getUTF8Value returns its empty default"
    );
    assert_eq!(value.try_get_int_value().expect("read paired value"), 11);
    let diagnostics = pdf.get_warnings();
    let warnings = diagnostics.entries();
    assert_eq!(warnings.len(), 1);
    assert!(String::from_utf8_lossy(warnings[0].get_message_detail())
        .contains("returning empty string"));

    cursor
        .next(&mut tree, &mut pdf)
        .expect("advance to next valid key");
    assert_eq!(
        cursor.current().map(|(key, _)| key),
        Some(b"valid".to_vec())
    );
}

#[test]
fn number_tree_begin_reads_qpdf_default_value_for_wrong_typed_key() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = pdf
        .make_indirect_from_object_handle(ObjectHandle::string(b"not-an-integer".to_vec()))
        .expect("allocate indirect wrong-typed number key");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                wrong_type_key,
                ObjectHandle::string(b"first".to_vec()),
                ObjectHandle::integer(4),
                ObjectHandle::string(b"next".to_vec()),
            ]),
        )]))
        .expect("allocate number-tree root");
    let mut tree = NumberTree::new_with_options(root, false);

    let mut cursor = tree.begin(&mut pdf).expect("match qpdf iterator begin");
    assert!(
        cursor.valid(),
        "qpdf positions the cursor at an in-range item"
    );
    let (key, value) = cursor.current().expect("current item");
    assert_eq!(key, 0, "qpdf getIntValue returns its zero default");
    assert_eq!(
        value.try_get_string_value().expect("read paired value"),
        b"first"
    );
    let diagnostics = pdf.get_warnings();
    let warnings = diagnostics.entries();
    assert_eq!(warnings.len(), 1);
    assert!(String::from_utf8_lossy(warnings[0].get_message_detail()).contains("returning 0"));

    cursor
        .next(&mut tree, &mut pdf)
        .expect("advance to next valid key");
    assert_eq!(cursor.current().map(|(key, _)| key), Some(4));
}

#[test]
fn name_tree_increment_skips_wrong_typed_key_and_warns_in_order() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = pdf
        .make_indirect_from_object_handle(ObjectHandle::integer(7))
        .expect("allocate indirect wrong-typed name key");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(b"first".to_vec()),
                ObjectHandle::integer(1),
                wrong_type_key,
                ObjectHandle::integer(2),
                ObjectHandle::string(b"last".to_vec()),
                ObjectHandle::integer(3),
            ]),
        )]))
        .expect("allocate name-tree root");
    let mut tree = NameTree::new_with_options(root, false);
    let mut cursor = tree.begin(&mut pdf).expect("begin at first valid key");

    cursor
        .next(&mut tree, &mut pdf)
        .expect("skip wrong-typed key");

    assert_eq!(cursor.current().map(|(key, _)| key), Some(b"last".to_vec()));
    let diagnostics = pdf.get_warnings();
    let warnings = diagnostics.entries();
    assert_eq!(warnings.len(), 1);
    assert!(String::from_utf8_lossy(warnings[0].get_message_detail())
        .contains("item 2 has the wrong type"));
}

#[test]
fn number_tree_increment_skips_wrong_typed_key_and_warns_in_order() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = pdf
        .make_indirect_from_object_handle(ObjectHandle::string(b"not-an-integer".to_vec()))
        .expect("allocate indirect wrong-typed number key");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(1),
                ObjectHandle::string(b"first".to_vec()),
                wrong_type_key,
                ObjectHandle::string(b"invalid".to_vec()),
                ObjectHandle::integer(3),
                ObjectHandle::string(b"last".to_vec()),
            ]),
        )]))
        .expect("allocate number-tree root");
    let mut tree = NumberTree::new_with_options(root, false);
    let mut cursor = tree.begin(&mut pdf).expect("begin at first valid key");

    cursor
        .next(&mut tree, &mut pdf)
        .expect("skip wrong-typed key");

    assert_eq!(cursor.current().map(|(key, _)| key), Some(3));
    let diagnostics = pdf.get_warnings();
    let warnings = diagnostics.entries();
    assert_eq!(warnings.len(), 1);
    assert!(String::from_utf8_lossy(warnings[0].get_message_detail())
        .contains("item 2 has the wrong type"));
}

#[test]
fn name_tree_increment_skips_wrong_typed_key_in_next_leaf() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let first_leaf = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(b"a".to_vec()),
                ObjectHandle::integer(1),
            ]),
        )]))
        .expect("allocate first name-tree leaf");
    let wrong_type_key = pdf
        .make_indirect_from_object_handle(ObjectHandle::integer(7))
        .expect("allocate indirect wrong-typed name key");
    let next_leaf = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![
                wrong_type_key,
                ObjectHandle::integer(2),
                ObjectHandle::string(b"z".to_vec()),
                ObjectHandle::integer(3),
            ]),
        )]))
        .expect("allocate next name-tree leaf");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Kids".to_vec(),
            ObjectHandle::array(vec![first_leaf, next_leaf]),
        )]))
        .expect("allocate name-tree root");
    let mut tree = NameTree::new_with_options(root, false);
    let mut cursor = tree.begin(&mut pdf).expect("begin at first leaf");

    cursor
        .next(&mut tree, &mut pdf)
        .expect("skip wrong-typed first key in next leaf");

    assert_eq!(cursor.current().map(|(key, _)| key), Some(b"z".to_vec()));
    let diagnostics = pdf.get_warnings();
    let warnings = diagnostics.entries();
    assert_eq!(warnings.len(), 1);
    assert!(String::from_utf8_lossy(warnings[0].get_message_detail())
        .contains("item 0 has the wrong type"));
}

#[test]
fn number_tree_increment_skips_wrong_typed_key_in_next_leaf() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let first_leaf = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(1),
                ObjectHandle::string(b"first".to_vec()),
            ]),
        )]))
        .expect("allocate first number-tree leaf");
    let wrong_type_key = pdf
        .make_indirect_from_object_handle(ObjectHandle::string(b"not-an-integer".to_vec()))
        .expect("allocate indirect wrong-typed number key");
    let next_leaf = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                wrong_type_key,
                ObjectHandle::string(b"invalid".to_vec()),
                ObjectHandle::integer(3),
                ObjectHandle::string(b"last".to_vec()),
            ]),
        )]))
        .expect("allocate next number-tree leaf");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Kids".to_vec(),
            ObjectHandle::array(vec![first_leaf, next_leaf]),
        )]))
        .expect("allocate number-tree root");
    let mut tree = NumberTree::new_with_options(root, false);
    let mut cursor = tree.begin(&mut pdf).expect("begin at first leaf");

    cursor
        .next(&mut tree, &mut pdf)
        .expect("skip wrong-typed first key in next leaf");

    assert_eq!(cursor.current().map(|(key, _)| key), Some(3));
    let diagnostics = pdf.get_warnings();
    let warnings = diagnostics.entries();
    assert_eq!(warnings.len(), 1);
    assert!(String::from_utf8_lossy(warnings[0].get_message_detail())
        .contains("item 0 has the wrong type"));
}

fn assert_name_tree_remove_last_leaf_with_wrong_previous_key(indirect: bool) {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = maybe_make_indirect(&mut pdf, ObjectHandle::integer(7), indirect);
    let previous_leaf = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(b"a".to_vec()),
                ObjectHandle::integer(1),
                wrong_type_key,
                ObjectHandle::integer(2),
            ]),
        )]))
        .expect("allocate previous name-tree leaf");
    let last_leaf = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(b"z".to_vec()),
                ObjectHandle::integer(3),
            ]),
        )]))
        .expect("allocate last name-tree leaf");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Kids".to_vec(),
            ObjectHandle::array(vec![previous_leaf, last_leaf]),
        )]))
        .expect("allocate name-tree root");
    let mut tree = NameTree::new_with_options(root, false);
    let mut cursor = tree
        .last(&mut pdf)
        .expect("position at last name-tree entry");
    assert_eq!(cursor.current().map(|(key, _)| key), Some(b"z".to_vec()));

    cursor
        .remove(&mut tree, &mut pdf)
        .expect("remove last leaf without reading previous malformed key");

    assert!(!cursor.valid());
    assert!(pdf.get_warnings().entries().is_empty());
}

#[test]
fn name_tree_remove_last_leaf_does_not_materialize_direct_previous_key() {
    assert_name_tree_remove_last_leaf_with_wrong_previous_key(false);
}

#[test]
fn name_tree_remove_last_leaf_does_not_materialize_indirect_previous_key() {
    assert_name_tree_remove_last_leaf_with_wrong_previous_key(true);
}

fn assert_number_tree_remove_last_leaf_with_wrong_previous_key(indirect: bool) {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = maybe_make_indirect(
        &mut pdf,
        ObjectHandle::string(b"not-an-integer".to_vec()),
        indirect,
    );
    let previous_leaf = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(1),
                ObjectHandle::string(b"first".to_vec()),
                wrong_type_key,
                ObjectHandle::string(b"invalid".to_vec()),
            ]),
        )]))
        .expect("allocate previous number-tree leaf");
    let last_leaf = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(9),
                ObjectHandle::string(b"last".to_vec()),
            ]),
        )]))
        .expect("allocate last number-tree leaf");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Kids".to_vec(),
            ObjectHandle::array(vec![previous_leaf, last_leaf]),
        )]))
        .expect("allocate number-tree root");
    let mut tree = NumberTree::new_with_options(root, false);
    let mut cursor = tree
        .last(&mut pdf)
        .expect("position at last number-tree entry");
    assert_eq!(cursor.current().map(|(key, _)| key), Some(9));

    cursor
        .remove(&mut tree, &mut pdf)
        .expect("remove last leaf without reading previous malformed key");

    assert!(!cursor.valid());
    assert!(pdf.get_warnings().entries().is_empty());
}

#[test]
fn number_tree_remove_last_leaf_does_not_materialize_direct_previous_key() {
    assert_number_tree_remove_last_leaf_with_wrong_previous_key(false);
}

#[test]
fn number_tree_remove_last_leaf_does_not_materialize_indirect_previous_key() {
    assert_number_tree_remove_last_leaf_with_wrong_previous_key(true);
}

fn assert_name_tree_remove_last_item_with_wrong_previous_key(indirect: bool) {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = maybe_make_indirect(&mut pdf, ObjectHandle::integer(7), indirect);
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![
                wrong_type_key,
                ObjectHandle::integer(1),
                ObjectHandle::string(b"z".to_vec()),
                ObjectHandle::integer(2),
            ]),
        )]))
        .expect("allocate name-tree root");
    let mut tree = NameTree::new_with_options(root, false);
    let mut cursor = tree
        .last(&mut pdf)
        .expect("position at last name-tree entry");
    assert_eq!(cursor.current().map(|(key, _)| key), Some(b"z".to_vec()));

    cursor
        .remove(&mut tree, &mut pdf)
        .expect("remove last item without reading preceding malformed key");

    assert!(!cursor.valid());
    assert!(pdf.get_warnings().entries().is_empty());
}

#[test]
fn name_tree_remove_last_item_does_not_materialize_direct_previous_key() {
    assert_name_tree_remove_last_item_with_wrong_previous_key(false);
}

#[test]
fn name_tree_remove_last_item_does_not_materialize_indirect_previous_key() {
    assert_name_tree_remove_last_item_with_wrong_previous_key(true);
}

fn assert_number_tree_remove_last_item_with_wrong_previous_key(indirect: bool) {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = maybe_make_indirect(
        &mut pdf,
        ObjectHandle::string(b"not-an-integer".to_vec()),
        indirect,
    );
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                wrong_type_key,
                ObjectHandle::string(b"invalid".to_vec()),
                ObjectHandle::integer(9),
                ObjectHandle::string(b"last".to_vec()),
            ]),
        )]))
        .expect("allocate number-tree root");
    let mut tree = NumberTree::new_with_options(root, false);
    let mut cursor = tree
        .last(&mut pdf)
        .expect("position at last number-tree entry");
    assert_eq!(cursor.current().map(|(key, _)| key), Some(9));

    cursor
        .remove(&mut tree, &mut pdf)
        .expect("remove last item without reading preceding malformed key");

    assert!(!cursor.valid());
    assert!(pdf.get_warnings().entries().is_empty());
}

#[test]
fn number_tree_remove_last_item_does_not_materialize_direct_previous_key() {
    assert_number_tree_remove_last_item_with_wrong_previous_key(false);
}

#[test]
fn number_tree_remove_last_item_does_not_materialize_indirect_previous_key() {
    assert_number_tree_remove_last_item_with_wrong_previous_key(true);
}

fn name_tree_with_wrong_successor<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    wrong_type_key: ObjectHandle,
    next_leaf: bool,
) -> NameTree {
    let entries = if next_leaf {
        ObjectHandle::array(vec![
            ObjectHandle::string(b"a".to_vec()),
            ObjectHandle::integer(11),
        ])
    } else {
        ObjectHandle::array(vec![
            ObjectHandle::string(b"a".to_vec()),
            ObjectHandle::integer(1),
            ObjectHandle::string(b"b".to_vec()),
            ObjectHandle::integer(2),
            ObjectHandle::string(b"c".to_vec()),
            ObjectHandle::integer(11),
            wrong_type_key.clone(),
            ObjectHandle::integer(12),
            ObjectHandle::string(b"z".to_vec()),
            ObjectHandle::integer(22),
        ])
    };
    let root = if next_leaf {
        let first_leaf = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![
                (
                    b"/Limits".to_vec(),
                    ObjectHandle::array(vec![
                        ObjectHandle::string(b"a".to_vec()),
                        ObjectHandle::string(b"a".to_vec()),
                    ]),
                ),
                (b"/Names".to_vec(), entries),
            ]))
            .expect("allocate first name-tree leaf");
        let last_leaf = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![
                (
                    b"/Limits".to_vec(),
                    ObjectHandle::array(vec![
                        ObjectHandle::string(b"b".to_vec()),
                        ObjectHandle::string(b"z".to_vec()),
                    ]),
                ),
                (
                    b"/Names".to_vec(),
                    ObjectHandle::array(vec![
                        wrong_type_key,
                        ObjectHandle::integer(12),
                        ObjectHandle::string(b"z".to_vec()),
                        ObjectHandle::integer(22),
                    ]),
                ),
            ]))
            .expect("allocate last name-tree leaf");
        pdf.make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Kids".to_vec(),
            ObjectHandle::array(vec![first_leaf, last_leaf]),
        )]))
        .expect("allocate name-tree root")
    } else {
        pdf.make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            entries,
        )]))
        .expect("allocate name-tree root")
    };
    NameTree::new_with_options(root, false)
}

fn number_tree_with_wrong_successor<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    wrong_type_key: ObjectHandle,
    next_leaf: bool,
) -> NumberTree {
    let entries = if next_leaf {
        ObjectHandle::array(vec![ObjectHandle::integer(1), ObjectHandle::integer(11)])
    } else {
        ObjectHandle::array(vec![
            ObjectHandle::integer(1),
            ObjectHandle::string(b"one".to_vec()),
            ObjectHandle::integer(2),
            ObjectHandle::string(b"two".to_vec()),
            ObjectHandle::integer(3),
            ObjectHandle::string(b"target".to_vec()),
            wrong_type_key.clone(),
            ObjectHandle::string(b"invalid".to_vec()),
            ObjectHandle::integer(9),
            ObjectHandle::string(b"nine".to_vec()),
        ])
    };
    let root = if next_leaf {
        let first_leaf = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![
                (
                    b"/Limits".to_vec(),
                    ObjectHandle::array(vec![ObjectHandle::integer(1), ObjectHandle::integer(1)]),
                ),
                (b"/Nums".to_vec(), entries),
            ]))
            .expect("allocate first number-tree leaf");
        let last_leaf = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![
                (
                    b"/Limits".to_vec(),
                    ObjectHandle::array(vec![ObjectHandle::integer(2), ObjectHandle::integer(9)]),
                ),
                (
                    b"/Nums".to_vec(),
                    ObjectHandle::array(vec![
                        wrong_type_key,
                        ObjectHandle::string(b"invalid".to_vec()),
                        ObjectHandle::integer(9),
                        ObjectHandle::string(b"nine".to_vec()),
                    ]),
                ),
            ]))
            .expect("allocate last number-tree leaf");
        pdf.make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Kids".to_vec(),
            ObjectHandle::array(vec![first_leaf, last_leaf]),
        )]))
        .expect("allocate number-tree root")
    } else {
        pdf.make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            entries,
        )]))
        .expect("allocate number-tree root")
    };
    NumberTree::new_with_options(root, false)
}

fn assert_name_tree_keyed_remove_does_not_materialize_successor(indirect: bool, next_leaf: bool) {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = maybe_make_indirect(&mut pdf, ObjectHandle::integer(7), indirect);
    let mut tree = name_tree_with_wrong_successor(&mut pdf, wrong_type_key, next_leaf);
    let key: &[u8] = if next_leaf { b"a" } else { b"c" };

    let removed = tree
        .remove(&mut pdf, key)
        .expect("keyed removal does not materialize the successor")
        .expect("remove matching entry");

    assert_eq!(removed.try_get_int_value().expect("removed value"), 11);
    assert!(pdf.get_warnings().entries().is_empty());
}

#[test]
fn name_tree_keyed_remove_keeps_malformed_successor_raw() {
    for next_leaf in [false, true] {
        for indirect in [false, true] {
            assert_name_tree_keyed_remove_does_not_materialize_successor(indirect, next_leaf);
        }
    }
}

fn assert_number_tree_keyed_remove_does_not_materialize_successor(indirect: bool, next_leaf: bool) {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = maybe_make_indirect(
        &mut pdf,
        ObjectHandle::string(b"not-an-integer".to_vec()),
        indirect,
    );
    let mut tree = number_tree_with_wrong_successor(&mut pdf, wrong_type_key, next_leaf);
    let key = if next_leaf { 1 } else { 3 };

    let removed = tree
        .remove(&mut pdf, key)
        .expect("keyed removal does not materialize the successor")
        .expect("remove matching entry");

    if next_leaf {
        assert_eq!(removed.try_get_int_value().expect("removed value"), 11);
    } else {
        assert_eq!(
            removed.try_get_string_value().expect("removed value"),
            b"target"
        );
    }
    assert!(pdf.get_warnings().entries().is_empty());
}

#[test]
fn number_tree_keyed_remove_keeps_malformed_successor_raw() {
    for next_leaf in [false, true] {
        for indirect in [false, true] {
            assert_number_tree_keyed_remove_does_not_materialize_successor(indirect, next_leaf);
        }
    }
}

#[test]
fn name_tree_keyed_remove_preserves_short_successor_pair_error() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Names".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(b"a".to_vec()),
                ObjectHandle::integer(11),
                ObjectHandle::string(b"b".to_vec()),
            ]),
        )]))
        .expect("allocate short name-tree root");
    let mut tree = NameTree::new_with_options(root, false);

    let error = match tree.remove(&mut pdf, b"a") {
        Ok(_) => panic!("qpdf rejects the short successor pair after removal"),
        Err(error) => error,
    };

    assert!(matches!(&error, Error::QpdfExc(_)));
    assert!(error
        .to_string()
        .contains("update ivalue: items array is too short"));
    assert!(pdf.get_warnings().entries().is_empty());
}

#[test]
fn number_tree_keyed_remove_preserves_short_successor_pair_error() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let root = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(1),
                ObjectHandle::integer(11),
                ObjectHandle::integer(2),
            ]),
        )]))
        .expect("allocate short number-tree root");
    let mut tree = NumberTree::new_with_options(root, false);

    let error = match tree.remove(&mut pdf, 1) {
        Ok(_) => panic!("qpdf rejects the short successor pair after removal"),
        Err(error) => error,
    };

    assert!(matches!(&error, Error::QpdfExc(_)));
    assert!(error
        .to_string()
        .contains("update ivalue: items array is too short"));
    assert!(pdf.get_warnings().entries().is_empty());
}

fn assert_name_tree_cursor_remove_materializes_successor(indirect: bool, next_leaf: bool) {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = maybe_make_indirect(&mut pdf, ObjectHandle::integer(7), indirect);
    let mut tree = name_tree_with_wrong_successor(&mut pdf, wrong_type_key, next_leaf);
    let mut cursor = if next_leaf {
        tree.begin(&mut pdf).expect("begin at left leaf")
    } else {
        tree.find(&mut pdf, b"c")
            .expect("find same-leaf removal target")
    };
    let removal = cursor.remove(&mut tree, &mut pdf);

    if indirect {
        removal.expect("indirect wrong key uses qpdf's default value");
        assert!(cursor.valid());
        let (key, value) = cursor.current().expect("cursor retains wrong-typed item");
        assert!(key.is_empty());
        assert_eq!(value.try_get_int_value().expect("successor value"), 12);
        let diagnostics = pdf.get_warnings();
        let warnings = diagnostics.entries();
        assert_eq!(warnings.len(), 1);
        assert!(String::from_utf8_lossy(warnings[0].get_message_detail())
            .contains("returning empty string"));
    } else {
        let error = removal.expect_err("direct wrong key raises qpdf typeWarning");
        assert!(matches!(&error, Error::QpdfExc(_)));
        assert!(error
            .to_string()
            .contains("operation for string attempted on object of type integer"));
        assert!(pdf.get_warnings().entries().is_empty());
    }
}

#[test]
fn name_tree_cursor_remove_materializes_malformed_successor() {
    for next_leaf in [false, true] {
        for indirect in [false, true] {
            assert_name_tree_cursor_remove_materializes_successor(indirect, next_leaf);
        }
    }
}

fn assert_number_tree_cursor_remove_materializes_successor(indirect: bool, next_leaf: bool) {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let wrong_type_key = maybe_make_indirect(
        &mut pdf,
        ObjectHandle::string(b"not-an-integer".to_vec()),
        indirect,
    );
    let mut tree = number_tree_with_wrong_successor(&mut pdf, wrong_type_key, next_leaf);
    let mut cursor = if next_leaf {
        tree.begin(&mut pdf).expect("begin at left leaf")
    } else {
        tree.find(&mut pdf, 3)
            .expect("find same-leaf removal target")
    };
    let removal = cursor.remove(&mut tree, &mut pdf);

    if indirect {
        removal.expect("indirect wrong key uses qpdf's default value");
        assert!(cursor.valid());
        let (key, value) = cursor.current().expect("cursor retains wrong-typed item");
        assert_eq!(key, 0);
        assert_eq!(
            value.try_get_string_value().expect("successor value"),
            b"invalid"
        );
        let diagnostics = pdf.get_warnings();
        let warnings = diagnostics.entries();
        assert_eq!(warnings.len(), 1);
        assert!(String::from_utf8_lossy(warnings[0].get_message_detail()).contains("returning 0"));
    } else {
        let error = removal.expect_err("direct wrong key raises qpdf typeWarning");
        assert!(matches!(&error, Error::QpdfExc(_)));
        assert!(error
            .to_string()
            .contains("operation for integer attempted on object of type string"));
        assert!(pdf.get_warnings().entries().is_empty());
    }
}

#[test]
fn number_tree_cursor_remove_materializes_malformed_successor() {
    for next_leaf in [false, true] {
        for indirect in [false, true] {
            assert_number_tree_cursor_remove_materializes_successor(indirect, next_leaf);
        }
    }
}
