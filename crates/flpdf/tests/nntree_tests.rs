use flpdf::{Dictionary, NameTree, NumberTree, Object, ObjectRef, Pdf};
use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::process::Command;

fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = BTreeMap::new();
    let max_object = objects.iter().map(|(number, _)| *number).max().unwrap_or(0);

    for (number, body) in objects {
        offsets.insert(*number, bytes.len() as u64);
        bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }

    let xref_offset = bytes.len() as u64;
    let size = max_object + 1;
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..=max_object {
        match offsets.get(&number) {
            Some(offset) => bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}

fn qpdf_11_9_available() -> bool {
    Command::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("qpdf version 11.9.0")
        })
        .unwrap_or(false)
}

fn canonical_name_tree_probe_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 4 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                4,
                "<< /Limits [(alpha) (beta)] /Names [(alpha) (A) (beta) (B)] >>",
            ),
        ],
        1,
    )
}

fn malformed_name_tree_probe_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 4 0 R >> /Outlines 5 0 R >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Kids [7 0 R] >>"),
            (5, "<< /Type /Outlines /First 6 0 R /Last 6 0 R /Count 1 >>"),
            (6, "<< /Title (bad) /Parent 5 0 R /Dest (m) >>"),
            (7, "<< /Names [42 [3 0 R /Fit] (m) [3 0 R /Fit]] >>"),
        ],
        1,
    )
}

#[test]
fn canonical_name_tree_probe_matches_qpdf_structure_and_output_bytes() {
    if !qpdf_11_9_available() {
        eprintln!("qpdf 11.9.0 not available; skipping NNTree differential probe");
        return;
    }

    let bytes = canonical_name_tree_probe_pdf();
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("open probe PDF");
    let mut tree = NameTree::new(Object::Reference(ObjectRef::new(4, 0)), true);
    assert_eq!(
        tree.as_map(&mut pdf).expect("read canonical name tree"),
        BTreeMap::from([
            (b"alpha".to_vec(), Object::String(b"A".to_vec())),
            (b"beta".to_vec(), Object::String(b"B".to_vec())),
        ])
    );

    let mut input = tempfile::NamedTempFile::new().expect("create qpdf probe input");
    input.write_all(&bytes).expect("write qpdf probe input");

    let check = Command::new("qpdf")
        .arg("--check")
        .arg(input.path())
        .output()
        .expect("run qpdf --check");
    assert!(
        check.status.success(),
        "qpdf --check rejected canonical name tree:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(
        check.stderr.is_empty(),
        "canonical name tree should be warning-free in qpdf 11.9.0: {}",
        String::from_utf8_lossy(&check.stderr)
    );

    let shown = Command::new("qpdf")
        .arg("--show-object=4")
        .arg(input.path())
        .output()
        .expect("run qpdf --show-object");
    assert!(
        shown.status.success(),
        "qpdf --show-object failed: {shown:?}"
    );
    let shown = String::from_utf8_lossy(&shown.stdout);
    for fragment in ["/Limits", "/Names", "(alpha)", "(beta)", "(A)", "(B)"] {
        assert!(
            shown.contains(fragment),
            "qpdf structural output is missing {fragment:?}: {shown}"
        );
    }

    let qdf = Command::new("qpdf")
        .args(["--qdf", "--object-streams=disable"])
        .arg(input.path())
        .arg("-")
        .output()
        .expect("run qpdf --qdf");
    assert!(qdf.status.success(), "qpdf --qdf failed: {qdf:?}");
    for fragment in [
        b"/Limits".as_slice(),
        b"/Names".as_slice(),
        b"(alpha)".as_slice(),
    ] {
        assert!(
            qdf.stdout
                .windows(fragment.len())
                .any(|window| window == fragment),
            "qpdf QDF output is missing byte fragment {fragment:?}"
        );
    }
}

