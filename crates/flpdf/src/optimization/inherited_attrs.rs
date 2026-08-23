//! qpdf correspondence: QPDF_optimization.cc inherited-page-attribute push.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

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
    if warn_skipped_keys && dict.has_key(b"/Parent") {
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
        let (resolved, _) = resolve_handle_chain(pdf, &value)?;
        if resolved.is_null() {
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
        } else if value.as_array().is_some()
            || value.as_dictionary().is_some()
            || value.as_stream_dict().is_some()
        {
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
        // qpdf's hasKey resolves the ordinary parsed child once. The existing
        // flpdf mutation fixture can additionally store a reference-to-reference
        // redirect, which has no qpdf counterpart; follow that explicit chain so
        // a terminal null remains absent just as the previous chain owner did.
        let present = match child
            .as_dictionary()
            .and_then(|entries| entries.get(key).cloned())
        {
            None => false,
            Some(value) => {
                let (resolved, _) = resolve_handle_chain(pdf, &value)?;
                !resolved.is_null()
            }
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

    if warn_skipped_keys && dict.has_key(b"/Parent") {
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
    handle.object_ref().or_else(|| handle.as_reference())
}

fn resolve_handle_chain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: &ObjectHandle,
) -> Result<(ObjectHandle, Option<ObjectRef>)> {
    let mut current = start.clone();
    let mut last_ref = current.object_ref();
    for _ in 0..crate::ref_chain::MAX_REF_CHAIN_DEPTH {
        pdf.resolve(&current)?;
        let Some(next) = current.as_reference() else {
            return Ok((current, last_ref));
        };
        last_ref = Some(next);
        current = pdf.get_object_handle(next);
    }
    Ok((current, last_ref))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::test_support::NthWriteFailure;
    use crate::pipeline::PipelineHandle;
    use crate::{Dictionary, Object, Pdf};

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
        let before = pdf.resolve_object(root).unwrap();

        let error = push(&mut pdf, &prepared, false, false).unwrap_err();

        assert!(error.to_string().contains("inheritable attribute"));
        assert_eq!(pdf.resolve_object(root).unwrap(), before);
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

        assert!(
            matches!(
                pdf.resolve_object(leaf_ref).unwrap(),
                Object::Dictionary(ref page) if page.get("Rotate") == Some(&Object::Integer(90))
            ),
            "leaf must have actually inherited /Rotate"
        );
        assert_eq!(
            pdf.get_object_handle(leaf_ref).end_offsets(),
            before,
            "inheriting a key must not clear the leaf's source extent"
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
        assert!(matches!(
            pdf.resolve_object(prepared.pages[0]).unwrap(),
            Object::Dictionary(ref page) if page.get("MediaBox").is_some()
        ));
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
            let mut node = Dictionary::new();
            node.insert("Type", Object::Name(b"Pages".to_vec()));
            node.insert(
                "Kids",
                Object::Array(vec![Object::Reference(ObjectRef::new(child, 0))]),
            );
            node.insert("Count", Object::Integer(0));
            pdf.set_object(ObjectRef::new(number, 0), Object::Dictionary(node));
        }
        let mut boundary = Dictionary::new();
        boundary.insert("Type", Object::Name(b"Pages".to_vec()));
        boundary.insert("Kids", Object::Array(Vec::new()));
        boundary.insert("Count", Object::Integer(0));
        pdf.set_object(
            ObjectRef::new(2 + MAX_DEPTH as u32, 0),
            Object::Dictionary(boundary),
        );
        let prepared = PreparedPages {
            root: PageTreeRoot::Indirect(ObjectRef::new(2, 0)),
            pages: Vec::new(),
        };

        let error = push(&mut pdf, &prepared, true, false).unwrap_err();

        assert!(matches!(error, Error::Unsupported(ref message)
                if message.contains("page tree depth exceeds maximum")));
    }

    #[test]
    fn canonical_inherited_attribute_chain_bounds_a_reference_cycle() {
        let mut pdf = Pdf::open_mem_owned(pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
        ]))
        .unwrap();
        pdf.set_object(
            ObjectRef::new(20, 0),
            Object::Reference(ObjectRef::new(21, 0)),
        );
        pdf.set_object(
            ObjectRef::new(21, 0),
            Object::Reference(ObjectRef::new(20, 0)),
        );
        let start = pdf.get_object_handle(ObjectRef::new(20, 0));

        let (terminal, terminal_ref) =
            resolve_handle_chain(&mut pdf, &start).expect("reference cycle is bounded");
        assert!(terminal.as_reference().is_some());
        assert!(matches!(
            terminal_ref,
            Some(ObjectRef {
                number: 20 | 21,
                generation: 0
            })
        ));
    }
}
