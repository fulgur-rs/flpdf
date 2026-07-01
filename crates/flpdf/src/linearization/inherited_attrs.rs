//! Push inherited page attributes down to `/Page` leaves and strip them from
//! interior `/Pages` nodes, mirroring qpdf's `pushInheritedAttributesToPage`
//! (`QPDF_pages.cc:298-410`). Linearization runs this unconditionally before
//! computing the linearization plan — qpdf's `Lin::optimize` always passes
//! `allow_changes=true` for linearized output (`QPDF_linearization.cc:127-130`,
//! called only from `QPDFWriter::writeLinearized`). The normal (non-linearized)
//! write path never performs this step and must keep emitting `/Pages` nodes
//! verbatim.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use crate::object::{Object, ObjectRef};
use crate::{Error, Pdf, Result};

/// The four page attributes a `/Pages` node may pass down to its descendants
/// (ISO 32000-2 §7.7.3.4 Table 30, "Inheritable").
// Alphabetical order, matching qpdf's own iteration order: `cur_pages.getKeys()`
// (QPDF_Dictionary.cc) returns keys via a sorted `std::set<std::string>`, so
// `QPDF_pages.cc`'s push loop visits inheritable keys as CropBox, MediaBox,
// Resources, Rotate. When a single node needs to mint more than one of these
// in the same visit (direct, non-indirect values), the mint order — and thus
// which new object number each gets — must match qpdf's, so this array is
// kept in that same order rather than declaration-convenient order.
const INHERITABLE_KEYS: [&[u8]; 4] = [b"CropBox", b"MediaBox", b"Resources", b"Rotate"];

/// Defensive cycle/depth bound. qpdf relies on an earlier `cache()` pass (which
/// repairs duplicate page objects and detects loops) before its own recursive
/// push runs unguarded; flpdf has no equivalent repair pass, so this function
/// guards itself. Matches the bound already used for page-tree walks elsewhere
/// in this crate ([`crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH`]).
const MAX_DEPTH: usize = crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

/// Push inherited attributes to every `/Page` leaf and strip them from interior
/// `/Pages` nodes, mutating `pdf` in place.
///
/// # Errors
///
/// Propagates any [`Error`] from resolving an object while walking the tree, and
/// returns [`Error::Unsupported`] if the tree exceeds [`MAX_DEPTH`].
pub(crate) fn push_inherited_attributes_to_pages<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(());
    };
    let Some(pages_ref) = (match pdf.resolve_borrowed(root_ref)? {
        Object::Dictionary(d) => d.get_ref("Pages"),
        _ => None,
    }) else {
        return Ok(());
    };

    let mut key_ancestors: BTreeMap<&'static [u8], Vec<Object>> = BTreeMap::new();
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    push_internal(pdf, pages_ref, &mut key_ancestors, &mut visited, 0)?;
    debug_assert!(
        key_ancestors.values().all(Vec::is_empty),
        "key_ancestors not empty after pushing inherited attributes to pages"
    );
    Ok(())
}

fn push_internal<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<Object>>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<()> {
    if depth >= MAX_DEPTH {
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {MAX_DEPTH} at {node_ref}"
        )));
    }
    if !visited.insert(node_ref) {
        // Cycle guard: a node already on the path back to /Root. qpdf relies on
        // an earlier repair pass to make this unreachable; flpdf has none, so
        // this function defends itself. A well-formed tree never hits this.
        return Ok(());
    }

    let Object::Dictionary(mut dict) = pdf.resolve(node_ref)? else {
        return Ok(()); // Non-dictionary node: leave untouched (matches PageWalk's silent skip).
    };

    let mut own_keys: Vec<&'static [u8]> = Vec::new();
    for &key in &INHERITABLE_KEYS {
        let Some(value) = dict.remove(key) else {
            continue;
        };
        let value = match value {
            Object::Reference(_) => value, // already indirect: descendants share this ref
            Object::Array(_) | Object::Dictionary(_) => {
                // Direct (non-indirect) non-scalar value: mint a new indirect
                // object so descendants share ONE object instead of each
                // duplicating the structure inline (mirrors qpdf's
                // makeIndirectObject call in QPDF_pages.cc:355-360).
                let new_ref = next_object_ref(pdf)?;
                pdf.set_object(new_ref, value);
                Object::Reference(new_ref)
            }
            // Integer/Real/Boolean/Name/String/Null: copy by value, no minting.
            scalar => scalar,
        };
        key_ancestors.entry(key).or_default().push(value);
        own_keys.push(key);
    }

    let kids = dict
        .get("Kids")
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec);
    pdf.set_object(node_ref, Object::Dictionary(dict));

    if let Some(kids) = kids {
        for kid in &kids {
            let Object::Reference(kid_ref) = kid else {
                continue;
            };
            let is_pages_node = matches!(
                pdf.resolve_borrowed(*kid_ref)?,
                Object::Dictionary(d)
                    if matches!(d.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"Pages")
            );
            if is_pages_node {
                push_internal(pdf, *kid_ref, key_ancestors, visited, depth + 1)?;
            } else {
                let Object::Dictionary(mut leaf) = pdf.resolve(*kid_ref)? else {
                    continue;
                };
                for (&key, values) in key_ancestors.iter() {
                    if leaf.get(key).is_none() {
                        if let Some(v) = values.last() {
                            leaf.insert(key, v.clone());
                        }
                    }
                }
                pdf.set_object(*kid_ref, Object::Dictionary(leaf));
            }
        }
    }

    for key in own_keys {
        if let Some(stack) = key_ancestors.get_mut(key) {
            stack.pop();
            if stack.is_empty() {
                key_ancestors.remove(key);
            }
        }
    }
    Ok(())
}

