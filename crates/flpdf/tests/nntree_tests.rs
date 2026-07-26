use flpdf::{
    read_name_tree, read_number_tree, Dictionary, NameTree, NumberTree, Object, ObjectRef, Pdf,
};
use std::collections::BTreeMap;
use std::io::Cursor;

fn empty_pdf() -> Pdf<Cursor<Vec<u8>>> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"%PDF-1.4\n");
    let off1 = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref = bytes.len() as u64;
    bytes.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \n\
             trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    Pdf::open(Cursor::new(bytes)).expect("open")
}

#[test]
fn number_tree_insert_exposes_value_through_find_object() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Nums", Object::Array(Vec::new()));
    let mut tree = NumberTree::new(Object::Dictionary(root), true);

    tree.insert(&mut pdf, 7, Object::String(b"seven".to_vec()))
        .expect("insert");

    assert_eq!(
        tree.find_object(&mut pdf, 7).expect("find"),
        Some(Object::String(b"seven".to_vec()))
    );
}

#[test]
fn number_tree_end_cursor_moves_to_first_or_last_entry() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Nums", Object::Array(Vec::new()));
    let mut tree = NumberTree::new(Object::Dictionary(root), true);
    tree.insert(&mut pdf, 10, Object::String(b"ten".to_vec()))
        .expect("insert ten");
    tree.insert(&mut pdf, 20, Object::String(b"twenty".to_vec()))
        .expect("insert twenty");

    let mut forward = tree.end();
    assert!(!forward.valid());
    forward.next(&mut tree, &mut pdf).expect("next from end");
    assert_eq!(
        forward.current(),
        Some((10, Object::String(b"ten".to_vec())))
    );

    let mut backward = tree.end();
    backward
        .previous(&mut tree, &mut pdf)
        .expect("previous from end");
    assert_eq!(
        backward.current(),
        Some((20, Object::String(b"twenty".to_vec())))
    );
}

#[test]
fn number_tree_queries_match_qpdf_boundaries() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Nums", Object::Array(Vec::new()));
    let mut tree = NumberTree::new(Object::Dictionary(root), true);
    tree.insert(&mut pdf, -3, Object::String(b"minus-three".to_vec()))
        .expect("insert -3");
    tree.insert(&mut pdf, 6, Object::String(b"six".to_vec()))
        .expect("insert 6");

    assert_eq!(tree.min(&mut pdf).expect("min"), -3);
    assert_eq!(tree.max(&mut pdf).expect("max"), 6);
    assert!(tree.has_index(&mut pdf, -3).expect("has -3"));
    assert!(!tree.has_index(&mut pdf, 5).expect("has 5"));
    assert_eq!(
        tree.find_object_at_or_below(&mut pdf, 5)
            .expect("at or below"),
        Some((Object::String(b"minus-three".to_vec()), 8))
    );
    assert_eq!(
        tree.find_object_at_or_below(&mut pdf, -4)
            .expect("below minimum"),
        None
    );
}

#[test]
fn number_tree_cursor_insert_after_and_remove_advances() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Nums", Object::Array(Vec::new()));
    let mut tree = NumberTree::new(Object::Dictionary(root), true);
    let mut cursor = tree.end();

    cursor
        .insert_after(&mut tree, &mut pdf, 10, Object::String(b"ten".to_vec()))
        .expect("insert ten after end");
    cursor
        .insert_after(&mut tree, &mut pdf, 20, Object::String(b"twenty".to_vec()))
        .expect("insert twenty after ten");
    cursor
        .previous(&mut tree, &mut pdf)
        .expect("move back to ten");
    assert_eq!(
        cursor.current(),
        Some((10, Object::String(b"ten".to_vec())))
    );

    cursor.remove(&mut tree, &mut pdf).expect("remove ten");
    assert_eq!(
        cursor.current(),
        Some((20, Object::String(b"twenty".to_vec())))
    );
}

#[test]
fn name_tree_insert_exposes_value_through_exact_lookup() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Names", Object::Array(Vec::new()));
    let mut tree = NameTree::new(Object::Dictionary(root), true);

    tree.insert(&mut pdf, "café", Object::String(b"destination".to_vec()))
        .expect("insert");

    assert!(tree.has_name(&mut pdf, "café").expect("has name"));
    assert_eq!(
        tree.find_object(&mut pdf, b"caf\xc3\xa9").expect("find"),
        Some(Object::String(b"destination".to_vec()))
    );
}

