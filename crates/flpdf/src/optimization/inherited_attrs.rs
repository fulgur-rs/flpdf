//! qpdf correspondence: QPDF_optimization.cc inherited-page-attribute push.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use crate::object::{Object, ObjectRef};
use crate::pages::repair::{PageTreeRoot, PreparedPages};
use crate::ref_chain::terminal_ref_of_chain;
use crate::{Error, Pdf, Result};

const INHERITABLE_KEYS: [&[u8]; 4] = [b"CropBox", b"MediaBox", b"Resources", b"Rotate"];
const MAX_DEPTH: usize = crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

pub(crate) fn push<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    prepared: &PreparedPages,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<()> {
    let mut key_ancestors: BTreeMap<&'static [u8], Vec<Object>> = BTreeMap::new();
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
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<Object>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<()> {
    // cov:ignore-start: PreparedPages::Direct is created and consumed without an intervening public mutation
    let Object::Dictionary(mut catalog) = pdf.resolve_object(catalog_ref)? else {
        return Ok(());
    };
    let Some(Object::Dictionary(mut root)) = catalog.get("Pages").cloned() else {
        return Ok(());
    };
    // cov:ignore-end
    push_direct_node(
        pdf,
        &mut root,
        key_ancestors,
        visited,
        allow_changes,
        warn_skipped_keys,
        0,
    )?;
    catalog.insert("Pages", Object::Dictionary(root));
    pdf.set_object(catalog_ref, Object::Dictionary(catalog));
    Ok(())
}

fn push_direct_node<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &mut crate::Dictionary,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<Object>>,
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
    if !matches!(dict.get("Type"), Some(Object::Name(name)) if name == b"Pages") {
        return Ok(());
    }
    // cov:ignore-end

    // cov:ignore-start: all production callers pass warn_skipped_keys=false
    if warn_skipped_keys && dict.get("Parent").is_some() {
        let entries = crate::qpdf_null::snapshot_entries(dict, false);
        for (key, _) in crate::qpdf_null::visible_entries(pdf, entries)? {
            if !INHERITABLE_KEYS.contains(&key.as_slice())
                && ![b"Type".as_slice(), b"Parent", b"Kids", b"Count"].contains(&key.as_slice())
            {
                pdf.push_warning(format!(
                    "Unknown key /{} in /Pages object is being discarded as a result of flattening the /Pages tree",
                    String::from_utf8_lossy(&key),
                ))?;
            }
        }
    }
    // cov:ignore-end

    let own_keys = push_node_attributes(pdf, dict, key_ancestors, allow_changes)?;
    let mut kids = dict
        .get("Kids")
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec)
        .unwrap_or_default();
    for kid in &mut kids {
        match kid {
            Object::Reference(kid_ref) => push_child_reference(
                pdf,
                *kid_ref,
                key_ancestors,
                visited,
                allow_changes,
                warn_skipped_keys,
                depth,
            )?, // cov:ignore: direct-root integration test exercises this branch; llvm maps the counter to push_child_reference
            Object::Dictionary(child) if child.get("Kids").is_some() => push_direct_node(
                pdf,
                child,
                key_ancestors,
                visited,
                allow_changes,
                warn_skipped_keys,
                depth + 1,
            )?, // cov:ignore: direct-descendant integration test exercises this branch; llvm maps the counter to the recursive callee
            _ => {} // cov:ignore: direct non-dictionary /Kids regression exercises this arm; llvm emits no counter for the empty match arm
        }
    }
    dict.insert("Kids", Object::Array(kids));
    pop_node_attributes(key_ancestors, own_keys);
    Ok(())
}

fn push_node_attributes<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &mut crate::Dictionary,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<Object>>,
    allow_changes: bool,
) -> Result<Vec<&'static [u8]>> {
    let mut own_keys = Vec::new();
    for &key in &INHERITABLE_KEYS {
        let Some(value) = dict.remove(key) else {
            continue;
        };
        let is_null = match &value {
            Object::Null => true,
            Object::Reference(reference) => {
                let terminal = terminal_ref_of_chain(pdf, *reference)?;
                matches!(pdf.resolve_borrowed(terminal)?, Object::Null)
            }
            _ => false,
        };
        if is_null {
            dict.insert(key, value);
            continue;
        }
        if !allow_changes {
            return Err(Error::Unsupported(
                "optimize detected an inheritable attribute when called in no-change mode"
                    .to_owned(),
            ));
        }
        let value = match value {
            Object::Reference(_) => value,
            Object::Array(_) | Object::Dictionary(_) => {
                let new_ref = next_object_ref(pdf)?;
                pdf.set_object(new_ref, value);
                Object::Reference(new_ref)
            }
            scalar => scalar,
        };
        key_ancestors.entry(key).or_default().push(value);
        own_keys.push(key);
    }
    Ok(own_keys)
}