#[test]
fn canonical_name_tree_probe_matches_qpdf_warning_context() {
    if !qpdf_11_9_available() {
        eprintln!("qpdf 11.9.0 not available; skipping NNTree warning probe");
        return;
    }

    let bytes = malformed_name_tree_probe_pdf();
    let mut input = tempfile::NamedTempFile::new().expect("create malformed qpdf probe input");
    input
        .write_all(&bytes)
        .expect("write malformed qpdf probe input");
    let qpdf = Command::new("qpdf")
        .args(["--json=2", "--json-key=outlines"])
        .arg(input.path())
        .output()
        .expect("run qpdf warning probe");
    assert_eq!(qpdf.status.code(), Some(2), "qpdf warning probe: {qpdf:?}");
    let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr);
    for fragment in [
        "attempting to repair after error",
        "item at index 0 is not the right type",
    ] {
        assert!(
            qpdf_stderr.contains(fragment),
            "qpdf warning output is missing {fragment:?}: {qpdf_stderr}"
        );
    }

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open malformed probe PDF");
    // qpdf's `--json=2 --json-key=outlines` above exits 2 while building the
    // JSON `dest` field, not while constructing the outline document helper
    // (`QPDFOutlineDocumentHelper`'s constructor never touches `/Dest`).
    // `get_tree()` mirrors that constructor and so must succeed here too;
    // the malformed named-tree lookup only runs, and only then can fail,
    // when `dest()` is actually called on the item.
    let mut helper = pdf.outline();
    let tree = helper.get_tree().expect("get_tree never touches /Dest");
    let item = tree[tree.roots()[0]].clone();
    let error = item
        .get_dest(&mut helper)
        .expect_err("malformed name tree must fail through the same consumer as qpdf");
    assert!(
        error
            .to_string()
            .contains("item at index 0 is not the right type"),
        "flpdf error diverged from qpdf warning context: {error}"
    );
    let diagnostics = pdf.repair_diagnostics();
    let flpdf_warning = diagnostics
        .entries()
        .iter()
        .find(|diagnostic| diagnostic.message.contains("node is missing /Limits"))
        .map(|diagnostic| diagnostic.message.as_str())
        .expect("flpdf must emit the qpdf repair warning");
    assert!(
        flpdf_warning.contains("Name/Number tree node (object 4)")
            && flpdf_warning.contains("Name/Number tree node (object 7)"),
        "flpdf repair warning lost qpdf object context: {flpdf_warning}"
    );
    for fragment in [
        "Name/Number tree node (object 4)",
        "Name/Number tree node (object 7)",
        "node is missing /Limits",
    ] {
        assert!(
            qpdf_stderr.contains(fragment),
            "qpdf warning output is missing the shared context {fragment:?}: {qpdf_stderr}"
        );
    }
}

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
fn number_tree_set_max_depth_bounds_kids_chain_traversal() {
    let mut pdf = empty_pdf();
    let leaf_ref = ObjectRef::new(3, 0);
    let mut leaf = Dictionary::new();
    leaf.insert(
        "Nums",
        Object::Array(vec![Object::Integer(0), Object::String(b"zero".to_vec())]),
    );
    leaf.insert(
        "Limits",
        Object::Array(vec![Object::Integer(0), Object::Integer(0)]),
    );
    pdf.set_object(leaf_ref, Object::Dictionary(leaf));

    let branch_ref = ObjectRef::new(2, 0);
    let mut branch = Dictionary::new();
    branch.insert("Kids", Object::Array(vec![Object::Reference(leaf_ref)]));
    branch.insert(
        "Limits",
        Object::Array(vec![Object::Integer(0), Object::Integer(0)]),
    );
    pdf.set_object(branch_ref, Object::Dictionary(branch));

    let mut root = Dictionary::new();
    root.insert("Kids", Object::Array(vec![Object::Reference(branch_ref)]));

    let mut bounded = NumberTree::new(Object::Dictionary(root.clone()), true);
    bounded.set_max_depth(1);
    let error = bounded
        .find_object(&mut pdf, 0)
        .expect_err("a 1-level cap must reject a 2-level /Kids chain");
    assert!(
        error.to_string().contains("depth limit"),
        "unexpected error: {error}"
    );

    let mut unbounded_enough = NumberTree::new(Object::Dictionary(root), true);
    unbounded_enough.set_max_depth(flpdf::DEFAULT_MAX_TREE_DEPTH);
    assert_eq!(
        unbounded_enough
            .find_object(&mut pdf, 0)
            .expect("within the default depth cap"),
        Some(Object::String(b"zero".to_vec()))
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
        pdf.resolve_object(root_ref).expect("resolve root"),
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
        pdf.resolve_object(root_ref).expect("resolve root"),
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
fn number_tree_threshold_two_keeps_all_entries_across_internal_split() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Nums", Object::Array(Vec::new()));
    let mut tree = NumberTree::new(Object::Dictionary(root), true);
    tree.set_split_threshold(2);

    for key in 0..5 {
        tree.insert(&mut pdf, key, Object::Integer(key))
            .expect("insert must survive an internal split");
    }

    assert_eq!(
        tree.as_map(&mut pdf).expect("map"),
        BTreeMap::from([
            (0, Object::Integer(0)),
            (1, Object::Integer(1)),
            (2, Object::Integer(2)),
            (3, Object::Integer(3)),
            (4, Object::Integer(4)),
        ])
    );
}

#[test]
fn number_tree_split_allocation_failure_leaves_tree_unchanged() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Nums", Object::Array(Vec::new()));
    let mut tree = NumberTree::new(Object::Dictionary(root), true);
    tree.set_split_threshold(2);
    tree.insert(&mut pdf, 0, Object::Integer(0))
        .expect("insert zero");
    tree.insert(&mut pdf, 1, Object::Integer(1))
        .expect("insert one");
    pdf.set_object(ObjectRef::new(u32::MAX - 1, 0), Object::Null);
    let before = tree.as_map(&mut pdf).expect("map before failed insert");

    let error = match tree.insert(&mut pdf, 2, Object::Integer(2)) {
        Err(error) => error,
        Ok(_) => panic!("root split needs two objects but only one remains"),
    };

    assert_eq!(
        error.to_string(),
        "unsupported PDF feature: max object id is too high to create new objects"
    );
    assert_eq!(
        tree.as_map(&mut pdf).expect("map after failed insert"),
        before
    );
    assert!(!pdf.object_refs().contains(&ObjectRef::new(u32::MAX, 0)));
}

