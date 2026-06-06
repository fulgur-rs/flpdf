//! Generic name-tree / number-tree iteration (ISO 32000-1 §7.9.6 / §7.9.7).
//!
//! Name trees (`/Names` leaf, string keys) and number trees (`/Nums` leaf,
//! integer keys) share the same shape: `/Kids` intermediate nodes, an optional
//! `/Limits [least greatest]` array, depth-first key-ascending order, and the
//! need for depth + cycle guards against hostile or cyclic `/Kids` chains.
//!
//! [`read_name_tree`] / [`read_number_tree`] enumerate a tree, decoding each
//! value via a caller-supplied hook (generic over the value type, so the same
//! walker serves verbatim-`Object`, reference-only, and resolved-`Dictionary`
//! views). [`build_name_tree`] rebuilds a name tree from sorted entries.
//!
//! This module owns only structural concerns (parse + build). Catalog wiring,
//! `/AF` upkeep, GC, and prune-during-walk stay in the consumer.

use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
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
/// `decode` returning `Ok(None)` skips that entry; non-string keys and the
/// trailing orphan of an odd-length leaf array are dropped silently.
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
    let mut out = Vec::new();
    let mut visited = BTreeSet::new();
    walk_tree(
        pdf,
        root,
        "Names",
        &|o| match o {
            Object::String(b) => Some(b),
            _ => None,
        },
        &mut decode,
        &mut out,
        &mut visited,
        0,
        max_depth,
    )?;
    Ok(out)
}

/// Enumerate a **number** tree rooted at `root` (a `/Kids` root node reference,
/// or an inline node dictionary), decoding each value via `decode`.
///
/// Same semantics as [`read_name_tree`] but with `/Nums` leaves and integer
/// keys; non-integer keys are skipped.
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
    let mut out = Vec::new();
    let mut visited = BTreeSet::new();
    walk_tree(
        pdf,
        root,
        "Nums",
        &|o| match o {
            Object::Integer(n) => Some(n),
            _ => None,
        },
        &mut decode,
        &mut out,
        &mut visited,
        0,
        max_depth,
    )?;
    Ok(out)
}

/// Build a name-tree from a **non-empty, pre-sorted** `(key, value)` slice.
///
/// Returns `(root_ref, nodes)` where `nodes` is every `(ObjectRef, Object)` the
/// caller must store via [`Pdf::set_object`]. The caller owns object numbering
/// (via `alloc`), the empty-entries case, and all catalog wiring.
///
/// Layout (qpdf-aligned, identical to the legacy embedded-files writer):
/// - `<= LEAF_MAX` entries → a single leaf node (`/Limits` + `/Names`), returned
///   as the root.
/// - `> LEAF_MAX` entries → leaves chunked by `div_ceil`, each `/Limits` +
///   `/Names`, under a root `/Limits` + `/Kids`. Leaves are allocated in order,
///   the root last.
///
/// # Panics (debug)
/// Debug-asserts `entries` is non-empty.
pub fn build_name_tree<A>(
    entries: &[(Vec<u8>, Object)],
    mut alloc: A,
) -> (ObjectRef, Vec<(ObjectRef, Object)>)
where
    A: FnMut() -> ObjectRef,
{
    debug_assert!(
        !entries.is_empty(),
        "build_name_tree requires non-empty entries"
    );
    let mut nodes: Vec<(ObjectRef, Object)> = Vec::new();

    if entries.len() <= LEAF_MAX {
        let leaf_ref = alloc();
        nodes.push((leaf_ref, Object::Dictionary(build_leaf_dict(entries))));
        return (leaf_ref, nodes);
    }

    let n_leaves = entries.len().div_ceil(LEAF_MAX);
    let chunk_size = entries.len().div_ceil(n_leaves);
    let mut kids: Vec<Object> = Vec::with_capacity(n_leaves);
    for chunk in entries.chunks(chunk_size) {
        let leaf_ref = alloc();
        nodes.push((leaf_ref, Object::Dictionary(build_leaf_dict(chunk))));
        kids.push(Object::Reference(leaf_ref));
    }
    let first = entries.first().map(|(k, _)| k.clone()).unwrap_or_default();
    let last = entries.last().map(|(k, _)| k.clone()).unwrap_or_default();
    let mut root = Dictionary::new();
    root.insert(
        "Limits",
        Object::Array(vec![Object::String(first), Object::String(last)]),
    );
    root.insert("Kids", Object::Array(kids));
    let root_ref = alloc();
    nodes.push((root_ref, Object::Dictionary(root)));
    (root_ref, nodes)
}