#[test]
fn name_tree_cursor_insert_after_and_remove_advances() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Names", Object::Array(Vec::new()));
    let mut tree = NameTree::new(Object::Dictionary(root), true);
    let mut cursor = tree.end();

    cursor
        .insert_after(
            &mut tree,
            &mut pdf,
            "alpha",
            Object::String(b"first".to_vec()),
        )
        .expect("insert alpha");
    cursor
        .insert_after(
            &mut tree,
            &mut pdf,
            b"beta",
            Object::String(b"second".to_vec()),
        )
        .expect("insert beta");
    cursor
        .previous(&mut tree, &mut pdf)
        .expect("move back to alpha");
    assert_eq!(
        cursor.current(),
        Some((b"alpha".to_vec(), Object::String(b"first".to_vec())))
    );

    cursor.remove(&mut tree, &mut pdf).expect("remove alpha");
    assert_eq!(
        cursor.current(),
        Some((b"beta".to_vec(), Object::String(b"second".to_vec())))
    );
}

#[test]
fn name_tree_new_empty_owns_an_indirect_root() {
    let mut pdf = empty_pdf();

    let tree = NameTree::new_empty(&mut pdf, true).expect("new empty");
    let root = tree.root().clone();
    let root_ref = root.as_ref_id().expect("indirect root");
    assert_eq!(
        pdf.resolve(root_ref).expect("resolve root"),
        Object::Dictionary({
            let mut dictionary = Dictionary::new();
            dictionary.insert("Names", Object::Array(Vec::new()));
            dictionary
        })
    );

    assert_eq!(tree.into_root(), root);
}

#[test]
fn number_tree_new_empty_owns_an_indirect_root() {
    let mut pdf = empty_pdf();

    let tree = NumberTree::new_empty(&mut pdf, false).expect("new empty");
    let root = tree.root().clone();
    let root_ref = root.as_ref_id().expect("indirect root");
    assert_eq!(
        pdf.resolve(root_ref).expect("resolve root"),
        Object::Dictionary({
            let mut dictionary = Dictionary::new();
            dictionary.insert("Nums", Object::Array(Vec::new()));
            dictionary
        })
    );

    assert_eq!(tree.into_root(), root);
}

#[test]
fn name_tree_helper_exposes_sorted_find_map_and_remove() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Names", Object::Array(Vec::new()));
    let mut tree = NameTree::new(Object::Dictionary(root), true);
    tree.set_split_threshold(2);
    tree.insert(&mut pdf, "beta", Object::Integer(2))
        .expect("insert beta");
    tree.insert(&mut pdf, "alpha", Object::Integer(1))
        .expect("insert alpha");

    assert_eq!(
        tree.begin(&mut pdf).expect("begin").current(),
        Some((b"alpha".to_vec(), Object::Integer(1)))
    );
    assert_eq!(
        tree.last(&mut pdf).expect("last").current(),
        Some((b"beta".to_vec(), Object::Integer(2)))
    );
    assert_eq!(
        tree.find(&mut pdf, "be", true)
            .expect("find previous")
            .current(),
        Some((b"alpha".to_vec(), Object::Integer(1)))
    );
    assert!(!tree
        .find(&mut pdf, "be", false)
        .expect("find exact")
        .valid());
    assert_eq!(
        tree.as_map(&mut pdf).expect("map"),
        BTreeMap::from([
            (b"alpha".to_vec(), Object::Integer(1)),
            (b"beta".to_vec(), Object::Integer(2)),
        ])
    );
    assert_eq!(
        tree.remove(&mut pdf, "beta").expect("remove beta"),
        Some(Object::Integer(2))
    );
    assert_eq!(tree.remove(&mut pdf, "beta").expect("remove missing"), None);
}