/// Allocate a fresh indirect-object reference (the new-object idiom used across
/// the crate): one past the current highest object number.
fn next_object_ref<R: Read + Seek>(pdf: &Pdf<R>) -> Result<ObjectRef> {
    let n = pdf
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
    Ok(ObjectRef::new(n, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pdf;
    use std::io::Cursor;

    /// One `/Pages` node, one `/Page` leaf, no inheritable keys anywhere.
    /// Object layout: 1 Catalog, 2 Pages, 3 Page.
    fn pdf_with_no_inheritable_keys() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn no_inheritable_keys_is_a_no_op() {
        let bytes = pdf_with_no_inheritable_keys();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "no inheritable keys present anywhere: no object should be minted"
        );
        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary");
        };
        assert!(
            page_dict.get("MediaBox").is_some(),
            "the page's own /MediaBox must be untouched"
        );
    }

    /// `/Pages` (2) has a direct, scalar `/Rotate 90`. `/Page` (3) has none.
    /// Object layout: 1 Catalog, 2 Pages, 3 Page.
    fn pdf_with_inherited_scalar_rotate() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 90 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn scalar_rotate_is_copied_by_value_not_minted() {
        let bytes = pdf_with_inherited_scalar_rotate();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "a scalar inherited value must never mint a new object"
        );

        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("pages is not a dictionary");
        };
        assert!(
            pages_dict.get("Rotate").is_none(),
            "/Rotate must be stripped from the interior /Pages node"
        );

        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary");
        };
        assert_eq!(
            page_dict.get("Rotate"),
            Some(&Object::Integer(90)),
            "/Rotate must be pushed to the leaf as a direct (literal) value"
        );
    }

    /// `/Pages` (2) has a direct `/Resources` dict (non-scalar). `/Page` (3) has
    /// none. Object layout: 1 Catalog, 2 Pages, 3 Page.
    fn pdf_with_inherited_direct_resources() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 \
              /Resources << /Font << /F1 4 0 R >> >> >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn direct_non_scalar_resources_is_minted_as_new_object() {
        let bytes = pdf_with_inherited_direct_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count + 1,
            "a direct non-scalar inherited value must mint exactly one new object"
        );

        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("pages is not a dictionary");
        };
        assert!(
            pages_dict.get("Resources").is_none(),
            "/Resources must be stripped from the interior /Pages node"
        );

        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary");
        };
        let Some(Object::Reference(resources_ref)) = page_dict.get("Resources") else {
            panic!("/Resources must be pushed to the leaf as an indirect reference, not inline");
        };
        assert_eq!(
            resources_ref.number, 5,
            "the minted object must be the next free object number (4 was already in use)"
        );
        let minted = pdf.resolve(*resources_ref).expect("minted object resolves");
        let Object::Dictionary(minted_dict) = minted else {
            panic!("minted object is not a dictionary");
        };
        assert!(
            minted_dict.get("Font").is_some(),
            "the minted object must carry the original /Resources content"
        );
    }

    /// `/Pages` (2) has BOTH a direct `/CropBox` array and a direct `/MediaBox`
    /// array (both non-scalar, both need minting). `/Page` (3) has neither.
    /// qpdf mints in alphabetical key order (CropBox before MediaBox), so the
    /// CropBox object must get the lower object number.
    fn pdf_with_two_direct_non_scalar_keys_on_one_node() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 \
              /CropBox [0 0 100 100] /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn multiple_direct_non_scalar_keys_mint_in_qpdf_alphabetical_order() {
        let bytes = pdf_with_two_direct_non_scalar_keys_on_one_node();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary");
        };
        let Some(Object::Reference(crop_ref)) = leaf_dict.get("CropBox") else {
            panic!("/CropBox must be pushed as an indirect reference");
        };
        let Some(Object::Reference(media_ref)) = leaf_dict.get("MediaBox") else {
            panic!("/MediaBox must be pushed as an indirect reference");
        };
        assert!(
            crop_ref.number < media_ref.number,
            "/CropBox must mint before /MediaBox (qpdf's alphabetical getKeys() \
             order), got CropBox={crop_ref} MediaBox={media_ref}"
        );
    }

    /// `/Pages` (2) has `/Resources` as an *existing* indirect reference (4 0 R)
    /// rather than a direct dict. Two leaves (3, 5) both lack their own
    /// /Resources, so both must end up pointing at the SAME object 4 — no
    /// minting.
    fn pdf_with_already_indirect_resources_shared_by_two_pages() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 6 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn already_indirect_value_is_shared_not_reminted() {
        let bytes = pdf_with_already_indirect_resources_shared_by_two_pages();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "an already-indirect inherited value must never be re-minted"
        );

        for page_num in [3u32, 5] {
            let page = pdf
                .resolve(ObjectRef::new(page_num, 0))
                .unwrap_or_else(|e| panic!("page {page_num} resolves: {e}"));
            let Object::Dictionary(page_dict) = page else {
                panic!("page {page_num} is not a dictionary");
            };
            assert_eq!(
                page_dict.get("Resources"),
                Some(&Object::Reference(ObjectRef::new(4, 0))),
                "page {page_num} must share the original object 4, not a copy"
            );
        }
    }

    /// `/Pages` (2) has `/Resources` (4 0 R). The leaf `/Page` (3) has its OWN
    /// `/Resources` (5 0 R). The leaf's own value must win.
    fn pdf_with_leaf_local_resources_override() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Resources 5 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 6 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F2 6 0 R >> >>\nendobj\n");

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn leaf_local_value_is_never_overwritten() {
        let bytes = pdf_with_leaf_local_resources_override();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary");
        };
        assert_eq!(
            page_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(5, 0))),
            "the leaf's own /Resources (5 0 R) must NOT be replaced by the \
             ancestor's (4 0 R)"
        );
    }

    /// 3-level tree: grandparent /Pages (2) supplies /Resources (4 0 R).
    /// Parent /Pages (3) supplies its OWN /Resources (5 0 R), shadowing the
    /// grandparent's for everything under it. Leaf /Page (6) has neither, so
    /// it must inherit the NEAREST ancestor's value (5 0 R from the parent),
    /// not the grandparent's (4 0 R).
    fn pdf_with_three_level_nearest_ancestor_wins() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [6 0 R] /Count 1 \
              /Resources 5 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 7 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F2 7 0 R >> >>\nendobj\n");

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off7:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn nearest_ancestor_value_wins_in_three_level_tree() {
        let bytes = pdf_with_three_level_nearest_ancestor_wins();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(6, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary");
        };
        assert_eq!(
            leaf_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(5, 0))),
            "the leaf must inherit the NEAREST ancestor's /Resources (5 0 R, \
             from the parent /Pages), not the grandparent's (4 0 R)"
        );

        // Both interior nodes must have /Resources stripped.
        let grandparent = pdf.resolve(ObjectRef::new(2, 0)).expect("grandparent resolves");
        let Object::Dictionary(gp_dict) = grandparent else {
            panic!("grandparent is not a dictionary");
        };
        assert!(gp_dict.get("Resources").is_none());

        let parent = pdf.resolve(ObjectRef::new(3, 0)).expect("parent resolves");
        let Object::Dictionary(parent_dict) = parent else {
            panic!("parent is not a dictionary");
        };
        assert!(parent_dict.get("Resources").is_none());
    }

    /// /Pages (2)'s /Kids includes itself (2 0 R) alongside a real leaf (3 0 R).
    /// The walk must not loop forever.
    fn pdf_with_self_referential_pages_node() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 1 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn self_referential_pages_node_terminates() {
        let bytes = pdf_with_self_referential_pages_node();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        // Must return (Ok or Err), not hang. The test harness's own timeout
        // is the real backstop; this assertion documents the expected outcome.
        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(result.is_ok(), "a self-referential /Kids entry must be skipped, not error");
    }

    /// `/Root /Pages` (2) itself resolves to a non-dictionary object (a bare
    /// integer). The walk's very first `push_internal` call must bail via the
    /// "resolved node is not a dictionary" branch, not panic.
    fn pdf_with_non_dictionary_pages_root() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n42\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn non_dictionary_pages_root_is_a_no_op() {
        let bytes = pdf_with_non_dictionary_pages_root();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            result.is_ok(),
            "a non-dictionary /Pages root must be a no-op, not an error: {result:?}"
        );
    }

    /// `/Pages` (2)'s `/Kids` mixes a direct (non-reference) entry (`42`), a
    /// reference to a non-dictionary object (3, a literal string), and one
    /// real `/Page` leaf (4). Both malformed entries must be skipped; the
    /// real leaf must still receive the inherited `/Rotate`.
    fn pdf_with_malformed_kids_entries() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [42 3 0 R 4 0 R] /Count 1 /Rotate 90 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n(not a dictionary)\nendobj\n");

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn malformed_kids_entries_are_skipped_valid_leaf_still_pushed() {
        let bytes = pdf_with_malformed_kids_entries();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(4, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary");
        };
        assert_eq!(
            leaf_dict.get("Rotate"),
            Some(&Object::Integer(90)),
            "the one valid leaf must still receive the inherited /Rotate despite \
             the malformed sibling entries in /Kids"
        );
    }
}
