//! Compatibility functions for name and number trees.
//!
//! Existing callers keep the original free-function API, while all structural
//! reads and writes are forwarded to the shared [`crate::NameTree`] and
//! [`crate::NumberTree`] component. Catalog wiring, `/AF` upkeep, GC, and
//! consumer-specific decoding stay in the caller.

use crate::{Object, ObjectRef, Pdf, Result};
use std::io::{Read, Seek};

/// Default `/Kids` descent depth limit (cyclic / maliciously deep guard).
pub const DEFAULT_MAX_TREE_DEPTH: usize = 100;

/// Max entries in a single leaf before [`build_name_tree`] splits into a
/// `/Kids` root (mirrors qpdf's aggressive rebuild threshold).
pub const LEAF_MAX: usize = 32;

/// Enumerate a **name** tree rooted at `root` (a `/Kids` root node reference,
/// or an inline node dictionary), decoding each value via `decode`.
///
/// Entries are returned in depth-first order (the spec mandates keys be sorted).
/// `decode` returning `Ok(None)` skips that entry. Malformed key/value arrays
/// follow the typed helper's qpdf cursor semantics: an unpositionable first
/// key may produce an empty result; after a complete pair, a dangling final
/// item is warned about and skipped.
///
/// # Errors
/// Propagates [`Pdf::resolve_borrowed`] (indirect-object resolution) errors and
/// returns [`crate::Error::Unsupported`] if a `/Kids` chain reaches `max_depth`.
pub fn read_name_tree<R, V, F>(
    pdf: &mut Pdf<R>,
    root: Object,
    mut decode: F,
    max_depth: usize,
) -> Result<Vec<(Vec<u8>, V)>>
where
    R: Read + Seek,
    F: FnMut(&mut Pdf<R>, Object) -> Result<Option<V>>,
{
    let mut tree = crate::NameTree::new(root, false);
    tree.set_max_depth(max_depth);
    let entries = tree.as_map(pdf)?;
    let mut out = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        if let Some(value) = decode(pdf, value)? {
            out.push((key, value));
        }
    }
    Ok(out)
}

/// Enumerate a **number** tree rooted at `root` (a `/Kids` root node reference,
/// or an inline node dictionary), decoding each value via `decode`.
///
/// Same malformed-tree and decoding semantics as [`read_name_tree`], but with
/// `/Nums` leaves and integer keys.
///
/// # Errors
/// Propagates [`Pdf::resolve_borrowed`] (indirect-object resolution) errors and
/// returns [`crate::Error::Unsupported`] if a `/Kids` chain reaches `max_depth`.
pub fn read_number_tree<R, V, F>(
    pdf: &mut Pdf<R>,
    root: Object,
    mut decode: F,
    max_depth: usize,
) -> Result<Vec<(i64, V)>>
where
    R: Read + Seek,
    F: FnMut(&mut Pdf<R>, Object) -> Result<Option<V>>,
{
    let mut tree = crate::NumberTree::new(root, false);
    tree.set_max_depth(max_depth);
    let entries = tree.as_map(pdf)?;
    let mut out = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        if let Some(value) = decode(pdf, value)? {
            out.push((key, value));
        }
    }
    Ok(out)
}

/// Build a name-tree from a **non-empty, pre-sorted** `(key, value)` slice.
///
/// Returns `(root_ref, nodes)` where `nodes` is every `(ObjectRef, Object)` the
/// caller must store via [`Pdf::set_object`]. The caller owns object numbering
/// (via `alloc`), the empty-entries case, and all catalog wiring.
///
/// Layout (qpdf-aligned). Per ISO 32000-2 §7.9.6, `/Limits` is carried by
/// intermediate and leaf nodes only; the root node omits it:
/// - `<= LEAF_MAX` entries → a single root node holding `/Names` only (no
///   `/Limits`), returned as the root.
/// - `> LEAF_MAX` entries → leaves chunked by `div_ceil`, each `/Limits` +
///   `/Names`, under a root carrying `/Kids` only (no `/Limits`). Leaves are
///   allocated in order, the root last.
///
/// The number-tree analogue is [`build_number_tree`]; the two share this exact
/// chunking/allocation layout and must be kept in sync (only the key encoding
/// and the `/Names` vs `/Nums` leaf key differ).
///
/// # Panics (debug)
/// Debug-asserts `entries` is non-empty.
pub fn build_name_tree<A>(
    entries: &[(Vec<u8>, Object)],
    alloc: A,
) -> (ObjectRef, Vec<(ObjectRef, Object)>)
where
    A: FnMut() -> ObjectRef,
{
    crate::nntree::build_name_tree_compat(entries, alloc)
}

