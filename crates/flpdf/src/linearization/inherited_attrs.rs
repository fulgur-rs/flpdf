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
// Alphabetical order, matching qpdf's own iteration order: `Pages::items` is a
// `std::map<std::string, QPDFObjectHandle>` (QPDF_pages.cc), so
// `cur_pages.getKeys()` visits inheritable keys as CropBox, MediaBox,
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
}