#[test]
fn number_tree_helper_exposes_sorted_find_map_and_remove() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Nums", Object::Array(Vec::new()));
    let mut tree = NumberTree::new(Object::Dictionary(root), true);
    tree.set_split_threshold(2);
    tree.insert(&mut pdf, 20, Object::String(b"twenty".to_vec()))
        .expect("insert twenty");
    tree.insert(&mut pdf, 10, Object::String(b"ten".to_vec()))
        .expect("insert ten");

    assert_eq!(
        tree.begin(&mut pdf).expect("begin").current(),
        Some((10, Object::String(b"ten".to_vec())))
    );
    assert_eq!(
        tree.last(&mut pdf).expect("last").current(),
        Some((20, Object::String(b"twenty".to_vec())))
    );
    assert_eq!(
        tree.find(&mut pdf, 19, true)
            .expect("find previous")
            .current(),
        Some((10, Object::String(b"ten".to_vec())))
    );
    assert!(!tree.find(&mut pdf, 19, false).expect("find exact").valid());
    assert_eq!(
        tree.as_map(&mut pdf).expect("map"),
        BTreeMap::from([
            (10, Object::String(b"ten".to_vec())),
            (20, Object::String(b"twenty".to_vec())),
        ])
    );
    assert_eq!(
        tree.remove(&mut pdf, 20).expect("remove twenty"),
        Some(Object::String(b"twenty".to_vec()))
    );
    assert_eq!(tree.remove(&mut pdf, 20).expect("remove missing"), None);
}

#[test]
fn compatibility_name_reader_returns_qpdf_normalized_utf8_key() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert(
        "Names",
        Object::Array(vec![Object::String(vec![0x80]), Object::Integer(7)]),
    );

    let entries = read_name_tree(
        &mut pdf,
        Object::Dictionary(root),
        |_, value| Ok(Some(value)),
        100,
    )
    .expect("read");

    assert_eq!(entries, vec![("•".as_bytes().to_vec(), Object::Integer(7))]);
}

#[test]
fn compatibility_number_reader_accepts_direct_kid() {
    let mut pdf = empty_pdf();
    let mut kid = Dictionary::new();
    kid.insert(
        "Nums",
        Object::Array(vec![Object::Integer(4), Object::String(b"four".to_vec())]),
    );
    let mut root = Dictionary::new();
    root.insert("Kids", Object::Array(vec![Object::Dictionary(kid)]));

    let entries = read_number_tree(
        &mut pdf,
        Object::Dictionary(root),
        |_, value| Ok(Some(value)),
        100,
    )
    .expect("read");

    assert_eq!(entries, vec![(4, Object::String(b"four".to_vec()))]);
}

#[test]
fn typed_cursors_are_cloneable_and_compare_by_qpdf_position() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert(
        "Names",
        Object::Array(vec![
            Object::String(b"a".to_vec()),
            Object::Integer(1),
            Object::String(b"b".to_vec()),
            Object::Integer(2),
        ]),
    );
    let mut tree = NameTree::new(Object::Dictionary(root), true);

    let first = tree.begin(&mut pdf).expect("first");
    let mut copy = first.clone();
    assert!(first == copy);
    copy.next(&mut tree, &mut pdf).expect("next");
    assert!(first != copy);
    assert!(tree.end() == tree.end());

    let mut number_root = Dictionary::new();
    number_root.insert(
        "Nums",
        Object::Array(vec![Object::Integer(3), Object::Integer(30)]),
    );
    let mut number_tree = NumberTree::new(Object::Dictionary(number_root), true);
    let number_first = number_tree.begin(&mut pdf).expect("number first");
    assert!(number_first == number_first.clone());
    assert!(number_first != number_tree.end());
}

#[test]
fn new_empty_reports_exhausted_object_number_space() {
    let mut pdf = empty_pdf();
    pdf.set_object(ObjectRef::new(u32::MAX, 0), Object::Null);

    assert!(NameTree::new_empty(&mut pdf, true).is_err());
    assert!(NumberTree::new_empty(&mut pdf, true).is_err());
}

#[test]
fn number_tree_at_or_below_reports_offset_overflow() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert(
        "Nums",
        Object::Array(vec![
            Object::Integer(i64::MIN),
            Object::String(b"minimum".to_vec()),
        ]),
    );
    let mut tree = NumberTree::new(Object::Dictionary(root), true);

    let error = tree
        .find_object_at_or_below(&mut pdf, i64::MAX)
        .expect_err("offset must overflow");

    assert!(error
        .to_string()
        .contains("number-tree at-or-below offset overflow"));
}