/// Build a **number** tree from a **non-empty, pre-sorted** `(key, value)` slice.
///
/// Number-tree analogue of [`build_name_tree`]: identical layout per ISO
/// 32000-2 §7.9.7. For `<= LEAF_MAX` entries the root holds `/Nums` only (no
/// `/Limits`); otherwise `div_ceil`-chunked leaves (each `/Limits` + `/Nums`,
/// **integer** `/Limits`) sit under a root carrying `/Kids` only (no
/// `/Limits`), leaves allocated first then the root. Returns `(root_ref,
/// nodes)` for the caller to [`Pdf::set_object`]; the caller owns numbering,
/// the empty case, and catalog wiring.
///
/// # Panics (debug)
/// Debug-asserts `entries` is non-empty.
pub fn build_number_tree<A>(
    entries: &[(i64, Object)],
    alloc: A,
) -> (ObjectRef, Vec<(ObjectRef, Object)>)
where
    A: FnMut() -> ObjectRef,
{
    crate::nntree::build_number_tree_compat(entries, alloc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Dictionary;

    // Shared decode hooks, reused across tests so each body is defined (and
    // covered) once instead of as a fresh inline closure per call site.
    fn ref_only<R: Read + Seek>(_: &mut Pdf<R>, v: Object) -> Result<Option<ObjectRef>> {
        Ok(v.as_ref_id())
    }
    fn verbatim<R: Read + Seek>(_: &mut Pdf<R>, v: Object) -> Result<Option<Object>> {
        Ok(Some(v))
    }

    #[test]
    fn read_name_tree_verbatim_passes_value_through() {
        // The verbatim decode keeps each leaf value as-is (used by the raw
        // collector view).
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Names",
            Object::Array(vec![Object::String(b"k".to_vec()), Object::Integer(7)]),
        );
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            verbatim,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(b"k".to_vec(), Object::Integer(7))]);
    }

    fn empty_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        // Minimal valid PDF; the readers don't need a real catalog because we
        // pass nodes directly via set_object refs.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(std::io::Cursor::new(bytes)).expect("open")
    }

    #[test]
    fn read_name_tree_inline_leaf_ref_only() {
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"a".to_vec()),
                Object::Reference(ObjectRef::new(10, 0)),
                Object::String(b"b".to_vec()),
                Object::Reference(ObjectRef::new(11, 0)),
            ]),
        );
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            ref_only,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(
            out,
            vec![
                (b"a".to_vec(), ObjectRef::new(10, 0)),
                (b"b".to_vec(), ObjectRef::new(11, 0)),
            ]
        );
    }

    #[test]
    fn read_name_tree_skips_when_decode_none() {
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"a".to_vec()),
                Object::Integer(5), // not a ref -> decode returns None -> skipped
                Object::String(b"b".to_vec()),
                Object::Reference(ObjectRef::new(11, 0)),
            ]),
        );
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            ref_only,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(b"b".to_vec(), ObjectRef::new(11, 0))]);
    }

    #[test]
    fn read_name_tree_descends_kids_via_reference() {
        let mut pdf = empty_pdf();
        // Leaf object at ref 20.
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"k".to_vec()),
                Object::Reference(ObjectRef::new(99, 0)),
            ]),
        );
        let leaf_ref = ObjectRef::new(20, 0);
        pdf.set_object(leaf_ref, Object::Dictionary(leaf));
        // Root with /Kids -> [20 0 R].
        let mut root = Dictionary::new();
        root.insert("Kids", Object::Array(vec![Object::Reference(leaf_ref)]));
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(root),
            ref_only,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(b"k".to_vec(), ObjectRef::new(99, 0))]);
    }

    #[test]
    fn read_name_tree_descends_kids_via_holder_chain() {
        let mut pdf = empty_pdf();
        // Leaf node at obj 20.
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"k".to_vec()),
                Object::Reference(ObjectRef::new(99, 0)),
            ]),
        );
        pdf.set_object(ObjectRef::new(20, 0), Object::Dictionary(leaf));
        // Holder: obj 21 is a bare reference to obj 20 (ref → ref → node).
        pdf.set_object(
            ObjectRef::new(21, 0),
            Object::Reference(ObjectRef::new(20, 0)),
        );
        // Root /Kids -> [21 0 R]; 21 is a holder chain, not a direct node.
        let mut root = Dictionary::new();
        root.insert(
            "Kids",
            Object::Array(vec![Object::Reference(ObjectRef::new(21, 0))]),
        );
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(root),
            ref_only,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(b"k".to_vec(), ObjectRef::new(99, 0))]);
    }

    #[test]
    fn read_name_tree_dedups_distinct_holders_to_same_terminal() {
        // Two distinct holder refs (21, 22) both resolve to the same terminal
        // leaf node (20). This is the holder-form of a direct `[20 0 R 20 0 R]`
        // duplicate: the shared child must be walked once, not once per holder.
        // Keying `visited` on the holder ref would emit the leaf entry twice.
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"k".to_vec()),
                Object::Reference(ObjectRef::new(99, 0)),
            ]),
        );
        pdf.set_object(ObjectRef::new(20, 0), Object::Dictionary(leaf));
        // Holders 21 and 22 are bare references to the same terminal node 20.
        pdf.set_object(
            ObjectRef::new(21, 0),
            Object::Reference(ObjectRef::new(20, 0)),
        );
        pdf.set_object(
            ObjectRef::new(22, 0),
            Object::Reference(ObjectRef::new(20, 0)),
        );
        let mut root = Dictionary::new();
        root.insert(
            "Kids",
            Object::Array(vec![
                Object::Reference(ObjectRef::new(21, 0)),
                Object::Reference(ObjectRef::new(22, 0)),
            ]),
        );
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(root),
            ref_only,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        // Exactly one entry: the second holder collapses onto the terminal ref.
        assert_eq!(out, vec![(b"k".to_vec(), ObjectRef::new(99, 0))]);
    }

    #[test]
    fn read_name_tree_accepts_inline_dict_kid() {
        // The qpdf helper descends a direct kid while warning that it should be
        // indirect when auto-repair is disabled.
        let mut pdf = empty_pdf();
        let mut inline_leaf = Dictionary::new();
        inline_leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"k".to_vec()),
                Object::Reference(ObjectRef::new(99, 0)),
            ]),
        );
        let mut root = Dictionary::new();
        root.insert("Kids", Object::Array(vec![Object::Dictionary(inline_leaf)]));
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(root),
            ref_only,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(b"k".to_vec(), ObjectRef::new(99, 0))]);
    }

    #[test]
    fn read_name_tree_cycle_terminates() {
        let mut pdf = empty_pdf();
        // Node 30 has /Kids -> [30 0 R] (self-cycle).
        let mut node = Dictionary::new();
        let node_ref = ObjectRef::new(30, 0);
        node.insert("Kids", Object::Array(vec![Object::Reference(node_ref)]));
        pdf.set_object(node_ref, Object::Dictionary(node));
        let out = read_name_tree(
            &mut pdf,
            Object::Reference(node_ref),
            ref_only,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn read_name_tree_depth_limit_errors() {
        let mut pdf = empty_pdf();
        // Chain of /Kids deeper than the limit.
        // 40 -> 41 -> 42 ...; with max_depth=2 the third level errors.
        let r40 = ObjectRef::new(40, 0);
        let r41 = ObjectRef::new(41, 0);
        let r42 = ObjectRef::new(42, 0);
        for (this, next) in [(r40, r41), (r41, r42)] {
            let mut d = Dictionary::new();
            d.insert("Kids", Object::Array(vec![Object::Reference(next)]));
            pdf.set_object(this, Object::Dictionary(d));
        }
        let mut leaf = Dictionary::new();
        leaf.insert("Names", Object::Array(vec![]));
        pdf.set_object(r42, Object::Dictionary(leaf));
        let err = read_name_tree(&mut pdf, Object::Reference(r40), verbatim, 2);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn read_number_tree_resolves_indirect_dict_value() {
        let mut pdf = empty_pdf();
        // Value at ref 50 is a label dict.
        let mut label = Dictionary::new();
        label.insert("S", Object::Name("D".into()));
        let label_ref = ObjectRef::new(50, 0);
        pdf.set_object(label_ref, Object::Dictionary(label));
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Nums",
            Object::Array(vec![
                Object::Integer(0),
                Object::Reference(label_ref), // indirect value -> resolve
                Object::Integer(5),
                Object::Dictionary({
                    let mut d = Dictionary::new();
                    d.insert("S", Object::Name("R".into()));
                    d
                }),
                Object::Integer(9),
                Object::Name("notadict".into()), // value not dict/ref -> decode _ => None -> skipped
            ]),
        );
        let out: Vec<(i64, Dictionary)> = read_number_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            |pdf, v| match v {
                Object::Dictionary(d) => Ok(Some(d)),
                Object::Reference(r) => Ok(pdf.resolve_borrowed(r)?.as_dict().cloned()),
                _ => Ok(None),
            },
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, 0);
        assert_eq!(out[0].1.get("S"), Some(&Object::Name("D".into())));
        assert_eq!(out[1].0, 5);
        assert_eq!(out[1].1.get("S"), Some(&Object::Name("R".into())));
    }

    #[test]
    fn read_number_tree_invalid_first_key_yields_no_entries() {
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Nums",
            Object::Array(vec![
                Object::Name("oops".into()),
                Object::Integer(1),
                Object::Integer(7),
                Object::Integer(2),
            ]),
        );
        let out: Vec<(i64, i64)> = read_number_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            |_, v| Ok(v.as_integer()),
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert!(out.is_empty());
    }

    fn mk_entries(n: usize) -> Vec<(Vec<u8>, Object)> {
        (0..n)
            .map(|i| {
                (
                    format!("{i:03}").into_bytes(),
                    Object::Reference(ObjectRef::new(1000 + i as u32, 0)),
                )
            })
            .collect()
    }

    #[test]
    fn build_name_tree_single_leaf_no_kids() {
        let entries = mk_entries(3);
        let mut next = 0u32;
        let (root, nodes) = build_name_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        assert_eq!(nodes.len(), 1);
        assert_eq!(root, nodes[0].0);
        let d = nodes[0].1.as_dict().expect("leaf dict");
        assert!(d.get("Kids").is_none(), "single leaf must not have /Kids");
        assert!(d.get("Names").is_some());
        assert!(
            d.get("Limits").is_none(),
            "single-node root omits /Limits (ISO 32000-2 7.9.6; qpdf)"
        );
    }

    #[test]
    fn build_name_tree_multi_leaf_root_kids_alloc_order() {
        let entries = mk_entries(LEAF_MAX + 1); // 33 -> 2 leaves + root
        let mut next = 0u32;
        let (root, nodes) = build_name_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        // Leaves allocated first (1,2), root last (3).
        assert_eq!(nodes.len(), 3);
        assert_eq!(root, ObjectRef::new(3, 0), "root allocated last");
        let root_dict = nodes[2].1.as_dict().expect("root dict");
        let kids = root_dict
            .get("Kids")
            .and_then(Object::as_array)
            .expect("root needs /Kids");
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0], Object::Reference(ObjectRef::new(1, 0)));
        assert_eq!(kids[1], Object::Reference(ObjectRef::new(2, 0)));
        // Root omits /Limits (ISO 32000-2 7.9.6); both leaves keep it.
        assert!(
            root_dict.get("Limits").is_none(),
            "multi-node root omits /Limits"
        );
        for (_, leaf) in &nodes[..2] {
            let lim = leaf
                .as_dict()
                .unwrap()
                .get("Limits")
                .and_then(Object::as_array)
                .expect("leaf keeps string /Limits");
            assert!(matches!(lim[0], Object::String(_)));
            assert!(matches!(lim[1], Object::String(_)));
        }
    }

    #[test]
    fn read_name_tree_invalid_first_key_yields_no_entries() {
        // The qpdf iterator treats an invalid first key as an invalid cursor;
        // the compatibility map therefore contains no entries from this leaf.
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::Integer(7), // non-string key -> skip pair
                Object::Reference(ObjectRef::new(10, 0)),
                Object::String(b"ok".to_vec()),
                Object::Reference(ObjectRef::new(11, 0)),
            ]),
        );
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            ref_only,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn read_name_tree_unexpected_root_type_is_empty() {
        // A root that is neither a Dictionary nor a Reference yields no entries.
        let mut pdf = empty_pdf();
        let out = read_name_tree(
            &mut pdf,
            Object::Integer(42),
            verbatim,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn read_name_tree_kid_resolving_to_non_dict_is_skipped() {
        // A /Kids reference resolving to a non-dictionary object is skipped
        // (malformed node), not an error.
        let mut pdf = empty_pdf();
        let bad_ref = ObjectRef::new(60, 0);
        pdf.set_object(bad_ref, Object::Integer(0));
        let mut root = Dictionary::new();
        root.insert("Kids", Object::Array(vec![Object::Reference(bad_ref)]));
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(root),
            verbatim,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn read_name_tree_odd_length_leaf_keeps_the_complete_pair_and_warns() {
        // qpdf increments past the complete pair, then warns and skips the
        // dangling final key.
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"a".to_vec()),
                Object::Reference(ObjectRef::new(10, 0)),
                Object::String(b"orphan".to_vec()), // no value -> dropped
            ]),
        );
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            ref_only,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(b"a".to_vec(), ObjectRef::new(10, 0))]);
        assert_eq!(
            pdf.repair_diagnostics().entries()[0].message,
            "Name/Number tree node: items array doesn't have enough elements"
        );
    }

    #[test]
    fn read_name_tree_node_without_names_or_kids_is_empty() {
        // A node carrying neither /Names nor /Kids contributes nothing; the walk
        // falls through to the end of walk_tree.
        let mut pdf = empty_pdf();
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(Dictionary::new()),
            verbatim,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn read_number_tree_depth_limit_errors() {
        // The depth guard fires for number trees too; the error propagates out
        // of read_number_tree via `?`.
        let mut pdf = empty_pdf();
        let r70 = ObjectRef::new(70, 0);
        let r71 = ObjectRef::new(71, 0);
        let mut node = Dictionary::new();
        node.insert("Kids", Object::Array(vec![Object::Reference(r71)]));
        pdf.set_object(r70, Object::Dictionary(node));
        // r71 need not exist: with max_depth=1 the guard fires before resolving it.
        let err: Result<Vec<(i64, Object)>> =
            read_number_tree(&mut pdf, Object::Reference(r70), verbatim, 1);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn read_number_tree_decode_none_skips_value() {
        // A number-tree value the decode hook rejects (returns None) is skipped.
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Nums",
            Object::Array(vec![
                Object::Integer(0),
                Object::Name("notanint".into()), // decode -> None -> skip
                Object::Integer(5),
                Object::Integer(99),
            ]),
        );
        let out: Vec<(i64, i64)> = read_number_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            |_, v| Ok(v.as_integer()),
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(5, 99)]);
    }

    fn mk_num_entries(n: usize) -> Vec<(i64, Object)> {
        (0..n)
            .map(|i| {
                (
                    i as i64 * 10,
                    Object::Reference(ObjectRef::new(1000 + i as u32, 0)),
                )
            })
            .collect()
    }

    #[test]
    fn build_number_tree_single_leaf_no_kids() {
        let entries = mk_num_entries(3);
        let mut next = 0u32;
        let (root, nodes) = build_number_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        assert_eq!(nodes.len(), 1);
        assert_eq!(root, nodes[0].0);
        let d = nodes[0].1.as_dict().expect("leaf dict");
        assert!(d.get("Kids").is_none(), "single leaf must not have /Kids");
        assert!(d.get("Nums").is_some());
        assert!(
            d.get("Limits").is_none(),
            "single-node number root omits /Limits"
        );
    }

    #[test]
    fn build_number_tree_multi_leaf_root_kids_alloc_order() {
        let entries = mk_num_entries(LEAF_MAX + 1); // 33 -> 2 leaves + root
        let mut next = 0u32;
        let (root, nodes) = build_number_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        assert_eq!(nodes.len(), 3);
        assert_eq!(root, ObjectRef::new(3, 0), "root allocated last");
        let root_dict = nodes[2].1.as_dict().expect("root dict");
        let kids = root_dict
            .get("Kids")
            .and_then(Object::as_array)
            .expect("root needs /Kids");
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0], Object::Reference(ObjectRef::new(1, 0)));
        // Root omits /Limits (ISO 32000-2 7.9.7); both leaves keep integer /Limits.
        assert!(
            root_dict.get("Limits").is_none(),
            "multi-node root omits /Limits"
        );
        for (_, leaf) in &nodes[..2] {
            let lim = leaf
                .as_dict()
                .unwrap()
                .get("Limits")
                .and_then(Object::as_array)
                .expect("leaf keeps integer /Limits");
            assert!(matches!(lim[0], Object::Integer(_)));
            assert!(matches!(lim[1], Object::Integer(_)));
        }
    }

    #[test]
    fn build_number_tree_round_trips_via_read_number_tree() {
        let mut pdf = empty_pdf();
        let entries: Vec<(i64, Object)> =
            vec![(0, Object::Integer(100)), (5, Object::Integer(200))];
        let mut next = 500u32;
        let (root, nodes) = build_number_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        for (r, n) in nodes {
            pdf.set_object(r, n);
        }
        let out: Vec<(i64, i64)> = read_number_tree(
            &mut pdf,
            Object::Reference(root),
            |_, v| Ok(v.as_integer()),
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(0, 100), (5, 200)]);
    }

    #[test]
    fn build_number_tree_multi_node_round_trips_via_read_number_tree() {
        // LEAF_MAX + 1 entries force a multi-node tree (2 leaves under a
        // /Kids-only root with no /Limits). Reading back from that root proves
        // the reader traverses a Limits-less root and enumerates every entry in
        // order via /Kids -> /Nums.
        let mut pdf = empty_pdf();
        let entries = mk_num_entries(LEAF_MAX + 1);
        let expected: Vec<(i64, ObjectRef)> = entries
            .iter()
            .map(|(k, v)| (*k, v.as_ref_id().unwrap()))
            .collect();
        let mut next = 0u32;
        let (root, nodes) = build_number_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        assert!(nodes.len() > 1, "expected a multi-node tree");
        assert!(
            nodes[nodes.len() - 1]
                .1
                .as_dict()
                .unwrap()
                .get("Limits")
                .is_none(),
            "multi-node root must omit /Limits"
        );
        for (r, n) in nodes {
            pdf.set_object(r, n);
        }
        let out: Vec<(i64, ObjectRef)> = read_number_tree(
            &mut pdf,
            Object::Reference(root),
            ref_only,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn build_name_tree_multi_node_round_trips_via_read_name_tree() {
        // Name-tree analogue: LEAF_MAX + 1 entries force a /Kids-only root
        // without /Limits. Reading back from the root recovers all entries in
        // order, proving the Limits-less root is traversed via /Kids -> /Names.
        let mut pdf = empty_pdf();
        let entries = mk_entries(LEAF_MAX + 1);
        let expected: Vec<(Vec<u8>, ObjectRef)> = entries
            .iter()
            .map(|(k, v)| (k.clone(), v.as_ref_id().unwrap()))
            .collect();
        let mut next = 0u32;
        let (root, nodes) = build_name_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        assert!(nodes.len() > 1, "expected a multi-node tree");
        assert!(
            nodes[nodes.len() - 1]
                .1
                .as_dict()
                .unwrap()
                .get("Limits")
                .is_none(),
            "multi-node root must omit /Limits"
        );
        for (r, n) in nodes {
            pdf.set_object(r, n);
        }
        let out: Vec<(Vec<u8>, ObjectRef)> = read_name_tree(
            &mut pdf,
            Object::Reference(root),
            ref_only,
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, expected);
    }
}