fn pop_node_attributes(
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<Object>>,
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
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<Object>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
    depth: usize,
) -> Result<()> {
    let is_pages_node = matches!(
        pdf.resolve_borrowed(kid_ref)?,
        Object::Dictionary(dict)
            if matches!(dict.get("Type"), Some(Object::Name(name)) if name == b"Pages")
    );
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

    let Object::Dictionary(mut leaf) = pdf.resolve_object(kid_ref)? else {
        return Ok(()); // cov:ignore: page-tree repair guarantees indirect children are dictionaries
    };
    let mut changed = false;
    for (&key, values) in key_ancestors.iter() {
        let present = match leaf.get(key) {
            None | Some(Object::Null) => false,
            Some(Object::Reference(reference)) => {
                let terminal = terminal_ref_of_chain(pdf, *reference)?;
                !matches!(pdf.resolve_borrowed(terminal)?, Object::Null)
            }
            Some(_) => true,
        };
        if !present {
            if let Some(value) = values.last() {
                leaf.insert(key, value.clone());
                changed = true;
            }
        }
    }
    // qpdf calls `kid.replaceKey(key, ...)` (`QPDF_optimization.cc:222-227`)
    // only for a key actually being added; an already-complete leaf is never
    // touched, and `replaceKey` itself never calls `updateCache`
    // (`QPDFObjectHandle.cc:1199-1209` delegates straight to the
    // dictionary's own key mutation), so even a leaf that *does* inherit a
    // key keeps its recorded source extent in qpdf. `Pdf::set_object` has no
    // equivalent surgical per-key primitive here (it always clears the
    // extent, matching qpdf's whole-object `replaceObject`/`updateCache`,
    // `QPDF.cc:1985-1993`); preserve the extent explicitly across the
    // otherwise-equivalent whole-dictionary write-back this module uses.
    if changed {
        let handle = pdf.get_object_handle(kid_ref);
        let extents = handle.end_offsets();
        pdf.set_object(kid_ref, Object::Dictionary(leaf));
        pdf.get_object_handle(kid_ref)
            .set_end_offsets(extents.0, extents.1);
    }
    Ok(())
}

fn push_internal<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<Object>>,
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

    let Object::Dictionary(mut dict) = pdf.resolve_object(node_ref)? else {
        return Ok(());
    };
    if !matches!(dict.get("Type"), Some(Object::Name(name)) if name == b"Pages") {
        return Ok(());
    }

    if warn_skipped_keys && dict.get("Parent").is_some() {
        let entries = crate::qpdf_null::snapshot_entries(&dict, false);
        for (key, _) in crate::qpdf_null::visible_entries(pdf, entries)? {
            if !INHERITABLE_KEYS.contains(&key.as_slice())
                && ![b"Type".as_slice(), b"Parent", b"Kids", b"Count"].contains(&key.as_slice())
            {
                pdf.push_warning(format!(
                    "Unknown key /{} in /Pages object is being discarded as a result of flattening the /Pages tree",
                    String::from_utf8_lossy(&key),
                ))?;
            }
        }
    }

    let own_keys = push_node_attributes(pdf, &mut dict, key_ancestors, allow_changes)?;

    let kids = dict
        .get("Kids")
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec);
    // qpdf calls `cur_pages.removeKey(key)` (`QPDF_optimization.cc:200`)
    // only for an inheritable key actually being pulled up; a node with no
    // inheritable attributes of its own is never written back, and
    // `removeKey` itself never calls `updateCache`
    // (`QPDFObjectHandle.cc:1227-1236` delegates straight to the
    // dictionary's own key mutation), so even a node that *does* have an
    // attribute pulled up keeps its recorded source extent in qpdf.
    // `Pdf::set_object` has no equivalent surgical per-key primitive here
    // (it always clears the extent, matching qpdf's whole-object
    // `replaceObject`/`updateCache`, `QPDF.cc:1985-1993`); preserve the
    // extent explicitly across the otherwise-equivalent whole-dictionary
    // write-back this module uses.
    if !own_keys.is_empty() {
        let handle = pdf.get_object_handle(node_ref);
        let extents = handle.end_offsets();
        pdf.set_object(node_ref, Object::Dictionary(dict));
        pdf.get_object_handle(node_ref)
            .set_end_offsets(extents.0, extents.1);
    }

    if let Some(kids) = kids {
        for kid in kids {
            let Object::Reference(kid_ref) = kid else {
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

fn next_object_ref<R: Read + Seek>(pdf: &Pdf<R>) -> Result<ObjectRef> {
    // qpdf's page optimization allocates through the same object cache that
    // `getAllPagesInternal` uses for direct-leaf promotion and duplicate-page
    // cloning. Looking only at the legacy raw cache can reuse a freshly minted
    // canonical page object number (`QPDF.cc:1271-1283,1872-1888`).
    pdf.next_obj_gen()
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
}
