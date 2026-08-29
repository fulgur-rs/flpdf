//! qpdf correspondence: QPDF_optimization.cc inherited-page-attribute push.
//!
//! Deviation: null checks on an inheritable key's value chase through a
//! [`Pdf::set_object`] bare-reference redirect to its terminal via
//! [`Pdf::resolve_to_terminal`], which has no qpdf counterpart (qpdf's own
//! object graph can never hold a stored "this object's value is another
//! reference" redirect the way `Pdf::set_object` permits). See
//! `pages.rs`'s `resolve_inherited_handle_with_max_depth` for the same
//! compensation in the sibling bottom-up attribute climb, and the inline
//! deviation-marker comments below for the exact call sites.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use crate::object_handle::is_scalar;
use crate::pages::repair::{PageTreeRoot, PreparedPages};
use crate::{Error, Pdf, Result};
use crate::{ObjectHandle, ObjectRef};

const INHERITABLE_KEYS: [&[u8]; 4] = [b"/CropBox", b"/MediaBox", b"/Resources", b"/Rotate"];
const MAX_DEPTH: usize = crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

pub(crate) fn push<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    prepared: &PreparedPages,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<()> {
    let mut key_ancestors: BTreeMap<&'static [u8], Vec<ObjectHandle>> = BTreeMap::new();
    let mut visited = BTreeSet::new();
    match prepared.root {
        PageTreeRoot::Indirect(root) => push_internal(
            pdf,
            root,
            &mut key_ancestors,
            &mut visited,
            allow_changes,
            warn_skipped_keys,
            0,
        )?,
        PageTreeRoot::Direct { catalog } => push_direct_root(
            pdf,
            catalog,
            &mut key_ancestors,
            &mut visited,
            allow_changes,
            warn_skipped_keys,
        )?, // cov:ignore: direct-root integration tests exercise this generic dispatch; llvm maps its counter to the callee
    }
    debug_assert!(
        key_ancestors.values().all(Vec::is_empty),
        "key_ancestors not empty after pushing inherited attributes to pages"
    );
    Ok(())
}

fn push_direct_root<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    catalog_ref: ObjectRef,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<()> {
    // cov:ignore-start: PreparedPages::Direct is created and consumed without an intervening public mutation
    let catalog = pdf.get_object_handle(catalog_ref);
    pdf.resolve(&catalog)?;
    let pages = catalog.get_key(b"/Pages");
    if pages.as_dictionary().is_none() {
        return Ok(());
    }
    // cov:ignore-end
    push_direct_node(
        pdf,
        &pages,
        key_ancestors,
        visited,
        allow_changes,
        warn_skipped_keys,
        0,
    )?;
    Ok(())
}

fn push_direct_node<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &ObjectHandle,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
    depth: usize,
) -> Result<()> {
    // cov:ignore-start: prepare_for_optimization traverses the same direct tree with this depth bound first
    if depth >= MAX_DEPTH {
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {MAX_DEPTH} in direct /Pages node"
        )));
    }
    if !is_pages_dictionary(dict) {
        return Ok(());
    }
    // cov:ignore-end

    // cov:ignore-start: all production callers pass warn_skipped_keys=false
    if warn_skipped_keys && dict.try_has_key(b"/Parent")? {
        for key in dict.try_get_keys()? {
            if !INHERITABLE_KEYS.contains(&key.as_slice())
                && ![b"/Type".as_slice(), b"/Parent", b"/Kids", b"/Count"].contains(&key.as_slice())
            {
                pdf.push_warning(format!(
                    "Unknown key /{} in /Pages object is being discarded as a result of flattening the /Pages tree",
                    String::from_utf8_lossy(key.strip_prefix(b"/").unwrap_or(&key)),
                ))?;
            }
        }
    }
    // cov:ignore-end

    let own_keys = push_node_attributes(pdf, dict, key_ancestors, allow_changes)?;
    let kids = dict.get_key(b"/Kids");
    if let Some(kids) = kids.as_array() {
        for kid in kids {
            if let Some(kid_ref) = handle_reference(&kid) {
                push_child_reference(
                    pdf,
                    kid_ref,
                    key_ancestors,
                    visited,
                    allow_changes,
                    warn_skipped_keys,
                    depth,
                )?; // cov:ignore: direct-root integration test exercises this branch; LLVM attributes the counter to push_child_reference
            } else if kid.as_dictionary().is_some() && kid.has_key(b"/Kids") {
                push_direct_node(
                    pdf,
                    &kid,
                    key_ancestors,
                    visited,
                    allow_changes,
                    warn_skipped_keys,
                    depth + 1,
                )?; // cov:ignore: direct-descendant integration test exercises this branch; LLVM attributes the counter to push_direct_node
            } // cov:ignore: direct-descendant integration test exercises the branch; LLVM attributes the counter to the recursive callee
        }
    }
    pop_node_attributes(key_ancestors, own_keys);
    Ok(())
}