/// Leaf node dict: `/Limits [first last]` + `/Names [k1 v1 ...]`.
fn build_leaf_dict(entries: &[(Vec<u8>, Object)]) -> Dictionary {
    let first = entries.first().map(|(k, _)| k.clone()).unwrap_or_default();
    let last = entries.last().map(|(k, _)| k.clone()).unwrap_or_default();
    let mut pairs: Vec<Object> = Vec::with_capacity(entries.len() * 2);
    for (key, val) in entries {
        pairs.push(Object::String(key.clone()));
        pairs.push(val.clone());
    }
    let mut dict = Dictionary::new();
    dict.insert(
        "Limits",
        Object::Array(vec![Object::String(first), Object::String(last)]),
    );
    dict.insert("Names", Object::Array(pairs));
    dict
}

/// Internal generic walker shared by name + number readers.
///
/// `node` is a `Reference` (resolved + cycle-tracked here) or a `Dictionary`.
/// `leaf_key` is `"Names"` or `"Nums"`; `parse_key` converts a leaf key object
/// to `K` (or `None` to skip the pair).
#[allow(clippy::too_many_arguments)]
fn walk_tree<R, K, V, FK, FV>(
    pdf: &mut Pdf<R>,
    node: Object,
    leaf_key: &str,
    parse_key: &FK,
    decode: &mut FV,
    out: &mut Vec<(K, V)>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<()>
where
    R: Read + Seek,
    FK: Fn(Object) -> Option<K>,
    FV: FnMut(&mut Pdf<R>, Object) -> Result<Option<V>>,
{
    if depth >= max_depth {
        return Err(crate::Error::Unsupported(format!(
            "name_number_tree: /Kids depth limit {max_depth} exceeded"
        )));
    }

    // Resolve a reference node (cycle-tracked); inline dicts pass through.
    let mut dict: Dictionary = match node {
        Object::Dictionary(d) => d,
        Object::Reference(r) => {
            if !visited.insert(r) {
                return Ok(()); // cycle — skip
            }
            match pdf.resolve_borrowed(r)?.as_dict() {
                Some(d) => d.clone(),
                None => return Ok(()), // malformed node — skip
            }
        }
        _ => return Ok(()), // unexpected node type — skip
    };

    // Leaf takes priority over /Kids (spec leaf vs. intermediate). `dict` is
    // owned here, so move the leaf array out instead of copying it. A non-array
    // (or absent) leaf value falls through to /Kids, matching the old
    // `.and_then(Object::as_array)` semantics.
    if let Some(Object::Array(pairs)) = dict.remove(leaf_key) {
        let mut it = pairs.into_iter();
        while let Some(key_obj) = it.next() {
            let Some(val_obj) = it.next() else {
                break; // odd-length array — drop orphan key
            };
            let Some(key) = parse_key(key_obj) else {
                continue; // non-matching key type — skip pair
            };
            if let Some(v) = decode(pdf, val_obj)? {
                out.push((key, v));
            }
        }
        return Ok(());
    }

    // Intermediate node. Only indirect-reference kids are descended (inline-dict
    // kids are dropped, preserving the two legacy walkers this module replaces).
    if let Some(kids) = dict.get("Kids").and_then(Object::as_array) {
        // `kids` borrows the owned local `dict`; the recursive call only borrows
        // `pdf`/`decode`/… (never `dict`), so iterate the filter_map directly and
        // avoid a per-node `Vec<ObjectRef>` heap allocation.
        for r in kids.iter().filter_map(Object::as_ref_id) {
            walk_tree(
                pdf,
                Object::Reference(r),
                leaf_key,
                parse_key,
                decode,
                out,
                visited,
                depth + 1,
                max_depth,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn read_name_tree_skips_inline_dict_kid() {
        // A /Kids array element that is an inline Dictionary leaf (not an
        // indirect reference) must NOT be descended — preserving the legacy
        // reference-only descent. Only Reference kids are followed.
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
        assert!(out.is_empty(), "inline-dict kid must not be descended");
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
    fn read_number_tree_skips_noninteger_key() {
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Nums",
            Object::Array(vec![
                Object::Name("oops".into()), // non-integer key -> skip pair
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
        assert_eq!(out, vec![(7, 2)]);
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
        assert!(d.get("Limits").is_some());
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
        // Every node carries /Limits.
        for (_, n) in &nodes {
            let d = n.as_dict().expect("node dict");
            assert!(d.get("Limits").is_some());
        }
    }

    #[test]
    fn read_name_tree_skips_non_string_key() {
        // A /Names leaf whose key is not a PDF string drops that pair; its value
        // is still consumed so the following pair stays aligned.
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
        assert_eq!(out, vec![(b"ok".to_vec(), ObjectRef::new(11, 0))]);
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
    fn read_name_tree_odd_length_leaf_drops_orphan() {
        // An odd-length /Names array drops the trailing key with no value.
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
}