#[test]
fn canonical_name_reader_returns_qpdf_normalized_utf8_key() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert(
        "Names",
        Object::Array(vec![Object::String(vec![0x80]), Object::Integer(7)]),
    );

    let mut tree = NameTree::new(Object::Dictionary(root), false);
    let entries = tree.as_map(&mut pdf).expect("read");

    assert_eq!(entries.get("•".as_bytes()), Some(&Object::Integer(7)));
}

#[test]
fn canonical_number_reader_accepts_direct_kid() {
    let mut pdf = empty_pdf();
    let mut kid = Dictionary::new();
    kid.insert(
        "Nums",
        Object::Array(vec![Object::Integer(4), Object::String(b"four".to_vec())]),
    );
    let mut root = Dictionary::new();
    root.insert("Kids", Object::Array(vec![Object::Dictionary(kid)]));

    let mut tree = NumberTree::new(Object::Dictionary(root), false);
    let entries = tree.as_map(&mut pdf).expect("read");

    assert_eq!(entries.get(&4), Some(&Object::String(b"four".to_vec())));
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

#[test]
fn cursors_reject_other_trees_and_invalid_remove() {
    let mut pdf = empty_pdf();
    let mut left_root = Dictionary::new();
    left_root.insert(
        "Names",
        Object::Array(vec![Object::String(b"left".to_vec()), Object::Integer(1)]),
    );
    let mut right_root = Dictionary::new();
    right_root.insert(
        "Names",
        Object::Array(vec![Object::String(b"right".to_vec()), Object::Integer(2)]),
    );
    let mut left = NameTree::new(Object::Dictionary(left_root), true);
    let mut right = NameTree::new(Object::Dictionary(right_root), true);
    let mut left_cursor = left.begin(&mut pdf).expect("left cursor");

    assert!(left_cursor.next(&mut right, &mut pdf).is_err());
    assert!(left_cursor.remove(&mut right, &mut pdf).is_err());
    assert_eq!(
        right.find_object(&mut pdf, b"right").expect("right lookup"),
        Some(Object::Integer(2))
    );
    assert!(left.end().remove(&mut left, &mut pdf).is_err());

    let mut number_root = Dictionary::new();
    number_root.insert(
        "Nums",
        Object::Array(vec![Object::Integer(1), Object::Integer(10)]),
    );
    let mut numbers = NumberTree::new(Object::Dictionary(number_root), true);
    let mut other_numbers = NumberTree::new(Object::Dictionary(Dictionary::new()), true);
    let mut number_cursor = numbers.begin(&mut pdf).expect("number cursor");
    assert!(number_cursor.next(&mut other_numbers, &mut pdf).is_err());
    assert!(numbers.end().remove(&mut numbers, &mut pdf).is_err());
}