fn push_node_attributes<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &ObjectHandle,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    allow_changes: bool,
) -> Result<Vec<&'static [u8]>> {
    let mut own_keys = Vec::new();
    for &key in &INHERITABLE_KEYS {
        let Some(value) = dict
            .as_dictionary()
            .and_then(|entries| entries.get(key).cloned())
        else {
            continue;
        };
        // qpdf-deviation: terminal chase compensates for a Pdf::set_object
        // bare-reference redirect that has no qpdf counterpart (see
        // reader.rs::resolve_to_terminal_ref).
        if pdf.resolve_to_terminal(&value)?.is_null() {
            continue;
        }
        if !allow_changes {
            return Err(Error::Unsupported(
                "optimize detected an inheritable attribute when called in no-change mode"
                    .to_owned(),
            ));
        }
        dict.remove_key(key);
        let value = if value.is_indirect() {
            value
        } else if !is_scalar(&value)? {
            pdf.make_indirect_from_object_handle(value)?
        } else {
            value
        };
        key_ancestors.entry(key).or_default().push(value);
        own_keys.push(key);
    }
    if !own_keys.is_empty() {
        pdf.mark_object_handle_dirty(dict)?;
    }
    Ok(own_keys)
}

fn pop_node_attributes(
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    own_keys: Vec<&'static [u8]>,
) {
    for key in own_keys {
        let stack = key_ancestors
            .get_mut(key)
            .expect("own inherited key must have an ancestor stack");
        stack.pop();
        if stack.is_empty() {
            key_ancestors.remove(key);
        }
    }
}

fn push_child_reference<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    kid_ref: ObjectRef,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
    depth: usize,
) -> Result<()> {
    let child = pdf.get_object_handle(kid_ref);
    pdf.resolve(&child)?;
    let is_pages_node = is_pages_dictionary(&child);
    if is_pages_node {
        return push_internal(
            pdf,
            kid_ref,
            key_ancestors,
            visited,
            allow_changes,
            warn_skipped_keys,
            depth + 1,
        );
    }

    if child.as_dictionary().is_none() {
        return Ok(()); // cov:ignore: page-tree repair guarantees indirect children are dictionaries
    }
    for (&key, values) in key_ancestors.iter() {
        let present = match child
            .as_dictionary()
            .and_then(|entries| entries.get(key).cloned())
        {
            None => false,
            // qpdf-deviation: terminal chase compensates for a Pdf::set_object
            // bare-reference redirect that has no qpdf counterpart (see
            // reader.rs::resolve_to_terminal_ref).
            Some(value) => !pdf.resolve_to_terminal(&value)?.is_null(),
        };
        if !present {
            if let Some(value) = values.last() {
                child.replace_key(key, value.clone())?;
                pdf.mark_object_handle_dirty(&child)?;
            }
        }
    }
    Ok(())
}

