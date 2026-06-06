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
/// Propagates [`Pdf::resolve`] errors and returns [`crate::Error::Unsupported`]
/// if a `/Kids` chain reaches `max_depth`.
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
/// Propagates [`Pdf::resolve`] errors and returns [`crate::Error::Unsupported`]
/// if a `/Kids` chain reaches `max_depth`.
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
    let dict: Dictionary = match node {
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

    // Leaf takes priority over /Kids (spec leaf vs. intermediate).
    if let Some(arr) = dict.get(leaf_key).and_then(Object::as_array) {
        let pairs = arr.to_vec(); // own the leaf array, drop the dict borrow
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

    // Intermediate node.
    if let Some(kids) = dict.get("Kids").and_then(Object::as_array) {
        for kid in kids.iter().cloned() {
            walk_tree(
                pdf, kid, leaf_key, parse_key, decode, out, visited, depth + 1,
                max_depth,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            |_, v| Ok(v.as_ref_id()),
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
            |_, v| Ok(v.as_ref_id()),
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
            |_, v| Ok(v.as_ref_id()),
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
            |_, v| Ok(v.as_ref_id()),
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
        let err = read_name_tree(
            &mut pdf,
            Object::Reference(r40),
            |_, v: Object| Ok(Some(v)),
            2,
        );
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
}
