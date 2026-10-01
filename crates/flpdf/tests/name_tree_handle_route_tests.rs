use std::path::Path;

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
    let mut tree = NameTree::new(root, false);

    let error = match tree.find(&mut pdf, b"probe", false) {
        Ok(_) => panic!("qpdf rejects a wrong-typed key during find"),
        Err(error) => error,
    };

    assert_invalid_tree_key_error(error);
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
    let mut tree = NumberTree::new(root, false);

    let error = match tree.find(&mut pdf, 1, false) {
        Ok(_) => panic!("qpdf rejects a wrong-typed key during find"),
        Err(error) => error,
    };

    assert_invalid_tree_key_error(error);
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
    let mut tree = NameTree::new(root, false);

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
    let mut tree = NumberTree::new(root, false);

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
    let mut tree = NameTree::new(root, false);

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
    let mut tree = NameTree::new(root, false);

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
    let mut tree = NumberTree::new(root, false);

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
    let mut tree = NameTree::new(root, false);
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
    let mut tree = NumberTree::new(root, false);
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