fn push_internal<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
    depth: usize,
) -> Result<()> {
    if depth >= MAX_DEPTH {
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {MAX_DEPTH} at {node_ref}"
        )));
    }
    if !visited.insert(node_ref) {
        return Ok(()); // cov:ignore: page-tree repair rejects cycles before inherited-attribute push
    }

    let dict = pdf.get_object_handle(node_ref);
    pdf.resolve(&dict)?;
    if dict.as_dictionary().is_none() {
        return Ok(());
    }
    if !is_pages_dictionary(&dict) {
        return Ok(());
    }

    if warn_skipped_keys && dict.try_has_key(b"/Parent")? {
        for key in dict.try_get_keys()? {
            if !INHERITABLE_KEYS.contains(&key.as_slice())
                && ![b"/Type".as_slice(), b"/Parent", b"/Kids", b"/Count"].contains(&key.as_slice())
            {
                pdf.push_warning(format!(
                    "Unknown key /{} in /Pages object is being discarded as a result of flattening the /Pages tree",
                    String::from_utf8_lossy(key.strip_prefix(b"/").unwrap_or(&key)),
                ))?;
            }
        }
    }

    let own_keys = push_node_attributes(pdf, &dict, key_ancestors, allow_changes)?;

    let kids = dict.get_key(b"/Kids").as_array();

    if let Some(kids) = kids {
        for kid in kids {
            let Some(kid_ref) = handle_reference(&kid) else {
                continue;
            };
            push_child_reference(
                pdf,
                kid_ref,
                key_ancestors,
                visited,
                allow_changes,
                warn_skipped_keys,
                depth,
            )?;
        }
    }

    pop_node_attributes(key_ancestors, own_keys);
    Ok(())
}

fn is_pages_dictionary(handle: &ObjectHandle) -> bool {
    handle
        .as_dictionary()
        .and_then(|entries| entries.get(b"/Type".as_slice()).cloned())
        .and_then(|value| value.as_name())
        .is_some_and(|name| name == b"Pages")
}

fn handle_reference(handle: &ObjectHandle) -> Option<ObjectRef> {
    handle.object_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::test_support::NthWriteFailure;
    use crate::pipeline::PipelineHandle;
    use crate::{ObjectHandle, Pdf};

    fn pdf_bytes(bodies: &[(u32, &[u8])]) -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0_u64; bodies.last().map_or(1, |(number, _)| *number as usize + 1)];
        for &(number, body) in bodies {
            offsets[number as usize] = pdf.len() as u64;
            pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len() as u64;
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                bodies.last().map_or(1, |(number, _)| number + 1)
            )
            .as_bytes(),
        );
        pdf
    }

    fn pdf_with_inherited_scalar_rotate() -> Vec<u8> {
        pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 90 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
        ])
    }

    fn pdf_with_unknown_intermediate_pages_key() -> Vec<u8> {
        pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
            ),
            (
                3,
                b"<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 /Unknown 7 >>",
            ),
            (4, b"<< /Type /Page /Parent 3 0 R >>"),
        ])
    }

    /// Installs a two-hop `Pdf::set_object` bare-reference redirect
    /// (`holder 0 R` -> `target 0 R` -> null) at `holder_ref`, where
    /// `target_ref` is a fresh object whose value resolves to null.
    fn install_multi_hop_null_redirect<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        holder_ref: ObjectRef,
        target_ref: ObjectRef,
    ) {
        pdf.replace_object(target_ref, ObjectHandle::null())
            .unwrap();
        pdf.replace_object(
            holder_ref,
            ObjectHandle::from_value(crate::object_handle::ObjectValue::Reference(target_ref)),
        )
        .unwrap();
    }

    #[test]
    fn no_change_mode_rejects_inheritable_key_before_mutation() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_inherited_scalar_rotate()).unwrap();
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();
        let PageTreeRoot::Indirect(root) = prepared.root else {
            // cov:ignore-start: fixture catalog has an indirect /Pages root
            panic!("fixture has an indirect /Pages root");
            // cov:ignore-end
        };
        let root_handle = pdf.get_object_handle(root);
        pdf.resolve(&root_handle).unwrap();
        let before = root_handle.get_key(b"/Rotate").as_integer();

        let error = push(&mut pdf, &prepared, false, false).unwrap_err();

        assert!(error.to_string().contains("inheritable attribute"));
        assert_eq!(root_handle.get_key(b"/Rotate").as_integer(), before);
    }

    #[test]
    fn pushing_an_inherited_key_onto_a_leaf_preserves_its_source_extent() {
        // The fixture's page 3 lacks /Rotate, so it actually inherits
        // /Rotate 90 from its /Pages parent -- exercising
        // `push_child_reference`'s `changed` branch, not the unchanged-leaf
        // skip. qpdf's `kid.replaceKey` never clears the leaf's recorded
        // source extent for this same mutation; the leaf's extent here must
        // match.
        let mut pdf = Pdf::open_mem_owned(pdf_with_inherited_scalar_rotate()).unwrap();
        let leaf_ref = ObjectRef::new(3, 0);
        pdf.get_object_handle(leaf_ref).try_dereference().unwrap();
        let before = pdf.get_object_handle(leaf_ref).end_offsets();
        assert_ne!(
            before,
            (-1, -1),
            "fixture leaf must have a real parse extent"
        );

        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();
        push(&mut pdf, &prepared, true, false).unwrap();

        let leaf = pdf.get_object_handle(leaf_ref);
        pdf.resolve(&leaf).unwrap();
        assert_eq!(
            leaf.get_key(b"/Rotate").as_integer(),
            Some(90),
            "leaf must have actually inherited /Rotate"
        );
        assert_eq!(
            pdf.get_object_handle(leaf_ref).end_offsets(),
            before,
            "inheriting a key must not clear the leaf's source extent"
        );
    }

    #[test]
    fn leaf_multi_hop_null_reference_chain_is_treated_as_absent() {
        let mut pdf = Pdf::open_mem_owned(pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>",
            ),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                  /Resources 6 0 R >>",
            ),
            (4, b"<< /Font << /F1 5 0 R >> >>"),
            (5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (6, b"null"),
            (7, b"null"),
        ]))
        .unwrap();
        install_multi_hop_null_redirect(&mut pdf, ObjectRef::new(6, 0), ObjectRef::new(7, 0));

        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();
        push(&mut pdf, &prepared, true, false).unwrap();

        let leaf = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve(&leaf).unwrap();
        assert_eq!(
            leaf.get_key(b"/Resources").object_ref(),
            Some(ObjectRef::new(4, 0)),
            "a /Resources reaching null through a two-hop reference chain \
             (6 0 R -> 7 0 R -> null) must be treated as absent and replaced \
             by the inherited value, not just a single-hop null"
        );
    }

    #[test]
    fn ancestor_multi_hop_null_reference_chain_does_not_shadow_grandparent() {
        let mut pdf = Pdf::open_mem_owned(pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 5 0 R >>",
            ),
            (
                3,
                b"<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 \
                  /Resources 7 0 R >>",
            ),
            (
                4,
                b"<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>",
            ),
            (5, b"<< /Font << /F1 6 0 R >> >>"),
            (6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (7, b"null"),
            (8, b"null"),
        ]))
        .unwrap();
        install_multi_hop_null_redirect(&mut pdf, ObjectRef::new(7, 0), ObjectRef::new(8, 0));

        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();
        push(&mut pdf, &prepared, true, false).unwrap();

        let child = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve(&child).unwrap();
        assert_eq!(
            child.get_key(b"/Resources").object_ref(),
            Some(ObjectRef::new(7, 0)),
            "a /Resources reaching null through a two-hop reference chain on \
             the child /Pages node must be left in place, not erased"
        );

        let leaf = pdf.get_object_handle(ObjectRef::new(4, 0));
        pdf.resolve(&leaf).unwrap();
        assert_eq!(
            leaf.get_key(b"/Resources").object_ref(),
            Some(ObjectRef::new(5, 0)),
            "the leaf must inherit the GRANDPARENT's real /Resources, not be \
             shadowed by the child's two-hop null reference chain"
        );
    }

    #[test]
    fn direct_pages_root_walks_direct_descendant_and_reference_kids() {
        let mut pdf = Pdf::open_mem_owned(pdf_bytes(&[
            (
                1,
                b"<< /Type /Catalog /Pages << /Type /Pages /Kids [<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 90 >>] /Count 1 >> >>",
            ),
            (
                3,
                b"<< /Type /Page /MediaBox [0 0 612 792] >>",
            ),
        ]))
        .unwrap();
        let prepared = PreparedPages {
            root: PageTreeRoot::Direct {
                catalog: ObjectRef::new(1, 0),
            },
            pages: vec![ObjectRef::new(3, 0)],
        };

        push(&mut pdf, &prepared, true, false).expect("direct root walk");

        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve(&page).expect("resolve direct-root page");
        assert_eq!(page.get_key(b"/Rotate").as_integer(), Some(90));
    }

    #[test]
    fn warning_mode_reports_unknown_intermediate_pages_key() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_unknown_intermediate_pages_key()).unwrap();
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();

        push(&mut pdf, &prepared, true, true).unwrap();

        assert!(pdf.repair_diagnostics().entries().iter().any(|diagnostic| {
            diagnostic.message.contains("Unknown key /Unknown")
                && diagnostic.message.contains("/Pages")
        }));
        let page = pdf.get_object_handle(prepared.pages[0]);
        pdf.resolve(&page).unwrap();
        assert!(page.has_key(b"/MediaBox"));
    }

    #[test]
    fn warning_sink_failure_propagates_from_an_intermediate_pages_key() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_unknown_intermediate_pages_key()).unwrap();
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();
        let logger = crate::QPDFLogger::create();
        logger.set_warn(Some(PipelineHandle::new(NthWriteFailure::new(1))));
        pdf.set_logger(logger);

        assert!(matches!(
            push(&mut pdf, &prepared, true, true),
            Err(crate::Error::System(ref message)) if message == "sink write failure 1"
        ));
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Unknown key /Unknown")));
    }

    #[test]
    fn excessive_depth_error_propagates_from_a_child_pages_node() {
        let mut pdf = Pdf::open_mem_owned(pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>"),
        ]))
        .unwrap();
        for depth in 0..MAX_DEPTH {
            let number = 2 + depth as u32;
            let child = number + 1;
            let node = ObjectHandle::dictionary(vec![
                (b"/Type".to_vec(), ObjectHandle::name(b"Pages".to_vec())),
                (
                    b"/Kids".to_vec(),
                    ObjectHandle::array(vec![pdf.get_object_handle(ObjectRef::new(child, 0))]),
                ),
                (b"/Count".to_vec(), ObjectHandle::integer(0)),
            ]);
            pdf.replace_object(ObjectRef::new(number, 0), node).unwrap();
        }
        let boundary = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Pages".to_vec())),
            (b"/Kids".to_vec(), ObjectHandle::array(Vec::new())),
            (b"/Count".to_vec(), ObjectHandle::integer(0)),
        ]);
        pdf.replace_object(ObjectRef::new(2 + MAX_DEPTH as u32, 0), boundary)
            .unwrap();
        let prepared = PreparedPages {
            root: PageTreeRoot::Indirect(ObjectRef::new(2, 0)),
            pages: Vec::new(),
        };

        let error = push(&mut pdf, &prepared, true, false).unwrap_err();

        assert!(matches!(error, Error::Unsupported(ref message)
                if message.contains("page tree depth exceeds maximum")));
    }
}
